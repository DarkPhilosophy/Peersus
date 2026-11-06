//! What this guest does when the application says the clipboard changed.
//!
//! This is only the wiring: every decision (who may be sent to, what counts as a new copy, when to start over) is taken
//! by `Automatic`, which is tested without a host. The loop here asks it, does what it says, and goes back to waiting.
//!
//! There is no clock, so nothing is polled. The application's change notice wakes this, and a notice that arrives while
//! an earlier one is being handled is kept by the session and handled next, never dropped.
use crate::actions::Session;
use crate::automatic::{Automatic, Eligibility, Epoch};
use crate::host::{Data, Wire};
use serde_json::Value;

/// Ticket zero: the hub raises its counter BEFORE it uses it (`self.ticket = self.ticket.checked_add(1)`, then
/// `let ticket = self.ticket`, in `peersus-plugin-api/src/hub.rs`), so the first ticket it ever issues is one and zero is
/// never a live call. This is not an answer to a call, so a `cancelled` notice for some call must never be taken as
/// cancelling it. The guest cannot read the hub's code, so what it relies on is stated here for whoever changes the hub.
const NOT_A_CALL: u64 = 0;

/// What a pass over one notice came to, for the caller's loop.
#[derive(Debug, PartialEq, Eq)]
pub enum Pass {
    /// Nothing to send, or sent. Carry on waiting.
    Done,
    /// The application went away or the protocol broke. Stop.
    Stop,
}

/// Look at the configuration and, when it changed what is allowed, start over. Returns whether the clipboard is to be read
/// now. That follows consent and not reachability, so observation survives every consenting device being offline; whether
/// anything is SENT is decided later, from the devices reachable at that moment.
fn arm<H: Wire + Data>(session: &mut Session<'_, H>, state: &mut Automatic) -> Option<bool> {
    let config = session.configuration()?;
    let now = Eligibility::of(&config);
    match state.epoch(&now) {
        // A new baseline: ask the platform to forget earlier copies, and only if that worked is observation allowed. The
        // reset also ends the platform's observing, and a change notice can only come while somebody observes, so the same
        // pass reads next: that read re-arms the platform and, being the first after the reset, is the baseline and never
        // a copy. Stopping here would leave the guest waiting for a notice that nothing could cause any more.
        Epoch::Rebaseline => {
            let done = session.reset();
            state.baseline_done(done);
            Some(state.can_observe())
        }
        // Only reachability changed: the baseline is still good and the platform is still observing. What was seen is
        // forgotten, so a snapshot read now is a baseline, while a copy the platform proves new is a real one, to be sent
        // if somebody is reachable now. Returning here instead would drop the very copy that woke the guest.
        Epoch::Forget => Some(state.can_observe()),
        Epoch::Same => Some(state.can_observe()),
    }
}

/// One change notice. Returns `Stop` only when the application is gone.
pub fn on_change<H: Wire + Data>(
    host: &mut H,
    state: &mut Automatic,
    next: &mut u64,
    backlog: &mut std::collections::VecDeque<Value>,
) -> Pass {
    let mut session = Session::new(host, NOT_A_CALL, *next);
    let outcome = send_new_copy(&mut session, state);
    *next = session.next();
    backlog.extend(session.waiting);
    outcome
}

fn send_new_copy<H: Wire + Data>(session: &mut Session<'_, H>, state: &mut Automatic) -> Pass {
    match arm(session, state) {
        None => return Pass::Done,
        Some(false) => return Pass::Done,
        Some(true) => {}
    }
    let Some(reading) = session.capture(true) else {
        // Nothing could be read: that breaks continuity, so the next reading is a baseline and not a copy.
        state.copy(crate::automatic::Reading::Unavailable);
        return Pass::Done;
    };
    let Some(text) = state.copy(reading) else {
        return Pass::Done;
    };
    for device in state.destinations().to_vec() {
        // Look again before EVERY destination: a slow first one must not keep a later, revoked one alive.
        let Some(latest) = session.configuration() else {
            return Pass::Done;
        };
        if !state.still_valid(&Eligibility::of(&latest)) {
            return Pass::Done;
        }
        if session.send_to(&text, &device).is_err() && session.cancelled {
            return Pass::Stop;
        }
    }
    Pass::Done
}

/// What the guest does once, before it waits for anything.
///
/// The platform reports a change only while somebody observes, and taking a baseline ends that observing, so a guest that
/// merely waited for its first notice would wait for ever. One pass takes the baseline and, in the same pass, reads, which
/// is what makes the platform start telling. A second pass exists for a baseline whose reset failed: that leaves the state
/// needing a baseline, and without a retry here a transient failure would keep the guest deaf until the next notice, which
/// nothing could cause. What was on the clipboard before is the baseline, so it is never sent as if it had just been
/// copied. With nobody to send to nothing is read: the guest then arms on the first pass that finds somebody, because the
/// application publishes no event when a device becomes eligible.
pub fn start<H: Wire + Data>(
    host: &mut H,
    state: &mut Automatic,
    next: &mut u64,
    backlog: &mut std::collections::VecDeque<Value>,
) -> Pass {
    for _ in 0..2 {
        if on_change(host, state, next, backlog) == Pass::Stop {
            return Pass::Stop;
        }
    }
    Pass::Done
}

/// Whether a delivery is something to wake for, so the caller's loop need not know what the notice looks like.
pub fn wakes(delivery: &Value) -> bool {
    crate::events::is_change(delivery)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::scripted::{Script, ack};
    use crate::wire;
    use serde_json::json;

    #[test]
    fn automatic_pass_preserves_concurrent_receive_calls_and_change_notices_in_order() {
        let mut host = Script::default();
        let receive = json!({"type":"call","ticket":77,"method":"receive","args":{}});
        let changed = json!({"type":"event","service":"peersus.clipboard","event":"changed"});
        host.deliveries.extend([receive.clone(), changed.clone()]);
        host.results.extend([config(false, &[]), reset_ok()]);
        let mut backlog =
            std::collections::VecDeque::from([json!({"type":"cancelled","ticket":12})]);
        let mut state = Automatic::new();
        let mut next = 0;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut backlog),
            Pass::Done
        );
        assert_eq!(
            backlog.into_iter().collect::<Vec<_>>(),
            [json!({"type":"cancelled","ticket":12}), receive, changed,]
        );
    }

    #[test]
    fn startup_preserves_remote_calls_received_while_waiting_for_configuration() {
        let mut host = Script::default();
        let receive = json!({"type":"call","ticket":78,"method":"receive","args":{}});
        host.deliveries.push_back(receive.clone());
        host.results
            .extend([config(false, &[]), reset_ok(), config(false, &[])]);
        let mut backlog = std::collections::VecDeque::new();
        assert_eq!(
            start(&mut host, &mut Automatic::new(), &mut 0, &mut backlog),
            Pass::Done
        );
        assert_eq!(backlog.into_iter().collect::<Vec<_>>(), [receive]);
    }

    fn config(automatic: bool, devices: &[(&str, bool, bool)]) -> Value {
        let permissions = json!([{"owner":"host","name":"peers.send"},{"owner":"clipboard","name":"clipboard.use"}]);
        let peers: Vec<_> = devices
            .iter()
            .map(|(name, online, sync)| {
                json!({"device":name,"online":online,"settings":{"sync":sync},"permissions":permissions})
            })
            .collect();
        json!({"status":"ok","value":{"plugin":"clipboard","settings":{"automatic":automatic},"peers":peers}})
    }
    fn reset_ok() -> Value {
        json!({"status":"ok","value":{"reset":true}})
    }
    fn captured(text: &str, changed: bool) -> Vec<Value> {
        vec![
            json!({"status":"ok","value":{"available":true,"clip":"clip-1","bytes":text.len(),"changed":changed}}),
            json!({"status":"ok","value":{"text":text,"next":text.len()}}),
            json!({"status":"ok","value":{"released":true}}),
        ]
    }
    fn claim() -> Value {
        json!({"status":"ok","value":{"claim":9}})
    }
    fn methods(host: &Script) -> Vec<String> {
        host.sent
            .iter()
            .map(|call| {
                format!(
                    "{}.{}",
                    call["service"].as_str().unwrap_or(""),
                    call["method"].as_str().unwrap_or("")
                )
            })
            .collect()
    }

    /// A state that has taken its baseline against one opted-in device, as after the first notice.
    fn armed(devices: &[(&str, bool, bool)]) -> (Automatic, Script) {
        let mut state = Automatic::new();
        let mut host = Script::default();
        host.results.extend([config(true, devices), reset_ok()]);
        let mut next = 0;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert!(!state.baseline_needed, "the baseline succeeded");
        host.sent.clear();
        (state, host)
    }

    #[test]
    fn the_first_notice_takes_a_baseline_then_reads_to_observe_and_sends_nothing() {
        let mut state = Automatic::new();
        let mut host = Script::default();
        host.results
            .extend([config(true, &[("phone", true, true)]), reset_ok()]);
        let mut next = 0;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert_eq!(
            methods(&host),
            [
                "peersus.config.get",
                "peersus.clipboard.reset",
                "peersus.clipboard.observe"
            ],
            "the baseline is followed by a read, which is what turns the platform's observing back on"
        );
        assert!(
            host.opened.is_empty() && host.written.is_empty(),
            "nothing was sent"
        );
    }

    /// The platform tells of a change only while somebody observes, and a reset ends that observing. So a guest that waited
    /// for its first notice would wait for ever: startup must itself leave observation on. Nothing is sent by doing so,
    /// and what is already on the clipboard is the baseline, never a copy.
    #[test]
    fn startup_leaves_observation_on_after_the_baseline_and_sends_nothing() {
        let mut state = Automatic::new();
        let mut host = Script::default();
        let devices = [("phone", true, true)];
        host.results.extend([config(true, &devices), reset_ok()]);
        host.results.extend(captured("already there", false));
        let mut next = 0;
        assert_eq!(
            start(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        let called = methods(&host);
        let reset = called
            .iter()
            .position(|m| m == "peersus.clipboard.reset")
            .expect("a baseline was taken");
        let observe = called
            .iter()
            .position(|m| m == "peersus.clipboard.observe")
            .expect("observation was left on");
        assert!(
            reset < observe,
            "observing comes after the reset, which would otherwise end it: {called:?}"
        );
        assert!(
            host.opened.is_empty() && host.written.is_empty(),
            "what was already there is not sent"
        );
    }

    /// Without anybody to send to there is nothing to read, and startup must not fail or send anything.
    #[test]
    fn startup_with_nobody_eligible_observes_nothing() {
        let mut state = Automatic::new();
        let mut host = Script::default();
        host.results
            .extend([config(true, &[("phone", true, false)]), reset_ok()]);
        let mut next = 0;
        assert_eq!(
            start(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert!(
            !methods(&host).contains(&"peersus.clipboard.observe".to_string()),
            "nobody to send to: never read"
        );
        assert!(host.opened.is_empty() && host.written.is_empty());
    }

    /// The same ending of the platform's observing happens at run time whenever consent changes, which is a normal thing to
    /// do: the pass that takes the new baseline must leave observation on too, or the next copy would be the last one heard.
    #[test]
    fn a_change_of_consent_at_run_time_takes_a_new_baseline_and_observes_again_without_sending() {
        let (mut state, mut host) = armed(&[("phone", true, true)]);
        // A second device is now opted in: the consenting set differs, so this is a new baseline, not a copy.
        let both = [("phone", true, true), ("tablet", true, true)];
        host.results.extend([config(true, &both), reset_ok()]);
        host.results.extend(captured("what was there", false));
        let mut next = 100;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        let called = methods(&host);
        let reset = called
            .iter()
            .position(|m| m == "peersus.clipboard.reset")
            .expect("a new baseline was taken");
        let observe = called
            .iter()
            .position(|m| m == "peersus.clipboard.observe")
            .expect("observation was turned on again");
        assert!(reset < observe, "{called:?}");
        assert!(
            host.opened.is_empty() && host.written.is_empty(),
            "the new baseline is never sent"
        );
    }

    /// The application publishes no event when a device comes online, so a guest that stopped observing while every
    /// consenting device was offline could never be woken again. Observing follows consent; sending follows reachability.
    #[test]
    fn a_consented_device_that_is_offline_keeps_the_platform_observing_and_nothing_is_sent() {
        let mut state = Automatic::new();
        let mut host = Script::default();
        host.results
            .extend([config(true, &[("phone", false, true)]), reset_ok()]);
        host.results.extend(captured("already there", false));
        let mut next = 0;
        assert_eq!(
            start(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert!(
            methods(&host).contains(&"peersus.clipboard.observe".to_string()),
            "still observing"
        );
        assert!(
            host.opened.is_empty() && host.written.is_empty(),
            "nobody is reachable, so nothing is sent"
        );
    }

    /// The first copy after the only device comes back is the one that wakes the guest. It must not be dropped: the
    /// platform proved it new, and somebody is reachable now.
    #[test]
    fn a_copy_proven_new_right_after_the_device_comes_back_is_sent_and_not_swallowed() {
        let (mut state, mut host) = armed(&[("phone", false, true)]);
        let item = wire::encode("first after return").unwrap();
        // Reachability changed while the consenting set did not: a forget, read in the same pass.
        host.results
            .push_back(config(true, &[("phone", true, true)]));
        host.results.extend(captured("first after return", true));
        host.results
            .push_back(config(true, &[("phone", true, true)]));
        host.results.push_back(claim());
        host.incoming.push_back(ack(item.len() as u64));
        let mut next = 100;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert_eq!(host.written, item, "the copy that woke the guest was sent");
    }

    /// What was on the clipboard before the device came back is not a copy: a snapshot after a forget is a baseline.
    #[test]
    fn what_was_on_the_clipboard_before_the_device_came_back_is_not_sent() {
        let (mut state, mut host) = armed(&[("phone", false, true)]);
        host.results
            .push_back(config(true, &[("phone", true, true)]));
        host.results.extend(captured("was already there", false));
        let mut next = 100;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert!(
            host.opened.is_empty() && host.written.is_empty(),
            "an old clipboard is never sent as if it were new"
        );
    }

    #[test]
    fn a_new_copy_after_the_baseline_is_sent_to_each_eligible_device_with_a_verified_delivery() {
        let (mut state, mut host) = armed(&[("phone", true, true)]);
        let item = wire::encode("hello").unwrap();
        host.results
            .push_back(config(true, &[("phone", true, true)]));
        host.results.extend(captured("hello", true));
        host.results
            .push_back(config(true, &[("phone", true, true)]));
        host.results.push_back(claim());
        host.incoming.push_back(ack(item.len() as u64));
        let mut next = 100;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert_eq!(host.written, item);
        assert_eq!(host.opened, [(true, 9)]);
        assert!(methods(&host).contains(&"peersus.peers.send".to_string()));
        assert_eq!(host.shutdowns, 1);
    }

    #[test]
    fn a_snapshot_that_is_not_a_proven_new_copy_is_not_sent() {
        let (mut state, mut host) = armed(&[("phone", true, true)]);
        host.results
            .push_back(config(true, &[("phone", true, true)]));
        host.results
            .extend(captured("whatever was already there", false));
        let mut next = 100;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert!(
            host.opened.is_empty() && host.written.is_empty(),
            "an old clipboard is never sent as if it were new"
        );
    }

    #[test]
    fn an_unreadable_clipboard_sends_nothing_and_breaks_continuity() {
        let (mut state, mut host) = armed(&[("phone", true, true)]);
        host.results
            .push_back(config(true, &[("phone", true, true)]));
        host.results
            .push_back(json!({"status":"ok","value":{"available":false}}));
        let mut next = 100;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert!(host.opened.is_empty());
        // What follows is a baseline: the chain was broken, so a snapshot is not a copy.
        assert_eq!(
            state.copy(crate::automatic::Reading::Snapshot("after the gap".into())),
            None
        );
    }

    #[test]
    fn consent_withdrawn_before_a_destination_stops_that_destination_and_the_rest() {
        let (mut state, mut host) = armed(&[("a", true, true), ("b", true, true)]);
        host.results
            .push_back(config(true, &[("a", true, true), ("b", true, true)]));
        host.results.extend(captured("copied", true));
        // Before the first destination, "b" has been revoked: everything that was decided is no longer valid.
        host.results
            .push_back(config(true, &[("a", true, true), ("b", true, false)]));
        let mut next = 100;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert!(
            host.opened.is_empty() && host.written.is_empty(),
            "nothing is sent once consent changed"
        );
    }

    #[test]
    fn nobody_eligible_means_the_clipboard_is_never_read() {
        let (mut state, mut host) = armed(&[("phone", true, false)]);
        host.results
            .push_back(config(true, &[("phone", true, false)]));
        let mut next = 100;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert_eq!(
            methods(&host),
            ["peersus.config.get"],
            "with nobody to send to, nothing reads the clipboard"
        );
    }

    #[test]
    fn a_text_this_device_placed_is_never_sent_back_when_its_own_write_raises_the_change_notice() {
        let (mut state, mut host) = armed(&[("phone", true, true)]);
        // The device received this text from another and put it on the clipboard: the loop reports it as received.
        state.received("from the other device");
        // Its own write now raises the change notice, and the platform proves the text as a copy made just now.
        host.results
            .push_back(config(true, &[("phone", true, true)]));
        host.results.extend(captured("from the other device", true));
        let mut next = 100;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert!(
            host.opened.is_empty() && host.written.is_empty(),
            "the text came from a device and goes nowhere"
        );
        assert!(
            !methods(&host).contains(&"peersus.peers.send".to_string()),
            "no destination was even asked for"
        );
    }

    #[test]
    fn a_failed_baseline_keeps_observation_blocked() {
        let mut state = Automatic::new();
        let mut host = Script::default();
        host.results.extend([
            config(true, &[("phone", true, true)]),
            json!({"status":"ok","value":{"reset":false}}),
        ]);
        let mut next = 0;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert!(
            !state.can_send(),
            "an unconfirmed baseline blocks observation rather than risking an old copy"
        );
    }

    #[test]
    fn an_unreadable_configuration_leaves_automatic_sending_off() {
        let mut state = Automatic::new();
        let mut host = Script::default();
        host.results
            .push_back(json!({"status":"error","code":"not-found","message":"x"}));
        let mut next = 0;
        assert_eq!(
            on_change(&mut host, &mut state, &mut next, &mut Default::default()),
            Pass::Done
        );
        assert_eq!(methods(&host), ["peersus.config.get"]);
        assert!(!state.can_send());
    }

    #[test]
    fn only_the_applications_own_notice_wakes_the_loop() {
        let notice = json!({"type":"event","provider":"host","service":"peersus.clipboard","version":1,"event":"changed","data":{}});
        assert!(wakes(&notice));
        let mut forged = notice.clone();
        forged["provider"] = json!("other");
        assert!(!wakes(&forged));
        assert!(!wakes(&json!({"type":"call","ticket":1})));
    }
}
