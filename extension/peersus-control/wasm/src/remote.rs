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
use crate::{
    State,
    host::{
        Blocked, CONFIG, Data, DeliveryAck, Handle, Permission, Wire, config_changed, read_ack,
        write_all,
    },
};
use serde_json::{Value, json};

/// The application's own services this extension calls.
const PEERS: &str = "peersus.peers";
const INPUT: &str = "peersus.input";
const DISPLAYS: &str = "peersus.displays";
const PAGES: &str = "peersus.pages";
/// The service the other device runs for this extension, and its version.
pub const SERVICE: &str = "control.pointer";
const VERSION: u32 = 1;
const CALL_MS: u32 = 4_000;
/// Broker admission refusals have not replayed anything. The four retries and final
/// release allow at most 655 ms of guest polling, even if control interrupts each wait.
const REPLAY_BACKOFF_MS: [i32; 4] = [10, 15, 25, 35];
const REPLAY_CALL_MS: u32 = 30;
const REPLAY_REPLY_MS: i32 = 40;
const BACKOFF_WAITS: usize = 3;
const REPLAY_BUSY: &str = "The input broker is busy";
const NO_REPLAY_REPLY: &str = "The input broker did not reply";
/// Longest wait for the acknowledgement of a connection that was closed. The application gives a
/// silent connection thirty seconds; this guest draws the pages and replays every other device's
/// input in between, so it stops waiting long before, and that delivery counts as not made.
const ACK_MS: i32 = 3_000;
/// Longest line one input may take on the connection: an input is small, a longer line is not one.
pub const MOST_LINE: usize = 1024;
/// Most characters one typed input carries: the application's input service refuses more.
const MOST_TYPED: usize = 256;
/// Most inputs replayed in one call to the application: its own bound.
const MOST_BATCH: usize = 64;
/// Both deferred deliveries and any readiness look-ahead stay within this bound.
const MOST_WAITING: usize = 64;
/// There can be at most 256 paired devices; bound detached handoffs and remote refusals.
const MOST_DEVICES: usize = 256;
/// A connection is closed and a new one opened after this many batches, so no one connection
/// outlives the time the other side gives a call.
pub const ROTATE_AFTER: u32 = 200;
/// ...or once it is this old by the application's clock. A held key may send nothing for
/// longer than the fifteen seconds the other side gives its receive call.
pub const ROTATE_MS: u64 = 5_000;
/// The last line of a rotated connection: the same gesture goes on in the next one, so what the
/// receiver holds (a pressed button, the pen near the screen) must not be let go between them.
pub const CONTINUES: &str = r#"{"continues":true}"#;
/// The last line of a connection closed because a gesture ended: what was pressed is let go, but
/// the pointer stays where it is (the controlled computer hides it once it has been still). The
/// connection is not kept open between gestures.
pub const RESTS: &str = r#"{"rests":true}"#;

fn host_guard(service: &str, method: &str) -> Option<&'static str> {
    match (service, method) {
        (PEERS, "list") => Some("peers.watch"),
        (INPUT, "inspect" | "inject" | "release") => Some("input.inject"),
        (DISPLAYS, "inspect") => Some("displays.watch"),
        (DISPLAYS, "arrange") => Some("displays.arrange"),
        _ => None,
    }
}

/// Text as typed inputs, in order, each within [`MOST_TYPED`] characters and, as one line, within
/// [`MOST_LINE`] bytes, so text a field accepts is never refused whole by either bound.
pub fn typed(text: &str) -> Vec<Value> {
    // `{"t":"text","text":""}`: what a typed input costs around its text.
    const FRAME: usize = 22;
    // What the text costs as JSON: serde_json escapes quotes, backslashes and control characters.
    let cost = |character: char| match character {
        '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
        character if u32::from(character) < 0x20 => 6,
        character => character.len_utf8(),
    };
    let mut inputs = Vec::new();
    let mut chunk = String::new();
    let (mut characters, mut bytes) = (0, FRAME);
    for character in text.chars() {
        if characters == MOST_TYPED || bytes + cost(character) > MOST_LINE {
            inputs.push(json!({"t":"text","text":std::mem::take(&mut chunk)}));
            (characters, bytes) = (0, FRAME);
        }
        chunk.push(character);
        characters += 1;
        bytes += cost(character);
    }
    if !chunk.is_empty() {
        inputs.push(json!({"t":"text","text":chunk}));
    }
    inputs
}

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
    /// The person did not allow what was asked (`denied` from the application).
    pub const DENIED: &str = "Not allowed";
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Other(&'static str),
    Blocked(Blocked),
}

impl From<&'static str> for Refusal {
    fn from(reason: &'static str) -> Self {
        Self::Other(reason)
    }
}

impl From<Blocked> for Refusal {
    fn from(blocked: Blocked) -> Self {
        Self::Blocked(blocked)
    }
}

impl Refusal {
    fn from_result(result: &Value) -> Self {
        if let Some(blocked) = Blocked::read(result) {
            return blocked.into();
        }
        match result["code"].as_str() {
            Some("cancelled") => answer::CANCELLED,
            Some("denied") => answer::DENIED,
            _ => answer::REFUSED,
        }
        .into()
    }
}

fn diagnostic_outcome(result: &Result<(), Refusal>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(Refusal::Blocked(_)) => 1,
        Err(Refusal::Other(reason)) => match *reason {
            answer::REFUSED | answer::DENIED => 1,
            answer::CANCELLED => 2,
            answer::NOT_UNDERSTOOD => 3,
            answer::STOPPED => 4,
            answer::NOT_REPLAYED => 5,
            _ => 8,
        },
    }
}

/// The connection kept open to the device being controlled.
pub struct Outgoing {
    pub device: String,
    pub handle: Handle,
    pub batches: u32,
    /// Every byte written on this connection, end marker included: what its acknowledgement must count.
    pub sent: u64,
    /// The first forwarded desktop input sample on the owner's clock.
    opened_at: Option<u64>,
    /// The most recent sample on that same clock.
    latest_at: u64,
}

impl Outgoing {
    fn record_time(&mut self, at: u64) {
        self.opened_at.get_or_insert(at);
        self.latest_at = self.latest_at.max(at);
    }

    fn rotation_wait_ms(&self) -> i32 {
        if self.batches >= ROTATE_AFTER {
            return 0;
        }
        let age = self.opened_at.map_or(0, |opened| self.latest_at - opened);
        ROTATE_MS.saturating_sub(age) as i32
    }

    fn forwarded_args(delivery: &Value) -> Option<&Value> {
        (delivery["type"] == "call" && delivery["method"] == "input").then_some(&delivery["args"])
    }

    /// An explicit end or switch can release on this stream, without a new connection.
    fn finishes_on(&self, delivery: &Value) -> bool {
        let Some(args) = Self::forwarded_args(delivery) else {
            return false;
        };
        let Some(peer) = args["peer"].as_str().filter(|_| args["events"].is_array()) else {
            return false;
        };
        peer != self.device || args["end"] == true
    }

    /// A batch from the same forwarding source measures the wait; other readiness does not.
    fn measure_delivery(&mut self, delivery: &Value) -> bool {
        if let Some(args) = Self::forwarded_args(delivery)
            && args["peer"].as_str() == Some(self.device.as_str())
            && let Some(at) = args["at"].as_u64().filter(|&at| at >= self.latest_at)
        {
            self.record_time(at);
            return true;
        }
        false
    }
}

impl State {
    /// Only the guest interprets these options; the host receives ordinary additional placements. A page is among the
    /// application's pages only while its option is on (`remote` exists only on a phone: the copy keeps its platforms).
    pub(crate) fn configure_pages<H: Wire>(&mut self, host: &mut H) -> Result<(), Refusal> {
        const OPTIONS: [(&str, &str); 2] = [("layout-page", "layout"), ("remote-page", "remote")];
        let config = self.call(host, CONFIG, "get", json!({}))?;
        self.readiness.update(&config)?;
        let wanted: Vec<&'static str> = OPTIONS
            .iter()
            .filter(|(option, _)| config["settings"][*option].as_bool().unwrap_or(false))
            .map(|(option, _)| *option)
            .collect();
        self.ui.permission_changed();
        self.layout.permission_changed();
        if self.placed_pages.as_ref() == Some(&wanted) {
            return Ok(());
        }
        let pages: Vec<Value> = OPTIONS
            .iter()
            .filter(|(option, _)| wanted.contains(option))
            .map(|(option, page)| json!({"id":option, "page":page, "placement":"primary"}))
            .collect();
        self.call(host, PAGES, "set", json!({ "pages": pages }))?;
        self.placed_pages = Some(wanted);
        Ok(())
    }

    pub(crate) fn missing_host(&self, name: &'static str) -> Option<Permission> {
        self.readiness.missing(None, "host", name)
    }

    /// The visible, caller-scoped grant set says nothing about the remote device's policy.
    pub(crate) fn send_block(&self, device: &str) -> Option<Permission> {
        self.readiness
            .missing(Some(device), "host", "peers.send")
            .or_else(|| {
                self.readiness
                    .missing(Some(device), self.readiness.owner()?, "control.use")
            })
    }

    pub(crate) fn remote_block(&self, device: &str) -> bool {
        self.remote_denials.contains(device)
    }

    pub(crate) fn local_block(&self, device: &str) -> bool {
        self.local_stops.contains(device)
    }

    fn remember_local_stop(&mut self, device: &str) {
        if self.local_stops.len() < MOST_DEVICES {
            self.local_stops.insert(device.to_owned());
        }
    }

    /// An explicit new choice may start a new operation; ticks and ordinary samples cannot.
    pub(crate) fn allow_new_choice(&mut self, device: &str) {
        self.remote_denials.remove(device);
        self.local_stops.remove(device);
        if self
            .early_end
            .as_ref()
            .is_some_and(|(peer, _)| peer == device)
        {
            self.early_end = None;
        }
    }

    fn remember_remote_no(&mut self, device: &str) {
        if self.remote_denials.len() < MOST_DEVICES {
            self.remote_denials.insert(device.to_owned());
        }
        if self.retained.as_deref() == Some(device) {
            self.retained = None;
        }
    }

    /// Stop this device locally, including any retained gesture. Do not open a release stream.
    pub(crate) fn stop_sending<H: Data>(&mut self, host: &mut H, device: &str) {
        if self
            .outgoing
            .as_ref()
            .is_some_and(|out| out.device == device)
            && let Some(out) = self.outgoing.take()
        {
            host.close(out.handle);
        }
        if self.retained.as_deref() == Some(device) {
            self.retained = None;
        }
        if self
            .early_end
            .as_ref()
            .is_some_and(|(peer, _)| peer == device)
        {
            self.early_end = None;
        }
    }

    fn on_config_changed<H: Wire + Data>(&mut self, host: &mut H) {
        self.pages_failed = self.configure_pages(host).is_err();
        if let Some(device) = self
            .outgoing
            .as_ref()
            .map(|out| &out.device)
            .or(self.retained.as_ref())
            && self.send_block(device).is_some()
        {
            let device = device.clone();
            self.remember_local_stop(&device);
            self.stop_sending(host, &device);
        }
        self.stop_revoked_receives(host);
    }

    fn receive_block(&self, device: &str) -> Option<Blocked> {
        if let Some(permission) = self.missing_host("input.inject") {
            return Some(Blocked::MissingPermission(permission));
        }
        self.readiness
            .missing(Some(device), self.readiness.owner()?, "control.use")
            .map(|_| Blocked::NotAccepted)
    }

    fn stop_revoked_receives<H: Wire + Data>(&mut self, host: &mut H) {
        let revoked: Vec<_> = self
            .incoming
            .iter()
            .filter_map(|(&handle, incoming)| {
                Some((handle, self.receive_block(&incoming.target.owner)?))
            })
            .collect();
        if self.missing_host("input.inject").is_some() {
            self.handoffs.clear();
        } else if let Some(owner) = self.readiness.owner() {
            self.handoffs.retain(|device| {
                self.readiness
                    .missing(Some(device), owner, "control.use")
                    .is_none()
            });
        }
        for (handle, reason) in revoked {
            if let Some(incoming) = self.incoming.remove(&handle) {
                let cancelled = self.waiting.iter().any(|delivery| {
                    delivery["type"] == "cancelled" && delivery["ticket"] == incoming.ticket
                });
                let reply = self.complete_receive(host, handle, incoming, Err(reason.into()));
                if !cancelled {
                    let _ = host.send(reply);
                }
            }
        }
    }

    /// Ask the application for something and wait for exactly that answer, keeping anything else
    /// that arrives meanwhile for later.
    pub(crate) fn call<H: Wire>(
        &mut self,
        host: &mut H,
        service: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, Refusal> {
        self.call_for(host, service, method, args, CALL_MS, Wire::receive)
    }

    fn call_for<H: Wire>(
        &mut self,
        host: &mut H,
        service: &str,
        method: &str,
        args: Value,
        timeout_ms: u32,
        mut receive: impl FnMut(&mut H) -> Result<Value, &'static str>,
    ) -> Result<Value, Refusal> {
        let guard = host_guard(service, method);
        if let Some(permission) = guard.and_then(|name| self.missing_host(name)) {
            return Err(Blocked::MissingPermission(permission).into());
        }
        self.next = self.next.checked_add(1).ok_or(answer::REFUSED)?;
        let id = self.next;
        host.send(json!({
            "op":"call","id":id,"provider":"host","service":service,"version":1,
            "method":method,"args":args,"timeout_ms":timeout_ms
        }))?;
        loop {
            let message = receive(host)?;
            if message["type"] == "result" && message["id"].as_u64() == Some(id) {
                let result = &message["result"];
                crate::host::diagnostic(
                    2,
                    if result["status"] == "ok" {
                        0
                    } else if result["code"] == "cancelled" {
                        2
                    } else {
                        1
                    },
                    id,
                    0,
                    0,
                );
                return if result["status"] == "ok" {
                    Ok(result["value"].clone())
                } else {
                    if (service, method) == (INPUT, "inject")
                        && result["status"] == "error"
                        && matches!(result["code"].as_str(), Some("rate-limited" | "busy"))
                    {
                        return Err(REPLAY_BUSY.into());
                    }
                    let refusal = Refusal::from_result(result);
                    if let Some(name) = guard
                        && let Refusal::Blocked(Blocked::MissingPermission(permission)) = &refusal
                        && permission.is("host", name)
                    {
                        self.readiness.deny(None, permission.clone());
                    }
                    Err(refusal)
                };
            }
            self.keep(message)?;
        }
    }

    fn replay_call<H: Wire + Data>(
        &mut self,
        host: &mut H,
        method: &str,
        args: Value,
    ) -> Result<Value, Refusal> {
        let mut waits = 0;
        self.call_for(host, INPUT, method, args, REPLAY_CALL_MS, |host| {
            // The broker deadline alone does not bound a reply: the transport may
            // still be throttling reads and not have admitted this request yet.
            if waits == 2 {
                return Err(NO_REPLAY_REPLY);
            }
            waits += 1;
            if host.poll(&[], REPLAY_REPLY_MS)? == Some(0) {
                host.receive()
            } else {
                Err(NO_REPLAY_REPLY)
            }
        })
    }

    /// Put aside what arrived while something else was awaited, to be handled afterwards. Anything
    /// that is not a delivery is dropped; one delivery too many is refused.
    fn keep(&mut self, message: Value) -> Result<(), &'static str> {
        match message["type"].as_str() {
            Some("call") | Some("cancelled") | Some("event")
                if self.waiting.len() < MOST_WAITING =>
            {
                self.waiting.push(message);
                Ok(())
            }
            Some("call") | Some("cancelled") | Some("event") => Err(answer::REFUSED),
            _ => Ok(()),
        }
    }

    fn connect<H: Wire + Data>(&mut self, host: &mut H, device: &str) -> Result<Outgoing, Refusal> {
        if let Some(permission) = self.send_block(device) {
            return Err(Blocked::MissingPermission(permission).into());
        }
        if self.remote_block(device) {
            return Err(Blocked::NotAccepted.into());
        }
        let sent = self.call(
            host,
            PEERS,
            "send",
            json!({"device":device,"service":SERVICE,"version":VERSION}),
        );
        match &sent {
            Err(Refusal::Blocked(Blocked::MissingPermission(permission))) => {
                self.readiness.deny(Some(device), permission.clone());
            }
            Err(Refusal::Blocked(Blocked::NotAccepted)) => self.remember_remote_no(device),
            _ => {}
        }
        let sent = sent?;
        let claim = sent["claim"].as_u64().ok_or(answer::REFUSED)?;
        let handle = host.open(true, claim).map_err(|_| answer::REFUSED)?;
        crate::host::diagnostic(3, 0, handle as u64, 0, 0);
        Ok(Outgoing {
            device: device.to_owned(),
            handle,
            batches: 0,
            sent: 0,
            opened_at: None,
            latest_at: 0,
        })
    }

    /// Wait until the acknowledgement of a closed connection can be read, for [`ACK_MS`] at most
    /// since anything last arrived. Deliveries that come first are kept, as [`Self::call`] keeps them.
    fn acknowledged<H: Wire + Data>(
        &mut self,
        host: &mut H,
        handle: Handle,
    ) -> Result<(), &'static str> {
        loop {
            match host.poll(&[handle], ACK_MS)? {
                Some(ready) if ready == handle => return Ok(()),
                Some(_) => {
                    let message = host.receive()?;
                    self.keep(message)?;
                }
                None => return Err(answer::STOPPED),
            }
        }
    }

    fn close<H: Wire + Data>(&mut self, host: &mut H, outgoing: Outgoing) -> Result<(), Refusal> {
        let written = host.shutdown(outgoing.handle).is_ok();
        self.settle(host, outgoing, written, false)
    }

    /// A refusal can arrive before shutdown or after an unsuccessful write. The host keeps the
    /// reverse half available for the same bounded acknowledgement; no send is tried again.
    fn settle<H: Wire + Data>(
        &mut self,
        host: &mut H,
        outgoing: Outgoing,
        written: bool,
        ready: bool,
    ) -> Result<(), Refusal> {
        let readable = if ready {
            Ok(())
        } else {
            self.acknowledged(host, outgoing.handle)
        };
        let acknowledgement = readable.and_then(|()| read_ack(host, outgoing.handle));
        let (actual, code, result) = match acknowledgement {
            Ok(DeliveryAck::Delivered(count)) if written && count == outgoing.sent => {
                (count, 0, Ok(()))
            }
            Ok(DeliveryAck::Delivered(count)) => (count, 6, Err(answer::STOPPED.into())),
            Ok(DeliveryAck::NotAccepted) => {
                self.remember_remote_no(&outgoing.device);
                (0, 1, Err(Blocked::NotAccepted.into()))
            }
            Err(_) => (0, 4, Err(answer::STOPPED.into())),
        };
        crate::host::diagnostic(4, code, outgoing.handle as u64, actual, outgoing.sent);
        host.close(outgoing.handle);
        result
    }

    /// Leaving also releases a pointer retained by an already closed, rested or rotated stream. A
    /// stream opened only to say so carries nothing and is not waited on: the other side reads the
    /// end of it when it reads, and this side, which runs everything else of the page, must not
    /// stand still until then.
    pub fn finish<H: Wire + Data>(&mut self, host: &mut H) -> Result<(), Refusal> {
        let early_end = self.early_end.take();
        // The device is forgotten before the network cleanup: an end or a release that cannot
        // reach it is reported once, never retried against the same stale identity, so a failed
        // leave cannot poison later ends or an explicit switch to another device.
        let retained = self.retained.take();
        crate::host::diagnostic(
            9,
            0,
            0,
            u64::from(self.outgoing.is_some()),
            u64::from(retained.is_some()),
        );
        if let Some(outgoing) = self.outgoing.take() {
            return self.close(host, outgoing);
        }
        let Some(device) = retained else {
            return early_end.map_or(Ok(()), |(_, reason)| Err(reason));
        };
        // Best effort only: a device that cannot be reached keeps whatever pointer the rested
        // stream left until it reads a real stream again; it is not asked a second time.
        if let Ok(outgoing) = self.connect(host, &device) {
            let _ = host.shutdown(outgoing.handle);
            host.close(outgoing.handle);
        }
        Ok(())
    }

    fn pause<H: Wire + Data>(&mut self, host: &mut H, marker: &str) -> Result<(), Refusal> {
        let Some(mut outgoing) = self.outgoing.take() else {
            return Ok(());
        };
        self.retained = Some(outgoing.device.clone());
        let mut line = marker.as_bytes().to_vec();
        line.push(b'\n');
        if write_all(host, outgoing.handle, &line).is_err() {
            return self.settle(host, outgoing, false, false);
        }
        outgoing.sent += line.len() as u64;
        self.close(host, outgoing)
    }

    /// Release held input between gestures without losing the device that still draws the pointer.
    pub fn rest<H: Wire + Data>(&mut self, host: &mut H) -> Result<(), Refusal> {
        self.pause(host, RESTS)
    }

    /// Rotate without releasing a gesture; a later explicit end still reaches the same device.
    pub fn rotate<H: Wire + Data>(&mut self, host: &mut H) -> Result<(), Refusal> {
        self.pause(host, CONTINUES)
    }

    /// `input`: write a batch to the device being controlled, opening a connection when there is none.
    pub fn input<H: Wire + Data>(&mut self, host: &mut H, args: &Value) -> Result<(), Refusal> {
        let device = args["peer"].as_str().ok_or(answer::REFUSED)?.to_owned();
        let events = args["events"].as_array().ok_or(answer::NOT_UNDERSTOOD)?;
        let end = args["end"].as_bool().unwrap_or(false);
        if let Some(stop) = args.get("stop") {
            if !end || !events.is_empty() || args.get("begin").is_some() {
                return Err(answer::NOT_UNDERSTOOD.into());
            }
            match Blocked::read(stop).ok_or(answer::NOT_UNDERSTOOD)? {
                Blocked::NotAccepted => self.remember_remote_no(&device),
                Blocked::MissingPermission(_) => self.remember_local_stop(&device),
            }
            self.stop_sending(host, &device);
            return Ok(());
        }
        let begin = args.get("begin");
        if begin.is_some_and(|begin| begin != true || end || events.is_empty()) {
            return Err(answer::NOT_UNDERSTOOD.into());
        }
        if begin.is_none()
            && self
                .early_end
                .as_ref()
                .is_some_and(|(peer, _)| peer == &device)
            && let Some((_, reason)) = self.early_end.take()
        {
            return Err(reason);
        }
        if let Some(permission) = self.send_block(&device) {
            self.stop_sending(host, &device);
            return if events.is_empty() && end {
                Ok(())
            } else {
                Err(Blocked::MissingPermission(permission).into())
            };
        }
        if begin.is_none() && (self.remote_block(&device) || self.local_block(&device)) {
            // A capture's explicit end is local bookkeeping. It cannot open another stream.
            return if events.is_empty() && end {
                Ok(())
            } else {
                Err(Blocked::NotAccepted.into())
            };
        }
        // Another device chosen: what was open to the first ends first.
        if self.outgoing.as_ref().is_some_and(|o| o.device != device)
            || self
                .retained
                .as_ref()
                .is_some_and(|previous| previous != &device)
        {
            let _ = self.finish(host);
        }
        if !events.is_empty() {
            let mut lines = Vec::new();
            for event in events {
                let line = serde_json::to_vec(event).map_err(|_| answer::NOT_UNDERSTOOD)?;
                if line.len() > MOST_LINE || !event.is_object() {
                    return Err(answer::NOT_UNDERSTOOD.into());
                }
                lines.extend_from_slice(&line);
                lines.push(b'\n');
            }
            if begin.is_some() {
                // Only a valid, owner-marked new crossing can replace the stopped visit.
                self.allow_new_choice(&device);
                // A new paired witness cannot keep using an older copy, even for
                // the same device when the preceding end was cancelled by the owner.
                self.stop_sending(host, &device);
            }
            if self.outgoing.is_none() {
                let result = self.connect(host, &device);
                if matches!(&result, Err(Refusal::Blocked(_))) {
                    self.stop_sending(host, &device);
                }
                self.outgoing = Some(result?);
                self.retained = None;
            }
            let outgoing = self.outgoing.as_mut().ok_or(answer::REFUSED)?;
            if write_all(host, outgoing.handle, &lines).is_err() {
                let gone = self.outgoing.take().ok_or(answer::REFUSED)?;
                return self.settle(host, gone, false, false);
            }
            outgoing.sent += lines.len() as u64;
            outgoing.batches += 1;
            if let Some(at) = args["at"].as_u64() {
                outgoing.record_time(at);
            }
            if outgoing.opened_at.is_none() && outgoing.batches >= ROTATE_AFTER && !end {
                // A page gesture has no forwarding clock. For forwarded input, `next` checks
                // ready ends before acting on the time or batch limit.
                return self.rotate(host);
            }
        }
        if end {
            self.finish(host)?;
        }
        Ok(())
    }

    /// Whether the application can draw a pointer of this device's own: `peersus.input`'s word,
    /// asked once per received connection and never guessed from the device or its system. A host
    /// without the method, or a reply without the flag, refuses the receive: no invented pointer.
    fn independent_pointer<H: Wire>(&mut self, host: &mut H) -> Result<bool, Refusal> {
        self.call(host, INPUT, "inspect", json!({}))?["independent_pointer"]
            .as_bool()
            .ok_or_else(|| answer::REFUSED.into())
    }

    /// Accept a receiver without reading it here: every peer must get a turn before EOF.
    pub fn receive<H: Wire + Data>(
        &mut self,
        host: &mut H,
        ticket: u64,
        args: &Value,
    ) -> Result<(), Refusal> {
        let owner = args["peer"].as_str().ok_or(answer::NOT_UNDERSTOOD)?;
        let opened = if let Some(reason) = self.receive_block(owner) {
            Err(reason.into())
        } else {
            self.pointer_options(host).and_then(|(own, named)| {
                host.open(false, ticket)
                    .map(|handle| (handle, own, named))
                    .map_err(|_| answer::NOT_UNDERSTOOD.into())
            })
        };
        let (handle, own, named) = match opened {
            Ok(opened) => opened,
            Err(reason) => {
                // Refusal before or during attachment cannot take over a continued press.
                if let Some(predecessor) = self
                    .incoming
                    .values_mut()
                    .rev()
                    .find(|i| i.target.owner == owner)
                {
                    predecessor.handoff_failed = true;
                } else if self.handoffs.contains(owner) {
                    // The record of the inherited hold goes only once its input is released (or the grant to
                    // release it is gone): a release the host refuses stays pending, and the next refused
                    // receive from that device tries again. An accepted successor takes it over instead.
                    if matches!(&reason, Refusal::Blocked(_))
                        || self.release(host, owner, false).is_ok()
                    {
                        self.handoffs.remove(owner);
                    }
                }
                return Err(reason);
            }
        };
        self.handoffs.remove(owner);
        self.incoming.insert(
            handle,
            Incoming {
                ticket,
                target: Target {
                    owner: owner.to_owned(),
                    own,
                    named,
                },
                pending: Vec::new(),
                ended: Ended::Closed,
                handoff_failed: false,
            },
        );
        Ok(())
    }

    fn pointer_options<H: Wire>(&mut self, host: &mut H) -> Result<(bool, bool), Refusal> {
        if let Some(permission) = self.missing_host("input.inject") {
            return Err(Blocked::MissingPermission(permission).into());
        }
        // Defaults are for settings omitted from a successful read, never for a failed read.
        let config = self.call(host, CONFIG, "get", json!({}))?;
        if let Some((owner, name)) =
            crate::host::missing_permissions(&config).find(|&pair| pair == ("host", "input.inject"))
        {
            let permission = Permission {
                owner: owner.into(),
                name: name.into(),
            };
            self.readiness.deny(None, permission.clone());
            return Err(Blocked::MissingPermission(permission).into());
        }
        let settings = config["settings"]
            .as_object()
            .ok_or(answer::NOT_UNDERSTOOD)?;
        let setting = |id: &str| {
            settings.get(id).map_or(Ok(true), |value| {
                value.as_bool().ok_or(answer::NOT_UNDERSTOOD)
            })
        };
        let independent = setting("independent")?;
        let name_tag = setting("name-tag")?;
        // "Independent pointer" stays on unless the person turned it off; whether this computer
        // can really draw one per device is the host's word, asked once per connection.
        let own = if independent {
            self.independent_pointer(host)?
        } else {
            false
        };
        Ok((own, own && name_tag))
    }

    /// A ready release must use the old connection, not depend on a new admission. The
    /// mixed poll may have chosen data ahead of control, so inspect control without waiting.
    /// Retain FIFO order within control deliveries, including between input batches.
    fn end_waiting<H: Wire + Data>(&mut self, host: &mut H) -> Result<bool, &'static str> {
        let Some(outgoing) = &self.outgoing else {
            return Ok(false);
        };
        if self
            .waiting
            .iter()
            .any(|delivery| outgoing.finishes_on(delivery))
        {
            return Ok(true);
        }
        for _ in self.waiting.len()..MOST_WAITING {
            if host.poll(&[], 0)? != Some(0) {
                break;
            }
            let delivery = host.receive()?;
            let end = self
                .outgoing
                .as_ref()
                .is_some_and(|out| out.finishes_on(&delivery));
            self.keep(delivery)?;
            if end {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Await one bounded piece of work. Queue reads and data reads remain separate so a
    /// live controller cannot monopolize the guest while another controller is idle.
    pub fn next<H: Wire + Data>(&mut self, host: &mut H) -> Result<Option<Value>, &'static str> {
        if self.waiting.first().is_some_and(|delivery| {
            config_changed(delivery)
                || self
                    .outgoing
                    .as_ref()
                    .is_some_and(|out| out.finishes_on(delivery))
        }) {
            return Ok(Some(self.waiting.remove(0)));
        }
        let mut handles = Vec::new();
        let mut owners = Vec::new();
        for (&handle, incoming) in &self.incoming {
            // Rotations from one peer retain their order, without delaying other peers.
            if !owners.contains(&incoming.target.owner.as_str()) {
                owners.push(incoming.target.owner.as_str());
                handles.push(handle);
            }
        }
        let outbound = self
            .outgoing
            .as_ref()
            .map(|out| out.handle)
            .filter(|handle| !handles.contains(handle));
        handles.extend(outbound);
        let timeout = if !self.waiting.is_empty() {
            0
        } else {
            self.outgoing
                .as_ref()
                .map_or(-1, Outgoing::rotation_wait_ms)
        };
        let ready = if handles.is_empty() && timeout == -1 {
            return host.receive().map(Some);
        } else if handles.is_empty() && !self.waiting.is_empty() {
            None
        } else {
            host.poll(&handles, timeout)?
        };
        let stream = ready.filter(|&handle| handle > 0 && Some(handle) != outbound);
        if ready.is_some()
            && ready == outbound
            && let Some(outgoing) = self.outgoing.take()
        {
            let device = outgoing.device.clone();
            if let Err(reason) = self.settle(host, outgoing, false, true) {
                self.early_end = Some((device, reason));
            }
        }
        let mut delivery = if ready == Some(0) && self.waiting.is_empty() {
            Some(host.receive()?)
        } else {
            None
        };
        let idle_timeout = ready.is_none() && timeout > 0;
        let next = self.waiting.first().or(delivery.as_ref());
        let forwarded = self
            .outgoing
            .as_ref()
            .is_some_and(|out| out.opened_at.is_some());
        let rotate = self.outgoing.as_mut().is_some_and(|out| {
            !next.is_some_and(config_changed)
                && (idle_timeout
                    || (forwarded
                        && (stream.is_some()
                            || next.is_some_and(|delivery| !out.measure_delivery(delivery))
                            || out.rotation_wait_ms() == 0)))
        });
        // A successful early `continues` preserves held input, but only a normal close
        // releases it. Inspect ready control before rotating rather than restarting an
        // interrupted wait or making an already-pending end need a fresh connection.
        if rotate {
            let finish_pending = if forwarded && !idle_timeout {
                if let Some(ready) = delivery.take() {
                    self.keep(ready)?;
                }
                self.end_waiting(host)?
            } else {
                false
            };
            if !finish_pending {
                let device = self.outgoing.as_ref().map(|out| out.device.clone());
                if let (Some(device), Err(reason)) = (device, self.rotate(host))
                    && (forwarded || matches!(&reason, Refusal::Blocked(_)))
                {
                    self.early_end = Some((device, reason));
                }
            }
        }
        // Give a deferred delivery a turn after at most one read, even on a busy stream.
        if let Some(handle) = stream
            && let Some(reply) = self.advance(host, handle)
        {
            host.send(reply)?;
        }
        if !self.waiting.is_empty() {
            delivery = Some(self.waiting.remove(0));
        }
        Ok(delivery)
    }

    fn advance<H: Wire + Data>(&mut self, host: &mut H, handle: Handle) -> Option<Value> {
        // The queued control delivery owns cleanup. Do not consume any more
        // bytes for this ticket, even when the host's poll picked it first.
        if self
            .incoming
            .get(&handle)
            .is_some_and(|incoming| replay_cancelled(&self.waiting, incoming.ticket))
        {
            return None;
        }
        let mut incoming = self.incoming.remove(&handle)?;
        let result = self.replay(host, handle, &mut incoming);
        if matches!(result, Ok(false)) {
            self.incoming.insert(handle, incoming);
            return None;
        }
        let cancelled = replay_cancelled(&self.waiting, incoming.ticket);
        let reply = self.complete_receive(host, handle, incoming, result.map(|_| ()));
        (!cancelled).then_some(reply)
    }

    fn release<H: Wire>(&mut self, host: &mut H, owner: &str, keep: bool) -> Result<(), Refusal> {
        self.call(
            host,
            INPUT,
            "release",
            json!({"owner": owner, "keep": keep}),
        )
        .map(|_| ())
        .map_err(|reason| match reason {
            Refusal::Blocked(_) => reason,
            Refusal::Other(_) => answer::NOT_REPLAYED.into(),
        })
    }

    fn hand_off(&mut self, incoming: &Incoming) -> bool {
        let owner = incoming.target.owner.as_str();
        if self.incoming.values().any(|i| i.target.owner == owner) {
            return true; // An accepted successor takes over even if another candidate failed.
        }
        if incoming.handoff_failed || self.handoffs.len() == MOST_DEVICES {
            return false; // Release rather than forgetting responsibility for a press.
        }
        self.handoffs.insert(owner.to_owned());
        true
    }

    fn complete_receive<H: Wire + Data>(
        &mut self,
        host: &mut H,
        handle: Handle,
        incoming: Incoming,
        result: Result<(), Refusal>,
    ) -> Value {
        host.close(handle);
        // Only the earliest open handle for an owner may replay. Cancelling a queued
        // replacement cannot release its live predecessor, but records a failed candidate.
        let predecessor = if result == Err(answer::CANCELLED.into()) {
            self.incoming
                .range_mut(..handle)
                .rev()
                .find_map(|(_, i)| (i.target.owner == incoming.target.owner).then_some(i))
        } else {
            None
        };
        let result = if let Some(predecessor) = predecessor {
            predecessor.handoff_failed = true;
            result
        } else if result.is_ok() && incoming.ended == Ended::Continues && self.hand_off(&incoming) {
            Ok(())
        } else if matches!(&result, Err(Refusal::Other(REPLAY_BUSY | NO_REPLAY_REPLY))) {
            self.handoffs.remove(&incoming.target.owner);
            // If the broker is still full, the failed receive itself tells the owner
            // to release the peer's held input. Do not wait indefinitely for cleanup.
            let _ = self.replay_call(
                host,
                "release",
                json!({"owner":incoming.target.owner,"keep":false}),
            );
            Err(answer::NOT_REPLAYED.into())
        } else if matches!(&result, Err(Refusal::Blocked(_))) {
            self.handoffs.remove(&incoming.target.owner);
            result
        } else {
            let keep = result.is_ok() && incoming.ended == Ended::Rests;
            let released = self.release(host, &incoming.target.owner, keep);
            result.and(released)
        };
        crate::host::diagnostic(6, diagnostic_outcome(&result), incoming.ticket, 0, 0);
        json!({"op":"reply","ticket":incoming.ticket,"result":outcome(result)})
    }

    fn replay<H: Wire + Data>(
        &mut self,
        host: &mut H,
        handle: Handle,
        incoming: &mut Incoming,
    ) -> Result<bool, Refusal> {
        let mut eof = read_replay(host, handle, &mut incoming.pending)?;
        // A whole turn can read ahead only once per allowed retry. Repeated
        // recoveries cannot keep this turn alive as other controllers wait.
        let mut lookahead = REPLAY_BACKOFF_MS.len();
        let mut batch = Vec::new();
        loop {
            take_replay_batch(incoming, &mut batch, false)?;
            if batch.is_empty() {
                return if eof && !incoming.pending.is_empty() {
                    Err(answer::NOT_UNDERSTOOD.into())
                } else {
                    Ok(eof)
                };
            }
            self.replay_batch(host, handle, incoming, &mut eof, &mut lookahead, &mut batch)?;
            batch.clear();
        }
    }

    fn replay_batch<H: Wire + Data>(
        &mut self,
        host: &mut H,
        handle: Handle,
        incoming: &mut Incoming,
        eof: &mut bool,
        lookahead: &mut usize,
        events: &mut Vec<Value>,
    ) -> Result<(), Refusal> {
        let mut retry = false;
        let mut backoffs = REPLAY_BACKOFF_MS.into_iter();
        loop {
            let args = replay_args(&incoming.target, events);
            let result = if retry {
                self.replay_call(host, "inject", args)
            } else {
                self.call(host, INPUT, "inject", args)
            };
            match result {
                // A cancellation can arrive while this batch was answered: nothing more of the stream is
                // applied, and a cancelled last batch cannot complete a `continues` handoff.
                Ok(_) if replay_cancelled(&self.waiting, incoming.ticket) => {
                    return Err(answer::CANCELLED.into());
                }
                Ok(_) => return Ok(()),
                Err(Refusal::Other(REPLAY_BUSY)) => {
                    let delay = backoffs.next().ok_or(REPLAY_BUSY)?;
                    self.wait_to_replay(host, incoming.ticket, delay)?;
                }
                // No answer is not a refusal before admission: the batch might have
                // run. Never send it a second time after an ambiguous result.
                Err(reason @ Refusal::Other(NO_REPLAY_REPLY)) => return Err(reason),
                Err(Refusal::Other(word)) if word != answer::CANCELLED => {
                    return Err(answer::NOT_REPLAYED.into());
                }
                Err(reason) => return Err(reason),
            }
            // Only collapse adjacent absolute places; a click must see the place before it,
            // and relative motion, buttons, keys, wheel and text all keep their order.
            events.dedup_by(|next, previous| {
                if replay_place(next) && replay_place(previous) {
                    std::mem::swap(next, previous);
                    true
                } else {
                    false
                }
            });
            take_replay_batch(incoming, events, true)?;
            if events.len() < MOST_BATCH && !*eof && *lookahead > 0 {
                match host.poll(&[handle], 0)? {
                    Some(ready) if ready == handle => {
                        *lookahead -= 1;
                        *eof = read_replay(host, handle, &mut incoming.pending)?;
                        take_replay_batch(incoming, events, true)?;
                    }
                    Some(0) => {
                        let message = host.receive()?;
                        self.keep(message)?;
                    }
                    _ => {}
                }
            }
            if replay_cancelled(&self.waiting, incoming.ticket) {
                return Err(answer::CANCELLED.into());
            }
            retry = true;
        }
    }

    fn wait_to_replay<H: Wire + Data>(
        &mut self,
        host: &mut H,
        ticket: u64,
        delay: i32,
    ) -> Result<(), Refusal> {
        for _ in 0..BACKOFF_WAITS {
            if replay_cancelled(&self.waiting, ticket) {
                return Err(answer::CANCELLED.into());
            }
            match host.poll(&[], delay)? {
                None => return Ok(()),
                Some(0) => {
                    let message = host.receive()?;
                    self.keep(message)?;
                }
                Some(_) => break,
            }
        }
        if replay_cancelled(&self.waiting, ticket) {
            Err(answer::CANCELLED.into())
        } else {
            Err(REPLAY_BUSY.into())
        }
    }
}

fn read_replay<H: Data>(
    host: &mut H,
    handle: Handle,
    pending: &mut Vec<u8>,
) -> Result<bool, Refusal> {
    let mut buffer = [0u8; 4096];
    let count = host
        .read(handle, &mut buffer)
        .map_err(|_| answer::STOPPED)?;
    let bytes = buffer.get(..count).ok_or(answer::NOT_UNDERSTOOD)?;
    pending.extend_from_slice(bytes);
    Ok(bytes.is_empty())
}

fn replay_place(event: &Value) -> bool {
    event.as_object().is_some_and(|fields| fields.len() == 3)
        && event["t"] == "position"
        && ["x", "y"].iter().all(|&axis| {
            event[axis]
                .as_f64()
                .is_some_and(|n| (0. ..=1.).contains(&n))
        })
}

fn replay_cancelled(waiting: &[Value], ticket: u64) -> bool {
    waiting
        .iter()
        .any(|message| message["type"] == "cancelled" && message["ticket"].as_u64() == Some(ticket))
}

/// Parse one ordered batch. Any tail (complete lines or a partial one) belongs to this
/// stream and stays there until its turn; a retry never consumes another peer's input.
fn take_replay_batch(
    incoming: &mut Incoming,
    batch: &mut Vec<Value>,
    coalesce: bool,
) -> Result<(), Refusal> {
    while batch.len() < MOST_BATCH {
        let Some(end) = incoming.pending.iter().position(|&b| b == b'\n') else {
            if incoming.pending.len() > MOST_LINE {
                return Err(answer::NOT_UNDERSTOOD.into());
            }
            break;
        };
        let line: Vec<u8> = incoming.pending.drain(..=end).collect();
        if line.len() > MOST_LINE + 1 {
            return Err(answer::NOT_UNDERSTOOD.into());
        }
        let event: Value =
            serde_json::from_slice(&line[..end]).map_err(|_| answer::NOT_UNDERSTOOD)?;
        if !event.is_object() {
            return Err(answer::NOT_UNDERSTOOD.into());
        }
        incoming.ended = if event == json!({"continues": true}) {
            Ended::Continues
        } else if event == json!({"rests": true}) {
            Ended::Rests
        } else {
            Ended::Closed
        };
        if incoming.ended != Ended::Closed {
            continue;
        }
        if coalesce
            && replay_place(&event)
            && let Some(previous) = batch.last_mut().filter(|previous| replay_place(previous))
        {
            *previous = event;
        } else {
            batch.push(event);
        }
    }
    Ok(())
}

fn replay_args(target: &Target, events: &[Value]) -> Value {
    let mut args = json!({"events": events, "owner": target.owner});
    if target.own {
        args["pointer"] = json!("own");
    }
    // Only asks: the application decides the name from the authenticated owner.
    if target.named {
        args["show_name"] = json!(true);
    }
    args
}

/// Whose input is replayed, and whether it moves a pointer of its own.
struct Target {
    owner: String,
    own: bool,
    /// Whether its pointer carries the controlling device's name.
    named: bool,
}

pub(crate) struct Incoming {
    ticket: u64,
    target: Target,
    pending: Vec<u8>,
    ended: Ended,
    /// A replacement failed; release at a continuing EOF if no accepted successor remains.
    handoff_failed: bool,
}

/// The code a failure is reported with: the application's own vocabulary, which is all it reads. A device that cannot be
/// reached is `unavailable`, so the person is told what is missing and not only that something failed.
fn code(reason: &str) -> &'static str {
    match reason {
        answer::CANCELLED => "cancelled",
        answer::REFUSED | answer::DENIED | answer::STOPPED => "unavailable",
        _ => "failed",
    }
}

fn failure(reason: &'static str) -> Value {
    json!({"status":"error","code":code(reason),"message":reason})
}

/// What to reply for one finished call.
fn outcome(result: Result<(), Refusal>) -> Value {
    match result {
        Ok(()) => json!({"status":"ok","value":true}),
        Err(Refusal::Other(reason)) => failure(reason),
        Err(Refusal::Blocked(blocked)) => blocked.value(),
    }
}

/// Answer one delivery. `None` for anything that is not a call.
pub fn handle<H: Wire + Data>(host: &mut H, state: &mut State, delivery: &Value) -> Option<Value> {
    // A page placement the host refused is not left behind the settings: the next delivery tries it again, so
    // nothing waits for a later change of the settings or a restart.
    if state.pages_failed && !config_changed(delivery) {
        state.pages_failed = state.configure_pages(host).is_err();
    }
    if config_changed(delivery) {
        state.on_config_changed(host);
        return None;
    }
    if delivery["type"] == "cancelled" {
        let ticket = delivery["ticket"].as_u64()?;
        let handle = state
            .incoming
            .iter()
            .find_map(|(&handle, incoming)| (incoming.ticket == ticket).then_some(handle));
        if let Some(handle) = handle
            && let Some(incoming) = state.incoming.remove(&handle)
        {
            // Cancellation ends only this peer's session; an expired ticket gets no late reply.
            let _ = state.complete_receive(host, handle, incoming, Err(answer::CANCELLED.into()));
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
            Err(reason) => {
                Some(json!({"op":"reply","ticket":ticket,"result":outcome(Err(reason))}))
            }
        };
    }
    if delivery["method"] == "view" || delivery["method"] == "layout" {
        let result = if delivery["method"] == "layout" {
            let mut ui = std::mem::take(&mut state.layout);
            let result = ui.event(
                state,
                host,
                &delivery["args"]["event"],
                delivery["args"]["page"].as_str(),
            );
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
            Err(reason) => failure(reason),
        };
        return Some(json!({"op":"reply","ticket":ticket,"result":result}));
    }
    let result = match delivery["method"].as_str() {
        Some("input") => state.input(host, &delivery["args"]),
        _ => Err(answer::UNKNOWN.into()),
    };
    crate::host::diagnostic(10, diagnostic_outcome(&result), ticket, 0, 0);
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

    fn configuration(settings: Value) -> Value {
        json!({"plugin":"control","settings":settings,"missing_permissions":[],"peers":[]})
    }

    fn config_event() -> Value {
        json!({"type":"event","provider":"host","service":CONFIG,"version":1,"event":"changed","data":{}})
    }

    fn neutral_ack() -> Vec<u8> {
        let bytes = br#"{"status":"not-accepted"}"#;
        let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(bytes);
        framed
    }

    #[test]
    fn a_page_placement_the_host_refused_is_tried_again_with_the_next_delivery() {
        let mut state = State::default();
        let mut host = Script::default();
        let config = configuration(json!({"layout-page": true}));
        host.results.extend([
            ok(config.clone()),
            json!({"status":"error","code":"failed"}),
        ]);
        super::handle(&mut host, &mut state, &config_event());
        assert_eq!(state.placed_pages, None);
        host.results.extend([ok(config), ok(json!({}))]);
        super::handle(
            &mut host,
            &mut state,
            &json!({"type":"event","service":"other"}),
        );
        assert_eq!(state.placed_pages, Some(vec!["layout-page"]));
        let asked = host.sent.len();
        super::handle(
            &mut host,
            &mut state,
            &json!({"type":"event","service":"other"}),
        );
        assert_eq!(
            host.sent.len(),
            asked,
            "a placement that holds is not asked again"
        );
    }

    #[test]
    fn a_remote_no_stops_the_visit_without_a_release_or_reopening_on_late_samples() {
        for early_ack in [false, true] {
            let mut state = State::default();
            let mut host = Script::default();
            let mut begin = input_call(1, json!([{"t":"key_code","code":29,"down":true}]), false);
            begin["args"]["begin"] = json!(true);
            begin["args"]["at"] = json!(1000);
            let no = json!({"status":"not-accepted"});
            let sample = input_call(2, json!([{"t":"text","text":"not replayed"}]), false);
            if early_ack {
                host.results.push_back(ok(json!({"claim":1})));
                assert_eq!(
                    super::handle(&mut host, &mut state, &begin).unwrap()["result"]["status"],
                    "ok"
                );
                host.incoming.push_back(neutral_ack());
                assert_eq!(
                    state.next(&mut host),
                    Ok(None),
                    "read the reverse half even before an end"
                );
                assert_eq!(
                    super::handle(&mut host, &mut state, &sample).unwrap()["result"],
                    no
                );
                assert_eq!(host.closed, 1);
            } else {
                host.results.push_back(no.clone());
                assert_eq!(
                    super::handle(&mut host, &mut state, &begin).unwrap()["result"],
                    no
                );
            }
            let bytes = host.written.len();
            let mut end = input_call(3, json!([]), true);
            end["args"]["stop"] = no.clone();
            assert_eq!(
                super::handle(&mut host, &mut state, &end).unwrap()["result"]["status"],
                "ok"
            );
            assert_eq!(
                super::handle(&mut host, &mut state, &sample).unwrap()["result"],
                no
            );
            assert_eq!(
                host.sent
                    .iter()
                    .filter(|message| message["method"] == "send")
                    .count(),
                1
            );
            assert_eq!(host.written.len(), bytes);
            assert_eq!(
                host.shutdowns, 0,
                "the explicit stopped end cannot open or finish a send"
            );
            assert!(state.outgoing.is_none() && state.retained.is_none());
            host.results.push_back(ok(json!({"claim":2})));
            assert_eq!(
                super::handle(&mut host, &mut state, &begin).unwrap()["result"]["status"],
                "ok"
            );
            assert_eq!(host.opened.last(), Some(&(true, 2)));
        }
    }

    #[test]
    fn a_controlling_grant_removed_mid_visit_closes_and_requires_a_new_begin_when_restored() {
        let mut state = State::default();
        let mut host = Script::default();
        let granted = json!({"device":"pc","permissions":[
            {"owner":"host","name":"peers.send"},{"owner":"control","name":"control.use"}
        ]});
        let permission = json!({"owner":"control","name":"control.use"});
        let mut config = configuration(json!({}));
        config["peers"] = json!([granted]);
        state.readiness.update(&config).unwrap();
        state.placed_pages = Some(vec![]);
        host.results.push_back(ok(json!({"claim":1})));
        let pressed = input_call(1, json!([{"t":"key_code","code":29,"down":true}]), false);
        assert_eq!(
            super::handle(&mut host, &mut state, &pressed).unwrap()["result"]["status"],
            "ok"
        );
        let bytes = host.written.len();
        config["peers"][0]["permissions"] = json!([{"owner":"host","name":"peers.send"}]);
        host.results.push_back(ok(config.clone()));
        super::handle(&mut host, &mut state, &config_event());
        assert_eq!(host.closed, 1);
        let missing = json!({"status":"missing-permission","permission":permission});
        let mut end = input_call(2, json!([]), true);
        end["args"]["stop"] = missing.clone();
        assert_eq!(
            super::handle(&mut host, &mut state, &end).unwrap()["result"]["status"],
            "ok"
        );
        let sample = input_call(3, json!([{"t":"move","dx":1,"dy":0}]), false);
        assert_eq!(
            super::handle(&mut host, &mut state, &sample).unwrap()["result"],
            missing
        );
        config["peers"][0] = granted;
        host.results.push_back(ok(config));
        super::handle(&mut host, &mut state, &config_event());
        assert_eq!(
            super::handle(&mut host, &mut state, &sample).unwrap()["result"]["status"],
            "not-accepted"
        );
        assert_eq!(host.written.len(), bytes);
        assert!(state.outgoing.is_none() && state.retained.is_none());
        assert_eq!(host.opened, [(true, 1)]);
        host.results.push_back(ok(json!({"claim":2})));
        let mut fresh = input_call(4, json!([{"t":"position","x":0.1,"y":0.5}]), false);
        fresh["args"]["begin"] = json!(true);
        assert_eq!(
            super::handle(&mut host, &mut state, &fresh).unwrap()["result"]["status"],
            "ok"
        );
        assert_eq!(host.opened, [(true, 1), (true, 2)]);
    }

    #[test]
    fn a_receiving_grant_removed_mid_visit_discards_unfinished_input_and_handoff() {
        let mut state = State::default();
        let mut host = Script::default();
        let settings = configuration(json!({"independent":false}));
        state.readiness.update(&settings).unwrap();
        state.placed_pages = Some(vec![]);
        host.results.push_back(ok(settings.clone()));
        let receive =
            json!({"type":"call","ticket":21,"method":"receive","args":{"peer":"sender"}});
        assert!(super::handle(&mut host, &mut state, &receive).is_none());
        host.results.push_back(ok(json!({})));
        host.incoming
            .push_back(b"{\"t\":\"key_code\",\"code\":29,\"down\":true}\n{\"t\":\"text\"".to_vec());
        assert!(state.advance(&mut host, 7).is_none());
        assert_eq!(host.sent.last().unwrap()["method"], "inject");
        state.handoffs.insert("other".into());
        let mut revoked = settings.clone();
        let permission = json!({"owner":"host","name":"input.inject"});
        revoked["missing_permissions"] = json!([permission]);
        host.results.push_back(ok(revoked));
        super::handle(&mut host, &mut state, &config_event());
        assert!(state.incoming.is_empty() && state.handoffs.is_empty());
        assert_eq!(host.closed, 1);
        assert_eq!(
            host.sent.last().unwrap()["result"],
            json!({"status":"missing-permission","permission":permission})
        );
        assert!(!host.sent.iter().any(|call| call["method"] == "release"));
        host.results.push_back(ok(settings));
        super::handle(&mut host, &mut state, &config_event());
        assert!(state.advance(&mut host, 7).is_none());
        assert_eq!(
            host.sent
                .iter()
                .filter(|message| message["method"] == "inject")
                .count(),
            1
        );
    }

    #[test]
    fn a_crossing_refused_by_the_local_grant_returns_the_named_permission_without_opening_a_stream()
    {
        let mut state = State::default();
        let mut host = Script::default();
        let missing = json!({"status":"missing-permission","permission":{"owner":"control","name":"control.use"}});
        host.results.push_back(missing.clone());
        let delivery = input_call(42, json!([{"t":"position","x":0.1,"y":0.5}]), false);
        let reply = super::handle(&mut host, &mut state, &delivery).unwrap();
        assert_eq!(reply, json!({"op":"reply","ticket":42,"result":missing}));
        assert!(host.opened.is_empty() && host.written.is_empty());
    }

    #[test]
    fn a_receiver_missing_injection_does_not_probe_a_guarded_method_in_shared_mode() {
        let mut state = State::default();
        let mut host = Script::default();
        let permission = json!({"owner":"host","name":"input.inject"});
        let missing = json!({"status":"missing-permission","permission":permission});
        host.results.push_back(ok(
            json!({"settings":{"independent":false},"missing_permissions":[permission]}),
        ));
        let reply = super::handle(
            &mut host,
            &mut state,
            &json!({
                "type":"call","ticket":17,"method":"receive","args":{"peer":"sender"}
            }),
        )
        .unwrap();
        assert_eq!(reply["result"], missing);
        assert_eq!(host.sent.len(), 1);
        assert_eq!(host.sent[0]["method"], "get");
        assert!(host.opened.is_empty() && state.incoming.is_empty());
    }

    #[test]
    fn a_receiving_permission_revoked_during_replay_stays_a_named_refusal() {
        let mut state = State::default();
        let mut host = Script::default();
        host.results.push_back(ok(
            json!({"settings":{"independent":false},"missing_permissions":[]}),
        ));
        let missing = json!({"status":"missing-permission","permission":{"owner":"host","name":"input.inject"}});
        host.results.push_back(missing.clone());
        host.incoming
            .push_back(b"{\"t\":\"move\",\"dx\":1,\"dy\":2}\n".to_vec());
        let reply = handle(
            &mut host,
            &mut state,
            &json!({
                "type":"call","ticket":18,"method":"receive","args":{"peer":"sender"}
            }),
        )
        .unwrap();
        assert_eq!(reply["result"], missing);
        assert_eq!(host.closed, 1);
        assert!(!host.sent.iter().any(|call| call["method"] == "release"));
        assert!(state.incoming.is_empty());
    }

    #[test]
    fn each_page_is_among_the_applications_pages_only_while_its_own_option_is_on() {
        let layout = json!({"id":"layout-page", "page":"layout", "placement":"primary"});
        let remote = json!({"id":"remote-page", "page":"remote", "placement":"primary"});
        for (layout_on, remote_on, placed) in [
            (false, false, json!([])),
            (true, false, json!([layout])),
            (false, true, json!([remote])),
            (true, true, json!([layout, remote])),
        ] {
            let mut host = Script::default();
            let mut state = State::default();
            host.results.push_back(ok(configuration(
                json!({"layout-page":layout_on, "remote-page":remote_on}),
            )));
            host.results.push_back(ok(json!({})));
            state.configure_pages(&mut host).unwrap();
            let call = &host.sent[1];
            assert_eq!(call["service"], PAGES);
            assert_eq!(call["method"], "set");
            assert_eq!(call["args"]["pages"], placed);
        }
    }

    #[test]
    fn a_changed_layout_option_is_placed_while_running_and_an_unchanged_one_is_not_set_again() {
        let mut host = Script::default();
        let mut state = State::default();
        host.results
            .push_back(ok(configuration(json!({"layout-page":false}))));
        host.results.push_back(ok(json!({})));
        state.configure_pages(&mut host).unwrap();
        let mut unrelated = config_event();
        unrelated["provider"] = json!("another");
        assert!(super::handle(&mut host, &mut state, &unrelated).is_none());
        assert_eq!(
            host.sent.len(),
            2,
            "unrelated events do not read configuration"
        );
        host.results
            .push_back(ok(configuration(json!({"layout-page":true}))));
        host.results.push_back(ok(json!({})));
        assert!(super::handle(&mut host, &mut state, &config_event()).is_none());
        assert_eq!(host.sent.last().unwrap()["method"], "set");
        assert_eq!(
            host.sent.last().unwrap()["args"]["pages"][0]["placement"],
            "primary"
        );
        // Unchanged: read, never set again.
        host.results
            .push_back(ok(configuration(json!({"layout-page":true}))));
        let before = host.sent.len();
        assert!(super::handle(&mut host, &mut state, &config_event()).is_none());
        assert_eq!(host.sent.len(), before + 1);
        assert_eq!(host.sent.last().unwrap()["method"], "get");
    }

    #[test]
    fn leaving_after_rest_releases_the_previous_device_without_new_motion() {
        let mut host = Script {
            acks: true,
            ..Script::default()
        };
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim":1})));
        state
            .input(
                &mut host,
                &json!({"peer":"pc","events":[{"t":"move","dx":8,"dy":0}]}),
            )
            .unwrap();
        state.rest(&mut host).unwrap();
        let before = host.written.clone();
        host.results.push_back(ok(json!({"claim":2})));
        state.finish(&mut host).unwrap();
        assert_eq!(
            host.opened,
            vec![(true, 1), (true, 2)],
            "leaving must reach the rested target"
        );
        assert_eq!(
            host.written, before,
            "release must not synthesize motion or clicks"
        );
        assert_eq!((host.shutdowns, host.closed), (2, 2));
        state.finish(&mut host).unwrap();
        assert_eq!(host.opened.len(), 2, "a completed leave is idempotent");
    }

    #[test]
    fn ending_immediately_after_rotation_releases_the_retained_pointer() {
        let mut host = Script {
            acks: true,
            ..Script::default()
        };
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim":1})));
        state
            .input(
                &mut host,
                &json!({"peer":"pc","events":[{"t":"move","dx":8,"dy":0}]}),
            )
            .unwrap();
        state.rotate(&mut host).unwrap();
        host.results.push_back(ok(json!({"claim":2})));
        state.finish(&mut host).unwrap();
        assert_eq!(host.opened, vec![(true, 1), (true, 2)]);
        assert_eq!((host.shutdowns, host.closed), (2, 2));
        assert_eq!(host.sent.last().unwrap()["args"]["device"], "pc");
    }

    #[test]
    fn submitted_text_is_split_within_the_services_and_the_connections_bounds() {
        let check = |text: &str| {
            let inputs = typed(text);
            let mut whole = String::new();
            for input in &inputs {
                let piece = input["text"].as_str().unwrap();
                assert!(piece.chars().count() <= MOST_TYPED);
                assert!(serde_json::to_vec(input).unwrap().len() <= MOST_LINE);
                whole.push_str(piece);
            }
            assert_eq!(whole, text, "nothing lost, nothing reordered");
            inputs.len()
        };
        // 256 four-byte emoji: one input of them would be 1,046 bytes, over the line bound.
        assert_eq!(check(&"😀".repeat(256)), 2);
        // Plain text over the service's 256 characters.
        assert_eq!(check(&"a".repeat(600)), 3);
        // Characters JSON escapes count at their escaped size.
        assert!(check(&"\"\\".repeat(500)) >= 2);
        assert_eq!(check("short"), 1);
        assert_eq!(typed("").len(), 0);
    }

    #[test]
    fn a_connection_left_open_through_a_silence_is_rotated_by_the_wait() {
        let mut host = Script {
            acks: true,
            ..Script::default()
        };
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim":1})));
        // A key pressed on this computer and forwarded: nothing more comes while it is held.
        state
            .input(
                &mut host,
                &json!({"peer":"pc","events":[{"t":"key_code","code":42,"down":true}]}),
            )
            .unwrap();
        host.silent = true;
        assert_eq!(state.next(&mut host), Ok(None));
        assert_eq!(
            host.waits[0], ROTATE_MS as i32,
            "the wait is bounded while it is open"
        );
        assert!(state.outgoing.is_none(), "rotated after the silence");
        assert_eq!(state.retained.as_deref(), Some("pc"));
        assert!(
            String::from_utf8(host.written.clone())
                .unwrap()
                .ends_with(&format!("{CONTINUES}\n"))
        );
    }

    #[test]
    fn a_timed_forwarding_batch_keeps_the_stream_and_shortens_its_next_wait() {
        let mut host = Script {
            acks: true,
            ..Script::default()
        };
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim":1})));
        state
            .input(
                &mut host,
                &json!({"peer":"pc","at":1000,"events":[{"t":"key_code","code":42,"down":true}]}),
            )
            .unwrap();
        let next = json!({
            "type":"call","ticket":1,"method":"input",
            "args":{"peer":"pc","at":2000,"events":[{"t":"move","dx":1,"dy":0}]}
        });
        host.deliveries.push_back(next.clone());
        assert_eq!(state.next(&mut host), Ok(Some(next.clone())));
        assert_eq!(
            super::handle(&mut host, &mut state, &next).unwrap()["result"]["status"],
            "ok"
        );
        assert_eq!(host.opened, [(true, 1)]);
        assert_eq!(host.shutdowns, 0, "measured work keeps its existing stream");

        host.silent = true;
        state.next(&mut host).unwrap();
        assert_eq!(host.waits[1], ROTATE_MS as i32 - 1000);
        assert!(state.outgoing.is_none());
    }

    #[test]
    fn a_forwarded_end_or_switch_closes_the_live_stream_without_rotating() {
        for deferred in [false, true] {
            for (peer, end) in [("pc", true), ("next-pc", false)] {
                let mut host = Script {
                    acks: true,
                    ..Script::default()
                };
                let mut state = State::default();
                host.results.push_back(ok(json!({"claim":1})));
                state.input(
                    &mut host,
                    &json!({"peer":"pc","at":1000,"events":[{"t":"key_code","code":42,"down":true}]}),
                ).unwrap();
                let events = if end {
                    json!([])
                } else {
                    json!([{"t":"move","dx":1,"dy":0}])
                };
                let delivery = json!({
                    "type":"call","ticket":1,"method":"input",
                    "args":{"peer":peer,"events":events,"end":end}
                });
                host.deliveries.push_back(delivery.clone());
                if deferred {
                    host.results.push_back(ok(json!([])));
                    state.call(&mut host, PEERS, "list", json!({})).unwrap();
                }
                if !end {
                    host.results.push_back(ok(json!({"claim":2})));
                }
                assert_eq!(state.next(&mut host), Ok(Some(delivery.clone())));
                assert_eq!(
                    host.shutdowns, 0,
                    "the old stream must be available to finish"
                );
                let reply = super::handle(&mut host, &mut state, &delivery).unwrap();
                assert_eq!(reply["result"]["status"], "ok");
                assert_eq!(host.shutdowns, 1);
                assert!(state.retained.is_none());
                assert!(
                    !String::from_utf8_lossy(&host.written)
                        .lines()
                        .any(|line| line == CONTINUES)
                );
                if end {
                    assert_eq!(host.opened, [(true, 1)]);
                    assert!(state.outgoing.is_none());
                } else {
                    assert_eq!(
                        host.opened,
                        [(true, 1), (true, 2)],
                        "no new connection to the old target"
                    );
                    assert_eq!(state.outgoing.as_ref().unwrap().device, peer);
                }
            }
        }
    }

    #[test]
    fn batches_go_down_one_connection_as_lines_and_the_end_closes_it_after_the_acknowledgement() {
        let mut host = Script {
            acks: true,
            ..Script::default()
        };
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
        let mut host = Script {
            acks: true,
            ..Script::default()
        };
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
        let mut host = Script {
            acks: true,
            ..Script::default()
        };
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim": 1})));
        for i in 0..ROTATE_AFTER {
            let last = i + 1 == ROTATE_AFTER;
            handle(
                &mut host,
                &mut state,
                &input_call(u64::from(i) + 1, json!([{"t":"move","dx":1,"dy":0}]), last),
            )
            .unwrap();
        }
        let written = String::from_utf8(host.written.clone()).unwrap();
        assert_ne!(
            written.lines().last(),
            Some(CONTINUES),
            "the gesture ended: nothing may stay held"
        );
        assert!(state.outgoing.is_none() && state.retained.is_none());
        assert_eq!((host.shutdowns, host.closed), (1, 1));
    }

    #[test]
    fn a_short_acknowledgement_is_not_a_delivery() {
        let mut host = Script::default();
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim": 1})));
        state
            .input(
                &mut host,
                &json!({"peer":"pc","events":[{"t":"move","dx":8,"dy":0}]}),
            )
            .unwrap();
        host.incoming.push_back(ack(0));
        assert_eq!(state.finish(&mut host), Err(answer::STOPPED.into()));
        host.results.push_back(ok(json!({"claim": 2})));
        state
            .input(
                &mut host,
                &json!({"peer":"pc","events":[{"t":"move","dx":8,"dy":0}]}),
            )
            .unwrap();
        let sent = host.written.len() as u64 / 2;
        host.incoming.push_back(ack(sent));
        assert_eq!(
            state.finish(&mut host),
            Ok(()),
            "every byte of its own stream acknowledged"
        );
    }

    #[test]
    fn an_end_that_loses_its_acknowledgement_is_reported_once_and_forgotten() {
        let mut host = Script::default();
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim":1})));
        state
            .input(
                &mut host,
                &json!({"peer":"pc","events":[{"t":"move","dx":8,"dy":0}]}),
            )
            .unwrap();
        // The old device answers a count that is not the stream's: the end is reported once...
        host.incoming.push_back(ack(0));
        assert_eq!(state.finish(&mut host), Err(answer::STOPPED.into()));
        assert!(
            state.retained.is_none(),
            "...and its stale identity is not kept for a retry"
        );
        state.finish(&mut host).unwrap();
        assert_eq!(
            host.sent.iter().filter(|m| m["method"] == "send").count(),
            1,
            "no release stream is attempted for a forgotten device"
        );
    }

    #[test]
    fn a_peer_that_never_acknowledges_is_given_up_after_a_bounded_wait_and_what_arrived_is_kept() {
        let mut host = Script::default();
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim":1})));
        state
            .input(
                &mut host,
                &json!({"peer":"pc","events":[{"t":"move","dx":8,"dy":0}]}),
            )
            .unwrap();
        // The device goes quiet; a page event and a stray answer arrive while its end is awaited.
        host.silent = true;
        host.deliveries
            .push_back(json!({"type":"event","name":"page"}));
        host.deliveries
            .push_back(json!({"type":"result","id":0,"result":{}}));
        assert_eq!(state.finish(&mut host), Err(answer::STOPPED.into()));
        assert_eq!(
            host.waits, [ACK_MS; 3],
            "each wait is bounded, never the connection's own thirty seconds"
        );
        assert_eq!(
            (host.shutdowns, host.closed),
            (1, 1),
            "the connection is closed all the same"
        );
        assert_eq!(
            state.waiting,
            [json!({"type":"event","name":"page"})],
            "the delivery is handled afterwards, the stray answer is not kept"
        );
        assert!(state.outgoing.is_none() && state.retained.is_none());
    }

    #[test]
    fn a_leave_refused_for_an_unreachable_device_is_tried_once_and_forgotten() {
        let mut host = Script {
            acks: true,
            ..Script::default()
        };
        let mut state = State::default();
        host.results.push_back(ok(json!({"claim":1})));
        state
            .input(
                &mut host,
                &json!({"peer":"pc","events":[{"t":"move","dx":8,"dy":0}]}),
            )
            .unwrap();
        state.rest(&mut host).unwrap();
        // Gone dark: the release stream is refused; the device is forgotten all the same.
        host.results
            .push_back(json!({"status":"error","code":"failed","message":"offline"}));
        state.finish(&mut host).unwrap();
        let sends = host.sent.iter().filter(|m| m["method"] == "send").count();
        assert_eq!(sends, 2, "the gesture stream plus one release stream");
        assert_eq!(host.opened.len(), 1, "a refused stream is never opened");
        assert!(state.retained.is_none());
        state.finish(&mut host).unwrap();
        assert_eq!(
            host.sent.iter().filter(|m| m["method"] == "send").count(),
            sends,
            "a forgotten device is not released again"
        );
    }

    #[test]
    fn a_rotated_connection_keeps_held_input_and_only_a_real_end_releases_it() {
        let run = |bytes: &[u8]| {
            let mut host = Script::default();
            let mut state = State::default();
            host.incoming.push_back(bytes.to_vec());
            host.results
                .push_back(ok(json!({"settings":{"independent":true}})));
            host.results
                .push_back(ok(json!({"independent_pointer":true})));
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
        assert_eq!(
            run(&rotated),
            None,
            "a rotation must not let go of the held button"
        );
        assert_eq!(
            run(held),
            Some(false),
            "a connection that ends without a word lets go of all"
        );
        let mut rested = held.to_vec();
        rested.extend_from_slice(RESTS.as_bytes());
        rested.push(b'\n');
        assert_eq!(
            run(&rested),
            Some(true),
            "a gesture that ended lets go but keeps the pointer"
        );
        let mut stale = rotated.clone();
        stale.extend_from_slice(b"{\"t\":\"move\",\"dx\":1,\"dy\":0}\n");
        assert_eq!(
            run(&stale),
            Some(false),
            "the marker counts only as the last line"
        );
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
            fn poll(
                &mut self,
                handles: &[Handle],
                timeout_ms: i32,
            ) -> Result<Option<Handle>, &'static str> {
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
        host.results
            .push_back(ok(json!({"independent_pointer":true})));
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
        assert_eq!(
            calls[1]["service"], INPUT,
            "the capability question goes to peersus.input"
        );
        assert_eq!(calls[1]["method"], "inspect");
        assert_eq!(calls[1]["args"], json!({}));
        let injects: Vec<&Value> = calls
            .iter()
            .filter(|m| m["method"] == "inject")
            .copied()
            .collect();
        assert_eq!(injects[0]["args"]["events"][0]["t"], "move");
        assert_eq!(injects[0]["args"]["owner"], "rog");
        assert_eq!(injects[0]["args"]["pointer"], "own", "a pointer of its own");
        assert_eq!(
            injects[0]["args"]["show_name"], true,
            "its name is asked for by default"
        );
        assert!(
            injects[0]["args"].get("label").is_none(),
            "the extension never writes the name itself"
        );
        assert_eq!(injects[1]["args"]["events"][0]["button"], "left");
        let release = calls.iter().find(|m| m["method"] == "release").unwrap();
        assert_eq!(
            release["args"]["owner"], "rog",
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
        assert!(
            host.sent.iter().all(|m| m["method"] != "inspect"),
            "the capability is asked only while the setting is on"
        );
        let inject = host.sent.iter().find(|m| m["method"] == "inject").unwrap();
        assert!(inject["args"].get("pointer").is_none());
        assert!(
            inject["args"].get("show_name").is_none(),
            "the shared pointer carries no name"
        );
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
        host.results
            .push_back(ok(json!({"independent_pointer":true})));
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
    fn a_host_that_cannot_draw_a_pointer_per_device_shares_its_own() {
        for capability in [false, true] {
            let mut host = Script::default();
            let mut state = State::default();
            host.incoming
                .push_back(b"{\"t\":\"move\",\"dx\":1,\"dy\":2}\n".to_vec());
            host.results
                .push_back(ok(json!({"settings":{"independent":true}})));
            host.results
                .push_back(ok(json!({"independent_pointer":capability})));
            host.results.push_back(ok(json!({"applied":1})));
            host.results.push_back(ok(json!({"released":true})));
            handle(
                &mut host,
                &mut state,
                &json!({"type":"call","ticket":1,"method":"receive","args":{"peer":"rog"}}),
            )
            .unwrap();
            let inspect = host.sent.iter().find(|m| m["method"] == "inspect").unwrap();
            assert_eq!(inspect["service"], INPUT);
            let inject = host.sent.iter().find(|m| m["method"] == "inject").unwrap();
            assert_eq!(
                inject["args"].get("pointer").is_some(),
                capability,
                "capability {capability}: pointer=own only when the host says it can"
            );
            assert_eq!(
                inject["args"].get("show_name").is_some(),
                capability,
                "a shared pointer carries no name"
            );
        }
    }

    #[test]
    fn a_failed_settings_read_does_not_accept_a_receive_with_pointer_defaults() {
        for (code, reason) in [("denied", answer::DENIED), ("failed", answer::REFUSED)] {
            let mut host = Script::default();
            let mut state = State::default();
            host.results
                .push_back(json!({"status":"error","code":code}));
            host.results
                .push_back(ok(json!({"independent_pointer":true})));
            let reply = super::handle(
                &mut host,
                &mut state,
                &json!({"type":"call","ticket":1,"method":"receive","args":{"peer":"controller"}}),
            )
            .unwrap();

            assert_eq!(reply["result"]["message"], reason);
            assert!(host.opened.is_empty());
            assert_eq!(host.sent.len(), 1, "no capability query or injection");
        }
    }

    #[test]
    fn a_receive_is_refused_when_the_capability_has_no_answer() {
        for inspect_reply in [
            json!({"status":"error","code":"failed","message":"unsupported method"}),
            json!({"status":"ok","value":{}}),
            json!({"status":"ok","value":{"independent_pointer":"yes"}}),
        ] {
            let mut host = Script::default();
            let mut state = State::default();
            host.results
                .push_back(ok(json!({"settings":{"independent":true}})));
            host.results.push_back(inspect_reply.clone());
            let reply = handle(
                &mut host,
                &mut state,
                &json!({"type":"call","ticket":1,"method":"receive","args":{"peer":"rog"}}),
            )
            .unwrap();
            assert_eq!(reply["result"]["status"], "error", "{inspect_reply}");
            assert!(
                host.opened.is_empty(),
                "a refused receive never takes the stream"
            );
            assert!(state.incoming.is_empty());
        }
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
            host.results
                .push_back(ok(json!({"independent_pointer":true})));
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
    use std::collections::{BTreeMap, VecDeque};

    #[derive(Default)]
    struct Concurrent {
        wire: Script,
        streams: BTreeMap<Handle, VecDeque<Vec<u8>>>,
        ready: VecDeque<Handle>,
        closed: Vec<Handle>,
        receive_error: Option<&'static str>,
        refuse_ticket: Option<u64>,
        quick_waiting: bool,
        withhold_quick_replies: bool,
    }
    impl Wire for Concurrent {
        fn receive(&mut self) -> Result<Value, &'static str> {
            let result = self
                .wire
                .receive()
                .map_err(|error| self.receive_error.unwrap_or(error))?;
            if result["type"] == "result" {
                self.quick_waiting = false;
            }
            Ok(result)
        }
        fn send(&mut self, value: Value) -> Result<(), &'static str> {
            self.quick_waiting = value["timeout_ms"].as_u64() == Some(u64::from(REPLAY_CALL_MS));
            self.wire.send(value)
        }
    }
    impl Data for Concurrent {
        fn open(&mut self, claim: bool, ticket: u64) -> Result<Handle, &'static str> {
            if !claim && self.refuse_ticket == Some(ticket) {
                return Err("The data connection could not be opened");
            }
            if claim {
                self.wire.open(claim, ticket)?;
            }
            Ok(i32::try_from(ticket).unwrap())
        }
        fn read(&mut self, handle: Handle, into: &mut [u8]) -> Result<usize, &'static str> {
            let Some(stream) = self.streams.get_mut(&handle) else {
                return self.wire.read(handle, into);
            };
            let bytes = stream
                .pop_front()
                .ok_or("an idle controller must never be read")?;
            assert!(bytes.len() <= into.len());
            into[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }
        fn write(&mut self, handle: Handle, from: &[u8]) -> Result<usize, &'static str> {
            self.wire.write(handle, from)
        }
        fn shutdown(&mut self, handle: Handle) -> Result<(), &'static str> {
            self.wire.shutdown(handle)
        }
        fn close(&mut self, handle: Handle) {
            self.closed.push(handle);
        }
        fn poll(
            &mut self,
            handles: &[Handle],
            timeout_ms: i32,
        ) -> Result<Option<Handle>, &'static str> {
            self.wire.waits.push(timeout_ms);
            // The host's rotating readiness cursor may select data ahead of already readable control.
            if self
                .ready
                .front()
                .is_some_and(|handle| handles.contains(handle))
            {
                Ok(self.ready.pop_front())
            } else if !self.wire.deliveries.is_empty()
                || (self.quick_waiting
                    && !self.withhold_quick_replies
                    && !self.wire.results.is_empty())
            {
                Ok(Some(0))
            } else if !self.wire.incoming.is_empty() {
                Ok(handles.first().copied())
            } else {
                Ok(None)
            }
        }
    }

    fn ok(value: Value) -> Value {
        json!({"status":"ok","value":value})
    }

    fn overload(code: &str) -> Value {
        json!({"status":"error","code":code,"message":"The request was refused"})
    }

    #[test]
    fn replay_recovers_from_broker_overload_without_skipping_discrete_input() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        host.wire
            .results
            .push_back(ok(json!({"settings":{"independent":false}})));
        state.receive(&mut host, 7, &json!({"peer":"pc"})).unwrap();
        let point = |x| json!({"t":"position","x":x,"y":0.5});
        let press = json!({"t":"button","button":"left","down":true});
        let key = json!({"t":"key_code","code":42,"down":true});
        let scroll = json!({"t":"scroll","dx":0,"dy":1});
        let lines = |events: &[Value]| {
            events
                .iter()
                .flat_map(|event| format!("{event}\n").into_bytes())
                .collect::<Vec<_>>()
        };
        host.streams.insert(
            7,
            [
                lines(&[point(0.1)]),
                lines(&[
                    point(0.2),
                    point(0.3),
                    press.clone(),
                    point(0.4),
                    point(0.5),
                    key.clone(),
                    scroll.clone(),
                    point(0.6),
                ]),
                lines(&[point(0.7)]),
                Vec::new(),
            ]
            .into(),
        );
        host.ready.extend([7, 7]);
        host.wire.results.extend([
            overload("rate-limited"),
            overload("busy"),
            ok(json!({"applied":6})),
            ok(json!({"released":true})),
        ]);
        assert!(
            state.advance(&mut host, 7).is_none(),
            "receive must remain open after transient overload"
        );
        let injects: Vec<_> = host
            .wire
            .sent
            .iter()
            .filter(|m| m["method"] == "inject")
            .collect();
        assert_eq!(injects.len(), 3);
        assert_eq!(
            injects[2]["args"]["events"],
            json!([point(0.3), press, point(0.5), key, scroll, point(0.7)])
        );
        assert_eq!(
            state.advance(&mut host, 7).unwrap()["result"]["status"],
            "ok"
        );
    }

    #[test]
    fn sustained_broker_overload_ends_replay_and_releases_held_input() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        host.wire
            .results
            .push_back(ok(json!({"settings":{"independent":false}})));
        state.receive(&mut host, 7, &json!({"peer":"pc"})).unwrap();
        host.streams.insert(
            7,
            [
                b"{\"t\":\"button\",\"button\":\"left\",\"down\":true}\n".to_vec(),
                b"{\"t\":\"scroll\",\"dx\":0,\"dy\":1}\n".to_vec(),
            ]
            .into(),
        );
        host.wire.results.push_back(ok(json!({"applied":1})));
        assert!(state.advance(&mut host, 7).is_none());
        host.wire
            .results
            .extend(std::iter::repeat_with(|| overload("busy")).take(5));
        host.wire.results.push_back(ok(json!({"released":true})));
        let failed = state.advance(&mut host, 7).unwrap();
        assert_eq!(failed["result"]["message"], answer::NOT_REPLAYED);
        let retried: Vec<_> = host
            .wire
            .sent
            .iter()
            .filter(|call| call["method"] == "inject")
            .skip(1)
            .collect();
        assert_eq!(retried.len(), 5);
        assert!(host.wire.waits.iter().filter(|&&ms| ms > 0).sum::<i32>() < 655);
        assert_eq!(
            host.wire
                .waits
                .iter()
                .filter(|&&ms| ms > 0 && ms != REPLAY_REPLY_MS)
                .count(),
            retried.len() - 1,
        );
        assert!(
            retried
                .iter()
                .all(|call| call["args"]["events"] == json!([{"t":"scroll","dx":0,"dy":1}]))
        );
        assert_eq!(
            host.wire.sent.last().unwrap()["args"],
            json!({"owner":"pc","keep":false})
        );
        assert_eq!(host.closed, [7]);
        assert!(state.incoming.is_empty());
    }

    #[test]
    fn a_missing_retry_reply_is_not_retried_and_cleanup_does_not_wait_indefinitely() {
        let mut host = Concurrent {
            withhold_quick_replies: true,
            ..Concurrent::default()
        };
        let mut state = State::default();
        host.wire
            .results
            .push_back(ok(json!({"settings":{"independent":false}})));
        state.receive(&mut host, 7, &json!({"peer":"pc"})).unwrap();
        host.streams.insert(
            7,
            [b"{\"t\":\"scroll\",\"dx\":0,\"dy\":1}\n".to_vec()].into(),
        );
        host.wire.results.push_back(overload("rate-limited"));
        let reply = state.advance(&mut host, 7).unwrap();
        let methods: Vec<_> = host
            .wire
            .sent
            .iter()
            .map(|call| call["method"].as_str().unwrap())
            .collect();
        assert_eq!(
            methods,
            ["get", "inject", "inject", "release"],
            "an unanswered inject may have run"
        );
        assert_eq!(reply["result"]["message"], answer::NOT_REPLAYED);
        assert!(host.wire.waits.iter().sum::<i32>() < 200);
        assert_eq!(host.closed, [7]);
    }

    #[test]
    fn cancellation_during_backoff_cannot_reapply_input_or_reply_to_a_gone_receive() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        host.wire
            .results
            .push_back(ok(json!({"settings":{"independent":false}})));
        state.receive(&mut host, 7, &json!({"peer":"pc"})).unwrap();
        host.streams.insert(
            7,
            [b"{\"t\":\"scroll\",\"dx\":0,\"dy\":1}\n".to_vec()].into(),
        );
        host.wire
            .deliveries
            .push_back(json!({"type":"cancelled","ticket":7}));
        host.wire
            .results
            .extend([overload("busy"), ok(json!({"released":true}))]);
        assert!(state.advance(&mut host, 7).is_none());
        assert_eq!(host.closed, [7]);
        assert!(state.incoming.is_empty());
        assert_eq!(
            host.wire
                .sent
                .iter()
                .filter(|call| call["method"] == "inject")
                .count(),
            1
        );
        assert_eq!(host.wire.sent.last().unwrap()["method"], "release");
    }

    #[test]
    fn a_cancellation_answered_with_an_applied_batch_stops_the_next_batch() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        host.wire
            .results
            .push_back(ok(json!({"settings":{"independent":false}})));
        state.receive(&mut host, 7, &json!({"peer":"pc"})).unwrap();
        // One read holds a batch and one event more.
        let events = "{\"t\":\"scroll\",\"dx\":0,\"dy\":1}\n".repeat(MOST_BATCH + 1);
        host.streams.insert(7, [events.into_bytes()].into());
        // The cancellation comes in while the first batch is being answered.
        host.wire
            .deliveries
            .push_back(json!({"type":"cancelled","ticket":7}));
        host.wire.results.extend([
            ok(json!({"applied":MOST_BATCH})),
            ok(json!({"released":true})),
        ]);
        assert!(state.advance(&mut host, 7).is_none());
        assert_eq!(
            host.wire
                .sent
                .iter()
                .filter(|call| call["method"] == "inject")
                .count(),
            1,
            "the event past the batch is never applied"
        );
        assert_eq!(host.wire.sent.last().unwrap()["method"], "release");
        assert!(state.incoming.is_empty() && state.handoffs.is_empty());
    }

    #[test]
    fn a_failed_background_rotation_is_reported_before_reopening_for_the_same_visit() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        host.wire.results.push_back(ok(json!({"claim":1})));
        let moving = json!({"peer":"pc","events":[{"t":"position","x":0.2,"y":0.3}],"at":1000});
        state.input(&mut host, &moving).unwrap();
        assert_eq!(
            state.next(&mut host),
            Ok(None),
            "the idle rotation got no ACK"
        );
        assert_eq!(state.input(&mut host, &moving), Err(answer::STOPPED.into()));
        assert_eq!(host.wire.opened, [(true, 1)], "no silent reconnect");
    }

    #[test]
    fn a_new_crossing_opens_a_new_copy_even_if_the_old_copy_is_still_open() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        host.wire
            .results
            .extend([ok(json!({"claim":1})), ok(json!({"claim":2}))]);
        let begin =
            json!({"peer":"pc","events":[{"t":"position","x":0.2,"y":0.3}],"at":1000,"begin":true});
        state.input(&mut host, &begin).unwrap();
        state.input(&mut host, &begin).unwrap();
        assert_eq!(host.wire.opened, [(true, 1), (true, 2)]);
        assert_eq!(
            host.closed,
            [1],
            "do not write the new visit into the old copy"
        );
    }

    #[test]
    fn forwarded_input_rotates_its_connection_by_the_applications_clock() {
        let mut host = Concurrent::default();
        host.wire.acks = true;
        let mut state = State::default();
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"claim":1}}));
        let batch = |at: u64| json!({"peer":"pc","events":[{"t":"key","key":"a"}],"at":at});
        state.input(&mut host, &batch(1_000)).unwrap();
        state
            .input(&mut host, &batch(1_000 + ROTATE_MS - 1))
            .unwrap();
        assert_eq!(
            host.wire.shutdowns, 0,
            "younger than the rotation age: kept open"
        );
        state.input(&mut host, &batch(1_000 + ROTATE_MS)).unwrap();
        assert_eq!(state.next(&mut host), Ok(None));
        assert_eq!(
            host.wire.shutdowns, 1,
            "old enough: rotated at the next scheduling turn"
        );
        assert!(state.outgoing.is_none());
        assert_eq!(
            String::from_utf8_lossy(&host.wire.written).lines().last(),
            Some(CONTINUES)
        );
        // The next batch opens a new connection, aged from its own first batch.
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"claim":2}}));
        state
            .input(&mut host, &batch(1_000 + ROTATE_MS + 10))
            .unwrap();
        assert_eq!(host.wire.opened, [(true, 1), (true, 2)]);
        assert_eq!(
            state.outgoing.as_ref().unwrap().opened_at,
            Some(1_010 + ROTATE_MS)
        );
    }

    #[test]
    fn unmeasured_readiness_rotates_a_forwarded_key_stream_before_dispatch() {
        for deferred in [false, true] {
            let mut host = Concurrent::default();
            host.wire.acks = true;
            let mut state = State::default();
            host.wire
                .results
                .push_back(json!({"status":"ok","value":{"claim":1}}));
            state.input(
                &mut host,
                &json!({"peer":"pc","at":1000,"events":[{"t":"key_code","code":42,"down":true}]}),
            ).unwrap();
            let other = json!({"type":"event","service":"unrelated"});
            host.wire.deliveries.push_back(other.clone());
            if deferred {
                host.wire
                    .results
                    .push_back(json!({"status":"ok","value":[]}));
                state.call(&mut host, PEERS, "list", json!({})).unwrap();
            }

            assert_eq!(state.next(&mut host), Ok(Some(other)));
            assert!(
                state.outgoing.is_none(),
                "unrelated work cannot extend the hold stream's wait"
            );
            assert_eq!(state.retained.as_deref(), Some("pc"));
            assert_eq!(
                String::from_utf8_lossy(&host.wire.written).lines().last(),
                Some(CONTINUES)
            );
        }
    }

    #[test]
    fn a_ready_end_or_switch_uses_the_live_stream_even_when_poll_selects_data() {
        for deferred in [false, true] {
            for (peer, end, events) in [
                ("pc", true, json!([])),
                ("pc", true, json!([{"t":"key_code","code":42,"down":false}])),
                ("next-pc", false, json!([{"t":"move","dx":1,"dy":0}])),
            ] {
                let mut host = Concurrent::default();
                host.wire.acks = true;
                let mut state = State::default();
                host.wire
                    .results
                    .push_back(json!({"status":"ok","value":{"settings":{"independent":false}}}));
                state
                    .receive(&mut host, 1, &json!({"peer":"controller"}))
                    .unwrap();
                host.wire
                    .results
                    .push_back(json!({"status":"ok","value":{"claim":2}}));
                state.input(
                &mut host,
                &json!({"peer":"pc","at":1000,"events":[{"t":"key_code","code":42,"down":true}]}),
            ).unwrap();
                host.streams
                    .insert(1, VecDeque::from([b"{\"t\":\"move\",\"dx\":".to_vec()]));
                host.ready.push_back(1);
                let delivery = json!({
                    "type":"call","ticket":3,"method":"input",
                    "args":{"peer":peer,"at":1200,"end":end,"events":events}
                });
                host.wire.deliveries.push_back(delivery.clone());
                if !end {
                    host.wire
                        .results
                        .push_back(json!({"status":"ok","value":{"claim":4}}));
                }
                if deferred {
                    let other = json!({"type":"event","service":"unrelated"});
                    state.keep(other.clone()).unwrap();
                    assert_eq!(state.next(&mut host), Ok(Some(other)));
                    assert_eq!(
                        host.wire.shutdowns, 0,
                        "an earlier deferred delivery cannot hide the pending end"
                    );
                }

                assert_eq!(state.next(&mut host), Ok(Some(delivery.clone())));
                assert_eq!(
                    host.wire.shutdowns, 0,
                    "the live stream must not rotate first"
                );
                assert_eq!(
                    host.streams[&1].len(),
                    0,
                    "suppressing rotation does not defer the selected read"
                );
                assert_eq!(
                    super::handle(&mut host, &mut state, &delivery).unwrap()["result"]["status"],
                    "ok"
                );
                assert_eq!(host.closed, [2]);
                assert!(
                    !String::from_utf8_lossy(&host.wire.written)
                        .lines()
                        .any(|line| line == CONTINUES)
                );
                let expected = if end {
                    vec![(true, 2)]
                } else {
                    vec![(true, 2), (true, 4)]
                };
                assert_eq!(
                    host.wire.opened, expected,
                    "never reconnect to the old target to release"
                );
            }
        }
    }

    #[test]
    fn ready_inbound_input_progresses_during_repeated_outgoing_ends() {
        let mut host = Concurrent::default();
        host.wire.acks = true;
        let mut state = State::default();
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"settings":{"independent":false}}}));
        state
            .receive(&mut host, 1, &json!({"peer":"controller"}))
            .unwrap();
        host.streams.insert(
            1,
            VecDeque::from([
                b"{\"t\":\"button\",\"button\":\"left\",\"down\":true}\n".to_vec(),
                b"{\"t\":\"button\",".to_vec(),
                b"\"button\":\"left\",".to_vec(),
                b"\"down\":false}\n".to_vec(),
            ]),
        );
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"applied":1}}));
        host.ready.push_back(1);
        assert_eq!(state.next(&mut host), Ok(None));

        // The mixed wait can select control to start each session, then data ahead of
        // the ready end. The inbound release spans those three reads before its 15 s limit.
        for (claim, at) in [(10, 1000), (11, 5000), (12, 10_000)] {
            let start = json!({
                "type":"call","ticket":claim * 2,"method":"input",
                "args":{"peer":"pc","at":at,"events":[{"t":"key_code","code":42,"down":true}]}
            });
            host.wire.deliveries.push_back(start.clone());
            host.wire
                .results
                .push_back(json!({"status":"ok","value":{"claim":claim}}));
            assert_eq!(state.next(&mut host), Ok(Some(start.clone())));
            assert_eq!(
                super::handle(&mut host, &mut state, &start).unwrap()["result"]["status"],
                "ok"
            );

            let end = json!({
                "type":"call","ticket":claim * 2 + 1,"method":"input",
                "args":{"peer":"pc","at":at + 100,"events":[],"end":true}
            });
            host.wire.deliveries.push_back(end.clone());
            if claim == 12 {
                host.wire
                    .results
                    .push_back(json!({"status":"ok","value":{"applied":1}}));
            }
            host.ready.push_back(1);
            assert_eq!(state.next(&mut host), Ok(Some(end.clone())));
            assert_eq!(
                super::handle(&mut host, &mut state, &end).unwrap()["result"]["status"],
                "ok"
            );
        }

        let injected: Vec<_> = host
            .wire
            .sent
            .iter()
            .filter(|message| message["method"] == "inject")
            .map(|message| message["args"].clone())
            .collect();
        assert_eq!(
            injected,
            [
                json!({"owner":"controller","events":[{"t":"button","button":"left","down":true}]}),
                json!({"owner":"controller","events":[{"t":"button","button":"left","down":false}]}),
            ],
            "the receiver must replay the actual release without waiting for cancellation"
        );
        assert_eq!(
            host.closed,
            [10, 11, 12],
            "only the outgoing sessions ended"
        );
        assert!(
            host.wire
                .sent
                .iter()
                .all(|message| message["method"] != "release")
        );
    }

    #[test]
    fn refusing_a_replacement_releases_the_input_its_predecessor_handed_over() {
        for after_eof in [true, false] {
            for (config, inspect, attachment_fails, expected) in [
                (
                    json!({"status":"error","code":"denied"}),
                    None,
                    false,
                    answer::DENIED,
                ),
                (
                    json!({"status":"ok","value":{"settings":{"name-tag":"invalid"}}}),
                    None,
                    false,
                    answer::NOT_UNDERSTOOD,
                ),
                (
                    json!({"status":"ok","value":{"settings":{}}}),
                    Some(json!({"status":"error","code":"failed"})),
                    false,
                    answer::REFUSED,
                ),
                (
                    json!({"status":"ok","value":{"settings":{"independent":false}}}),
                    None,
                    true,
                    answer::NOT_UNDERSTOOD,
                ),
            ] {
                let mut host = Concurrent::default();
                let mut state = State::default();
                host.wire
                    .results
                    .push_back(json!({"status":"ok","value":{"settings":{"independent":false}}}));
                state
                    .receive(&mut host, 1, &json!({"peer":"controller"}))
                    .unwrap();
                host.streams.insert(
                    1,
                    VecDeque::from([
                        format!(
                            "{{\"t\":\"button\",\"button\":\"left\",\"down\":true}}\n{CONTINUES}\n"
                        )
                        .into_bytes(),
                        Vec::new(),
                    ]),
                );
                host.wire
                    .results
                    .push_back(json!({"status":"ok","value":{"applied":1}}));
                host.ready.push_back(1);
                assert_eq!(state.next(&mut host), Ok(None));
                if after_eof {
                    host.ready.push_back(1);
                    assert_eq!(state.next(&mut host), Ok(None));
                }
                let release_count = |host: &Concurrent| {
                    host.wire
                        .sent
                        .iter()
                        .filter(|m| m["method"] == "release")
                        .count()
                };
                assert_eq!(release_count(&host), 0);

                host.wire.results.push_back(config);
                host.wire.results.extend(inspect);
                host.refuse_ticket = attachment_fails.then_some(2);
                host.wire
                    .results
                    .push_back(json!({"status":"ok","value":{"released":true}}));
                let reply = super::handle(&mut host, &mut state,
                    &json!({"type":"call","ticket":2,"method":"receive","args":{"peer":"controller"}})).unwrap();
                assert_eq!(reply["result"]["message"], expected);
                if !after_eof {
                    assert_eq!(
                        release_count(&host),
                        0,
                        "do not release an active predecessor"
                    );
                    host.ready.push_back(1);
                    assert_eq!(state.next(&mut host), Ok(None));
                }
                assert_eq!(
                    release_count(&host),
                    1,
                    "release inherited input on refusal, after_eof={after_eof}"
                );
                let release = host
                    .wire
                    .sent
                    .iter()
                    .find(|m| m["method"] == "release")
                    .unwrap();
                assert_eq!(release["args"], json!({"owner":"controller","keep":false}));
                assert_eq!(host.closed, [1]);
            }
        }
    }

    #[test]
    fn a_refused_release_of_inherited_input_stays_pending_and_is_tried_again() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"settings":{"independent":false}}}));
        state
            .receive(&mut host, 1, &json!({"peer":"controller"}))
            .unwrap();
        host.streams.insert(
            1,
            VecDeque::from([
                format!("{{\"t\":\"button\",\"button\":\"left\",\"down\":true}}\n{CONTINUES}\n")
                    .into_bytes(),
                Vec::new(),
            ]),
        );
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"applied":1}}));
        for _ in 0..2 {
            host.ready.push_back(1);
            assert_eq!(state.next(&mut host), Ok(None));
        }
        assert!(state.handoffs.contains("controller"));
        let releases = |host: &Concurrent| {
            host.wire
                .sent
                .iter()
                .filter(|m| m["method"] == "release")
                .count()
        };
        // A refused replacement asks to release the inherited press; the host refuses that cleanup.
        host.wire.results.extend([
            json!({"status":"error","code":"denied"}),
            json!({"status":"error","code":"failed"}),
        ]);
        let refused = |host: &mut Concurrent, state: &mut State, ticket: u64| {
            super::handle(host, state,
                &json!({"type":"call","ticket":ticket,"method":"receive","args":{"peer":"controller"}})).unwrap()
        };
        assert_eq!(
            refused(&mut host, &mut state, 2)["result"]["message"],
            answer::DENIED
        );
        assert_eq!(releases(&host), 1);
        assert!(
            state.handoffs.contains("controller"),
            "the hold is still owed a release"
        );
        // The next refused receive from that device releases it, and only then is the record gone.
        host.wire.results.extend([
            json!({"status":"error","code":"denied"}),
            json!({"status":"ok","value":{"released":true}}),
        ]);
        assert_eq!(
            refused(&mut host, &mut state, 3)["result"]["message"],
            answer::DENIED
        );
        assert_eq!(releases(&host), 2);
        assert!(state.handoffs.is_empty());
    }

    #[test]
    fn a_cancelled_queued_replacement_does_not_accept_a_later_handoff() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        for ticket in [1, 2] {
            host.wire
                .results
                .push_back(json!({"status":"ok","value":{"settings":{"independent":false}}}));
            state
                .receive(&mut host, ticket, &json!({"peer":"controller"}))
                .unwrap();
        }
        host.streams.insert(
            1,
            VecDeque::from([
                format!("{{\"t\":\"button\",\"button\":\"left\",\"down\":true}}\n{CONTINUES}\n")
                    .into_bytes(),
                Vec::new(),
            ]),
        );
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"applied":1}}));
        host.ready.push_back(1);
        assert_eq!(state.next(&mut host), Ok(None));
        super::handle(
            &mut host,
            &mut state,
            &json!({"type":"cancelled","ticket":2}),
        );
        assert_eq!(host.closed, [2]);
        assert!(host.wire.sent.iter().all(|m| m["method"] != "release"));

        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"released":true}}));
        host.ready.push_back(1);
        assert_eq!(state.next(&mut host), Ok(None));
        let release = host
            .wire
            .sent
            .iter()
            .find(|m| m["method"] == "release")
            .expect("a broken handoff must release its predecessor");
        assert_eq!(release["args"], json!({"owner":"controller","keep":false}));
        let last = host.wire.sent.last().unwrap();
        assert_eq!(last["ticket"], 1);
        assert_eq!(last["result"]["status"], "ok");
    }

    fn assert_accepted_replacement_inherits_the_held_button(cancel_queued: bool) {
        let mut host = Concurrent::default();
        let mut state = State::default();
        let accepted = || json!({"status":"ok","value":{"settings":{"independent":false}}});
        let receive = |ticket| {
            json!({
                "type":"call","ticket":ticket,"method":"receive","args":{"peer":"controller"}
            })
        };
        host.wire.results.push_back(accepted());
        assert!(super::handle(&mut host, &mut state, &receive(1)).is_none());
        host.streams.insert(
            1,
            VecDeque::from([
                format!("{{\"t\":\"button\",\"button\":\"left\",\"down\":true}}\n{CONTINUES}\n")
                    .into_bytes(),
                Vec::new(),
            ]),
        );
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"applied":1}}));
        host.ready.push_back(1);
        assert_eq!(state.next(&mut host), Ok(None));
        assert_eq!(
            host.wire.sent.last().unwrap()["args"]["events"][0]["down"],
            true
        );

        if cancel_queued {
            host.wire.results.push_back(accepted());
            assert!(super::handle(&mut host, &mut state, &receive(2)).is_none());
        } else {
            host.wire
                .results
                .push_back(json!({"status":"error","code":"denied"}));
            let refused = super::handle(&mut host, &mut state, &receive(2)).unwrap();
            assert_eq!(refused["result"]["message"], answer::DENIED);
        }
        host.wire.results.push_back(accepted());
        assert!(super::handle(&mut host, &mut state, &receive(3)).is_none());
        if cancel_queued {
            assert!(
                super::handle(
                    &mut host,
                    &mut state,
                    &json!({"type":"cancelled","ticket":2})
                )
                .is_none()
            );
            assert_eq!(host.closed, [2]);
        }
        assert!(
            host.wire
                .sent
                .iter()
                .all(|message| message["method"] != "release")
        );
        host.ready.push_back(1);
        assert_eq!(state.next(&mut host), Ok(None));
        assert_eq!(host.closed.last().copied(), Some(1));
        assert!(
            host.wire
                .sent
                .iter()
                .all(|message| message["method"] != "release"),
            "a failed candidate must not cut the hold when a valid successor exists"
        );
        assert_eq!(host.wire.sent.last().unwrap()["result"]["status"], "ok");

        host.streams.insert(
            3,
            VecDeque::from([b"{\"t\":\"move\",\"dx\":1,\"dy\":0}\n".to_vec(), Vec::new()]),
        );
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"applied":1}}));
        host.ready.push_back(3);
        assert_eq!(state.next(&mut host), Ok(None));
        assert_eq!(
            host.wire.sent.last().unwrap()["args"],
            json!({"owner":"controller","events":[{"t":"move","dx":1,"dy":0}]})
        );
        assert!(
            host.wire
                .sent
                .iter()
                .all(|message| message["method"] != "release"),
            "the successor moves with the original press held"
        );

        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"released":true}}));
        host.ready.push_back(3);
        assert_eq!(state.next(&mut host), Ok(None));
        assert_eq!(host.closed.last().copied(), Some(3));
        let releases: Vec<_> = host
            .wire
            .sent
            .iter()
            .filter(|message| message["method"] == "release")
            .collect();
        assert_eq!(releases.len(), 1);
        assert_eq!(
            releases[0]["args"],
            json!({"owner":"controller","keep":false})
        );
    }

    #[test]
    fn an_accepted_replacement_inherits_held_input_after_an_earlier_refusal() {
        assert_accepted_replacement_inherits_the_held_button(false);
    }

    #[test]
    fn an_accepted_replacement_inherits_held_input_when_an_earlier_queue_entry_is_cancelled() {
        assert_accepted_replacement_inherits_the_held_button(true);
    }

    #[test]
    fn disconnect_exits_cleanly_but_an_invalid_host_message_fails_the_guest() {
        for (receive_error, exit) in [("The host disconnected", 0), ("Invalid host response", 1)] {
            let mut host = Concurrent {
                receive_error: Some(receive_error),
                ..Concurrent::default()
            };
            for value in [
                json!({"settings":{},"missing_permissions":[],"peers":[]}),
                json!({}),
            ] {
                host.wire
                    .results
                    .push_back(json!({"status":"ok","value":value}));
            }
            assert_eq!(crate::run(&mut host), exit, "{receive_error}");
            assert_eq!(
                host.wire.sent.first(),
                Some(&crate::host::subscribe_config())
            );
        }
    }

    #[test]
    fn a_queued_delivery_gets_a_turn_before_a_readable_stream_drains() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{"settings":{"independent":false}}}));
        state
            .receive(&mut host, 1, &json!({"peer":"controller"}))
            .unwrap();
        host.streams.insert(
            1,
            VecDeque::from([
                b"{\"t\":\"move\",\"dx\":1,\"dy\":0}\n".to_vec(),
                b"{\"t\":\"move\",\"dx\":2,\"dy\":0}\n".to_vec(),
            ]),
        );
        host.ready.extend([1, 1]);
        let deferred = json!({"type":"cancelled","ticket":2});
        // The delivery reaches the guest while it awaits a result of its own.
        host.wire.deliveries.push_back(deferred.clone());
        for _ in 0..3 {
            host.wire
                .results
                .push_back(json!({"status":"ok","value":{}}));
        }
        state.call(&mut host, PEERS, "list", json!({})).unwrap();

        assert_eq!(state.next(&mut host), Ok(Some(deferred)));
        assert_eq!(host.wire.sent.last().unwrap()["args"]["events"][0]["dx"], 1);
        assert_eq!(state.next(&mut host), Ok(None));
        assert_eq!(host.wire.sent.last().unwrap()["args"]["events"][0]["dx"], 2);
    }

    #[test]
    fn cancelling_a_queued_replacement_does_not_release_the_active_streams_owner() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        for ticket in [1, 2] {
            host.wire
                .results
                .push_back(json!({"status":"ok","value":{"settings":{"independent":false}}}));
            state
                .receive(&mut host, ticket, &json!({"peer":"controller"}))
                .unwrap();
        }
        host.streams.insert(
            1,
            VecDeque::from([b"{\"t\":\"button\",\"button\":\"left\",\"down\":true}\n".to_vec()]),
        );
        host.ready.push_back(1);
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{}}));
        assert_eq!(state.next(&mut host), Ok(None));
        assert_eq!(
            host.wire.sent.last().unwrap()["args"]["events"][0]["down"],
            true
        );

        for (ticket, releases) in [(2, 0), (1, 1)] {
            if releases > 0 {
                host.wire
                    .results
                    .push_back(json!({"status":"ok","value":{}}));
            }
            let cancellation = json!({"type":"cancelled","ticket":ticket});
            assert!(super::handle(&mut host, &mut state, &cancellation).is_none());
            assert_eq!(host.closed.last().copied(), Some(ticket as Handle));
            let release_calls: Vec<_> = host
                .wire
                .sent
                .iter()
                .filter(|message| message["method"] == "release")
                .collect();
            assert_eq!(release_calls.len(), releases);
            if let Some(release) = release_calls.first() {
                assert_eq!(release["args"], json!({"owner":"controller","keep":false}));
            }
        }
    }

    #[test]
    fn a_readable_stream_is_not_read_when_its_cancellation_is_already_queued() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        host.wire
            .results
            .push_back(ok(json!({"settings":{"independent":false}})));
        state
            .receive(&mut host, 1, &json!({"peer":"controller"}))
            .unwrap();
        host.streams.insert(
            1,
            VecDeque::from([b"{\"t\":\"key\",\"key\":\"enter\"}\n".to_vec()]),
        );
        host.ready.push_back(1);
        let cancel = json!({"type":"cancelled","ticket":1});
        state.waiting.push(cancel.clone());
        // The existing cancellation handler releases the owner once it receives the delivery.
        host.wire.results.push_back(ok(json!({})));
        assert_eq!(state.next(&mut host), Ok(Some(cancel.clone())));
        assert_eq!(host.streams[&1].len(), 1);
        assert!(super::handle(&mut host, &mut state, &cancel).is_none());
        assert_eq!(host.closed, [1]);
        assert!(
            host.wire
                .sent
                .iter()
                .all(|call| call["method"] != "inject" && call["op"] != "reply")
        );
    }

    #[test]
    fn second_controller_moves_before_first_eof_and_each_keeps_its_partial_line_and_owner() {
        let mut host = Concurrent::default();
        let mut state = State::default();
        for ticket in [1, 2] {
            host.wire
                .results
                .push_back(json!({"status":"ok","value":{"settings":{"independent":true}}}));
            host.wire
                .results
                .push_back(json!({"status":"ok","value":{"independent_pointer":true}}));
            assert!(super::handle(&mut host, &mut state,
                &json!({"type":"call","ticket":ticket,"method":"receive","args":{"peer":format!("controller-{ticket}")}})).is_none());
        }
        host.streams.insert(
            1,
            VecDeque::from([
                b"{\"t\":\"move\",\"dx\":".to_vec(),
                b"1,\"dy\":0}\n".to_vec(),
                Vec::new(),
            ]),
        );
        host.streams.insert(
            2,
            VecDeque::from([b"{\"t\":\"move\",\"dx\":2,\"dy\":0}\n".to_vec()]),
        );
        host.ready.extend([1, 2, 1, 1]);
        for _ in 0..3 {
            host.wire
                .results
                .push_back(json!({"status":"ok","value":{}}));
        }
        assert!(state.next(&mut host).unwrap().is_none());
        assert!(state.next(&mut host).unwrap().is_none());
        let injects: Vec<_> = host
            .wire
            .sent
            .iter()
            .filter(|m| m["method"] == "inject")
            .collect();
        assert_eq!(injects.len(), 1);
        assert_eq!(injects[0]["args"]["owner"], "controller-2");
        assert_eq!(injects[0]["args"]["events"][0]["dx"], 2);
        assert_eq!(injects[0]["args"]["pointer"], "own");
        assert!(
            host.closed.is_empty(),
            "second controller must not wait for first EOF"
        );
        state.next(&mut host).unwrap();
        let injects: Vec<_> = host
            .wire
            .sent
            .iter()
            .filter(|m| m["method"] == "inject")
            .collect();
        assert_eq!(injects[1]["args"]["owner"], "controller-1");
        assert_eq!(injects[1]["args"]["events"][0]["dx"], 1);
        state.next(&mut host).unwrap();
        assert_eq!(host.closed, [1]);
        let release = host
            .wire
            .sent
            .iter()
            .find(|m| m["method"] == "release")
            .unwrap();
        assert_eq!(release["args"]["owner"], "controller-1");
        assert_eq!(release["args"]["keep"], false);
        assert!(
            state.incoming.contains_key(&2),
            "one peer ending must not close the other"
        );
        let reply = host.wire.sent.iter().find(|m| m["op"] == "reply").unwrap();
        assert_eq!(reply["ticket"], 1);
        assert_eq!(reply["result"]["status"], "ok");
        host.wire
            .results
            .push_back(json!({"status":"ok","value":{}}));
        let replies = host.wire.sent.iter().filter(|m| m["op"] == "reply").count();
        assert!(
            super::handle(
                &mut host,
                &mut state,
                &json!({"type":"cancelled","ticket":2})
            )
            .is_none()
        );
        assert_eq!(host.closed, [1, 2]);
        assert!(state.incoming.is_empty());
        let release = host
            .wire
            .sent
            .iter()
            .rev()
            .find(|m| m["method"] == "release")
            .unwrap();
        assert_eq!(release["args"]["owner"], "controller-2");
        assert_eq!(
            host.wire.sent.iter().filter(|m| m["op"] == "reply").count(),
            replies,
            "cancelled tickets must not receive a late reply"
        );
    }
}
