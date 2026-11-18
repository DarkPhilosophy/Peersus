//! Typed stream preambles keep file offers separate from agnostic plugin traffic.
//! Routing is not authorization: the host must still admit the peer and service.
use crate::{transfer::Offer, transport};
use iroh::endpoint::{RecvStream, SendStream};
use peersus_core::plugins::{MAX_ACTIVE_STATUSES, StatusInfo};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    File {
        offer: Offer,
    },
    Plugin {
        service: String,
        version: u32,
    },
    /// A device telling this one which profile picture it holds. It carries nothing: the digest and
    /// the picture come on the stream itself, under `profile_exchange`, so that what the stream is
    /// for is one word and the rest is judged where it is read.
    Profile {},
    /// A device telling this one how it is: what it is called now, whether it sleeps, and which statuses of plugins it has now. It is never
    /// answered. Asleep means the screen is off (or it has gone idle): it is there but slow, and nothing here should read its silence as
    /// being gone. The statuses are open, each with whose it is and the words its author gave it. The name is what the person called this
    /// device: it is told here because the pairing is the only other time it travels, and a device renamed afterwards would stay under its
    /// old name for ever. This is the whole of what a device says of itself and is always said in full, so that what arrives replaces what
    /// was known and nothing has to be added up from pieces. Only a connection that is already a session can say it, and it is only ever
    /// about the device that says it.
    Presence {
        asleep: bool,
        #[serde(default)]
        statuses: Vec<StatusInfo>,
        /// `None` from a device that does not say its name (an older one): what is known of its name is then left as it was.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// A device telling this one the displays of its desktop, in its own coordinates: what a layout of
    /// devices places side by side. Never answered, always the whole set, and only ever about the device
    /// that says it. An older device reads it as [`Request::Unknown`] and lets it go.
    Displays {
        displays: Vec<peersus_core::layout::Display>,
    },
    /// A device telling this one which extension services it would take a stream to FROM THIS DEVICE now: those of its running
    /// extensions whose permission its person gave for this device. Never answered, always the whole set, said again whenever it
    /// changes, and only ever about the device that says it and the device it says it to. With it an extension here can tell a
    /// device that is ready for it from one that has not the same extension, or does not allow it, before it sends anything. An
    /// older device reads it as [`Request::Unknown`] and lets it go; one that never said it is not known either way.
    Accepts {
        services: Vec<ServiceRef>,
    },
    /// A device telling this one that from now on it says nothing of itself: its person keeps it private. Never answered, and
    /// only ever about the device that says it. What was known of it over this connection (that it is awake or asleep, its
    /// statuses, its displays, what it takes) is forgotten, and it is shown as not there until it says how it is again. An
    /// older device reads it as [`Request::Unknown`] and lets it go.
    Withdrawn {},
    /// A request of a kind this build does not know, from a newer device. Only ever received, never sent: the stream is let go and the
    /// connection goes on. Without it an older device would show every new kind as an invalid stream.
    #[serde(other)]
    Unknown,
}

/// One extension service, by its name and version: what a stream is routed by.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceRef {
    pub service: String,
    pub version: u32,
}

/// Most services one device says it takes from another: far more than a device runs extensions, and what is said stays small.
pub const MAX_ACCEPTED: usize = 64;

impl ServiceRef {
    /// The rule every service name and version on the wire is judged by: a version from one, a name of at most 128 letters,
    /// digits, dots, dashes and underscores.
    pub fn valid(service: &str, version: u32) -> bool {
        version != 0
            && !service.is_empty()
            && service.len() <= 128
            && service
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    }
}

impl Request {
    pub fn validate(&self) -> Result<(), transport::Error> {
        match self {
            Self::File { offer } => offer.validate().map_err(|_| transport::Error::Invalid),
            Self::Profile {} | Self::Withdrawn {} => Ok(()),
            Self::Displays { displays } => peersus_core::layout::valid_displays(displays)
                .then_some(())
                .ok_or(transport::Error::Invalid),
            Self::Presence { statuses, name, .. } => {
                // Bounded as a whole, and each one named once: what a device says of itself does not grow with the number of plugins
                // it has, and one status cannot be said twice with two different labels. The name, when there is one, is judged by the
                // very rule that judged it at pairing.
                let mut seen = std::collections::BTreeSet::new();
                (statuses.len() <= MAX_ACTIVE_STATUSES
                    && statuses.iter().all(|status| seen.insert(&status.key))
                    && name.as_deref().is_none_or(crate::peers::is_valid_name))
                .then_some(())
                .ok_or(transport::Error::Invalid)
            }
            // Never sent: it stands for something that arrived and was not understood.
            Self::Unknown => Err(transport::Error::Invalid),
            Self::Plugin { service, version } => ServiceRef::valid(service, *version)
                .then_some(())
                .ok_or(transport::Error::Invalid),
            // Bounded, each one valid and said once: the whole set, not a list that can be padded.
            Self::Accepts { services } => {
                let mut seen = std::collections::BTreeSet::new();
                (services.len() <= MAX_ACCEPTED
                    && services
                        .iter()
                        .all(|s| ServiceRef::valid(&s.service, s.version) && seen.insert(s)))
                .then_some(())
                .ok_or(transport::Error::Invalid)
            }
        }
    }
}

pub async fn send(stream: &mut SendStream, request: &Request) -> Result<(), transport::Error> {
    request.validate()?;
    transport::send(stream, request).await
}

/// The next request on a stream. A kind this build does not know comes back as [`Request::Unknown`] and not as an error: a newer device may
/// say something this one cannot read, and that is no fault of the stream, so it must not be shown to the person as one. Everything else
/// that is wrong (a broken message, a field that does not belong, a value of the wrong type) is still an error.
pub async fn receive(stream: &mut RecvStream) -> Result<Request, transport::Error> {
    let request: Request = transport::receive(stream).await?;
    if !matches!(request, Request::Unknown) {
        request.validate()?;
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;
    use peersus_core::config::NetworkSettings;

    #[tokio::test]
    async fn file_and_plugin_streams_route_independently_on_one_connection() {
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        };
        let a = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let b = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let (left, right) = tokio::join!(a.connect(b.addr(), transport::ALPN), async {
            b.accept().await.unwrap().await
        });
        let left = left.unwrap();
        let right = right.unwrap();
        let send_all = async {
            for request in [
                Request::Plugin {
                    service: "example.service".into(),
                    version: 1,
                },
                Request::File {
                    offer: Offer {
                        id: "a".repeat(32),
                        name: "example.txt".into(),
                        bytes: 3,
                        folder: None,
                    },
                },
            ] {
                let (mut tx, mut rx) = left.open_bi().await.unwrap();
                send(&mut tx, &request).await.unwrap();
                tx.finish().unwrap();
                assert!(transport::receive::<bool>(&mut rx).await.unwrap());
            }
        };
        let route_all = async {
            for plugin in [true, false] {
                let (mut tx, mut rx) = right.accept_bi().await.unwrap();
                let request = receive(&mut rx).await.unwrap();
                match request {
                    Request::Plugin { service, version } => {
                        assert!(plugin);
                        assert_eq!(service, "example.service");
                        assert_eq!(version, 1);
                    }
                    Request::File { offer } => {
                        assert!(!plugin);
                        assert_eq!(offer.bytes, 3);
                    }
                    Request::Profile {} => unreachable!("this test sends no profile stream"),
                    Request::Presence { .. } => unreachable!("this test sends no presence stream"),
                    Request::Displays { .. } => unreachable!("this test sends no displays stream"),
                    Request::Accepts { .. } => unreachable!("this test sends no accepts stream"),
                    Request::Withdrawn {} => unreachable!("this test sends no withdrawal"),
                    Request::Unknown => unreachable!("this test sends only kinds this build knows"),
                }
                transport::send(&mut tx, &true).await.unwrap();
                tx.finish().unwrap();
            }
        };
        tokio::join!(send_all, route_all);
        left.close(0_u32.into(), b"done");
        right.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }

    #[test]
    fn a_profile_request_is_one_word_and_nothing_can_ride_along_with_it() {
        let request: Request = serde_json::from_str(r#"{"kind":"profile"}"#).unwrap();
        assert!(matches!(request, Request::Profile {}));
        request.validate().unwrap();
        assert_eq!(
            serde_json::to_string(&Request::Profile {}).unwrap(),
            r#"{"kind":"profile"}"#
        );
        for bad in [
            r#"{"kind":"profile","digest":"00"}"#,
            r#"{"kind":"profile","x":1}"#,
        ] {
            assert!(
                serde_json::from_str::<Request>(bad).is_err(),
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn a_presence_request_is_the_whole_of_what_a_device_says_and_it_is_bounded_and_checked() {
        // A message from before statuses existed still reads: the statuses are none.
        let old: Request = serde_json::from_str(r#"{"kind":"presence","asleep":true}"#).unwrap();
        assert!(
            matches!(&old, Request::Presence { asleep: true, statuses, .. } if statuses.is_empty())
        );
        old.validate().unwrap();
        // Statuses carry both whose they are and the words of their author, and a round trip drops neither.
        let said = r#"{"kind":"presence","asleep":false,"statuses":[{"plugin":"player","id":"watching","label":"Watching a video"}]}"#;
        let request: Request = serde_json::from_str(said).unwrap();
        request.validate().unwrap();
        assert_eq!(serde_json::to_string(&request).unwrap(), said);
        let Request::Presence { statuses, .. } = &request else {
            panic!("not a presence request")
        };
        assert_eq!(statuses[0].key.plugin.as_str(), "player");
        assert_eq!(statuses[0].key.id.as_str(), "watching");
        assert_eq!(statuses[0].label.as_str(), "Watching a video");

        // What a device says of itself does not grow with the number of plugins it has: eight at most, and one status once.
        let entry =
            |plugin: &str, id: &str| format!(r#"{{"plugin":"{plugin}","id":"{id}","label":"x"}}"#);
        let message = |entries: Vec<String>| {
            format!(
                r#"{{"kind":"presence","asleep":false,"statuses":[{}]}}"#,
                entries.join(",")
            )
        };
        let eight: Vec<String> = (0..MAX_ACTIVE_STATUSES)
            .map(|i| entry("p", &format!("s{i}")))
            .collect();
        serde_json::from_str::<Request>(&message(eight.clone()))
            .unwrap()
            .validate()
            .unwrap();
        let nine: Vec<String> = (0..=MAX_ACTIVE_STATUSES)
            .map(|i| entry("p", &format!("s{i}")))
            .collect();
        assert!(
            serde_json::from_str::<Request>(&message(nine))
                .unwrap()
                .validate()
                .is_err(),
            "nine statuses were accepted"
        );
        let twice = vec![entry("p", "same"), entry("p", "same")];
        assert!(
            serde_json::from_str::<Request>(&message(twice))
                .unwrap()
                .validate()
                .is_err(),
            "one status said twice was accepted"
        );
        // Two plugins may use the same identifier: they are two statuses, so this is not a repeat.
        let two_plugins = vec![entry("a", "same"), entry("b", "same")];
        serde_json::from_str::<Request>(&message(two_plugins))
            .unwrap()
            .validate()
            .unwrap();

        // Nothing rides along, and each status is checked again on arrival: the application's words, hidden characters, and
        // anything that is not an identifier are refused when read.
        for bad in [
            r#"{"kind":"presence"}"#,
            r#"{"kind":"presence","asleep":"yes"}"#,
            r#"{"kind":"presence","asleep":1}"#,
            r#"{"kind":"presence","asleep":true,"since":5}"#,
            r#"{"kind":"presence","asleep":true,"statuses":"x"}"#,
            r#"{"kind":"presence","asleep":true,"statuses":[{"plugin":"p","id":"offline","label":"x"}]}"#,
            r#"{"kind":"presence","asleep":true,"statuses":[{"plugin":"p","id":"a","label":"Online"}]}"#,
            r#"{"kind":"presence","asleep":true,"statuses":[{"plugin":"p","id":"a","label":"a\u202eb"}]}"#,
            r#"{"kind":"presence","asleep":true,"statuses":[{"plugin":"p q","id":"a","label":"x"}]}"#,
            r#"{"kind":"presence","asleep":true,"statuses":[{"plugin":"p","id":"a","label":"x","extra":1}]}"#,
        ] {
            assert!(
                serde_json::from_str::<Request>(bad).is_err(),
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn displays_are_said_whole_bounded_and_unknown_to_an_older_reader() {
        let said = r#"{"kind":"displays","displays":[{"id":"HDMI-1","x":0,"y":0,"width":1920,"height":1080}]}"#;
        let request: Request = serde_json::from_str(said).unwrap();
        request.validate().unwrap();
        assert_eq!(serde_json::to_string(&request).unwrap(), said);
        for bad in [
            r#"{"kind":"displays","displays":[{"id":"","x":0,"y":0,"width":1,"height":1}]}"#,
            r#"{"kind":"displays","displays":[{"id":"a","x":0,"y":0,"width":0,"height":1}]}"#,
            r#"{"kind":"displays","displays":[{"id":"a\n","x":0,"y":0,"width":1,"height":1}]}"#,
        ] {
            let request: Request = serde_json::from_str(bad).unwrap();
            assert!(request.validate().is_err(), "{bad} was accepted");
        }
        let many = (0..=peersus_core::layout::MAX_DISPLAYS)
            .map(|i| format!(r#"{{"id":"{i}","x":0,"y":0,"width":1,"height":1}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let request: Request =
            serde_json::from_str(&format!(r#"{{"kind":"displays","displays":[{many}]}}"#)).unwrap();
        assert!(request.validate().is_err());
        assert!(
            serde_json::from_str::<Request>(r#"{"kind":"displays","displays":[],"x":1}"#).is_err()
        );
    }

    #[test]
    fn what_a_device_takes_from_another_is_said_whole_bounded_and_each_service_once() {
        let said = r#"{"kind":"accepts","services":[{"service":"monitor.readings","version":1}]}"#;
        let request: Request = serde_json::from_str(said).unwrap();
        request.validate().unwrap();
        assert_eq!(serde_json::to_string(&request).unwrap(), said);
        // Nothing taken is said too: an extension removed, or a permission withdrawn, leaves the set empty.
        let none: Request = serde_json::from_str(r#"{"kind":"accepts","services":[]}"#).unwrap();
        none.validate().unwrap();
        for bad in [
            r#"{"kind":"accepts","services":[{"service":"a","version":0}]}"#,
            r#"{"kind":"accepts","services":[{"service":"","version":1}]}"#,
            r#"{"kind":"accepts","services":[{"service":"a b","version":1}]}"#,
            r#"{"kind":"accepts","services":[{"service":"a","version":1},{"service":"a","version":1}]}"#,
        ] {
            let request: Request = serde_json::from_str(bad).unwrap();
            assert!(request.validate().is_err(), "{bad} was accepted");
        }
        // Nothing rides along.
        assert!(
            serde_json::from_str::<Request>(r#"{"kind":"accepts","services":[],"x":1}"#).is_err()
        );
        assert!(
            serde_json::from_str::<Request>(
                r#"{"kind":"accepts","services":[{"service":"a","version":1,"x":1}]}"#
            )
            .is_err()
        );
        let many = (0..=MAX_ACCEPTED)
            .map(|i| format!(r#"{{"service":"s{i}","version":1}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let request: Request =
            serde_json::from_str(&format!(r#"{{"kind":"accepts","services":[{many}]}}"#)).unwrap();
        assert!(request.validate().is_err());
    }

    #[test]
    fn the_name_a_device_says_is_optional_and_judged_by_the_rule_of_pairing() {
        // From before names were said: it reads, and it says no name, so what is known of the name is left as it was.
        let old: Request = serde_json::from_str(r#"{"kind":"presence","asleep":false}"#).unwrap();
        assert!(matches!(&old, Request::Presence { name: None, .. }));
        old.validate().unwrap();
        // A name round trips whole, with the characters of a person's language, and says nothing when there is none.
        for good in [
            "ROG",
            "Telefonul lui Alex",
            "Țară ăâîșț 😀",
            &"x".repeat(128),
        ] {
            let said = Request::Presence {
                asleep: false,
                statuses: Vec::new(),
                name: Some(good.to_owned()),
            };
            said.validate().unwrap();
            let json = serde_json::to_string(&said).unwrap();
            let Request::Presence { name, .. } = serde_json::from_str(&json).unwrap() else {
                panic!("not presence")
            };
            assert_eq!(name.as_deref(), Some(good));
        }
        let none = Request::Presence {
            asleep: false,
            statuses: Vec::new(),
            name: None,
        };
        assert_eq!(
            serde_json::to_string(&none).unwrap(),
            r#"{"kind":"presence","asleep":false,"statuses":[]}"#
        );
        // The same rule as at pairing: not blank, at most 128 bytes, no control characters. A name that is wrong refuses the whole message,
        // so that what a device says of itself is taken whole or not at all.
        for bad in [
            "",
            "   ",
            "two\nlines",
            "bell\u{7}",
            &"x".repeat(129),
            &"é".repeat(65),
        ] {
            let said = Request::Presence {
                asleep: true,
                statuses: Vec::new(),
                name: Some(bad.to_owned()),
            };
            assert!(said.validate().is_err(), "{bad:?} was accepted as a name");
        }
        // Nothing else rides along with it.
        assert!(
            serde_json::from_str::<Request>(
                r#"{"kind":"presence","asleep":true,"name":"A","nick":"B"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Request>(r#"{"kind":"presence","asleep":true,"name":5}"#)
                .is_err()
        );
    }

    #[test]
    fn a_kind_from_a_newer_device_is_read_as_unknown_and_everything_else_wrong_is_still_refused() {
        // A kind this build has never heard of is no fault of the stream: it is read, and the host lets it go without a word. A known
        // kind spelled differently is, to this build, a kind it does not know.
        for future in [
            r#"{"kind":"from-the-future"}"#,
            r#"{"kind":"from-the-future","anything":1}"#,
            r#"{"kind":"x","nested":{"a":[1,2]}}"#,
            r#"{"kind":"Profile"}"#,
            r#"{"kind":"profiles"}"#,
            r#"{"kind":"Presence","asleep":true}"#,
        ] {
            assert!(
                matches!(
                    serde_json::from_str::<Request>(future),
                    Ok(Request::Unknown)
                ),
                "{future} was not read as unknown"
            );
        }
        // Nothing else became lenient: a field that does not belong to a known kind, a wrong type, a missing value, a broken message,
        // no kind at all, and something that is not a message.
        for bad in [
            r#"{"kind":"profile","x":1}"#,
            r#"{"kind":"presence","asleep":"yes"}"#,
            r#"{"kind":"presence"}"#,
            r#"{"kind":"pres"#,
            r#"{"asleep":true}"#,
            r#"[]"#,
        ] {
            assert!(
                serde_json::from_str::<Request>(bad).is_err(),
                "{bad} was accepted"
            );
        }
        // And it is only ever received: a device cannot send it, so no fault of this code can put it on the wire.
        assert!(Request::Unknown.validate().is_err());
    }

    #[test]
    fn invalid_routes_and_offers_are_rejected_before_dispatch() {
        for (service, version) in [("", 1), ("example.service", 0), ("../example", 1)] {
            assert!(
                Request::Plugin {
                    service: service.into(),
                    version
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            Request::File {
                offer: Offer {
                    id: "a".repeat(32),
                    name: "../outside".into(),
                    bytes: 0,
                    folder: None,
                }
            }
            .validate()
            .is_err()
        );
        // A kind nobody here knows is read (a newer device may say it) but is not a route: it is let go and never dispatched or sent.
        let unknown: Request = serde_json::from_str(r#"{"kind":"unknown"}"#).unwrap();
        assert!(matches!(unknown, Request::Unknown));
        assert!(unknown.validate().is_err());
    }
}
