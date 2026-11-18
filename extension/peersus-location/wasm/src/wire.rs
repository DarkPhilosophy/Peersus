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
use serde::de::{Deserialize, Deserializer, Error, MapAccess, Visitor};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::marker::PhantomData;

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

/// A JSON object in which every key is written once. `serde_json`'s own map keeps the last of a key written twice, which
/// would read two different messages as one; here a key written twice is not an object at all.
struct Once<V>(BTreeMap<String, V>);

impl<'de, V: Deserialize<'de>> Deserialize<'de> for Once<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Keys<V>(PhantomData<V>);
        impl<'de, V: Deserialize<'de>> Visitor<'de> for Keys<V> {
            type Value = Once<V>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an object with every key written once")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Once<V>, A::Error> {
                let mut fields = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, V>()? {
                    if fields.insert(key, value).is_some() {
                        return Err(A::Error::custom("a key written twice"));
                    }
                }
                Ok(Once(fields))
            }
        }
        deserializer.deserialize_map(Keys(PhantomData))
    }
}

/// The message for `shared`, or nothing when it is older than another Location understands ([`AGE_MAX`]).
pub fn encode(shared: &Shared) -> Option<Vec<u8>> {
    if shared.age > AGE_MAX {
        return None;
    }
    serde_json::to_vec(&json!({"position":{
        "latitude": shared.fix.latitude,
        "longitude": shared.fix.longitude,
        "accuracy": shared.fix.accuracy,
        "age": shared.age,
    }}))
    .ok()
}

/// A position as the application says it of this device: `{latitude, longitude, accuracy, age}`, those keys and no others,
/// `accuracy` optional. An accuracy the system cannot have meant is the system not saying; what another device says is
/// held to more ([`parse`]).
pub fn position(value: &Value) -> Result<Shared, NotUnderstood> {
    fields(value.as_object().ok_or(NotUnderstood)?)
}

fn fields(fields: &Map<String, Value>) -> Result<Shared, NotUnderstood> {
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
    let Once(mut message): Once<Once<Value>> =
        serde_json::from_slice(bytes).map_err(|_| NotUnderstood)?;
    let (1, Some(Once(said))) = (message.len(), message.remove("position")) else {
        return Err(NotUnderstood);
    };
    let said: Map<String, Value> = said.into_iter().collect();
    let shared = fields(&said)?;
    // An accuracy another device said and that cannot be meant is not left out for it: it is not a position.
    if shared.fix.accuracy.is_none() && said.get("accuracy").is_some_and(|said| !said.is_null()) {
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
        assert_eq!(parse(&encode(&shared).unwrap()), Ok(shared));
        let unsure = Shared {
            fix: Fix::new(-1.0, -2.0, None).unwrap(),
            age: 0,
        };
        assert_eq!(parse(&encode(&unsure).unwrap()), Ok(unsure));
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
            // A key written twice, at either depth: not the last one read as if it were the only one.
            br#"{"position":null,"position":{"latitude":1,"longitude":2,"age":3}}"#,
            br#"{"position":{"latitude":1,"longitude":2,"age":3},"position":{"latitude":1,"longitude":2,"age":3}}"#,
            br#"{"position":{"latitude":95,"latitude":1,"longitude":2,"age":3}}"#,
            br#"{"position":{"latitude":1,"longitude":2,"age":3,"age":3}}"#,
            br#"{"position":[1,2,3]}"#,
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
    fn a_position_too_old_to_be_understood_is_not_written() {
        let aged = |age| Shared {
            fix: Fix::new(1.0, 2.0, None).unwrap(),
            age,
        };
        assert_eq!(parse(&encode(&aged(AGE_MAX)).unwrap()), Ok(aged(AGE_MAX)));
        assert_eq!(encode(&aged(AGE_MAX + 1)), None);
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
