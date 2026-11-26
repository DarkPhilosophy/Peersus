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
    /// The last placement of the pages failed: it is tried again with the next delivery.
    pages_failed: bool,
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
            Err(_) => {
                // A broken exchange still tries to let go of a pointer or gesture this device holds on the other
                // one; whatever the host can no longer take is not retried.
                let _ = state.finish(host);
                return 1;
            }
        };
        if let Some(reply) = remote::handle(host, &mut state, &delivery)
            && host.send(reply).is_err()
        {
            return 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host::{Handle, scripted::Script};
    use serde_json::{Value, json};

    /// A host whose control channel always looks ready and then breaks: an answer that never comes.
    struct Broken(Script);
    impl Wire for Broken {
        fn send(&mut self, value: Value) -> Result<(), &'static str> {
            self.0.send(value)
        }
        fn receive(&mut self) -> Result<Value, &'static str> {
            self.0.receive()
        }
    }
    impl Data for Broken {
        fn open(&mut self, claim: bool, number: u64) -> Result<Handle, &'static str> {
            self.0.open(claim, number)
        }
        fn read(&mut self, handle: Handle, into: &mut [u8]) -> Result<usize, &'static str> {
            self.0.read(handle, into)
        }
        fn write(&mut self, handle: Handle, from: &[u8]) -> Result<usize, &'static str> {
            self.0.write(handle, from)
        }
        fn shutdown(&mut self, handle: Handle) -> Result<(), &'static str> {
            self.0.shutdown(handle)
        }
        fn close(&mut self, handle: Handle) {
            self.0.close(handle)
        }
        fn poll(&mut self, _: &[Handle], _: i32) -> Result<Option<Handle>, &'static str> {
            Ok(Some(0))
        }
    }

    #[test]
    fn a_broken_exchange_still_lets_go_of_the_pointer_it_holds_on_the_other_device() {
        let mut host = Broken(Script::default());
        let config = json!({"plugin":"control","settings":{},"missing_permissions":[],"peers":[
            {"device":"pc","permissions":[{"owner":"host","name":"peers.send"},{"owner":"control","name":"control.use"}]}
        ]});
        // Settings, the page placement, then a stream for a press held on the other device.
        host.0.results.extend([
            json!({"status":"ok","value":config}),
            json!({"status":"ok","value":{}}),
            json!({"status":"ok","value":{"claim":1}}),
        ]);
        host.0
            .deliveries
            .push_back(json!({"type":"call","ticket":1,"method":"input","args":
            {"peer":"pc","events":[{"t":"button","button":"left","down":true}],"end":false}}));
        // Nothing more is scripted: the next exchange fails as a broken protocol does.
        assert_eq!(run(&mut host), 1);
        assert_eq!(
            host.0.shutdowns, 1,
            "the held stream is ended, so the other side lets go"
        );
    }
}
