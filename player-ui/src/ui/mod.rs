//! Floating-over-video controls (Milestone 3).
//!
//! The window content is a `gtk::Overlay`: the video surface is the main child,
//! and the header / playback bar are overlay children that fade out when the
//! pointer has been idle, mpv-style. Every control acts through
//! `player_core`'s engine trait — never on mpv directly. Files dragged onto
//! the window open too (a `DropTarget` on the window; see [`dnd`]).
//!
//! The modules follow the pieces of the window, not the GTK class hierarchy:
//!
//! * [`window`]   — window assembly: overlay, header, playbar, center button,
//!   window-move drag, double-click fullscreen
//! * [`controls`] — the widget-sync struct the event pump refreshes, plus the
//!   idle-hide state machine
//! * [`seek`]     — the seek slider's ownership + commit machine
//! * [`dnd`]      — the drop target and its hint
//! * [`volume`]   — wheel → volume steps, and the left-edge volume OSD

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
