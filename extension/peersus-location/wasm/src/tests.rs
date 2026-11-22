use super::*;
use host::scripted::{Script, ack};

thread_local! {
    /// What the next presentation of a canvas answers instead of taking the frame, once: set by a test of a frame the
    /// helper refuses. Kept here and not in the scripted host, which is the SDK's file, copied byte for byte.
    static NEXT_CANVAS_STATUS: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

/// The helper answers the next presentation with `status` and takes no frame.
fn next_canvas_answers(status: i32) {
    NEXT_CANVAS_STATUS.set(Some(status));
}

impl CanvasIo for Script {
    fn open_canvas(&mut self, claim: u64) -> Option<Handle> {
        self.open(true, claim).ok()
    }
    fn present_canvas(&mut self, bytes: &[u8]) -> i32 {
        if let Some(status) = NEXT_CANVAS_STATUS.take() {
            return status;
        }
        self.written.clear();
        self.written.extend_from_slice(bytes);
        canvas_sdk::PRESENT_OK
    }
}

fn size(x: i32, y: i32, width: u32, height: u32) -> Value {
    json!({"t":"canvas_size","id":"map","x":x,"y":y,"width":width,"height":height,"scale":1000,
        "palette":{"dark":true,"surface":[1,2,3,255],"raised":[20,30,40,255],"text":[255,250,245,255],
        "muted":[120,130,140,255],"border":[45,60,80,255],"scrim":[0,0,0,180],"accent":[45,180,255,255],
        "success":[20,180,45,255],"danger":[180,45,45,255]}})
}

#[derive(Debug)]
struct LabelSample {
    text: String,
    size: u32,
    clip: Option<[f64; 4]>,
    direction: [f64; 2],
}

#[derive(Debug)]
struct PathSample {
    mode: u8,
    color: u8,
    width: f32,
    points: Vec<[f64; 2]>,
    verbs: u16,
    moves: usize,
    closed: usize,
    clip: Option<[f64; 4]>,
}

#[derive(Default)]
struct Art {
    seq: u64,
    settled: u64,
    raster: [u32; 3],
    surface: [u8; 4],
    labels: Vec<LabelSample>,
    paths: Vec<PathSample>,
}

impl Art {
    fn parse(mut bytes: &[u8]) -> Self {
        fn take<'a>(bytes: &mut &'a [u8], len: usize) -> &'a [u8] {
            let (value, rest) = bytes.split_at(len);
            *bytes = rest;
            value
        }
        fn array<const N: usize>(src: &mut &[u8]) -> [u8; N] {
            take(src, N).try_into().unwrap()
        }
        fn f32(src: &mut &[u8]) -> f32 {
            f32::from_le_bytes(array(src))
        }
        fn byte(src: &mut &[u8]) -> u8 {
            take(src, 1)[0]
        }
        fn color(src: &mut &[u8]) -> u8 {
            let tag = byte(src);
            if tag == 0 {
                take(src, 4);
            }
            assert!(tag <= 10);
            tag
        }
        assert_eq!(take(&mut bytes, 6), b"PSD1\x01\x03");
        assert_eq!(take(&mut bytes, 3), b"map");
        assert!(i32::from_le_bytes(array(&mut bytes)) > 0);
        let mut art = Self::default();
        art.seq = u64::from_le_bytes(array(&mut bytes));
        art.settled = u64::from_le_bytes(array(&mut bytes));
        art.raster = std::array::from_fn(|_| u32::from_le_bytes(array(&mut bytes)));
        art.surface = array(&mut bytes);
        take(&mut bytes, 32);
        let mut clip = Vec::new();
        let mut transform: Vec<[f64; 6]> = vec![[1., 0., 0., 1., 0., 0.]];
        let point =
            |[a, b, c, d, e, f]: [f64; 6], [x, y]: [f64; 2]| [a * x + c * y + e, b * x + d * y + f];

        loop {
            let affine = *transform.last().unwrap();
            match byte(&mut bytes) {
                2 => {
                    color(&mut bytes);
                }
                3 => {
                    let mode = byte(&mut bytes);
                    let color = color(&mut bytes);
                    let width = f32(&mut bytes)
                        * affine[0].hypot(affine[1]).max(affine[2].hypot(affine[3])) as f32;
                    take(&mut bytes, 3);
                    let verbs = u16::from_le_bytes(array(&mut bytes));
                    assert!((1..=512).contains(&verbs));
                    let mut points = Vec::new();
                    let (mut moves, mut closed) = (0, 0);
                    for _ in 0..verbs {
                        let pairs = match byte(&mut bytes) {
                            0 => {
                                moves += 1;
                                1
                            }
                            1 => 1,
                            2 => 2,
                            3 => 3,
                            4 => {
                                closed += 1;
                                0
                            }
                            _ => panic!("not a path verb"),
                        };
                        for _ in 0..pairs {
                            let xy = std::array::from_fn(|_| f64::from(f32(&mut bytes)));
                            assert!(xy.iter().all(|v| v.abs() <= 65_536.));
                            points.push(point(affine, xy));
                        }
                    }
                    art.paths.push(PathSample {
                        mode,
                        color,
                        width,
                        points,
                        verbs,
                        moves,
                        closed,
                        clip: clip.last().copied(),
                    });
                }
                4 => {
                    let size = f32(&mut bytes) as u32;
                    let ink = color(&mut bytes);
                    take(&mut bytes, 9);
                    let len = usize::from(u16::from_le_bytes(array(&mut bytes)));
                    let text = std::str::from_utf8(take(&mut bytes, len))
                        .unwrap()
                        .to_owned();
                    if ink == 4 {
                        art.labels.push(LabelSample {
                            text,
                            size,
                            clip: clip.last().copied(),
                            direction: [affine[0], affine[1]],
                        });
                    }
                }
                6 => {
                    let [x, y, w, h] = std::array::from_fn(|_| f64::from(f32(&mut bytes)));
                    let [l, t, r, b] = [[x, y], [x + w, y], [x, y + h], [x + w, y + h]]
                        .into_iter()
                        .map(|p| point(affine, p))
                        .fold(
                            [
                                f64::INFINITY,
                                f64::INFINITY,
                                f64::NEG_INFINITY,
                                f64::NEG_INFINITY,
                            ],
                            |[l, t, r, b], [x, y]| [l.min(x), t.min(y), r.max(x), b.max(y)],
                        );
                    clip.push([l, t, r - l, b - t]);
                }
                7 => {
                    clip.pop().expect("matching clip push");
                }
                8 => {
                    let [a, b, c, d, e, f] = std::array::from_fn(|_| f64::from(f32(&mut bytes)));
                    let [ax, ay] = point(affine, [a + e, b + f]);
                    let [bx, by] = point(affine, [c + e, d + f]);
                    let [x, y] = point(affine, [e, f]);
                    transform.push([ax - x, ay - y, bx - x, by - y, x, y]);
                }
                9 => {
                    transform.pop().expect("matching transform push");
                }
                11 => break,
                other => panic!("unexpected Canvas command {other}"),
            }
        }
        assert!(bytes.is_empty() && clip.is_empty() && transform.len() == 1);
        assert!(
            art.paths
                .iter()
                .map(|p| usize::from(p.verbs))
                .sum::<usize>()
                <= 65_536
        );
        art
    }

    fn label(&self, name: &str) -> bool {
        self.labels.iter().any(|l| l.text == name)
    }
}

pub(super) fn drawn_tile_paths(bytes: &[u8], mode: u8, color: u8) -> usize {
    Art::parse(bytes)
        .paths
        .iter()
        .filter(|path| path.mode == mode && path.color == color && path.clip.is_some())
        .count()
}

fn raw_art(state: &State) -> (Vec<u8>, canvas_scene::Picture) {
    let world::Frame(w, h) = state.frame();
    let metrics = state.surface.metrics.unwrap_or_else(|| {
        canvas_scene::Metrics::from_event(&size(0, 0, u32::from(w) * 1000, u32::from(h) * 1000))
            .unwrap()
    });
    let stamp = canvas_scene::Stamp {
        metrics,
        view: state.view.kept(),
        settled: state.settled,
        markers: (0, None),
        online: None,
        detail: tile::Detail::Full,
    };
    let mut buf = Vec::new();
    let mut list = canvas_sdk::List::begin(
        &mut buf,
        canvas_sdk::Frame {
            id: "map",
            stream: 7,
            seq: 1,
            settled: 0,
            raster: metrics.raster,
            palette: &metrics.palette,
        },
    );
    let picture = canvas_scene::render(state, stamp, true, &mut list);
    list.finish().expect("a bounded Canvas frame");
    (buf, picture)
}

fn art(state: &State) -> (Art, canvas_scene::Picture) {
    let (bytes, picture) = raw_art(state);
    (Art::parse(&bytes), picture)
}

fn pointer(state: &mut State, phase: &str, (x, y): (f64, f64), at: u64) {
    let (left, top, _, _) = state.map_at.unwrap();
    state.act(&json!({"t":"pointer","id":"map","phase":phase,"x":(x*1000.+left) as i32,"y":(y*1000.+top) as i32,"at":at}));
}

fn event(ticket: u64, event: Value) -> Value {
    json!({"type":"call","ticket":ticket,"method":"event","args":{"event":event}})
}

fn result(id: u64, result: Value) -> Value {
    json!({"type":"result","id":id,"result":result})
}

/// Encode the bytes a fixture delivers through the host's HTTP resource service.
pub(super) fn fixture_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").unwrap();
    }
    encoded
}

fn found(latitude: f64, longitude: f64, age: u64) -> Value {
    json!({"status":"ok","value":{"latitude":latitude,"longitude":longitude,"accuracy":10.0,"age":age}})
}

fn peer(alias: &str, name: &str, ready: bool) -> Value {
    json!({"device":alias,"name":name,"online":true,"presence":"online","allowed":true,
        "accepts": if ready { json!([wire::SERVICE]) } else { json!([]) }})
}

/// A position told at the start of the page's clock, with no age and no accuracy.
fn told(latitude: f64, longitude: f64) -> Told {
    Told {
        shared: wire::position(&json!({"latitude":latitude,"longitude":longitude,"age":0}))
            .unwrap(),
        at: 0,
        slack: 0,
    }
}

/// A stream the application opened under `claim` for a share of `told` to `alias`, that the person pressed for.
fn sending(claim: u64, alias: &str, told: Told) -> Sending {
    Sending {
        number: claim,
        alias: alias.to_owned(),
        told,
    }
}

/// The number of the `nth` call the extension sent, of `service` and `method`.
fn asked(host: &Script, service: &str, method: &str, nth: usize) -> u64 {
    calls(host, service, method)[nth]["id"].as_u64().unwrap()
}

/// Every call the extension sent, of `service` and `method`.
fn calls<'a>(host: &'a Script, service: &str, method: &str) -> Vec<&'a Value> {
    host.sent
        .iter()
        .filter(|sent| {
            sent["op"] == "call" && sent["service"] == service && sent["method"] == method
        })
        .collect()
}

/// The page of the reply to `ticket`.
fn page(host: &Script, ticket: u64) -> String {
    host.sent
        .iter()
        .rev()
        .filter(|sent| {
            sent["op"] == "reply" && sent["ticket"].as_u64().is_some_and(|id| id <= ticket)
        })
        .map(|reply| &reply["result"]["value"])
        .find(|view| !view.is_null())
        .map(Value::to_string)
        .expect("the page was not answered")
}

/// Handle every delivery, including what was kept for later while a stream was waited for.
fn run_all(host: &mut Script, state: &mut State) {
    while let Some(delivery) = state
        .waiting
        .pop_front()
        .or_else(|| host.deliveries.pop_front())
    {
        step(host, state, &delivery).unwrap();
    }
}

fn list_id(state: &mut State) -> u64 {
    if state.asked_list == 0 {
        state.list().unwrap()["id"].as_u64().unwrap()
    } else {
        state.asked_list
    }
}

fn permissions(host: &mut Script, state: &mut State, peers: Value, now: u64) {
    state.now = now;
    let id = list_id(state);
    host.deliveries
        .push_back(result(id, json!({"status":"ok","value":peers})));
    run_all(host, state);
}

fn missing_permission(owner: &str, name: &str) -> Value {
    json!({"status":"missing-permission","permission":{"owner":owner,"name":name}})
}

fn configuration(enabled: bool, missing: &[&str], peers: Value) -> Value {
    let missing: Vec<_> = missing
        .iter()
        .map(|name| json!({"owner":"host","name":name}))
        .collect();
    json!({"status":"ok","value":{
        "plugin":"location","settings":{"online-streets":enabled,"cache-online-areas":false},
        "missing_permissions":missing,"peers":peers
    }})
}

fn allowed(alias: &str, sends: bool) -> Value {
    let mut grants = vec![
        json!({"owner":"host","name":WATCH}),
        json!({"owner":"location","name":SHARE}),
    ];
    if sends {
        grants.push(json!({"owner":"host","name":SEND}));
    }
    json!({"device":alias,"permissions":grants})
}

#[test]
fn a_checked_listing_refusal_evicts_old_positions_without_periodic_retries() {
    let mut state = State {
        placed: Some(false),
        ..State::default()
    };
    let mut host = Script::default();
    state.options = true;
    state.own = Own::Found(told(45., 20.));
    state.seen.insert("a1".into(), told(44., 19.));
    let list = list_id(&mut state);
    step(
        &mut host,
        &mut state,
        &result(list, missing_permission("host", WATCH)),
    )
    .unwrap();
    assert!(state.seen.is_empty());
    assert!(
        state.own_now().is_some(),
        "permission to use the local position is independent"
    );
    assert!(
        state
            .page()
            .to_string()
            .contains("Paired devices: missing permission peers.watch on this device.")
    );
    state.now = 2 * IDLE_LIST_EVERY;
    assert!(state.waited().is_none());
    let change = json!({"type":"event","provider":"host","service":CONFIG,"version":1,"event":"changed","data":{}});
    step(&mut host, &mut state, &change).unwrap();
    let get = asked(&host, CONFIG, "get", 0);
    step(
        &mut host,
        &mut state,
        &result(get, configuration(false, &[], json!([allowed("a1", true)]))),
    )
    .unwrap();
    assert_eq!(host.sent.len(), 1, "a fresh grant is not a replay");
    assert!(state.waited().is_none());
    step(&mut host, &mut state, &event(7, json!({"t":"open"}))).unwrap();
    assert_eq!(
        calls(&host, PEERS, "list").len(),
        1,
        "an explicit reopen starts a new listing"
    );
}

#[test]
fn a_checked_fix_refusal_stops_automatic_finding_but_not_a_fresh_local_action() {
    let mut host = Script::default();
    let mut state = State::default();
    let peers = json!([peer("a1", "Phone", true)]);
    permissions(&mut host, &mut state, peers.clone(), 0);
    let fix = asked(&host, LOCATION, "current", 0);
    step(
        &mut host,
        &mut state,
        &result(fix, missing_permission("host", READ)),
    )
    .unwrap();
    assert_eq!(
        state.own_line(),
        "Locate this device: missing permission location.read on this device."
    );
    permissions(&mut host, &mut state, peers.clone(), SHARE_EVERY);
    assert_eq!(
        calls(&host, LOCATION, "current").len(),
        1,
        "a blocked automatic fix is not retried"
    );
    let cfg = state.ask_settings()["id"].as_u64().unwrap();
    step(
        &mut host,
        &mut state,
        &result(cfg, configuration(false, &[], json!([allowed("a1", true)]))),
    )
    .unwrap();
    permissions(&mut host, &mut state, peers, 2 * SHARE_EVERY);
    assert_eq!(
        calls(&host, LOCATION, "current").len(),
        1,
        "a new grant does not replay the automatic fix"
    );
    step(
        &mut host,
        &mut state,
        &event(6, json!({"t":"press","id":"find"})),
    )
    .unwrap();
    let fresh = asked(&host, LOCATION, "current", 1);
    step(&mut host, &mut state, &result(fresh, found(44., 19., 0))).unwrap();
    assert!(state.own_now().is_some());
    assert!(
        calls(&host, PEERS, "send").is_empty(),
        "locating is not a fresh share"
    );
}

#[test]
fn a_receive_only_device_stays_visible_and_cannot_be_sent_a_fix_or_question() {
    let mut host = Script::default();
    let mut state = State::default();
    let mut receiver = peer("a1", "Phone", true);
    receiver["accepts"] = json!([wire::SERVICE, wire::WANTED]);
    receiver["allowed"] = json!(false);
    receiver["missing_permissions"] = json!([{"owner":"host","name":SEND}]);
    permissions(&mut host, &mut state, json!([receiver]), 0);
    assert!(calls(&host, LOCATION, "current").is_empty());
    assert!(calls(&host, PEERS, "send").is_empty());
    host.incoming
        .push_back(wire::encode(&told(45., 20.).shared).unwrap());
    step(&mut host, &mut state, &json!({"type":"call","ticket":3,"service":wire::SERVICE,"method":"receive","args":{"peer":"a1"}})).unwrap();
    assert!(
        state.seen.contains_key("a1"),
        "read and send rights are independent"
    );
    assert!(state.device_lines()[0].contains("missing permission peers.send on this device."));
    let cfg = state.ask_settings()["id"].as_u64().unwrap();
    step(
        &mut host,
        &mut state,
        &result(cfg, configuration(false, &[WATCH], json!([]))),
    )
    .unwrap();
    assert!(
        state.seen.is_empty(),
        "loss of watch permission removes the retained position"
    );
    step(&mut host, &mut state, &json!({"type":"call","ticket":4,"service":wire::SERVICE,"method":"receive","args":{"peer":"a1"}})).unwrap();
    assert_eq!(
        host.sent.last().unwrap()["result"],
        json!({"status":"not-accepted"})
    );
    assert_eq!(host.opened, [(false, 3)], "no second payload is read");
}

#[test]
fn a_named_share_block_and_neutral_ack_leave_other_destinations_operational() {
    let mut host = Script::default();
    let mut state = State::default();
    let peers = json!([
        peer("a1", "Phone", true),
        peer("b2", "Desk", true),
        peer("c3", "Laptop", true)
    ]);
    permissions(&mut host, &mut state, peers.clone(), 0);
    let fix = asked(&host, LOCATION, "current", 0);
    step(&mut host, &mut state, &result(fix, found(45., 20., 0))).unwrap();
    let phone = asked(&host, PEERS, "send", 0);
    step(
        &mut host,
        &mut state,
        &result(phone, missing_permission("location", SHARE)),
    )
    .unwrap();
    let desk = asked(&host, PEERS, "send", 1);
    let body = br#"{"status":"not-accepted"}"#;
    let mut refusal = (body.len() as u32).to_be_bytes().to_vec();
    refusal.extend_from_slice(body);
    host.incoming.push_back(refusal);
    step(
        &mut host,
        &mut state,
        &result(desk, json!({"status":"ok","value":{"claim":41}})),
    )
    .unwrap();
    let laptop = asked(&host, PEERS, "send", 2);
    host.acks = true;
    step(
        &mut host,
        &mut state,
        &result(laptop, json!({"status":"ok","value":{"claim":42}})),
    )
    .unwrap();
    assert_eq!(host.opened, [(true, 41), (true, 42)]);
    assert_eq!(host.closed, 2);
    assert_eq!(
        state.device_lines()[0],
        "Phone: missing permission location.sharing on this device."
    );
    assert_eq!(state.device_lines()[1], "Desk: not accepted.");
    assert_eq!(state.share_line().as_deref(), Some("Shared with Laptop."));
    permissions(&mut host, &mut state, peers, SHARE_EVERY);
    let next = asked(&host, LOCATION, "current", 1);
    step(&mut host, &mut state, &result(next, found(44., 19., 0))).unwrap();
    let sends = calls(&host, PEERS, "send");
    assert_eq!(sends.len(), 4);
    assert_eq!(sends[3]["args"]["device"], "c3");
}

#[test]
fn a_refused_question_stops_but_a_transient_failure_keeps_the_normal_backoff() {
    let mut state = State::default();
    let mut host = Script::default();
    let mut phone = peer("a1", "Phone", false);
    let mut desk = peer("b2", "Desk", false);
    phone["accepts"] = json!([wire::WANTED]);
    desk["accepts"] = json!([wire::WANTED]);
    let peers = json!([phone, desk]);
    permissions(&mut host, &mut state, peers.clone(), 0);
    let a = asked(&host, PEERS, "send", 0);
    let b = asked(&host, PEERS, "send", 1);
    step(
        &mut host,
        &mut state,
        &result(a, json!({"status":"not-accepted"})),
    )
    .unwrap();
    step(
        &mut host,
        &mut state,
        &result(b, json!({"status":"error","code":"timeout"})),
    )
    .unwrap();
    permissions(&mut host, &mut state, peers.clone(), ASK_AGAIN - 1);
    assert_eq!(calls(&host, PEERS, "send").len(), 2);
    permissions(&mut host, &mut state, peers, ASK_AGAIN);
    let sends = calls(&host, PEERS, "send");
    assert_eq!(sends.len(), 3);
    assert_eq!(sends[2]["args"]["device"], "b2");
    assert_eq!(state.device_lines()[0], "Phone: not accepted.");
}

#[test]
fn the_page_is_among_the_applications_pages_only_while_the_persons_option_is_on() {
    let mut host = Script::default();
    let mut state = State::default();
    let settings = |on: Option<bool>| match on {
        Some(on) => json!({"status":"ok","value":{"settings":{PAGE_SETTING: on}}}),
        None => json!({"status":"ok","value":{"settings":{}}}),
    };
    let answer = |host: &mut Script, state: &mut State, said: Value| {
        let asked = state.ask_settings()["id"].as_u64().unwrap();
        host.deliveries.push_back(result(asked, said));
        run_all(host, state);
        calls(host, PAGES, "set")
            .last()
            .map(|call| (call["args"]["pages"].clone(), call["id"].as_u64().unwrap()))
    };
    // Never set: the page stays in the extension's settings, and that is said, so nothing earlier places it.
    let (pages, _) = answer(&mut host, &mut state, settings(None)).unwrap();
    assert_eq!(pages, json!([]));
    // Turned on: placed, from the declared page.
    let (pages, placing) = answer(&mut host, &mut state, settings(Some(true))).unwrap();
    assert_eq!(
        pages,
        json!([{"id":PAGE_SETTING,"page":PAGE,"placement":"primary"}])
    );
    // Said again unchanged: nothing more is asked.
    let before = calls(&host, PAGES, "set").len();
    answer(&mut host, &mut state, settings(Some(true)));
    assert_eq!(calls(&host, PAGES, "set").len(), before);
    // The application refused the placement: the next answer of the settings places it again.
    host.deliveries.push_back(result(
        placing,
        json!({"status":"error","code":"refused","message":"no"}),
    ));
    run_all(&mut host, &mut state);
    answer(&mut host, &mut state, settings(Some(true)));
    assert_eq!(calls(&host, PAGES, "set").len(), before + 1);
    // Turned off: taken out again.
    let (pages, _) = answer(&mut host, &mut state, settings(Some(false))).unwrap();
    assert_eq!(pages, json!([]));
}

#[test]
fn sharing_finds_this_device_and_sends_it_once_to_each_device_that_takes_it() {
    let mut host = Script {
        acks: true,
        ..Script::default()
    };
    let mut state = State {
        camera: Viewport {
            options: true,
            ..Viewport::default()
        },
        ..State::default()
    };
    host.deliveries.push_back(event(1, json!({"t":"open"})));
    run_all(&mut host, &mut state);
    let list = calls(&host, PEERS, "list")[0]["id"].as_u64().unwrap();
    host.deliveries.push_back(result(
        list,
        json!({"status":"ok","value":[peer("a1","Phone",true),peer("b2","Laptop",false)]}),
    ));
    host.deliveries
        .push_back(event(2, json!({"t":"press","id":"share"})));
    run_all(&mut host, &mut state);
    assert!(page(&host, 2).contains(words::FINDING));
    // Nothing is sent before this device is found.
    assert!(calls(&host, PEERS, "send").is_empty());
    let find = calls(&host, LOCATION, "current")[0];
    assert_eq!(find["timeout_ms"], FIND_MS);
    let find = find["id"].as_u64().unwrap();
    host.deliveries
        .push_back(result(find, found(44.4268, 26.1025, 3)));
    run_all(&mut host, &mut state);
    // Only the device whose Location takes it is sent to.
    let sends = calls(&host, PEERS, "send");
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["args"]["device"], "a1");
    assert_eq!(sends[0]["args"]["service"], wire::SERVICE);
    let send = sends[0]["id"].as_u64().unwrap();
    host.deliveries
        .push_back(result(send, json!({"status":"ok","value":{"claim":41}})));
    run_all(&mut host, &mut state);
    assert_eq!(host.opened, vec![(true, 41)]);
    let sent = wire::parse(&host.written).unwrap();
    assert_eq!(
        (sent.fix.latitude, sent.fix.longitude, sent.age),
        (44.4268, 26.1025, 3)
    );
    host.deliveries
        .push_back(event(3, json!({"t":"tick","id":"clock","at":1000})));
    run_all(&mut host, &mut state);
    assert!(page(&host, 3).contains("Shared with Phone."));
}

#[test]
fn no_eligible_recipient_does_not_even_request_a_position() {
    let mut host = Script::default();
    let mut state = State::default();
    permissions(
        &mut host,
        &mut state,
        json!([peer("b2", "Laptop", false)]),
        0,
    );
    assert!(calls(&host, LOCATION, "current").is_empty());
    assert!(calls(&host, PEERS, "send").is_empty());
}

#[test]
fn showing_this_device_sends_it_nowhere_and_a_refusal_is_said() {
    let mut host = Script::default();
    let mut state = State {
        camera: Viewport {
            options: true,
            ..Viewport::default()
        },
        ..State::default()
    };
    state.listed(&[peer("a1", "Phone", true)]);
    host.deliveries
        .push_back(event(1, json!({"t":"press","id":"find"})));
    run_all(&mut host, &mut state);
    let find = calls(&host, LOCATION, "current")[0]["id"].as_u64().unwrap();
    host.deliveries.push_back(result(find, found(1.0, 2.0, 0)));
    host.deliveries
        .push_back(event(2, json!({"t":"tick","id":"clock","at":1000})));
    run_all(&mut host, &mut state);
    assert!(calls(&host, PEERS, "send").is_empty());
    assert!(art(&state).0.label("This device"));
    // Not allowed: the person is told where to allow it, and nothing else is tried.
    host.deliveries
        .push_back(event(3, json!({"t":"press","id":"find"})));
    run_all(&mut host, &mut state);
    let find = calls(&host, LOCATION, "current")[1]["id"].as_u64().unwrap();
    host.deliveries.push_back(result(
        find,
        json!({"status":"error","code":"denied","message":"x"}),
    ));
    host.deliveries
        .push_back(event(4, json!({"t":"tick","id":"clock","at":2000})));
    run_all(&mut host, &mut state);
    assert!(calls(&host, PEERS, "send").is_empty());
    assert!(page(&host, 4).contains(words::NOT_ALLOWED));
    // The system said nothing: the application's own words are shown, on one bounded line.
    host.deliveries
        .push_back(event(5, json!({"t":"press","id":"find"})));
    run_all(&mut host, &mut state);
    let find = calls(&host, LOCATION, "current")[2]["id"].as_u64().unwrap();
    host.deliveries.push_back(result(
        find,
        json!({"status":"error","code":"unavailable","message":"Location is turned off\non this device"}),
    ));
    host.deliveries
        .push_back(event(6, json!({"t":"tick","id":"clock","at":3000})));
    run_all(&mut host, &mut state);
    assert!(page(&host, 6).contains("Location is turned off on this device"));
}

#[test]
fn a_position_shared_with_this_device_is_kept_under_its_name_and_drawn_from_here() {
    let mut host = Script::default();
    let mut state = State::default();
    state.listed(&[peer("a1", "Phone", true)]);
    state.own = Own::Found(told(44.4268, 26.1025));
    host.incoming.push_back(
        wire::encode(&wire::Shared {
            fix: geo::Fix::new(46.7712, 23.6236, Some(30.0)).unwrap(),
            age: 60,
        })
        .unwrap(),
    );
    host.deliveries
        .push_back(json!({"type":"call","ticket":9,"method":"receive","args":{"peer":"a1"}}));
    run_all(&mut host, &mut state);
    assert_eq!(host.opened, vec![(false, 9)]);
    let reply = host.sent.iter().find(|sent| sent["ticket"] == 9).unwrap();
    assert_eq!(reply["result"]["status"], "ok");
    host.deliveries
        .push_back(event(10, json!({"t":"tick","id":"clock","at":120_000})));
    run_all(&mut host, &mut state);
    let (shown, picture) = art(&state);
    assert!(picture.markers.iter().any(|m| m.id == "a1"));
    assert!(shown.label("Phone"));
    // Forget: gone from the page.
    host.deliveries
        .push_back(event(11, json!({"t":"press","id":"forget"})));
    run_all(&mut host, &mut state);
    assert!(!art(&state).0.label("Phone"));
}

#[test]
fn what_a_device_sends_that_is_not_a_position_is_refused_and_nothing_is_kept() {
    let mut host = Script::default();
    let mut state = State::default();
    host.incoming
        .push_back(br#"{"position":{"latitude":99,"longitude":0,"age":0}}"#.to_vec());
    host.deliveries
        .push_back(json!({"type":"call","ticket":4,"method":"receive","args":{"peer":"a1"}}));
    run_all(&mut host, &mut state);
    let reply = host.sent.iter().find(|sent| sent["ticket"] == 4).unwrap();
    assert_eq!(reply["result"]["status"], "error");
    assert!(state.seen.is_empty());
    // No name of the device that sent it: refused before anything is read.
    host.deliveries
        .push_back(json!({"type":"call","ticket":5,"method":"receive","args":{}}));
    run_all(&mut host, &mut state);
    assert_eq!(host.opened, vec![(false, 4)]);
}

#[test]
fn a_device_that_never_closes_its_stream_cannot_hold_the_extension() {
    let mut host = Script {
        silent: true,
        ..Script::default()
    };
    let mut state = State::default();
    host.deliveries
        .push_back(json!({"type":"call","ticket":4,"method":"receive","args":{"peer":"a1"}}));
    run_all(&mut host, &mut state);
    let reply = host.sent.iter().find(|sent| sent["ticket"] == 4).unwrap();
    assert_eq!(reply["result"]["status"], "error");
    assert!(host.waits.iter().all(|wait| *wait <= WAIT_MS));
    assert_eq!(host.closed, 1);
}

#[test]
fn keep_sharing_sends_again_only_every_five_minutes_and_stops_when_turned_off() {
    let mut host = Script::default();
    let mut state = State::default();
    permissions(&mut host, &mut state, json!([peer("a1", "Phone", true)]), 0);
    let finds = |host: &Script| calls(host, LOCATION, "current").len();
    assert_eq!(finds(&host), 1);
    let find = calls(&host, LOCATION, "current")[0]["id"].as_u64().unwrap();
    host.deliveries.push_back(result(find, found(1.0, 2.0, 0)));
    run_all(&mut host, &mut state);
    for at in [1_000, 60_000, SHARE_EVERY - 1] {
        permissions(
            &mut host,
            &mut state,
            json!([peer("a1", "Phone", true)]),
            at,
        );
    }
    assert_eq!(finds(&host), 1);
    permissions(
        &mut host,
        &mut state,
        json!([peer("a1", "Phone", true)]),
        SHARE_EVERY,
    );
    assert_eq!(finds(&host), 2);
    permissions(&mut host, &mut state, json!([]), 3 * SHARE_EVERY);
    assert_eq!(finds(&host), 2);
}

#[test]
fn keep_sharing_waits_five_minutes_after_this_device_was_not_found_too() {
    let mut host = Script::default();
    let mut state = State::default();
    permissions(&mut host, &mut state, json!([peer("a1", "Phone", true)]), 0);
    let find = asked(&host, LOCATION, "current", 0);
    host.deliveries.push_back(result(
        find,
        json!({"status":"error","code":"unavailable","message":"x"}),
    ));
    for at in [1_000, 60_000, SHARE_EVERY - 1] {
        permissions(
            &mut host,
            &mut state,
            json!([peer("a1", "Phone", true)]),
            at,
        );
    }
    run_all(&mut host, &mut state);
    assert_eq!(calls(&host, LOCATION, "current").len(), 1);
    permissions(
        &mut host,
        &mut state,
        json!([peer("a1", "Phone", true)]),
        SHARE_EVERY,
    );
    assert_eq!(calls(&host, LOCATION, "current").len(), 2);
    // Nothing was shared: only a share that went out counts as one.
    assert_eq!(state.shared_at, None);
}

/// A device whose Location takes a position and the question of where it is.
fn asking_peer(alias: &str, name: &str) -> Value {
    json!({"device":alias,"name":name,"online":true,"presence":"online","allowed":true,
        "accepts":[wire::SERVICE, wire::WANTED]})
}

/// The calls that asked the application for a stream of `service` to a device.
fn streams<'a>(host: &'a Script, service: &str) -> Vec<&'a Value> {
    calls(host, PEERS, "send")
        .into_iter()
        .filter(|call| call["args"]["service"] == service)
        .collect()
}

#[test]
fn a_device_nothing_is_known_of_is_asked_at_once_and_again_until_it_shares() {
    let mut host = Script {
        acks: true,
        ..Script::default()
    };
    let mut state = State::default();
    let devices = || json!([asking_peer("a1", "Phone"), peer("b2", "Laptop", true)]);
    permissions(&mut host, &mut state, devices(), 0);
    // Only the device whose Location takes the question is asked.
    let asks = streams(&host, wire::WANTED);
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0]["args"]["device"], "a1");
    assert_eq!(asks[0]["args"]["version"], wire::WANTED_VERSION);
    let ask = asks[0]["id"].as_u64().unwrap();
    host.deliveries
        .push_back(result(ask, json!({"status":"ok","value":{"claim":77}})));
    run_all(&mut host, &mut state);
    assert!(host.opened.contains(&(true, 77)));
    assert_eq!(host.written, wire::WANTED_MESSAGE);
    // It has not shared: asked again after a quarter of a minute, then after twice as long each time, and in the end as
    // often as a share is due. A listing in between asks nothing.
    let (mut at, mut times) = (0u64, 1usize);
    for wait in [
        15_000,
        30_000,
        60_000,
        120_000,
        240_000,
        SHARE_EVERY,
        SHARE_EVERY,
    ] {
        permissions(&mut host, &mut state, devices(), at + wait - 1);
        assert_eq!(streams(&host, wire::WANTED).len(), times, "before {wait}");
        at += wait;
        permissions(&mut host, &mut state, devices(), at);
        times += 1;
        assert_eq!(streams(&host, wire::WANTED).len(), times, "after {wait}");
    }
    // It shares: nothing more is asked while that is fresh.
    let arrived = at + 1_000;
    state.now = arrived;
    host.incoming.push_back(
        wire::encode(&wire::Shared {
            fix: geo::Fix::new(46.7712, 23.6236, Some(30.0)).unwrap(),
            age: 0,
        })
        .unwrap(),
    );
    host.deliveries
        .push_back(json!({"type":"call","ticket":9,"service":wire::SERVICE,
        "method":"receive","args":{"peer":"a1"}}));
    run_all(&mut host, &mut state);
    assert!(state.seen.contains_key("a1"));
    for later in [0, SHARE_EVERY, STALE - 1] {
        permissions(&mut host, &mut state, devices(), arrived + later);
    }
    assert_eq!(streams(&host, wire::WANTED).len(), times);
    // Old, and nothing new arrived: asked again, at once and then after a quarter of a minute, as at the start.
    permissions(&mut host, &mut state, devices(), arrived + STALE);
    assert_eq!(streams(&host, wire::WANTED).len(), times + 1);
    permissions(&mut host, &mut state, devices(), arrived + STALE + 14_999);
    assert_eq!(streams(&host, wire::WANTED).len(), times + 1);
    permissions(&mut host, &mut state, devices(), arrived + STALE + 15_000);
    assert_eq!(streams(&host, wire::WANTED).len(), times + 2);
    // No longer allowed to see any device: what was asked is forgotten with the rest.
    state.now = arrived + STALE + 15_001;
    let list = list_id(&mut state);
    host.deliveries.push_back(result(
        list,
        json!({"status":"error","code":"denied","message":"no"}),
    ));
    run_all(&mut host, &mut state);
    assert!(state.asked.is_empty() && state.asking.is_empty() && state.asks.is_empty());
}

#[test]
fn a_question_is_answered_to_the_device_that_asked_with_what_this_device_already_knows() {
    let mut host = Script {
        acks: true,
        ..Script::default()
    };
    let mut state = State::default();
    let devices = || json!([peer("a1", "Phone", true), peer("b2", "Laptop", true)]);
    let question = |ticket: u64, from: &str| {
        json!({"type":"call","ticket":ticket,"caller":"host","service":wire::WANTED,
            "version":wire::WANTED_VERSION,"method":"receive","args":{"peer":from}})
    };
    let ask = |host: &mut Script, state: &mut State, ticket: u64, from: &str, at: u64| {
        state.now = at;
        host.incoming.push_back(wire::WANTED_MESSAGE.to_vec());
        host.deliveries.push_back(question(ticket, from));
        run_all(host, state);
        let reply = host
            .sent
            .iter()
            .find(|sent| sent["ticket"] == ticket)
            .unwrap();
        assert_eq!(reply["result"]["status"], "ok");
    };
    let listed = |host: &mut Script, state: &mut State| {
        let list = calls(host, PEERS, "list").last().unwrap()["id"]
            .as_u64()
            .unwrap();
        host.deliveries
            .push_back(result(list, json!({"status":"ok","value":devices()})));
        run_all(host, state);
    };
    // Its own turn: found and shared once, with both.
    permissions(&mut host, &mut state, devices(), 0);
    let find = asked(&host, LOCATION, "current", 0);
    host.deliveries.push_back(result(find, found(1.0, 2.0, 0)));
    run_all(&mut host, &mut state);
    assert_eq!(streams(&host, wire::SERVICE).len(), 2);
    // A minute later one device asks: the devices are listed again and what is known goes to that device alone, a minute
    // older. The system is not asked where this device is before its next turn.
    let lists = calls(&host, PEERS, "list").len();
    ask(&mut host, &mut state, 7, "a1", 60_000);
    assert_eq!(calls(&host, PEERS, "list").len(), lists + 1);
    listed(&mut host, &mut state);
    let shares = streams(&host, wire::SERVICE);
    assert_eq!(shares.len(), 3);
    assert_eq!(shares[2]["args"]["device"], "a1");
    assert_eq!(calls(&host, LOCATION, "current").len(), 1);
    let share = shares[2]["id"].as_u64().unwrap();
    host.deliveries
        .push_back(result(share, json!({"status":"ok","value":{"claim":91}})));
    run_all(&mut host, &mut state);
    let sent = wire::parse(&host.written).unwrap();
    assert_eq!(
        (sent.fix.latitude, sent.fix.longitude, sent.age),
        (1.0, 2.0, 60)
    );
    // The same device asks again before its limit: understood, and nothing more is done.
    ask(&mut host, &mut state, 8, "a1", 60_000 + ANSWER_EVERY - 1);
    assert_eq!(calls(&host, PEERS, "list").len(), lists + 1);
    // Another device asks in that time: it is answered, and it alone.
    ask(&mut host, &mut state, 9, "b2", 60_000 + ANSWER_EVERY - 1);
    listed(&mut host, &mut state);
    let shares = streams(&host, wire::SERVICE);
    assert_eq!(shares.len(), 4);
    assert_eq!(shares[3]["args"]["device"], "b2");
    // After its limit the first is answered again.
    ask(&mut host, &mut state, 10, "a1", 60_000 + ANSWER_EVERY);
    listed(&mut host, &mut state);
    assert_eq!(streams(&host, wire::SERVICE).len(), 5);
    // What is not exactly the question is refused and leads to nothing, however long after.
    state.now = 10 * SHARE_EVERY;
    let lists = calls(&host, PEERS, "list").len();
    host.incoming
        .push_back(br#"{"wanted":"position","more":1}"#.to_vec());
    host.deliveries.push_back(question(11, "a1"));
    run_all(&mut host, &mut state);
    let reply = host.sent.iter().find(|sent| sent["ticket"] == 11).unwrap();
    assert_eq!(reply["result"]["status"], "error");
    assert_eq!(calls(&host, PEERS, "list").len(), lists);
    assert!(state.wanted.is_empty());
}

#[test]
fn a_question_that_cannot_be_answered_yet_is_answered_by_the_share_that_follows() {
    let mut host = Script::default();
    let mut state = State::default();
    let devices = || json!([peer("a1", "Phone", true)]);
    // Its turn has begun and the system has not said yet where this device is.
    permissions(&mut host, &mut state, devices(), 0);
    let find = asked(&host, LOCATION, "current", 0);
    // The device asks meanwhile: nothing can be sent, and the question is kept.
    state.now = 2_000;
    host.incoming.push_back(wire::WANTED_MESSAGE.to_vec());
    host.deliveries
        .push_back(json!({"type":"call","ticket":7,"service":wire::WANTED,
        "method":"receive","args":{"peer":"a1"}}));
    run_all(&mut host, &mut state);
    let list = calls(&host, PEERS, "list").last().unwrap()["id"]
        .as_u64()
        .unwrap();
    host.deliveries
        .push_back(result(list, json!({"status":"ok","value":devices()})));
    run_all(&mut host, &mut state);
    assert!(streams(&host, wire::SERVICE).is_empty());
    assert!(state.wanted.contains_key("a1"));
    // The system answers: the share of this turn goes to the device, and its question is answered by it.
    host.deliveries.push_back(result(find, found(1.0, 2.0, 0)));
    run_all(&mut host, &mut state);
    assert_eq!(streams(&host, wire::SERVICE).len(), 1);
    assert!(state.wanted.is_empty());
    assert_eq!(calls(&host, LOCATION, "current").len(), 1);
}

#[test]
fn a_question_to_a_device_that_was_never_found_waits_for_its_own_turn() {
    let mut host = Script::default();
    let mut state = State::default();
    // Its turn came and the system did not say where it is.
    permissions(&mut host, &mut state, json!([peer("a1", "Phone", true)]), 0);
    let find = asked(&host, LOCATION, "current", 0);
    host.deliveries.push_back(result(
        find,
        json!({"status":"error","code":"failed","message":"no fix"}),
    ));
    run_all(&mut host, &mut state);
    // Asked a minute later: there is nothing to send, and the system is not asked again before the next turn.
    state.now = 60_000;
    host.incoming.push_back(wire::WANTED_MESSAGE.to_vec());
    host.deliveries.push_back(
        json!({"type":"call","ticket":7,"service":wire::WANTED,"method":"receive","args":{"peer":"a1"}}),
    );
    run_all(&mut host, &mut state);
    let list = calls(&host, PEERS, "list").last().unwrap()["id"]
        .as_u64()
        .unwrap();
    host.deliveries.push_back(result(
        list,
        json!({"status":"ok","value":[peer("a1","Phone",true)]}),
    ));
    run_all(&mut host, &mut state);
    assert_eq!(calls(&host, LOCATION, "current").len(), 1);
    assert!(streams(&host, wire::SERVICE).is_empty());
}

#[test]
fn what_a_device_shared_stays_known_until_it_shares_again_however_long_that_takes() {
    let mut host = Script::default();
    let mut state = State::default();
    let listing = |host: &mut Script, state: &mut State, at: u64, answer: Value| {
        state.now = at;
        let list = list_id(state);
        host.deliveries.push_back(result(list, answer));
        run_all(host, state);
    };
    let devices = json!({"status":"ok","value":[peer("a1","Phone",true)]});
    listing(&mut host, &mut state, 0, devices.clone());
    state.seen.insert("a1".into(), told(44.43, 26.10));
    // Hours without a new share, the device gone offline meanwhile: where it was last is still what is known.
    let offline = json!({"status":"ok","value":[
        {"device":"a1","name":"Phone","online":false,"presence":"offline","allowed":true}]});
    for hour in 1..=6 {
        listing(&mut host, &mut state, hour * 3_600_000, offline.clone());
    }
    assert!(state.seen.contains_key("a1"));
    // A listing the application could not answer just now takes nothing away either.
    for failure in ["timeout", "busy", "failed"] {
        listing(
            &mut host,
            &mut state,
            7 * 3_600_000,
            json!({"status":"error","code":failure,"message":"later"}),
        );
        assert!(state.seen.contains_key("a1"), "{failure}");
        assert_eq!(state.devices.len(), 1, "{failure}");
    }
    // A refusal does: Location may no longer see any device.
    listing(
        &mut host,
        &mut state,
        8 * 3_600_000,
        json!({"status":"error","code":"denied","message":"no"}),
    );
    assert!(state.seen.is_empty() && state.devices.is_empty());
}

#[test]
fn revoking_the_recipient_before_a_fix_prevents_delivery_even_after_a_stale_share_press() {
    let mut host = Script::default();
    let mut state = State::default();
    permissions(&mut host, &mut state, json!([peer("a1", "Phone", true)]), 0);
    permissions(&mut host, &mut state, json!([]), 1);
    let find = asked(&host, LOCATION, "current", 0);
    host.deliveries.push_back(result(find, found(1.0, 2.0, 0)));
    run_all(&mut host, &mut state);
    // Found, and shown here, but the person turned Keep sharing off before: it goes nowhere.
    assert!(matches!(state.own, Own::Found(_)));
    assert!(calls(&host, PEERS, "send").is_empty());
    assert!(state.act(&json!({"t":"press","id":"share"})).is_empty());
    assert!(
        state
            .act(&json!({"t":"toggle","id":"keep","on":true}))
            .is_empty()
    );
    assert!(calls(&host, PEERS, "send").is_empty());
}

#[test]
fn a_share_sends_the_position_it_is_of_whatever_this_device_is_found_to_be_since() {
    let mut host = Script {
        acks: true,
        ..Script::default()
    };
    let mut state = State::default();
    permissions(&mut host, &mut state, json!([peer("a1", "Phone", true)]), 0);
    let find = asked(&host, LOCATION, "current", 0);
    host.deliveries
        .push_back(result(find, found(10.0, 20.0, 0)));
    // The stream is not open yet when the person only looks where this device is, and it is somewhere else.
    host.deliveries
        .push_back(event(2, json!({"t":"press","id":"find"})));
    run_all(&mut host, &mut state);
    let find = asked(&host, LOCATION, "current", 1);
    host.deliveries
        .push_back(result(find, found(30.0, 40.0, 0)));
    host.deliveries.push_back(result(
        asked(&host, PEERS, "send", 0),
        json!({"status":"ok","value":{"claim":41}}),
    ));
    run_all(&mut host, &mut state);
    assert_eq!(state.own_now().unwrap().fix.latitude, 30.0);
    assert_eq!(host.opened, vec![(true, 41)]);
    let sent = wire::parse(&host.written).unwrap();
    assert_eq!((sent.fix.latitude, sent.fix.longitude), (10.0, 20.0));
}

#[test]
fn a_stream_opened_for_a_share_that_is_no_longer_the_last_is_not_written() {
    let mut host = Script {
        acks: true,
        ..Script::default()
    };
    let mut state = State::default();
    state.listed(&[peer("a1", "Phone", true)]);
    for (nth, latitude) in [(0, 10.0), (1, 30.0)] {
        permissions(
            &mut host,
            &mut state,
            json!([peer("a1", "Phone", true)]),
            nth as u64 * SHARE_EVERY,
        );
        let find = asked(&host, LOCATION, "current", nth);
        host.deliveries
            .push_back(result(find, found(latitude, 0.0, 0)));
        run_all(&mut host, &mut state);
    }
    // The first share's stream comes only now, after the second share began: nothing is written on it.
    host.deliveries.push_back(result(
        asked(&host, PEERS, "send", 0),
        json!({"status":"ok","value":{"claim":41}}),
    ));
    run_all(&mut host, &mut state);
    assert!(host.opened.is_empty());
    host.deliveries.push_back(result(
        asked(&host, PEERS, "send", 1),
        json!({"status":"ok","value":{"claim":42}}),
    ));
    run_all(&mut host, &mut state);
    assert_eq!(host.opened, vec![(true, 42)]);
    assert_eq!(wire::parse(&host.written).unwrap().fix.latitude, 30.0);
    assert_eq!(state.delivered.get("a1"), Some(&true));
}

#[test]
fn a_position_that_arrives_in_many_pieces_is_one_message_bounded_by_its_bytes_not_its_pieces() {
    let mut host = Script::default();
    let mut state = State::default();
    let message = wire::encode(&wire::Shared {
        fix: geo::Fix::new(46.7712, 23.6236, Some(30.0)).unwrap(),
        age: 60,
    })
    .unwrap();
    // One byte a read: far more reads than a message has pieces on any real connection.
    assert!(message.len() > 32);
    host.incoming.extend(message.iter().map(|byte| vec![*byte]));
    host.deliveries
        .push_back(json!({"type":"call","ticket":9,"method":"receive","args":{"peer":"a1"}}));
    run_all(&mut host, &mut state);
    let reply = host.sent.iter().find(|sent| sent["ticket"] == 9).unwrap();
    assert_eq!(reply["result"]["status"], "ok");
    assert_eq!(state.seen["a1"].shared.fix.latitude, 46.7712);
    // One wait before every read, the last one for its end; each for as long as a piece is waited for, never longer.
    assert_eq!(host.waits.len(), message.len() + 1);
    assert!(host.waits.iter().all(|wait| *wait == WAIT_MS));
    // One byte more than a message may have, in pieces as small: seen to be too long, and refused.
    host.waits.clear();
    host.incoming
        .extend(std::iter::repeat_n(vec![b' '], wire::MAX + 1));
    host.deliveries
        .push_back(json!({"type":"call","ticket":10,"method":"receive","args":{"peer":"b2"}}));
    run_all(&mut host, &mut state);
    let reply = host.sent.iter().find(|sent| sent["ticket"] == 10).unwrap();
    assert_eq!(reply["result"]["status"], "error");
    assert!(!state.seen.contains_key("b2"));
    // Read no further than the one byte that shows it: what is left of it is never asked for.
    assert_eq!(host.waits.len(), wire::MAX + 1);
}

#[test]
fn a_share_written_waits_for_the_application_to_say_every_byte_arrived() {
    let mut host = Script::default();
    let mut state = State::default();
    // The application says fewer bytes reached the other side than were written: not delivered.
    host.incoming.push_back(ack(3));
    state.outgoing.push(sending(5, "a1", told(1.0, 2.0)));
    state.flush(&mut host);
    assert_eq!(state.delivered.get("a1"), Some(&false));
    assert_eq!(host.shutdowns, 1);
    assert_eq!(host.closed, 1);
    // The application never says it arrived: waited for only as long as an acknowledgement is, not delivered, closed.
    let mut host = Script {
        silent: true,
        ..Script::default()
    };
    state.outgoing.push(sending(6, "b2", told(1.0, 2.0)));
    state.flush(&mut host);
    assert_eq!(state.delivered.get("b2"), Some(&false));
    assert_eq!(host.waits, [ACK_MS]);
    assert_eq!(host.closed, 1);
}

#[test]
fn a_position_grown_too_old_to_be_understood_is_not_written_on_its_stream() {
    let mut host = Script {
        acks: true,
        ..Script::default()
    };
    let mut state = State::default();
    let mut old = told(1.0, 2.0);
    old.shared.age = geo::AGE_MAX;
    state.outgoing.push(sending(5, "a1", old));
    state.flush(&mut host);
    assert_eq!(state.delivered.get("a1"), Some(&true));
    // One second later the same position is a second too old: its stream is not opened at all.
    state.now = 1_000;
    state.outgoing.push(sending(6, "b2", old));
    state.flush(&mut host);
    assert_eq!(state.delivered.get("b2"), Some(&false));
    assert_eq!(host.opened, vec![(true, 5)]);
}

#[test]
fn keep_sharing_turned_off_before_its_streams_are_open_writes_nothing_on_them() {
    let mut host = Script {
        acks: true,
        ..Script::default()
    };
    let mut state = State::default();
    permissions(&mut host, &mut state, json!([peer("a1", "Phone", true)]), 0);
    host.deliveries.push_back(result(
        asked(&host, LOCATION, "current", 0),
        found(1.0, 2.0, 0),
    ));
    permissions(&mut host, &mut state, json!([]), 1);
    host.deliveries.push_back(result(
        asked(&host, PEERS, "send", 0),
        json!({"status":"ok","value":{"claim":41}}),
    ));
    run_all(&mut host, &mut state);
    assert!(host.opened.is_empty());
    // Explicit regrant permits a fresh fix, never the old claim.
    permissions(
        &mut host,
        &mut state,
        json!([peer("a1", "Phone", true)]),
        SHARE_EVERY,
    );
    host.deliveries.push_back(result(
        asked(&host, LOCATION, "current", 1),
        found(1.0, 2.0, 0),
    ));
    run_all(&mut host, &mut state);
    host.deliveries.push_back(result(
        asked(&host, PEERS, "send", 1),
        json!({"status":"ok","value":{"claim":42}}),
    ));
    run_all(&mut host, &mut state);
    assert_eq!(host.opened, vec![(true, 42)]);
}

#[test]
fn the_guest_answers_what_comes_in_order_and_ends_quietly_when_the_application_goes() {
    let mut host = Script::default();
    host.deliveries.push_back(event(1, json!({"t":"open"})));
    host.deliveries
        .push_back(json!({"type":"call","ticket":2,"method":"other","args":{}}));
    assert_eq!(run(&mut host), 0);
    assert_eq!(host.sent[0], host::subscribe_config());
    assert_eq!(
        (&host.sent[1]["service"], &host.sent[1]["method"]),
        (&json!(CONFIG), &json!("get"))
    );
    let replies: Vec<_> = host
        .sent
        .iter()
        .filter(|sent| sent["op"] == "reply")
        .collect();
    assert_eq!(
        (
            replies[0]["ticket"].as_u64(),
            replies[0]["result"]["status"].as_str()
        ),
        (Some(1), Some("ok"))
    );
    assert_eq!(
        (
            replies[1]["ticket"].as_u64(),
            replies[1]["result"]["status"].as_str()
        ),
        (Some(2), Some("error"))
    );
}

#[test]
fn a_device_unpaired_or_no_longer_allowed_is_forgotten_at_once() {
    let mut state = State::default();
    let shared = wire::position(&json!({"latitude":1.0,"longitude":2.0,"age":0})).unwrap();
    for alias in ["a1", "b2", "c3"] {
        state.seen.insert(
            alias.to_owned(),
            Told {
                shared,
                at: 0,
                slack: 0,
            },
        );
    }
    let mut no_longer = peer("b2", "Laptop", true);
    no_longer["allowed"] = json!(false);
    // c3 was unpaired: it is not listed at all. a1 is only offline: its last position stays.
    let mut offline = peer("a1", "Phone", false);
    offline["online"] = json!(false);
    offline["presence"] = json!("offline");
    state.listed(&[offline, no_longer]);
    assert_eq!(state.seen.keys().collect::<Vec<_>>(), ["a1"]);
    assert!(art(&state).0.label("Phone"));
    assert!(!art(&state).0.label("Laptop"));
    // Seeing paired devices is no longer allowed at all: nothing of any is shown.
    state.asked_list = 7;
    state.told(&result(7, json!({"status":"error","code":"denied"})));
    assert!(state.seen.is_empty());
    assert!(!state.page().to_string().contains(words::DEVICES_DENIED));
    state.options = true;
    assert!(state.page().to_string().contains(words::DEVICES_DENIED));
}

#[test]
fn options_say_why_a_paired_device_is_not_on_the_map() {
    let mut state = State::default();
    state.options = true;
    // Nothing allowed for any device: the application lists none, and the page says what a device needs to be shown.
    state.listed(&[]);
    assert!(state.page().to_string().contains(words::NONE_ALLOWED));
    // Allowed devices are said one by one: where one is, that one has not said yet, that one cannot say now. A position
    // its device shared within its turn says no age.
    let shared = wire::position(&json!({"latitude":45.0,"longitude":25.0,"age":120})).unwrap();
    let told = state.tell(shared);
    state.seen.insert("a1".to_owned(), told);
    state.listed(&[
        peer("a1", "Phone", true),
        peer("b2", "Laptop", true),
        peer("c3", "Tablet", false),
    ]);
    assert_eq!(
        state.device_lines(),
        [
            "Phone: 45.0000° N, 25.0000° E".to_owned(),
            format!("Laptop: {}", words::NOT_YET),
            format!("Tablet: {}", words::NOT_READY),
        ]
    );
    let page = state.page().to_string();
    assert!(page.contains("Laptop") && !page.contains(words::NONE_ALLOWED));
    // With Options closed the map is alone, as before.
    state.options = false;
    assert!(!state.page().to_string().contains("Laptop"));
}

/// A host for the main loop alone: each idle wait runs out, or is cut short by the delivery scripted for it. When the
/// script is over the wait itself fails, so nothing more is counted.
#[derive(Default)]
struct Idle {
    waits: VecDeque<Option<Value>>,
    next: Option<Value>,
    sent: Vec<Value>,
    waited: Vec<i32>,
}

impl Wire for Idle {
    fn receive(&mut self) -> Result<Value, &'static str> {
        self.next.take().ok_or("The application is gone")
    }
    fn send(&mut self, value: Value) -> Result<(), &'static str> {
        self.sent.push(value);
        Ok(())
    }
}

impl Data for Idle {
    fn open(&mut self, _: bool, _: u64) -> Result<Handle, &'static str> {
        Err("No stream is scripted")
    }
    fn read(&mut self, _: Handle, _: &mut [u8]) -> Result<usize, &'static str> {
        Err("No stream is scripted")
    }
    fn write(&mut self, _: Handle, _: &[u8]) -> Result<usize, &'static str> {
        Err("No stream is scripted")
    }
    fn shutdown(&mut self, _: Handle) -> Result<(), &'static str> {
        Err("No stream is scripted")
    }
    fn close(&mut self, _: Handle) {}
    fn poll(
        &mut self,
        handles: &[Handle],
        timeout_ms: i32,
    ) -> Result<Option<Handle>, &'static str> {
        assert!(handles.is_empty(), "only the idle wait is scripted");
        self.waited.push(timeout_ms);
        match self.waits.pop_front() {
            Some(None) => Ok(None),
            Some(delivery) => {
                self.next = delivery;
                Ok(Some(0))
            }
            None => Err("The script is over"),
        }
    }
}

impl CanvasIo for Idle {
    fn open_canvas(&mut self, _: u64) -> Option<Handle> {
        None
    }
    fn present_canvas(&mut self, _: &[u8]) -> i32 {
        canvas_sdk::PRESENT_OK
    }
}

/// Something the application delivers that leads to nothing here: the answer to a call nobody waits for.
fn unrelated() -> Value {
    result(9_999, json!({"status":"ok","value":null}))
}

/// `seconds` with no page open, in which something is delivered every `every` seconds: the idle waits before each
/// delivery run out, and the one it arrives in is cut short at its very end.
fn idle_with_deliveries(seconds: u64, every: u64) -> VecDeque<Option<Value>> {
    let wait = u64::from(IDLE_WAIT) / 1000;
    assert!(seconds % every == 0 && every % wait == 0);
    let mut waits = VecDeque::new();
    for _ in 0..seconds / every {
        waits.extend((1..every / wait).map(|_| None));
        waits.push_back(Some(unrelated()));
    }
    waits
}

#[test]
fn a_position_says_its_age_only_once_its_device_missed_a_turn() {
    let mut state = State::default();
    state.act(&size(0, 0, 900000, 600000));
    state.view = world::View {
        lon: 25.,
        lat: 45.,
        span: 5.,
        roll: 0.,
    };
    state.listed(&[peer("a1", "Phone", true)]);
    let shared = wire::position(&json!({"latitude":45.0,"longitude":25.0,"age":0})).unwrap();
    let told = state.tell(shared);
    state.seen.insert("a1".to_owned(), told);
    state.own = Own::Found(told);
    // Within its turn, and the minute a turn may take: where it is, and no age anywhere.
    state.now = (LAST_AFTER - 1) * 1000;
    assert_eq!(state.device_lines(), ["Phone: 45.0000° N, 25.0000° E"]);
    assert_eq!(state.own_line(), "This device: 45.0000° N, 25.0000° E");
    let (frame, picture) = art(&state);
    assert!(frame.label("Phone") && frame.labels.iter().all(|l| !l.text.contains("ago")));
    assert!(picture.markers.iter().all(|marker| marker.last.is_none()));
    // Nothing arrived for longer than that: it is the last position known, and how old it is, in the lines and under the
    // name on the map.
    state.now = LAST_AFTER * 1000;
    assert_eq!(
        state.device_lines(),
        ["Phone: 45.0000° N, 25.0000° E, last location 6 min ago"]
    );
    assert_eq!(
        state.own_line(),
        "This device: 45.0000° N, 25.0000° E, last location 6 min ago"
    );
    let (frame, picture) = art(&state);
    assert!(frame.label("Phone") && frame.label("last location 6 min ago"));
    let phone = picture.markers.iter().find(|m| m.id == "a1").unwrap();
    assert_eq!(phone.last.as_deref(), Some("last location 6 min ago"));
    // The map is painted again when what it says of an age changes, and only then.
    let stamp = state.marker_stamp;
    state.act(&json!({"t":"tick","id":"clock","at":1}));
    assert_eq!(state.marker_stamp, stamp + 1);
    state.act(&json!({"t":"tick","id":"clock","at":1_001}));
    assert_eq!(state.marker_stamp, stamp + 1);
    state.act(&json!({"t":"tick","id":"clock","at":30_001}));
    assert_eq!(state.marker_stamp, stamp + 1, "still six minutes");
    state.act(&json!({"t":"tick","id":"clock","at":60_001}));
    assert_eq!(state.marker_stamp, stamp + 2);
    assert_eq!(
        state.device_lines(),
        ["Phone: 45.0000° N, 25.0000° E, last location 7 min ago"]
    );
    // A new position from the device: it is where the device is again.
    let told = state.tell(shared);
    state.seen.insert("a1".to_owned(), told);
    assert_eq!(state.device_lines(), ["Phone: 45.0000° N, 25.0000° E"]);
}

#[test]
fn time_no_wait_measured_is_never_said_as_a_known_age() {
    let shared = wire::position(&json!({"latitude":45.0,"longitude":25.0,"age":0})).unwrap();
    let arrived = |state: &mut State| {
        state.listed(&[peer("a1", "Phone", true)]);
        let told = state.tell(shared);
        state.seen.insert("a1".to_owned(), told);
    };
    // Nine minutes with no page open and something delivered every twenty seconds: every fourth wait is cut short.
    let mut state = State::default();
    arrived(&mut state);
    let mut host = Idle {
        waits: idle_with_deliveries(9 * 60, 20),
        ..Idle::default()
    };
    assert_eq!(serve(&mut host, &mut state), 1);
    assert!(host.waited.iter().all(|wait| *wait == i32::from(IDLE_WAIT)));
    // Three of every four waits ran out: that much surely passed. The rest may have, and is not counted as passed.
    assert_eq!(state.now, 27 * 15_000);
    assert_eq!((state.unsure, state.cut), (27 * 5_000, 0));
    // The listing of an idle guest was not held back by what kept arriving.
    assert_eq!(calls_of(&host.sent, PEERS, "list"), 1);
    // Six and three quarter minutes are sure and nine were possible: the age is the least it can be, and says so.
    assert_eq!(
        state.device_lines(),
        ["Phone: 45.0000° N, 25.0000° E, last location 6+ min ago"]
    );
    // The same nine minutes with nothing delivered: every wait ran out, and the age is known.
    let mut state = State::default();
    arrived(&mut state);
    let mut host = Idle {
        waits: (0..9 * 60 / 5).map(|_| None).collect(),
        ..Idle::default()
    };
    assert_eq!(serve(&mut host, &mut state), 1);
    assert_eq!((state.now, state.unsure, state.cut), (9 * 60_000, 0, 0));
    assert_eq!(
        state.device_lines(),
        ["Phone: 45.0000° N, 25.0000° E, last location 9 min ago"]
    );
    // One wait cut short in seven minutes is far less than the minute the words are said in: no plus for it.
    let mut state = State::default();
    arrived(&mut state);
    let mut waits = idle_with_deliveries(20, 20);
    waits.extend((0..7 * 60 / 5).map(|_| None));
    let mut host = Idle {
        waits,
        ..Idle::default()
    };
    assert_eq!(serve(&mut host, &mut state), 1);
    assert_eq!(state.unsure, 5_000);
    assert_eq!(
        state.device_lines(),
        ["Phone: 45.0000° N, 25.0000° E, last location 7 min ago"]
    );
    // A position that arrives after the doubt began carries none of it.
    let mut state = State::default();
    let mut host = Idle {
        waits: idle_with_deliveries(60, 20),
        ..Idle::default()
    };
    assert_eq!(serve(&mut host, &mut state), 1);
    assert_eq!(state.unsure, 15_000);
    arrived(&mut state);
    let mut host = Idle {
        waits: (0..7 * 60 / 5).map(|_| None).collect(),
        ..Idle::default()
    };
    assert_eq!(serve(&mut host, &mut state), 1);
    assert_eq!(
        state.device_lines(),
        ["Phone: 45.0000° N, 25.0000° E, last location 7 min ago"]
    );
}

#[test]
fn the_time_an_open_page_says_leaves_no_doubt_and_a_fresh_clock_measures_nothing() {
    let mut state = State::default();
    // A page opens and its events say the time, a second apart, each cutting an idle wait short.
    let page = |at: u64| Some(event(at, json!({"t":"tick","id":"clock","at":at})));
    let mut host = Idle {
        waits: (1..=100).map(|second| page(second * 1_000)).collect(),
        ..Idle::default()
    };
    assert_eq!(serve(&mut host, &mut state), 1);
    // Only the wait before the first event was measured by nothing: what went before a page opened is not known.
    assert_eq!(state.now, 99_000);
    assert_eq!((state.unsure, state.cut), (u64::from(IDLE_WAIT), 0));
    // The window is opened again: its clock starts over. Nothing is counted from the difference, and the wait it cut
    // short stays in doubt.
    let mut host = Idle {
        waits: [page(500), page(1_500)].into(),
        ..Idle::default()
    };
    assert_eq!(serve(&mut host, &mut state), 1);
    assert_eq!(state.now, 100_000);
    assert_eq!((state.unsure, state.cut), (2 * u64::from(IDLE_WAIT), 0));
}

#[test]
fn an_idle_guest_lists_the_devices_every_half_minute_it_counted() {
    let mut state = State::default();
    let mut host = Idle {
        waits: (0..3).map(|_| None).collect(),
        ..Idle::default()
    };
    assert_eq!(serve(&mut host, &mut state), 1);
    // At once the first time; nothing more while that listing is not answered.
    assert_eq!(calls_of(&host.sent, PEERS, "list"), 1);
    let list = host.sent[0]["id"].as_u64().unwrap();
    let mut waits: VecDeque<Option<Value>> =
        [Some(result(list, json!({"status":"ok","value":[]})))].into();
    waits.extend((0..3).map(|_| None));
    let mut host = Idle {
        waits,
        ..Idle::default()
    };
    assert_eq!(serve(&mut host, &mut state), 1);
    assert_eq!(
        calls_of(&host.sent, PEERS, "list"),
        0,
        "25 s after the last"
    );
    let mut host = Idle {
        waits: [None].into(),
        ..Idle::default()
    };
    assert_eq!(serve(&mut host, &mut state), 1);
    assert_eq!(
        calls_of(&host.sent, PEERS, "list"),
        1,
        "30 s after the last"
    );
}

/// How many calls of `service` and `method` are among `sent`.
fn calls_of(sent: &[Value], service: &str, method: &str) -> usize {
    sent.iter()
        .filter(|sent| {
            sent["op"] == "call" && sent["service"] == service && sent["method"] == method
        })
        .count()
}

#[test]
fn the_map_and_its_controls_fit_the_contract_with_every_device() {
    // The worst a page is asked to show: this device, every position the extension keeps, and names at their longest.
    let mut state = State {
        own: Own::Found(told(45.0, 25.0)),
        ..State::default()
    };
    for n in 0..MOST_SEEN {
        // Around the world and both sides of the date line, some of them far away.
        let latitude = [
            61.0, -33.0, 35.0, -22.0, 55.0, 1.0, 78.0, -78.0, 0.0, 12.0, -45.0, 66.0,
        ][n % 12];
        let longitude = [
            25.0, -70.0, 139.0, 18.0, -100.0, 179.5, 10.0, -179.5, 0.0, -8.0, 170.0, -20.0,
        ][n % 12];
        let shared = wire::position(&json!({
            "latitude": latitude,
            "longitude": longitude,
            "accuracy": 900.0,
            "age": 60,
        }))
        .unwrap();
        state.seen.insert(
            "d".repeat(ALIAS_MAX - 1) + &n.to_string(),
            Told {
                shared,
                at: 0,
                slack: 0,
            },
        );
    }
    state.listed(
        &(0..MOST_SEEN)
            .map(|n| {
                peer(
                    &("d".repeat(ALIAS_MAX - 1) + &n.to_string()),
                    &"N".repeat(NAME_MAX),
                    true,
                )
            })
            .collect::<Vec<_>>(),
    );
    state.place(&json!({"page_features":["canvas","pinch","rotate"]}));
    state.act(&size(0, 0, 1368000, 970000));
    for (lon, lat, span) in [(179., 60., 360.), (25., 45., 5.), (25., 45., 0.002)] {
        state.view = world::View {
            lon,
            lat,
            span,
            roll: 0.,
        };
        let (art, picture) = art(&state);
        let cam = world::Camera::new(state.view, state.frame());
        for marker in picture.markers {
            assert_eq!(
                Some(marker.point),
                cam.point(marker.fix.longitude, marker.fix.latitude)
            );
            if let Some(badge) = marker.badge {
                assert!(art.labels.iter().any(|l| l.text == marker.name
                    && l.clip.is_some_and(|b| {
                        b.into_iter().zip(badge).all(|(a, b)| (a - b).abs() < 0.01)
                    })));
            }
        }
        let page = state.page();
        assert_eq!(find(&page, "map").unwrap()["k"], "canvas");
        assert!(page.to_string().len() < 2560);
    }
}

/// The first node of an id, from the page down.
fn find<'a>(node: &'a Value, id: &str) -> Option<&'a Value> {
    if node["id"] == id {
        return Some(node);
    }
    node["children"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(node.get("child"))
        .find_map(|child| find(child, id))
}

#[test]
fn a_canvas_host_gets_one_map_surface_and_no_drawings_to_rasterize() {
    let mut state = State::default();
    state.place(&json!({"settings":{},"page_features":["canvas","pinch","rotate"]}));
    let page = state.page();
    let map = find(&page, "map").expect("a Canvas host must get a map");
    assert_eq!(map["k"], "canvas");
    assert!(map.get("children").is_none());
    assert!(map["style"].get("follow").is_none());
    assert!(find(&page, "options").is_some());
}

#[test]
fn the_map_opens_without_a_location_request_and_moves_by_button_wheel_and_finger() {
    let mut state = State {
        own: Own::Found(told(45.0, 25.0)),
        ..State::default()
    };
    state.place(&json!({"page_features":["canvas"]}));
    let calls = state.act(&json!({"t":"open","at":0}));
    assert!(calls.iter().all(|call| call["service"] != LOCATION));
    assert!(find(&state.page(), "share").is_none());
    assert_eq!(state.view, world::View::whole());
    state.act(&json!({"t":"press","id":"centre"}));
    assert_eq!(
        state.view,
        world::View {
            lon: 25.0,
            lat: 45.0,
            span: 5.0,
            roll: 0.,
        }
    );
    assert!(
        art(&state)
            .0
            .paths
            .iter()
            .any(|p| p.mode == 0 && p.color == 6)
    );
    // Closer, wider, and the whole world again, each by its own button.
    state.view.span = 60.;
    state.act(&json!({"t":"press","id":"closer","at":10}));
    assert!((state.view.span - 60.0 / CLOSER).abs() < 1e-9);
    state.act(&json!({"t":"press","id":"wider","at":20}));
    assert!((state.view.span - 60.0).abs() < 1e-9);
    state.act(&json!({"t":"press","id":"world","at":30}));
    assert_eq!(state.view, world::View::whole());
    // Wheel distance accumulates without moving the place under the pointer.
    state.view = world::View {
        lon: 20.0,
        lat: 40.0,
        span: 40.0,
        roll: 0.,
    }
    .kept();
    state.map_at = Some((0.0, 0.0, 300.0, 150.0));
    let under = world::Camera::new(state.view, state.frame())
        .locate(220., 40.)
        .unwrap();
    state.act(&json!({"t":"wheel","id":"map","x":220_000,"y":40_000,"dx":0,"dy":-1000,"at":1_000}));
    assert!(
        (state.view.span - 40.0 * 2_f64.powf(-1. / f64::from(WHEEL_PIXELS_PER_DOUBLING))).abs()
            < 1e-9
    );
    let (x, y) = world::Camera::new(state.view, state.frame())
        .point(under.0, under.1)
        .unwrap();
    assert!((x - 220.0).abs() < 2.0 && (y - 40.0).abs() < 2.0, "{x} {y}");
    let kept = state.view;
    state.act(&json!({"t":"wheel","id":"map","x":220_000,"y":40_000,"dx":0,"dy":-1000,"at":1_050}));
    assert!(
        state.view.span < kept.span,
        "the next small sample must not be discarded"
    );
    state.act(&json!({"t":"wheel","id":"map","x":220_000,"y":40_000,"dx":0,"dy":2000,"at":2_000}));
    assert!((state.view.span - 40.0).abs() < 1e-9, "{}", state.view.span);
    // A finger drags what the map shows, and a place of its own button is what it is centred on.
    state.view = world::View {
        lon: 20.0,
        lat: 40.0,
        span: 40.0,
        roll: 0.,
    }
    .kept();
    state.act(&json!({"t":"pointer","id":"map","phase":"down","x":150_000,"y":75_000,"at":3_000}));
    state.act(&json!({"t":"pointer","id":"map","phase":"move","x":120_000,"y":90_000,"at":3_010}));
    assert!(state.view.lon > 20.0 && state.view.lat > 40.0);
    state.act(&json!({"t":"pointer","id":"map","phase":"up","x":120_000,"y":90_000,"at":3_020}));
    let moved = (state.view.lon, state.view.lat);
    state.act(&json!({"t":"pointer","id":"map","phase":"move","x":20_000,"y":10_000,"at":3_030}));
    assert_eq!(
        (state.view.lon, state.view.lat),
        moved,
        "a lifted finger moves nothing"
    );
    state.act(&json!({"t":"press","id":"centre","at":4_000}));
    assert_eq!((state.view.lon, state.view.lat), (25.0, 45.0));
    let calls = state.act(&json!({"t":"press","id":"options","at":5_000}));
    assert!(
        calls.iter().all(|c| c["method"] == "resource-cache"),
        "Options asks only how much is saved: {calls:?}"
    );
    assert!(find(&state.page(), "share").is_none() && find(&state.page(), "keep").is_none());
    assert!(find(&state.page(), "map").is_some());
}

#[test]
fn tapping_a_picture_centres_once_and_ticks_keep_the_marker() {
    let mut state = State::default();
    state.act(&size(0, 0, 900000, 600000));
    state.listed(&[peer("alias", "Synthetic", true)]);
    state.seen.insert("alias".into(), told(44.43, 26.10));
    state.surface.picture = art(&state).1;
    let point = state.surface.picture.markers[0].point;
    pointer(&mut state, "down", point, 1);
    pointer(&mut state, "up", point, 2);
    state.act(&json!({"t":"tick","at":2+FOCUS_MS}));
    assert!((state.view.lat - 44.43).abs() < 1e-6 && (state.view.lon - 26.10).abs() < 1e-6);
    assert_eq!(state.view.span, 0.006);
    assert!(!atlas::project(state.view, state.frame()).roads.is_empty());
    let (after, picture) = art(&state);
    let marker = &picture.markers[0];
    assert!(after.label("Synthetic"));
    assert!(marker.badge.unwrap()[0] >= marker.dot()[0] + 32.);
    state.act(&json!({"t":"tick","at":1000}));
    assert_eq!(art(&state).1.markers[0].point, marker.point);
}

#[test]
fn devices_on_one_spot_are_drawn_apart_with_names_that_cover_neither() {
    let covers = |[x, y, w, h]: [f64; 4], [a, b, c, d]: [f64; 4]| {
        x < a + c && x + w > a && y < b + d && y + h > b
    };
    let mut state = State::default();
    state.act(&size(0, 0, 900000, 600000));
    state.view = world::View {
        lon: 26.10,
        lat: 44.43,
        span: 0.006,
        roll: 0.,
    };
    // Alone, a marker is where its device is.
    state.listed(&[peer("a1", "Phone", true)]);
    state.seen.insert("a1".into(), told(44.43, 26.10));
    let alone = art(&state).1.markers[0].point;
    // A second device on the same desk: two markers side by side around that spot, each with its name.
    state.listed(&[peer("a1", "Phone", true), peer("b2", "Tablet", true)]);
    state.seen.insert("b2".into(), told(44.43, 26.10));
    let (shown, picture) = art(&state);
    let [first, second] = &picture.markers[..] else {
        panic!("both devices are drawn");
    };
    assert!(!covers(first.dot(), second.dot()));
    assert!((first.point.1 - alone.1).abs() < 1e-6 && (second.point.1 - alone.1).abs() < 1e-6);
    assert!(((first.point.0 + second.point.0) / 2. - alone.0).abs() < 1e-6);
    assert!(shown.label("Phone") && shown.label("Tablet"));
    let (a, b) = (first.badge.unwrap(), second.badge.unwrap());
    assert!(!covers(a, b) && !covers(a, second.dot()) && !covers(b, first.dot()));
    // What each marker is of did not move: pressing one still goes to where the device is.
    assert_eq!(
        (first.fix.latitude, first.fix.longitude),
        (second.fix.latitude, second.fix.longitude)
    );
    // A third on the same spot: none covers another.
    state.listed(&[
        peer("a1", "Phone", true),
        peer("b2", "Tablet", true),
        peer("c3", "Laptop", true),
    ]);
    state.seen.insert("c3".into(), told(44.43, 26.10));
    // A drawn marker is a circle 32 across with its outline.
    let points: Vec<(f64, f64)> = art(&state).1.markers.iter().map(|m| m.point).collect();
    assert_eq!(points.len(), 3);
    for (index, &(x, y)) in points.iter().enumerate() {
        assert!(
            points[index + 1..]
                .iter()
                .all(|&(a, b)| (x - a).hypot(y - b) > 32.)
        );
    }
    // Far enough apart on the screen, each is where its device is again.
    state.seen.clear();
    state.listed(&[peer("a1", "Phone", true), peer("b2", "Tablet", true)]);
    state.seen.insert("a1".into(), told(44.43, 26.10));
    state.seen.insert("b2".into(), told(44.431, 26.10));
    let picture = art(&state).1;
    let near = picture.markers.iter().find(|m| m.id == "a1").unwrap();
    assert!((near.point.0 - alone.0).abs() < 1e-6 && (near.point.1 - alone.1).abs() < 1e-6);
}

#[test]
fn pressing_a_devices_name_on_the_map_takes_the_person_there() {
    let mut state = State::default();
    state.act(&size(0, 0, 900000, 600000));
    state.listed(&[peer("alias", "Synthetic", true)]);
    state.seen.insert("alias".into(), told(44.43, 26.1));
    state.surface.picture = art(&state).1;
    let [x, y, w, h] = state.surface.picture.markers[0].badge.unwrap();
    let p = (x + w / 2., y + h / 2.);
    pointer(&mut state, "down", p, 1);
    pointer(&mut state, "move", (p.0 + 40., p.1), 2);
    pointer(&mut state, "up", (p.0 + 40., p.1), 3);
    assert!(state.focus.is_none());
    pointer(&mut state, "down", p, 4);
    pointer(&mut state, "up", p, 5);
    state.act(&json!({"t":"tick","at":5+FOCUS_MS}));
    assert!((state.view.lat - 44.43).abs() < 1e-6 && (state.view.lon - 26.1).abs() < 1e-6);
}

#[test]
fn a_device_press_moves_through_intermediate_views_to_the_available_detail() {
    for start in [360., world::REGIONAL_SPAN] {
        for (lon, lat, target_span) in [(21.23, 45.75, 0.08), (26.10, 44.43, STREET_FOCUS)] {
            let mut state = State::default();
            if start < 360. {
                state.view = world::View {
                    lon: lon - 0.5,
                    lat: lat - 0.5,
                    span: start,
                    roll: 0.,
                };
            }
            state.act(&size(0, 0, 900000, 600000));
            state.listed(&[peer("alias", "Synthetic", true)]);
            state.seen.insert("alias".to_owned(), told(lat, lon));
            let original = state.view;
            state.surface.picture = art(&state).1;
            let point = state.surface.picture.markers[0].point;
            pointer(&mut state, "down", point, 1);
            pointer(&mut state, "up", point, 2);
            assert_eq!(state.view, original);
            assert_eq!(state.page()["milliseconds"], 100);
            let mut last = original.span;
            for elapsed in (100..=FOCUS_MS).step_by(100) {
                state.act(&json!({"t":"tick","id":"clock","at":2 + elapsed}));
                assert!(state.view.span < last);
                last = state.view.span;
            }
            assert_eq!(state.view.span, target_span);
            let (shown_lon, shown_lat) = state.view.centre();
            assert!((shown_lon - lon).abs() < 1e-10 && (shown_lat - lat).abs() < 1e-10);
            assert!(state.focus.is_none());
        }
    }
}

/// Drive the guest's actual page and stream handlers; marker geometry comes from an accepted frame.
#[derive(Default)]
struct MarkerMap {
    state: State,
    host: Script,
    ticket: u64,
}

impl MarkerMap {
    fn action(&mut self, page_id: &str, input: Value) {
        self.ticket += 1;
        let mut delivery = event(self.ticket, input);
        delivery["args"]["page"] = json!(page_id);
        step(&mut self.host, &mut self.state, &delivery).unwrap();
    }

    fn open(&mut self, page_id: &str, metrics: Value) {
        self.action(page_id, json!({"t":"open"}));
        self.action(page_id, metrics);
        let id = calls(&self.host, canvas_sdk::SERVICE, "open")
            .last()
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        self.answer(id, json!({"status":"ok","value":{"claim":self.ticket}}));
    }

    fn mounted() -> Self {
        let mut map = Self::default();
        map.open(PAGE, size(0, 0, 900_000, 600_000));
        map.tick(1_000);
        map.state.listed(&[peer("phone", "Phone", false)]);
        map
    }

    fn tick(&mut self, at: u64) {
        self.action(PAGE, json!({"t":"tick","id":"clock","at":at}));
    }

    fn answer(&mut self, id: u64, answer: Value) {
        step(&mut self.host, &mut self.state, &result(id, answer)).unwrap();
    }

    fn share(&mut self, lon: f64) {
        self.ticket += 1;
        let mut shared = told(0., lon).shared;
        shared.age = LAST_AFTER;
        self.host.incoming.push_back(wire::encode(&shared).unwrap());
        let delivery = json!({"type":"call","ticket":self.ticket,"method":"receive",
            "service":wire::SERVICE,"args":{"peer":"phone"}});
        step(&mut self.host, &mut self.state, &delivery).unwrap();
    }

    fn marker(&self, id: &str) -> &canvas_scene::Marker {
        self.state
            .surface
            .picture
            .markers
            .iter()
            .find(|marker| marker.id == id)
            .unwrap()
    }

    fn projected(&self, lon: f64) -> (f64, f64) {
        world::Camera::new(self.state.view.kept(), self.state.frame())
            .point(lon, 0.)
            .unwrap()
    }
}

fn badge_tap(state: &State, id: &str, at: u64) -> [Value; 2] {
    let [x, y, w, h] = state
        .surface
        .picture
        .markers
        .iter()
        .find(|marker| marker.id == id)
        .and_then(|marker| marker.badge)
        .expect("the peer's name is drawn");
    let (left, top, _, _) = state.map_at.unwrap();
    let x = ((x + w / 2.) * 1_000. + left) as i32;
    let y = ((y + h / 2.) * 1_000. + top) as i32;
    [("down", at - 1), ("up", at)]
        .map(|(phase, at)| json!({"t":"pointer","id":"map","phase":phase,"x":x,"y":y,"at":at}))
}

#[test]
fn peer_badges_zoom_and_retarget_from_the_visible_camera() {
    for (online, latitude, longitude, span) in [
        (false, 18., 12., 0.08),
        (true, 18., 12., 0.08),
        (false, 44.43, 26.10, STREET_FOCUS),
    ] {
        let other = longitude + 0.001;
        let mut map = MarkerMap::default();
        let config = map.state.ask_settings()["id"].as_u64().unwrap();
        let missing = if online {
            [].as_slice()
        } else {
            [PUBLIC].as_slice()
        };
        map.answer(
            config,
            configuration(
                online,
                missing,
                json!([allowed("a1", true), allowed("b2", true)]),
            ),
        );
        if online {
            let restore = asked(&map.host, online::SERVICE, "restore-read", 0);
            map.answer(restore, json!({"status":"error","code":"unavailable"}));
        }
        map.state
            .listed(&[peer("a1", "One", true), peer("b2", "Two", true)]);
        map.state
            .seen
            .insert("a1".into(), told(latitude, longitude));
        map.state.seen.insert("b2".into(), told(latitude, other));
        map.tick(1_000);
        next_canvas_answers(canvas_sdk::PRESENT_LIMITED);
        map.open(PAGE, size(0, 0, 1_540_000, 830_000));
        map.tick(1_100);
        assert!(Art::parse(&map.host.written).raster[2] < 1_000);

        let shown = |map: &MarkerMap| map.state.surface.painted.unwrap().view;
        let arrived = |map: &MarkerMap, longitude: f64, span: f64| {
            let view = shown(map);
            let (lon, lat) = view.centre();
            assert!((lon - longitude).abs() < 1e-9 && (lat - latitude).abs() < 1e-9);
            assert_eq!(view.span, span);
        };
        let select = |map: &mut MarkerMap, peer: &str, at: u64| {
            for input in badge_tap(&map.state, peer, at) {
                map.action(PAGE, input);
            }
        };

        let world = shown(&map);
        select(&mut map, "a1", 1_200);
        assert_eq!(shown(&map), world, "selection does not jump");
        map.tick(1_400);
        assert!(span < shown(&map).span && shown(&map).span < world.span);
        assert!(world.lon < shown(&map).lon && shown(&map).lon < longitude);
        map.tick(1_200 + FOCUS_MS);
        arrived(&map, longitude, span);
        select(&mut map, "a1", 1_710);
        map.tick(1_750);
        arrived(&map, longitude, span);
        select(&mut map, "b2", 1_800);
        map.tick(1_800 + FOCUS_MS);
        arrived(&map, other, span);

        map.action(PAGE, json!({"t":"press","id":"closer","at":2_400}));
        let closer = shown(&map).span;
        assert!(closer < span);
        select(&mut map, "a1", 2_500);
        map.tick(2_500 + FOCUS_MS);
        arrived(&map, longitude, closer);

        map.action(PAGE, json!({"t":"press","id":"world","at":3_100}));
        select(&mut map, "a1", 3_200);
        // Sample while both badges still fit in the viewport.
        map.tick(3_250);
        let visible = shown(&map);
        next_canvas_answers(canvas_sdk::PRESENT_LIMITED);
        map.tick(3_500);
        let refused = map.state.view;
        assert_eq!(shown(&map), visible);
        assert!(refused.span < visible.span);
        select(&mut map, "b2", 3_510);
        assert_eq!(shown(&map), visible);
        map.tick(3_560);
        let next = shown(&map);
        assert!(
            refused.span < next.span && next.span < visible.span,
            "a new selection starts from the visible span {}, not the refused span {} (got {})",
            visible.span,
            refused.span,
            next.span
        );
        assert!(visible.lon < next.lon && next.lon < refused.lon);
        map.tick(3_510 + FOCUS_MS);
        arrived(&map, other, span);
    }
}

#[test]
fn an_existing_marker_and_its_badge_glide_monotonically_and_then_stop_repainting() {
    let mut map = MarkerMap::mounted();
    map.action(PAGE, json!({"t":"press","id":"options"}));
    map.share(-45.);
    let old = map.projected(-45.);
    let target = map.projected(25.);
    let badge_x = map.marker("phone").badge.unwrap()[0];
    assert_eq!(
        map.marker("phone").point,
        old,
        "first sighting is immediate"
    );
    assert_eq!(map.state.page()["milliseconds"], TICK);
    map.tick(1_100); // Settle the first position's age label before counting glide frames.
    let before = map.state.surface.seq;

    map.share(25.);
    assert_eq!(map.marker("phone").point, old);
    assert!(map.state.page().to_string().contains("25.0000° E"));
    map.tick(2_000);
    assert!(page(&map.host, map.ticket).contains("25.0000° E"));
    assert_eq!(map.marker("phone").point, old);
    assert_eq!(map.state.page()["milliseconds"], 100);

    let mut last = old.0;
    for at in (2_100..3_000).step_by(100) {
        map.tick(at);
        let marker = map.marker("phone");
        assert!(last < marker.point.0 && marker.point.0 < target.0);
        assert_eq!(marker.point.1, old.1);
        let linear_lon = -45. + 70. * (at - 2_000) as f64 / 1_000.;
        if at == 2_200 {
            assert!(marker.fix.longitude < linear_lon, "ease in");
        } else if at == 2_800 {
            assert!(marker.fix.longitude > linear_lon, "ease out");
        }
        let badge = marker.badge.unwrap();
        assert!((badge[0] - badge_x - (marker.point.0 - old.0)).abs() < 1e-9);
        let art = Art::parse(&map.host.written);
        assert!(art.label("Phone") && art.label("last location 6 min ago"));
        last = marker.point.0;
    }
    map.tick(3_000);
    assert_eq!(map.marker("phone").point, target);
    assert_eq!(map.marker("phone").fix, map.state.seen["phone"].shared.fix);
    assert!(map.state.peer_glides.is_empty());
    let seq = map.state.surface.seq;
    assert_eq!(seq - before, 11, "one origin frame and ten moving frames");
    assert_eq!(map.state.page()["milliseconds"], TICK);
    map.tick(3_100);
    assert_eq!(
        map.state.surface.seq, seq,
        "no frame after the accepted endpoint"
    );
    assert_eq!(map.state.last_page.as_ref().unwrap()["milliseconds"], TICK);
}

#[test]
fn retargeting_uses_the_last_presented_position_and_a_refused_endpoint_is_retried() {
    let mut map = MarkerMap::mounted();
    map.share(-45.);
    map.share(25.);
    map.tick(2_000);
    map.tick(2_200);
    let shown = map.marker("phone").point;
    next_canvas_answers(canvas_sdk::PRESENT_LIMITED);
    map.tick(2_500);
    assert_eq!(map.marker("phone").point, shown);

    map.share(-65.);
    assert_eq!(map.state.peer_glides.len(), 1);
    map.tick(2_600);
    assert_eq!(
        map.marker("phone").point,
        shown,
        "a refused sample was never drawn"
    );
    let target = map.projected(-65.);
    map.tick(2_900);
    let halfway = map.marker("phone").point;
    assert!(target.0 < halfway.0 && halfway.0 < shown.0);
    map.share(-65.); // The same destination must not restart a glide.
    next_canvas_answers(canvas_sdk::PRESENT_LIMITED);
    map.tick(3_600);
    assert!(
        map.state.peer_glides.is_empty(),
        "time advances even on refusal"
    );
    assert_eq!(map.marker("phone").point, halfway);
    assert_eq!(map.state.page()["milliseconds"], 100);

    map.tick(3_700);
    assert_eq!(map.marker("phone").point, target);
    assert_eq!(map.state.page()["milliseconds"], TICK);
}

#[test]
fn a_marker_glides_from_its_actual_dot_when_leaving_an_overlapping_group() {
    let mut map = MarkerMap::mounted();
    let keep = map.state.find(Ask::Keep).unwrap();
    map.answer(keep["id"].as_u64().unwrap(), found(0., 0., 0));
    map.share(0.);
    let shared = map.marker("phone").point;
    assert!(
        shared.0 > map.projected(0.).0,
        "the phone is drawn beside this device"
    );

    map.share(25.);
    map.tick(2_000);
    assert_eq!(map.marker("phone").point, shared);
    map.tick(2_500);
    assert!(shared.0 < map.marker("phone").point.0);
    assert!(map.marker("phone").point.0 < map.projected(25.).0);
    map.tick(3_000);
    assert_eq!(map.marker("phone").point, map.projected(25.));
}

#[test]
fn a_glide_takes_the_short_turn_across_the_date_line() {
    let mut map = MarkerMap::mounted();
    map.state.view.lon = 180.;
    map.share(179.);
    map.share(-179.);
    map.tick(2_000);
    assert_eq!(map.marker("phone").point, map.projected(179.));
    map.tick(2_500);
    assert_eq!(map.marker("phone").point, map.projected(180.));
    map.tick(3_000);
    assert_eq!(map.marker("phone").point, map.projected(-179.));
}

#[test]
fn forgetting_a_device_cancels_its_glide_and_its_next_sighting_is_immediate() {
    for removal in ["forget", "unpair", "denied"] {
        let mut map = MarkerMap::mounted();
        map.share(-45.);
        map.share(25.);
        map.tick(2_000);
        map.tick(2_300);
        assert!(map.state.peer_glides.contains_key("phone"));
        if removal == "forget" {
            map.action(PAGE, json!({"t":"press","id":"forget"}));
        } else {
            let result = if removal == "unpair" {
                json!({"status":"ok","value":[]})
            } else {
                json!({"status":"error","code":"denied"})
            };
            let id = list_id(&mut map.state);
            map.answer(id, result);
        }
        assert!(map.state.seen.is_empty() && map.state.peer_glides.is_empty());
        map.tick(2_400);
        assert!(map.state.surface.picture.markers.is_empty());
        map.state.listed(&[peer("phone", "Phone", false)]);
        map.share(-35.);
        assert_eq!(map.marker("phone").point, map.projected(-35.));
    }
}

#[test]
fn opening_a_map_shows_current_fixes_without_replaying_another_pages_glide() {
    let mut map = MarkerMap::mounted();
    map.share(-45.);
    map.share(25.);
    map.tick(2_000);
    map.tick(2_300);
    let moving = map.marker("phone").point;
    map.open(PAGE_SETTING, size(0, 0, 900_000, 600_000));
    assert_eq!(map.marker("phone").point, map.projected(25.));
    map.tick(2_500);
    let still_gliding = map.marker("phone").point;
    assert!(moving.0 < still_gliding.0 && still_gliding.0 < map.projected(25.).0);
    map.open(PAGE, size(0, 0, 900_000, 600_000));
    assert_eq!(map.marker("phone").point, map.projected(25.));
    assert!(map.state.peer_glides.is_empty());
}

#[test]
fn a_closed_map_keeps_new_fixes_without_animating_or_asking_for_canvas_work() {
    let mut map = MarkerMap::default();
    map.share(-45.);
    map.share(25.);
    assert!(map.state.peer_glides.is_empty());
    assert!(calls(&map.host, canvas_sdk::SERVICE, "open").is_empty());
    map.open(PAGE, size(0, 0, 900_000, 600_000));
    assert_eq!(map.marker("phone").point, map.projected(25.));
    map.tick(1_000);
    map.share(-45.);
    map.tick(2_000);
    assert!(map.state.peer_glides.contains_key("phone"));
    map.state.waited();
    assert!(map.state.peer_glides.is_empty());
    map.share(-65.);
    assert!(map.state.peer_glides.is_empty());
    map.open(PAGE, size(0, 0, 900_000, 600_000));
    assert_eq!(map.marker("phone").point, map.projected(-65.));
}

#[test]
fn a_glide_uses_the_same_page_and_idle_clocks_as_a_still_map() {
    let mut still = MarkerMap::mounted();
    let mut moving = MarkerMap::mounted();
    moving.share(-45.);
    for map in [&mut still, &mut moving] {
        map.share(25.);
        map.tick(2_000);
    }
    assert!(still.state.peer_glides.is_empty());
    assert!(moving.state.peer_glides.contains_key("phone"));

    let clock = |state: &State| (state.now, state.cut, state.unsure, state.ui_time);
    let tick = |at| Some(event(at, json!({"t":"tick","id":"clock","at":at})));
    for next in [
        Some(unrelated()),
        tick(2_300),
        Some(unrelated()),
        tick(5), // A new page-clock epoch cannot measure what the old one left in doubt.
        tick(705),
        None,
    ] {
        for (map, next) in [(&mut still, next.clone()), (&mut moving, next)] {
            let mut host = Idle {
                waits: [next].into(),
                ..Idle::default()
            };
            assert_eq!(serve(&mut host, &mut map.state), 1);
            assert!(host.waited.iter().all(|&ms| ms == i32::from(IDLE_WAIT)));
        }
        assert_eq!(clock(&moving.state), clock(&still.state));
    }
    assert_eq!(clock(&moving.state), (7_000, 0, 10_000, None));
    assert!(moving.state.peer_glides.is_empty());
}

#[test]
fn local_fixes_glide_and_only_an_explicit_locate_moves_the_camera() {
    let mut map = MarkerMap::mounted();
    map.action(PAGE, json!({"t":"press","id":"options"}));
    let locate = |map: &mut MarkerMap, lon: f64| {
        map.action(PAGE, json!({"t":"press","id":"find"}));
        let id = map.state.asked_own;
        map.answer(id, found(0., lon, 0));
    };
    let own = canvas_scene::OWN;
    locate(&mut map, 0.);
    assert_eq!(map.marker(own).point, map.projected(0.));
    let camera = map.state.view;
    let keep = map.state.find(Ask::Keep).unwrap();
    map.answer(keep["id"].as_u64().unwrap(), found(0., 0.5, 0));
    assert!(map.state.own_line().contains("0.5000° E"));
    map.tick(2_000);
    assert!(page(&map.host, map.ticket).contains("0.5000° E"));
    map.tick(2_500);
    assert_eq!(map.state.view, camera);
    assert!(map.projected(0.).0 < map.marker(own).point.0);
    assert!(map.marker(own).point.0 < map.projected(0.5).0);
    map.tick(3_000);
    assert_eq!(map.marker(own).point, map.projected(0.5));

    // Re-centre on the known position before testing a subsequent, explicit lookup.
    map.action(PAGE, json!({"t":"press","id":"centre"}));
    let old = map.state.view;
    let centre = map.marker(own).point;
    locate(&mut map, 1.);
    assert_eq!(map.state.view, old, "no camera jump when the fix arrives");
    map.tick(4_000);
    map.tick(4_500);
    assert!(old.lon < map.state.view.lon && map.state.view.lon < 1.);
    assert_eq!(
        map.marker(own).point,
        centre,
        "the camera and marker move together"
    );
    map.tick(5_000);
    assert_eq!(map.state.view.centre(), (1., 0.));
    assert_eq!(map.marker(own).point, centre);
    assert!(map.state.own_glide.is_none());
}

#[test]
fn large_rasters_pace_glide_samples_without_losing_the_endpoint() {
    let mut map = MarkerMap::default();
    map.open(PAGE, size(0, 0, 3_000_000, 2_400_000));
    map.tick(1_000);
    map.share(-45.);
    map.tick(1_100);
    let before = map.state.surface.seq;
    map.share(25.);
    map.tick(2_000);
    let seq = map.state.surface.seq;
    map.tick(2_100);
    assert_eq!(
        map.state.surface.seq, seq,
        "28.8 MB per frame needs more than 100 ms"
    );
    map.tick(2_200);
    assert_eq!(map.state.surface.seq, seq + 1);
    for at in (2_300..=3_000).step_by(100) {
        map.tick(at);
    }
    assert_eq!(map.marker("phone").point, map.projected(25.));
    assert_eq!(
        map.state.surface.seq - before,
        6,
        "one origin frame and five moving frames"
    );
}

#[test]
fn the_device_name_flips_to_the_left_near_the_right_edge() {
    let frame = world::Frame(320, 420);
    let (x, y) = (255., 180.);
    let [left, _, width, _] =
        marker_badge_bounds("Remote device", None, x, y, map_label_bounds(frame), |_| {
            true
        })
        .unwrap();
    assert!(left >= 0. && left + width < x - 16.);
}

#[test]
fn place_labels_do_not_cover_a_device_avatar_or_its_name() {
    let mut state = State::default();
    state.act(&size(0, 0, 900000, 600000));
    state.view = world::View {
        lon: 26.1,
        lat: 44.43,
        span: 9.,
        roll: 0.,
    };
    assert!(art(&state).0.label("Bucharest"));
    state.listed(&[peer("alias", "Test device", true)]);
    state.seen.insert("alias".into(), told(44.43, 26.1));
    let (frame, picture) = art(&state);
    assert!(!frame.label("Bucharest") && frame.label("Test device"));
    let marker = &picture.markers[0];
    for l in frame
        .labels
        .iter()
        .filter(|l| l.text != "Test device" && l.size != 12)
    {
        if let Some(bounds) = l.clip {
            assert!(!overlaps(bounds, marker.dot()));
            assert!(!overlaps(bounds, marker.badge.unwrap()));
        }
    }
}

#[test]
fn zoom_rail_anchors_to_the_measured_right_edge_and_contains_the_controls() {
    let mut state = State::default();
    state.act(&size(25000, 100000, 640000, 500000));
    state.view.span = 0.01;
    let (frame, _) = art(&state);
    let rail = ZoomRail::new(state.frame()).unwrap();
    assert_eq!(rail.left() + rail.width + ZoomRail::EDGE, 640);
    assert!(frame.label("+") && frame.label("−") && frame.label("348 m"));
    pointer(&mut state, "down", (615., 25.), 1);
    pointer(&mut state, "up", (615., 25.), 2);
    assert_eq!(state.view.span, 0.01 / CLOSER);
    pointer(&mut state, "down", (615., 475.), 3);
    pointer(&mut state, "up", (615., 475.), 4);
    assert!((state.view.span - 0.01).abs() < 1e-10);
}

#[test]
fn the_ground_scale_fits_its_backing_and_measures_a_round_distance() {
    for (width, height, span, elevation, want, metres, want_length) in [
        (620, 970, 20., "697 km", "200 km", 200_000., 154.14),
        (1616, 930, 20., "697 km", "200 km", 200_000., 231.22),
        (
            1616,
            930,
            20. * 930. / 1616.,
            "401 km",
            "100 km",
            100_000.,
            200.88,
        ),
        (640, 500, 0.01, "348 m", "100 m", 100., 124.32),
        (164, 140, 0.05, "1.7 km", "1 km", 1_000., 69.62),
        (300, 150, 360., "12 540 km", "5000 km", 5_000_000., 50.48),
    ] {
        let mut state = State::default();
        state.act(&size(25000, 100000, width * 1000, height * 1000));
        state.view = world::View {
            lon: 25.,
            lat: 46.,
            span,
            roll: 34.,
        };
        let (frame, _) = art(&state);
        let viewport = state.frame();
        let camera = world::Camera::new(state.view, viewport);
        let bar = ScaleBar::new(viewport, &camera).unwrap();
        let [x, y, w, h] = bar.bounds();
        let rail_x = f64::from(ZoomRail::new(viewport).unwrap().left());
        assert!(
            x >= 0. && x + w < rail_x && y > f64::from(height) - 64. && y + h < f64::from(height)
        );
        assert!(frame.label(want) && frame.labels.iter().any(|l| l.text.contains(elevation)));
        let ruler = frame
            .paths
            .iter()
            .find(|p| p.mode == 1 && p.color == 4 && p.width == 1.4 && p.points.len() == 6)
            .unwrap();
        assert_eq!(ruler.verbs, 6, "ruler has two end ticks and a centre tick");
        let length = ruler.points[2][0] - ruler.points[0][0];
        assert!(
            (length - want_length).abs() <= 0.02,
            "{width}×{height}: {length}"
        );
        let r = 0.44 * f64::from(width.min(height)) * 360. / span;
        let ground = 2. * 6_371_008.8 * (length / (2. * r)).asin();
        assert!((ground / metres - 1.).abs() < 0.0002, "{ground} m");
        eprintln!(
            "{width}×{height}, span {span:.3}°: elevation {elevation}; {want} scale is {length:.2} logical px"
        );
    }
}

#[test]
fn a_map_with_no_measured_room_for_a_scale_does_not_emit_a_cropped_bar() {
    let mut state = State::default();
    state.act(&size(0, 0, 130000, 970000));
    let (frame, _) = art(&state);
    assert!(!frame.paths.iter().any(|p| p.mode == 1 && p.width == 1.4));
}

#[test]
fn zoom_rail_drag_spans_the_zoom_range_at_a_fixed_camera_centre() {
    let mut state = State::default();
    state.act(&size(25000, 100000, 640000, 500000));
    state.view = world::View {
        span: 5.,
        lat: 45.,
        lon: 21.,
        roll: 0.,
    };
    let centre = state.view.centre();
    let drag = |state: &mut State, phase, y| {
        state.act(&json!({"t":"pointer","id":"map","phase":phase,"x":640000,"y":y}));
    };
    drag(&mut state, "move", 149_000);
    assert_eq!(state.view.span, 5., "motion before a press is not a drag");
    drag(&mut state, "down", 149_000);
    assert_eq!(state.view.span, 0.002);
    drag(&mut state, "move", 555_000);
    assert_eq!(state.view.span, 360.);
    assert_eq!(state.view.centre(), centre);
    drag(&mut state, "cancel", 0);
    drag(&mut state, "move", 149_000);
    assert_eq!(state.view.span, 360., "cancel must end the drag");
}

#[test]
fn a_reopened_window_renews_the_page_clock_without_stalling_the_worker() {
    let mut state = State::default();
    for (at, elapsed) in [(1_000_000, 0), (1_001_000, 1000), (100, 1000), (350, 1250)] {
        state.act(&json!({"t":"tick","at":at}));
        assert_eq!(state.now, elapsed);
    }
}

#[test]
fn the_same_geographic_point_stays_under_the_pointer_on_the_globe_and_as_it_zooms() {
    let frame = world::Frame(920, 600);
    for span in [360., 240., 150., 90., 35.] {
        let view = world::View {
            lon: 175.,
            lat: 42.,
            span,
            roll: 0.,
        };
        let cam = world::Camera::new(view, frame);
        let target = (-179., 45.);
        let pixel = cam.point(target.0, target.1).unwrap();
        let place = cam.locate(pixel.0, pixel.1).unwrap();
        let next = world::Camera::anchor(view.zoomed(1.35, None), frame, place, pixel);
        let actual = world::Camera::new(next, frame)
            .point(target.0, target.1)
            .unwrap();
        assert!(
            (actual.0 - pixel.0).hypot(actual.1 - pixel.1) < 0.2,
            "span {span}"
        );
    }
}

#[test]
fn regional_roads_and_labels_are_not_drawn_on_the_far_globe() {
    let frame = world::Frame(920, 600);
    let region = atlas::project(
        world::View {
            lon: 26.1,
            lat: 44.43,
            span: 9.,
            roll: 0.,
        },
        frame,
    );
    assert!(
        region.roads.is_empty(),
        "no disconnected roads at country scale"
    );
    assert!(region.names.iter().any(|(_, _, n)| n.contains("Bucharest")));
    let west = world::View {
        lon: 21.23,
        lat: 45.75,
        span: world::REGIONAL_SPAN,
        roll: 0.,
    };
    let regional = atlas::project(west, frame);
    assert!(!regional.roads.is_empty() && !regional.secondary_roads.is_empty());
    assert!(
        atlas::project(world::View { span: 9., ..west }, frame)
            .secondary_roads
            .is_empty()
    );
    let far = atlas::project(world::View::whole(), frame);
    assert!(far.roads.is_empty() && !far.countries.is_empty());
    assert!(!far.names.iter().any(|(_, _, n)| *n == "Bucharest"));
    for (lon, lat) in [(26.100, 44.433), (7.425, 43.739)] {
        let view = world::View {
            lon,
            lat,
            span: 0.012,
            roll: 0.,
        };
        let detail = atlas::project(view, frame);
        assert!(
            !detail.roads.is_empty() && !detail.names.is_empty(),
            "{lon}, {lat}"
        );
        assert!(detail.roads.len() <= 10500);
    }
    // Rotating over a pole still shows the same city and permits street-level zoom.
    let inverted = world::View {
        lon: -153.9,
        lat: 135.57,
        span: world::REGIONAL_SPAN,
        roll: 0.,
    };
    let regional = atlas::project(inverted, frame);
    assert!(!regional.roads.is_empty());
    assert!(
        regional
            .names
            .iter()
            .any(|(_, _, name)| name.contains("Bucharest"))
    );
    let street = inverted.zoomed(600., None);
    assert!(street.span < 0.02);
    assert!(!atlas::project(street, frame).roads.is_empty());
}

#[test]
fn optional_touch_events_are_only_requested_from_hosts_that_advertise_them() {
    let mut state = State::default();
    for (features, observations) in [
        (json!(["canvas"]), json!(["wheel", "size"])),
        (
            json!(["canvas", "pinch"]),
            json!(["wheel", "size", "pinch"]),
        ),
        (
            json!(["canvas", "rotate"]),
            json!(["wheel", "size", "rotate"]),
        ),
        (
            json!(["canvas", "pinch", "rotate"]),
            json!(["wheel", "size", "pinch", "rotate"]),
        ),
    ] {
        state.place(&json!({"settings": {}, "page_features": features}));
        let page = state.page();
        assert_eq!(find(&page, "map").unwrap()["observe"], observations);
    }
}

#[test]
fn a_globe_drag_rotates_the_sphere_under_the_finger_without_flat_follow() {
    for span in [360., 20.] {
        let mut state = State::default();
        state.place(&json!({"settings":{},"page_features":["canvas","pinch"]}));
        state.act(&size(0, 0, 620000, 970000));
        state.view.span = span;
        let grab = world::Camera::new(state.view, state.frame())
            .locate(350., 600.)
            .unwrap();
        let pointer = |phase, x, y, at| {
            json!({"t":"pointer","id":"map","phase":phase,
            "x":x,"y":y,"at":at})
        };
        state.act(&pointer("down", 350000, 600000, 1));
        state.act(&pointer("move", 375000, 505000, 2));
        let projected = world::Camera::new(state.view, state.frame())
            .point(grab.0, grab.1)
            .unwrap();
        assert!(
            (projected.0 - 375.).hypot(projected.1 - 505.) < 0.02,
            "{projected:?}"
        );
        let page = state.page();
        assert!(find(&page, "map").unwrap()["style"].get("follow").is_none());
    }
}

#[test]
fn two_finger_twist_pivots_locally_and_leaves_the_globe_centre_fixed() {
    for span in [0.05, 360.] {
        let mut state = State::default();
        state.place(&json!({"settings":{},"page_features":["canvas","rotate","pinch"]}));
        state.act(&size(0, 0, 800000, 700000));
        state.view.span = span;
        let (x, y) = if span < 1. {
            (480., 290.)
        } else {
            (400., 350.)
        };
        let camera = world::Camera::new(state.view, state.frame());
        let pivot = camera.locate(x, y).unwrap();
        let east = camera.locate(x + 50., y).unwrap();
        let twist = |begin, turn| {
            json!({"t":"rotate","id":"map","begin":begin,
            "x":480000,"y":290000,"turn":turn,"at":if begin { 4 } else { 6 }})
        };
        state.act(&twist(true, 0));
        state.act(&twist(false, 9000));
        let camera = world::Camera::new(state.view, state.frame());
        let anchor = camera.point(pivot.0, pivot.1).unwrap();
        assert!(
            (anchor.0 - x).hypot(anchor.1 - y) < 0.02,
            "span {span}: {anchor:?}"
        );
        let projected = camera.point(east.0, east.1).unwrap();
        assert!(
            (projected.0 - x).hypot(projected.1 - y - 50.) < 0.02,
            "span {span}: {projected:?}"
        );
        let page = state.page();
        assert!(
            find(&page, "map").unwrap()["observe"]
                .as_array()
                .unwrap()
                .contains(&json!("rotate"))
        );
        if span < 1. {
            state.act(&json!({"t":"pinch","id":"map","begin":false,
                "x":480000,"y":290000,"scale":20000,"at":8}));
            assert_eq!(
                state.view.roll, 90.,
                "a scale sample must retain the absolute twist"
            );
            assert_eq!(state.view.span, span / 2.);
        }
        for phase in ["down", "up"] {
            state.act(&json!({"t":"pointer","id":"map","phase":phase,"x":756000,"y":350000}));
        }
        let cam = world::Camera::new(state.view, state.frame());
        let (lon, lat) = state.view.centre();
        let north = cam.point(lon, lat + span.min(1.) / 10.).unwrap();
        assert!((north.0 - 400.).abs() < 0.02 && north.1 < 350.);
    }
}

#[test]
fn several_ranked_romanian_cities_fit_on_a_country_scale_phone() {
    let mut state = State::default();
    state.act(&size(0, 0, 620000, 970000));
    state.view = world::View {
        lon: 25.,
        lat: 46.,
        span: 20.,
        roll: 0.,
    };
    let (frame, _) = art(&state);
    let names = [
        "Bucharest",
        "Cluj-Napoca",
        "Timișoara",
        "Iași",
        "Constanța",
        "Brașov",
        "Craiova",
    ];
    let cities: Vec<_> = frame
        .labels
        .iter()
        .filter(|l| names.contains(&l.text.as_str()))
        .collect();
    assert!(cities.len() >= 5 && frame.label("Bucharest"), "{cities:?}");
    let bar = ScaleBar::new(
        state.frame(),
        &world::Camera::new(state.view, state.frame()),
    )
    .unwrap()
    .bounds();
    for (i, label) in cities.iter().enumerate() {
        let area @ [x, y, w, h] = label.clip.unwrap();
        assert!(x >= 8. && y >= 8. && x + w <= 568. && y + h <= 962.);
        let rail = ZoomRail::new(state.frame()).unwrap();
        let elevation = rail.elevation(state.view, &world::Camera::new(state.view, state.frame()));
        assert!(!overlaps(area, elevation.bounds) && !overlaps(area, bar));
        assert!(cities[..i].iter().all(|l| !overlaps(area, l.clip.unwrap())));
    }
}

#[test]
fn the_country_labels_fit_at_world_and_country_scale() {
    let mut state = State {
        camera: Viewport {
            map_at: Some((0., 0., 1368., 970.)),
            ..Viewport::default()
        },
        ..State::default()
    };
    state.place(&json!({"page_features":["text_size"]}));
    let mut world_france = None;
    for (lon, lat, span, minimum) in [
        (21., 45., 360., 45),
        (12., 48., 72., 24),
        (-90., 47., 360., 28),
    ] {
        state.view = world::View {
            lon,
            lat,
            span,
            roll: 0.,
        };
        let (frame, _) = art(&state);
        let mut aired = Vec::new();
        let rail = ZoomRail::new(state.frame()).unwrap();
        for l in &frame.labels {
            let Some(area @ [x, y, w, h]) = l.clip else {
                continue;
            };
            if x >= f64::from(rail.left()) {
                continue;
            }
            assert!(x >= 0. && y >= 0. && x + w <= 1368. && y + h <= 970.);
            assert!(
                aired.iter().all(|&(_, _, bounds)| !overlaps(area, bounds)),
                "{} overlapped at {span}",
                l.text
            );
            aired.push((l.text.as_str(), l.size, area));
        }
        let size = |wanted| {
            aired
                .iter()
                .find(|&&(name, _, _)| name == wanted)
                .map(|entry| entry.1)
                .unwrap_or_else(|| panic!("missing {wanted} at span {span}"))
        };
        let smallest = aired.iter().map(|entry| entry.1).min().unwrap_or(0);
        let largest = aired.iter().map(|entry| entry.1).max().unwrap_or(0);
        eprintln!(
            "country labels at ({lon},{lat}) span {span}: {}, {smallest}–{largest} px",
            aired.len()
        );
        assert!(aired.len() >= minimum, "only {aired:?}");
        assert!(largest >= smallest + 8);
        assert!(aired.windows(2).all(|pair| pair[0].1 >= pair[1].1));
        if lon == 21. {
            assert!(size("Russia") > size("France"));
            world_france = Some(size("France"));
        } else if lon == 12. {
            assert!(size("France") > size("Switzerland"));
            assert!(size("France") > world_france.unwrap());
        } else {
            assert!(size("Canada") > size("Cuba"));
        }
    }
}

#[test]
fn national_land_boundaries_do_not_disappear_at_the_continental_zoom_cutoff() {
    let frame = world::Frame(1368, 970);
    for (lon, lat) in [(12., 48.), (-96., 47.), (22., 8.)] {
        for span in [360., 160., 90., 61., 59., 24.] {
            let view = world::View {
                lon,
                lat,
                span,
                roll: 0.,
            };
            let path = atlas::project(view, frame).borders;
            assert!(
                path.len() >= 8,
                "missing land boundaries at ({lon},{lat}), span {span}"
            );
        }
    }
    let view = world::View {
        lon: 7.8,
        lat: 48.59,
        span: 0.3,
        roll: 0.,
    };
    assert!(!atlas::project(view, frame).borders.is_empty());
    let ocean = world::View {
        lon: -150.,
        lat: -35.,
        span: 24.,
        roll: 0.,
    };
    assert!(atlas::project(ocean, frame).borders.is_empty());
}

#[test]
fn dragging_past_both_poles_twice_returns_the_same_visible_hemisphere() {
    let mut state = State::default();
    let frame = world::Frame(920, 600);
    state.map_at = Some((0., 0., 920., 600.));
    let radius = 264.;
    let step = radius * 10_f64.to_radians().sin() * 1000.;
    for i in 1..=72 {
        state.act(&json!({"t":"pointer","id":"map","phase":"down","x":460000,"y":300000}));
        state.act(&json!({"t":"pointer","id":"map","phase":"move","x":460000,"y":300000.+step}));
        let cam = world::Camera::new(state.view, frame);
        let p = cam.locate(460., 300.).unwrap();
        let angle = (f64::from(i) * 10.).to_radians();
        assert!((p.1.to_radians().sin() - angle.sin()).abs() < 1e-6);
        assert!((p.1.to_radians().cos() * p.0.to_radians().cos() - angle.cos()).abs() < 1e-6);
    }
}

#[test]
fn dragging_over_a_pole_at_any_scale_never_stands_still() {
    for span in [40., 6., 0.5] {
        let mut state = State {
            camera: Viewport {
                map_at: Some((0., 0., 920., 600.)),
                ..Viewport::default()
            },
            ..State::default()
        };
        state.view = world::View {
            lon: 10.,
            lat: 90. - span / 4.,
            span,
            roll: 0.,
        };
        state.act(&json!({"t":"pointer","id":"map","phase":"down","x":460_000,"y":300_000}));
        let mut passed = false;
        for step in 1..=400 {
            let before = state.view;
            let y = 320_000;
            state.act(&json!({"t":"pointer","id":"map","phase":"move","x":460_000,"y":y}));
            assert_ne!(
                state.view, before,
                "a drag stood still at span {span}, move {step}"
            );
            passed |= (state.view.centre().0 - 10.).to_radians().cos() < 0.;
            // Begin again where it is, as a finger lifted and put down does.
            state.act(&json!({"t":"pointer","id":"map","phase":"up","x":460_000,"y":y}));
            state.act(&json!({"t":"pointer","id":"map","phase":"down","x":460_000,"y":300_000}));
        }
        assert!(passed, "the drag never went over the pole at span {span}");
    }
}

#[test]
fn mounted_settings_cannot_move_or_resize_the_primary_map() {
    let mut state = State::default();
    state.place(&json!({"page_features":["canvas"]}));
    let mut host = Script::default();
    let on = |host: &mut Script, state: &mut State, page, ticket, e| {
        let mut d = event(ticket, e);
        d["args"]["page"] = json!(page);
        step(host, state, &d).unwrap();
    };
    on(&mut host, &mut state, PAGE_SETTING, 1, json!({"t":"open"}));
    on(
        &mut host,
        &mut state,
        PAGE_SETTING,
        2,
        size(0, 0, 400000, 760000),
    );
    let primary = asked(&host, canvas_sdk::SERVICE, "open", 0);
    on(&mut host, &mut state, PAGE, 3, json!({"t":"open"}));
    on(&mut host, &mut state, PAGE, 4, size(0, 0, 220000, 360000));
    let settings = asked(&host, canvas_sdk::SERVICE, "open", 1);
    assert_eq!(
        calls(&host, canvas_sdk::SERVICE, "open")[0]["args"]["page"],
        PAGE_SETTING
    );
    assert_eq!(
        calls(&host, canvas_sdk::SERVICE, "open")[1]["args"]["page"],
        PAGE
    );
    step(
        &mut host,
        &mut state,
        &result(primary, json!({"status":"ok","value":{"claim":11}})),
    )
    .unwrap();
    assert_eq!(Art::parse(&host.written).raster, [400, 760, 1000]);
    on(
        &mut host,
        &mut state,
        PAGE_SETTING,
        5,
        json!({"t":"wheel","id":"map","x":200000,"y":380000,"dx":0,"dy":-20000,"at":20}),
    );
    let primary = Art::parse(&host.written);
    assert_eq!(primary.settled, 20);
    step(
        &mut host,
        &mut state,
        &result(settings, json!({"status":"ok","value":{"claim":12}})),
    )
    .unwrap();
    let settings = Art::parse(&host.written);
    assert_eq!(settings.raster, [220, 360, 1000]);
    assert_eq!(settings.settled, 0);
    on(
        &mut host,
        &mut state,
        PAGE_SETTING,
        6,
        json!({"t":"tick","id":"clock","at":23}),
    );
    assert!(state.primary_surface);
    assert_eq!(state.surface.seq, primary.seq);
    assert_eq!(state.settled, 20);
}

#[test]
fn fingers_spread_and_close_on_the_same_globe() {
    let mut state = State {
        camera: Viewport {
            map_at: Some((0., 0., 400., 760.)),
            ..Viewport::default()
        },
        ..State::default()
    };
    let initial = state.view;
    let event = |scale, begin| json!({"t":"pinch","id":"map","x":200000,"y":380000,"scale":scale,"begin":begin});
    state.act(&event(10000, true));
    state.act(&event(20000, false));
    assert!((state.view.span - initial.span / 2.).abs() < 1e-8);
    state.act(&event(10000, false));
    assert!((state.view.span - initial.span).abs() < 1e-8);
}

#[test]
fn unapproved_online_streets_never_prompt_when_the_map_zooms() {
    let mut state = State::default();
    let id = state.ask_settings()["id"].as_u64().unwrap();
    let calls = state.told(&result(
        id,
        json!({"status":"ok","value":{"settings":{"online-streets":true}}}),
    ));
    let restore = calls
        .iter()
        .find(|call| call["method"] == "restore-read")
        .unwrap();
    state.told(&result(
        restore["id"].as_u64().unwrap(),
        json!({"status":"error","code":"unavailable"}),
    ));
    state.view.span = world::REGIONAL_SPAN;
    for _ in 0..4 {
        let calls = state.act(&json!({"t":"press","id":"closer"}));
        assert!(calls.iter().all(|c| c["service"] != online::SERVICE));
    }
}

#[test]
fn online_streets_load_around_a_device_once_the_person_turned_them_on() {
    let mut state = State::default();
    let presses = |id| json!({"t":"press","id":id});
    state.online_enabled = true;
    state.view.span = world::REGIONAL_SPAN;
    let restore = state.online.restore();
    let call = state.online_call(restore, true);
    state.told(&result(
        call["id"].as_u64().unwrap(),
        json!({"status":"ok","value":{"authorization":"test-resource"}}),
    ));
    state.own = Own::Found(told(52.52, 13.405));
    let calls = state.act(&presses("centre"));
    let gets: Vec<_> = calls
        .iter()
        .filter(|c| c["method"] == "get-resource")
        .collect();
    assert_eq!(
        gets.len(),
        1,
        "centring on the device asks for its area at once"
    );
    assert_eq!(gets[0]["args"]["path"], "/planet");
}

#[test]
fn options_say_how_much_is_saved_and_clear_removes_it() {
    let mut state = State::default();
    let calls = state.act(&json!({"t":"press","id":"options"}));
    let asked = calls
        .iter()
        .find(|c| c["method"] == "resource-cache")
        .unwrap();
    assert_eq!(asked["service"], online::SERVICE);
    state.told(&result(
        asked["id"].as_u64().unwrap(),
        json!({"status":"ok","value":{"resources":3,"bytes":3_250_000,"most_bytes":134_217_728}}),
    ));
    let page = state.page().to_string();
    assert!(
        page.contains("Map areas saved on this device: 3 (3.1 MB)"),
        "{page}"
    );
    assert!(page.contains(r#""id":"clear-kept""#));
    let calls = state.act(&json!({"t":"press","id":"clear-kept"}));
    let cleared = calls
        .iter()
        .find(|c| c["method"] == "clear-resource-cache")
        .unwrap();
    state.told(&result(
        cleared["id"].as_u64().unwrap(),
        json!({"status":"ok","value":{"resources":0,"bytes":0,"most_bytes":134_217_728}}),
    ));
    let page = state.page().to_string();
    assert!(
        page.contains("Map areas saved on this device: 0 (0 KB)"),
        "{page}"
    );
    assert!(
        !page.contains(r#""id":"clear-kept""#),
        "nothing left to clear"
    );
    // Closing Options asks nothing.
    assert!(state.act(&json!({"t":"press","id":"options"})).is_empty());
}

#[test]
fn small_wheel_samples_keep_zooming_below_regional_scale() {
    let mut state = State {
        camera: Viewport {
            map_at: Some((0., 0., 888., 449.)),
            ..Viewport::default()
        },
        ..State::default()
    };
    state.view = world::View {
        lon: 20.,
        lat: 45.,
        span: world::REGIONAL_SPAN,
        roll: 0.,
    };
    let before = state.view.span;
    let wheel =
        |at, dy| json!({"t":"wheel","id":"map","x":444000,"y":224500,"dx":0,"dy":dy,"at":at});
    let first = state.act(&wheel(100, -4_000));
    let middle = state.view.span;
    let second = state.act(&wheel(116, -8_000));
    assert!(
        middle < before && state.view.span < middle,
        "both small samples must advance the view"
    );
    assert!((middle / before - 2_f64.powf(-4. / 160.)).abs() < 1e-7);
    assert!((state.view.span / middle - (middle / before).powi(2)).abs() < 1e-7);
    assert!(
        first
            .iter()
            .chain(&second)
            .all(|call| call["service"] != online::SERVICE)
    );
}

#[test]
fn a_coalesced_wheel_burst_has_the_scale_of_its_individual_samples() {
    let mut merged = State::default();
    let mut individual = State::default();
    merged.view.span = 1.;
    individual.view.span = 1.;
    let wheel = |dy| json!({"t":"wheel","id":"map","dx":0,"dy":dy});
    merged.act(&wheel(800_000));
    for _ in 0..5 {
        individual.act(&wheel(160_000));
    }
    assert_eq!(merged.view.span, individual.view.span);
    assert_eq!(merged.view.span, 32.);
}

#[test]
fn wheel_answers_keep_the_pointer_anchor_and_observe_both_scale_limits() {
    for (span, dy, expected) in [
        (0.064, -40_000, 0.064 / 2_f64.powf(0.25)),
        (0.064, 800_000, 2.048),
        (0.064, -1_600_000, 0.002),
        (0.002, -800_000, 0.002),
        (360., 800_000, 360.),
    ] {
        let mut state = State {
            camera: Viewport {
                map_at: Some((0., 0., 640., 480.)),
                view: world::View {
                    lon: 20.,
                    lat: 44.,
                    span,
                    roll: 0.,
                },
                ..Viewport::default()
            },
            ..State::default()
        };
        let frame = state.frame();
        let pixel = (350., 220.);
        let place = world::Camera::new(state.view, frame)
            .locate(pixel.0, pixel.1)
            .unwrap();
        state.act(&json!({"t":"wheel","id":"map","x":350000,"y":220000,"dx":0,"dy":dy,"at":17}));
        assert!((state.view.span - expected).abs() < 1e-10);
        let reached = world::Camera::new(state.view, frame)
            .point(place.0, place.1)
            .unwrap();
        assert!(
            (reached.0 - pixel.0).hypot(reached.1 - pixel.1) < 1.,
            "{span}, {dy}: {reached:?}"
        );
    }
}

#[test]
fn device_scale_and_palette_updates_cap_large_rasters_and_repaint_the_same_camera() {
    let mut state = State::default();
    state.act(&size(0, 0, 640100, 400200));
    let before = state.view;
    let mut retina = size(0, 0, 640100, 400200);
    retina["scale"] = json!(1500);
    retina["palette"]["surface"] = json!([200, 210, 220, 255]);
    state.act(&retina);
    let frame = art(&state).0;
    assert_eq!(frame.raster, [961, 601, 1500]);
    assert_eq!(frame.surface, [200, 210, 220, 255]);
    assert_eq!(state.view, before);
    let mut huge = size(0, 0, 4096000, 2160000);
    huge["scale"] = json!(2500);
    state.act(&huge);
    let raster = state.surface.metrics.unwrap().raster;
    assert!(raster.pixels[0] <= 4096);
    assert!(u64::from(raster.pixels[0]) * u64::from(raster.pixels[1]) * 4 <= 32 * 1024 * 1024);
    assert!(raster.scale < 2500 && state.view == before);
}

#[test]
fn the_attribution_explains_unavailable_streets_only_where_they_are_turned_off() {
    const HINT: &str =
        "Street detail is off: turn on OpenStreetMap street detail in Location's settings";
    let mut state = State::default();
    let attribution = |state: &State| state.page()["child"]["children"][2].to_string();
    assert!(!attribution(&state).contains(HINT), "not at globe zoom");
    state.view = world::View {
        lon: 21.23,
        lat: 45.75,
        span: 0.04,
        roll: 0.,
    };
    assert!(attribution(&state).contains(HINT));
    state.online_enabled = true;
    assert!(!attribution(&state).contains(HINT), "already enabled");
    state.online_enabled = false;
    state.view = world::View {
        lon: 26.1025,
        lat: 44.4268,
        ..state.view
    };
    assert!(
        !attribution(&state).contains(HINT),
        "bundled streets are available"
    );
}

#[test]
fn approved_public_browsing_loads_roads_at_regional_zoom() {
    let mut state = State {
        online_enabled: true,
        ..State::default()
    };
    state.view = world::View {
        lon: 21.23,
        lat: 45.75,
        span: world::REGIONAL_SPAN,
        roll: 0.,
    };
    let restore = state.online.restore();
    let call = state.online_call(restore, true);
    state.told(&result(
        call["id"].as_u64().unwrap(),
        json!({"status":"ok","value":{"authorization":"approved"}}),
    ));
    let calls = state.act(&json!({"t":"tick","id":"clock","at":100}));
    assert!(
        calls
            .iter()
            .any(|call| call["method"] == "get-resource" && call["args"]["path"] == "/planet")
    );
    assert!(
        state.page()["child"]["children"][2]
            .to_string()
            .contains("Loading online streets")
    );
}

#[test]
fn options_overlays_the_map_without_changing_its_view_or_toolbar_height() {
    let mut state = State {
        camera: Viewport {
            map_at: Some((0., 0., 888., 527.)),
            ..Viewport::default()
        },
        ..State::default()
    };
    state.view = world::View {
        lon: 20.,
        lat: 45.,
        span: 225.,
        roll: 0.,
    };
    state.place(&json!({"page_features":["canvas"]}));
    let before = state.page();
    state.act(&json!({"t":"press","id":"options"}));
    let after = state.page();
    assert_eq!(find(&before, "map"), find(&after, "map"));
    let root = &after["child"]["children"];
    assert_eq!(before["child"]["children"][0]["style"], root[0]["style"]);
    let viewport = &root[1]["children"];
    assert_eq!(viewport[0]["id"], "map");
    let panel = viewport.as_array().unwrap().last().unwrap();
    assert_eq!(panel["style"]["overlay"], true);
    assert!(find(panel, "find").is_some());
    let restore = state.online.restore();
    let call = state.online_call(restore, true);
    state.online_enabled = true;
    state.told(&result(
        call["id"].as_u64().unwrap(),
        json!({"status":"ok","value":{"authorization":"approved"}}),
    ));
    let connected = state.page();
    assert_eq!(
        after["child"]["children"][2]["style"],
        connected["child"]["children"][2]["style"]
    );
    assert!(
        !connected["child"]["children"][2]
            .to_string()
            .contains("OpenMapTiles"),
        "authorization at world zoom is not evidence that any online geometry is drawn"
    );
    assert_eq!(find(&after, "map"), find(&connected, "map"));
}

#[test]
fn settings_enable_silent_restore_once_and_turning_off_releases_access() {
    let mut state = State::default();
    let config = |state: &mut State, settings: Value| {
        let id = state.ask_settings()["id"].as_u64().unwrap();
        state.told(&result(
            id,
            json!({"status":"ok","value":{"settings":settings}}),
        ))
    };
    assert!(
        config(&mut state, json!({}))
            .iter()
            .all(|call| call["service"] != online::SERVICE)
    );
    let enabled = json!({"online-streets":true});
    let calls = config(&mut state, enabled.clone());
    let restore = calls
        .iter()
        .find(|call| call["method"] == "restore-read")
        .unwrap();
    assert!(
        config(&mut state, enabled)
            .iter()
            .all(|call| call["service"] != online::SERVICE)
    );
    state.told(&result(
        restore["id"].as_u64().unwrap(),
        json!({"status":"ok","value":{"authorization":"restored"}}),
    ));
    assert!(state.online.ready());
    let calls = config(&mut state, json!({"online-streets":false}));
    assert!(calls.iter().any(|call| call["method"] == "release-read"));
    state.view.span = world::REGIONAL_SPAN;
    assert!(
        state
            .act(&json!({"t":"press","id":"closer"}))
            .iter()
            .all(|call| call["service"] != online::SERVICE)
    );
}

#[test]
fn a_resource_read_block_keeps_the_offline_map_and_needs_a_fresh_online_session() {
    let mut state = State {
        camera: Viewport {
            options: true,
            ..Viewport::default()
        },
        ..State::default()
    };
    state.view.span = world::REGIONAL_SPAN;
    let online_call = |calls: Vec<Value>, method: &str| {
        calls
            .into_iter()
            .find(|call| call["method"] == method)
            .unwrap_or_else(|| panic!("expected {method}"))["id"]
            .as_u64()
            .unwrap()
    };
    let cfg = state.ask_settings()["id"].as_u64().unwrap();
    let restore = online_call(
        state.told(&result(cfg, configuration(true, &[], json!([])))),
        "restore-read",
    );
    state.told(&result(
        restore,
        json!({"status":"ok","value":{"authorization":"old"}}),
    ));
    let download = online_call(state.act(&json!({"t":"tick","at":1000})), "get-resource");
    let read = online_call(
        state.told(&result(
            download,
            json!({"status":"ok","value":{"resource":"first","bytes":4}}),
        )),
        "read-resource",
    );
    assert!(
        state
            .told(&result(read, missing_permission("host", PUBLIC)))
            .is_empty(),
        "do not release or read again after refusal"
    );
    assert!(!state.online.ready());
    let page = state.page().to_string();
    assert!(page.contains("Online maps: missing permission network.public on this device."));
    assert!(page.contains("Location map. Drag to pan"));
    state.now += 100_000;
    let no_https = |calls: Vec<Value>| calls.iter().all(|call| call["service"] != online::SERVICE);
    assert!(no_https(state.act(&json!({"t":"tick","at":2000}))));
    assert!(no_https(state.act(&json!({"t":"press","id":"clear-kept"}))));
    state.act(&json!({"t":"press","id":"options"}));
    assert!(no_https(state.act(&json!({"t":"press","id":"options"}))));
    let cfg = state.ask_settings()["id"].as_u64().unwrap();
    assert!(no_https(
        state.told(&result(cfg, configuration(false, &[PUBLIC], json!([]))))
    ));
    let cfg = state.ask_settings()["id"].as_u64().unwrap();
    assert!(no_https(
        state.told(&result(cfg, configuration(true, &[], json!([]))))
    ));
    assert!(no_https(state.act(&json!({"t":"tick","at":3000}))));
    assert_ne!(
        online_call(state.act(&json!({"t":"open"})), "restore-read"),
        restore
    );
    assert!(
        state
            .told(&result(
                read,
                json!({"status":"ok","value":{"hex":"00000000","end":true}})
            ))
            .is_empty()
    );
}

#[test]
fn a_neutral_release_refusal_does_not_restart_or_expose_a_permission_hint() {
    let mut state = State {
        camera: Viewport {
            options: true,
            ..Viewport::default()
        },
        ..State::default()
    };
    let settings = |state: &mut State, enabled: bool| {
        let id = state.ask_settings()["id"].as_u64().unwrap();
        state.told(&result(id, configuration(enabled, &[], json!([]))))
    };
    let id = |calls: Vec<Value>, method: &str| {
        calls
            .iter()
            .find(|call| call["method"] == method)
            .expect("expected online operation")["id"]
            .as_u64()
            .unwrap()
    };
    let restore = id(settings(&mut state, true), "restore-read");
    state.told(&result(
        restore,
        json!({"status":"ok","value":{"authorization":"approved"}}),
    ));
    let release = id(settings(&mut state, false), "release-read");
    assert!(
        state
            .told(&result(release, json!({"status":"not-accepted"})))
            .is_empty()
    );
    assert!(
        state
            .page()
            .to_string()
            .contains("Online maps: not accepted.")
    );
    assert!(settings(&mut state, false).is_empty());
}

#[test]
fn map_keeps_quick_ticks_between_metadata_and_the_first_tile() {
    let mut state = State {
        online_enabled: true,
        ..State::default()
    };
    state.view = world::View {
        lon: 21.23,
        lat: 45.75,
        span: 0.1,
        roll: 0.,
    };
    let restore = state.online.restore();
    let call = state.online_call(restore, true);
    state.told(&result(
        call["id"].as_u64().unwrap(),
        json!({"status":"ok","value":{"authorization":"approved"}}),
    ));
    let calls = state.act(&json!({"t":"tick","id":"clock","at":0}));
    let get = calls
        .iter()
        .find(|call| call["method"] == "get-resource")
        .unwrap();
    let metadata =
        br#"{"tiles":["https://tiles.openfreemap.org/planet/test/{z}/{x}/{y}.pbf"],"maxzoom":14}"#;
    let read = state.told(&result(
        get["id"].as_u64().unwrap(),
        json!({"status":"ok","value":{"resource":"metadata","bytes":metadata.len()}}),
    ));
    state.told(&result(
        read[0]["id"].as_u64().unwrap(),
        json!({"status":"ok","value":{"hex":fixture_hex(metadata),"end":true}}),
    ));
    assert!(
        state
            .act(&json!({"t":"tick","id":"clock","at":100}))
            .iter()
            .all(|call| call["method"] != "get-resource")
    );
    assert_eq!(state.page()["milliseconds"], 100);
    let calls = state.act(&json!({"t":"tick","id":"clock","at":300}));
    assert!(calls.iter().any(|call| call["method"] == "get-resource"
        && call["args"]["path"].as_str().unwrap().ends_with(".pbf")));
}

/// A real opt-in and resource read, with the provider and the owner scripted at the wire boundary.
struct StreetScript {
    host: Script,
    state: State,
    next_get: usize,
    ticket: u64,
    now: u64,
}

impl StreetScript {
    fn new(view: world::View) -> Self {
        let mut session = Self::offline(view);
        session.enable_streets();
        session
    }

    fn offline(view: world::View) -> Self {
        let mut session = Self {
            host: Script::default(),
            state: State::default(),
            next_get: 0,
            ticket: 0,
            now: 0,
        };
        session.state.view = view;
        session.state.act(&size(0, 0, 640000, 500000));
        session.state.surface.stream = Some(7);
        session
    }

    fn enable_streets(&mut self) {
        self.configure(true);
        let restore = calls(&self.host, online::SERVICE, "restore-read")
            .last()
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        self.reply(restore, json!({"authorization":"approved"}));
        self.tick();
        let get = asked(&self.host, online::SERVICE, "get-resource", self.next_get);
        self.next_get += 1;
        self.read_resource(get, br#"{"tiles":["https://tiles.openfreemap.org/planet/test/{z}/{x}/{y}.pbf"],"maxzoom":14}"#);
    }

    fn select(&mut self, id: &str) {
        self.now += 20;
        for input in badge_tap(&self.state, id, self.now) {
            self.action(input);
        }
    }

    fn send(&mut self, delivery: Value) {
        let from = self.host.sent.len();
        self.host.deliveries.push_back(delivery);
        run_all(&mut self.host, &mut self.state);
        let sent = &self.host.sent[from..];
        for message in sent {
            assert!(serde_json::to_vec(message).unwrap().len() <= 48 * 1024);
        }
    }

    fn reply(&mut self, id: u64, value: Value) {
        self.send(result(id, json!({"status":"ok","value":value})));
    }

    fn configure(&mut self, enabled: bool) {
        let config = self.state.ask_settings()["id"].as_u64().unwrap();
        self.reply(
            config,
            json!({"settings":{"online-streets":enabled},"page_features":["canvas","pinch","rotate"]}),
        );
    }

    fn action(&mut self, action: Value) -> Value {
        self.ticket += 1;
        self.send(event(self.ticket, action));
        serde_json::from_str(&page(&self.host, self.ticket)).unwrap()
    }

    fn tick(&mut self) -> Value {
        self.now += 100;
        self.action(json!({"t":"tick","id":"clock","at":self.now}))
    }

    fn read_resource(&mut self, get: u64, data: &[u8]) {
        self.reply(get, json!({"resource":"test","bytes":data.len()}));
        for (index, chunk) in data.chunks(16 * 1024).enumerate() {
            let reads = calls(&self.host, online::SERVICE, "read-resource");
            let read = reads.last().unwrap();
            assert_eq!(read["args"]["offset"], index * 16 * 1024);
            self.reply(
                read["id"].as_u64().unwrap(),
                json!({"hex":fixture_hex(chunk),"end":(index+1)*16*1024>=data.len()}),
            );
        }
    }

    fn next_tile(&mut self) -> (u64, tile::Key) {
        for _ in 0..8 {
            if let Some(get) = calls(&self.host, online::SERVICE, "get-resource").get(self.next_get)
            {
                self.next_get += 1;
                let id = get["id"].as_u64().unwrap();
                let path = get["args"]["path"].as_str().unwrap();
                let key = path
                    .strip_prefix("/planet/test/")
                    .unwrap()
                    .strip_suffix(".pbf")
                    .unwrap();
                let mut key = key.split('/').map(|n| n.parse::<u32>().unwrap());
                return (
                    id,
                    tile::Key {
                        z: key.next().unwrap() as u8,
                        x: key.next().unwrap(),
                        y: key.next().unwrap(),
                    },
                );
            }
            self.tick();
        }
        panic!("no next tile was requested");
    }

    fn load(&mut self, data: &[u8]) -> tile::Key {
        let (id, prefix) = self.next_tile();
        self.read_resource(id, data);
        prefix
    }

    fn show(&mut self, key: tile::Key) {
        for _ in 0..80 {
            self.tick();
            if self.state.online.cached_keys().contains(&key) && !self.state.online.decode_pending()
            {
                self.tick();
                return;
            }
        }
        panic!("decoded tile {key:?} never became available");
    }

    fn art(&self) -> Art {
        Art::parse(&self.host.written)
    }
    fn shown(&self) -> Vec<tile::Key> {
        self.state
            .online
            .shown_keys(self.state.view, self.state.frame())
    }
}

#[test]
fn a_selected_peer_continues_into_drawable_streets_after_opt_in() {
    let street = world::View {
        lon: 0.011,
        lat: 0.011,
        span: STREET_FOCUS,
        roll: 0.,
    }
    .kept();
    let offline = world::View {
        span: 0.08,
        ..street
    };
    let mut session = StreetScript::offline(offline);
    session.state.listed(&[peer("phone", "Phone", true)]);
    session
        .state
        .seen
        .insert("phone".into(), told(street.lat, street.lon));
    session.tick();
    let shown = |s: &StreetScript| s.state.surface.painted.unwrap().view;
    session.select("phone");
    assert_eq!(shown(&session), offline);
    assert!(
        session
            .state
            .page()
            .to_string()
            .contains("Street detail is off")
    );
    session.enable_streets();
    assert_eq!(
        shown(&session),
        offline,
        "approval and metadata are not street coverage"
    );
    let keys = tile::visible(street, session.state.frame(), 14);
    let tile = road_tile(128);
    for i in 0..keys.len() {
        let key = session.load(&tile);
        assert!(
            keys.contains(&key),
            "load the selected camera, not the wider offline viewport"
        );
        session.show(key);
        if i + 1 < keys.len() {
            assert_eq!(
                shown(&session),
                offline,
                "partial coverage must not move the camera"
            );
        }
    }
    let mut intermediate = false;
    for _ in 0..7 {
        session.tick();
        let view = shown(&session);
        intermediate |= street.span < view.span && view.span < offline.span;
        assert_eq!(view.centre(), street.centre());
    }
    assert!(intermediate, "late coverage starts a glide, not a jump");
    assert_eq!(shown(&session), street);
    assert!(
        session
            .art()
            .paths
            .iter()
            .any(|path| path.clip.is_some() && path.color == 8)
    );

    for span in [360., world::REGIONAL_SPAN] {
        session.action(json!({"t":"press","id":"world","at":session.now + 1}));
        if span < 360. {
            session.state.view = world::View {
                span,
                lon: street.lon - 0.1,
                ..street
            };
            session.tick();
        }
        let before = shown(&session);
        assert_eq!(before.span, span);
        session.select("phone");
        assert_eq!(shown(&session), before);
        for _ in 0..5 {
            session.tick();
        }
        assert_eq!(
            shown(&session),
            street,
            "cached streets can be focused in one glide from {span}°"
        );
    }
}

/// Buffered transportation geometry with eight vertices per road, all distinct, all valid.
fn road_tile(roads: usize) -> Vec<u8> {
    road_tile_rows((1..=roads).map(|row| row * 3930 / roads))
}

fn road_tile_rows(rows: impl IntoIterator<Item = usize>) -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut n: usize) {
        while n > 127 {
            out.push((n as u8 & 127) | 128);
            n >>= 7;
        }
        out.push(n as u8);
    }
    fn field(out: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
        out.push(tag << 3 | 2);
        varint(out, bytes.len());
        out.extend_from_slice(bytes);
    }
    let mut layer = Vec::new();
    field(&mut layer, 1, b"transportation");
    for y in rows {
        let mut geom = vec![9];
        varint(&mut geom, 200);
        varint(&mut geom, y * 2);
        geom.push(7 << 3 | 2);
        for _ in 0..7 {
            varint(&mut geom, 800);
            geom.push(2);
        }
        let mut feature = vec![18, 2, 0, 0, 24, 2];
        field(&mut feature, 4, &geom);
        field(&mut layer, 2, &feature);
    }
    field(&mut layer, 3, b"class");
    field(&mut layer, 4, b"\x0a\x07primary");
    layer.extend_from_slice(b"\x28\x80\x20");
    let mut tile = Vec::new();
    field(&mut tile, 3, &layer);
    tile
}

fn street_view(lon: f64, span: f64) -> world::View {
    world::View {
        lon,
        lat: -0.01,
        span,
        roll: 0.,
    }
}

#[test]
fn a_decoded_tile_is_reused_and_its_roads_move_when_the_canvas_pans() {
    let mut session = StreetScript::new(street_view(0.011, 0.02));
    let key = session.load(&road_tile(1024));
    session.show(key);
    let roads = |art: Art| {
        art.paths
            .into_iter()
            .filter(|p| p.mode == 1 && p.color == 8 && (p.width - 1.6).abs() < 0.01)
            .flat_map(|p| p.points)
            .collect::<Vec<_>>()
    };
    let before = roads(session.art());
    assert!(!before.is_empty());
    session
        .action(json!({"t":"pointer","id":"map","phase":"down","x":320000,"y":250000,"at":10000}));
    session
        .action(json!({"t":"pointer","id":"map","phase":"move","x":400000,"y":250000,"at":10010}));
    let after = roads(session.art());
    assert!(!after.is_empty() && after != before);
    assert!(session.shown().contains(&key));
    let url = format!("/planet/test/{}/{}/{}.pbf", key.z, key.x, key.y);
    assert_eq!(
        calls(&session.host, online::SERVICE, "get-resource")
            .iter()
            .filter(|c| c["args"]["path"] == url)
            .count(),
        1
    );
}

#[test]
fn maximum_overzoom_at_a_tile_edge_still_shows_the_cached_streets() {
    let view = world::View {
        lon: 0.011,
        lat: -0.0208,
        span: 0.032,
        roll: 0.,
    };
    let mut session = StreetScript::new(view);
    let key = session.load(&road_tile_rows(3875..=3885));
    assert_eq!(
        key,
        tile::Key {
            z: 14,
            x: 8192,
            y: 8192
        }
    );
    session.show(key);
    session.action(size(0, 0, 1200000, 800000));
    session.action(
        json!({"t":"wheel","id":"map","x":600000,"y":400000,"dx":0,"dy":-640000,"at":2000}),
    );
    assert_eq!(session.state.view.span, 0.002);
    let frame = session.art();
    assert_eq!(frame.settled, 2000);
    assert!(
        frame
            .paths
            .iter()
            .any(|p| p.mode == 1 && p.color == 8 && (p.width - 1.6).abs() < 0.01),
        "the overzoomed road must remain visible"
    );
    session
        .action(json!({"t":"wheel","id":"map","x":600000,"y":400000,"dx":0,"dy":640000,"at":2050}));
    assert!(session.shown().contains(&key));
}

#[test]
fn a_dense_tile_batches_visible_roads_without_exceeding_the_display_list_budget() {
    let mut session = StreetScript::new(street_view(0.011, 0.02));
    let key = session.load(&road_tile(3200));
    session.show(key);
    let frame = session.art();
    let roads: Vec<_> = frame
        .paths
        .iter()
        .filter(|p| p.mode == 1 && p.color == 8 && (p.width - 1.6).abs() < 0.01)
        .collect();
    assert!(
        roads.len() >= 3,
        "a dense tile is split across bounded native paths; emitted {} batches / {} verbs",
        roads.len(),
        roads.iter().map(|p| u32::from(p.verbs)).sum::<u32>()
    );
    assert!(
        roads.iter().all(|p| p.clip.is_some()),
        "native tile clips own the boundaries"
    );
    assert!(
        roads.iter().map(|p| p.moves).sum::<usize>() >= 1000,
        "the centre is not thinned to the inline budget"
    );
}

#[test]
fn only_a_complete_replacement_changes_the_presented_map() {
    let view = world::View {
        lon: 0.02197,
        lat: -0.02197,
        span: 0.05,
        roll: 0.,
    };
    let mut session = StreetScript::new(view);
    let tile = road_tile(48);
    let count = tile::visible(view, session.state.frame(), 14).len();
    assert_eq!(count, 4);
    for i in 1..=count {
        let key = session.load(&tile);
        session.show(key);
        let roads = session
            .art()
            .paths
            .iter()
            .any(|p| p.clip.is_some() && p.mode == 1 && p.color == 8);
        assert_eq!(roads, i == count, "no partial network ({i}/{count} tiles)");
    }
    let completed = session.art().seq;
    let stamp = session.state.surface.painted;
    session.state.view.span *= 0.999_999;
    next_canvas_answers(canvas_sdk::PRESENT_LIMITED);
    session.state.pump_canvas(&mut session.host, true).unwrap();
    assert_eq!(
        session.art().seq,
        completed,
        "limit cannot publish partial pixels"
    );
    assert!(
        session.state.surface.painted == stamp,
        "retain the last accepted camera"
    );
    session.state.now += 34;
    session.state.pump_canvas(&mut session.host, true).unwrap();
    let replacement = session.art().seq;
    assert_eq!(
        replacement,
        completed + 1,
        "the retry publishes one complete frame"
    );
    assert!(session.state.surface.painted != stamp);

    session.state.view.lon += 0.2;
    let first = session.load(&tile);
    session.show(first);
    assert_eq!(
        session.art().seq,
        replacement,
        "waiting keeps the full map on screen"
    );
    let (get, _) = session.next_tile();
    session.send(result(get, json!({"status":"error","code":"unavailable"})));
    session.tick();
    assert!(
        session.art().seq > replacement,
        "unavailable tiles fall back to the offline view"
    );
    assert!(!session.state.surface.picture.online_drawn);
}

#[test]
fn a_limited_first_frame_still_shows_a_map_on_open_and_reopen() {
    for (online, missing) in [
        (false, [].as_slice()),
        (true, [].as_slice()),
        (true, [READ, WATCH, PUBLIC].as_slice()),
    ] {
        let mut map = MarkerMap::default();
        let config = map.state.ask_settings()["id"].as_u64().unwrap();
        map.answer(config, configuration(online, missing, json!([])));
        if online && missing.is_empty() {
            let restore = asked(&map.host, online::SERVICE, "restore-read", 0);
            map.answer(
                restore,
                json!({"status":"ok","value":{"authorization":"fixture"}}),
            );
        }
        for at in [1_000, 3_000] {
            map.tick(at);
            map.host.written.clear();
            next_canvas_answers(canvas_sdk::PRESENT_LIMITED);
            map.open(PAGE, size(0, 0, 1_540_000, 830_000));
            assert!(
                map.host.written.is_empty(),
                "a limited frame was not presented"
            );
            map.tick(at + 100);
            let shown = Art::parse(&map.host.written);
            assert_eq!((shown.seq, shown.raster), (1, [924, 498, 600]));
            assert!(
                shown
                    .paths
                    .iter()
                    .any(|path| path.mode == 0 && path.verbs > 10)
            );
            assert!(shown.label("+") && shown.label("−"));
            let has_coast_stroke = |art: &Art| {
                art.paths.iter().any(|path| {
                    path.mode == 1 && path.color == 5 && path.width == 1.2 && path.verbs > 10
                })
            };
            assert!(
                !has_coast_stroke(&shown),
                "defer expensive detail for the first map"
            );
            map.tick(at + 200);
            let detailed = Art::parse(&map.host.written);
            assert!(detailed.seq > shown.seq && has_coast_stroke(&detailed));
        }
        assert_eq!(calls(&map.host, canvas_sdk::SERVICE, "open").len(), 2);
    }
}

#[test]
fn native_pressure_preserves_the_whole_frame_and_restores_settled_resolution() {
    let mut state = State::default();
    let mut event = size(0, 0, 640000, 500000);
    event["scale"] = json!(400);
    state.act(&event);
    state.surface.stream = Some(7);
    let mut host = Script::default();
    state.pump_canvas(&mut host, true).unwrap();
    let initial = Art::parse(&host.written);
    assert_eq!(initial.raster, [256, 200, 400]);

    state.last_motion = Some(state.now);
    state.view.span /= 2.;
    for _ in 0..2 {
        next_canvas_answers(canvas_sdk::PRESENT_LIMITED);
        state.pump_canvas(&mut host, false).unwrap();
        assert_eq!(Art::parse(&host.written).seq, initial.seq);
        state.now += 34;
    }
    state.pump_canvas(&mut host, false).unwrap();
    let preview = Art::parse(&host.written);
    assert_eq!(preview.seq, initial.seq + 1);
    assert_eq!(preview.raster, [154, 120, 240]);
    assert!(preview.label("+") && preview.label("−"));

    state.now += DETAIL_AFTER_MOTION_MS;
    state.pump_canvas(&mut host, true).unwrap();
    let settled = Art::parse(&host.written);
    assert_eq!(settled.seq, preview.seq + 1);
    assert_eq!(settled.raster, initial.raster);
    assert_eq!(settled.settled, preview.settled);
}

#[test]
fn a_refused_offline_globe_preview_does_not_reduce_native_density() {
    let mut state = State::default();
    let mut measured = size(0, 0, 335_714, 533_928);
    measured["scale"] = json!(2800);
    state.act(&measured);
    state.surface.stream = Some(7);
    let mut host = Script::default();
    state.pump_canvas(&mut host, true).unwrap();
    let initial = Art::parse(&host.written);
    let has_coast_stroke = |art: &Art| {
        art.paths
            .iter()
            .any(|p| p.mode == 1 && p.color == 5 && p.width == 1.2 && p.verbs > 10)
    };
    assert!(has_coast_stroke(&initial));

    let wheel = |at| {
        json!({
            "t":"wheel","id":"map","x":168000,"y":267000,"dx":0,"dy":-16000,"at":at
        })
    };
    state.act(&wheel(100));
    next_canvas_answers(canvas_sdk::PRESENT_LIMITED);
    state.pump_canvas(&mut host, false).unwrap();
    assert_eq!(Art::parse(&host.written).seq, initial.seq);

    state.act(&wheel(140));
    state.pump_canvas(&mut host, false).unwrap();
    let next = Art::parse(&host.written);
    assert_eq!(next.seq, initial.seq + 1);
    assert_eq!(next.settled, 140);
    assert_eq!(next.raster, [940, 1495, 2800]);
    assert!(
        !has_coast_stroke(&next),
        "defer the second pass over the land contours"
    );
    assert!(
        next.paths
            .iter()
            .any(|p| p.mode == 0 && p.color == 6 && p.verbs > 10)
    );
    assert!(
        next.paths
            .iter()
            .any(|p| p.mode == 1 && p.color == 4 && p.verbs > 10)
    );

    state.now += DETAIL_AFTER_MOTION_MS;
    state.pump_canvas(&mut host, true).unwrap();
    let settled = Art::parse(&host.written);
    assert_eq!(settled.seq, next.seq + 1);
    assert!(
        has_coast_stroke(&settled),
        "restore the coast stroke when idle"
    );
}

#[test]
fn a_sharp_phone_zoom_repaints_the_camera_before_restoring_street_labels() {
    fn field(out: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
        assert!(bytes.len() < 128);
        out.extend([tag << 3 | 2, bytes.len() as u8]);
        out.extend(bytes);
    }

    let mut session = StreetScript::new(street_view(0.011, 0.014));
    let mut measured = size(0, 0, 335_714, 533_928);
    measured["scale"] = json!(2800);
    session.action(measured);

    let name = "Strada Fagului";
    let mut value = Vec::new();
    field(&mut value, 1, name.as_bytes());
    let mut way = vec![18, 2, 0, 0, 24, 2];
    // Move to (1000, 1900); end the line at (3000, 2100).
    field(&mut way, 4, &[9, 208, 15, 216, 29, 10, 160, 31, 144, 3]);
    let mut layer = Vec::new();
    field(&mut layer, 1, b"transportation_name");
    field(&mut layer, 2, &way);
    field(&mut layer, 3, b"name");
    field(&mut layer, 4, &value);
    let mut tile = road_tile(64);
    field(&mut tile, 3, &layer);
    let key = session.load(&tile);
    session.show(key);
    let initial = session.art();
    assert!(initial.label(name), "street label is drawn at rest");

    let wheel = |at| {
        json!({
            "t":"wheel","id":"map","x":168000,"y":267000,"dx":0,"dy":-16000,"at":at
        })
    };
    let at = session.now + 100;
    session.action(wheel(at));
    let quick = session.art();
    assert_eq!(
        quick.raster,
        [940, 1495, 2800],
        "rasterise fresh text at native density"
    );
    assert_eq!(
        quick.settled, at,
        "paint the zoomed camera, not the old bitmap"
    );
    assert!(quick.label("+") && quick.label("−"));
    let major = |frame: &Art| {
        frame
            .paths
            .iter()
            .any(|p| p.mode == 1 && p.color == 8 && p.width >= 1.5)
    };
    assert!(major(&quick));
    assert!(!quick.label(name), "street text waits for the full frame");

    session.action(wheel(at + 16));
    assert_eq!(
        session.art().seq,
        quick.seq,
        "coalesce rapid samples before the stream is full"
    );

    next_canvas_answers(canvas_sdk::PRESENT_LIMITED);
    session.action(wheel(at + 40));
    assert_eq!(
        session.art().seq,
        quick.seq,
        "keep the prior full raster on refusal"
    );
    let at = at + 80;
    session.action(wheel(at));
    let next = session.art();
    assert_eq!(next.seq, quick.seq + 1);
    assert_eq!(next.settled, at);
    assert_eq!(
        next.raster, quick.raster,
        "a refused preview must not blur the next one"
    );
    assert!(major(&next));

    session.action(json!({"t":"tick","id":"clock","at":at + DETAIL_AFTER_MOTION_MS}));
    let settled = session.art();
    assert!(settled.seq > next.seq && settled.label(name) && major(&settled));
    assert_eq!(settled.raster, quick.raster);
    assert_eq!(settled.settled, at);
}

#[test]
fn retained_tiles_switch_levels_only_when_the_whole_view_is_ready() {
    let mut session = StreetScript::new(world::View {
        lon: 0.02197,
        lat: -0.02197,
        span: 0.11,
        roll: 0.,
    });
    let tile = road_tile(64);
    let parent = session.load(&tile);
    assert_eq!(
        parent,
        tile::Key {
            z: 13,
            x: 4096,
            y: 4096
        }
    );
    session.show(parent);
    session.state.view.span = 0.05;
    let expected = tile::visible(session.state.view, session.state.frame(), 14);
    assert_eq!(expected.len(), 4);
    for i in 0..expected.len() {
        let (get, key) = loop {
            let (get, key) = session.next_tile();
            if key.z == 14 {
                break (get, key);
            }
            session.send(result(get, json!({"status":"error","code":"unavailable"})));
        };
        session.read_resource(get, &tile);
        session.show(key);
        let selected = session.shown();
        if i + 1 < expected.len() {
            assert_eq!(selected, [parent]);
        } else {
            assert_eq!(selected, expected);
        }
    }
}

fn place_tile(places: &[(&str, u32, u32)]) -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut n: u32) {
        while n >= 128 {
            out.push(n as u8 | 128);
            n >>= 7;
        }
        out.push(n as u8);
    }
    fn field(out: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
        out.push(tag << 3 | 2);
        varint(out, bytes.len() as u32);
        out.extend_from_slice(bytes);
    }
    let mut layer = Vec::new();
    field(&mut layer, 1, b"place");
    field(&mut layer, 3, b"name");
    for (i, &(label, x, y)) in places.iter().enumerate() {
        let mut geometry = vec![9];
        varint(&mut geometry, x * 2);
        varint(&mut geometry, y * 2);
        let mut feature = vec![18, 2, 0, i as u8, 24, 1];
        field(&mut feature, 4, &geometry);
        field(&mut layer, 2, &feature);
        let mut value = Vec::new();
        field(&mut value, 1, label.as_bytes());
        field(&mut layer, 4, &value);
    }
    layer.extend_from_slice(b"\x28\x80\x20");
    let mut tile = road_tile(8);
    field(&mut tile, 3, &layer);
    tile
}

#[test]
fn place_labels_stay_inside_the_measured_view_and_never_overlap() {
    let mut session = StreetScript::new(world::View {
        lon: 0.011,
        lat: -0.011,
        span: 0.032,
        roll: 0.,
    });
    let places = [
        ("Dacia", 2000, 1800),
        ("Bucovina", 2005, 1803),
        ("Iosefin", 570, 2100),
        ("Soarelui", 3360, 2500),
        ("Timișoara", 2120, 2400),
    ];
    let key = session.load(&place_tile(&places));
    session.show(key);
    let frame = session.art();
    assert!(frame.label("Dacia") && frame.label("Timișoara"));
    let labels: Vec<_> = frame
        .labels
        .iter()
        .filter(|l| places.iter().any(|p| p.0 == l.text))
        .collect();
    for (i, label) in labels.iter().enumerate() {
        let area @ [x, y, w, h] = label.clip.unwrap();
        assert!(x >= 0. && y >= 0. && x + w <= 584. && y + h <= 500.);
        assert!(labels[..i].iter().all(|l| !overlaps(l.clip.unwrap(), area)));
        assert!(
            frame
                .paths
                .iter()
                .any(|p| p.mode == 0 && p.color == 0 && p.points.first() == Some(&[x, y]))
        );
    }
}

#[test]
fn evicting_and_clearing_tiles_changes_the_guest_cache_not_the_host_drawing_store() {
    let mut session = StreetScript::new(street_view(0.011, 0.02));
    let tile = road_tile(8);
    let mut keys = Vec::new();
    for i in 0..19 {
        session.state.view.lon = (f64::from(i) * 32. + 0.5) * 360. / 16384.;
        let key = session.load(&tile);
        session.show(key);
        keys.push(key);
    }
    let cached = session.state.online.cached_keys();
    assert_eq!(cached.len(), 18);
    assert!(!cached.contains(&keys[0]) && cached.contains(keys.last().unwrap()));
    session.action(json!({"t":"press","id":"clear-kept"}));
    assert!(session.state.online.cached_keys().is_empty());
    assert_eq!(
        calls(&session.host, online::SERVICE, "clear-resource-cache").len(),
        1
    );
}

#[test]
fn turning_off_streets_cancels_pending_io_and_ignores_its_late_tile() {
    let mut session = StreetScript::new(street_view(0.011, 0.02));
    let first = session.load(&road_tile(8));
    session.show(first);
    session.state.view.lon = 1.;
    let (get, _) = session.next_tile();
    session.configure(false);
    assert!(
        session
            .host
            .sent
            .iter()
            .any(|op| op["op"] == "cancel" && op["id"] == get)
    );
    session.reply(get, json!({"resource":"late","bytes":4}));
    session.tick();
    assert!(
        session.state.online.cached_keys().is_empty()
            && !session.state.surface.picture.online_drawn
    );
    assert_eq!(
        calls(&session.host, online::SERVICE, "release-read").len(),
        1
    );
}

#[test]
fn a_fifty_kilometre_view_draws_one_regional_level_with_water_and_built_up_land() {
    let mut session = StreetScript::new(world::View {
        lon: 21.23,
        lat: 45.75,
        span: 0.75,
        roll: 0.,
    });
    session.action(size(0, 0, 1616000, 930000));
    let mut tile = road_tile(24);
    fn field(out: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
        assert!(bytes.len() < 128);
        out.extend([tag << 3 | 2, bytes.len() as u8]);
        out.extend(bytes);
    }
    for name in [b"water".as_slice(), b"landuse"] {
        let mut layer = Vec::new();
        field(&mut layer, 1, name);
        let mut feature = vec![18, 2, 0, 0, 24, 3];
        field(
            &mut feature,
            4,
            b"\x09\x00\x00\x1a\x80\x40\x00\x00\x80\x40\xff\x3f\x00\x0f",
        );
        field(&mut layer, 2, &feature);
        field(&mut layer, 3, b"class");
        field(&mut layer, 4, b"\x0a\x0bresidential");
        layer.extend_from_slice(b"\x28\x80\x20");
        field(&mut tile, 3, &layer);
    }
    let visible = tile::visible(session.state.view, session.state.frame(), 14);
    assert_eq!(visible.len(), 9);
    assert!(visible.iter().all(|key| key.z == 10));
    for _ in &visible {
        let key = session.load(&tile);
        assert_eq!(key.z, 10);
        session.show(key);
    }
    assert_eq!(session.shown(), visible);
    let art = session.art();
    for (mode, color) in [(1, 8), (0, 2), (0, 5)] {
        assert!(
            art.paths.iter().any(|p| p.mode == mode && p.color == color),
            "missing regional paint {mode}, {color}"
        );
    }
}

#[test]
fn slim_controls_and_whole_labels_fit_phone_and_desktop_canvases() {
    for (w, h, rail_width) in [(620u16, 970u16, 38), (1616, 930, 46), (3392, 1600, 46)] {
        let mut state = State::default();
        state.act(&size(0, 0, u32::from(w) * 1000, u32::from(h) * 1000));
        state.view = world::View {
            lon: 21.,
            lat: 45.,
            span: 168.,
            roll: 13.,
        };
        let frame = state.frame();
        let rail = ZoomRail::new(frame).unwrap();
        assert_eq!(rail.width, rail_width);
        let art = art(&state).0;
        let camera = world::Camera::new(state.view, frame);
        let readout = rail.elevation(state.view, &camera);
        let [left, top, wide, tall] = rail.bounds();
        let contains = |[x, y, w, h]: [f64; 4]| {
            x >= left && x + w <= left + wide + 0.001 && y >= top && y + h <= top + tall + 0.001
        };
        assert_eq!(left + wide + 6., f64::from(w));
        let title = art.labels.iter().find(|l| l.text == readout.text).unwrap();
        let area @ [_, y, width, height] = title.clip.unwrap();
        assert!(contains(area), "the whole readout is on the rail");
        assert_eq!(title.direction, [0., -1.]);
        assert_eq!(width, 14.);
        assert!(height >= tile::text_width(&title.text, f64::from(readout.font)));
        assert!((y + height / 2. - rail.marker(state.view)).abs() < 0.01);
        for name in ["+", "−"] {
            let control = art.labels.iter().find(|l| l.text == name).unwrap();
            assert!(
                contains(control.clip.unwrap()),
                "{name} is outside the rail"
            );
        }
        let backing = art
            .paths
            .iter()
            .position(|p| {
                p.mode == 0 && p.color == 0 && p.points.first() == Some(&[left + wide / 2., top])
            })
            .expect("the rail has its own rounded backing");
        let rail_paths: Vec<_> = art.paths[backing..]
            .iter()
            .take_while(|p| p.points.first().is_some_and(|[x, _]| *x >= left))
            .collect();
        assert!(rail_paths.len() >= 5, "backing, track, marker and compass");
        for p in rail_paths {
            assert!(p.points.iter().all(|&[x, y]| contains([x, y, 0., 0.])));
        }
        assert!(f64::from(w) - left >= 44., "full touch width");
        for x in [left + 0.01, f64::from(w) - 0.01] {
            for y in [0., 43.99] {
                assert!(matches!(canvas_scene::hit(&[], frame, (x, y)), Hit::Closer));
            }
            for y in [f64::from(h) - 44., f64::from(h) - 0.01] {
                assert!(matches!(canvas_scene::hit(&[], frame, (x, y)), Hit::Wider));
            }
        }
        let x = rail.centre_x();
        for y in [f64::from(rail.middle()), f64::from(rail.middle()) + 43.99] {
            assert!(matches!(canvas_scene::hit(&[], frame, (x, y)), Hit::Centre));
        }
        let bounds = map_label_bounds(frame);
        let scale = ScaleBar::new(frame, &camera).unwrap().bounds();
        let mut labels = 0;
        for label in art
            .labels
            .iter()
            .filter(|l| !matches!(l.text.as_str(), "+" | "−") && l.text != readout.text)
        {
            let Some(area) = label.clip else {
                continue;
            };
            labels += 1;
            assert!(bounds.fits(area), "{:?} clipped", label);
            assert!(
                !overlaps(area, scale) && !overlaps(area, readout.bounds),
                "{:?} under chrome",
                label
            );
            assert!(area[2] >= tile::text_width(&label.text, f64::from(label.size)));
        }
        assert!(labels >= 8, "continental map must retain useful labels");
        eprintln!(
            "{w}x{h}: rail {rail_width}px; +/−/centre 44px tall; {} {:?}; {labels} map label boxes",
            readout.text,
            title.clip.unwrap()
        );
    }
}

/// The tiles a device really saved are many times denser than the fixtures made here: a folder of them (an application's
/// saved map areas of this extension) is read whole, each tile in the turns a page has while a person waits.
#[test]
#[ignore = "requires LOCATION_SAVED_TILES: a folder of tiles a device really saved"]
fn real_saved_tiles_are_read_in_few_turns() {
    let folder = std::env::var_os("LOCATION_SAVED_TILES").expect("saved tile directory");
    let mut files: Vec<_> = std::fs::read_dir(folder)
        .unwrap()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    for file in files {
        let bytes = std::fs::read(&file).unwrap();
        let key = tile::Key {
            z: 14,
            x: 9379,
            y: 5930,
        };
        let mut decoder = tile::Decode::start(key, bytes).expect("a tile the provider served");
        // Ten turns a second: more than these and a city would take minutes to appear.
        let turns = (1..=40).find(|_| decoder.step().expect("valid geometry"));
        assert!(turns.is_some(), "{file:?} is not read in forty turns");
    }
}

#[test]
#[ignore = "requires LOCATION_PARITY_FIXTURES with the public tile snapshot"]
fn matched_public_city_geometry_and_controls() {
    let root = std::path::PathBuf::from(
        std::env::var_os("LOCATION_PARITY_FIXTURES").expect("public tile fixture directory"),
    );
    for (w, h) in [(100, 100), (1616, 930), (3392, 1600), (620, 970)] {
        // The portrait city camera needs three more z13 tiles than were cached.
        let spans: &[_] = if w == 620 {
            &[0.029, 0.184]
        } else {
            &[0.029, 0.092, 0.184]
        };
        for &span in spans {
            let mut state = State {
                online_enabled: true,
                ..State::default()
            };
            state.view = world::View {
                lon: 21.2287,
                lat: 45.754,
                span,
                roll: 0.,
            };
            state.act(&size(0, 0, w * 1000, h * 1000));
            let (v, f) = (state.view, state.frame());
            let cached = state.online.stage_fixture(&root, v, f);
            let expected = state.online.with_drawing(v, f, |selected| {
                let (drawing, complete) = selected.expect("the selected provider level");
                assert!(complete, "the full viewport is staged");
                drawing.packed_counts()
            });
            let (bytes, picture) = raw_art(&state);
            assert!(picture.online_drawn);
            let art = Art::parse(&bytes);
            let styles = [
                (0, 5, 0.),
                (0, 2, 0.),
                (0, 8, 0.),
                (0, 3, 0.),
                (1, 5, 0.65),
                (1, 5, 1.),
                (1, 8, 1.6),
            ];
            let sent = styles.map(|(mode, color, width)| {
                art.paths
                    .iter()
                    .filter(|p| {
                        p.clip.is_some()
                            && p.mode == mode
                            && p.color == color
                            && (p.width - width).abs() < 0.01
                    })
                    .fold((0, 0), |(paths, verbs), p| {
                        (paths + 1, verbs + usize::from(p.verbs))
                    })
            });
            assert_eq!(
                sent, expected,
                "{w}×{h} span {span}: every visible path has room"
            );
            let count = |mode, color| {
                art.paths
                    .iter()
                    .filter(|p| p.mode == mode && p.color == color && p.clip.is_some())
                    .map(|p| (p.moves, p.closed))
                    .fold((0, 0), |(a, b), (c, d)| (a + c, b + d))
            };
            let buildings = count(0, 3).1;
            let roads = count(1, 8).0 + count(1, 5).0;
            let verbs = art
                .paths
                .iter()
                .map(|p| usize::from(p.verbs))
                .sum::<usize>();
            let labels = art
                .labels
                .iter()
                .filter(|l| l.direction == [1., 0.] && l.clip.is_some())
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>();
            eprintln!(
                "{w}x{h} span {span}: {buildings} building rings, {roads} road pieces, {} paths, {verbs} verbs, {} bytes, {cached} cache bytes; labels {labels:?}",
                art.paths.len(),
                bytes.len()
            );
            eprintln!(
                "  land / water / parks / buildings / paths / local / arterial (paths, verbs): {sent:?}"
            );
            if span == 0.029 {
                let minimum = match w {
                    100 => 1100,
                    620 | 1616 => 1650,
                    3392 => 2000,
                    _ => unreachable!(),
                };
                assert!(
                    buildings >= minimum && roads >= 1500,
                    "the street fixture must not lose dense detail"
                );
            }
            if span == 0.184 && w >= 620 {
                for name in ["Dacia", "Fabric", "Iosefin"] {
                    assert!(art.label(name), "{w}×{h}: missing neighbourhood {name}");
                }
            }
            // At 3392×1600 the district camera chooses z11, where this
            // provider does not supply Elisabetin; the closer city view does.
            if (w == 3392 && span == 0.092) || (matches!(w, 620 | 1616) && span == 0.184) {
                assert!(art.label("Elisabetin"), "{w}×{h}: missing Elisabetin");
            }
            assert!(!art.label("Strada fără nume"));
            if let Some(rail) = ZoomRail::new(f) {
                let elevation = rail.elevation(v, &world::Camera::new(v, f));
                assert!(art.labels.iter().any(|l| l.text == elevation.text));
            }
            if let Some(out) = std::env::var_os("LOCATION_PARITY_OUT") {
                let name = format!("display-{w}-{h}-{}.bin", (span * 100000.) as u32);
                std::fs::write(std::path::PathBuf::from(out).join(name), bytes).unwrap();
            }
        }
    }
}
