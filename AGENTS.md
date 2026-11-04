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
- `PROGRESS.md` — the work journal (local file, deliberately **not tracked
  in git**): what was verified and how, known issues, next steps, and the
  environment this box runs. Update it when you verify something
  non-obvious; keep durable API facts in `docs/api-notes.md` instead, and
  machine-specific facts in the journal. If it is missing, you are in a
  fresh clone — work without it or start a new journal.
- The "Stack decisions" section below — what the stack is pinned to and what
  was rejected, so the same choices don't get re-litigated per session.

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

## Stack decisions (verified against upstream 2026-11-03)

Plain `gtk4-rs` + `libadwaita` bindings — no Relm4, no reactive layer of our
own. System library versions a build needs are in `README.md`; what is pinned
here are the crates and the choices around them.

| Crate | Pinned | Why |
|---|---|---|
| `gtk` (`package = "gtk4"`) | 0.11.5, feature `gnome_50` | the rename is required, see api-notes |
| `libadwaita` | 0.9.2, `v1_10` + `gtk_v4_24` | matches the system libraries |
| `mpv-engine` | 0.2.1 (over `rsmpv` 0.2.1) | **chosen**: typed events, property observation, the `attach_gl_render`/`render_gl` seam, software fallback |
| `mpv` | 0.2.3 | **rejected** — frozen since 2017 |

- **The binding hedge.** `mpv-engine` is young (0.2.x), so it stays inside
  `player-core`: its types never cross `engine.rs`. If it stalls, the swap is
  hand-written FFI over `client.h` (~600 lines) or a subprocess + JSON-RPC
  adapter — the UI and the sink don't move.
- **mpv 0.41**: `gpu-next` is the default VO and Vulkan hwdec is preferred.
  Neither touches the client render-API path used here; the GL render API is
  unchanged.
- **No plugin registry, on purpose.** The placeholder `plugin.rs` was deleted
  in the 2026-11-03 refactor: a trait designed without a real plugin drifts.
  The registry lands *with* the first real one (playlist, subtitles, or
  media-info), designed against that plugin's needs; dynamic `.so` plugins
  with a stable ABI come after the trait surface settles.
- **Missing binding, cheapest first**: hand-written FFI for a small header;
  `gir` from `.gir` for a GObject library; `bindgen` + a thin safe wrapper for
  C; `cxx` or a C shim for C++; subprocess + JSON-RPC socket for a
  binary-only library. Every adapter implements an existing `player-core`
  trait, so a missing binding is a bounded task in one crate, never a redesign.
- **Open work**, in the order that keeps the seam honest: plugin registry with
  its first plugin → preferences window, OSD, settings persistence → JSON-RPC
  IPC, then dynamic plugins.

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
  real user (the plugin-registry note in "Stack decisions" is the pattern).
- Pure decision logic belongs in `player-core` with unit tests, not in UI
  code — see `state.rs` for the shape.
- A claim about on-screen behavior is settled by screen capture, not by
  argument (tooling: PROGRESS.md, "Screen capture tooling").
