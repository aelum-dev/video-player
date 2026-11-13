//! Window assembly: GNOME chrome — a header bar and a bottom playback
//! toolbar (an `AdwToolbarView`) — framing the video surface, plus the
//! floating OSD controls and the window-level gestures (double-click
//! fullscreen, volume by wheel).
//!
//! The playback bar is ONE set of widgets that changes parent with the
//! mode: the toolbar's bottom slot while windowed (theme chrome), and an
//! `.osd` pill inside the overlay while fullscreen (over the video, fading
//! on idle). See `Controls::set_fullscreen`.
//!
//! The interface hides on idle in **both** modes and whether or not something
//! is playing: the idle tick retracts both chrome bars (`AdwToolbarView`'s
//! animated reveal properties) so the video takes the whole window, and any
//! pointer or key activity brings them back. The one state it never hides is an
//! empty player — with no title loaded the play button is the only control.
//! See `Controls::idle_tick` for what counts as idle.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use player_core::PlaybackEngine;
use player_video::VideoArea;

use crate::i18n;
use crate::Shared;

use super::controls::{Controls, IDLE_TICK_MS};
use super::now_ms;
use super::{dnd, seek, volume};

/// Mark a widget and everything below it right-to-left (no-op in LTR locales).
///
/// GTK's widget direction is NOT inherited: `set_direction` on the window
/// leaves every child at LTR — settled by screen capture, a box whose own
/// direction is RTL mirrors its children, a box under an RTL window does not.
/// That is why this walks the subtree instead of setting the window.
fn apply_rtl(root: &impl IsA<gtk::Widget>) {
    if !i18n::is_rtl() {
        return;
    }
    let root = root.as_ref();
    root.set_direction(gtk::TextDirection::Rtl);
    let mut child = root.first_child();
    while let Some(widget) = child {
        apply_rtl(&widget);
        child = widget.next_sibling();
    }
}

fn flat_button(icon: &str, action: Option<&str>, tip: &str) -> gtk::Button {
    let button = gtk::Button::builder().icon_name(icon).tooltip_text(tip).build();
    button.add_css_class("flat");
    if let Some(action) = action {
        button.set_action_name(Some(action));
    }
    button
}

/// The header bar: real GNOME chrome. The title is the window title, which
/// the event pump already mirrors to the media title; the window controls and
/// the drag-to-move come from libadwaita itself (an `AdwToolbarView` bar is
/// a titlebar — the hand-rolled `begin_move` drag this replaces was a
/// workaround for a window that had none).
fn build_header() -> adw::HeaderBar {
    let header = adw::HeaderBar::new();

    let open = flat_button(
        "document-open-symbolic",
        Some("app.open"),
        &i18n::t("header.open"),
    );
    header.pack_start(&open);

    // pack_end places from the right edge inward, so the first one packed is
    // the outermost corner: the info button there, the primary menu next to it.
    let about = flat_button(
        "help-about-symbolic",
        Some("app.about"),
        &i18n::t("about.label"),
    );
    header.pack_end(&about);

    let menu_model = gtk::gio::Menu::new();
    menu_model.append(Some(&i18n::t("menu.open")), Some("app.open"));
    menu_model.append(Some(&i18n::t("menu.fullscreen")), Some("app.fullscreen"));
    menu_model.append(Some(&i18n::t("menu.quit")), Some("app.quit"));
    let menu = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text(i18n::t("menu.label"))
        .build();
    menu.add_css_class("flat");
    menu.set_menu_model(Some(&menu_model));
    header.pack_end(&menu);

    header
}

/// The playback bar. Windowed it is a `.toolbar` in the chrome; fullscreen
/// the same box becomes an `.osd` pill — its buttons flip `.flat` ↔ `.osd`
/// with it (see `Controls::set_fullscreen`), so no rule here owns a color.
fn build_playbar(shared: &Shared) -> (gtk::Box, PlayBar) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("toolbar");

    // Start as a play triangle: the player opens with nothing loaded and
    // this is the control that starts something (the last title — see
    // `toggle_playback`).
    let play = flat_button(
        Controls::play_icon(true),
        Some("app.play-pause"),
        &i18n::t("bar.playPause"),
    );

    // Previous/next **file** in the directory — the bar is the file control,
    // the same actions the `<` / `>` keys carry. The buttons beside the
    // on-video play button step chapters instead.
    let prev = flat_button(
        "media-skip-backward-symbolic",
        Some("app.skip-prev"),
        &i18n::t("bar.prevFile"),
    );
    let next = flat_button(
        "media-skip-forward-symbolic",
        Some("app.skip-next"),
        &i18n::t("bar.nextFile"),
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

    let volume_button = gtk::MenuButton::builder()
        .icon_name("audio-volume-high-symbolic")
        .tooltip_text(i18n::t("bar.volume"))
        .build();
    volume_button.add_css_class("flat");
    let volume_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    volume_scale.set_draw_value(false);
    volume_scale.set_size_request(140, -1);
    let volume_scale_changed = {
        let engine = shared.engine.clone();
        let state = shared.state.clone();
        volume_scale.connect_value_changed(move |scale| {
            let value = scale.value();
            if let Err(err) = engine.set_volume(value) {
                tracing::warn!(%err, "set_volume failed");
            } else {
                // Same fold as the wheel step: a following relative step
                // builds on the dragged value, not on the observed one
                // still on its way back.
                state.borrow_mut().request_volume(value);
            }
        })
    };
    let mute = gtk::ToggleButton::builder()
        .icon_name("audio-volume-muted-symbolic")
        .tooltip_text(i18n::t("bar.mute"))
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

    let speed_button = gtk::MenuButton::builder()
        .label("1.00×")
        .tooltip_text(i18n::t("bar.speed"))
        .build();
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
    // Quick jumps for the speeds people actually step through. They only
    // move the adjustment: the slider's value-changed handler stays the one
    // path that updates the button label and mpv, so the controls cannot
    // drift apart. ("Normal" was a second reset button; 1.0× in the row
    // does the same job in the same shape as the other presets.)
    let speed_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for (label, speed) in [
        ("1.0×", 1.0_f64),
        ("1.25×", 1.25),
        ("1.50×", 1.5),
        ("1.75×", 1.75),
        ("2.00×", 2.0),
    ] {
        let preset = gtk::Button::builder().label(label).build();
        let speed_scale = speed_scale.clone();
        preset.connect_clicked(move |_| speed_scale.set_value(speed));
        speed_row.append(&preset);
    }
    let speed_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    speed_box.append(&speed_scale);
    speed_box.append(&speed_row);
    speed_button.set_popover(Some(&gtk::Popover::builder().child(&speed_box).build()));

    // Pin the window on top (a Hyprland window state — see the `hyprland`
    // module). It sits with the fullscreen button because both are window-mode
    // controls: the player as a picture-in-picture while another window is the
    // work. A ToggleButton because `app.pin` carries a boolean state — GTK keeps
    // the button's checked state in step with the action, and the theme draws it;
    // `.aelum-pin` is the hook style.css gives it the accent on.
    let pin = gtk::ToggleButton::builder()
        .icon_name("lock-symbolic")
        .action_name("app.pin")
        .tooltip_text(i18n::t("bar.pin"))
        .build();
    pin.add_css_class("flat");
    pin.add_css_class("aelum-pin");

    let fullscreen = flat_button(
        "view-fullscreen-symbolic",
        Some("app.fullscreen"),
        &i18n::t("bar.fullscreen"),
    );

    bar.append(&play);
    bar.append(&prev);
    bar.append(&next);
    bar.append(&time_label);
    bar.append(&seek);
    bar.append(&duration_label);
    bar.append(&volume_button);
    bar.append(&speed_button);
    bar.append(&pin);
    bar.append(&fullscreen);

    // Every button whose style class flips with the mode. The menu buttons
    // are listed as wrappers: `.flat`/`.osd` belong on the `menubutton`
    // node, whose inner `button` the theme styles (api-notes).
    let bar_buttons = vec![
        play.clone().upcast(),
        prev.upcast(),
        next.upcast(),
        volume_button.clone().upcast(),
        speed_button.clone().upcast(),
        pin.clone().upcast(),
        fullscreen.clone().upcast(),
    ];

    (
        bar,
        PlayBar {
            bar_buttons,
            play,
            seek,
            time_label,
            duration_label,
            volume_button,
            volume_scale,
            volume_scale_changed,
            speed_button,
            speed_scale,
            pin_button: pin,
            fullscreen_button: fullscreen,
        },
    )
}

struct PlayBar {
    bar_buttons: Vec<gtk::Widget>,
    play: gtk::Button,
    seek: gtk::Scale,
    time_label: gtk::Label,
    duration_label: gtk::Label,
    volume_button: gtk::MenuButton,
    volume_scale: gtk::Scale,
    /// The slider's `value-changed` handler — blocked while `refresh`
    /// syncs the slider, so a sync never re-enters the engine.
    volume_scale_changed: gtk::glib::SignalHandlerId,
    speed_button: gtk::MenuButton,
    speed_scale: gtk::Scale,
    pin_button: gtk::ToggleButton,
    fullscreen_button: gtk::Button,
}

/// The big play/pause button in the middle of the video, with the chapter
/// jumps flanking it. Every button is an `.osd .circular` one — libadwaita's
/// own over-content control (dark disc, white ink, its own hover/press states)
/// — and the row rides with the interface: faded whenever that is, and the
/// play button the only control left standing when nothing is loaded (the
/// idle-hide never hides that state — see `Controls::idle_tick`).
///
/// The chapter buttons start hidden and `Controls::set_chapters` shows them:
/// a title without at least two markers has nowhere to jump, and a button
/// that cannot do anything is not drawn.
struct CenterRow {
    revealer: gtk::Revealer,
    play: gtk::Button,
    prev_chapter: gtk::Button,
    next_chapter: gtk::Button,
}

fn build_center() -> CenterRow {
    let play = gtk::Button::builder()
        .icon_name(Controls::play_icon(true))
        .action_name("app.play-pause")
        .build();
    play.add_css_class("osd");
    play.add_css_class("circular");
    play.add_css_class("center-play");

    // Previous/next **chapter**, beside the play button: these are the
    // reach-for-them-while-watching controls, and the row is where the eyes
    // already are. They carry `app.chapter-prev/next` (Ctrl+←/→), and their
    // icons are the *seek* pair rather than the bar's *skip* pair — inside
    // the title versus to another file is the difference being drawn.
    //
    // `valign: center` is not decoration: the row is as tall as the 92px
    // play button, and a box stretches its children to fill vertically —
    // the chapter buttons' press box would be a 56x92 ellipse, not a circle.
    let prev_chapter = gtk::Button::builder()
        .icon_name("media-seek-backward-symbolic")
        .action_name("app.chapter-prev")
        .tooltip_text(i18n::t("center.prevChapter"))
        .build();
    prev_chapter.add_css_class("osd");
    prev_chapter.add_css_class("circular");
    prev_chapter.add_css_class("center-skip");
    prev_chapter.set_valign(gtk::Align::Center);
    prev_chapter.set_visible(false);
    let next_chapter = gtk::Button::builder()
        .icon_name("media-seek-forward-symbolic")
        .action_name("app.chapter-next")
        .tooltip_text(i18n::t("center.nextChapter"))
        .build();
    next_chapter.add_css_class("osd");
    next_chapter.add_css_class("circular");
    next_chapter.add_css_class("center-skip");
    next_chapter.set_valign(gtk::Align::Center);
    next_chapter.set_visible(false);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&prev_chapter);
    row.append(&play);
    row.append(&next_chapter);

    let revealer = gtk::Revealer::new();
    revealer.set_child(Some(&row));
    revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
    revealer.set_transition_duration(250);
    revealer.set_halign(gtk::Align::Center);
    revealer.set_valign(gtk::Align::Center);
    // Start revealed to match the controls' initial state (the player opens
    // paused with nothing loaded); `refresh` owns its visibility from then on.
    revealer.set_reveal_child(true);
    CenterRow {
        revealer,
        play,
        prev_chapter,
        next_chapter,
    }
}

/// Double-click pair state machine: `last` is the previous press time
/// (0 = none). A press within `gap` of it completes a pair: report
/// `fired` and reset, so a third click starts over instead of chaining
/// (`GestureClick`'s failure mode: its counter climbs past 2 and the
/// binding sticks). Returns the new state.
fn click_pair(last: i64, now: i64, gap: i64) -> (i64, bool) {
    if last != 0 && now - last <= gap {
        (0, true)
    } else {
        (now, false)
    }
}

/// Build the window: GNOME chrome (header bar + bottom playback toolbar)
/// around the video surface; in fullscreen the chrome retracts and the
/// playback bar moves over the video as an OSD pill.
pub fn build_window(app: &adw::Application, shared: &Shared) -> adw::ApplicationWindow {
    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some(i18n::t("app.name").as_str()));
    // 16:9 for the content area: the video fills it (the bars are chrome
    // outside it), so a 16:9 default shows typical content with no
    // letterbox bars.
    window.set_default_size(1024, 576);
    window.add_css_class("player-window");

    let video = VideoArea::new(shared.engine.clone());

    let toolbar = adw::ToolbarView::new();
    let header = build_header();
    toolbar.add_top_bar(&header);
    let (playbar, bar) = build_playbar(shared);
    toolbar.add_bottom_bar(&playbar);

    // The playback bar's fullscreen parent: an empty revealer while
    // windowed, its parent while fullscreen.
    let fs_playbar = gtk::Revealer::new();
    fs_playbar.set_child(None::<&gtk::Widget>);
    fs_playbar.set_transition_type(gtk::RevealerTransitionType::Crossfade);
    fs_playbar.set_transition_duration(250);
    fs_playbar.set_valign(gtk::Align::End);
    fs_playbar.set_hexpand(true);

    let center = build_center();
    let volume_osd = volume::build(shared);
    let seek_handle = seek::attach(&bar.seek, &shared.engine);

    let controls = Controls {
        toolbar: toolbar.clone(),
        playbar: playbar.clone(),
        fs_playbar: fs_playbar.clone(),
        bar_buttons: bar.bar_buttons,
        play_button: bar.play,
        seek: bar.seek,
        time_label: bar.time_label,
        duration_label: bar.duration_label,
        volume_button: bar.volume_button,
        volume_scale: bar.volume_scale,
        volume_scale_changed: Rc::new(bar.volume_scale_changed),
        speed_button: bar.speed_button,
        speed_scale: bar.speed_scale,
        fullscreen_button: bar.fullscreen_button,
        pin_button: bar.pin_button,
        center: center.revealer.clone(),
        center_button: center.play.clone(),
        center_prev_chapter: center.prev_chapter.clone(),
        center_next_chapter: center.next_chapter.clone(),
        volume_osd: volume_osd.clone(),
        seeking: seek_handle.seeking,
        seek_settle: seek_handle.seek_settle,
        hover: Rc::new(Cell::new(false)),
        dragging: Rc::new(Cell::new(false)),
        revealed: Rc::new(Cell::new(true)),
        fullscreen: Rc::new(Cell::new(false)),
        last_motion: Rc::new(Cell::new(now_ms())),
        last_pos: Rc::new(Cell::new((-1.0, -1.0))),
    };

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&video));
    overlay.add_overlay(&fs_playbar);
    // Last: the center row sits above the bars if they ever overlap.
    overlay.add_overlay(&center.revealer);
    // The volume OSD on the right edge: an indicator, not a control — it
    // appears on any volume change and fades again (see `ui::volume`).
    overlay.add_overlay(volume_osd.revealer());

    // Drop feedback: a dashed frame and a hint drawn *over* the video (topmost
    // overlay child), hidden until a file drag enters the window. A window-
    // level CSS outline would not do: the GLArea paints over it.
    let drop_hint = gtk::Label::builder()
        .label(i18n::t("drop.hint"))
        .xalign(0.5)
        .yalign(0.5)
        .build();
    drop_hint.add_css_class("osd");
    drop_hint.add_css_class("drop-hint");
    drop_hint.set_halign(gtk::Align::Fill);
    drop_hint.set_valign(gtk::Align::Fill);
    drop_hint.set_visible(false);
    overlay.add_overlay(&drop_hint);

    toolbar.set_content(Some(&overlay));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar));
    window.set_content(Some(&toasts));
    *shared.toasts.borrow_mut() = Some(toasts);
    *shared.controls.borrow_mut() = Some(controls.clone());

    // RTL locales mirror the layout (ar-SA, fa-IR, he-IL). Done here, once the
    // tree is assembled, so every control is covered — see `apply_rtl` for why
    // setting the direction on the window alone does nothing. The video surface
    // is unaffected: mpv draws in its own coordinates.
    apply_rtl(&window);

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

    // Resting the pointer on either bar, the center row, or in a popover must
    // not hide it — the interface does not vanish out from under a hand that is
    // already reaching for it.
    let hover_targets: Vec<gtk::Widget> = vec![
        header.upcast(),
        playbar.clone().upcast(),
        center.revealer.clone().upcast(),
    ];
    for bar in &hover_targets {
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

    // Keyboard activity counts too: with the chrome able to hide while windowed,
    // a key that changes playback (pause, a seek, a skip) has to bring the bar
    // back — otherwise the only feedback for the key is the video itself.
    let keys = gtk::EventControllerKey::new();
    {
        let controls = controls.clone();
        keys.connect_key_pressed(move |_, _, _, _| {
            controls.poke();
            glib::Propagation::Proceed
        });
    }
    window.add_controller(keys);

    // Idle check: a 500 ms tick is cheaper than re-arming a source on every
    // motion event.
    glib::timeout_add_local(Duration::from_millis(IDLE_TICK_MS), {
        let controls = controls.clone();
        let state = shared.state.clone();
        move || {
            controls.idle_tick(&state.borrow());
            glib::ControlFlow::Continue
        }
    });

    // Double-click on the video toggles fullscreen. Play/pause is *not* bound
    // to clicking the video (that was an easy accidental pause); it lives on
    // the center button, the bar, and Space.
    //
    // Manual pair detection, not `GestureClick`: its multi-press counter
    // keeps climbing while clicks stay inside its timing window (3, 4, 5…),
    // so `n_press == 2` matches only the first pair and the binding then
    // appears stuck no matter how often you double-click. A non-claiming
    // legacy controller plus a timestamp pair is stateless per pair and
    // always re-arms; the gap honours the `gtk-double-click-time` setting.
    let dbl = gtk::EventControllerLegacy::new();
    {
        let window_clone = window.clone();
        let last_press = Rc::new(Cell::new(0i64));
        dbl.connect_event(move |_ctrl, event| {
            if event.event_type() == gtk::gdk::EventType::ButtonPress
                && let Some(button) = event.downcast_ref::<gtk::gdk::ButtonEvent>()
                && button.button() == 1
            {
                let now = now_ms();
                let gap = gtk::Settings::default()
                    .map_or(400, |s| s.gtk_double_click_time())
                    .max(1) as i64;
                let (state, fired) = click_pair(last_press.get(), now, gap);
                last_press.set(state);
                if fired {
                    if window_clone.is_fullscreen() {
                        window_clone.unfullscreen();
                    } else {
                        window_clone.fullscreen();
                    }
                }
            }
            glib::Propagation::Proceed
        });
    }
    video.add_controller(dbl);

    dnd::attach(&window, &drop_hint, &controls, shared);

    // Wheel anywhere in the window steps the volume through `app.volume-rel`,
    // the same action the Up/Down keys use. The bars' own sliders claim
    // their scroll sequences first, so this never steals them (see `ui::volume`).
    volume::attach_scroll(&window);

    // Keep the fullscreen controls honest: the chrome retracts, the bar
    // moves over the video, and its button shows the way out.
    window.connect_fullscreened_notify({
        let controls = controls.clone();
        move |window| {
            let fullscreen = window.is_fullscreen();
            controls.set_fullscreen(fullscreen);
            controls.fullscreen_button.set_icon_name(if fullscreen {
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
            overlay.add_toast(adw::Toast::new(&i18n::t("toast.noGl")));
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

#[cfg(test)]
mod tests {
    use super::click_pair;

    #[test]
    fn ten_rapid_clicks_never_stick() {
        // The reported bug: after the first pair, `GestureClick`'s counter
        // climbed to 3, 4, 5… and no further double click registered.
        let gap = 400;
        let mut state = 0;
        let mut fired = Vec::new();
        for i in 0..10 {
            let now = 1000 + i * 100;
            let (next, hit) = click_pair(state, now, gap);
            state = next;
            if hit {
                fired.push(now);
            }
        }
        // Five clean pairs, spread across the whole burst — not just the first.
        assert_eq!(fired.len(), 5);
        assert_eq!(fired[4], 1900);
    }

    #[test]
    fn clicks_outside_the_gap_are_singles() {
        let (state, hit) = click_pair(0, 1000, 400);
        assert!(!hit);
        let (state, hit) = click_pair(state, 1600, 400);
        assert!(!hit);
        assert_eq!(state, 1600);
    }

    #[test]
    fn a_third_click_starts_a_new_pair() {
        // double + single must not chain into another double.
        let (state, hit) = click_pair(1000, 1100, 400);
        assert!(hit && state == 0);
        let (state, hit) = click_pair(state, 1200, 400);
        assert!(!hit && state == 1200);
    }
}
