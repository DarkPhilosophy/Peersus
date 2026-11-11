//! A page of its own, drawn by the application from plain elements. It tries each element and each
//! event, and the limits: one button answers with a tree far too large, which the application must
//! refuse while it keeps showing the page as it was. A second set of buttons makes this extension
//! misbehave on purpose (flood the application, answer nonsense, answer never, spin, fill its
//! memory, leave): whatever it does, only this extension may suffer for it.
use serde_json::{Value, json};

#[derive(Default)]
pub struct State {
    pressed: u32,
    on: bool,
    said: String,
    rows: u32,
    spammed: u32,
}

/// A way to misbehave that a press asks for. None of them is an answer: each is something the
/// application must survive, and the entry point does it instead of answering.
#[derive(Debug, PartialEq, Eq)]
pub enum Trouble {
    /// Thousands of operations nobody asked for, and then the ordinary answer.
    Spam,
    /// Bytes that are no operation, an operation that does not exist, an answer to a call that was
    /// never delivered, and then an answer that is no page.
    Nonsense,
    /// No answer to this call, ever. Later calls are answered.
    Late,
    /// Work without end inside one turn.
    Spin,
    /// Memory taken until there is no more.
    Fill,
    /// The entry point returns an error.
    Leave,
}

/// How many pairs of operations one press of "Flood" sends.
pub const SPAM: u32 = 2000;

/// The trouble this delivery asks for, if it is a press of one of those buttons.
pub fn trouble(delivery: &Value) -> Option<Trouble> {
    let event = &delivery["args"]["event"];
    if delivery["type"] != "call" || delivery["method"] != "event" || event["t"] != "press" {
        return None;
    }
    match event["id"].as_str()? {
        "spam" => Some(Trouble::Spam),
        "nonsense" => Some(Trouble::Nonsense),
        "late" => Some(Trouble::Late),
        "spin" => Some(Trouble::Spin),
        "fill" => Some(Trouble::Fill),
        "leave" => Some(Trouble::Leave),
        _ => None,
    }
}

impl State {
    /// The page after `event`.
    pub fn answer(&mut self, event: &Value) -> Value {
        match (event["t"].as_str(), event["id"].as_str()) {
            (Some("press"), Some("count")) => self.pressed += 1,
            (Some("press"), Some("grow")) => self.rows = (self.rows + 1).min(12),
            (Some("press"), Some("clear")) => self.rows = 0,
            (Some("press"), Some("flood")) => return flood(),
            (Some("toggle"), Some("switch")) => self.on = event["on"].as_bool().unwrap_or(false),
            (Some("submit"), Some("say")) => {
                self.said = event["text"].as_str().unwrap_or_default().chars().take(200).collect()
            }
            (Some("press"), Some("spam")) => self.spammed += 1,
            _ => {}
        }
        self.page()
    }

    fn page(&self) -> Value {
        let mut rows: Vec<Value> = (1..=self.rows)
            .map(|n| json!({"k":"row","children":[
                {"k":"text","text":format!("Row {n}")},
                {"k":"text","text":"drawn inside the page","tone":"muted"}
            ]}))
            .collect();
        let mut children = vec![
            json!({"k":"text","text":"Test","tone":"title"}),
            json!({"k":"text","text":"Everything here is drawn by the application from what this extension answered.","tone":"muted"}),
            json!({"k":"divider"}),
            json!({"k":"row","children":[
                {"k":"button","id":"count","label":"Press"},
                {"k":"text","text":format!("Pressed {} times", self.pressed)}
            ]}),
            json!({"k":"toggle","id":"switch","label":"A switch","on":self.on}),
            json!({"k":"field","id":"say","placeholder":"Write something and send it"}),
            json!({"k":"text","text": if self.said.is_empty() { "Nothing sent yet".to_owned() } else { format!("You sent: {}", self.said) }}),
            json!({"k":"divider"}),
            json!({"k":"row","children":[
                {"k":"button","id":"grow","label":"Add a row"},
                {"k":"button","id":"clear","label":"Clear rows"},
                {"k":"button","id":"flood","label":"Too large"}
            ]}),
            json!({"k":"divider"}),
            json!({"k":"text","text":"Misbehave on purpose. The application must stay as it is.","tone":"muted"}),
            json!({"k":"row","children":[
                {"k":"button","id":"spam","label":"Flood"},
                {"k":"button","id":"nonsense","label":"Nonsense"},
                {"k":"button","id":"late","label":"Never answer"}
            ]}),
            json!({"k":"row","children":[
                {"k":"button","id":"spin","label":"Spin"},
                {"k":"button","id":"fill","label":"Fill memory"},
                {"k":"button","id":"leave","label":"Leave"}
            ]}),
            json!({"k":"text","text":format!("Flooded {} times, {} operations each", self.spammed, 2 * SPAM)}),
        ];
        children.append(&mut rows);
        json!({"k":"column","children":children})
    }
}

/// A tree far deeper than the application draws: it must keep what it showed and not draw this.
/// Nested, and not long, so that making it costs this guest almost nothing of its turn.
fn flood() -> Value {
    let mut tree = json!({"k":"text","text":"too deep"});
    for _ in 0..40 {
        tree = json!({"k":"column","children":[tree]});
    }
    tree
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

#[cfg(target_family = "wasm")]
mod import {
    #[link(wasm_import_module = "peersus_v1")]
    unsafe extern "C" {
        #[link_name = "receive"]
        pub fn receive(buffer: *mut u8, capacity: usize) -> i32;
        #[link_name = "send"]
        pub fn send(buffer: *const u8, length: usize) -> i32;
    }
}

/// The entry the interpreter calls. Returns zero when the application goes away, non-zero on a
/// broken protocol.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    const LIMIT: usize = 48 * 1024;
    let mut state = State::default();
    let mut buffer = vec![0u8; LIMIT];
    loop {
        let count = unsafe { import::receive(buffer.as_mut_ptr(), buffer.len()) };
        if count <= 0 || count as usize > LIMIT {
            return 0;
        }
        let Ok(delivery) = serde_json::from_slice::<Value>(&buffer[..count as usize]) else {
            return 1;
        };
        let ticket = delivery["ticket"].as_u64().unwrap_or(0);
        // Every refusal of what is sent here is the point, so no result is looked at.
        let say = |bytes: &[u8]| {
            let _ = unsafe { import::send(bytes.as_ptr(), bytes.len()) };
        };
        match trouble(&delivery) {
            Some(Trouble::Spam) => {
                for n in 0..SPAM {
                    say(format!(r#"{{"op":"call","id":{n},"provider":"host","service":"test.nobody","version":1,"method":"nothing","args":{{}},"timeout_ms":1000}}"#).as_bytes());
                    say(br#"{"op":"publish","service":"test.page","version":1,"event":"nothing","data":{}}"#);
                }
            }
            Some(Trouble::Nonsense) => {
                say(b"\xff\xfe not an operation at all");
                say(br#"{"op":"dance","with":"nobody"}"#);
                say(format!(r#"{{"op":"reply","ticket":{},"result":{{"status":"ok","value":null}}}}"#, ticket.wrapping_add(1_000_000)).as_bytes());
                say(format!(r#"{{"op":"reply","ticket":{ticket},"result":{{"status":"ok","value":{{"k":"no-such-element","children":7}}}}}}"#).as_bytes());
                continue;
            }
            Some(Trouble::Late) => continue,
            Some(Trouble::Spin) => {
                let mut turns = 0u64;
                loop {
                    turns = core::hint::black_box(turns.wrapping_add(1));
                }
            }
            Some(Trouble::Fill) => {
                let mut held = Vec::new();
                loop {
                    held.push(vec![1u8; 1 << 20]);
                    core::hint::black_box(&held);
                }
            }
            Some(Trouble::Leave) => return 1,
            None => {}
        }
        if let Some(reply) = handle(&mut state, &delivery) {
            let Ok(bytes) = serde_json::to_vec(&reply) else {
                return 1;
            };
            if bytes.len() > LIMIT || unsafe { import::send(bytes.as_ptr(), bytes.len()) } != 0 {
                return 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_changes_the_page_and_the_flood_is_far_past_the_limits() {
        let mut state = State::default();
        let open = state.answer(&json!({"t":"open"}));
        assert_eq!(open["k"], "column");
        state.answer(&json!({"t":"press","id":"count"}));
        state.answer(&json!({"t":"toggle","id":"switch","on":true}));
        let page = state.answer(&json!({"t":"submit","id":"say","text":"hi"}));
        let text = page.to_string();
        assert!(text.contains("Pressed 1 times") && text.contains("You sent: hi"));
        assert_eq!(flood().to_string().matches("column").count(), 40);
        let reply = handle(&mut state, &json!({"type":"call","ticket":3,"method":"event","args":{"event":{"t":"open"}}})).unwrap();
        assert_eq!(reply["ticket"], 3);
        assert_eq!(reply["result"]["status"], "ok");
    }

    #[test]
    fn only_a_press_of_its_own_button_asks_for_trouble() {
        let press = |id: &str| json!({"type":"call","ticket":1,"method":"event","args":{"event":{"t":"press","id":id}}});
        for (id, wanted) in [
            ("spam", Trouble::Spam),
            ("nonsense", Trouble::Nonsense),
            ("late", Trouble::Late),
            ("spin", Trouble::Spin),
            ("fill", Trouble::Fill),
            ("leave", Trouble::Leave),
        ] {
            assert_eq!(trouble(&press(id)), Some(wanted));
        }
        assert_eq!(trouble(&press("count")), None);
        assert_eq!(trouble(&json!({"type":"event","args":{"event":{"t":"press","id":"spin"}}})), None);
        assert_eq!(trouble(&json!({"type":"call","method":"event","args":{"event":{"t":"submit","id":"spin"}}})), None);
        let page = State::default().answer(&json!({"t":"open"})).to_string();
        for id in ["spam", "nonsense", "late", "spin", "fill", "leave"] {
            assert!(page.contains(&format!(r#""id":"{id}""#)), "{id}");
        }
    }
}
