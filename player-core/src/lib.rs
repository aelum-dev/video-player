//! Playback core: engine seam, playback state, plugin registry.
//!
//! This crate has no GTK dependency. UI front-ends (GTK today, others later)
//! and the video sink consume [`PlaybackEngine`] and [`PlayerEvent`] only —
//! the mpv binding (`mpv-engine`) stays hidden behind [`MpvEngine`].

pub mod engine;
pub mod plugin;
pub mod state;

pub use engine::{
    Chapter, EndReason, MpvEngine, PlaybackEngine, PlayerEvent, PropertyFormat, PropertyValue,
    parse_chapters,
};
pub use plugin::{PlayerPlugin, PluginContext, PluginRegistry};
pub use state::PlaybackState;
