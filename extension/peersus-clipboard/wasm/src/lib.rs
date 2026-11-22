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
    run_loop(&mut host::Host)
}

fn run_loop<H: host::Wire + host::Data>(host: &mut H) -> i32 {
    let mut next = 0u64;
    let mut state = automatic::Automatic::new();
    let mut backlog: std::collections::VecDeque<Value> = std::collections::VecDeque::new();
    // Ask once to be told when the clipboard changes. A refusal is not fatal: without the permission this guest still
    // answers every explicit call, and automatic sending simply never wakes.
    if host.send(host::subscribe_config()).is_err() || host.send(events::subscribe()).is_err() {
        return 1;
    }
    // The platform reports a change only while somebody observes, and the only observing this guest does is in answer to
    // a change. Waiting for the first notice would therefore wait for ever: startup takes the baseline and then reads
    // once, which is what makes the platform start telling. It sends only what the settings already allow.
    if watch::start(host, &mut state, &mut next, &mut backlog) == watch::Pass::Stop {
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
                            if watch::on_change(host, &mut state, &mut next, &mut backlog)
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
        if host::config_changed(&delivery) {
            if watch::on_config_changed(host, &mut state, &mut next, &mut backlog)
                == watch::Pass::Stop
            {
                return 1;
            }
            continue;
        }
        if watch::wakes(&delivery) {
            if watch::on_change(host, &mut state, &mut next, &mut backlog) == watch::Pass::Stop {
                return 1;
            }
            continue;
        }
        let (waiting, placed, reply) =
            match actions::handle(host, &delivery, &mut next, &mut state.readiness) {
                actions::Handled::Ignored => continue,
                actions::Handled::Stop => return 1,
                actions::Handled::Cancelled { waiting, placed } => (waiting, placed, None),
                actions::Handled::Replied {
                    reply,
                    waiting,
                    placed,
                } => (waiting, placed, Some(reply)),
            };
        // Mark a completed write even if cleanup was cancelled, before running its deferred notice.
        if let Some(text) = placed {
            state.received(text);
        }
        backlog.extend(waiting);
        if let Some(reply) = reply
            && host.send(reply).is_err()
        {
            return 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::scripted::Script;
    use serde_json::json;

    #[test]
    fn a_receive_cancelled_during_release_does_not_echo_its_committed_write() {
        let text = "received sample";
        let config = json!({"plugin":"clipboard","settings":{"automatic":true},"peers":[{
            "device":"phone","online":true,"settings":{"sync":true},
            "permissions":[{"owner":"host","name":"peers.send"},{"owner":"clipboard","name":"clipboard.use"}]
        }]});
        let result =
            |id, value| json!({"type":"result","id":id,"result":{"status":"ok","value":value}});
        let mut host = Script {
            acks: true,
            ..Script::default()
        };
        host.incoming.push_back(wire::encode(text).unwrap());
        host.deliveries.extend([
            result(1, config.clone()),
            result(2, json!({"reset":true})),
            result(3, json!({"available":false})),
            result(4, config.clone()),
            result(5, json!({"available":false})),
            json!({"type":"call","ticket":41,"method":"receive"}),
            result(6, json!({"put":"put-1"})),
            result(7, json!({"next":text.len()})),
            result(8, json!({"written":true})),
            json!({"type":"event","provider":"host","service":"peersus.clipboard","version":1,"event":"changed","data":{}}),
            json!({"type":"cancelled","ticket":41}),
            result(10, config.clone()),
            result(11, json!({"available":true,"clip":"clip-2","bytes":text.len(),"changed":true})),
            result(12, json!({"text":text,"next":text.len()})),
            result(13, json!({"released":true})),
            result(14, config),
            result(15, json!({"claim":9})),
        ]);

        assert_eq!(run_loop(&mut host), 0);
        let cancelled = host
            .sent
            .iter()
            .position(|message| message["op"] == "cancel" && message["id"] == 9)
            .expect("the completed write was cancelled during release");
        assert!(
            host.sent[cancelled + 1..]
                .iter()
                .any(|message| message["method"] == "observe")
        );
        assert!(
            host.sent
                .iter()
                .all(|message| message["op"] != "reply" && message["method"] != "send")
        );
        assert_eq!(host.opened, [(false, 41)]);
    }
}
