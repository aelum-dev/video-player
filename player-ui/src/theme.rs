//! Theming: application styles, the user's palette, and following the system.
//!
//! Three separate things live here, and keeping them separate is the point:
//!
//! * **Chrome** — menus, popovers, dialogs. `AdwStyleManager` follows the
//!   xdg-desktop-portal Settings interface on its own, and DMS publishes the
//!   dark/light switch there (`org.freedesktop.appearance color-scheme` 0/1,
//!   with the matching `org.gnome.desktop.interface color-scheme` gsetting),
//!   so following the system takes no code from us. Nothing is forced.
//! * **The user's palette** — matugen/DMS rewrite `~/.config/gtk-4.0/
//!   dank-colors.css`, which `~/.config/gtk-4.0/gtk.css` imports. GTK loads
//!   that file once, at startup, at USER priority (above the libadwaita
//!   theme), and gives no way to ask it to re-read — so a palette change used
//!   to need an app restart. We mirror it into a provider we own and reload
//!   that when the directory changes; the palette and the chrome variant come
//!   from the same DMS switch, so they stay in step.
//! * **The controls over the video** — the fullscreen playback pill, the
//!   center play row, the volume OSD and the drop hint wear libadwaita's
//!   own `.osd` style class: dark shell, white ink and the over-video
//!   accent are all theme-drawn. `data/style.css` adds only shape and
//!   sizes for those, plus two translucent troughs that must stay visible
//!   over any picture. The windowed chrome (header bar, bottom toolbar) is
//!   plain libadwaita and gets no app rules at all.
//!
//! SIGHUP is ignored — see [`ignore_hangup`].

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;

/// One palette write is really several: matugen rewrites the palette file, the
/// importing `gtk.css` and `settings.ini` in a single pass. Coalesce a burst
/// into one reload.
const RELOAD_DEBOUNCE_MS: u64 = 300;

/// The palette watcher. It is kept alive only by this struct, so whoever
/// installs the theme has to hold on to the token — dropping it stops the hot
/// reload.
pub struct Theme {
    _watcher: Option<gtk::gio::FileMonitor>,
}

impl Theme {
    /// Adds the application stylesheet and the user-palette mirror to the
    /// default display and starts watching `~/.config/gtk-4.0`. Call from
    /// `connect_startup`: before that there is no display to style.
    pub fn install() -> Option<Theme> {
        let display = gtk::gdk::Display::default()?;

        let app_css = gtk::CssProvider::new();
        // One load per provider: every `load_from_*` *replaces* what the
        // provider had, so several sheets must be concatenated or split across
        // providers (splitting them wrongly once dropped all the sizing).
        app_css.load_from_bytes(&gtk::glib::Bytes::from_static(include_bytes!(
            "../data/style.css"
        )));
        gtk::style_context_add_provider_for_display(
            &display,
            &app_css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        // The palette mirror: a SECOND provider, added after ours, at USER
        // priority — where GTK itself puts the user's file, above the
        // libadwaita theme, and later-added wins ties. It has to be ours
        // because GTK reads the user's file once at startup and cannot be
        // asked to re-read it; only a provider we own can be reloaded.
        let palette = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &display,
            &palette,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        let user_css = gtk::glib::user_config_dir().join("gtk-4.0/gtk.css");
        reload_palette(&palette, &user_css);

        let watcher = watch_palette_dir(&user_css, &palette);
        if watcher.is_none() {
            tracing::debug!(path = %user_css.display(), "no palette watcher active");
        }
        Some(Theme { _watcher: watcher })
    }
}

/// Ignores SIGHUP.
///
/// DMS's theme switch ends with `pkill -HUP -f 'gtk'` — a "reload, GTK apps"
/// nudge whose pattern matches any command line containing `gtk`, this binary's
/// path included. SIGHUP's default disposition is terminate, which is the whole
/// of the "the player dies when I toggle dark mode" report: exit status 129,
/// no coredump, no panic line, and only ever for a binary named `*-gtk`.
/// A player launched from a launcher has no terminal to be hung up on, so the
/// signal is ignored rather than fatal. Call it before anything can be killed.
pub fn ignore_hangup() {
    // Safe: a signal disposition change, made first thing in `main` while the
    // process is still single-threaded; nothing else here claims SIGHUP
    // (libmpv installs no handler for it).
    unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
}

/// Watches the directory holding the user's stylesheet. The directory, not the
/// file: matugen rewrites the *imported* palette file and leaves `gtk.css`
/// alone, so the file monitor would never see the change.
fn watch_palette_dir(user_css: &Path, palette: &gtk::CssProvider) -> Option<gtk::gio::FileMonitor> {
    let dir = user_css.parent()?;
    let monitor = gtk::gio::File::for_path(dir)
        .monitor_directory(gtk::gio::FileMonitorFlags::empty(), gtk::gio::Cancellable::NONE)
        .inspect_err(|err| {
            tracing::warn!(%err, path = %dir.display(), "cannot watch the palette directory");
        })
        .ok()?;

    let path = user_css.to_path_buf();
    let palette = palette.clone();
    let pending: Rc<RefCell<Option<gtk::glib::SourceId>>> = Rc::default();
    monitor.connect_changed(move |_monitor, file, _other, event| {
        use gtk::gio::FileMonitorEvent as Ev;
        if !matches!(event, Ev::Changed | Ev::ChangesDoneHint | Ev::Created | Ev::Deleted | Ev::MovedIn | Ev::MovedOut | Ev::Renamed)
        {
            return;
        }
        // Stylesheets only: `settings.ini` and friends are GTK's business.
        let is_css = file
            .path()
            .is_some_and(|p| p.extension().is_some_and(|ext| ext == "css"));
        if !is_css {
            return;
        }
        tracing::debug!(path = %file.path().unwrap_or_default().display(), "palette file changed");
        schedule_reload(palette.clone(), path.clone(), pending.clone());
    });
    Some(monitor)
}

/// Defers the reload a moment and collapses the writes of one matugen pass
/// into it. Replaces any pending reload, so the timer always sees the final
/// state of the files.
fn schedule_reload(
    palette: gtk::CssProvider,
    path: PathBuf,
    pending: Rc<RefCell<Option<gtk::glib::SourceId>>>,
) {
    if let Some(id) = pending.borrow_mut().take() {
        id.remove();
    }
    let slot = pending.clone();
    let id = gtk::glib::timeout_add_local_once(
        Duration::from_millis(RELOAD_DEBOUNCE_MS),
        move || {
            *slot.borrow_mut() = None;
            reload_palette(&palette, &path);
        },
    );
    *pending.borrow_mut() = Some(id);
}

/// Re-reads the user's stylesheet into the provider.
///
/// `load_from_*` replaces the provider's previous styles, which is exactly the
/// update we want, and `load_from_path` keeps relative `@import`s resolving
/// against the user's file. A removed stylesheet loads as empty: the palette
/// must not linger once the user has deleted it.
fn reload_palette(provider: &gtk::CssProvider, path: &Path) {
    if path.is_file() {
        provider.load_from_path(path);
        tracing::info!(path = %path.display(), "using the user GTK palette");
    } else {
        provider.load_from_string("");
        tracing::info!("user GTK palette is gone; back to the libadwaita theme");
    }
}

#[cfg(test)]
mod tests {
    /// The palette names `data/style.css` may reference. libadwaita defines
    /// every one of them in both variants, and a matugen/DMS palette overrides
    /// them with the user's colors.
    const PALETTE_NAMES: &[&str] = &[
        "accent_bg_color",
        "card_bg_color",
        "card_fg_color",
        "popover_fg_color",
        "window_bg_color",
        "window_fg_color",
    ];

    /// Every color in that stylesheet is a palette *name*, so a typo there is
    /// the easy way to break the theme — and the CSS parser says nothing about
    /// an unknown name (it resolves lazily, and nothing resolves it without a
    /// widget on screen). Check the names directly instead.
    #[test]
    fn stylesheet_uses_only_known_palette_names() {
        let css = std::str::from_utf8(include_bytes!("../data/style.css"))
            .expect("the stylesheet is UTF-8");
        let mut unknown = Vec::new();
        for (at, _) in css.match_indices('@') {
            let name: String = css[at + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && !PALETTE_NAMES.contains(&name.as_str()) {
                unknown.push(format!("@{name}"));
            }
        }
        assert!(unknown.is_empty(), "unknown palette names in style.css: {unknown:?}");
    }

    /// A stylesheet that does not parse shows up in the app only as a
    /// mis-rendered playbar, so catch it in `cargo test`. GTK reports parse
    /// failures as warnings (with no log domain set), which is what the writer
    /// func below collects.
    #[test]
    fn app_stylesheet_parses() {
        // A CssProvider needs an initialized GTK; the parse itself needs no
        // display, so this only skips where GTK cannot initialize at all.
        if let Err(err) = gtk::init() {
            eprintln!("skipped: GTK cannot initialize here: {err}");
            return;
        }
        let css = std::str::from_utf8(include_bytes!("../data/style.css"))
            .expect("the stylesheet is UTF-8");
        let problems = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = problems.clone();
        // glib accepts one writer func per process, so this test owns logging
        // for the whole binary; nothing else here logs through glib.
        gtk::glib::log_set_writer_func(move |level, fields| {
            let message = fields
                .iter()
                .find(|f| f.key() == "MESSAGE")
                .and_then(|f| f.value_bytes());
            let noisy = matches!(
                level,
                gtk::glib::LogLevel::Error
                    | gtk::glib::LogLevel::Critical
                    | gtk::glib::LogLevel::Warning
            );
            if noisy
                && let Some(message) = message
            {
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(message).into_owned());
            }
            gtk::glib::LogWriterOutput::Handled
        });

        let provider = gtk::CssProvider::new();
        provider.load_from_string(css);

        let problems = problems.lock().unwrap();
        assert!(problems.is_empty(), "data/style.css does not parse: {problems:#?}");
    }
}
