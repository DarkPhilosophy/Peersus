//! What one Location says to another, and nothing else: one position, shared once.
//!
//! The application carries a stream one way only: the device that shares writes it (`peersus.peers` `send`) and the device
//! it shares with reads it (the exported `receive`). Nothing is asked and nothing answers: a position goes only when its
//! person shares it.
//!
//! ```text
//! {"position":{"latitude":44.4268,"longitude":26.1025,"accuracy":12.5,"age":30}}
//! ```
//!
//! `accuracy` is metres (`null` or left out when the system did not say; one that is said is between zero and
//! [`crate::geo::ACCURACY_MAX`]), `age` the seconds since the system found it, at the moment it was sent. Whatever arrives is judged here
//! and nowhere else: what is not exactly this is not understood, and nothing of it is used.
use crate::geo::{AGE_MAX, Fix};
use serde_json::{Map, Value, json};

/// The service one Location sends to another and its version: what the application routes a stream by. A change an older
/// Location could not read is a new version, never a changed word.
pub const SERVICE: &str = "location.position";
pub const VERSION: u32 = 1;
/// Most bytes of one message: one short line.
pub const MAX: usize = 512;

/// A position as it is shared: where, and how old it was when it left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shared {
    pub fix: Fix,
    pub age: u64,
}

/// Said of whatever was not exactly a position. Nothing more is said of it.
#[derive(Debug, PartialEq, Eq)]
pub struct NotUnderstood;

pub fn encode(shared: &Shared) -> Vec<u8> {
    serde_json::to_vec(&json!({"position":{
        "latitude": shared.fix.latitude,
        "longitude": shared.fix.longitude,
        "accuracy": shared.fix.accuracy,
        "age": shared.age,
    }}))
    .unwrap_or_default()
}

/// A position as the application says it of this device: `{latitude, longitude, accuracy, age}`, those keys and no others,
/// `accuracy` optional. An accuracy the system cannot have meant is the system not saying; what another device says is
/// held to more ([`parse`]).
pub fn position(value: &Value) -> Result<Shared, NotUnderstood> {
    let fields = value.as_object().ok_or(NotUnderstood)?;
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "latitude" | "longitude" | "accuracy" | "age"))
    {
        return Err(NotUnderstood);
    }
    let number = |key: &str| fields.get(key).and_then(Value::as_f64).ok_or(NotUnderstood);
    let accuracy = match fields.get("accuracy") {
        None | Some(Value::Null) => None,
        Some(said) => Some(said.as_f64().ok_or(NotUnderstood)?),
    };
    let fix = Fix::new(number("latitude")?, number("longitude")?, accuracy).ok_or(NotUnderstood)?;
    let age = fields
        .get("age")
        .and_then(Value::as_u64)
        .filter(|age| *age <= AGE_MAX)
        .ok_or(NotUnderstood)?;
    Ok(Shared { fix, age })
}

pub fn parse(bytes: &[u8]) -> Result<Shared, NotUnderstood> {
    if bytes.len() > MAX {
        return Err(NotUnderstood);
    }
    let message: Map<String, Value> = serde_json::from_slice(bytes).map_err(|_| NotUnderstood)?;
    let (1, Some(said)) = (message.len(), message.get("position")) else {
        return Err(NotUnderstood);
    };
    let shared = position(said)?;
    // An accuracy another device said and that cannot be meant is not left out for it: it is not a position.
    if shared.fix.accuracy.is_none() && !said["accuracy"].is_null() {
        return Err(NotUnderstood);
    }
    Ok(shared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::ACCURACY_MAX;

    #[test]
    fn a_position_goes_and_comes_back_the_same() {
        let shared = Shared {
            fix: Fix::new(44.4268, 26.1025, Some(12.5)).unwrap(),
            age: 30,
        };
        assert_eq!(parse(&encode(&shared)), Ok(shared));
        let unsure = Shared {
            fix: Fix::new(-1.0, -2.0, None).unwrap(),
            age: 0,
        };
        assert_eq!(parse(&encode(&unsure)), Ok(unsure));
    }

    #[test]
    fn what_is_not_exactly_a_position_is_not_understood() {
        for said in [
            &b"not json"[..],
            br#"{}"#,
            br#"{"position":{"latitude":1,"longitude":2,"age":3},"more":1}"#,
            br#"{"position":{"latitude":1,"longitude":2,"age":3,"name":"home"}}"#,
            br#"{"position":{"latitude":95,"longitude":2,"age":3}}"#,
            br#"{"position":{"latitude":1,"longitude":2}}"#,
            br#"{"position":{"latitude":1,"longitude":2,"age":-3}}"#,
            br#"{"position":{"latitude":1,"longitude":2,"age":99999999999}}"#,
            br#"{"position":{"latitude":"1","longitude":2,"age":3}}"#,
            br#"{"position":{"latitude":1,"longitude":2,"accuracy":"far","age":3}}"#,
            br#"{"position":{"latitude":1,"longitude":2,"accuracy":-1,"age":3}}"#,
            br#"{"position":{"latitude":1,"longitude":2,"accuracy":1000001,"age":3}}"#,
        ] {
            assert_eq!(
                parse(said),
                Err(NotUnderstood),
                "{}",
                String::from_utf8_lossy(said)
            );
        }
        let long = format!(
            r#"{{"position":{{"latitude":1,"longitude":2,"age":3,"accuracy":1{}}}}}"#,
            "0".repeat(MAX)
        );
        assert_eq!(parse(long.as_bytes()), Err(NotUnderstood));
    }

    #[test]
    fn an_accuracy_is_said_within_its_bounds_or_not_at_all() {
        let at = |accuracy: &str| {
            parse(
                format!(
                    r#"{{"position":{{"latitude":1,"longitude":2,"accuracy":{accuracy},"age":3}}}}"#
                )
                .as_bytes(),
            )
            .map(|shared| shared.fix.accuracy)
        };
        assert_eq!(at("0"), Ok(Some(0.0)));
        assert_eq!(at("1000000"), Ok(Some(ACCURACY_MAX)));
        assert_eq!(at("null"), Ok(None));
        assert_eq!(
            parse(br#"{"position":{"latitude":1,"longitude":2,"age":3}}"#)
                .map(|shared| shared.fix.accuracy),
            Ok(None)
        );
        // The application's answer of this device is not another device's word: there it is the system not saying.
        let own =
            position(&json!({"latitude":1,"longitude":2,"accuracy":1000001,"age":3})).unwrap();
        assert_eq!(own.fix.accuracy, None);
    }
}
