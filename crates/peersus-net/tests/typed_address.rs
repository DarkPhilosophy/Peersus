//! A paired device that cannot be found at the address it was paired at, found by an address a person TYPED: an IP, and a name.
//!
//! This is the case the "Change IP" button exists for, which reconciliation cannot mend by itself: both devices changed address, so neither
//! knows where the other is. What is shown here, on connections that really happen:
//!
//! * the address that was paired at is wrong, and the device is found at the one that was typed;
//! * a NAME works the same way, and is looked up at each connection (`localhost` stands for a name that resolves);
//! * what is typed only ADDS: a wrong one does not take away the way the device was already reachable;
//! * a typed address finds the device, but WHO answers is decided by the key: a different device at that address is refused.

use iroh::{EndpointAddr, SecretKey};
use peersus_core::{
    address::TypedAddress,
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

/// A and B paired for real, each remembering the other. Returns `(a, b, peers of a, peers of b)`.
async fn paired(
    dir: &Path,
    key_a: &SecretKey,
    key_b: &SecretKey,
) -> (iroh::Endpoint, iroh::Endpoint, Peers, Peers) {
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
        Peers::open(dir.join("a.json")).unwrap(),
        Peers::open(dir.join("b.json")).unwrap(),
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
    tasks_a.choose(&request_a, prompt_a.options[1]).unwrap();
    tasks_b.choose(&request_b, prompt_b.options[1]).unwrap();
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

/// Make what A holds of B WRONG: an address nothing listens at, as if B had moved and A had not learnt where.
fn forget_where_b_is(peers_a: &mut Peers, b: &PeerId, key_b: &SecretKey) {
    let wrong = EndpointAddr::new(key_b.public()).with_ip_addr("127.0.0.1:9".parse().unwrap());
    peers_a.update_address(wrong).unwrap();
    assert_eq!(
        peers_a.get(b).unwrap().address.ip_addrs().count(),
        1,
        "the premise: A holds one address, and it is wrong"
    );
}

/// Test the connection from A to B the way the button does, with B answering as the real host does: it listens for as long as the test
/// lasts, so a connection left over from the pairing (closed already) is let go and the one of the test is the one that is served.
async fn test_from_a(
    a: &iroh::Endpoint,
    b: &iroh::Endpoint,
    peers_a: &Peers,
    peers_b: &Peers,
    of: &PeerId,
) -> LinkTest {
    let remembered = peers_a.get(of).unwrap().clone();
    // The connection B admits is HELD until the test is over, as the real host holds it in its sessions: letting it go here would close it
    // before A has read the answer, and the test would be of the test and not of the code.
    let held = std::sync::Mutex::new(None);
    let serve = async {
        loop {
            let Some(incoming) = b.accept().await else {
                return;
            };
            if let Ok(connection) = incoming.await
                && connection.alpn() == reconnect::ALPN
                && let Ok(admitted) = reconnect::accept(connection, peers_b).await
            {
                *held.lock().unwrap() = Some(admitted);
                return;
            }
        }
    };
    let probe = reconnect::probe(a, &remembered);
    tokio::pin!(probe);
    tokio::pin!(serve);
    let mut serving = true;
    loop {
        tokio::select! {
            found = &mut probe => return found,
            () = &mut serve, if serving => serving = false,
        }
    }
}

#[tokio::test]
async fn a_device_that_is_not_where_it_was_paired_is_found_at_the_ip_that_was_typed() {
    let dir = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (a, b, mut peers_a, peers_b) = paired(dir.path(), &key_a, &key_b).await;
    let of = PeerId(key_b.public().to_string());
    forget_where_b_is(&mut peers_a, &of, &key_b);
    // A starts again, as after a restart: what iroh had learnt of where B is while they were paired is gone, so that the only thing that can
    // tell A where B is, is what A has stored. Without this the two are in one process and iroh finds B at once whatever is stored.
    a.close().await;
    let a = transport::bind(key_a.clone(), &policy()).await.unwrap();

    // Before anything is typed, A cannot find B: this is what the button is for.
    let before = tokio::time::timeout(
        std::time::Duration::from_secs(40),
        test_from_a(&a, &b, &peers_a, &peers_b, &of),
    )
    .await
    .unwrap();
    assert_eq!(
        before,
        LinkTest::Unreachable,
        "the premise: at the wrong address B is not found"
    );

    // The person types where B is now, with the port B is listening on.
    let port = b
        .bound_sockets()
        .iter()
        .find(|socket| socket.ip().is_loopback())
        .expect("B listens on loopback")
        .port();
    let typed = TypedAddress::parse(&format!("127.0.0.1:{port}")).unwrap();
    assert!(peers_a.set_typed(&of, Some(typed)).unwrap());

    assert_eq!(
        test_from_a(&a, &b, &peers_a, &peers_b, &of).await,
        LinkTest::Paired,
        "B was not found at the address that was typed"
    );
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn a_name_that_resolves_finds_the_device_and_is_looked_up_again_each_time() {
    let dir = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (a, b, mut peers_a, peers_b) = paired(dir.path(), &key_a, &key_b).await;
    let of = PeerId(key_b.public().to_string());
    forget_where_b_is(&mut peers_a, &of, &key_b);
    let port = b
        .bound_sockets()
        .iter()
        .find(|socket| socket.ip().is_loopback())
        .expect("B listens on loopback")
        .port();

    // A NAME, kept as a name: `localhost` resolves to the loopback address here, as a DNS name of a phone would to its address.
    let typed = TypedAddress::parse(&format!("localhost:{port}")).unwrap();
    peers_a.set_typed(&of, Some(typed.clone())).unwrap();
    assert_eq!(
        test_from_a(&a, &b, &peers_a, &peers_b, &of).await,
        LinkTest::Paired,
        "the name was not resolved to the device"
    );

    // It is kept AS A NAME and not as the address it gave today, which is what makes a name worth typing: it is looked up again next time.
    let stored = Peers::open(dir.path().join("a.json")).unwrap();
    assert_eq!(stored.get(&of).unwrap().typed.as_ref(), Some(&typed));
    assert!(
        stored
            .get(&of)
            .unwrap()
            .address
            .ip_addrs()
            .all(|address| address.port() == 9),
        "the name's address was written over what was held"
    );
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn a_typed_address_that_is_wrong_takes_nothing_away_from_the_way_the_device_was_found() {
    let dir = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (a, b, mut peers_a, peers_b) = paired(dir.path(), &key_a, &key_b).await;
    let of = PeerId(key_b.public().to_string());
    // A holds the RIGHT address (as paired). Then the person types a wrong one, a typo.
    peers_a.update_address(b.addr()).unwrap();
    peers_a
        .set_typed(&of, Some(TypedAddress::parse("127.0.0.1:9").unwrap()))
        .unwrap();
    assert_eq!(
        test_from_a(&a, &b, &peers_a, &peers_b, &of).await,
        LinkTest::Paired,
        "a wrong typed address made a device that was reachable unreachable"
    );
    // And a name that does not resolve at all costs nothing either.
    peers_a
        .set_typed(
            &of,
            Some(TypedAddress::parse("no-such-name.invalid:9").unwrap()),
        )
        .unwrap();
    assert_eq!(
        test_from_a(&a, &b, &peers_a, &peers_b, &of).await,
        LinkTest::Paired,
        "a name that does not resolve broke the connection"
    );
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn the_key_decides_who_answers_so_another_device_at_the_typed_address_is_not_taken_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let (a, b, mut peers_a, peers_b) = paired(dir.path(), &key_a, &key_b).await;
    let of = PeerId(key_b.public().to_string());
    forget_where_b_is(&mut peers_a, &of, &key_b);
    // A DIFFERENT device listens where the person typed: it is not B, and it does not have B's key.
    let stranger = transport::bind(SecretKey::generate(), &policy())
        .await
        .unwrap();
    let port = stranger
        .bound_sockets()
        .iter()
        .find(|socket| socket.ip().is_loopback())
        .unwrap()
        .port();
    peers_a
        .set_typed(
            &of,
            Some(TypedAddress::parse(&format!("127.0.0.1:{port}")).unwrap()),
        )
        .unwrap();
    let remembered = peers_a.get(&of).unwrap().clone();
    let found = tokio::time::timeout(
        std::time::Duration::from_secs(40),
        reconnect::probe(&a, &remembered),
    )
    .await
    .unwrap();
    assert_ne!(
        found,
        LinkTest::Paired,
        "a device that is not B was taken for B because of where it was"
    );
    stranger.close().await;
    a.close().await;
    b.close().await;
    let _ = peers_b;
}

#[test]
fn the_typed_address_is_kept_beside_the_learnt_ones_and_a_file_from_before_still_reads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    // A file written before this existed has no such field.
    let key = SecretKey::generate();
    let remote = key.public();
    let _ = remote;
    let json = serde_json::json!({ "version": 2, "peers": {} });
    std::fs::write(&path, json.to_string()).unwrap();
    assert!(Peers::open(&path).is_ok(), "an older file did not read");
}
