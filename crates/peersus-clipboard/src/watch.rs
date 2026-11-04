//! Real clipboard-change events on Linux, from the compositor itself.
//!
//! Compositors that offer the `ext-data-control` or `wlr-data-control` protocol (KDE, Hyprland,
//! sway, recent GNOME) tell a client the moment another application copies. Polling cannot: it
//! sees only what is there now, so it cannot tell a new copy from old contents, and has to
//! discard the first text after anything that was not text. An event can.
//!
//! The thread here NEVER reads what was copied. It hears only that the selection changed and
//! which types are on offer, and says whether one of them is text. Reading the text is left to
//! the owner thread, and happens only when the session asks, which is only while a person has
//! switched automatic sending on: with it off, nothing here ever looks at the clipboard.
use std::sync::mpsc::SyncSender;
#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
use std::thread;

/// What the compositor said about the selection, and nothing about its contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Signal {
    /// Something was copied that offers text.
    Text,
    /// A picture, a file list, or a cleared selection: announced, but not text.
    Other,
}

/// Whether any offered type is plain text. A type that names another charset does not count,
/// because it could not be trusted to decode.
pub fn offers_text(offered: &[String]) -> bool {
    const TEXT: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
    offered
        .iter()
        .any(|have| TEXT.iter().any(|wanted| have.eq_ignore_ascii_case(wanted)))
}

/// Start listening. False if neither source of real copy events exists in this session, and the
/// caller then keeps comparing snapshots, as before.
///
/// Two sources, tried in this order:
/// 1. The compositor's own data-control protocol (KDE, Hyprland, sway, COSMIC, ...).
/// 2. The X11 selection, through XFixes. GNOME offers no data-control at all, but it mirrors the
///    clipboard to XWayland, which tells a client the moment the owner changes. A pure X11
///    session is the same thing without the mirror.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
pub fn start(events: SyncSender<Signal>) -> bool {
    wayland(events.clone()) || x11(events)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
fn wayland(events: SyncSender<Signal>) -> bool {
    use wl_clipboard_rs::{
        paste::{ClipboardType, Seat},
        watch::{ClipboardEvent, Watcher},
    };
    // Constructing it here, not on the thread, is what lets this answer "is there such a
    // protocol" truthfully before anything depends on the answer.
    let Ok(mut watcher) = Watcher::new(ClipboardType::Regular.into(), Seat::Unspecified) else {
        return false;
    };
    thread::Builder::new()
        .name("clipboard-watch".into())
        .spawn(move || {
            loop {
                let signal = match watcher.next_event() {
                    Ok(Some(ClipboardEvent::Changed { mime_types, .. })) => {
                        if offers_text(&mime_types) {
                            Signal::Text
                        } else {
                            Signal::Other
                        }
                    }
                    Ok(Some(ClipboardEvent::Cleared { .. })) => Signal::Other,
                    // Cancelled, or the compositor went away: nothing more will ever arrive.
                    Ok(None) | Err(_) => return,
                };
                if !announce(&events, signal) {
                    return;
                }
            }
        })
        .is_ok()
}

/// X11 reports each change of owner, and often several for one copy (the owner is set, the old
/// window is destroyed, the bridge sets it again). They all mean "something changed", which is
/// all a signal says, and the owner thread keeps only the newest.
///
/// Whether the new owner offers text is not asked here: asking means talking to the owner, which
/// is reading. Every announcement is `Signal::Text`, and the owner thread's read, which only
/// happens when the session asks, returns text, or nothing if what was copied is not text.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
fn x11(events: SyncSender<Signal>) -> bool {
    use x11rb::{
        COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT,
        connection::Connection,
        protocol::{
            Event,
            xfixes::{ConnectionExt as _, SelectionEventMask},
            xproto::{ConnectionExt as _, CreateWindowAux, WindowClass},
        },
    };
    let Ok((conn, screen)) = x11rb::connect(None) else {
        return false;
    };
    // Absent on a server too old or too restricted to say: then there is nothing to listen to.
    if conn
        .xfixes_query_version(5, 0)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_none()
    {
        return false;
    }
    let (Ok(window), Some(root)) = (
        conn.generate_id(),
        conn.setup().roots.get(screen).map(|s| s.root),
    ) else {
        return false;
    };
    // Each request is confirmed by the server before going on: an INPUT_ONLY window that was not
    // made, or a selection that was refused, would otherwise listen to nothing and say nothing.
    let created = conn
        .create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            COPY_FROM_PARENT,
            &CreateWindowAux::new(),
        )
        .ok()
        .and_then(|cookie| cookie.check().ok());
    let Some(clipboard) = conn
        .intern_atom(false, b"CLIPBOARD")
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return false;
    };
    let selected = conn
        .xfixes_select_selection_input(
            window,
            clipboard.atom,
            SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )
        .ok()
        .and_then(|cookie| cookie.check().ok());
    if created.is_none() || selected.is_none() {
        return false;
    }
    thread::Builder::new()
        .name("clipboard-watch-x11".into())
        .spawn(move || {
            loop {
                match conn.wait_for_event() {
                    Ok(Event::XfixesSelectionNotify(_)) => {
                        if !announce(&events, Signal::Text) {
                            return;
                        }
                    }
                    Ok(_) => {}
                    // The server went away: nothing more will ever arrive.
                    Err(_) => return,
                }
            }
        })
        .is_ok()
}

/// Tell the owner thread. A full queue means the plugin is behind: the owner thread drains to the
/// newest signal anyway, so a dropped one only ever loses something already replaced, and this
/// thread must never block. False once the plugin is gone.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
fn announce(events: &SyncSender<Signal>, signal: Signal) -> bool {
    !matches!(
        events.try_send(signal),
        Err(std::sync::mpsc::TrySendError::Disconnected(_))
    )
}

/// Everywhere else the system library is all there is: no copy events, so snapshots.
#[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android")))))]
pub fn start(_events: SyncSender<Signal>) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(types: &[&str]) -> Vec<String> {
        types.iter().map(|t| (*t).to_owned()).collect()
    }

    #[test]
    fn plain_text_types_count_and_other_charsets_and_pictures_do_not() {
        assert!(offers_text(&offered(&[
            "TARGETS",
            "text/plain",
            "text/plain;charset=utf-8"
        ])));
        assert!(offers_text(&offered(&["text/plain"])));
        assert!(offers_text(&offered(&["UTF8_STRING"])));
        assert!(offers_text(&offered(&["TEXT/PLAIN;CHARSET=UTF-8"])));
        assert!(!offers_text(&offered(&["text/plain;charset=iso-8859-1"])));
        assert!(!offers_text(&offered(&["image/png"])));
        assert!(!offers_text(&offered(&[
            "text/uri-list",
            "x-special/gnome-copied-files"
        ])));
        assert!(!offers_text(&[]));
    }
}
