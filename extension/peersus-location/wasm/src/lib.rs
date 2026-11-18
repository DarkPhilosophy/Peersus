//! A page that says where this device is and, on a drawn map with this device in the middle when it is known (otherwise
//! one of the devices that shared with it), where the paired devices that shared their location with it were: how far away,
//! in which direction and how long ago.
//!
//! This device's position is asked of the application (`peersus.location`, the person's `location.read`) only when the person
//! presses a button, or every five minutes while the page is open when they turned that on. Sharing sends it once to every
//! paired device that takes it now (`peersus.peers`: online, allowed, and its Location there accepts `location.position`);
//! the application admits it there only from a device that device's person allowed Location for (`location.share`). What
//! arrives is kept in memory only, one position per device, and is forgotten when the extension stops, the person presses
//! Forget, or the device is no longer listed as allowed (unpaired, or Location no longer allowed for it). Nothing is asked of another device: a position goes only when its own person shares it.
mod geo;
mod host;
mod native;
mod wire;
use host::{Data, Handle, Wire, read_ack, write_all};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

const PEERS: &str = "peersus.peers";
const LOCATION: &str = "peersus.location";
/// How long the application is given to answer an ordinary call, in milliseconds.
const CALL_MS: u32 = 5_000;
/// How long it is given to find this device: a system without a fix may need a few seconds (the application's own ceiling).
const FIND_MS: u32 = 20_000;
/// Longest wait for a stream to be readable, in milliseconds.
const WAIT_MS: i32 = 400;
/// Longest one stream is waited for in all, in milliseconds: what bounds a message is this and its bytes, never how many
/// pieces it arrives in.
const STREAM_MS: i32 = 4_000;
/// Most deliveries kept for later while a stream is waited for.
const MOST_KEPT: usize = 64;
/// Longest name of a device the application gives, in characters.
const ALIAS_MAX: usize = 128;
/// Most devices whose position is kept, and drawn.
const MOST_SEEN: usize = 12;
/// The page is read again this often while it is open, in milliseconds.
const TICK: u16 = 1_000;
/// The paired devices are listed again this often while the page is open (page clock, milliseconds).
const LIST_EVERY: u64 = 10_000;
/// Keep sharing sends again this often (page clock, milliseconds).
const SHARE_EVERY: u64 = 5 * 60_000;
/// The map's side, in pixels.
const MAP: u16 = 300;
/// Most characters of a device's name shown.
const NAME_MAX: usize = 40;
/// Most characters of what the application said when it could not find this device.
const SAID_MAX: usize = 160;

/// Fixed words: nothing a device or the application wrote is in them unless said so.
mod words {
    pub const FINDING: &str = "Finding where this device is…";
    pub const UNKNOWN: &str = "Where this device is has not been asked yet.";
    pub const NOT_ALLOWED: &str =
        "Location may not know where this device is: allow it in this extension's permissions.";
    pub const NOT_FOUND: &str = "This device did not say where it is.";
    pub const NOBODY: &str = "No paired device can receive it now: on both devices Location must be installed and allowed for the other, and the other must be online.";
    pub const DEVICES_DENIED: &str = "Location may not see your paired devices: allow it for them in this extension's permissions.";
    pub const SOMEONE: &str = "A paired device";
}

/// What is known of where this device is.
#[derive(Clone, Debug, Default, PartialEq)]
enum Own {
    #[default]
    Unknown,
    Finding,
    /// Found: the position, how old the system said it was, and when (page clock) it was told.
    Found(Told),
    /// Not found, in words to show.
    Not(String),
}

/// A paired device as `peersus.peers` lists it.
#[derive(Clone, Debug, PartialEq)]
struct Device {
    alias: String,
    name: String,
    /// It takes a position now: online, allowed here, and its Location accepts it.
    ready: bool,
}

/// A position and when (page clock) it was told: this device's by the application, a paired device's when it arrived.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Told {
    shared: wire::Shared,
    at: u64,
}

impl Told {
    /// What it is at `now`: the age it was told with, plus the time since.
    fn aged(&self, now: u64) -> wire::Shared {
        wire::Shared {
            fix: self.shared.fix,
            age: self
                .shared
                .age
                .saturating_add(now.saturating_sub(self.at) / 1000),
        }
    }
}

/// Why this device is asked where it is.
#[derive(Clone, Copy, Debug)]
enum Ask {
    /// Only to show it here.
    Show,
    /// The person pressed Share: it is sent once it is found.
    Share,
    /// Keep sharing is due: it is sent once it is found, unless Keep sharing is turned off before.
    Keep,
}

/// What is left of the time one stream may be waited for ([`STREAM_MS`]). A guest has no clock: every wait counts as having
/// taken all it was given, so the time really waited is never more. When none is left a stream is only looked at: what
/// already arrived is still read, and nothing more is waited for.
struct Patience(i32);

impl Patience {
    fn new() -> Self {
        Self(STREAM_MS)
    }

    /// How long the next wait may take, in milliseconds: zero when nothing is left.
    fn next(&mut self) -> i32 {
        let wait = self.0.min(WAIT_MS);
        self.0 -= wait;
        wait
    }
}

#[derive(Default)]
pub struct State {
    calls: u64,
    /// The page's clock, in milliseconds, as its last event said it.
    now: u64,
    own: Own,
    asked_own: u64,
    /// The person pressed Share: send as soon as this device is found.
    share_once: bool,
    /// Keep sharing asked: send as soon as this device is found, unless it is turned off before.
    share_kept: bool,
    devices: Vec<Device>,
    devices_denied: bool,
    asked_list: u64,
    listed_at: Option<u64>,
    seen: BTreeMap<String, Told>,
    /// Calls that asked for a stream to a device: the device, and the position that share is of.
    sending: Vec<(u64, String, Told)>,
    /// Streams the application opened for a share, to write that share's position on.
    outgoing: Vec<(u64, String, Told)>,
    /// What came of the last share to each device: whether every byte reached it.
    delivered: BTreeMap<String, bool>,
    /// The last share reached no device: nobody could take it.
    nobody: bool,
    keep: bool,
    /// When (page clock) a share last went out.
    shared_at: Option<u64>,
    /// When (page clock) Keep sharing last asked where this device is, found or not.
    kept_at: Option<u64>,
    waiting: VecDeque<Value>,
}

fn px(n: u16) -> Value {
    json!({"unit":"px","value":n})
}
fn fill() -> Value {
    json!({"unit":"percent","value":10000})
}
fn bx(style: Value, children: Vec<Value>) -> Value {
    json!({"k":"box","style":style,"children":children})
}
fn text(text: &str, tone: &str) -> Value {
    json!({"k":"text","text":text,"tone":tone})
}
fn button(id: &str, label: &str) -> Value {
    json!({"k":"button","id":id,"label":label})
}
fn shape(path: String, stroke: &str, line: u16, fill: &str, opacity: Option<u16>) -> Value {
    json!({"path":path,"stroke":stroke,"line":line,"fill":fill,"opacity":opacity,"round":true})
}

/// `place`, then how sure the system was and how old the position is.
fn described(shared: &wire::Shared, place: String) -> String {
    let mut line = place;
    if let Some(accuracy) = shared.fix.accuracy {
        line.push_str(&format!(" ± {}", geo::distance(accuracy)));
    }
    line.push_str(&format!(", {}", geo::age(shared.age)));
    line
}

/// At most `most` characters of `said`, on one line.
fn clipped(said: &str, most: usize) -> String {
    let line: String = said
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = line.trim();
    match line.char_indices().nth(most) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

impl State {
    fn call(&mut self, service: &str, method: &str, args: Value, timeout: u32) -> (u64, Value) {
        self.calls = self.calls.checked_add(1).unwrap_or(1);
        let call = json!({"op":"call","id":self.calls,"provider":"host","service":service,
            "version":1,"method":method,"args":args,"timeout_ms":timeout});
        (self.calls, call)
    }

    fn list(&mut self) -> Value {
        let (id, call) = self.call(PEERS, "list", json!({}), CALL_MS);
        self.asked_list = id;
        self.listed_at = Some(self.now);
        call
    }

    /// Ask where this device is, for `ask`. One answer serves every reason it was asked for: while one is awaited, nothing
    /// more is asked.
    fn find(&mut self, ask: Ask) -> Option<Value> {
        match ask {
            Ask::Show => {}
            Ask::Share => self.share_once = true,
            Ask::Keep => {
                self.share_kept = true;
                self.kept_at = Some(self.now);
            }
        }
        if self.own == Own::Finding {
            return None;
        }
        self.own = Own::Finding;
        let (id, call) = self.call(LOCATION, "current", json!({}), FIND_MS);
        self.asked_own = id;
        Some(call)
    }

    /// One stream asked for, to every device that takes a position now, each for `told`: the position this share is of.
    /// What an earlier share still waited for is dropped: it is no longer the last one.
    fn share(&mut self, told: Told) -> Vec<Value> {
        self.shared_at = Some(self.now);
        self.sending.clear();
        self.delivered.clear();
        let ready: Vec<String> = self
            .devices
            .iter()
            .filter(|device| device.ready)
            .map(|device| device.alias.clone())
            .collect();
        self.nobody = ready.is_empty();
        ready
            .into_iter()
            .map(|alias| {
                let (id, call) = self.call(
                    PEERS,
                    "send",
                    json!({"device":alias,"service":wire::SERVICE,"version":wire::VERSION}),
                    CALL_MS,
                );
                self.sending.push((id, alias, told));
                call
            })
            .collect()
    }

    /// What this device's position is now: the age the system said, plus the time since.
    fn own_now(&self) -> Option<wire::Shared> {
        match &self.own {
            Own::Found(told) => Some(told.aged(self.now)),
            _ => None,
        }
    }

    /// What the application answered to a call this extension made, and the calls it leads to.
    pub fn told(&mut self, delivery: &Value) -> Vec<Value> {
        let Some(id) = delivery["id"].as_u64().filter(|id| *id != 0) else {
            return Vec::new();
        };
        let result = &delivery["result"];
        let ok = result["status"] == "ok";
        let denied = result["code"] == "denied";
        if id == self.asked_list {
            self.asked_list = 0;
            self.devices_denied = denied;
            if let Some(peers) = result["value"].as_array().filter(|_| ok) {
                self.listed(peers);
            } else if denied {
                // No device may be seen any more: nothing of one is shown either.
                self.devices.clear();
                self.seen.clear();
            }
        } else if id == self.asked_own {
            self.asked_own = 0;
            let share = self.share_once || self.share_kept;
            self.share_once = false;
            self.share_kept = false;
            match result["value"]
                .as_object()
                .filter(|_| ok)
                .map(|_| wire::position(&result["value"]))
            {
                Some(Ok(shared)) => {
                    let told = Told {
                        shared,
                        at: self.now,
                    };
                    self.own = Own::Found(told);
                    if share {
                        return self.share(told);
                    }
                }
                _ if denied => self.own = Own::Not(words::NOT_ALLOWED.to_owned()),
                // The application's own words of why it could not, when it gave some.
                _ => {
                    self.own = Own::Not(
                        result["message"]
                            .as_str()
                            .map(|said| clipped(said, SAID_MAX))
                            .filter(|said| !said.is_empty())
                            .unwrap_or_else(|| words::NOT_FOUND.to_owned()),
                    )
                }
            }
        } else if let Some(index) = self.sending.iter().position(|(asked, ..)| *asked == id) {
            let (_, alias, told) = self.sending.swap_remove(index);
            match result["value"]["claim"].as_u64().filter(|_| ok) {
                Some(claim) => self.outgoing.push((claim, alias, told)),
                None => {
                    self.delivered.insert(alias, false);
                }
            }
        }
        Vec::new()
    }

    /// The paired devices as the application lists them now. A position is kept only of a device listed and allowed: one the
    /// person unpaired, or no longer allows Location for, is forgotten at once.
    fn listed(&mut self, peers: &[Value]) {
        self.devices = peers
            .iter()
            .filter(|peer| peer["allowed"] == true)
            .filter_map(|peer| {
                let alias = peer["device"]
                    .as_str()
                    .filter(|alias| !alias.is_empty() && alias.len() <= ALIAS_MAX)?;
                let ready = peer["presence"] != "offline"
                    && peer["online"] == true
                    && peer["accepts"].as_array().is_some_and(|accepts| {
                        accepts.iter().any(|service| service == wire::SERVICE)
                    });
                Some(Device {
                    alias: alias.to_owned(),
                    name: clipped(peer["name"].as_str().unwrap_or(words::SOMEONE), NAME_MAX),
                    ready,
                })
            })
            .collect();
        let devices = &self.devices;
        self.seen
            .retain(|alias, _| devices.iter().any(|device| device.alias == *alias));
    }

    /// What a person did on the page, and the calls it leads to.
    fn act(&mut self, event: &Value) -> Vec<Value> {
        if let Some(at) = event["at"].as_u64() {
            self.now = self.now.max(at);
        }
        let mut calls = Vec::new();
        match (event["t"].as_str(), event["id"].as_str()) {
            (Some("open"), _) => calls.push(self.list()),
            (Some("press"), Some("share")) => calls.extend(self.find(Ask::Share)),
            (Some("press"), Some("find")) => calls.extend(self.find(Ask::Show)),
            (Some("press"), Some("forget")) => self.seen.clear(),
            (Some("toggle"), Some("keep")) => {
                self.keep = event["on"] == true;
                if self.keep {
                    calls.extend(self.find(Ask::Keep));
                } else {
                    // Turned off while this device was being found for it: that position is not sent.
                    self.share_kept = false;
                }
            }
            (Some("tick"), _) => {
                if self.asked_list == 0
                    && self
                        .listed_at
                        .is_none_or(|at| self.now.saturating_sub(at) >= LIST_EVERY)
                {
                    calls.push(self.list());
                }
                // Five minutes after the last share, and after Keep sharing last asked: not found waits as long as found.
                if self.keep
                    && self
                        .shared_at
                        .max(self.kept_at)
                        .is_none_or(|at| self.now.saturating_sub(at) >= SHARE_EVERY)
                {
                    calls.extend(self.find(Ask::Keep));
                }
            }
            _ => {}
        }
        calls
    }

    /// The reply to a call of the page's service: the page as it is after what the person did.
    pub fn answer(&mut self, delivery: &Value) -> (Vec<Value>, Option<Value>) {
        let Some(ticket) = delivery["ticket"].as_u64() else {
            return (Vec::new(), None);
        };
        if delivery["method"] != "event" {
            let refused =
                json!({"status":"error","code":"failed","message":"Nothing here answers that"});
            return (
                Vec::new(),
                Some(json!({"op":"reply","ticket":ticket,"result":refused})),
            );
        }
        let calls = self.act(&delivery["args"]["event"]);
        let page = self.page();
        (
            calls,
            Some(json!({"op":"reply","ticket":ticket,"result":{"status":"ok","value":page}})),
        )
    }

    fn own_line(&self) -> String {
        if let Some(shared) = self.own_now() {
            return format!(
                "This device: {}",
                described(&shared, geo::coordinates(&shared.fix))
            );
        }
        match &self.own {
            Own::Finding => words::FINDING.to_owned(),
            Own::Not(said) => said.clone(),
            Own::Unknown | Own::Found(_) => words::UNKNOWN.to_owned(),
        }
    }

    fn name(&self, alias: &str) -> String {
        self.devices
            .iter()
            .find(|device| device.alias == alias)
            .map_or_else(|| words::SOMEONE.to_owned(), |device| device.name.clone())
    }

    /// What came of the last share, when there was one.
    fn share_line(&self) -> Option<String> {
        if self.nobody {
            return Some(words::NOBODY.to_owned());
        }
        if self.delivered.is_empty() {
            return None;
        }
        let names = |reached: bool| {
            self.delivered
                .iter()
                .filter(|(_, delivered)| **delivered == reached)
                .map(|(alias, _)| self.name(alias))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let (reached, missed) = (names(true), names(false));
        Some(match (reached.is_empty(), missed.is_empty()) {
            (false, true) => format!("Shared with {reached}."),
            (true, _) => format!("Not shared: {missed} did not take it."),
            (false, false) => format!("Shared with {reached}. {missed} did not take it."),
        })
    }

    /// Every position known, by whose it is: this device first, when it is known.
    fn positions(&self) -> Vec<(String, wire::Shared, bool)> {
        let mut all: Vec<_> = self
            .own_now()
            .map(|own| ("This device".to_owned(), own, true))
            .into_iter()
            .collect();
        all.extend(
            self.seen
                .iter()
                .map(|(alias, told)| (self.name(alias), told.aged(self.now), false)),
        );
        all
    }

    /// The map: the first position in the middle, every other one at its true distance and direction from it, rings of
    /// round distances, north up. Names are written beside their dots.
    fn map(&self, positions: &[(String, wire::Shared, bool)]) -> Option<Value> {
        let (_, centre, _) = positions.first()?;
        let placed: Vec<(f64, f64)> = positions
            .iter()
            .map(|(_, shared, _)| geo::between(&centre.fix, &shared.fix))
            .collect();
        let farthest = placed.iter().map(|(metres, _)| *metres).fold(0.0, f64::max);
        let size = f64::from(MAP);
        let map = geo::Map::fit(size, farthest);
        let middle = size / 2.0;
        let mut shapes = vec![shape(
            geo::circle(middle, middle, middle - 2.0),
            "border",
            10,
            "raised",
            None,
        )];
        for ring in 1..=map.rings {
            let radius = map.pixels(map.ring * f64::from(ring));
            shapes.push(shape(
                geo::circle(middle, middle, radius),
                "border",
                8,
                "transparent",
                Some(7000),
            ));
        }
        shapes.push(shape(
            format!(
                "M {middle:.1} 4 L {middle:.1} {:.1} M 4 {middle:.1} L {:.1} {middle:.1}",
                size - 4.0,
                size - 4.0
            ),
            "border",
            6,
            "transparent",
            Some(5000),
        ));
        // North, a small arrow at the top.
        shapes.push(shape(
            format!(
                "M {middle:.1} 6 L {:.1} 16 L {:.1} 16 Z",
                middle - 5.0,
                middle + 5.0
            ),
            "muted",
            0,
            "muted",
            None,
        ));
        let mut labels = Vec::new();
        for ((name, shared, own), (metres, bearing)) in positions.iter().zip(&placed) {
            let (x, y) = map.place(*metres, *bearing);
            if let Some(accuracy) = shared
                .fix
                .accuracy
                .map(|metres| map.pixels(metres))
                .filter(|r| *r >= 4.0)
            {
                shapes.push(shape(
                    geo::circle(x, y, accuracy),
                    "transparent",
                    0,
                    "accent",
                    Some(1500),
                ));
            }
            let (colour, radius) = if *own {
                ("success", 6.0)
            } else {
                ("accent", 5.0)
            };
            shapes.push(shape(
                geo::circle(x, y, radius),
                "surface",
                15,
                colour,
                None,
            ));
            // Beside the dot, kept inside the map; at the left of it in the map's right half.
            let left = if x > middle {
                (x - 8.0 - 110.0).max(0.0)
            } else {
                (x + 8.0).min(size - 110.0)
            };
            labels.push(bx(
                json!({"overlay":true,"at":[(left * 1000.0) as i32, ((y - 9.0).clamp(0.0, size - 18.0) * 1000.0) as i32],
                    "width":px(110),"padding":1,"radius":[4,4,4,4],"background":"surface","opacity":8500}),
                vec![json!({"k":"text","text":clipped(name, 16),"tone":"muted","lines":1})],
            ));
        }
        let mut children = vec![json!({"k":"drawing","width":MAP,"height":MAP,"shapes":shapes})];
        children.extend(labels);
        let rings = format!("Rings every {}", geo::distance(map.ring));
        Some(bx(
            json!({"gap":4}),
            vec![
                bx(
                    json!({"width":px(MAP),"height":px(MAP),"clip":true}),
                    children,
                ),
                text(&rings, "muted"),
            ],
        ))
    }

    /// The page: what this device is, the buttons, the map and one line per device that shared with it.
    pub fn page(&self) -> Value {
        let mut children = vec![
            text("Location", "title"),
            text(
                "Share where this device is with your paired devices, and see where the ones that shared with it were. \
                 Received locations are kept only while the application runs.",
                "muted",
            ),
            bx(
                json!({"horizontal":true,"wrap":true,"gap":8}),
                vec![
                    button("share", "Share my location"),
                    button("find", "Show where this device is"),
                    button("forget", "Forget received locations"),
                ],
            ),
            json!({"k":"toggle","id":"keep","label":"Keep sharing every 5 minutes while this page is open","on":self.keep}),
            text(&self.own_line(), "body"),
        ];
        if let Some(line) = self.share_line() {
            children.push(text(&line, "muted"));
        }
        if self.devices_denied {
            children.push(text(words::DEVICES_DENIED, "muted"));
        }
        let positions = self.positions();
        children.extend(self.map(&positions));
        let centre = positions.first().map(|(_, shared, _)| shared.fix);
        let from_here = positions.first().is_some_and(|(_, _, own)| *own);
        for (name, shared, own) in &positions {
            if *own {
                continue;
            }
            // How far and which way from this device when it is known, else where it is.
            let place = match centre {
                Some(centre) if from_here => {
                    let (metres, bearing) = geo::between(&centre, &shared.fix);
                    format!("{} {}", geo::distance(metres), geo::compass(bearing))
                }
                _ => geo::coordinates(&shared.fix),
            };
            children.push(text(
                &format!("{name} — {}", described(shared, place)),
                "body",
            ));
        }
        let root = bx(
            json!({"width":fill(),"height":fill(),"scroll":true,"gap":12}),
            children,
        );
        json!({"k":"interval","id":"clock","milliseconds":TICK,"child":root})
    }
}

/// Streams: what is written for a share, and what a paired device shared.
impl State {
    /// The next delivery to handle: what was kept for later comes first, in the order it came.
    fn next<H: Wire>(&mut self, host: &mut H) -> Result<Value, &'static str> {
        match self.waiting.pop_front() {
            Some(delivery) => Ok(delivery),
            None => host.receive(),
        }
    }

    /// Wait until `handle` can be read, keeping what the application delivers meanwhile: [`WAIT_MS`] at most each time, and
    /// only while `patience` lasts.
    fn wait<H: Wire + Data>(
        &mut self,
        host: &mut H,
        handle: Handle,
        patience: &mut Patience,
    ) -> Result<(), &'static str> {
        for _ in 0..=MOST_KEPT {
            match host.poll(&[handle], patience.next())? {
                Some(ready) if ready == handle => return Ok(()),
                Some(_) => {
                    if self.waiting.len() >= MOST_KEPT {
                        return Err("Too much arrived to be waited for");
                    }
                    let delivery = host.receive()?;
                    self.waiting.push_back(delivery);
                }
                None => return Err("The other side did not answer in time"),
            }
        }
        Err("Too much arrived to be waited for")
    }

    /// Write, on every stream the application opened for a share, the position that share is of, and wait for each to be
    /// taken. Where this device was asked or found since changes nothing of it.
    fn flush<H: Wire + Data>(&mut self, host: &mut H) {
        for (claim, alias, told) in std::mem::take(&mut self.outgoing) {
            // As it leaves now: the age it was found with, plus the time since.
            let bytes = wire::encode(&told.aged(self.now));
            let delivered = match host.open(true, claim) {
                Ok(handle) => {
                    let delivered = self.deliver(host, handle, &bytes);
                    host.close(handle);
                    delivered
                }
                Err(_) => false,
            };
            self.delivered.insert(alias, delivered);
        }
    }

    fn deliver<H: Wire + Data>(&mut self, host: &mut H, handle: Handle, bytes: &[u8]) -> bool {
        write_all(host, handle, bytes).is_ok()
            && host.shutdown(handle).is_ok()
            && self.wait(host, handle, &mut Patience::new()).is_ok()
            && read_ack(host, handle).is_ok_and(|count| u64::try_from(bytes.len()) == Ok(count))
    }

    /// A paired device shared its position: read it, keep it under the name the application gave that device.
    fn receive<H: Wire + Data>(&mut self, host: &mut H, delivery: &Value) -> Option<Value> {
        let ticket = delivery["ticket"].as_u64()?;
        let result = match self.take(host, ticket, &delivery["args"]) {
            Ok(()) => json!({"status":"ok","value":true}),
            Err(reason) => json!({"status":"error","code":"failed","message":reason}),
        };
        Some(json!({"op":"reply","ticket":ticket,"result":result}))
    }

    fn take<H: Wire + Data>(
        &mut self,
        host: &mut H,
        ticket: u64,
        args: &Value,
    ) -> Result<(), &'static str> {
        let alias = args["peer"]
            .as_str()
            .filter(|alias| !alias.is_empty() && alias.len() <= ALIAS_MAX)
            .ok_or("What was sent was not understood")?
            .to_owned();
        if !self.seen.contains_key(&alias) && self.seen.len() >= MOST_SEEN {
            return Err("Too many devices shared their location");
        }
        let handle = host
            .open(false, ticket)
            .map_err(|_| "What was sent was not understood")?;
        let read = self.read_whole(host, handle);
        host.close(handle);
        let shared = wire::parse(&read?).map_err(|_| "What was sent was not understood")?;
        self.seen.insert(
            alias,
            Told {
                shared,
                at: self.now,
            },
        );
        Ok(())
    }

    /// Everything a paired device wrote on `handle` until it closed it. However many pieces it arrives in, it is one
    /// message: its bytes ([`wire::MAX`]) and the time waited for it ([`STREAM_MS`]) bound it, never the number of reads.
    fn read_whole<H: Wire + Data>(
        &mut self,
        host: &mut H,
        handle: Handle,
    ) -> Result<Vec<u8>, &'static str> {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 512];
        let mut patience = Patience::new();
        // Ends within `wire::MAX + 2` reads: each takes at least one byte, the end takes none, one byte too many is refused.
        loop {
            self.wait(host, handle, &mut patience)?;
            // One byte more than the most, so that something longer is seen to be longer.
            let room = chunk.len().min(wire::MAX + 1 - bytes.len());
            let count = host.read(handle, &mut chunk[..room])?;
            if count == 0 {
                return Ok(bytes);
            }
            bytes.extend_from_slice(
                chunk
                    .get(..count)
                    .ok_or("What was sent was not understood")?,
            );
            if bytes.len() > wire::MAX {
                return Err("What was sent was not understood");
            }
        }
    }
}

/// What one delivery leads to. `Err` only when the application cannot be written to.
fn step<H: Wire + Data>(
    host: &mut H,
    state: &mut State,
    delivery: &Value,
) -> Result<(), &'static str> {
    let (calls, reply) = match delivery["type"].as_str() {
        Some("result") => (state.told(delivery), None),
        Some("call") if delivery["method"] == "receive" => {
            (Vec::new(), state.receive(host, delivery))
        }
        Some("call") => state.answer(delivery),
        _ => (Vec::new(), None),
    };
    for call in calls {
        host.send(call)?;
    }
    state.flush(host);
    if let Some(reply) = reply {
        host.send(reply)?;
    }
    Ok(())
}

/// The whole life of the guest: every delivery handled in the order it came (what was kept while a stream was waited for
/// comes first). Returns zero when the application goes away, non-zero when it cannot be written to.
pub fn run<H: Wire + Data>(host: &mut H) -> i32 {
    let mut state = State::default();
    loop {
        let Ok(delivery) = state.next(host) else {
            return 0;
        };
        if step(host, &mut state, &delivery).is_err() {
            return 1;
        }
    }
}

/// The entry the interpreter calls. Returns zero when the application goes away, non-zero on a broken protocol.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    run(&mut host::Host)
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
    let result = run(&mut host::Host);
    native::leave();
    result
}

#[cfg(test)]
mod tests;
