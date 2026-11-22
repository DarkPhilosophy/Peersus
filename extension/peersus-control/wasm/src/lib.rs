//! Control's extension-owned interface, gesture interpretation and remote input transport.
#![cfg(any(target_family = "wasm", feature = "native", test))]

mod host;
mod layout;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
mod native;
mod remote;
mod ui;

use host::{Data, Wire};

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
    /// Owners whose completed rotation has not yet been taken over by a new receive stream.
    handoffs: std::collections::BTreeSet<String>,
    pub ui: ui::Ui,
    pub layout: layout::Ui,
    /// The caller-scoped permission snapshot and any checked denial since it was read.
    readiness: host::Readiness,
    /// A stopped visit cannot reopen from an ordinary or stale sample, even after a grant returns.
    remote_denials: std::collections::BTreeSet<String>,
    local_stops: std::collections::BTreeSet<String>,
    /// An acknowledgement can arrive while a gesture is still active, ahead of its next call.
    early_end: Option<(String, remote::Refusal)>,
    /// Which of its pages the person's settings placed among the application's pages.
    placed_pages: Option<Vec<&'static str>>,
}

/// The entry the interpreter calls. Returns zero when the application goes away, non-zero on a
/// broken protocol.
/// The entry the interpreter calls.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    run(&mut host::Host)
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
    let result = run(&mut host::Host);
    native::leave();
    result
}

fn run(host: &mut (impl Wire + Data)) -> i32 {
    let mut state = State::default();
    if host.send(host::subscribe_config()).is_err() || state.configure_pages(host).is_err() {
        return 1;
    }
    loop {
        let delivery = match state.next(host) {
            Ok(Some(delivery)) => delivery,
            Ok(None) => continue,
            Err("The host disconnected") => return 0,
            Err(_) => return 1,
        };
        if let Some(reply) = remote::handle(host, &mut state, &delivery)
            && host.send(reply).is_err()
        {
            return 1;
        }
    }
}
