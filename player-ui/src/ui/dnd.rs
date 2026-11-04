//! Drag & drop: dropping files anywhere on the window opens them. The
//! target type is `gdk::FileList` — GTK's own representation of
//! `text/uri-list` — so a file-manager drag (one file or many) and a
//! dragged link all arrive as a list of GFiles, and non-file drags never
//! reach the handlers: the target rejects them by format.
//!
//! Advertise COPY *and* MOVE: what a source offers is its own choice —
//! Nautilus drags here as MOVE-only, and a COPY-only target fails the
//! action negotiation before the drop signal ever fires (the silent
//! "nothing happens" first attempt). `enter` must answer with a single
//! action from the intersection; the default handler computes exactly
//! that, so mirror it rather than hard-coding COPY.

use gtk::prelude::*;

use crate::Shared;

use super::controls::Controls;

/// Attach the window's drop target. `drop_hint` is the "Drop to play" overlay,
/// shown while a file drag is inside the window.
pub(super) fn attach(
    window: &impl IsA<gtk::Widget>,
    drop_hint: &gtk::Label,
    controls: &Controls,
    shared: &Shared,
) {
    let drop = gtk::DropTarget::new(
        gtk::gdk::FileList::static_type(),
        gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
    );
    {
        let (hint, ctrl) = (drop_hint.clone(), controls.clone());
        drop.connect_enter(move |target, _x, _y| {
            hint.set_visible(true);
            ctrl.set_drag_active(true);
            let offered = target
                .current_drop()
                .map(|d| d.actions())
                .unwrap_or(gtk::gdk::DragAction::COPY);
            for action in [
                gtk::gdk::DragAction::COPY,
                gtk::gdk::DragAction::MOVE,
                gtk::gdk::DragAction::LINK,
            ] {
                if offered.contains(action) {
                    return action;
                }
            }
            gtk::gdk::DragAction::MOVE
        });
        let (hint, ctrl) = (drop_hint.clone(), controls.clone());
        drop.connect_leave(move |_| {
            hint.set_visible(false);
            ctrl.set_drag_active(false);
        });
        let (hint, ctrl, shared) = (drop_hint.clone(), controls.clone(), shared.clone());
        drop.connect_drop(move |_target, value, _x, _y| {
            hint.set_visible(false);
            // The drag session ends here; `leave` is not guaranteed to follow a
            // drop, so clear the flag directly (a rejected drop must not leave
            // the controls stuck hidden). `open_dropped` then pokes them back.
            ctrl.set_drag_active(false);
            let Ok(list) = value.get_owned::<gtk::gdk::FileList>() else {
                return false;
            };
            open_dropped(&shared, &ctrl, &list.files())
        });
    }
    window.add_controller(drop);
}

/// Open the files carried by a drop: the first one plays, the rest are
/// reported (there is no playlist yet). Returns whether the drop was handled.
fn open_dropped(shared: &Shared, controls: &Controls, files: &[gtk::gio::File]) -> bool {
    let Some(first) = files.first() else {
        return false;
    };
    let source = crate::source_of(first);
    tracing::info!(%source, count = files.len(), "dropped files");
    crate::open_source(shared, &source, 0.0);
    if files.len() > 1 {
        let count = files.len().to_string();
        crate::toast(
            shared,
            &crate::i18n::t_fmt("toast.dndExtra", &[("count", &count)]),
        );
    }
    // A DND drag delivers no motion events, so the idle timer may have hidden
    // the controls while the drop was in flight.
    controls.poke();
    true
}
