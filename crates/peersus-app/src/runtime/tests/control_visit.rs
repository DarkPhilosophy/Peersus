//! Control's real shape between two paired owners: real QUIC between the owners, the owner's own
//! forwarding, visits, copies, acknowledgements and replay, and a guest on each side that keeps
//! Control's own protocol (see `plugins/peersus-control/wasm/src/remote.rs`): one data connection
//! carries many batches and is rotated after 200 batches, the acknowledgement of the closed one is
//! waited for before the next is opened, a receive is accepted first and then read a chunk at a time.
use super::*;
use crate::{
    desktop_input::{DesktopInput, Device, Raw},
    platform_services::{
        Call, INPUT_CAPABILITY, INPUT_SERVICE, InputLease, PlatformServices, Response,
    },
};
use peersus_api::{
    contributions::{EdgePressure, Input},
    manifest::Import,
    message::Outcome,
};
use peersus_plugin::{
    data::DataHello,
    frame,
    link::Hello,
    supervisor::{Bootstrap, Launcher, Lifecycle},
    transport,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Instant,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SERVICE: &str = "control.pointer";
const ROTATE_AFTER: u32 = 200;
const ROTATE_MS: u64 = 5_000;
const ACK_MS: u64 = 3_000;
const CALL_MS: u32 = 4_000;
const MOST_WAITING: usize = 64;
const MOST_BATCH: usize = 64;

/// What the controlled computer's pointer was told, with when.
#[derive(Clone, Default)]
struct Pointer(Arc<std::sync::Mutex<Vec<Instant>>>);
impl Device for Pointer {
    fn emit(&mut self, events: &[Raw]) -> std::io::Result<()> {
        if !events.is_empty()
            && let Ok(mut moved) = self.0.lock()
        {
            moved.push(Instant::now());
        }
        Ok(())
    }
}
impl Pointer {
    fn moves(&self) -> Vec<Instant> {
        self.0.lock().unwrap().clone()
    }
}

struct Replay(DesktopInput);
impl PlatformServices for Replay {
    fn dispatch(&self, call: Call) -> Response {
        self.0.dispatch(call)
    }
    fn dispatch_input(&self, call: Call, lease: Option<InputLease>) -> Response {
        self.0.dispatch_leased(call, lease)
    }
    fn release_peer_input(&self, caller: &PluginId, owner: &str) {
        self.0.release_peer(caller, owner);
    }
    fn revoke(&self, caller: &PluginId) {
        self.0.revoke(caller);
    }
}

struct FixtureGuest {
    control: transport::Stream,
    bootstrap: Bootstrap,
}
struct FixtureLauncher(mpsc::Sender<FixtureGuest>);
struct FixtureProcess(tokio::task::JoinHandle<()>);
impl Launcher for FixtureLauncher {
    fn launch(&self, bootstrap: Bootstrap) -> std::io::Result<Box<dyn Lifecycle>> {
        let linked = self.0.clone();
        Ok(Box::new(FixtureProcess(tokio::spawn(async move {
            let mut control = transport::connect(&bootstrap.socket).await.unwrap();
            frame::write(
                &mut control,
                &Hello {
                    token: bootstrap.token.clone(),
                },
                1024,
            )
            .await
            .unwrap();
            if linked
                .send(FixtureGuest { control, bootstrap })
                .await
                .is_ok()
            {
                std::future::pending::<()>().await;
            }
        }))))
    }
}
impl Lifecycle for FixtureProcess {
    fn wait(&mut self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            let _ = (&mut self.0).await;
        })
    }
    fn stop(&mut self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.0.abort();
        self.wait()
    }
}
impl Drop for FixtureProcess {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Why something was refused, as Control tells them apart.
#[derive(Debug, Clone, PartialEq)]
enum Refusal {
    NotAccepted,
    Missing(Permission),
    Other(String),
}
fn other(text: &str) -> Refusal {
    Refusal::Other(text.to_owned())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ended {
    Closed,
    Continues,
    Rests,
}

struct Outgoing {
    device: String,
    stream: transport::Stream,
    batches: u32,
    sent: u64,
    opened_at: Option<u64>,
    latest_at: u64,
}
impl Outgoing {
    fn record_time(&mut self, at: u64) {
        self.opened_at.get_or_insert(at);
        self.latest_at = self.latest_at.max(at);
    }
    fn rotation_wait_ms(&self) -> u64 {
        if self.batches >= ROTATE_AFTER {
            return 0;
        }
        let age = self.opened_at.map_or(0, |opened| self.latest_at - opened);
        ROTATE_MS.saturating_sub(age)
    }
    fn forwarded_args(delivery: &Delivery) -> Option<&Value> {
        match delivery {
            Delivery::Call { method, args, .. } if method == "input" => Some(args),
            _ => None,
        }
    }
    fn finishes_on(&self, delivery: &Delivery) -> bool {
        let Some(args) = Self::forwarded_args(delivery) else {
            return false;
        };
        let Some(peer) = args["peer"].as_str().filter(|_| args["events"].is_array()) else {
            return false;
        };
        peer != self.device || args["end"] == true
    }
    fn measure_delivery(&mut self, delivery: &Delivery) -> bool {
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

struct Incoming {
    owner: String,
    stream: transport::Stream,
    pending: Vec<u8>,
    ended: Ended,
}

/// What the test reads off a guest afterwards.
#[derive(Default)]
struct Probe {
    /// How long each closed data connection waited for its acknowledgement.
    acks: std::sync::Mutex<Vec<Duration>>,
    /// Every refusal or failure the guest met, as it was told.
    problems: std::sync::Mutex<Vec<String>>,
    /// Connections opened for sending, and receives accepted.
    opened: std::sync::atomic::AtomicUsize,
    received: std::sync::atomic::AtomicUsize,
    started: Option<Instant>,
}
impl Probe {
    fn problem(&self, what: impl Into<String>) {
        let at = self
            .started
            .map_or(0, |started| started.elapsed().as_millis());
        self.problems
            .lock()
            .unwrap()
            .push(format!("{at} ms: {}", what.into()));
    }
}

/// One extension process, the way Control behaves on a computer: it both controls and is controlled.
struct Control {
    write: tokio::net::unix::OwnedWriteHalf,
    inbox: mpsc::UnboundedReceiver<Delivery>,
    bootstrap: Bootstrap,
    waiting: VecDeque<Delivery>,
    next_id: u64,
    outgoing: Option<Outgoing>,
    retained: Option<String>,
    incoming: BTreeMap<u64, Incoming>,
    handoffs: BTreeSet<String>,
    remote_denials: BTreeSet<String>,
    early_end: Option<(String, Refusal)>,
    probe: Arc<Probe>,
}

fn blocked(result: &Outcome) -> Refusal {
    match result {
        Outcome::NotAccepted {} => Refusal::NotAccepted,
        Outcome::MissingPermission { permission } => Refusal::Missing(permission.clone()),
        Outcome::Error { code, message } => Refusal::Other(format!("{code}: {message}")),
        Outcome::Ok { .. } => other("ok is not a refusal"),
    }
}

impl Control {
    fn start(guest: FixtureGuest, probe: Arc<Probe>) -> Self {
        let (mut read, write) = guest.control.into_split();
        let (queue, inbox) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok(delivery) = frame::read::<Delivery>(&mut read, frame::CONTROL_LIMIT).await
            {
                if queue.send(delivery).is_err() {
                    break;
                }
            }
        });
        Self {
            write,
            inbox,
            bootstrap: guest.bootstrap,
            waiting: VecDeque::new(),
            next_id: 0,
            outgoing: None,
            retained: None,
            incoming: BTreeMap::new(),
            handoffs: BTreeSet::new(),
            remote_denials: BTreeSet::new(),
            early_end: None,
            probe,
        }
    }

    async fn submit(&mut self, operation: Operation) {
        frame::write(&mut self.write, &operation, frame::CONTROL_LIMIT)
            .await
            .unwrap();
    }

    async fn data(&self, ticket: u64, claim: Option<u64>) -> transport::Stream {
        let mut stream = transport::connect(&self.bootstrap.data_socket)
            .await
            .unwrap();
        frame::write(
            &mut stream,
            &DataHello {
                token: self.bootstrap.token.clone(),
                ticket,
                claim,
            },
            1024,
        )
        .await
        .unwrap();
        stream
    }

    /// Anything that is a delivery is put aside to be handled afterwards; one too many is refused.
    fn keep(&mut self, delivery: Delivery) -> Result<(), Refusal> {
        match delivery {
            Delivery::Call { .. } | Delivery::Cancelled { .. } | Delivery::Event { .. }
                if self.waiting.len() < MOST_WAITING =>
            {
                self.waiting.push_back(delivery);
                Ok(())
            }
            Delivery::Call { .. } | Delivery::Cancelled { .. } | Delivery::Event { .. } => {
                Err(other("That device cannot be controlled"))
            }
            Delivery::Result { .. } => Ok(()),
        }
    }

    async fn call(&mut self, service: &str, method: &str, args: Value) -> Result<Value, Refusal> {
        self.next_id += 1;
        let id = self.next_id;
        self.submit(Operation::Call {
            id,
            provider: HOST.to_owned(),
            service: service.to_owned(),
            version: 1,
            method: method.to_owned(),
            args,
            timeout_ms: CALL_MS,
        })
        .await;
        loop {
            let message = self.inbox.recv().await.ok_or_else(|| other("closed"))?;
            if let Delivery::Result { id: got, result } = &message
                && *got == id
            {
                return match result {
                    Outcome::Ok { value } => Ok(value.clone()),
                    refused => Err(blocked(refused)),
                };
            }
            self.keep(message)?;
        }
    }

    fn remember_remote_no(&mut self, device: &str) {
        self.remote_denials.insert(device.to_owned());
        if self.retained.as_deref() == Some(device) {
            self.retained = None;
        }
    }

    async fn connect(&mut self, device: &str) -> Result<Outgoing, Refusal> {
        if self.remote_denials.contains(device) {
            return Err(Refusal::NotAccepted);
        }
        let sent = self
            .call(
                PEERS_SERVICE,
                "send",
                json!({"device":device,"service":SERVICE,"version":1}),
            )
            .await;
        if matches!(&sent, Err(Refusal::NotAccepted)) {
            self.remember_remote_no(device);
        }
        let sent = sent?;
        let claim = sent["claim"].as_u64().ok_or_else(|| other("no claim"))?;
        let stream = self.data(0, Some(claim)).await;
        self.probe
            .opened
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Outgoing {
            device: device.to_owned(),
            stream,
            batches: 0,
            sent: 0,
            opened_at: None,
            latest_at: 0,
        })
    }

    /// Wait for the acknowledgement of a closed connection and read it. Deliveries that come first
    /// are kept, and the wait is `ACK_MS` since anything last arrived.
    async fn settle(&mut self, outgoing: Outgoing, written: bool) -> Result<(), Refusal> {
        let began = Instant::now();
        let Outgoing {
            device,
            mut stream,
            sent,
            ..
        } = outgoing;
        let acknowledgement = {
            let read = frame::read::<Value>(&mut stream, 1024);
            tokio::pin!(read);
            let mut deadline = Box::pin(tokio::time::sleep(Duration::from_millis(ACK_MS)));
            loop {
                tokio::select! {
                    biased;
                    result = &mut read => break result.map_err(|_| other("The other device stopped listening")),
                    message = self.inbox.recv() => {
                        match message {
                            Some(message) => {
                                if let Err(reason) = self.keep(message) { break Err(reason); }
                                deadline = Box::pin(tokio::time::sleep(Duration::from_millis(ACK_MS)));
                            }
                            None => break Err(other("closed")),
                        }
                    }
                    () = &mut deadline => break Err(other("The other device stopped listening")),
                }
            }
        };
        self.probe.acks.lock().unwrap().push(began.elapsed());
        match acknowledgement {
            Ok(value) if value.as_u64() == Some(sent) && written => Ok(()),
            Ok(value) if value.is_u64() => Err(other("The other device stopped listening")),
            Ok(_) => {
                self.remember_remote_no(&device);
                Err(Refusal::NotAccepted)
            }
            Err(reason) => Err(reason),
        }
    }

    async fn close(&mut self, mut outgoing: Outgoing) -> Result<(), Refusal> {
        let written = outgoing.stream.shutdown().await.is_ok();
        self.settle(outgoing, written).await
    }

    async fn pause(&mut self, marker: &str) -> Result<(), Refusal> {
        let Some(mut outgoing) = self.outgoing.take() else {
            return Ok(());
        };
        self.retained = Some(outgoing.device.clone());
        let mut line = marker.as_bytes().to_vec();
        line.push(b'\n');
        if outgoing.stream.write_all(&line).await.is_err() {
            return self.settle(outgoing, false).await;
        }
        outgoing.sent += line.len() as u64;
        self.close(outgoing).await
    }

    async fn rotate(&mut self) -> Result<(), Refusal> {
        self.pause(r#"{"continues":true}"#).await
    }

    async fn finish(&mut self) -> Result<(), Refusal> {
        let early_end = self.early_end.take();
        let retained = self.retained.take();
        if let Some(outgoing) = self.outgoing.take() {
            return self.close(outgoing).await;
        }
        let Some(device) = retained else {
            return early_end.map_or(Ok(()), |(_, reason)| Err(reason));
        };
        if let Ok(mut outgoing) = self.connect(&device).await {
            let _ = outgoing.stream.shutdown().await;
        }
        Ok(())
    }

    fn allow_new_choice(&mut self, device: &str) {
        self.remote_denials.remove(device);
        if self
            .early_end
            .as_ref()
            .is_some_and(|(peer, _)| peer == device)
        {
            self.early_end = None;
        }
    }

    /// `input`: write a batch to the device being controlled, opening a connection when there is none.
    async fn input(&mut self, args: &Value) -> Result<(), Refusal> {
        let device = args["peer"]
            .as_str()
            .ok_or_else(|| other("no peer"))?
            .to_owned();
        let events = args["events"]
            .as_array()
            .ok_or_else(|| other("no events"))?;
        let end = args["end"].as_bool().unwrap_or(false);
        if let Some(stop) = args.get("stop") {
            let _ = stop;
            self.remember_remote_no(&device);
            if let Some(out) = self.outgoing.take()
                && out.device == device
            {
                drop(out);
            }
            self.retained = None;
            return Ok(());
        }
        let begin = args.get("begin");
        if begin.is_none()
            && self
                .early_end
                .as_ref()
                .is_some_and(|(peer, _)| peer == &device)
            && let Some((_, reason)) = self.early_end.take()
        {
            return Err(reason);
        }
        if begin.is_none() && self.remote_denials.contains(&device) {
            return if events.is_empty() && end {
                Ok(())
            } else {
                Err(Refusal::NotAccepted)
            };
        }
        if self.outgoing.as_ref().is_some_and(|o| o.device != device)
            || self
                .retained
                .as_ref()
                .is_some_and(|previous| previous != &device)
        {
            let _ = self.finish().await;
        }
        if !events.is_empty() {
            let mut lines = Vec::new();
            for event in events {
                lines.extend_from_slice(&serde_json::to_vec(event).unwrap());
                lines.push(b'\n');
            }
            if begin.is_some() {
                self.allow_new_choice(&device);
            }
            if self.outgoing.is_none() {
                let outgoing = self.connect(&device).await;
                if let Err(Refusal::NotAccepted | Refusal::Missing(_)) = &outgoing {
                    self.retained = None;
                }
                self.outgoing = Some(outgoing?);
                self.retained = None;
            }
            let outgoing = self.outgoing.as_mut().ok_or_else(|| other("no stream"))?;
            if outgoing.stream.write_all(&lines).await.is_err() {
                let gone = self.outgoing.take().ok_or_else(|| other("no stream"))?;
                return self.settle(gone, false).await;
            }
            outgoing.sent += lines.len() as u64;
            outgoing.batches += 1;
            if let Some(at) = args["at"].as_u64() {
                outgoing.record_time(at);
            }
            if outgoing.opened_at.is_none() && outgoing.batches >= ROTATE_AFTER && !end {
                return self.rotate().await;
            }
        }
        if end {
            self.finish().await?;
        }
        Ok(())
    }

    async fn receive(&mut self, ticket: u64, args: &Value) -> Result<(), Refusal> {
        let owner = args["peer"]
            .as_str()
            .ok_or_else(|| other("no peer"))?
            .to_owned();
        let config = self.call(CONFIG_SERVICE, "get", json!({})).await?;
        let _ = config;
        self.call(INPUT_SERVICE, "inspect", json!({})).await?;
        let stream = self.data(ticket, None).await;
        self.handoffs.remove(&owner);
        self.probe
            .received
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.incoming.insert(
            ticket,
            Incoming {
                owner,
                stream,
                pending: Vec::new(),
                ended: Ended::Closed,
            },
        );
        Ok(())
    }

    /// Read one chunk of one receive and replay what it carries. `Ok(true)` when it ended.
    async fn replay(&mut self, ticket: u64, chunk: Vec<u8>) -> Result<bool, Refusal> {
        let Some(incoming) = self.incoming.get_mut(&ticket) else {
            return Ok(true);
        };
        if chunk.is_empty() {
            return if incoming.pending.is_empty() {
                Ok(true)
            } else {
                Err(other("What was sent was not understood"))
            };
        }
        incoming.pending.extend_from_slice(&chunk);
        let mut events = Vec::new();
        while let Some(end) = incoming.pending.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = incoming.pending.drain(..=end).collect();
            let value: Value = serde_json::from_slice(&line[..line.len() - 1])
                .map_err(|_| other("What was sent was not understood"))?;
            if value.get("continues").is_some() {
                incoming.ended = Ended::Continues;
            } else if value.get("rests").is_some() {
                incoming.ended = Ended::Rests;
            } else {
                events.push(value);
            }
        }
        let owner = incoming.owner.clone();
        for batch in events.chunks(MOST_BATCH) {
            let injected = self
                .call(
                    INPUT_SERVICE,
                    "inject",
                    json!({"owner": owner, "events": batch}),
                )
                .await;
            if let Err(reason) = &injected {
                self.probe.problem(format!("inject refused: {reason:?}"));
            }
            injected.map_err(|reason| match reason {
                Refusal::Other(_) => other("This computer could not replay the input"),
                blocked => blocked,
            })?;
        }
        Ok(false)
    }

    fn hand_off(&mut self, ticket: u64, owner: &str) -> bool {
        if self
            .incoming
            .iter()
            .any(|(other, incoming)| *other != ticket && incoming.owner == owner)
        {
            return true;
        }
        self.handoffs.insert(owner.to_owned());
        true
    }

    async fn complete_receive(&mut self, ticket: u64, result: Result<(), Refusal>) {
        let Some(incoming) = self.incoming.remove(&ticket) else {
            return;
        };
        let ended = incoming.ended;
        let owner = incoming.owner.clone();
        drop(incoming);
        let result = if result.is_ok() && ended == Ended::Continues && self.hand_off(ticket, &owner)
        {
            Ok(())
        } else if matches!(&result, Err(Refusal::NotAccepted | Refusal::Missing(_))) {
            self.handoffs.remove(&owner);
            result
        } else {
            let keep = result.is_ok() && ended == Ended::Rests;
            let released = self
                .call(
                    INPUT_SERVICE,
                    "release",
                    json!({"owner": owner, "keep": keep}),
                )
                .await;
            if let Err(reason) = &released {
                self.probe.problem(format!("release refused: {reason:?}"));
            }
            let released = released.map(|_| ()).map_err(|reason| match reason {
                Refusal::Other(_) => other("This computer could not replay the input"),
                blocked => blocked,
            });
            result.and(released)
        };
        if let Err(reason) = &result {
            self.probe.problem(format!("a receive ended: {reason:?}"));
        }
        let result = match result {
            Ok(()) => Outcome::ok(json!({})),
            Err(Refusal::NotAccepted) => Outcome::NotAccepted {},
            Err(Refusal::Missing(permission)) => Outcome::MissingPermission { permission },
            Err(Refusal::Other(reason)) => Outcome::error("failed", &reason),
        };
        self.submit(Operation::Reply { ticket, result }).await;
    }

    /// Handle one delivery the way Control's loop does.
    async fn handle(&mut self, delivery: Delivery) {
        match delivery {
            Delivery::Call {
                ticket,
                method,
                args,
                ..
            } => match method.as_str() {
                "input" => {
                    let result = self.input(&args).await;
                    if let Err(reason) = &result {
                        self.probe.problem(format!("input ended: {reason:?}"));
                    }
                    let result = match result {
                        Ok(()) => Outcome::ok(json!({})),
                        Err(Refusal::NotAccepted) => Outcome::NotAccepted {},
                        Err(Refusal::Missing(permission)) => {
                            Outcome::MissingPermission { permission }
                        }
                        Err(Refusal::Other(reason)) => Outcome::error("failed", &reason),
                    };
                    self.submit(Operation::Reply { ticket, result }).await;
                }
                "receive" => {
                    if let Err(reason) = self.receive(ticket, &args).await {
                        self.probe.problem(format!("receive refused: {reason:?}"));
                        let result = match reason {
                            Refusal::NotAccepted => Outcome::NotAccepted {},
                            Refusal::Missing(permission) => {
                                Outcome::MissingPermission { permission }
                            }
                            Refusal::Other(reason) => Outcome::error("failed", &reason),
                        };
                        self.submit(Operation::Reply { ticket, result }).await;
                    }
                }
                _ => {
                    self.submit(Operation::Reply {
                        ticket,
                        result: Outcome::error("failed", "Nothing here answers that"),
                    })
                    .await;
                }
            },
            Delivery::Cancelled { ticket } => {
                self.incoming.remove(&ticket);
            }
            Delivery::Event { .. } | Delivery::Result { .. } => {}
        }
    }

    /// One bounded piece of work, as `State::next` does: a control delivery, one read of the
    /// earliest receive of each peer, or the idle rotation.
    async fn step(&mut self) -> bool {
        if let Some(delivery) = self.waiting.pop_front() {
            self.handle(delivery).await;
            return true;
        }
        enum Ready {
            Message(Option<Delivery>),
            Chunk(u64, std::io::Result<Vec<u8>>),
            Idle,
        }
        let ready = {
            let Self {
                inbox,
                incoming,
                outgoing,
                ..
            } = &mut *self;
            let timeout = outgoing
                .as_ref()
                .map(|out| Duration::from_millis(out.rotation_wait_ms()));
            let mut owners = BTreeSet::new();
            let mut reads = Vec::new();
            for (ticket, receive) in incoming.iter_mut() {
                if owners.insert(receive.owner.clone()) {
                    let ticket = *ticket;
                    let stream = &mut receive.stream;
                    reads.push(Box::pin(async move {
                        let mut chunk = vec![0u8; 4096];
                        let result = stream.read(&mut chunk).await;
                        (
                            ticket,
                            result.map(|count| {
                                chunk.truncate(count);
                                chunk
                            }),
                        )
                    }));
                }
            }
            let idle = async {
                match timeout {
                    Some(wait) => tokio::time::sleep(wait).await,
                    None => std::future::pending().await,
                }
            };
            let any_read = async {
                if reads.is_empty() {
                    std::future::pending().await
                } else {
                    futures_select_all(&mut reads).await
                }
            };
            tokio::select! {
                message = inbox.recv() => Ready::Message(message),
                (ticket, result) = any_read => Ready::Chunk(ticket, result),
                () = idle => Ready::Idle,
            }
        };
        match ready {
            Ready::Message(None) => return false,
            Ready::Message(Some(delivery)) => {
                let forwarded = self
                    .outgoing
                    .as_ref()
                    .is_some_and(|out| out.opened_at.is_some());
                let rotate = self.outgoing.as_mut().is_some_and(|out| {
                    forwarded && (!out.measure_delivery(&delivery) || out.rotation_wait_ms() == 0)
                });
                if rotate
                    && !self
                        .outgoing
                        .as_ref()
                        .is_some_and(|out| out.finishes_on(&delivery))
                {
                    self.rotate_remembering().await;
                }
                self.waiting.push_front(delivery);
            }
            Ready::Chunk(ticket, result) => {
                // Data waiting on a receive while a forwarded connection is open: that one is
                // rotated first, as Control's loop does.
                if self
                    .outgoing
                    .as_ref()
                    .is_some_and(|out| out.opened_at.is_some())
                {
                    self.rotate_remembering().await;
                }
                let result = match result {
                    Ok(chunk) => self.replay(ticket, chunk).await,
                    Err(_) => Err(other("The other device stopped listening")),
                };
                match result {
                    Ok(false) => {}
                    ended => self.complete_receive(ticket, ended.map(|_| ())).await,
                }
            }
            Ready::Idle => self.rotate_remembering().await,
        }
        true
    }

    /// Rotate the open connection; a refusal that ends the visit is kept for the next `input`.
    async fn rotate_remembering(&mut self) {
        let Some(device) = self.outgoing.as_ref().map(|out| out.device.clone()) else {
            return;
        };
        if let Err(reason) = self.rotate().await {
            self.probe.problem(format!("a rotation ended: {reason:?}"));
            self.early_end = Some((device, reason));
        }
    }

    async fn run(mut self) {
        while self.step().await {}
    }
}

/// The first of several futures to finish (they are all cancel-safe reads).
async fn futures_select_all<T, F: Future<Output = T>>(futures: &mut [std::pin::Pin<Box<F>>]) -> T {
    std::future::poll_fn(|cx| {
        for future in futures.iter_mut() {
            if let std::task::Poll::Ready(value) = future.as_mut().poll(cx) {
                return std::task::Poll::Ready(value);
            }
        }
        std::task::Poll::Pending
    })
    .await
}

fn control_spec() -> PluginSpec {
    let mut spec = plugin_wanting_peers("control", &["list", "send"]);
    spec.manifest.imports.push(Import {
        provider: HOST.into(),
        service: CONFIG_SERVICE.into(),
        version: 1,
        methods: BTreeSet::from(["get".to_owned()]),
        events: BTreeSet::from([CONFIG_CHANGED.to_owned()]),
    });
    spec.manifest.imports.push(Import {
        provider: HOST.into(),
        service: INPUT_SERVICE.into(),
        version: 1,
        methods: BTreeSet::from(["inject".into(), "release".into(), "inspect".into()]),
        events: BTreeSet::new(),
    });
    spec.manifest
        .capabilities
        .insert("control.use".into(), "Control a paired computer.".into());
    spec.manifest.contributions.input = Some(Input {
        source: Source {
            service: SERVICE.into(),
            method: "input".into(),
        },
        capability: "control.use".into(),
        edge_pressure: EdgePressure::Off {},
    });
    spec.manifest.exports.push(Export {
        service: SERVICE.into(),
        version: 1,
        description: "Carries pointer input between paired computers.".into(),
        consumers: BTreeSet::from([HOST.into()]),
        methods: [
            ("input".to_owned(), member(Some("control.use"))),
            (
                crate::plugin_host::RECEIVE_METHOD.to_owned(),
                member(Some("control.use")),
            ),
        ]
        .into(),
        events: BTreeMap::new(),
    });
    spec
}

/// A running computer: its owner, its window's client, and what its pointer was told.
struct Computer {
    client: Client,
    task: tokio::task::JoinHandle<()>,
    pointer: Pointer,
    /// Each time the owner starts Control, the process it started.
    started: mpsc::Receiver<FixtureGuest>,
}

async fn computer(directory: &std::path::Path) -> (Computer, FixtureGuest) {
    let mut registry = Registry::default();
    registry.register(control_spec()).unwrap();
    let (runtime, client) = loopback_runtime(directory, registry).await;
    let pointer = Pointer::default();
    let recorder = pointer.clone();
    let input = DesktopInput::new(Box::new(move || Ok(Box::new(recorder.clone()))));
    let runtime = runtime.with_platform_services(Arc::new(Replay(input)));
    let (launch, mut started) = mpsc::channel(1);
    let runtime = runtime.with_launcher(
        PluginId::new("control").unwrap(),
        Arc::new(FixtureLauncher(launch)),
    );
    let task = tokio::spawn(async move {
        let _ = runtime.run().await;
    });
    let guest = within(Duration::from_secs(4), started.recv())
        .await
        .unwrap()
        .unwrap();
    (
        Computer {
            client,
            task,
            pointer,
            started,
        },
        guest,
    )
}

async fn tell(client: &Client, predicate: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    let mut state = client.subscribe();
    within(Duration::from_secs(8), state.wait_for(|s| predicate(s)))
        .await
        .unwrap()
        .unwrap()
        .clone()
}

/// Two paired computers, each with Control allowed to control and to be controlled by the other.
async fn pair_of_computers(
    left_dir: &std::path::Path,
    right_dir: &std::path::Path,
) -> (
    Computer,
    FixtureGuest,
    Computer,
    FixtureGuest,
    PeerId,
    PeerId,
) {
    let ((left, left_guest), (right, right_guest)) =
        tokio::join!(computer(left_dir), computer(right_dir));
    let Reply::Invitation(invitation) = right
        .client
        .command(Command::CreateInvitation {})
        .await
        .unwrap()
    else {
        panic!("pairing invitation unavailable")
    };
    left.client
        .command(Command::JoinInvitation { invitation })
        .await
        .unwrap();
    let (left_prompt, right_prompt) = tokio::join!(
        tell(&left.client, |s| !s.pairing.is_empty()),
        tell(&right.client, |s| !s.pairing.is_empty())
    );
    let number = left_prompt.pairing[0].options[0];
    for (client, state) in [(&left.client, left_prompt), (&right.client, right_prompt)] {
        client
            .command(Command::ChoosePairing {
                request: state.pairing[0].id.clone(),
                number,
            })
            .await
            .unwrap();
    }
    let (left_seen, right_seen) = tokio::join!(
        tell(&left.client, |s| s.peers.iter().any(|p| p.online)),
        tell(&right.client, |s| s.peers.iter().any(|p| p.online))
    );
    let right_peer = left_seen.peers[0].id.clone();
    let left_peer = right_seen.peers[0].id.clone();
    let control = PluginId::new("control").unwrap();
    let watch = permission(HOST, PEERS_WATCH_CAPABILITY);
    let send = permission(HOST, PEERS_SEND_CAPABILITY);
    let replay = permission(HOST, INPUT_CAPABILITY);
    let use_control = permission("control", "control.use");
    for (computer, other) in [(&left, &right_peer), (&right, &left_peer)] {
        computer
            .client
            .command(Command::SetGrants {
                plugin: control.clone(),
                peer: Some(other.clone()),
                grants: Grants::new([watch.clone(), send.clone(), use_control.clone()]).unwrap(),
            })
            .await
            .unwrap();
        computer
            .client
            .command(Command::SetGrants {
                plugin: control.clone(),
                peer: None,
                grants: Grants::new([replay.clone()]).unwrap(),
            })
            .await
            .unwrap();
    }
    (left, left_guest, right, right_guest, left_peer, right_peer)
}

/// Spread of the intervals between moves of a pointer: the longest wait between two, and how many.
fn longest_gap(moves: &[Instant]) -> (Duration, usize) {
    (
        moves
            .windows(2)
            .map(|pair| pair[1].duration_since(pair[0]))
            .max()
            .unwrap_or_default(),
        moves.len(),
    )
}

/// The person drives the other computer for `duration`, the mouse reporting a place every `period`, through what the
/// owner does with captured input (`input_follow::forward_in_order`): one visit, which fails if any call fails.
async fn drive(
    from: &Computer,
    to: &PeerId,
    visit: u64,
    period: Duration,
    duration: Duration,
) -> Result<(), &'static str> {
    use crate::input_follow::{Outgoing, forward_in_order};
    use peersus_api::contributions::PointerInput;
    let plugin = PluginId::new("control").unwrap();
    let (outgoing, queued) = mpsc::unbounded_channel::<Outgoing>();
    let failed: Arc<std::sync::Mutex<Option<&'static str>>> = Arc::default();
    let (note, client) = (failed.clone(), from.client.clone());
    let forwarder = tokio::spawn(forward_in_order(
        queued,
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        move |(plugin, peer, events, ending, visit): Outgoing| {
            let (note, client) = (note.clone(), client.clone());
            async move {
                let events = events
                    .iter()
                    .filter_map(|event| serde_json::to_value(event).ok())
                    .collect();
                let command = Command::ForwardInput {
                    plugin,
                    peer,
                    events,
                    end: ending.is_some(),
                };
                if let Err(reason) = client
                    .forward_capture(
                        command,
                        CaptureInput {
                            serial: visit,
                            resting: ending == Some(crate::surfaces::Ending::Copy),
                        },
                    )
                    .await
                {
                    note.lock().unwrap().get_or_insert(reason);
                }
            }
        },
    ));
    let started = Instant::now();
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut report = 0u32;
    while started.elapsed() < duration && failed.lock().unwrap().is_none() {
        ticker.tick().await;
        report += 1;
        let place = PointerInput::Position {
            x: (report % 1000) as f32 / 1000.,
            y: 0.5,
        };
        if outgoing
            .send((plugin.clone(), to.clone(), vec![place], None, visit))
            .is_err()
        {
            break;
        }
    }
    drop(outgoing);
    let _ = forwarder.await;
    let failure = *failed.lock().unwrap();
    failure.map_or(Ok(()), Err)
}

/// The two guests of a test run, each started with a probe, and what the probes saw.
struct Running {
    left_probe: Arc<Probe>,
    right_probe: Arc<Probe>,
    left_run: tokio::task::JoinHandle<()>,
    right_run: tokio::task::JoinHandle<()>,
}

fn run_guests(left_guest: FixtureGuest, right_guest: FixtureGuest) -> Running {
    let started = Instant::now();
    let probe = || {
        Arc::new(Probe {
            started: Some(started),
            ..Probe::default()
        })
    };
    let (left_probe, right_probe) = (probe(), probe());
    let left_run = tokio::spawn(Control::start(left_guest, left_probe.clone()).run());
    let right_run = tokio::spawn(Control::start(right_guest, right_probe.clone()).run());
    Running {
        left_probe,
        right_probe,
        left_run,
        right_run,
    }
}

impl Running {
    fn report(&self, left: &Computer, right: &Computer) -> String {
        let (gap, count) = longest_gap(&right.pointer.moves());
        format!(
            "{count} moves, longest gap {gap:?}; acks {:?}; left opened {}; right received {}; \
             left problems {:?}; right problems {:?}; left errors {:?}",
            self.left_probe.acks.lock().unwrap(),
            self.left_probe
                .opened
                .load(std::sync::atomic::Ordering::SeqCst),
            self.right_probe
                .received
                .load(std::sync::atomic::Ordering::SeqCst),
            self.left_probe.problems.lock().unwrap(),
            self.right_probe.problems.lock().unwrap(),
            left.client.snapshot().errors,
        )
    }
}

/// One person drives one computer from another for `duration`, the mouse reporting a place every `period`. Returns what was
/// seen, and whether the visit lasted: no notice that it ended and no wait between two moves of 500 ms or more.
async fn scenario(period: Duration, duration: Duration) -> (String, bool) {
    let (left_dir, right_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (left, left_guest, right, right_guest, _left_peer, right_peer) =
        pair_of_computers(left_dir.path(), right_dir.path()).await;
    let running = run_guests(left_guest, right_guest);
    // The window is told when the visit ends.
    let mut notices = left.client.surface_failures();
    let result = drive(&left, &right_peer, 1, period, duration).await;
    let (gap, _) = longest_gap(&right.pointer.moves());
    let ended = std::iter::from_fn(|| notices.try_recv().ok()).count();
    let report = format!(
        "period {period:?}: result {result:?}; {}; visit notices {ended}",
        running.report(&left, &right)
    );
    running.left_run.abort();
    running.right_run.abort();
    left.task.abort();
    right.task.abort();
    (report, ended == 0 && gap < Duration::from_millis(500))
}

/// A mouse that reports a thousand times a second, which used to end the visit within two seconds.
#[tokio::test(flavor = "multi_thread")]
async fn control_between_two_computers_keeps_moving_through_every_rotation() {
    let (report, lasted) = scenario(Duration::from_millis(1), Duration::from_secs(4)).await;
    eprintln!("{report}");
    assert!(lasted, "{report}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_visit_resumes_after_the_idle_copy_closes() {
    let (left_dir, right_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (left, left_guest, right, right_guest, _, right_peer) =
        pair_of_computers(left_dir.path(), right_dir.path()).await;
    let running = run_guests(left_guest, right_guest);
    let mut notices = left.client.surface_failures();
    let send_motion = || {
        drive(
            &left,
            &right_peer,
            1,
            Duration::from_millis(5),
            Duration::from_millis(250),
        )
    };
    send_motion().await.unwrap();
    let rest = Command::ForwardInput {
        plugin: PluginId::new("control").unwrap(),
        peer: right_peer.clone(),
        end: true,
        events: Vec::new(),
    };
    left.client
        .forward_capture(
            rest,
            CaptureInput {
                serial: 1,
                resting: true,
            },
        )
        .await
        .unwrap();
    within(Duration::from_secs(2), async {
        while running.left_probe.acks.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first copy closed at rest");
    let before = right.pointer.moves().len();
    send_motion().await.unwrap();
    let after = right.pointer.moves().len();
    let report = running.report(&left, &right);
    running.left_run.abort();
    running.right_run.abort();
    left.task.abort();
    right.task.abort();
    assert!(
        before > 0 && after > before,
        "the same visit must still replay after rest: {report}"
    );
    assert!(notices.try_recv().is_err(), "{report}");
}

/// What happens on the visited computer while it is being visited, after which everything is as it was.
#[derive(Clone, Copy, Debug)]
enum Disturbance {
    /// Control is restarted there.
    ControlRestarts,
    /// The person there takes away, and a moment later gives back, what lets Control replay input.
    ReplayGrantBlip,
    /// The person there takes away, and a moment later gives back, what lets Control be driven by the other computer.
    ControlGrantBlip,
}

/// A visit is under way, the visited computer is disturbed, and the person crosses again: the second visit must work, with
/// nothing restarted on the computer they cross from, and the first must be the only one told to have ended.
async fn crossing_after(disturbance: Disturbance) -> (String, bool) {
    let (left_dir, right_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (left, left_guest, mut right, right_guest, left_peer, right_peer) =
        pair_of_computers(left_dir.path(), right_dir.path()).await;
    let running = run_guests(left_guest, right_guest);
    let mut notices = left.client.surface_failures();
    let first = drive(
        &left,
        &right_peer,
        1,
        Duration::from_millis(5),
        Duration::from_millis(600),
    )
    .await;
    let moved_first = right.pointer.moves().len();
    let control = PluginId::new("control").unwrap();
    let replay = permission(HOST, INPUT_CAPABILITY);
    let right_grants = |peer: Option<PeerId>, given: &[Permission]| {
        right.client.command(Command::SetGrants {
            plugin: control.clone(),
            peer,
            grants: Grants::new(given.iter().cloned()).unwrap(),
        })
    };
    let mut revived = None;
    match disturbance {
        Disturbance::ControlRestarts => {
            right
                .client
                .command(Command::RestartPlugin {
                    plugin: control.clone(),
                })
                .await
                .unwrap();
            running.right_run.abort();
            let restarted = within(Duration::from_secs(8), right.started.recv())
                .await
                .unwrap()
                .unwrap();
            let probe = Arc::new(Probe {
                started: Some(Instant::now()),
                ..Probe::default()
            });
            revived = Some((
                tokio::spawn(Control::start(restarted, probe.clone()).run()),
                probe,
            ));
        }
        Disturbance::ReplayGrantBlip => {
            right_grants(None, &[]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
            right_grants(None, std::slice::from_ref(&replay))
                .await
                .unwrap();
        }
        Disturbance::ControlGrantBlip => {
            let (watch, send) = (
                permission(HOST, PEERS_WATCH_CAPABILITY),
                permission(HOST, PEERS_SEND_CAPABILITY),
            );
            right_grants(Some(left_peer.clone()), &[watch.clone(), send.clone()])
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
            right_grants(
                Some(left_peer.clone()),
                &[watch, send, permission("control", "control.use")],
            )
            .await
            .unwrap();
        }
    }
    // A surface notice releases capture. The owner must also tell the person why it stopped.
    let ended = within(Duration::from_secs(2), notices.recv())
        .await
        .is_some_and(|notice| notice.is_ok_and(|notice| notice.visit == Some(1)));
    let visible = within(Duration::from_secs(2), async {
        let mut state = left.client.subscribe();
        state
            .wait_for(|s| s.errors.iter().any(|error| error.contains("input visit")))
            .await
            .is_ok()
    })
    .await
        == Some(true);
    // Time for what the right computer says of itself to reach the left one.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let second = drive(
        &left,
        &right_peer,
        2,
        Duration::from_millis(5),
        Duration::from_millis(600),
    )
    .await;
    let moved_second = right.pointer.moves().len() - moved_first;
    let later: Vec<_> = std::iter::from_fn(|| notices.try_recv().ok()).collect();
    let report = format!(
        "{disturbance:?}: first {first:?} moved {moved_first}; ended {ended}, visible {visible}; second \
         {second:?} moved {moved_second}; later notices {:?}; left problems {:?}; left errors {:?}",
        later.iter().map(|notice| notice.visit).collect::<Vec<_>>(),
        running.left_probe.problems.lock().unwrap(),
        left.client.snapshot().errors,
    );
    running.left_run.abort();
    running.right_run.abort();
    if let Some((task, _)) = revived {
        task.abort();
    }
    left.task.abort();
    right.task.abort();
    let worked = first.is_ok()
        && moved_first > 20
        && ended
        && visible
        && second.is_ok()
        && moved_second > 20
        && later.iter().all(|notice| notice.visit == Some(1));
    (report, worked)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_visit_ended_by_the_other_computer_restarting_control_does_not_block_the_next_crossing() {
    let (report, worked) = crossing_after(Disturbance::ControlRestarts).await;
    eprintln!("{report}");
    assert!(worked, "{report}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_visit_ended_by_the_other_computer_withdrawing_and_restoring_replay_does_not_block_the_next_crossing()
 {
    let (report, worked) = crossing_after(Disturbance::ReplayGrantBlip).await;
    eprintln!("{report}");
    assert!(worked, "{report}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_visit_ended_by_the_other_computer_withdrawing_and_restoring_control_does_not_block_the_next_crossing()
 {
    let (report, worked) = crossing_after(Disturbance::ControlGrantBlip).await;
    eprintln!("{report}");
    assert!(worked, "{report}");
}
