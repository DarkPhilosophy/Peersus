use iroh::SecretKey;
use peersus_core::{
    config::NetworkSettings,
    model::{PeerId, TransferState},
};
use peersus_net::{
    file_session::Incoming, history::History, streams, transfer::Source,
    transfer_tasks::TransferTasks, transport,
};
use std::time::Duration;

async fn exercise(accept: bool) {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("example.bin");
    let destination = directory.path().join("received");
    std::fs::create_dir(&destination).unwrap();
    let bytes = vec![42; 150_000];
    std::fs::write(&source_path, &bytes).unwrap();
    let source = Source::open(&source_path).await.unwrap();
    let remote_id = source.offer.id.clone();
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
    let mut outgoing = TransferTasks::default();
    let mut incoming = TransferTasks::default();
    let mut history_a = History::open(directory.path().join("a.json")).unwrap();
    let path_b = directory.path().join("b.json");
    let mut history_b = History::open(&path_b).unwrap();
    let sent_id = outgoing
        .send(
            left.clone(),
            Some("Receiver".into()),
            None,
            source,
            &mut history_a,
            100,
        )
        .unwrap();
    let (tx, mut rx) = right.accept_bi().await.unwrap();
    let streams::Request::File { offer } = streams::receive(&mut rx).await.unwrap() else {
        panic!("Expected a file stream");
    };
    let received_id = incoming
        .receive(
            PeerId(a.id().to_string()),
            Some("Sender".into()),
            Incoming { tx, rx, offer },
            right.clone(),
            100,
        )
        .unwrap();
    assert_ne!(received_id, remote_id);
    // Each side keeps the name the other gave, as a label of the record, and the identity stays the key.
    assert_eq!(
        history_a.entries()[0].peer_name.as_deref(),
        Some("Receiver")
    );
    let offer = incoming.next(&mut history_b).await.unwrap();
    assert_eq!(offer.id, received_id);
    // Kept on the persisted record too, which is where the history reads it from.
    assert_eq!(offer.peer_name.as_deref(), Some("Sender"));
    assert_eq!(history_b.entries()[0].peer_name.as_deref(), Some("Sender"));
    assert_eq!(offer.state, TransferState::Offered);
    assert!(!destination.join("example.bin").exists());
    if accept {
        incoming
            .accept(&received_id, destination.clone(), &mut history_b)
            .unwrap();
        assert!(
            incoming
                .accept(&received_id, destination.clone(), &mut history_b)
                .is_err()
        );
        let (sent, received) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(outgoing.next(&mut history_a), incoming.next(&mut history_b))
        })
        .await
        .unwrap();
        let sent = sent.unwrap();
        let received = received.unwrap();
        assert_eq!(sent.id, sent_id);
        assert_eq!(sent.state, TransferState::Complete);
        assert_eq!(received.state, TransferState::Complete);
        assert_eq!(received.completed_bytes, bytes.len() as u64);
        assert_eq!(
            std::fs::read(destination.join("example.bin")).unwrap(),
            bytes
        );
        let reopened = History::open(path_b).unwrap();
        assert_eq!(reopened.entries()[0].state, TransferState::Complete);
    } else {
        incoming.cancel(&received_id, &mut history_b).unwrap();
        assert!(
            incoming
                .accept(&received_id, destination.clone(), &mut history_b)
                .is_err()
        );
        let sent = tokio::time::timeout(Duration::from_secs(5), outgoing.next(&mut history_a))
            .await
            .unwrap()
            .unwrap();
        assert!(!matches!(
            sent.state,
            TransferState::Complete | TransferState::Running
        ));
        assert_eq!(
            incoming.snapshot(&history_b)[0].state,
            TransferState::Cancelled
        );
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        assert_eq!(
            History::open(path_b).unwrap().entries()[0].state,
            TransferState::Cancelled
        );
    }
    drop(incoming);
    drop(outgoing);
    left.close(0_u32.into(), b"done");
    right.close(0_u32.into(), b"done");
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn accepted_transfer_updates_durable_history_after_verified_receipt() {
    exercise(true).await;
}

#[tokio::test]
async fn cancelled_offer_cannot_be_accepted_or_publish_content() {
    exercise(false).await;
}

/// An offer from `outgoing` (which waits `own` seconds) to a receiver that waits `theirs` seconds, up to the point where the receiver
/// has shown it. Returns what the test needs to look at.
struct Offered {
    outgoing: TransferTasks,
    incoming: TransferTasks,
    history_a: History,
    sent_id: String,
    received_id: String,
    left: iroh::endpoint::Connection,
    right: iroh::endpoint::Connection,
    a: iroh::Endpoint,
    b: iroh::Endpoint,
    _directory: tempfile::TempDir,
}

async fn offered(own: u32, theirs: u32) -> Offered {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("example.bin");
    std::fs::write(&path, vec![7; 20_000]).unwrap();
    let source = Source::open(&path).await.unwrap();
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
    let (left, right) = (left.unwrap(), right.unwrap());
    let (mut outgoing, mut incoming) = (TransferTasks::default(), TransferTasks::default());
    outgoing.set_wait(own);
    incoming.set_wait(theirs);
    let mut history_a = History::open(directory.path().join("a.json")).unwrap();
    let mut history_b = History::open(directory.path().join("b.json")).unwrap();
    let sent_id = outgoing
        .send(left.clone(), None, None, source, &mut history_a, 1_000)
        .unwrap();
    let (tx, mut rx) = right.accept_bi().await.unwrap();
    let streams::Request::File { offer } = streams::receive(&mut rx).await.unwrap() else {
        panic!("Expected a file stream")
    };
    let received_id = incoming
        .receive(
            PeerId(a.id().to_string()),
            None,
            Incoming { tx, rx, offer },
            right.clone(),
            1_000,
        )
        .unwrap();
    // The receiver shows the offer: this is when it tells the other device how long it waits.
    incoming.next(&mut history_b).await.unwrap();
    Offered {
        outgoing,
        incoming,
        history_a,
        sent_id,
        received_id,
        left,
        right,
        a,
        b,
        _directory: directory,
    }
}

impl Offered {
    async fn close(self) {
        drop(self.incoming);
        drop(self.outgoing);
        self.left.close(0_u32.into(), b"done");
        self.right.close(0_u32.into(), b"done");
        self.a.close().await;
        self.b.close().await;
    }
}

/// Wait until the sending side's record of the offer carries `deadline`, or say what it carried.
async fn until_sender_deadline(offered: &mut Offered, deadline: u64) -> Option<u64> {
    let sent_id = offered.sent_id.clone();
    let seen = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let record = offered.outgoing.next(&mut offered.history_a).await.unwrap();
            if record.id == sent_id && record.expires_at == Some(deadline) {
                return record.expires_at;
            }
        }
    })
    .await;
    seen.ok().flatten()
}

#[tokio::test]
async fn the_sender_counts_down_the_deadline_the_receiver_told_whether_it_is_shorter_or_longer_than_its_own()
 {
    // The receiver waits 5 s and the sender 120: the sender's record comes to carry the RECEIVER'S deadline, the moment it was made
    // plus 5. The receiver decides (it is its person who answers), so a longer wait than the sender's own is taken as told too.
    let mut shorter = offered(120, 5).await;
    assert_eq!(
        shorter.outgoing.snapshot(&shorter.history_a)[0].expires_at,
        Some(1_000 + 120),
        "before it is told, the sender counts its own wait"
    );
    assert_eq!(
        until_sender_deadline(&mut shorter, 1_000 + 5).await,
        Some(1_005),
        "the sender never came to count the receiver's deadline"
    );
    shorter.close().await;

    let mut longer = offered(30, 600).await;
    assert_eq!(
        until_sender_deadline(&mut longer, 1_000 + 600).await,
        Some(1_600),
        "a longer wait than its own was not taken as told"
    );
    longer.close().await;
}

#[tokio::test]
async fn the_sender_stops_waiting_when_the_receivers_told_deadline_ends_and_not_at_its_own() {
    // The receiver waits the shortest wait there is and nobody answers; the sender's own wait is two minutes. The offer must end at
    // the receiver's deadline on BOTH sides, in seconds and not minutes.
    let mut short = offered(120, peersus_core::config::MIN_OFFER_TIMEOUT).await;
    let started = std::time::Instant::now();
    let ended = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let record = short.outgoing.next(&mut short.history_a).await.unwrap();
            if record.state != TransferState::Offered {
                return record;
            }
        }
    })
    .await
    .expect("the sender went on waiting past the deadline the receiver told");
    assert_eq!(
        ended.state,
        TransferState::Cancelled,
        "an offer nobody answered ended on the sender as {:?}, and not as a refusal",
        ended.state
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the sender waited {:?}, on its own two minutes",
        started.elapsed()
    );
    let _ = short.received_id.clone();
    short.close().await;
}

/// A receiver of an older version tells nothing: it opens no stream and only answers on the one the offer came on.
#[tokio::test]
async fn a_receiver_that_tells_nothing_leaves_the_sender_on_its_own_wait_and_the_transfer_still_completes()
 {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("example.bin");
    let bytes = vec![9; 30_000];
    std::fs::write(&path, &bytes).unwrap();
    let source = Source::open(&path).await.unwrap();
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
    let (left, right) = (left.unwrap(), right.unwrap());
    let mut outgoing = TransferTasks::default();
    outgoing.set_wait(90);
    let mut history_a = History::open(directory.path().join("a.json")).unwrap();
    let sent_id = outgoing
        .send(left.clone(), None, None, source, &mut history_a, 2_000)
        .unwrap();
    // The old receiver: accepts the stream, reads the offer, answers `accepted`, reads the file, answers `complete`. It never
    // accepts a unidirectional stream and never opens one.
    let destination = directory.path().join("received");
    std::fs::create_dir(&destination).unwrap();
    let old_receiver = async {
        let (mut tx, mut rx) = right.accept_bi().await.unwrap();
        let streams::Request::File { offer } = streams::receive(&mut rx).await.unwrap() else {
            panic!("Expected a file stream")
        };
        transport::send(&mut tx, &"accepted").await.unwrap();
        peersus_net::transfer::receive(&mut rx, &offer, &destination, |_| {})
            .await
            .unwrap();
        transport::send(&mut tx, &"complete").await.unwrap();
        tx.finish().unwrap();
    };
    let sender = async {
        // Nothing is told, so until the end the sender counts its own wait.
        loop {
            let record = outgoing.next(&mut history_a).await.unwrap();
            if record.state != TransferState::Offered {
                break record;
            }
            assert_eq!(
                record.expires_at,
                Some(2_000 + 90),
                "a deadline appeared that nobody told"
            );
        }
    };
    let (record, ()) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(sender, old_receiver)
    })
    .await
    .expect("a receiver that tells nothing held the transfer up");
    assert_eq!(record.id, sent_id);
    assert_eq!(
        record.state,
        TransferState::Complete,
        "the transfer to an older receiver did not complete"
    );
    assert_eq!(
        std::fs::read(destination.join("example.bin")).unwrap(),
        bytes
    );
    drop(outgoing);
    left.close(0_u32.into(), b"done");
    right.close(0_u32.into(), b"done");
    a.close().await;
    b.close().await;
}

/// The receiver announces that it waits the shortest time and then NEVER answers, so that only the sender's own use of the told
/// deadline can end the wait: its own wait is two minutes, and nothing else will refuse the offer.
#[tokio::test]
async fn the_sender_ends_the_wait_itself_at_the_deadline_the_receiver_told_when_nothing_answers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("example.bin");
    std::fs::write(&path, vec![1; 10_000]).unwrap();
    let source = Source::open(&path).await.unwrap();
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
    let (left, right) = (left.unwrap(), right.unwrap());
    let mut outgoing = TransferTasks::default();
    outgoing.set_wait(120);
    let mut history_a = History::open(directory.path().join("a.json")).unwrap();
    let sent_id = outgoing
        .send(left.clone(), None, None, source, &mut history_a, 3_000)
        .unwrap();
    let told = peersus_core::config::MIN_OFFER_TIMEOUT;
    // Reads the offer, tells its deadline, and keeps the stream open without ever answering on it.
    let silent_receiver = async {
        let (_tx, mut rx) = right.accept_bi().await.unwrap();
        let streams::Request::File { offer } = streams::receive(&mut rx).await.unwrap() else {
            panic!("Expected a file stream")
        };
        peersus_net::announce::tell(&right, &offer.id, told).await;
        std::future::pending::<()>().await;
    };
    let sender = async {
        let started = std::time::Instant::now();
        loop {
            let record = outgoing.next(&mut history_a).await.unwrap();
            if record.id == sent_id && record.state != TransferState::Offered {
                return (record, started.elapsed());
            }
        }
    };
    let (record, waited) = tokio::select! {
        done = tokio::time::timeout(Duration::from_secs(30), sender) => done.expect("the sender went on waiting past the deadline the receiver told"),
        () = silent_receiver => unreachable!("the silent receiver ends only with the test"),
    };
    assert_eq!(
        record.state,
        TransferState::Cancelled,
        "an offer nobody answered ended on the sender as {:?}, and not as a refusal",
        record.state
    );
    assert!(
        waited >= Duration::from_secs(u64::from(told)) - Duration::from_secs(1),
        "the sender gave up after {waited:?}, before the told deadline"
    );
    assert!(
        waited < Duration::from_secs(u64::from(told) + 10),
        "the sender waited {waited:?}: on its own two minutes and not on the told deadline"
    );
    drop(outgoing);
    left.close(0_u32.into(), b"done");
    right.close(0_u32.into(), b"done");
    a.close().await;
    b.close().await;
}
