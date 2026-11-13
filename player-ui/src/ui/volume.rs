//! Volume via the mouse wheel, and the on-screen volume indicator.
//!
//! The wheel steps the volume through the same `app.volume-rel` action the
//! Up/Down keys use — one clamp, one path to mpv; the wheel's own step is
//! coarser (`WHEEL_VOLUME_STEP`). A notch accumulator turns wheel notches
//! (dy = ±1) and the touchpad's fractional stream into whole steps.
//!
//! The right-edge indicator is an OSD that can also be dragged: it appears for
//! *any* volume change and is driven from `Controls::refresh` — the folded
//! state is the one truth, so it can never disagree with the engine. It fades
//! on its own schedule, independent of the controls' idle-hide (mpv's
//! behaviour), except while the pointer is on the capsule: an OSD you cannot
//! reach is not a control.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use player_core::PlaybackEngine as _;

use crate::MAX_VOLUME;
use crate::Shared;

use super::controls::volume_icon;
use super::now_ms;

/// How long the indicator stays up after the last volume change.
const OSD_HIDE_MS: u64 = 1_200;
/// How long an edit owns the bar after the last change reaches it from the
/// pointer: the fill tween stays out of the way for the rest of the gesture.
const EDIT_HOLD_MS: u64 = 400;
/// Per wheel notch — the coarse gesture. The keys keep the fine 5%.
const WHEEL_VOLUME_STEP: i32 = 10;
/// Bar size. A range defaults to halign fill and stretches to the box, so the
/// size is pinned here and the bar is centered. Slimmer than the old 30px
/// slab, but a real bar: 12px measured as too thin over video.
const BAR_WIDTH: i32 = 18;
const BAR_HEIGHT: i32 = 140;
/// The fill of a Scale follows its value, so the tween is the value writes;
/// `ANIM_MS` is the ease duration. 250 ms read as laggy against the wheel's
/// pace; 150 ms still reads as motion, not a jump-cut.
const ANIM_MS: i64 = 150;
const ANIM_TICK_MS: u32 = 16;

/// Fold one scroll event into the notch accumulator and report the whole
/// steps produced (positive = volume up). A step fires only once a full
/// notch has accumulated; the remainder carries over.
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

/// The fill tween's value at `elapsed_ms` from a step: cubic ease-out,
/// clamped at both ends so the last frame lands exactly on the target.
/// Pure so the curve is unit-tested.
fn tween_value(from: f64, to: f64, elapsed_ms: i64, dur: i64) -> f64 {
    let t = (elapsed_ms as f64 / dur as f64).clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t).powi(3);
    from + (to - from) * eased
}

/// Who owns the bar right now, and what that means for the fade and the tween.
///
/// `value-changed` is the only signal that says "the pointer moved this bar":
/// every write the OSD makes itself is signal-blocked, so an emission can only
/// come from a drag or a wheel over the bar. A gesture would be the other way
/// round — `GtkRange` claims the drag sequence for itself and a competing
/// gesture on the same widget can be skipped without a word, which is how a
/// tween ended up running against the drag (the flicker).
#[derive(Clone)]
struct Edit {
    /// A change came from the pointer: the fill tween stands down and the
    /// readout follows the bar, not the folded state one pump tick behind it.
    editing: Rc<Cell<bool>>,
    /// The pointer is on the capsule.
    hover: Rc<Cell<bool>>,
    hide_id: Rc<RefCell<Option<glib::SourceId>>>,
    edit_id: Rc<RefCell<Option<glib::SourceId>>>,
    anim_id: Rc<RefCell<Option<glib::SourceId>>>,
    revealer: gtk::Revealer,
}

impl Edit {
    /// Called for every change that reached the bar from the pointer.
    fn note(&self) {
        self.editing.set(true);
        self.cancel_hide();
        if let Some(id) = self.edit_id.borrow_mut().take() {
            id.remove();
        }
        // A tween already running would write over the drag.
        if let Some(id) = self.anim_id.borrow_mut().take() {
            id.remove();
        }
        let next = self.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(EDIT_HOLD_MS), move || {
            *next.edit_id.borrow_mut() = None;
            next.editing.set(false);
            next.arm_hide();
        });
        *self.edit_id.borrow_mut() = Some(id);
    }

    /// True while the bar answers to the pointer rather than to `sync`.
    fn is_editing(&self) -> bool {
        self.editing.get()
    }

    fn cancel_hide(&self) {
        if let Some(id) = self.hide_id.borrow_mut().take() {
            id.remove();
        }
    }

    /// Re-arm the fade-out — unless the pointer is on the capsule or an edit is
    /// in flight, in which case the OSD stays up until that ends.
    fn arm_hide(&self) {
        self.cancel_hide();
        if self.hover.get() || self.editing.get() {
            return;
        }
        let next = self.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(OSD_HIDE_MS), move || {
            *next.hide_id.borrow_mut() = None;
            next.revealer.set_reveal_child(false);
        });
        *self.hide_id.borrow_mut() = Some(id);
    }
}

/// The right-edge volume OSD: an `.osd` capsule (libadwaita's own
/// over-content style — dark translucent shell, white ink) holding a volume bar
/// that can be dragged. `sync` shows only when the (volume, muted) pair
/// actually changed.
#[derive(Clone)]
pub(super) struct VolumeOsd {
    edit: Edit,
    bar: gtk::Scale,
    percent: gtk::Label,
    icon: gtk::Image,
    /// The (volume, muted) pair currently shown — the change detector.
    shown: Rc<Cell<(f64, bool)>>,
    /// The bar's `value-changed` handler, blocked while the OSD writes the bar
    /// itself — the fill tween is a sync, not an edit, and must not re-enter
    /// the engine. `Rc`: `VolumeOsd` is `Clone`, `SignalHandlerId` is not.
    bar_changed: Rc<gtk::glib::SignalHandlerId>,
}

pub(super) fn build(shared: &Shared) -> VolumeOsd {
    let icon = gtk::Image::from_icon_name("audio-volume-high-symbolic");
    icon.set_halign(gtk::Align::Center);
    // A Scale as the bar, not a LevelBar. A continuous LevelBar is drawn as two
    // sibling rectangles (`block.filled` + `block.empty`) and GTK's CSS never
    // clips a node to its parent's radius, so the fill's straight edge lands on
    // top of the track's rounded ends and the track goes square at 0.
    // `scale > trough > highlight` is the one fill the theme rounds to match its
    // own track. The keyboard stays with the app's Up/Down accels, and the
    // accessible role is the meter this is, not a slider.
    let bar = gtk::Scale::builder()
        .orientation(gtk::Orientation::Vertical)
        // The initial value matches `shown`'s: the first change animates from
        // the level claimed, not from an empty bar. The step is the wheel's own
        // notch, so a wheel over the bar steps like a wheel anywhere else.
        .adjustment(&gtk::Adjustment::new(
            100.0,
            0.0,
            MAX_VOLUME,
            WHEEL_VOLUME_STEP as f64,
            WHEEL_VOLUME_STEP as f64,
            0.0,
        ))
        // A vertical Scale fills top-down by default; a volume bar grows
        // bottom-up.
        .inverted(true)
        .draw_value(false)
        .focusable(false)
        .accessible_role(gtk::AccessibleRole::Meter)
        .build();
    // A drag lands on whole percents, not on the adjustment's step.
    bar.set_round_digits(0);
    bar.set_size_request(BAR_WIDTH, BAR_HEIGHT);
    bar.set_halign(gtk::Align::Center);
    let percent = gtk::Label::builder()
        .label("100%")
        // Odometer, not centering: the slot is the width of the widest
        // readout (CSS min-width fits "130%") and the text is right-aligned
        // in it, so the "%" and every digit column stay pinned when the
        // digit count changes — centering makes the digits drift ±half a
        // glyph at every 2↔3-digit transition (the user's "digits drift").
        .xalign(1.0)
        .halign(gtk::Align::Center)
        .build();

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 12);
    box_.add_css_class("osd");
    box_.add_css_class("volume-osd");
    // No width request here on purpose: the readout's fixed digit slot
    // (style.css) is what keeps the capsule's width constant, and a request on
    // top of it only added slack — a 64px request measured as a 60px capsule
    // around a 31px readout.
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

    let edit = Edit {
        editing: Rc::new(Cell::new(false)),
        hover: Rc::new(Cell::new(false)),
        hide_id: Rc::new(RefCell::new(None)),
        edit_id: Rc::new(RefCell::new(None)),
        anim_id: Rc::new(RefCell::new(None)),
        revealer,
    };

    // The capsule, not the bar, owns the pointer hold: reaching an 18px bar
    // means aiming at the capsule, and a motion controller on a plain box never
    // competes with the range's own gesture. Without this the OSD fades out from
    // under a hand reaching for it.
    let motion = gtk::EventControllerMotion::new();
    {
        let edit = edit.clone();
        motion.connect_enter(move |_ctrl, _x, _y| {
            edit.hover.set(true);
            edit.cancel_hide();
        });
    }
    {
        let edit = edit.clone();
        motion.connect_leave(move |_ctrl| {
            edit.hover.set(false);
            edit.arm_hide();
        });
    }
    box_.add_controller(motion);

    let bar_changed = {
        let engine = shared.engine.clone();
        let state = shared.state.clone();
        let edit = edit.clone();
        bar.connect_value_changed(move |scale| {
            // Reaching here means the pointer moved the bar: the OSD's own
            // writes are signal-blocked. Stand the tween down first, so it
            // never writes against a drag.
            edit.note();
            let value = scale.value();
            if let Err(err) = engine.set_volume(value) {
                tracing::warn!(%err, "set_volume failed");
            } else {
                // Same optimistic fold as the wheel and the popover slider: a
                // following relative step builds on the dragged value, not on
                // the observed one still on its way back.
                state.borrow_mut().request_volume(value);
            }
        })
    };

    VolumeOsd {
        edit,
        bar,
        percent,
        icon,
        // Matches `PlaybackState::default`: the first pump refresh must not
        // flash for a volume nobody changed.
        shown: Rc::new(Cell::new((100.0, false))),
        bar_changed: Rc::new(bar_changed),
    }
}

impl VolumeOsd {
    pub(super) fn revealer(&self) -> &gtk::Revealer {
        &self.edit.revealer
    }

    /// Show the indicator for (volume, muted) unless already shown, and
    /// re-arm the fade-out.
    pub(super) fn sync(&self, volume: f64, muted: bool) {
        // While the pointer owns the bar, the bar is the truth: the folded
        // state is an event-pump tick behind it, and painting that lag over the
        // user's position is what made the bar flicker under the pointer.
        let volume = if self.edit.is_editing() {
            self.bar.value()
        } else {
            volume
        };
        if self.shown.get() == (volume, muted) {
            return;
        }
        self.shown.set((volume, muted));
        if !self.edit.is_editing() {
            self.animate_to(volume);
        }
        self.percent.set_text(&format!("{}%", volume.round()));
        self.icon.set_icon_name(Some(volume_icon(volume, muted)));
        self.edit.revealer.set_reveal_child(true);
        self.edit.arm_hide();
    }

    /// Start or re-anchor the fill tween. A mid-tween change continues from
    /// where the bar is, not from the old target.
    fn animate_to(&self, target: f64) {
        let from = self.bar.value();
        if let Some(id) = self.edit.anim_id.borrow_mut().take() {
            id.remove();
        }
        let start = now_ms();
        let bar = self.bar.clone();
        let changed = self.bar_changed.clone();
        let slot = self.edit.anim_id.clone();
        let id = glib::timeout_add_local(
            Duration::from_millis(ANIM_TICK_MS as u64),
            move || -> glib::ControlFlow {
                // The tween moves the bar; that is a sync, not an edit.
                bar.block_signal(&changed);
                bar.set_value(tween_value(from, target, now_ms() - start, ANIM_MS));
                bar.unblock_signal(&changed);
                if now_ms() - start >= ANIM_MS {
                    *slot.borrow_mut() = None;
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            },
        );
        *self.edit.anim_id.borrow_mut() = Some(id);
    }
}

/// Wheel-over-window volume control. Window-level: the wheel changes the
/// volume wherever the pointer is (mpv's behaviour), except over the seek
/// and popover sliders, whose own scroll handling claims the sequence
/// first. `DISCRETE` is off so the touchpad's fractional stream also
/// reaches the accumulator.
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
        assert_eq!(tween_value(100.0, 80.0, 0, ANIM_MS), 100.0);
        assert_eq!(tween_value(100.0, 80.0, ANIM_MS, ANIM_MS), 80.0);
        // Clamped at both ends: the last frame never overshoots, and a
        // late tick keeps reporting the target, not past it.
        assert_eq!(tween_value(100.0, 80.0, ANIM_MS * 3, ANIM_MS), 80.0);
    }

    #[test]
    fn the_tween_eases_out_moves_fast_first_and_covers_most_of_the_step_early() {
        let mid = tween_value(100.0, 80.0, ANIM_MS / 2, ANIM_MS);
        // Cubic ease-out: past the halfway point of the range at the
        // halfway point of the time.
        assert!(mid < 90.0 && mid > 80.0, "mid value {mid} must be past halfway");
        // And monotone toward the target across the whole run.
        let mut prev = 100.0;
        for t in (0..=ANIM_MS).step_by(16) {
            let v = tween_value(100.0, 80.0, t, ANIM_MS);
            assert!(v <= prev && v >= 80.0);
            prev = v;
        }
    }
}
