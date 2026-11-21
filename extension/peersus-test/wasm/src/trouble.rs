//! Misbehaving on purpose. Each button makes this extension do something the application must survive; none of them
//! is an answer. The page says, beside each, what the application should do, so a person can see whether it did.
use crate::host::Wire;
use serde_json::{Value, json};

/// A way to misbehave that a press asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trouble {
    /// Thousands of operations nobody asked for, and then the ordinary answer.
    Spam,
    /// Thousands of events nobody listens to: only this extension's rate stands between it and the hub.
    Events,
    /// Messages that are no operation, an operation that does not exist, an answer to a call that was never delivered,
    /// and then an answer that is no page.
    Nonsense,
    /// No answer to this call, ever. Later calls are answered.
    Late,
    /// Work without end inside one turn.
    Spin,
    /// Memory taken until there is no more.
    Fill,
    /// The entry point returns an error.
    Leave,
}

/// Every misbehaviour: its button, its label and what the application should do.
pub const TROUBLES: &[(Trouble, &str, &str, &str)] = &[
    (
        Trouble::Spam,
        "spam",
        "Flood",
        "Refuses what is past this extension's rate, may suspend it for a while and say so; the window stays responsive.",
    ),
    (
        Trouble::Events,
        "events",
        "Flood events",
        "Drops the events past its rate; nobody else misses an event and nothing else slows down.",
    ),
    (
        Trouble::Nonsense,
        "nonsense",
        "Nonsense",
        "Refuses what it does not understand and keeps the page as it was; it may stop and restart this extension.",
    ),
    (
        Trouble::Late,
        "late",
        "Never answer",
        "Gives up on that press after its time and says the extension did not answer; the next press works.",
    ),
    (
        Trouble::Spin,
        "spin",
        "Spin",
        "Stops this extension when its work allowance runs out and says so; the rest goes on.",
    ),
    (
        Trouble::Fill,
        "fill",
        "Fill memory",
        "Stops this extension at its memory limit and says so; the application's memory is not touched.",
    ),
    (
        Trouble::Leave,
        "leave",
        "Leave",
        "Says the extension stopped, and starts it again or leaves it stopped; nothing else is affected.",
    ),
];

/// How many pairs of operations one press of "Flood" sends.
pub const SPAM: u32 = 2000;
/// How many events one press of "Flood events" publishes.
pub const EVENTS: u32 = 5000;

/// The trouble this delivery asks for, if it is a press of one of those buttons.
pub fn asked(delivery: &Value) -> Option<Trouble> {
    let event = &delivery["args"]["event"];
    if delivery["type"] != "call" || delivery["method"] != "event" || event["t"] != "press" {
        return None;
    }
    let id = event["id"].as_str()?;
    TROUBLES
        .iter()
        .find(|(_, button, _, _)| *button == id)
        .map(|(trouble, ..)| *trouble)
}

/// What happens to the delivery after the trouble was made.
#[derive(Debug, PartialEq, Eq)]
pub enum After {
    /// Answer it as usual.
    Answer,
    /// Leave it unanswered.
    Skip,
    /// End the extension with an error.
    Leave,
}

/// Make the trouble. What the host refuses meanwhile is the point, so no refusal is looked at; only a host that went
/// away ends it early, since nothing more can be sent.
pub fn make<H: Wire>(host: &mut H, trouble: Trouble, ticket: u64) -> After {
    match trouble {
        Trouble::Spam => {
            for n in 0..SPAM {
                let call = json!({"op":"call","id":n,"provider":"host","service":"test.nobody","version":1,
                    "method":"nothing","args":{},"timeout_ms":1000});
                let event = json!({"op":"publish","service":"test.page","version":1,"event":"nothing","data":{}});
                if host.send(call).is_err() || host.send(event).is_err() {
                    break;
                }
            }
            After::Answer
        }
        Trouble::Events => {
            for _ in 0..EVENTS {
                let event = json!({"op":"publish","service":"test.page","version":1,"event":"nothing","data":{}});
                if host.send(event).is_err() {
                    break;
                }
            }
            After::Answer
        }
        Trouble::Nonsense => {
            let _ = host
                .send(json!("not an operation at all"))
                .and_then(|()| host.send(json!({"op":"dance","with":"nobody"})))
                .and_then(|()| {
                    host.send(json!({"op":"reply","ticket":ticket.wrapping_add(1_000_000),
                        "result":{"status":"ok","value":null}}))
                })
                .and_then(|()| {
                    host.send(json!({"op":"reply","ticket":ticket,
                        "result":{"status":"ok","value":{"k":"no-such-element","children":7}}}))
                });
            After::Skip
        }
        Trouble::Late => After::Skip,
        Trouble::Spin => {
            let mut turns = 0u64;
            loop {
                turns = core::hint::black_box(turns.wrapping_add(1));
            }
        }
        Trouble::Fill => {
            let mut held = Vec::new();
            loop {
                held.push(vec![1u8; 1 << 20]);
                core::hint::black_box(&held);
            }
        }
        Trouble::Leave => After::Leave,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::scripted::Script;

    fn press(id: &str) -> Value {
        json!({"type":"call","ticket":1,"method":"event","args":{"event":{"t":"press","id":id}}})
    }

    #[test]
    fn only_a_press_of_its_own_button_asks_for_trouble() {
        for (trouble, id, ..) in TROUBLES {
            assert_eq!(asked(&press(id)), Some(*trouble));
        }
        assert_eq!(asked(&press("count")), None);
        assert_eq!(
            asked(&json!({"type":"event","args":{"event":{"t":"press","id":"spin"}}})),
            None
        );
        assert_eq!(
            asked(
                &json!({"type":"call","method":"event","args":{"event":{"t":"submit","id":"spin"}}})
            ),
            None
        );
    }

    #[test]
    fn the_floods_send_their_counts_and_nonsense_answers_nothing_valid() {
        let mut host = Script::default();
        assert_eq!(make(&mut host, Trouble::Spam, 1), After::Answer);
        assert_eq!(host.sent.len(), 2 * SPAM as usize);
        let mut host = Script::default();
        assert_eq!(make(&mut host, Trouble::Events, 1), After::Answer);
        assert_eq!(host.sent.len(), EVENTS as usize);
        let mut host = Script::default();
        assert_eq!(make(&mut host, Trouble::Nonsense, 5), After::Skip);
        assert_eq!(host.sent.len(), 4);
        assert_eq!(host.sent[3]["result"]["value"]["k"], "no-such-element");
        assert_eq!(make(&mut host, Trouble::Late, 5), After::Skip);
        assert_eq!(make(&mut host, Trouble::Leave, 5), After::Leave);
    }
}
