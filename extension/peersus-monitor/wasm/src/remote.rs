//! Monitor between two devices: asking a paired device for its numbers, and answering a paired device that asks.
//!
//! Both are the one exported `receive`, and both are two streams (see [`crate::wire`]): the question goes to the paired
//! device, whose Monitor reads each part from its own application, which answers only what its person allowed Monitor
//! to read there (`denied` otherwise), and sends the answer back to the device that asked, by the name this extension
//! was given for it. The application admits a stream to `receive` only from a device its person allowed Monitor for (the
//! capability `monitor.share`), and says whose it is under `peer`: a device is never named by anything it wrote.
//!
//! Questions are asked while the page is open and one device of the bar is chosen, once each time the page's clock has
//! gone a second further, and never while one is still out; a question not answered in time is lost, and a device that
//! lets three go by is not asked again until it is chosen again or the page is opened again. Every wait in this file
//! is bounded and gives way to what the application delivers meanwhile, which is kept for later.
use crate::{
    State,
    gauges::Gauges,
    host::{Data, Handle, Wire, read_ack, write_all},
    wire::{self, Answer, Message, Outcome},
};
use serde_json::{Value, json};

/// The application's own services this extension calls to answer a question and to send one.
const PEERS: &str = "peersus.peers";
const SYSTEM: &str = "peersus.system";
/// How long the application is given to answer a call that is waited for here, in milliseconds.
const CALL_MS: u32 = 2_000;
/// Longest wait for a stream to be readable, in milliseconds. A stream whose other end is silent costs this much, once.
const WAIT_MS: i32 = 400;
/// Most reads of one stream before its end. What the application carries to this extension arrives in a piece or two,
/// so a stream that comes in more is not one a device made of a question or an answer, and cannot hold this guest for
/// as long as it chooses to drip: this many waits are less than the five seconds the application gives a page to answer.
const MOST_READS: usize = 12;
/// Most deliveries kept for later while something is waited for. The application's own queue for this extension holds
/// no more at a time.
const MOST_KEPT: usize = 64;
/// Longest name of a device the application gives, in characters. It gives short hexadecimal ones.
const ALIAS_MAX: usize = 128;
/// A question goes no sooner than this after the last one (page clock, milliseconds): about once a second.
pub const MIN_GAP: u64 = 900;
/// A question not answered after this long (page clock, milliseconds) is lost.
pub const ANSWER_MS: u64 = 3_000;
/// Questions in a row that are lost before the device is let be.
pub const MOST_SILENT: u8 = 3;

/// Why something that was started did not finish. Fixed words: nothing a device wrote is in them.
pub mod fail {
    pub const REFUSED: &str = "That device cannot be reached";
    pub const STOPPED: &str = "The other device stopped listening";
    pub const NOT_UNDERSTOOD: &str = "What was sent was not understood";
    pub const TIMED_OUT: &str = "The other side did not answer in time";
    pub const BUSY: &str = "Too much arrived to be waited for";
    pub const CANCELLED: &str = "The operation was cancelled";
    pub const DENIED: &str = "Not allowed";
}

/// The question that is out: its number, and the moment (the page's clock) it was asked, which is when its answer's
/// counters are taken to have been read.
#[derive(Debug)]
pub struct Asked {
    pub seq: u64,
    pub at: u64,
}

/// What is said with the chosen paired device, and its numbers.
#[derive(Default)]
pub struct Conversation {
    pub gauges: Gauges,
    pub asked: Option<Asked>,
    /// When (the page's clock) the last question was asked.
    pub last_at: Option<u64>,
    /// Questions in a row that were lost.
    pub silent: u8,
    /// Whether the last question was answered: what the gauges hold is from an answer.
    pub answered: bool,
    /// The application refused to send to the device: this extension may not.
    pub refused: bool,
}

/// How it stands with the chosen device, as the page says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Nothing answered yet.
    Asking,
    /// Answered, and asked again.
    Live,
    /// Not answering: it is not asked any more.
    Silent,
    /// This device's person did not allow it.
    Refused,
}

impl Conversation {
    /// A question that was lost, or could not be sent: what was known of the device is not kept as if it still held.
    fn failed(&mut self) {
        self.silent = self.silent.saturating_add(1);
        self.asked = None;
        self.answered = false;
        self.gauges = Gauges::default();
    }

    pub fn link(&self) -> Link {
        if self.refused {
            Link::Refused
        } else if self.silent >= MOST_SILENT {
            Link::Silent
        } else if self.answered {
            Link::Live
        } else {
            Link::Asking
        }
    }
}

/// The code a failure is reported with: the application's own vocabulary, which is all it reads.
fn code(reason: &str) -> &'static str {
    match reason {
        fail::CANCELLED => "cancelled",
        fail::BUSY => "busy",
        fail::REFUSED | fail::DENIED | fail::STOPPED | fail::TIMED_OUT => "unavailable",
        _ => "failed",
    }
}

/// What to reply for one finished `receive`.
fn outcome(result: Result<(), &'static str>) -> Value {
    match result {
        Ok(()) => json!({"status":"ok","value":true}),
        Err(reason) => json!({"status":"error","code":code(reason),"message":reason}),
    }
}

impl State {
    /// The next delivery to handle: what was kept for later comes first, in the order it came.
    pub fn next<H: Wire>(&mut self, host: &mut H) -> Result<Value, &'static str> {
        match self.waiting.pop_front() {
            Some(delivery) => Ok(delivery),
            None => host.receive(),
        }
    }

    /// Put aside what arrived while something else was waited for. One delivery too many is refused.
    fn keep(&mut self, delivery: Value) -> Result<(), &'static str> {
        if self.waiting.len() >= MOST_KEPT {
            return Err(fail::BUSY);
        }
        self.waiting.push_back(delivery);
        Ok(())
    }

    /// Whether call `ticket` was cancelled, while it waited or while something else was waited for. A call withdrawn
    /// is not started, and one withdrawn meanwhile gets no late reply and causes no late send.
    fn withdrawn(&self, ticket: u64) -> bool {
        self.waiting.iter().any(|delivery| {
            delivery["type"] == "cancelled" && delivery["ticket"].as_u64() == Some(ticket)
        })
    }

    /// A cancellation that was acted on is not kept for later.
    fn settled(&mut self, ticket: u64) {
        self.waiting.retain(|delivery| {
            !(delivery["type"] == "cancelled" && delivery["ticket"].as_u64() == Some(ticket))
        });
    }

    /// Ask the application for something and wait for exactly that answer, keeping what else arrives meanwhile.
    fn call_now<H: Wire>(
        &mut self,
        host: &mut H,
        service: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, &'static str> {
        let (id, mut call) = self.call(service, method, args);
        call["timeout_ms"] = json!(CALL_MS);
        host.send(call)?;
        loop {
            let delivery = host.receive()?;
            if delivery["type"] == "result" && delivery["id"].as_u64() == Some(id) {
                let result = &delivery["result"];
                return match (result["status"].as_str(), result["code"].as_str()) {
                    (Some("ok"), _) => Ok(result["value"].clone()),
                    (_, Some("denied")) => Err(fail::DENIED),
                    (_, Some("cancelled")) => Err(fail::CANCELLED),
                    _ => Err(fail::REFUSED),
                };
            }
            self.keep(delivery)?;
        }
    }

    /// Wait until `handle` can be read (bytes, or the end of the stream), for [`WAIT_MS`] at most since anything last
    /// arrived. What the application delivers first is kept for later, and only so much of it is: a wait does not last
    /// as long as the other side keeps the application talking.
    fn wait<H: Wire + Data>(&mut self, host: &mut H, handle: Handle) -> Result<(), &'static str> {
        for _ in 0..=MOST_KEPT {
            match host.poll(&[handle], WAIT_MS)? {
                Some(ready) if ready == handle => return Ok(()),
                Some(_) => {
                    let delivery = host.receive()?;
                    self.keep(delivery)?;
                }
                None => return Err(fail::TIMED_OUT),
            }
        }
        Err(fail::BUSY)
    }

    /// Write `bytes` as the whole of the stream the application issued `claim` for, and wait for the acknowledgement
    /// that every byte reached its pump: a close is not a delivery.
    fn transmit<H: Wire + Data>(
        &mut self,
        host: &mut H,
        claim: u64,
        bytes: &[u8],
    ) -> Result<(), &'static str> {
        let handle = host.open(true, claim).map_err(|_| fail::REFUSED)?;
        let delivered = self.write_and_acknowledge(host, handle, bytes);
        host.close(handle);
        delivered
    }

    fn write_and_acknowledge<H: Wire + Data>(
        &mut self,
        host: &mut H,
        handle: Handle,
        bytes: &[u8],
    ) -> Result<(), &'static str> {
        write_all(host, handle, bytes).map_err(|_| fail::STOPPED)?;
        host.shutdown(handle).map_err(|_| fail::STOPPED)?;
        self.wait(host, handle)?;
        match read_ack(host, handle) {
            Ok(count) if u64::try_from(bytes.len()).is_ok_and(|sent| sent == count) => Ok(()),
            _ => Err(fail::STOPPED),
        }
    }

    /// Read the whole of the stream the application parked under `ticket`, at most as many bytes as an answer may have.
    fn take<H: Wire + Data>(&mut self, host: &mut H, ticket: u64) -> Result<Vec<u8>, &'static str> {
        let handle = host.open(false, ticket).map_err(|_| fail::NOT_UNDERSTOOD)?;
        let read = self.read_whole(host, handle);
        host.close(handle);
        read
    }

    fn read_whole<H: Wire + Data>(
        &mut self,
        host: &mut H,
        handle: Handle,
    ) -> Result<Vec<u8>, &'static str> {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        for _ in 0..MOST_READS {
            self.wait(host, handle)?;
            // One byte more than the most, so that something longer is seen to be longer.
            let room = chunk.len().min(wire::ANSWER_MAX + 1 - bytes.len());
            let count = host
                .read(handle, &mut chunk[..room])
                .map_err(|_| fail::STOPPED)?;
            if count == 0 {
                return Ok(bytes);
            }
            if count > room {
                return Err(fail::NOT_UNDERSTOOD);
            }
            bytes.extend_from_slice(&chunk[..count]);
            if bytes.len() > wire::ANSWER_MAX {
                return Err(fail::NOT_UNDERSTOOD);
            }
        }
        Err(fail::NOT_UNDERSTOOD)
    }

    /// Send the question that the application opened a stream for: only while it is the question that is out.
    pub fn flush<H: Wire + Data>(&mut self, host: &mut H) {
        for (claim, bytes) in std::mem::take(&mut self.outgoing) {
            if self.transmit(host, claim, &bytes).is_err() {
                self.talk.failed();
            }
        }
    }

    /// A call to `receive`: a stream from a paired device, a question to answer or the answer to one asked. `None` for
    /// a call cancelled meanwhile, which gets no reply.
    pub fn receive<H: Wire + Data>(&mut self, host: &mut H, delivery: &Value) -> Option<Value> {
        let ticket = delivery["ticket"].as_u64()?;
        if self.withdrawn(ticket) {
            self.settled(ticket);
            return None;
        }
        let result = self.attend(host, ticket, &delivery["args"]);
        if self.withdrawn(ticket) {
            self.settled(ticket);
            return None;
        }
        Some(json!({"op":"reply","ticket":ticket,"result":outcome(result)}))
    }

    fn attend<H: Wire + Data>(
        &mut self,
        host: &mut H,
        ticket: u64,
        args: &Value,
    ) -> Result<(), &'static str> {
        let peer = args["peer"]
            .as_str()
            .filter(|peer| !peer.is_empty() && peer.len() <= ALIAS_MAX)
            .ok_or(fail::NOT_UNDERSTOOD)?
            .to_owned();
        let bytes = self.take(host, ticket)?;
        match wire::parse(&bytes) {
            Ok(Message::Ask(ask)) => self.reply_to(host, ticket, &peer, &ask),
            Ok(Message::Answer(answer)) => {
                self.heard(&peer, answer);
                Ok(())
            }
            Err(_) => Err(fail::NOT_UNDERSTOOD),
        }
    }

    /// Answer a question of device `peer`: each part wanted is read from this device's application, which answers only
    /// what the person allowed Monitor to read, and what it refuses is said as refused. Nothing is kept: a reading is
    /// made for this question and for no other, so a permission withdrawn is withdrawn from the next answer.
    fn reply_to<H: Wire + Data>(
        &mut self,
        host: &mut H,
        ticket: u64,
        peer: &str,
        ask: &wire::Ask,
    ) -> Result<(), &'static str> {
        let mut parts = Vec::with_capacity(ask.parts.len());
        for &part in &ask.parts {
            let outcome = match self.call_now(host, SYSTEM, part.method(), json!({})) {
                Ok(said) if said.is_object() => Outcome::Ok(wire::value(part, &said)),
                Err(fail::DENIED) => Outcome::Denied,
                _ => Outcome::Unavailable,
            };
            parts.push((part, outcome));
        }
        // Nothing is sent for a question that was withdrawn while it was answered.
        if self.withdrawn(ticket) {
            return Err(fail::CANCELLED);
        }
        let bytes = wire::answer(ask.seq, &parts);
        if bytes.len() > wire::ANSWER_MAX {
            return Err(fail::NOT_UNDERSTOOD);
        }
        let opened = self.call_now(
            host,
            PEERS,
            "send",
            json!({"device":peer,"service":wire::SERVICE,"version":wire::VERSION}),
        )?;
        let claim = opened["claim"].as_u64().ok_or(fail::REFUSED)?;
        self.transmit(host, claim, &bytes)
    }

    /// The application's answer to the call that asked for a stream to the chosen device: the claim to write the
    /// question under, or why it was not given. Only for the question that is out.
    pub fn sent(&mut self, result: &Value) {
        let Some(asked) = &self.talk.asked else {
            return;
        };
        let seq = asked.seq;
        match (result["status"].as_str(), result["value"]["claim"].as_u64()) {
            (Some("ok"), Some(claim)) => self.outgoing.push((claim, wire::ask(seq))),
            // The person did not allow this extension to send to that device: said as such, and not asked again.
            _ if result["code"] == "denied" => {
                self.talk.refused = true;
                self.talk.asked = None;
            }
            _ => self.talk.failed(),
        }
    }

    /// The answer of the device `peer`. Taken only as the answer to the question that is out, from the device that was
    /// asked: anything else (late, unasked, from another device) is let go, and nothing of it is shown.
    fn heard(&mut self, peer: &str, answer: Answer) {
        let Some(asked) = &self.talk.asked else {
            return;
        };
        if self.chosen.as_deref() != Some(peer) || asked.seq != answer.seq {
            return;
        }
        let at = asked.at;
        for part in crate::PARTS {
            let outcome = answer
                .parts
                .iter()
                .find(|(said, _)| *said == part)
                .map_or(Outcome::Unavailable, |(_, outcome)| outcome.clone());
            match outcome {
                Outcome::Ok(value) => self.talk.gauges.read(part, value, at),
                Outcome::Denied => self.talk.gauges.deny(part),
                Outcome::Unavailable => self.talk.gauges.unavailable(part),
            }
        }
        self.talk.asked = None;
        self.talk.silent = 0;
        self.talk.answered = true;
    }

    /// The call, if one is due, that asks the application for a stream to the chosen device to write the question on.
    /// Due when this device's person allows it, no question is out (or the one out was lost), the page's clock has
    /// gone a second further than the last one, and the device has not let [`MOST_SILENT`] go by.
    pub fn ask_device(&mut self, opened: bool, at: u64) -> Option<Value> {
        let device = self.chosen.clone()?;
        if opened {
            self.choose(Some(device.clone()));
        }
        if self.refused_here() {
            return None;
        }
        if self
            .talk
            .asked
            .as_ref()
            .is_some_and(|asked| at.saturating_sub(asked.at) >= ANSWER_MS)
        {
            self.talk.failed();
            self.outgoing.clear();
        }
        if self.talk.asked.is_some() || self.talk.silent >= MOST_SILENT {
            return None;
        }
        if self
            .talk
            .last_at
            .is_some_and(|last| at.saturating_sub(last) < MIN_GAP)
        {
            return None;
        }
        self.seq = self.seq.checked_add(1).unwrap_or(1);
        let (id, call) = self.call(
            PEERS,
            "send",
            json!({"device":device,"service":wire::SERVICE,"version":wire::VERSION}),
        );
        self.asked_send = id;
        self.talk.asked = Some(Asked { seq: self.seq, at });
        self.talk.last_at = Some(at);
        Some(call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PARTS, Part, host::scripted::Script};

    fn ok(value: Value) -> Value {
        json!({"status":"ok","value":value})
    }
    fn error(code: &str) -> Value {
        json!({"status":"error","code":code,"message":"fixed"})
    }
    fn result(id: &Value, result: Value) -> Value {
        json!({"type":"result","id":id,"result":result})
    }
    fn receive_call(ticket: u64, peer: &str) -> Value {
        json!({"type":"call","ticket":ticket,"service":wire::SERVICE,"version":wire::VERSION,
            "method":"receive","args":{"peer":peer}})
    }
    fn event(t: &str, at: u64) -> Value {
        json!({"type":"call","ticket":1,"method":"event","args":{"event":{"t":t,"id":"watch","at":at}}})
    }
    fn script() -> Script {
        Script {
            acks: true,
            ..Script::default()
        }
    }
    /// What the application of a device says of each part, in the order of the cards.
    fn said(busy: u64, total: u64, received: u64) -> Vec<Value> {
        vec![
            ok(
                json!({"name":"Ryzen","cores":8,"busy":busy,"total":total,"celsius":55.4,"serial":"X1"}),
            ),
            ok(json!({"name":"AMD","busy":2500,"celsius":null})),
            ok(json!({"total":16_000_000_000u64,"available":4_000_000_000u64})),
            ok(json!({"total":1_000_000_000_000u64,"available":250_000_000_000u64})),
            ok(json!({"received":received,"sent":1_000})),
            ok(json!({"charge":4250,"charging":false,"minutes":135})),
        ]
    }
    /// The answer a device gives when its application says `said`.
    fn answer_of(seq: u64, said: &[Value]) -> Vec<u8> {
        let parts: Vec<(Part, Outcome)> = PARTS
            .iter()
            .zip(said)
            .map(|(part, said)| {
                let outcome = match said["status"].as_str() {
                    Some("ok") => Outcome::Ok(wire::value(*part, &said["value"])),
                    _ if said["code"] == "denied" => Outcome::Denied,
                    _ => Outcome::Unavailable,
                };
                (*part, outcome)
            })
            .collect();
        wire::answer(seq, &parts)
    }
    fn device(id: &str, name: &str) -> Value {
        json!({"device":id,"name":name,"presence":"online","allowed":true})
    }
    /// A state that has listed the devices `ids` and shows the first.
    fn showing(ids: &[(&str, &str)]) -> State {
        let mut state = State {
            devices: ids.iter().map(|(id, name)| device(id, name)).collect(),
            listed: true,
            ..State::default()
        };
        state.choose(Some(ids[0].0.to_owned()));
        state
    }
    fn sends(calls: &[Value]) -> Vec<&Value> {
        calls
            .iter()
            .filter(|call| call["method"] == "send")
            .collect()
    }
    /// A tick at `at`; the question it asks, if one is due, is given the stream `claim` and written.
    fn tick(state: &mut State, host: &mut Script, at: u64, claim: u64) -> Vec<Value> {
        let calls = state.ask(&event("tick", at));
        if let Some(send) = sends(&calls).first() {
            state.told(&result(&send["id"], ok(json!({"claim":claim}))));
            state.flush(host);
        }
        calls
    }
    fn asked(host: &Script) -> wire::Ask {
        let Ok(Message::Ask(ask)) = wire::parse(&host.written) else {
            panic!("a question was written");
        };
        ask
    }
    fn page(state: &State) -> String {
        state.page().to_string()
    }

    #[test]
    fn a_chosen_device_is_asked_for_every_part_and_not_again_while_the_question_is_out() {
        let mut state = showing(&[("a", "Desk")]);
        let mut host = script();
        let calls = tick(&mut state, &mut host, 1000, 9);
        // Of the application, the way to send to the device is asked, and the devices: this device's own parts are
        // not read while another device's are shown.
        assert!(
            calls
                .iter()
                .all(|call| call["service"] == PEERS || call["service"] == "peersus.config")
        );
        let send = sends(&calls);
        assert_eq!(send.len(), 1);
        assert_eq!(
            send[0]["args"],
            json!({"device":"a","service":"monitor.readings","version":1})
        );
        // The question is written whole on the stream issued for it, which is closed once it is acknowledged.
        assert_eq!(host.opened, [(true, 9)]);
        assert_eq!(
            asked(&host),
            wire::Ask {
                seq: 1,
                parts: PARTS.to_vec()
            }
        );
        assert_eq!((host.shutdowns, host.closed), (1, 1));
        // None while it is out, however the page's clock goes on.
        for at in [1500, 2000, 3000] {
            assert!(sends(&state.ask(&event("tick", at))).is_empty(), "{at}");
        }
    }

    #[test]
    fn the_answer_to_the_question_that_is_out_is_shown_as_the_numbers_of_that_device() {
        let mut state = showing(&[("a", "Desk")]);
        let mut host = script();
        tick(&mut state, &mut host, 1000, 9);
        host.incoming
            .push_back(answer_of(1, &said(100, 400, 5_000)));
        let reply = state.receive(&mut host, &receive_call(5, "a")).unwrap();
        assert_eq!(
            reply,
            json!({"op":"reply","ticket":5,"result":{"status":"ok","value":true}})
        );
        assert_eq!(host.opened.last(), Some(&(false, 5)));
        let page = page(&state);
        assert!(page.contains("Ryzen") && page.contains("8 cores") && page.contains("55°C"));
        assert!(page.contains("AMD") && page.contains("11.2 / 14.9 GiB"));
        assert!(page.contains("698.5 / 931.3 GiB"));
        assert!(page.contains(r#""text":"43%""#) && page.contains(r#""text":"2 h 15 min""#));
        // A field the device wrote that a part does not have is nowhere in what is shown.
        assert!(!page.contains("X1"));
        assert!(state.talk.answered && state.talk.asked.is_none());
        assert_eq!(state.talk.link(), Link::Live);
        // The next second asks again, under the next number.
        assert_eq!(sends(&state.ask(&event("tick", 2000))).len(), 1);
        assert_eq!(state.talk.asked.as_ref().map(|asked| asked.seq), Some(2));
    }

    #[test]
    fn the_rates_of_a_device_are_made_of_its_own_counters_at_the_times_it_was_asked() {
        let mut state = showing(&[("a", "Desk")]);
        let mut host = script();
        for (seq, at, busy, total, received) in
            [(1, 1000, 100, 400, 5_000), (2, 3000, 400, 800, 45_000)]
        {
            tick(&mut state, &mut host, at, 9 + seq);
            host.incoming
                .push_back(answer_of(seq, &said(busy, total, received)));
            state.receive(&mut host, &receive_call(5, "a"));
            if seq == 1 {
                // One answer is no rate.
                assert!(state.talk.gauges.busy.is_none() && state.talk.gauges.speed.is_none());
            }
        }
        assert_eq!(state.talk.gauges.busy, Some(7500));
        assert_eq!(state.talk.gauges.speed, Some((20_000, 0)));
        // Nothing of this device was asked or kept in their place.
        assert!(state.own.busy.is_none() && state.own.speed.is_none());
        assert!(page(&state).contains("75%") && page(&state).contains("20 KB/s"));
    }

    #[test]
    fn an_answer_is_taken_only_for_the_question_that_is_out_from_the_device_it_was_asked_of() {
        let mut state = showing(&[("a", "Desk"), ("b", "Laptop")]);
        let mut host = script();
        let nothing_known = |state: &State| {
            state
                .talk
                .gauges
                .parts
                .iter()
                .all(|part| *part == crate::gauges::Reading::Unknown)
        };
        // Not asked yet: an answer that comes is nobody's.
        host.incoming.push_back(answer_of(1, &said(1, 2, 3)));
        assert_eq!(
            state.receive(&mut host, &receive_call(5, "a")).unwrap()["result"]["status"],
            "ok"
        );
        assert!(nothing_known(&state));
        tick(&mut state, &mut host, 1000, 9);
        // Another number, another device, or one that was not chosen: none of them is the answer.
        for (seq, peer) in [(2, "a"), (1, "b"), (1, "stranger")] {
            host.incoming.push_back(answer_of(seq, &said(1, 2, 3)));
            let reply = state.receive(&mut host, &receive_call(6, peer)).unwrap();
            assert_eq!(reply["result"]["status"], "ok");
            assert!(
                nothing_known(&state) && state.talk.asked.is_some(),
                "{seq} {peer}"
            );
        }
        // The right one is, once; the same answer again, late, is not.
        host.incoming.push_back(answer_of(1, &said(1, 2, 3)));
        state.receive(&mut host, &receive_call(7, "a"));
        assert!(!nothing_known(&state));
        let before = page(&state);
        host.incoming.push_back(answer_of(1, &said(9, 9, 9)));
        state.receive(&mut host, &receive_call(8, "a"));
        assert_eq!(page(&state), before);
    }

    #[test]
    fn an_answer_that_is_not_understood_changes_nothing_and_is_told_so() {
        let mut state = showing(&[("a", "Desk")]);
        let mut host = script();
        tick(&mut state, &mut host, 1000, 9);
        for bad in [
            br#"{"answer":{"seq":1,"parts":{"memory":{"status":"ok","value":5}}}}"#.to_vec(),
            br#"{"answer":{"seq":1,"parts":{},"also":true}}"#.to_vec(),
            b"not json".to_vec(),
            Vec::new(),
        ] {
            host.incoming.push_back(bad);
            let reply = state.receive(&mut host, &receive_call(5, "a")).unwrap();
            assert_eq!(reply["result"]["status"], "error");
            assert_eq!(reply["result"]["code"], "failed");
            assert!(state.talk.asked.is_some() && !state.talk.answered);
        }
    }

    #[test]
    fn a_part_the_device_did_not_allow_says_so_in_its_card_and_the_others_show() {
        let mut state = showing(&[("a", "Desk")]);
        let mut host = script();
        tick(&mut state, &mut host, 1000, 9);
        let mut answer = said(1, 2, 3);
        answer[1] = error("denied");
        answer[5] = error("refused");
        host.incoming.push_back(answer_of(1, &answer));
        state.receive(&mut host, &receive_call(5, "a"));
        let page = page(&state);
        // The graphics are not allowed there and say where to allow them; the battery could not be read (a desktop has
        // none), which is not the same: it has no card at all.
        assert_eq!(page.matches("Not allowed").count(), 1);
        assert!(page.contains(
            "Desk does not let Monitor share it. Allow it there, in this extension's permissions."
        ));
        assert!(!page.contains("Allow it in this extension's permissions to see it here."));
        assert!(!page.contains("Battery"), "{page}");
        assert!(page.contains("8 cores") && page.contains("11.2 / 14.9 GiB"));
        assert_eq!(
            state.talk.gauges.parts[Part::Graphics as usize],
            crate::gauges::Reading::Denied
        );
        assert_eq!(
            state.talk.gauges.parts[Part::Battery as usize],
            crate::gauges::Reading::Unavailable
        );
    }

    #[test]
    fn a_question_that_is_lost_is_asked_again_and_a_device_that_loses_three_is_let_be() {
        let mut state = showing(&[("a", "Desk")]);
        let mut host = script();
        tick(&mut state, &mut host, 1000, 9);
        // Not lost while it is waited for.
        assert!(sends(&state.ask(&event("tick", 3999))).is_empty());
        // Lost after three seconds, and the next one goes at once.
        assert_eq!(sends(&tick(&mut state, &mut host, 4000, 10)).len(), 1);
        assert_eq!(state.talk.silent, 1);
        assert_eq!(sends(&tick(&mut state, &mut host, 7000, 11)).len(), 1);
        assert_eq!(state.talk.silent, 2);
        // The third is lost, and there is no fourth; that it does not answer is told once, through the application's
        // notifications, and not written on the page.
        let told = tick(&mut state, &mut host, 10_000, 12);
        assert!(sends(&told).is_empty());
        assert_eq!((state.talk.silent, state.talk.link()), (3, Link::Silent));
        let shown: Vec<&Value> = told
            .iter()
            .filter(|call| call["service"] == "peersus.notify")
            .collect();
        assert_eq!(shown.len(), 1);
        assert_eq!(
            shown[0]["args"]["text"],
            "Desk did not answer. Check that Monitor is installed there and allows this device, then choose it again."
        );
        let later = tick(&mut state, &mut host, 20_000, 13);
        assert!(sends(&later).is_empty());
        assert!(!later.iter().any(|call| call["service"] == "peersus.notify"));
        let page = page(&state);
        assert!(!page.contains("did not answer"));
        // What was never answered shows no number.
        assert!(!page.contains("Ryzen"));
        // Choosing it again in the application's chooser is asking again.
        state.answer(&json!({"t":"choose","id":"device","device":"a"}));
        assert_eq!(sends(&tick(&mut state, &mut host, 22_000, 14)).len(), 1);
        assert_eq!(state.talk.silent, 0);
    }

    #[test]
    fn a_device_that_stops_answering_shows_no_number_that_it_no_longer_says() {
        let mut state = showing(&[("a", "Desk")]);
        let mut host = script();
        tick(&mut state, &mut host, 1000, 9);
        host.incoming.push_back(answer_of(1, &said(1, 2, 3)));
        state.receive(&mut host, &receive_call(5, "a"));
        assert!(page(&state).contains("Ryzen"));
        tick(&mut state, &mut host, 2000, 10);
        // Lost: what it said before is not kept as if it still held.
        tick(&mut state, &mut host, 5000, 11);
        let page = page(&state);
        assert!(!page.contains("Ryzen") && !page.contains("12 GB"));
        assert!(page.contains("Asking Desk…"));
    }

    #[test]
    fn a_question_that_could_not_be_delivered_counts_as_lost_and_no_wait_is_longer_than_its_bound()
    {
        // The stream is not acknowledged: written and closed is not delivered.
        let mut state = showing(&[("a", "Desk")]);
        let mut host = Script::default();
        tick(&mut state, &mut host, 1000, 9);
        assert_eq!((state.talk.silent, state.talk.asked.is_none()), (1, true));
        assert_eq!(host.closed, 1);
        // The other end says nothing at all: one bounded wait, and nothing is read from it.
        let mut state = showing(&[("a", "Desk")]);
        let mut host = Script {
            silent: true,
            ..Script::default()
        };
        tick(&mut state, &mut host, 1000, 9);
        assert_eq!(host.waits, [WAIT_MS]);
        assert_eq!((state.talk.silent, host.closed), (1, 1));
        // The application gave no stream: the same.
        let mut state = showing(&[("a", "Desk")]);
        let calls = state.ask(&event("tick", 1000));
        let send = sends(&calls)[0];
        state.told(&result(&send["id"], error("refused")));
        assert_eq!(
            (
                state.talk.silent,
                state.talk.asked.is_none(),
                state.outgoing.len()
            ),
            (1, true, 0)
        );
    }

    #[test]
    fn a_device_this_person_did_not_allow_is_not_asked_and_its_card_says_so() {
        let mut state = showing(&[("a", "Desk")]);
        state.devices[0]["allowed"] = json!(false);
        let mut host = script();
        assert!(sends(&tick(&mut state, &mut host, 1000, 9)).is_empty());
        assert!(host.opened.is_empty());
        let page = page(&state);
        assert!(page.contains("Not allowed"));
        assert!(page.contains(
            "Allow this extension, for Desk, to see it, to send to it and to share numbers with it"
        ));
        // One card for the device, and none of the six.
        assert!(!page.contains("Usage") && !page.contains("Download"));
        // Allowed now: asked at once, and the page is of numbers again.
        state.listed_devices(&[device("a", "Desk")]);
        assert_eq!(sends(&tick(&mut state, &mut host, 2000, 9)).len(), 1);
        assert!(!self::page(&state).contains("Allow this extension, for Desk"));
    }

    #[test]
    fn a_refusal_of_the_application_is_not_asked_again() {
        let mut state = showing(&[("a", "Desk")]);
        let calls = state.ask(&event("tick", 1000));
        let send = sends(&calls)[0];
        state.told(&result(&send["id"], error("denied")));
        assert_eq!(state.link(), Some(Link::Refused));
        assert!(state.outgoing.is_empty());
        assert!(page(&state).contains("Allow this extension, for Desk"));
        for at in [2000, 9000, 20_000] {
            assert!(sends(&state.ask(&event("tick", at))).is_empty());
        }
        // The list says it is allowed now: asked again.
        state.listed_devices(&[device("a", "Desk")]);
        assert_eq!(sends(&state.ask(&event("tick", 21_000))).len(), 1);
    }

    #[test]
    fn choosing_another_device_or_this_one_stops_asking_the_first() {
        let mut state = showing(&[("a", "Desk"), ("b", "Laptop")]);
        let mut host = script();
        tick(&mut state, &mut host, 1000, 9);
        // Another is chosen: what was out for the first is let go, and the answer that comes for it is not the
        // other's.
        state.choose(Some("b".to_owned()));
        assert!(state.talk.asked.is_none() && state.outgoing.is_empty());
        host.incoming.push_back(answer_of(1, &said(1, 2, 3)));
        state.receive(&mut host, &receive_call(5, "a"));
        assert!(!state.talk.answered && !page(&state).contains("Ryzen"));
        // The other is asked on the next tick, by its own name; the stream the first was given is not used for it.
        let calls = state.ask(&event("tick", 2000));
        assert_eq!(sends(&calls)[0]["args"]["device"], "b");
        let late = sends(&calls)[0]["id"].clone();
        state.told(&result(&late, ok(json!({"claim":10}))));
        assert_eq!(state.outgoing.len(), 1);
        // This device is chosen: no device is asked any more, and this device's own parts are.
        state.choose(None);
        assert!(state.outgoing.is_empty());
        let calls = state.ask(&event("tick", 3000));
        assert!(sends(&calls).is_empty());
        assert_eq!(
            calls
                .iter()
                .filter(|call| call["service"] == SYSTEM)
                .count(),
            6
        );
        // What the one that was chosen says, late, changes nothing of what is shown.
        host.incoming.push_back(answer_of(2, &said(1, 2, 3)));
        state.receive(&mut host, &receive_call(6, "b"));
        assert!(!page(&state).contains("Ryzen"));
        // And what a stream that was claimed for it is let to wait: nothing was written for the late claim.
        state.flush(&mut host);
        assert_eq!(host.opened, [(true, 9), (false, 5), (false, 6)]);
    }

    #[test]
    fn a_question_goes_about_once_a_second_whatever_the_pace_of_the_pages_clock() {
        let mut state = showing(&[("a", "Desk")]);
        let mut host = script();
        tick(&mut state, &mut host, 1000, 9);
        host.incoming.push_back(answer_of(1, &said(1, 2, 3)));
        state.receive(&mut host, &receive_call(5, "a"));
        for at in [1100, 1300, 1500, 1899] {
            assert!(sends(&state.ask(&event("tick", at))).is_empty(), "{at}");
        }
        assert_eq!(sends(&state.ask(&event("tick", 1900))).len(), 1);
    }

    #[test]
    fn opening_the_page_again_starts_afresh() {
        let mut state = showing(&[("a", "Desk")]);
        let mut host = script();
        tick(&mut state, &mut host, 1000, 9);
        tick(&mut state, &mut host, 4000, 10);
        tick(&mut state, &mut host, 7000, 11);
        let last = tick(&mut state, &mut host, 10_000, 12);
        assert!(sends(&last).is_empty());
        let old = state.asked_send;
        assert_eq!(state.talk.link(), Link::Silent);
        // The page closed and opened: the device is asked at once, the silence is forgotten, and the question that was
        // out is not waited for.
        let calls = state.ask(&event("open", 0));
        assert_eq!(sends(&calls).len(), 1);
        assert_eq!(state.talk.silent, 0);
        assert_eq!(state.talk.asked.as_ref().map(|asked| asked.seq), Some(4));
        // A stream the application gave for a question of before is not written.
        assert_ne!(state.asked_send, old);
        state.told(&result(&json!(old), ok(json!({"claim":77}))));
        assert!(state.outgoing.is_empty());
    }

    #[test]
    fn a_question_is_answered_with_what_the_application_says_of_each_part_and_sent_to_the_device_that_asked()
     {
        let mut state = State::default();
        let mut host = script();
        host.incoming.push_back(wire::ask(7));
        let mut answers = said(100, 400, 5_000);
        answers[1] = error("denied");
        answers[5] = error("refused");
        host.results.extend(answers.clone());
        host.results.push_back(ok(json!({"claim":41})));
        let reply = state.receive(&mut host, &receive_call(3, "alex")).unwrap();
        assert_eq!(
            reply,
            json!({"op":"reply","ticket":3,"result":{"status":"ok","value":true}})
        );
        // Each part, once and in order, from the application and nowhere else; then the way to send to the device that
        // asked, by the name this extension has for it.
        let calls: Vec<(&str, &str)> = host
            .sent
            .iter()
            .map(|call| {
                (
                    call["service"].as_str().unwrap(),
                    call["method"].as_str().unwrap(),
                )
            })
            .collect();
        let mut expected: Vec<(&str, &str)> =
            PARTS.iter().map(|part| (SYSTEM, part.method())).collect();
        expected.push((PEERS, "send"));
        assert_eq!(calls, expected);
        assert!(
            host.sent
                .iter()
                .all(|call| call["args"] == json!({}) || call["method"] == "send")
        );
        assert_eq!(
            host.sent[6]["args"],
            json!({"device":"alex","service":"monitor.readings","version":1})
        );
        assert!(host.sent.iter().all(|call| call["timeout_ms"] == CALL_MS));
        // The ticket's stream was read, the claim's was written, and what was written is the answer to this question,
        // with what the person allowed and nothing the application said beyond the fields of a part.
        assert_eq!(host.opened, [(false, 3), (true, 41)]);
        assert_eq!((host.shutdowns, host.closed), (1, 2));
        assert_eq!(host.written, answer_of(7, &answers));
        let Ok(Message::Answer(answer)) = wire::parse(&host.written) else {
            panic!("an answer was written");
        };
        assert_eq!(answer.seq, 7);
        assert_eq!(answer.parts[1], (Part::Graphics, Outcome::Denied));
        assert_eq!(answer.parts[5], (Part::Battery, Outcome::Unavailable));
        assert!(!String::from_utf8(host.written).unwrap().contains("X1"));
    }

    #[test]
    fn only_the_parts_asked_for_are_read() {
        let mut state = State::default();
        let mut host = script();
        host.incoming
            .push_back(br#"{"ask":{"seq":2,"parts":["battery","memory"]}}"#.to_vec());
        host.results
            .push_back(ok(json!({"total":10,"available":4})));
        host.results
            .push_back(ok(json!({"charge":5000,"charging":true,"minutes":null})));
        host.results.push_back(ok(json!({"claim":5})));
        state.receive(&mut host, &receive_call(3, "alex")).unwrap();
        let methods: Vec<&str> = host
            .sent
            .iter()
            .map(|call| call["method"].as_str().unwrap())
            .collect();
        // Read in the order asked, and the way back last.
        assert_eq!(methods, ["battery", "memory", "send"]);
    }

    #[test]
    fn nothing_is_kept_between_questions_so_a_permission_withdrawn_is_withdrawn_from_the_next_answer()
     {
        let mut state = State::default();
        let mut host = script();
        let mut written = Vec::new();
        for (ticket, allowed) in [(3, true), (4, false)] {
            host.incoming.push_back(wire::ask(ticket));
            let mut answers = said(1, 2, 3);
            if !allowed {
                answers[0] = error("denied");
            }
            host.results.extend(answers);
            host.results.push_back(ok(json!({"claim":ticket})));
            state
                .receive(&mut host, &receive_call(ticket, "alex"))
                .unwrap();
            written.push(host.written.len());
        }
        // Two answers, written one after the other: the first has the processor, and the second, to the question after
        // the person withdrew it, says it is not allowed, whatever was said before.
        let (first, second) = host.written.split_at(written[0]);
        let processor = |bytes: &[u8]| match wire::parse(bytes) {
            Ok(Message::Answer(answer)) => answer.parts[0].clone(),
            other => panic!("an answer was written: {other:?}"),
        };
        assert!(matches!(
            processor(first),
            (Part::Processor, Outcome::Ok(_))
        ));
        assert_eq!(processor(second), (Part::Processor, Outcome::Denied));
        assert!(!String::from_utf8_lossy(second).contains("Ryzen"));
    }

    #[test]
    fn a_question_that_is_not_understood_is_refused_before_the_application_is_asked_anything() {
        for bad in [
            b"not json".to_vec(),
            br#"{"ask":{"seq":1}}"#.to_vec(),
            br#"{"ask":{"seq":1,"parts":["memory"],"also":1}}"#.to_vec(),
            vec![b'['; 100],
            Vec::new(),
        ] {
            let mut state = State::default();
            let mut host = script();
            host.incoming.push_back(bad);
            let reply = state.receive(&mut host, &receive_call(3, "alex")).unwrap();
            assert_eq!(reply["result"]["status"], "error");
            assert_eq!(reply["result"]["code"], "failed");
            assert!(host.sent.is_empty() && host.written.is_empty());
            assert_eq!((host.opened.clone(), host.closed), (vec![(false, 3)], 1));
        }
        // A stream with no device named is not even opened.
        let mut state = State::default();
        let mut host = script();
        for args in [
            json!({}),
            json!({"peer":""}),
            json!({"peer":7}),
            json!({"peer":"x".repeat(ALIAS_MAX + 1)}),
        ] {
            let call = json!({"type":"call","ticket":3,"method":"receive","args":args});
            let reply = state.receive(&mut host, &call).unwrap();
            assert_eq!(reply["result"]["code"], "failed");
        }
        assert!(host.opened.is_empty());
        // A call with no ticket cannot be replied to.
        assert!(
            state
                .receive(&mut host, &json!({"type":"call","method":"receive"}))
                .is_none()
        );
    }

    #[test]
    fn what_a_device_sends_is_read_no_further_than_its_size() {
        let mut state = State::default();
        let mut host = script();
        host.incoming.push_back(vec![b' '; 100_000]);
        let reply = state.receive(&mut host, &receive_call(3, "alex")).unwrap();
        assert_eq!(reply["result"]["code"], "failed");
        let unread = host.incoming.front().map_or(0, Vec::len);
        assert!(
            unread > 100_000 - 2 * wire::ANSWER_MAX,
            "{unread} bytes were not read"
        );
        assert!(host.sent.is_empty());
    }

    #[test]
    fn a_stream_that_drips_is_cut_off_after_its_reads_and_a_whole_one_in_pieces_is_not() {
        let mut state = State::default();
        let mut host = script();
        for _ in 0..100 {
            host.incoming.push_back(vec![b' ']);
        }
        let reply = state.receive(&mut host, &receive_call(3, "alex")).unwrap();
        assert_eq!(reply["result"]["code"], "failed");
        assert_eq!(host.incoming.len(), 100 - MOST_READS);
        assert!(host.sent.is_empty() && host.written.is_empty());
        // A question that comes in a few pieces is the question.
        let mut host = script();
        let question = wire::ask(7);
        for piece in question.chunks(question.len() / 5 + 1) {
            host.incoming.push_back(piece.to_vec());
        }
        host.results.extend(said(1, 2, 3));
        host.results.push_back(ok(json!({"claim":8})));
        let reply = state.receive(&mut host, &receive_call(4, "alex")).unwrap();
        assert_eq!(reply["result"]["status"], "ok");
        // An answer as long as one may be, in pieces, is heard: its words are what is judged, not its length.
        let mut state = showing(&[("a", "Desk")]);
        let mut host = script();
        tick(&mut state, &mut host, 1000, 9);
        let mut long = answer_of(1, &said(100, 400, 5_000));
        long.resize(wire::ANSWER_MAX, b' ');
        for piece in long.chunks(3000) {
            host.incoming.push_back(piece.to_vec());
        }
        state.receive(&mut host, &receive_call(5, "a")).unwrap();
        assert!(state.talk.answered);
    }

    #[test]
    fn an_answer_that_cannot_be_sent_back_says_so_and_nothing_is_written() {
        // The application will not send to the device.
        let mut state = State::default();
        let mut host = script();
        host.incoming.push_back(wire::ask(7));
        host.results.extend(said(1, 2, 3));
        host.results.push_back(error("refused"));
        let reply = state.receive(&mut host, &receive_call(3, "alex")).unwrap();
        assert_eq!(reply["result"]["code"], "unavailable");
        assert!(host.written.is_empty() && host.opened == [(false, 3)]);
        // The stream is not acknowledged: it was written and it is not a delivery.
        let mut host = Script::default();
        host.incoming.push_back(wire::ask(7));
        host.results.extend(said(1, 2, 3));
        host.results.push_back(ok(json!({"claim":8})));
        let reply = state.receive(&mut host, &receive_call(4, "alex")).unwrap();
        assert_eq!(reply["result"]["code"], "unavailable");
        assert_eq!(
            (host.opened.clone(), host.closed),
            (vec![(false, 4), (true, 8)], 2)
        );
        // The application gave no claim.
        let mut host = script();
        host.incoming.push_back(wire::ask(7));
        host.results.extend(said(1, 2, 3));
        host.results.push_back(ok(json!({})));
        let reply = state.receive(&mut host, &receive_call(5, "alex")).unwrap();
        assert_eq!(reply["result"]["code"], "unavailable");
    }

    #[test]
    fn a_device_that_opens_a_stream_and_says_nothing_is_waited_for_once_and_not_longer() {
        let mut state = State::default();
        let mut host = Script {
            silent: true,
            ..Script::default()
        };
        let reply = state.receive(&mut host, &receive_call(3, "alex")).unwrap();
        assert_eq!(reply["result"]["code"], "unavailable");
        assert_eq!(host.waits, [WAIT_MS]);
        assert_eq!((host.closed, host.sent.len()), (1, 0));
    }

    #[test]
    fn what_the_application_delivers_while_something_is_waited_for_is_kept_in_order_for_later() {
        let mut state = State::default();
        let mut host = script();
        host.incoming.push_back(wire::ask(7));
        for ticket in [11, 12, 13] {
            host.deliveries.push_back(event_with(ticket));
        }
        host.results.extend(said(1, 2, 3));
        host.results.push_back(ok(json!({"claim":8})));
        let reply = state.receive(&mut host, &receive_call(3, "alex")).unwrap();
        assert_eq!(reply["result"]["status"], "ok");
        let kept: Vec<u64> = state
            .waiting
            .iter()
            .map(|d| d["ticket"].as_u64().unwrap())
            .collect();
        assert_eq!(kept, [11, 12, 13]);
        // And they are handled first, in the order they came.
        assert_eq!(state.next(&mut host).unwrap()["ticket"], 11);
        assert_eq!(state.next(&mut host).unwrap()["ticket"], 12);
    }
    fn event_with(ticket: u64) -> Value {
        json!({"type":"call","ticket":ticket,"method":"event","args":{"event":{"t":"tick","id":"watch","at":1}}})
    }

    #[test]
    fn so_much_arriving_that_it_cannot_be_kept_ends_the_wait_instead_of_growing_it() {
        let mut state = State::default();
        let mut host = script();
        host.incoming.push_back(wire::ask(7));
        for ticket in 0..200 {
            host.deliveries.push_back(event_with(ticket));
        }
        let reply = state.receive(&mut host, &receive_call(3, "alex")).unwrap();
        assert_eq!(reply["result"]["code"], "busy");
        assert_eq!(state.waiting.len(), MOST_KEPT);
        assert!(host.sent.is_empty());
    }

    #[test]
    fn a_question_cancelled_before_it_is_started_or_while_it_is_answered_gets_no_reply_and_sends_nothing()
     {
        // Before: not started, and the cancellation is not kept.
        let mut state = State::default();
        let mut host = script();
        host.incoming.push_back(wire::ask(7));
        state
            .waiting
            .push_back(json!({"type":"cancelled","ticket":3}));
        assert!(state.receive(&mut host, &receive_call(3, "alex")).is_none());
        assert!(host.opened.is_empty() && host.sent.is_empty() && state.waiting.is_empty());
        // While: it was cancelled as the application's parts were being read; nothing is sent to the device and there
        // is no reply to a call that no longer waits.
        let mut host = script();
        host.incoming.push_back(wire::ask(7));
        host.deliveries
            .push_back(json!({"type":"cancelled","ticket":3}));
        host.results.extend(said(1, 2, 3));
        host.results.push_back(ok(json!({"claim":8})));
        assert!(state.receive(&mut host, &receive_call(3, "alex")).is_none());
        assert!(host.sent.iter().all(|call| call["method"] != "send"));
        assert!(host.written.is_empty() && state.waiting.is_empty());
        // Another call's cancellation is not this one's.
        let mut host = script();
        host.incoming.push_back(wire::ask(7));
        host.deliveries
            .push_back(json!({"type":"cancelled","ticket":99}));
        host.results.extend(said(1, 2, 3));
        host.results.push_back(ok(json!({"claim":8})));
        assert!(state.receive(&mut host, &receive_call(3, "alex")).is_some());
        assert_eq!(state.waiting.len(), 1);
    }

    #[test]
    fn two_monitors_show_one_the_numbers_of_the_other_with_what_its_person_allowed() {
        // This device chooses Desk.
        let mut here = showing(&[("desk", "Desk")]);
        let mut here_host = script();
        tick(&mut here, &mut here_host, 1000, 21);
        // Desk's Monitor gets the question from the stream of this device (which it knows as `alex`), its person having
        // allowed everything but the graphics.
        let mut there = State::default();
        let mut there_host = script();
        there_host.incoming.push_back(here_host.written.clone());
        let mut answers = said(100, 400, 5_000);
        answers[1] = error("denied");
        there_host.results.extend(answers);
        there_host.results.push_back(ok(json!({"claim":33})));
        let reply = there
            .receive(&mut there_host, &receive_call(4, "alex"))
            .unwrap();
        assert_eq!(reply["result"]["status"], "ok");
        assert_eq!(
            there_host.sent.last().unwrap()["args"],
            json!({"device":"alex","service":"monitor.readings","version":1})
        );
        // The answer travels to this device, which hears it from Desk.
        here_host.incoming.push_back(there_host.written.clone());
        here.receive(&mut here_host, &receive_call(8, "desk"))
            .unwrap();
        let page = page(&here);
        assert!(page.contains("8 cores") && page.contains("11.2 / 14.9 GiB"));
        assert_eq!(page.matches("Not allowed").count(), 1);
        assert!(page.contains("Desk does not let Monitor share it."));
        // This device was never asked for a number of its own, and shows none.
        assert!(here_host.sent.iter().all(|call| call["service"] != SYSTEM));
    }

    #[test]
    fn the_guest_answers_a_question_through_its_whole_loop() {
        let mut host = script();
        host.deliveries.push_back(receive_call(3, "alex"));
        host.incoming.push_back(wire::ask(7));
        host.results.extend(said(1, 2, 3));
        host.results.push_back(ok(json!({"claim":41})));
        // The loop ends when the application goes away, which is when it has nothing more to say.
        assert_eq!(crate::run(&mut host), 0);
        // It first asks for its settings, then answers: the six parts, the way back, and the reply to the call.
        assert_eq!(host.sent[0]["service"], "peersus.config");
        assert_eq!(host.sent.len(), 1 + 6 + 1 + 1);
        assert_eq!(host.sent[8]["op"], "reply");
        assert_eq!(host.sent[8]["ticket"], 3);
        assert_eq!(host.sent[8]["result"]["status"], "ok");
        assert_eq!(
            wire::parse(&host.written).map(|m| matches!(m, Message::Answer(_))),
            Ok(true)
        );
    }

    #[test]
    fn the_guest_asks_for_what_the_page_shows_and_answers_the_page_at_once() {
        let mut host = script();
        host.deliveries.push_back(event("open", 0));
        assert_eq!(crate::run(&mut host), 0);
        let asked: Vec<&str> = host
            .sent
            .iter()
            .filter(|call| call["op"] == "call")
            .map(|call| call["service"].as_str().unwrap())
            .collect();
        // Its settings (twice: at the start and when the page opens), this device's six parts and the devices.
        assert_eq!(
            asked.iter().filter(|service| **service == SYSTEM).count(),
            6
        );
        assert_eq!(asked.iter().filter(|service| **service == PEERS).count(), 1);
        let reply = host.sent.last().unwrap();
        assert_eq!(
            (reply["op"].as_str(), reply["ticket"].as_u64()),
            (Some("reply"), Some(1))
        );
        assert_eq!(reply["result"]["value"]["k"], "interval");
    }

    #[test]
    fn what_a_part_says_is_never_shown_as_that_of_the_other_device() {
        // This device's numbers are known; another device is chosen and has said nothing.
        let mut state = showing(&[("a", "Desk")]);
        state.choose(None);
        state.own.read(
            Part::Processor,
            json!({"name":"Mine","cores":4,"busy":1,"total":2,"celsius":40.}),
            1000,
        );
        assert!(page(&state).contains("Mine"));
        state.choose(Some("a".to_owned()));
        assert!(!page(&state).contains("Mine"));
        state.choose(None);
        assert!(page(&state).contains("Mine"));
    }
}
