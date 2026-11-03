//! Floating-over-video controls (Milestone 3).
//!
//! The window content is a `gtk::Overlay`: the video surface is the main child,
//! and the header / playback bar are overlay children that fade out when the
//! pointer has been idle, mpv-style. Every control acts through
//! `player_core`'s engine trait — never on mpv directly.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use player_core::{Chapter, PlaybackEngine, PlaybackState};
use player_video::VideoArea;

use crate::format;
use crate::Shared;

/// Hide the controls this long after the last pointer motion. mpv's default is
/// ~1 s; a little more so reaching for a button does not hide them.
const IDLE_HIDE_MS: i64 = 2500;
/// How often the idle check runs (cheap: a few `Cell` reads).
const IDLE_TICK_MS: u64 = 500;
/// Grace before a single click on the video counts as play/pause, so a double
/// click (fullscreen) can win.
const CLICK_GRACE_MS: u64 = 250;

/// The widgets the event pump has to keep in sync, plus the idle-hide state.
#[derive(Clone)]
pub struct Controls {
    header: gtk::Revealer,
    playbar: gtk::Revealer,
    title: gtk::Label,
    play_button: gtk::Button,
    seek: gtk::Scale,
    time_label: gtk::Label,
    duration_label: gtk::Label,
    volume_button: gtk::MenuButton,
    volume_scale: gtk::Scale,
    speed_button: gtk::MenuButton,
    speed_scale: gtk::Scale,
    fullscreen_button: gtk::Button,
    /// Big round play/pause button floating in the middle of the video;
    /// revealed exactly while playback is paused.
    center: gtk::Revealer,
    center_button: gtk::Button,
    /// True while the user owns the seek slider (drag or arrows): the pump must
    /// not fight them for it.
    seeking: Rc<Cell<bool>>,
    /// Pointer is over one of the control bars, or a popover is open.
    hover: Rc<Cell<bool>>,
    revealed: Rc<Cell<bool>>,
    last_motion: Rc<Cell<i64>>,
    last_double_click: Rc<Cell<i64>>,
    /// Last pointer position. Wayland re-sends the (unchanged) pointer
    /// position as a motion event after every surface commit — with a
    /// constantly repainting GLArea that is several times a second. Only a
    /// *changed* position is real user activity; otherwise the idle timer
    /// would never expire while the pointer is inside the window.
    last_pos: Rc<Cell<(f64, f64)>>,
}

impl Controls {
    fn now_ms() -> i64 {
        glib::monotonic_time() / 1000
    }

    fn set_revealed(&self, on: bool) {
        if self.revealed.get() == on {
            return;
        }
        self.revealed.set(on);
        self.header.set_reveal_child(on);
        self.playbar.set_reveal_child(on);
    }

    /// Pointer activity: show the controls and restart the idle window.
    fn poke(&self) {
        self.last_motion.set(Self::now_ms());
        self.set_revealed(true);
    }

    fn busy(&self) -> bool {
        self.hover.get()
            || self.volume_button.is_active()
            || self.speed_button.is_active()
    }

    /// Runs on the idle tick: fade out when the pointer has been still for a
    /// while and nothing is being interacted with. Stays up while paused —
    /// that is when people reach for the controls.
    fn idle_tick(&self, paused: bool) {
        if !self.revealed.get() || self.busy() || paused {
            return;
        }
        if Self::now_ms() - self.last_motion.get() < IDLE_HIDE_MS {
            return;
        }
        self.set_revealed(false);
    }

    /// Draw chapter ticks on the seek slider.
    pub fn set_chapters(&self, chapters: &[Chapter]) {
        self.seek.clear_marks();
        for chapter in chapters {
            if chapter.time > 0.0 {
                // Ticks only: chapter titles as mark labels would clutter.
                self.seek.add_mark(chapter.time, gtk::PositionType::Bottom, None);
            }
        }
    }

    /// Sync every control from the folded playback state.
    pub fn refresh(&self, state: &PlaybackState) {
        self.play_button.set_icon_name(if state.paused {
            "media-playback-start-symbolic"
        } else {
            "media-playback-pause-symbolic"
        });
        // The center button mirrors the play state: it appears when paused
        // (that is when people look for it) and stays out of the picture
        // while playing.
        self.center_button.set_icon_name(if state.paused {
            "media-playback-start-symbolic"
        } else {
            "media-playback-pause-symbolic"
        });
        self.center.set_reveal_child(state.paused);
        self.time_label
            .set_text(&format::clock(state.position));
        self.duration_label
            .set_text(&format::clock(state.duration));
        self.title
            .set_text(state.title().unwrap_or_else(|| "mpv-gtk".to_owned()).as_str());
        self.volume_button
            .set_icon_name(volume_icon(state.volume, state.muted));
        self.speed_button.set_label(&format!("{:.2}×", state.speed));

        if !self.seeking.get() {
            let duration = state.duration.max(0.0);
            self.seek.set_range(0.0, duration);
            self.seek.set_value(state.position.clamp(0.0, duration));
        }
        // While a popover is open the user may be dragging its slider.
        if !self.volume_button.is_active() {
            let volume = state.volume.clamp(0.0, 100.0);
            if (self.volume_scale.value() - volume).abs() > f64::EPSILON {
                self.volume_scale.set_value(volume);
            }
        }
        if !self.speed_button.is_active() && (self.speed_scale.value() - state.speed).abs() > 0.001 {
            self.speed_scale.set_value(state.speed);
        }
    }
}

fn volume_icon(volume: f64, muted: bool) -> &'static str {
    if muted || volume <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if volume < 34.0 {
        "audio-volume-low-symbolic"
    } else if volume < 67.0 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    }
}

/// Wrap a bar so it can fade in/out as an overlay child pinned to one edge.
fn overlay_revealer(bar: &impl IsA<gtk::Widget>, valign: gtk::Align) -> gtk::Revealer {
    let revealer = gtk::Revealer::new();
    revealer.set_child(Some(bar));
    revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
    revealer.set_transition_duration(250);
    revealer.set_valign(valign);
    revealer.set_hexpand(true);
    revealer.set_reveal_child(true);
    revealer
}

fn flat_button(icon: &str, action: Option<&str>, tip: &str) -> gtk::Button {
    let button = gtk::Button::builder().icon_name(icon).tooltip_text(tip).build();
    button.add_css_class("flat");
    if let Some(action) = action {
        button.set_action_name(Some(action));
    }
    button
}

fn build_header(window: &adw::ApplicationWindow) -> (gtk::Revealer, gtk::Label) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("floating-bar");
    bar.add_css_class("floating-header");

    let open = flat_button(
        "document-open-symbolic",
        Some("app.open"),
        "Open media (Ctrl+O)",
    );

    let title = gtk::Label::builder()
        .label("mpv-gtk")
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(48)
        .xalign(0.5)
        .build();
    title.add_css_class("header-title");
    title.set_hexpand(true);
    title.set_halign(gtk::Align::Center);

    let menu_model = gtk::gio::Menu::new();
    menu_model.append(Some("Open media…"), Some("app.open"));
    menu_model.append(Some("Fullscreen"), Some("app.fullscreen"));
    menu_model.append(Some("Quit"), Some("app.quit"));
    let menu = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Menu")
        .build();
    menu.add_css_class("flat");
    menu.set_menu_model(Some(&menu_model));

    let close = flat_button("window-close-symbolic", None, "Close (Ctrl+Q)");
    let window_clone = window.clone();
    close.connect_clicked(move |_| window_clone.close());

    bar.append(&open);
    bar.append(&title);
    bar.append(&menu);
    bar.append(&close);

    (overlay_revealer(&bar, gtk::Align::Start), title)
}

fn build_playbar(shared: &Shared) -> (gtk::Revealer, PlayBar) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    bar.add_css_class("floating-bar");
    bar.add_css_class("floating-playbar");

    let play_button = flat_button(
        "media-playback-start-symbolic",
        Some("app.play-pause"),
        "Play/pause (Space)",
   );
    let prev_chapter = flat_button(
        "media-skip-backward-symbolic",
        Some("app.chapter-prev"),
        "Previous chapter",
    );
    let next_chapter = flat_button(
        "media-skip-forward-symbolic",
        Some("app.chapter-next"),
        "Next chapter",
    );

    let time_label = gtk::Label::new(Some("0:00"));
    time_label.add_css_class("time-label");
    let duration_label = gtk::Label::new(Some("0:00"));
    duration_label.add_css_class("time-label");

    // GTK requires min < max at construction; `refresh` sets the real range
    // once a duration is known.
    let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
    seek.set_draw_value(false);
    seek.set_hexpand(true);
    seek.add_css_class("seek");

    let volume_button = gtk::MenuButton::builder()
        .icon_name("audio-volume-high-symbolic")
        .tooltip_text("Volume (↑/↓)")
        .build();
    volume_button.add_css_class("flat");
    let volume_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    volume_scale.set_draw_value(false);
    volume_scale.set_size_request(140, -1);
    {
        let engine = shared.engine.clone();
        volume_scale.connect_value_changed(move |scale| {
            if let Err(err) = engine.set_volume(scale.value()) {
                tracing::warn!(%err, "set_volume failed");
            }
        });
    }
    let mute = gtk::ToggleButton::builder()
        .icon_name("audio-volume-muted-symbolic")
        .tooltip_text("Mute (m)")
        .build();
    {
        let engine = shared.engine.clone();
        mute.connect_toggled(move |button| {
            if let Err(err) = engine.set_muted(button.is_active()) {
                tracing::warn!(%err, "set_muted failed");
            }
        });
    }
    let volume_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    volume_box.append(&mute);
    volume_box.append(&volume_scale);
    volume_button.set_popover(Some(&gtk::Popover::builder().child(&volume_box).build()));

    let speed_button = gtk::MenuButton::builder().label("1.00×").tooltip_text("Speed").build();
    speed_button.add_css_class("flat");
    let speed_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.25, 4.0, 0.05);
    speed_scale.set_draw_value(false);
    speed_scale.set_size_request(160, -1);
    speed_scale.set_value(1.0);
    {
        let engine = shared.engine.clone();
        let speed_button = speed_button.clone();
        speed_scale.connect_value_changed(move |scale| {
            let speed = scale.value();
            speed_button.set_label(&format!("{speed:.2}×"));
            if let Err(err) = engine.set_speed(speed) {
                tracing::warn!(%err, "set_speed failed");
            }
        });
    }
    let speed_reset = gtk::Button::builder().label("Normal").build();
    {
        let speed_scale = speed_scale.clone();
        speed_reset.connect_clicked(move |_| speed_scale.set_value(1.0));
    }
    let speed_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    speed_box.append(&speed_scale);
    speed_box.append(&speed_reset);
    speed_button.set_popover(Some(&gtk::Popover::builder().child(&speed_box).build()));

    let fullscreen_button = flat_button(
        "view-fullscreen-symbolic",
        Some("app.fullscreen"),
        "Fullscreen (f / F11)",
    );

    bar.append(&play_button);
    bar.append(&prev_chapter);
    bar.append(&next_chapter);
    bar.append(&time_label);
    bar.append(&seek);
    bar.append(&duration_label);
    bar.append(&volume_button);
    bar.append(&speed_button);
    bar.append(&fullscreen_button);

    (
        overlay_revealer(&bar, gtk::Align::End),
        PlayBar {
            play_button,
            seek,
            time_label,
            duration_label,
            volume_button,
            volume_scale,
            speed_button,
            speed_scale,
            fullscreen_button,
        },
    )
}

struct PlayBar {
    play_button: gtk::Button,
    seek: gtk::Scale,
    time_label: gtk::Label,
    duration_label: gtk::Label,
    volume_button: gtk::MenuButton,
    volume_scale: gtk::Scale,
    speed_button: gtk::MenuButton,
    speed_scale: gtk::Scale,
    fullscreen_button: gtk::Button,
}

/// The big round play/pause button in the middle of the video. It is only
/// revealed while playback is paused (see `Controls::refresh`), so it never
/// covers moving pictures.
fn build_center() -> (gtk::Revealer, gtk::Button) {
    let button = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .action_name("app.play-pause")
        .build();
    button.add_css_class("center-play");
    let revealer = gtk::Revealer::new();
    revealer.set_child(Some(&button));
    revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
    revealer.set_transition_duration(250);
    revealer.set_halign(gtk::Align::Center);
    revealer.set_valign(gtk::Align::Center);
    revealer.set_reveal_child(false);
    (revealer, button)
}

/// Build the window: video surface with the controls floating over it.
pub fn build_window(app: &adw::Application, shared: &Shared) -> adw::ApplicationWindow {
    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("mpv-gtk"));
    window.set_default_size(1000, 600);
    window.add_css_class("player-window");

    let video = VideoArea::new(shared.engine.clone());
    let (header, title) = build_header(&window);
    let (playbar, bar) = build_playbar(shared);
    let (center, center_button) = build_center();

    // Seek ownership: the user drags, we commit on release (or key release),
    // and the pump leaves the slider alone in between.
    let seeking = Rc::new(Cell::new(false));
    let seek_target = Rc::new(Cell::new(0.0));
    {
        let (seeking, seek_target) = (seeking.clone(), seek_target.clone());
        bar.seek.connect_change_value(move |_scale, _scroll, value| {
            seeking.set(true);
            seek_target.set(value);
            glib::Propagation::Proceed
        });
    }
    {
        let engine = shared.engine.clone();
        let (seeking, seek_target) = (seeking.clone(), seek_target.clone());
        let slider = bar.seek.clone();
        let commit = Rc::new(move || {
            if seeking.get() {
                seeking.set(false);
                if let Err(err) = engine.seek(seek_target.get()) {
                    tracing::warn!(%err, "seek failed");
                }
            }
        });
        let commit_on_key = commit.clone();
        let click = gtk::GestureClick::new();
        click.connect_released(move |_, _, _, _| commit());
        slider.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_released(move |_, _, _, _| commit_on_key());
        slider.add_controller(keys);
    }

    let controls = Controls {
        header: header.clone(),
        playbar: playbar.clone(),
        title,
        play_button: bar.play_button,
        seek: bar.seek,
        time_label: bar.time_label,
        duration_label: bar.duration_label,
        volume_button: bar.volume_button,
        volume_scale: bar.volume_scale,
        speed_button: bar.speed_button,
        speed_scale: bar.speed_scale,
        fullscreen_button: bar.fullscreen_button,
        center: center.clone(),
        center_button: center_button.clone(),
        seeking,
        hover: Rc::new(Cell::new(false)),
        revealed: Rc::new(Cell::new(true)),
        last_motion: Rc::new(Cell::new(Controls::now_ms())),
        last_double_click: Rc::new(Cell::new(0)),
        last_pos: Rc::new(Cell::new((-1.0, -1.0))),
    };

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&video));
    overlay.add_overlay(&header);
    overlay.add_overlay(&playbar);
    // Last: the center button sits above the bars if they ever overlap.
    overlay.add_overlay(&center);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&overlay));
    window.set_content(Some(&toasts));
    *shared.toasts.borrow_mut() = Some(toasts);
    *shared.controls.borrow_mut() = Some(controls.clone());

    // Pointer anywhere in the window: controls come back.
    let motion = gtk::EventControllerMotion::new();
    {
        let controls = controls.clone();
        motion.connect_motion(move |_, x, y| {
            // Ignore the synthetic same-position motion events (see `last_pos`).
            if controls.last_pos.replace((x, y)) != (x, y) {
                controls.poke();
            }
        });
    }
    window.add_controller(motion);

    // Resting the pointer on a bar (or in a popover) must not hide it.
    for bar in [&header, &playbar] {
        let hover_motion = gtk::EventControllerMotion::new();
        {
            let on_enter = controls.clone();
            hover_motion.connect_enter(move |_, _, _| {
                on_enter.hover.set(true);
                on_enter.poke();
            });
            let on_leave = controls.clone();
            hover_motion.connect_leave(move |_| {
                on_leave.hover.set(false);
                on_leave.poke();
            });
        }
        bar.add_controller(hover_motion);
    }

    // Idle check: a 500 ms tick is cheaper than re-arming a source on every
    // motion event.
    glib::timeout_add_local(Duration::from_millis(IDLE_TICK_MS), {
        let controls = controls.clone();
        let state = shared.state.clone();
        move || {
            controls.idle_tick(state.borrow().paused);
            glib::ControlFlow::Continue
        }
    });

    // mpv-style mouse bindings on the video: click = play/pause, double click
    // = fullscreen. The double click wins by timestamp, so no timer to cancel.
    let click = gtk::GestureClick::new();
    click.set_button(1);
    {
        let engine = shared.engine.clone();
        let last_double_cell = controls.last_double_click.clone();
        click.connect_released(move |_, n_press, _, _| {
            if n_press != 1 {
                return;
            }
            let started = Controls::now_ms();
            let engine = engine.clone();
            let last_double = last_double_cell.clone();
            glib::timeout_add_local_once(Duration::from_millis(CLICK_GRACE_MS), move || {
                // A double click that landed after this release owns the gesture.
                if last_double.get() < started
                    && let Err(err) = engine.toggle_play()
                {
                    tracing::warn!(%err, "play/pause failed");
                }
            });
        });
    }
    let double = gtk::GestureClick::new();
    double.set_button(1);
    {
        let window_clone = window.clone();
        let last_double = controls.last_double_click.clone();
        double.connect_pressed(move |_, n_press, _, _| {
            if n_press == 2 {
                let now = Controls::now_ms();
                if window_clone.is_fullscreen() {
                    window_clone.unfullscreen();
                } else {
                    window_clone.fullscreen();
                }
                // Publish only after the toggle: the single click compares it.
                last_double.set(now);
            }
        });
    }
    video.add_controller(click);
    video.add_controller(double);

    // Keep the fullscreen button's icon honest.
    window.connect_fullscreened_notify({
        let controls = controls.clone();
        move |window| {
            controls.fullscreen_button.set_icon_name(if window.is_fullscreen() {
                "view-restore-symbolic"
            } else {
                "view-fullscreen-symbolic"
            });
        }
    });

    // A failed GL attach otherwise shows up as an unexplained black window
    // (that cost two debugging sessions), so say so out loud instead.
    let engine = shared.engine.clone();
    let toasts_slot = shared.toasts.clone();
    glib::timeout_add_local(Duration::from_millis(1500), move || {
        if !engine.inner().has_render()
            && let Some(overlay) = toasts_slot.borrow().as_ref()
        {
            overlay.add_toast(adw::Toast::new(
                "Video surface failed to start: no GL render context (see log)",
            ));
        }
        glib::ControlFlow::Break
    });

    let window_slot = shared.window.clone();
    window.connect_close_request(move |_| {
        *window_slot.borrow_mut() = None;
        gtk::glib::Propagation::Proceed
    });

    window.present();
    window
}
