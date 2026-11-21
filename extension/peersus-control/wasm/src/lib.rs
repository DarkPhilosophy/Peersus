//! Control's extension-owned interface, gesture interpretation and remote input transport.
#![cfg(any(target_family = "wasm", feature = "native", test))]

mod host;
mod layout;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
mod native;
mod remote;
mod ui;

/// What this guest keeps between calls: the connections of the remote part, the deliveries that wait, and the state of its two
/// pages. Kept here, above the three, so that the part that moves input and the pages that draw it need each other only one way.
#[derive(Default)]
pub(crate) struct State {
    pub outgoing: Option<remote::Outgoing>,
    /// A closed stream may leave a resting pointer or an ongoing gesture on this device.
    retained: Option<String>,
    pub next: u64,
    /// Deliveries that arrived while waiting for an answer of ours, to be handled afterwards.
    pub waiting: Vec<serde_json::Value>,
    incoming: std::collections::BTreeMap<host::Handle, remote::Incoming>,
    pub ui: ui::Ui,
    pub layout: layout::Ui,
    /// Which of its pages the person's options placed among the application's pages, as last placed, and when (page time) they
    /// were last read: the application keeps the guest running when an option changes, so the guest looks again while its
    /// pages are used.
    placed_pages: Option<Vec<&'static str>>,
    pages_checked: u64,
}

/// The entry the interpreter calls. Returns zero when the application goes away, non-zero on a
/// broken protocol.
/// The entry the interpreter calls.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    run()
}

/// The entry the native host calls, with its table of the same functions the interpreter offers.
///
/// # Safety
/// `host` is the native host's table, valid for the whole call.
#[cfg(all(feature = "native", not(target_family = "wasm")))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn peersus_plugin_main(host: *const native::HostV1) -> i32 {
    if !native::enter(host) {
        return 1;
    }
    let result = run();
    native::leave();
    result
}

#[cfg(any(target_family = "wasm", feature = "native"))]
fn run() -> i32 {
    use host::{Host, Wire};
    let mut host = Host;
    let mut state = State::default();
    if state.configure_pages(&mut host).is_err() {
        return 1;
    }
    loop {
        let delivery = match state.next(&mut host) {
            Ok(Some(delivery)) => delivery,
            Ok(None) => continue,
            Err(_) => return 0,
        };
        if let Some(reply) = remote::handle(&mut host, &mut state, &delivery)
            && host.send(reply).is_err()
        {
            return 1;
        }
    }
}
