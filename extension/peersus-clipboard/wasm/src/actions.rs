//! What this extension does when the application calls it. All of it goes through `Wire` and `Data`, so it is the
//! same logic under test and in the interpreter.
//!
//! Three calls are answered: `share` (the text on the clipboard), `send_text` (text that was shared into the
//! application) and `receive` (text a device sent). Answers are fixed words: nothing read from the clipboard, a
//! device or the network is repeated back, because they reach the screen and the log.
use crate::automatic::Reading;
use crate::config::Config;
use crate::host::{Data, Wire, read_ack, read_limited, write_all};
use crate::wire::{self, HEADER, MAX_TEXT};
use serde_json::{Value, json};

/// The application's own services this extension calls, and their version.
const PEERS: &str = "peersus.peers";
const CLIPBOARD: &str = "peersus.clipboard";
/// The service the other device runs for this extension, and its version.
const SERVICE: &str = "clipboard.text";
const VERSION: u32 = 1;
/// Control calls to the application wait at most this long, in milliseconds.
const CALL_MS: u32 = 8_000;
/// Most bytes of one item ever read from a data connection: its header, the largest text, and one more.
const MOST_READ: usize = HEADER + MAX_TEXT + 1;

pub mod answer {
    pub const NOTHING: &str = "There is no text to share";
    pub const REFUSED: &str = "That device cannot be sent to";
    pub const FAILED: &str = "The text could not be sent";
    pub const NOT_ACCEPTED: &str = "What was sent was not accepted";
    pub const NOT_PLACED: &str = "The text could not be placed on the clipboard";
    pub const UNKNOWN: &str = "Nothing here answers that";
    pub const CANCELLED: &str = "The operation was cancelled";
}

/// One call to the application, answered or cancelled. Calls from others that arrive meanwhile are kept in order.
pub struct Session<'a, H: Wire + Data> {
    pub host: &'a mut H,
    /// The hub ticket of the call being answered.
    pub ticket: u64,
    next: u64,
    /// Deliveries that arrived while waiting for an answer of ours, to be handled afterwards.
    pub waiting: Vec<Value>,
    pub cancelled: bool,
    /// The text this session put on the clipboard, if it did: the loop must know it so that it is never sent back.
    pub placed: Option<String>,
}

impl<'a, H: Wire + Data> Session<'a, H> {
    pub fn new(host: &'a mut H, ticket: u64, next: u64) -> Self {
        Self {
            host,
            ticket,
            next,
            waiting: Vec::new(),
            cancelled: false,
            placed: None,
        }
    }

    pub fn next(&self) -> u64 {
        self.next
    }

    /// Ask the application for something and wait for exactly that answer.
    fn call(
        &mut self,
        provider: &str,
        service: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, &'static str> {
        self.next = self.next.checked_add(1).ok_or(answer::FAILED)?;
        let id = self.next;
        self.host.send(json!({
            "op":"call","id":id,"provider":provider,"service":service,"version":1,
            "method":method,"args":args,"timeout_ms":CALL_MS
        }))?;
        loop {
            let message = self.host.receive()?;
            match message["type"].as_str() {
                Some("result") if message["id"].as_u64() == Some(id) => {
                    let result = &message["result"];
                    return if result["status"] == "ok" {
                        Ok(result["value"].clone())
                    } else if result["code"] == "cancelled" {
                        Err(answer::CANCELLED)
                    } else {
                        Err(answer::REFUSED)
                    };
                }
                Some("cancelled") if message["ticket"].as_u64() == Some(self.ticket) => {
                    self.cancelled = true;
                    self.host.send(json!({"op":"cancel","id":id}))?;
                    return Err(answer::CANCELLED);
                }
                // Somebody else's call, a change notice, or a result for a call we gave up on. Calls and notices are kept,
                // never dropped, and bounded; a stray result for an answer nobody awaits is of no use to anyone.
                Some("call") | Some("cancelled") | Some("event") if self.waiting.len() < 32 => {
                    self.waiting.push(message)
                }
                Some("call") | Some("cancelled") | Some("event") => return Err(answer::FAILED),
                _ => {}
            }
        }
    }

    /// The whole text behind a captured handle, taken in bounded pieces.
    fn take(&mut self, captured: &Value) -> Result<String, &'static str> {
        let clip = captured["clip"].as_str().ok_or(answer::NOTHING)?.to_owned();
        let bytes = captured["bytes"].as_u64().ok_or(answer::NOTHING)? as usize;
        if bytes == 0 || bytes > MAX_TEXT {
            return Err(answer::NOTHING);
        }
        let mut text = String::with_capacity(bytes);
        while text.len() < bytes {
            let piece = self.call(
                "host",
                CLIPBOARD,
                "chunk",
                json!({"clip":clip,"offset":text.len()}),
            )?;
            let part = piece["text"].as_str().ok_or(answer::FAILED)?;
            // A piece that makes no progress, or says a next offset it did not earn, ends the loop rather than spinning.
            if part.is_empty() || piece["next"].as_u64() != Some((text.len() + part.len()) as u64) {
                return Err(answer::FAILED);
            }
            text.push_str(part);
        }
        if text.len() != bytes {
            return Err(answer::FAILED);
        }
        Ok(text)
    }

    /// The text to send: the clipboard's, or the text shared into the application, never a fallback from one to the other.
    fn text(&mut self, context: Option<u64>) -> Result<String, &'static str> {
        let captured = match context {
            Some(context) => {
                self.call("host", CLIPBOARD, "selected", json!({"context":context}))?
            }
            None => self.call("host", CLIPBOARD, "read", json!({}))?,
        };
        if captured["available"] != true {
            return Err(answer::NOTHING);
        }
        let result = self.take(&captured);
        // The handle is ours to give back whether or not the text arrived.
        let _ = self.call("host", CLIPBOARD, "release", json!({}));
        result
    }

    /// Send `text` to `device` and report success only when the application says the bytes went out.
    fn deliver(&mut self, text: &str, device: &str) -> Result<(), &'static str> {
        let bytes = wire::encode(text).map_err(|_| answer::NOTHING)?;
        let sent = self.call(
            "host",
            PEERS,
            "send",
            json!({"device":device,"service":SERVICE,"version":VERSION}),
        )?;
        let claim = sent["claim"].as_u64().ok_or(answer::FAILED)?;
        let handle = self.host.open(true, claim).map_err(|_| answer::FAILED)?;
        let result = (|| {
            write_all(self.host, handle, &bytes).map_err(|_| answer::FAILED)?;
            self.host.shutdown(handle).map_err(|_| answer::FAILED)?;
            // Writing and half-closing is not delivery: only the application's byte count is.
            let delivered = read_ack(self.host, handle).map_err(|_| answer::FAILED)?;
            if delivered == bytes.len() as u64 {
                Ok(())
            } else {
                Err(answer::FAILED)
            }
        })();
        self.host.close(handle);
        result
    }

    /// The application's account of this extension and the devices it may send to. `None` when it cannot be read or is
    /// not the expected shape: automatic sending then stays off, and explicit sharing is unaffected.
    pub fn configuration(&mut self) -> Option<Config> {
        let value = self.call("host", "peersus.config", "get", json!({})).ok()?;
        Config::parse(value)
    }

    /// Capture what is on the clipboard now, as the platform reports it. `None` for nothing readable.
    pub fn capture(&mut self, observe: bool) -> Option<Reading> {
        let method = if observe { "observe" } else { "read" };
        let captured = self.call("host", CLIPBOARD, method, json!({})).ok()?;
        if captured["available"] != true {
            return Some(Reading::Unavailable);
        }
        let changed = captured["changed"] == true;
        let text = self.take(&captured);
        let _ = self.call("host", CLIPBOARD, "release", json!({}));
        let text = text.ok()?;
        Some(if changed {
            Reading::Changed(text)
        } else {
            Reading::Snapshot(text)
        })
    }

    /// Forget earlier copies, so that only later ones are reported.
    pub fn reset(&mut self) -> bool {
        self.call("host", CLIPBOARD, "reset", json!({}))
            .is_ok_and(|value| value["reset"] == true)
    }

    /// Send an already captured text to one device, with the same verified delivery as an explicit share.
    pub fn send_to(&mut self, text: &str, device: &str) -> Result<(), &'static str> {
        self.deliver(text, device)
    }

    /// `share` and `send_text`: one text, one device, one verified delivery.
    pub fn send(&mut self, args: &Value, from_context: bool) -> Result<(), &'static str> {
        let device = args["peer"].as_str().ok_or(answer::REFUSED)?.to_owned();
        let context = if from_context {
            Some(
                args["context"]
                    .as_u64()
                    .filter(|context| *context > 0)
                    .ok_or(answer::NOTHING)?,
            )
        } else {
            None
        };
        let text = self.text(context)?;
        self.deliver(&text, &device)
    }

    /// `receive`: collect what a device sent, check it is exactly one text, and only then place it.
    pub fn receive(&mut self) -> Result<(), &'static str> {
        let handle = self
            .host
            .open(false, self.ticket)
            .map_err(|_| answer::NOT_ACCEPTED)?;
        let bytes = read_limited(self.host, handle, MOST_READ).map_err(|_| answer::NOT_ACCEPTED);
        self.host.close(handle);
        let bytes = bytes?;
        // One item and nothing after it: the extra byte of the read bound is what lets that be seen.
        if !wire::whole(&bytes) {
            return Err(answer::NOT_ACCEPTED);
        }
        let text = wire::decode(&bytes).map_err(|_| answer::NOT_ACCEPTED)?;
        self.place(&text)
    }

    /// Replace the clipboard with `text`, in bounded pieces, committing only a complete sequence.
    fn place(&mut self, text: &str) -> Result<(), &'static str> {
        let begun = self
            .call("host", CLIPBOARD, "write", json!({"bytes":text.len()}))
            .map_err(|_| answer::NOT_PLACED)?;
        let put = begun["put"].as_str().ok_or(answer::NOT_PLACED)?.to_owned();
        let mut offset = 0;
        let mut sent = Ok(());
        while offset < text.len() {
            let mut end = (offset + 8 * 1024).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if self
                .call(
                    "host",
                    CLIPBOARD,
                    "append",
                    json!({"put":put,"offset":offset,"text":&text[offset..end]}),
                )
                .is_err()
            {
                sent = Err(answer::NOT_PLACED);
                break;
            }
            offset = end;
        }
        let done = sent.and_then(|()| {
            self.call("host", CLIPBOARD, "commit", json!({"put":put}))
                .map(drop)
                .map_err(|_| answer::NOT_PLACED)
        });
        // Whatever happened, nothing of an unfinished replacement is left with the application.
        let _ = self.call("host", CLIPBOARD, "release", json!({}));
        // Only a text that really reached the clipboard is remembered as received: it must not be sent back as a copy.
        if done.is_ok() {
            self.placed = Some(text.to_owned());
        }
        done.map(|_| ())
    }
}

/// What to reply for one finished call.
pub fn outcome(result: Result<(), &'static str>) -> Value {
    match result {
        Ok(()) => json!({"status":"ok","value":true}),
        Err(reason) if reason == answer::CANCELLED => {
            json!({"status":"error","code":"cancelled","message":reason})
        }
        Err(reason) => json!({"status":"error","code":"failed","message":reason}),
    }
}

/// Whether `delivery` is a call whose cancellation already waits in `backlog`; if so the cancellation is taken out too, and
/// the call must not be started.
pub fn withdrawn(delivery: &Value, backlog: &mut std::collections::VecDeque<Value>) -> bool {
    let Some(ticket) = delivery["ticket"]
        .as_u64()
        .filter(|_| delivery["type"] == "call")
    else {
        return false;
    };
    match backlog
        .iter()
        .position(|later| later["type"] == "cancelled" && later["ticket"].as_u64() == Some(ticket))
    {
        Some(at) => {
            backlog.remove(at);
            true
        }
        None => false,
    }
}

/// Answer one delivery from the application. Returns the call's ticket and its reply, or `None` for anything that is not a call.
pub fn handle<H: Wire + Data>(host: &mut H, delivery: &Value, next: &mut u64) -> Handled {
    if delivery["type"] != "call" {
        return Handled::Ignored;
    }
    let Some(ticket) = delivery["ticket"].as_u64() else {
        return Handled::Stop;
    };
    let mut session = Session::new(host, ticket, *next);
    let result = match delivery["method"].as_str() {
        Some("share") => session.send(&delivery["args"], false),
        Some("send_text") => session.send(&delivery["args"], true),
        Some("receive") => session.receive(),
        _ => Err(answer::UNKNOWN),
    };
    *next = session.next();
    let (cancelled, waiting, placed) = (
        session.cancelled,
        std::mem::take(&mut session.waiting),
        session.placed.take(),
    );
    if cancelled {
        // A cancelled call is answered by nobody: the application no longer waits for it.
        return Handled::Cancelled { waiting };
    }
    let reply = json!({"op":"reply","ticket":ticket,"result":outcome(result)});
    Handled::Replied {
        reply,
        waiting,
        placed,
    }
}

pub enum Handled {
    Ignored,
    Stop,
    Cancelled {
        waiting: Vec<Value>,
    },
    /// `placed` is the text this call put on the clipboard, if it did, so the caller can mark it as received.
    Replied {
        reply: Value,
        waiting: Vec<Value>,
        placed: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::scripted::{Script, ack};
    use serde_json::json;

    fn call(method: &str, args: Value) -> Value {
        json!({"type":"call","ticket":5,"caller":"host","service":"clipboard.text","version":1,"method":method,"args":args})
    }
    fn captured(text: &str) -> Vec<Value> {
        vec![
            json!({"status":"ok","value":{"available":true,"clip":"clip-1","bytes":text.len(),"changed":false}}),
            json!({"status":"ok","value":{"text":text,"next":text.len()}}),
            json!({"status":"ok","value":{"released":true}}),
        ]
    }

    #[test]
    fn share_sends_the_clipboard_text_and_succeeds_only_on_the_matching_acknowledgement() {
        let item = wire::encode("hello").unwrap();
        let mut host = Script::default();
        host.results.extend(captured("hello"));
        host.results
            .push_back(json!({"status":"ok","value":{"claim":99}}));
        host.incoming.push_back(ack(item.len() as u64));
        let mut next = 0;
        let Handled::Replied { reply, .. } = handle(
            &mut host,
            &call("share", json!({"peer":"phone"})),
            &mut next,
        ) else {
            panic!("a finished call is replied to");
        };
        assert_eq!(reply["result"]["status"], "ok");
        assert_eq!(host.written, item);
        assert_eq!(
            host.opened,
            [(true, 99)],
            "the data connection names the claim, never an identity"
        );
        assert_eq!(host.shutdowns, 1);
        assert_eq!(host.closed, 1);
        let methods: Vec<_> = host
            .sent
            .iter()
            .map(|c| c["method"].as_str().unwrap())
            .collect();
        assert_eq!(methods, ["read", "chunk", "release", "send"]);
        assert_eq!(
            host.sent[3]["args"],
            json!({"device":"phone","service":"clipboard.text","version":1})
        );
    }

    #[test]
    fn a_wrong_or_missing_acknowledgement_is_never_reported_as_delivered() {
        for incoming in [vec![ack(1)], vec![]] {
            let mut host = Script::default();
            host.results.extend(captured("hello"));
            host.results
                .push_back(json!({"status":"ok","value":{"claim":99}}));
            host.incoming.extend(incoming);
            let mut next = 0;
            let Handled::Replied { reply, .. } = handle(
                &mut host,
                &call("share", json!({"peer":"phone"})),
                &mut next,
            ) else {
                panic!("replied");
            };
            assert_eq!(reply["result"]["status"], "error");
            assert_eq!(reply["result"]["message"], answer::FAILED);
            assert_eq!(host.closed, 1, "the connection is closed whatever happened");
        }
    }

    #[test]
    fn nothing_readable_sends_nothing_and_opens_no_connection() {
        let mut host = Script::default();
        host.results
            .push_back(json!({"status":"ok","value":{"available":false}}));
        let mut next = 0;
        let Handled::Replied { reply, .. } = handle(
            &mut host,
            &call("share", json!({"peer":"phone"})),
            &mut next,
        ) else {
            panic!("replied");
        };
        assert_eq!(reply["result"]["message"], answer::NOTHING);
        assert!(host.opened.is_empty() && host.written.is_empty());
    }

    #[test]
    fn text_over_the_limit_is_refused_before_any_device_is_asked() {
        let mut host = Script::default();
        host.results.push_back(
            json!({"status":"ok","value":{"available":true,"clip":"c","bytes":MAX_TEXT + 1}}),
        );
        host.results
            .push_back(json!({"status":"ok","value":{"released":true}}));
        let mut next = 0;
        let Handled::Replied { reply, .. } = handle(
            &mut host,
            &call("share", json!({"peer":"phone"})),
            &mut next,
        ) else {
            panic!("replied");
        };
        assert_eq!(reply["result"]["message"], answer::NOTHING);
        assert!(host.sent.iter().all(|call| call["method"] != "send"));
    }

    #[test]
    fn send_text_redeems_exactly_its_context_and_never_reads_the_clipboard() {
        let item = wire::encode("shared").unwrap();
        let mut host = Script::default();
        host.results.extend(captured("shared"));
        host.results
            .push_back(json!({"status":"ok","value":{"claim":3}}));
        host.incoming.push_back(ack(item.len() as u64));
        let mut next = 0;
        let delivery = call("send_text", json!({"peer":"phone","context":42}));
        let Handled::Replied { reply, .. } = handle(&mut host, &delivery, &mut next) else {
            panic!("replied")
        };
        assert_eq!(reply["result"]["status"], "ok");
        assert_eq!(host.sent[0]["method"], "selected");
        assert_eq!(host.sent[0]["args"], json!({"context":42}));
        assert!(host.sent.iter().all(|call| call["method"] != "read"));
    }

    #[test]
    fn a_cancelled_call_is_not_answered_and_cancels_the_pending_request() {
        let mut host = Script::default();
        host.deliveries.push_back(call("receive", json!({})));
        host.deliveries
            .push_back(json!({"type":"cancelled","ticket":5}));
        let mut next = 0;
        let Handled::Cancelled { waiting } = handle(
            &mut host,
            &call("share", json!({"peer":"phone"})),
            &mut next,
        ) else {
            panic!("a cancelled call is not answered");
        };
        assert_eq!(
            waiting.len(),
            1,
            "a call received before the cancellation is not lost with it"
        );
        assert!(host.sent.iter().any(|call| call["op"] == "cancel"));
        assert!(host.sent.iter().all(|call| call["op"] != "reply"));
    }

    #[test]
    fn a_call_cancelled_while_it_waited_is_dropped_with_its_cancellation_and_others_are_kept() {
        let mut backlog: std::collections::VecDeque<Value> = [
            json!({"type":"call","ticket":9,"method":"receive"}),
            json!({"type":"cancelled","ticket":5}),
            json!({"type":"cancelled","ticket":9}),
        ]
        .into();
        assert!(withdrawn(
            &call("share", json!({"peer":"phone"})),
            &mut backlog
        ));
        assert_eq!(
            backlog.len(),
            2,
            "only the matching cancellation is taken out"
        );
        assert!(
            backlog
                .iter()
                .all(|d| !(d["type"] == "cancelled" && d["ticket"] == 5))
        );
        // A call with no waiting cancellation, and anything that is not a call, is started as usual.
        assert!(!withdrawn(
            &json!({"type":"call","ticket":7,"method":"receive"}),
            &mut backlog
        ));
        assert!(!withdrawn(
            &json!({"type":"event","ticket":9}),
            &mut backlog
        ));
        assert_eq!(backlog.len(), 2, "nothing else was taken out");
    }

    #[test]
    fn send_text_without_a_context_does_not_fall_back_to_the_clipboard() {
        for args in [json!({"peer":"phone"}), json!({"peer":"phone","context":0})] {
            let mut host = Script::default();
            let mut next = 0;
            let Handled::Replied { reply, .. } =
                handle(&mut host, &call("send_text", args), &mut next)
            else {
                panic!("replied");
            };
            assert_eq!(reply["result"]["message"], answer::NOTHING);
            assert!(host.sent.is_empty(), "no read of any kind was attempted");
        }
    }

    #[test]
    fn receive_places_one_text_in_bounded_pieces_and_commits_once() {
        let text = "é".repeat(10_000);
        let mut host = Script::default();
        host.incoming.push_back(wire::encode(&text).unwrap());
        for _ in 0..4 {
            host.results
                .push_back(json!({"status":"ok","value":{"put":"put-1","next":0}}));
        }
        host.results.extend(std::iter::repeat_n(
            json!({"status":"ok","value":{"next":0}}),
            8,
        ));
        let mut next = 0;
        let Handled::Replied { reply, .. } =
            handle(&mut host, &call("receive", json!({})), &mut next)
        else {
            panic!("replied");
        };
        assert_eq!(reply["result"]["status"], "ok");
        assert_eq!(
            host.opened,
            [(false, 5)],
            "the ticket of the call being answered"
        );
        let mut rebuilt = String::new();
        for call in host.sent.iter().filter(|call| call["method"] == "append") {
            assert_eq!(
                call["args"]["offset"].as_u64().unwrap() as usize,
                rebuilt.len()
            );
            rebuilt.push_str(call["args"]["text"].as_str().unwrap());
        }
        assert_eq!(rebuilt, text);
        assert_eq!(
            host.sent
                .iter()
                .filter(|call| call["method"] == "commit")
                .count(),
            1
        );
        assert_eq!(host.sent.first().unwrap()["method"], "write");
        assert_eq!(
            host.sent.first().unwrap()["args"],
            json!({"bytes":text.len()})
        );
    }

    #[test]
    fn receive_refuses_anything_that_is_not_exactly_one_text() {
        let mut with_trailing = wire::encode("abc").unwrap();
        with_trailing.push(0);
        let mut picture = vec![];
        picture.extend_from_slice(b"PCB1\x02\x00\x00\x00\x00");
        for bytes in [b"hello".to_vec(), with_trailing, picture, vec![]] {
            let mut host = Script::default();
            host.incoming.push_back(bytes);
            let mut next = 0;
            let Handled::Replied { reply, .. } =
                handle(&mut host, &call("receive", json!({})), &mut next)
            else {
                panic!("replied");
            };
            assert_eq!(reply["result"]["message"], answer::NOT_ACCEPTED);
            assert!(host.sent.is_empty(), "nothing reached the clipboard");
        }
    }

    #[test]
    fn a_refused_write_is_reported_and_leaves_nothing_behind() {
        let mut host = Script::default();
        host.incoming.push_back(wire::encode("abc").unwrap());
        host.results
            .push_back(json!({"status":"ok","value":{"put":"put-1"}}));
        host.results
            .push_back(json!({"status":"ok","value":{"next":3}}));
        host.results
            .push_back(json!({"status":"error","code":"platform-refused","message":"x"}));
        host.results
            .push_back(json!({"status":"ok","value":{"released":true}}));
        let mut next = 0;
        let Handled::Replied { reply, .. } =
            handle(&mut host, &call("receive", json!({})), &mut next)
        else {
            panic!("replied");
        };
        assert_eq!(reply["result"]["message"], answer::NOT_PLACED);
        assert_eq!(host.sent.last().unwrap()["method"], "release");
    }

    #[test]
    fn another_call_that_arrives_while_waiting_is_kept_and_not_lost() {
        let mut host = Script::default();
        host.deliveries.push_back(call("receive", json!({})));
        host.results
            .push_back(json!({"status":"ok","value":{"available":false}}));
        let mut next = 0;
        let Handled::Replied { waiting, .. } = handle(
            &mut host,
            &call("share", json!({"peer":"phone"})),
            &mut next,
        ) else {
            panic!("replied");
        };
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0]["method"], "receive");
    }

    #[test]
    fn an_unknown_method_gets_the_fixed_words_and_a_non_call_is_ignored() {
        let mut host = Script::default();
        let mut next = 0;
        let Handled::Replied { reply, .. } =
            handle(&mut host, &call("nothing", json!({})), &mut next)
        else {
            panic!("replied");
        };
        assert_eq!(reply["result"]["message"], answer::UNKNOWN);
        assert!(matches!(
            handle(&mut host, &json!({"type":"event"}), &mut next),
            Handled::Ignored
        ));
    }

    #[test]
    fn a_missing_device_is_refused_without_reading_anything() {
        let mut host = Script::default();
        let mut next = 0;
        let Handled::Replied { reply, .. } =
            handle(&mut host, &call("share", json!({})), &mut next)
        else {
            panic!("replied");
        };
        assert_eq!(reply["result"]["message"], answer::REFUSED);
        assert!(host.sent.is_empty());
    }

    #[test]
    fn a_change_notice_that_arrives_while_waiting_for_an_answer_is_kept_not_lost() {
        let mut host = Script::default();
        let notice = json!({"type":"event","provider":"host","service":"peersus.clipboard","version":1,"event":"changed","data":{}});
        host.deliveries.push_back(notice.clone());
        host.results
            .push_back(json!({"status":"ok","value":{"available":false}}));
        let mut next = 0;
        let Handled::Replied { waiting, .. } = handle(
            &mut host,
            &call("share", json!({"peer":"phone"})),
            &mut next,
        ) else {
            panic!("replied");
        };
        assert_eq!(
            waiting,
            [notice],
            "the notice is handed back to the loop, in order, untouched"
        );
    }

    #[test]
    fn a_received_text_is_reported_as_placed_and_a_failed_one_is_not() {
        let mut host = Script::default();
        host.incoming
            .push_back(wire::encode("from a device").unwrap());
        host.results
            .push_back(json!({"status":"ok","value":{"put":"put-1"}}));
        host.results
            .push_back(json!({"status":"ok","value":{"next":13}}));
        host.results
            .push_back(json!({"status":"ok","value":{"written":true}}));
        host.results
            .push_back(json!({"status":"ok","value":{"released":true}}));
        let mut next = 0;
        let Handled::Replied { placed, .. } =
            handle(&mut host, &call("receive", json!({})), &mut next)
        else {
            panic!("replied");
        };
        assert_eq!(placed.as_deref(), Some("from a device"));

        let mut refusing = Script::default();
        refusing
            .incoming
            .push_back(wire::encode("from a device").unwrap());
        refusing
            .results
            .push_back(json!({"status":"ok","value":{"put":"put-1"}}));
        refusing
            .results
            .push_back(json!({"status":"ok","value":{"next":13}}));
        refusing
            .results
            .push_back(json!({"status":"error","code":"platform-refused","message":"x"}));
        refusing
            .results
            .push_back(json!({"status":"ok","value":{"released":true}}));
        let mut next = 0;
        let Handled::Replied { placed, .. } =
            handle(&mut refusing, &call("receive", json!({})), &mut next)
        else {
            panic!("replied");
        };
        assert_eq!(
            placed, None,
            "a text that never reached the clipboard is not remembered as received"
        );
    }
}
