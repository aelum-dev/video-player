//! The widgets the event pump keeps in sync, the windowed/fullscreen mode
//! switch, and the idle-hide state machine that fades the interface while the
//! pointer is idle — the chrome bars while windowed, the floating bar while
//! fullscreen.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use libadwaita as adw;
use player_core::{Chapter, PlaybackState};

use crate::format;

use super::now_ms;
use super::volume::VolumeOsd;

/// Hide the controls this long after the last pointer motion. mpv's default is
/// ~1 s; a little more so reaching for a button does not hide them.
const IDLE_HIDE_MS: i64 = 2500;
/// How often the idle check runs (cheap: a few `Cell` reads).
pub(super) const IDLE_TICK_MS: u64 = 500;

/// The widgets the event pump has to keep in sync, plus the mode and
/// idle-hide state.
#[derive(Clone)]
pub struct Controls {
    /// The chrome framing the video. In fullscreen its top bar (the header)
    /// retracts and the playback bar leaves its bottom slot.
    pub(super) toolbar: adw::ToolbarView,
    /// The playback bar itself — one set of widgets, parented to the
    /// toolbar while windowed and to `fs_playbar` while fullscreen.
    pub(super) playbar: gtk::Box,
    /// The bar's fullscreen parent: an empty revealer while windowed.
    pub(super) fs_playbar: gtk::Revealer,
    /// The bar's buttons — their style class flips with the mode
    /// (`.flat` on chrome, `.osd` over video). Menu buttons are listed as
    /// wrappers: the class belongs on the `menubutton` node (api-notes).
    pub(super) bar_buttons: Vec<gtk::Widget>,
    pub(super) play_button: gtk::Button,
    pub(super) seek: gtk::Scale,
    pub(super) time_label: gtk::Label,
    pub(super) duration_label: gtk::Label,
    pub(super) volume_button: gtk::MenuButton,
    pub(super) volume_scale: gtk::Scale,
    /// The slider's `value-changed` handler, blocked while `refresh`
    /// syncs the slider — a sync is not an edit and must not re-enter
    /// the engine (and the volume can pass 100 while the slider stops
    /// there, so the sync value is deliberately not the state value).
    /// `Rc`: `Controls` is `Clone`, `SignalHandlerId` is not.
    pub(super) volume_scale_changed: Rc<gtk::glib::SignalHandlerId>,
    pub(super) speed_button: gtk::MenuButton,
    pub(super) speed_scale: gtk::Scale,
    pub(super) fullscreen_button: gtk::Button,
    /// Big play/pause button (`.osd .circular`) floating in the middle of
    /// the video; `refresh` owns its visibility (see there).
    pub(super) center: gtk::Revealer,
    pub(super) center_button: gtk::Button,
    /// Right-edge volume OSD: shown from `refresh` whenever the folded
    /// volume changes, whatever moved it (wheel, keys, the popover slider).
    pub(super) volume_osd: VolumeOsd,
    /// True while the user owns the seek slider (drag or arrows): the pump must
    /// not fight them for it.
    pub(super) seeking: Rc<Cell<bool>>,
    /// Monotonic-ms deadline until which the pump must keep its hands off the
    /// seek knob *after* a seek is committed. mpv's seek is async, so without
    /// this the pump would snap the knob back to the stale position for a frame
    /// or two before the seek lands.
    pub(super) seek_settle: Rc<Cell<i64>>,
    /// Pointer is over one of the floating controls, or a popover is open.
    pub(super) hover: Rc<Cell<bool>>,
    /// A file drag is over the window. While set, the controls stay hidden so
    /// the drop hint has the video to itself (see `set_drag_active`).
    pub(super) dragging: Rc<Cell<bool>>,
    pub(super) revealed: Rc<Cell<bool>>,
    /// True while the window is fullscreen: the playback bar floats over the
    /// video instead of framing it. The idle-hide applies in both modes — what
    /// differs is which widgets carry the interface (see `apply_revealed`).
    pub(super) fullscreen: Rc<Cell<bool>>,
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

    /// Enter (`on`) or leave fullscreen. The header retracts and the
    /// playback bar moves between the toolbar's bottom slot (chrome) and
    /// the overlay revealer (an `.osd` pill over the video) — one set of
    /// widgets, so the event pump, the seek wiring and the popovers never
    /// notice. The `.flat` ↔ `.osd` flip is what makes the same buttons
    /// read as chrome or as over-content controls; both are theme styles.
    pub(super) fn set_fullscreen(&self, on: bool) {
        if self.fullscreen.get() == on {
            return;
        }
        self.fullscreen.set(on);
        if on {
            self.toolbar.remove(&self.playbar);
            self.playbar.remove_css_class("toolbar");
            self.playbar.add_css_class("osd");
            self.playbar.add_css_class("floating-bar");
            for button in &self.bar_buttons {
                button.remove_css_class("flat");
                button.add_css_class("osd");
            }
            self.fs_playbar.set_child(Some(&self.playbar));
        } else {
            self.fs_playbar.set_child(None::<&gtk::Widget>);
            self.playbar.remove_css_class("osd");
            self.playbar.remove_css_class("floating-bar");
            self.playbar.add_css_class("toolbar");
            for button in &self.bar_buttons {
                button.remove_css_class("osd");
                button.add_css_class("flat");
            }
            self.toolbar.add_bottom_bar(&self.playbar);
        }
        // A mode change is user activity, and the new parent starts untouched:
        // bring the interface up and push the state onto the widgets this mode
        // uses. `poke` alone would skip it — the flag itself did not change.
        self.poke();
        self.apply_revealed();
    }

    /// Push the current reveal state onto the widgets that carry the interface
    /// in this mode: both chrome bars while windowed, the floating bar (with
    /// the header kept retracted) while fullscreen.
    fn apply_revealed(&self) {
        let on = self.revealed.get() && !self.dragging.get();
        if self.fullscreen.get() {
            self.toolbar.set_reveal_top_bars(false);
            self.fs_playbar.set_reveal_child(on);
        } else {
            self.toolbar.set_reveal_top_bars(on);
            self.toolbar.set_reveal_bottom_bars(on);
        }
    }

    /// Show or fade the interface — the chrome bars while windowed, the
    /// floating bar over the video while fullscreen.
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
        self.apply_revealed();
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

    /// Runs on the idle tick: fade the interface out when the pointer has been
    /// still for a while and nothing is being interacted with — the chrome bars
    /// while windowed, the floating bar while fullscreen. Playing or paused
    /// makes no difference; nothing loaded does — that is the state the player
    /// opens in, and the play button is the only control there.
    pub(super) fn idle_tick(&self, state: &PlaybackState) {
        if !self.revealed.get() || self.busy() || state.source.is_none() {
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
        // Both play buttons mirror the play state with their icon.
        self.center_button
            .set_icon_name(Self::play_icon(state.paused));
        self.play_button
            .set_icon_name(Self::play_icon(state.paused));
        // The center row rides with the interface in both modes. The one state
        // where it must survive is "nothing loaded", and that is what
        // `idle_tick` refuses to hide — so the play button the player opens
        // with is never faded away.
        self.center.set_reveal_child(self.revealed.get());
        self.time_label
            .set_text(&format::clock(state.position));
        self.duration_label
            .set_text(&format::clock(state.duration));
        self.volume_button
            .set_icon_name(volume_icon(state.volume, state.muted));
        // The right-edge OSD flashes on any change of the pair — including
        // mute alone, so pressing `m` gets the same feedback as scrolling.
        self.volume_osd.sync(state.volume, state.muted);
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
                // Block the edit handler: this is a sync, not a user edit.
                // Unblocked, it would re-enter the engine — and above 100
                // the sync value (clamped to the slider's range) is not
                // the state value, so it would pull the volume back down.
                self.volume_scale.block_signal(&self.volume_scale_changed);
                self.volume_scale.set_value(volume);
                self.volume_scale.unblock_signal(&self.volume_scale_changed);
            }
        }
        if !self.speed_button.is_active() && (self.speed_scale.value() - state.speed).abs() > 0.001 {
            self.speed_scale.set_value(state.speed);
        }
    }
}

/// Shared with the volume OSD (`ui::volume`): the icon the playbar button
/// and the indicator show is the same one, by the same thresholds.
pub(super) fn volume_icon(volume: f64, muted: bool) -> &'static str {
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
