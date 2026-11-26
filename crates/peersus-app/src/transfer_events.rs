//! What the application says to plugins about transfers: facts with an identity, never an instant to be guessed from.
//!
//! A plugin that colours a file when it is done, or lights something when it arrives, must know WHICH file and WHAT happened to it. It is told so by
//! the application, which sees every change, and not left to compare two lists. What it is told is the least that does the job, and what is personal
//! is behind a permission of its own:
//!
//! * `changed` (capability `transfers.watch`): the transfer's id, the way it goes, its state as one closed word, and how many bytes of how many. No
//!   name of a file, no name of a device, no path, no reason for a failure (a reason is free text and may name a place on the disk).
//! * `changed-named` (capability `transfers.names`): the same, and the name of the file and the name the device gives itself. Two events and not one
//!   with an optional field, because the hub hands ONE payload to everyone who subscribed to an event: a name could not be withheld from one
//!   subscriber and given to another, so a plugin without the second permission is not subscribed to it at all. For the same reason nothing in it is
//!   made for one plugin (an alias of a device is, and is only ever given by `peersus.peers`): everything in it is the same for every subscriber.
//!
//! Neither tells a plugin where a file was saved, what is in it, which key a device has, or anything about the history: only what changed now.
//! Reading is all there is; a plugin cannot accept, refuse or cancel a transfer through either.
use peersus_core::model::{Transfer, TransferState};
use serde_json::{Value, json};

/// The service the application offers plugins about transfers.
pub const TRANSFERS_SERVICE: &str = "peersus.transfers";
/// The event that says a transfer changed, without any name.
pub const TRANSFERS_CHANGED: &str = "changed";
/// The same event with the name of the file and the name the device gives itself; for plugins that were granted [`TRANSFERS_NAMES_CAPABILITY`].
pub const TRANSFERS_CHANGED_NAMED: &str = "changed-named";
/// The capability that guards [`TRANSFERS_CHANGED`].
pub const TRANSFERS_WATCH_CAPABILITY: &str = "transfers.watch";
/// The capability that guards [`TRANSFERS_CHANGED_NAMED`].
pub const TRANSFERS_NAMES_CAPABILITY: &str = "transfers.names";
/// What the first permission lets a plugin do, in the words a person reads when granting it. One copy, for the manifest and for whatever shows it.
pub const TRANSFERS_WATCH_DESCRIPTION: &str = "Be told when a transfer starts, moves, finishes, is cancelled or fails: which transfer, and how far. It cannot see the name of a file, \
     the name of a device, or where anything is saved, and it cannot accept, refuse or cancel a transfer.";
/// What the second permission adds.
pub const TRANSFERS_NAMES_DESCRIPTION: &str = "Also see the NAME of each file that is transferred and the name of the device it goes to or comes from. File names can say a lot about a person.";

/// A transfer's state as one closed word. The reason of a failure is left out on purpose.
fn state_word(state: &TransferState) -> &'static str {
    match state {
        TransferState::Offered => "offered",
        TransferState::Running => "running",
        TransferState::Complete => "complete",
        TransferState::Cancelled => "cancelled",
        TransferState::Failed { .. } => "failed",
    }
}

/// The unnamed fact about one transfer: the one definition, so that the named event can never carry less than this nor the unnamed more.
fn base(transfer: &Transfer) -> serde_json::Map<String, Value> {
    let mut entry = serde_json::Map::new();
    entry.insert("id".into(), json!(transfer.id));
    entry.insert(
        "direction".into(),
        json!(if transfer.incoming {
            "incoming"
        } else {
            "outgoing"
        }),
    );
    entry.insert("state".into(), json!(state_word(&transfer.state)));
    entry.insert("bytes".into(), json!(transfer.bytes));
    entry.insert("completed".into(), json!(transfer.completed_bytes));
    entry
}

/// What `changed` carries for a transfer.
pub fn changed(transfer: &Transfer) -> Value {
    Value::Object(base(transfer))
}

/// What `changed-named` carries: [`changed`], the name of the file, and the name the device gave itself (`null` when it gave none). Never the key of
/// the device, and nothing that is different for different plugins.
pub fn changed_named(transfer: &Transfer) -> Value {
    let mut entry = base(transfer);
    entry.insert("name".into(), json!(transfer.name));
    entry.insert("device".into(), json!(transfer.peer_name));
    Value::Object(entry)
}

/// The method that says what is going on now, without any name; guarded by [`TRANSFERS_WATCH_CAPABILITY`], like [`TRANSFERS_CHANGED`].
pub const TRANSFERS_ACTIVE: &str = "active";
/// The same with the names; guarded by [`TRANSFERS_NAMES_CAPABILITY`], like [`TRANSFERS_CHANGED_NAMED`].
pub const TRANSFERS_ACTIVE_NAMED: &str = "active-named";

/// What the transfers that are going now are told as, in the answer to a read: each one exactly as the matching event says it, so that what a plugin
/// is handed by asking can be neither more nor less than what it is handed by listening. `named` picks the event whose words are used.
pub fn active(now: &[Transfer], named: bool) -> Value {
    Value::Array(
        going_for_a_new_listener(now)
            .into_iter()
            .map(|transfer| {
                if named {
                    changed_named(transfer)
                } else {
                    changed(transfer)
                }
            })
            .collect(),
    )
}

/// What the application must tell plugins now, and what it must remember having told: the one decision behind every publish.
///
/// * `told` is what was remembered last time: `None` before the first look.
/// * The first look tells nothing (it is the history the person already had) and only remembers.
/// * With nobody listening nothing is told and nothing is kept, so the record is not copied for plugins that do not exist.
/// * Otherwise it is what changed since last time, once each.
///
/// Returns the transfers to tell of, and the record to remember (`None` when nobody listens, so the next look is a first look again and a plugin
/// that comes later is told by [`going_for_a_new_listener`] and not by a stale difference).
pub fn to_tell<'a>(
    told: Option<&[Transfer]>,
    now: &'a [Transfer],
    anyone_listens: bool,
) -> (Vec<&'a Transfer>, Option<Vec<Transfer>>) {
    if !anyone_listens {
        return (Vec::new(), None);
    }
    match told {
        None => (going_for_a_new_listener(now), Some(now.to_vec())),
        Some(before) => (changed_between(before, now), Some(now.to_vec())),
    }
}

/// The transfers that changed between two looks at the record: new ones, and ones whose state or progress is not what it was. A transfer that is
/// the same as before is not told again, and one that is no longer in the record is not told at all (the history is the person's, not an event).
pub fn changed_between<'a>(before: &[Transfer], now: &'a [Transfer]) -> Vec<&'a Transfer> {
    now.iter()
        .filter(|transfer| {
            before
                .iter()
                .find(|old| old.id == transfer.id)
                .is_none_or(|old| {
                    old.state != transfer.state || old.completed_bytes != transfer.completed_bytes
                })
        })
        .collect()
}

/// What a plugin that has only just begun to listen is told of, since the transfers that are going began before it could hear them: those that are still
/// going (offered, or moving), once each, so that it never has to guess that something is under way. What is over is history, and is not told.
pub fn going_for_a_new_listener(now: &[Transfer]) -> Vec<&Transfer> {
    now.iter()
        .filter(|transfer| transfer.state.is_active())
        .collect()
}

#[cfg(test)]
mod new_listener_tests {
    use super::*;
    use peersus_core::model::PeerId;

    fn transfer(id: &str, state: TransferState) -> Transfer {
        Transfer {
            id: id.into(),
            peer: PeerId("p".into()),
            peer_name: None,
            name: "n".into(),
            folders: Vec::new(),
            bytes: 10,
            completed_bytes: 0,
            incoming: false,
            state,
            started_at: 1,
            code: None,
            expires_at: None,
            saved_as: None,
        }
    }

    #[test]
    fn a_listener_that_comes_late_is_told_of_what_is_still_going_and_never_of_the_history() {
        let record = vec![
            transfer("old", TransferState::Complete),
            transfer("gone", TransferState::Cancelled),
            transfer("failed", TransferState::Failed { reason: "x".into() }),
            transfer("waiting", TransferState::Offered),
            transfer("moving", TransferState::Running),
        ];
        let ids: Vec<&str> = going_for_a_new_listener(&record)
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(ids, ["waiting", "moving"]);
    }

    #[test]
    fn a_record_with_nothing_going_tells_a_new_listener_nothing() {
        let history: Vec<Transfer> = (0..1_000)
            .map(|n| transfer(&format!("o{n}"), TransferState::Complete))
            .collect();
        assert!(going_for_a_new_listener(&history).is_empty());
    }
}

#[cfg(test)]
mod words_tests {
    use super::*;

    #[test]
    fn every_text_a_person_reads_for_transfers_fits_in_what_a_manifest_allows() {
        for (name, text) in [
            ("watch", TRANSFERS_WATCH_DESCRIPTION),
            ("names", TRANSFERS_NAMES_DESCRIPTION),
        ] {
            let length = text.chars().count();
            assert!(
                length <= peersus_api::manifest::MAX_DESCRIPTION,
                "the words of the {name} permission are {length} characters, and a manifest allows {}",
                peersus_api::manifest::MAX_DESCRIPTION
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use peersus_core::model::PeerId;

    fn transfer(id: &str, state: TransferState, completed: u64) -> Transfer {
        Transfer {
            id: id.into(),
            peer: PeerId("a-device-key-that-must-not-leave".into()),
            peer_name: Some("Alex's laptop".into()),
            name: "secret-plans.pdf".into(),
            folders: Vec::new(),
            bytes: 100,
            completed_bytes: completed,
            incoming: false,
            state,
            started_at: 1,
            code: None,
            expires_at: None,
            saved_as: Some("/home/alex/Documents/secret-plans.pdf".into()),
        }
    }

    #[test]
    fn the_unnamed_event_says_which_transfer_and_how_far_and_nothing_a_person_would_call_private() {
        let value = changed(&transfer("t1", TransferState::Running, 40));
        assert_eq!(
            value,
            json!({ "id": "t1", "direction": "outgoing", "state": "running", "bytes": 100, "completed": 40 })
        );
        let text = value.to_string();
        for leaked in ["secret-plans", "Alex", "a-device-key", "/home", "Documents"] {
            assert!(
                !text.contains(leaked),
                "the unnamed event carried {leaked}: {text}"
            );
        }
    }

    #[test]
    fn a_failure_is_told_as_the_word_failed_and_never_with_its_reason() {
        let failed = transfer(
            "t1",
            TransferState::Failed {
                reason: "could not write /home/alex/Documents/x".into(),
            },
            10,
        );
        let text = changed(&failed).to_string();
        assert!(text.contains("\"failed\""));
        assert!(
            !text.contains("could not write") && !text.contains("/home"),
            "the reason of a failure reached a plugin: {text}"
        );
    }

    #[test]
    fn the_named_event_adds_the_file_name_and_the_name_the_device_gave_itself_and_never_the_key_or_the_path()
     {
        let value = changed_named(&transfer("t1", TransferState::Complete, 100));
        assert_eq!(value["name"], "secret-plans.pdf");
        assert_eq!(value["device"], "Alex's laptop");
        assert_eq!(value["state"], "complete");
        let text = value.to_string();
        for leaked in ["a-device-key", "/home", "Documents"] {
            assert!(
                !text.contains(leaked),
                "the named event carried {leaked}: {text}"
            );
        }
    }

    #[test]
    fn a_device_that_gave_no_name_is_null_and_not_its_key() {
        let mut anonymous = transfer("t1", TransferState::Complete, 100);
        anonymous.peer_name = None;
        let value = changed_named(&anonymous);
        assert!(value["device"].is_null());
        assert!(!value.to_string().contains("a-device-key"));
    }

    #[test]
    fn the_named_event_carries_everything_the_unnamed_one_does_so_the_two_can_never_disagree() {
        let t = transfer("t1", TransferState::Running, 55);
        let plain = changed(&t);
        let named = changed_named(&t);
        for key in plain.as_object().unwrap().keys() {
            assert_eq!(
                plain[key], named[key],
                "{key} differs between the two events"
            );
        }
    }

    #[test]
    fn nothing_in_the_named_event_is_made_for_one_plugin_so_every_subscriber_gets_the_same_payload()
    {
        let t = transfer("t1", TransferState::Complete, 100);
        assert_eq!(changed_named(&t), changed_named(&t));
    }

    #[test]
    fn only_what_changed_is_told_and_what_is_the_same_is_not_told_again() {
        let before = vec![
            transfer("a", TransferState::Running, 10),
            transfer("b", TransferState::Complete, 100),
        ];
        let now = vec![
            transfer("a", TransferState::Running, 10),
            transfer("b", TransferState::Complete, 100),
            transfer("c", TransferState::Offered, 0),
        ];
        let ids: Vec<&str> = changed_between(&before, &now)
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(ids, ["c"], "an unchanged transfer was told again: {ids:?}");
    }

    #[test]
    fn a_transfer_that_moved_or_finished_is_told_and_each_one_exactly_once() {
        let before = vec![
            transfer("a", TransferState::Running, 10),
            transfer("b", TransferState::Running, 90),
        ];
        let now = vec![
            transfer("a", TransferState::Running, 50),
            transfer("b", TransferState::Complete, 100),
        ];
        let ids: Vec<&str> = changed_between(&before, &now)
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn a_whole_history_that_was_already_there_is_never_told_as_news() {
        let history: Vec<Transfer> = (0..5_000)
            .map(|n| transfer(&format!("old-{n}"), TransferState::Complete, 100))
            .collect();
        assert!(changed_between(&history, &history).is_empty());
    }

    #[test]
    fn a_transfer_that_left_the_record_is_not_told_as_a_change() {
        let before = vec![transfer("a", TransferState::Complete, 100)];
        assert!(changed_between(&before, &[]).is_empty());
    }

    #[test]
    fn the_capabilities_are_two_and_each_has_words_a_person_can_read() {
        assert_ne!(TRANSFERS_WATCH_CAPABILITY, TRANSFERS_NAMES_CAPABILITY);
        assert!(TRANSFERS_WATCH_DESCRIPTION.contains("cannot see the name"));
        assert!(TRANSFERS_NAMES_DESCRIPTION.contains("NAME"));
        assert_ne!(TRANSFERS_CHANGED, TRANSFERS_CHANGED_NAMED);
    }
}

#[cfg(test)]
mod to_tell_tests {
    use super::*;
    use peersus_core::model::PeerId;

    fn transfer(id: &str, state: TransferState, completed: u64) -> Transfer {
        Transfer {
            id: id.into(),
            peer: PeerId("p".into()),
            peer_name: None,
            name: "n".into(),
            folders: Vec::new(),
            bytes: 100,
            completed_bytes: completed,
            incoming: false,
            state,
            started_at: 1,
            code: None,
            expires_at: None,
            saved_as: None,
        }
    }
    fn ids(told: &[&Transfer]) -> Vec<String> {
        told.iter().map(|t| t.id.clone()).collect()
    }

    #[test]
    fn nobody_listening_tells_nothing_and_keeps_nothing() {
        let now = vec![transfer("a", TransferState::Running, 10)];
        let (told, keep) = to_tell(None, &now, false);
        assert!(told.is_empty());
        assert!(
            keep.is_none(),
            "the record was copied for plugins that do not exist"
        );
        let before = vec![transfer("a", TransferState::Running, 5)];
        assert!(to_tell(Some(&before), &now, false).0.is_empty());
    }

    #[test]
    fn the_first_look_tells_only_what_is_going_and_never_the_history() {
        let now = vec![
            transfer("old", TransferState::Complete, 100),
            transfer("gone", TransferState::Cancelled, 5),
            transfer("moving", TransferState::Running, 40),
            transfer("waiting", TransferState::Offered, 0),
        ];
        let (told, keep) = to_tell(None, &now, true);
        assert_eq!(
            ids(&told),
            ["moving", "waiting"],
            "the history was told as news on the first look"
        );
        assert_eq!(
            keep.as_deref().map(<[Transfer]>::len),
            Some(4),
            "what was seen was not remembered for the next look"
        );
    }

    #[test]
    fn after_that_only_what_changed_is_told_once_each() {
        let before = vec![
            transfer("a", TransferState::Running, 10),
            transfer("b", TransferState::Running, 90),
        ];
        let now = vec![
            transfer("a", TransferState::Running, 10),
            transfer("b", TransferState::Complete, 100),
            transfer("c", TransferState::Offered, 0),
        ];
        let (told, _) = to_tell(Some(&before), &now, true);
        assert_eq!(ids(&told), ["b", "c"]);
    }

    #[test]
    fn when_the_last_plugin_goes_the_memory_goes_and_one_that_comes_later_is_told_what_is_going_and_not_a_stale_difference()
     {
        let now = vec![
            transfer("moving", TransferState::Running, 40),
            transfer("done", TransferState::Complete, 100),
        ];
        let (_, keep) = to_tell(Some(&now), &now, false);
        assert!(keep.is_none());
        let (told, _) = to_tell(keep.as_deref(), &now, true);
        assert_eq!(ids(&told), ["moving"]);
    }
}

#[cfg(test)]
mod finishing_tests {
    use super::*;
    use peersus_core::model::PeerId;

    fn transfer(id: &str, state: TransferState, completed: u64) -> Transfer {
        Transfer {
            id: id.into(),
            peer: PeerId("p".into()),
            peer_name: None,
            name: "n".into(),
            folders: Vec::new(),
            bytes: 100,
            completed_bytes: completed,
            incoming: false,
            state,
            started_at: 1,
            code: None,
            expires_at: None,
            saved_as: None,
        }
    }

    #[test]
    fn a_transfer_that_goes_from_running_to_complete_is_told_once_as_complete_and_not_again_while_it_stays_in_the_record()
     {
        // The record the owner publishes keeps what is finished, so a finished transfer is still there on the next look: it is not new, and it is
        // not told again.
        let running = vec![transfer("t", TransferState::Running, 60)];
        let complete = vec![transfer("t", TransferState::Complete, 100)];
        let (_, remembered) = to_tell(None, &running, true);
        let (told, remembered) = to_tell(remembered.as_deref(), &complete, true);
        assert_eq!(told.len(), 1);
        assert_eq!(changed(told[0])["state"], "complete");
        assert_eq!(changed(told[0])["completed"], 100);
        let (told, remembered) = to_tell(remembered.as_deref(), &complete, true);
        assert!(
            told.is_empty(),
            "a finished transfer was told again: {told:?}"
        );
        let (told, _) = to_tell(remembered.as_deref(), &complete, true);
        assert!(told.is_empty());
    }
}
