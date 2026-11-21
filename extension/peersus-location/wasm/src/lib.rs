//! A page that says where this device is and, on a drawn map with this device in the middle when it is known (otherwise
//! one of the devices that shared with it), where the paired devices that shared their location with it were: how far away,
//! in which direction and how long ago. The page lives in this extension's settings; its own option (`location-page`, read
//! from `peersus.config`) also places it among the application's pages (`peersus.pages`).
//!
//! This device's position is asked of the application (`peersus.location`, the person's `location.read`) only when the person
//! presses a button, or every five minutes while the page is open when they turned that on. Sharing sends it once to every
//! paired device that takes it now (`peersus.peers`: online, allowed, and its Location there accepts `location.position`);
//! the application admits it there only from a device that device's person allowed Location for (`location.share`). What
//! arrives is kept in memory only, one position per device, and is forgotten when the extension stops, the person presses
//! Forget, or the device is no longer listed as allowed (unpaired, or Location no longer allowed for it). Nothing is asked of another device: a position goes only when its own person shares it.
mod atlas;
mod borders;
mod canvas_scene;
mod canvas_sdk;
mod geo;
mod geometry;
mod host;
mod map_cache;
mod online;
mod streets;
mod tile;
mod wire;
mod world;
use canvas_scene::{CanvasIo, Hit};
use host::{Data, Handle, Wire, read_ack, read_limited, write_all};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

const PEERS: &str = "peersus.peers";
const LOCATION: &str = "peersus.location";
const CONFIG: &str = "peersus.config";
const PAGES: &str = "peersus.pages";
/// The page this extension declares; it lives in its settings.
const PAGE: &str = "where";
/// The person's option that also places the page among the application's own pages, and the id of that placement.
const PAGE_SETTING: &str = "location-page";
/// How long the application is given to answer an ordinary call, in milliseconds.
const CALL_MS: u32 = 5_000;
/// How long it is given to find this device: a system without a fix may need a few seconds (the application's own ceiling).
const FIND_MS: u32 = 20_000;
/// Longest single wait for more of what a paired device writes, in milliseconds. A guest has no clock: this is each wait,
/// not the whole (see [`Ready`]).
const WAIT_MS: i32 = 400;
/// Longest single wait for each read of the application's word that a share reached the other device, in milliseconds: it
/// says so only after it opened a stream to that device and wrote everything on it.
const ACK_MS: i32 = 3_000;
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
/// Scale change of the dedicated zoom buttons.
const CLOSER: f64 = 1.6;
/// A burst of this many logical wheel pixels doubles the magnification.
const WHEEL_PIXELS_PER_DOUBLING: u16 = 160;
const STREET_FOCUS: f64 = 0.006;
const FOCUS_MS: u64 = 500;
const DETAIL_AFTER_MOTION_MS: u64 = 150;
// Do not turn a render timeout into a raster too small to read the street network.
const MIN_PREVIEW_RASTER_SCALE: u32 = 240;
// A soft cap below the Canvas stream's rolling byte budget leaves room for
// headers, settled frames, and other work on the same worker.
const GESTURE_BYTES_PER_SECOND: u64 = 224 * 1024 * 1024;
/// How wide the map of the world opens when the person turns it on: a region, not the whole world.
fn focus_span(lon: f64, lat: f64) -> f64 {
    if streets::covered(lon, lat) {
        STREET_FOCUS
    } else {
        world::REGIONAL_SPAN
    }
}
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
    pub const NONE_ALLOWED: &str = "No paired device is on the map: a device is shown only once Location is allowed for it, in that device's permissions, on this device and on that one.";
    pub const NOT_YET: &str = "no location received yet";
    pub const NOT_READY: &str = "offline, or Location is not installed and allowed there";
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
    /// Keep sharing is due: it is sent once it is found, unless Keep sharing is turned off before.
    Keep,
}

/// One stream to a device for a share: asked of the application, then opened by it.
#[derive(Clone, Debug, PartialEq)]
struct Sending {
    /// The call that asked for the stream; once the application opened it, the claim it is written under.
    number: u64,
    alias: String,
    /// The position this share is of.
    told: Told,
}

/// The application's streams, read only when there is something to read: every read first waits until the stream can be
/// read, and keeps in `kept` what the application delivers meanwhile. A read never stands still, whatever the other side
/// does or stops doing: each single wait is `wait` milliseconds at most and is begun again only after such a delivery,
/// of which [`MOST_KEPT`] at most are kept, so one read waits `(MOST_KEPT + 1) * wait` at the very most. The whole of a
/// stream has no time of its own here (a guest has no clock): the application ends it with the call it belongs to.
struct Ready<'a, H> {
    host: &'a mut H,
    kept: &'a mut VecDeque<Value>,
    wait: i32,
}

impl<H: Wire + Data> Ready<'_, H> {
    fn wait(&mut self, handle: Handle) -> Result<(), &'static str> {
        for _ in 0..=MOST_KEPT {
            match self.host.poll(&[handle], self.wait)? {
                Some(ready) if ready == handle => return Ok(()),
                Some(_) => {
                    if self.kept.len() >= MOST_KEPT {
                        return Err("Too much arrived to be waited for");
                    }
                    let delivery = self.host.receive()?;
                    self.kept.push_back(delivery);
                }
                None => return Err("The other side did not answer in time"),
            }
        }
        Err("Too much arrived to be waited for")
    }
}

impl<H: Wire + Data> Data for Ready<'_, H> {
    fn open(&mut self, claim: bool, number: u64) -> Result<Handle, &'static str> {
        self.host.open(claim, number)
    }
    fn read(&mut self, handle: Handle, into: &mut [u8]) -> Result<usize, &'static str> {
        self.wait(handle)?;
        self.host.read(handle, into)
    }
    fn write(&mut self, handle: Handle, from: &[u8]) -> Result<usize, &'static str> {
        self.host.write(handle, from)
    }
    fn shutdown(&mut self, handle: Handle) -> Result<(), &'static str> {
        self.host.shutdown(handle)
    }
    fn close(&mut self, handle: Handle) {
        self.host.close(handle)
    }
    fn poll(
        &mut self,
        handles: &[Handle],
        timeout_ms: i32,
    ) -> Result<Option<Handle>, &'static str> {
        self.host.poll(handles, timeout_ms)
    }
}

#[derive(Default)]
pub struct State {
    calls: u64,
    /// Elapsed worker time, independent of the window process's clock origin.
    now: u64,
    ui_time: Option<u64>,
    own: Own,
    asked_own: u64,
    focus_fix: bool,
    /// Keep sharing asked: send as soon as this device is found, unless it is turned off before.
    share_kept: bool,
    devices: Vec<Device>,
    devices_denied: bool,
    asked_list: u64,
    listed_at: Option<u64>,
    seen: BTreeMap<String, Told>,
    /// Streams asked of the application for the last share, not open yet.
    sending: Vec<Sending>,
    /// Streams the application opened for it, to write that share's position on.
    outgoing: Vec<Sending>,
    /// What came of the last share to each device: whether every byte reached it.
    delivered: BTreeMap<String, bool>,
    /// The last share reached no device: nobody could take it.
    nobody: bool,
    /// When (page clock) a share last went out.
    shared_at: Option<u64>,
    /// When (page clock) Keep sharing last asked where this device is, found or not.
    kept_at: Option<u64>,
    waiting: VecDeque<Value>,
    /// The question for this extension's settings still out, and the one placing its page.
    asked_config: u64,
    asked_pages: u64,
    /// Whether the page was last placed among the application's pages; `None` until that is known to have been said.
    placed: Option<bool>,
    primary_surface: bool,
    other_surface: Viewport,
    focus_surface: bool,
    other_focus: Option<world::View>,
    camera: Viewport,
    canvas_buffer: Vec<u8>,
    retire_canvas: Option<Handle>,
    marker_stamp: u64,
    observe_pinch: bool,
    observe_rotate: bool,
    online: online::Online,
    online_enabled: bool,
    decoded_at: u64,
    config_at: u64,
    /// The question still out about the map areas saved on this device, and their last answer: how many and their bytes.
    asked_kept: u64,
    clearing_kept: bool,
    kept: Option<(u64, u64)>,
}

/// Each mounted page has its own measurements, gestures, and rendering cache.
#[derive(Default)]
pub struct Viewport {
    /// Location and sharing remain explicit actions, never consequences of opening the map.
    options: bool,
    /// The map of the world as they left it: its middle, and how many degrees fit across it.
    view: world::View,
    /// Base layers on this installation only. Markers are composited later and never cached.
    cache: std::cell::RefCell<map_cache::Cache>,
    surface: canvas_scene::Surface,
    last_page: Option<Value>,
    /// Last map gesture reflected in this viewport's frame, not the worker's clock.
    settled: u64,
    last_motion: Option<u64>,
    active_pointer: Option<(Hit, (f64, f64))>,
    focus: Option<Focus>,
    /// Where a finger pressed the map, while it is still down.
    dragging: Option<(f64, f64)>,
    touch: Option<TouchGesture>,
    /// Where the map is on the page, in thousandths of a pixel, as the application last measured it.
    map_at: Option<(f64, f64, f64, f64)>,
}

#[derive(Clone, Copy)]
struct TouchGesture {
    origin: world::View,
    anchor: Option<(f64, f64)>,
    globe: bool,
    scale: f64,
    turn: f64,
}

struct Focus {
    from: world::View,
    target: world::View,
    start: u64,
}

impl Focus {
    fn at(&self, now: u64) -> world::View {
        let t = now.saturating_sub(self.start).min(FOCUS_MS) as f64 / FOCUS_MS as f64;
        self.from.towards(self.target, t * t * (3. - 2. * t))
    }
}

impl std::ops::Deref for State {
    type Target = Viewport;
    fn deref(&self) -> &Viewport {
        &self.camera
    }
}
impl std::ops::DerefMut for State {
    fn deref_mut(&mut self) -> &mut Viewport {
        &mut self.camera
    }
}

fn px(n: u16) -> Value {
    json!({"unit":"px","value":n})
}
fn fill() -> Value {
    json!({"unit":"percent","value":10000})
}
fn bx(style: Value, children: Vec<Value>) -> Value {
    let mut node = json!({"k":"box"});
    // Move completed subtrees instead of serializing them again as `Value` through json!.
    node["style"] = style;
    node["children"] = Value::Array(children);
    node
}
fn text(text: &str, tone: &str) -> Value {
    json!({"k":"text","text":text,"tone":tone})
}
fn button(id: &str, label: &str) -> Value {
    json!({"k":"button","id":id,"label":label})
}
/// Measured geometry shared by the rail's drawing and its global pointer events.
#[derive(Clone, Copy)]
struct ZoomRail {
    frame: world::Frame,
    width: u16,
}

struct Elevation {
    text: String,
    bounds: [f64; 4],
    font: f32,
}

impl ZoomRail {
    const TARGET: u16 = 44;
    const EDGE: u16 = 6;
    const READOUT_FONT: f32 = 11.;

    fn new(frame: world::Frame) -> Option<Self> {
        let width = if frame.0 <= 720 { 38 } else { 46 };
        (frame.0 >= width + Self::EDGE && frame.1 >= 3 * Self::TARGET)
            .then_some(Self { frame, width })
    }

    fn left(self) -> u16 {
        self.frame.0 - self.width - Self::EDGE
    }

    fn bounds(self) -> [f64; 4] {
        [
            self.left(),
            Self::EDGE,
            self.width,
            self.frame.1 - 2 * Self::EDGE,
        ]
        .map(f64::from)
    }

    fn track_x(self) -> f64 {
        f64::from(self.left() + self.width - 5)
    }

    fn centre_x(self) -> f64 {
        self.track_x() - 10.
    }

    fn track(self) -> [f64; 2] {
        let margin = (Self::TARGET + 34).min((self.frame.1 - 14) / 2);
        [f64::from(margin), f64::from(self.frame.1 - margin)]
    }

    fn from_y(self, y: f64) -> f64 {
        let [top, bottom] = self.track();
        (y - top) / (bottom - top)
    }

    fn marker(self, view: world::View) -> f64 {
        let [top, bottom] = self.track();
        top + (bottom - top) * view.zoom_position()
    }

    fn middle(self) -> u16 {
        (self.frame.1 - Self::TARGET) / 2
    }

    fn elevation(self, view: world::View, camera: &world::Camera) -> Elevation {
        let metres = camera.elevation();
        let km = (metres / 1000.).round() as u64;
        let text = if km >= 1000 {
            format!("{} {:03} km", km / 1000, km % 1000)
        } else {
            geo::distance(metres)
        };
        let top = f64::from(Self::TARGET + 4);
        let bottom = f64::from(self.frame.1 - Self::TARGET - 4);
        let available = bottom - top;
        let font =
            f64::from(Self::READOUT_FONT).min((available - 6.) / tile::text_width(&text, 1.));
        let length = tile::text_width(&text, font) + 5.;
        let y = (self.marker(view) - length / 2.).clamp(top, bottom - length);
        Elevation {
            text,
            bounds: [f64::from(self.left()) + 3., y, 14., length],
            font: font as f32,
        }
    }
}

struct ScaleBar {
    top: u16,
    width: u16,
    metres: u64,
    length: f64,
}

impl ScaleBar {
    const INSET: u16 = 8;
    const HEIGHT: u16 = 36;
    const MIN_WIDTH: u16 = 88;

    fn new(viewport: world::Frame, camera: &world::Camera) -> Option<Self> {
        let right = ZoomRail::new(viewport).map_or(viewport.0, ZoomRail::left);
        let available = right.checked_sub(2 * Self::INSET)?;
        if available < Self::MIN_WIDTH || viewport.1 < 80 {
            return None;
        }
        let limit = (viewport.0 / 4)
            .clamp(Self::MIN_WIDTH, 240)
            .min(available - 2 * Self::INSET);
        let largest = camera.ground_distance(f64::from(limit)) as u64;
        let decade = 10_u64.pow(largest.checked_ilog10()?);
        let metres = [5, 2, 1]
            .into_iter()
            .map(|n| n * decade)
            .find(|&metres| metres <= largest)?;
        let length = camera.ground_pixels(metres as f64);
        let width = (length.ceil() as u16 + 2 * Self::INSET)
            .max(Self::MIN_WIDTH)
            .min(available);
        Some(Self {
            top: viewport.1 - Self::INSET - Self::HEIGHT,
            width,
            metres,
            length,
        })
    }

    fn bounds(&self) -> [f64; 4] {
        [
            f64::from(Self::INSET),
            f64::from(self.top),
            f64::from(self.width),
            f64::from(Self::HEIGHT),
        ]
    }

    fn caption(&self) -> String {
        if self.metres >= 1000 {
            format!("{} km", self.metres / 1000)
        } else {
            format!("{} m", self.metres)
        }
    }
}

fn map_label_bounds(frame: world::Frame) -> tile::layers::Rect {
    const GUTTER: i32 = 8;
    let right = ZoomRail::new(frame).map_or(frame.0, ZoomRail::left);
    tile::layers::Rect([
        GUTTER,
        GUTTER,
        i32::from(right) - GUTTER,
        i32::from(frame.1) - GUTTER,
    ])
}

fn marker_badge_bounds(
    name: &str,
    x: f64,
    y: f64,
    bounds: tile::layers::Rect,
    clear: impl Fn([f64; 4]) -> bool,
) -> Option<[f64; 4]> {
    let width = (8. + tile::text_width(name, 15.)).max(40.);
    let height = 26.;
    let [left, top, right, bottom] = bounds.0.map(f64::from);
    if width > 148. || width > right - left || height > bottom - top {
        return None;
    }
    let dot = [x - 16., y - 16., 32., 32.];
    for (x, y) in [
        (x + 20., y - height / 2.),
        (x - 20. - width, y - height / 2.),
        (x - width / 2., y + 20.),
        (x - width / 2., y - 20. - height),
    ] {
        let area = [
            x.clamp(left, right - width),
            y.clamp(top, bottom - height),
            width,
            height,
        ];
        if clear(area) && !overlaps(area, dot) {
            return Some(area);
        }
    }
    None
}

fn overlaps([x, y, w, h]: [f64; 4], [a, b, c, d]: [f64; 4]) -> bool {
    x < a + c + 4. && x + w + 4. > a && y < b + d + 4. && y + h + 4. > b
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

/// A number an event carries, or zero where it carries none.
fn number(value: &Value) -> f64 {
    value.as_f64().unwrap_or(0.0)
}

/// How wide a view shows, in degrees, as a person reads it.
fn across(span: f64) -> String {
    if span >= 10.0 {
        format!("{span:.0}°")
    } else if span >= 1.0 {
        format!("{span:.1}°")
    } else {
        format!("{span:.2}°")
    }
}

/// A number of bytes as a person reads it.
fn saved_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else {
        format!("{} KB", bytes.div_ceil(KB))
    }
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

fn page_supports(config: &Value, feature: &str) -> bool {
    config["page_features"]
        .as_array()
        .is_some_and(|items| items.iter().any(|f| f == feature))
}

impl State {
    fn call(&mut self, service: &str, method: &str, args: Value, timeout: u32) -> (u64, Value) {
        self.calls = self.calls.checked_add(1).unwrap_or(1);
        let call = json!({"op":"call","id":self.calls,"provider":"host","service":service,
            "version":1,"method":method,"args":args,"timeout_ms":timeout});
        (self.calls, call)
    }

    fn online_call(&mut self, request: online::Call, track: bool) -> Value {
        let (id, call) = self.call(
            online::SERVICE,
            request.method,
            request.args,
            request.timeout,
        );
        if track {
            self.online.sent(id);
        }
        call
    }

    fn list(&mut self) -> Value {
        let (id, call) = self.call(PEERS, "list", json!({}), CALL_MS);
        self.asked_list = id;
        self.listed_at = Some(self.now);
        call
    }

    /// Ask how many map areas this device keeps for this extension and their bytes, after removing them all when `clear`.
    fn ask_kept(&mut self, clear: bool) -> Value {
        let method = if clear {
            "clear-resource-cache"
        } else {
            "resource-cache"
        };
        let (id, call) = self.call(online::SERVICE, method, json!({}), CALL_MS);
        self.asked_kept = id;
        self.clearing_kept = clear;
        call
    }

    /// Ask this extension's settings: whether the person wants its page among the application's own. Asked when it starts
    /// and whenever the devices are listed, so a change of the option is followed while the page is open in the settings.
    pub fn ask_settings(&mut self) -> Value {
        let (id, call) = self.call(CONFIG, "get", json!({}), CALL_MS);
        self.asked_config = id;
        call
    }

    /// Place the page among the application's pages, or take it out, when the option says otherwise than what was said last.
    fn place(&mut self, config: &Value) -> Option<Value> {
        self.observe_pinch = page_supports(config, "pinch");
        self.observe_rotate = page_supports(config, "rotate");
        let wanted = config["settings"][PAGE_SETTING].as_bool().unwrap_or(false);
        if self.placed == Some(wanted) {
            return None;
        }
        let pages = if wanted {
            json!([{"id":PAGE_SETTING,"page":PAGE,"placement":"primary"}])
        } else {
            json!([])
        };
        let (id, call) = self.call(PAGES, "set", json!({ "pages": pages }), CALL_MS);
        self.asked_pages = id;
        self.placed = Some(wanted);
        Some(call)
    }

    /// Ask where this device is, for `ask`. One answer serves every reason it was asked for: while one is awaited, nothing
    /// more is asked.
    // A local focus request is independent from any in-flight share of the same fix.
    fn find(&mut self, ask: Ask) -> Option<Value> {
        match ask {
            Ask::Show => {
                self.focus_fix = true;
                self.focus_surface = self.primary_surface;
            }
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
    /// `kept` when only Keep sharing asked for it. What an earlier share still waited for is dropped: it is no longer the
    /// last one.
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
                let (number, call) = self.call(
                    PEERS,
                    "send",
                    json!({"device":alias,"service":wire::SERVICE,"version":wire::VERSION}),
                    CALL_MS,
                );
                self.sending.push(Sending {
                    number,
                    alias,
                    told,
                });
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
        if self.online.request(id) {
            let next = self.online.result(result, self.now);
            return next
                .map(|r| self.online_call(r, true))
                .into_iter()
                .collect();
        }
        let ok = result["status"] == "ok";
        let denied = result["code"] == "denied";
        if id == self.asked_kept {
            self.asked_kept = 0;
            self.clearing_kept = false;
            let value = &result["value"];
            self.kept = ok
                .then(|| value["resources"].as_u64().zip(value["bytes"].as_u64()))
                .flatten();
            return Vec::new();
        }
        if id == self.asked_config {
            self.asked_config = 0;
            if !ok || !result["value"].is_object() {
                return Vec::new();
            }
            let config = &result["value"];
            let enabled = config["settings"]["online-streets"]
                .as_bool()
                .unwrap_or(false);
            let mut calls: Vec<_> = self.place(config).into_iter().collect();
            if enabled != self.online_enabled {
                self.online_enabled = enabled;
                if enabled {
                    let restore = self.online.restore();
                    calls.push(self.online_call(restore, true));
                } else {
                    let (cancel, release) = self.online.disable();
                    if cancel != 0 {
                        calls.push(json!({"op":"cancel","id":cancel}));
                    }
                    if let Some(release) = release {
                        calls.push(self.online_call(release, false));
                    }
                }
            }
            self.online.cache_recent(
                config["settings"]["cache-online-areas"]
                    .as_bool()
                    .unwrap_or(true),
            );
            return calls;
        }
        if id == self.asked_pages {
            self.asked_pages = 0;
            if !ok {
                // Not placed after all: said again with the next answer of the settings.
                self.placed = None;
            }
            return Vec::new();
        }
        if id == self.asked_list {
            self.marker_stamp = self.marker_stamp.wrapping_add(1);
            self.asked_list = 0;
            self.devices_denied = denied;
            if let Some(peers) = result["value"].as_array().filter(|_| ok) {
                self.listed(peers);
                if self.devices.iter().any(|d| d.ready)
                    && self
                        .kept_at
                        .is_none_or(|at| self.now.saturating_sub(at) >= SHARE_EVERY)
                {
                    return self.find(Ask::Keep).into_iter().collect();
                }
            } else {
                // No device may be seen any more: nothing of one is shown either.
                self.devices.clear();
                self.seen.clear();
                self.sending.clear();
                self.outgoing.clear();
                self.share_kept = false;
            }
        } else if id == self.asked_own {
            self.marker_stamp = self.marker_stamp.wrapping_add(1);
            self.asked_own = 0;
            let share = std::mem::take(&mut self.share_kept);
            let focus = std::mem::take(&mut self.focus_fix);
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
                    if focus {
                        let view = world::View {
                            lon: shared.fix.longitude,
                            lat: shared.fix.latitude,
                            span: focus_span(shared.fix.longitude, shared.fix.latitude),
                            roll: self.view.roll,
                        }
                        .kept();
                        if self.focus_surface == self.primary_surface {
                            self.view = view;
                        } else {
                            self.other_focus = Some(view);
                        }
                    }
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
        } else if let Some(index) = self.sending.iter().position(|sending| sending.number == id) {
            let sending = self.sending.swap_remove(index);
            match result["value"]["claim"].as_u64().filter(|_| ok) {
                Some(claim) => self.outgoing.push(Sending {
                    number: claim,
                    ..sending
                }),
                None => {
                    self.delivered.insert(sending.alias, false);
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
        let ready = |s: &Sending| devices.iter().any(|d| d.ready && d.alias == s.alias);
        self.sending.retain(ready);
        self.outgoing.retain(ready);
        if !devices.iter().any(|d| d.ready) {
            self.share_kept = false;
        }
    }

    /// What a person did on the page, and the calls it leads to.
    fn act(&mut self, event: &Value) -> Vec<Value> {
        if let Some(at) = event["at"].as_u64() {
            // A reopened window has a fresh clock. Only elapsed time advances
            // retries and sharing; a new epoch never holds them back.
            let elapsed = self
                .ui_time
                .replace(at)
                .map_or(0, |last| at.saturating_sub(last));
            if elapsed <= 30_000 {
                self.now = self.now.saturating_add(elapsed);
            }
        }
        let mut calls = Vec::new();
        let before = self.view;
        if matches!(
            event["t"].as_str(),
            Some("open" | "wheel" | "pinch" | "rotate" | "pointer" | "press")
        ) {
            self.focus = None;
        }
        match (event["t"].as_str(), event["id"].as_str()) {
            (Some("open"), _) => {
                // A newly mounted map has no gesture of its own.
                self.settled = 0;
                self.last_motion = None;
                self.dragging = None;
                self.touch = None;
                self.active_pointer = None;
                self.map_at = None;
                self.retire_canvas = self.surface.stream.take();
                self.surface = canvas_scene::Surface::default();
                self.last_page = None;
                *self.cache.get_mut() = map_cache::Cache::default();
                calls.extend([self.list(), self.ask_settings()]);
            }
            (Some("press"), Some("find")) => calls.extend(self.find(Ask::Show)),
            (Some("press"), Some("forget")) => {
                self.seen.clear();
                self.marker_stamp = self.marker_stamp.wrapping_add(1);
            }
            (Some("press"), Some("options")) => {
                self.options = !self.options;
                if self.options && !self.clearing_kept {
                    calls.push(self.ask_kept(false));
                }
            }
            (Some("press"), Some("clear-kept")) if !self.clearing_kept => {
                let cancel = self.online.forget_tiles();
                if cancel != 0 {
                    calls.push(json!({"op":"cancel","id":cancel}));
                }
                calls.push(self.ask_kept(true));
            }
            (Some("press"), Some("centre")) => self.centre_first(),
            (Some("press"), Some("closer")) => self.view = self.view.zoomed(CLOSER, None),
            (Some("press"), Some("wider")) => self.view = self.view.zoomed(1.0 / CLOSER, None),
            (Some("press"), Some("world")) => self.view = world::View::whole(),
            (Some("pointer"), Some("map")) => self.pointer(event),
            (Some("pinch"), Some("map")) => self.two_fingers(event, false),
            (Some("rotate"), Some("map")) if self.observe_rotate => self.two_fingers(event, true),
            (Some("wheel"), Some("map")) => self.wheeled(event),
            (Some("canvas_size"), Some("map")) => self.sized(event),
            (Some("tick"), _) => {
                if let Some(focus) = self.camera.focus.as_ref() {
                    self.camera.view = focus.at(self.now);
                    if self.now.saturating_sub(focus.start) >= FOCUS_MS {
                        self.focus = None;
                    }
                }
                if self.asked_config == 0
                    && self.now.saturating_sub(self.config_at) >= u64::from(TICK)
                {
                    self.config_at = self.now;
                    calls.push(self.ask_settings());
                }
                if self.asked_list == 0
                    && self
                        .listed_at
                        .is_none_or(|at| self.now.saturating_sub(at) >= LIST_EVERY)
                {
                    calls.extend([self.list(), self.ask_settings()]);
                }
            }
            _ => {}
        }
        if self.view != before
            && event["id"] == "map"
            && matches!(
                event["t"].as_str(),
                Some("pointer" | "wheel" | "pinch" | "rotate")
            )
            && let Some(at) = event["at"].as_u64()
        {
            // Only frames whose camera changed need a new input stamp.
            self.settled = at;
            self.last_motion = Some(self.now);
        }
        if self.online_enabled && !self.clearing_kept && self.focus.is_none() {
            let (view, frame) = (self.view, self.frame());
            let cancel = self.online.cached_first(self.now, view, frame);
            if cancel != 0 {
                calls.push(json!({"op":"cancel","id":cancel}));
            }
            if let Some(request) = self.online.advance(self.now, view, frame) {
                calls.push(self.online_call(request, true));
            }
        }
        calls
    }

    /// The reply to a call of the page's service: the page as it is after what the person did.
    fn answer(&mut self, delivery: &Value) -> (Vec<Value>, Option<Value>, bool) {
        let Some(ticket) = delivery["ticket"].as_u64() else {
            return (Vec::new(), None, true);
        };
        if delivery["method"] != "event" {
            let refused =
                json!({"status":"error","code":"failed","message":"Nothing here answers that"});
            return (
                Vec::new(),
                Some(json!({"op":"reply","ticket":ticket,"result":refused})),
                true,
            );
        }
        let primary = delivery["args"]["page"].as_str() == Some(PAGE_SETTING);
        if primary != self.primary_surface {
            std::mem::swap(&mut self.camera, &mut self.other_surface);
            self.primary_surface = primary;
        }
        if self.primary_surface == self.focus_surface
            && let Some(view) = self.other_focus.take()
        {
            self.view = view;
        }
        let event = &delivery["args"]["event"];
        let calls = self.act(event);
        let page = self.page();
        let value = if self.last_page.as_ref() == Some(&page) {
            Value::Null
        } else {
            self.last_page = Some(page.clone());
            page
        };
        let reply = json!({"op":"reply","ticket":ticket,"result":{"status":"ok","value":value}});
        (calls, Some(reply), event["t"] == "tick")
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

    /// Most devices said one by one in Options; a longer list ends with how many more there are.
    const DEVICES_SAID: usize = 6;

    /// The paired devices allowed here, each with what is known of where it is or why nothing is. A device is on the map
    /// only once it shared a position, and that needs Location installed and allowed on both sides: without these lines a
    /// map with nobody on it says nothing of which of those is missing. The application lists no device Location is
    /// allowed nothing for, so none listed is said the same way whether or not any device is paired.
    fn device_lines(&self) -> Vec<String> {
        if self.devices.is_empty() {
            return (!self.devices_denied)
                .then(|| words::NONE_ALLOWED.to_owned())
                .into_iter()
                .collect();
        }
        let mut lines: Vec<String> = self
            .devices
            .iter()
            .take(Self::DEVICES_SAID)
            .map(|device| {
                let state = match self.seen.get(&device.alias) {
                    Some(told) => {
                        let shared = told.aged(self.now);
                        described(&shared, geo::coordinates(&shared.fix))
                    }
                    None if device.ready => words::NOT_YET.to_owned(),
                    None => words::NOT_READY.to_owned(),
                };
                format!("{}: {state}", device.name)
            })
            .collect();
        if let Some(more) = self
            .devices
            .len()
            .checked_sub(Self::DEVICES_SAID)
            .filter(|&more| more > 0)
        {
            lines.push(format!("and {more} more"));
        }
        lines
    }

    fn centre_first(&mut self) {
        self.view = self.view.north_up();
        self.touch = None;
        let first = self
            .own_now()
            .or_else(|| self.seen.values().next().map(|told| told.shared))
            .map(|shared| (shared.fix.longitude, shared.fix.latitude));
        if let Some((lon, lat)) = first {
            self.view = world::View {
                lon,
                lat,
                span: self.view.span.min(focus_span(lon, lat)),
                roll: 0.,
            }
            .kept();
        }
    }

    fn frame(&self) -> world::Frame {
        self.map_at
            .map_or(world::Frame(world::WIDTH, world::HEIGHT), |(_, _, w, h)| {
                world::Frame(
                    w.round().clamp(1., 16_384.) as u16,
                    h.round().clamp(1., 16_384.) as u16,
                )
            })
    }

    fn pointer(&mut self, event: &Value) {
        let phase = event["phase"].as_str();
        if phase == Some("down") {
            let Some(point) = self.pointed(event) else {
                return;
            };
            self.touch = None;
            self.dragging = None;
            self.active_pointer = Some((
                canvas_scene::hit(&self.surface.picture.markers, self.frame(), point),
                point,
            ));
        }
        if !matches!(phase, Some("down" | "move" | "up")) {
            self.active_pointer = None;
            self.dragging = None;
            return;
        }
        let Some((target, start)) = self.active_pointer.take() else {
            return;
        };
        let point = self.local_point(event);
        match &target {
            Hit::Map => self.dragged(event),
            Hit::Rail => {
                if let (Some(rail), Some((_, y))) = (ZoomRail::new(self.frame()), point) {
                    self.view = self.view.zoom_at(rail.from_y(y));
                }
            }
            _ => {
                if point.is_none_or(|(x, y)| (x - start.0).hypot(y - start.1) >= 8.) {
                    return;
                }
                if phase == Some("up") {
                    match target {
                        Hit::Centre => self.centre_first(),
                        Hit::Closer => self.view = self.view.zoomed(CLOSER, None),
                        Hit::Wider => self.view = self.view.zoomed(1. / CLOSER, None),
                        Hit::Marker(id) => self.focus_marker(&id),
                        _ => {}
                    }
                    return;
                }
            }
        }
        if phase != Some("up") {
            self.active_pointer = Some((target, start));
        }
    }

    fn focus_marker(&mut self, id: &str) {
        let Some(fix) = (if id == canvas_scene::OWN {
            self.own_now()
        } else {
            self.seen.get(id).map(|told| told.shared)
        })
        .map(|shared| shared.fix) else {
            return;
        };
        let span = if self.online_enabled {
            STREET_FOCUS
        } else {
            focus_span(fix.longitude, fix.latitude)
        };
        let target = world::View {
            lon: fix.longitude,
            lat: fix.latitude,
            span: self.view.span.min(span),
            roll: self.view.roll,
        }
        .kept();
        if target != self.view {
            self.focus = Some(Focus {
                from: self.view,
                target,
                start: self.now,
            });
        }
    }

    /// The globe uses a true 3D rotation. In a local map, panning keeps its existing bearing.
    fn dragged(&mut self, event: &Value) {
        let (x, y) = (number(&event["x"]), number(&event["y"]));
        match event["phase"].as_str() {
            Some("down") => {
                self.dragging = Some((x, y));
                self.touch = None;
            }
            Some("move") => {
                if let Some((last_x, last_y)) = self.dragging {
                    let frame = self.frame();
                    let camera = world::Camera::new(self.view, frame);
                    let (left, top, _, _) = self.map_at.unwrap_or_default();
                    let from = ((last_x - left) / 1000., (last_y - top) / 1000.);
                    let to = ((x - left) / 1000., (y - top) / 1000.);
                    self.view = if !camera.local_map() {
                        camera.trackball(from, to)
                    } else {
                        camera
                            .locate(from.0, from.1)
                            .and_then(|place| world::Camera::grab(self.view, frame, place, to))
                            .unwrap_or_else(|| camera.orbit(to.0 - from.0, to.1 - from.1))
                    };
                    self.dragging = Some((x, y));
                }
            }
            _ => self.dragging = None,
        }
    }

    /// Follow the accumulated wheel or touchpad distance and keep the place under the pointer fixed.
    fn wheeled(&mut self, event: &Value) {
        let delta = number(&event["dy"]) / 1000.;
        if delta == 0. {
            return;
        }
        let factor = (-delta / f64::from(WHEEL_PIXELS_PER_DOUBLING)).exp2();
        let next = self.view.zoomed(factor, None);
        if next == self.view {
            return;
        }
        let pixel = self.pointed(event);
        let frame = self.frame();
        let place = pixel.and_then(|(x, y)| world::Camera::new(self.view, frame).locate(x, y));
        self.view = match (pixel, place) {
            (Some(pixel), Some(place)) => world::Camera::anchor(next, frame, place, pixel),
            _ => next,
        };
    }

    /// The owner reports absolute samples for pinch and turn from the same contact pair.
    fn two_fingers(&mut self, event: &Value, rotate: bool) {
        let pixel = self.pointed(event);
        let frame = self.frame();
        if event["begin"] == true {
            self.touch = pixel.map(|(x, y)| {
                let origin = self.view;
                let camera = world::Camera::new(origin, frame);
                TouchGesture {
                    origin,
                    anchor: camera.locate(x, y),
                    globe: !camera.local_map(),
                    scale: 1.,
                    turn: 0.,
                }
            });
            return;
        }
        let (Some(mut touch), Some(pixel)) = (self.touch, pixel) else {
            return;
        };
        if rotate {
            let Some(turn) = event["turn"].as_i64().and_then(|n| i32::try_from(n).ok()) else {
                return;
            };
            touch.turn = f64::from(turn) / 100.;
        } else {
            let Some(scale) = event["scale"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|&n| n > 0)
            else {
                return;
            };
            touch.scale = f64::from(scale) / 10000.;
        }
        self.touch = Some(touch);
        let rotated = |view: world::View| {
            world::View {
                roll: touch.origin.roll + touch.turn,
                ..view
            }
            .kept()
        };
        let next = touch.origin.zoomed(touch.scale, None);
        let anchor = |view| {
            touch.anchor.map_or(view, |place| {
                world::Camera::anchor(view, frame, place, pixel)
            })
        };
        self.view = if touch.globe {
            // The turn itself rotates about the view axis, not the off-centre pinch midpoint.
            rotated(if touch.scale == 1. {
                next
            } else {
                anchor(next)
            })
        } else {
            anchor(rotated(next))
        };
    }

    fn local_point(&self, event: &Value) -> Option<(f64, f64)> {
        let (left, top, _, _) = self.map_at?;
        Some((
            (number(&event["x"]) - left) / 1000.,
            (number(&event["y"]) - top) / 1000.,
        ))
    }

    /// Pointer in local logical pixels, inside the last measured map.
    fn pointed(&self, event: &Value) -> Option<(f64, f64)> {
        let (_, _, w, h) = self.map_at?;
        let (x, y) = self.local_point(event)?;
        ((0. ..=w).contains(&x) && (0. ..=h).contains(&y)).then_some((x, y))
    }

    fn sized(&mut self, event: &Value) {
        if let Some(metrics) = canvas_scene::Metrics::from_event(event) {
            self.map_at = Some(metrics.bounds);
            if self.surface.metrics != Some(metrics) {
                self.surface.raster_pressure = None;
            }
            self.surface.metrics = Some(metrics);
        }
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

    fn page(&self) -> Value {
        let mut observe = vec!["wheel", "size"];
        if self.observe_pinch {
            observe.push("pinch");
        }
        if self.observe_rotate {
            observe.push("rotate");
        }
        let map = json!({"k":"canvas","id":canvas_scene::ID,"observe":observe,
            "style":{"width":fill(),"height":{"unit":"flex","value":1},"clip":true,"radius":[6,6,6,6]},
            "a11y":[{"id":"map-description","role":"image","label":"Location map. Drag to pan, scroll or pinch to zoom, and twist to rotate.","rect":{"x":0,"y":0,"width":1000,"height":1000}}]});
        let online_drawn = self.surface.picture.online_drawn;
        let toolbar = bx(
            json!({"horizontal":true,"wrap":true,"gap":4,"padding":4}),
            vec![button("options", "Options")],
        );
        let (lon, lat) = self.view.centre();
        let updating = self.online_enabled && self.online.busy(self.now, self.view, self.frame());
        let attribution = if online_drawn {
            Some("© OpenStreetMap contributors · © OpenMapTiles · OpenFreeMap")
        } else if self.view.span < 0.4 && streets::covered(lon, lat) {
            Some("© OpenStreetMap contributors · ODbL")
        } else if updating {
            Some("Loading online streets…")
        } else if self.view.span < 0.4 && !self.online_enabled {
            Some("Street detail is off: turn on OpenStreetMap street detail in Location's settings")
        } else {
            None
        };
        let mut viewport = vec![map];
        if self.options {
            let mut panel = Vec::new();
            if let Some(notice) = self.online.notice() {
                panel.push(text(notice, "muted"));
            }
            panel.extend([
                button("find", "Locate this device"),
                button("forget", "Forget received locations"),
                button("world", "Whole world"),
                text(&format!("View: {} across", across(self.view.span)), "muted"),
            ]);
            if !matches!(self.own, Own::Unknown) {
                panel.push(text(&self.own_line(), "muted"));
            }
            if self.devices_denied {
                panel.push(text(words::DEVICES_DENIED, "muted"));
            }
            for line in self.device_lines() {
                panel.push(text(&line, "muted"));
            }
            if let Some((resources, bytes)) = self.kept {
                panel.push(text(
                    &format!(
                        "Map areas saved on this device: {resources} ({})",
                        saved_size(bytes)
                    ),
                    "muted",
                ));
                if resources > 0 {
                    panel.push(button("clear-kept", "Clear saved map areas"));
                }
            }
            if let Some(line) = self.share_line() {
                panel.push(text(&line, "muted"));
            }
            viewport.push(bx(
                json!({"overlay":true,"at":[4000,4000],"width":px(self.frame().0.saturating_sub(8).clamp(1, 320)),
                    "padding":8,"gap":6,"radius":[8,8,8,8],"background":"raised","border":true}),
                panel,
            ));
        }
        let viewport = bx(
            json!({"width":fill(),"height":{"unit":"flex","value":1},"clip":true}),
            viewport,
        );
        let mut content = vec![toolbar, viewport];
        // Reserve the same space as the provider connects and tiles arrive.
        // The credit is outside the map and never changes the camera viewport.
        content.push(bx(
            json!({"width":fill(),"height":px(38),"clip":true}),
            attribution
                .into_iter()
                .map(|caption| text(caption, "muted"))
                .collect(),
        ));
        let root = bx(
            json!({"width":fill(),"height":fill(),"clip":true,"gap":4}),
            content,
        );
        let tick = if updating
            || self.focus.is_some()
            || self.cache.borrow().pending()
            || self.surface.retry_at != 0
            || self.frame_stamp().is_some_and(|stamp| {
                self.surface.painted != Some(stamp) || self.map_moving() && stamp.detail.preview()
            }) {
            100
        } else {
            TICK
        };
        let mut interval = json!({"k":"interval","id":"clock","milliseconds":tick});
        interval["child"] = root;
        interval
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

    /// Write, on every stream the application opened for a share, the position that share is of, and wait for the
    /// application to say each reached its device. Where this device was asked or found since changes nothing of it.
    fn flush<H: Wire + Data>(&mut self, host: &mut H) {
        for sending in std::mem::take(&mut self.outgoing) {
            // As it leaves now: the age it was found with, plus the time since. One grown too old for another Location to
            // understand is not written.
            let delivered = wire::encode(&sending.told.aged(self.now)).is_some_and(|bytes| {
                match host.open(true, sending.number) {
                    Ok(handle) => {
                        let delivered = self.deliver(host, handle, &bytes);
                        host.close(handle);
                        delivered
                    }
                    Err(_) => false,
                }
            });
            self.delivered.insert(sending.alias, delivered);
        }
    }

    fn deliver<H: Wire + Data>(&mut self, host: &mut H, handle: Handle, bytes: &[u8]) -> bool {
        write_all(host, handle, bytes).is_ok()
            && host.shutdown(handle).is_ok()
            && read_ack(
                &mut Ready {
                    host,
                    kept: &mut self.waiting,
                    wait: ACK_MS,
                },
                handle,
            )
            .is_ok_and(|count| u64::try_from(bytes.len()) == Ok(count))
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
        // However many pieces it arrives in, it is one message. Its bytes bound it (one more than the most is read, so that
        // something longer is seen to be longer) and so does time: each piece is waited for `WAIT_MS` at most, and the
        // application ends the stream when the time it gives this call is over, which is read here as its end.
        let read = read_limited(
            &mut Ready {
                host,
                kept: &mut self.waiting,
                wait: WAIT_MS,
            },
            handle,
            wire::MAX,
        );
        host.close(handle);
        let shared = wire::parse(&read?).map_err(|_| "What was sent was not understood")?;
        self.marker_stamp = self.marker_stamp.wrapping_add(1);
        self.seen.insert(
            alias,
            Told {
                shared,
                at: self.now,
            },
        );
        Ok(())
    }
}

impl State {
    fn open_canvas(&mut self, host: &mut impl CanvasIo, claim: &Value) {
        self.surface.opening = None;
        self.surface.stream = claim["value"]["claim"]
            .as_u64()
            .filter(|_| claim["status"] == "ok")
            .and_then(|claim| host.open_canvas(claim));
        self.surface.seq = 0;
        self.surface.painted = None;
        if self.surface.stream.is_none() {
            self.surface.retry_at = self.now.saturating_add(500);
        }
    }

    fn frame_stamp(&self) -> Option<canvas_scene::Stamp> {
        let mut stamp = canvas_scene::Stamp {
            metrics: self.surface.metrics?,
            view: self.view.kept(),
            settled: self.settled,
            markers: self.marker_stamp,
            online: self.online_enabled.then(|| self.online.revision()),
            detail: tile::Detail::Full,
        };
        if self.sharp_gesture() {
            stamp.detail = tile::Detail::Gesture;
        } else if let Some(pressure) = &self.surface.raster_pressure
            && (self.map_moving() || pressure.full_refused == Some(stamp))
        {
            stamp.metrics = stamp.metrics.at_scale(pressure.scale);
            stamp.detail = tile::Detail::Pressure;
        }
        Some(stamp)
    }

    fn map_moving(&self) -> bool {
        self.last_motion
            .is_some_and(|at| self.now.saturating_sub(at) < DETAIL_AFTER_MOTION_MS)
    }

    fn sharp_gesture(&self) -> bool {
        self.map_moving()
            && self.surface.metrics.is_some_and(|metrics| {
                let (_, _, width, height) = metrics.bounds;
                metrics.raster.scale >= 2000 && width.min(height) <= 720.
            })
    }

    fn pump_canvas(&mut self, host: &mut impl CanvasIo, advance: bool) -> Result<(), &'static str> {
        if self.now < self.surface.retry_at {
            return Ok(());
        }
        let Some(stamp) = self.frame_stamp() else {
            return Ok(());
        };
        let Some(stream) = self.surface.stream else {
            if self.surface.opening.is_none() {
                let page = if self.primary_surface {
                    PAGE_SETTING
                } else {
                    PAGE
                };
                let (id, call) = self.call(
                    canvas_sdk::SERVICE,
                    "open",
                    json!({"page":page,"canvas":canvas_scene::ID}),
                    CALL_MS,
                );
                self.surface.opening = Some(id);
                host.send(call)?;
            }
            return Ok(());
        };
        if stamp.detail == tile::Detail::Gesture
            && self.surface.painted.is_some_and(|previous| {
                previous.detail == tile::Detail::Gesture
                    && previous.metrics == stamp.metrics
                    && previous.markers == stamp.markers
            })
        {
            let [width, height] = stamp.metrics.raster.pixels.map(u64::from);
            let spacing = (width * height * 4 * 1000)
                .div_ceil(GESTURE_BYTES_PER_SECOND)
                .max(24);
            if self.now.saturating_sub(self.surface.painted_at) < spacing {
                return Ok(());
            }
        }
        // A gesture immediately paints its new camera. Finish finer offline roads
        // after the input settles, not in a frame that the next sample will discard.
        let moving = self.map_moving();
        let advance = advance && !moving;
        if self.surface.retry_at == 0
            && self.surface.painted == Some(stamp)
            && !(advance && self.cache.borrow().pending())
        {
            return Ok(());
        }
        // Keep the accepted full map on screen while a new viewport is loading.
        // A resize, theme or marker change must still get a coherent fallback.
        if !moving
            && self.online_enabled
            && !self.clearing_kept
            && self.surface.picture.online_drawn
            && self.surface.painted.is_some_and(|painted| {
                painted.metrics.bounds == stamp.metrics.bounds
                    && painted.metrics.palette == stamp.metrics.palette
                    && painted.markers == stamp.markers
            })
            && self
                .online
                .awaiting_coverage(stamp.view, self.frame(), self.now)
        {
            return Ok(());
        }
        let Some(seq) = self.surface.seq.checked_add(1) else {
            return Err("Canvas sequence exhausted");
        };
        let metrics = stamp.metrics;
        let mut bytes = std::mem::take(&mut self.canvas_buffer);
        let mut list = canvas_sdk::List::begin(
            &mut bytes,
            canvas_sdk::Frame {
                id: canvas_scene::ID,
                stream,
                seq,
                settled: stamp.settled,
                raster: metrics.raster,
                palette: &metrics.palette,
            },
        );
        let picture = canvas_scene::render(self, stamp, advance, &mut list);
        match list.finish().map(|bytes| host.present_canvas(bytes)) {
            Some(canvas_sdk::PRESENT_OK) => {
                self.surface.seq = seq;
                self.surface.painted = Some(stamp);
                self.surface.painted_at = self.now;
                self.surface.picture = picture;
                self.surface.retry_at = 0;
                if !moving && !stamp.detail.preview() {
                    self.surface.raster_pressure = None;
                }
                if !self.surface.picture.previewing_tiles {
                    self.online.frame_shown(stamp.view, self.frame());
                }
            }
            Some(canvas_sdk::PRESENT_LIMITED) => {
                // The same status also reports a full stream quota. In the
                // sharp gesture path, hold the last frame, not a blurrier one.
                if self.surface.seq > 0 && stamp.detail != tile::Detail::Gesture {
                    let scale = metrics.raster.scale;
                    let floor = MIN_PREVIEW_RASTER_SCALE
                        .min(self.surface.metrics.map_or(scale, |m| m.raster.scale));
                    let pressure =
                        self.surface
                            .raster_pressure
                            .get_or_insert(canvas_scene::RasterPressure {
                                scale,
                                full_refused: None,
                            });
                    if !stamp.detail.preview() && !moving {
                        pressure.full_refused = Some(stamp);
                    }
                    if scale <= pressure.scale {
                        pressure.scale = (scale * 3 / 5).max(floor);
                    }
                }
                self.surface.retry_at = self.now.saturating_add(34);
            }
            Some(canvas_sdk::PRESENT_STREAM_FAILED) => {
                self.surface.stream = None;
                self.surface.retry_at = self.now.saturating_add(100);
            }
            _ => {
                // A malformed scene must not stop location sharing or spin on the frame import.
                self.surface.retry_at = self.now.saturating_add(1000);
            }
        }
        self.canvas_buffer = bytes;
        Ok(())
    }
}

/// What one delivery leads to. Online decode and map painting use different fuel turns.
fn step<H: CanvasIo>(
    host: &mut H,
    state: &mut State,
    delivery: &Value,
) -> Result<(), &'static str> {
    let result = delivery["type"] == "result";
    let id = delivery["id"].as_u64();
    let online_result = result && id.is_some_and(|id| state.online.request(id));
    let (calls, reply, tick) = match delivery["type"].as_str() {
        Some("result") if id.is_some() && state.surface.opening == id => {
            state.open_canvas(host, &delivery["result"]);
            (Vec::new(), None, false)
        }
        Some("result") if id.is_some() && state.other_surface.surface.opening == id => {
            // The primary and settings views have distinct claims and cameras.
            std::mem::swap(&mut state.camera, &mut state.other_surface);
            state.primary_surface = !state.primary_surface;
            state.open_canvas(host, &delivery["result"]);
            (Vec::new(), None, false)
        }
        Some("result") => (state.told(delivery), None, false),
        Some("call") if delivery["method"] == "receive" => {
            (Vec::new(), state.receive(host, delivery), false)
        }
        Some("call") => state.answer(delivery),
        _ => (Vec::new(), None, false),
    };
    if let Some(handle) = state.retire_canvas.take() {
        host.close(handle);
    }
    for call in calls {
        host.send(call)?;
    }
    state.flush(host);
    if let Some(reply) = reply {
        host.send(reply)?;
    }
    let camera_input = matches!(
        delivery["args"]["event"]["t"].as_str(),
        Some("wheel" | "pinch" | "rotate" | "pointer")
    );
    // A compact high-DPI gesture should publish immediately. Its short page
    // timer and HTTP results keep decoding in separate fuel turns.
    let decode_turn = online_result
        || tick
        || camera_input
            && !state.sharp_gesture()
            && state.now.saturating_sub(state.decoded_at) >= 100;
    if state.online_enabled && decode_turn && state.online.decode_pending() {
        state.decoded_at = state.now;
        let (view, frame) = (state.view, state.frame());
        state.online.tick(state.now, view, frame);
    } else {
        state.pump_canvas(host, tick)?;
    }
    if online_result && state.online_enabled && !state.clearing_kept && state.focus.is_none() {
        let (view, frame) = (state.view, state.frame());
        if let Some(request) = state.online.advance_cached(state.now, view, frame) {
            host.send(state.online_call(request, true))?;
        }
    }
    Ok(())
}

/// The whole life of the guest: every delivery handled in the order it came (what was kept while a stream was waited for
/// comes first). Returns zero when the application goes away, non-zero when it cannot be written to.
fn run<H: CanvasIo>(host: &mut H) -> i32 {
    let mut state = State::default();
    // Its settings first: the page is placed among the application's pages when the person asked for it, whether or not it
    // is ever opened.
    let settings = state.ask_settings();
    if host.send(settings).is_err() {
        return 1;
    }
    if host.send(state.list()).is_err() {
        return 1;
    }
    loop {
        if state.waiting.is_empty() {
            match host.poll(&[], 30_000) {
                Ok(None) => {
                    state.now = state.now.saturating_add(30_000);
                    state.ui_time = None;
                    if state.asked_list == 0 && host.send(state.list()).is_err() {
                        return 1;
                    }
                    continue;
                }
                Ok(Some(_)) => {}
                Err(_) => return 1,
            }
        }
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

#[cfg(test)]
mod tests;
