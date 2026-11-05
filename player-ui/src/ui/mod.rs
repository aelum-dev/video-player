//! GNOME chrome around a video surface (Milestone 3).
//!
//! The window content is an `AdwToolbarView`: a real header bar on top, a
//! real playback toolbar at the bottom, and between them a `gtk::Overlay`
//! whose main child is the video surface. The interface fades out when the
//! pointer has been idle, mpv-style, in both modes: windowed that retracts
//! the chrome bars, fullscreen the `.osd` pill the same playback bar moves
//! into once the chrome is gone. Every control acts through `player_core`'s
//! engine trait — never on mpv directly. Files dragged onto the window open
//! too (a `DropTarget` on the window; see [`dnd`]).
//!
//! The modules follow the pieces of the window, not the GTK class hierarchy:
//!
//! * [`window`]   — window assembly: toolbar view, header, playback bar,
//!   center button, double-click fullscreen
//! * [`controls`] — the widget-sync struct the event pump refreshes, the
//!   windowed/fullscreen mode switch, and the idle-hide state machine
//! * [`seek`]     — the seek slider's ownership + commit machine
//! * [`dnd`]      — the drop target and its hint
//! * [`volume`]   — wheel → volume steps, and the right-edge volume OSD

mod controls;
mod dnd;
mod seek;
mod volume;
mod window;

pub use controls::Controls;
pub use window::build_window;

/// Monotonic milliseconds — the clock every idle/settle timer here shares.
pub(super) fn now_ms() -> i64 {
    gtk::glib::monotonic_time() / 1000
}
