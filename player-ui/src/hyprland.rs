//! The Hyprland seam. "Pinned" is the compositor's window state, not a GTK
//! property — only Hyprland knows what "stay above everything, on every
//! workspace" means — so the player asks for the change through `hyprctl` and
//! reads it back from the compositor's own event stream. Aelum is a Hyprland
//! desktop, which is why this talks to a compositor at all; with none to talk to
//! the pin button does nothing and the log says why.

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use gtk::glib;

/// How often the event socket is drained. The read never blocks, so this only
/// bounds how soon a change made outside the player is reflected in the button.
const PIN_POLL_MS: u64 = 300;

/// Ask the compositor to pin (`on`) or unpin *this* window, then read the state
/// back. The selector is our pid rather than the active window: the button then
/// works whatever has focus, and a request with no target is refused outright
/// (Hyprland answers "Window does not qualify to be pinned") — which would
/// otherwise light the button for a state the compositor never took.
///
/// `None` = there is nothing to read back from (no hyprctl, no window), and the
/// caller keeps what it asked for.
pub fn set_pin(on: bool) -> Option<bool> {
    let pid = std::process::id();
    let call = format!(
        "hl.dsp.window.pin({{ action = \"{}\", window = \"pid:{pid}\" }})",
        if on { "on" } else { "off" }
    );
    match std::process::Command::new("hyprctl")
        .args(["dispatch", &call])
        .status()
    {
        Ok(status) if !status.success() => {
            tracing::warn!(code = ?status.code(), "hyprctl pin failed");
        }
        Err(err) => tracing::warn!(%err, "could not run hyprctl — not a Hyprland session?"),
        _ => {}
    }
    pinned_now()
}

/// This window's pinned state as the compositor sees it (`hyprctl clients -j`
/// carries our pid). `None` when this process has no Hyprland window, which is
/// the signal to leave the current state alone rather than guess.
fn pinned_now() -> Option<bool> {
    let output = std::process::Command::new("hyprctl")
        .args(["clients", "-j"])
        .output()
        .ok()?;
    let clients: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    clients
        .as_array()?
        .iter()
        .find(|client| client["pid"].as_i64() == Some(std::process::id() as i64))?
        .get("pinned")?
        .as_bool()
}

/// Keep the caller's idea of the pin state equal to the window's real one.
///
/// The compositor's event socket (`$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/
/// .socket2.sock`) streams one line per event, for every window — pinning reads
/// `pin>><address>,<0|1>`. Any pin event therefore makes us *measure* our own
/// state again instead of trusting what we last asked for: a user who unpins with
/// their own keybind sees the button follow, and the button never claims a state
/// the compositor does not have.
///
/// Non-blocking read on a slow timer: no thread, no per-event source, and the
/// poll only decides how fast an outside change shows up. No socket (not a
/// Hyprland session, a stale instance signature) means no watcher — the button
/// still works, it just keeps the state it asked for.
pub fn watch_pin_state(on_pin: impl Fn(bool) + 'static) {
    let path = match (
        std::env::var("XDG_RUNTIME_DIR"),
        std::env::var("HYPRLAND_INSTANCE_SIGNATURE"),
    ) {
        (Ok(runtime), Ok(signature)) => format!("{runtime}/hypr/{signature}/.socket2.sock"),
        _ => return,
    };
    let Ok(mut socket) = UnixStream::connect(&path) else {
        tracing::info!(
            path = %path,
            "no Hyprland event socket: the pin button will not follow changes made outside it"
        );
        return;
    };
    if socket.set_nonblocking(true).is_err() {
        return;
    }

    let mut buffer = String::new();
    let mut chunk = [0u8; 4096];
    glib::timeout_add_local(Duration::from_millis(PIN_POLL_MS), move || {
        match socket.read(&mut chunk) {
            // 0 bytes = the compositor closed it; a closed socket is not worth
            // re-opening, the player's own clicks still work.
            Ok(0) => return glib::ControlFlow::Break,
            Ok(read) => buffer.push_str(&String::from_utf8_lossy(&chunk[..read])),
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            Err(err) => {
                tracing::debug!(%err, "Hyprland event socket stopped");
                return glib::ControlFlow::Break;
            }
        }
        // One event per line; a partial line waits for the rest of it.
        let mut pin_event = false;
        while let Some(end) = buffer.find('\n') {
            let line = buffer[..end].to_owned();
            buffer.drain(..=end);
            if line.split(">>").next() == Some("pin") {
                pin_event = true;
            }
        }
        if pin_event {
            match pinned_now() {
                Some(pinned) => on_pin(pinned),
                None => tracing::debug!("a pin event for a window that is not ours"),
            }
        }
        glib::ControlFlow::Continue
    });
}
