//! The player's view of playback state, updated from [`PlayerEvent`]s.

use std::collections::VecDeque;
use std::path::Path;

use crate::engine::{PlayerEvent, PropertyValue};

/// A title left this close to its end counts as finished: the next start
/// plays it again instead of opening on its last frame.
const NEAR_END_SECS: f64 = 2.0;

#[derive(Debug, Clone)]
pub struct PlaybackState {
    pub source: Option<String>,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    /// The current title ran to its end — mpv's `eof-reached`. At the end
    /// of the playlist `keep-open=yes` holds the last frame instead of
    /// ending the file, so "play" has to mean start over, not resume at EOF.
    pub ended: bool,
    pub volume: f64,
    /// Volumes the front-end asked the engine for but mpv has not
    /// confirmed yet, oldest first. A fast wheel burst fires several
    /// `volume-rel` steps between pump ticks; each next step builds on
    /// the last request instead of on the stale observed value (the
    /// "fast scroll is ignored" bug).
    volume_requests: VecDeque<f64>,
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
            volume_requests: VecDeque::new(),
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
            // Advancing to the next playlist entry (or a replace-load / a
            // quit) arrives as `end-file`; the next `Loaded` resets what
            // matters, so there is nothing to fold here. A title at the end
            // of the playlist never ends by itself — `eof-reached` (folded
            // below) is what marks it over.
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
                // The file mpv actually has loaded — mpv is the authority.
                // A playlist step was not initiated by the front-end, so
                // `set_source` never heard about it; this keeps the title
                // and the last-played record on the file that is on screen.
                // An empty path (an idle core) says nothing and is ignored.
                ("path", PropertyValue::Str(v)) if !v.is_empty() => {
                    self.source = Some(v.clone());
                }
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
                ("volume", PropertyValue::Double(v)) => {
                    // The echo of our own request pops it and must not
                    // write: a burst's later step must not roll back
                    // behind it. A value we did not ask for (a config,
                    // a script, another client, an engine clamp) is
                    // mpv's truth, and pending requests are stale.
                    if self.volume_requests.front() == Some(v) {
                        self.volume_requests.pop_front();
                    } else {
                        self.volume_requests.clear();
                        self.volume = *v;
                    }
                }
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

    /// Records a volume the front-end has just asked the engine for, so
    /// the next step in a fast wheel burst builds on it and not on the
    /// value mpv has not reported yet. The `volume` events confirm these
    /// in order (see the fold); an unrequested value clears the queue
    /// and wins. Same optimistic pattern as [`Self::set_play_state`].
    pub fn request_volume(&mut self, volume: f64) {
        // Only a real change is queued: mpv emits no event for a value
        // it already has, and an entry nothing confirms would linger.
        if volume == self.volume {
            return;
        }
        self.volume_requests.push_back(volume);
        self.volume = volume;
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

    /// The position worth recording for the current title: 0 when it is at
    /// its end, so the next start plays it again instead of opening on a
    /// still frame.
    pub fn resume_point(&self) -> f64 {
        let position = self.position.max(0.0);
        let at_end =
            self.ended || (self.duration > 0.0 && self.duration - position < NEAR_END_SECS);
        if at_end { 0.0 } else { position }
    }

    /// True when "play" means start over: at the end of the playlist
    /// `keep-open=yes` leaves an ended title paused on its last frame, and
    /// resuming there would end again immediately.
    pub fn should_restart(&self) -> bool {
        self.ended && self.paused
    }

    /// Something is loaded and playing — what tells a front-end to keep
    /// refreshing the last-played record.
    pub fn is_playing(&self) -> bool {
        self.source.is_some() && !self.paused
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

    #[test]
    fn a_playlist_step_follows_the_path_property() {
        // `playlist_step` changes the file without the front-end calling
        // `set_source`; mpv's `path` is what tells the fold about it.
        let mut state = PlaybackState::default();
        state.set_source("/d/a.mkv");
        state.apply(&PlayerEvent::Loaded);
        state.apply(&PlayerEvent::Property {
            name: "path".into(),
            value: PropertyValue::Str("/d/b.mkv".into()),
        });
        assert_eq!(state.source.as_deref(), Some("/d/b.mkv"));
        assert_eq!(state.title().as_deref(), Some("b.mkv"));

        // An idle core reports no path: that must not erase the title.
        state.apply(&PlayerEvent::Property {
            name: "path".into(),
            value: PropertyValue::Str(String::new()),
        });
        assert_eq!(state.source.as_deref(), Some("/d/b.mkv"));
    }

    #[test]
    fn a_fast_volume_burst_accumulates_before_mpv_confirms_any_step() {
        // The reported bug: several wheel steps inside one 100 ms pump
        // interval each added to the same stale observed volume, so the
        // burst collapsed into one step.
        let mut state = PlaybackState::default();
        state.request_volume(state.volume + 10.0);
        state.request_volume(state.volume + 10.0);
        assert_eq!(state.volume, 120.0);
        // The confirmations come back later, in order, and change nothing.
        state.apply(&number("volume", 110.0));
        assert_eq!(state.volume, 120.0, "a confirmation must not roll back a later request");
        state.apply(&number("volume", 120.0));
        assert_eq!(state.volume, 120.0);
    }

    #[test]
    fn an_unrequested_volume_still_belongs_to_mpv() {
        // A config, a script, a second client, or an engine clamp: a value
        // we never asked for is the truth, and pending requests are stale.
        let mut state = PlaybackState::default();
        state.request_volume(130.0);
        state.apply(&number("volume", 100.0));
        assert_eq!(state.volume, 100.0);
        // Observation works normally again afterwards.
        state.apply(&number("volume", 90.0));
        assert_eq!(state.volume, 90.0);
    }

    #[test]
    fn resume_point_records_the_position_except_near_the_end() {
        let mut state = PlaybackState::default();
        state.set_source("/a.mkv");
        state.duration = 100.0;
        state.position = 42.0;
        assert_eq!(state.resume_point(), 42.0);

        // Within NEAR_END_SECS of the end counts as finished: reopen from 0.
        state.position = 98.5;
        assert_eq!(state.resume_point(), 0.0);

        // An ended title never resumes on its last frame.
        state.position = 100.0;
        state.apply(&flag("eof-reached", true));
        assert_eq!(state.resume_point(), 0.0);

        // Unknown duration (stream): record the position as-is; negatives clamp.
        state.ended = false;
        state.duration = 0.0;
        state.position = 12.0;
        assert_eq!(state.resume_point(), 12.0);
        state.position = -3.0;
        assert_eq!(state.resume_point(), 0.0);
    }

    #[test]
    fn play_means_restart_only_when_paused_at_the_end() {
        let mut state = PlaybackState::default();
        state.set_source("/a.mkv");
        state.apply(&flag("eof-reached", true));
        state.apply(&flag("pause", true));
        assert!(state.should_restart());

        // keep-open-pause=no: a title can sit at EOF still playing, where
        // the button means pause, not start over.
        state.apply(&flag("pause", false));
        assert!(!state.should_restart());

        // Mid-title, paused: the button resumes, it does not restart.
        state.apply(&flag("eof-reached", false));
        state.apply(&flag("pause", true));
        assert!(!state.should_restart());
    }

    #[test]
    fn is_playing_requires_a_loaded_unpaused_title() {
        let mut state = PlaybackState::default();
        assert!(!state.is_playing(), "nothing loaded, nothing playing");
        state.set_source("/a.mkv");
        assert!(!state.is_playing(), "a paused load waits on the play button");
        state.set_play_state(false);
        assert!(state.is_playing());
        state.apply(&flag("pause", true));
        assert!(!state.is_playing());
    }
}
