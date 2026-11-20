//! Volume via the mouse wheel, and the on-screen volume indicator.
//!
//! The wheel steps the volume through the same `app.volume-rel` action the
//! Up/Down keys use — one clamp, one path to mpv; the wheel's own step is
//! coarser (`WHEEL_VOLUME_STEP`). A notch accumulator turns wheel notches
//! (dy = ±1) and the touchpad's fractional stream into whole steps, and
//! drops the remainder once a gesture has ended.
//!
//! The right-edge indicator is an OSD that can also be dragged. The widget
//! that moved the volume tells the OSD directly, from the change site; the
//! event pump calls `show` too, as a backstop for changes that did not
//! start here (a config, a script, another mpv client) — the change
//! detector makes those extra calls free. Polling alone is what made the
//! OSD lag: the folded state reaches the pump up to 100 ms late, and each
//! change eased over a tween that re-anchored at that stale pace, so a
//! fast wheel burst showed up as a fill chasing the readout. Now changes
//! inside one gesture move the fill 1:1 (`BURST_MS`) and only a lone
//! change eases. The OSD fades on its own schedule, independent of the
//! controls' idle-hide (mpv's behaviour), except while the pointer is on
//! the capsule: an OSD you cannot reach is not a control.

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
/// Two changes this close are one gesture: the fill moves to the value at
/// once. Easing a live gesture is latency — a tween re-anchored mid-flight
/// trails the input for its whole duration.
const BURST_MS: i64 = 250;
/// The ease for a lone change (a key step, `m`, a value arriving from mpv).
/// 250 ms read as laggy against the wheel's pace; 150 ms still reads as
/// motion, not a jump-cut.
const ANIM_MS: i64 = 150;
const ANIM_TICK_MS: u64 = 16;
/// Scroll this long after the previous one starts a new gesture: the
/// accumulator drops its remainder instead of letting an old touchpad
/// half-notch add a phantom notch to an unrelated flick.
const GESTURE_GAP_MS: i64 = 300;
/// Per wheel notch — the coarse gesture. The keys keep the fine 5%.
const WHEEL_VOLUME_STEP: i32 = 10;
/// Bar size. A range defaults to halign fill and stretches to the box, so the
/// size is pinned here and the bar is centered. Slimmer than the old 30px
/// slab, but a real bar: 12px measured as too thin over video.
const BAR_WIDTH: i32 = 18;
const BAR_HEIGHT: i32 = 140;

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

/// `scroll_accumulate` with the gesture boundary: scroll landing more than
/// `GESTURE_GAP_MS` after `last_ms` starts from a clean accumulator.
/// Returns the new accumulator, the new `last_ms`, and the whole steps.
fn scroll_step(acc: f64, last_ms: i64, dy: f64, now_ms: i64) -> (f64, i64, i32) {
    let base = if now_ms - last_ms > GESTURE_GAP_MS { 0.0 } else { acc };
    let (acc, steps) = scroll_accumulate(base, dy);
    (acc, now_ms, steps)
}

/// How the fill moves to a newly shown value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    /// Part of a gesture: the fill is simply where the input says it is.
    Snap,
    /// A lone change: ease so the eye can follow it.
    Ease,
}

/// A change within `BURST_MS` of the previous one belongs to the same
/// gesture. Pure so the policy is unit-tested.
fn fill_motion(now_ms: i64, last_change_ms: i64) -> Fill {
    if last_change_ms != 0 && now_ms - last_change_ms <= BURST_MS {
        Fill::Snap
    } else {
        Fill::Ease
    }
}

/// The fill tween's value at `elapsed_ms` from a step: cubic ease-out,
/// clamped at both ends so the last frame lands exactly on the target.
/// Pure so the curve is unit-tested.
fn tween_value(from: f64, to: f64, elapsed_ms: i64, dur: i64) -> f64 {
    let t = (elapsed_ms as f64 / dur as f64).clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t).powi(3);
    from + (to - from) * eased
}

/// The state behind the OSD's motion and fade, shared between the handle
/// and every closure that drives it.
struct Osd {
    bar: gtk::Scale,
    percent: gtk::Label,
    icon: gtk::Image,
    capsule: gtk::Box,
    revealer: gtk::Revealer,
    /// The (volume, muted) pair currently shown — the change detector that
    /// makes the pump's backstop calls free.
    shown: Cell<(f64, bool)>,
    /// When the last change reached the OSD — the burst detector.
    last_change: Cell<i64>,
    /// The pointer is on the capsule.
    hover: Cell<bool>,
    anim_id: RefCell<Option<glib::SourceId>>,
    hide_id: RefCell<Option<glib::SourceId>>,
    /// The bar's `value-changed` handler, blocked while the OSD writes the
    /// bar itself — a backstop sync or a tween is not a drag and must not
    /// re-enter the engine (api-notes).
    bar_changed: RefCell<Option<glib::SignalHandlerId>>,
}

/// The right-edge volume OSD: an `.osd` capsule (libadwaita's own
/// over-content style — dark translucent shell, white ink) holding a volume
/// bar that can be dragged. `show` acts only when the (volume, muted) pair
/// actually changed.
#[derive(Clone)]
pub(super) struct VolumeOsd {
    osd: Rc<Osd>,
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

    let capsule = gtk::Box::new(gtk::Orientation::Vertical, 12);
    capsule.add_css_class("osd");
    capsule.add_css_class("volume-osd");
    // No width request here on purpose: the readout's fixed digit slot
    // (style.css) is what keeps the capsule's width constant, and a request on
    // top of it only added slack — a 64px request measured as a 60px capsule
    // around a 31px readout.
    capsule.append(&icon);
    capsule.append(&bar);
    capsule.append(&percent);

    let revealer = gtk::Revealer::new();
    revealer.set_child(Some(&capsule));
    revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
    revealer.set_transition_duration(250);
    revealer.set_halign(gtk::Align::End);
    revealer.set_valign(gtk::Align::Center);
    revealer.set_margin_end(28);
    revealer.set_reveal_child(false);

    let osd = Rc::new(Osd {
        bar: bar.clone(),
        percent,
        icon,
        capsule,
        revealer,
        // Matches `PlaybackState::default`: the first backstop sync must not
        // flash for a volume nobody changed.
        shown: Cell::new((100.0, false)),
        last_change: Cell::new(0),
        hover: Cell::new(false),
        anim_id: RefCell::new(None),
        hide_id: RefCell::new(None),
        bar_changed: RefCell::new(None),
    });

    // The capsule, not the bar, owns the pointer hold: reaching an 18px bar
    // means aiming at the capsule, and a motion controller on a plain box never
    // competes with the range's own gesture. Without this the OSD fades out
    // from under a hand reaching for it.
    let motion = gtk::EventControllerMotion::new();
    {
        let osd = osd.clone();
        motion.connect_enter(move |_ctrl, _x, _y| {
            osd.hover.set(true);
            cancel_hide(&osd);
        });
    }
    {
        let osd = osd.clone();
        motion.connect_leave(move |_ctrl| {
            osd.hover.set(false);
            arm_hide(&osd);
        });
    }
    osd.capsule.add_controller(motion);

    let bar_changed = {
        let osd = osd.clone();
        let engine = shared.engine.clone();
        let state = shared.state.clone();
        bar.connect_value_changed(move |scale| {
            // Reaching here means the pointer moved the bar: every write the
            // OSD makes itself is signal-blocked, so an emission can only come
            // from a drag or a wheel over the bar. A gesture would be the
            // other way round — `GtkRange` claims the drag sequence for
            // itself and a competing gesture on the same widget can be
            // skipped without a word.
            let value = scale.value();
            let muted = state.borrow().muted;
            if let Err(err) = engine.set_volume(value) {
                // The bar sits where the pointer put it; the pump's backstop
                // puts it back where mpv says the volume actually is.
                tracing::warn!(%err, "set_volume failed");
                return;
            }
            // Same optimistic fold as the wheel and the popover slider: a
            // following relative step builds on the dragged value, not on the
            // observed one still on its way back.
            state.borrow_mut().request_volume(value);
            show_at_bar(&osd, value, muted);
        })
    };
    *osd.bar_changed.borrow_mut() = Some(bar_changed);

    VolumeOsd { osd }
}

impl VolumeOsd {
    pub(super) fn revealer(&self) -> &gtk::Revealer {
        &self.osd.revealer
    }

    /// Show (volume, muted) — the change sites' entry point and the pump's
    /// backstop (see the module docs and `show`).
    pub(super) fn show(&self, volume: f64, muted: bool) {
        show(&self.osd, volume, muted);
    }
}

/// Show (volume, muted): reveal if it is a change, move the fill (1:1
/// inside a gesture, eased on its own), re-arm the fade.
fn show(osd: &Rc<Osd>, volume: f64, muted: bool) {
    show_inner(osd, volume, muted, false)
}

/// The same for a change the bar already shows — a drag or a wheel over
/// it. The fill is where the pointer put it; the only thing left to do is
/// stand any running tween down, or it would write over the drag.
fn show_at_bar(osd: &Rc<Osd>, volume: f64, muted: bool) {
    show_inner(osd, volume, muted, true)
}

fn show_inner(osd: &Rc<Osd>, volume: f64, muted: bool, at_bar: bool) {
    if osd.shown.get() == (volume, muted) {
        return;
    }
    let now = now_ms();
    let motion = fill_motion(now, osd.last_change.get());
    osd.shown.set((volume, muted));
    osd.last_change.set(now);
    if at_bar {
        stop_anim(osd);
    } else if motion == Fill::Snap {
        stop_anim(osd);
        set_bar(osd, volume);
    } else {
        animate_to(osd, volume);
    }
    if muted {
        osd.capsule.add_css_class("muted");
    } else {
        osd.capsule.remove_css_class("muted");
    }
    osd.percent.set_text(&format!("{}%", volume.round()));
    osd.icon.set_icon_name(Some(volume_icon(volume, muted)));
    osd.revealer.set_reveal_child(true);
    arm_hide(osd);
}

/// Write the bar without the bar claiming the pointer moved it. Clamped so
/// a value arriving from mpv can never push the fill off its track.
fn set_bar(osd: &Rc<Osd>, value: f64) {
    let changed = osd.bar_changed.borrow();
    let changed = changed
        .as_ref()
        .expect("build wires the handler before anything shows");
    osd.bar.block_signal(changed);
    osd.bar.set_value(value.clamp(0.0, MAX_VOLUME));
    osd.bar.unblock_signal(changed);
}

fn stop_anim(osd: &Rc<Osd>) {
    if let Some(id) = osd.anim_id.borrow_mut().take() {
        id.remove();
    }
}

/// Start or re-anchor the fill ease. A mid-tween change continues from
/// where the bar is, not from the old target.
fn animate_to(osd: &Rc<Osd>, target: f64) {
    stop_anim(osd);
    let from = osd.bar.value();
    let start = now_ms();
    let anim_osd = osd.clone();
    let id = glib::timeout_add_local(
        Duration::from_millis(ANIM_TICK_MS),
        move || -> glib::ControlFlow {
            // The tween moves the bar; that is a sync, not an edit.
            set_bar(&anim_osd, tween_value(from, target, now_ms() - start, ANIM_MS));
            if now_ms() - start >= ANIM_MS {
                // Break releases the closure, and with it the `Rc` back to
                // the Osd — the SourceId alone never keeps this alive.
                *anim_osd.anim_id.borrow_mut() = None;
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        },
    );
    *osd.anim_id.borrow_mut() = Some(id);
}

fn cancel_hide(osd: &Rc<Osd>) {
    if let Some(id) = osd.hide_id.borrow_mut().take() {
        id.remove();
    }
}

/// Re-arm the fade-out — unless the pointer is on the capsule, in which
/// case leaving is what arms it.
fn arm_hide(osd: &Rc<Osd>) {
    cancel_hide(osd);
    if osd.hover.get() {
        return;
    }
    let next = osd.clone();
    let id = glib::timeout_add_local_once(Duration::from_millis(OSD_HIDE_MS), move || {
        *next.hide_id.borrow_mut() = None;
        next.revealer.set_reveal_child(false);
    });
    *osd.hide_id.borrow_mut() = Some(id);
}

/// Wheel-over-window volume control. Window-level: the wheel changes the
/// volume wherever the pointer is (mpv's behaviour), except over the seek
/// and popover sliders, whose own scroll handling claims the sequence
/// first. `DISCRETE` is off so the touchpad's fractional stream also
/// reaches the accumulator.
pub(super) fn attach_scroll(window: &impl IsA<gtk::Widget>) {
    let window = window.as_ref().clone();
    let acc = Rc::new(Cell::new(0.0f64));
    let last = Rc::new(Cell::new(0i64));
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    let action_target = window.clone();
    scroll.connect_scroll(move |_ctrl, _dx, dy| {
        let (new_acc, new_last, steps) = scroll_step(acc.get(), last.get(), dy, now_ms());
        acc.set(new_acc);
        last.set(new_last);
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
    use super::{
        ANIM_MS, BURST_MS, GESTURE_GAP_MS, Fill, fill_motion, scroll_accumulate, scroll_step,
        tween_value,
    };

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
    fn a_scroll_inside_the_gap_carries_the_remainder() {
        let (acc, last, steps) = scroll_step(0.0, 0, -0.5, 1_000);
        assert_eq!((acc, steps), (-0.5, 0));
        let (acc, _, steps) = scroll_step(acc, last, -0.5, 1_000 + GESTURE_GAP_MS);
        assert_eq!((acc, steps), (0.0, 1));
    }

    #[test]
    fn a_scroll_after_the_gap_starts_from_zero() {
        // A leftover half-notch must not add a phantom notch to a new flick.
        let (acc, last, _) = scroll_step(0.0, 0, -0.5, 1_000);
        let (_, _, steps) = scroll_step(acc, last, -0.5, 1_000 + GESTURE_GAP_MS + 1);
        assert_eq!(steps, 0, "the old remainder is dropped; the new half-notch waits alone");
    }

    #[test]
    fn changes_inside_a_burst_snap_and_a_lone_change_eases() {
        assert_eq!(fill_motion(1_000, 0), Fill::Ease, "the first change eases");
        assert_eq!(fill_motion(1_000 + BURST_MS, 1_000), Fill::Snap);
        assert_eq!(fill_motion(1_000 + BURST_MS + 1, 1_000), Fill::Ease);
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
