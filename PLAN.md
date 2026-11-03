# Modular GTK4 Media Player with libmpv — Implementation Plan

Verified against upstream sources: 2026-11-03.

## Goal

A modern, beautiful, modular media player: GTK4 + libadwaita UI, libmpv playback
backend, extensible plugin architecture, reproducible Docker builds.

## Verified stack (Nov 2026)

| Component | Host has | Latest upstream | Status |
|---|---|---|---|
| GTK 4 | 4.24.1 | 4.24.x | current |
| libadwaita | 1.10.0 | 1.10 stable (1.11.alpha in dev) | current |
| mpv / libmpv | v0.41.0 (libmpv 2.5.0) | v0.41.0 | current |
| `gtk4` crate | — | 0.11.5 (Sep 2026), features through `gnome_50` | active |
| `libadwaita` crate | — | 0.9.2 (Jul 2026), features `v1_10` + `gtk_v4_24` | matches host |
| `mpv` crate (old pick) | — | 0.2.3, frozen since 2017 | **replaced** |
| `mpv-engine` crate | — | 0.2.1 (Oct 2026) | **chosen** |
| `rsmpv` / `rsmpv-sys` | — | 0.2.1 / 0.1.1 (Sep 2026) | foundation of mpv-engine |

### Key decisions
- **Language: Rust**, plain `gtk4-rs` + `libadwaita` bindings (no Relm4).
- **mpv integration: `mpv-engine` 0.2.1** — toolkit-agnostic libmpv embedding core
  (typed `PlaybackEvent`s, property observation, `attach_gl_render`/`render_gl`
  GLArea seam, software fallback, DMA-BUF export feature for later). Built on
  `rsmpv` safe FFI. MIT/Apache-2.0.
- **Risk hedge**: `mpv-engine` is young (v0.2.x). `player-core` wraps it behind
  our own trait; fallbacks are hand-written FFI over `client.h` (~600 lines) or
  a subprocess + JSON-RPC adapter. UI and plugins never touch the binding layer.
- **mpv 0.41 note**: `gpu-next` is now the default VO and Vulkan hwdec is
  preferred — irrelevant to the client render API path we use; GL render API is
  unchanged. Requires FFmpeg 6.1+ / libplacebo 6.338.2+ (satisfied by host).

## Architecture

```
video-player/                 # Cargo workspace (binary: video-player)
├── Cargo.toml
├── Dockerfile                # Arch base matching the host stack
├── player-core/              # NO UI/GTK deps: engine adapter, state model, resume record
│   ├── engine.rs             # our PlaybackEngine trait + mpv-engine adapter (event pump → typed bus)
│   ├── state.rs              # PlaybackState: event fold + resume/restart policy
│   └── last_played.rs        # resume record, written into mpv's own config dir
├── player-video/             # GLArea render sink (gllink.rs = raw GL symbol loader)
├── player-ui/                # libadwaita app: main.rs (lifecycle, actions, event pump),
│                             # ui/ (window, controls, seek, dnd), theme.rs, data/style.css
└── player-ipc/               # JSON-RPC over local socket + D-Bus (later phase)
```

Dependency rule: `player-core` depends on nothing GTK. UI, video, and IPC consume
its event stream. A future TUI/web front-end plugs into the same core.

### Plugin architecture (static now, dynamic later)
- Static registry of built-in modules: `playback`, `playlist`, `subtitles`,
  `media-info`, `screenshot`, `equalizer` (mpv `af=equalizer`).
- The placeholder `plugin.rs` was removed during the 2026-11-03 refactor: it
  had no users, and a trait designed without a real plugin drifts. The
  registry lands together with the first real plugin (Milestone 4), designed
  against that plugin's actual needs.
- Each plugin contributes: event handlers, app actions/shortcuts, optional UI
  sections (header-bar button, preferences page).
- Phase 2: `libloading`-based `.so` plugins with a stable ABI struct once the
  trait surface settles.

### UI design (libadwaita)
- `AdwApplicationWindow` + `AdwToolbarView`: adaptive header bar (title, media
  menu) + bottom playback bar — play/pause, prev/next, seek slider with chapter
  ticks and hover preview, time labels, volume popover, speed popover, fullscreen.
- Collapsible `AdwFlap` sidebar: playlist / track list.
- OSD toast overlays, `AdwPreferencesWindow`, keyboard shortcuts window.
- `style.css` with accent color, light/dark following the system.
- All controls drive the core through typed commands, never mpv directly.

### Video output
- libmpv render API via `mpv-engine` GL seam: `GtkGLArea` paint →
  `engine.render_gl()` with `GlRenderOptions` tuned for a toolkit paint loop.
- Native Wayland/X11 embedding, correct aspect on resize, no reparenting.
- Sink behind a trait → software fallback and DMA-BUF/GSK path remain options.

## Missing-bindings playbook (for future dependencies)

1. **Generate**: `bindgen` from C headers → `foo-sys` + hand-written safe `foo`
   wrapper; `pkg-config`/`cc` crates for build glue. GObject libs: `gir` from
   `.gir` introspection (how gtk4-rs itself is made).
2. **Hand-write FFI** for small headers (< a few thousand lines).
3. **C++ libs**: `cxx` crate or bindgen + thin C shim.
4. **Binary-only libs**: subprocess + JSON-RPC socket adapter (crash-isolated).

All adapters implement existing `player-core` traits, so a missing binding is a
bounded task in one crate, never a redesign.

## Milestones

1. **Scaffold** — Cargo workspace, Dockerfile, window shell, engine init,
   open-file/CLI args.
2. **Video** — GLArea render sink, fullscreen, resize-correct aspect.
3. **Controls** — playback bar, seek/volume, keyboard shortcuts, window bindings.
4. **Plugins** — registry + first three plugins (playlist, subtitles, media-info).
5. **Polish** — CSS theme, preferences window, OSD, settings persistence.
6. **Extensibility** — dynamic `.so` plugin loading + JSON-RPC IPC.

## Verification

- `cargo build` + `cargo clippy` on host (toolchain already present).
- `docker build` on freedesktop SDK image to prove reproducibility.
- Smoke test against sample video files.

## Pinned dependencies

```toml
[workspace.dependencies]
gtk4       = { version = "0.11.5", features = ["gnome_50"] }
libadwaita = { version = "0.9.2", features = ["v1_10", "gtk_v4_24"] }
mpv-engine = { version = "0.2.1" }   # GL seam only; no wgpu/export yet
anyhow     = "1"
tracing    = "0.1"
```
