//! One button for each host call Test probes. Pressing it sends exactly that call and shows a short summary.
//! The location probe never shows the coordinates or repeats text supplied by the host.
use serde_json::{Value, json};

/// One call Test can make, by the id of its button.
pub struct Probe {
    pub id: &'static str,
    pub label: &'static str,
    /// What the application should do, in words the page shows beside the answer.
    pub expect: &'static str,
    pub service: &'static str,
    pub method: &'static str,
}

/// The location probe's button suffix and result discriminator.
pub const LOCATION: &str = "location";

/// Every probe, in the order the page shows them. Its service and method are in the manifest's `imports`;
/// the host binds any permission to that import and refuses calls that were not imported.
pub const PROBES: &[Probe] = &[
    Probe {
        id: "config",
        label: "Settings",
        expect: "Its own settings and the devices it was granted something for; no permission needed.",
        service: "peersus.config",
        method: "get",
    },
    Probe {
        id: "tos",
        label: "Licence accepted?",
        expect: "accepted: true, since it runs only after the licence and notice were accepted.",
        service: "peersus.extension",
        method: "tos",
    },
    Probe {
        id: "outside",
        label: "From outside the store?",
        expect: "Whether the person accepted the outside-store warning for it: false when installed from the store.",
        service: "peersus.extension",
        method: "outside_store",
    },
    Probe {
        id: "platform",
        label: "System",
        expect: "The operating system and processor, with platform.read; denied without it.",
        service: "peersus.platform",
        method: "inspect",
    },
    Probe {
        id: "language",
        label: "Language",
        expect: "The person's language as a tag (en, ro), with language.read; denied without it.",
        service: "peersus.language",
        method: "get",
    },
    Probe {
        id: LOCATION,
        label: "Check device location",
        expect: "With location.read, asks where this device is once. Shows whether a position was received and its accuracy, never its coordinates.",
        service: "peersus.location",
        method: "current",
    },
    Probe {
        id: "peers",
        label: "Paired devices",
        expect: "Only the devices it may see (peers.watch per device), with their pictures below; denied with none.",
        service: "peersus.peers",
        method: "list",
    },
    Probe {
        id: "plugins",
        label: "Other extensions",
        expect: "Ids and whether each runs, with plugins.watch; denied without it.",
        service: "peersus.plugins",
        method: "list",
    },
    Probe {
        id: "define",
        label: "Make a status",
        expect: "Defines or changes this device's status using the label above; denied without status.define.",
        service: "peersus.status",
        method: "define",
    },
    Probe {
        id: "set",
        label: "Show the status",
        expect: "Paired devices show this device's current status on their Devices page; denied without status.set.",
        service: "peersus.status",
        method: "set",
    },
    Probe {
        id: "clear",
        label: "Hide the status",
        expect: "This device's status is no longer shown on paired devices; denied without status.set.",
        service: "peersus.status",
        method: "clear",
    },
    Probe {
        id: "own",
        label: "My statuses",
        expect: "The statuses it made, and which are on; no permission needed.",
        service: "peersus.status",
        method: "own",
    },
];

/// The id of a probe's button on the page.
pub fn button(probe: &Probe) -> String {
    format!("probe-{}", probe.id)
}

/// The probe a pressed button asks for.
pub fn pressed(id: &str) -> Option<usize> {
    let wanted = id.strip_prefix("probe-")?;
    PROBES.iter().position(|probe| probe.id == wanted)
}

/// The arguments of a probe: what the host contract asks for.
pub fn args(probe: &Probe) -> Value {
    match probe.id {
        "define" => json!({"id":"testing","label":"Testing"}),
        "set" | "clear" => json!({"id":"testing"}),
        _ => json!({}),
    }
}

/// A short line for what the application answered: the value as compact JSON, cut to what a line holds, or the
/// refusal's code and words.
pub fn summary(result: &Value) -> String {
    let text = match result["status"].as_str() {
        Some("ok") => format!("ok: {}", result["value"]),
        Some("error") => format!(
            "refused ({}): {}",
            result["code"].as_str().unwrap_or("?"),
            result["message"].as_str().unwrap_or("")
        ),
        _ => "an answer that is no outcome".to_owned(),
    };
    cut(&text, 240)
}

/// Validate the host's position but return only a receipt and the accuracy, never any raw reply text.
pub fn location_summary(result: &Value) -> String {
    const MALFORMED: &str = "Malformed location answer.";
    match result["status"].as_str() {
        Some("ok") => {
            let position = &result["value"];
            if !position["latitude"]
                .as_f64()
                .is_some_and(|degrees| (-90.0..=90.0).contains(&degrees))
                || !position["longitude"]
                    .as_f64()
                    .is_some_and(|degrees| (-180.0..=180.0).contains(&degrees))
                || position["age"].as_u64().is_none()
            {
                return MALFORMED.into();
            }
            match position.get("accuracy") {
                Some(Value::Null) => "Position received; accuracy not reported.".into(),
                Some(Value::Number(accuracy))
                    if accuracy.as_f64().is_some_and(|m| m.is_finite() && m >= 0.0) =>
                {
                    format!("Position received; accuracy: {accuracy} m.")
                }
                _ => MALFORMED.into(),
            }
        }
        Some("error") if result["message"].is_string() => match result["code"].as_str() {
            Some("denied") => "Location permission not granted.".into(),
            Some("unavailable") => "Location is unavailable on this system.".into(),
            Some("timeout") => "No location answer in time.".into(),
            Some(_) => "Location request was refused.".into(),
            None => MALFORMED.into(),
        },
        _ => MALFORMED.into(),
    }
}

/// The test's own definitions in words instead of JSON, including whether each is visible.
pub fn own_summary(result: &Value) -> String {
    let Some(defined) = result["value"]
        .as_array()
        .filter(|_| result["status"] == "ok")
    else {
        return summary(result);
    };
    if defined.is_empty() {
        return "No statuses defined".into();
    }
    let entries: Option<Vec<String>> = defined
        .iter()
        .map(|entry| {
            Some(format!(
                "{} ({})",
                entry["label"].as_str()?,
                if entry["on"].as_bool()? { "on" } else { "off" }
            ))
        })
        .collect();
    // An entry that is not a label and whether it is on is the application not keeping its contract: the answer is
    // shown as it came, never with that entry left out.
    match entries {
        Some(entries) => cut(&entries.join(", "), 240),
        None => summary(result),
    }
}

/// At most `most` characters of `text`, with an ellipsis when it was cut.
pub fn cut(text: &str, most: usize) -> String {
    let mut chars = text.chars();
    let mut kept: String = chars.by_ref().take(most).collect();
    if chars.next().is_some() {
        kept.push('…');
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_probe_has_its_button_and_no_probe_changes_a_different_page() {
        for (index, probe) in PROBES.iter().enumerate() {
            assert_eq!(pressed(&button(probe)), Some(index));
        }
        assert_eq!(pressed("probe-nothing"), None);
        assert_eq!(pressed("count"), None);
        assert_eq!(pressed("probe-pages"), None);
        assert_eq!(pressed("probe-withhold"), None);
    }

    #[test]
    fn an_answer_is_summed_up_in_one_bounded_line() {
        assert_eq!(
            summary(&json!({"status":"ok","value":{"accepted":true}})),
            r#"ok: {"accepted":true}"#
        );
        assert_eq!(
            summary(&json!({"status":"error","code":"denied","message":"Not allowed"})),
            "refused (denied): Not allowed"
        );
        let long = summary(&json!({"status":"ok","value":"x".repeat(1000)}));
        assert_eq!(long.chars().count(), 241);
        assert!(long.ends_with('…'));
        assert_eq!(summary(&json!(7)), "an answer that is no outcome");
    }

    #[test]
    fn its_own_statuses_are_said_in_words_and_an_entry_that_is_not_one_is_shown_as_it_came() {
        let own = |value: Value| own_summary(&json!({"status":"ok","value":value}));
        assert_eq!(own(json!([])), "No statuses defined");
        assert_eq!(
            own(
                json!([{"id":"a","label":"Testing","on":true},{"id":"b","label":"Away","on":false}])
            ),
            "Testing (on), Away (off)"
        );
        // No `on`, alone or among entries that have it: nothing is left out, the whole answer is shown.
        assert_eq!(
            own(json!([{"label":"Testing"}])),
            r#"ok: [{"label":"Testing"}]"#
        );
        assert_eq!(
            own(json!([{"label":"Testing","on":true},{"label":7,"on":false}])),
            r#"ok: [{"label":"Testing","on":true},{"label":7,"on":false}]"#
        );
        assert_eq!(
            own_summary(&json!({"status":"error","code":"denied","message":"Not allowed"})),
            "refused (denied): Not allowed"
        );
    }
}
