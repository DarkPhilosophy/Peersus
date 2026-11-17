//! One button for each thing the application offers an extension to ask: pressing it sends exactly that call, and the
//! page says what came back. Nothing is interpreted beyond a short summary: a refusal is shown with its code, so a
//! permission the person has not granted reads as `denied`, which is what the application must answer.
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

/// Every probe, in the order the page shows them. Each service, method and permission here is in the manifest's
/// `imports`; a call the manifest did not import is refused by the hub before the application sees it.
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
        expect: "Defines its status 'Testing', with status.define; denied without it.",
        service: "peersus.status",
        method: "define",
    },
    Probe {
        id: "set",
        label: "Show the status",
        expect: "Paired devices that watch statuses see 'Testing' for this device, with status.set.",
        service: "peersus.status",
        method: "set",
    },
    Probe {
        id: "clear",
        label: "Hide the status",
        expect: "The status is no longer shown, with status.set.",
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
    Probe {
        id: "pages",
        label: "Also in Settings",
        expect: "A second copy of this page appears in Settings, or goes again on the next press; no permission needed.",
        service: "peersus.pages",
        method: "set",
    },
    Probe {
        id: "withhold",
        label: "Hide an action",
        expect: "'Count, with no description' leaves Settings › Binds, or comes back on the next press; no permission needed.",
        service: "peersus.actions",
        method: "withhold",
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

/// The arguments of a probe: what the contract asks for, and for the two that turn something on and off, which way now.
pub fn args(probe: &Probe, on: bool) -> Value {
    match probe.id {
        "define" => json!({"id":"testing","label":"Testing"}),
        "set" | "clear" => json!({"id":"testing"}),
        "pages" if on => {
            json!({"pages":[{"id":"test-settings","page":"test","placement":"settings"}]})
        }
        "pages" => json!({"pages":[]}),
        "withhold" if on => json!({"actions":["count-plain"]}),
        "withhold" => json!({"actions":[]}),
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
    fn every_probe_has_its_button_and_the_two_switches_alternate() {
        for (index, probe) in PROBES.iter().enumerate() {
            assert_eq!(pressed(&button(probe)), Some(index));
        }
        assert_eq!(pressed("probe-nothing"), None);
        assert_eq!(pressed("count"), None);
        let pages = &PROBES[pressed("probe-pages").unwrap()];
        assert_eq!(args(pages, true)["pages"][0]["placement"], "settings");
        assert_eq!(args(pages, false), json!({"pages":[]}));
        let withhold = &PROBES[pressed("probe-withhold").unwrap()];
        assert_eq!(args(withhold, true), json!({"actions":["count-plain"]}));
        assert_eq!(args(withhold, false), json!({"actions":[]}));
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
}
