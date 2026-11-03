//! The player's view of playback state, updated from [`PlayerEvent`]s.

use std::path::Path;

use crate::engine::{PlayerEvent, PropertyValue};

#[derive(Debug, Clone)]
pub struct PlaybackState {
    pub source: Option<String>,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    /// The current title ran to its end — mpv's `eof-reached`. With
    /// `keep-open=always` the player holds the last frame instead of ending
    /// the file, so "play" has to mean start over, not resume at EOF.
    pub ended: bool,
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
            // Nothing is loaded and nothing is playing: the controls show the
            // play shape, which is the control that starts something.
            paused: true,
            ended: false,
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
                // A new title is not over. Its play state is the one the
                // front-end asked `open` for (see [`Self::set_play_state`]);
                // mpv's own `pause` event confirms it right after, so the
                // fold does not guess it here.
                self.ended = false;
                self.position = 0.0;
            }
            // With keep-open=always a title never ends by itself — `eof-reached`
            // (folded below) is what marks it over. An `end-file` here is a
            // replace-load or a quit, and the next `Loaded` resets what matters.
            PlayerEvent::Ended { .. } => {}
            PlayerEvent::Failed { .. } => {
                self.ended = false;
                self.position = 0.0;
                self.duration = 0.0;
            }
            // A seek landed: the title is where the user put it, not at its end.
            // (mpv's own `eof-reached` says so too, a moment later.)
            PlayerEvent::Restart => self.ended = false,
            PlayerEvent::Property { name, value } => match (name.as_str(), value) {
                ("time-pos", PropertyValue::Double(v)) => self.position = *v,
                ("duration", PropertyValue::Double(v)) => self.duration = *v,
                // Only for a loaded title. An idle core reports `pause=false`
                // the moment it is observed, and with nothing open that says
                // nothing about the front-end — it would flip the play button
                // to pause bars on an empty window.
                ("pause", PropertyValue::Flag(v)) if self.source.is_some() => self.paused = *v,
                ("eof-reached", PropertyValue::Flag(v)) if self.source.is_some() => {
                    self.ended = *v;
                }
                ("volume", PropertyValue::Double(v)) => self.volume = *v,
                ("mute", PropertyValue::Flag(v)) => self.muted = *v,
                ("speed", PropertyValue::Double(v)) => self.speed = *v,
                _ => {}
            },
            PlayerEvent::Shutdown => {}
        }
    }

    pub fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }

    /// Records the play state a load was started with, so the controls show
    /// the right icon before mpv's `pause` event arrives.
    pub fn set_play_state(&mut self, paused: bool) {
        self.paused = paused;
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

#[cfg(test)]
mod tests {
    use super::PlaybackState;
    use crate::engine::{EndReason, PlayerEvent, PropertyValue};

    fn flag(name: &str, value: bool) -> PlayerEvent {
        PlayerEvent::Property { name: name.into(), value: PropertyValue::Flag(value) }
    }

    fn number(name: &str, value: f64) -> PlayerEvent {
        PlayerEvent::Property { name: name.into(), value: PropertyValue::Double(value) }
    }

    #[test]
    fn a_title_opened_paused_stays_paused_through_load() {
        let mut state = PlaybackState::default();
        state.set_source("/a.mkv");
        state.set_play_state(true);
        state.apply(&PlayerEvent::Loaded);
        state.apply(&number("duration", 100.0));
        assert!(state.paused, "the play button must not flip to pause bars");
        assert!(!state.ended);
    }

    #[test]
    fn playing_wins_when_the_load_says_playing() {
        let mut state = PlaybackState::default();
        state.set_source("/a.mkv");
        state.set_play_state(false);
        state.apply(&PlayerEvent::Loaded);
        assert!(!state.paused);
        state.apply(&flag("pause", true));
        assert!(state.paused, "mpv stays the authority");
    }

    #[test]
    fn an_idle_core_cannot_take_the_play_button_off_the_player() {
        // Observing `pause` reports the core's current value at once, and an
        // idle core is "not paused" — with nothing loaded that must not flip
        // the button to pause bars.
        let mut state = PlaybackState::default();
        for event in [flag("pause", false), flag("eof-reached", false)] {
            state.apply(&event);
        }
        assert!(state.paused, "the play button stays a play triangle");
        assert!(!state.ended);
        // And once a title is open, mpv is the authority again.
        state.set_source("/a.mkv");
        state.apply(&PlayerEvent::Loaded);
        state.apply(&flag("pause", false));
        assert!(!state.paused);
    }

    #[test]
    fn eof_reached_marks_the_title_over_and_clears_on_a_seek() {
        let mut state = PlaybackState::default();
        state.set_source("/a.mkv");
        state.set_play_state(false);
        state.apply(&number("time-pos", 100.0));
        state.apply(&number("duration", 100.0));
        // keep-open=always: mpv pauses and flags the end, it does not end the file.
        state.apply(&flag("pause", true));
        state.apply(&flag("eof-reached", true));
        assert!(state.ended && state.paused);

        state.apply(&PlayerEvent::Restart);
        assert!(!state.ended, "Play must not jump to 0 after a manual seek");
        state.apply(&flag("eof-reached", false));
        assert!(!state.ended);
    }

    #[test]
    fn a_new_title_clears_the_ended_flag() {
        let mut state = PlaybackState::default();
        state.set_source("/a.mkv");
        state.apply(&flag("eof-reached", true));
        assert!(state.ended);
        // Opening the next title: end-file for the old one arrives as Stop,
        // which is not "this title is over".
        state.apply(&PlayerEvent::Ended { reason: EndReason::Stop });
        assert!(state.ended);
        state.set_play_state(false);
        state.apply(&PlayerEvent::Loaded);
        assert!(!state.ended && state.position == 0.0);
    }
}
