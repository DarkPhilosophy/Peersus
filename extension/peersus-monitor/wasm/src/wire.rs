//! What two Monitors say to each other, and nothing else.
//!
//! The application carries a stream one way only: the sender writes it (`peersus.peers` `send`) and the receiver reads it
//! (the exported `receive`). A question and its answer are therefore two streams of this service, each sent by the
//! device that speaks to the device it speaks to, by the name this extension was given for it. A question carries a
//! number of its own, `seq`, and the answer says it again, so that an answer is taken only for the question that is out.
//!
//! ```text
//! {"ask":{"seq":7,"parts":["processor","graphics","memory","storage","network","battery"]}}
//! {"answer":{"seq":7,"parts":{"processor":{"status":"ok","value":{"name":"…","cores":8,"busy":1,"total":2,"celsius":55.0}},
//!                             "graphics":{"status":"error","code":"denied"}, …}}}
//! ```
//!
//! A part of an answer is what the application said to the answering Monitor of that part, in the application's own
//! outcome words (`ok` with the value, or `error` with `denied`: the person there did not allow it; or `unavailable`:
//! the application there could not say it), kept to the fields this extension knows and to what each can hold. Whatever
//! arrives is judged here and nowhere else: what is not exactly this is not understood, and nothing of it is used.
use crate::{PARTS, Part};
use serde_json::{Map, Value, json};

/// The service one Monitor sends to another and its version: what the application routes a stream by. A change that an
/// older Monitor could not read is a new version, never a changed word.
pub const SERVICE: &str = "monitor.readings";
pub const VERSION: u32 = 1;
/// Most bytes of a question and of an answer. A question is one short line; an answer holds six parts of a few numbers
/// and, for two of them, a name.
pub const ASK_MAX: usize = 512;
pub const ANSWER_MAX: usize = 8 * 1024;
/// Most characters of a name of a processor or a graphics card.
const NAME_MAX: usize = 128;
/// The longest a battery can say it will last, in minutes: the application's own bound.
const MINUTES_MAX: u64 = 7 * 24 * 60;

/// What one part of an answer says.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The part, kept to the fields it has (see [`value`]).
    Ok(Value),
    /// The person did not allow Monitor to see it on the device that answered.
    Denied,
    /// The device that answered could not say it.
    Unavailable,
}

/// A question: which parts are wanted, under the number the answer must say again.
#[derive(Debug, PartialEq)]
pub struct Ask {
    pub seq: u64,
    pub parts: Vec<Part>,
}

/// An answer: each part it says, in the order of the cards.
#[derive(Debug, PartialEq)]
pub struct Answer {
    pub seq: u64,
    pub parts: Vec<(Part, Outcome)>,
}

#[derive(Debug, PartialEq)]
pub enum Message {
    Ask(Ask),
    Answer(Answer),
}

/// Said of whatever was not exactly a question or an answer. Nothing more is said of it: what was wrong with it is not
/// something to tell the device that sent it.
#[derive(Debug, PartialEq, Eq)]
pub struct NotUnderstood;

/// The question of every part, under `seq`.
pub fn ask(seq: u64) -> Vec<u8> {
    let parts: Vec<&str> = PARTS.iter().map(|part| part.method()).collect();
    serde_json::to_vec(&json!({"ask":{"seq":seq,"parts":parts}})).unwrap_or_default()
}

/// The answer to question `seq`.
pub fn answer(seq: u64, parts: &[(Part, Outcome)]) -> Vec<u8> {
    let mut said = Map::new();
    for (part, outcome) in parts {
        let outcome = match outcome {
            Outcome::Ok(value) => json!({"status":"ok","value":value}),
            Outcome::Denied => json!({"status":"error","code":"denied"}),
            Outcome::Unavailable => json!({"status":"error","code":"unavailable"}),
        };
        said.insert(part.method().to_owned(), outcome);
    }
    serde_json::to_vec(&json!({"answer":{"seq":seq,"parts":said}})).unwrap_or_default()
}

/// What `bytes` say, or that they are not understood: a question or an answer, whole, with the words and the fields of
/// this version and no other, within its size.
pub fn parse(bytes: &[u8]) -> Result<Message, NotUnderstood> {
    if bytes.len() > ANSWER_MAX {
        return Err(NotUnderstood);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| NotUnderstood)?;
    let object = value.as_object().ok_or(NotUnderstood)?;
    if object.len() != 1 {
        return Err(NotUnderstood);
    }
    if let Some(body) = object.get("ask") {
        // One short line: a longer one is not a question.
        return if bytes.len() <= ASK_MAX {
            asked(body).map(Message::Ask)
        } else {
            Err(NotUnderstood)
        };
    }
    if let Some(body) = object.get("answer") {
        return answered(body).map(Message::Answer);
    }
    Err(NotUnderstood)
}

/// Whether `object` has exactly the fields `keys`: none missing, none more.
fn exactly(object: &Map<String, Value>, keys: &[&str]) -> bool {
    object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
}

fn number(body: &Value) -> Result<u64, NotUnderstood> {
    body["seq"]
        .as_u64()
        .filter(|seq| *seq != 0)
        .ok_or(NotUnderstood)
}

fn asked(body: &Value) -> Result<Ask, NotUnderstood> {
    body.as_object()
        .filter(|object| exactly(object, &["seq", "parts"]))
        .ok_or(NotUnderstood)?;
    let named = body["parts"]
        .as_array()
        .filter(|named| (1..=PARTS.len()).contains(&named.len()))
        .ok_or(NotUnderstood)?;
    let mut parts = Vec::new();
    for name in named {
        let part = name
            .as_str()
            .and_then(Part::of)
            .filter(|part| !parts.contains(part))
            .ok_or(NotUnderstood)?;
        parts.push(part);
    }
    Ok(Ask {
        seq: number(body)?,
        parts,
    })
}

fn answered(body: &Value) -> Result<Answer, NotUnderstood> {
    body.as_object()
        .filter(|object| exactly(object, &["seq", "parts"]))
        .ok_or(NotUnderstood)?;
    let said = body["parts"].as_object().ok_or(NotUnderstood)?;
    let mut parts = Vec::new();
    for part in PARTS {
        if let Some(outcome) = said.get(part.method()) {
            parts.push((part, outcome_of(part, outcome)?));
        }
    }
    // A name that is no part of a device is not an answer of this service.
    if parts.len() != said.len() {
        return Err(NotUnderstood);
    }
    Ok(Answer {
        seq: number(body)?,
        parts,
    })
}

fn outcome_of(part: Part, said: &Value) -> Result<Outcome, NotUnderstood> {
    let object = said.as_object().ok_or(NotUnderstood)?;
    match said["status"].as_str() {
        Some("ok") if exactly(object, &["status", "value"]) && said["value"].is_object() => {
            Ok(Outcome::Ok(value(part, &said["value"])))
        }
        Some("error") if exactly(object, &["status", "code"]) => match said["code"].as_str() {
            Some("denied") => Ok(Outcome::Denied),
            Some("unavailable") => Ok(Outcome::Unavailable),
            _ => Err(NotUnderstood),
        },
        _ => Err(NotUnderstood),
    }
}

/// A part as the application says it, kept to the fields this extension knows, each to what it can hold: a field that
/// is missing, of another kind or out of its bounds is not known (`null`), never repaired. The one place that decides
/// what of a part leaves this device and what of a part from another device is used.
pub fn value(part: Part, raw: &Value) -> Value {
    let count = |key: &str| raw[key].as_u64();
    let share = |key: &str| raw[key].as_u64().filter(|share| *share <= 10_000);
    let clock = |key: &str| raw[key].as_u64().filter(|mhz| (1..100_000).contains(mhz));
    match part {
        Part::Processor => json!({
            "name": name(&raw["name"]),
            "cores": count("cores").filter(|cores| *cores <= 4096),
            "mhz": clock("mhz"),
            "busy": count("busy"),
            "total": count("total"),
            "celsius": celsius(&raw["celsius"]),
        }),
        Part::Graphics => json!({
            "name": name(&raw["name"]),
            "mhz": clock("mhz"),
            "memory_used": count("memory_used"),
            "memory_total": count("memory_total"),
            "busy": share("busy"),
            "celsius": celsius(&raw["celsius"]),
        }),
        Part::Memory => json!({
            "total": count("total"),
            "available": count("available"),
        }),
        Part::Storage => json!({
            "device": name(&raw["device"]),
            "total": count("total"),
            "available": count("available"),
        }),
        Part::Network => json!({
            "received": count("received"),
            "sent": count("sent"),
        }),
        Part::Battery => json!({
            "charge": share("charge"),
            "charging": raw["charging"].as_bool(),
            "minutes": count("minutes").filter(|minutes| *minutes <= MINUTES_MAX),
        }),
    }
}

/// A name that a person can read: some text, not long, with no control character.
fn name(said: &Value) -> Value {
    said.as_str()
        .filter(|name| {
            !name.is_empty()
                && name.chars().count() <= NAME_MAX
                && !name.chars().any(char::is_control)
        })
        .map_or(Value::Null, Value::from)
}

/// A temperature in the range the application itself believes.
fn celsius(said: &Value) -> Value {
    said.as_f64()
        .filter(|degrees| (-40. ..=150.).contains(degrees))
        .map_or(Value::Null, Value::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }
    fn memory(total: u64, available: u64) -> Value {
        json!({"total":total,"available":available})
    }

    #[test]
    fn a_question_and_an_answer_are_exactly_these_words() {
        assert_eq!(
            said(&ask(7)),
            json!({"ask":{"seq":7,"parts":["processor","graphics","memory","storage","network","battery"]}})
        );
        let parts = [
            (
                Part::Processor,
                Outcome::Ok(json!({"name":"Ryzen","cores":8,"busy":1,"total":2,"celsius":55.5})),
            ),
            (Part::Graphics, Outcome::Denied),
            (Part::Memory, Outcome::Ok(memory(10, 4))),
            (Part::Battery, Outcome::Unavailable),
        ];
        assert_eq!(
            said(&answer(7, &parts)),
            json!({"answer":{"seq":7,"parts":{
                "processor":{"status":"ok","value":{"name":"Ryzen","cores":8,"busy":1,"total":2,"celsius":55.5}},
                "graphics":{"status":"error","code":"denied"},
                "memory":{"status":"ok","value":{"total":10,"available":4}},
                "battery":{"status":"error","code":"unavailable"}
            }}})
        );
    }

    #[test]
    fn what_is_written_is_what_is_read_back() {
        assert_eq!(
            parse(&ask(u64::MAX)),
            Ok(Message::Ask(Ask {
                seq: u64::MAX,
                parts: PARTS.to_vec()
            }))
        );
        let parts = vec![
            (Part::Graphics, Outcome::Denied),
            (
                Part::Memory,
                Outcome::Ok(value(Part::Memory, &memory(10, 4))),
            ),
            (Part::Network, Outcome::Unavailable),
        ];
        assert_eq!(
            parse(&answer(3, &parts)),
            Ok(Message::Answer(Answer { seq: 3, parts }))
        );
        // A device that says nothing of any part has said an answer all the same: every part of it is not available.
        assert_eq!(
            parse(br#"{"answer":{"seq":1,"parts":{}}}"#),
            Ok(Message::Answer(Answer {
                seq: 1,
                parts: Vec::new()
            }))
        );
    }

    #[test]
    fn a_question_is_understood_only_whole_and_exact() {
        for bad in [
            "",
            "not json",
            "7",
            "[]",
            "{}",
            r#""ask""#,
            r#"{"ask":null}"#,
            r#"{"ask":{"seq":1,"parts":["memory"]},"answer":{"seq":1,"parts":{}}}"#,
            // The number is a whole number from one.
            r#"{"ask":{"seq":0,"parts":["memory"]}}"#,
            r#"{"ask":{"seq":-1,"parts":["memory"]}}"#,
            r#"{"ask":{"seq":1.5,"parts":["memory"]}}"#,
            r#"{"ask":{"seq":"1","parts":["memory"]}}"#,
            r#"{"ask":{"parts":["memory"]}}"#,
            // Something is asked, once each, of parts a device has.
            r#"{"ask":{"seq":1}}"#,
            r#"{"ask":{"seq":1,"parts":[]}}"#,
            r#"{"ask":{"seq":1,"parts":"memory"}}"#,
            r#"{"ask":{"seq":1,"parts":["memory","memory"]}}"#,
            r#"{"ask":{"seq":1,"parts":["memory","disk"]}}"#,
            r#"{"ask":{"seq":1,"parts":[7]}}"#,
            r#"{"ask":{"seq":1,"parts":["processor","graphics","memory","storage","network","battery","memory"]}}"#,
            // A field that does not belong is not a question of this version.
            r#"{"ask":{"seq":1,"parts":["memory"],"also":true}}"#,
        ] {
            assert_eq!(parse(bad.as_bytes()), Err(NotUnderstood), "{bad}");
        }
    }

    #[test]
    fn an_answer_is_understood_only_whole_and_exact() {
        for bad in [
            r#"{"answer":null}"#,
            r#"{"answer":{"seq":1}}"#,
            r#"{"answer":{"seq":0,"parts":{}}}"#,
            r#"{"answer":{"seq":1,"parts":[]}}"#,
            r#"{"answer":{"seq":1,"parts":{},"also":1}}"#,
            // A name that is no part of a device.
            r#"{"answer":{"seq":1,"parts":{"disk":{"status":"error","code":"denied"}}}}"#,
            // What a part says is the application's outcome, in its words and no others.
            r#"{"answer":{"seq":1,"parts":{"memory":"ok"}}}"#,
            r#"{"answer":{"seq":1,"parts":{"memory":{"status":"maybe"}}}}"#,
            r#"{"answer":{"seq":1,"parts":{"memory":{"status":"ok"}}}}"#,
            r#"{"answer":{"seq":1,"parts":{"memory":{"status":"ok","value":7}}}}"#,
            r#"{"answer":{"seq":1,"parts":{"memory":{"status":"ok","value":{},"also":1}}}}"#,
            r#"{"answer":{"seq":1,"parts":{"memory":{"status":"error"}}}}"#,
            r#"{"answer":{"seq":1,"parts":{"memory":{"status":"error","code":"boom"}}}}"#,
            r#"{"answer":{"seq":1,"parts":{"memory":{"status":"error","code":"denied","value":{}}}}}"#,
        ] {
            assert_eq!(parse(bad.as_bytes()), Err(NotUnderstood), "{bad}");
        }
    }

    #[test]
    fn a_question_and_an_answer_are_cut_at_their_sizes() {
        // A question is one short line: one byte over its size is not one, whatever it says.
        let mut question = ask(1);
        question.resize(ASK_MAX, b' ');
        assert!(matches!(parse(&question), Ok(Message::Ask(_))));
        question.push(b' ');
        assert_eq!(parse(&question), Err(NotUnderstood));
        // An answer may be longer, up to its own size.
        let mut reply = answer(1, &[(Part::Memory, Outcome::Unavailable)]);
        reply.resize(ANSWER_MAX, b' ');
        assert!(matches!(parse(&reply), Ok(Message::Answer(_))));
        reply.push(b' ');
        assert_eq!(parse(&reply), Err(NotUnderstood));
        // What nests deeper than the reader follows is refused, not followed.
        let deep = format!("{}{}", "[".repeat(4000), "]".repeat(4000));
        assert_eq!(parse(deep.as_bytes()), Err(NotUnderstood));
        // Nothing this device writes is larger than the sizes it holds others to.
        let longest = "x".repeat(NAME_MAX);
        let parts: Vec<(Part, Outcome)> = PARTS
            .iter()
            .map(|part| {
                (
                    *part,
                    Outcome::Ok(value(
                        *part,
                        &json!({"name":longest,"device":longest,"busy":1,"total":2,"available":3,"received":4,"sent":5,"charge":6,"minutes":7,"charging":true,"celsius":8.5,"cores":9,"mhz":99_999,"memory_used":u64::MAX,"memory_total":u64::MAX}),
                    )),
                )
            })
            .collect();
        assert!(answer(u64::MAX, &parts).len() <= ANSWER_MAX);
    }

    #[test]
    fn what_is_said_of_a_part_is_kept_to_the_fields_it_has_and_to_what_each_can_hold() {
        // A field a part does not have neither leaves this device nor is taken from another.
        assert_eq!(
            value(
                Part::Processor,
                &json!({"name":"Ryzen","cores":8,"mhz":3300,"busy":1,"total":2,"celsius":55.5,"serial":"X1","more":{"a":1}})
            ),
            json!({"name":"Ryzen","cores":8,"mhz":3300,"busy":1,"total":2,"celsius":55.5})
        );
        assert_eq!(
            value(
                Part::Storage,
                &json!({"device":"nvme0n1p2","total":5,"available":2,"path":"/home"})
            ),
            json!({"device":"nvme0n1p2","total":5,"available":2})
        );
        assert_eq!(
            value(Part::Memory, &json!({"total":5,"available":2,"swap":1})),
            memory(5, 2)
        );
        // What is missing, of another kind or beyond what it can hold is not known: never repaired, never invented.
        let nothing =
            json!({"name":null,"cores":null,"mhz":null,"busy":null,"total":null,"celsius":null});
        for odd in [
            json!({}),
            json!(null),
            json!(7),
            json!("Ryzen"),
            json!([1, 2]),
            json!({"name":"a\nb","cores":1_000_000,"mhz":0,"busy":-1,"total":"2","celsius":500}),
            json!({"name":"","cores":4097,"mhz":100_000,"busy":1.5,"total":null,"celsius":"hot"}),
            json!({"name":"x".repeat(NAME_MAX + 1),"celsius":-40.5}),
        ] {
            assert_eq!(value(Part::Processor, &odd), nothing, "{odd}");
        }
        // The edges themselves are held.
        assert_eq!(
            value(
                Part::Processor,
                &json!({"name":"x".repeat(NAME_MAX),"cores":4096,"celsius":-40})
            ),
            json!({"name":"x".repeat(NAME_MAX),"cores":4096,"mhz":null,"busy":null,"total":null,"celsius":-40.0})
        );
        assert_eq!(
            value(
                Part::Graphics,
                &json!({"name":"AMD","mhz":2375,"memory_used":1,"memory_total":2,"busy":10_000,"celsius":150})
            ),
            json!({"name":"AMD","mhz":2375,"memory_used":1,"memory_total":2,"busy":10_000,"celsius":150.0})
        );
        assert_eq!(
            value(
                Part::Graphics,
                &json!({"busy":10_001,"celsius":150.5,"memory_used":-1})
            ),
            json!({"name":null,"mhz":null,"memory_used":null,"memory_total":null,"busy":null,"celsius":null})
        );
        assert_eq!(
            value(
                Part::Battery,
                &json!({"charge":10_000,"charging":true,"minutes":MINUTES_MAX})
            ),
            json!({"charge":10_000,"charging":true,"minutes":MINUTES_MAX})
        );
        assert_eq!(
            value(
                Part::Battery,
                &json!({"charge":10_001,"charging":"yes","minutes":MINUTES_MAX + 1})
            ),
            json!({"charge":null,"charging":null,"minutes":null})
        );
        assert_eq!(
            value(Part::Network, &json!({"received":u64::MAX,"sent":-1})),
            json!({"received":u64::MAX,"sent":null})
        );
        // An answer is judged by the same table: what it says beyond it is not in what is used.
        let Ok(Message::Answer(heard)) = parse(
            br#"{"answer":{"seq":1,"parts":{"memory":{"status":"ok","value":{"total":9,"available":1,"serial":"X1"}}}}}"#,
        ) else {
            panic!("a whole answer");
        };
        assert_eq!(heard.parts, [(Part::Memory, Outcome::Ok(memory(9, 1)))]);
    }

    #[test]
    fn the_manifest_declares_what_the_guest_speaks() {
        let manifest: Value = serde_json::from_str(include_str!("../plugin.json")).unwrap();
        let exports = manifest["exports"].as_array().unwrap();
        let export = exports
            .iter()
            .find(|export| export["service"] == SERVICE)
            .expect("the service one Monitor sends to another is exported");
        assert_eq!(export["version"], VERSION);
        // The application routes a device's stream to `receive`, and only when the person allowed this capability for
        // that device; the application is its one consumer.
        assert_eq!(export["consumers"], json!(["host"]));
        assert_eq!(export["methods"]["receive"]["capability"], "monitor.share");
        assert!(manifest["capabilities"]["monitor.share"].is_string());
        // Every other export is the page's own.
        assert_eq!(exports.len(), 2);
        // What the guest asks of the application is imported: the paired devices to list and to send to, and every
        // part of this device, each in the order of the cards.
        let imported = |service: &str| -> Vec<&str> {
            let import = manifest["imports"]
                .as_array()
                .unwrap()
                .iter()
                .find(|import| import["service"] == service)
                .unwrap_or_else(|| panic!("{service} is imported"));
            import["methods"]
                .as_array()
                .unwrap()
                .iter()
                .map(|method| method.as_str().unwrap())
                .collect()
        };
        assert_eq!(imported("peersus.peers"), ["list", "send"]);
        let methods: Vec<&str> = PARTS.iter().map(|part| part.method()).collect();
        assert_eq!(imported("peersus.system"), methods);
        // What a person reads before accepting fits what the application allows of it.
        for page in manifest["agreements"]["pages"].as_array().unwrap() {
            for field in ["explains", "changes"] {
                let words = page[field].as_str().unwrap().chars().count();
                assert!(
                    words <= 600,
                    "{} {field} has {words} characters",
                    page["title"]
                );
            }
        }
    }
}
