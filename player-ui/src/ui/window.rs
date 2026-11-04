//! Window assembly: the video surface with the controls floating over it,
//! the header and playback bars, and the window-level gestures (move by
//! header drag, double-click fullscreen, volume by wheel).

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

fn build_header(window: &adw::ApplicationWindow) -> (gtk::Revealer, gtk::Label) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("floating-bar");
    bar.add_css_class("floating-header");

    let open = flat_button(
        "document-open-symbolic",
        Some("app.open"),
        &i18n::t("header.open"),
    );

    let title = gtk::Label::builder()
        .label(i18n::t("app.name"))
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(48)
        .xalign(0.5)
        .build();
    title.add_css_class("header-title");
    // The title is the visible handle of the window-move drag below. GTK4
    // CSS has no `cursor` property (that was GTK3) — the cursor is a widget
    // property set in code.
    title.set_cursor_from_name(Some("grab"));
    title.set_hexpand(true);
    title.set_halign(gtk::Align::Center);

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

    let close = flat_button("window-close-symbolic", None, &i18n::t("header.close"));
    let window_clone = window.clone();
    close.connect_clicked(move |_| window_clone.close());

    // Drag the header bar to move the window: it is the titlebar of a window
    // that has none. `begin_move` sends xdg_toplevel.move, which Hyprland
    // only began honoring in 0.56 ("xdg interactive drags") — before that a
    // client-initiated move was ignored and the drag did nothing.
    //
    // A claimed sequence still reaches the claiming widget's ancestors, so a
    // press on one of the bar's buttons WOULD arrive here too — hit-test the
    // press against every control's bounds and leave those to the buttons.
    // Presses on the bar itself (title, gaps, padding) start the move; the
    // compositor takes over from there, a press without motion is a no-op,
    // and the controls stay up because the hover controller keeps reporting
    // the pointer as inside the bar.
    let move_drag = gtk::EventControllerLegacy::new();
    {
        let window = window.clone();
        let controls: Vec<gtk::Widget> =
            vec![open.clone().upcast(), menu.clone().upcast(), close.clone().upcast()];
        move_drag.connect_event(move |_ctrl, event| {
            if event.event_type() == gtk::gdk::EventType::ButtonPress
                && let Some(button) = event.downcast_ref::<gtk::gdk::ButtonEvent>()
                && button.button() == 1
                && !window.is_fullscreen()
                && let Some((x, y)) = event.position()
                && !controls.iter().any(|control| {
                    control.compute_bounds(&window).is_some_and(|bounds| {
                        x >= f64::from(bounds.x())
                            && x < f64::from(bounds.x() + bounds.width())
                            && y >= f64::from(bounds.y())
                            && y < f64::from(bounds.y() + bounds.height())
                    })
                })
                && let Some(device) = event.device()
                && let Some(toplevel) = window
                    .surface()
                    .and_then(|surface| surface.downcast::<gtk::gdk::Toplevel>().ok())
            {
                toplevel.begin_move(&device, 1, x, y, event.time());
            }
            glib::Propagation::Proceed
        });
    }
    bar.add_controller(move_drag);

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
        preset.add_css_class("pill");
        let speed_scale = speed_scale.clone();
        preset.connect_clicked(move |_| speed_scale.set_value(speed));
        speed_row.append(&preset);
    }
    let speed_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    speed_box.append(&speed_scale);
    speed_box.append(&speed_row);
    speed_button.set_popover(Some(&gtk::Popover::builder().child(&speed_box).build()));

    let fullscreen_button = flat_button(
        "view-fullscreen-symbolic",
        Some("app.fullscreen"),
        &i18n::t("bar.fullscreen"),
    );

    bar.append(&time_label);
    bar.append(&seek);
    bar.append(&duration_label);
    bar.append(&volume_button);
    bar.append(&speed_button);
    bar.append(&fullscreen_button);

    (
        overlay_revealer(&bar, gtk::Align::End),
        PlayBar {
            seek,
            time_label,
            duration_label,
            volume_button,
            volume_scale,
            volume_scale_changed,
            speed_button,
            speed_scale,
            fullscreen_button,
        },
    )
}

struct PlayBar {
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
    fullscreen_button: gtk::Button,
}

/// The big play/pause button in the middle of the video (icon only, no
/// background), with the prev/next skip buttons flanking it left and right.
/// The row rides with
/// the floating controls (see `Controls::set_revealed`): visible while the
/// user is interacting or paused, faded out on idle. Its icon tracks the play
/// state, and it starts as a play triangle — the player opens with nothing
/// loaded and this button is the control that starts something.
fn build_center() -> (gtk::Revealer, gtk::Button) {
    let button = gtk::Button::builder()
        .icon_name(Controls::play_icon(true))
        .action_name("app.play-pause")
        .build();
    button.add_css_class("flat");
    button.add_css_class("center-play");

    // Skip to the previous/next file in the directory; with no playlist to
    // walk the core steps chapters instead (see `skip_target`). They sit
    // beside the play button, not in the playbar: these are the reach-for-
    // them-while-watching controls, and the center row is where the eyes
    // already are. `<`/`>` keep the same actions.
    //
    // `valign: center` is not decoration: the row is as tall as the 92px
    // play button, and a box stretches its children to fill vertically —
    // the skip buttons' press box would be a 56x92 ellipse, not a circle.
    let prev_skip = flat_button(
        "media-skip-backward-symbolic",
        Some("app.skip-prev"),
        &i18n::t("center.prev"),
    );
    prev_skip.add_css_class("center-skip");
    prev_skip.set_valign(gtk::Align::Center);
    let next_skip = flat_button(
        "media-skip-forward-symbolic",
        Some("app.skip-next"),
        &i18n::t("center.next"),
    );
    next_skip.add_css_class("center-skip");
    next_skip.set_valign(gtk::Align::Center);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&prev_skip);
    row.append(&button);
    row.append(&next_skip);

    let revealer = gtk::Revealer::new();
    revealer.set_child(Some(&row));
    revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
    revealer.set_transition_duration(250);
    revealer.set_halign(gtk::Align::Center);
    revealer.set_valign(gtk::Align::Center);
    // Start revealed to match the controls' initial state (they are shown on
    // open and fade on idle); `set_revealed` keeps them in sync from then on.
    revealer.set_reveal_child(true);
    (revealer, button)
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

/// Build the window: video surface with the controls floating over it.
pub fn build_window(app: &adw::Application, shared: &Shared) -> adw::ApplicationWindow {
    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some(i18n::t("app.name").as_str()));
    // 16:9: the video fills the window (header/playbar are overlays), so a
    // 16:9 default shows typical content with no letterbox bars.
    window.set_default_size(1024, 576);
    window.add_css_class("player-window");

    let video = VideoArea::new(shared.engine.clone());
    let (header, title) = build_header(&window);
    let (playbar, bar) = build_playbar(shared);
    let (center, center_button) = build_center();
    let volume_osd = volume::build();
    let seek_handle = seek::attach(&bar.seek, &shared.engine);

    let controls = Controls {
        header: header.clone(),
        playbar: playbar.clone(),
        title,
        seek: bar.seek,
        time_label: bar.time_label,
        duration_label: bar.duration_label,
        volume_button: bar.volume_button,
        volume_scale: bar.volume_scale,
        volume_scale_changed: Rc::new(bar.volume_scale_changed),
        speed_button: bar.speed_button,
        speed_scale: bar.speed_scale,
        fullscreen_button: bar.fullscreen_button,
        center: center.clone(),
        center_button: center_button.clone(),
        volume_osd: volume_osd.clone(),
        seeking: seek_handle.seeking,
        seek_settle: seek_handle.seek_settle,
        hover: Rc::new(Cell::new(false)),
        dragging: Rc::new(Cell::new(false)),
        revealed: Rc::new(Cell::new(true)),
        last_motion: Rc::new(Cell::new(now_ms())),
        last_pos: Rc::new(Cell::new((-1.0, -1.0))),
    };

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&video));
    overlay.add_overlay(&header);
    overlay.add_overlay(&playbar);
    // Last: the center button sits above the bars if they ever overlap.
    overlay.add_overlay(&center);
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
    drop_hint.add_css_class("drop-hint");
    drop_hint.set_halign(gtk::Align::Fill);
    drop_hint.set_valign(gtk::Align::Fill);
    drop_hint.set_visible(false);
    overlay.add_overlay(&drop_hint);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&overlay));
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

    // Resting the pointer on a bar, the center row, or in a popover must not
    // hide it.
    for bar in [&header, &playbar, &center] {
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
            controls.idle_tick(&state.borrow());
            glib::ControlFlow::Continue
        }
    });

    // Double-click on the video toggles fullscreen. Play/pause is *not* bound
    // to clicking the video (that was an easy accidental pause); it lives on
    // the center button and Space.
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
