//! Runs the real copy listener against the real desktop it is started from. It needs a display, so
//! it is skipped unless asked for:
//!
//!     cargo test -p peersus-clipboard --test live_clipboard_signal -- --ignored
//!
//! Nothing here reads or reports what is copied; it only proves that a copy is announced.
#![cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
use peersus_clipboard::watch::{self, Signal};
use std::{sync::mpsc, time::Duration};

#[test]
#[ignore = "needs a real desktop session (Wayland with data-control, or X11/XWayland)"]
fn a_copy_made_by_another_application_is_announced() {
    let (signals, heard) = mpsc::sync_channel(16);
    assert!(
        watch::start(signals),
        "this session offers neither data-control nor an X11 selection to listen to"
    );
    // The system clipboard library is what every other application's copy looks like from here.
    let mut other_application = arboard::Clipboard::new().expect("a clipboard to copy to");
    other_application
        .set_text("peersus live signal test")
        .expect("a copy");
    let first = heard
        .recv_timeout(Duration::from_secs(5))
        .expect("the copy was never announced");
    assert!(matches!(first, Signal::Text | Signal::Other));
    // A second, different copy is announced too: it is not a one-time event.
    while heard.try_recv().is_ok() {}
    other_application
        .set_text("peersus live signal test, again")
        .expect("a second copy");
    heard
        .recv_timeout(Duration::from_secs(5))
        .expect("the second copy was never announced");
}
