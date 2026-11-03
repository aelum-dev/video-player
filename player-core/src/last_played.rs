//! The last-played record: the title the player's play button starts, and
//! where it left off.
//!
//! A small `key=value` file in **mpv's own config directory**, next to
//! `mpv.conf` and written in that file's syntax, so it can be read and
//! edited by hand. mpv reads only specific names from that directory
//! (`mpv.conf`, `input.conf`, `scripts/`, …), so an extra file there is
//! inert to mpv itself — and it follows the user's `MPV_HOME` /
//! `XDG_CONFIG_HOME` relocation for free, landing wherever their `mpv.conf`
//! actually is.
//!
//! Reading never fails: a missing, truncated, or hand-mangled file degrades
//! to "no record" rather than stopping the player from starting (the same
//! discipline as [`crate::engine::parse_chapters`]).

use std::env;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// File name inside mpv's config directory.
const FILE: &str = "last_played";

/// What the play button starts when nothing is loaded, and where to resume.
#[derive(Debug, Clone, PartialEq)]
pub struct LastPlayed {
    /// Path or URL, exactly as it was given to the engine.
    pub source: String,
    /// Position in seconds to resume at (0 = from the beginning).
    pub position: f64,
}

/// A non-empty environment variable as a path.
fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// mpv's config directory, resolved the way mpv resolves it (manual,
/// FILES): `$MPV_HOME` overrides everything, otherwise `$XDG_CONFIG_HOME`
/// (default `~/.config`) + `/mpv`; the pre-0.5.0 `~/.mpv` is used when the
/// standard directory is absent. `None` only when there is no home to
/// write to at all.
pub fn config_dir() -> Option<PathBuf> {
    if let home @ Some(_) = env_path("MPV_HOME") {
        return home;
    }
    let config =
        env_path("XDG_CONFIG_HOME").or_else(|| env_path("HOME").map(|home| home.join(".config")))?;
    let standard = config.join("mpv");
    if standard.is_dir() {
        return Some(standard);
    }
    env_path("HOME")
        .map(|home| home.join(".mpv"))
        .filter(|legacy| legacy.is_dir())
        .or(Some(standard))
}

/// Where the record lives, if there is a config directory to put it in.
pub fn path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(FILE))
}

/// Parse the file's contents. Unknown keys are ignored (so the format can
/// grow), and anything unusable means "no record".
fn parse(text: &str) -> Option<LastPlayed> {
    let mut source = None;
    let mut position: f64 = 0.0;
    for line in text.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "source" => source = Some(value.trim().to_owned()),
            "position" => position = value.trim().parse().unwrap_or(0.0),
            _ => {}
        }
    }
    let source = source.filter(|source| !source.is_empty())?;
    // A nonsense position is no reason to forget the title.
    let position = if position.is_finite() && position > 0.0 { position } else { 0.0 };
    Some(LastPlayed { source, position })
}

/// Read the record, if there is a usable one.
pub fn load() -> Option<LastPlayed> {
    let text = fs::read_to_string(path()?).ok()?;
    parse(&text)
}

/// Write the record. Written through a temporary name and renamed, so a
/// crash mid-write cannot leave a half-written record behind.
pub fn save(entry: &LastPlayed) -> Result<()> {
    let path = path().context("no config directory to write the record to")?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temp = path.with_file_name(format!("{FILE}.tmp"));
    fs::write(&temp, entry.to_string())?;
    fs::rename(&temp, &path)?;
    Ok(())
}

impl fmt::Display for LastPlayed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `mpv.conf`'s syntax: `#` comments and `key=value`. The source is
        // written raw — a `#` inside a value is safe because only a leading
        // `#` is treated as a comment.
        writeln!(f, "# Written by the video player: the title Play starts, and where it left off.")?;
        writeln!(f, "source={}", self.source)?;
        writeln!(f, "position={:.3}", self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::{LastPlayed, parse};

    #[test]
    fn round_trips_through_the_written_file() {
        let entry = LastPlayed { source: "/home/me/vids/clip.mkv".into(), position: 82.5 };
        assert_eq!(parse(&entry.to_string()).as_ref(), Some(&entry));
    }

    #[test]
    fn ignores_comments_blanks_and_unknown_keys() {
        let entry = parse(
            "  # hand-written\n\n\
             source=/home/me/a file.mkv\n\
             position = 12.25\n\
             volume=90\n",
        )
        .expect("record");
        assert_eq!(entry.source, "/home/me/a file.mkv");
        assert_eq!(entry.position, 12.25);
    }

    #[test]
    fn a_path_may_contain_equals_and_hash() {
        let entry = parse("source=/mnt/x#1/ep=01.mkv\nposition=3").expect("record");
        assert_eq!(entry.source, "/mnt/x#1/ep=01.mkv");
        assert_eq!(entry.position, 3.0);
    }

    #[test]
    fn unusable_input_degrades_to_no_record_or_a_cold_start() {
        assert!(parse("").is_none());
        assert!(parse("# just a comment\n").is_none());
        assert!(parse("position=30\n").is_none(), "no title to reopen");
        assert!(parse("source=\nposition=30\n").is_none(), "empty title");
        // A broken position still reopens the title, from the start.
        let entry = parse("source=/a.mkv\nposition=nan-ish\n").expect("record");
        assert_eq!(entry.position, 0.0);
        let entry = parse("source=/a.mkv\nposition=-5\n").expect("record");
        assert_eq!(entry.position, 0.0);
    }
}
