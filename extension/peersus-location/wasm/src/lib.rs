//! A page that says where this device is and, on a drawn map with this device in the middle when it is known (otherwise
//! one of the devices that shared with it), where the paired devices that shared their location with it were: how far away
//! and in which direction, and of a position older than a turn allows ([`LAST_AFTER`]), that it is the last one known and
//! how long ago. The page lives in this extension's settings; its own option (`location-page`, read
//! from `peersus.config`) also places it among the application's pages (`peersus.pages`).
//!
//! This device's position is asked of the application (`peersus.location`, the person's `location.read`) only when the person
//! presses a button, or when a share is due: at most every five minutes while a paired device takes it. Sharing sends it once
//! to every paired device that takes it now (`peersus.peers`: online, allowed, and its Location there accepts
//! `location.position`); the application admits it there only from a device that device's person allowed Location for
//! (`location.sharing`). What arrives is kept in memory only, one position per device, and is forgotten when the extension
//! stops, the person presses Forget, or the device is no longer listed as allowed (unpaired, or Location no longer allowed
//! for it). A device of which nothing is known here, or only something old, is asked where it is (`location.wanted`, under
//! the same permission on both sides): it answers by sharing what it knows, so a map is not empty until every device's next
//! turn. A question never makes a device look for itself sooner than it would have.
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
use canvas_scene::{CanvasIo, Glide, Hit, Marker};
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
/// What is known of a device is old once two of its turns went by without a share arriving (page clock, milliseconds).
const STALE: u64 = 2 * SHARE_EVERY;
/// A device that was asked where it is and has not shared since is asked again after this long, then after twice as long
/// each time, and in the end as often as a share is due (page clock, milliseconds). One question may come to nothing: the
/// device did not take a position from this one yet, or did not know where it was.
const ASK_AGAIN: u64 = 15_000;
/// One device's question is answered no more often than this (page clock, milliseconds).
const ANSWER_EVERY: u64 = 10_000;
/// A question that cannot be answered yet is kept this long (page clock, milliseconds).
const WANTED_FOR: u64 = 60_000;
/// Longest single wait for anything at all while there is nothing to do, in milliseconds. A guest has no clock. While a
/// page is open its events say the time; otherwise time is counted from these waits: one that ran out surely took this
/// long, and one a delivery cut short took an unknown part of it, which is kept apart as time that may have passed
/// ([`State::unsure`]) and is never added to an age as if it were known. The shorter the wait, the less each delivery
/// leaves uncounted.
const IDLE_WAIT: u16 = 5_000;
/// The paired devices are listed again this often while no page is open (counted time, milliseconds).
const IDLE_LIST_EVERY: u64 = 30_000;
/// Two events of a page further apart than this are not one measured stretch of time (milliseconds).
const PAGE_GAP: u64 = 30_000;
/// A position this old, in seconds, is no longer said as where its device is: it is the last one known, with its age. A
/// device that keeps its turn ([`SHARE_EVERY`]) shares again before that, the minute over allowing for the time a turn
/// takes, and nothing is said of an age that changes nothing.
const LAST_AFTER: u64 = SHARE_EVERY / 1000 + 60;
/// Scale change of the dedicated zoom buttons.
const CLOSER: f64 = 1.6;
/// A burst of this many logical wheel pixels doubles the magnification.
const WHEEL_PIXELS_PER_DOUBLING: u16 = 160;
const STREET_FOCUS: f64 = 0.006;
const FOCUS_MS: u64 = 500;
const MARKER_GLIDE_MS: u64 = 1_000;
const DETAIL_AFTER_MOTION_MS: u64 = 150;
// Do not turn a render timeout into a raster too small to read the street network.
const MIN_PREVIEW_RASTER_SCALE: u32 = 240;
// A soft cap below the Canvas stream's rolling byte budget leaves room for
// headers, settled frames, and other work on the same worker.
const MOTION_BYTES_PER_SECOND: u64 = 224 * 1024 * 1024;
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
    Finding(Option<Told>),
    /// Found: the position, how old the system said it was, and when (page clock) it was told.
    Found(Told),
    /// Not found, in words to show.
    Not(String),
}

impl Own {
    /// Keep the last dot on the map while the system looks for a replacement.
    fn marker(&self) -> Option<&Told> {
        match self {
            Self::Found(told) | Self::Finding(Some(told)) => Some(told),
            _ => None,
        }
    }
}

/// A paired device as `peersus.peers` lists it.
#[derive(Clone, Debug, PartialEq)]
struct Device {
    alias: String,
    name: String,
    /// It takes a position now: online, allowed here, and its Location accepts it.
    ready: bool,
    /// It takes the question of where it is now: online, allowed here, and its Location accepts that.
    asks: bool,
}

/// A position and when (counted time) it was told: this device's by the application, a paired device's when it arrived.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Told {
    shared: wire::Shared,
    at: u64,
    /// The time that nothing measured ([`State::unsure`]) when it was told.
    slack: u64,
}

impl Told {
    /// What it is at `now`: the age it was told with, plus the time that surely passed since. Never older than it is.
    fn aged(&self, now: u64) -> wire::Shared {
        wire::Shared {
            fix: self.shared.fix,
            age: self
                .shared
                .age
                .saturating_add(now.saturating_sub(self.at) / 1000),
        }
    }

    /// How many seconds older than [`Told::aged`] says it may be, at most: the waits since it was told that nothing
    /// measured, given the time `unsure` now.
    fn loose(&self, unsure: u64) -> u64 {
        unsure.saturating_sub(self.slack).div_ceil(1000)
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
    /// Where the time these waits may have taken is kept ([`State::cut`]).
    cut: &'a mut u64,
}

impl<H: Wire + Data> Ready<'_, H> {
    fn wait(&mut self, handle: Handle) -> Result<(), &'static str> {
        for _ in 0..=MOST_KEPT {
            // However it ends, this wait takes `wait` at most, and nothing here says how long.
            *self.cut = self
                .cut
                .saturating_add(u64::try_from(self.wait).unwrap_or(0));
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
    /// Time that surely passed since the guest started, in milliseconds, whatever the window's own clock began at: what
    /// a page's events measured while one was open, and every idle wait that ran out. Never ahead of the time that
    /// really passed; behind it by `unsure` and `cut` at most.
    now: u64,
    ui_time: Option<u64>,
    /// Waits something cut short since time was last measured, in milliseconds at most. The events of an open page may
    /// still say how long they took; once nothing can, they are added to `unsure`.
    cut: u64,
    /// Time that may have passed beyond `now`, in milliseconds at most: waits cut short that nothing measured. It only
    /// grows.
    unsure: u64,
    /// What the map last said of the age of each position on it ([`State::statuses`]).
    said: Vec<Option<String>>,
    own: Own,
    own_glide: Option<Glide>,
    asked_own: u64,
    focus_fix: bool,
    /// Keep sharing asked: send as soon as this device is found, unless it is turned off before.
    share_kept: bool,
    devices: Vec<Device>,
    devices_denied: bool,
    asked_list: u64,
    listed_at: Option<u64>,
    seen: BTreeMap<String, Told>,
    peer_glides: BTreeMap<String, Glide>,
    /// Streams asked of the application for the last share, not open yet.
    sending: Vec<Sending>,
    /// Streams the application opened for it, to write that share's position on.
    outgoing: Vec<Sending>,
    /// Questions asked of the application, not open yet: the call and the device it is for.
    asking: Vec<(u64, String)>,
    /// Questions the application opened a stream for: the claim to write it under, and the device.
    asks: Vec<(u64, String)>,
    /// When (page clock) each device was last asked where it is, and how many times since it last shared.
    asked: BTreeMap<String, (u64, u32)>,
    /// The devices that asked where this one is and are not answered yet, and when (page clock) each last asked.
    wanted: BTreeMap<String, u64>,
    /// When (page clock) each device's question was last answered.
    answered: BTreeMap<String, u64>,
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
    event_at: Option<u64>,
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

impl Viewport {
    fn drawn_marker(&self, id: &str, now: u64) -> Option<&Marker> {
        // There is no page-close event. Do not use pictures left by a page that stopped ticking
        // while the other one is still mounted.
        if now.saturating_sub(self.event_at?) > 2 * u64::from(TICK) {
            return None;
        }
        self.surface.picture.markers.iter().find(|m| m.id == id)
    }
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
    start: Option<u64>,
    duration: u64,
}

fn eased(elapsed: u64, duration: u64) -> f64 {
    let t = elapsed.min(duration) as f64 / duration as f64;
    t * t * (3. - 2. * t)
}

fn different_place(a: geo::Fix, b: geo::Fix) -> bool {
    a.latitude != b.latitude
        || (a.longitude != b.longitude && (a.longitude - b.longitude).abs() != 360.)
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

/// The font of what a marker says of its position's age, under its name.
const STATUS_FONT: f64 = 11.;

/// Room beside a marker for its name and, under it, what is said of its position's age.
fn marker_badge_bounds(
    name: &str,
    status: Option<&str>,
    x: f64,
    y: f64,
    bounds: tile::layers::Rect,
    clear: impl Fn([f64; 4]) -> bool,
) -> Option<[f64; 4]> {
    let named = 8. + tile::text_width(name, 15.);
    let width = named
        .max(status.map_or(0., |status| 8. + tile::text_width(status, STATUS_FONT)))
        .max(40.);
    let height = if status.is_some() { 42. } else { 26. };
    let [left, top, right, bottom] = bounds.0.map(f64::from);
    if named > 148. || width > 190. || width > right - left || height > bottom - top {
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

/// `place`, then how sure the system was and, of a position older than a turn allows, what is said of its age.
fn described(shared: &wire::Shared, place: String, last: Option<String>) -> String {
    let mut line = place;
    if let Some(accuracy) = shared.fix.accuracy {
        line.push_str(&format!(" ± {}", geo::distance(accuracy)));
    }
    if let Some(last) = last {
        line.push_str(", ");
        line.push_str(&last);
    }
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
        if matches!(self.own, Own::Finding(_)) {
            return None;
        }
        self.own = Own::Finding(self.own.marker().copied());
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
        // A share to every device is also the answer to those that asked.
        for alias in &ready {
            self.wanted.remove(alias);
        }
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

    /// Answer the devices that asked where this one is and take a position now: one stream to each, for the position this
    /// device already knows, as old as it is. The system is not asked where this device is: that waits for its turn. A
    /// device that cannot be answered yet stays asked for a while ([`WANTED_FOR`]) and is answered by a later listing or
    /// by the next share.
    fn answer_wanted(&mut self) -> Vec<Value> {
        let now = self.now;
        self.wanted
            .retain(|_, at| now.saturating_sub(*at) < WANTED_FOR);
        let Own::Found(told) = &self.own else {
            return Vec::new();
        };
        let told = *told;
        let ready: Vec<String> = self
            .wanted
            .keys()
            .filter(|alias| self.devices.iter().any(|d| d.ready && d.alias == **alias))
            .cloned()
            .collect();
        ready
            .into_iter()
            .map(|alias| {
                let (number, call) = self.call(
                    PEERS,
                    "send",
                    json!({"device":alias,"service":wire::SERVICE,"version":wire::VERSION}),
                    CALL_MS,
                );
                self.wanted.remove(&alias);
                self.answered.insert(alias.clone(), now);
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
        self.own.marker().map(|told| told.aged(self.now))
    }

    /// A position told now. The waits cut short before it are not its doubt: with no page to measure them they are
    /// settled first, and those a page may still measure are left to it.
    fn tell(&mut self, shared: wire::Shared) -> Told {
        if self.ui_time.is_none() {
            self.uncounted();
        }
        Told {
            shared,
            at: self.now,
            slack: self.unsure,
        }
    }

    /// The waits cut short since time was last measured are time nothing will measure any more.
    fn uncounted(&mut self) {
        self.unsure = self.unsure.saturating_add(std::mem::take(&mut self.cut));
    }

    /// A whole idle wait went by with nothing delivered: that much time surely passed, and what was cut short before
    /// it stays unmeasured. With no page open this is also when the devices are listed again, every
    /// [`IDLE_LIST_EVERY`]: for a share that is due, a device that came back, a question still to answer.
    fn waited(&mut self) -> Option<Value> {
        self.now = self.now.saturating_add(u64::from(IDLE_WAIT));
        self.ui_time = None;
        self.own_glide = None;
        self.peer_glides.clear();
        self.camera.focus = None;
        self.other_surface.focus = None;
        self.uncounted();
        (self.asked_list == 0
            && self
                .listed_at
                .is_none_or(|at| self.now.saturating_sub(at) >= IDLE_LIST_EVERY))
        .then(|| self.list())
    }

    /// What is said of the age of a position: nothing while its device keeps its turn, and of one older than a turn
    /// allows ([`LAST_AFTER`]) that it is the last one known and how old. The age is the time that surely passed. Once
    /// the waits nothing measured since it was told may add up to a whole unit of what is said (a minute of an age in
    /// minutes), the age is the least it can be and says so with a plus; less than that is within what the words
    /// cannot tell apart anyway. A wait a page may still measure changes nothing of what is said.
    fn last(&self, told: &Told) -> Option<String> {
        let age = told.aged(self.now).age;
        (age >= LAST_AFTER).then(|| {
            if told.loose(self.unsure) < geo::age_unit(age) {
                format!("last location {}", geo::age(age))
            } else {
                format!("last location {}", geo::age_or_more(age))
            }
        })
    }

    /// What the map says of the age of each position on it, this device's first.
    fn statuses(&self) -> Vec<Option<String>> {
        self.own
            .marker()
            .into_iter()
            .chain(self.seen.values())
            .map(|told| self.last(told))
            .collect()
    }

    /// Start (or retarget) one glide from each page's last accepted drawing. A previously hidden
    /// marker has no origin on that page, so it will appear at its fix when it first fits.
    fn marker_glide(&self, id: &str, target: geo::Fix) -> Option<Glide> {
        let mut pages = [&self.camera, &self.other_surface];
        if self.primary_surface {
            pages.swap(0, 1);
        }
        let from = pages.map(|page| {
            let marker = page.drawn_marker(id, self.now)?;
            different_place(marker.fix, target).then(|| marker.origin())
        });
        from.iter()
            .any(Option::is_some)
            .then_some(Glide { from, start: None })
    }

    fn gliding(&self) -> bool {
        let slot = usize::from(self.primary_surface);
        self.own_glide
            .iter()
            .chain(self.peer_glides.values())
            .any(|g| g.from[slot].is_some())
    }

    fn advance_glides(&mut self) {
        let now = self.now;
        // A fix delivery has no timestamp. The first page sample starts the full glide,
        // even if it arrived just after the previous (normally one-second) tick.
        self.peer_glides.retain(|_, glide| glide.advance(now));
        if self
            .own_glide
            .as_mut()
            .is_some_and(|glide| !glide.advance(now))
        {
            self.own_glide = None;
        }
        for page in [&mut self.camera, &mut self.other_surface] {
            if let Some(focus) = &mut page.focus {
                focus.start.get_or_insert(now);
            }
        }
    }

    fn clear_page_glides(&mut self) {
        let slot = usize::from(self.primary_surface);
        self.peer_glides.retain(|_, glide| glide.remove_page(slot));
        if self
            .own_glide
            .as_mut()
            .is_some_and(|glide| !glide.remove_page(slot))
        {
            self.own_glide = None;
        }
    }

    fn locate_view(&mut self, fix: geo::Fix) {
        let slot = usize::from(self.focus_surface);
        let follows_glide = self
            .own_glide
            .as_ref()
            .is_some_and(|glide| glide.from[slot].is_some());
        let page = if self.focus_surface == self.primary_surface {
            &mut self.camera
        } else {
            &mut self.other_surface
        };
        let target = world::View {
            lon: fix.longitude,
            lat: fix.latitude,
            span: focus_span(fix.longitude, fix.latitude),
            roll: page.view.roll,
        }
        .kept();
        if let Some(painted) = page.surface.painted
            && follows_glide
            && painted.view != target
        {
            page.view = painted.view;
            page.focus = Some(Focus {
                from: painted.view,
                target,
                start: None,
                duration: MARKER_GLIDE_MS,
            });
        } else {
            page.view = target;
            page.focus = None;
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
                let mut calls = self.ask_devices();
                if self.devices.iter().any(|d| d.ready) {
                    if self
                        .kept_at
                        .is_none_or(|at| self.now.saturating_sub(at) >= SHARE_EVERY)
                    {
                        // Its turn: the share that follows goes to every device, those that asked among them.
                        calls.extend(self.find(Ask::Keep));
                    } else {
                        calls.extend(self.answer_wanted());
                    }
                }
                return calls;
            } else if denied {
                // No device may be seen any more: nothing of one is shown either. Only a refusal says that: a listing
                // the application could not answer just now (busy, late) takes nothing away, and is asked again.
                self.devices.clear();
                self.seen.clear();
                self.peer_glides.clear();
                self.sending.clear();
                self.outgoing.clear();
                self.asking.clear();
                self.asks.clear();
                self.asked.clear();
                self.wanted.clear();
                self.answered.clear();
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
                    if self
                        .own
                        .marker()
                        .is_some_and(|old| focus || different_place(old.shared.fix, shared.fix))
                    {
                        self.own_glide = self.marker_glide(canvas_scene::OWN, shared.fix);
                    }
                    let told = self.tell(shared);
                    self.own = Own::Found(told);
                    if focus {
                        self.locate_view(shared.fix);
                    }
                    if share {
                        return self.share(told);
                    }
                }
                _ if denied => {
                    self.own_glide = None;
                    self.own = Own::Not(words::NOT_ALLOWED.to_owned());
                }
                // The application's own words of why it could not, when it gave some.
                _ => {
                    self.own_glide = None;
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
        } else if let Some(index) = self.asking.iter().position(|(number, _)| *number == id) {
            let (_, alias) = self.asking.swap_remove(index);
            if let Some(claim) = result["value"]["claim"].as_u64().filter(|_| ok) {
                self.asks.push((claim, alias));
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
                let takes = |service: &str| {
                    peer["accepts"]
                        .as_array()
                        .is_some_and(|accepts| accepts.iter().any(|accepted| accepted == service))
                };
                let online = peer["presence"] != "offline" && peer["online"] == true;
                Some(Device {
                    alias: alias.to_owned(),
                    name: clipped(peer["name"].as_str().unwrap_or(words::SOMEONE), NAME_MAX),
                    ready: online && takes(wire::SERVICE),
                    asks: online && takes(wire::WANTED),
                })
            })
            .collect();
        let devices = &self.devices;
        self.seen
            .retain(|alias, _| devices.iter().any(|device| device.alias == *alias));
        self.peer_glides
            .retain(|alias, _| self.seen.contains_key(alias));
        let ready = |s: &Sending| devices.iter().any(|d| d.ready && d.alias == s.alias);
        self.sending.retain(ready);
        self.outgoing.retain(ready);
        let asks = |(_, alias): &(u64, String)| devices.iter().any(|d| d.asks && d.alias == *alias);
        self.asking.retain(asks);
        self.asks.retain(asks);
        self.asked
            .retain(|alias, _| devices.iter().any(|device| device.alias == *alias));
        self.wanted
            .retain(|alias, _| devices.iter().any(|device| device.alias == *alias));
        self.answered
            .retain(|alias, _| devices.iter().any(|device| device.alias == *alias));
        if !devices.iter().any(|d| d.ready) {
            self.share_kept = false;
        }
    }

    /// Ask every device that takes the question, and of which nothing is known here or only something old, where it is:
    /// at once the first time, then after [`ASK_AGAIN`] and twice as long each time while it has not shared, in the end as
    /// often as a share is due. Nothing answers on that stream: a device that agrees shares what it knows with this one.
    fn ask_devices(&mut self) -> Vec<Value> {
        let now = self.now;
        let unknown: Vec<String> = self
            .devices
            .iter()
            .filter(|device| device.asks)
            .filter(|device| {
                self.seen
                    .get(&device.alias)
                    .is_none_or(|told| now.saturating_sub(told.at) >= STALE)
            })
            .filter(|device| {
                self.asked.get(&device.alias).is_none_or(|&(at, times)| {
                    let wait = ASK_AGAIN
                        .saturating_mul(1 << times.saturating_sub(1).min(8))
                        .min(SHARE_EVERY);
                    now.saturating_sub(at) >= wait
                })
            })
            .map(|device| device.alias.clone())
            .collect();
        unknown
            .into_iter()
            .map(|alias| {
                let (number, call) = self.call(
                    PEERS,
                    "send",
                    json!({"device":alias,"service":wire::WANTED,"version":wire::WANTED_VERSION}),
                    CALL_MS,
                );
                let times = self.asked.get(&alias).map_or(0, |&(_, times)| times);
                self.asked
                    .insert(alias.clone(), (now, times.saturating_add(1)));
                self.asking.push((number, alias));
                call
            })
            .collect()
    }

    /// What a person did on the page, and the calls it leads to.
    fn act(&mut self, event: &Value) -> Vec<Value> {
        if let Some(at) = event["at"].as_u64() {
            match self
                .ui_time
                .replace(at)
                .and_then(|last| at.checked_sub(last))
            {
                // The page's own time says how long it was, whatever was waited for meanwhile. Only elapsed time
                // advances retries and sharing.
                Some(elapsed) if elapsed <= PAGE_GAP => {
                    self.now = self.now.saturating_add(elapsed);
                    self.cut = 0;
                }
                // The first event of a page, a reopened window (a fresh clock) or a long silence: nothing measured
                // what went before, and a new epoch never holds anything back.
                _ => self.uncounted(),
            }
            self.advance_glides();
        }
        self.camera.event_at = Some(self.now);
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
                self.clear_page_glides();
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
                self.peer_glides.clear();
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
                // What the map says of a position's age changes with time alone.
                let said = self.statuses();
                if said != self.said {
                    self.said = said;
                    self.marker_stamp = self.marker_stamp.wrapping_add(1);
                }
                if let Some(focus) = self.camera.focus.as_ref() {
                    let elapsed = self.now.saturating_sub(focus.start.unwrap_or(self.now));
                    self.camera.view = focus
                        .from
                        .towards(focus.target, eased(elapsed, focus.duration));
                    if elapsed >= focus.duration {
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
        if let Own::Found(told) = &self.own {
            let shared = told.aged(self.now);
            return format!(
                "This device: {}",
                described(&shared, geo::coordinates(&shared.fix), self.last(told))
            );
        }
        match &self.own {
            Own::Finding(_) => words::FINDING.to_owned(),
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
                        described(&shared, geo::coordinates(&shared.fix), self.last(told))
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
                start: Some(self.now),
                duration: FOCUS_MS,
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
            || self.gliding()
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
        // A question is the same few bytes for every device, and what came of it is not said: its answer is a share.
        for (claim, _) in std::mem::take(&mut self.asks) {
            if let Ok(handle) = host.open(true, claim) {
                self.deliver(host, handle, wire::WANTED_MESSAGE);
                host.close(handle);
            }
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
                    cut: &mut self.cut,
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
                cut: &mut self.cut,
            },
            handle,
            wire::MAX,
        );
        host.close(handle);
        let shared = wire::parse(&read?).map_err(|_| "What was sent was not understood")?;
        self.marker_stamp = self.marker_stamp.wrapping_add(1);
        // It shared: what was asked of it is answered, and the next question, if one is ever due, starts over.
        self.asked.remove(&alias);
        if self
            .seen
            .get(&alias)
            .is_some_and(|old| different_place(old.shared.fix, shared.fix))
        {
            if let Some(glide) = self.marker_glide(&alias, shared.fix) {
                self.peer_glides.insert(alias.clone(), glide);
            } else {
                self.peer_glides.remove(&alias);
            }
        }
        let told = self.tell(shared);
        self.seen.insert(alias, told);
        Ok(())
    }

    /// A paired device asks where this one is. What it wrote is read and judged like anything a device sends, and nothing
    /// answers on its stream. A question understood leads, no more often than [`ANSWER_EVERY`] for one device, to the
    /// devices being listed and the position this device already knows being shared with the device that asked, if it
    /// takes one now: what the person's permission lets this device send it at its own turn.
    fn asked_for<H: Wire + Data>(
        &mut self,
        host: &mut H,
        delivery: &Value,
    ) -> (Vec<Value>, Option<Value>) {
        let Some(ticket) = delivery["ticket"].as_u64() else {
            return (Vec::new(), None);
        };
        let understood = match host.open(false, ticket) {
            Ok(handle) => {
                let read = read_limited(
                    &mut Ready {
                        host,
                        kept: &mut self.waiting,
                        wait: WAIT_MS,
                        cut: &mut self.cut,
                    },
                    handle,
                    wire::MAX,
                );
                host.close(handle);
                read.is_ok_and(|said| said == wire::WANTED_MESSAGE)
            }
            Err(_) => false,
        };
        let asker = delivery["args"]["peer"]
            .as_str()
            .filter(|alias| !alias.is_empty() && alias.len() <= ALIAS_MAX);
        let mut calls = Vec::new();
        if understood
            && let Some(asker) = asker
            && self
                .answered
                .get(asker)
                .is_none_or(|at| self.now.saturating_sub(*at) >= ANSWER_EVERY)
            && (self.wanted.contains_key(asker) || self.wanted.len() < MOST_SEEN)
        {
            self.wanted.insert(asker.to_owned(), self.now);
            if self.asked_list == 0 {
                calls.push(self.list());
            }
        }
        let result = if understood {
            json!({"status":"ok","value":true})
        } else {
            json!({"status":"error","code":"failed","message":"What was sent was not understood"})
        };
        (
            calls,
            Some(json!({"op":"reply","ticket":ticket,"result":result})),
        )
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
            markers: (self.marker_stamp, self.gliding().then_some(self.now)),
            online: self.online_enabled.then(|| self.online.revision()),
            detail: tile::Detail::Full,
        };
        if self.sharp_gesture() {
            stamp.detail = tile::Detail::Gesture;
        } else if let Some(pressure) = &self.surface.raster_pressure
            && (self.map_moving()
                || stamp.markers.1.is_some()
                || pressure.full_refused == Some(stamp))
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
        if self.surface.painted.is_some_and(|previous| {
            stamp.markers.1.is_some()
                || previous.markers.1.is_some()
                || (stamp.detail == tile::Detail::Gesture
                    && previous.detail == tile::Detail::Gesture
                    && previous.metrics == stamp.metrics
                    && previous.markers == stamp.markers)
        }) {
            let [width, height] = stamp.metrics.raster.pixels.map(u64::from);
            let spacing = (width * height * 4 * 1000)
                .div_ceil(MOTION_BYTES_PER_SECOND)
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
        Some("call") if delivery["method"] == "receive" && delivery["service"] == wire::WANTED => {
            let (calls, reply) = state.asked_for(host, delivery);
            (calls, reply, false)
        }
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

/// Handle every delivery in the order it came (what was kept while a stream was waited for comes first), and count the
/// time between them. Returns zero when the application goes away, non-zero when it cannot be written to.
fn serve<H: CanvasIo>(host: &mut H, state: &mut State) -> i32 {
    loop {
        if state.waiting.is_empty() {
            // Poll timeouts and Canvas presents do not renew WASM fuel. Let each page
            // delivery pay for its glide frame rather than drawing on idle wake-ups.
            match host.poll(&[], i32::from(IDLE_WAIT)) {
                Ok(None) => {
                    if let Some(list) = state.waited()
                        && host.send(list).is_err()
                    {
                        return 1;
                    }
                    continue;
                }
                // Cut short by a delivery: it took an unknown part of a whole wait.
                Ok(Some(_)) => state.cut = state.cut.saturating_add(u64::from(IDLE_WAIT)),
                Err(_) => return 1,
            }
        }
        let Ok(delivery) = state.next(host) else {
            return 0;
        };
        if step(host, state, &delivery).is_err() {
            return 1;
        }
        if state.ui_time.is_none() {
            // No page says the time: what was waited for stays unmeasured.
            state.uncounted();
        }
    }
}

/// The whole life of the guest. Returns zero when the application goes away, non-zero when it cannot be written to.
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
    serve(host, &mut state)
}

/// The entry the interpreter calls. Returns zero when the application goes away, non-zero on a broken protocol.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    run(&mut host::Host)
}

#[cfg(test)]
mod tests;
