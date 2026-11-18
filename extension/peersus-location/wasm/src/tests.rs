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

/// A position told at the start of the page's clock, with no age and no accuracy.
fn told(latitude: f64, longitude: f64) -> Told {
    Told {
        shared: wire::position(&json!({"latitude":latitude,"longitude":longitude,"age":0}))
            .unwrap(),
        at: 0,
    }
}

/// A stream the application opened under `claim` for a share of `told` to `alias`, that the person pressed for.
fn sending(claim: u64, alias: &str, told: Told) -> Sending {
    Sending {
        number: claim,
        alias: alias.to_owned(),
        told,
        kept: false,
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
fn keep_sharing_waits_five_minutes_after_this_device_was_not_found_too() {
    let mut host = Script::default();
    let mut state = State::default();
    state.listed(&[peer("a1", "Phone", true)]);
    host.deliveries
        .push_back(event(1, json!({"t":"toggle","id":"keep","on":true})));
    run_all(&mut host, &mut state);
    let find = asked(&host, LOCATION, "current", 0);
    host.deliveries.push_back(result(
        find,
        json!({"status":"error","code":"unavailable","message":"x"}),
    ));
    for at in [1_000, 60_000, SHARE_EVERY - 1] {
        host.deliveries
            .push_back(event(2, json!({"t":"tick","id":"clock","at":at})));
    }
    run_all(&mut host, &mut state);
    assert_eq!(calls(&host, LOCATION, "current").len(), 1);
    host.deliveries
        .push_back(event(3, json!({"t":"tick","id":"clock","at":SHARE_EVERY})));
    run_all(&mut host, &mut state);
    assert_eq!(calls(&host, LOCATION, "current").len(), 2);
    // Nothing was shared: only a share that went out counts as one.
    assert_eq!(state.shared_at, None);
}

#[test]
fn keep_sharing_turned_off_while_this_device_is_found_sends_nothing_but_a_pressed_share_still_goes()
{
    let mut host = Script::default();
    let mut state = State::default();
    state.listed(&[peer("a1", "Phone", true)]);
    host.deliveries
        .push_back(event(1, json!({"t":"toggle","id":"keep","on":true})));
    host.deliveries
        .push_back(event(2, json!({"t":"toggle","id":"keep","on":false})));
    run_all(&mut host, &mut state);
    let find = asked(&host, LOCATION, "current", 0);
    host.deliveries.push_back(result(find, found(1.0, 2.0, 0)));
    run_all(&mut host, &mut state);
    // Found, and shown here, but the person turned Keep sharing off before: it goes nowhere.
    assert!(matches!(state.own, Own::Found(_)));
    assert!(calls(&host, PEERS, "send").is_empty());
    // The same, with Share pressed meanwhile: one question answers both, and turning Keep sharing off takes back only
    // its own.
    for (ticket, did) in [
        (3, json!({"t":"toggle","id":"keep","on":true})),
        (4, json!({"t":"press","id":"share"})),
        (5, json!({"t":"toggle","id":"keep","on":false})),
    ] {
        host.deliveries.push_back(event(ticket, did));
    }
    run_all(&mut host, &mut state);
    assert_eq!(calls(&host, LOCATION, "current").len(), 2);
    let find = asked(&host, LOCATION, "current", 1);
    host.deliveries.push_back(result(find, found(1.0, 2.0, 0)));
    run_all(&mut host, &mut state);
    assert_eq!(calls(&host, PEERS, "send").len(), 1);
}

#[test]
fn a_share_sends_the_position_it_is_of_whatever_this_device_is_found_to_be_since() {
    let mut host = Script {
        acks: true,
        ..Script::default()
    };
    let mut state = State::default();
    state.listed(&[peer("a1", "Phone", true)]);
    host.deliveries
        .push_back(event(1, json!({"t":"press","id":"share"})));
    run_all(&mut host, &mut state);
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
        host.deliveries
            .push_back(event(1, json!({"t":"press","id":"share"})));
        run_all(&mut host, &mut state);
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
    state.listed(&[peer("a1", "Phone", true)]);
    // Found for Keep sharing, the stream asked for; turned off before the application opened it.
    host.deliveries
        .push_back(event(1, json!({"t":"toggle","id":"keep","on":true})));
    run_all(&mut host, &mut state);
    host.deliveries.push_back(result(
        asked(&host, LOCATION, "current", 0),
        found(1.0, 2.0, 0),
    ));
    host.deliveries
        .push_back(event(2, json!({"t":"toggle","id":"keep","on":false})));
    run_all(&mut host, &mut state);
    host.deliveries.push_back(result(
        asked(&host, PEERS, "send", 0),
        json!({"status":"ok","value":{"claim":41}}),
    ));
    run_all(&mut host, &mut state);
    assert!(host.opened.is_empty());
    // A share the person pressed for is not Keep sharing's to take back: it goes although Keep sharing is off again.
    for (ticket, did) in [
        (3, json!({"t":"toggle","id":"keep","on":true})),
        (4, json!({"t":"press","id":"share"})),
    ] {
        host.deliveries.push_back(event(ticket, did));
    }
    run_all(&mut host, &mut state);
    host.deliveries.push_back(result(
        asked(&host, LOCATION, "current", 1),
        found(1.0, 2.0, 0),
    ));
    host.deliveries
        .push_back(event(5, json!({"t":"toggle","id":"keep","on":false})));
    run_all(&mut host, &mut state);
    host.deliveries.push_back(result(
        asked(&host, PEERS, "send", 1),
        json!({"status":"ok","value":{"claim":42}}),
    ));
    run_all(&mut host, &mut state);
    assert_eq!(host.opened, vec![(true, 42)]);
}

#[test]
fn the_map_never_has_more_shapes_than_a_drawing_takes() {
    let mut state = State {
        own: Own::Found(Told {
            shared: wire::position(
                &json!({"latitude":0.0,"longitude":0.0,"accuracy":500.0,"age":0}),
            )
            .unwrap(),
            at: 0,
        }),
        ..State::default()
    };
    for n in 0..MOST_SEEN {
        let shared = wire::position(
            &json!({"latitude":n as f64 * 0.01,"longitude":0.5,"accuracy":800.0,"age":0}),
        )
        .unwrap();
        state.seen.insert(format!("d{n}"), Told { shared, at: 0 });
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
        state.seen.insert(alias.to_owned(), Told { shared, at: 0 });
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
