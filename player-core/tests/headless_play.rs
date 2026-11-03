//! Core smoke test: a silent WAV through the headless+null-ao engine.
//! No display or audio device required — runs in CI and Docker.

use std::path::Path;
use std::time::{Duration, Instant};

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
    let dir = std::env::temp_dir().join("player-core-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("silence.wav");
    write_silent_wav(&path);

    let engine = MpvEngine::silent().expect("engine init");
    engine.open(path.to_str().unwrap()).expect("open");

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
