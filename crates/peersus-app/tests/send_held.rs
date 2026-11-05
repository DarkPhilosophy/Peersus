//! Files that the system handed over already open (on Android a picked file has no path), sent from where they are
//! held, between two owners that really pair over the network.
//!
//! The interface keeps a file in a registry and says only its number in the command, because a command travels as
//! text and a file cannot be in it. The owner takes the file by that number. What is shown here is that a file with
//! no path at all arrives intact, that the number works once, and that the limits of an ordinary send hold here too.

use iroh::SecretKey;
use peersus_app::runtime::{Client, Reply, Runtime};
use peersus_core::{
    Settings,
    config::NetworkSettings,
    model::{Command, FileDestination, PeerId, Snapshot, TransferState},
};
use peersus_net::{
    held::Held, history::History, network_host::NetworkHost, peers::Peers, settings::SettingsStore,
    transport,
};
use std::{io::Write, path::Path, sync::Arc, time::Duration};

/// A short private directory for the sockets of plugin processes.
fn sockets() -> tempfile::TempDir {
    // Unix socket paths are short; elsewhere there are no such sockets, and the system's own temporary folder is used.
    #[cfg(unix)]
    let directory = tempfile::Builder::new()
        .prefix("sh")
        .tempdir_in("/tmp")
        .unwrap();
    #[cfg(not(unix))]
    let directory = tempfile::Builder::new().prefix("sh").tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

/// An owner that keeps its state under `root/name` and holds files in `held`.
async fn start(
    root: &Path,
    name: &str,
    held: Held,
) -> (Client, tokio::task::JoinHandle<()>, PeerId) {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    let key = SecretKey::generate();
    let mut store = SettingsStore::open(path.join("preferences.json")).unwrap();
    let settings = Settings {
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        },
        device_name: name.to_owned(),
        ..Settings::default()
    };
    store.replace(settings.clone()).unwrap();
    let id = PeerId(key.public().to_string());
    let endpoint = transport::bind(key, &settings.network).await.unwrap();
    let network = NetworkHost::new(
        endpoint,
        settings.device_name,
        Peers::open(path.join("peers.json")).unwrap(),
        History::open(path.join("history.json")).unwrap(),
    );
    let directory = sockets();
    let standalone = peersus_net::grants::StandaloneGrants::open(path.join("grants.json")).unwrap();
    let (runtime, client) = Runtime::new(
        network,
        standalone,
        store,
        Default::default(),
        directory.path().to_path_buf(),
    )
    .unwrap();
    let runtime = runtime.with_held(held);
    let task = tokio::spawn(async move {
        runtime.run().await;
        drop(directory);
    });
    (client, task, id)
}

async fn until(client: &Client, predicate: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    let mut updates = client.subscribe();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = client.snapshot();
            if predicate(&current) {
                return current;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .expect("the owner did not reach the expected state in time")
}

/// Pair `a` with `b` the way two people do.
async fn pair(a: &Client, b: &Client) {
    let Reply::Invitation(invitation) = a.command(Command::CreateInvitation {}).await.unwrap()
    else {
        panic!("no invitation")
    };
    b.command(Command::JoinInvitation { invitation })
        .await
        .unwrap();
    let pa = until(a, |s| !s.pairing.is_empty()).await.pairing[0].clone();
    let pb = until(b, |s| !s.pairing.is_empty()).await.pairing[0].clone();
    a.command(Command::ChoosePairing {
        request: pa.id.clone(),
        number: pa.options[0],
    })
    .await
    .unwrap();
    b.command(Command::ChoosePairing {
        request: pb.id.clone(),
        number: pa.options[0],
    })
    .await
    .unwrap();
    until(a, |s| s.peers.iter().any(|p| p.online)).await;
    until(b, |s| s.peers.iter().any(|p| p.online)).await;
}

/// A file as the system hands it over: it exists only as an open file, with no path anywhere.
fn pathless(bytes: &[u8]) -> std::fs::File {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    std::io::Seek::rewind(&mut file).unwrap();
    file
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_with_no_path_is_sent_by_its_number_and_arrives_intact_under_the_name_given() {
    let root = tempfile::tempdir().unwrap();
    let held = Held::default();
    let (sender, _sender_task, _) = start(root.path(), "phone", held.clone()).await;
    let (receiver, _receiver_task, _) = start(root.path(), "laptop", Held::default()).await;
    pair(&sender, &receiver).await;
    let to = until(&sender, |s| s.peers.iter().any(|p| p.online))
        .await
        .peers[0]
        .id
        .clone();

    let data: Vec<u8> = (0..300_000).map(|n| (n % 251) as u8).collect();
    let number = held.keep(pathless(&data)).expect("the file was not kept");
    let reply = sender
        .command(Command::SendHeld {
            peer: to,
            files: vec![(number, "holiday.bin".to_owned())],
        })
        .await
        .expect("a held file was refused");
    assert!(matches!(&reply, Reply::Transfers(ids) if ids.len() == 1));
    // The number was used: the file left the registry and nothing is held for it any more.
    assert!(
        held.is_empty(),
        "the file was still held after the send took it"
    );

    let offered = until(&receiver, |s| {
        s.transfers
            .iter()
            .any(|t| t.incoming && t.state == TransferState::Offered)
    })
    .await;
    let transfer = offered.transfers.iter().find(|t| t.incoming).unwrap();
    assert_eq!(
        transfer.name, "holiday.bin",
        "the name offered is the one given and not a number or a path"
    );
    assert_eq!(transfer.bytes, data.len() as u64);
    let destination = tempfile::tempdir().unwrap();
    receiver
        .command(Command::AcceptTransfer {
            transfer: transfer.id.clone(),
            directory: destination.path().to_path_buf(),
        })
        .await
        .unwrap();
    until(&receiver, |s| {
        s.transfers
            .iter()
            .any(|t| t.incoming && t.state == TransferState::Complete)
    })
    .await;
    assert_eq!(
        std::fs::read(destination.path().join("holiday.bin")).unwrap(),
        data
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_number_that_was_used_or_never_given_is_refused_and_a_file_cannot_be_sent_twice() {
    let root = tempfile::tempdir().unwrap();
    let held = Held::default();
    let (sender, _s, _) = start(root.path(), "phone", held.clone()).await;
    let (receiver, _r, _) = start(root.path(), "laptop", Held::default()).await;
    pair(&sender, &receiver).await;
    let to = until(&sender, |s| s.peers.iter().any(|p| p.online))
        .await
        .peers[0]
        .id
        .clone();

    let number = held.keep(pathless(b"once")).unwrap();
    let send = |files: Vec<(u64, String)>| {
        let (sender, to) = (sender.clone(), to.clone());
        async move { sender.command(Command::SendHeld { peer: to, files }).await }
    };
    assert!(send(vec![(number, "a.bin".into())]).await.is_ok());
    // The same number again: the file is gone, so a stale or replayed command reaches nothing.
    assert!(
        send(vec![(number, "a.bin".into())]).await.is_err(),
        "a file was sent twice by one number"
    );
    // A number that was never given.
    assert!(
        send(vec![(987_654, "b.bin".into())]).await.is_err(),
        "an unknown number was accepted"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_limits_of_an_ordinary_send_hold_for_held_files_and_one_bad_file_leaves_no_partial_offer()
 {
    let root = tempfile::tempdir().unwrap();
    let held = Held::default();
    let (sender, _s, _) = start(root.path(), "phone", held.clone()).await;
    let (receiver, _r, _) = start(root.path(), "laptop", Held::default()).await;
    pair(&sender, &receiver).await;
    let to = until(&sender, |s| s.peers.iter().any(|p| p.online))
        .await
        .peers[0]
        .id
        .clone();

    // None, and more than sixteen: refused, and every number of the refused command is let go of, because the interface cannot
    // know which the owner reached and a number kept would fill the registry for the next choice.
    assert!(
        sender
            .command(Command::SendHeld {
                peer: to.clone(),
                files: vec![]
            })
            .await
            .is_err()
    );
    let many: Vec<(u64, String)> = (0..17)
        .map(|n| (held.keep(pathless(b"x")).unwrap(), format!("f{n}.bin")))
        .collect();
    assert!(
        sender
            .command(Command::SendHeld {
                peer: to.clone(),
                files: many.clone()
            })
            .await
            .is_err()
    );
    assert!(
        held.is_empty(),
        "a command refused for its count left {} files held",
        held.len()
    );

    // Three files where the second has a name the rule refuses: none is offered, and nothing arrives.
    let good = held.keep(pathless(b"good")).unwrap();
    let bad = held.keep(pathless(b"bad")).unwrap();
    let last = held.keep(pathless(b"last")).unwrap();
    let before = receiver.snapshot().transfers.len();
    let outcome = sender
        .command(Command::SendHeld {
            peer: to,
            files: vec![
                (good, "good.bin".into()),
                (bad, "../escape".into()),
                (last, "last.bin".into()),
            ],
        })
        .await;
    assert!(
        outcome.is_err(),
        "a command with a name the rule refuses was accepted"
    );
    // The one not reached (`last`) is let go of with the rest, not left held behind a refused command.
    assert!(
        held.is_empty(),
        "a refused command left {} files held",
        held.len()
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        receiver.snapshot().transfers.len(),
        before,
        "a partial offer reached the other device"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_outgoing_queue_retains_held_files_until_capacity_returns() {
    let root = tempfile::tempdir().unwrap();
    let held = Held::default();
    let (sender, sender_task, _) = start(root.path(), "phone", held.clone()).await;
    let (receiver, receiver_task, _) = start(root.path(), "laptop", Held::default()).await;
    pair(&sender, &receiver).await;
    let peer = sender
        .snapshot()
        .peers
        .iter()
        .find(|p| p.online)
        .unwrap()
        .id
        .clone();
    // Fill the sixteen active slots and the bounded five-hundred-file queue.
    for offset in (0..516).step_by(16) {
        let files = (offset..(offset + 16).min(516))
            .map(|index| {
                (
                    held.keep(pathless(b"queued")).unwrap(),
                    format!("queued-{index}.bin"),
                )
            })
            .collect();
        assert!(matches!(
            sender
                .command(Command::QueueHeld {
                    destination: FileDestination::Peer { peer: peer.clone() },
                    files
                })
                .await
                .unwrap(),
            Reply::Transfers(_)
        ));
    }
    let number = held
        .keep(pathless(b"retained during backpressure"))
        .unwrap();
    let pending = Command::QueueHeld {
        destination: FileDestination::Peer { peer: peer.clone() },
        files: vec![(number, "retained.bin".into())],
    };
    let errors_before = sender.snapshot().errors.clone();
    assert!(matches!(
        sender.command(pending.clone()).await.unwrap(),
        Reply::QueueBusy
    ));
    assert_eq!(
        held.len(),
        1,
        "backpressure consumed or released the caller's file"
    );
    assert_eq!(
        sender.snapshot().errors,
        errors_before,
        "normal backpressure was reported as a failure"
    );
    let offer = until(&receiver, |s| {
        s.transfers
            .iter()
            .any(|t| t.incoming && t.state == TransferState::Offered)
    })
    .await;
    let transfer = offer
        .transfers
        .iter()
        .find(|t| t.incoming && t.state == TransferState::Offered)
        .unwrap();
    let destination = tempfile::tempdir().unwrap();
    receiver
        .command(Command::AcceptTransfer {
            transfer: transfer.id.clone(),
            directory: destination.path().into(),
        })
        .await
        .unwrap();
    let mut changes = sender.subscribe();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match sender.command(pending.clone()).await.unwrap() {
                Reply::QueueBusy => changes.changed().await.unwrap(),
                Reply::Transfers(ids) => {
                    assert_eq!(ids.len(), 1);
                    break;
                }
                _ => panic!("a queued handoff returned an unrelated reply"),
            }
        }
    })
    .await
    .expect("a completed transfer did not release capacity");
    assert!(held.is_empty());
    sender.command(Command::Shutdown {}).await.unwrap();
    receiver.command(Command::Shutdown {}).await.unwrap();
    sender_task.await.unwrap();
    receiver_task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_single_code_streams_eighty_pathless_choices_in_descriptor_sized_batches() {
    let root = tempfile::tempdir().unwrap();
    let held = Held::default();
    let (sender, sender_task, _) = start(root.path(), "phone", held.clone()).await;
    let (receiver, receiver_task, _) = start(root.path(), "laptop", Held::default()).await;
    let Reply::FilesInvitation {
        code, generation, ..
    } = sender
        .command(Command::BeginFilesInvitation { count: 80 })
        .await
        .unwrap()
    else {
        panic!("no files code")
    };
    receiver
        .command(Command::JoinInvitation { invitation: code })
        .await
        .unwrap();
    for offset in (0..80).step_by(16) {
        let files = (offset..offset + 16)
            .map(|index| {
                (
                    held.keep(pathless(&[index as u8; 29])).unwrap(),
                    format!("chosen-{index:02}.bin"),
                )
            })
            .collect();
        let reply = sender
            .command(Command::QueueHeld {
                destination: FileDestination::Invitation { generation },
                files,
            })
            .await
            .unwrap();
        assert!(matches!(reply, Reply::Transfers(_)));
        assert!(
            held.is_empty(),
            "the owner did not consume the acknowledged descriptor batch"
        );
    }
    sender
        .command(Command::FinishFilesInvitation {
            generation,
            refused: 0,
        })
        .await
        .unwrap();
    let destination = tempfile::tempdir().unwrap();
    let mut accepted = std::collections::BTreeSet::new();
    let mut changes = receiver.subscribe();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let snapshot = receiver.snapshot();
            for transfer in &snapshot.transfers {
                assert!(!matches!(
                    transfer.state,
                    TransferState::Failed { .. } | TransferState::Cancelled
                ));
                if transfer.incoming
                    && transfer.state == TransferState::Offered
                    && accepted.insert(transfer.id.clone())
                {
                    receiver
                        .command(Command::AcceptTransfer {
                            transfer: transfer.id.clone(),
                            directory: destination.path().into(),
                        })
                        .await
                        .unwrap();
                }
            }
            if snapshot.transfers.len() == 80
                && snapshot
                    .transfers
                    .iter()
                    .all(|t| t.state == TransferState::Complete)
            {
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the complete selection did not cross its single files code");
    until(&sender, |s| {
        s.transfers.len() == 80
            && s.transfers
                .iter()
                .all(|t| t.state == TransferState::Complete)
    })
    .await;
    assert!(sender.snapshot().peers.is_empty() && receiver.snapshot().peers.is_empty());
    for index in 0..80 {
        assert_eq!(
            std::fs::read(destination.path().join(format!("chosen-{index:02}.bin"))).unwrap(),
            [index as u8; 29]
        );
    }
    sender.command(Command::Shutdown {}).await.unwrap();
    receiver.command(Command::Shutdown {}).await.unwrap();
    sender_task.await.unwrap();
    receiver_task.await.unwrap();
}
