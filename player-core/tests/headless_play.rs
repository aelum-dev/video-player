//! Core smoke test: a silent WAV through the headless+null-ao engine.
//! No display or audio device required — runs in CI and Docker.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::Scratch;
use player_core::{MpvEngine, PlaybackEngine, PlayerEvent, PropertyValue};

/// One second of 8 kHz 16-bit mono silence.
fn write_silent_wav(path: &Path) {
    let sample_rate = 8000u32;
    let samples = sample_rate as usize;
    let data_len = samples * 2;
    let mut wav = Vec::with_capacity(44 + data_len);

    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_len as u32).to_le_bytes());
    wav.resize(44 + data_len, 0);

    std::fs::write(path, wav).expect("write wav");
}

#[test]
fn loads_a_file_and_reports_events() {
    let scratch = Scratch::new("smoke");
    let path = scratch.join("silence.wav");
    write_silent_wav(&path);

    let engine = MpvEngine::silent().expect("engine init");
    engine.open(path.to_str().unwrap(), true).expect("open");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut loaded = false;
    let mut position = None;
    let mut duration = None;
    while Instant::now() < deadline && !(loaded && position.is_some() && duration.is_some()) {
        for event in engine.pump_events() {
            match &event {
                PlayerEvent::Loaded => loaded = true,
                PlayerEvent::Property { name, value } => match (name.as_str(), value) {
                    ("time-pos", PropertyValue::Double(v)) if *v > 0.0 => position = Some(*v),
                    ("duration", PropertyValue::Double(v)) if *v > 0.0 => duration = Some(*v),
                    _ => {}
                },
                PlayerEvent::Failed { message, .. } => panic!("playback failed: {message}"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(loaded, "mpv never emitted Loaded");
    assert!(position.is_some(), "time-pos never advanced");
    assert!(duration.is_some(), "duration never reported");
}

/// The engine's `autoplay=false` path: the title waits paused on its first
/// frame and only `set_paused(false)` starts it. The app opens every named
/// file playing now, but the seam's contract stays tested — and the playlist
/// test below depends on a load that does not run on its own.
#[test]
fn a_title_opened_without_autoplay_waits_paused_on_its_first_frame() {
    let scratch = Scratch::new("paused");
    let path = scratch.join("silence-paused.wav");
    write_silent_wav(&path);

    let engine = MpvEngine::silent().expect("engine init");
    engine.open(path.to_str().unwrap(), false).expect("open");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut loaded = false;
    while Instant::now() < deadline && !loaded {
        for event in engine.pump_events() {
            match &event {
                PlayerEvent::Loaded => loaded = true,
                PlayerEvent::Failed { message, .. } => panic!("playback failed: {message}"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(loaded, "mpv never emitted Loaded");

    assert!(engine.is_paused(), "the play button would show pause bars, not play");
    assert!(engine.position().unwrap_or(0.0) < 0.5, "a paused load must not play on");

    // Pressing play is what starts it.
    engine.set_paused(false).expect("unpause");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut position = None;
    while Instant::now() < deadline && position.is_none() {
        for event in engine.pump_events() {
            if let PlayerEvent::Property { name, value } = event
                && name == "time-pos"
                && let PropertyValue::Double(v) = value
                && v > 0.0
            {
                position = Some(v);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(position.is_some(), "play did not start the title");
}

/// The directory-playlist feature end to end: opening one file makes mpv
/// build a playlist of the same-type files in its directory
/// (`autocreate-playlist=same`), and `skip_step` walks it — what the
/// playbar's skip buttons and the < / > keys do. The observed `path`
/// property is what tells the front-end which file is now on screen.
#[test]
fn one_file_opens_a_directory_playlist_that_steps_forward_and_back() {
    // A directory of its own, empty to start: the count below is exact, so a
    // file left over from another run would join the playlist.
    let dir = Scratch::new("playlist");
    for name in ["a.wav", "b.wav", "c.wav"] {
        write_silent_wav(&dir.join(name));
    }

    let engine = MpvEngine::silent().expect("engine init");
    // `silent()` deliberately skips the app's option re-assertions, so name
    // the two options the feature needs (the ones `reassert_app_options` sets).
    engine
        .inner()
        .set_property("autocreate-playlist", "same")
        .expect("autocreate option");
    engine
        .inner()
        .set_property("directory-mode", "ignore")
        .expect("directory-mode option");

    // Opened paused: no title reaches EOF while the test walks the playlist,
    // and mpv keeps `pause` across steps, so nothing races the assertions.
    engine
        .open(dir.join("b.wav").to_str().unwrap(), false)
        .expect("open");

    // The load reports itself as a `path` change…
    let wait_path = |want: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            for event in engine.pump_events() {
                if let PlayerEvent::Property { name, value: PropertyValue::Str(v) } = event
                    && name == "path"
                    && v.ends_with(want)
                {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    };
    assert!(wait_path("b.wav"), "the opened file never reported its path");

    // …and the directory scan lands the other two files beside it.
    let count = || engine.inner().get_property::<i64>("playlist-count").unwrap_or(0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && count() < 3 {
        engine.pump_events();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(count(), 3, "opening one file never populated the directory");

    engine.skip_step(1).expect("next");
    assert!(wait_path("c.wav"), "next did not land on c.wav");
    engine.skip_step(-1).expect("prev");
    assert!(wait_path("b.wav"), "prev did not land back on b.wav");

    // Stepping past the first entry stays on it: mpv's default `weak` flag
    // answers the command with an error and changes nothing (the front-end
    // logs and ignores it), so the controls never need disabling.
    engine.skip_step(-1).expect("prev");
    let _ = engine.skip_step(-1);
    assert_eq!(
        engine.inner().get_property::<i64>("playlist-pos").unwrap_or(-1),
        0,
        "a step past the first entry must stay on it"
    );
}

/// The directory-playlist options are app *defaults*, not overrides: set
/// pre-init, an explicit line in the user's mpv.conf wins (the config is
/// applied during `mpv_initialize`, after pre-init properties). Pinned so
/// nobody "re-asserts" the user's choice away again.
#[test]
fn user_config_can_disable_the_directory_playlist() {
    let conf = Scratch::new("confprio");
    std::fs::write(conf.join("mpv.conf"), "autocreate-playlist=no\n").expect("write mpv.conf");

    let engine = mpv_engine::Engine::headless()
        .property("config", "yes")
        .property("config-dir", conf.path().to_str().unwrap())
        .property("autocreate-playlist", "same") // the app default, as user_config sets
        .property("directory-mode", "ignore")
        .property("ao", "null")
        .build()
        .expect("engine init");
    assert_eq!(
        engine.get_property::<String>("autocreate-playlist").ok().as_deref(),
        Some("no"),
        "the user's mpv.conf must beat the app's pre-init default"
    );

    // With no config file in the directory, the app default stands.
    let empty = Scratch::new("confprio-empty");
    let engine = mpv_engine::Engine::headless()
        .property("config", "yes")
        .property("config-dir", empty.path().to_str().unwrap())
        .property("autocreate-playlist", "same")
        .property("directory-mode", "ignore")
        .property("ao", "null")
        .build()
        .expect("engine init");
    assert_eq!(
        engine.get_property::<String>("autocreate-playlist").ok().as_deref(),
        Some("same"),
        "without a user override the default must stand"
    );
}
