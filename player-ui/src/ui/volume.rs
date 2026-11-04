//! Volume via the mouse wheel, and the on-screen volume indicator.
//!
//! The wheel steps the volume through the same `app.volume-rel` action the
//! Up/Down keys use: one clamp, one path to mpv (the wheel's own step is
//! coarser — `WHEEL_VOLUME_STEP`). A notch accumulator turns
//! both discrete wheel events (dy = ±1 per notch) and smooth touchpad
//! scrolling (many fractional deltas) into whole steps.
//!
//! The indicator on the right edge is an OSD, not a control: it never takes
//! input, and it appears for *any* volume change — wheel, keys, the popover
//! slider — because that is the moment people look for confirmation. It is
//! driven from `Controls::refresh`: the folded state is the one truth, so
//! the OSD can never disagree with what the engine actually did. It fades
//! out after a quiet interval, independent of the controls' own idle-hide —
//! a volume change while the controls are down still shows it (mpv's
//! behaviour, and the whole point of the gesture).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use crate::MAX_VOLUME;

use super::controls::volume_icon;
use super::now_ms;

/// How long the indicator stays up after the last volume change. Short
/// enough to never sit over the picture while watching, long enough to
/// re-read during a deliberate scroll.
const OSD_HIDE_MS: u64 = 1_200;
/// Volume percentage per wheel notch. The wheel is the coarse gesture —
/// a few notches should cross the useful range — so it steps wider than
/// the Up/Down keys' fine 5% (`VOLUME_STEP` in main.rs).
const WHEEL_VOLUME_STEP: i32 = 10;
/// The capsule's width is its own decision, not the text's: the percent
/// readout is fully detached from the layout (a label reservation still
/// let the digits nudge the box). 56 logical px fits the widest readout
/// ("130%", measured 31 px) with the bar centred inside it.
const OSD_WIDTH: i32 = 56;
/// The bar's width/length. The bar is pinned (halign center) inside the
/// fixed-width box, so these are its real size — CSS keeps colors only.
const BAR_WIDTH: i32 = 30;
const BAR_HEIGHT: i32 = 160;
/// The fill tween's duration and tick after a volume step: short enough
/// to feel instant, long enough for the eye to follow where the level
/// went (the label shows the exact number at once; the bar eases to it).
const ANIM_MS: i64 = 250;
const ANIM_TICK_MS: u32 = 16;

/// Fold one scroll event into the notch accumulator and report the whole
/// steps it produced (positive = volume up: scrolling up increases).
///
/// GTK delivers wheel notches as dy = ±1 and touchpad scrolling as a stream
/// of fractional deltas; both become a volume step only once a full notch
/// has accumulated, and the remainder carries over to the next event.
fn scroll_accumulate(acc: f64, dy: f64) -> (f64, i32) {
    let mut acc = acc + dy;
    let mut steps = 0;
    while acc >= 1.0 {
        acc -= 1.0;
        steps -= 1;
    }
    while acc <= -1.0 {
        acc += 1.0;
        steps += 1;
    }
    (acc, steps)
}

/// The fill tween's value at progress `t` (ms since the step started):
/// cubic ease-out from `from` to `to`, clamped at both ends so the last
/// frame lands exactly on the target. Pure so the curve is unit-tested.
fn tween_value(from: f64, to: f64, elapsed_ms: i64) -> f64 {
    let t = (elapsed_ms as f64 / ANIM_MS as f64).clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t).powi(3);
    from + (to - from) * eased
}

/// The right-edge volume OSD. `sync` is called with every folded state;
/// it shows only when the (volume, muted) pair actually changed.
#[derive(Clone)]
pub(super) struct VolumeOsd {
    revealer: gtk::Revealer,
    bar: gtk::LevelBar,
    percent: gtk::Label,
    icon: gtk::Image,
    /// The (volume, muted) pair currently shown — the change detector.
    shown: Rc<Cell<(f64, bool)>>,
    hide_id: Rc<RefCell<Option<glib::SourceId>>>,
    /// The running fill tween, if any (re-anchored from wherever the bar
    /// currently sits whenever a new change arrives mid-animation).
    anim_id: Rc<RefCell<Option<glib::SourceId>>>,
}

pub(super) fn build() -> VolumeOsd {
    let icon = gtk::Image::from_icon_name("audio-volume-high-symbolic");
    // LevelBar, not a Scale: it is an indicator, it never takes input.
    // The vertical orientation has no setter in the bindings — builder only.
    let bar = gtk::LevelBar::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    // A vertical LevelBar fills from the TOP by default (seen on screen:
    // 85% put the accent at the top). A volume bar must grow from the
    // bottom up — inverted=true flips it.
    bar.set_inverted(true);
    bar.set_min_value(0.0);
    bar.set_max_value(MAX_VOLUME);
    bar.add_css_class("volume-osd-bar");
    // The bar's size is code's decision (see the constants): a LevelBar
    // defaults to halign fill and would stretch to whatever the content
    // box is — that stretched bar is what "grew wider at 100%" was.
    bar.set_size_request(BAR_WIDTH, BAR_HEIGHT);
    bar.set_halign(gtk::Align::Center);
    // Match `shown`'s initial (100, false): the first change animates
    // FROM the level the OSD claims to be at, never from an empty bar.
    bar.set_value(100.0);
    let percent = gtk::Label::builder()
        .label("100%")
        .xalign(0.5)
        .build();
    percent.add_css_class("volume-osd-percent");

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 10);
    box_.add_css_class("volume-osd");
    // Fixed width: the percent readout is fully detached from the layout —
    // digits of any length paint inside this, they never move anything.
    box_.set_size_request(OSD_WIDTH, -1);
    box_.append(&icon);
    box_.append(&bar);
    box_.append(&percent);

    let revealer = gtk::Revealer::new();
    revealer.set_child(Some(&box_));
    revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
    revealer.set_transition_duration(250);
    revealer.set_halign(gtk::Align::End);
    revealer.set_valign(gtk::Align::Center);
    revealer.set_margin_end(28);
    revealer.set_reveal_child(false);

    VolumeOsd {
        revealer,
        bar,
        percent,
        icon,
        // Matches `PlaybackState::default`: the first pump refresh must not
        // flash an indicator for a volume nobody changed.
        shown: Rc::new(Cell::new((100.0, false))),
        hide_id: Rc::new(RefCell::new(None)),
        anim_id: Rc::new(RefCell::new(None)),
    }
}

impl VolumeOsd {
    pub(super) fn revealer(&self) -> &gtk::Revealer {
        &self.revealer
    }

    /// Show the indicator for (volume, muted) unless it is what is already
    /// shown, and re-arm the fade-out.
    pub(super) fn sync(&self, volume: f64, muted: bool) {
        if self.shown.get() == (volume, muted) {
            return;
        }
        self.shown.set((volume, muted));
        self.bar.set_value(self.animate_to(volume));
        self.percent.set_text(&format!("{}%", volume.round()));
        self.icon.set_icon_name(Some(volume_icon(volume, muted)));
        self.revealer.set_reveal_child(true);
        if let Some(id) = self.hide_id.borrow_mut().take() {
            id.remove();
        }
        let revealer = self.revealer.clone();
        let slot = self.hide_id.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(OSD_HIDE_MS), move || {
            *slot.borrow_mut() = None;
            revealer.set_reveal_child(false);
        });
        *self.hide_id.borrow_mut() = Some(id);
    }

    /// Start (or re-anchor) the fill tween toward `target` and return the
    /// value to paint on THIS frame — the first frame of a new tween must
    /// not wait a tick, and a mid-tween change must continue from where
    /// the bar actually is, not from the old target.
    fn animate_to(&self, target: f64) -> f64 {
        let from = self.bar.value();
        if let Some(id) = self.anim_id.borrow_mut().take() {
            id.remove();
        }
        let start = now_ms();
        let bar = self.bar.clone();
        let slot = self.anim_id.clone();
        let id = glib::timeout_add_local(
            Duration::from_millis(ANIM_TICK_MS as u64),
            move || -> glib::ControlFlow {
                let value = tween_value(from, target, now_ms() - start);
                bar.set_value(value);
                if now_ms() - start >= ANIM_MS {
                    *slot.borrow_mut() = None;
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            },
        );
        *self.anim_id.borrow_mut() = Some(id);
        tween_value(from, target, 0)
    }
}

/// Wheel-over-window volume control: a scroll controller on the window.
///
/// Window-level, not video-level: the wheel changes the volume wherever the
/// pointer is (mpv's behaviour) — except over the seek and popover sliders,
/// whose own scroll handling claims the sequence first. `DISCRETE` is left
/// off on purpose: without it the controller receives wheel notches *and*
/// the touchpad's fractional stream, which feeds the same accumulator.
pub(super) fn attach_scroll(window: &impl IsA<gtk::Widget>) {
    let window = window.as_ref().clone();
    let acc = Rc::new(Cell::new(0.0f64));
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    let action_target = window.clone();
    scroll.connect_scroll(move |_ctrl, _dx, dy| {
        let (new_acc, steps) = scroll_accumulate(acc.get(), dy);
        acc.set(new_acc);
        if steps != 0 {
            let delta = steps * WHEEL_VOLUME_STEP;
            if let Err(err) = action_target.activate_action("app.volume-rel", Some(&delta.to_variant()))
            {
                tracing::warn!(%err, "volume-rel action unavailable");
            }
        }
        glib::Propagation::Stop
    });
    window.add_controller(scroll);
}

#[cfg(test)]
mod tests {
    use super::{ANIM_MS, scroll_accumulate, tween_value};

    #[test]
    fn one_wheel_notch_steps_once() {
        // dy < 0 is scroll up: volume up.
        assert_eq!(scroll_accumulate(0.0, -1.0), (0.0, 1));
        assert_eq!(scroll_accumulate(0.0, 1.0), (0.0, -1));
    }

    #[test]
    fn smooth_halves_accumulate_into_one_step() {
        let (acc, steps) = scroll_accumulate(0.0, -0.5);
        assert_eq!((acc, steps), (-0.5, 0));
        let (acc, steps) = scroll_accumulate(acc, -0.5);
        assert_eq!((acc, steps), (0.0, 1));
    }

    #[test]
    fn a_fast_wheel_burst_steps_per_notch_and_keeps_the_remainder() {
        let (acc, steps) = scroll_accumulate(0.0, -2.75);
        assert_eq!((acc, steps), (-0.75, 2));
    }

    #[test]
    fn opposite_scroll_cancels_before_it_steps() {
        let (acc, steps) = scroll_accumulate(0.0, -0.5);
        assert_eq!((acc, steps), (-0.5, 0));
        let (acc, steps) = scroll_accumulate(acc, 0.3);
        assert_eq!((acc, steps), (-0.2, 0));
        // 0.7 of a downward notch accumulated — not a whole one yet.
        let (acc, steps) = scroll_accumulate(acc, 0.9);
        assert_eq!((acc, steps), (0.7, 0));
        let (acc, steps) = scroll_accumulate(acc, 0.3);
        assert_eq!((acc, steps), (0.0, -1));
    }

    #[test]
    fn the_tween_starts_at_the_current_level_and_lands_on_the_target() {
        assert_eq!(tween_value(100.0, 80.0, 0), 100.0);
        assert_eq!(tween_value(100.0, 80.0, ANIM_MS), 80.0);
        // Clamped at both ends: the last frame never overshoots, and a
        // late tick keeps reporting the target, not past it.
        assert_eq!(tween_value(100.0, 80.0, ANIM_MS * 3), 80.0);
    }

    #[test]
    fn the_tween_eases_out_moves_fast_first_and_covers_most_of_the_step_early() {
        let mid = tween_value(100.0, 80.0, ANIM_MS / 2);
        // Cubic ease-out: past the halfway point of the range at the
        // halfway point of the time.
        assert!(mid < 90.0 && mid > 80.0, "mid value {mid} must be past halfway");
        // And monotone toward the target across the whole run.
        let mut prev = 100.0;
        for t in (0..=ANIM_MS).step_by(16) {
            let v = tween_value(100.0, 80.0, t);
            assert!(v <= prev && v >= 80.0);
            prev = v;
        }
    }
}
