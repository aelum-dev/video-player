//! mpv-gtk — the GTK shell: application, actions, and the event pump.
//!
//! Opens files from the CLI or Ctrl+O and plays them through the core engine;
//! the window itself (video surface with controls floating over it) is built
//! by [`ui`]. This file only wires application lifecycle, actions/accels, and
//! the event pump that keeps the UI in sync from the core's event stream.
//!
//! All UI state lives on the GTK main thread (`Rc<RefCell<_>>`); only the
//! engine crosses threads (`Arc<dyn PlaybackEngine>`, mpv's render/event
//! threads).

mod format;
mod ui;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::prelude::*;
use libadwaita as adw;
use player_core::{MpvEngine, PlaybackEngine, PlaybackState, PlayerEvent};

const APP_ID: &str = "dev.mpvgtk.Player";
/// Seconds jumped by the Left/Right keys (mpv's default).
const SEEK_STEP_SECS: i64 = 5;
/// Volume percentage stepped by the Up/Down keys.
const VOLUME_STEP: i64 = 5;

/// State shared between actions, the window, and the event pump.
/// Main-thread only except `engine`.
#[derive(Clone)]
struct Shared {
    engine: Arc<MpvEngine>,
    state: Rc<RefCell<PlaybackState>>,
    window: Rc<RefCell<Option<adw::ApplicationWindow>>>,
    toasts: Rc<RefCell<Option<adw::ToastOverlay>>>,
    controls: Rc<RefCell<Option<ui::Controls>>>,
}

fn main() -> gtk::glib::ExitCode {
    isolate_user_gtk_css();

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let app = adw::Application::new(Some(APP_ID), gtk::gio::ApplicationFlags::HANDLES_OPEN);

    // Video engine (vo=libmpv): loads are deferred by `load_when_ready`
    // until the VideoArea attaches the GL render context.
    let engine: Arc<MpvEngine> =
        Arc::new(MpvEngine::video().expect("failed to initialize playback engine"));
    let shared = Shared {
        engine,
        state: Rc::new(RefCell::new(PlaybackState::default())),
        window: Rc::new(RefCell::new(None)),
        toasts: Rc::new(RefCell::new(None)),
        controls: Rc::new(RefCell::new(None)),
    };

    app.connect_startup(move |_| {
        setup_css();
        // Dark chrome only: no light mode anywhere. Without this the menus
        // and popovers would follow the system setting (light on this box).
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    });
    app.connect_activate({
        let shared = shared.clone();
        move |app| ensure_window(app, &shared)
    });
    app.connect_open({
        let shared = shared.clone();
        move |app, files, _hint| {
            ensure_window(app, &shared);
            for file in files {
                if let Some(path) = file.path() {
                    open_source(&shared, &path.to_string_lossy());
                }
            }
        }
    });

    setup_actions(&app, &shared);
    start_event_pump(&shared);

    app.run()
}

/// GTK4 always loads `~/.config/gtk-4.0/gtk.css` at USER priority — above
/// the libadwaita theme, so a personal palette (this box has a Matugen/DMS
/// override redefining libadwaita's own `@define-color` names like
/// `popover_bg_color`) silently beats `ForceDark`. The app is dark-only,
/// so the automatic load is disabled: `XDG_CONFIG_HOME` is pointed at a
/// private dir carrying symlinks to the non-color config (`settings.ini`
/// for the icon theme, `fontconfig` for fonts). The user palette is then
/// applied *conditionally* by `maybe_load_user_palette` — GTK never finds
/// a user `gtk.css`. Must run before GTK opens a display and before glib
/// caches the config dirs — hence the raw `$HOME`, no `glib::user_config_dir()`.
fn isolate_user_gtk_css() {
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    let real = std::path::Path::new(&home).join(".config");
    let private = std::path::Path::new(&home)
        .join(".local/share/dev.mpvgtk.Player/gtk-config");
    if std::fs::create_dir_all(private.join("gtk-4.0")).is_err() {
        return;
    }
    for rel in ["gtk-4.0/settings.ini", "fontconfig"] {
        let src = real.join(rel);
        if !src.exists() {
            continue;
        }
        let dst = private.join(rel);
        let _ = std::fs::remove_file(&dst);
        let _ = std::os::unix::fs::symlink(&src, &dst);
    }
    // Safe: first thing in `main`, single-threaded, before any library
    // reads the environment.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &private) };
}

fn setup_css() {
    let provider = gtk::CssProvider::new();
    // One load: each `load_from_*` on a provider *replaces* the previous
    // styles, so loading several sheets separately would leave only the
    // last one in effect. The controls are always dark-styled here; the
    // system dark/light scheme only drives libadwaita chrome (menus,
    // popovers, dialogs), which themes itself through `AdwStyleManager`.
    provider.load_from_bytes(&gtk::glib::Bytes::from_static(include_bytes!(
        "../data/style.css"
    )));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        // The user palette goes in a SECOND provider added after ours: a
        // second `load_from_*` on the same provider would *replace* our
        // styles (the sizing-dropping gotcha). Same priority + later add
        // = its `@define-color` palette wins over libadwaita's.
        if let Some(palette) = user_palette_provider() {
            gtk::style_context_add_provider_for_display(
                &display,
                &palette,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    }
}

/// This player is the distro's player: it should wear the user's matugen
/// palette, not stock Adwaita. The palette file is only honored while its
/// current variant is *dark* (the app is dark-only; a light variant would
/// clash with the always-dark controls) — when DMS/matugen regenerates it
/// light, the app falls back to libadwaita's dark palette. `load_from_path`
/// keeps relative `@import`s resolving against the user's file.
/// (A palette change needs an app restart; DMS rewrites the file on switch.)
fn user_palette_provider() -> Option<gtk::CssProvider> {
    let home = std::env::var("HOME").ok()?;
    let user_css = std::path::Path::new(&home).join(".config/gtk-4.0/gtk.css");
    if !user_css.is_file() {
        return None;
    }
    let text = read_css_chain(&user_css)?;
    if !user_palette_is_dark(&text) {
        tracing::info!("user GTK palette is light; keeping libadwaita dark");
        return None;
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_path(&user_css);
    tracing::info!("using user GTK palette (dark variant)");
    Some(provider)
}

/// The user's `gtk.css` plus the files it `@import`s (one level - the
/// matugen/DMS layout).
fn read_css_chain(path: &std::path::Path) -> Option<String> {
    let mut text = std::fs::read_to_string(path).ok()?;
    let dir = path.parent()?;
    let imports: Vec<String> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("@import").map(str::to_owned))
        .collect();
    for rest in imports {
        let Some(start) = rest.find('"').or_else(|| rest.find('\'')) else {
            continue;
        };
        let quote = rest.as_bytes()[start] as char;
        let Some(end) = rest[start + 1..].find(quote) else {
            continue;
        };
        let target = dir.join(&rest[start + 1..start + 1 + end]);
        if let Ok(extra) = std::fs::read_to_string(&target) {
            text.push('\n');
            text.push_str(&extra);
        }
    }
    Some(text)
}

/// Decide from the palette's own background colors: the last
/// `window_bg_color`/`popover_bg_color` hex definition wins (CSS order);
/// luma below the midpoint means dark. No background defined at all →
/// treat as dark (the file cannot make the light chrome appear).
fn user_palette_is_dark(css: &str) -> bool {
    let hex = ["window_bg_color", "popover_bg_color"]
        .iter()
        .find_map(|name| last_hex_define(css, name));
    match hex {
        Some((r, g, b)) => 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b) < 128.0,
        None => true,
    }
}

/// Last `@define-color <name> #rgb|#rrggbb;` in the text.
fn last_hex_define(css: &str, name: &str) -> Option<(u8, u8, u8)> {
    let needle = format!("@define-color {name}");
    let mut found = None;
    let mut from = 0;
    while let Some(rel) = css[from..].find(&needle) {
        let at = from + rel + needle.len();
        let rest = css[at..].trim_start();
        let mut chars = rest.chars();
        if chars.next() == Some('#') {
            let digits: String = chars.take_while(|c| c.is_ascii_hexdigit()).collect();
            let rgb = if digits.len() == 3 {
                Some((
                    u8::from_str_radix(&digits[0..1], 16).ok()? * 17,
                    u8::from_str_radix(&digits[1..2], 16).ok()? * 17,
                    u8::from_str_radix(&digits[2..3], 16).ok()? * 17,
                ))
            } else if digits.len() >= 6 {
                Some((
                    u8::from_str_radix(&digits[0..2], 16).ok()?,
                    u8::from_str_radix(&digits[2..4], 16).ok()?,
                    u8::from_str_radix(&digits[4..6], 16).ok()?,
                ))
            } else {
                None
            };
            if rgb.is_some() {
                found = rgb;
            }
        }
        from = at;
    }
    found
}

fn ensure_window(app: &adw::Application, shared: &Shared) {
    if shared.window.borrow().is_none() {
        *shared.window.borrow_mut() = Some(ui::build_window(app, shared));
    }
    if let Some(win) = shared.window.borrow().clone() {
        win.present();
    }
}

fn setup_actions(app: &adw::Application, shared: &Shared) {
    let open = gtk::gio::SimpleAction::new("open", None);
    open.connect_activate({
        let shared = shared.clone();
        move |_, _| {
            let Some(win) = shared.window.borrow().clone() else {
                return;
            };
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Open media");
            dialog.set_modal(true);
            dialog.open(
                Some(&win),
                None::<&gtk::gio::Cancellable>,
                {
                    let shared = shared.clone();
                    move |result: Result<gtk::gio::File, gtk::glib::Error>| {
                        // Err = cancelled by the user.
                        let Ok(file) = result else {
                            return;
                        };
                        // What the chooser actually gave us matters: under a
                        // file portal the returned GFile can be a document
                        // (no local path), and mpv only understands paths and
                        // URLs it can open itself.
                        let uri: String = file.uri().into();
                        let local = file.path();
                        tracing::info!(
                            %uri,
                            native = file.is_native(),
                            local = ?local,
                            "file chooser selection"
                        );
                        let source = match local {
                            Some(p) => p.to_string_lossy().into_owned(),
                            None => {
                                tracing::warn!(%uri, "selection has no local path, passing the URI to mpv");
                                uri
                            }
                        };
                        open_source(&shared, &source);
                    }
                },
            );
        }
    });
    app.add_action(&open);

    let play_pause = gtk::gio::SimpleAction::new("play-pause", None);
    play_pause.connect_activate({
        let shared = shared.clone();
        move |_, _| {
            if let Err(err) = shared.engine.toggle_play() {
                tracing::warn!(%err, "play/pause failed");
            }
        }
    });
    app.add_action(&play_pause);

    // Parameterised with the offset in seconds, so the accels can be
    // `app.seek-rel(-5)` and `app.seek-rel(5)`.
    let seek_rel = gtk::gio::SimpleAction::new("seek-rel", Some(&i32::static_variant_type()));
    seek_rel.connect_activate({
        let shared = shared.clone();
        move |_, parameter| {
            let secs = parameter.and_then(|p| p.get::<i32>()).unwrap_or(0) as f64;
            if let Err(err) = shared.engine.seek_relative(secs) {
                tracing::warn!(%err, "seek failed");
            }
        }
    });
    app.add_action(&seek_rel);

    for (name, delta) in [("chapter-prev", -1), ("chapter-next", 1)] {
        let action = gtk::gio::SimpleAction::new(name, None);
        let engine = shared.engine.clone();
        action.connect_activate(move |_, _| {
            if let Err(err) = engine.chapter_step(delta) {
                // No chapters in the file: mpv answers with an error.
                tracing::debug!(%err, "chapter step failed");
            }
        });
        app.add_action(&action);
    }

    let volume_rel = gtk::gio::SimpleAction::new("volume-rel", Some(&i32::static_variant_type()));
    volume_rel.connect_activate({
        let shared = shared.clone();
        move |_, parameter| {
            let delta = parameter.and_then(|p| p.get::<i32>()).unwrap_or(0) as f64;
            let target = (shared.state.borrow().volume + delta).clamp(0.0, 130.0);
            if let Err(err) = shared.engine.set_volume(target) {
                tracing::warn!(%err, "set_volume failed");
            }
        }
    });
    app.add_action(&volume_rel);

    let mute = gtk::gio::SimpleAction::new("mute", None);
    mute.connect_activate({
        let shared = shared.clone();
        move |_, _| {
            let muted = !shared.state.borrow().muted;
            if let Err(err) = shared.engine.set_muted(muted) {
                tracing::warn!(%err, "set_muted failed");
            }
        }
    });
    app.add_action(&mute);

    let fullscreen = gtk::gio::SimpleAction::new("fullscreen", None);
    fullscreen.connect_activate({
        let window_slot = shared.window.clone();
        move |_, _| {
            let Some(win) = window_slot.borrow().clone() else {
                return;
            };
            if win.is_fullscreen() {
                win.unfullscreen();
            } else {
                win.fullscreen();
            }
        }
    });
    app.add_action(&fullscreen);

    let quit = gtk::gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let app = app.clone();
        let window_slot = shared.window.clone();
        move |_, _| {
            match window_slot.borrow().clone() {
                Some(win) => win.close(),
                None => app.quit(),
            }
        }
    });
    app.add_action(&quit);

    app.set_accels_for_action("app.open", &["<Ctrl>o"]);
    app.set_accels_for_action("app.quit", &["<Ctrl>q"]);
    app.set_accels_for_action("app.play-pause", &["space"]);
    app.set_accels_for_action(&format!("app.seek-rel(-{SEEK_STEP_SECS})"), &["Left"]);
    app.set_accels_for_action(&format!("app.seek-rel({SEEK_STEP_SECS})"), &["Right"]);
    app.set_accels_for_action("app.chapter-prev", &["<Primary>Left"]);
    app.set_accels_for_action("app.chapter-next", &["<Primary>Right"]);
    app.set_accels_for_action(&format!("app.volume-rel({VOLUME_STEP})"), &["Up"]);
    app.set_accels_for_action(&format!("app.volume-rel(-{VOLUME_STEP})"), &["Down"]);
    app.set_accels_for_action("app.mute", &["m"]);
    app.set_accels_for_action("app.fullscreen", &["f", "F11"]);
}

fn open_source(shared: &Shared, path: &str) {
    tracing::info!(path, "opening source");
    shared.state.borrow_mut().set_source(path);
    if let Err(err) = shared.engine.open(path) {
        tracing::error!(%err, path, "open failed");
        toast(shared, &format!("Could not open {path}: {err}"));
        return;
    }
    let title = shared.state.borrow().title();
    let window = shared.window.borrow().clone();
    if let (Some(title), Some(win)) = (title, window) {
        win.set_title(Some(&title));
    }
}

fn toast(shared: &Shared, text: &str) {
    if let Some(overlay) = shared.toasts.borrow().as_ref() {
        overlay.add_toast(adw::Toast::new(text));
    }
}

/// Drains the core's event queue on the GTK main loop, folds the events into
/// `PlaybackState`, and refreshes the floating controls. 100 ms keeps the
/// seek slider visibly smooth; an engine wakeup callback can drop this timer
/// later.
fn start_event_pump(shared: &Shared) {
    gtk::glib::timeout_add_local(Duration::from_millis(100), {
        let shared = shared.clone();
        move || {
            let events = shared.engine.pump_events();
            let mut loaded = false;
            {
                let mut state = shared.state.borrow_mut();
                for event in &events {
                    state.apply(event);
                    match event {
                        PlayerEvent::Loaded => {
                            loaded = true;
                            tracing::info!(title = ?state.title(), "media loaded");
                            if let (Some(title), Some(win)) =
                                (state.title(), shared.window.borrow().clone())
                            {
                                win.set_title(Some(&title));
                            }
                        }
                        PlayerEvent::Failed { message, .. } => {
                            toast(&shared, &format!("Playback failed: {message}"));
                        }
                        _ => {}
                    }
                }
            }
            if let Some(controls) = shared.controls.borrow().as_ref() {
                // Chapter markers only exist once the title is loaded.
                if loaded {
                    controls.set_chapters(&shared.engine.chapters());
                }
                controls.refresh(&shared.state.borrow());
            }
            gtk::glib::ControlFlow::Continue
        }
    });
}

#[cfg(test)]
mod theme_tests {
    use super::{last_hex_define, user_palette_is_dark};

    const DARK: &str = r#"
        @define-color window_bg_color #0f0d12;
        @define-color popover_bg_color #1b181f;
    "#;

    const LIGHT: &str = r#"
        @define-color window_bg_color #fff7f9;
        @define-color popover_bg_color #f9eaf1;
    "#;

    #[test]
    fn dark_palette_is_detected() {
        assert!(user_palette_is_dark(DARK));
    }

    #[test]
    fn light_palette_is_detected() {
        assert!(!user_palette_is_dark(LIGHT));
    }

    #[test]
    fn last_definition_wins() {
        let css = format!("{LIGHT}\n@define-color window_bg_color #000000;");
        assert!(user_palette_is_dark(&css));
    }

    #[test]
    fn short_hex_and_named_colors_parse() {
        assert_eq!(last_hex_define(DARK, "window_bg_color"), Some((15, 13, 18)));
        assert_eq!(
            last_hex_define("@define-color x #abc;", "x"),
            Some((0xaa, 0xbb, 0xcc))
        );
        // A reference, not a hex: no value, so the palette stays "dark" by
        // default rather than guessing a light chrome.
        assert_eq!(last_hex_define("@define-color x @window_bg_color;", "x"), None);
        assert!(user_palette_is_dark("@define-color accent_bg_color #cdc0ec;"));
    }
}
