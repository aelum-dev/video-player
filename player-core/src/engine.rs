//! The engine seam: our own [`PlaybackEngine`] trait over `mpv-engine`.
//!
//! `mpv-engine` is the current adapter; if it stalls we can swap in
//! hand-written FFI or a JSON-RPC subprocess adapter without touching UI
//! or plugins (see PLAN.md).

use std::sync::Arc;

use anyhow::Result;
pub use mpv_engine::{EndReason, PropertyFormat, PropertyValue};

/// Playback lifecycle events delivered to the UI and plugins.
///
/// Mirrors `mpv_engine::PlaybackEvent`; the mapping lives here so the
/// binding's enum never appears in consumer code.
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    /// A file finished loading and playback is starting.
    Loaded,
    /// Playback ended without error.
    Ended { reason: EndReason },
    /// The displayed frame matches the position again (post-seek).
    Restart,
    /// An observed property changed; also fires once with the current
    /// value right after [`PlaybackEngine::observe`].
    Property { name: String, value: PropertyValue },
    /// The mpv core is shutting down.
    Shutdown,
    /// mpv aborted playback (unreadable, corrupt, unsupported format).
    Failed { code: i32, message: String },
}

/// A chapter marker of the current title.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chapter {
    pub title: Option<String>,
    pub time: f64,
}

/// Everything a front-end needs from a playback engine.
///
/// Volume is in mpv percent (0..100+), times in seconds.
pub trait PlaybackEngine: Send + Sync {
    /// Load a file path or URL and start playback.
    fn open(&self, source: &str) -> Result<()>;
    fn set_paused(&self, paused: bool) -> Result<()>;
    fn is_paused(&self) -> bool;
    fn toggle_play(&self) -> Result<()> {
        self.set_paused(!self.is_paused())
    }
    /// Seek to an absolute position in seconds.
    fn seek(&self, secs: f64) -> Result<()>;
    /// Seek by an offset in seconds (negative goes back).
    fn seek_relative(&self, secs: f64) -> Result<()>;
    fn set_volume(&self, percent: f64) -> Result<()>;
    fn set_muted(&self, muted: bool) -> Result<()>;
    /// Playback speed multiplier (1.0 is normal speed).
    fn set_speed(&self, speed: f64) -> Result<()>;
    /// Jump `delta` chapters (-1 previous, +1 next).
    fn chapter_step(&self, delta: i32) -> Result<()>;
    /// Chapter markers of the current title; empty when there are none.
    fn chapters(&self) -> Vec<Chapter>;
    fn stop(&self) -> Result<()>;
    fn position(&self) -> Option<f64>;
    fn duration(&self) -> Option<f64>;
    /// Subscribe to an mpv property; changes arrive as
    /// [`PlayerEvent::Property`].
    fn observe(&self, name: &str, format: PropertyFormat) -> Result<()>;
    /// Drain pending events (pull-based; call from the shell's loop).
    fn pump_events(&self) -> Vec<PlayerEvent>;
}

/// `PlaybackEngine` implemented over `mpv-engine`.
pub struct MpvEngine {
    inner: Arc<mpv_engine::Engine>,
}

impl MpvEngine {
    fn from_engine(engine: mpv_engine::Engine) -> Result<Self> {
        let this = Self {
            inner: Arc::new(engine),
        };
        // Standard observations every front-end needs.
        this.inner.observe("time-pos", PropertyFormat::Double)?;
        this.inner.observe("duration", PropertyFormat::Double)?;
        this.inner.observe("pause", PropertyFormat::Flag)?;
        this.inner.observe("volume", PropertyFormat::Double)?;
        this.inner.observe("mute", PropertyFormat::Flag)?;
        this.inner.observe("speed", PropertyFormat::Double)?;
        Ok(this)
    }

    /// Video engine (`vo=libmpv`): frames appear once a render context is
    /// attached — used from Milestone 2 by `player-video`.
    pub fn video() -> Result<Self> {
        // mpv's own OSD would double up with the front-end's controls
        // (time/position text over the video), so it stays off; the UI owns
        // all on-screen feedback.
        let engine = mpv_engine::Engine::video()
            .property("osd-level", "0")
            .build()?;
        Self::from_engine(engine)
    }

    /// Audio-only / headless engine (`vo=null`): loads play immediately.
    /// This is what Milestone 1 runs with.
    pub fn headless() -> Result<Self> {
        Self::from_engine(mpv_engine::Engine::headless().build()?)
    }

    /// Headless with `ao=null` as well: no audio device required (tests).
    pub fn silent() -> Result<Self> {
        Self::from_engine(
            mpv_engine::Engine::headless()
                .property("ao", "null")
                .build()?,
        )
    }

    /// Escape hatch for the render seam (`attach_gl_render` etc.) used by
    /// `player-video`; not part of the front-end-facing trait.
    pub fn inner(&self) -> &mpv_engine::Engine {
        &self.inner
    }

    /// Draw the current video frame into the bound GL framebuffer `fbo`
    /// of size `w`×`h` (physical pixels). Called from the shell's paint
    /// handler with that GL context current.
    pub fn render_gl(&self, fbo: i32, w: i32, h: i32, flip_y: bool) -> Result<()> {
        self.inner.render_gl(fbo, w, h, flip_y)?;
        Ok(())
    }
}

impl PlaybackEngine for MpvEngine {
    fn open(&self, source: &str) -> Result<()> {
        self.inner.load_when_ready(source)?;
        Ok(())
    }

    fn set_paused(&self, paused: bool) -> Result<()> {
        self.inner.set_paused(paused)?;
        Ok(())
    }

    fn is_paused(&self) -> bool {
        self.inner.is_paused()
    }

    fn seek(&self, secs: f64) -> Result<()> {
        self.inner.seek_absolute(secs)?;
        Ok(())
    }

    fn seek_relative(&self, secs: f64) -> Result<()> {
        self.inner.seek_relative(secs)?;
        Ok(())
    }

    fn set_muted(&self, muted: bool) -> Result<()> {
        self.inner.set_muted(muted)?;
        Ok(())
    }

    fn set_speed(&self, speed: f64) -> Result<()> {
        self.inner.set_speed(speed)?;
        Ok(())
    }

    fn chapter_step(&self, delta: i32) -> Result<()> {
        // mpv's `add chapter <delta>` is the documented chapter jump.
        self.inner.command("add", &["chapter", &delta.to_string()])?;
        Ok(())
    }

    fn chapters(&self) -> Vec<Chapter> {
        self.inner
            .get_property::<String>("chapter-list")
            .map(|json| parse_chapters(&json))
            .unwrap_or_default()
    }

    fn set_volume(&self, percent: f64) -> Result<()> {
        self.inner.set_volume(percent)?;
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        self.inner.stop()?;
        Ok(())
    }

    fn position(&self) -> Option<f64> {
        self.inner.position()
    }

    fn duration(&self) -> Option<f64> {
        self.inner.duration()
    }

    fn observe(&self, name: &str, format: PropertyFormat) -> Result<()> {
        self.inner.observe(name, format)?;
        Ok(())
    }

    fn pump_events(&self) -> Vec<PlayerEvent> {
        self.inner
            .pump_events()
            .into_iter()
            .map(|ev| match ev {
                mpv_engine::PlaybackEvent::Loaded => PlayerEvent::Loaded,
                mpv_engine::PlaybackEvent::Ended { reason } => PlayerEvent::Ended { reason },
                mpv_engine::PlaybackEvent::PlaybackRestart => PlayerEvent::Restart,
                mpv_engine::PlaybackEvent::PropertyChanged { name, value, .. } => {
                    PlayerEvent::Property { name, value }
                }
                mpv_engine::PlaybackEvent::Shutdown => PlayerEvent::Shutdown,
                mpv_engine::PlaybackEvent::Failed { code, message } => {
                    PlayerEvent::Failed { code, message }
                }
                // PlaybackEvent is non-exhaustive: additive upstream variants
                // are ignored here rather than breaking this crate.
                _ => PlayerEvent::Restart,
            })
            .collect()
    }
}

/// Parse mpv's `chapter-list` rendered as JSON (`[{"title":…,"time":…}, …]`).
///
/// Chapter ticks are decoration, so a malformed or unavailable list degrades to
/// "no chapters" rather than an error.
pub fn parse_chapters(json: &str) -> Vec<Chapter> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(items) = root.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let time = item.get("time").and_then(|t| t.as_f64())?;
            let title = item
                .get("title")
                .and_then(|t| t.as_str())
                .map(str::to_owned)
                .filter(|t| !t.is_empty());
            Some(Chapter { title, time })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_chapters;

    #[test]
    fn chapter_json_parses_titles_and_times() {
        let chapters = parse_chapters(
            r#"[{"title":"Start","time":0},{"title":"","time":12.5},{"time":30}]"#,
        );
        assert_eq!(chapters.len(), 3);
        assert_eq!(chapters[0].title.as_deref(), Some("Start"));
        assert_eq!(chapters[1].title, None);
        assert_eq!(chapters[2].time, 30.0);
    }

    #[test]
    fn chapter_json_degrades_to_empty() {
        assert!(parse_chapters("").is_empty());
        assert!(parse_chapters("(not json)").is_empty());
        assert!(parse_chapters(r#"{"title":"nope"}"#).is_empty());
    }
}
