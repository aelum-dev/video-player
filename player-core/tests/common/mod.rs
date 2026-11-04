//! Shared scaffolding for the integration tests: a scratch directory that is
//! **empty when the test starts** and gone when it ends.
//!
//! The fixed-name temp dirs this replaced were a real reproducibility trap:
//! the directory-playlist test asserts an exact playlist count, so a file an
//! older version of the test left behind in `/tmp` joined the playlist and
//! failed the run. Anything unique to the run (pid + clock) plus removal on
//! drop makes the same test give the same answer on every machine and in
//! every rerun.
#![allow(dead_code)] // each integration test compiles its own copy and uses part of it

use std::path::{Path, PathBuf};

pub struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    /// A fresh, empty directory under the system temp dir, named after `tag`
    /// plus this process and the current clock so parallel tests and reruns
    /// never share one.
    pub fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "video-player-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self { dir }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// A path inside the scratch directory (it need not exist yet).
    pub fn join(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a leftover directory must not fail an otherwise
        // passing test.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
