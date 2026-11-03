use iroh::SecretKey;
use peersus_core::{
    config::NetworkSettings,
    model::{PeerId, TransferState},
};
use peersus_net::{
    history::History,
    network_host::{Event, NetworkHost},
    peers::Peers,
    transfer::Source,
    transport,
};
use std::time::Duration;

async fn drive(
    a: &mut NetworkHost,
    b: &mut NetworkHost,
    done: impl Fn(&NetworkHost, &NetworkHost) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !done(a, b) {
            let event =
                tokio::select! { event = a.next(101) => event, event = b.next(101) => event };
            match event {
                Event::Changed => {}
                Event::Error(reason) => panic!("Unexpected host failure: {reason}"),
                Event::Plugin { .. } => panic!("No plugin stream expected"),
                Event::Picture { .. } => panic!("No picture expected"),
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn host_pairs_remembers_transfers_and_forgets_without_injecting_remote_addresses() {
    let directory = tempfile::tempdir().unwrap();
    let policy = NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..NetworkSettings::default()
    };
    let key_a = SecretKey::generate();
    let endpoint_a = transport::bind(key_a.clone(), &policy).await.unwrap();
    let endpoint_b = transport::bind(SecretKey::generate(), &policy)
        .await
        .unwrap();
    let peer_a = PeerId(endpoint_a.id().to_string());
    let peer_b = PeerId(endpoint_b.id().to_string());
    let path_a = directory.path().join("peers-a.json");
    let path_b = directory.path().join("peers-b.json");
    let mut a = NetworkHost::new(
        endpoint_a,
        "A".into(),
        Peers::open(&path_a).unwrap(),
        History::open(directory.path().join("history-a.json")).unwrap(),
    );
    let mut b = NetworkHost::new(
        endpoint_b,
        "B".into(),
        Peers::open(&path_b).unwrap(),
        History::open(directory.path().join("history-b.json")).unwrap(),
    );
    a.join(b.invite(100).unwrap()).unwrap();
    drive(&mut a, &mut b, |a, b| {
        !a.pairing().is_empty() && !b.pairing().is_empty()
    })
    .await;
    assert!(a.peers().is_empty());
    assert!(b.peers().is_empty());
    let pa = a.pairing().remove(0);
    let pb = b.pairing().remove(0);
    a.choose(&pa.id, pa.options[0]).unwrap();
    b.choose(&pb.id, pa.options[0]).unwrap();
    drive(&mut a, &mut b, |a, b| {
        a.peers().iter().any(|p| p.online) && b.peers().iter().any(|p| p.online)
    })
    .await;
    assert!(Peers::open(&path_a).unwrap().get(&peer_b).is_some());
    assert!(Peers::open(&path_b).unwrap().get(&peer_a).is_some());
    let source = directory.path().join("example.bin");
    let bytes = vec![123; 300_000];
    std::fs::write(&source, &bytes).unwrap();
    let destination = directory.path().join("received");
    std::fs::create_dir(&destination).unwrap();
    a.send(&peer_b, Source::open(&source).await.unwrap(), 101)
        .unwrap();
    drive(&mut a, &mut b, |_, b| !b.transfers().is_empty()).await;
    assert!(!destination.join("example.bin").exists());
    b.accept(&b.transfers()[0].id, destination.clone()).unwrap();
    drive(&mut a, &mut b, |a, b| {
        a.transfers()[0].state == TransferState::Complete
            && b.transfers()[0].state == TransferState::Complete
    })
    .await;
    assert_eq!(
        std::fs::read(destination.join("example.bin")).unwrap(),
        bytes
    );
    // Reopen only from durable identity/history/pairing state. The restarted
    // endpoint gets a new UDP address; neither host receives a new invitation.
    a.close().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while b.peers().iter().any(|peer| peer.online) {
            b.next(102).await;
        }
    })
    .await
    .unwrap();
    a = NetworkHost::new(
        transport::bind(key_a, &policy).await.unwrap(),
        "A".into(),
        Peers::open(&path_a).unwrap(),
        History::open(directory.path().join("history-a.json")).unwrap(),
    );
    assert!(a.peers().iter().all(|peer| !peer.online));
    assert_eq!(a.transfers()[0].state, TransferState::Complete);
    drive(&mut a, &mut b, |a, b| {
        a.peers().iter().any(|peer| peer.online) && b.peers().iter().any(|peer| peer.online)
    })
    .await;
    assert!(a.pairing().is_empty() && b.pairing().is_empty());
    a.forget(&peer_b).unwrap();
    drive(&mut a, &mut b, |a, b| {
        a.peers().is_empty() && b.peers().iter().all(|p| !p.online)
    })
    .await;
    assert!(Peers::open(&path_a).unwrap().get(&peer_b).is_none());
    a.close().await;
    b.close().await;
}

struct Pair {
    directory: tempfile::TempDir,
    sender: NetworkHost,
    receiver: NetworkHost,
    receiver_id: PeerId,
}

/// Two hosts that have never met: nothing paired, nothing remembered.
async fn strangers() -> Pair {
    let directory = tempfile::tempdir().unwrap();
    let policy = NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..NetworkSettings::default()
    };
    let endpoint_a = transport::bind(SecretKey::generate(), &policy)
        .await
        .unwrap();
    let endpoint_b = transport::bind(SecretKey::generate(), &policy)
        .await
        .unwrap();
    let receiver_id = PeerId(endpoint_b.id().to_string());
    let sender = NetworkHost::new(
        endpoint_a,
        "Sender".into(),
        Peers::open(directory.path().join("peers-a.json")).unwrap(),
        History::open(directory.path().join("history-a.json")).unwrap(),
    );
    let receiver = NetworkHost::new(
        endpoint_b,
        "Receiver".into(),
        Peers::open(directory.path().join("peers-b.json")).unwrap(),
        History::open(directory.path().join("history-b.json")).unwrap(),
    );
    Pair {
        directory,
        sender,
        receiver,
        receiver_id,
    }
}

async fn file_of(directory: &std::path::Path, name: &str, bytes: &[u8]) -> Source {
    let path = directory.join(name);
    std::fs::write(&path, bytes).unwrap();
    Source::open(&path).await.unwrap()
}

#[tokio::test]
async fn a_scanned_files_code_delivers_the_file_with_no_pairing_and_leaves_no_peer_behind() {
    let Pair {
        directory,
        mut sender,
        mut receiver,
        receiver_id,
        ..
    } = strangers().await;
    let bytes = vec![7u8; 200_000];
    let source = file_of(directory.path(), "photo.bin", &bytes).await;
    // The sender shows a code for files; the receiver scans it.
    let code = sender.invite_for_files(vec![source], 100).unwrap();
    receiver.join(code).unwrap();
    // The file is OFFERED to the receiver, who has to accept it, and no pairing prompt ever appears on either side.
    drive(&mut sender, &mut receiver, |_, receiver| {
        !receiver.transfers().is_empty()
    })
    .await;
    assert!(
        sender.pairing().is_empty() && receiver.pairing().is_empty(),
        "scanning a files code asked for a pairing"
    );
    let destination = directory.path().join("received");
    std::fs::create_dir(&destination).unwrap();
    assert!(
        !destination.join("photo.bin").exists(),
        "the file arrived before it was accepted"
    );
    let offered = receiver.transfers().remove(0);
    assert!(offered.incoming);
    receiver.accept(&offered.id, destination.clone()).unwrap();
    drive(&mut sender, &mut receiver, |sender, receiver| {
        sender
            .transfers()
            .iter()
            .all(|t| t.state == TransferState::Complete)
            && receiver
                .transfers()
                .iter()
                .all(|t| t.state == TransferState::Complete)
    })
    .await;
    assert_eq!(std::fs::read(destination.join("photo.bin")).unwrap(), bytes);
    // The trust was for this file only: nobody is paired or remembered, on either side, in memory or on disk.
    assert!(
        sender.peers().is_empty() && receiver.peers().is_empty(),
        "a device became a peer from sending a file"
    );
    assert!(
        Peers::open(directory.path().join("peers-a.json"))
            .unwrap()
            .entries()
            .next()
            .is_none()
    );
    assert!(
        Peers::open(directory.path().join("peers-b.json"))
            .unwrap()
            .entries()
            .next()
            .is_none()
    );
    // And so a NEW file cannot be sent to that device directly: it has to scan again, or be paired in Peers.
    let another = file_of(directory.path(), "another.bin", b"more").await;
    assert!(
        sender.send(&receiver_id, another, 101).is_err(),
        "a file went to a device that is not paired"
    );
    sender.close().await;
    receiver.close().await;
}

#[tokio::test]
async fn a_replaced_files_code_delivers_only_the_files_of_the_code_that_was_scanned() {
    let Pair {
        directory,
        mut sender,
        mut receiver,
        ..
    } = strangers().await;
    let first = file_of(directory.path(), "first.bin", b"for the first code").await;
    let second = file_of(directory.path(), "second.bin", b"for the second code").await;
    let _old = sender.invite_for_files(vec![first], 100).unwrap();
    let current = sender.invite_for_files(vec![second], 100).unwrap();
    receiver.join(current).unwrap();
    drive(&mut sender, &mut receiver, |_, receiver| {
        !receiver.transfers().is_empty()
    })
    .await;
    let offered = receiver.transfers();
    assert_eq!(
        offered.len(),
        1,
        "files of a replaced code were offered too"
    );
    let destination = directory.path().join("received");
    std::fs::create_dir(&destination).unwrap();
    receiver
        .accept(&offered[0].id, destination.clone())
        .unwrap();
    drive(&mut sender, &mut receiver, |_, receiver| {
        receiver
            .transfers()
            .iter()
            .all(|t| t.state == TransferState::Complete)
    })
    .await;
    assert!(destination.join("second.bin").exists());
    assert!(
        !destination.join("first.bin").exists(),
        "the replaced code's file was delivered"
    );
    sender.close().await;
    receiver.close().await;
}

#[tokio::test]
async fn a_pairing_code_scanned_still_pairs_and_a_files_code_never_does() {
    // A code shown in Peers is a pairing: both devices are asked, and nothing is offered.
    let Pair {
        directory,
        mut sender,
        mut receiver,
        ..
    } = strangers().await;
    let source = file_of(directory.path(), "held.bin", b"must stay").await;
    let _files = sender.invite_for_files(vec![source], 100).unwrap();
    receiver.join(sender.invite(100).unwrap()).unwrap();
    drive(&mut sender, &mut receiver, |sender, receiver| {
        !sender.pairing().is_empty() && !receiver.pairing().is_empty()
    })
    .await;
    // Showing a pairing code did not revoke the files code, and the files were NOT offered on the pairing connection.
    assert!(
        receiver.transfers().is_empty(),
        "files were offered to a device that scanned a pairing code"
    );
    sender.close().await;
    receiver.close().await;
}

// (`strangers()` above is the pair these tests start from: a sender and a receiver never met, whose files code is how they meet.)

/// Send one file by a code and have it accepted, so that the sender has a finished transfer.
async fn one_finished_send(pair: &mut Pair) {
    let source = file_of(pair.directory.path(), "photo.bin", &[7u8; 50_000]).await;
    let code = pair.sender.invite_for_files(vec![source], 100).unwrap();
    pair.receiver.join(code).unwrap();
    drive(&mut pair.sender, &mut pair.receiver, |_, receiver| {
        !receiver.transfers().is_empty()
    })
    .await;
    let destination = pair.directory.path().join("received");
    std::fs::create_dir(&destination).unwrap();
    let offered = pair.receiver.transfers().remove(0);
    pair.receiver.accept(&offered.id, destination).unwrap();
    drive(&mut pair.sender, &mut pair.receiver, |sender, receiver| {
        sender
            .transfers()
            .iter()
            .all(|t| t.state == TransferState::Complete)
            && receiver
                .transfers()
                .iter()
                .all(|t| t.state == TransferState::Complete)
    })
    .await;
}

#[tokio::test]
async fn clearing_the_list_leaves_nothing_of_what_passed_with_any_device() {
    let mut pair = strangers().await;
    one_finished_send(&mut pair).await;
    assert_eq!(pair.sender.transfers().len(), 1);
    // ONE list is what Send, Receive and a device's own page all read: when Clear forgets what is over there is nothing left to show
    // anywhere, and no second memory that Clear does not reach.
    assert_eq!(pair.sender.clear_history().unwrap(), 1);
    assert!(
        pair.sender.transfers().is_empty(),
        "Clear left a finished transfer in the list"
    );
    pair.sender.close().await;
    pair.receiver.close().await;
}

#[tokio::test]
async fn the_history_of_a_forgotten_device_does_not_come_back_at_the_next_change_or_after_a_restart()
 {
    let mut pair = strangers().await;
    one_finished_send(&mut pair).await;
    assert!(
        pair.sender
            .transfers()
            .iter()
            .any(|t| t.peer == pair.receiver_id)
    );
    // The device is forgotten: what passed with it goes. (It was met by a code, so it was never saved.)
    pair.sender.forget(&pair.receiver_id).unwrap();
    assert!(
        pair.sender
            .transfers()
            .iter()
            .all(|t| t.peer != pair.receiver_id),
        "forgetting a device left a record of it"
    );
    // Any later change of the list is a synchronization: Clear is one.
    pair.sender.clear_history().unwrap();
    assert!(
        pair.sender
            .transfers()
            .iter()
            .all(|t| t.peer != pair.receiver_id),
        "a forgotten device's record came back at the next change"
    );
    // And a restart: the list is opened again from its file.
    let reopened = History::open(pair.directory.path().join("history-a.json")).unwrap();
    assert!(
        reopened
            .entries()
            .iter()
            .all(|t| t.peer != pair.receiver_id),
        "it came back on disk"
    );
    pair.sender.close().await;
    pair.receiver.close().await;
}

#[tokio::test]
async fn forgetting_a_device_while_its_file_is_still_waiting_brings_nothing_back_when_that_transfer_closes()
 {
    let mut pair = strangers().await;
    // A file is offered and nobody answers: it is waiting, not over, when the device is forgotten.
    let source = file_of(pair.directory.path(), "waiting.bin", &[3u8; 30_000]).await;
    let code = pair.sender.invite_for_files(vec![source], 100).unwrap();
    pair.receiver.join(code).unwrap();
    drive(&mut pair.sender, &mut pair.receiver, |sender, receiver| {
        !receiver.transfers().is_empty()
            && sender
                .transfers()
                .iter()
                .any(|t| t.state == TransferState::Offered)
    })
    .await;
    pair.sender.forget(&pair.receiver_id).unwrap();
    // Its task was stopped by the forgetting and its connection closed: nothing of it can be written after this point.
    assert!(
        pair.sender
            .transfers()
            .iter()
            .all(|t| t.peer != pair.receiver_id),
        "a record of the forgotten device was left in the list"
    );
    // Any later change of the list must not bring it back either, in the list or on disk.
    pair.sender.clear_history().unwrap();
    assert!(
        pair.sender
            .transfers()
            .iter()
            .all(|t| t.peer != pair.receiver_id),
        "the next change brought the device back"
    );
    let reopened = History::open(pair.directory.path().join("history-a.json")).unwrap();
    assert!(
        reopened
            .entries()
            .iter()
            .all(|t| t.peer != pair.receiver_id),
        "it came back on disk"
    );
    pair.sender.close().await;
    pair.receiver.close().await;
}

#[tokio::test]
async fn a_busy_receiver_backpressures_a_second_sender_without_losing_its_files() {
    let Pair {
        directory,
        mut sender,
        mut receiver,
        receiver_id,
    } = strangers().await;
    let policy = NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..NetworkSettings::default()
    };
    let mut other = NetworkHost::new(
        transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap(),
        "Second sender".into(),
        Peers::open(directory.path().join("peers-c.json")).unwrap(),
        History::open(directory.path().join("history-c.json")).unwrap(),
    );
    for outgoing in [&mut sender, &mut other] {
        outgoing.join(receiver.invite(100).unwrap()).unwrap();
        drive(outgoing, &mut receiver, |a, b| {
            !a.pairing().is_empty() && !b.pairing().is_empty()
        })
        .await;
        let a = outgoing.pairing().remove(0);
        let b = receiver.pairing().remove(0);
        outgoing.choose(&a.id, a.options[0]).unwrap();
        receiver.choose(&b.id, a.options[0]).unwrap();
        drive(outgoing, &mut receiver, |a, b| {
            a.peers().iter().any(|peer| peer.online) && b.peers().iter().any(|peer| peer.online)
        })
        .await;
    }
    for index in 0..16 {
        let source = file_of(
            directory.path(),
            &format!("first-{index:02}.txt"),
            &[index; 37],
        )
        .await;
        sender.send(&receiver_id, source, 101).unwrap();
    }
    drive(&mut sender, &mut receiver, |_, receiver| {
        receiver.transfers().len() == 16
    })
    .await;
    for index in 0..16 {
        let source = file_of(
            directory.path(),
            &format!("second-{index:02}.txt"),
            &[index + 16; 41],
        )
        .await;
        other.send(&receiver_id, source, 101).unwrap();
    }
    // The receiver deliberately does not consent yet. Filling its active window must
    // wait at the transport boundary, not reset another sender's next file stream.
    let pause = tokio::time::sleep(Duration::from_millis(150));
    tokio::pin!(pause);
    loop {
        let event = tokio::select! {
            () = &mut pause => break,
            event = sender.next(101) => event,
            event = other.next(101) => event,
            event = receiver.next(101) => event,
        };
        assert!(
            matches!(event, Event::Changed),
            "a full receiver rejected an authenticated file offer"
        );
    }
    assert_eq!(
        receiver.transfers().len(),
        16,
        "the active receive bound was exceeded"
    );
    assert!(
        sender
            .transfers()
            .iter()
            .chain(other.transfers().iter())
            .all(|transfer| transfer.state.is_active())
    );
    let destination = directory.path().join("received-backpressure");
    std::fs::create_dir(&destination).unwrap();
    let mut accepted = std::collections::BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let incoming = receiver.transfers();
            assert!(
                incoming
                    .iter()
                    .filter(|transfer| transfer.state.is_active())
                    .count()
                    <= 16
            );
            for transfer in &incoming {
                if transfer.state == TransferState::Offered && accepted.insert(transfer.id.clone())
                {
                    receiver.accept(&transfer.id, destination.clone()).unwrap();
                }
            }
            if incoming.len() == 32
                && incoming
                    .iter()
                    .all(|transfer| transfer.state == TransferState::Complete)
                && sender
                    .transfers()
                    .iter()
                    .chain(other.transfers().iter())
                    .all(|transfer| transfer.state == TransferState::Complete)
            {
                break;
            }
            let event = tokio::select! {
                event = sender.next(101) => event,
                event = other.next(101) => event,
                event = receiver.next(101) => event,
            };
            assert!(
                matches!(event, Event::Changed),
                "a queued incoming transfer failed"
            );
        }
    })
    .await
    .expect("a waiting sender did not resume after receiver consent");
    assert_eq!(accepted.len(), 32);
    for index in 0..16 {
        assert_eq!(
            std::fs::read(destination.join(format!("first-{index:02}.txt"))).unwrap(),
            [index; 37]
        );
        assert_eq!(
            std::fs::read(destination.join(format!("second-{index:02}.txt"))).unwrap(),
            [index + 16; 41]
        );
    }
    sender.close().await;
    other.close().await;
    receiver.close().await;
}

#[tokio::test]
async fn full_outgoing_windows_do_not_block_simultaneous_incoming_files() {
    let Pair {
        directory,
        mut sender,
        mut receiver,
        receiver_id,
    } = strangers().await;
    sender.join(receiver.invite(100).unwrap()).unwrap();
    drive(&mut sender, &mut receiver, |a, b| {
        !a.pairing().is_empty() && !b.pairing().is_empty()
    })
    .await;
    let a = sender.pairing().remove(0);
    let b = receiver.pairing().remove(0);
    sender.choose(&a.id, a.options[0]).unwrap();
    receiver.choose(&b.id, a.options[0]).unwrap();
    drive(&mut sender, &mut receiver, |a, b| {
        a.peers().iter().any(|peer| peer.online) && b.peers().iter().any(|peer| peer.online)
    })
    .await;
    let sender_id = receiver.peers().remove(0).id;
    for index in 0..16 {
        let outgoing = file_of(
            directory.path(),
            &format!("forward-{index:02}.txt"),
            &[index; 23],
        )
        .await;
        let incoming = file_of(
            directory.path(),
            &format!("reverse-{index:02}.txt"),
            &[index + 16; 29],
        )
        .await;
        sender.send(&receiver_id, outgoing, 101).unwrap();
        receiver.send(&sender_id, incoming, 101).unwrap();
    }
    // Both devices have filled their sending window before either grants consent.
    // Receiving must have its own bounded capacity, or neither side can progress.
    drive(&mut sender, &mut receiver, |a, b| {
        a.transfers().len() == 32 && b.transfers().len() == 32
    })
    .await;
    let destination = directory.path().join("received-bidirectional");
    std::fs::create_dir(&destination).unwrap();
    for host in [&mut sender, &mut receiver] {
        let incoming: Vec<_> = host
            .transfers()
            .into_iter()
            .filter(|transfer| transfer.incoming)
            .collect();
        assert_eq!(incoming.len(), 16);
        for transfer in incoming {
            host.accept(&transfer.id, destination.clone()).unwrap();
        }
    }
    drive(&mut sender, &mut receiver, |a, b| {
        a.transfers()
            .iter()
            .chain(b.transfers().iter())
            .all(|transfer| transfer.state == TransferState::Complete)
    })
    .await;
    for index in 0..16 {
        assert_eq!(
            std::fs::read(destination.join(format!("forward-{index:02}.txt"))).unwrap(),
            [index; 23]
        );
        assert_eq!(
            std::fs::read(destination.join(format!("reverse-{index:02}.txt"))).unwrap(),
            [index + 16; 29]
        );
    }
    sender.close().await;
    receiver.close().await;
}

#[tokio::test]
async fn one_files_code_delivers_a_selection_larger_than_the_active_window_without_pairing() {
    let mut pair = strangers().await;
    let mut sources = Vec::new();
    for index in 0..40 {
        sources.push(
            file_of(
                pair.directory.path(),
                &format!("selection-{index:02}.txt"),
                &[index; 43],
            )
            .await,
        );
    }
    let code = pair.sender.invite_for_files(sources, 100).unwrap();
    pair.receiver.join(code).unwrap();
    let destination = pair.directory.path().join("received-one-code");
    std::fs::create_dir(&destination).unwrap();
    let mut accepted = std::collections::BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            for transfer in pair.receiver.transfers() {
                if transfer.state == TransferState::Offered && accepted.insert(transfer.id.clone())
                {
                    pair.receiver
                        .accept(&transfer.id, destination.clone())
                        .unwrap();
                }
            }
            let outgoing = pair.sender.transfers();
            let incoming = pair.receiver.transfers();
            assert!(
                outgoing.iter().chain(incoming.iter()).all(|transfer| {
                    !matches!(
                        transfer.state,
                        TransferState::Failed { .. } | TransferState::Cancelled
                    )
                }),
                "one code closed before its complete selected set had crossed"
            );
            if outgoing.len() == 40
                && incoming.len() == 40
                && outgoing
                    .iter()
                    .chain(incoming.iter())
                    .all(|transfer| transfer.state == TransferState::Complete)
            {
                break;
            }
            let event = tokio::select! {
                event = pair.sender.next(101) => event,
                event = pair.receiver.next(101) => event,
            };
            assert!(
                matches!(event, Event::Changed),
                "a selected file was refused by the files-only connection"
            );
        }
    })
    .await
    .expect("one scanned code did not deliver its full selection");
    assert_eq!(accepted.len(), 40);
    assert!(
        pair.sender.peers().is_empty() && pair.receiver.peers().is_empty(),
        "a files-only scan silently paired devices"
    );
    for index in 0..40 {
        assert_eq!(
            std::fs::read(destination.join(format!("selection-{index:02}.txt"))).unwrap(),
            [index; 43]
        );
    }
    pair.sender.close().await;
    pair.receiver.close().await;
}

#[tokio::test]
async fn one_code_accepts_a_lazy_selection_after_scanning_before_any_file_is_open() {
    let mut pair = strangers().await;
    let code = pair.sender.begin_files_selection(40, 100).unwrap();
    let generation = pair.sender.files_code().unwrap().generation;
    assert!(pair.sender.files_code().unwrap().pending);
    pair.receiver.join(code).unwrap();
    drive(&mut pair.sender, &mut pair.receiver, |a, _| {
        !a.files_code().unwrap().live
    })
    .await;
    let destination = pair.directory.path().join("received-lazy-selection");
    std::fs::create_dir(&destination).unwrap();
    let mut accepted = std::collections::BTreeSet::new();
    for offset in (0..40).step_by(8) {
        assert!(pair.sender.can_queue_selection(generation, 8, 101).unwrap());
        let mut sources = Vec::new();
        for index in offset..offset + 8 {
            sources.push(
                file_of(
                    pair.directory.path(),
                    &format!("lazy-{index:02}.bin"),
                    &[index as u8; 37],
                )
                .await,
            );
        }
        pair.sender
            .queue_selection(generation, sources, 101)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                for transfer in pair.receiver.transfers() {
                    if transfer.state == TransferState::Offered && accepted.insert(transfer.id.clone()) {
                        pair.receiver.accept(&transfer.id, destination.clone()).unwrap();
                    }
                }
                let outgoing = pair.sender.transfers();
                let incoming = pair.receiver.transfers();
                assert!(outgoing.iter().chain(&incoming).all(|t| !matches!(t.state, TransferState::Failed { .. } | TransferState::Cancelled)));
                if outgoing.len() == offset + 8 && incoming.len() == offset + 8
                    && outgoing.iter().chain(&incoming).all(|t| t.state == TransferState::Complete) {
                    break;
                }
                let event = tokio::select! { event = pair.sender.next(101) => event, event = pair.receiver.next(101) => event };
                assert!(matches!(event, Event::Changed), "the empty producer gap closed its files-only connection");
            }
        }).await.expect("a later batch did not use the original scanned code");
        assert!(
            pair.sender.files_code().unwrap().pending,
            "a gap between batches ended the source lifetime"
        );
    }
    assert!(
        pair.sender.can_queue_selection(generation, 1, 101).is_err(),
        "the declared selection quota was exceeded"
    );
    pair.sender.finish_files_selection(generation, 0).unwrap();
    assert!(
        !pair.sender.files_code().unwrap().pending,
        "a finished and drained producer retained background protection"
    );
    assert!(
        pair.sender.can_queue_selection(generation, 1, 101).is_err(),
        "a finished producer was reopened"
    );
    assert!(pair.sender.peers().is_empty() && pair.receiver.peers().is_empty());
    for index in 0..40 {
        assert_eq!(
            std::fs::read(destination.join(format!("lazy-{index:02}.bin"))).unwrap(),
            [index as u8; 37]
        );
    }
    pair.sender.close().await;
    pair.receiver.close().await;
}
