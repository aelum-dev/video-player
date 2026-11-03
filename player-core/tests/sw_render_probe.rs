//! Video pipeline smoke test: the software render path must produce
//! non-black frames. Isolates mpv's decode/scale pipeline from the GL
//! sink in `player-video` — no display or GPU required.
//!
//! Default fixture is a tiny generated RGB24 AVI (self-contained).
//! `MPV_TEST_VIDEO=/path/to/file` points the probe at another clip.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use player_core::{MpvEngine, PlaybackEngine, PlayerEvent};

/// Minimal uncompressed AVI (BI_RGB, 64×64 RGB24, bright gradient).
fn write_colorful_avi(path: &Path, frames: usize) {
    let (w, h) = (64u32, 64u32);
    let frame_bytes = (w * h * 3) as usize;
    let fps = 10u32;

    fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(tag);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
    }
    fn list(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&((data.len() + 4) as u32).to_le_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(data);
    }

    let mut frame_data = Vec::with_capacity(frame_bytes);
    for y in 0..h {
        for x in 0..w {
            frame_data.push((x * 4) as u8);
            frame_data.push((y * 4) as u8);
            frame_data.push(128);
        }
    }

    let mut avih = Vec::new();
    avih.extend_from_slice(&(1_000_000 / fps).to_le_bytes());
    avih.extend_from_slice(&0u32.to_le_bytes());
    avih.extend_from_slice(&0u32.to_le_bytes());
    avih.extend_from_slice(&0x10u32.to_le_bytes()); // AVIF_HASINDEX
    avih.extend_from_slice(&(frames as u32).to_le_bytes());
    avih.extend_from_slice(&0u32.to_le_bytes());
    avih.extend_from_slice(&1u32.to_le_bytes()); // one stream
    avih.extend_from_slice(&0u32.to_le_bytes());
    avih.extend_from_slice(&w.to_le_bytes());
    avih.extend_from_slice(&h.to_le_bytes());
    avih.extend_from_slice(&[0u8; 16]);

    let mut strh = Vec::new();
    strh.extend_from_slice(b"vids"); // fccType: video stream
    strh.extend_from_slice(b"DIB "); // fccHandler: uncompressed DIB
    strh.extend_from_slice(&0u16.to_le_bytes());
    strh.extend_from_slice(&0u16.to_le_bytes());
    strh.extend_from_slice(&0u32.to_le_bytes());
    strh.extend_from_slice(&1u32.to_le_bytes()); // scale
    strh.extend_from_slice(&fps.to_le_bytes()); // rate
    strh.extend_from_slice(&0u32.to_le_bytes());
    strh.extend_from_slice(&(frames as u32).to_le_bytes());
    strh.extend_from_slice(&(frame_bytes as u32).to_le_bytes());
    strh.extend_from_slice(&u32::MAX.to_le_bytes());
    strh.extend_from_slice(&0u32.to_le_bytes());
    strh.extend_from_slice(&[0u8; 16]); // rcFrame

    let mut strf = Vec::new();
    strf.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
    strf.extend_from_slice(&w.to_le_bytes());
    strf.extend_from_slice(&h.to_le_bytes());
    strf.extend_from_slice(&1u16.to_le_bytes());
    strf.extend_from_slice(&24u16.to_le_bytes());
    strf.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    strf.extend_from_slice(&(frame_bytes as u32).to_le_bytes());
    strf.extend_from_slice(&[0u8; 16]);

    let mut hdrl = Vec::new();
    chunk(&mut hdrl, b"avih", &avih);
    let mut strl = Vec::new();
    chunk(&mut strl, b"strh", &strh);
    chunk(&mut strl, b"strf", &strf);
    list(&mut hdrl, b"strl", &strl);

    let mut movi = Vec::new();
    let mut idx1 = Vec::new();
    let mut offset = 0u32;
    for i in 0..frames {
        let tag: [u8; 4] = format!("{i:02}db").as_bytes().try_into().unwrap();
        chunk(&mut movi, &tag, &frame_data);
        idx1.extend_from_slice(&tag);
        idx1.extend_from_slice(&0u32.to_le_bytes()); // AVIIF_INDEX
        idx1.extend_from_slice(&offset.to_le_bytes());
        idx1.extend_from_slice(&(frame_bytes as u32).to_le_bytes());
        offset += (8 + frame_bytes) as u32;
    }

    let mut body = Vec::new();
    body.extend_from_slice(b"AVI ");
    list(&mut body, b"hdrl", &hdrl);
    list(&mut body, b"movi", &movi);
    chunk(&mut body, b"idx1", &idx1);

    let mut avi = Vec::new();
    avi.extend_from_slice(b"RIFF");
    avi.extend_from_slice(&(body.len() as u32).to_le_bytes());
    avi.extend_from_slice(&body);

    std::fs::write(path, avi).expect("write avi");
}

#[test]
fn software_render_produces_video_frames() {
    let dir = std::env::temp_dir().join("player-core-swtest");
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = match std::env::var("MPV_TEST_VIDEO") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            let p = dir.join("color.avi");
            write_colorful_avi(&p, 30);
            p
        }
    };

    let engine = MpvEngine::video().expect("video engine init");
    engine.inner().attach_sw_render(|| {}).expect("sw render attach");
    engine.open(path.to_str().unwrap()).expect("open");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut loaded = false;
    while Instant::now() < deadline && !loaded {
        for event in engine.pump_events() {
            match event {
                PlayerEvent::Loaded => loaded = true,
                PlayerEvent::Failed { message, .. } => panic!("playback failed: {message}"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(loaded, "mpv never emitted Loaded");

    let mut buf = Vec::new();
    let mut lit_max = 0usize;
    for _ in 0..30 {
        engine.inner().render_sw(64, 64, &mut buf).expect("render_sw");
        lit_max = lit_max.max(
            buf.chunks(4)
                .filter(|px| px.iter().take(3).any(|&c| c > 16))
                .count(),
        );
        if lit_max > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        lit_max > 0,
        "software renderer produced only black frames"
    );
}
