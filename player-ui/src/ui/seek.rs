//! The seek slider: who owns the knob, and when a seek is issued.
//!
//! Seek ownership: any user change (drag, keys, click-to-jump below) marks
//! the slider user-owned and re-arms a short commit timer; the seek is
//! issued once the user stops moving, and the pump leaves the slider alone
//! until then.
//!
//! No GestureClick here: a capturing gesture on the slider steals the
//! pointer sequence from the Scale's own drag gesture, and dragging the
//! knob stops working entirely (the "seek bar not working" bug). The
//! pointer handling goes through a legacy controller instead — it observes
//! the raw stream without claiming the sequence, so the Scale's own drag
//! gesture keeps working.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use player_core::{MpvEngine, PlaybackEngine};

use super::now_ms;

/// Delay between the last slider move and the committed seek: short enough to
/// feel instant, long enough to let a whole drag run as one seek.
const SEEK_COMMIT_MS: u64 = 350;
/// How long the pump holds the seek knob at the target after a seek is issued,
/// covering mpv's async seek latency so the knob never snaps back to the stale
/// position for a frame or two.
const SEEK_SETTLE_MS: i64 = 400;
/// Half of the seek knob (38 px in style.css): click-to-position maps the
/// track between the knob's edge stops.
const KNOB_HALF_PX: f64 = 19.0;

/// The pressed pixel (already in the slider's own coordinate space) as a
/// 0..=1 fraction of the track. `None` when the slider is too narrow to map.
///
/// An RTL slider draws its trough from the right edge (the direction is set
/// per widget, see `window::apply_rtl`), so its pixel→value map is flipped —
/// without that, clicking the right end of an Arabic UI would seek to the start.
fn click_fraction(x: f64, width: f64, rtl: bool) -> Option<f64> {
    if width <= 2.0 * KNOB_HALF_PX {
        return None;
    }
    let fraction = ((x - KNOB_HALF_PX) / (width - 2.0 * KNOB_HALF_PX)).clamp(0.0, 1.0);
    Some(if rtl { 1.0 - fraction } else { fraction })
}

/// The two cells `Controls` needs in order to leave the slider alone while
/// the user owns it (and for a short settle window after each seek).
pub(super) struct SeekHandle {
    pub seeking: Rc<Cell<bool>>,
    pub seek_settle: Rc<Cell<i64>>,
}

/// Wire the slider's interaction handling onto `scale`:
///
/// - Press (click or grab): jump to the pressed position immediately.
///   A click is a deliberate seek; grabbing the knob maps to ~the
///   current position, so it is a no-op there.
/// - Release: commit the seek to wherever the drag ended, right away.
/// - The debounce timer stays only as a safety net in case the release
///   is never delivered; on a normal release it is cancelled here.
pub(super) fn attach(scale: &gtk::Scale, engine: &Arc<MpvEngine>) -> SeekHandle {
    let seeking = Rc::new(Cell::new(false));
    let seek_target = Rc::new(Cell::new(0.0));
    let seek_settle = Rc::new(Cell::new(0i64));
    let pending_commit: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let arm_commit = {
        let engine = engine.clone();
        let (seeking, seek_target, seek_settle, pending_commit) = (
            seeking.clone(),
            seek_target.clone(),
            seek_settle.clone(),
            pending_commit.clone(),
        );
        Rc::new(move || {
            if let Some(id) = pending_commit.borrow_mut().take() {
                id.remove();
            }
            let (engine, seeking, seek_target, seek_settle) = (
                engine.clone(),
                seeking.clone(),
                seek_target.clone(),
                seek_settle.clone(),
            );
            let pending_inner = pending_commit.clone();
            let id = glib::timeout_add_local_once(
                Duration::from_millis(SEEK_COMMIT_MS),
                move || {
                    *pending_inner.borrow_mut() = None;
                    if seeking.get() {
                        seeking.set(false);
                        seek_settle.set(now_ms() + SEEK_SETTLE_MS);
                        if let Err(err) = engine.seek(seek_target.get()) {
                            tracing::warn!(%err, "seek failed");
                        }
                    }
                },
            );
            *pending_commit.borrow_mut() = Some(id);
        })
    };
    {
        let (seeking, seek_target) = (seeking.clone(), seek_target.clone());
        let arm_commit = arm_commit.clone();
        scale.connect_change_value(move |_scale, _scroll, value| {
            seeking.set(true);
            seek_target.set(value);
            arm_commit();
            glib::Propagation::Proceed
        });
    }
    {
        let engine = engine.clone();
        let slider = scale.clone();
        let (seeking, seek_target, seek_settle) =
            (seeking.clone(), seek_target.clone(), seek_settle.clone());
        let (arm_commit, pending_commit) = (arm_commit.clone(), pending_commit.clone());
        let press = gtk::EventControllerLegacy::new();
        press.connect_event(move |_ctrl, event| {
            match event.event_type() {
                gtk::gdk::EventType::ButtonPress => {
                    if let Some(button) = event.downcast_ref::<gtk::gdk::ButtonEvent>()
                        && button.button() == 1
                        && let Some((sx, sy)) = event.position()
                    {
                        // `event.position()` is *surface*-relative, but the
                        // slider sits offset inside the window (after the
                        // transport buttons and the time label). Using the raw
                        // x made the fraction too large and clamped to 1.0 —
                        // the knob "teleported to the end". Convert the point
                        // into the slider's own coordinate space first.
                        let local_x = slider.root().and_then(|root| {
                            let src = gtk::graphene::Point::new(sx as f32, sy as f32);
                            root.compute_point(&slider, &src).map(|p| f64::from(p.x()))
                        });
                        let Some(x) = local_x else {
                            return glib::Propagation::Proceed;
                        };
                        let Some(fraction) = click_fraction(
                            x,
                            f64::from(slider.width()),
                            slider.direction() == gtk::TextDirection::Rtl,
                        ) else {
                            return glib::Propagation::Proceed;
                        };
                        let value = slider.adjustment().upper() * fraction;
                        seeking.set(true);
                        seek_target.set(value);
                        slider.set_value(value);
                        seek_settle.set(now_ms() + SEEK_SETTLE_MS);
                        if let Err(err) = engine.seek(value) {
                            tracing::warn!(%err, "seek failed");
                        }
                        arm_commit();
                    }
                }
                gtk::gdk::EventType::ButtonRelease => {
                    if let Some(button) = event.downcast_ref::<gtk::gdk::ButtonEvent>()
                        && button.button() == 1
                        && seeking.get()
                    {
                        seeking.set(false);
                        if let Some(id) = pending_commit.borrow_mut().take() {
                            id.remove();
                        }
                        seek_settle.set(now_ms() + SEEK_SETTLE_MS);
                        if let Err(err) = engine.seek(seek_target.get()) {
                            tracing::warn!(%err, "seek failed");
                        }
                    }
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        scale.add_controller(press);
    }
    SeekHandle { seeking, seek_settle }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1000 px slider: the knob centre can travel 19..=981, so the midpoint
    // of the track is x = 19 + 962/2 = 500.
    #[test]
    fn click_to_position_maps_the_track_between_the_knob_stops() {
        assert_eq!(click_fraction(19.0, 1000.0, false), Some(0.0));
        assert_eq!(click_fraction(500.0, 1000.0, false), Some(0.5));
        assert_eq!(click_fraction(981.0, 1000.0, false), Some(1.0));
        // Presses past the stops clamp instead of running off the adjustment.
        assert_eq!(click_fraction(0.0, 1000.0, false), Some(0.0));
        assert_eq!(click_fraction(1200.0, 1000.0, false), Some(1.0));
    }

    #[test]
    fn an_rtl_slider_seeks_from_the_right_edge() {
        assert_eq!(click_fraction(981.0, 1000.0, true), Some(0.0));
        assert_eq!(click_fraction(500.0, 1000.0, true), Some(0.5));
        assert_eq!(click_fraction(19.0, 1000.0, true), Some(1.0));
    }

    #[test]
    fn a_slider_narrower_than_its_knob_is_not_mapped() {
        assert_eq!(click_fraction(10.0, 2.0 * KNOB_HALF_PX, false), None);
        assert_eq!(click_fraction(10.0, 0.0, true), None);
    }
}
