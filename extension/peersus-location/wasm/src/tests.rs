use super::*;
use host::scripted::{Script, ack};

fn event(ticket: u64, event: Value) -> Value {
    json!({"type":"call","ticket":ticket,"method":"event","args":{"event":event}})
}

fn result(id: u64, result: Value) -> Value {
    json!({"type":"result","id":id,"result":result})
}

fn found(latitude: f64, longitude: f64, age: u64) -> Value {
    json!({"status":"ok","value":{"latitude":latitude,"longitude":longitude,"accuracy":10.0,"age":age}})
}

fn peer(alias: &str, name: &str, ready: bool) -> Value {
    json!({"device":alias,"name":name,"online":true,"presence":"online","allowed":true,
        "accepts": if ready { json!([wire::SERVICE]) } else { json!([]) }})
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
        .find(|sent| sent["op"] == "reply" && sent["ticket"] == ticket)
        .map(|reply| reply["result"]["value"].to_string())
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

#[test]
fn sharing_finds_this_device_and_sends_it_once_to_each_device_that_takes_it() {
    let mut host = Script {
        acks: true,
        ..Script::default()
    };
    let mut state = State::default();
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
fn a_share_nobody_can_take_says_so_and_sends_nothing() {
    let mut host = Script::default();
    let mut state = State::default();
    state.listed(&[peer("b2", "Laptop", false)]);
    host.deliveries
        .push_back(event(1, json!({"t":"press","id":"share"})));
    run_all(&mut host, &mut state);
    let find = calls(&host, LOCATION, "current")[0]["id"].as_u64().unwrap();
    host.deliveries.push_back(result(find, found(1.0, 2.0, 0)));
    host.deliveries
        .push_back(event(2, json!({"t":"tick","id":"clock","at":1000})));
    run_all(&mut host, &mut state);
    assert!(calls(&host, PEERS, "send").is_empty());
    assert!(page(&host, 2).contains(words::NOBODY));
}

#[test]
fn showing_this_device_sends_it_nowhere_and_a_refusal_is_said() {
    let mut host = Script::default();
    let mut state = State::default();
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
    assert!(page(&host, 2).contains("This device: 1.0000° N, 2.0000° E"));
    // Not allowed: the person is told where to allow it, and nothing else is tried.
    host.deliveries
        .push_back(event(3, json!({"t":"press","id":"share"})));
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
    state.own = Own::Found {
        shared: wire::position(&json!({"latitude":44.4268,"longitude":26.1025,"age":0})).unwrap(),
        at: 0,
    };
    host.incoming.push_back(wire::encode(&wire::Shared {
        fix: geo::Fix::new(46.7712, 23.6236, Some(30.0)).unwrap(),
        age: 60,
    }));
    host.deliveries
        .push_back(json!({"type":"call","ticket":9,"method":"receive","args":{"peer":"a1"}}));
    run_all(&mut host, &mut state);
    assert_eq!(host.opened, vec![(false, 9)]);
    let reply = host.sent.iter().find(|sent| sent["ticket"] == 9).unwrap();
    assert_eq!(reply["result"]["status"], "ok");
    host.deliveries
        .push_back(event(10, json!({"t":"tick","id":"clock","at":120_000})));
    run_all(&mut host, &mut state);
    let shown = page(&host, 10);
    // How far and which way from here, how sure, and how old: it was a minute old and two more went by.
    assert!(
        shown.contains("Phone — 324 km NW ± 30 m, 3 min ago"),
        "{shown}"
    );
    assert!(shown.contains("\"k\":\"drawing\""));
    // Forget: gone from the page.
    host.deliveries
        .push_back(event(11, json!({"t":"press","id":"forget"})));
    run_all(&mut host, &mut state);
    assert!(!page(&host, 11).contains("Phone —"));
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
    state.listed(&[peer("a1", "Phone", true)]);
    host.deliveries
        .push_back(event(1, json!({"t":"toggle","id":"keep","on":true})));
    run_all(&mut host, &mut state);
    let finds = |host: &Script| calls(host, LOCATION, "current").len();
    assert_eq!(finds(&host), 1);
    let find = calls(&host, LOCATION, "current")[0]["id"].as_u64().unwrap();
    host.deliveries.push_back(result(find, found(1.0, 2.0, 0)));
    run_all(&mut host, &mut state);
    for at in [1_000, 60_000, SHARE_EVERY - 1] {
        host.deliveries
            .push_back(event(2, json!({"t":"tick","id":"clock","at":at})));
        run_all(&mut host, &mut state);
    }
    assert_eq!(finds(&host), 1);
    host.deliveries
        .push_back(event(3, json!({"t":"tick","id":"clock","at":SHARE_EVERY})));
    run_all(&mut host, &mut state);
    assert_eq!(finds(&host), 2);
    host.deliveries
        .push_back(event(4, json!({"t":"toggle","id":"keep","on":false})));
    host.deliveries.push_back(event(
        5,
        json!({"t":"tick","id":"clock","at":3 * SHARE_EVERY}),
    ));
    run_all(&mut host, &mut state);
    assert_eq!(finds(&host), 2);
}

#[test]
fn a_share_written_waits_for_the_application_to_say_every_byte_arrived() {
    let mut host = Script::default();
    let mut state = State {
        own: Own::Found {
            shared: wire::position(&json!({"latitude":1.0,"longitude":2.0,"age":0})).unwrap(),
            at: 0,
        },
        ..State::default()
    };
    // The application says fewer bytes reached the other side than were written: not delivered.
    host.incoming.push_back(ack(3));
    state.outgoing.push((5, "a1".to_owned()));
    state.flush(&mut host);
    assert_eq!(state.delivered.get("a1"), Some(&false));
    assert_eq!(host.shutdowns, 1);
    assert_eq!(host.closed, 1);
}

#[test]
fn the_map_never_has_more_shapes_than_a_drawing_takes() {
    let mut state = State {
        own: Own::Found {
            shared: wire::position(
                &json!({"latitude":0.0,"longitude":0.0,"accuracy":500.0,"age":0}),
            )
            .unwrap(),
            at: 0,
        },
        ..State::default()
    };
    for n in 0..MOST_SEEN {
        let shared = wire::position(
            &json!({"latitude":n as f64 * 0.01,"longitude":0.5,"accuracy":800.0,"age":0}),
        )
        .unwrap();
        state.seen.insert(format!("d{n}"), Seen { shared, at: 0 });
    }
    let page = state.page();
    let drawing = find_drawing(&page).expect("no map");
    assert!(drawing["shapes"].as_array().unwrap().len() <= 32);
}

fn find_drawing(node: &Value) -> Option<&Value> {
    if node["k"] == "drawing" {
        return Some(node);
    }
    node["children"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(node.get("child"))
        .find_map(find_drawing)
}

#[test]
fn the_guest_answers_what_comes_in_order_and_ends_quietly_when_the_application_goes() {
    let mut host = Script::default();
    host.deliveries.push_back(event(1, json!({"t":"open"})));
    host.deliveries
        .push_back(json!({"type":"call","ticket":2,"method":"other","args":{}}));
    assert_eq!(run(&mut host), 0);
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
        state.seen.insert(alias.to_owned(), Seen { shared, at: 0 });
    }
    let mut no_longer = peer("b2", "Laptop", true);
    no_longer["allowed"] = json!(false);
    // c3 was unpaired: it is not listed at all. a1 is only offline: its last position stays.
    let mut offline = peer("a1", "Phone", false);
    offline["online"] = json!(false);
    offline["presence"] = json!("offline");
    state.listed(&[offline, no_longer]);
    assert_eq!(state.seen.keys().collect::<Vec<_>>(), ["a1"]);
    assert!(state.page().to_string().contains("Phone"));
    assert!(!state.page().to_string().contains("Laptop"));
    // Seeing paired devices is no longer allowed at all: nothing of any is shown.
    state.asked_list = 7;
    state.told(&result(7, json!({"status":"error","code":"denied"})));
    assert!(state.seen.is_empty());
    assert!(state.page().to_string().contains(words::DEVICES_DENIED));
}
