# AGENTS.md — working notes for coding agents

A modular media player: GTK4 + libadwaita UI, libmpv playback through the
`mpv-engine` crate, Rust Cargo workspace. Binary: `video-player`
(app ID `aelum.videoplayer.Player`).

## Read these first

- **`docs/api-notes.md`** — durable API facts for gtk4-rs 0.11 / glib 0.22 /
  libadwaita 0.9, the mpv render API, and GL/EGL FFI. Check it **before**
  writing code against those APIs: several entries record traps that cost
  real debugging sessions (e.g. `keep-open=always` means no `end-file` at
  EOF; `GestureClick`'s counter climbs past 2 and the binding sticks).
- `PLAN.md` — architecture rules, verified stack decisions, open milestones.
- `PROGRESS.md` — the work journal (local file, deliberately **not tracked
  in git**): what was verified and how, known issues, next steps, and the
  environment this box runs. Update it when you verify something
  non-obvious; keep durable API facts in `docs/api-notes.md` instead, and
  machine-specific facts in the journal. If it is missing, you are in a
  fresh clone — work without it or start a new journal.

## Layout and the one hard rule

- `player-core/` — the engine seam (`PlaybackEngine` trait over `mpv-engine`),
  the `PlaybackState` event fold + resume/restart policy, the last-played
  record. **No GTK dependency, ever.**
- `player-video/` — the GLArea render sink; `gllink.rs` is the raw GL
  symbol loader (pure FFI).
- `player-ui/` — the only crate that knows libadwaita: `main.rs`
  (lifecycle, actions, event pump), `ui/` (`window`, `controls`, `seek`,
  `dnd`), `theme.rs`, `i18n.rs` (locale selection + embedded catalogs —
  same key/fallback contract as the installer frontend), `data/`
  (`style.css`, `translations/`, and the app-identity files: desktop
  entry, icon SVG, metainfo — all named by the app ID).

Front-ends and the sink consume `player-core` through its traits and events
only — UI code never calls mpv directly.

## Commands

```bash
cargo build                                # workspace build
cargo test --workspace                     # incl. headless mpv smoke + sw-render probe
cargo clippy --workspace --all-targets     # must stay at 0 warnings
pkill -x video-player                      # GApplication is single-instance: kill leftovers first
cargo run -p player-ui -- some-video.mkv   # GUI
./scripts/build-pkg.sh                     # Arch .pkg.tar.zst + sha256 + GPG .asc → dist/
```

More (display-path checks, D-Bus driving of a running instance, Docker):
`PROGRESS.md`, section "Commands".

## Conventions

- Comments here document *why*, especially GTK/mpv traps — preserve them
  when moving code, and put a new one where the next trap lives.
- Every file earns its keep: no speculative traits or registries without a
  real user (see the plugin-registry note in PLAN.md for the pattern).
- Pure decision logic belongs in `player-core` with unit tests, not in UI
  code — see `state.rs` for the shape.
- A claim about on-screen behavior is settled by screen capture, not by
  argument (tooling: PROGRESS.md, "Screen capture tooling").
