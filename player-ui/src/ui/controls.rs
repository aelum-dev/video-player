//! The widgets the event pump keeps in sync, plus the idle-hide state
//! machine that fades the controls over the video.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use player_core::{Chapter, PlaybackState};

use crate::format;

use super::now_ms;

/// Hide the controls this long after the last pointer motion. mpv's default is
/// ~1 s; a little more so reaching for a button does not hide them.
const IDLE_HIDE_MS: i64 = 2500;
/// How often the idle check runs (cheap: a few `Cell` reads).
pub(super) const IDLE_TICK_MS: u64 = 500;

/// The widgets the event pump has to keep in sync, plus the idle-hide state.
#[derive(Clone)]
pub struct Controls {
    pub(super) header: gtk::Revealer,
    pub(super) playbar: gtk::Revealer,
    pub(super) title: gtk::Label,
    pub(super) seek: gtk::Scale,
    pub(super) time_label: gtk::Label,
    pub(super) duration_label: gtk::Label,
    pub(super) volume_button: gtk::MenuButton,
    pub(super) volume_scale: gtk::Scale,
    pub(super) speed_button: gtk::MenuButton,
    pub(super) speed_scale: gtk::Scale,
    pub(super) fullscreen_button: gtk::Button,
    /// Big play/pause button (icon only, no background) floating in the
    /// middle of the video;
    /// revealed whenever the controls are.
    pub(super) center: gtk::Revealer,
    pub(super) center_button: gtk::Button,
    /// True while the user owns the seek slider (drag or arrows): the pump must
    /// not fight them for it.
    pub(super) seeking: Rc<Cell<bool>>,
    /// Monotonic-ms deadline until which the pump must keep its hands off the
    /// seek knob *after* a seek is committed. mpv's seek is async, so without
    /// this the pump would snap the knob back to the stale position for a frame
    /// or two before the seek lands.
    pub(super) seek_settle: Rc<Cell<i64>>,
    /// Pointer is over one of the control bars, or a popover is open.
    pub(super) hover: Rc<Cell<bool>>,
    /// A file drag is over the window. While set, the controls stay hidden so
    /// the drop hint has the video to itself (see `set_drag_active`).
    pub(super) dragging: Rc<Cell<bool>>,
    pub(super) revealed: Rc<Cell<bool>>,
    pub(super) last_motion: Rc<Cell<i64>>,
    /// Last pointer position. Wayland re-sends the (unchanged) pointer
    /// position as a motion event after every surface commit — with a
    /// constantly repainting GLArea that is several times a second. Only a
    /// *changed* position is real user activity; otherwise the idle timer
    /// would never expire while the pointer is inside the window.
    pub(super) last_pos: Rc<Cell<(f64, f64)>>,
}

impl Controls {
    /// Icon for the play/pause buttons: the action the click performs —
    /// pause bars while playing, play triangle while paused.
    pub(super) fn play_icon(paused: bool) -> &'static str {
        if paused {
            "media-playback-start-symbolic"
        } else {
            "media-playback-pause-symbolic"
        }
    }

    pub(super) fn set_revealed(&self, on: bool) {
        // While a file drag is over the window the controls are forced down:
        // the drop hint owns the screen. `set_drag_active(false)` brings them
        // back, so any `poke` from the hover/motion controllers during the drag
        // is ignored here rather than fighting the hide.
        let on = on && !self.dragging.get();
        if self.revealed.get() == on {
            return;
        }
        self.revealed.set(on);
        self.header.set_reveal_child(on);
        self.playbar.set_reveal_child(on);
        // The center play/pause button rides with the controls: it is there
        // whenever the user is interacting (and stays while paused), and fades
        // out with everything else on idle.
        self.center.set_reveal_child(on);
    }

    /// A file drag entered (`on`) or left (`off`) the window. Hide the controls
    /// for the duration so the drop hint is the only thing over the video, and
    /// bring them back the moment the drag is over.
    pub(super) fn set_drag_active(&self, on: bool) {
        if self.dragging.get() == on {
            return;
        }
        self.dragging.set(on);
        if on {
            self.set_revealed(false);
        } else {
            self.poke();
        }
    }

    /// Pointer activity: show the controls and restart the idle window.
    pub(super) fn poke(&self) {
        self.last_motion.set(now_ms());
        self.set_revealed(true);
    }

    fn busy(&self) -> bool {
        self.hover.get()
            || self.volume_button.is_active()
            || self.speed_button.is_active()
    }

    /// Runs on the idle tick: fade out when the pointer has been still for a
    /// while and nothing is being interacted with. Stays up while paused —
    /// that is when people reach for the controls, and at startup (nothing
    /// loaded) the play button is the one thing the window is offering.
    pub(super) fn idle_tick(&self, state: &PlaybackState) {
        if !self.revealed.get() || self.busy() || state.paused {
            return;
        }
        if now_ms() - self.last_motion.get() < IDLE_HIDE_MS {
            return;
        }
        self.set_revealed(false);
    }

    /// Draw chapter ticks on the seek slider.
    pub fn set_chapters(&self, chapters: &[Chapter]) {
        self.seek.clear_marks();
        for chapter in chapters {
            if chapter.time > 0.0 {
                // Ticks only: chapter titles as mark labels would clutter.
                self.seek.add_mark(chapter.time, gtk::PositionType::Bottom, None);
            }
        }
    }

    /// Sync every control from the folded playback state.
    pub fn refresh(&self, state: &PlaybackState) {
        // The center button mirrors the play state with its icon; its
        // visibility follows the controls (see `set_revealed`).
        self.center_button.set_icon_name(Self::play_icon(state.paused));
        self.time_label
            .set_text(&format::clock(state.position));
        self.duration_label
            .set_text(&format::clock(state.duration));
        self.title
            .set_text(state.title().unwrap_or_else(|| "Video player".to_owned()).as_str());
        self.volume_button
            .set_icon_name(volume_icon(state.volume, state.muted));
        self.speed_button.set_label(&format!("{:.2}×", state.speed));

        if !self.seeking.get() && now_ms() >= self.seek_settle.get() {
            let duration = state.duration.max(0.0);
            self.seek.set_range(0.0, duration);
            self.seek.set_value(state.position.clamp(0.0, duration));
        }
        // While a popover is open the user may be dragging its slider.
        if !self.volume_button.is_active() {
            let volume = state.volume.clamp(0.0, 100.0);
            if (self.volume_scale.value() - volume).abs() > f64::EPSILON {
                self.volume_scale.set_value(volume);
            }
        }
        if !self.speed_button.is_active() && (self.speed_scale.value() - state.speed).abs() > 0.001 {
            self.speed_scale.set_value(state.speed);
        }
    }
}

fn volume_icon(volume: f64, muted: bool) -> &'static str {
    if muted || volume <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if volume < 34.0 {
        "audio-volume-low-symbolic"
    } else if volume < 67.0 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    }
}
