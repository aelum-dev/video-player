# Video player

A media player for the [Aelum](https://aelum.dev) desktop: GTK4 + libadwaita on the outside,
libmpv on the inside. A GNOME header bar and a bottom playback toolbar; in fullscreen the same
controls float over the video and fade when the pointer goes idle.
Binary `video-player`, app ID `aelum.videoplayer.Player`.

It is a Cargo workspace of three crates with one hard rule between them: the playback core knows
nothing about GTK, and the UI never calls mpv directly. That seam is the reason the engine can be
swapped, and the reason the whole pipeline is testable without a display.

## What it does

- Plays video and audio through libmpv, drawn into a `GtkGLArea` via the mpv render API.
- A real GNOME window: header bar with the media title and a primary menu, playback controls
  in a bottom toolbar — play/pause, seek bar with chapter ticks, volume, speed presets,
  fullscreen. In fullscreen the toolbar floats over the video and fades when idle.
- A pin button (the lock icon) asks Hyprland to keep the window on top across
  workspaces — the player as a picture-in-picture while another window is the work.
- An About dialog (`AdwAboutDialog`) behind the header's info button.
- Drop a file on the window to open it; the window moves by dragging its header bar.
- Opening one file builds a playlist from its directory, so `<` / `>` walk to the next title.
  With nothing to walk they step chapters instead.
- Reopens where you left off: the play button starts the last title at the position it stopped at.
- Follows the system light/dark variant, and picks up the desktop palette live when it is recolored.
- Reads your own `~/.config/mpv/mpv.conf` — hardware decoding, subtitles, audio language, all of it.
- Interface in 28 languages, with the Arabic, Hebrew and Persian layouts mirrored right-to-left.

Keyboard: `Space` play/pause, `←`/`→` seek 5 s, `Ctrl`+`←`/`→` chapters, `<`/`>` previous/next
title, `↑`/`↓` volume, `m` mute, `f` or `F11` fullscreen, `Ctrl`+`O` open, `Ctrl`+`Q` quit.

## Requirements

| Needs | Version | Notes |
|---|---|---|
| Rust | 1.85+ | the workspace uses edition 2024 |
| GTK | 4.24 | the `gtk4` crate is built with the `gnome_50` feature |
| libadwaita | 1.10 | features `v1_10` + `gtk_v4_24` |
| mpv / libmpv | 0.39+ | built and verified against 0.41; the directory playlist needs 0.39+ |
| pkgconf | any | to find the two libraries above |
| OpenGL | 3.3+ | loaded at runtime through libglvnd, so nothing names it at link time |

On Arch that is `pacman -S rust pkgconf gtk4 libadwaita mpv libglvnd` — the same set the
[Dockerfile](Dockerfile) installs.

## Build and run

```bash
cargo build --release --locked   # → target/release/video-player
cargo run -p player-ui -- some-video.mkv
```

`--locked` matters: `Cargo.lock` is committed, and both the Arch package and the container build
use it, so a build elsewhere resolves exactly the crates this one was tested with.

The player is a single-instance `GApplication`. If you have a copy running from an earlier `cargo
run`, start a new one only after that one is gone (`pkill -x video-player`).

## Test it

```bash
cargo test --workspace                    # unit + headless engine tests
cargo clippy --workspace --all-targets    # the tree is kept at 0 warnings
```

The engine tests need no display, no GPU and no audio device: they drive mpv headless with
`ao=null` and check its software renderer against a generated fixture. They create their own
scratch directories under the system temp dir and remove them again, so they behave the same on
every machine and on every rerun. Point `MPV_TEST_VIDEO=/path/to/clip.mkv` at a real clip to run
the render probe against that instead of the fixture.

## Reproduce the build in a container

```bash
docker build -t video-player-build .
docker run --rm -e DISPLAY="$DISPLAY" -v /tmp/.X11-unix:/tmp/.X11-unix \
  video-player-build target/release/video-player some-video.mkv
```

The image is `archlinux:latest` plus the packages above, and it builds with `--locked` — the
quickest way to check the tree builds on a machine with none of this project's setup.

## Arch package

```bash
cp release.env.example release.env   # then set AELUM_GPG_KEY, see the file
./scripts/build-pkg.sh               # → dist/aelum-video-player-<ver>-1-x86_64.pkg.tar.zst
                                     #   + .sha256 + detached .asc signature
./scripts/build-pkg.sh --no-sign     # same package, unsigned, for a test install
```

`build-pkg.sh` checks the PKGBUILD's `pkgver` against the workspace version, packages the tracked
tree, signs the result, and verifies the checksum and signature before it returns. Install the
package on a test host with `sudo pacman -U dist/aelum-video-player-<ver>-1-x86_64.pkg.tar.zst`;
it is not in any repository yet.

## Your mpv configuration

The player reads `~/.config/mpv/mpv.conf` (and its `input.conf` and `scripts/`) like the mpv CLI
does, and your file wins where the two overlap — `autocreate-playlist=no` turns the directory
playlist off, `directory-mode=…` picks how the directory is scanned. Only the options the interface
itself depends on (`vo`, `keep-open`) are set by the player. The resume record lives next to that
file as `~/.config/mpv/last_played`, in mpv's own `key=value` format, so you can read or delete it
by hand. `MPV_HOME` relocates the whole directory, exactly as it does for mpv.

## How it is put together

```
player-core/    engine seam (the PlaybackEngine trait over mpv-engine), the playback state
                fold, the resume record. No GTK dependency, ever.
player-video/   the GLArea render sink; gllink.rs loads the GL symbols by hand (pure FFI)
player-ui/      the only crate that knows libadwaita: main.rs, ui/, theme.rs, i18n.rs, data/
packaging/      PKGBUILD for the Arch package
scripts/        build-pkg.sh
docs/           api-notes.md — API facts that cost real debugging time
```

- [`AGENTS.md`](AGENTS.md) — working notes: the layout, the rules, the pinned stack and the
  reasoning behind it, the commands.
- [`docs/api-notes.md`](docs/api-notes.md) — gtk4-rs / libadwaita / mpv / GL traps worth knowing
  before writing code against them.

## License

GPL-3.0-only — see [LICENSE](LICENSE). The binary links libmpv (GPL), GTK and libadwaita (LGPL);
the workspace crates carry the same license as the rest of the Aelum repositories.
