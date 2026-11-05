//! What the application offers plugins about statuses: to create their own, to say which are true of this device, and to read what
//! the paired devices say of themselves.
//!
//! The application knows no status. Three words are its own (`online`, `sleeping`, `offline`) and no plugin can say them; everything else is
//! made by a plugin while it runs, and nothing lists what a plugin may make. What makes a status is the pair of the plugin that owns it and
//! the identifier that plugin gave it: the pair is what every method and every event below carries, so that what a plugin reads of a status
//! always says whose it is, and no plugin can speak for another.
//!
//! Three permissions, each its own, because they are three different things to allow:
//!
//! * `status.define` creates and rewords the plugin's own statuses. A status that is only defined says nothing about the device.
//! * `status.set` turns one of the plugin's own statuses on or off. This is the one that is SEEN: what is on is told to every paired device
//!   and shown on its screen, in the words the plugin's author gave it.
//! * `status.watch` reads what the paired devices say of themselves and is told when it changes. What a person does on another device
//!   (watching a video, being in a call) can be personal, so it is behind a permission of its own.
//!
//! The methods and the event say what they say in exactly the same words (see [`entry`]), so that what a plugin is handed by asking can be
//! neither more nor less than what it is handed by listening, and one payload goes to everyone who listens: nothing in it is made for one plugin.
use peersus_core::{model::Peer, plugins::StatusInfo};
use serde_json::{Value, json};

/// The service the application offers plugins about statuses.
pub const STATUS_SERVICE: &str = "peersus.status";
/// Create or reword one of the plugin's own statuses.
pub const STATUS_DEFINE: &str = "define";
/// Turn one of the plugin's own statuses on.
pub const STATUS_SET: &str = "set";
/// Turn one of the plugin's own statuses off.
pub const STATUS_CLEAR: &str = "clear";
/// What the paired devices say of themselves now: for a plugin that starts late or starts again.
pub const STATUS_LIST: &str = "list";
/// The statuses the calling plugin made itself, and which of them are on.
pub const STATUS_OWN: &str = "own";
/// The event that says what a device says of itself changed.
pub const STATUS_CHANGED: &str = "changed";

/// The capability that guards [`STATUS_DEFINE`].
pub const STATUS_DEFINE_CAPABILITY: &str = "status.define";
/// The capability that guards [`STATUS_SET`] and [`STATUS_CLEAR`].
pub const STATUS_SET_CAPABILITY: &str = "status.set";
/// The capability that guards [`STATUS_LIST`] and [`STATUS_CHANGED`].
pub const STATUS_WATCH_CAPABILITY: &str = "status.watch";

/// What each permission lets a plugin do, in the words a person reads when granting it. One copy, for the manifest and for whatever shows it.
pub const STATUS_DEFINE_DESCRIPTION: &str = "Create its own statuses and choose the words that name them. A status that is only created says nothing about you until it is turned on.";
pub const STATUS_SET_DESCRIPTION: &str = "Turn its own statuses on and off. What is on is SHOWN to every device you paired, in the words the plugin chose, for as long as it is on.";
pub const STATUS_WATCH_DESCRIPTION: &str = "See which statuses your paired devices show, and be told when they change. What a status says (watching something, being busy) can be personal.";

/// How a status of a device is told to a plugin: whose it is, what it is called, and the words of its author. The one definition, used by the
/// answer to a read and by the event alike. Never a device's key.
fn status_entry(status: &StatusInfo) -> Value {
    json!({ "plugin": status.key.plugin, "id": status.key.id, "label": status.label })
}

/// What one device says of itself: how it is reachable and the statuses it shows, named the way `peersus.peers` names a device so that the two
/// can be used together: `device` is what the plugin that READS calls the device (the alias `peersus.peers` gives and `send` takes), and
/// `name` is the name the device gave itself (`null` when it gave none), never its key.
///
/// `device` belongs to the answer to one plugin's read only: an alias is different for every plugin on purpose, so the event, which is one
/// payload for everybody who listens, has no `device` and says `name` alone. A plugin that must know WHICH device an event is about reads
/// `list`, where it has both, and finds it there by `name`; two devices of one name are told apart in `list`, not in the event.
pub fn entry(
    alias: Option<&str>,
    name: Option<&str>,
    awake_or_not: &str,
    statuses: &[StatusInfo],
) -> Value {
    let mut entry = serde_json::Map::new();
    if let Some(alias) = alias {
        entry.insert("device".into(), Value::String(alias.to_owned()));
    }
    entry.insert("name".into(), json!(name));
    entry.insert("presence".into(), json!(awake_or_not));
    entry.insert(
        "statuses".into(),
        Value::Array(statuses.iter().map(status_entry).collect()),
    );
    Value::Object(entry)
}

/// The word for how a device is reachable: one of the three the application owns. Nothing else is ever put in this place, so a plugin can
/// rely on it and a custom status can never be mistaken for it.
pub fn presence_word(peer: &Peer) -> &'static str {
    match peer.presence() {
        peersus_core::model::Presence::Online => "online",
        peersus_core::model::Presence::Sleeping => "sleeping",
        peersus_core::model::Presence::Offline => "offline",
    }
}

/// What every paired device says of itself now, as `plugin` reads it: each with the alias that plugin knows the device by.
pub fn list(
    aliases: &crate::aliases::Aliases,
    plugin: &peersus_core::plugins::PluginId,
    peers: &[Peer],
) -> Value {
    Value::Array(
        peers
            .iter()
            .map(|peer| {
                entry(
                    Some(&aliases.alias(plugin, &peer.id)),
                    Some(&peer.name),
                    presence_word(peer),
                    peer.custom_statuses(),
                )
            })
            .collect(),
    )
}

/// The statuses a plugin made itself and which of them are on: what it needs to take up again after it was started again, since nothing it
/// made outlives its process. Its own, so it needs no permission beyond the one to make them.
pub fn own(defined: &[StatusInfo], on: &[StatusInfo]) -> Value {
    Value::Array(
        defined
            .iter()
            .map(|status| {
                let mut entry = status_entry(status);
                entry["on"] = Value::Bool(on.iter().any(|active| active.key == status.key));
                entry
            })
            .collect(),
    )
}

/// What was last said of each device, by the device's identity: what [`to_tell`] compares with, and never what a plugin is handed.
pub type Seen = std::collections::BTreeMap<peersus_core::model::PeerId, Value>;

/// What a plugin that listens is told of: the entries of the devices that say something different from the last time it was looked at, each
/// once, with the record to remember for next time.
///
/// What changed is found by the identity of the DEVICE (its key, which never reaches a plugin) and not by what its entry says: two devices
/// may have the same name and be in the same state, and one of them coming online must still be told. The entry that is told carries no
/// identity, because it is one payload for everybody who listens and an alias is different for each of them.
///
/// * `told` is what was remembered last time: `None` before the first look.
/// * The first look tells nothing and only remembers: it is how things already were, and a plugin that starts later asks `list`.
/// * With nobody listening nothing is told and nothing is kept, so the record is not copied for plugins that do not exist, and the next look
///   is a first look again.
/// * A device that is no longer there is not told of: its leaving is what `peersus.peers` says, and nothing here says it. One that is new
///   is told of.
///
/// Returns the entries to tell, and the record to keep.
pub fn to_tell(
    told: Option<&Seen>,
    now: &[Peer],
    anyone_listens: bool,
) -> (Vec<Value>, Option<Seen>) {
    if !anyone_listens {
        return (Vec::new(), None);
    }
    let current: Seen = now
        .iter()
        .map(|peer| {
            (
                peer.id.clone(),
                entry(
                    None,
                    Some(&peer.name),
                    presence_word(peer),
                    peer.custom_statuses(),
                ),
            )
        })
        .collect();
    let Some(before) = told else {
        return (Vec::new(), Some(current));
    };
    let changed = current
        .iter()
        .filter(|(id, now)| before.get(*id) != Some(*now))
        .map(|(_, now)| now.clone())
        .collect();
    (changed, Some(current))
}

/// What a plugin asks `define` for: the identifier it gives a status and the words that name it.
#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DefineRequest {
    id: String,
    label: String,
}

/// What a plugin asks `set` or `clear` for: which of its own statuses. Nothing else: the plugin it speaks for is the one the host knows it
/// is, never one it names, so there is no field in which a plugin could claim to be another.
#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OneRequest {
    id: String,
}

/// Read a `define` request exactly: an identifier and a label, and nothing beyond them.
pub fn parse_define(args: &Value) -> Result<(String, String), &'static str> {
    serde_json::from_value::<DefineRequest>(args.clone())
        .map(|request| (request.id, request.label))
        .map_err(|_| "A status is made from an id and a label")
}

/// Read a `set` or `clear` request exactly: one id, and nothing beyond it.
pub fn parse_one(args: &Value) -> Result<String, &'static str> {
    serde_json::from_value::<OneRequest>(args.clone())
        .map(|request| request.id)
        .map_err(|_| "A status is named by its id")
}

/// What a plugin is told when the application refuses. One sentence per reason and the same for `define`, `set` and `clear`, and never one
/// that says whether another plugin has a status of that name: a plugin must not be able to find out what the others made.
pub fn refusal(refused: &crate::statuses::Refused) -> &'static str {
    use crate::statuses::Refused;
    match refused {
        Refused::Invalid => {
            "That is not a valid status: an id is letters, digits, dots, dashes and underscores, a label is plain text up to 80 characters, and online, sleeping and offline are the application's own"
        }
        Refused::TooManyDefined => "This plugin already has as many statuses as it may have",
        Refused::Undefined => "This plugin has no such status",
        Refused::TooManyActive => "That many statuses are already on",
    }
}

/// What came of one `define`, `set` or `clear`: what the plugin is answered, and whether what this device says of itself may have changed
/// (so that the paired devices are told). A refusal is one sentence and changes nothing.
pub struct Applied {
    pub answer: Result<Value, &'static str>,
    pub changed: bool,
}

/// Carry out one of the three methods that change a status, for the plugin the HOST knows is calling: the one thing both the application and
/// its tests run, so what is tried is what runs. `method` is one of [`STATUS_DEFINE`], [`STATUS_SET`] and [`STATUS_CLEAR`]; any other is not
/// one this function carries out and changes nothing.
pub fn apply(
    statuses: &mut crate::statuses::Statuses,
    plugin: &peersus_core::plugins::PluginId,
    method: &str,
    args: &Value,
) -> Applied {
    let refused = |reason: &'static str| Applied {
        answer: Err(reason),
        changed: false,
    };
    match method {
        STATUS_DEFINE => match parse_define(args) {
            Err(reason) => refused(reason),
            Ok((id, label)) => match statuses.define(plugin, &id, &label) {
                // Defining does not turn it on or off, but rewording one that is on changes what the device says.
                Ok(made) => Applied {
                    answer: Ok(json!({ "id": made.key.id, "label": made.label })),
                    changed: true,
                },
                Err(reason) => refused(refusal(&reason)),
            },
        },
        STATUS_SET | STATUS_CLEAR => match parse_one(args) {
            Err(reason) => refused(reason),
            Ok(id) => {
                let done = if method == STATUS_SET {
                    statuses.set(plugin, &id)
                } else {
                    statuses.clear(plugin, &id)
                };
                match done {
                    Ok(()) => Applied {
                        answer: Ok(json!({})),
                        changed: true,
                    },
                    Err(reason) => refused(refusal(&reason)),
                }
            }
        },
        _ => refused("The application does not offer that"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_core::{
        model::PeerId,
        plugins::{PluginId, StatusId, StatusKey, StatusLabel},
    };
    use serde_json::json;

    fn call(
        statuses: &mut crate::statuses::Statuses,
        plugin: &str,
        method: &str,
        args: Value,
    ) -> Applied {
        apply(statuses, &PluginId::new(plugin).unwrap(), method, &args)
    }
    fn on(statuses: &crate::statuses::Statuses) -> Vec<(String, String, String)> {
        statuses
            .active()
            .iter()
            .map(|s| {
                (
                    s.key.plugin.as_str().into(),
                    s.key.id.as_str().into(),
                    s.label.as_str().into(),
                )
            })
            .collect()
    }

    #[test]
    fn a_plugin_makes_two_statuses_of_its_own_turns_one_on_and_off_and_the_device_says_each_change()
    {
        let mut statuses = crate::statuses::Statuses::default();
        // Its own statuses, with its own words: two of them, nothing listed by the application.
        let made = call(
            &mut statuses,
            "player",
            STATUS_DEFINE,
            json!({ "id": "watching", "label": "Watching a video" }),
        );
        assert_eq!(
            made.answer.unwrap(),
            json!({ "id": "watching", "label": "Watching a video" })
        );
        assert!(made.changed);
        call(
            &mut statuses,
            "player",
            STATUS_DEFINE,
            json!({ "id": "paused", "label": "Paused" }),
        )
        .answer
        .unwrap();
        assert!(
            on(&statuses).is_empty(),
            "making a status said something of the device before it was turned on"
        );
        // Turned on: the device now says it, with whose it is and the words of its author.
        assert!(
            call(
                &mut statuses,
                "player",
                STATUS_SET,
                json!({ "id": "watching" })
            )
            .answer
            .is_ok()
        );
        assert_eq!(
            on(&statuses),
            [(
                "player".into(),
                "watching".into(),
                "Watching a video".into()
            )]
        );
        // A second one beside it, and then the first off: what is on is exactly what was turned on.
        call(
            &mut statuses,
            "player",
            STATUS_SET,
            json!({ "id": "paused" }),
        )
        .answer
        .unwrap();
        assert_eq!(on(&statuses).len(), 2);
        call(
            &mut statuses,
            "player",
            STATUS_CLEAR,
            json!({ "id": "watching" }),
        )
        .answer
        .unwrap();
        assert_eq!(
            on(&statuses),
            [("player".into(), "paused".into(), "Paused".into())]
        );
    }

    #[test]
    fn what_a_plugin_is_told_when_refused_is_one_sentence_and_nothing_changed() {
        let mut statuses = crate::statuses::Statuses::default();
        call(
            &mut statuses,
            "player",
            STATUS_DEFINE,
            json!({ "id": "ok", "label": "Fine" }),
        )
        .answer
        .unwrap();
        for (method, args, why) in [
            (
                STATUS_DEFINE,
                json!({ "id": "offline", "label": "Fine" }),
                "the application's own word as an id",
            ),
            (
                STATUS_DEFINE,
                json!({ "id": "ok2", "label": "Online" }),
                "the application's own word as a label",
            ),
            (
                STATUS_DEFINE,
                json!({ "id": "ok2", "label": "a\u{202e}b" }),
                "a label that redirects the text",
            ),
            (
                STATUS_DEFINE,
                json!({ "id": "a b", "label": "Fine" }),
                "an id that is not an identifier",
            ),
            (STATUS_DEFINE, json!({ "id": "ok2" }), "no label"),
            (
                STATUS_DEFINE,
                json!({ "id": "ok2", "label": "Fine", "plugin": "other" }),
                "a plugin naming who it is",
            ),
            (
                STATUS_SET,
                json!({ "id": "never-made" }),
                "turning on what was never made",
            ),
            (STATUS_SET, json!({}), "no id"),
            (
                STATUS_SET,
                json!({ "id": "ok", "plugin": "other" }),
                "a plugin naming who it speaks for",
            ),
            (STATUS_CLEAR, json!({ "id": 5 }), "an id that is not text"),
            (
                "explode",
                json!({}),
                "a method the application does not have",
            ),
        ] {
            let refused = call(&mut statuses, "player", method, args);
            assert!(refused.answer.is_err(), "{why} was accepted");
            assert!(!refused.changed, "{why} changed what the device says");
        }
        assert!(on(&statuses).is_empty());
        assert_eq!(
            statuses.defined_by(&PluginId::new("player").unwrap()).len(),
            1,
            "a refused call made or removed something"
        );
    }

    #[test]
    fn a_plugin_cannot_turn_on_turn_off_or_reword_the_statuses_of_another() {
        let mut statuses = crate::statuses::Statuses::default();
        call(
            &mut statuses,
            "owner",
            STATUS_DEFINE,
            json!({ "id": "busy", "label": "Busy" }),
        )
        .answer
        .unwrap();
        call(&mut statuses, "owner", STATUS_SET, json!({ "id": "busy" }))
            .answer
            .unwrap();
        // The intruder says the same id, because an identifier is only ever one plugin's: it names its own, which does not exist.
        for method in [STATUS_SET, STATUS_CLEAR] {
            let refused = call(&mut statuses, "intruder", method, json!({ "id": "busy" }));
            assert!(
                refused.answer.is_err(),
                "{method} reached the status of another plugin"
            );
        }
        // And making its own status of the same id creates a different one: the owner's words and state are untouched.
        call(
            &mut statuses,
            "intruder",
            STATUS_DEFINE,
            json!({ "id": "busy", "label": "Pretending" }),
        )
        .answer
        .unwrap();
        assert_eq!(
            on(&statuses),
            [("owner".into(), "busy".into(), "Busy".into())]
        );
        // The refusal does not say whether the other plugin has such a status: it is the same sentence as for one nobody made.
        let other = call(
            &mut statuses,
            "intruder",
            STATUS_SET,
            json!({ "id": "never-existed" }),
        )
        .answer
        .unwrap_err();
        let mine = call(&mut statuses, "nobody", STATUS_SET, json!({ "id": "busy" }))
            .answer
            .unwrap_err();
        assert_eq!(
            other, mine,
            "the refusal tells whether another plugin has such a status"
        );
    }
    fn status(plugin: &str, id: &str, label: &str) -> StatusInfo {
        StatusInfo {
            key: StatusKey {
                plugin: PluginId::new(plugin).unwrap(),
                id: StatusId::new(id).unwrap(),
            },
            label: StatusLabel::new(label).unwrap(),
        }
    }
    fn peer(name: &str, online: bool, sleeping: bool, statuses: Vec<StatusInfo>) -> Peer {
        peer_with_key("KEY-SECRET-0123456789", name, online, sleeping, statuses)
    }
    fn peer_with_key(
        key: &str,
        name: &str,
        online: bool,
        sleeping: bool,
        statuses: Vec<StatusInfo>,
    ) -> Peer {
        Peer {
            id: PeerId(key.into()),
            name: name.into(),
            paired_at: 1,
            last_seen: None,
            online,
            sleeping,
            statuses,
            grants: Default::default(),
            plugin_values: Default::default(),
        }
    }
    fn reader(id: &str) -> (crate::aliases::Aliases, PluginId) {
        (crate::aliases::Aliases::new(), PluginId::new(id).unwrap())
    }

    #[test]
    fn a_status_is_told_with_whose_it_is_what_it_is_called_and_the_words_of_its_author() {
        let (aliases, reader) = reader("reader");
        let phone = peer(
            "Phone",
            true,
            false,
            vec![status("player", "watching", "Watching a video")],
        );
        let said = list(&aliases, &reader, std::slice::from_ref(&phone));
        assert_eq!(
            said,
            json!([{
                "device": aliases.alias(&reader, &phone.id),
                "name": "Phone",
                "presence": "online",
                "statuses": [{ "plugin": "player", "id": "watching", "label": "Watching a video" }],
            }])
        );
    }

    #[test]
    fn the_word_device_means_here_what_it_means_in_peers_list_so_the_two_can_be_used_together() {
        // `peers.list` says `device` for the alias the plugin knows a device by and `name` for what it calls itself. Here it is the same, or a
        // plugin that reads both would match nothing.
        let (aliases, reader) = reader("reader");
        let peers = [
            peer_with_key("KEY-A", "Phone", true, false, vec![]),
            peer_with_key("KEY-B", "Laptop", false, false, vec![]),
        ];
        let here = list(&aliases, &reader, &peers);
        let there = crate::runtime::peers_listing(&aliases, &reader, &peers);
        for index in 0..2 {
            assert_eq!(
                here[index]["device"], there[index]["device"],
                "device is the alias in both"
            );
            assert_eq!(
                here[index]["name"], there[index]["name"],
                "name is what the device calls itself in both"
            );
        }
    }

    #[test]
    fn two_devices_of_one_name_are_told_apart_in_a_read_and_each_plugin_sees_its_own_alias() {
        let (aliases, one) = reader("one");
        let other = PluginId::new("other").unwrap();
        let peers = [
            peer_with_key("KEY-A", "Phone", true, false, vec![]),
            peer_with_key("KEY-B", "Phone", true, false, vec![]),
        ];
        let seen_by_one = list(&aliases, &one, &peers);
        assert_ne!(
            seen_by_one[0]["device"], seen_by_one[1]["device"],
            "two devices of one name must not read as one"
        );
        assert_eq!(seen_by_one[0]["name"], seen_by_one[1]["name"]);
        // And a different plugin is given different aliases for the same devices: the two cannot compare what they read.
        let seen_by_other = list(&aliases, &other, &peers);
        assert_ne!(seen_by_one[0]["device"], seen_by_other[0]["device"]);
    }

    #[test]
    fn the_three_words_of_the_application_are_the_only_ones_in_the_place_of_presence() {
        let (aliases, reader) = reader("reader");
        let said = list(
            &aliases,
            &reader,
            &[
                peer_with_key("1", "a", true, false, vec![]),
                peer_with_key("2", "b", true, true, vec![]),
                peer_with_key("3", "c", false, false, vec![]),
                // Whatever a plugin said, presence stays one of the application's words.
                peer_with_key(
                    "4",
                    "d",
                    true,
                    false,
                    vec![status("p", "offline-ish", "Not really offline")],
                ),
            ],
        );
        let words: Vec<&str> = said
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["presence"].as_str().unwrap())
            .collect();
        assert_eq!(words, ["online", "sleeping", "offline", "online"]);
    }

    #[test]
    fn a_device_that_is_not_connected_shows_no_status_and_the_key_is_never_in_what_is_told() {
        let (aliases, reader) = reader("reader");
        let gone = peer(
            "Gone",
            false,
            false,
            vec![status("player", "watching", "Watching")],
        );
        let said = list(&aliases, &reader, std::slice::from_ref(&gone));
        assert_eq!(
            said[0]["statuses"],
            json!([]),
            "a device that is not connected says nothing"
        );
        assert!(
            !said.to_string().contains("KEY-SECRET"),
            "a key was told to a plugin"
        );
    }

    #[test]
    fn what_is_told_to_everybody_has_no_alias_because_an_alias_is_one_plugins_own() {
        let (_, record) = to_tell(
            None,
            &[peer_with_key("KEY-A", "Phone", true, false, vec![])],
            true,
        );
        let (told, _) = to_tell(
            record.as_ref(),
            &[peer_with_key("KEY-A", "Phone", true, true, vec![])],
            true,
        );
        assert_eq!(told.len(), 1);
        assert!(
            told[0].get("device").is_none(),
            "the event carries an alias, which is different for every plugin"
        );
        assert_eq!(told[0]["name"], "Phone");
        assert_eq!(told[0]["presence"], "sleeping");
        assert!(
            !told[0].to_string().contains("KEY-A"),
            "a key was told to a plugin"
        );
    }

    #[test]
    fn a_change_is_found_by_the_device_and_not_by_what_it_says_so_two_of_one_name_cannot_hide_each_other()
     {
        // Two devices of one name, one online and one gone: nothing is told on the first look.
        let before = [
            peer_with_key("KEY-A", "Phone", true, false, vec![]),
            peer_with_key("KEY-B", "Phone", false, false, vec![]),
        ];
        let (told, record) = to_tell(None, &before, true);
        assert!(told.is_empty(), "the first look tells nothing");
        // The second one comes online: it now says exactly what the first says, and it must still be told.
        let after = [
            peer_with_key("KEY-A", "Phone", true, false, vec![]),
            peer_with_key("KEY-B", "Phone", true, false, vec![]),
        ];
        let (told, record) = to_tell(record.as_ref(), &after, true);
        assert_eq!(
            told.len(),
            1,
            "the second device coming online was lost behind the first, which says the same"
        );
        // Nothing changed since: nothing is told again.
        let (told, _) = to_tell(record.as_ref(), &after, true);
        assert!(told.is_empty(), "the same thing was told twice");
    }

    #[test]
    fn with_nobody_listening_nothing_is_told_or_kept_and_a_new_device_is_told_of() {
        let one = [peer_with_key("KEY-A", "Phone", true, false, vec![])];
        let (told, record) = to_tell(None, &one, false);
        assert!(
            told.is_empty() && record.is_none(),
            "something was kept for nobody"
        );
        // A device that was not there before is told of; one that left is not.
        let (_, record) = to_tell(None, &one, true);
        let two = [
            peer_with_key("KEY-A", "Phone", true, false, vec![]),
            peer_with_key("KEY-B", "New", true, false, vec![]),
        ];
        let (told, record) = to_tell(record.as_ref(), &two, true);
        assert_eq!(told.len(), 1);
        assert_eq!(told[0]["name"], "New");
        let (told, _) = to_tell(record.as_ref(), &one, true);
        assert!(told.is_empty(), "a device that left was told of");
    }

    #[test]
    fn every_permission_has_words_a_person_can_read_and_the_three_are_different() {
        let all = [
            STATUS_DEFINE_DESCRIPTION,
            STATUS_SET_DESCRIPTION,
            STATUS_WATCH_DESCRIPTION,
        ];
        for words in all {
            assert!(words.len() > 40 && words.chars().count() <= 280, "{words}");
        }
        let distinct: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(distinct.len(), 3);
        // The one that is SEEN says so plainly.
        assert!(STATUS_SET_DESCRIPTION.contains("SHOWN"));
    }
}
