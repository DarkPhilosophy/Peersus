//! Two hosts that really paired, and whether each one is asleep, as the other one sees it.
//!
//! What this shows that the request's own tests do not: that a device which says it sleeps is shown as sleeping and still
//! counted as online (a plugin and the saved file read `online`), that waking is told as well, that a device which connects
//! while the other one sleeps hears it without being told again, and that a connection lost while asleep is gone and not
//! asleep.

use iroh::SecretKey;
use peersus_core::{
    config::NetworkSettings,
    model::{Peer, PeerId, Presence},
    plugins::{MAX_ACTIVE_STATUSES, PluginId, StatusId, StatusInfo, StatusKey, StatusLabel},
};
use peersus_net::{
    history::History,
    network_host::{Event, NetworkHost},
    peers::Peers,
    transport,
};
use std::{path::Path, time::Duration};

async fn drive(
    a: &mut NetworkHost,
    b: &mut NetworkHost,
    done: impl Fn(&NetworkHost, &NetworkHost) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !done(a, b) {
            let event =
                tokio::select! { event = a.next(101) => event, event = b.next(101) => event };
            match event {
                Event::Changed | Event::Picture { .. } => {}
                Event::Error(reason) => panic!("Unexpected host failure: {reason}"),
                Event::Plugin { .. } => panic!("No plugin stream expected"),
            }
        }
    })
    .await
    .expect("the hosts did not reach the expected state in time");
}

async fn host(directory: &Path, name: &str, key: SecretKey) -> NetworkHost {
    let policy = NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..NetworkSettings::default()
    };
    let endpoint = transport::bind(key, &policy).await.unwrap();
    NetworkHost::new(
        endpoint,
        name.into(),
        Peers::open(directory.join(format!("peers-{name}.json"))).unwrap(),
        History::open(directory.join(format!("history-{name}.json"))).unwrap(),
    )
}

/// Pair `a` with `b` and wait until each is online for the other.
async fn pair(a: &mut NetworkHost, b: &mut NetworkHost) {
    a.join(b.invite(100).unwrap()).unwrap();
    drive(a, b, |a, b| {
        !a.pairing().is_empty() && !b.pairing().is_empty()
    })
    .await;
    let pa = a.pairing().remove(0);
    let pb = b.pairing().remove(0);
    a.choose(&pa.id, pa.options[0]).unwrap();
    b.choose(&pb.id, pa.options[0]).unwrap();
    drive(a, b, |a, b| {
        a.peers().iter().any(|p| p.online) && b.peers().iter().any(|p| p.online)
    })
    .await;
}

/// How `host` sees the device `of`.
fn seen(host: &NetworkHost, of: &PeerId) -> Peer {
    host.peers()
        .into_iter()
        .find(|peer| &peer.id == of)
        .expect("the device is remembered")
}

#[tokio::test]
async fn a_device_that_says_it_sleeps_is_shown_asleep_and_still_online_until_it_wakes() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (peer_a, peer_b) = (
        PeerId(key_a.public().to_string()),
        PeerId(key_b.public().to_string()),
    );
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b).await;
    pair(&mut a, &mut b).await;
    assert_eq!(
        seen(&b, &peer_a).presence(),
        Presence::Online,
        "a device that has said nothing is awake"
    );

    a.set_asleep(true);
    drive(&mut a, &mut b, |_, b| seen(b, &peer_a).sleeping).await;
    let asleep = seen(&b, &peer_a);
    assert_eq!(asleep.presence(), Presence::Sleeping);
    // The bool that the saved file and the plugins read is still true: it is there, it only answers slowly.
    assert!(
        asleep.online,
        "a sleeping device is still reachable, and a plugin must still see it as online"
    );
    // Sleeping is a fact about the device that said it, not about the one that heard it.
    assert_eq!(seen(&a, &peer_b).presence(), Presence::Online);

    a.set_asleep(false);
    drive(&mut a, &mut b, |_, b| !seen(b, &peer_a).sleeping).await;
    assert_eq!(seen(&b, &peer_a).presence(), Presence::Online);
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn a_device_that_connects_while_the_other_sleeps_is_told_without_being_asked() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let mut a = host(directory.path(), "a", key_a.clone()).await;
    let mut b = host(directory.path(), "b", key_b.clone()).await;
    pair(&mut a, &mut b).await;

    // B goes away and A goes to sleep while it is gone: nobody was connected to hear it.
    b.close().await;
    drive_one(&mut a, |a| a.peers().iter().all(|p| !p.online)).await;
    a.set_asleep(true);

    // B comes back, and the first thing it knows of A is that A sleeps: it was said on the new connection, not remembered.
    let mut b = host(directory.path(), "b", key_b).await;
    drive(&mut a, &mut b, |_, b| b.peers().iter().any(|p| p.online)).await;
    drive(&mut a, &mut b, |_, b| seen(b, &peer_a).sleeping).await;
    assert_eq!(seen(&b, &peer_a).presence(), Presence::Sleeping);
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn a_connection_lost_while_asleep_is_offline_and_a_new_one_is_awake() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let mut a = host(directory.path(), "a", key_a.clone()).await;
    let mut b = host(directory.path(), "b", key_b).await;
    pair(&mut a, &mut b).await;
    a.set_asleep(true);
    drive(&mut a, &mut b, |_, b| seen(b, &peer_a).sleeping).await;

    // A is gone altogether: asleep means connected and quiet, and this is not connected.
    a.close().await;
    drive_one(&mut b, |b| !seen(b, &peer_a).online).await;
    let gone = seen(&b, &peer_a);
    assert_eq!(gone.presence(), Presence::Offline);
    assert!(
        !gone.sleeping,
        "a device that is not connected is not asleep, it is gone"
    );

    // A starts again: a new process is awake until it says otherwise, whatever it said before.
    let mut a = host(directory.path(), "a", key_a).await;
    drive(&mut a, &mut b, |_, b| seen(b, &peer_a).online).await;
    assert_eq!(seen(&b, &peer_a).presence(), Presence::Online);
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn remembered_pairing_survives_years_and_restart_without_another_invitation() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let peer_b = PeerId(key_b.public().to_string());
    let mut a = host(directory.path(), "a", key_a.clone()).await;
    let mut b = host(directory.path(), "b", key_b).await;
    pair(&mut a, &mut b).await;
    let paired_at = seen(&b, &peer_a).paired_at;
    a.close().await;
    drive_one(&mut b, |b| !seen(b, &peer_a).online).await;
    let mut a = host(directory.path(), "a", key_a).await;
    let years_later = paired_at + 10 * 365 * 24 * 60 * 60;
    tokio::time::timeout(Duration::from_secs(15), async {
        while !seen(&a, &peer_b).online || !seen(&b, &peer_a).online {
            let event = tokio::select! {
                event = a.next(years_later) => event,
                event = b.next(years_later) => event,
            };
            assert!(matches!(event, Event::Changed | Event::Picture { .. }));
        }
    })
    .await
    .expect("permanent pairing did not reconnect after restart");
    assert_eq!(seen(&b, &peer_a).paired_at, paired_at);
    assert!(a.pairing().is_empty() && b.pairing().is_empty());
    assert!(
        Peers::open(directory.path().join("peers-a.json"))
            .unwrap()
            .get(&peer_b)
            .is_some()
    );
    a.close().await;
    b.close().await;
}

/// Drive one host alone until `done`, for the moments when the other has closed.
async fn drive_one(host: &mut NetworkHost, done: impl Fn(&NetworkHost) -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !done(host) {
            match host.next(101).await {
                Event::Changed | Event::Picture { .. } => {}
                Event::Error(reason) => panic!("Unexpected host failure: {reason}"),
                Event::Plugin { .. } => panic!("No plugin stream expected"),
            }
        }
    })
    .await
    .expect("the host did not reach the expected state in time");
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

#[tokio::test]
async fn a_status_alone_is_seen_by_the_other_device_with_its_owner_and_its_words_and_clearing_removes_it()
 {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b).await;
    pair(&mut a, &mut b).await;
    assert!(
        seen(&b, &peer_a).statuses.is_empty(),
        "nothing is said until a plugin says it"
    );

    // Awake, and with a status only: the sleep flag does not change, so only the statuses can tell this apart.
    let watching = status("player", "watching", "Watching a video");
    a.set_statuses(vec![watching.clone()]).unwrap();
    drive(&mut a, &mut b, |_, b| !seen(b, &peer_a).statuses.is_empty()).await;
    let said = seen(&b, &peer_a);
    assert_eq!(
        said.statuses,
        vec![watching.clone()],
        "the owner, the identifier and the words all arrive"
    );
    assert_eq!(
        said.presence(),
        Presence::Online,
        "a status is no change of being awake"
    );
    assert_eq!(said.custom_statuses().len(), 1);

    // Cleared: the other device does not keep a word nobody says any more.
    a.set_statuses(Vec::new()).unwrap();
    drive(&mut a, &mut b, |_, b| seen(b, &peer_a).statuses.is_empty()).await;
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn what_a_device_says_replaces_what_it_said_and_two_plugins_may_use_the_same_word() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b).await;
    pair(&mut a, &mut b).await;

    // The same identifier from two plugins: two statuses, and the other device keeps both with their owners.
    let (one, two) = (
        status("player", "busy", "Playing"),
        status("timer", "busy", "In a timer"),
    );
    a.set_statuses(vec![one.clone(), two.clone()]).unwrap();
    drive(&mut a, &mut b, |_, b| seen(b, &peer_a).statuses.len() == 2).await;
    assert_eq!(seen(&b, &peer_a).statuses, vec![one, two.clone()]);

    // What it says next is the whole of it, not an addition: the first one is gone and only the second remains.
    a.set_statuses(vec![two.clone()]).unwrap();
    drive(&mut a, &mut b, |_, b| seen(b, &peer_a).statuses.len() == 1).await;
    assert_eq!(seen(&b, &peer_a).statuses, vec![two]);
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn a_device_that_connects_later_is_told_the_statuses_without_asking_and_a_lost_connection_keeps_none()
 {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let mut a = host(directory.path(), "a", key_a.clone()).await;
    let mut b = host(directory.path(), "b", key_b.clone()).await;
    pair(&mut a, &mut b).await;

    // B goes away; A sets a status while nobody is connected to hear it, awake all the while.
    b.close().await;
    drive_one(&mut a, |a| a.peers().iter().all(|p| !p.online)).await;
    let busy = status("player", "busy", "Playing");
    a.set_statuses(vec![busy.clone()]).unwrap();

    // B comes back and the first thing it knows of A is the status, said on the new connection. A is not asleep: statuses alone.
    let mut b = host(directory.path(), "b", key_b).await;
    drive(&mut a, &mut b, |_, b| {
        seen(b, &peer_a).statuses == vec![busy.clone()]
    })
    .await;
    assert_eq!(seen(&b, &peer_a).presence(), Presence::Online);

    // A is gone altogether: a device that is not connected says nothing, so no status outlives its connection.
    a.close().await;
    drive_one(&mut b, |b| !seen(b, &peer_a).online).await;
    let gone = seen(&b, &peer_a);
    assert!(
        gone.statuses.is_empty(),
        "a status outlived the connection it was said on"
    );
    assert!(gone.custom_statuses().is_empty());
    b.close().await;
}

#[tokio::test]
async fn a_device_cannot_say_more_than_the_limit_or_one_status_twice_and_a_refusal_changes_nothing()
{
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b).await;
    pair(&mut a, &mut b).await;

    let one = status("player", "busy", "Playing");
    a.set_statuses(vec![one.clone()]).unwrap();
    let too_many: Vec<StatusInfo> = (0..=MAX_ACTIVE_STATUSES)
        .map(|i| status("p", &format!("s{i}"), "x"))
        .collect();
    assert!(
        a.set_statuses(too_many).is_err(),
        "more than the limit was accepted"
    );
    assert!(
        a.set_statuses(vec![one.clone(), one.clone()]).is_err(),
        "one status twice was accepted"
    );
    assert_eq!(
        a.own_statuses(),
        &[one],
        "a refusal changed what the device says"
    );
    a.close().await;
    b.close().await;
}

/// A host together with the store of its settings, so that a rename goes the way the application does it: through `update_settings`.
async fn host_with_settings(
    directory: &Path,
    name: &str,
    key: SecretKey,
) -> (NetworkHost, peersus_net::settings::SettingsStore) {
    let mut store = peersus_net::settings::SettingsStore::open(
        directory.join(format!("preferences-{name}.json")),
    )
    .unwrap();
    let settings = peersus_core::Settings {
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        },
        device_name: name.into(),
        ..Default::default()
    };
    store.replace(settings.clone()).unwrap();
    let endpoint = transport::bind(key, &settings.network).await.unwrap();
    let host = NetworkHost::new(
        endpoint,
        name.into(),
        Peers::open(directory.join(format!("peers-{name}.json"))).unwrap(),
        History::open(directory.join(format!("history-{name}.json"))).unwrap(),
    );
    (host, store)
}

/// What the application does when the person renames a device in Settings.
async fn rename(
    host: &mut NetworkHost,
    store: &mut peersus_net::settings::SettingsStore,
    name: &str,
) {
    let mut candidate = store.current().clone();
    candidate.device_name = name.into();
    host.update_settings(store, candidate).await.unwrap();
}

#[tokio::test]
async fn a_device_renamed_while_connected_is_called_by_its_new_name_by_the_other_one_and_it_is_kept()
 {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let (mut a, mut store_a) = host_with_settings(directory.path(), "Alex", key_a).await;
    let mut b = host(directory.path(), "Phone", key_b).await;
    pair(&mut a, &mut b).await;
    assert_eq!(
        seen(&b, &peer_a).name,
        "Alex",
        "it was paired under the name it had"
    );

    // Renamed in Settings, with the other device connected all the while: nobody asks and nobody re-pairs.
    rename(&mut a, &mut store_a, "ROG").await;
    drive(&mut a, &mut b, |_, b| seen(b, &peer_a).name == "ROG").await;
    assert_eq!(seen(&b, &peer_a).name, "ROG");
    a.close().await;
    b.close().await;

    // And it is KEPT: the other device remembers the new name, not only for as long as the connection lives.
    let reopened = Peers::open(directory.path().join("peers-Phone.json")).unwrap();
    let kept = reopened
        .entries()
        .find(|entry| entry.peer.id == peer_a)
        .expect("still remembered");
    assert_eq!(kept.peer.name, "ROG", "the new name was not saved");
}

#[tokio::test]
async fn a_device_renamed_while_the_other_was_away_is_called_by_its_new_name_when_it_comes_back() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let (mut a, mut store_a) = host_with_settings(directory.path(), "Alex", key_a).await;
    let mut b = host(directory.path(), "Phone", key_b.clone()).await;
    pair(&mut a, &mut b).await;

    // The other device is gone while this one is renamed: nothing could be told.
    b.close().await;
    drive_one(&mut a, |a| a.peers().iter().all(|p| !p.online)).await;
    rename(&mut a, &mut store_a, "ROG").await;

    // It comes back and learns the name on the new connection, with no further change made: the very case of a renamed phone that stays
    // under its old name for ever.
    let mut b = host(directory.path(), "Phone", key_b).await;
    assert_eq!(
        seen(&b, &peer_a).name,
        "Alex",
        "before it hears, it still has the old name"
    );
    drive(&mut a, &mut b, |_, b| seen(b, &peer_a).name == "ROG").await;
    assert_eq!(seen(&b, &peer_a).name, "ROG");
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn a_device_that_says_nothing_of_its_name_keeps_the_name_it_had_and_renaming_to_the_same_name_changes_nothing()
 {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let (mut a, mut store_a) = host_with_settings(directory.path(), "Alex", key_a).await;
    let mut b = host(directory.path(), "Phone", key_b).await;
    pair(&mut a, &mut b).await;
    // What a device says of itself besides its name (a status) changes without the name being touched.
    a.set_statuses(vec![status("player", "busy", "Playing")])
        .unwrap();
    drive(&mut a, &mut b, |_, b| !seen(b, &peer_a).statuses.is_empty()).await;
    assert_eq!(seen(&b, &peer_a).name, "Alex");
    // The same name again is no change at all: nothing is written and nothing is told.
    rename(&mut a, &mut store_a, "Alex").await;
    a.set_statuses(Vec::new()).unwrap();
    drive(&mut a, &mut b, |_, b| seen(b, &peer_a).statuses.is_empty()).await;
    assert_eq!(seen(&b, &peer_a).name, "Alex");
    a.close().await;
    b.close().await;

    // The store refuses what is not a rename of a remembered device: an unknown device, a blank name, and the same name is no change.
    let mut store = Peers::open(directory.path().join("peers-Phone.json")).unwrap();
    assert!(
        store.rename(&PeerId("nobody".into()), "X").is_err(),
        "an unknown device was renamed"
    );
    assert!(
        store.rename(&peer_a, "   ").is_err(),
        "a blank name was accepted"
    );
    assert!(
        !store.rename(&peer_a, "Alex").unwrap(),
        "the same name counted as a change"
    );
}

/// The direct addresses a host has stored for `of`, read from the file it really writes, which is what it will use at its next start.
fn stored_ips(directory: &Path, owner: &str, of: &PeerId) -> Vec<std::net::SocketAddr> {
    let peers = Peers::open(directory.join(format!("peers-{owner}.json"))).unwrap();
    peers
        .entries()
        .find(|entry| entry.peer.id == *of)
        .expect("remembered")
        .address
        .ip_addrs()
        .copied()
        .collect()
}

#[tokio::test]
async fn a_device_that_comes_back_at_a_new_address_is_found_by_the_one_that_did_not_move_and_both_keep_the_new_place()
 {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (peer_a, peer_b) = (
        PeerId(key_a.public().to_string()),
        PeerId(key_b.public().to_string()),
    );
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b.clone()).await;
    pair(&mut a, &mut b).await;
    let before = stored_ips(directory.path(), "a", &peer_b);
    assert!(!before.is_empty(), "pairing left A with no address of B");

    // B goes away and comes back with the same key at ANOTHER address: A still holds the old one, so A cannot find B by itself.
    b.close().await;
    drive_one(&mut a, |a| a.peers().iter().all(|p| !p.online)).await;
    let mut b = host(directory.path(), "b", key_b).await;
    // B is the one that moved, and it knows where A is: it calls A, and A, which knows B, accepts it.
    drive(&mut a, &mut b, |a, b| {
        seen(a, &peer_b).online && seen(b, &peer_a).online
    })
    .await;

    // What A kept is the place B is at now, not only the place it was paired at. It is learnt a moment after the connection is up, when the
    // connection has settled on a direct path, so it is waited for (by the one condition that matters: the new place is in the file) and
    // not read at once.
    let new_place_is_kept = |_: &NetworkHost| {
        stored_ips(directory.path(), "a", &peer_b)
            .iter()
            .any(|address| !before.contains(address))
    };
    drive_one(&mut a, new_place_is_kept).await;
    let after = stored_ips(directory.path(), "a", &peer_b);
    assert!(
        after.iter().any(|address| !before.contains(address)),
        "A did not learn where B is now: {before:?} then {after:?}"
    );
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn what_a_device_learns_of_the_place_of_another_keeps_the_old_place_too_and_never_grows_past_its_bound()
 {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (peer_a, peer_b) = (
        PeerId(key_a.public().to_string()),
        PeerId(key_b.public().to_string()),
    );
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b.clone()).await;
    pair(&mut a, &mut b).await;
    let paired_at = stored_ips(directory.path(), "a", &peer_b);

    // B moves, calls A, and A learns where it is now.
    b.close().await;
    drive_one(&mut a, |a| a.peers().iter().all(|p| !p.online)).await;
    let mut b = host(directory.path(), "b", key_b).await;
    drive(&mut a, &mut b, |a, b| {
        seen(a, &peer_b).online && seen(b, &peer_a).online
    })
    .await;
    drive_one(&mut a, |_| {
        stored_ips(directory.path(), "a", &peer_b)
            .iter()
            .any(|address| !paired_at.contains(address))
    })
    .await;
    a.close().await;
    b.close().await;

    // What A holds on disc now is BOTH places: the one B was paired at and the one it moved to, so that a device found at either is found,
    // and nothing it was known by is thrown away for the sake of the newest one while there is room.
    let held = stored_ips(directory.path(), "a", &peer_b);
    assert!(
        paired_at.iter().all(|old| held.contains(old)),
        "the place B was paired at was dropped for the new one: {held:?}"
    );
    assert!(
        held.len() > paired_at.len(),
        "nothing new was kept: {held:?}"
    );
    assert!(
        held.len() <= Peers::MAX_DIRECT_ADDRESSES,
        "more addresses were kept than a device is allowed: {held:?}"
    );
}

#[tokio::test]
async fn the_address_a_person_types_for_a_paired_device_is_kept_as_typed_and_shown_back_and_clearing_removes_it()
 {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_b = PeerId(key_b.public().to_string());
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b).await;
    pair(&mut a, &mut b).await;
    assert_eq!(
        a.typed_address(&peer_b),
        None,
        "an address was there before one was typed"
    );

    // A name with a port is kept as a NAME, and shown back the way it can be typed again.
    a.set_address(&peer_b, Some("  Phone.Example.org:5000 "))
        .unwrap();
    assert_eq!(
        a.typed_address(&peer_b).as_deref(),
        Some("phone.example.org:5000")
    );
    // It is on disc, which is what the next start uses.
    let stored = Peers::open(directory.path().join("peers-a.json")).unwrap();
    assert!(stored.get(&peer_b).unwrap().typed.is_some());
    // Typed over: the new one replaces the old.
    a.set_address(&peer_b, Some("192.168.1.20:6000")).unwrap();
    assert_eq!(
        a.typed_address(&peer_b).as_deref(),
        Some("192.168.1.20:6000")
    );
    // Empty, or blanks, or nothing: cleared.
    for clearing in [None, Some(""), Some("   ")] {
        a.set_address(&peer_b, Some("10.0.0.1:1")).unwrap();
        a.set_address(&peer_b, clearing).unwrap();
        assert_eq!(
            a.typed_address(&peer_b),
            None,
            "{clearing:?} did not clear it"
        );
    }
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn what_is_not_a_place_is_refused_with_a_sentence_that_says_why_and_changes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_b = PeerId(key_b.public().to_string());
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b).await;
    pair(&mut a, &mut b).await;
    a.set_address(&peer_b, Some("192.168.1.20:5000")).unwrap();

    for (typed, sentence) in [
        (
            "https://phone.example.org",
            "That is not an IP address or a name",
        ),
        ("phone example.org", "That is not an IP address or a name"),
        (
            "phone.example.org:0",
            "The port must be a number from 1 to 65535",
        ),
        (
            "phone.example.org:99999",
            "The port must be a number from 1 to 65535",
        ),
        (
            "0.0.0.0:5000",
            "That address cannot be reached from another device",
        ),
        (
            "224.0.0.1:5000",
            "That address cannot be reached from another device",
        ),
    ] {
        assert_eq!(
            a.set_address(&peer_b, Some(typed)),
            Err(sentence),
            "{typed:?}"
        );
        // A refusal changes nothing: what was there is still there.
        assert_eq!(
            a.typed_address(&peer_b).as_deref(),
            Some("192.168.1.20:5000"),
            "{typed:?} changed what was kept"
        );
    }
    // A device that is not paired cannot be given an address: nobody is introduced by being typed.
    assert_eq!(
        a.set_address(&PeerId("nobody".into()), Some("192.168.1.20:5000")),
        Err("Device is not paired")
    );
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn an_address_typed_without_a_port_is_taken_when_the_device_was_seen_at_one_and_asked_for_when_it_was_not()
 {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_b = PeerId(key_b.public().to_string());
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b).await;
    pair(&mut a, &mut b).await;
    // Paired on a real connection, so A has seen B at a port: an address without one is taken, and that port is used.
    a.set_address(&peer_b, Some("phone.example.org")).unwrap();
    assert_eq!(
        a.typed_address(&peer_b).as_deref(),
        Some("phone.example.org")
    );
    a.close().await;
    b.close().await;

    // A device that was never seen at any port has nothing to fall back on, so it is asked for one: what could never be tried is not saved.
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_b = PeerId(key_b.public().to_string());
    let mut a = host(directory.path(), "a", key_a.clone()).await;
    let mut b = host(directory.path(), "b", key_b.clone()).await;
    pair(&mut a, &mut b).await;
    a.close().await;
    b.close().await;
    let mut bare = Peers::open(directory.path().join("peers-a.json")).unwrap();
    bare.update_address(iroh::EndpointAddr::new(key_b.public()))
        .unwrap();
    drop(bare);
    let mut a = host(directory.path(), "a", key_a).await;
    let asked = a.set_address(&peer_b, Some("phone.example.org"));
    assert!(
        asked.is_err_and(|sentence| sentence.contains("write the port")),
        "a device with no port known was not asked for one"
    );
    assert_eq!(
        a.typed_address(&peer_b),
        None,
        "something that could never be tried was saved"
    );
    // With the port written it is taken.
    a.set_address(&peer_b, Some("phone.example.org:5000"))
        .unwrap();
    a.close().await;
}

#[tokio::test]
async fn a_private_device_stays_connected_and_is_shown_as_offline_until_it_opens_to_the_other() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (peer_a, peer_b) = (
        PeerId(key_a.public().to_string()),
        PeerId(key_b.public().to_string()),
    );
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b).await;
    pair(&mut a, &mut b).await;
    b.set_statuses(vec![status("player", "watching", "Watching")])
        .unwrap();
    drive(&mut a, &mut b, |a, _| {
        !seen(a, &peer_b).custom_statuses().is_empty()
    })
    .await;

    // B keeps to itself: A forgets what B said and shows it as not there, on the connection that is still up.
    b.set_private(true);
    drive(&mut a, &mut b, |a, _| !seen(a, &peer_b).online).await;
    let hidden = seen(&a, &peer_b);
    assert_eq!(hidden.presence(), Presence::Offline);
    assert!(hidden.custom_statuses().is_empty() && !hidden.sleeping);
    assert!(
        a.connection(&peer_b).is_some(),
        "the connection was given up"
    );
    // B still hears A: what A says of itself arrives as before.
    a.set_asleep(true);
    drive(&mut a, &mut b, |_, b| seen(b, &peer_a).sleeping).await;
    assert_eq!(seen(&b, &peer_a).presence(), Presence::Sleeping);

    // B's person starts something for A: A is told how B is, statuses and all.
    assert!(b.open_to(&peer_a));
    assert!(!b.open_to(&peer_a), "opened twice");
    drive(&mut a, &mut b, |a, _| {
        let shown = seen(a, &peer_b);
        shown.online && shown.custom_statuses().len() == 1
    })
    .await;
    // And when that is over, A knows nothing of B again.
    assert!(b.close_to(&peer_a));
    drive(&mut a, &mut b, |a, _| !seen(a, &peer_b).online).await;
    assert!(seen(&a, &peer_b).custom_statuses().is_empty());

    // Giving it up tells everyone how B is once more.
    b.set_private(false);
    drive(&mut a, &mut b, |a, _| seen(a, &peer_b).online).await;
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn a_private_device_that_connects_says_nothing_and_is_not_shown_until_it_opens() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (peer_a, peer_b) = (
        PeerId(key_a.public().to_string()),
        PeerId(key_b.public().to_string()),
    );
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b.clone()).await;
    pair(&mut a, &mut b).await;
    b.close().await;
    drive_one(&mut a, |a| !seen(a, &peer_b).online).await;

    // B starts again, private from the start: it connects and hears A, and A holds the connection without being told a thing.
    let mut b = host(directory.path(), "b", key_b).await;
    b.set_private(true);
    drive(&mut a, &mut b, |a, b| {
        seen(b, &peer_a).online && a.connection(&peer_b).is_some()
    })
    .await;
    assert_eq!(seen(&a, &peer_b).presence(), Presence::Offline);
    assert!(b.open_to(&peer_a));
    drive(&mut a, &mut b, |a, _| seen(a, &peer_b).online).await;
    a.close().await;
    b.close().await;
}
