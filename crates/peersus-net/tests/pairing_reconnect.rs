use iroh::SecretKey;
use peersus_core::{config::NetworkSettings, model::PeerId, pairing::Role};
use peersus_net::{
    handshake,
    invitation::Invitation,
    invitations::Invitations,
    pairing_tasks::{Event, PairingTasks},
    peers::Peers,
    transport,
};

#[tokio::test]
async fn invitation_consent_persistence_and_reconnection_preserve_identity() {
    let directory = tempfile::tempdir().unwrap();
    let policy = NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
    };
    let key_a = SecretKey::generate();
    let key_b = SecretKey::generate();
    let a = transport::bind(key_a.clone(), &policy).await.unwrap();
    let b = transport::bind(key_b.clone(), &policy).await.unwrap();
    let gate = Invitations::default();
    let invitation = gate.create(&b, 100).unwrap();
    let scanned = Invitation::decode(&invitation.encode().unwrap(), 101).unwrap();
    let (left, right) = tokio::join!(handshake::join(&a, scanned, "A".into()), async {
        gate.accept(b.accept().await.unwrap().await.unwrap(), "B".into())
            .await
    });
    let left = left.unwrap();
    let (right, purpose, _) = right.unwrap();
    assert_eq!(purpose, peersus_net::invitations::Purpose::Pairing, "a code shown to pair must admit as pairing");
    let path_a = directory.path().join("a.json");
    let path_b = directory.path().join("b.json");
    let mut peers_a = Peers::open(&path_a).unwrap();
    let mut peers_b = Peers::open(&path_b).unwrap();
    let mut tasks_a = PairingTasks::new(a.id());
    let mut tasks_b = PairingTasks::new(b.id());
    let request_a = tasks_a.start(left, Role::Initiator).unwrap();
    let request_b = tasks_b.start(right, Role::Responder).unwrap();
    let (prompt_a, prompt_b) = tokio::join!(
        tasks_a.next(&mut peers_a, 102),
        tasks_b.next(&mut peers_b, 102)
    );
    let (Event::Prompt(prompt_a), Event::Prompt(prompt_b)) = (prompt_a, prompt_b) else {
        panic!("both hosts must request independent local consent");
    };
    assert_eq!(prompt_a.options, prompt_b.options);
    assert_eq!(peers_a.entries().count(), 0);
    assert_eq!(peers_b.entries().count(), 0);
    assert!(tasks_a.choose(&request_a, 255).is_err());
    tasks_a.choose(&request_a, prompt_a.options[1]).unwrap();
    assert!(tasks_a.choose(&request_a, prompt_a.options[1]).is_err());
    assert!(tasks_a.prompts()[0].selected);
    assert!(!tasks_b.prompts()[0].selected);
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
        panic!("both hosts must persist completed consent");
    };
    assert!(tasks_a.prompts().is_empty());
    assert!(tasks_b.prompts().is_empty());
    left.close(0_u32.into(), b"restart");
    right.close(0_u32.into(), b"restart");
    a.close().await;
    b.close().await;

    let a = transport::bind(key_a, &policy).await.unwrap();
    let b = transport::bind(key_b, &policy).await.unwrap();
    let mut remembered_a = Peers::open(path_a).unwrap();
    let mut remembered_b = Peers::open(path_b).unwrap();
    assert!(
        !remembered_a
            .get(&PeerId(b.id().to_string()))
            .unwrap()
            .peer
            .online
    );
    // Fresh addressing stands in for discovery, not a new invitation or identity.
    remembered_a.update_address(b.addr()).unwrap();
    let remote = PeerId(b.id().to_string());
    let (outgoing, incoming) = tokio::join!(
        peersus_net::reconnect::connect(&a, &remembered_a, &remote),
        async {
            peersus_net::reconnect::accept(b.accept().await.unwrap().await.unwrap(), &remembered_b)
                .await
        }
    );
    let outgoing = outgoing.unwrap();
    let incoming = incoming.unwrap();
    assert!(
        remembered_a
            .get(&PeerId(outgoing.remote_id().to_string()))
            .is_some()
    );
    assert!(
        remembered_b
            .get(&PeerId(incoming.remote_id().to_string()))
            .is_some()
    );
    outgoing.close(0_u32.into(), b"done");
    incoming.close(0_u32.into(), b"done");
    remembered_b.forget(&PeerId(a.id().to_string())).unwrap();
    let (outgoing, incoming) = tokio::join!(
        peersus_net::reconnect::connect(&a, &remembered_a, &remote),
        async {
            peersus_net::reconnect::accept(b.accept().await.unwrap().await.unwrap(), &remembered_b)
                .await
        }
    );
    assert!(
        outgoing.is_err(),
        "forgotten device must not report admission"
    );
    assert!(incoming.is_err(), "forgotten identity must be refused");
    a.close().await;
    b.close().await;
}
