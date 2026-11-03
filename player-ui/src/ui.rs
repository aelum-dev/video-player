//! Floating-over-video controls (Milestone 3).
//!
//! The window content is a `gtk::Overlay`: the video surface is the main child,
//! and the header / playback bar are overlay children that fade out when the
//! pointer has been idle, mpv-style. Every control acts through
//! `player_core`'s engine trait — never on mpv directly.

use std::cell::{Cell, RefCell};
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
/// Delay between the last slider move and the committed seek: short enough to
/// feel instant, long enough to let a whole drag run as one seek.
const SEEK_COMMIT_MS: u64 = 350;
/// How long the pump holds the seek knob at the target after a seek is issued,
/// covering mpv's async seek latency so the knob never snaps back to the stale
/// position for a frame or two.
const SEEK_SETTLE_MS: i64 = 400;
/// Half of the seek knob (38 px in style.css): click-to-position maps the
/// track between the knob's edge stops.
const KNOB_HALF_PX: f64 = 19.0;

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
    /// Monotonic-ms deadline until which the pump must keep its hands off the
    /// seek knob *after* a seek is committed. mpv's seek is async, so without
    /// this the pump would snap the knob back to the stale position for a frame
    /// or two before the seek lands.
    seek_settle: Rc<Cell<i64>>,
    /// Pointer is over one of the control bars, or a popover is open.
    hover: Rc<Cell<bool>>,
    revealed: Rc<Cell<bool>>,
    last_motion: Rc<Cell<i64>>,
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

    /// Icon for the play/pause buttons: the action the click performs —
    /// pause bars while playing, play triangle while paused.
    fn play_icon(paused: bool) -> &'static str {
        if paused {
            "media-playback-start-symbolic"
        } else {
            "media-playback-pause-symbolic"
        }
    }


    fn set_revealed(&self, on: bool) {
        if self.revealed.get() == on {
            return;
        }
        self.revealed.set(on);
        self.header.set_reveal_child(on);
        self.playbar.set_reveal_child(on);
        // The center play/pause button rides with the controls: it is there
        // whenever the user is interacting (and stays while paused), and fades
        // out with everything else on idle.
        self.center.set_reveal_child(on);
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
        // The icon shows the action the click performs: pause bars while
        // playing, play triangle while paused.
        self.play_button.set_icon_name(Self::play_icon(state.paused));
        // The center button mirrors the play state with its icon; its
        // visibility follows the controls (see `set_revealed`).
        self.center_button.set_icon_name(Self::play_icon(state.paused));
        self.time_label
            .set_text(&format::clock(state.position));
        self.duration_label
            .set_text(&format::clock(state.duration));
        self.title
            .set_text(state.title().unwrap_or_else(|| "mpv-gtk".to_owned()).as_str());
        self.volume_button
            .set_icon_name(volume_icon(state.volume, state.muted));
        self.speed_button.set_label(&format!("{:.2}×", state.speed));

        if !self.seeking.get() && Controls::now_ms() >= self.seek_settle.get() {
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
    let appearance = gtk::gio::Menu::new();
    appearance.append(Some("Follow System"), Some("app.theme::\"default\""));
    appearance.append(Some("Light"), Some("app.theme::\"light\""));
    appearance.append(Some("Dark"), Some("app.theme::\"dark\""));
    menu_model.append_section(Some("Appearance"), &appearance);
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

    // Start on the icon for the default state (playing): pause bars, i.e.
    // "click to pause". `refresh` keeps it in step with the play state.
    let play_button = flat_button(
        Controls::play_icon(false),
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
    // once a duration is known. Page increment stays 0 so GTK's native trough
    // click never steps the value out from under our click-to-jump handler.
    let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
    seek.set_draw_value(false);
    seek.set_hexpand(true);
    seek.adjustment().set_page_increment(0.0);
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

/// The big round play/pause button in the middle of the video. It rides with
/// the floating controls (see `Controls::set_revealed`): visible while the
/// user is interacting or paused, faded out on idle. Its icon tracks the play
/// state.
fn build_center() -> (gtk::Revealer, gtk::Button) {
    let button = gtk::Button::builder()
        .icon_name(Controls::play_icon(false))
        .action_name("app.play-pause")
        .build();
    button.add_css_class("center-play");
    let revealer = gtk::Revealer::new();
    revealer.set_child(Some(&button));
    revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
    revealer.set_transition_duration(250);
    revealer.set_halign(gtk::Align::Center);
    revealer.set_valign(gtk::Align::Center);
    // Start revealed to match the controls' initial state (they are shown on
    // open and fade on idle); `set_revealed` keeps them in sync from then on.
    revealer.set_reveal_child(true);
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

    // Seek ownership: any user change (drag, keys, click-to-jump below) marks
    // the slider user-owned and re-arms a short commit timer; the seek is
    // issued once the user stops moving, and the pump leaves the slider alone
    // until then.
    //
    // No GestureClick here: a capturing gesture on the slider steals the
    // pointer sequence from the Scale's own drag gesture, and dragging the
    // knob stops working entirely (the "seek bar not working" bug).
    let seeking = Rc::new(Cell::new(false));
    let seek_target = Rc::new(Cell::new(0.0));
    let seek_settle = Rc::new(Cell::new(0i64));
    let pending_commit: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let arm_commit = {
        let engine = shared.engine.clone();
        let (seeking, seek_target, seek_settle, pending_commit) = (
            seeking.clone(),
            seek_target.clone(),
            seek_settle.clone(),
            pending_commit.clone(),
        );
        Rc::new(move || {
            if let Some(id) = pending_commit.borrow_mut().take() {
                id.remove();
            }
            let (engine, seeking, seek_target, seek_settle) = (
                engine.clone(),
                seeking.clone(),
                seek_target.clone(),
                seek_settle.clone(),
            );
            let pending_inner = pending_commit.clone();
            let id = glib::timeout_add_local_once(
                Duration::from_millis(SEEK_COMMIT_MS),
                move || {
                    *pending_inner.borrow_mut() = None;
                    if seeking.get() {
                        seeking.set(false);
                        seek_settle.set(Controls::now_ms() + SEEK_SETTLE_MS);
                        if let Err(err) = engine.seek(seek_target.get()) {
                            tracing::warn!(%err, "seek failed");
                        }
                    }
                },
            );
            *pending_commit.borrow_mut() = Some(id);
        })
    };
    {
        let (seeking, seek_target) = (seeking.clone(), seek_target.clone());
        let arm_commit = arm_commit.clone();
        bar.seek.connect_change_value(move |_scale, _scroll, value| {
            seeking.set(true);
            seek_target.set(value);
            arm_commit();
            glib::Propagation::Proceed
        });
    }
    {
        // Pointer handling on the seek slider, via a legacy controller (it
        // observes the raw stream without claiming the sequence, so the
        // Scale's own drag gesture keeps working — a GestureClick here would
        // steal it, the original "seek bar not working" bug).
        //
        // - Press (click or grab): jump to the pressed position immediately.
        //   A click is a deliberate seek; grabbing the knob maps to ~the
        //   current position, so it is a no-op there.
        // - Release: commit the seek to wherever the drag ended, right away.
        // - The debounce timer stays only as a safety net in case the release
        //   is never delivered; on a normal release it is cancelled here.
        let engine = shared.engine.clone();
        let slider = bar.seek.clone();
        let (seeking, seek_target, seek_settle) =
            (seeking.clone(), seek_target.clone(), seek_settle.clone());
        let (arm_commit, pending_commit) = (arm_commit.clone(), pending_commit.clone());
        let press = gtk::EventControllerLegacy::new();
        press.connect_event(move |_ctrl, event| {
            match event.event_type() {
                gtk::gdk::EventType::ButtonPress => {
                    if let Some(button) = event.downcast_ref::<gtk::gdk::ButtonEvent>()
                        && button.button() == 1
                        && let Some((sx, sy)) = event.position()
                    {
                        // `event.position()` is *surface*-relative, but the
                        // slider sits offset inside the window (after the
                        // transport buttons and the time label). Using the raw
                        // x made the fraction too large and clamped to 1.0 —
                        // the knob "teleported to the end". Convert the point
                        // into the slider's own coordinate space first.
                        let local_x = slider.root().and_then(|root| {
                            let src = gtk::graphene::Point::new(sx as f32, sy as f32);
                            root.compute_point(&slider, &src).map(|p| f64::from(p.x()))
                        });
                        let Some(x) = local_x else {
                            return glib::Propagation::Proceed;
                        };
                        let width = slider.width() as f64;
                        if width > 2.0 * KNOB_HALF_PX {
                            let fraction = ((x - KNOB_HALF_PX)
                                / (width - 2.0 * KNOB_HALF_PX))
                                .clamp(0.0, 1.0);
                            let value = slider.adjustment().upper() * fraction;
                            seeking.set(true);
                            seek_target.set(value);
                            slider.set_value(value);
                            seek_settle.set(Controls::now_ms() + SEEK_SETTLE_MS);
                            if let Err(err) = engine.seek(value) {
                                tracing::warn!(%err, "seek failed");
                            }
                            arm_commit();
                        }
                    }
                }
                gtk::gdk::EventType::ButtonRelease => {
                    if let Some(button) = event.downcast_ref::<gtk::gdk::ButtonEvent>()
                        && button.button() == 1
                        && seeking.get()
                    {
                        seeking.set(false);
                        if let Some(id) = pending_commit.borrow_mut().take() {
                            id.remove();
                        }
                        seek_settle.set(Controls::now_ms() + SEEK_SETTLE_MS);
                        if let Err(err) = engine.seek(seek_target.get()) {
                            tracing::warn!(%err, "seek failed");
                        }
                    }
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        bar.seek.add_controller(press);
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
        seek_settle,
        hover: Rc::new(Cell::new(false)),
        revealed: Rc::new(Cell::new(true)),
        last_motion: Rc::new(Cell::new(Controls::now_ms())),
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

    // Double-click on the video toggles fullscreen. Play/pause is *not* bound
    // to clicking the video (that was an easy accidental pause); it lives on
    // the center button, the playbar button, and Space.
    let double = gtk::GestureClick::new();
    double.set_button(1);
    {
        let window_clone = window.clone();
        double.connect_pressed(move |_, n_press, _, _| {
            if n_press == 2 {
                if window_clone.is_fullscreen() {
                    window_clone.unfullscreen();
                } else {
                    window_clone.fullscreen();
                }
            }
        });
    }
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
