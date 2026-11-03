//! The player's view of playback state, updated from [`PlayerEvent`]s.

use std::path::Path;

use crate::engine::{PlayerEvent, PropertyValue};

#[derive(Debug, Clone)]
pub struct PlaybackState {
    pub source: Option<String>,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub volume: f64,
    pub muted: bool,
    pub speed: f64,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            source: None,
            position: 0.0,
            duration: 0.0,
            // No media is playing until the user presses play: the controls
            // show the play shape by default (mirrors `PlaybackEngine::open`).
            paused: true,
            volume: 100.0,
            muted: false,
            speed: 1.0,
        }
    }
}

impl PlaybackState {
    /// Fold one event into the state. Unknown property names are ignored.
    pub fn apply(&mut self, event: &PlayerEvent) {
        match event {
            PlayerEvent::Loaded => {
                // `open` starts every title paused; this keeps the fold in
                // sync even if the `pause` property event is still in flight.
                self.paused = true;
                self.position = 0.0;
            }
            PlayerEvent::Ended { .. } | PlayerEvent::Failed { .. } => {
                self.position = 0.0;
                self.duration = 0.0;
            }
            PlayerEvent::Property { name, value } => match (name.as_str(), value) {
                ("time-pos", PropertyValue::Double(v)) => self.position = *v,
                ("duration", PropertyValue::Double(v)) => self.duration = *v,
                ("pause", PropertyValue::Flag(v)) => self.paused = *v,
                ("volume", PropertyValue::Double(v)) => self.volume = *v,
                ("mute", PropertyValue::Flag(v)) => self.muted = *v,
                ("speed", PropertyValue::Double(v)) => self.speed = *v,
                _ => {}
            },
            PlayerEvent::Restart | PlayerEvent::Shutdown => {}
        }
    }

    pub fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }

    /// File name of the current source, for the window title.
    pub fn title(&self) -> Option<String> {
        self.source.as_ref().map(|p| {
            Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone())
        })
    }
}
