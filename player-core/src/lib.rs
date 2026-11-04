//! Playback core: engine seam, playback state, resume persistence.
//!
//! This crate has no GTK dependency. UI front-ends (GTK today, others later)
//! and the video sink consume [`PlaybackEngine`] and [`PlayerEvent`] only —
//! the mpv binding (`mpv-engine`) stays hidden behind [`MpvEngine`].
//!
//! The plugin registry was removed before Milestone 4: it had no users, and
//! a trait designed without a real plugin drifts. Re-introduce it together
//! with the first plugin (see PLAN.md).

pub mod engine;
pub mod last_played;
pub mod state;

pub use engine::{
    Chapter, EndReason, MpvEngine, PlaybackEngine, PlayerEvent, PropertyFormat, PropertyValue,
    SkipTarget, parse_chapters, skip_target,
};
pub use last_played::LastPlayed;
pub use state::PlaybackState;
