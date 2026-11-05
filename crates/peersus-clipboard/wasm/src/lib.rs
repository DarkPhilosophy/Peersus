//! Clipboard text sharing as a sandboxed guest: the application owns every operating-system resource.
//!
//! The guest reads and replaces the clipboard only through the application's `peersus.clipboard` service and
//! moves bytes only through the data connections the application issues. It names no path, socket or clipboard API.
#![cfg(any(target_family = "wasm", test))]

mod actions;
mod automatic;
mod config;
mod events;
mod host;
mod observation;
mod watch;
mod wire;

#[cfg(target_family = "wasm")]
use serde_json::Value;

/// The entry the interpreter calls. Returns zero when the application goes away, non-zero on a broken protocol.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    use host::{Host, Wire};
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
    loop {
        // Anything that arrived while an earlier delivery was handled comes first, in the order it came.
        let delivery = match backlog.pop_front() {
            Some(delivery) => delivery,
            None => match host.receive() {
                Ok(delivery) => delivery,
                Err(_) => return 0,
            },
        };
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
