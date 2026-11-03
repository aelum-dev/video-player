//! Video player — the GTK shell: application, actions, and the event pump.
//!
//! Opens files from the CLI or Ctrl+O and plays them through the core engine.
//! Started without files it loads nothing: the window opens black with the play
//! button as the active control, and pressing it starts the last title the
//! player had (`last_played`). The window itself (video surface with controls
//! floating over it) is built by [`ui`]. This file only wires application
//! lifecycle, actions/accels, and the event pump that keeps the UI in sync from
//! the core's event stream.
//!
//! All UI state lives on the GTK main thread (`Rc<RefCell<_>>`); only the
//! engine crosses threads (`Arc<dyn PlaybackEngine>`, mpv's render/event
//! threads).

mod format;
mod theme;
mod ui;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::prelude::*;
use libadwaita as adw;
use player_core::last_played::{self, LastPlayed};
use player_core::{MpvEngine, PlaybackEngine, PlaybackState, PlayerEvent};

const APP_ID: &str = "aelum.videoplayer.Player";
/// Seconds jumped by the Left/Right keys (mpv's default).
const SEEK_STEP_SECS: i64 = 5;
/// Volume percentage stepped by the Up/Down keys.
const VOLUME_STEP: i64 = 5;
/// How often the last-played record is refreshed while playing. A few
/// hundred bytes every five seconds, and the difference between resuming
/// where the user left off and resuming five seconds earlier when the
/// player is killed rather than closed.
const LAST_PLAYED_REFRESH_MS: i64 = 5_000;

/// State shared between actions, the window, and the event pump.
/// Main-thread only except `engine`.
///
/// `engine` is the concrete `MpvEngine`, not `Arc<dyn PlaybackEngine>`:
/// with one engine, dyn-dispatch buys nothing the constructor swap can't
/// do, and the GL seam (`VideoArea`, the render check below) needs the
/// concrete type anyway. The trait seam in `player_core::engine` is what
/// a binding swap would rework — see PLAN.md.
#[derive(Clone)]
struct Shared {
    engine: Arc<MpvEngine>,
    state: Rc<RefCell<PlaybackState>>,
    window: Rc<RefCell<Option<adw::ApplicationWindow>>>,
    toasts: Rc<RefCell<Option<adw::ToastOverlay>>>,
    controls: Rc<RefCell<Option<ui::Controls>>>,
    /// Keeps the palette watcher alive (dropping it stops the hot reload).
    theme: Rc<RefCell<Option<theme::Theme>>>,
    /// Has this session opened a title yet? The first one starts paused (the
    /// window opens on its play button); anything opened later plays at once.
    opened_any: Rc<Cell<bool>>,
    /// The record read at startup: what the play button opens while nothing is
    /// loaded. Consumed by that press, and never used once a title is open.
    pending: Rc<RefCell<Option<LastPlayed>>>,
    /// Position to jump to once the title queued by `open_source` has loaded
    /// (0 = none). The seek cannot be issued earlier: the file is not loaded yet.
    resume_at: Rc<Cell<f64>>,
    /// When the last-played record was last written.
    last_saved: Rc<Cell<i64>>,
}

fn main() -> gtk::glib::ExitCode {
    // Kept as a safety net: DMS's theme switch ends with
    // `pkill -HUP -f 'gtk'`, which matched the old `mpv-gtk` binary name.
    theme::ignore_hangup();

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
        theme: Rc::new(RefCell::new(None)),
        opened_any: Rc::new(Cell::new(false)),
        pending: Rc::new(RefCell::new(None)),
        resume_at: Rc::new(Cell::new(0.0)),
        last_saved: Rc::new(Cell::new(0)),
    };

    app.connect_startup({
        let shared = shared.clone();
        move |_| {
            // The chrome (menus, popovers, dialogs) follows the system:
            // AdwStyleManager watches the portal, and DMS publishes the
            // dark/light switch there. What we install is the app's own
            // overlay styles plus a reloadable mirror of the user's palette.
            *shared.theme.borrow_mut() = theme::Theme::install();
        }
    });
    app.connect_activate({
        let shared = shared.clone();
        move |app| {
            ensure_window(app, &shared);
            // Started without files: load nothing. The record is only read, so
            // the play button has something to start when the user presses it.
            *shared.pending.borrow_mut() = last_played::load();
            if shared.pending.borrow().is_none() {
                tracing::info!("no last-played record; the play button opens nothing until a file is chosen");
            }
        }
    });
    app.connect_open({
        let shared = shared.clone();
        move |app, files, _hint| {
            ensure_window(app, &shared);
            // The first title of a session — files on the command line of a
            // cold start — opens paused, so the window shows its play button
            // instead of running. Files opened later play straight away.
            let autoplay = shared.opened_any.get();
            shared.opened_any.set(true);
            for file in files {
                open_source(&shared, &source_of(file), autoplay, 0.0);
            }
        }
    });
    // Whatever the exit path (close button, Ctrl+Q, app.quit), leave the
    // record pointing at the last title and where it stopped.
    app.connect_shutdown({
        let shared = shared.clone();
        move |_| {
            save_last_played(&shared, shared.state.borrow().resume_point());
        }
    });

    setup_actions(&app, &shared);
    start_event_pump(&shared);

    app.run()
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
                        tracing::info!(
                            %uri,
                            native = file.is_native(),
                            local = ?file.path(),
                            "file chooser selection"
                        );
                        open_source(&shared, &source_of(&file), true, 0.0);
                    }
                },
            );
        }
    });
    app.add_action(&open);

    let play_pause = gtk::gio::SimpleAction::new("play-pause", None);
    play_pause.connect_activate({
        let shared = shared.clone();
        move |_, _| toggle_playback(&shared)
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

/// The string mpv should open for a chosen or dropped `GFile`: a local path
/// when there is one, otherwise the URI — document-portal picks and dragged
/// links have no local path, and mpv understands `file://` and plain URLs.
pub(crate) fn source_of(file: &gtk::gio::File) -> String {
    match file.path() {
        Some(p) => p.to_string_lossy().into_owned(),
        None => {
            let uri: String = file.uri().into();
            tracing::warn!(%uri, "no local path, passing the URI to mpv");
            uri
        }
    }
}

/// Load a source. `autoplay` decides whether it starts playing or waits for
/// the play button; `starts_at` is the position to jump to once it has loaded
/// (0 = the beginning), and the position the last-played record keeps.
pub(crate) fn open_source(shared: &Shared, path: &str, autoplay: bool, starts_at: f64) {
    let starts_at = starts_at.max(0.0);
    tracing::info!(path, autoplay, starts_at, "opening source");
    {
        let mut state = shared.state.borrow_mut();
        state.set_source(path);
        // Record the play state the engine is about to be told, so the button
        // shows the right shape before mpv's `pause` event arrives.
        state.set_play_state(!autoplay);
    }
    if let Err(err) = shared.engine.open(path, autoplay) {
        tracing::error!(%err, path, "open failed");
        toast(shared, &format!("Could not open {path}: {err}"));
        return;
    }
    // The seek is queued for the `Loaded` event (a seek needs a loaded file,
    // and the load itself is deferred until the render context is attached).
    shared.resume_at.set(starts_at);
    // Recorded at once: even a player that dies before it plays should reopen
    // this title on the next start.
    save_last_played(shared, starts_at);
    let title = shared.state.borrow().title();
    let window = shared.window.borrow().clone();
    if let (Some(title), Some(win)) = (title, window) {
        win.set_title(Some(&title));
    }
}

/// Space, and the button in the middle of the video.
///
/// With nothing loaded — the state the player starts in — this is the button
/// that starts the last title the player had, at the position it stopped at.
///
/// A title that ran to its end starts over: `keep-open=always` leaves mpv
/// holding the last frame, and resuming there would end again immediately.
/// (Only while paused — with `keep-open-pause=no` a title can sit at EOF
/// still "playing", where the button means pause.)
fn toggle_playback(shared: &Shared) {
    if shared.state.borrow().source.is_none()
        && let Some(entry) = shared.pending.borrow_mut().take()
    {
        tracing::info!(
            source = %entry.source,
            position = entry.position,
            "play: starting the last played title"
        );
        open_source(shared, &entry.source, true, entry.position);
        return;
    }
    let restart = shared.state.borrow().should_restart();
    let result = if restart {
        tracing::info!("title ended: play starts it over from 0:00");
        {
            let mut state = shared.state.borrow_mut();
            state.ended = false;
            state.position = 0.0;
        }
        shared.engine.seek(0.0).and_then(|()| shared.engine.set_paused(false))
    } else {
        shared.engine.toggle_play()
    };
    if let Err(err) = result {
        tracing::warn!(%err, "play/pause failed");
    }
}

/// Writes the last-played record for the current title. A failure is logged
/// and forgotten: an unwritable config directory must never get in the way
/// of playing something.
fn save_last_played(shared: &Shared, position: f64) {
    let Some(source) = shared.state.borrow().source.clone() else {
        return;
    };
    shared.last_saved.set(gtk::glib::monotonic_time() / 1000);
    if let Err(err) = last_played::save(&LastPlayed { source, position: position.max(0.0) }) {
        tracing::warn!(%err, "could not write the last-played record");
    }
}

pub(crate) fn toast(shared: &Shared, text: &str) {
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
                            // The position `open_source` queued, if any — this is
                            // the first moment a seek is legal.
                            let resume = shared.resume_at.replace(0.0);
                            if resume > 0.0 {
                                state.position = resume;
                                if let Err(err) = shared.engine.seek(resume) {
                                    tracing::warn!(%err, "resume seek failed");
                                }
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
            // Keep the record current while something is playing, so a player
            // that is killed (not closed) still reopens where it left off.
            let playing = shared.state.borrow().is_playing();
            let now = gtk::glib::monotonic_time() / 1000;
            if playing && now - shared.last_saved.get() >= LAST_PLAYED_REFRESH_MS {
                let position = shared.state.borrow().resume_point();
                save_last_played(&shared, position);
            }
            gtk::glib::ControlFlow::Continue
        }
    });
}
