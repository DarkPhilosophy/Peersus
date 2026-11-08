//! Remote control as a sandboxed guest: one side sends what a person does on the application's
//! touchpad, the other replays it through the application's own input service.
//!
//! Two calls are answered:
//! - `input`, from the application on the controlling device: a batch of what the person did on
//!   the Remote page (`{"peer", "events", "end"}`). The batch is written on to that device over a
//!   connection kept open while the person keeps going, so a gesture is not a new connection per
//!   move.
//! - `receive`, from the application on the controlled computer: a connection from a device the
//!   person allowed. Each line on it is one input; complete lines are replayed as they arrive.
//!
//! The guest never reads or names a device of the system: the inputs are plain data, and only the
//! application decides what they become. Answers are fixed words.
use crate::host::{Data, Handle, Wire, read_ack, write_all};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The application's own services this extension calls.
const PEERS: &str = "peersus.peers";
const INPUT: &str = "peersus.input";
const CONFIG: &str = "peersus.config";
const PAGES: &str = "peersus.pages";
/// The service the other device runs for this extension, and its version.
pub const SERVICE: &str = "control.pointer";
const VERSION: u32 = 1;
const CALL_MS: u32 = 4_000;
/// Longest line one input may take on the connection: an input is small, a longer line is not one.
pub const MOST_LINE: usize = 1024;
/// Most inputs replayed in one call to the application: its own bound.
const MOST_BATCH: usize = 64;
/// A connection is closed and a new one opened after this many batches, so no one connection
/// outlives the time the other side gives a call.
pub const ROTATE_AFTER: u32 = 200;
/// The last line of a rotated connection: the same gesture goes on in the next one, so what the
/// receiver holds (a pressed button, the pen near the screen) must not be let go between them.
pub const CONTINUES: &str = r#"{"continues":true}"#;
/// The last line of a connection closed because a gesture ended: what was pressed is let go, but
/// the pointer stays where it is (the controlled computer hides it once it has been still). The
/// connection is not kept open between gestures.
pub const RESTS: &str = r#"{"rests":true}"#;

/// How a connection that was replayed ended.
#[derive(Debug, PartialEq)]
enum Ended {
    /// Rotated mid-gesture: nothing is let go.
    Continues,
    /// A gesture ended: what is held is let go, the pointer stays.
    Rests,
    /// Closed with no word: everything is let go and the pointer leaves.
    Closed,
}

pub mod answer {
    pub const REFUSED: &str = "That device cannot be controlled";
    pub const STOPPED: &str = "The other device stopped listening";
    pub const NOT_UNDERSTOOD: &str = "What was sent was not understood";
    pub const NOT_REPLAYED: &str = "This computer could not replay the input";
    pub const UNKNOWN: &str = "Nothing here answers that";
    pub const CANCELLED: &str = "The operation was cancelled";
}

/// The connection kept open to the device being controlled.
pub struct Outgoing {
    pub device: String,
    pub handle: Handle,
    pub batches: u32,
    /// Every byte written on this connection, end marker included: what its acknowledgement must count.
    pub sent: u64,
}

/// What this guest keeps between calls.
#[derive(Default)]
pub struct State {
    pub outgoing: Option<Outgoing>,
    /// A closed stream may leave a resting pointer or an ongoing gesture on this device.
    retained: Option<String>,
    pub next: u64,
    /// Deliveries that arrived while waiting for an answer of ours, to be handled afterwards.
    pub waiting: Vec<Value>,
    incoming: BTreeMap<Handle, Incoming>,
    pub ui: crate::ui::Ui,
    pub layout: crate::layout::Ui,
    /// The layout-page option as last placed, and when (page time) it was last read: the application
    /// keeps the guest running when the option changes, so the guest looks again while its pages are used.
    layout_page: Option<bool>,
    pages_checked: u64,
}

impl State {
    /// Only the guest interprets this option; the host receives ordinary additional placements.
    pub(crate) fn configure_pages<H: Wire>(&mut self, host: &mut H) -> Result<(), &'static str> {
        let config = self.call(host, CONFIG, "get", json!({}))?;
        let wanted = config["settings"]["layout-page"].as_bool().unwrap_or(false);
        if self.layout_page == Some(wanted) {
            return Ok(());
        }
        let pages = if wanted {
            json!([{"id":"layout-page", "page":"layout", "placement":"primary"}])
        } else {
            json!([])
        };
        self.call(host, PAGES, "set", json!({"pages":pages}))?;
        self.layout_page = Some(wanted);
        Ok(())
    }

    /// At most every two seconds of page time, place the layout page again if its option changed.
    fn follow_pages<H: Wire>(&mut self, host: &mut H, at: u64) {
        if at.saturating_sub(self.pages_checked) < 2000 && self.pages_checked != 0 {
            return;
        }
        self.pages_checked = at.max(1);
        let _ = self.configure_pages(host);
    }

    /// Ask the application for something and wait for exactly that answer, keeping anything else
    /// that arrives meanwhile for later.
    pub(crate) fn call<H: Wire>(
        &mut self,
        host: &mut H,
        service: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, &'static str> {
        self.next = self.next.checked_add(1).ok_or(answer::REFUSED)?;
        let id = self.next;
        host.send(json!({
            "op":"call","id":id,"provider":"host","service":service,"version":1,
            "method":method,"args":args,"timeout_ms":CALL_MS
        }))?;
        loop {
            let message = host.receive()?;
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
                Some("call") | Some("cancelled") | Some("event") if self.waiting.len() < 64 => {
                    self.waiting.push(message)
                }
                Some("call") | Some("cancelled") | Some("event") => return Err(answer::REFUSED),
                _ => {}
            }
        }
    }

    fn connect<H: Wire + Data>(&mut self, host: &mut H, device: &str) -> Result<Outgoing, &'static str> {
        let sent = self.call(
            host, PEERS, "send",
            json!({"device":device,"service":SERVICE,"version":VERSION}),
        )?;
        let claim = sent["claim"].as_u64().ok_or(answer::REFUSED)?;
        let handle = host.open(true, claim).map_err(|_| answer::REFUSED)?;
        Ok(Outgoing { device: device.to_owned(), handle, batches: 0, sent: 0 })
    }

    fn close<H: Data>(host: &mut H, outgoing: Outgoing) -> Result<(), &'static str> {
        // Delivered means every byte written: a shorter count is input lost on the way, never a success.
        let done = host.shutdown(outgoing.handle)
            .and_then(|()| read_ack(host, outgoing.handle))
            .map_err(|_| answer::STOPPED)
            .and_then(|count| if count == outgoing.sent { Ok(()) } else { Err(answer::STOPPED) });
        host.close(outgoing.handle);
        done
    }

    /// Leaving also releases a pointer retained by an already closed, rested or rotated stream. A
    /// stream opened only to say so carries nothing and is not waited on: the other side reads the
    /// end of it when it reads, and this side, which runs everything else of the page, must not
    /// stand still until then.
    pub fn finish<H: Wire + Data>(&mut self, host: &mut H) -> Result<(), &'static str> {
        match self.outgoing.take() {
            Some(outgoing) => {
                self.retained = Some(outgoing.device.clone());
                Self::close(host, outgoing)?;
            }
            None => {
                let Some(device) = self.retained.clone() else { return Ok(()) };
                let outgoing = self.connect(host, &device)?;
                let _ = host.shutdown(outgoing.handle);
                host.close(outgoing.handle);
            }
        }
        self.retained = None;
        Ok(())
    }

    fn pause<H: Wire + Data>(&mut self, host: &mut H, marker: &str) -> Result<(), &'static str> {
        let Some(mut outgoing) = self.outgoing.take() else { return Ok(()) };
        self.retained = Some(outgoing.device.clone());
        let mut line = marker.as_bytes().to_vec();
        line.push(b'\n');
        if write_all(host, outgoing.handle, &line).is_err() {
            host.close(outgoing.handle);
            return Err(answer::STOPPED);
        }
        outgoing.sent += line.len() as u64;
        Self::close(host, outgoing)
    }

    /// Release held input between gestures without losing the device that still draws the pointer.
    pub fn rest<H: Wire + Data>(&mut self, host: &mut H) -> Result<(), &'static str> {
        self.pause(host, RESTS)
    }

    /// Rotate without releasing a gesture; a later explicit end still reaches the same device.
    pub fn rotate<H: Wire + Data>(&mut self, host: &mut H) -> Result<(), &'static str> {
        self.pause(host, CONTINUES)
    }

    /// `input`: write a batch to the device being controlled, opening a connection when there is none.
    pub fn input<H: Wire + Data>(
        &mut self,
        host: &mut H,
        args: &Value,
    ) -> Result<(), &'static str> {
        let device = args["peer"].as_str().ok_or(answer::REFUSED)?.to_owned();
        let events = args["events"].as_array().ok_or(answer::NOT_UNDERSTOOD)?;
        let end = args["end"].as_bool().unwrap_or(false);
        // Another device chosen: what was open to the first ends first.
        if self.outgoing.as_ref().is_some_and(|o| o.device != device)
            || self.retained.as_ref().is_some_and(|previous| previous != &device) {
            let _ = self.finish(host);
        }
        if !events.is_empty() {
            let mut lines = Vec::new();
            for event in events {
                let line = serde_json::to_vec(event).map_err(|_| answer::NOT_UNDERSTOOD)?;
                if line.len() > MOST_LINE || !event.is_object() {
                    return Err(answer::NOT_UNDERSTOOD);
                }
                lines.extend_from_slice(&line);
                lines.push(b'\n');
            }
            if self.outgoing.is_none() {
                self.outgoing = Some(self.connect(host, &device)?);
                self.retained = None;
            }
            let outgoing = self.outgoing.as_mut().ok_or(answer::REFUSED)?;
            if write_all(host, outgoing.handle, &lines).is_err() {
                // The other side is gone: this connection is over, the next batch opens another.
                let handle = outgoing.handle;
                self.outgoing = None;
                host.close(handle);
                return Err(answer::STOPPED);
            }
            outgoing.sent += lines.len() as u64;
            outgoing.batches += 1;
            if outgoing.batches >= ROTATE_AFTER && !end {
                // Said before the end, so the other side keeps a held drag held across the rotation.
                return self.rotate(host);
            }
        }
        if end {
            self.finish(host)?;
        }
        Ok(())
    }

    /// Accept a receiver without reading it here: every peer must get a turn before EOF.
    pub fn receive<H: Wire + Data>(
        &mut self,
        host: &mut H,
        ticket: u64,
        args: &Value,
    ) -> Result<(), &'static str> {
        let owner = args["peer"]
            .as_str()
            .ok_or(answer::NOT_UNDERSTOOD)?
            .to_owned();
        // On unless the person turned it off; a reply that cannot be read keeps the default.
        let config = self.call(host, CONFIG, "get", json!({})).ok();
        let setting = |id: &str| {
            config
                .as_ref()
                .and_then(|config| config["settings"][id].as_bool())
                .unwrap_or(true)
        };
        let own = setting("independent");
        let named = own && setting("name-tag");
        let handle = host
            .open(false, ticket)
            .map_err(|_| answer::NOT_UNDERSTOOD)?;
        self.incoming.insert(handle, Incoming {
            ticket, target: Target { owner, own, named }, pending: Vec::new(), ended: Ended::Closed,
        });
        Ok(())
    }

    /// Await one bounded piece of work. Queue reads and data reads remain separate so a
    /// live controller cannot monopolize the guest while another controller is idle.
    pub fn next<H: Wire + Data>(&mut self, host: &mut H) -> Result<Option<Value>, &'static str> {
        let mut handles = Vec::new();
        let mut owners = Vec::new();
        for (&handle, incoming) in &self.incoming {
            // Rotations from one peer retain their order, without delaying other peers.
            if !owners.contains(&incoming.target.owner.as_str()) {
                owners.push(incoming.target.owner.as_str());
                handles.push(handle);
            }
        }
        if handles.is_empty() {
            return if self.waiting.is_empty() { host.receive().map(Some) }
                else { Ok(Some(self.waiting.remove(0))) };
        }
        let ready = host.poll(&handles, if self.waiting.is_empty() { -1 } else { 0 })?;
        if let Some(handle) = ready.filter(|&handle| handle > 0) {
            if let Some(reply) = self.advance(host, handle) {
                host.send(reply)?;
            }
            return Ok(None);
        }
        if !self.waiting.is_empty() {
            return Ok(Some(self.waiting.remove(0)));
        }
        match ready {
            Some(0) => host.receive().map(Some),
            _ => Ok(None),
        }
    }

    fn advance<H: Wire + Data>(&mut self, host: &mut H, handle: Handle) -> Option<Value> {
        let mut incoming = self.incoming.remove(&handle)?;
        let result = self.replay(host, handle, &mut incoming);
        if matches!(result, Ok(false)) {
            self.incoming.insert(handle, incoming);
            return None;
        }
        Some(self.complete_receive(host, handle, incoming, result.map(|_| ())))
    }

    fn complete_receive<H: Wire + Data>(
        &mut self, host: &mut H, handle: Handle, incoming: Incoming,
        result: Result<(), &'static str>,
    ) -> Value {
        host.close(handle);
        let result = match result {
            Ok(_) if incoming.ended == Ended::Continues => Ok(()),
            result => {
                let keep = result.is_ok() && incoming.ended == Ended::Rests;
                let released = self.call(host, INPUT, "release",
                    json!({"owner": incoming.target.owner, "keep": keep}));
                result.map(|_| ()).and(released.map(|_| ()).map_err(|_| answer::NOT_REPLAYED))
            }
        };
        json!({"op":"reply","ticket":incoming.ticket,"result":outcome(result)})
    }

    fn replay<H: Wire + Data>(
        &mut self,
        host: &mut H,
        handle: Handle,
        incoming: &mut Incoming,
    ) -> Result<bool, &'static str> {
        let mut buffer = [0u8; 4096];
        let count = host.read(handle, &mut buffer).map_err(|_| answer::STOPPED)?;
        if count == 0 {
            return if incoming.pending.is_empty() { Ok(true) } else { Err(answer::NOT_UNDERSTOOD) };
        }
        if count > buffer.len() {
            return Err(answer::NOT_UNDERSTOOD);
        }
        incoming.pending.extend_from_slice(&buffer[..count]);
        let mut batch = Vec::new();
        while let Some(end) = incoming.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = incoming.pending.drain(..=end).collect();
            if line.len() > MOST_LINE + 1 {
                return Err(answer::NOT_UNDERSTOOD);
            }
            let event: Value = serde_json::from_slice(&line[..line.len() - 1])
                .map_err(|_| answer::NOT_UNDERSTOOD)?;
            if !event.is_object() {
                return Err(answer::NOT_UNDERSTOOD);
            }
            incoming.ended = if event == json!({"continues": true}) { Ended::Continues }
                else if event == json!({"rests": true}) { Ended::Rests }
                else { Ended::Closed };
            if incoming.ended != Ended::Closed {
                continue;
            }
            batch.push(event);
            if batch.len() == MOST_BATCH {
                self.replay_batch(host, std::mem::take(&mut batch), &incoming.target)?;
            }
        }
        if incoming.pending.len() > MOST_LINE {
            return Err(answer::NOT_UNDERSTOOD);
        }
        if !batch.is_empty() {
            self.replay_batch(host, batch, &incoming.target)?;
        }
        Ok(false)
    }

    fn replay_batch<H: Wire>(
        &mut self,
        host: &mut H,
        events: Vec<Value>,
        target: &Target,
    ) -> Result<(), &'static str> {
        let mut args = json!({"events": events, "owner": target.owner});
        if target.own {
            args["pointer"] = json!("own");
        }
        // Only asks: the application decides the name, from the device it knows by `owner`, and draws it.
        if target.named {
            args["show_name"] = json!(true);
        }
        self.call(host, INPUT, "inject", args)
            .map(|_| ())
            .map_err(|reason| {
                if reason == answer::CANCELLED {
                    reason
                } else {
                    answer::NOT_REPLAYED
                }
            })
    }
}

/// Whose input is replayed, and whether it moves a pointer of its own.
struct Target {
    owner: String,
    own: bool,
    /// Whether its pointer carries the controlling device's name.
    named: bool,
}

struct Incoming {
    ticket: u64,
    target: Target,
    pending: Vec<u8>,
    ended: Ended,
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

/// Answer one delivery. `None` for anything that is not a call.
pub fn handle<H: Wire + Data>(host: &mut H, state: &mut State, delivery: &Value) -> Option<Value> {
    if delivery["type"] == "cancelled" {
        let ticket = delivery["ticket"].as_u64()?;
        let handle = state.incoming.iter()
            .find_map(|(&handle, incoming)| (incoming.ticket == ticket).then_some(handle));
        if let Some(handle) = handle
            && let Some(incoming) = state.incoming.remove(&handle)
        {
            // Cancellation ends only this peer's session; an expired ticket gets no late reply.
            let _ = state.complete_receive(host, handle, incoming, Err(answer::CANCELLED));
        }
        return None;
    }
    if delivery["type"] != "call" {
        return None;
    }
    let ticket = delivery["ticket"].as_u64()?;
    if delivery["method"] == "receive" {
        return match state.receive(host, ticket, &delivery["args"]) {
            Ok(()) => None, // The original ticket is answered when this stream actually ends.
            Err(reason) => Some(json!({"op":"reply","ticket":ticket,"result":outcome(Err(reason))})),
        };
    }
    if delivery["method"] == "view" || delivery["method"] == "layout" {
        if let Some(at) = delivery["args"]["event"]["at"].as_u64() {
            state.follow_pages(host, at);
        }
        let result = if delivery["method"] == "layout" {
            let mut ui = std::mem::take(&mut state.layout);
            let result = ui.event(state, host, &delivery["args"]["event"], delivery["args"]["page"].as_str());
            state.layout = ui;
            result
        } else {
            let mut ui = std::mem::take(&mut state.ui);
            let result = ui.event(state, host, &delivery["args"]["event"]);
            state.ui = ui;
            result
        };
        let result = match result {
            Ok(value) => json!({"status":"ok","value":value}),
            Err(reason) => json!({"status":"error","code":"failed","message":reason}),
        };
        return Some(json!({"op":"reply","ticket":ticket,"result":result}));
    }
    let result = match delivery["method"].as_str() {
        Some("input") => state.input(host, &delivery["args"]),
        _ => Err(answer::UNKNOWN),
    };
    Some(json!({"op":"reply","ticket":ticket,"result":outcome(result)}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::scripted::{Script, ack};

    /// Existing finite-stream cases check their eventual, deferred receive reply.
    fn handle<H: Wire + Data>(host: &mut H, state: &mut State, delivery: &Value) -> Option<Value> {
        let reply = super::handle(host, state, delivery);
        if reply.is_some() || delivery["method"] != "receive" {
            return reply;
        }
        while let Some(handle) = state.incoming.keys().next().copied() {
            if let Some(reply) = state.advance(host, handle) {
                return Some(reply);
            }
        }
        None
    }

    fn ok(value: Value) -> Value {
        json!({"status":"ok","value":value})
    }

    fn input_call(ticket: u64, events: Value, end: bool) -> Value {
        json!({"type":"call","ticket":ticket,"method":"input","args":{"peer":"pc","events":events,"end":end}})
    }

    #[test]
    fn layout_option_adds_or_withdraws_a_second_placement_of_the_same_page() {
        for enabled in [true, false] {
            let mut host = Script::default();
            let mut state = State::default();
            host.results.push_back(ok(json!({"settings":{"layout-page":enabled}})));
            host.results.push_back(ok(json!({})));
            state.configure_pages(&mut host).unwrap();
            let call = &host.sent[1];
            assert_eq!(call["service"], PAGES);
            assert_eq!(call["method"], "set");
            assert_eq!(call["args"]["pages"], if enabled {
                json!([{"id":"layout-page", "page":"layout", "placement":"primary"}])
            } else { json!([]) });
        }
    }

    #[test]
    fn a_changed_layout_option_is_placed_while_running_and_an_unchanged_one_is_not_set_again() {
        let mut host = Script::default();
        let mut state = State::default();
        host.results.push_back(ok(json!({"settings":{"layout-page":false}})));
        host.results.push_back(ok(json!({})));
        state.configure_pages(&mut host).unwrap();
        // Within two seconds nothing is asked; then the option, turned on meanwhile, is placed.
        state.follow_pages(&mut host, 1000);
        state.follow_pages(&mut host, 1500);
        assert_eq!(host.sent.len(), 3, "one look at the start of page time, none within the next two seconds");
        host.results.push_back(ok(json!({"settings":{"layout-page":true}})));
        host.results.push_back(ok(json!({})));
        state.follow_pages(&mut host, 3100);
        assert_eq!(host.sent.last().unwrap()["method"], "set");
        assert_eq!(host.sent.last().unwrap()["args"]["pages"][0]["placement"], "primary");
        // Unchanged: read, never set again.
        host.results.push_back(ok(json!({"settings":{"layout-page":true}})));
        let before = host.sent.len();
        state.follow_pages(&mut host, 5200);
        assert_eq!(host.sent.len(), before + 1);
        assert_eq!(host.sent.last().unwrap()["method"], "get");
    }

    #[test]
    fn leaving_after_rest_releases_the_previous_device_without_new_motion() {
        let mut host = Script { acks: true, ..Script::default() };
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim":1})));
        state.input(&mut host, &json!({"peer":"pc","events":[{"t":"move","dx":8,"dy":0}]})).unwrap();
        state.rest(&mut host).unwrap();
        let before = host.written.clone();
        host.results.push_back(ok(json!({"claim":2})));
        state.finish(&mut host).unwrap();
        assert_eq!(host.opened, vec![(true,1),(true,2)], "leaving must reach the rested target");
        assert_eq!(host.written, before, "release must not synthesize motion or clicks");
        assert_eq!((host.shutdowns,host.closed),(2,2));
        state.finish(&mut host).unwrap();
        assert_eq!(host.opened.len(),2,"a completed leave is idempotent");
    }

    #[test]
    fn ending_immediately_after_rotation_releases_the_retained_pointer() {
        let mut host = Script { acks: true, ..Script::default() };
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim":1})));
        state.input(&mut host, &json!({"peer":"pc","events":[{"t":"move","dx":8,"dy":0}]})).unwrap();
        state.rotate(&mut host).unwrap();
        host.results.push_back(ok(json!({"claim":2})));
        state.finish(&mut host).unwrap();
        assert_eq!(host.opened,vec![(true,1),(true,2)]);
        assert_eq!((host.shutdowns,host.closed),(2,2));
        assert_eq!(host.sent.last().unwrap()["args"]["device"],"pc");
    }

    #[test]
    fn batches_go_down_one_connection_as_lines_and_the_end_closes_it_after_the_acknowledgement() {
        let mut host = Script { acks: true, ..Script::default() };
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim": 42})));
        let first = json!([{"t":"move","dx":3,"dy":-1}]);
        let reply = handle(&mut host, &mut state, &input_call(1, first, false)).unwrap();
        assert_eq!(reply["result"]["status"], "ok");
        assert_eq!(host.opened, vec![(true, 42)]);
        assert_eq!(host.sent[0]["method"], "send");
        assert_eq!(host.sent[0]["args"]["service"], SERVICE);
        // The second batch reuses the open connection: no new `send`.
        let second = json!([{"t":"button","button":"left","down":true},{"t":"button","button":"left","down":false}]);
        handle(&mut host, &mut state, &input_call(2, second, true)).unwrap();
        assert_eq!(host.opened.len(), 1);
        let written = String::from_utf8(host.written.clone()).unwrap();
        let lines: Vec<Value> = written
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1]["button"], "left");
        assert_eq!((host.shutdowns, host.closed), (1, 1));
        assert!(state.outgoing.is_none());
    }

    #[test]
    fn a_connection_is_rotated_before_it_outlives_the_other_side_s_call() {
        let mut host = Script { acks: true, ..Script::default() };
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim": 1})));
        for i in 0..ROTATE_AFTER {
            let reply = handle(
                &mut host,
                &mut state,
                &input_call(u64::from(i) + 1, json!([{"t":"move","dx":1,"dy":0}]), false),
            )
            .unwrap();
            assert_eq!(reply["result"]["status"], "ok");
        }
        assert!(state.outgoing.is_none());
        assert_eq!(host.closed, 1);
        let written = String::from_utf8(host.written.clone()).unwrap();
        assert_eq!(
            written.lines().last(),
            Some(CONTINUES),
            "a rotation says the gesture goes on, so a held drag survives it"
        );
    }

    #[test]
    fn an_end_on_the_batch_that_would_rotate_releases_instead_of_continuing() {
        let mut host = Script { acks: true, ..Script::default() };
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim": 1})));
        for i in 0..ROTATE_AFTER {
            let last = i + 1 == ROTATE_AFTER;
            handle(&mut host, &mut state,
                &input_call(u64::from(i) + 1, json!([{"t":"move","dx":1,"dy":0}]), last)).unwrap();
        }
        let written = String::from_utf8(host.written.clone()).unwrap();
        assert_ne!(written.lines().last(), Some(CONTINUES), "the gesture ended: nothing may stay held");
        assert!(state.outgoing.is_none() && state.retained.is_none());
        assert_eq!((host.shutdowns, host.closed), (1, 1));
    }

    #[test]
    fn a_short_acknowledgement_is_not_a_delivery() {
        let mut host = Script::default();
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim": 1})));
        state.input(&mut host, &json!({"peer":"pc","events":[{"t":"move","dx":8,"dy":0}]})).unwrap();
        host.incoming.push_back(ack(0));
        assert_eq!(state.finish(&mut host), Err(answer::STOPPED));
        host.results.push_back(ok(json!({"claim": 2})));
        state.input(&mut host, &json!({"peer":"pc","events":[{"t":"move","dx":8,"dy":0}]})).unwrap();
        let sent = host.written.len() as u64 / 2;
        host.incoming.push_back(ack(sent));
        assert_eq!(state.finish(&mut host), Ok(()), "every byte of its own stream acknowledged");
    }

    #[test]
    fn a_rotated_connection_keeps_held_input_and_only_a_real_end_releases_it() {
        let run = |bytes: &[u8]| {
            let mut host = Script::default();
            let mut state = State::default();
            host.incoming.push_back(bytes.to_vec());
            host.results.push_back(ok(json!({"settings":{"independent":true}})));
            host.results.push_back(ok(json!({"applied":1})));
            host.results.push_back(ok(json!({"released":true})));
            let reply = handle(
                &mut host,
                &mut state,
                &json!({"type":"call","ticket":3,"method":"receive","args":{"peer":"rog"}}),
            )
            .unwrap();
            assert_eq!(reply["result"]["status"], "ok", "{reply}");
            host.sent
                .iter()
                .find(|m| m["method"] == "release")
                .map(|m| m["args"]["keep"] == true)
        };
        let held = b"{\"t\":\"button\",\"button\":\"left\",\"down\":true}\n";
        let mut rotated = held.to_vec();
        rotated.extend_from_slice(CONTINUES.as_bytes());
        rotated.push(b'\n');
        assert_eq!(run(&rotated), None, "a rotation must not let go of the held button");
        assert_eq!(run(held), Some(false), "a connection that ends without a word lets go of all");
        let mut rested = held.to_vec();
        rested.extend_from_slice(RESTS.as_bytes());
        rested.push(b'\n');
        assert_eq!(run(&rested), Some(true), "a gesture that ended lets go but keeps the pointer");
        let mut stale = rotated.clone();
        stale.extend_from_slice(b"{\"t\":\"move\",\"dx\":1,\"dy\":0}\n");
        assert_eq!(run(&stale), Some(false), "the marker counts only as the last line");
    }

    #[test]
    fn a_device_that_stopped_listening_is_reported_and_the_next_batch_reconnects() {
        struct Broken(Script);
        impl Wire for Broken {
            fn receive(&mut self) -> Result<Value, &'static str> {
                self.0.receive()
            }
            fn send(&mut self, value: Value) -> Result<(), &'static str> {
                self.0.send(value)
            }
        }
        impl Data for Broken {
            fn open(&mut self, claim: bool, number: u64) -> Result<Handle, &'static str> {
                self.0.open(claim, number)
            }
            fn read(&mut self, h: Handle, into: &mut [u8]) -> Result<usize, &'static str> {
                self.0.read(h, into)
            }
            fn write(&mut self, _: Handle, _: &[u8]) -> Result<usize, &'static str> {
                Err("gone")
            }
            fn shutdown(&mut self, h: Handle) -> Result<(), &'static str> {
                self.0.shutdown(h)
            }
            fn close(&mut self, h: Handle) {
                self.0.close(h)
            }
            fn poll(&mut self, handles: &[Handle], timeout_ms: i32) -> Result<Option<Handle>, &'static str> {
                self.0.poll(handles, timeout_ms)
            }
        }
        let mut host = Broken(Script::default());
        let mut state = State::default();
        host.0.results.push_back(ok(json!({"claim": 5})));
        let reply = handle(
            &mut host,
            &mut state,
            &input_call(1, json!([{"t":"move","dx":1,"dy":0}]), false),
        )
        .unwrap();
        assert_eq!(reply["result"]["message"], answer::STOPPED);
        assert!(state.outgoing.is_none());
        assert_eq!(host.0.closed, 1);
    }

    #[test]
    fn received_lines_are_replayed_as_they_arrive_split_anywhere_and_everything_is_released_at_the_end()
     {
        let mut host = Script::default();
        let mut state = State::default();
        // The bytes arrive cut in the middle of a line: only complete lines are replayed.
        host.incoming.push_back(
            br#"{"t":"move","dx":1,"dy":2}
{"t":"butt"#
                .to_vec(),
        );
        host.incoming.push_back(
            br#"on","button":"left","down":true}
"#
            .to_vec(),
        );
        host.results.push_back(ok(
            json!({"plugin":"control","settings":{"independent":true},"peers":[]}),
        ));
        for _ in 0..3 {
            host.results.push_back(ok(json!({"applied":1})));
        }
        let reply = handle(
            &mut host,
            &mut state,
            &json!({"type":"call","ticket":9,"method":"receive","args":{"peer":"rog"}}),
        )
        .unwrap();
        assert_eq!(reply["result"]["status"], "ok", "{reply}");
        assert_eq!(host.opened, vec![(false, 9)]);
        let calls: Vec<&Value> = host.sent.iter().filter(|m| m["op"] == "call").collect();
        assert_eq!(calls[0]["service"], CONFIG);
        assert_eq!(calls[1]["service"], INPUT);
        assert_eq!(calls[1]["args"]["events"][0]["t"], "move");
        assert_eq!(calls[1]["args"]["owner"], "rog");
        assert_eq!(calls[1]["args"]["pointer"], "own", "a pointer of its own");
        assert_eq!(calls[1]["args"]["show_name"], true, "its name is asked for by default");
        assert!(calls[1]["args"].get("label").is_none(), "the extension never writes the name itself");
        assert_eq!(calls[2]["args"]["events"][0]["button"], "left");
        assert_eq!(calls[3]["method"], "release", "nothing is left held down");
        assert_eq!(
            calls[3]["args"]["owner"], "rog",
            "only what this device held"
        );
        assert_eq!(host.closed, 1);
    }

    #[test]
    fn with_independent_pointers_off_the_shared_pointer_moves() {
        let mut host = Script::default();
        let mut state = State::default();
        host.incoming
            .push_back(b"{\"t\":\"move\",\"dx\":1,\"dy\":2}\n".to_vec());
        host.results.push_back(ok(
            json!({"plugin":"control","settings":{"independent":false},"peers":[]}),
        ));
        host.results.push_back(ok(json!({"applied":1})));
        host.results.push_back(ok(json!({"released":true})));
        handle(
            &mut host,
            &mut state,
            &json!({"type":"call","ticket":1,"method":"receive","args":{"peer":"rog"}}),
        )
        .unwrap();
        let inject = host.sent.iter().find(|m| m["method"] == "inject").unwrap();
        assert!(inject["args"].get("pointer").is_none());
        assert!(inject["args"].get("show_name").is_none(), "the shared pointer carries no name");
        assert_eq!(inject["args"]["owner"], "rog");
    }

    #[test]
    fn the_name_tag_can_be_turned_off() {
        let mut host = Script::default();
        let mut state = State::default();
        host.incoming
            .push_back(b"{\"t\":\"move\",\"dx\":1,\"dy\":2}\n".to_vec());
        host.results.push_back(ok(
            json!({"plugin":"control","settings":{"independent":true,"name-tag":false},"peers":[]}),
        ));
        host.results.push_back(ok(json!({"applied":1})));
        host.results.push_back(ok(json!({"released":true})));
        handle(
            &mut host,
            &mut state,
            &json!({"type":"call","ticket":1,"method":"receive","args":{"peer":"rog"}}),
        )
        .unwrap();
        let inject = host.sent.iter().find(|m| m["method"] == "inject").unwrap();
        assert_eq!(inject["args"]["pointer"], "own");
        assert!(inject["args"].get("show_name").is_none());
    }

    #[test]
    fn something_that_is_not_an_input_ends_the_connection_and_still_releases() {
        for garbage in [
            b"not json\n".to_vec(),
            b"[1,2]\n".to_vec(),
            vec![b'x'; MOST_LINE + 2],
            br#"{"t":"move"}"#.to_vec(),
        ] {
            let mut host = Script::default();
            let mut state = State::default();
            host.incoming.push_back(garbage);
            host.results.push_back(ok(json!({"settings":{}})));
            host.results.push_back(ok(json!({"released":true})));
            let reply = handle(
                &mut host,
                &mut state,
                &json!({"type":"call","ticket":3,"method":"receive","args":{"peer":"rog"}}),
            )
            .unwrap();
            assert_eq!(reply["result"]["message"], answer::NOT_UNDERSTOOD);
            assert_eq!(host.sent.last().unwrap()["method"], "release");
        }
    }
}

#[cfg(test)]
mod multiplex_tests {
    use super::*;
    use crate::host::scripted::Script;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Concurrent {
        wire: Script,
        streams: BTreeMap<Handle, VecDeque<Vec<u8>>>,
        ready: VecDeque<Handle>,
        closed: Vec<Handle>,
    }
    impl Wire for Concurrent {
        fn receive(&mut self) -> Result<Value, &'static str> { self.wire.receive() }
        fn send(&mut self, value: Value) -> Result<(), &'static str> { self.wire.send(value) }
    }
    impl Data for Concurrent {
        fn open(&mut self, _: bool, ticket: u64) -> Result<Handle, &'static str> {
            Ok(i32::try_from(ticket).unwrap())
        }
        fn read(&mut self, handle: Handle, into: &mut [u8]) -> Result<usize, &'static str> {
            let bytes = self.streams.get_mut(&handle).unwrap().pop_front()
                .ok_or("an idle controller must never be read")?;
            assert!(bytes.len() <= into.len());
            into[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }
        fn write(&mut self, _: Handle, _: &[u8]) -> Result<usize, &'static str> { unreachable!() }
        fn shutdown(&mut self, _: Handle) -> Result<(), &'static str> { unreachable!() }
        fn close(&mut self, handle: Handle) { self.closed.push(handle); }
        fn poll(&mut self, handles: &[Handle], _: i32) -> Result<Option<Handle>, &'static str> {
            let ready = self.ready.pop_front();
            if let Some(handle) = ready { assert!(handles.contains(&handle)); }
            Ok(ready)
        }
    }

    #[test]
    fn second_controller_moves_before_first_eof_and_each_keeps_its_partial_line_and_owner() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        for ticket in [1, 2] {
            host.wire.results.push_back(json!({"status":"ok","value":{"settings":{"independent":true}}}));
            assert!(super::handle(&mut host, &mut state,
                &json!({"type":"call","ticket":ticket,"method":"receive","args":{"peer":format!("controller-{ticket}")}})).is_none());
        }
        host.streams.insert(1, VecDeque::from([b"{\"t\":\"move\",\"dx\":".to_vec(), b"1,\"dy\":0}\n".to_vec(), Vec::new()]));
        host.streams.insert(2, VecDeque::from([b"{\"t\":\"move\",\"dx\":2,\"dy\":0}\n".to_vec()]));
        host.ready.extend([1, 2, 1, 1]);
        for _ in 0..3 {
            host.wire.results.push_back(json!({"status":"ok","value":{}}));
        }
        assert!(state.next(&mut host).unwrap().is_none());
        assert!(state.next(&mut host).unwrap().is_none());
        let injects: Vec<_> = host.wire.sent.iter().filter(|m| m["method"] == "inject").collect();
        assert_eq!(injects.len(), 1);
        assert_eq!(injects[0]["args"]["owner"], "controller-2");
        assert_eq!(injects[0]["args"]["events"][0]["dx"], 2);
        assert_eq!(injects[0]["args"]["pointer"], "own");
        assert!(host.closed.is_empty(), "second controller must not wait for first EOF");
        state.next(&mut host).unwrap();
        let injects: Vec<_> = host.wire.sent.iter().filter(|m| m["method"] == "inject").collect();
        assert_eq!(injects[1]["args"]["owner"], "controller-1");
        assert_eq!(injects[1]["args"]["events"][0]["dx"], 1);
        state.next(&mut host).unwrap();
        assert_eq!(host.closed, [1]);
        let release = host.wire.sent.iter().find(|m| m["method"] == "release").unwrap();
        assert_eq!(release["args"]["owner"], "controller-1");
        assert_eq!(release["args"]["keep"], false);
        assert!(state.incoming.contains_key(&2), "one peer ending must not close the other");
        let reply = host.wire.sent.iter().find(|m| m["op"] == "reply").unwrap();
        assert_eq!(reply["ticket"], 1);
        assert_eq!(reply["result"]["status"], "ok");
        host.wire.results.push_back(json!({"status":"ok","value":{}}));
        let replies = host.wire.sent.iter().filter(|m| m["op"] == "reply").count();
        assert!(super::handle(&mut host, &mut state,
            &json!({"type":"cancelled","ticket":2})).is_none());
        assert_eq!(host.closed, [1, 2]);
        assert!(state.incoming.is_empty());
        let release = host.wire.sent.iter().rev().find(|m| m["method"] == "release").unwrap();
        assert_eq!(release["args"]["owner"], "controller-2");
        assert_eq!(host.wire.sent.iter().filter(|m| m["op"] == "reply").count(), replies,
            "cancelled tickets must not receive a late reply");
    }
}
