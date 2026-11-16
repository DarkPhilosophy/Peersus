//! A page that says how a device is doing: processor, graphics, memory, storage, network and battery, each in a card
//! of its own, under a bar of the devices it can be about.
//!
//! The page lives in this extension's settings; a setting of its own also places it among the application's pages
//! (`peersus.pages`), read from `peersus.config`. Each part of this device is asked of the application separately
//! (`peersus.system`), because the person allows each part separately: a part not allowed says so in its card, and
//! the others still show. A number the system does not publish is drawn as not known, never invented. The
//! application answers counters, so the share of the processor that was busy and the network's speed are made here
//! from two answers in a row. The bar shows this device and the paired devices the person let this extension see
//! (`peersus.peers`), online or asleep. Choosing a paired device shows its numbers instead: this extension asks that
//! device's own Monitor for them (`remote`, `wire`), which answers only with the parts its person allowed Monitor to
//! read there, and only when the people of both devices allowed Monitor for each other. A part, or a device, that is
//! not allowed says so in the page, and a number is never shown that was not told.
#![cfg(any(target_family = "wasm", feature = "native", test))]

mod gauges;
mod host;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
mod native;
mod remote;
mod wire;

use gauges::{Gauges, Reading};
use host::{Data, Wire};
use remote::Link;
use serde_json::{Value, json};
use std::collections::VecDeque;

/// From this width on (logical pixels) the battery stands tall beside the other cards, as on a desktop; under it
/// every card goes in a grid of as many columns as the width holds.
const WIDE: u32 = 720;
/// The narrowest a card is made in the grid: two columns on a phone held upright, as in the concept, the narrowest
/// phones included (about 335 logical pixels wide, less the application's margins).
const CARD: u32 = 130;
/// Under this width a card is drawn compact: a smaller picture and number tile, a smaller storage ring.
const COMPACT: u32 = 260;
/// Under this width a compact card is also tight: no picture beside the title, its two figures one under the other, the
/// network's speeds by their arrows.
const TIGHT: u32 = 170;
/// The layout, in pixels: the space between cards, a card's padding, the battery card's width on a wide page.
const GAP: u32 = 12;
const PADDING: u32 = 16;
const BATTERY: u32 = 180;
/// The id of the page's device chooser.
const CHOOSER: &str = "device";
/// Most devices kept from one answer of the application.
const DEVICES: usize = 64;
/// What stands where a number is not known.
const UNKNOWN: &str = "—";
/// How often the numbers are read once the page is whole, in milliseconds.
const EVERY: u16 = 1000;
/// The paired devices are asked again every this many ticks, and the settings every this many.
const DEVICES_EVERY: u64 = 5;
const CONFIG_EVERY: u64 = 3;
/// Height of a graph, in pixels.
const GRAPH: u16 = 64;

/// The setting that also places the page among the application's pages, the page it places, and where.
const PAGE_SETTING: &str = "monitor-page";
const PAGE: &str = "mon";

/// The parts of a device, each a method of `peersus.system` behind its own permission, in the order the cards are
/// asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Processor,
    Graphics,
    Memory,
    Storage,
    Network,
    Battery,
}
const PARTS: [Part; 6] = [
    Part::Processor,
    Part::Graphics,
    Part::Memory,
    Part::Storage,
    Part::Network,
    Part::Battery,
];
impl Part {
    fn method(self) -> &'static str {
        match self {
            Part::Processor => "processor",
            Part::Graphics => "graphics",
            Part::Memory => "memory",
            Part::Storage => "storage",
            Part::Network => "network",
            Part::Battery => "battery",
        }
    }
    /// The part whose method of `peersus.system` is `method`: the names a question and an answer use.
    pub fn of(method: &str) -> Option<Part> {
        PARTS.into_iter().find(|part| part.method() == method)
    }
    fn title(self) -> &'static str {
        match self {
            Part::Processor => "CPU",
            Part::Graphics => "GPU",
            Part::Memory => "Memory",
            Part::Storage => "Storage",
            Part::Network => "Network",
            Part::Battery => "Battery",
        }
    }
    /// The part's own picture, on a 24-pixel square, as outlines.
    fn icon(self) -> &'static str {
        match self {
            Part::Processor => {
                "M 7 7 L 17 7 L 17 17 L 7 17 Z M 10 10 L 14 10 L 14 14 L 10 14 Z M 9 3 L 9 7 M 12 3 L 12 7 M 15 3 L 15 7 \
                 M 9 17 L 9 21 M 12 17 L 12 21 M 15 17 L 15 21 M 3 9 L 7 9 M 3 12 L 7 12 M 3 15 L 7 15 M 17 9 L 21 9 \
                 M 17 12 L 21 12 M 17 15 L 21 15"
            }
            Part::Graphics => {
                "M 2 7 L 22 7 L 22 17 L 2 17 Z M 9 9 A 3 3 0 0 1 9 15 A 3 3 0 0 1 9 9 M 15 10 L 19 10 M 15 14 L 19 14 \
                 M 5 17 L 5 20"
            }
            Part::Memory => {
                "M 3 7 L 21 7 L 21 15 L 3 15 Z M 7 10 L 7 12 M 11 10 L 11 12 M 15 10 L 15 12 M 6 15 L 6 18 \
                 M 10 15 L 10 18 M 14 15 L 14 18 M 18 15 L 18 18"
            }
            Part::Storage => {
                "M 4 6 A 8 3 0 0 1 20 6 L 20 18 A 8 3 0 0 1 4 18 Z M 4 6 A 8 3 0 0 0 20 6 M 4 12 A 8 3 0 0 0 20 12"
            }
            Part::Network => {
                "M 8 4 L 8 20 M 4 8 L 8 4 L 12 8 M 16 20 L 16 4 M 12 16 L 16 20 L 20 16"
            }
            Part::Battery => "M 3 8 L 19 8 L 19 16 L 3 16 Z M 21 11 L 21 13",
        }
    }
}

/// How long the application may take to answer a call of this extension (`timeout_ms`), by the page's clock.
const CALL_MS: u64 = 5_000;

/// One request out: its call's id (0: none) and when it was asked, by the page's clock. Its answer is taken until it
/// came or the call's time ran out, so a slow answer is not dropped by asking again on every tick.
#[derive(Clone, Copy, Default)]
struct Pending {
    id: u64,
    at: u64,
}

impl Pending {
    /// Whether a new request may be made: none is out, the one out had all its time, or the page's clock started over.
    fn free(self, now: u64) -> bool {
        self.id == 0 || now < self.at || now - self.at >= CALL_MS
    }
}

#[derive(Default)]
pub struct State {
    /// The paired devices as the application last listed them: only those the person let this extension see.
    devices: Vec<Value>,
    /// Whether the application has answered the list at least once.
    listed: bool,
    /// Whether the person let this extension see no paired device at all.
    devices_denied: bool,
    /// The paired device whose numbers are shown, by the name the application gave this extension for it.
    /// `None`: this device.
    chosen: Option<String>,
    /// What the page says of a chosen device that stopped being online.
    dropped: Option<String>,
    /// The page's width in pixels, as the application measured it.
    width: Option<u32>,
    /// The id of the last call of each kind; only its answer is taken.
    asked_devices: Pending,
    asked_parts: [Pending; 6],
    asked_config: u64,
    asked_pages: u64,
    /// The call that asked for a stream to the chosen device to write a question on.
    asked_send: u64,
    /// The call that showed the last problem as a notification.
    asked_notify: u64,
    /// The problem last told (see `State::problem`), so that each is told once.
    told_problem: Option<String>,
    /// Whether the person did not let Monitor show notifications: problems are written on the page instead.
    notify_denied: bool,
    /// Whether the page is placed among the application's pages, as last set; `None` before it was.
    placed: Option<bool>,
    /// Every call's id, counted up.
    calls: u64,
    /// The number of the last question asked of a paired device, counted up.
    seq: u64,
    /// The ticks of the page's clock so far.
    ticks: u64,
    /// This device's numbers.
    own: Gauges,
    /// What is said with the chosen paired device, and its numbers.
    talk: remote::Conversation,
    /// A question on its way: the stream the application issued for it, and what is written on it.
    outgoing: Vec<(u64, Vec<u8>)>,
    /// Deliveries that arrived while something was waited for, to be handled afterwards, oldest first.
    waiting: VecDeque<Value>,
}

fn px(n: u32) -> Value {
    json!({"unit":"px","value":n.min(4096)})
}
fn fill() -> Value {
    json!({"unit":"percent","value":10000})
}
fn flex() -> Value {
    json!({"unit":"flex","value":1})
}
fn bx(id: Option<&str>, style: Value, children: Vec<Value>) -> Value {
    json!({"k":"box","id":id,"style":style,"children":children})
}
fn text(text: &str, tone: &str) -> Value {
    json!({"k":"text","text":text,"tone":tone})
}
fn across(gap: u16, children: Vec<Value>) -> Value {
    bx(
        None,
        json!({"horizontal":true,"gap":gap,"center":true}),
        children,
    )
}
/// A row as wide as the room it has, its things side by side from the left and centred across. A row with no width is
/// only as wide as what it holds, so a flexible thing in it (`column`) is sized to the room the row was offered and not
/// to the row, and the toolkit's centring then moves the whole row out of its card, to the left by half of what the
/// flexible thing took: a row that holds a flexible thing is made with this.
fn beside(gap: u16, children: Vec<Value>) -> Value {
    bx(
        None,
        json!({"width":fill(),"horizontal":true,"gap":gap,"center":true}),
        children,
    )
}
fn column(gap: u16, children: Vec<Value>) -> Value {
    bx(None, json!({"width":flex(),"gap":gap}), children)
}

/// Whether a paired device is connected, awake or asleep: a sleeping device still answers. An application that says no
/// `presence` is an older one: it says only who is online.
fn connected(peer: &Value) -> bool {
    match peer["presence"].as_str() {
        Some(presence) => presence == "online" || presence == "sleeping",
        None => peer["online"] == true,
    }
}
/// Whether a paired device can be asked now: connected, and Monitor is there to answer. One that does not say `accepts` has
/// not been told by the device (an older one), which then answers or refuses for itself.
fn askable(peer: &Value) -> bool {
    connected(peer)
        && peer["accepts"]
            .as_array()
            .is_none_or(|accepts| accepts.iter().any(|service| service == wire::SERVICE))
}
fn name(peer: &Value) -> &str {
    peer["name"].as_str().unwrap_or("Device")
}

/// A number of bytes as a person reads it: `512 B`, `3.4 GB`.
fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000. && unit + 1 < UNITS.len() {
        value /= 1000.;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else if value < 10. {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}
/// A share in hundredths of a percent as a whole percent.
fn percent(share: u16) -> String {
    format!("{}%", (share + 50) / 100)
}
/// The share `part` is of `whole`, in hundredths of a percent.
fn share(part: u64, whole: u64) -> Option<u16> {
    (whole > 0).then(|| (u128::from(part.min(whole)) * 10000 / u128::from(whole)) as u16)
}
/// A time in minutes as a person reads it: `45 min`, `2 h 5 min`.
fn duration(minutes: u64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// A share of a whole as a bar; empty when the share is not known. Hundredths of a percent.
fn bar(share: Option<u16>, colour: &str) -> Value {
    let filled = share
        .map(|share| {
            bx(
                None,
                json!({"width":{"unit":"percent","value":share.min(10000)},"height":px(6),
                    "radius":[3,3,3,3],"background":colour}),
                vec![],
            )
        })
        .into_iter()
        .collect();
    bx(
        None,
        json!({"width":fill(),"height":px(6),"radius":[3,3,3,3],"background":"border"}),
        filled,
    )
}
/// A temperature as a thermometer and its number, over a bar from 0 to 100 °C coloured by how warm it is.
fn warmth(celsius: &Value) -> Vec<Value> {
    let degrees = celsius.as_f64().filter(|c| c.is_finite());
    let colour = if degrees.is_some_and(|c| c >= 85.) {
        "danger"
    } else {
        "accent"
    };
    let said = degrees.map_or(format!("{UNKNOWN} °C"), |c| format!("{c:.0}°C"));
    vec![
        across(8, vec![picture(THERMOMETER, "muted"), text(&said, "body")]),
        bar(degrees.map(|c| (c.clamp(0., 100.) * 100.) as u16), colour),
    ]
}
/// The outline of a thermometer, on a 24-pixel square.
const THERMOMETER: &str = "M 10 4 A 2 2 0 0 1 14 4 L 14 14 A 4 4 0 1 1 10 14 Z";
/// The outline of a disk, on a 24-pixel square.
const DISK: &str = "M 4 6 L 20 6 L 20 11 L 4 11 Z M 4 13 L 20 13 L 20 18 L 4 18 Z M 7 8.5 L 8 8.5 M 7 15.5 L 8 15.5";
/// An outline on a 24-pixel square, in `colour`.
fn picture(path: &str, colour: &str) -> Value {
    json!({"k":"drawing","width":24,"height":24,"shapes":[
        {"path":path,"stroke":colour,"line":18,"round":true}
    ]})
}
/// The number of a card in a rounded tile at its right: how busy the part is.
fn pill(said: &str, compact: bool) -> Value {
    bx(
        None,
        json!({"padding":if compact { 8 } else { 14 },"radius":[22,22,22,22],"background":"border","center":true}),
        vec![text(said, if compact { "body" } else { "title" })],
    )
}
/// A text cut to `lines` lines, ending in an ellipsis: a device's long name of its processor.
fn cut(said: &str, tone: &str, lines: u8) -> Value {
    json!({"k":"text","text":said,"tone":tone,"lines":lines})
}
/// A small framed figure with its name over it: `Clock / 3.33 GHz`.
fn tile(label: &str, value: &str, compact: bool) -> Value {
    bx(
        None,
        json!({"width":flex(),"padding":if compact { 6 } else { 10 },"radius":[12,12,12,12],"border":true,"gap":4}),
        vec![text(label, "muted"), cut(value, "body", 1)],
    )
}
/// A share of a whole as an open ring, three quarters of a circle with its gap at the bottom, filled clockwise from
/// its lower left end, with the figure and its caption in the middle. Hundredths of a percent; the track alone when the
/// share is not known.
fn arc(share: Option<u16>, size: u16, tone: &str, caption: &str) -> Value {
    const SWEEP: f64 = 270.;
    let line = (size / 11).max(6);
    let centre = f64::from(size) / 2.;
    let radius = centre - f64::from(line) / 2. - 1.;
    // Degrees from the top, clockwise.
    let at = |degrees: f64| {
        let angle = degrees.to_radians();
        (centre + radius * angle.sin(), centre - radius * angle.cos())
    };
    let start = 225.;
    let path_to = |sweep: f64| {
        let (x0, y0) = at(start);
        let (x1, y1) = at(start + sweep);
        let large = u8::from(sweep > 180.);
        format!("M {x0:.1} {y0:.1} A {radius:.1} {radius:.1} 0 {large} 1 {x1:.1} {y1:.1}")
    };
    let mut shapes =
        vec![json!({"path":path_to(SWEEP),"stroke":"border","line":line * 10,"round":true})];
    if let Some(share) = share.filter(|share| *share >= 10) {
        let sweep = SWEEP * f64::from(share.min(10000)) / 10000.;
        shapes.push(json!({"path":path_to(sweep),"stroke":"accent","line":line * 10,"round":true}));
    }
    bx(
        None,
        json!({"width":px(size.into()),"height":px(size.into())}),
        vec![
            json!({"k":"drawing","width":size,"height":size,"shapes":shapes}),
            bx(
                None,
                json!({"overlay":true,"width":fill(),"height":fill(),"center":true}),
                vec![
                    text(&share.map_or(UNKNOWN.to_owned(), percent), tone),
                    text(caption, "muted"),
                ],
            ),
        ],
    )
}
/// A clock in megahertz as a person reads it: `3.33 GHz`, `800 MHz`.
fn clock(mhz: Option<u64>) -> String {
    match mhz {
        Some(mhz) if mhz >= 1000 => format!("{:.2} GHz", mhz as f64 / 1000.),
        Some(mhz) => format!("{mhz} MHz"),
        None => UNKNOWN.to_owned(),
    }
}
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
/// A size in binary units, as memory and disks are counted: `13.5 GiB`.
fn gib(n: u64) -> String {
    match n {
        n if n >= GIB => format!("{:.1} GiB", n as f64 / GIB as f64),
        n if n >= MIB => format!("{:.0} MiB", n as f64 / MIB as f64),
        n => format!("{n} B"),
    }
}
/// How much of a whole is used, in the whole's unit, written once as in the concept: `1.9 / 13.5 GiB`.
fn of_whole(part: u64, whole: u64) -> String {
    let part = part.min(whole);
    match whole {
        w if w >= GIB => format!(
            "{:.1} / {:.1} GiB",
            part as f64 / GIB as f64,
            w as f64 / GIB as f64
        ),
        w if w >= MIB => format!(
            "{:.0} / {:.0} MiB",
            part as f64 / MIB as f64,
            w as f64 / MIB as f64
        ),
        w => format!("{part} / {w} B"),
    }
}
/// A graph of `history` (oldest first), scaled to its own largest point so that a quiet minute still shows its shape,
/// drawn by this extension `width` pixels wide: the width its card leaves it, worked out from the page's measured width
/// and this extension's own layout (a measured box of its own would take the touches that scroll the page).
fn graph(width: Option<u32>, history: &[u64]) -> Value {
    let top = history.iter().copied().max().unwrap_or(0).max(1);
    let mut children = Vec::new();
    if let Some(width) = width
        .and_then(|width| u16::try_from(width.min(4096)).ok())
        .filter(|width| *width >= 8)
        && history.len() >= 2
    {
        let step = f64::from(width - 2) / (history.len() - 1) as f64;
        let height = f64::from(GRAPH);
        let points: Vec<String> = history
            .iter()
            .enumerate()
            .map(|(index, n)| {
                let x = 1. + step * index as f64;
                let y = 1. + (height - 2.) * (1. - *n as f64 / top as f64);
                format!("{x:.1} {y:.1}")
            })
            .collect();
        let line = format!("M {}", points.join(" L "));
        let area = format!(
            "{line} L {:.1} {height:.1} L 1 {height:.1} Z",
            f64::from(width) - 1.
        );
        children.push(json!({"k":"drawing","width":width,"height":GRAPH,"shapes":[
            {"path":area,"fill":"accent","opacity":1800},
            {"path":line,"stroke":"accent","line":20,"round":true}
        ]}));
    }
    // Clipped: drawn from the width worked out for its card, it never spills out of a card that came out narrower.
    json!({"k":"box","style":{"width":fill(),"height":px(GRAPH.into()),"clip":true},"children":children})
}

fn card(width: Value, children: Vec<Value>) -> Value {
    bx(
        None,
        json!({"width":width,"background":"raised","radius":[20,20,20,20],"padding":PADDING,"gap":12}),
        children,
    )
}
/// The part's picture in a small ring, `size` across.
fn badge(part: Part, size: u32) -> Value {
    bx(
        None,
        json!({"width":px(size),"height":px(size),"radius":[20,20,20,20],"border":true,"center":true}),
        vec![picture(part.icon(), "accent")],
    )
}
/// The part's picture in a small ring, its title, and what it is.
fn heading(part: Part, about: &str) -> Value {
    let mut words = vec![text(part.title(), "title")];
    if !about.is_empty() {
        words.push(text(about, "muted"));
    }
    beside(10, vec![badge(part, 40), column(2, words)])
}
/// The part's picture and its title, on one line; on a tight card the title alone, which would not fit beside it.
fn title_row(part: Part, tight: bool) -> Value {
    if tight {
        return text(part.title(), "title");
    }
    across(
        8,
        vec![picture(part.icon(), "accent"), text(part.title(), "title")],
    )
}
fn line(label: &str, value: &str) -> Value {
    bx(
        None,
        json!({"width":fill(),"horizontal":true,"gap":8}),
        vec![
            bx(None, json!({"width":flex()}), vec![text(label, "body")]),
            text(value, "body"),
        ],
    )
}
/// What a card says of a part that may not be seen: that it is not allowed, and `hint`, where to allow it.
fn not_allowed(part: Part, width: Value, hint: &str) -> Value {
    card(
        width,
        vec![
            heading(part, ""),
            text("Not allowed", "title"),
            text(hint, "muted"),
        ],
    )
}

/// A space and how much of it is used, as the application answered it.
struct Space {
    used: Option<u16>,
    said: String,
}
fn space(value: &Value) -> Space {
    match (value["total"].as_u64(), value["available"].as_u64()) {
        (Some(total), Some(available)) if total > 0 => Space {
            used: share(total.saturating_sub(available), total),
            said: of_whole(total.saturating_sub(available), total),
        },
        _ => Space {
            used: None,
            said: "Not available".to_owned(),
        },
    }
}

/// The widths the page's cards get, worked out from the page's measured width, so that each graph is drawn as wide
/// as its card leaves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    /// Not measured yet: every card as wide as the page, graphs not drawn.
    Unmeasured,
    /// A wide page: the battery tall at the right, when the device has one; at the left (`left` wide, the whole room
    /// when there is no battery) the processor and graphics side by side, under them storage, network and memory.
    /// Widths, as measured, of a processor card, of storage, of memory and of the network card, and of the room in all
    /// (`inner`).
    Wide {
        inner: u32,
        left: u32,
        pair: u32,
        storage: u32,
        memory: u32,
        network: u32,
    },
    /// A grid of `columns` cards each about `card` wide, in `inner`.
    Grid { inner: u32, columns: u32, card: u32 },
}
/// `part` of `whole` as a share of its parent: a card is never a fixed width, it is its share of the row it is in, so a
/// row always fills the page whatever the page's width really is (a measure of another place the page is shown in, a
/// window being resized), and only the space between cards is in pixels.
fn portion(part: u32, whole: u32) -> Value {
    let hundredths = u64::from(part) * 10_000 / u64::from(whole.max(1));
    json!({"unit":"percent","value":hundredths.min(10_000)})
}
impl Layout {
    /// The layout of a page `width` wide (not measured yet: `None`), with a battery card or without one.
    fn of(width: Option<u32>, battery: bool) -> Self {
        // The measure is a line inside the page's padding: what it reports is already the room the cards have.
        let Some(inner) = width else {
            return Layout::Unmeasured;
        };
        if inner + 2 * GAP >= WIDE {
            let left = if battery {
                inner.saturating_sub(BATTERY + GAP)
            } else {
                inner
            };
            let pair = left.saturating_sub(GAP) / 2;
            // As in the concept: storage, with its ring and sizes side by side, takes the most of the second row after
            // the network's graph; memory, a ring over its sizes, the least.
            let row = left.saturating_sub(2 * GAP);
            let storage = row * 38 / 100;
            let memory = row * 22 / 100;
            let network = row.saturating_sub(storage + memory);
            Layout::Wide {
                inner,
                left,
                pair,
                storage,
                memory,
                network,
            }
        } else {
            let columns = ((inner + GAP) / (CARD + GAP)).max(1);
            let card = inner.saturating_sub((columns - 1) * GAP) / columns;
            Layout::Grid {
                inner,
                columns,
                card,
            }
        }
    }
    /// The width of the card of `part`, as its share of the row it is in.
    fn width(self, part: Part) -> Value {
        match (self, part) {
            (Layout::Unmeasured, _) => fill(),
            (Layout::Wide { inner, .. }, Part::Battery) => portion(BATTERY, inner),
            (Layout::Wide { left, pair, .. }, Part::Processor | Part::Graphics) => {
                portion(pair, left)
            }
            (Layout::Wide { left, storage, .. }, Part::Storage) => portion(storage, left),
            (Layout::Wide { left, memory, .. }, Part::Memory) => portion(memory, left),
            (Layout::Wide { left, network, .. }, Part::Network) => portion(network, left),
            (Layout::Grid { inner, card, .. }, _) => portion(card, inner),
        }
    }
    /// The width a graph in the card of `part` is drawn: the card's, less its padding.
    fn graph(self, part: Part) -> Option<u32> {
        let card = match (self, part) {
            (Layout::Unmeasured, _) => return None,
            (Layout::Wide { pair, .. }, Part::Processor | Part::Graphics) => pair,
            (Layout::Wide { network, .. }, Part::Network) => network,
            (Layout::Wide { storage, .. }, Part::Storage) => storage,
            (Layout::Wide { memory, .. }, _) => memory,
            (Layout::Grid { card, .. }, _) => card,
        };
        Some(card.saturating_sub(2 * PADDING))
    }
    /// The height of the battery's card: as tall as the cards beside it in its row; before the page is measured, when
    /// the cards are not in rows, that of a card.
    fn battery_height(self) -> Value {
        match self {
            Layout::Unmeasured => px(300),
            _ => json!({"unit":"fill_minimum"}),
        }
    }
    /// Whether the cards are narrow enough to be drawn compact.
    fn compact(self) -> bool {
        matches!(self, Layout::Grid { card, .. } if card < COMPACT)
    }
    /// Whether the cards are narrower still, as on a narrow phone in two columns.
    fn tight(self) -> bool {
        matches!(self, Layout::Grid { card, .. } if card < TIGHT)
    }
}

impl State {
    fn call(&mut self, service: &str, method: &str, args: Value) -> (u64, Value) {
        self.calls = self.calls.checked_add(1).unwrap_or(1);
        let call = json!({"op":"call","id":self.calls,"provider":"host","service":service,
            "version":1,"method":method,"args":args,"timeout_ms":CALL_MS});
        (self.calls, call)
    }

    /// What the application is asked once the extension starts: its settings, so that the page is placed among the
    /// application's pages when the person asked for it, whether or not the page is ever opened.
    pub fn start(&mut self) -> Vec<Value> {
        let (id, call) = self.call("peersus.config", "get", json!({}));
        self.asked_config = id;
        vec![call]
    }

    /// What the application is asked when the page opens and each time its clock ticks: this device's parts, or, while a
    /// paired device is chosen, the question to it (`remote`) when one is due; and the paired devices and the settings
    /// now and then. Nothing for any other delivery.
    pub fn ask(&mut self, delivery: &Value) -> Vec<Value> {
        let event = &delivery["args"]["event"];
        if delivery["type"] != "call"
            || delivery["method"] != "event"
            || !matches!(event["t"].as_str(), Some("open" | "tick"))
        {
            return Vec::new();
        }
        let opened = event["t"] == "open";
        let mut calls = Vec::new();
        let at = event["at"].as_u64().unwrap_or(0);
        if self.chosen.is_none() {
            for (index, part) in PARTS.iter().enumerate() {
                if !self.asked_parts[index].free(at) {
                    continue;
                }
                let (id, call) = self.call("peersus.system", part.method(), json!({}));
                self.asked_parts[index] = Pending { id, at };
                calls.push(call);
            }
        } else if let Some(call) = self.ask_device(opened, at) {
            calls.push(call);
        }
        self.ticks = self.ticks.wrapping_add(1);
        if (opened
            || !self.listed
            || self.ticks.is_multiple_of(DEVICES_EVERY)
            || self.chosen.is_some())
            && self.asked_devices.free(at)
        {
            let (id, call) = self.call("peersus.peers", "list", json!({}));
            self.asked_devices = Pending { id, at };
            calls.push(call);
        }
        if opened || self.ticks.is_multiple_of(CONFIG_EVERY) {
            let (id, call) = self.call("peersus.config", "get", json!({}));
            self.asked_config = id;
            calls.push(call);
        }
        calls.extend(self.tell_problem());
        calls
    }

    /// The application's answer to the last question of a kind, and what it leads to be asked: placing the page
    /// when its setting changed. Any other delivery changes nothing; an answer of the wrong shape keeps what was
    /// known, and a refusal of a part or of the devices is said where they would be.
    pub fn told(&mut self, delivery: &Value) -> Vec<Value> {
        if delivery["type"] != "result" {
            return Vec::new();
        }
        let Some(id) = delivery["id"].as_u64().filter(|id| *id != 0) else {
            return Vec::new();
        };
        let result = &delivery["result"];
        let denied = result["status"] == "error" && result["code"] == "denied";
        let value = (result["status"] == "ok").then_some(&result["value"]);
        if id == self.asked_devices.id {
            self.asked_devices = Pending::default();
            if denied {
                self.devices_denied = true;
                self.listed = true;
                self.listed_devices(&[]);
            } else if let Some(peers) = value.and_then(Value::as_array) {
                self.devices_denied = false;
                self.listed = true;
                self.listed_devices(peers);
            }
        } else if let Some(index) = self.asked_parts.iter().position(|asked| asked.id == id) {
            let asked = std::mem::take(&mut self.asked_parts[index]);
            let part = PARTS[index];
            if denied {
                self.own.deny(part);
            } else if let Some(value) = value.filter(|value| value.is_object()) {
                // Bounded as a reading from another device is: what the application says is data like any other.
                self.own.read(part, wire::value(part, value), asked.at);
            } else if result["status"] == "error" {
                // Not read now: what was read before is not shown as if it were current.
                self.own.unavailable(part);
            }
        } else if id == self.asked_send {
            self.sent(result);
        } else if id == self.asked_config {
            if let Some(config) = value {
                let wanted = config["settings"][PAGE_SETTING].as_bool().unwrap_or(false);
                if self.placed != Some(wanted) {
                    let pages = if wanted {
                        json!([{"id":"monitor-page","page":PAGE,"placement":"primary"}])
                    } else {
                        json!([])
                    };
                    let (id, call) = self.call("peersus.pages", "set", json!({"pages":pages}));
                    self.asked_pages = id;
                    self.placed = Some(wanted);
                    return vec![call];
                }
            }
        } else if id == self.asked_pages && result["status"] != "ok" {
            // Not placed after all: asked again with the next settings.
            self.placed = None;
        } else if id == self.asked_notify && denied {
            // Not allowed to show notifications: the page says it instead, from now on.
            self.notify_denied = true;
        }
        Vec::new()
    }

    fn listed_devices(&mut self, peers: &[Value]) {
        self.devices = peers.iter().take(DEVICES).cloned().collect();
        let Some(chosen) = self.chosen.as_deref() else {
            return;
        };
        let now = self
            .devices
            .iter()
            .find(|peer| peer["device"].as_str() == Some(chosen));
        if now.is_some_and(askable) {
            // Allowed again on this device: it is asked again, and the refusal is not remembered.
            if now.is_some_and(|peer| peer["allowed"] != false) {
                self.talk.refused = false;
            }
            return;
        }
        // A chosen device that stops being one to ask is unselected where the person sees it, not kept silently.
        let said = match now {
            Some(peer) if connected(peer) => {
                format!("{} was unselected: Monitor is not ready there.", name(peer))
            }
            Some(peer) => format!("{} was unselected: it is offline.", name(peer)),
            None if self.devices_denied => {
                "The chosen device was unselected: this extension may no longer see it.".to_owned()
            }
            None => "The chosen device was unselected: it is no longer shown here.".to_owned(),
        };
        self.choose(None);
        self.dropped = Some(said);
    }

    /// Choose the device whose numbers are shown (`None`: this device). What was said with the one before, and what is
    /// still out for it (a question, the stream it is to be written on), is let go: none of it is for the new one.
    fn choose(&mut self, device: Option<String>) {
        self.chosen = device;
        self.dropped = None;
        self.talk = remote::Conversation::default();
        self.asked_send = 0;
        self.outgoing.clear();
    }

    /// The page after `event`.
    pub fn answer(&mut self, event: &Value) -> Value {
        match (event["t"].as_str(), event["id"].as_str()) {
            (Some("size"), Some("width")) => {
                self.width = event["width"]
                    .as_u64()
                    .and_then(|width| u32::try_from(width / 1000).ok());
            }
            // A device chosen in the application's chooser, as `peersus.peers` names it; none is this device. Chosen only if it
            // can be asked, by the list last read.
            (Some("choose"), Some(CHOOSER)) => match event["device"].as_str() {
                None => self.choose(None),
                Some(device) => {
                    if let Some(peer) = self
                        .devices
                        .iter()
                        .find(|peer| peer["device"].as_str() == Some(device))
                        .filter(|peer| askable(peer) && peer["allowed"] != false)
                    {
                        let device = peer["device"].as_str().map(str::to_owned);
                        self.choose(device);
                    }
                }
            },
            _ => {}
        }
        self.page()
    }

    /// The numbers the cards show: those of the chosen paired device, as it answered, else this device's. Never one
    /// device's numbers under the other's name.
    fn shown(&self) -> &Gauges {
        if self.chosen.is_some() {
            &self.talk.gauges
        } else {
            &self.own
        }
    }
    /// What is known of `part` of the device shown.
    fn reading(&self, part: Part) -> &Reading {
        &self.shown().parts[part as usize]
    }
    /// The fields of `part`, `null` throughout when not known.
    fn known(&self, part: Part) -> &Value {
        static NOTHING: Value = Value::Null;
        match self.reading(part) {
            Reading::Known(value) => value,
            _ => &NOTHING,
        }
    }

    /// The chosen paired device, as the application last listed it.
    fn chosen_peer(&self) -> Option<&Value> {
        let chosen = self.chosen.as_deref()?;
        self.devices
            .iter()
            .find(|peer| peer["device"].as_str() == Some(chosen))
    }
    /// How it stands with the chosen device; `None` while the numbers shown are this device's.
    fn link(&self) -> Option<Link> {
        self.chosen.as_ref()?;
        // An application that says no `allowed` is an older one: it says only who may be seen.
        let refused = self
            .chosen_peer()
            .is_some_and(|peer| peer["allowed"] == false);
        Some(if refused {
            Link::Refused
        } else {
            self.talk.link()
        })
    }
    /// Whether this device's person did not allow Monitor what asking the chosen device takes.
    fn refused_here(&self) -> bool {
        self.link() == Some(Link::Refused)
    }
    /// Whose numbers the cards say: this device's, or the chosen device's by its name.
    fn whose(&self) -> &str {
        self.chosen_peer().map_or("This device", name)
    }

    /// The application's device chooser: this device first, then every paired device this extension may see. A device can be
    /// chosen while it is connected, awake or asleep (a sleeping device still answers), where the person allowed Monitor
    /// for it here and where Monitor is there to answer; the others are listed dimmed, saying why.
    fn chooser(&self) -> Value {
        let chooser = json!({"k":"devices","id":CHOOSER,"chosen":self.chosen,"this_device":true,
            "pick":{"presence":["online","sleeping"],"allowed":true,"accepts":wire::SERVICE}});
        bx(None, json!({"width":fill(),"gap":6}), vec![chooser])
    }

    /// What a card of a part not allowed says to do: for this device, where to allow it; for another device, that it
    /// is the person there who allows it, there.
    fn denial(&self) -> String {
        match self.chosen_peer() {
            Some(peer) => format!(
                "{} does not let Monitor share it. Allow it there, in this extension's permissions.",
                name(peer)
            ),
            None => "Allow it in this extension's permissions to see it here.".to_owned(),
        }
    }

    /// What the card of the graphics says of what its device does not publish, so that a number that is missing is
    /// explained and not only left empty. Only of an answer that came: before it, nothing is known to be missing.
    fn graphics_note(&self) -> Option<String> {
        let Reading::Known(value) = self.reading(Part::Graphics) else {
            return None;
        };
        let whose = self.whose();
        match (
            value["name"].is_string(),
            value["busy"].is_u64(),
            value["celsius"].is_number(),
        ) {
            (false, false, false) => Some(format!(
                "{whose}'s system does not publish its graphics to the application: it reads them only on Linux and Android, from the graphics driver."
            )),
            (_, false, _) => Some(format!(
                "{whose}'s graphics driver does not publish how busy it is."
            )),
            _ => None,
        }
    }

    /// What the page says under the bar of how it stands with the chosen device, when that is not the numbers and not a
    /// problem (see [`State::problem`]).
    fn talk_note(&self) -> Option<Value> {
        let who = name(self.chosen_peer()?);
        (self.link()? == Link::Asking).then(|| text(&format!("Asking {who}…"), "muted"))
    }
    /// The one thing wrong now that the person should know of, in Monitor's words: a chosen device that was unselected, one
    /// that does not answer, or paired devices not shown at all. Told once through the application's notifications.
    fn problem(&self) -> Option<String> {
        if let Some(dropped) = &self.dropped {
            return Some(dropped.clone());
        }
        if let Some(peer) = self.chosen_peer()
            && self.link() == Some(Link::Silent)
        {
            return Some(format!(
                "{} did not answer. Check that Monitor is installed there and allows this device, then choose it again.",
                name(peer)
            ));
        }
        self.devices_denied.then(|| {
            "Paired devices are not shown: allow this extension to see them in its permissions."
                .to_owned()
        })
    }
    /// The notification of a problem not told yet; a problem gone is forgotten, so that it is told again if it comes back.
    fn tell_problem(&mut self) -> Option<Value> {
        let problem = self.problem();
        if problem == self.told_problem {
            return None;
        }
        self.told_problem = problem.clone();
        let text = problem.filter(|_| !self.notify_denied)?;
        let (id, call) = self.call("peersus.notify", "show", json!({"text":text}));
        self.asked_notify = id;
        Some(call)
    }
    /// The one card of a chosen device that this device's person did not allow Monitor to ask.
    fn refused_card(&self) -> Value {
        let who = self.chosen_peer().map_or("that device", name);
        card(
            fill(),
            vec![
                text("Not allowed", "title"),
                text(
                    &format!(
                        "Allow this extension, for {who}, to see it, to send to it and to share numbers with it, in this extension's permissions. {who} has to allow the same for this device."
                    ),
                    "muted",
                ),
            ],
        )
    }

    /// The card of the processor or the graphics, as the concept draws it: its picture, name and maker with how busy it
    /// is in a tile at the right; the last minute of how busy as a line; two figures in frames (the clock, and the cores
    /// or the memory of the graphics); how warm, as a thermometer and a bar.
    fn chip_card(&self, part: Part, layout: Layout) -> Value {
        let width = layout.width(part);
        if *self.reading(part) == Reading::Denied {
            return not_allowed(part, width, &self.denial());
        }
        let value = self.known(part);
        let about = value["name"].as_str().unwrap_or("Not available");
        let shown = self.shown();
        let (busy, history) = match part {
            Part::Processor => (shown.busy, &shown.busy_history[..]),
            _ => (
                value["busy"].as_u64().and_then(|n| u16::try_from(n).ok()),
                &shown.graphics_history[..],
            ),
        };
        let compact = layout.compact();
        let second = match part {
            Part::Processor => tile(
                "Cores",
                &value["cores"]
                    .as_u64()
                    .map_or(UNKNOWN.to_owned(), |cores| format!("{cores} cores")),
                compact,
            ),
            _ => tile(
                "VRAM used",
                &value["memory_used"]
                    .as_u64()
                    .map_or(UNKNOWN.to_owned(), gib),
                compact,
            ),
        };
        let tight = layout.tight();
        let mut head = Vec::new();
        if !tight {
            head.push(badge(part, if compact { 30 } else { 40 }));
        }
        // The model on one line, as in the concept: a long one ends in an ellipsis.
        head.push(column(
            2,
            vec![text(part.title(), "title"), cut(about, "muted", 1)],
        ));
        head.push(pill(&busy.map_or(UNKNOWN.to_owned(), percent), compact));
        let tiles = vec![
            tile("Clock", &clock(value["mhz"].as_u64()), compact),
            second,
        ];
        let mut body = vec![
            beside(if compact { 6 } else { 10 }, head),
            graph(layout.graph(part), history),
            if tight {
                let tiles = tiles
                    .into_iter()
                    .map(|mut tile| {
                        tile["style"]["width"] = fill();
                        tile
                    })
                    .collect();
                bx(None, json!({"width":fill(),"gap":6}), tiles)
            } else {
                beside(if compact { 6 } else { 8 }, tiles)
            },
        ];
        body.extend(warmth(&value["celsius"]));
        if part == Part::Graphics
            && let Some(note) = self.graphics_note()
        {
            body.push(text(&note, "muted"));
        }
        card(width, body)
    }
    /// The card of the memory: its name, a large open ring of how much is used, and the sizes under it.
    fn memory_card(&self, layout: Layout) -> Value {
        let width = layout.width(Part::Memory);
        if *self.reading(Part::Memory) == Reading::Denied {
            return not_allowed(Part::Memory, width, &self.denial());
        }
        let space = space(self.known(Part::Memory));
        card(
            width,
            vec![
                title_row(Part::Memory, layout.tight()),
                bx(
                    None,
                    json!({"width":fill(),"center":true,"gap":4}),
                    vec![
                        // As large as the card leaves room for, up to 150.
                        arc(
                            space.used,
                            layout
                                .graph(Part::Memory)
                                .map_or(150, |w| w.clamp(90, 150) as u16),
                            "figure",
                            "Used",
                        ),
                        text(&space.said, "body"),
                    ],
                ),
            ],
        )
    }
    /// The card of the storage: an open ring of how much is used beside its name and sizes, and the disk it is on.
    fn storage_card(&self, layout: Layout) -> Value {
        let width = layout.width(Part::Storage);
        if *self.reading(Part::Storage) == Reading::Denied {
            return not_allowed(Part::Storage, width, &self.denial());
        }
        let value = self.known(Part::Storage);
        let space = space(value);
        // On a narrow card its name over a ring with the sizes under it, as the memory's; on a wide one the ring beside
        // its name and sizes, as in the concept.
        let mut body = if layout.compact() {
            vec![
                title_row(Part::Storage, layout.tight()),
                bx(
                    None,
                    json!({"width":fill(),"center":true,"gap":4}),
                    vec![
                        arc(space.used, 110, "title", "Used"),
                        text(&space.said, "body"),
                    ],
                ),
            ]
        } else {
            vec![beside(
                14,
                vec![
                    arc(space.used, 120, "title", "Used"),
                    column(4, vec![text("Storage", "title"), text(&space.said, "body")]),
                ],
            )]
        };
        if let Some(device) = value["device"].as_str() {
            body.push(bx(
                None,
                json!({"width":fill(),"horizontal":true,"gap":8,"padding":10,"radius":[14,14,14,14],
                    "background":"border","center":true}),
                vec![picture(DISK, "text"), text(device, "body")],
            ));
        }
        card(width, body)
    }
    /// The card of the network: its name, the last minute of its speed, the speeds now and what it carried.
    fn network(&self, layout: Layout) -> Value {
        let width = layout.width(Part::Network);
        if *self.reading(Part::Network) == Reading::Denied {
            return not_allowed(Part::Network, width, &self.denial());
        }
        let shown = self.shown();
        let rate = |n: Option<u64>| n.map_or(UNKNOWN.to_owned(), |n| format!("{}/s", bytes(n)));
        let network = self.known(Part::Network);
        let total = network["received"]
            .as_u64()
            .zip(network["sent"].as_u64())
            .map_or(UNKNOWN.to_owned(), |(r, s)| {
                format!("Total ↓ {} · ↑ {}", bytes(r), bytes(s))
            });
        card(
            width,
            vec![
                title_row(Part::Network, layout.tight()),
                graph(layout.graph(Part::Network), &shown.speed_history),
                line(
                    if layout.tight() {
                        "↓"
                    } else {
                        "↓ Download"
                    },
                    &rate(shown.speed.map(|s| s.0)),
                ),
                line(
                    if layout.tight() { "↑" } else { "↑ Upload" },
                    &rate(shown.speed.map(|s| s.1)),
                ),
                text(&total, "muted"),
            ],
        )
    }
    /// The card of the battery, as tall as the cards beside it: its name in a band at the top, and under it the card
    /// filled from the bottom as far as the battery is charged, with the time it gives, the charge and what it is doing.
    fn battery(&self, layout: Layout) -> Value {
        let width = layout.width(Part::Battery);
        if *self.reading(Part::Battery) == Reading::Denied {
            return not_allowed(Part::Battery, width, &self.denial());
        }
        let battery = self.known(Part::Battery);
        let charge = battery["charge"]
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .map(|n| n.min(10000));
        let state = match (charge, battery["charging"].as_bool()) {
            (None, _) => "Not available",
            (_, Some(true)) => "Charging",
            (_, Some(false)) => "Discharging",
            (_, None) => "",
        };
        let level = charge.map(|charge| {
            bx(
                None,
                json!({"width":fill(),"height":{"unit":"percent","value":charge},
                    "background":if charge < 2000 { "danger" } else { "accent" },"opacity":3500}),
                vec![],
            )
        });
        let mut figures = Vec::new();
        if let Some(left) = battery["minutes"].as_u64().filter(|_| charge.is_some()) {
            figures.push(text(&duration(left), "muted"));
        }
        figures.push(text(&charge.map_or(UNKNOWN.to_owned(), percent), "figure"));
        if !state.is_empty() {
            figures.push(text(state, "muted"));
        }
        bx(
            None,
            json!({"width":width,"height":layout.battery_height(),"background":"raised",
                "radius":[20,20,20,20],"clip":true}),
            vec![
                // The whole card fills from its bottom as far as the battery is charged, under its words.
                bx(
                    None,
                    json!({"overlay":true,"width":fill(),"height":fill(),"bottom":true}),
                    level.into_iter().collect(),
                ),
                bx(
                    None,
                    json!({"overlay":true,"width":fill(),"height":fill(),"padding":PADDING,"gap":2}),
                    [
                        vec![
                            title_row(Part::Battery, layout.tight()),
                            bx(None, json!({"width":fill(),"height":flex()}), vec![]),
                        ],
                        figures,
                    ]
                    .concat(),
                ),
            ],
        )
    }

    fn page(&self) -> Value {
        // A thin line as wide as the page, measured by the application: an element with an id takes the touches that
        // fall on it, so the measure is not the page's own box, which must stay free to be moved under a finger.
        let mut measure = bx(
            Some("width"),
            json!({"width":fill(),"height":px(1)}),
            vec![],
        );
        measure["observe"] = json!(["size"]);
        let mut children = vec![measure, self.chooser()];
        // A problem is told through the application's notifications; written here only where the person did not let
        // Monitor show any.
        if self.notify_denied
            && let Some(problem) = self.problem()
        {
            children.push(text(&problem, "body"));
        }
        if let Some(note) = self.talk_note() {
            children.push(note);
        }
        // A device with no battery (a desktop) has no battery card, and the other cards take its room: one that cannot read
        // a battery, or answers that it knows neither its charge nor whether it charges. Before the first answer there is
        // no card either, so a desktop never shows an empty one for a moment; a battery not allowed still says so.
        let battery = match self.reading(Part::Battery) {
            Reading::Denied => true,
            Reading::Known(value) => !(value["charge"].is_null() && value["charging"].is_null()),
            Reading::Unknown | Reading::Unavailable => false,
        };
        let layout = Layout::of(self.width, battery);
        // A row of cards, every one as tall as the tallest of them (as in the concept): the battery as tall as the two
        // rows beside it, and the cards of a row as tall as each other.
        let row = |cards: Vec<Value>| {
            let cards = cards
                .into_iter()
                .map(|mut card| {
                    card["style"]["height"] = json!({"unit":"fill_minimum"});
                    card
                })
                .collect();
            bx(
                None,
                json!({"width":fill(),"horizontal":true,"gap":GAP}),
                cards,
            )
        };
        if self.refused_here() {
            children.push(self.refused_card());
        } else {
            match layout {
                // In a row whose cards match the tallest, a flexible width is not shared out: the column of the four
                // cards is as wide as the two side by side.
                Layout::Wide { inner, left, .. } => {
                    let mut columns = vec![bx(
                        None,
                        json!({"width":portion(left, inner),"gap":GAP}),
                        vec![
                            row(vec![
                                self.chip_card(Part::Processor, layout),
                                self.chip_card(Part::Graphics, layout),
                            ]),
                            row(vec![
                                self.storage_card(layout),
                                self.network(layout),
                                self.memory_card(layout),
                            ]),
                        ],
                    )];
                    if battery {
                        columns.push(self.battery(layout));
                    }
                    children.push(row(columns));
                }
                Layout::Unmeasured | Layout::Grid { .. } => {
                    let mut cards = vec![
                        self.chip_card(Part::Processor, layout),
                        self.chip_card(Part::Graphics, layout),
                        self.memory_card(layout),
                        self.storage_card(layout),
                        self.network(layout),
                    ];
                    if battery {
                        cards.push(self.battery(layout));
                    }
                    match layout {
                        // Rows of as many cards as fit, every card of a row as tall as the tallest.
                        Layout::Grid { columns, .. } => {
                            let mut cards = cards.into_iter().peekable();
                            while cards.peek().is_some() {
                                children.push(row(cards.by_ref().take(columns as usize).collect()));
                            }
                        }
                        _ => children.push(bx(
                            None,
                            json!({"width":fill(),"horizontal":true,"wrap":true,"gap":GAP}),
                            cards,
                        )),
                    }
                }
            }
        }
        // As tall as the room it is given, and its content moves in it: six cards are taller than a phone.
        let root = bx(
            None,
            // No margin of its own: as a page it is in the application's frame, which gives the margins of every page.
            json!({"width":fill(),"height":fill(),"scroll":true,"gap":GAP}),
            children,
        );
        // Read again each time the clock ticks: soon until the devices were listed once, so that the bar is whole at
        // once, then every second.
        json!({"k":"interval","id":"watch","milliseconds":if self.listed { EVERY } else { 300 },"child":root})
    }
}

/// Answer one delivery. `None` for anything that is not a call.
pub fn handle(state: &mut State, delivery: &Value) -> Option<Value> {
    if delivery["type"] != "call" {
        return None;
    }
    let ticket = delivery["ticket"].as_u64()?;
    let result = if delivery["method"] == "event" {
        json!({"status":"ok","value":state.answer(&delivery["args"]["event"])})
    } else {
        json!({"status":"error","code":"failed","message":"Nothing here answers that"})
    };
    Some(json!({"op":"reply","ticket":ticket,"result":result}))
}

/// What one delivery leads to: what it tells, what is then asked, what is written of a question the application gave a
/// stream for, and its reply. `Err` only when the application cannot be written to.
fn step<H: Wire + Data>(
    host: &mut H,
    state: &mut State,
    delivery: &Value,
) -> Result<(), &'static str> {
    // Asked without waiting for the answers: the page is answered at once with what is known, and the next tick
    // draws what the application said meanwhile.
    let mut calls = state.told(delivery);
    calls.extend(state.ask(delivery));
    for call in calls {
        host.send(call)?;
    }
    state.flush(host);
    let reply = if delivery["type"] == "call" && delivery["method"] == "receive" {
        state.receive(host, delivery)
    } else {
        handle(state, delivery)
    };
    if let Some(reply) = reply {
        host.send(reply)?;
    }
    Ok(())
}

/// The whole life of the guest: its settings are asked once it starts, then every delivery is handled in the order it
/// came (what was kept while something was waited for comes first). Returns zero when the application goes away,
/// non-zero when it cannot be written to.
fn run<H: Wire + Data>(host: &mut H) -> i32 {
    let mut state = State::default();
    for call in state.start() {
        if host.send(call).is_err() {
            return 1;
        }
    }
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
