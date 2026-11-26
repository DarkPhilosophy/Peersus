use super::*;
use crate::host::scripted::Script;

fn call(event: Value) -> Value {
    json!({"type":"call","ticket":3,"method":"event","args":{"event":event}})
}
fn press(id: &str) -> Value {
    call(json!({"t":"press","id":id}))
}
/// The kinds of every element in a tree, and the tones of its texts.
fn kinds(tree: &Value, found: &mut Vec<String>) {
    match tree {
        Value::Object(map) => {
            if let Some(kind) = map.get("k").and_then(Value::as_str) {
                found.push(kind.to_owned());
            }
            if let Some(tone) = map.get("tone").and_then(Value::as_str) {
                found.push(format!("tone:{tone}"));
            }
            map.values().for_each(|value| kinds(value, found));
        }
        Value::Array(items) => items.iter().for_each(|item| kinds(item, found)),
        _ => {}
    }
}
/// Answer every call the state sent with `result`, in order.
fn answer_all(state: &mut State, calls: &[Value], result: &Value) {
    for call in calls {
        state.told(&json!({"type":"result","id":call["id"],"result":result}));
    }
}

#[test]
fn the_page_holds_every_element_kind_and_tone_of_the_contract() {
    let mut state = State::new();
    // A paired device listed, so its picture is drawn too.
    let peers = state.calls_for(&press("probe-peers"));
    answer_all(
        &mut state,
        &peers,
        &json!({"status":"ok","value":[{"device":"desk","name":"Desk","online":true}]}),
    );
    let page = state.answer(&json!({"t":"open"}));
    let mut found = Vec::new();
    kinds(&page, &mut found);
    for kind in [
        "column",
        "row",
        "text",
        "button",
        "toggle",
        "field",
        "divider",
        "box",
        "avatar",
        "icon",
        "interval",
        "drawing",
        "devices",
        "tone:title",
        "tone:body",
        "tone:muted",
        "tone:figure",
    ] {
        assert!(found.iter().any(|seen| seen == kind), "{kind} is missing");
    }
    let drawn = page.to_string();
    assert!(drawn.contains(r#""lines":2"#) && drawn.contains(r#""observe":["size","wheel"]"#));
    for colour in [
        "surface", "raised", "text", "muted", "border", "scrim", "accent", "success", "danger",
    ] {
        assert!(
            drawn.contains(&format!(r#""background":"{colour}""#)),
            "{colour}"
        );
    }
}

#[test]
fn each_probe_sends_exactly_its_call_and_the_page_shows_the_answer() {
    let mut state = State::new();
    for (index, probe) in probes::PROBES.iter().enumerate() {
        let calls = state.calls_for(&press(&probes::button(probe)));
        assert_eq!(calls.len(), 1, "{}", probe.id);
        let sent = &calls[0];
        assert_eq!(
            (
                sent["provider"].as_str(),
                sent["service"].as_str(),
                sent["method"].as_str()
            ),
            (Some("host"), Some(probe.service), Some(probe.method))
        );
        assert_eq!(sent["args"], probes::args(probe), "{}", probe.id);
        assert!(state.page().to_string().contains("Asked…"));
        state.told(&json!({"type":"result","id":sent["id"],"result":{"status":"error","code":"denied","message":"Not allowed"}}));
        assert_eq!(
            state.probed[index].as_deref(),
            Some(if probe.id == "location" {
                "Location permission not granted."
            } else {
                "refused (denied): Not allowed"
            })
        );
    }
    assert!(!state.waiting(), "every call was answered");
    // An answer to a call this extension never made changes nothing.
    state.told(&json!({"type":"result","id":999,"result":{"status":"ok","value":1}}));
    assert!(state.calls.is_empty());
}

#[test]
fn permission_readiness_blocks_only_that_probe_and_never_repeats_it_on_change() {
    let mut state = State::new();
    let permission = json!({"owner":"host","name":"location.read"});
    let config = |missing: Value| json!({"status":"ok","value":{"plugin":"test","settings":{},"missing_permissions":missing,"peers":[]}});
    let initial = state.ask_config();
    answer_all(&mut state, &[initial], &config(json!([permission])));
    assert!(state.calls_for(&press("probe-location")).is_empty());
    let page = state.page().to_string();
    assert!(
        page.contains("Check device location: missing permission location.read on this device.")
    );
    assert!(page.contains("missing-permission"));
    assert_eq!(
        state.calls_for(&press("probe-platform"))[0]["method"],
        "inspect"
    );
    assert!(
        state
            .calls_for(&call(json!({"t":"tick","id":"clock"})))
            .is_empty()
    );

    let changed = state.calls_for(&json!({
        "type":"event","provider":"host","service":"peersus.config","version":1,"event":"changed","data":{}
    }));
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0]["method"], "get");
    assert!(
        state
            .told(&json!({"type":"result","id":changed[0]["id"],"result":config(json!([]))}))
            .is_none()
    );
    assert_eq!(
        state.calls_for(&press("probe-location"))[0]["method"],
        "current"
    );
}

#[test]
fn checked_probe_refusals_are_named_states_instead_of_malformed_answers() {
    let page = location_probe_page(json!({
        "status":"missing-permission","permission":{"owner":"host","name":"location.read"}
    }));
    assert!(page.contains("location.read on this device") && page.contains("missing-permission"));
    assert!(!page.contains("Malformed location answer."));
    let page = location_probe_page(json!({"status":"not-accepted"}));
    assert!(page.contains("not-accepted"));
    assert!(!page.contains("Malformed location answer."));
}

fn location_probe_page(result: Value) -> String {
    let mut host = Script::default();
    let mut state = State::new();
    step(&mut host, &mut state, &press("probe-location")).unwrap();
    assert_eq!(host.sent.len(), 2, "one host call, then the page reply");
    let id = &host.sent[0]["id"];
    assert_eq!(
        host.sent[0],
        json!({"op":"call","id":id,"provider":"host","service":"peersus.location",
            "version":1,"method":"current","args":{},"timeout_ms":20_000})
    );
    assert!(host.sent[1].to_string().contains("Asked…"));
    let result = json!({"type":"result","id":id,"result":result});
    step(&mut host, &mut state, &result).unwrap();
    assert_eq!(
        host.sent.len(),
        2,
        "receiving a fix or refusal sends nothing"
    );
    assert!(!state.waiting());
    state.page().to_string()
}

#[test]
fn location_reports_only_receipt_and_accuracy_of_a_valid_position() {
    let page = location_probe_page(
        json!({"status":"ok","value":{"latitude":12.345678,"longitude":-98.765432,"accuracy":14.5,"age":2}}),
    );
    assert!(page.contains("Position received; accuracy: 14.5 m."));
    assert!(!page.contains("12.345678") && !page.contains("-98.765432"));
}

#[test]
fn location_accepts_boundary_coordinates_and_explicitly_unknown_accuracy() {
    let page = location_probe_page(
        json!({"status":"ok","value":{"latitude":-90,"longitude":180,"accuracy":null,"age":0}}),
    );
    assert!(page.contains("Position received; accuracy not reported."));
}

#[test]
fn location_reports_permission_not_granted_without_echoing_host_text() {
    let page = location_probe_page(
        json!({"status":"error","code":"denied","message":"private host text"}),
    );
    assert!(page.contains("Location permission not granted."));
    assert!(!page.contains("private host text"));
}

#[test]
fn location_reports_unavailable_on_this_system() {
    let page = location_probe_page(
        json!({"status":"error","code":"unavailable","message":"No system fix"}),
    );
    assert!(page.contains("Location is unavailable on this system."));
}

#[test]
fn location_reports_malformed_answers_without_exposing_the_payload() {
    for value in [
        json!("private host text"),
        json!({"latitude": "12.345678", "longitude": 0, "accuracy": 4, "age": 0}),
        json!({"latitude": 90.00001, "longitude": 0, "accuracy": 4, "age": 0}),
        json!({"latitude": 0, "longitude": -180.00001, "accuracy": 4, "age": 0}),
        json!({"latitude": 0, "longitude": 0, "accuracy": -1, "age": 0}),
        json!({"latitude": 0, "longitude": 0, "accuracy": "private host text", "age": 0}),
        json!({"latitude": 0, "longitude": 0, "accuracy": 4, "age": -1}),
        json!({"latitude": 0, "longitude": 0, "accuracy": 4, "age": 0.5}),
        json!({"latitude": 0, "longitude": 0, "age": 0}),
    ] {
        let page = location_probe_page(json!({"status":"ok","value":value}));
        assert!(page.contains("Malformed location answer."));
        assert!(!page.contains("private host text") && !page.contains("12.345678"));
    }
    for result in [Value::Null, json!({"status":"error","code":"denied"})] {
        assert!(location_probe_page(result).contains("Malformed location answer."));
    }
}

#[test]
fn location_reports_the_hub_deadline_expiring() {
    let page = location_probe_page(
        json!({"status":"error","code":"timeout","message":"The request timed out"}),
    );
    assert!(page.contains("No location answer in time."));
}

#[test]
fn location_reports_other_refusals_without_echoing_host_text() {
    let page = location_probe_page(
        json!({"status":"error","code":"rate-limited","message":"private host text"}),
    );
    assert!(page.contains("Location request was refused."));
    assert!(!page.contains("private host text"));
}

#[test]
fn status_text_can_be_defined_changed_and_reset_without_changing_its_identity() {
    let mut state = State::new();
    let define = state.calls_for(&press("probe-define"));
    assert_eq!(define[0]["args"], json!({"id":"testing","label":"Testing"}));
    answer_all(
        &mut state,
        &define,
        &json!({"status":"ok","value":{"id":"testing","label":"Testing"}}),
    );
    let custom = state.calls_for(&call(
        json!({"t":"submit","id":"status-label","text":"Reviewing"}),
    ));
    assert_eq!(custom[0]["service"], "peersus.status");
    assert_eq!(custom[0]["method"], "define");
    assert_eq!(
        custom[0]["args"],
        json!({"id":"testing","label":"Reviewing"})
    );
    answer_all(
        &mut state,
        &custom,
        &json!({"status":"ok","value":{"id":"testing","label":"Reviewing"}}),
    );
    assert!(state.page().to_string().contains("Reviewing"));
    let show = state.calls_for(&press("probe-set"));
    assert_eq!(show[0]["args"], json!({"id":"testing"}));
    let hide = state.calls_for(&press("probe-clear"));
    assert_eq!(hide[0]["args"], show[0]["args"]);
    let reset = state.calls_for(&press("reset-status"));
    assert_eq!(reset[0]["args"], define[0]["args"]);
    answer_all(
        &mut state,
        &reset,
        &json!({"status":"ok","value":{"id":"testing","label":"Testing"}}),
    );
    assert!(state.page().to_string().contains("Testing"));
}

#[test]
fn expanding_content_and_added_rows_stay_beside_their_controls() {
    let mut state = State::new();
    state.answer(&json!({"t":"toggle","id":"reveal","on":true}));
    state.answer(&json!({"t":"press","id":"grow"}));
    let page = state.page();
    let elements = &page["children"][2]["children"];
    let location = elements
        .as_array()
        .unwrap()
        .iter()
        .position(|node| node.to_string().contains(r#""id":"grow""#))
        .unwrap();
    let area = &elements[location];
    assert!(
        area.to_string().contains("Row 1"),
        "added row must stay in Elements, beside Add a row"
    );
    assert!(
        area.to_string()
            .contains("This grew out of the switch above."),
        "reveal must stay in the same Elements area as its switch"
    );
}

#[test]
fn pointer_and_wheel_feedback_are_drawn_in_the_pad_card_not_in_another_section() {
    let state = State::new();
    let page = state.page();
    let elements = &page["children"][2];
    let touch = &page["children"][3];
    assert!(!elements.to_string().contains(r#""k":"drawing""#));
    let feedback_area = &touch["children"][2];
    assert!(feedback_area.to_string().contains(r#""id":"pad""#));
    assert!(
        feedback_area.to_string().contains(r#""k":"drawing""#),
        "the drawing belongs beside the pad on wide screens, underneath on narrow screens"
    );
}

#[test]
fn failed_status_change_leaves_the_last_accepted_label_and_own_reports_on_off_in_words() {
    let mut state = State::new();
    let define = state.calls_for(&press("probe-define"));
    answer_all(
        &mut state,
        &define,
        &json!({"status":"ok","value":{"id":"testing","label":"Testing"}}),
    );
    let bad = state.calls_for(&call(json!({"t":"submit","id":"status-label","text":""})));
    answer_all(
        &mut state,
        &bad,
        &json!({"status":"error","code":"refused","message":"invalid label"}),
    );
    assert_eq!(state.status_label.as_deref(), Some("Testing"));
    assert!(
        state
            .page()
            .to_string()
            .contains("refused (refused): invalid label")
    );
    let own = state.calls_for(&press("probe-own"));
    answer_all(
        &mut state,
        &own,
        &json!({"status":"ok","value":[
            {"plugin":"test","id":"testing","label":"Testing","on":false}
        ]}),
    );
    let index = probes::pressed("probe-own").unwrap();
    assert_eq!(state.probed[index].as_deref(), Some("Testing (off)"));
}

#[test]
fn the_misbehaving_buttons_follow_the_setting() {
    let mut state = State::new();
    let start = state.ask_config();
    assert!(
        !state.page().to_string().contains(r#""id":"spin""#),
        "hidden until the setting says"
    );
    answer_all(
        &mut state,
        &[start],
        &json!({"status":"ok","value":{"settings":{"show_trouble":true}}}),
    );
    assert!(!state.page().to_string().contains(r#""id":"spin""#));
    let start = state.ask_config();
    state.told(&json!({"type":"result","id":start["id"],"result":{"status":"ok","value":{"settings":{"show-trouble":true}}}}));
    let page = state.page().to_string();
    for (_, id, _, expect) in trouble::TROUBLES {
        assert!(
            page.contains(&format!(r#""id":"{id}""#)) && page.contains(expect),
            "{id}"
        );
    }
    // Opening the page reads the settings again, so turning it off shows.
    let calls = state.calls_for(&call(json!({"t":"open"})));
    assert_eq!(calls[0]["service"], "peersus.config");
    answer_all(
        &mut state,
        &calls,
        &json!({"status":"ok","value":{"settings":{"show-trouble":false}}}),
    );
    assert!(!state.page().to_string().contains(r#""id":"spin""#));
}

#[test]
fn the_page_is_among_the_applications_pages_only_while_the_persons_option_is_on() {
    let mut state = State::new();
    let said = |state: &mut State, settings: Value| {
        let asked = state.ask_config();
        state.told(&json!({"type":"result","id":asked["id"],"result":{"status":"ok","value":{"settings":settings}}}))
    };
    // Never set: nothing is placed, and that is said.
    let placing = said(&mut state, json!({})).unwrap();
    assert_eq!(placing["service"], "peersus.pages");
    assert_eq!(placing["args"]["pages"], json!([]));
    // Turned on: placed from the declared page; said again unchanged, nothing more.
    let placing = said(&mut state, json!({PAGE_SETTING: true})).unwrap();
    assert_eq!(
        placing["args"]["pages"],
        json!([{"id":PAGE_SETTING,"page":PAGE,"placement":"primary"}])
    );
    assert!(said(&mut state, json!({PAGE_SETTING: true})).is_none());
    // Refused by the application: the next answer places it again.
    state.told(
        &json!({"type":"result","id":placing["id"],"result":{"status":"error","code":"refused"}}),
    );
    assert!(said(&mut state, json!({PAGE_SETTING: true})).is_some());
    // Turned off: taken out.
    let placing = said(&mut state, json!({PAGE_SETTING: false})).unwrap();
    assert_eq!(placing["args"]["pages"], json!([]));
}

#[test]
fn touches_sizes_wheels_ticks_and_choices_change_the_page() {
    let mut state = State::new();
    state
        .answer(&json!({"t":"size","id":"pad","x":10000,"y":20000,"width":200000,"height":120000}));
    let page = state
        .answer(&json!({"t":"pointer","id":"pad","phase":"down","x":110000,"y":30000,"at":5}))
        .to_string();
    assert!(
        page.contains("Measured 200 × 120 at 10, 20")
            && page.contains("Pointer down at 110.0, 30.0")
    );
    assert_eq!(state.pointer_share, Some(5000), "half across the pad");
    assert!(page.contains(r#""stroke":"accent""#), "the ring shows it");
    let page = state
        .answer(&json!({"t":"wheel","id":"pad","x":0,"y":0,"dx":0,"dy":-3000,"at":6}))
        .to_string();
    assert!(page.contains("Wheel 0, -3000") && page.contains(r#""stroke":"text""#));
    state.answer(&json!({"t":"tick","id":"clock","at":1000}));
    assert!(state.page().to_string().contains("ticked 1 times"));
    assert!(
        state
            .answer(&json!({"t":"choose","id":"chooser","device":"desk"}))
            .to_string()
            .contains("Chosen: desk")
    );
    assert!(
        state
            .answer(&json!({"t":"choose","id":"chooser"}))
            .to_string()
            .contains("Chosen: this device")
    );
    let page = state
        .answer(&json!({"t":"submit","id":"say","text":"hi"}))
        .to_string();
    assert!(page.contains("You sent: hi"));
    assert_eq!(page::flood().to_string().matches("column").count(), 40);
}

#[test]
fn a_press_or_a_bound_key_asks_for_a_notification_and_the_page_says_what_was_answered() {
    let mut state = State::new();
    let inside = state.calls_for(&json!({"type":"call","ticket":5,"method":"notify","args":null}));
    assert!(inside[0]["args"].get("reach").is_none());
    let outside = state.calls_for(&press("question"));
    assert_eq!(outside[0]["args"]["reach"], "outside");
    assert_eq!(outside[0]["args"]["buttons"].as_array().unwrap().len(), 2);
    assert_eq!((state.asked, state.notifying), (2, 2));
    assert!(
        state.page().to_string().contains(r#""id":"answers""#),
        "redrawn by itself while waiting"
    );
    state.told(&json!({"type":"result","id":outside[0]["id"],"result":{"status":"ok","value":{"answer":"pressed","button":"that"}}}));
    state.told(&json!({"type":"result","id":inside[0]["id"],"result":{"status":"error","code":"denied","message":"no"}}));
    let page = state.page().to_string();
    assert!(
        page.contains("Last answer: refused (denied) (0 waiting)")
            && !page.contains(r#""id":"answers""#)
    );
    assert!(
        state
            .calls_for(&json!({"type":"call","ticket":5,"method":"bump","args":null}))
            .is_empty()
    );
}

#[test]
fn a_bound_action_counts_and_anything_unknown_is_refused() {
    let mut state = State::new();
    let manifest: Value = serde_json::from_str(include_str!("../plugin.json")).unwrap();
    // Peersus stores the action ID in the binding, not the exported method.
    let bound = manifest["contributions"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["id"] == "count-key")
        .expect("the existing count-key binding still resolves");
    let bump = json!({"type":"call","ticket":9,"service":bound["target"]["service"],
        "method":bound["target"]["method"],"args":null});
    assert_eq!(handle(&mut state, &bump).unwrap()["result"]["status"], "ok");
    handle(&mut state, &bump);
    assert!(state.page().to_string().contains("Run by a key 2 times"));
    let other = json!({"type":"call","ticket":1,"method":"nothing","args":null});
    assert_eq!(
        handle(&mut state, &other).unwrap()["result"]["status"],
        "error"
    );
    assert_eq!(handle(&mut state, &json!({"type":"result","id":1})), None);
}

#[test]
fn the_loop_answers_what_it_should_and_leaves_only_on_purpose() {
    let mut host = Script::default();
    let mut state = State::new();
    step(&mut host, &mut state, &press("probe-tos")).unwrap();
    assert_eq!(host.sent.len(), 2, "the probe's call, then the page");
    assert_eq!(host.sent[0]["service"], "peersus.extension");
    assert_eq!(host.sent[1]["op"], "reply");
    let mut host = Script::default();
    step(&mut host, &mut state, &press("late")).unwrap();
    assert!(host.sent.is_empty(), "never answered");
    assert_eq!(
        step(&mut host, &mut state, &press("leave")),
        Err(End::Leave)
    );
    let mut host = Script::default();
    step(&mut host, &mut state, &press("spam")).unwrap();
    assert_eq!(
        host.sent.last().unwrap()["op"],
        "reply",
        "answered after the flood"
    );
}

#[cfg(not(target_family = "wasm"))]
#[test]
fn the_native_guest_refuses_interpreter_tests_in_the_page_and_keeps_answering() {
    for id in ["spin", "fill"] {
        let mut host = Script::default();
        let mut state = State::new();
        state.show_trouble = true;
        step(&mut host, &mut state, &press(id)).unwrap();
        assert_eq!(host.sent.len(), 1, "{id} returned a single page");
        assert_eq!(host.sent[0]["op"], "reply");
        assert_eq!(host.sent[0]["ticket"], 3);
        assert_eq!(host.sent[0]["result"]["status"], "ok");
        assert!(
            host.sent[0]["result"]["value"]
                .to_string()
                .contains("Test did not run that case: the native library and process have no WebAssembly interpreter fuel or memory limits.")
        );

        step(&mut host, &mut state, &press("count")).unwrap();
        assert_eq!(state.pressed, 1);
        assert_eq!(host.sent.len(), 2);
        assert_eq!(host.sent[1]["result"]["status"], "ok");
    }
}

#[test]
fn the_fullest_page_stays_well_within_one_message() {
    let mut state = State::new();
    state.show_trouble = true;
    state.rows = 12;
    state.reveal = true;
    state.peers = (0..6)
        .map(|n| (format!("device-{n}"), "N".repeat(80)))
        .collect();
    state.probed = vec![Some("x".repeat(241)); probes::PROBES.len()];
    state.wheel_steps = vec![30; WHEEL_STEPS];
    state.pointer_share = Some(9_999);
    let bytes = serde_json::to_vec(
        &json!({"op":"reply","ticket":1,"result":{"status":"ok","value":state.page()}}),
    )
    .unwrap();
    assert!(bytes.len() < 40 * 1024, "{} bytes", bytes.len());
}

#[test]
fn the_manifest_imports_every_call_and_declares_the_setting() {
    let manifest: Value = serde_json::from_str(include_str!("../plugin.json")).unwrap();
    // Hosts that render declared placements directly must not show Test in navigation before opt-in.
    let pages = manifest["contributions"]["pages"].as_array().unwrap();
    assert!(
        pages
            .iter()
            .any(|p| p["id"] == PAGE && p["placement"] == "settings")
    );
    assert!(!pages.iter().any(|p| p["placement"] == "primary"));
    let imported = |service: &str, method: &str| {
        manifest["imports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|import| {
                import["provider"] == "host"
                    && import["service"] == service
                    && import["methods"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|m| m == method)
            })
    };
    for probe in probes::PROBES {
        assert!(
            imported(probe.service, probe.method),
            "{}.{}",
            probe.service,
            probe.method
        );
    }
    assert!(imported("peersus.config", "get") && imported("peersus.notify", "show"));
    assert!(imported("peersus.pages", "set"));
    let settings = manifest["contributions"]["settings"].as_array().unwrap();
    assert!(
        settings
            .iter()
            .any(|setting| setting["id"] == SHOW_TROUBLE && setting["kind"]["type"] == "toggle")
    );
    assert_eq!(manifest["package"]["version"], env!("CARGO_PKG_VERSION"));
}
