//! A test of the connection to a paired device, against a device that really answers, one that forgot this one, and one that is gone.
//!
//! The three things a person is told apart, because each means doing something else: nothing, pairing again, or looking at the network.
//! What this shows that the unit tests of the classification do not: that the refusal of the other side really arrives as a refusal (and
//! is not taken for a network that went), and that a test never keeps the connection it opened.

use iroh::{Endpoint, SecretKey};
use peersus_core::{
    config::NetworkSettings,
    model::{LinkTest, PeerId},
    pairing::Role,
};
use peersus_net::{
    handshake,
    invitation::Invitation,
    invitations::Invitations,
    pairing_tasks::{Event, PairingTasks},
    peers::Peers,
    reconnect, transport,
};
use std::path::Path;

fn policy() -> NetworkSettings {
    NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..NetworkSettings::default()
    }
}

/// Two endpoints that went through the real pairing, each holding the other as remembered: `(a, b, peers of a, peers of b)`.
async fn paired(
    directory: &Path,
    key_a: &SecretKey,
    key_b: &SecretKey,
) -> (Endpoint, Endpoint, Peers, Peers) {
    let a = transport::bind(key_a.clone(), &policy()).await.unwrap();
    let b = transport::bind(key_b.clone(), &policy()).await.unwrap();
    let gate = Invitations::default();
    let invitation = gate.create(&b, 100).unwrap();
    let scanned = Invitation::decode(&invitation.encode().unwrap(), 101).unwrap();
    let (left, right) = tokio::join!(handshake::join(&a, scanned, "A".into()), async {
        gate.accept(b.accept().await.unwrap().await.unwrap(), "B".into())
            .await
    });
    let left = left.unwrap();
    let (right, _, _) = right.unwrap();
    let (mut peers_a, mut peers_b) = (
        Peers::open(directory.join("a.json")).unwrap(),
        Peers::open(directory.join("b.json")).unwrap(),
    );
    let (mut tasks_a, mut tasks_b) = (PairingTasks::new(a.id()), PairingTasks::new(b.id()));
    let request_a = tasks_a.start(left, Role::Initiator).unwrap();
    let request_b = tasks_b.start(right, Role::Responder).unwrap();
    let (prompt_a, prompt_b) = tokio::join!(
        tasks_a.next(&mut peers_a, 102),
        tasks_b.next(&mut peers_b, 102)
    );
    let (Event::Prompt(prompt_a), Event::Prompt(prompt_b)) = (prompt_a, prompt_b) else {
        panic!("both sides must ask for consent")
    };
    // The same number on both, which each device shows at a place of its own.
    let number = prompt_a.options[1];
    assert!(prompt_b.options.contains(&number));
    tasks_a.choose(&request_a, number).unwrap();
    tasks_b.choose(&request_b, number).unwrap();
    let (result_a, result_b) = tokio::join!(
        tasks_a.next(&mut peers_a, 102),
        tasks_b.next(&mut peers_b, 102)
    );
    let (
        Event::Paired {
            connection: left, ..
        },
        Event::Paired {
            connection: right, ..
        },
    ) = (result_a, result_b)
    else {
        panic!("both sides must have paired")
    };
    left.close(0_u32.into(), b"paired");
    right.close(0_u32.into(), b"paired");
    (a, b, peers_a, peers_b)
}

#[tokio::test]
async fn a_device_that_knows_this_one_is_found_paired_and_the_test_keeps_no_connection() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (a, b, mut peers_a, peers_b) = paired(directory.path(), &key_a, &key_b).await;
    // Fresh addressing stands in for discovery: the routing hint, not a new identity.
    peers_a.update_address(b.addr()).unwrap();
    let remembered = peers_a.get(&PeerId(b.id().to_string())).unwrap().clone();
    let (found, accepted) = tokio::join!(reconnect::probe(&a, &remembered), async {
        reconnect::accept(b.accept().await.unwrap().await.unwrap(), &peers_b).await
    });
    assert_eq!(found, LinkTest::Paired);
    // The side that was tested saw an ordinary session, and the test let go of its own: nothing is left open on either.
    let accepted =
        accepted.expect("the device that was tested admitted the test as it would a session");
    tokio::time::timeout(std::time::Duration::from_secs(5), accepted.closed())
        .await
        .expect("the test kept its connection open");
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn a_device_that_forgot_this_one_is_found_to_need_pairing_and_not_unreachable() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (a, b, mut peers_a, mut peers_b) = paired(directory.path(), &key_a, &key_b).await;
    peers_a.update_address(b.addr()).unwrap();
    // B no longer knows A: the case of a device that was removed or reset, which only pairing again mends.
    peers_b.forget(&PeerId(a.id().to_string())).unwrap();
    let remembered = peers_a.get(&PeerId(b.id().to_string())).unwrap().clone();
    let (found, _) = tokio::join!(reconnect::probe(&a, &remembered), async {
        reconnect::accept(b.accept().await.unwrap().await.unwrap(), &peers_b).await
    });
    assert_eq!(
        found,
        LinkTest::NeedsPairing,
        "a device that does not know this one was taken for one that did not answer"
    );
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn a_device_that_is_gone_is_found_unreachable_and_never_told_to_pair_again() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (a, b, mut peers_a, _peers_b) = paired(directory.path(), &key_a, &key_b).await;
    peers_a.update_address(b.addr()).unwrap();
    let remembered = peers_a.get(&PeerId(b.id().to_string())).unwrap().clone();
    // The other device stops altogether: nothing listens at the address any more.
    b.close().await;
    let found = tokio::time::timeout(
        std::time::Duration::from_secs(40),
        reconnect::probe(&a, &remembered),
    )
    .await
    .expect("the test did not end");
    assert_eq!(
        found,
        LinkTest::Unreachable,
        "a device that is gone was taken for one that wants pairing again"
    );
    a.close().await;
}

#[test]
fn only_a_refusal_by_the_other_side_says_that_it_does_not_know_this_device() {
    use iroh::endpoint::{ApplicationClose, ConnectionError};
    let closed_with = |code: u32| {
        ConnectionError::ApplicationClosed(ApplicationClose {
            error_code: code.into(),
            reason: Vec::new().into(),
        })
    };
    // The code the refusing side gives, and no other: this is what sends a person to pair again.
    assert_eq!(
        reconnect::classify(Some(closed_with(reconnect::REFUSED))),
        LinkTest::NeedsPairing
    );
    // A device that ended the connection some other way, or the network, or nothing at all: not a reason to pair again.
    assert_eq!(
        reconnect::classify(Some(closed_with(0))),
        LinkTest::Unreachable
    );
    assert_eq!(
        reconnect::classify(Some(closed_with(2))),
        LinkTest::Unreachable
    );
    assert_eq!(
        reconnect::classify(Some(ConnectionError::TimedOut)),
        LinkTest::Unreachable
    );
    assert_eq!(
        reconnect::classify(Some(ConnectionError::Reset)),
        LinkTest::Unreachable
    );
    assert_eq!(reconnect::classify(None), LinkTest::Unreachable);
}
