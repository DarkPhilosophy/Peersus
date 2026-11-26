use peersus_api::{
    canvas::FrameHeader,
    elements::{FEATURE_CANVAS, Node},
};
use peersus_plugin_canvas::run;
use peersus_plugin_runtime::{DataRequest, Transport};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeSet, VecDeque},
    fmt::Write,
    io::{self, Read},
    rc::Rc,
    time::{Duration, Instant},
};

const CLAIM: u64 = 1;
const ROAD: [u8; 4] = [248, 47, 26, 255];
const TEXT: [u8; 4] = [247, 235, 218, 255];
const SURFACE: [u8; 4] = [12, 18, 25, 255];

#[derive(Default)]
struct OnlineFixture {
    tile: Vec<u8>,
    /// Real tiles served one after another instead of `tile`, when there are any.
    real: Vec<Vec<u8>>,
    body: Body,
    requests: Vec<(u64, String)>,
    saved: Option<SavedMap>,
    downloads: usize,
    last_download: Option<u64>,
    painted_at: Option<u64>,
}

impl OnlineFixture {
    fn first_tile_at(&self) -> Option<u64> {
        self.requests
            .iter()
            .find_map(|(at, path)| path.ends_with(".pbf").then_some(*at))
    }

    fn ninth_tile_at(&self) -> Option<u64> {
        let mut keys = BTreeSet::new();
        self.requests.iter().find_map(|(at, path)| {
            (path.ends_with(".pbf") && keys.insert(path) && keys.len() == 9).then_some(*at)
        })
    }

    fn covered_at(&self) -> Option<u64> {
        self.painted_at
    }
}

fn map_node(node: &Value) -> Option<&Value> {
    if node["id"] == "map" {
        return Some(node);
    }
    node["children"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(node.get("child"))
        .find_map(map_node)
}

fn size(width: u32, height: u32) -> Value {
    json!({"t":"canvas_size","id":"map","x":0,"y":0,"width":width*1000,"height":height*1000,
        "scale":400,"palette":{"dark":true,"surface":SURFACE,"raised":[28,38,48,255],
        "text":TEXT,"muted":[161,170,179,255],"border":[45,102,151,255],"scrim":[0,0,0,180],
        "accent":ROAD,"success":[23,195,25,255],"danger":[188,52,53,255]}})
}

#[derive(Clone, Copy)]
struct FrameSample {
    at: u64,
    settled: u64,
    pixels: [usize; 2],
    hash: u64,
    land: usize,
    blank_permyriad: usize,
    text: usize,
    road_columns: u32,
}

#[derive(Default)]
struct Frames {
    at: u64,
    seq: u64,
    frames: Vec<FrameSample>,
}

struct CanvasStream {
    output: Rc<RefCell<Frames>>,
    header: Option<(u64, u64, usize, usize)>,
}

impl Read for CanvasStream {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("a canvas stream is write-only"))
    }
}

impl io::Write for CanvasStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some((seq, settled, w, h)) = self.header.take() {
            assert_eq!(bytes.len(), w * h * 4);
            let mut output = self.output.borrow_mut();
            assert!(seq > output.seq);
            output.seq = seq;
            let mut hash = 0u64;
            let (mut land, mut text, mut roads) = (0, 0, 0u32);
            let right = w - 23; // do not count the right-hand rail as landscape.
            let mut blank = 0;
            for y in (0..h - 16).step_by(2) {
                let row = &bytes[y * w * 4..(y * w + right) * 4];
                for (x, rgba) in row.chunks_exact(4).enumerate().step_by(2) {
                    let [r, g, b, _] = <[u8; 4]>::try_from(rgba).unwrap();
                    blank += usize::from(rgba == SURFACE);
                    hash = hash
                        .rotate_left(3)
                        .wrapping_add(u64::from(u32::from_le_bytes(rgba.try_into().unwrap())));
                    land += usize::from(b > 105 && g > 65 && r < 70);
                    text += usize::from(r > 180 && g > 170 && b > 160);
                    if r > 150 && g < 95 && b < 125 {
                        roads |= 1 << (x * 9 / right);
                    }
                }
            }
            let at = output.at;
            output.frames.push(FrameSample {
                at,
                settled,
                pixels: [w, h],
                hash,
                land,
                blank_permyriad: 10_000 * blank / (right.div_ceil(2) * (h - 16).div_ceil(2)),
                text,
                road_columns: roads.count_ones(),
            });
        } else {
            let header = FrameHeader::decode(bytes).unwrap();
            assert_eq!(header.id, "map");
            let w = header.layout.width() as usize;
            let h = header.layout.height() as usize;
            assert_eq!(header.layout.stride() as usize, w * 4);
            self.header = Some((header.seq, header.settled, w, h));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
enum Body {
    #[default]
    Tile,
    Index,
    NewIndex,
}

#[derive(Default)]
struct SavedMap {
    age: u64,
    network_available: bool,
    new_index_saved: bool,
    new_tiles_saved: BTreeSet<String>,
    index_checks: usize,
}

impl OnlineFixture {
    fn body(&self) -> &[u8] {
        match self.body {
            Body::Tile => &self.tile,
            Body::Index => METADATA,
            Body::NewIndex => FRESH_METADATA,
        }
    }

    fn get(&mut self, args: &Value, now: u64) -> Value {
        let path = args["path"].as_str().unwrap();
        if path.ends_with(".pbf") && !self.real.is_empty() {
            self.tile = self.real[self.requests.len() % self.real.len()].clone();
        }
        self.requests.push((now, path.into()));
        let keep = args["keep"] == true;
        let refresh = args["refresh"] == true;
        let kept_only = args["kept_only"] == true;
        let age = if let Some(cache) = &mut self.saved {
            let is_index = path == "/planet";
            let is_old_tile = path.starts_with("/planet/test/");
            let saved = keep
                && !refresh
                && (is_index || is_old_tile || cache.new_tiles_saved.contains(path));
            if !saved {
                if is_index && !keep && refresh {
                    cache.index_checks += 1;
                }
                if kept_only || !cache.network_available {
                    return json!({"status":"error","code":"unavailable"});
                }
                if keep && is_index {
                    assert!(refresh);
                    assert_eq!(
                        cache.new_tiles_saved.len(),
                        9,
                        "all replacements must already be saved"
                    );
                    cache.new_index_saved = true;
                } else if keep && !is_old_tile {
                    cache.new_tiles_saved.insert(path.into());
                }
            }
            self.body = if !is_index {
                Body::Tile
            } else if refresh || cache.new_index_saved {
                Body::NewIndex
            } else {
                Body::Index
            };
            // Model the new owner: null denotes network, an integer denotes a local copy.
            Some(saved.then_some(cache.age))
        } else {
            // The old owner neither reports age nor understands the new cache-only flags.
            assert!(!kept_only && !refresh);
            self.body = if path == "/planet" {
                Body::Index
            } else {
                Body::Tile
            };
            None
        };
        if age.flatten().is_none() {
            if let Some(last) = self.last_download {
                assert!(now - last >= 240, "request violates the owner's pacing");
            }
            self.last_download = Some(now);
            self.downloads += 1;
        }
        let mut result =
            json!({"status":"ok","value":{"resource":"fixture","bytes":self.body().len()}});
        if let Some(age) = age {
            result["value"]["age"] = json!(age);
        }
        result
    }
}

#[derive(Default)]
struct MapSession {
    incoming: VecDeque<Value>,
    events: VecDeque<Value>,
    last_event: Value,
    ticket: u64,
    answered: u64,
    fix: (f64, f64),
    now: u64,
    online: Option<OnlineFixture>,
    usage: Vec<(u64, usize)>,
    largest_reply: usize,
    last_delivery: String,
    heaviest: (u64, String),
    /// Every turn over 20M of fuel, with what was delivered before it: what to look at when a ceiling is near.
    heavy: Vec<(u64, String)>,
    frames: Rc<RefCell<Frames>>,
    watched: bool,
    claimed: bool,
    open_count: usize,
    pace: Option<Duration>,
    previous_event: Option<Instant>,
}

impl MapSession {
    fn drain_stats(&mut self) {
        if let Some(online) = &mut self.online {
            let frames = self.frames.borrow();
            if online.painted_at.is_none()
                && let Some(ninth) = online.ninth_tile_at()
                && let Some(frame) = frames.frames.last()
                && frame.at >= ninth
                && frame.road_columns >= 6
            {
                online.painted_at = Some(frame.at);
            }
        }
    }

    fn run(&mut self, module: &[u8], label: &str) {
        let expected = self.events.len() as u64;
        let result = run(module, self);
        self.drain_stats();
        let (fuel, memory) = self
            .usage
            .iter()
            .copied()
            .fold((0, 0), |(f, m), (fuel, bytes)| (f.max(fuel), m.max(bytes)));
        let frames = self.frames.borrow().frames.len();
        eprintln!(
            "{label}: {}/{expected} replies, {frames} raster frames, peak fuel {fuel} ({}), peak linear bytes {memory}, largest reply {} bytes",
            self.answered, self.heaviest.1, self.largest_reply
        );
        if !self.heavy.is_empty() {
            let mut heavy = self.heavy.clone();
            heavy.sort_by_key(|(fuel, _)| std::cmp::Reverse(*fuel));
            heavy.truncate(8);
            eprintln!(
                "{label}: {} turns over 20M of fuel, the heaviest {heavy:?}",
                self.heavy.len()
            );
        }
        assert!(
            result.is_ok(),
            "guest ended: {result:?}; last event {}",
            self.last_event
        );
        assert_eq!(self.answered, expected);
        assert!(
            frames > 0 && self.open_count == 1,
            "the replay must go through Canvas"
        );
        assert!(
            fuel <= 30_000_000,
            "{label}: {fuel} exceeds the 30M turn ceiling"
        );
    }
}

impl Transport for MapSession {
    type Stream = CanvasStream;
    const MEASURE_TURNS: bool = true;

    fn receive(&mut self, into: &mut [u8]) -> io::Result<usize> {
        self.drain_stats();
        let delivery = if let Some(delivery) = self.incoming.pop_front() {
            delivery
        } else if let Some(event) = self.events.pop_front() {
            assert_eq!(self.ticket, self.answered, "{:#}", self.last_event);
            if let Some(pace) = self.pace {
                // Use a paced physical renderer while virtual timestamps drive
                // the simulated gesture and HTTP scheduler.
                if let Some(previous) = self.previous_event {
                    std::thread::sleep(pace.saturating_sub(previous.elapsed()));
                }
                self.previous_event = Some(Instant::now());
            }
            self.ticket += 1;
            self.last_event = event;
            self.now = self.last_event["at"].as_u64().unwrap_or(self.now);
            self.frames.borrow_mut().at = self.now;
            json!({"type":"call","ticket":self.ticket,"method":"event","args":{"page":"where","event":self.last_event}})
        } else {
            return Ok(0);
        };
        self.last_delivery = if delivery["type"] == "call" {
            format!("{} at {}", self.last_event["t"].as_str().unwrap(), self.now)
        } else {
            format!("result {} at {}", delivery["id"], self.now)
        };
        let bytes = serde_json::to_vec(&delivery).unwrap();
        into[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }

    fn send(&mut self, message: &[u8]) -> io::Result<()> {
        let sent: Value = serde_json::from_slice(message)?;
        match sent["op"].as_str() {
            Some("call") => {
                let value = match (sent["service"].as_str(), sent["method"].as_str()) {
                    (Some("peersus.config"), Some("get")) => json!({
                        "settings":{"online-streets":self.online.is_some()},
                        "plugin":"location",
                        "missing_permissions":[],
                        "peers":[],
                        "page_features":[FEATURE_CANVAS,"pinch","rotate"]
                    }),
                    (Some("peersus.pages"), Some("set")) => json!({}),
                    (Some("peersus.peers"), Some("list")) => json!([]),
                    (Some("peersus.canvas"), Some("open")) => {
                        assert!(self.watched && !self.claimed);
                        assert_eq!(sent["args"], json!({"page":"where","canvas":"map"}));
                        self.claimed = true;
                        json!({"claim":CLAIM})
                    }
                    (Some("peersus.location"), Some("current")) => {
                        json!({"longitude":self.fix.0,"latitude":self.fix.1,"accuracy":10,"age":0})
                    }
                    (Some("peersus.http"), Some("restore-read")) => {
                        json!({"authorization":"fixture"})
                    }
                    (Some("peersus.http"), Some("get-resource")) => {
                        let result = self.online.as_mut().unwrap().get(&sent["args"], self.now);
                        self.incoming
                            .push_back(json!({"type":"result","id":sent["id"],"result":result}));
                        return Ok(());
                    }
                    (Some("peersus.http"), Some("read-resource")) => {
                        let body = self.online.as_ref().unwrap().body();
                        let start = sent["args"]["offset"].as_u64().unwrap() as usize;
                        let end = (start + 16 * 1024).min(body.len());
                        let mut hex = String::with_capacity(2 * (end - start));
                        for byte in &body[start..end] {
                            write!(hex, "{byte:02x}").unwrap();
                        }
                        json!({"hex":hex,"end":end==body.len()})
                    }
                    _ => panic!(
                        "unexpected Location call: {} / {}",
                        sent["service"], sent["method"]
                    ),
                };
                self.incoming.push_back(
                    json!({"type":"result","id":sent["id"],"result":{"status":"ok","value":value}}),
                );
            }
            Some("reply") => {
                assert_eq!(sent["ticket"], self.ticket);
                assert_eq!(sent["result"]["status"], "ok");
                let page = &sent["result"]["value"];
                if !page.is_null() {
                    assert!(
                        Node::parse(page).is_ok(),
                        "invalid tree after {}",
                        self.last_event
                    );
                    let map = map_node(page).expect("this host advertised Canvas");
                    assert_eq!(map["k"], "canvas");
                    assert_eq!(map["observe"], json!(["wheel", "size", "pinch", "rotate"]));
                    assert!(map.get("children").is_none() && map["style"].get("follow").is_none());
                    self.watched = true;
                }
                self.largest_reply = self.largest_reply.max(message.len());
                self.answered = self.ticket;
            }
            Some("subscribe") => assert_eq!(
                sent,
                json!({"op":"subscribe","provider":"host","service":"peersus.config","version":1,"event":"changed"})
            ),
            Some("cancel") => { /* The in-process fixture has no outstanding network futures. */ }
            _ => panic!("unexpected Location operation: {}", sent["op"]),
        }
        Ok(())
    }

    fn open_data(&mut self, request: DataRequest) -> io::Result<Self::Stream> {
        assert_eq!(request, DataRequest::CanvasClaim(CLAIM));
        assert!(std::mem::take(&mut self.claimed));
        self.open_count += 1;
        Ok(CanvasStream {
            output: Rc::clone(&self.frames),
            header: None,
        })
    }

    fn shutdown_data(&mut self, _: &mut Self::Stream) -> io::Result<()> {
        unreachable!()
    }

    fn poll(&mut self, _: &[(i32, &Self::Stream)], _: i32) -> io::Result<Option<i32>> {
        Ok(Some(0))
    }

    fn turn_usage(&mut self, fuel: u64, memory_bytes: usize) {
        if fuel > self.heaviest.0 {
            self.heaviest = (fuel, self.last_delivery.clone());
        }
        if fuel > 20_000_000 {
            self.heavy.push((fuel, self.last_delivery.clone()));
        }
        self.usage.push((fuel, memory_bytes));
    }
}

const METADATA: &[u8] =
    br#"{"tiles":["https://tiles.openfreemap.org/planet/test/{z}/{x}/{y}.pbf"],"maxzoom":14}"#;
const FRESH_METADATA: &[u8] =
    br#"{"tiles":["https://tiles.openfreemap.org/planet/fresh/{z}/{x}/{y}.pbf"],"maxzoom":14}"#;

fn module() -> Vec<u8> {
    std::fs::read(std::env::var_os("PEERSUS_TEST_LOCATION_MODULE").expect("guest build required"))
        .unwrap()
}

fn road_tile(roads: usize) -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut value: usize) {
        while value > 127 {
            out.push((value as u8 & 127) | 128);
            value >>= 7;
        }
        out.push(value as u8);
    }
    fn field(out: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
        out.push(tag << 3 | 2);
        varint(out, bytes.len());
        out.extend_from_slice(bytes);
    }
    let mut layer = Vec::new();
    field(&mut layer, 1, b"transportation");
    for i in 1..=roads {
        let mut geometry = vec![9, 0];
        varint(&mut geometry, i * 7900 / roads);
        geometry.push(7 << 3 | 2);
        for _ in 0..7 {
            varint(&mut geometry, 1140);
            geometry.push(2);
        }
        let mut feature = vec![18, 2, 0, 0, 24, 2];
        field(&mut feature, 4, &geometry);
        field(&mut layer, 2, &feature);
    }
    field(&mut layer, 3, b"class");
    field(&mut layer, 4, b"\x0a\x07primary");
    layer.extend_from_slice(b"\x28\x80\x20");
    let mut tile = Vec::new();
    field(&mut tile, 3, &layer);
    tile
}

fn wheel(dy: i32, at: u64) -> Value {
    json!({"t":"wheel","id":"map","x":793000,"y":461000,"dx":0,"dy":dy,"at":at})
}

#[test]
#[ignore = "requires PEERSUS_TEST_LOCATION_MODULE pointing to the built Location guest"]
fn the_real_guest_answers_every_desktop_zoom() {
    let mut events = VecDeque::from([
        json!({"t":"open"}),
        size(1616, 930),
        json!({"t":"press","id":"find"}),
    ]);
    let mut at = 0;
    for dy in [-40_000, 40_000, -40_000, 40_000] {
        for _ in 0..72 {
            at += 17;
            events.push_back(wheel(dy, at));
        }
    }
    let mut session = MapSession {
        events,
        fix: (26.1025, 44.4268),
        ..MapSession::default()
    };
    session.run(&module(), "offline map");
    let frames = session.frames.borrow();
    assert!(
        frames
            .frames
            .iter()
            .map(|f| f.hash)
            .collect::<BTreeSet<_>>()
            .len()
            > 20
    );
}

#[test]
#[ignore = "requires PEERSUS_TEST_LOCATION_MODULE pointing to the built Location guest"]
fn globe_and_continental_zoom_always_have_borders_and_country_labels() {
    let module = module();
    for (w, h) in [(1368, 970), (4096, 2160)] {
        let mut events = VecDeque::from([json!({"t":"open"}), size(w, h)]);
        // A cold font manager may defer the first raster. Exercise all gestures
        // after the helper has had time to initialize it.
        for at in [100, 200, 300] {
            events.push_back(json!({"t":"tick","id":"clock","at":at}));
        }
        for step in 1..=12 {
            events.push_back(
                json!({"t":"wheel","id":"map","x":w*500,"y":h*500,"dx":0,"dy":-16000,"at":300+step*100}),
            );
        }
        let mut session = MapSession {
            events,
            pace: Some(Duration::from_millis(18)),
            ..MapSession::default()
        };
        session.run(&module, "globe and continental zoom");
        let frames = session.frames.borrow();
        assert!(frames.frames.len() >= 12);
        assert!(frames.frames.iter().all(|f| f.land > 700 && f.text > 50));
    }
}

#[test]
#[ignore = "requires PEERSUS_TEST_LOCATION_MODULE pointing to the built Location guest"]
fn online_map_reaches_steady_state_while_gestures_and_tiles_continue() {
    let mut events = VecDeque::from([
        json!({"t":"open"}),
        size(1900, 980),
        json!({"t":"press","id":"find"}),
    ]);
    let mut at = 0;
    for _ in 0..40 {
        at += 100;
        events.push_back(json!({"t":"tick","id":"clock","at":at}));
    }
    for cycle in 0..280 {
        at += 17;
        events.push_back(wheel(-640_000, at));
        for (phase, x) in [
            ("down", 900000),
            ("move", 1140000),
            ("move", 1088000),
            ("up", 1088000),
        ] {
            at += 17;
            events.push_back(
                json!({"t":"pointer","id":"map","phase":phase,"x":x,"y":470000,"at":at}),
            );
        }
        for _ in 0..5 {
            at += 100;
            events.push_back(json!({"t":"tick","id":"clock","at":at}));
        }
        at += 17;
        events.push_back(wheel(640_000, at));
        if cycle % 19 == 0 {
            events.push_back(json!({"t":"press","id":"find"}));
        }
    }
    let mut session = MapSession {
        events,
        fix: (21.23, 45.75),
        online: Some(OnlineFixture {
            tile: road_tile(256),
            ..Default::default()
        }),
        ..Default::default()
    };
    session.run(&module(), "online stress");
    let online = session.online.as_ref().unwrap();
    assert!(online.requests.len() > 20, "the cache did not turn over");
    let distances: Vec<_> = online
        .requests
        .iter()
        .filter(|(at, path)| *at <= 4000 && path.ends_with(".pbf"))
        .map(|(_, p)| {
            let mut coordinate = p.trim_end_matches(".pbf").rsplit('/');
            let y: f64 = coordinate.next().unwrap().parse().unwrap();
            let x: f64 = coordinate.next().unwrap().parse().unwrap();
            let z: i32 = coordinate.next().unwrap().parse().unwrap();
            let n = 2_f64.powi(z);
            let (lon, lat) = session.fix;
            let cx = (lon + 180.) / 360. * n;
            let cy = (1. - lat.to_radians().tan().asinh() / std::f64::consts::PI) * n / 2.;
            (x + 0.5 - cx).hypot(y + 0.5 - cy)
        })
        .collect();
    assert_eq!(distances.len(), 9);
    assert!(
        distances.windows(2).all(|pair| pair[0] <= pair[1]),
        "tiles are requested centre-first"
    );
    let frames = session.frames.borrow();
    assert!(frames.frames.iter().any(|f| f.road_columns >= 6));
    let middle = session.usage[session.usage.len() / 2].1;
    let end = session.usage.last().unwrap().1;
    assert!(
        end.saturating_sub(middle) <= 1024 * 1024,
        "memory kept growing: {middle} to {end}"
    );
}

/// The tiles a device really saved: every file of the folder `PEERSUS_TEST_LOCATION_TILES` names, largest first. A city's
/// tiles carry many times the roads, buildings and names of [`road_tile`], and what a turn costs follows what a tile carries.
fn saved_tiles() -> Vec<Vec<u8>> {
    let folder = std::env::var_os("PEERSUS_TEST_LOCATION_TILES").expect("saved tiles required");
    let mut tiles: Vec<Vec<u8>> = std::fs::read_dir(folder)
        .unwrap()
        .filter_map(|entry| std::fs::read(entry.ok()?.path()).ok())
        .collect();
    tiles.sort_by_key(|tile| std::cmp::Reverse(tile.len()));
    assert!(!tiles.is_empty(), "the folder holds no tile");
    tiles
}

#[test]
#[ignore = "requires PEERSUS_TEST_LOCATION_MODULE and PEERSUS_TEST_LOCATION_TILES (a folder of real saved tiles)"]
fn a_city_of_real_tiles_is_loaded_zoomed_and_dragged_within_the_turn_budget() {
    let mut events = VecDeque::from([
        json!({"t":"open"}),
        size(1900, 980),
        json!({"t":"press","id":"find"}),
    ]);
    let mut at = 0;
    for _ in 0..40 {
        at += 100;
        events.push_back(json!({"t":"tick","id":"clock","at":at}));
    }
    // What a person does over a city, as in the stress above, with time for each tile to be read: wider and closer with
    // the wheel, a drag, a rest.
    for cycle in 0..120 {
        at += 17;
        events.push_back(wheel(-640_000, at));
        for (phase, x) in [
            ("down", 900000),
            ("move", 1140000),
            ("move", 1088000),
            ("up", 1088000),
        ] {
            at += 17;
            events.push_back(
                json!({"t":"pointer","id":"map","phase":phase,"x":x,"y":470000,"at":at}),
            );
        }
        for _ in 0..30 {
            at += 100;
            events.push_back(json!({"t":"tick","id":"clock","at":at}));
        }
        at += 17;
        events.push_back(wheel(640_000, at));
        if cycle % 19 == 0 {
            events.push_back(json!({"t":"press","id":"find"}));
        }
    }
    let mut session = MapSession {
        events,
        fix: (21.23, 45.75),
        online: Some(OnlineFixture {
            real: saved_tiles(),
            ..Default::default()
        }),
        ..Default::default()
    };
    session.run(&module(), "a city of real tiles");
    assert!(session.online.as_ref().unwrap().requests.len() >= 10);
}

#[test]
#[ignore = "requires PEERSUS_TEST_LOCATION_MODULE pointing to the built Location guest"]
fn continuous_gestures_keep_online_tiles_and_viewport_coverage_moving() {
    const START: u64 = 4_000;
    const SAMPLES: u64 = 30 * 60;
    let mut events = saved_session(SavedMap::default(), 40).events;
    for sample in 0..SAMPLES {
        let at = START + (sample + 1) * 1_000 / 60;
        let step = sample % 75;
        let (phase, x) = match step {
            0 => ("down", 350_000),
            73 => ("up", 350_000 + step * 8_000),
            74 => {
                events.push_back(wheel(
                    if sample / 75 % 2 == 0 {
                        -16_000
                    } else {
                        16_000
                    },
                    at,
                ));
                continue;
            }
            _ => ("move", 350_000 + step * 8_000),
        };
        events.push_back(json!({"t":"pointer","id":"map","phase":phase,"x":x,"y":490000,"at":at}));
    }
    let mut session = MapSession {
        events,
        fix: (112., 50.),
        pace: Some(Duration::from_millis(18)),
        online: Some(OnlineFixture {
            tile: road_tile(256),
            ..Default::default()
        }),
        ..Default::default()
    };
    session.run(&module(), "30 s of uninterrupted 60 Hz local map input");
    let online = session.online.as_ref().unwrap();
    let frames = session.frames.borrow();
    let moving: Vec<_> = frames.frames.iter().filter(|f| f.at > START).collect();
    assert!(moving.len() >= 1500);
    assert!(moving.last().unwrap().settled >= START + 29_000);
    // Dense roads overpaint land. Count only genuinely blank raster pixels,
    // not the absence of the land colour where a road covers it.
    let worst = moving.iter().max_by_key(|f| f.blank_permyriad).unwrap();
    assert!(
        worst.blank_permyriad <= 100,
        "{} per 10,000 pixels blank at {} ms",
        worst.blank_permyriad,
        worst.at
    );
    for period in 0..3 {
        let range = START + period * 10000 + 1..=START + (period + 1) * 10000;
        assert!(
            moving
                .iter()
                .any(|f| range.contains(&f.at) && f.road_columns >= 5)
        );
        assert!(
            online
                .requests
                .iter()
                .filter(|(at, path)| range.contains(at) && path.ends_with(".pbf"))
                .count()
                >= 3
        );
    }
    let middle = session.usage[session.usage.len() / 2].1;
    let end = session.usage.last().unwrap().1;
    assert!(end.saturating_sub(middle) <= 1024 * 1024);
}

#[test]
#[ignore = "requires PEERSUS_TEST_LOCATION_MODULE pointing to the built Location guest"]
fn twist_reprojects_the_globe_and_dense_streets_within_the_turn_budget() {
    const LOCAL_START: u64 = 10_500;
    const TURNS: u64 = 120;
    const WIDTH: u32 = 1_900;
    let rotate = |begin, turn, at| json!({"t":"rotate","id":"map","x":950000,"y":490000,"begin":begin,"turn":turn,"at":at});
    let zoom =
        |at| json!({"t":"wheel","id":"map","x":950000,"y":490000,"dx":0,"dy":-640000,"at":at});
    let mut events = VecDeque::from([
        json!({"t":"open"}),
        size(WIDTH, 980),
        json!({"t":"tick","id":"clock","at":100}),
        json!({"t":"tick","id":"clock","at":200}),
        rotate(true, 0, 201),
        rotate(false, 3500, 240),
        json!({"t":"tick","id":"clock","at":280}),
        json!({"t":"press","id":"centre"}),
        json!({"t":"press","id":"find"}),
        zoom(300),
        zoom(600),
    ]);
    for at in (700..=10_400).step_by(100) {
        events.push_back(json!({"t":"tick","id":"clock","at":at}));
    }
    events.push_back(rotate(true, 0, LOCAL_START));
    for turn in 1..=TURNS {
        events.push_back(rotate(false, turn * 120, LOCAL_START + turn * 40));
    }
    let settled = LOCAL_START + (TURNS + 1) * 40;
    events.push_back(rotate(false, 0, settled));
    for quiet in [100, 200, 300] {
        events.push_back(json!({"t":"tick","id":"clock","at":settled+quiet}));
    }
    let mut session = MapSession {
        events,
        fix: (21.23, 45.75),
        pace: Some(Duration::from_millis(40)),
        online: Some(OnlineFixture {
            tile: road_tile(1200),
            ..Default::default()
        }),
        ..Default::default()
    };
    session.run(&module(), "dense street twists");
    let frames = session.frames.borrow();
    let density = |frame: &FrameSample| frame.pixels[0] * 1000 / WIDTH as usize;
    if std::env::var_os("PEERSUS_TRACE_LOCATION_DENSITY").is_some() {
        for (index, frame) in frames.frames.iter().enumerate() {
            eprintln!(
                "twist frame {} at {}ms settled {}ms: {}×{} density {}‰, roads {}/9",
                index + 1,
                frame.at,
                frame.settled,
                frame.pixels[0],
                frame.pixels[1],
                density(frame),
                frame.road_columns,
            );
        }
    }
    let globe = frames
        .frames
        .iter()
        .find(|f| f.at <= 201)
        .expect("unrotated globe raster");
    let turned = frames
        .frames
        .iter()
        .find(|f| f.settled == 240)
        .expect("the globe publishes a rotated raster");
    assert_ne!(globe.hash, turned.hash);
    let local: Vec<_> = frames
        .frames
        .iter()
        .filter(|f| f.settled > LOCAL_START)
        .collect();
    assert!(
        local.len() >= 105,
        "dense rotated maps must keep publishing: {} raster frames, last settled {:?}",
        local.len(),
        local.last().map(|f| f.settled)
    );
    assert!(
        local.iter().all(|frame| density(frame) >= 240),
        "dense rotation must not publish below the preview density floor"
    );
    if let Some(frame) = local.iter().find(|frame| frame.road_columns < 3) {
        panic!(
            "twisting lost the road network at {}ms, {}‰ density: {}/9 columns",
            frame.settled,
            density(frame),
            frame.road_columns,
        );
    }
    assert!(local.iter().map(|f| f.hash).collect::<BTreeSet<_>>().len() >= 100);
    assert_eq!(local.last().unwrap().settled, settled);
}
#[test]
#[ignore = "requires PEERSUS_TEST_LOCATION_MODULE pointing to the built Location guest"]
fn restarted_guest_draws_all_nine_saved_tiles_without_a_download() {
    let mut session = saved_session(
        SavedMap {
            age: 3600,
            ..SavedMap::default()
        },
        40,
    );
    session.run(&module(), "saved map, network down");
    let online = session.online.unwrap();
    assert!(!session.frames.borrow().frames.is_empty());
    let covered = online
        .covered_at()
        .expect("all nine saved tiles were read and painted");
    eprintln!(
        "saved map: first tile at {:?} ms, complete viewport at {covered} ms; {} requests, {} downloads",
        online.first_tile_at(),
        online.requests.len(),
        online.downloads
    );
    assert!(covered <= 1_000);
    assert!(
        session
            .frames
            .borrow()
            .frames
            .iter()
            .any(|f| f.road_columns >= 6)
    );
    assert_eq!(online.downloads, 0);
    assert_eq!(
        online.requests.len(),
        10,
        "one saved index and nine saved tiles"
    );
}

fn saved_session(saved: SavedMap, ticks: u64) -> MapSession {
    let mut events = VecDeque::from([
        json!({"t":"open"}),
        size(1900, 980),
        json!({"t":"press","id":"find"}),
    ]);
    for n in 1..=ticks {
        events.push_back(json!({"t":"tick","id":"clock","at":n * 100}));
    }
    MapSession {
        events,
        fix: (21.23, 45.75),
        pace: Some(Duration::from_millis(18)),
        online: Some(OnlineFixture {
            tile: road_tile(256),
            saved: Some(saved),
            ..OnlineFixture::default()
        }),
        ..MapSession::default()
    }
}

#[test]
#[ignore = "requires PEERSUS_TEST_LOCATION_MODULE pointing to the built Location guest"]
fn a_restart_mid_index_migration_still_opens_the_previous_saved_map() {
    let module = module();
    let mut stage = saved_session(
        SavedMap {
            age: 90_000,
            network_available: true,
            ..SavedMap::default()
        },
        16,
    );
    stage.run(&module, "staged index before restart");
    let mut disk = stage.online.unwrap().saved.unwrap();
    assert_eq!(disk.index_checks, 1);
    assert!(
        !disk.new_index_saved,
        "the index changed before its tiles were saved"
    );
    let migrated = disk.new_tiles_saved.len();
    assert!(
        migrated > 0,
        "the first runtime stopped before it downloaded a replacement"
    );
    assert!(disk.new_tiles_saved.len() < 9);

    disk.network_available = false;
    let mut restarted = saved_session(disk, 20);
    restarted.run(&module, "mid-migration restart, network down");
    let offline = restarted.online.unwrap();
    let recovered_at = offline
        .covered_at()
        .expect("the offline map was painted again");
    assert!(recovered_at <= 1_000);
    assert_eq!(offline.downloads, 0);
    let mut disk = offline.saved.unwrap();
    assert!(!disk.new_index_saved);

    disk.network_available = true;
    let mut completed = saved_session(disk, 60);
    completed.run(&module, "finished saved-map migration");
    let online = completed.online.unwrap();
    let mut disk = online.saved.unwrap();
    assert!(disk.new_index_saved);
    assert_eq!(disk.new_tiles_saved.len(), 9);
    eprintln!(
        "staged index migration: stopped with {migrated}/9 replacements; previous viewport at {:?} ms after restart; {} downloads to finish",
        Some(recovered_at),
        online.downloads
    );
    disk.network_available = false;
    disk.age = 0;
    let mut reopened = saved_session(disk, 20);
    reopened.run(&module, "migrated saved map, network down");
    let reopened = reopened.online.unwrap();
    assert!(reopened.covered_at().is_some_and(|at| at <= 1_000));
    assert_eq!(reopened.downloads, 0);
}
