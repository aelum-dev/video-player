//! The engine seam: our own [`PlaybackEngine`] trait over `mpv-engine`.
//!
//! `mpv-engine` is the current adapter; if it stalls we can swap in
//! hand-written FFI or a JSON-RPC subprocess adapter without touching UI
//! or plugins.

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
    /// Load a file path or URL.
    ///
    /// `autoplay` chooses the state the title starts in: `true` starts it
    /// playing, `false` leaves it paused on its first frame so the
    /// front-end's play button is the deliberate start. Either way the play
    /// state is reset — mpv keeps `pause` across file changes.
    fn open(&self, source: &str, autoplay: bool) -> Result<()>;
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
    /// Jump `delta` chapters (-1 previous, +1 next) **inside** the current
    /// title. The buttons on the video and Ctrl+←/→; a title without chapters
    /// answers with an error. Changing the file is [`playlist_step`].
    fn chapter_step(&self, delta: i32) -> Result<()>;
    /// Chapter markers of the current title; empty when there are none.
    fn chapters(&self) -> Vec<Chapter>;
    /// Jump `delta` playlist entries (-1 previous file, +1 next file).
    ///
    /// This is what the skip controls are: the playbar's `<` `>` pair and the
    /// `<` / `>` keys. They change the **file**; what the buttons on the video
    /// do is `chapter_step`.
    ///
    /// The playlist is populated by mpv's directory autoload (see
    /// [`reassert_app_options`]). At its ends — and for a file alone in its
    /// directory — the step errors and changes nothing: log it and move on.
    fn playlist_step(&self, delta: i32) -> Result<()>;
    fn stop(&self) -> Result<()>;
    fn position(&self) -> Option<f64>;
    fn duration(&self) -> Option<f64>;
    /// Subscribe to an mpv property; changes arrive as
    /// [`PlayerEvent::Property`].
    fn observe(&self, name: &str, format: PropertyFormat) -> Result<()>;
    /// Drain pending events (pull-based; call from the shell's loop).
    fn pump_events(&self) -> Vec<PlayerEvent>;
}

/// Enable reading the user's mpv configuration — `~/.config/mpv/mpv.conf`
/// (plus its `input.conf` and `scripts/`; mpv's default config dir, we
/// never set `config-dir`). Must be set before `build()`: config loading
/// happens inside `mpv_initialize`. (`config-file` is not an option in
/// mpv 0.41 — `config` alone gates `mpv.conf`; setting it fails with
/// OPTION_NOT_FOUND.)
///
/// Precedence note, verified against mpv 0.41: the config file is applied
/// during init and *overrides* pre-init builder settings — an `mpv.conf`
/// `vo=gpu-next` beats a pre-init `vo=libmpv`. So the options the app
/// itself depends on are re-asserted after `build()`; see
/// [`reassert_app_options`].
///
/// The opposite side of that rule is used here on purpose: the directory
/// playlist options are set pre-init as *defaults*, so a line in the
/// user's `mpv.conf` wins — `autocreate-playlist=no` turns the feature
/// off, `directory-mode=…` picks the scan mode. They are user
/// preferences, not app dependencies: with the playlist left empty the
/// file's `<`/`>` steps just no-op (mpv's `weak` flag).
///
/// `autocreate-playlist=same` is mpv's native autoload (0.39+, the
/// successor of `autoload.lua`): opening one local file builds a playlist
/// from its directory, restricted to files of the same category as the one
/// opened — a video opens only videos. `directory-mode=ignore` keeps that
/// playlist flat: subdirectories contribute no entries.
fn user_config(builder: mpv_engine::EngineBuilder) -> mpv_engine::EngineBuilder {
    builder
        .property("config", "yes")
        .property("autocreate-playlist", "same")
        .property("directory-mode", "ignore")
}

/// Re-assert the options the app depends on, after the config file has
/// had its say (see [`user_config`] for why pre-init setting is not
/// enough *for those*). Setting `vo` at runtime is safe before any load —
/// the VO is only built at loadfile time, so the config's `vo` never opens
/// a window. Everything else the user wrote — hwdec, scaling, subtitles,
/// alang, the directory-playlist defaults, … — stays exactly as written.
///
/// `keep-open=yes`: at EOF a title advances to the next playlist entry and
/// only holds the last frame at the end of the playlist (`always` would
/// not advance at all, which is what the player did before directory
/// playlists existed). The end-of-file fold and restart-on-play are built
/// on holding that frame, so this one is an app dependency, not a default.
fn reassert_app_options(engine: &mpv_engine::Engine, vo: &str) -> Result<()> {
    engine.set_property("vo", vo)?;
    engine.set_property("keep-open", "yes")?;
    Ok(())
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
        // At the end of the playlist `keep-open=yes` holds the last frame
        // instead of ending the file, so this — not `end-file` — is what
        // says the title is over there.
        this.inner.observe("eof-reached", PropertyFormat::Flag)?;
        // The file mpv actually has loaded. Unlike the path `open` was given,
        // this follows playlist steps (`playlist_step`), so the front-end's
        // title and last-played record track the file on screen.
        this.inner.observe("path", PropertyFormat::Str)?;
        this.inner.observe("volume", PropertyFormat::Double)?;
        this.inner.observe("mute", PropertyFormat::Flag)?;
        this.inner.observe("speed", PropertyFormat::Double)?;
        Ok(this)
    }

    /// Video engine (`vo=libmpv`): frames appear once a render context is
    /// attached — used from Milestone 2 by `player-video`.
    ///
    /// Reads the user's `~/.config/mpv/mpv.conf` (see [`user_config`]).
    pub fn video() -> Result<Self> {
        let engine = user_config(mpv_engine::Engine::video()).build()?;
        reassert_app_options(&engine, "libmpv")?;
        // mpv's own OSD would double up with the front-end's controls
        // (time/position text over the video), so it stays off; the UI owns
        // all on-screen feedback.
        engine.set_property("osd-level", "0")?;
        Self::from_engine(engine)
    }

    /// Video engine with the user's config switched off: the test/CI twin of
    /// [`Self::video`], identical except that nothing in `~/.config/mpv/` can
    /// change what it does. A probe that reads the user's `mpv.conf` gives
    /// different results on different machines — on the author's box it has a
    /// software-friendly profile, on someone else's anything at all.
    pub fn video_isolated() -> Result<Self> {
        let engine = mpv_engine::Engine::video()
            .property("config", "no")
            .build()?;
        reassert_app_options(&engine, "libmpv")?;
        Self::from_engine(engine)
    }

    /// Audio-only / headless engine (`vo=null`): loads play immediately.
    /// This is what Milestone 1 runs with.
    ///
    /// Reads the user's `~/.config/mpv/mpv.conf` (see [`user_config`]).
    pub fn headless() -> Result<Self> {
        let engine = user_config(mpv_engine::Engine::headless()).build()?;
        reassert_app_options(&engine, "null")?;
        Self::from_engine(engine)
    }

    /// Headless with `ao=null` as well: no audio device required (tests).
    /// Deliberately skips the user config — tests must not change behavior
    /// with whatever happens to be in `~/.config/mpv/`.
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
    fn open(&self, source: &str, autoplay: bool) -> Result<()> {
        // mpv keeps the `pause` property across file changes, so the state a
        // new title starts in has to be set here rather than left over from
        // the previous one.
        self.inner.set_paused(!autoplay)?;
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

    fn playlist_step(&self, delta: i32) -> Result<()> {
        // mpv's playlist-prev / playlist-next (the command is NOT named
        // "playlist-previous"). The default `weak` flag: at the first/last
        // entry the command reports an error and changes nothing — playback
        // is NOT stopped, which is what `force` would do. Callers may log
        // the error and ignore it; the front-end never has to pre-check the
        // position.
        let cmd = if delta < 0 { "playlist-prev" } else { "playlist-next" };
        self.inner.command(cmd, &[])?;
        Ok(())
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
