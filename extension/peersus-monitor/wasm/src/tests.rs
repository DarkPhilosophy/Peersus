use super::*;

fn call(event: Value) -> Value {
    json!({"type":"call","ticket":1,"method":"event","args":{"event":event}})
}
fn ok(id: &Value, value: Value) -> Value {
    json!({"type":"result","id":id,"result":{"status":"ok","value":value}})
}
fn denied(id: &Value) -> Value {
    json!({"type":"result","id":id,"result":{"status":"error","code":"denied","message":"Not allowed"}})
}
/// The application's answer to a request for a stream to a device that cannot be sent to.
fn refused_send(id: &Value) -> Value {
    json!({"type":"result","id":id,"result":{"status":"error","code":"refused","message":"That device cannot be sent to"}})
}
fn readings(busy: u64, total: u64, received: u64) -> Value {
    json!({
        "processor":{"name":"Ryzen","cores":8,"busy":busy,"total":total,"celsius":55.4},
        "graphics":{"name":"AMD","busy":2500,"celsius":null},
        "memory":{"total":16_000_000_000u64,"available":4_000_000_000u64},
        "storage":{"total":1_000_000_000_000u64,"available":250_000_000_000u64},
        "network":{"received":received,"sent":0},
        "battery":{"charge":null,"status":null,"plugged":null,"minutes":null}
    })
}
/// The same readings from a device with a battery (a phone or a laptop).
fn with_battery(mut readings: Value) -> Value {
    readings["battery"] =
        json!({"charge":8100,"status":"discharging","plugged":false,"minutes":120});
    readings
}
thread_local! {
    /// The notifications Monitor asked the application to show, in order, as the helper below answered them.
    static NOTIFIED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Whether the helper answers a notification `denied`, as without the person's permission.
    static NOTIFY_DENIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// What Monitor asked to be shown since the last time this was read.
fn notified() -> Vec<String> {
    NOTIFIED.with(|said| std::mem::take(&mut *said.borrow_mut()))
}
/// Asks as `event` does and answers every question: the devices with `peers` (`None`: refused), each part with its
/// field of `readings` unless it is in `refused`, the settings with `config`, a notification as `NOTIFY_DENIED` says.
/// Returns what the answers led to ask.
fn answered(
    state: &mut State,
    event: Value,
    peers: Option<&Value>,
    readings: &Value,
    refused: &[&str],
    config: &Value,
) -> Vec<Value> {
    let mut more = Vec::new();
    for asked in state.ask(&call(event)) {
        let method = asked["method"].as_str().unwrap();
        let answer = match asked["service"].as_str() {
            Some("peersus.peers") if method == "send" => refused_send(&asked["id"]),
            Some("peersus.peers") => {
                peers.map_or_else(|| denied(&asked["id"]), |p| ok(&asked["id"], p.clone()))
            }
            Some("peersus.system") if refused.contains(&method) => denied(&asked["id"]),
            Some("peersus.system") => ok(&asked["id"], readings[method].clone()),
            Some("peersus.config") => ok(&asked["id"], config.clone()),
            Some("peersus.notify") if NOTIFY_DENIED.with(std::cell::Cell::get) => {
                denied(&asked["id"])
            }
            Some("peersus.notify") => {
                let text = asked["args"]["text"].as_str().unwrap().to_owned();
                NOTIFIED.with(|said| said.borrow_mut().push(text));
                ok(&asked["id"], json!({}))
            }
            other => panic!("asked {other:?}"),
        };
        more.extend(state.told(&answer));
    }
    more
}
fn tick(state: &mut State, at: u64, peers: Option<&Value>, readings: &Value, refused: &[&str]) {
    answered(
        state,
        json!({"t":"tick","id":"watch","at":at}),
        peers,
        readings,
        refused,
        &json!({"settings":{}}),
    );
}
/// A device chosen in the application's chooser (`None`: this device).
fn choose(state: &mut State, device: Option<&str>) -> Value {
    state.answer(&json!({"t":"choose","id":"device","device":device}))
}
fn peers() -> Value {
    json!([
        {"device":"a","name":"Desk","presence":"online"},
        {"device":"b","name":"Laptop","presence":"offline"},
        {"device":"c","name":"Phone","presence":"sleeping"}
    ])
}
fn page(state: &mut State) -> String {
    state
        .answer(&json!({"t":"tick","id":"watch","at":0}))
        .to_string()
}

#[test]
fn the_page_is_placed_among_the_pages_only_while_its_setting_asks_and_is_asked_once_per_change() {
    let mut state = State::default();
    let started = state.start();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["service"], "peersus.config");
    let set = |state: &mut State, id: &Value, on: Option<bool>| {
        let settings = on.map_or(json!({}), |on| json!({(PAGE_SETTING): on}));
        state.told(&ok(id, json!({"settings":settings})))
    };
    // Off, as no setting says otherwise: nothing is placed, which the application is told once.
    let placed = set(&mut state, &started[0]["id"], None);
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0]["service"], "peersus.pages");
    assert_eq!(placed[0]["args"]["pages"], json!([]));
    // Turned on: placed among the pages, then nothing more while it stays on.
    for (on, again) in [(true, true), (true, false), (false, true)] {
        let asked = state.ask(&call(json!({"t":"open"})));
        let config = asked
            .iter()
            .find(|a| a["service"] == "peersus.config")
            .unwrap();
        let placed = set(&mut state, &config["id"], Some(on));
        assert_eq!(placed.len(), usize::from(again), "{on} {again}");
        if again {
            let expected = if on {
                json!([{"id":"monitor-page","page":PAGE,"placement":"primary"}])
            } else {
                json!([])
            };
            assert_eq!(placed[0]["args"]["pages"], expected);
        }
    }
    // A placement the application refused is asked again with the next settings.
    let asked = state.ask(&call(json!({"t":"open"})));
    let config = asked
        .iter()
        .find(|a| a["service"] == "peersus.config")
        .unwrap();
    let placed = set(&mut state, &config["id"], Some(true));
    state.told(&json!({"type":"result","id":placed[0]["id"],"result":{"status":"error","code":"failed","message":"no"}}));
    let asked = state.ask(&call(json!({"t":"open"})));
    let config = asked
        .iter()
        .find(|a| a["service"] == "peersus.config")
        .unwrap();
    assert_eq!(set(&mut state, &config["id"], Some(true)).len(), 1);
}

#[test]
fn each_part_is_asked_on_its_own_and_not_again_while_its_answer_may_still_come() {
    let mut state = State::default();
    let first = state.ask(&call(json!({"t":"open","at":0})));
    let asked = |calls: &[Value]| -> Vec<String> {
        calls
            .iter()
            .filter(|a| a["service"] == "peersus.system")
            .map(|a| a["method"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        asked(&first),
        [
            "processor",
            "graphics",
            "memory",
            "storage",
            "network",
            "battery"
        ]
    );
    assert!(first.iter().any(|a| a["service"] == "peersus.peers"));
    // A tick while every answer may still come asks no part again, nor the devices, so a slow answer is not dropped.
    let second = state.ask(&call(json!({"t":"tick","id":"watch","at":1000})));
    assert!(asked(&second).is_empty());
    assert!(second.iter().all(|a| a["method"] != "list"));
    let memory = first.iter().find(|a| a["method"] == "memory").unwrap();
    state.told(&ok(&memory["id"], json!({"total":10,"available":5})));
    assert!(page(&mut state).contains("5 / 10 B"));
    // Answered: memory is asked at the next tick, the others still wait for theirs.
    let third = state.ask(&call(json!({"t":"tick","id":"watch","at":2000})));
    assert_eq!(asked(&third), ["memory"]);
    // A call whose time ran out is asked again, the part is not shown as read meanwhile, and an answer to the old call
    // after that is not taken.
    let fourth = state.ask(&call(json!({"t":"tick","id":"watch","at":5000})));
    assert!(asked(&fourth).contains(&"processor".to_owned()));
    assert_eq!(
        state.own.parts[Part::Processor as usize],
        Reading::Unavailable
    );
    let late = first.iter().find(|a| a["method"] == "processor").unwrap();
    state.told(&ok(&late["id"], json!({"name":"Late","cores":2})));
    assert_eq!(
        state.own.parts[Part::Processor as usize],
        Reading::Unavailable
    );
    // A delivery that is not a call of the page asks nothing.
    assert!(state.ask(&json!({"type":"result","id":1})).is_empty());
}

#[test]
fn a_reading_whose_next_question_ran_out_stops_showing_the_last_one() {
    let mut state = State::default();
    let first = state.ask(&call(json!({"t":"open","at":0})));
    let memory = first.iter().find(|a| a["method"] == "memory").unwrap();
    state.told(&ok(&memory["id"], json!({"total":10,"available":5})));
    assert!(page(&mut state).contains("5 / 10 B"));
    // Asked again at the next tick, and never answered.
    let again = state.ask(&call(json!({"t":"tick","id":"watch","at":1000})));
    let unanswered = again
        .iter()
        .find(|a| a["method"] == "memory")
        .unwrap()
        .clone();
    // Still within its time: the reading stays.
    state.ask(&call(json!({"t":"tick","id":"watch","at":5999})));
    assert!(page(&mut state).contains("5 / 10 B"));
    // Its time ran out: asked once more, and the old number is not shown as current.
    let after = state.ask(&call(json!({"t":"tick","id":"watch","at":6000})));
    assert!(after.iter().any(|a| a["method"] == "memory"));
    assert_eq!(state.own.parts[Part::Memory as usize], Reading::Unavailable);
    assert!(!page(&mut state).contains("5 / 10 B"));
    // An answer to the call that ran out is not taken.
    state.told(&ok(&unanswered["id"], json!({"total":10,"available":7})));
    assert_eq!(state.own.parts[Part::Memory as usize], Reading::Unavailable);
}

#[test]
fn a_battery_no_longer_readable_does_not_keep_showing_its_previous_state() {
    for result in [
        json!({"status":"error","code":"unavailable","message":"gone"}),
        json!({"status":"ok","value":false}),
        json!({"status":"ok","value":null}),
    ] {
        let mut state = State::default();
        let first = state.ask(&call(json!({"t":"open","at":0})));
        let battery = first.iter().find(|a| a["method"] == "battery").unwrap();
        state.told(&ok(
            &battery["id"],
            json!({"charge":8100,"status":"discharging","minutes":120}),
        ));
        assert!(page(&mut state).contains(r#""text":"Discharging""#));
        let again = state.ask(&call(json!({"t":"tick","id":"watch","at":1000})));
        let battery = again.iter().find(|a| a["method"] == "battery").unwrap();
        state.told(&json!({"type":"result","id":battery["id"],"result":result}));
        assert!(
            !page(&mut state).contains(r#""text":"Battery""#),
            "{result}"
        );
    }
}

#[test]
fn this_devices_numbers_are_bounded_like_another_devices() {
    let mut state = State::default();
    let first = state.ask(&call(json!({"t":"open","at":0})));
    let graphics = first.iter().find(|a| a["method"] == "graphics").unwrap();
    // A share past 100 % is not a share: dropped as it would be from a paired device, not overflowed.
    state.told(&ok(&graphics["id"], json!({"name":"GPU","busy":65535})));
    let page = page(&mut state);
    assert!(page.contains("GPU"));
    assert!(!page.contains("655"));
}

#[test]
fn a_part_not_allowed_says_so_in_its_card_and_the_others_still_show() {
    let mut state = State::default();
    tick(
        &mut state,
        1000,
        Some(&peers()),
        &readings(100, 1000, 5_000),
        &[],
    );
    tick(
        &mut state,
        2000,
        Some(&peers()),
        &readings(400, 2000, 25_000),
        &[],
    );
    assert!(state.own.busy.is_some() && !state.own.busy_history.is_empty());
    tick(
        &mut state,
        3000,
        Some(&peers()),
        &readings(500, 3000, 30_000),
        &["processor", "battery"],
    );
    let page = page(&mut state);
    assert_eq!(page.matches("Not allowed").count(), 2);
    assert!(page.contains("Allow it in this extension's permissions to see it here."));
    // What was seen of the processor is not kept once it may not be seen.
    assert!(state.own.busy.is_none() && state.own.busy_history.is_empty());
    assert!(page.contains("698.5 / 931.3 GiB") && page.contains("AMD"));
    // The clock is not a fresh request after a refusal. Reopening the page is.
    tick(
        &mut state,
        4000,
        Some(&peers()),
        &readings(600, 4000, 30_000),
        &[],
    );
    assert!(!page_has_no_refusal(&mut state));
    answered(
        &mut state,
        json!({"t":"open"}),
        Some(&peers()),
        &readings(700, 5000, 30_000),
        &[],
        &json!({"settings":{}}),
    );
    assert!(page_has_no_refusal(&mut state));
}

#[test]
fn a_checked_part_refusal_stops_only_that_part_even_after_a_grant_refresh() {
    let mut state = State::default();
    let open = call(json!({"t":"open"}));
    let config = |missing| json!({"plugin":"monitor","missing_permissions":missing,"peers":[]});
    let asked = state.ask(&open);
    let part = |method: &str| asked.iter().find(|call| call["method"] == method).unwrap();
    let gpu = json!({"owner":"host","name":"device.info.gpu"});
    // A fresh snapshot excludes GPU before its already pending reply can show it.
    state.told(&ok(&part("get")["id"], config(json!([gpu]))));
    state.told(&ok(&part("graphics")["id"], json!({"name":"Old graphics"})));
    let cpu = json!({"owner":"host","name":"device.info.cpu"});
    state.told(&json!({"type":"result","id":part("processor")["id"],"result":{"status":"missing-permission","permission":cpu}}));
    state.told(&ok(
        &part("memory")["id"],
        json!({"total":16_000_000_000u64,"available":4_000_000_000u64}),
    ));
    let shown = page(&mut state);
    assert!(shown.contains("missing permission device.info.gpu on this device"));
    assert!(shown.contains("missing permission device.info.cpu on this device"));
    assert!(!shown.contains("Old graphics"));
    assert!(shown.contains("11.2 / 14.9 GiB"));

    let changed = state.ask(&json!({"type":"event","provider":"host","service":"peersus.config","version":1,"event":"changed","data":{}}));
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0]["method"], "get");
    state.told(&ok(&changed[0]["id"], config(json!([]))));
    let tick = state.ask(&call(json!({"t":"tick","at":6000})));
    assert!(tick.iter().any(|call| call["method"] == "memory"));
    assert!(
        tick.iter()
            .all(|call| !matches!(call["method"].as_str(), Some("processor" | "graphics")))
    );
    let asked = state.ask(&open);
    for method in ["processor", "graphics"] {
        assert!(asked.iter().any(|call| call["method"] == method));
    }
}
fn page_has_no_refusal(state: &mut State) -> bool {
    !page(state).contains("Not allowed")
}

#[test]
fn rates_are_made_of_two_readings_and_nothing_is_invented_before_them() {
    let mut state = State::default();
    tick(
        &mut state,
        1000,
        Some(&peers()),
        &readings(100, 1000, 5_000),
        &[],
    );
    let page1 = page(&mut state);
    assert!(state.own.busy.is_none() && state.own.speed.is_none());
    assert!(page1.contains("Ryzen") && page1.contains("8 cores") && page1.contains("55°C"));
    assert!(page1.contains("AMD") && page1.contains("11.2 / 14.9 GiB"));
    assert!(page1.contains("698.5 / 931.3 GiB"));
    assert!(page1.contains(r#""text":"25%""#) && page1.contains(r#""text":"75%""#));
    // A device that knows nothing of a battery (a desktop) shows no battery card at all, rather than an empty one.
    assert!(!page1.contains("Battery"), "{page1}");
    tick(
        &mut state,
        3000,
        Some(&peers()),
        &readings(400, 2000, 25_000),
        &[],
    );
    assert_eq!(state.own.busy, Some(3000));
    assert_eq!(state.own.speed, Some((10_000, 0)));
    let page2 = page(&mut state);
    assert!(page2.contains(r#""text":"30%""#) && page2.contains("10 KB/s"));
    // Counters that went back (the system restarted) give no share, rather than a wrong one.
    tick(&mut state, 4000, Some(&peers()), &readings(10, 100, 1), &[]);
    assert_eq!((state.own.busy, state.own.speed), (None, Some((0, 0))));
}

#[test]
fn a_battery_says_its_charge_and_how_long_it_lasts_or_that_it_charges() {
    let mut state = State::default();
    let mut with = readings(1, 2, 3);
    with["battery"] = json!({"charge":4250,"status":"discharging","plugged":false,"minutes":135});
    tick(&mut state, 1000, Some(&peers()), &with, &[]);
    let page1 = page(&mut state);
    assert!(
        page1.contains(r#""text":"43%""#)
            && page1.contains(r#""text":"2 h 15 min""#)
            && page1.contains("Discharging")
    );
    with["battery"] = json!({"charge":1500,"status":"charging","plugged":true,"minutes":40});
    tick(&mut state, 2000, Some(&peers()), &with, &[]);
    let page2 = page(&mut state);
    assert!(page2.contains("Charging") && !page2.contains("Discharging"));
    // Low: the card is filled from the bottom in the warning colour.
    assert!(page2.contains(r#""background":"danger""#));
    assert!(!page1.contains(r#""background":"danger""#));
}

#[test]
fn a_battery_says_its_platform_status_even_without_a_charge_percentage() {
    for (status, plugged, expected) in [
        ("charging", Some(true), "Charging"),
        ("discharging", Some(true), "Discharging"),
        ("not_charging", Some(true), "Plugged in, not charging"),
        ("not_charging", Some(false), "Not charging"),
        ("not_charging", None, "Not charging"),
        ("full", Some(true), "Full"),
    ] {
        let mut state = State::default();
        let with = json!({"battery":{"status":status,"plugged":plugged}});
        tick(&mut state, 1000, Some(&peers()), &with, &[]);
        assert!(
            page(&mut state).contains(&format!(r#""text":"{expected}""#)),
            "{with}"
        );
    }
}

#[test]
fn a_battery_without_a_valid_status_never_claims_to_be_discharging_or_full() {
    for (mut battery, expected) in [
        (json!({}), "Status unknown"),
        (json!({"status":null}), "Status unknown"),
        (
            json!({"status":"unknown","plugged":false}),
            "Status unknown",
        ),
        (json!({"status":"Charging"}), "Status unknown"),
        (json!({"status":"Not charging"}), "Status unknown"),
        (json!({"status":2}), "Status unknown"),
        (json!({"status":false,"plugged":"true"}), "Status unknown"),
        (json!({"charging":false}), "Status unknown"),
        (json!({"charging":true}), "Status unknown"),
        (
            json!({"status":"unknown","plugged":true}),
            "Plugged in; status unknown",
        ),
    ] {
        let mut state = State::default();
        battery["charge"] = json!(10_000);
        battery["minutes"] = json!(135);
        tick(
            &mut state,
            1000,
            Some(&peers()),
            &json!({"battery":battery}),
            &[],
        );
        let drawn = page(&mut state);
        assert!(
            drawn.contains(&format!(r#""text":"{expected}""#)),
            "{battery}"
        );
        assert!(drawn.contains(r#""text":"100%""#), "{battery}");
        assert!(!drawn.contains(r#""text":"Discharging""#), "{battery}");
        assert!(!drawn.contains(r#""text":"Full""#), "{battery}");
        assert!(!drawn.contains(r#""text":"2 h 15 min""#), "{battery}");
    }
}

#[test]
fn the_chooser_is_the_applications_and_a_sleeping_device_can_be_chosen() {
    let mut state = State::default();
    tick(&mut state, 1000, Some(&peers()), &readings(1, 2, 3), &[]);
    let page1: Value = serde_json::from_str(&page(&mut state)).unwrap();
    fn chooser_in(node: &Value) -> Option<&Value> {
        if node["k"] == "devices" {
            return Some(node);
        }
        node["children"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(node.get("child"))
            .find_map(chooser_in)
    }
    // The application draws it: this device first, then a paired device connected, awake or asleep, allowed here and
    // where Monitor answers.
    let chooser = chooser_in(&page1).unwrap();
    assert_eq!(
        chooser,
        &json!({"k":"devices","id":"device","chosen":null,"this_device":true,
            "pick":{"presence":["online","sleeping"],"allowed":true,"accepts":"monitor.readings"}})
    );
    // An offline device cannot be chosen, whatever is asked.
    choose(&mut state, Some("b"));
    assert!(state.chosen.is_none());
    // A sleeping one can: it still answers.
    choose(&mut state, Some("c"));
    assert_eq!(state.chosen.as_deref(), Some("c"));
    choose(&mut state, Some("a"));
    assert_eq!(state.chosen.as_deref(), Some("a"));
    let page2 = page(&mut state);
    // The numbers of the chosen device are asked of it, not shown as if they were known.
    assert!(page2.contains("Asking Desk…"));
    // Nothing of this device is shown as if it were the other's.
    assert!(!page2.contains("Ryzen"));
    choose(&mut state, None);
    assert!(state.chosen.is_none());
}

#[test]
fn devices_not_allowed_leave_this_device_alone_and_say_why() {
    let mut state = State::default();
    tick(&mut state, 1000, Some(&peers()), &readings(1, 2, 3), &[]);
    choose(&mut state, Some("a"));
    tick(&mut state, 2000, None, &readings(1, 2, 3), &[]);
    assert!(state.devices.is_empty() && state.devices_denied && state.chosen.is_none());
    // Told through the application's notifications at the next tick, once; not written on the page.
    tick(&mut state, 3000, None, &readings(1, 2, 3), &[]);
    tick(&mut state, 4000, None, &readings(1, 2, 3), &[]);
    let told = notified();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("may no longer see it"));
    assert!(!page(&mut state).contains("may no longer see it"));
}

#[test]
fn a_problem_is_told_once_through_a_notification_and_written_on_the_page_only_without_that_permission()
 {
    let mut state = State::default();
    // Not allowed to see any paired device: one problem, told once.
    for at in [1000, 2000, 3000] {
        tick(&mut state, at, None, &readings(1, 2, 3), &[]);
    }
    assert_eq!(
        notified(),
        ["Paired devices are not shown: allow this extension to see them in its permissions."]
    );
    assert!(!page(&mut state).contains("Paired devices are not shown"));
    // Gone, then back: told again. Opening the page asks the devices at once.
    let open = |state: &mut State, peers: Option<&Value>| {
        answered(
            state,
            json!({"t":"open"}),
            peers,
            &readings(1, 2, 3),
            &[],
            &json!({"settings":{}}),
        );
    };
    open(&mut state, Some(&peers()));
    tick(&mut state, 5000, Some(&peers()), &readings(1, 2, 3), &[]);
    assert!(notified().is_empty());
    open(&mut state, None);
    tick(&mut state, 7000, None, &readings(1, 2, 3), &[]);
    assert_eq!(notified().len(), 1);
    // Without the permission to notify, the page says it from then on.
    let mut state = State::default();
    NOTIFY_DENIED.with(|denied| denied.set(true));
    for at in [1000, 2000, 3000] {
        tick(&mut state, at, None, &readings(1, 2, 3), &[]);
    }
    NOTIFY_DENIED.with(|denied| denied.set(false));
    assert!(notified().is_empty());
    assert!(page(&mut state).contains("Paired devices are not shown"));
}

#[test]
fn a_chosen_device_stays_chosen_asleep_and_is_unselected_offline_or_where_monitor_is_not() {
    let mut state = State::default();
    tick(&mut state, 1000, Some(&peers()), &readings(1, 2, 3), &[]);
    choose(&mut state, Some("a"));
    // While a device is chosen, the devices are asked every tick.
    let asleep = json!([{"device":"a","name":"Desk","presence":"sleeping"}]);
    tick(&mut state, 2000, Some(&asleep), &readings(1, 2, 3), &[]);
    assert_eq!(
        state.chosen.as_deref(),
        Some("a"),
        "asleep, it still answers"
    );
    let gone = json!([{"device":"a","name":"Desk","presence":"online","accepts":[]}]);
    tick(&mut state, 3000, Some(&gone), &readings(1, 2, 3), &[]);
    assert!(state.chosen.is_none());
    tick(&mut state, 3500, Some(&gone), &readings(1, 2, 3), &[]);
    assert_eq!(
        notified(),
        ["Desk was unselected: Monitor is not ready there."]
    );
    // Not chosen again while Monitor is not there.
    choose(&mut state, Some("a"));
    assert!(state.chosen.is_none());
    let offline = json!([{"device":"a","name":"Desk","presence":"offline"}]);
    tick(&mut state, 4000, Some(&offline), &readings(1, 2, 3), &[]);
    assert!(notified().is_empty(), "the same problem is not told twice");
    // Choosing again, once the page lists it there, clears what was said.
    answered(
        &mut state,
        json!({"t":"open"}),
        Some(&peers()),
        &readings(1, 2, 3),
        &[],
        &json!({"settings":{}}),
    );
    choose(&mut state, Some("a"));
    assert!(state.dropped.is_none());
    let offline = json!([{"device":"a","name":"Desk","presence":"offline"}]);
    tick(&mut state, 5000, Some(&offline), &readings(1, 2, 3), &[]);
    tick(&mut state, 6000, Some(&offline), &readings(1, 2, 3), &[]);
    assert_eq!(notified(), ["Desk was unselected: it is offline."]);
}

#[test]
fn the_cards_fit_the_page_whatever_its_width_and_graphs_are_as_wide_as_their_cards() {
    assert_eq!(Layout::of(None, true), Layout::Unmeasured);
    assert_eq!(Layout::Unmeasured.graph(Part::Network), None);
    // A phone 416 wide: the application's frame leaves 376, two columns as in the concept, drawn compact.
    let phone = Layout::of(Some(376), true);
    assert_eq!(
        phone,
        Layout::Grid {
            inner: 376,
            columns: 2,
            card: 182
        }
    );
    assert!(phone.compact() && !phone.tight());
    assert_eq!(phone.graph(Part::Processor), Some(182 - 2 * PADDING));
    // A card is its share of the row, never a fixed width: the two cards and the space between them fill the row.
    assert_eq!(
        phone.width(Part::Memory),
        json!({"unit":"percent","value":182 * 10_000 / 376})
    );
    // The narrowest phone in use here (335 wide, 295 inside the frame): still two columns, drawn tight.
    let narrow = Layout::of(Some(295), true);
    assert_eq!(
        narrow,
        Layout::Grid {
            inner: 295,
            columns: 2,
            card: 141
        }
    );
    assert!(narrow.tight());
    // A tablet held upright: four columns.
    assert_eq!(
        Layout::of(Some(676), true),
        Layout::Grid {
            inner: 676,
            columns: 4,
            card: 160
        }
    );
    // Narrower than two cards: one column, compact but not tight, as wide as the page.
    assert_eq!(
        Layout::of(Some(250), true),
        Layout::Grid {
            inner: 250,
            columns: 1,
            card: 250
        }
    );
    assert_eq!(
        Layout::of(Some(250), true).width(Part::Network),
        json!({"unit":"percent","value":10_000})
    );
    assert!(Layout::of(Some(250), true).compact() && !Layout::of(Some(250), true).tight());
    // A desktop 1280 wide: the battery beside, and the rows filling exactly what is left.
    let Layout::Wide {
        pair,
        storage,
        memory,
        network,
        ..
    } = Layout::of(Some(1256), true)
    else {
        panic!("a desktop page is wide");
    };
    let left = 1280 - 2 * GAP - BATTERY - GAP;
    assert_eq!(2 * pair + GAP, left);
    assert_eq!(storage + memory + network + 2 * GAP, left);
    assert!(memory < storage && storage < network);
    // The page as measured by the application, drawn: the battery is a card of its own at the right, and every card is
    // its share of what holds it.
    let mut state = State::default();
    state.answer(&json!({"t":"size","id":"width","width":1_256_000,"height":800_000}));
    tick(
        &mut state,
        1000,
        Some(&peers()),
        &with_battery(readings(1, 2, 3)),
        &[],
    );
    tick(
        &mut state,
        2000,
        Some(&peers()),
        &with_battery(readings(3, 4, 5)),
        &[],
    );
    tick(
        &mut state,
        3000,
        Some(&peers()),
        &with_battery(readings(5, 6, 7)),
        &[],
    );
    let page = page(&mut state);
    assert!(page.contains(&format!(
        r#"{{"unit":"percent","value":{}}}"#,
        network * 10_000 / left
    )));
    assert!(page.contains(&format!(r#""width":{}"#, pair - 2 * PADDING)));
}

#[test]
fn a_device_without_a_battery_has_no_battery_card_and_the_others_take_its_room() {
    // A desktop answers that it cannot read a battery: there is none, and an empty card would only take room.
    let Layout::Wide {
        left,
        pair,
        storage,
        memory,
        network,
        ..
    } = Layout::of(Some(1256), false)
    else {
        panic!("a desktop page is wide");
    };
    assert_eq!(left, 1256);
    assert_eq!(2 * pair + GAP, left);
    assert_eq!(storage + memory + network + 2 * GAP, left);
    let mut state = State::default();
    state.answer(&json!({"t":"size","id":"width","width":1_256_000,"height":800_000}));
    // A desktop answers nulls: nothing about a battery is known.
    tick(&mut state, 1000, Some(&peers()), &readings(1, 2, 3), &[]);
    let drawn = page(&mut state);
    assert!(!drawn.contains("Battery"), "{drawn}");
    // A battery that cannot be read is not a card either.
    state.own.unavailable(Part::Battery);
    assert!(!page(&mut state).contains("Battery"));
    // A battery read shows its card.
    tick(
        &mut state,
        2000,
        Some(&peers()),
        &with_battery(readings(3, 4, 5)),
        &[],
    );
    assert!(page(&mut state).contains("Battery"));
}

#[test]
fn quantities_are_written_as_a_person_reads_them() {
    assert_eq!(bytes(512), "512 B");
    assert_eq!(bytes(3_400_000_000), "3.4 GB");
    assert_eq!(bytes(25_000), "25 KB");
    assert_eq!(percent(4249), "42%");
    assert_eq!(percent(4250), "43%");
    assert_eq!(share(1, 0), None);
    assert_eq!(share(5, 3), Some(10000));
    assert_eq!(duration(45), "45 min");
    assert_eq!(duration(120), "2 h");
    assert_eq!(duration(135), "2 h 15 min");
}

#[test]
fn every_picture_is_one_the_application_accepts() {
    // The outline notation the application allows: its commands, numbers, spaces.
    let allowed = |path: &str| {
        path.bytes()
            .all(|b| b.is_ascii_digit() || b" .,-MmLlHhVvCcSsQqTtAaZz".contains(&b))
    };
    for part in PARTS {
        assert!(allowed(part.icon()), "{part:?}");
    }
    for drawn in [
        arc(Some(5000), 96, "title", "").to_string(),
        picture(THERMOMETER, "muted").to_string(),
        picture(DISK, "text").to_string(),
    ] {
        let value: Value = serde_json::from_str(&drawn).unwrap();
        let drawing = if value["k"] == "drawing" {
            &value
        } else {
            &value["children"][0]
        };
        for shape in drawing["shapes"].as_array().unwrap() {
            assert!(allowed(shape["path"].as_str().unwrap()));
        }
    }
}

/// The end of every arc of `path`, with the point it starts from.
fn arcs(path: &str) -> Vec<((f64, f64), (f64, f64))> {
    let words: Vec<&str> = path.split_whitespace().collect();
    let mut at = (0., 0.);
    let mut found = Vec::new();
    let mut index = 0;
    while index < words.len() {
        let number = |i: usize| words[i].parse::<f64>().unwrap();
        match words[index] {
            "M" | "L" => {
                at = (number(index + 1), number(index + 2));
                index += 3;
            }
            "A" => {
                let end = (number(index + 6), number(index + 7));
                found.push((at, end));
                at = end;
                index += 8;
            }
            other => panic!("{other} in {path}"),
        }
    }
    found
}

#[test]
fn an_open_ring_is_a_track_of_three_quarters_and_its_share_over_it() {
    for share in [
        None,
        Some(0),
        Some(1),
        Some(2500),
        Some(5000),
        Some(9995),
        Some(10000),
    ] {
        let ring = arc(share, 96, "title", "Used");
        let shapes = ring["children"][0]["shapes"].as_array().unwrap().clone();
        assert_eq!(shapes[0]["stroke"], "border", "{share:?}");
        let drawn = if share.unwrap_or(0) >= 10 { 2 } else { 1 };
        assert_eq!(shapes.len(), drawn, "{share:?}");
        for shape in &shapes {
            for (from, to) in arcs(shape["path"].as_str().unwrap()) {
                assert!(from != to, "{share:?}: an arc from {from:?} to itself");
            }
        }
    }
    // The track starts at the lower left and ends at the lower right; half of it ends at the top.
    let ends = |share| {
        let ring = arc(Some(share), 96, "title", "");
        *arcs(ring["children"][0]["shapes"][1]["path"].as_str().unwrap())
            .last()
            .unwrap()
    };
    let (start, whole) = ends(10000);
    assert!(start.0 < 48. && start.1 > 48. && whole.0 > 48. && whole.1 > 48.);
    assert!(
        (whole.1 - start.1).abs() < 0.2,
        "the gap is level at the bottom"
    );
    let (_, half) = ends(5000);
    assert!((half.0 - 48.).abs() < 0.2 && half.1 < 10.);
}

/// Every box of `node` and below, parent first.
fn boxes<'a>(node: &'a Value, found: &mut Vec<&'a Value>) {
    match node["k"].as_str() {
        Some("box") => {
            found.push(node);
            for child in node["children"].as_array().into_iter().flatten() {
                boxes(child, found);
            }
        }
        Some("interval") => boxes(&node["child"], found),
        _ => {}
    }
}
/// The page of this device in each state its cards take, as wide as a desktop and as a phone, and of a paired device
/// that answered, did not allow a part, was not answered yet, did not answer, or was not allowed.
fn pages() -> Vec<(String, Value)> {
    const EVERY: [&str; 6] = [
        "processor",
        "graphics",
        "memory",
        "storage",
        "network",
        "battery",
    ];
    let mut found = Vec::new();
    for width in [1_280_000u64, 400_000] {
        let measured = || {
            let mut state = State::default();
            state.answer(&json!({"t":"size","id":"width","width":width,"height":800_000}));
            state
        };
        let mut state = measured();
        found.push((format!("{width} not asked"), state.page()));
        for at in [1000, 2000, 3000] {
            tick(
                &mut state,
                at,
                Some(&peers()),
                &readings(at, at * 2, at * 5),
                &[],
            );
        }
        found.push((format!("{width} allowed"), state.page()));
        tick(&mut state, 4000, Some(&peers()), &readings(1, 2, 3), &EVERY);
        found.push((format!("{width} not allowed"), state.page()));
        // A paired device.
        let mut state = measured();
        tick(&mut state, 1000, Some(&peers()), &readings(1, 2, 3), &[]);
        choose(&mut state, Some("a"));
        found.push((format!("{width} device asked"), state.page()));
        let gauges = &mut state.talk.gauges;
        gauges.read(
            Part::Processor,
            json!({"name":"Ryzen","cores":8,"busy":1,"total":2,"celsius":55.0}),
            1000,
        );
        gauges.read(
            Part::Graphics,
            json!({"name":"AMD","busy":2500,"celsius":null}),
            1000,
        );
        gauges.read(Part::Memory, json!({"total":10,"available":4}), 1000);
        gauges.read(Part::Storage, json!({"total":10,"available":4}), 1000);
        gauges.read(Part::Network, json!({"received":5,"sent":4}), 1000);
        gauges.deny(Part::Battery);
        state.talk.answered = true;
        found.push((format!("{width} device answered"), state.page()));
        state.talk.silent = crate::remote::MOST_SILENT;
        found.push((format!("{width} device silent"), state.page()));
        state.talk.refused = true;
        found.push((format!("{width} device refused"), state.page()));
    }
    found
}

#[test]
fn a_row_that_centres_what_it_holds_and_holds_a_flexible_thing_has_a_width_of_its_own() {
    // The toolkit sizes a flexible thing to the room its row was offered, and a row with no width is only as wide as
    // what it holds, so centring moved the whole row out of its card, to the left by half of what the flexible thing
    // took: the title and picture of a card went out of it or out of sight, whatever the card said.
    let mut checked = 0;
    for (state, page) in pages() {
        let mut found = Vec::new();
        boxes(&page, &mut found);
        for row in found {
            let style = &row["style"];
            let flexible = row["children"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|child| child["style"]["width"]["unit"] == "flex");
            if style["horizontal"] == true && style["center"] == true && flexible {
                checked += 1;
                assert!(
                    matches!(
                        style["width"]["unit"].as_str(),
                        Some("percent" | "px" | "flex")
                    ),
                    "{state}: a centred row with a flexible thing in it has no width: {row}"
                );
            }
        }
    }
    // The rows that matter were there to be looked at: the title row of each card, and the ring row of two.
    assert!(checked >= 60, "{checked}");
}

#[test]
fn the_card_of_the_graphics_says_why_a_number_is_missing_and_not_before_it_is_known_to_be() {
    let mut state = State::default();
    // Before an answer nothing is known to be missing.
    assert!(!page(&mut state).contains("publish"));
    // A system that publishes nothing of its graphics says so, and where the application can read them.
    let mut said = readings(1, 2, 3);
    said["graphics"] = json!({"name":null,"busy":null,"celsius":null});
    tick(&mut state, 1000, Some(&peers()), &said, &[]);
    let page1 = page(&mut state);
    assert!(page1.contains("This device's system does not publish its graphics to the application: it reads them only on Linux and Android, from the graphics driver."));
    assert!(page1.contains(r#""text":"Not available""#));
    // A driver that gives who made the card but not how busy it is says that, and only that.
    said["graphics"] = json!({"name":"Intel","busy":null,"celsius":61.0});
    tick(&mut state, 2000, Some(&peers()), &said, &[]);
    let page2 = page(&mut state);
    assert!(page2.contains("This device's graphics driver does not publish how busy it is."));
    assert!(!page2.contains("does not publish its graphics"));
    // A whole answer needs no excuse.
    said["graphics"] = json!({"name":"AMD","busy":2500,"celsius":61.0});
    tick(&mut state, 3000, Some(&peers()), &said, &[]);
    assert!(!page(&mut state).contains("does not publish"));
    // Nor does a part that was not allowed: it was not told, so nothing is said of what it would have said.
    tick(&mut state, 4000, Some(&peers()), &said, &["graphics"]);
    let page4 = page(&mut state);
    assert!(page4.contains("Not allowed") && !page4.contains("does not publish"));
    // Said of a paired device, it names that device and not this one.
    tick(&mut state, 5000, Some(&peers()), &said, &[]);
    choose(&mut state, Some("a"));
    state.talk.gauges.read(
        Part::Graphics,
        json!({"name":null,"busy":null,"celsius":null}),
        1000,
    );
    state.talk.answered = true;
    let page5 = page(&mut state);
    assert!(page5.contains("Desk's system does not publish its graphics to the application"));
    assert!(!page5.contains("This device's system"));
}
