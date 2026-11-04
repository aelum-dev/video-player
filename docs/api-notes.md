# API & environment notes

Durable reference facts learned the hard way, moved out of PROGRESS.md on
2026-11-03 so the progress journal stays a journal. Verify against upstream
before trusting a version-specific claim.

## Key API facts learned (gtk4-rs 0.11 / glib 0.22 / libadwaita 0.9)
- Crate must be renamed: `gtk = { package = "gtk4", ... }` in workspace deps.
- `GLArea` render/resize are **class vfuncs** → subclass via `GLAreaImpl`
  (`render(&self, &GLContext) -> Propagation`, `resize(w,h)`); realize/unrealize
  via `WidgetImpl`. Wrapper needs `@implements Accessible, Buildable, ConstraintTarget`.
- `gtk::gdk::GLAPI::{GL, GLES}` + `GLArea::set_allowed_apis` (v4_6 API, enabled by
  the `gnome_50` feature) control which context GTK creates.
- GTK creates the GLArea context during `realize` but does **not** make it current —
  `context.make_current()` is required before `attach_gl_render`.
- GTK hands the GLArea's FBO id to us only as the **bound** framebuffer
  (`glGetIntegerv(GL_FRAMEBUFFER_BINDING)`); the render vfunc gets no size argument —
  take it from the `resize` vfunc (physical pixels).
- `glib::MainContext::channel` and `unix_fd_add` are gone in glib 0.22;
  `MainContext::invoke` needs `Send` closures — widgets are neither `Send` nor `Sync`.
- `Propagation::Proceed` (not `Continue`); `set_icon_name(&str)` /
  `StatusPage::set_title(&str)` take plain `&str`; `File::path()` returns `Option`.
- `FileDialog::open(parent, cancellable, callback)` — callback gets
  `Result<gio::File, Error>` directly. `gio::File::uri()` returns `GString`
  (`.into()` → `String`); `FileExt::scheme` is prelude-only and collides with other
  traits' `scheme`, so it is easier not to call it.
- `glib::ObjectWeak` is now `glib::WeakRef` (not usable for widgets).
- GApplication emits `open` **instead of** `activate` when launched with files —
  both handlers must ensure the window exists.
- GTK accelerator strings: keysym names lowercase (`"space"`, not `"Space"`).
- `tracing_subscriber::EnvFilter` had a regex incompatibility in this dependency
  set — use `with_max_level(...)` instead.
- `gl` crate 0.14 is pointer-style only (no `Gl` trait / `Glrs`) — direct
  dlopen/dlsym is simpler for a handful of entry points.
- `mpv_engine::GlRenderOptions` has **no** DRM-display / dmabuf knob — zero-copy
  hwaccel interop would have to go through rsmpv's raw render params.

### Milestone 3 additions
- `gtk::Scale::with_range` **asserts min < max** at construction (a 0..0 seek
  slider aborts the process); start at 0..1 and `set_range` once duration is known.
  `Scale` has `add_mark`/`clear_marks` — there is **no** `remove_mark`.
- Parameterised actions: `SimpleAction::new("seek-rel", Some(&i32::static_variant_type()))`
  (`StaticVariantType` via the prelude; `Type → &VariantTy` conversions don't exist);
  accels bind per value: `app.seek-rel(-5)` → `Left`. **Use `i32`, not `i64`**:
  GTK's accel parser and `gapplication action` both produce `i` (int32); an `x`
  (int64) parameter type rejects them (`Invalid parameter for action: expected x
  but got i`).
- `WidgetExt::add_controller` takes the controller **by value** (`add_controller(ctrl)`,
  not `&ctrl`).
- `EventControllerLegacy` (bubble, non-claiming) can observe `ButtonPress`/
  `ButtonRelease` on a widget **without** stealing the sequence from that widget's
  own gesture — the way to detect a seek-slider grab/release while keeping the
  Scale's native drag alive. `GestureClick`/`GestureDrag` *do* claim, so they
  break the host widget's gesture.
- `EventControllerMotion::connect_motion` fires on **synthetic same-coordinate**
  events too (see the auto-hide note) — filter by coordinate change.
- `MenuButton::set_popover(Some(&gtk::Popover::builder().child(&box).build()))`
  for custom popover content; `is_active()` tells whether it is open.
- `GestureClick` `n_press` distinguishes single/double click on the same button;
  a double click can "win" over a pending single-click timer by timestamp
  comparison — no timer cancellation needed. **But** its multi-press counter
  keeps climbing while clicks stay inside its timing window (3, 4, 5…), so an
  `n_press == 2` binding fires only for the first pair and then appears stuck
  (the "double-click fullscreen stops working" bug). For repeat-pair actions,
  use the non-claiming `EventControllerLegacy` + a timestamp pair state machine
  (`click_pair`, unit-tested) honouring `gtk-double-click-time`.
- `gapplication action aelum.videoplayer.Player <name>` (name **without** the
  `app.` prefix) activates the app's actions over D-Bus — how the running app
  was driven from the CLI for testing, when no input-injection tool is
  installed.
- `glib::monotonic_time()` (µs) + a 500 ms `timeout_add_local` tick is a cheap
  idle detector; no per-motion source churn.

### Drag & drop additions (gtk4-rs 0.11 / GTK 4.24)
- `DropTarget::new` takes a `glib::Type` **by value**. The `drop` signal is
  **not in the auto bindings** — gtk4-rs hand-implies `connect_drop(
  &DropTarget, &glib::Value, x, y) -> bool` in `src/drop_target.rs`; the
  auto-generated `accept` signal takes `&gdk::Drop` (the 4.24 signature).
- `gdk::FileList` IS bound (`files() -> Vec<gio::File>`); a DropTarget of
  that type auto-registers `text/uri-list`. Extract the value with
  `value.get_owned::<gdk::FileList>()` — boxed types only implement
  `FromValue<'static>`, so plain `get()` doesn't fit the borrowed Value.
- `DropTarget`'s default `accept` matches formats *and* actions; the default
  `enter`/`motion` return `make_action_unique(self.actions ∩ drop.actions)`.
  A custom `enter` handler must return a **unique** action from that
  intersection, or GTK logs `critical` and fixes it up.

### Window-move additions (gtk4-rs 0.11 / GTK 4.24)
- `gtk_window_begin_move_drag` is **gone in 4.24**; the API is
  `gdk::ToplevelExt::begin_move(&device, button, x, y, timestamp)` with
  x/y in surface coordinates (`event.position()`) and `event.time()`.
  Get the toplevel via `window.surface()` (`NativeExt`) → downcast.
- GTK4 CSS has **no** `cursor` property — use `WidgetExt::set_cursor_from_name`.
- `WidgetExt::compute_bounds(&other)` returns a **graphene Rect** (f32), not a
  `gdk::Rectangle` — no `contains()`, compare bounds manually.

### Theming additions
- `CssProvider::load_from_*` **replaces** the provider's previous styles —
  loading several sheets one after another leaves only the **last** sheet in
  effect (this silently dropped all sizing rules and the controls rendered at
  libadwaita defaults). Concatenate into one `load_from_bytes`, or use a
  SECOND provider added later at the same priority (later add wins ties —
  that's how the user palette overrides libadwaita's `@define-color` names).
- GTK4 always loads `~/.config/gtk-4.0/gtk.css` at USER priority — above the
  libadwaita theme, so personal palettes silently beat `ForceDark`. To opt
  out: point `XDG_CONFIG_HOME` at a private dir before GTK opens a display
  (glib caches user dirs on first call — use raw `$HOME`, and `set_var` is
  `unsafe` in edition 2024).
- Named palette colors are usable from app CSS: `alpha(@card_bg_color, 0.75)`
  resolves against whatever palette is active (user's or libadwaita's).
- libadwaita tags the window node with `.dark`/`.light` (usable to scope
  custom CSS), and `AdwStyleManager` follows the system via the portal Settings
  interface — on this box xdg-desktop-portal + the **gtk** backend read
  `gsettings org.gnome.desktop.interface color-scheme` (`prefer-dark` flips the
  app live, no restart). `StyleManager::set_color_scheme(ForceLight/ForceDark/
  Default)` overrides.
- Radio menu group: one `SimpleAction::new_stateful(name, Some(VariantTy::STRING),
  &"default".to_variant())` + menu items `app.theme::"light"` etc. — GTK renders
  the matching target checked. `connect_change_state` must call `set_state`.
- `gtk::style_context_add_provider_for_display` (free function) replaces the
  deprecated `StyleContext::add_provider_for_display`.
- `GtkMenuButton`'s CSS node tree is `menubutton ▸ button`: `add_css_class`
  lands on the **`menubutton` wrapper**, while the visible background/fill is
  drawn by the inner `button` node, which carries none of the widget's
  classes. Rules scoped as `.flat button` or `.bar > button` never hit a
  menu button; the matching leg is `menubutton.flat > button` — child
  combinator, not descendant: the popover's own node hangs under that
  inner `button`, so a *descendant* leg (`menubutton.flat button`) also
  styles everything inside the popover (that's how 44/58px min-sizes and
  big labels leaked into the speed/volume dropdowns). Settled with
  red-border probes on both node kinds.

### i18n / text-direction additions (GTK 4.24)
- **Widget direction is NOT inherited.** `window.set_direction(Rtl)` leaves
  every child at LTR — both `widget.direction()` and what is drawn. Settled by
  screen capture: a box whose **own** direction is RTL mirrors its children
  (`[A][B][C]` renders C,B,A), a box under an RTL window does not. So set it on
  each container — `ui::window::apply_rtl` walks `first_child`/`next_sibling`.
- GTK4 has **no `gtk-direction` setting** (the GTK3 `GtkSettings` property is
  gone — not in `Gtk-4.0.gir`), so there is no global switch to lean on.
- Mirroring needs no `CenterBox` and no re-packing: `GtkBox` reverses child
  order once its own direction is RTL.
- **A `MenuButton`'s popover is part of the widget tree** — `menubutton
  .first_child()` is the popover — so a subtree walk covers popover contents
  (verified: the volume/speed boxes came out RTL from one call on the window).
- A `GtkScale` with RTL direction draws its trough **from the right edge**, so
  hand-written pixel→value math must flip with it (`seek::click_fraction`;
  without the flip, clicking the right end of an Arabic UI seeks to the start).
- Directional symbolic icons mirror themselves in RTL (the play triangle
  flipped) — that is GTK doing it, not a bug to work around.
- `set_direction` twice (once via a subtree walk, once directly) is idempotent,
  so overlapping calls cost nothing.

### Desktop-entry localization (the launcher reads no catalog of ours)
- A localized key is a **POSIX locale code, never a BCP-47 code**: GLib's
  `DesktopAppInfo` tries `Name[de_DE.UTF-8]` → `Name[de_DE]` → `Name[de]` and
  then the unlocalized `Name`. `Name[de-DE]` is simply never matched — and
  `desktop-file-validate` 0.28 does not look at the locale part of a key at all,
  so the wrong form is silent. A language-only key covers every region of that
  language (`LC_ALL=pt_BR.UTF-8` picked `Name[pt]`), which is what lets the
  entry resolve like `i18n::select_locale`.
- That lookup reads **`LC_ALL`/`LC_MESSAGES`/`LANG` from the environment**, not
  the C library's locale: calling `setlocale()` without exporting the variables
  leaves the launcher on the unlocalized `Name`.
- The locale lookup string is computed **once per process** (`g_once_init_enter`),
  so probing several locales needs one process per locale.
- `Gio.DesktopAppInfo` is deprecated in this glib — `GioUnix.DesktopAppInfo` is
  the same type; `get_name()` is what a shell shows, `get_locale_string("Name")`
  is the key lookup behind it.

## Key facts learned about the mpv render API
- **`mpv_render_context_render()` leaves the FBO binding at 0.** Any readback must
  bind `GL_READ_FRAMEBUFFER` (0x8CA8) first; and rebind the toolkit's FBO after the
  call so the compositor's state cache stays truthful.
- **`flip_y=true` for GTK's GLArea** (top-left-origin texture). `false` = upside down.
- `load_when_ready` is the right call for the first load: `loadfile` before a render
  context exists fails VO init and **drops the video track** (black window, audio
  plays). After attach it degrades to a plain `load`. `Engine::has_render()` is the
  cheap "is the sink live" check.
- mpv accepts `file://` URLs (verified with `mpv --vo=null --ao=null --frames=1
  file://…`) — useful as the fallback when a GFile has no local path.
- mpv 0.41's GL render path is its own `video/out/gpu/video.c` +
  `video/out/opengl/ra_gl.c` (NOT libplacebo — that's gpu-next).
- mpv installs its KHR_debug callback only with `gpu-debug=yes`; `gl_set_debug_logger`
  needs `gl->debug_context`, which the render-API path sets from that option.
- mpv never calls `glColorMask` / never touches `GL_RASTERIZER_DISCARD`.
- `glGetError` is per-context and sticky; mpv's "after X: OpenGL error" lines are its
  own checkpoints draining whatever was pending — the generator may be elsewhere.
- `glGetIntegerv(GL_COLOR_WRITERED…)` is NOT accepted by Mesa (INVALID_ENUM).
- vo_libmpv `flip_page`: if `mpv_render_context_render()` doesn't consume a frame
  within 200 ms it logs "not being called or stuck" and drops it. Not seen now.
- `EngineBuilder::property` = `mpv_set_property` before init: works for options that
  are also properties, fails with `-8 MPV_ERROR_PROPERTY_NOT_FOUND` for pure options
  (`sw`, `dither-depth`, `target-prim`, `background`, `chapter-file`). Needs an
  `mpv_set_option_string` seam for those.
- **`keep-open=always` means there is no `end-file` at EOF.** The file stays
  loaded: mpv sets `pause=yes` and the `eof-reached` property, and `end-file`
  only arrives later, when the file is replaced (`reason: Stop`) or the player
  quits. So "this title is over" must be observed as `observe("eof-reached",
  Flag)` — an `EndReason::Eof` arm never runs in this configuration (that cost a
  round of "Play at the end does nothing" debugging).
- **`keep-open=yes`** (the app's setting since directory playlists) advances to
  the next playlist entry at EOF — `end-file (EndOfFile)` + `file-loaded`
  arrive as at any replace-load — and only at the END of the playlist holds the
  last frame the way `always` does. `eof-reached` remains the end signal there.

## Key facts learned about mpv playlists & directory autoload (mpv 0.41)
- `autocreate-playlist=<no|filter|same>` (mpv 0.39+, the native successor of
  `autoload.lua`) builds a playlist from the opened file's directory, placing
  the opened file at its sorted position; `same` keeps only the category of
  the opened file (`--video-exts` etc.). It IS runtime-settable via
  `set_property` (verified 0.41), so it belongs in `reassert_app_options`,
  after the user's mpv.conf has had its say.
- `directory-mode=ignore` keeps that playlist flat. The default (`auto` →
  `lazy`) contributes subdirectory entries too when a single file is opened.
- The step commands are `playlist-next` / `playlist-prev` — the name is NOT
  "playlist-previous" (an unknown command whose error is easy to misread as a
  playlist problem).
- Their default flag is `weak`: at the first/last entry the command **returns
  an error** (`-12 INVALID_PARAMETER`) and changes nothing; playback is not
  stopped (that is what `force` does). Callers log-and-ignore, so the controls
  never need disabling or position pre-checks.
- The `path` property (`Str`, observable) is the authoritative "what is
  loaded now": it follows playlist steps the front-end did not initiate. Its
  change event lands AFTER `file-loaded` (probed on 0.41) — anything that
  names the new file (window title, logs) must read the fold after the
  whole event batch, not at the `Loaded` event. When idle `path` goes
  unavailable — mpv-engine drops unavailable payloads, so no event arrives
  and the folded source never vanishes on its own; the empty-string guard
  in `state.rs` is defensive.
- At a playlist end, `weak` steps and `add chapter` on a chapterless file
  both return the SAME error (`-12 INVALID_PARAMETER`) — you cannot tell
  which leg ran from the error code alone.
- `chapter-file` is a pure option (set_property → `-8 PROPERTY_NOT_FOUND`)
  and `chapter-list` cannot be SET from a string (`-9 UNSUPPORTED_FORMAT`,
  it wants a native node) — so a chaptered fixture cannot be conjured
  through the current crate; it joins the `mpv_set_option_string` seam list.

## Key facts learned about GL/EGL FFI
- `glGetString(GL_EXTENSIONS)` returns **NULL** in a 4.6 core context — use
  `glGetIntegerv(GL_NUM_EXTENSIONS=0x821D)` + `glGetStringi`.
- `GL_SAMPLES` (current FBO's sample count) = **0x80A9**; 0x8D57 is `GL_MAX_SAMPLES`.
  `GL_CONTEXT_FLAGS` = 0x821E. `GL_FRAMEBUFFER_BINDING` = 0x8CA6.
- `EGLBoolean` is `khronos_uint8_t` — FFI must declare those returns as `u8`.
- EGL constants that bit us: `EGL_RENDERABLE_TYPE` = **0x3040**,
  `EGL_CONTEXT_OPENGL_PROFILE_MASK` = **0x30FD**, `EGL_CONFIG_ID` = **0x3028**.
- `eglGetError` is sticky-ish: drain it before a call.
- Reading the default framebuffer (fbo 0) inside a GTK GLArea render vfunc →
  `GL_INVALID_FRAMEBUFFER_OPERATION`; not a usable probe.
- KHR_debug only emits messages for **debug** contexts
  (`gdk_gl_context_set_debug_enabled(true)` before `realize`).

## Testing environment

How the project is exercised. Box-specific state (GPU, monitor, scale
value, personal media paths, pinned versions) deliberately stays out —
that lives in the untracked journal; the versions a build needs are in
README.md, the crate pins in AGENTS.md.

- Wayland under Hyprland (a wlroots-class compositor). Two display-path
  consequences: Xwayland there is rootless — an x11grab of its root is
  always black, so settle display-path claims from a Wayland output
  capture; and a client-initiated window move (`xdg_toplevel.move`, the
  header-drag code) is ignored unless the compositor honors interactive
  drags (Hyprland ≥ 0.56).
- Fractional display scale is the norm on the dev setup: the
  `GLArea::resize` vfunc reports **physical** pixels while client
  geometry tools (`hyprctl clients -j`) report logical — convert before
  comparing any on-screen measurement.
- Test media fixture (silent-tone video, self-contained):
  `ffmpeg -y -f lavfi -i testsrc2=duration=8:size=640x360:rate=30
  -f lavfi -i sine=frequency=440:duration=8 -c:v libx264 -pix_fmt yuv420p
  -c:a aac test.mkv`
- For manual path-handling checks, use real clips whose names contain
  spaces and non-ASCII characters — generated fixtures never exercise
  that path.
