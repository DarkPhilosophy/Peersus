//! Clipboard text sharing as a sandboxed guest: the application owns every operating-system resource.
//!
//! The guest reads and replaces the clipboard only through the application's `peersus.clipboard` service and
//! moves bytes only through the data connections the application issues. It names no path, socket or clipboard API.
#![cfg(any(target_family = "wasm", feature = "native", test))]

mod actions;
mod automatic;
mod config;
mod events;
mod host;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
mod native;
mod observation;
mod watch;
mod wire;

#[cfg(any(target_family = "wasm", feature = "native"))]
use serde_json::Value;

/// The entry the interpreter calls. Returns zero when the application goes away, non-zero on a broken protocol.
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
    use host::{Data, Host, Wire};
    let mut host = Host;
    let mut next = 0u64;
    let mut state = automatic::Automatic::new();
    let mut backlog: std::collections::VecDeque<Value> = std::collections::VecDeque::new();
    // Ask once to be told when the clipboard changes. A refusal is not fatal: without the permission this guest still
    // answers every explicit call, and automatic sending simply never wakes.
    if host.send(events::subscribe()).is_err() {
        return 1;
    }
    // The platform reports a change only while somebody observes, and the only observing this guest does is in answer to
    // a change. Waiting for the first notice would therefore wait for ever: startup takes the baseline and then reads
    // once, which is what makes the platform start telling. It sends only what the settings already allow.
    if watch::start(&mut host, &mut state, &mut next, &mut backlog) == watch::Pass::Stop {
        return 1;
    }
    let mut retry = watch::Retry::new();
    loop {
        // Anything that arrived while an earlier delivery was handled comes first, in the order it came.
        let delivery = match backlog.pop_front() {
            Some(delivery) => delivery,
            None => {
                // A baseline that failed is retried after a bounded wait, because no notice can come to retry it.
                if let Some(wait) = retry.wait(&state) {
                    match host.poll(&[], wait) {
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            if watch::on_change(&mut host, &mut state, &mut next, &mut backlog)
                                == watch::Pass::Stop
                            {
                                return 1;
                            }
                            continue;
                        }
                        Err(_) => return 1,
                    }
                }
                match host.receive() {
                    Ok(delivery) => delivery,
                    Err(_) => return 0,
                }
            }
        };
        // A call already cancelled while it waited is not started: the application no longer waits for it, and starting it
        // could send clipboard text before the cancellation is looked at.
        if actions::withdrawn(&delivery, &mut backlog) {
            continue;
        }
        if watch::wakes(&delivery) {
            if watch::on_change(&mut host, &mut state, &mut next, &mut backlog) == watch::Pass::Stop
            {
                return 1;
            }
            continue;
        }
        match actions::handle(&mut host, &delivery, &mut next) {
            actions::Handled::Ignored => {}
            actions::Handled::Stop => return 1,
            actions::Handled::Cancelled { waiting } => backlog.extend(waiting),
            actions::Handled::Replied {
                reply,
                waiting,
                placed,
            } => {
                // Before anything else is looked at, including the notice this very write is about to cause: that text is
                // this device's own and must never be sent back as if somebody had just copied it.
                if let Some(text) = placed {
                    state.received(&text);
                }
                backlog.extend(waiting);
                if host.send(reply).is_err() {
                    return 1;
                }
            }
        }
    }
}
