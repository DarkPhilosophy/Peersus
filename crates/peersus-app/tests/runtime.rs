//! Behavior tests run real runtimes and real encrypted connections between them. The
//! plugin side of the application is tested in `plugins.rs`, with real plugin processes.
use clap::Parser;
use iroh::SecretKey;
use peersus_app::{
    cli::Cli,
    plugin_host::Registry,
    runtime::{Client, Reply, Runtime},
};
use peersus_core::{
    Settings,
    config::NetworkSettings,
    model::{Command, PeerId, Snapshot, TransferState},
};
use peersus_net::{
    history::History, network_host::NetworkHost, peers::Peers, settings::SettingsStore, transport,
};
use std::{path::Path, sync::Arc, time::Duration};

async fn start(path: &Path) -> (Client, tokio::task::JoinHandle<()>) {
    start_with(path, Registry::default()).await
}
async fn start_with(path: &Path, plugins: Registry) -> (Client, tokio::task::JoinHandle<()>) {
    start_waiting(path, plugins, Settings::default().offer_timeout_seconds).await
}
/// A runtime that starts from a SAVED wait for an answer to an offer: the path every start takes.
async fn start_waiting(
    path: &Path,
    plugins: Registry,
    offer_timeout_seconds: u32,
) -> (Client, tokio::task::JoinHandle<()>) {
    std::fs::create_dir_all(path).unwrap();
    let mut store = SettingsStore::open(path.join("preferences.json")).unwrap();
    let settings = Settings {
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        },
        offer_timeout_seconds,
        ..Settings::default()
    };
    store.replace(settings.clone()).unwrap();
    let endpoint = transport::bind(SecretKey::generate(), &settings.network)
        .await
        .unwrap();
    let network = NetworkHost::new(
        endpoint,
        settings.device_name,
        Peers::open(path.join("peers.json")).unwrap(),
        History::open(path.join("history.json")).unwrap(),
    );
    // A Unix socket path has a small length limit and the directory must be private to
    // the user, so it is a short directory of its own rather than a subdirectory here.
    let sockets = short_private_directory();
    let standalone = peersus_net::grants::StandaloneGrants::open(path.join("grants.json")).unwrap();
    let (runtime, client) = Runtime::new(
        network,
        standalone,
        store,
        plugins,
        sockets.path().to_path_buf(),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        runtime.run().await;
        drop(sockets);
    });
    (client, task)
}

/// A private directory with a short path, for sockets of plugin processes.
fn short_private_directory() -> tempfile::TempDir {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::Builder::new()
            .prefix("pa")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }
    #[cfg(not(unix))]
    tempfile::tempdir().unwrap()
}
async fn until(client: &Client, predicate: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    let mut updates = client.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = Arc::clone(&updates.borrow_and_update());
            if predicate(&snapshot) {
                return snapshot;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}
/// Pair two runtimes through the real three-circle ceremony.
async fn pair(a: &Client, b: &Client) -> PeerId {
    let Reply::Invitation(invitation) = b.command(Command::CreateInvitation {}).await.unwrap()
    else {
        panic!("Expected invitation")
    };
    a.command(Command::JoinInvitation { invitation })
        .await
        .unwrap();
    let pa = until(a, |s| !s.pairing.is_empty()).await;
    let pb = until(b, |s| !s.pairing.is_empty()).await;
    let number = pa.pairing[0].options[0];
    for (client, prompt) in [(a, &pa), (b, &pb)] {
        client
            .command(Command::ChoosePairing {
                request: prompt.pairing[0].id.clone(),
                number,
            })
            .await
            .unwrap();
    }
    let state = until(a, |s| s.peers.iter().any(|p| p.online)).await;
    until(b, |s| s.peers.iter().any(|p| p.online)).await;
    state.peers[0].id.clone()
}
async fn stop(clients: [(&Client, tokio::task::JoinHandle<()>); 2]) {
    for (client, task) in clients {
        client.command(Command::Shutdown {}).await.unwrap();
        task.await.unwrap();
    }
}

#[tokio::test]
async fn selected_batches_wait_for_stream_capacity_and_queued_cancellation_never_offers_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let (sender, sender_task) = start(&directory.path().join("sender")).await;
    let (receiver, receiver_task) = start(&directory.path().join("receiver")).await;
    let peer = pair(&sender, &receiver).await;
    let destination = directory.path().join("received");
    std::fs::create_dir(&destination).unwrap();
    let paths: Vec<_> = (0..32)
        .map(|index| {
            let path = directory.path().join(format!("selected-{index:02}.bin"));
            std::fs::write(&path, vec![index as u8; index + 1]).unwrap();
            path
        })
        .collect();
    for batch in paths.chunks(16) {
        sender
            .command(Command::SendFiles {
                peer: peer.clone(),
                paths: batch.to_vec(),
            })
            .await
            .expect("a selected batch must wait instead of being rejected when streams are busy");
    }
    let queued = until(&sender, |snapshot| snapshot.transfers.len() == 32).await;
    assert_eq!(
        queued
            .transfers
            .iter()
            .filter(|transfer| transfer.expires_at.is_none())
            .count(),
        16,
        "queued files must not spend their offer timeout before a stream is available"
    );
    let cancelled = queued
        .transfers
        .iter()
        .find(|transfer| transfer.name == "selected-31.bin")
        .unwrap();
    sender
        .command(Command::CancelTransfer {
            transfer: cancelled.id.clone(),
        })
        .await
        .unwrap();
    let initially_offered = until(&receiver, |snapshot| snapshot.transfers.len() == 16).await;
    assert!(
        initially_offered
            .transfers
            .iter()
            .all(|transfer| transfer.name != "selected-31.bin")
    );
    let mut accepted = std::collections::BTreeSet::new();
    while accepted.len() < 31 {
        let available = until(&receiver, |snapshot| {
            snapshot.transfers.iter().any(|transfer| {
                transfer.state == TransferState::Offered && !accepted.contains(&transfer.id)
            })
        })
        .await;
        for transfer in &available.transfers {
            assert_ne!(
                transfer.name, "selected-31.bin",
                "a cancelled queued file reached the receiver"
            );
            if transfer.state == TransferState::Offered && accepted.insert(transfer.id.clone()) {
                receiver
                    .command(Command::AcceptTransfer {
                        transfer: transfer.id.clone(),
                        directory: destination.clone(),
                    })
                    .await
                    .unwrap();
            }
        }
    }
    for client in [&sender, &receiver] {
        until(client, |snapshot| {
            snapshot
                .transfers
                .iter()
                .filter(|transfer| transfer.state == TransferState::Complete)
                .count()
                == 31
        })
        .await;
    }
    for (index, path) in paths.iter().enumerate().take(31) {
        assert_eq!(
            std::fs::read(destination.join(path.file_name().unwrap())).unwrap(),
            vec![index as u8; index + 1]
        );
    }
    assert!(!destination.join("selected-31.bin").exists());
    stop([(&sender, sender_task), (&receiver, receiver_task)]).await;
}

#[tokio::test]
async fn clients_pair_transfer_and_apply_cli_or_ui_preferences_through_one_owner() {
    let directory = tempfile::tempdir().unwrap();
    let path_a = directory.path().join("a");
    let (a, task_a) = start(&path_a).await;
    let (b, task_b) = start(&directory.path().join("b")).await;
    let peer = pair(&a, &b).await;
    let source = directory.path().join("payload.bin");
    let destination = directory.path().join("received");
    std::fs::create_dir(&destination).unwrap();
    let bytes = vec![47; 120_000];
    std::fs::write(&source, &bytes).unwrap();
    a.command(Command::SendFiles {
        peer,
        paths: vec![source],
    })
    .await
    .unwrap();
    let offer = until(&b, |s| !s.transfers.is_empty()).await;
    let cli = || {
        Cli::try_parse_from([
            "peersus",
            "--background",
            "--bind",
            "127.0.0.2",
            "--allow-relay=false",
        ])
        .unwrap()
    };
    let candidate = cli()
        .preferences
        .patch()
        .apply(&a.snapshot().settings)
        .unwrap();
    // A transfer is in flight, so the network cannot be swapped underneath it.
    assert!(cli().execute(&a).await.is_err());
    assert!(!a.snapshot().settings.background);
    b.command(Command::AcceptTransfer {
        transfer: offer.transfers[0].id.clone(),
        directory: destination.clone(),
    })
    .await
    .unwrap();
    for client in [&a, &b] {
        until(client, |s| s.transfers[0].state == TransferState::Complete).await;
    }
    assert_eq!(
        std::fs::read(destination.join("payload.bin")).unwrap(),
        bytes
    );
    // The record of the finished transfer says where the file is, on the receiving side only.
    assert_eq!(
        b.snapshot().transfers[0].saved_as.as_deref(),
        Some(destination.join("payload.bin").as_path())
    );
    assert_eq!(a.snapshot().transfers[0].saved_as, None);
    cli().execute(&a).await.unwrap();
    assert_eq!(a.snapshot().settings, candidate);
    assert_eq!(
        SettingsStore::open(path_a.join("preferences.json"))
            .unwrap()
            .current(),
        &candidate
    );
    // GUI replacements use an optimistic baseline; CLI patches merge in the owner.
    let mut gui = a.snapshot().settings.clone();
    gui.background = false;
    a.command(Command::UpdateSettings {
        settings: Box::new(gui.clone()),
        expected: Box::new(a.snapshot().settings.clone()),
    })
    .await
    .unwrap();
    assert_eq!(a.snapshot().settings, gui);
    let preview = Cli::try_parse_from(["peersus", "--background", "settings"])
        .unwrap()
        .execute(&a)
        .await
        .unwrap();
    let peersus_app::cli::Output::Settings(preview) = preview else {
        panic!("Expected settings preview")
    };
    assert!(preview.background);
    assert!(!a.snapshot().settings.background, "preview must not mutate");
    let invalid = Cli::try_parse_from(["peersus", "--scale", "5", "settings"])
        .unwrap()
        .execute(&a)
        .await;
    assert_eq!(
        invalid.err(),
        Some(peersus_core::SettingsError::Scale.to_string())
    );
    assert_eq!(a.snapshot().settings, gui);
    stop([(&a, task_a), (&b, task_b)]).await;
}

#[tokio::test]
async fn scanning_a_files_code_delivers_the_files_with_no_pairing_and_no_peer_and_a_second_send_needs_a_new_scan()
 {
    let directory = tempfile::tempdir().unwrap();
    let (sender, task_sender) = start(&directory.path().join("sender")).await;
    let (receiver, task_receiver) = start(&directory.path().join("receiver")).await;
    let payload = directory.path().join("report.bin");
    let bytes = vec![11; 90_000];
    std::fs::write(&payload, &bytes).unwrap();
    let destination = directory.path().join("received");
    std::fs::create_dir(&destination).unwrap();

    // The sender shows a code for the chosen file, and the receiver scans it with the one command a scan always is.
    let Reply::FilesInvitation {
        code,
        generation,
        expires_at,
    } = sender
        .command(Command::CreateFilesInvitation {
            paths: vec![payload.clone()],
        })
        .await
        .unwrap()
    else {
        panic!("Expected a files code")
    };
    assert!(expires_at > 0);
    // On show and able to admit a device, for this generation: what a window draws the code from.
    let shown = sender
        .snapshot()
        .files_code
        .expect("the code made was not reported");
    assert_eq!((shown.generation, shown.live), (generation, true));
    receiver
        .command(Command::JoinInvitation { invitation: code })
        .await
        .unwrap();
    // Scanned: the same generation, no longer live, and the file that came of it carries that generation.
    let sent = until(&sender, |s| s.transfers.iter().any(|t| !t.incoming)).await;
    assert_eq!(
        sent.transfers.iter().find(|t| !t.incoming).unwrap().code,
        Some(generation),
        "the file was not tagged with its code"
    );
    let spent = sender
        .snapshot()
        .files_code
        .expect("the code went from the snapshot at the scan");
    assert_eq!(
        (spent.generation, spent.live),
        (generation, false),
        "a scanned code was still live"
    );

    // The file is OFFERED, to be accepted. Nothing was asked of anyone about pairing.
    let offered = until(&receiver, |s| !s.transfers.is_empty()).await;
    assert!(
        sender.snapshot().pairing.is_empty() && receiver.snapshot().pairing.is_empty(),
        "a scan asked for a pairing"
    );
    receiver
        .command(Command::AcceptTransfer {
            transfer: offered.transfers[0].id.clone(),
            directory: destination.clone(),
        })
        .await
        .unwrap();
    // While the files are crossing, the connection that carries them is open, and that is not a device being online.
    for client in [&sender, &receiver] {
        assert!(
            client.snapshot().peers.is_empty(),
            "a device was listed while only its files were crossing"
        );
    }
    for client in [&sender, &receiver] {
        until(client, |s| {
            s.transfers
                .iter()
                .all(|t| t.state == TransferState::Complete)
                && !s.transfers.is_empty()
        })
        .await;
    }
    assert_eq!(
        std::fs::read(destination.join("report.bin")).unwrap(),
        bytes
    );

    // The trust was these files only: neither owner lists the other as a device. Said as presence too, since a device that
    // was never approved must not be seen coming and going: no entry at all, online or offline, at any moment.
    assert!(
        sender.snapshot().peers.is_empty() && receiver.snapshot().peers.is_empty(),
        "a scan made a peer"
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    for client in [&sender, &receiver] {
        let snapshot = client.snapshot();
        assert!(
            snapshot.peers.is_empty(),
            "a device that is only in the history was listed, with a presence"
        );
        assert!(snapshot.peers.iter().all(|peer| !peer.online));
    }
    // The record of the transfer stays, on both sides, as history.
    assert_eq!(sender.snapshot().transfers.len(), 1);
    assert_eq!(receiver.snapshot().transfers.len(), 1);
    // And a NEW file cannot go to that device by itself: it is not paired, so it has to be scanned again.
    let other_device = receiver.snapshot().transfers[0].peer.clone();
    let refused = sender
        .command(Command::SendFiles {
            peer: other_device,
            paths: vec![payload],
        })
        .await;
    assert!(
        refused.is_err(),
        "a file was sent to a device that is only in the history"
    );
    stop([(&sender, task_sender), (&receiver, task_receiver)]).await;
}

#[tokio::test]
async fn sending_a_folder_preserves_nested_paths_and_empty_files_over_real_connections() {
    let directory = tempfile::tempdir().unwrap();
    let (sender, task_sender) = start(&directory.path().join("sender")).await;
    let (receiver, task_receiver) = start(&directory.path().join("receiver")).await;
    let peer = pair(&sender, &receiver).await;
    let folder = directory.path().join("pictures");
    let files = [
        ("a.txt", b"root".as_slice()),
        ("nested/deeper/b.txt", b"nested".as_slice()),
        ("nested/empty.txt", b"".as_slice()),
    ];
    for (name, bytes) in files {
        let path = folder.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    sender
        .command(Command::SendFolder { peer, root: folder })
        .await
        .unwrap();
    let offered = until(&receiver, |snapshot| {
        snapshot.transfers.len() == files.len()
    })
    .await;
    let destination = directory.path().join("received");
    std::fs::create_dir(&destination).unwrap();
    for transfer in &offered.transfers {
        receiver
            .command(Command::AcceptTransfer {
                transfer: transfer.id.clone(),
                directory: destination.clone(),
            })
            .await
            .unwrap();
    }
    for client in [&sender, &receiver] {
        until(client, |snapshot| {
            snapshot.transfers.len() == files.len()
                && snapshot
                    .transfers
                    .iter()
                    .all(|transfer| transfer.state == TransferState::Complete)
        })
        .await;
    }
    for (name, bytes) in files {
        assert_eq!(
            std::fs::read(destination.join("pictures").join(name)).unwrap(),
            bytes
        );
    }
    stop([(&sender, task_sender), (&receiver, task_receiver)]).await;
}

#[tokio::test]
async fn the_saved_wait_decides_how_long_an_offer_waits_and_an_offer_nobody_answers_is_refused_on_its_own()
 {
    let directory = tempfile::tempdir().unwrap();
    let (sender, task_sender) = start(&directory.path().join("sender")).await;
    // The receiver is the one with the person who must answer, and its saved wait is the shortest that can be chosen.
    let (receiver, task_receiver) = start_waiting(
        &directory.path().join("receiver"),
        Registry::default(),
        peersus_core::config::MIN_OFFER_TIMEOUT,
    )
    .await;
    let payload = directory.path().join("unanswered.bin");
    std::fs::write(&payload, vec![9; 10_000]).unwrap();
    let Reply::FilesInvitation { code, .. } = sender
        .command(Command::CreateFilesInvitation {
            paths: vec![payload],
        })
        .await
        .unwrap()
    else {
        panic!("Expected a files code")
    };
    receiver
        .command(Command::JoinInvitation { invitation: code })
        .await
        .unwrap();
    let offered = until(&receiver, |s| {
        s.transfers
            .iter()
            .any(|t| t.incoming && t.state == TransferState::Offered)
    })
    .await;
    // While it waits, the owner says when it ends: this device's own wait from the moment it was offered, as data.
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ends = offered
        .transfers
        .iter()
        .find(|t| t.incoming)
        .unwrap()
        .expires_at
        .expect("a waiting offer reported no deadline");
    let wait = u64::from(peersus_core::config::MIN_OFFER_TIMEOUT);
    assert!(
        (clock + wait - 1..=clock + wait + 1).contains(&ends),
        "the deadline {ends} is not the wait of {wait} s from now ({clock}): it was not made from the wait"
    );
    // The sending side counts down the wait IT enforces: its own preferences (the default here). That is when it stops waiting for an
    // answer whatever the other device chose, so it is a true time for the sender and NOT a claim about the receiver's deadline:
    // here they differ, and the receiver's own wait is the shorter one. Aligning the two needs the receiver to say its deadline, a
    // protocol change that is not made here.
    let sent = until(&sender, |s| s.transfers.iter().any(|t| !t.incoming)).await;
    let sender_ends = sent
        .transfers
        .iter()
        .find(|t| !t.incoming)
        .unwrap()
        .expires_at
        .expect("the sending side reported no deadline");
    let own = u64::from(peersus_core::config::DEFAULT_OFFER_TIMEOUT);
    assert!(
        (clock + own - 2..=clock + own + 2).contains(&sender_ends),
        "the sender's deadline {sender_ends} is not its own wait of {own} s from now ({clock})"
    );
    let before = std::time::Instant::now();
    // Nobody answers. The offer ends by itself, on both sides, and by its own deadline: not at once, and not at two minutes.
    for client in [&receiver, &sender] {
        until(client, |s| {
            !s.transfers.is_empty()
                && s.transfers
                    .iter()
                    .all(|t| t.state == TransferState::Cancelled)
        })
        .await;
    }
    let waited = before.elapsed();
    assert!(
        receiver
            .snapshot()
            .transfers
            .iter()
            .all(|t| t.expires_at.is_none()),
        "an offer that is over still reports a deadline"
    );
    assert!(
        sender
            .snapshot()
            .transfers
            .iter()
            .all(|t| t.expires_at.is_none()),
        "a send that is over still counts down on the sending side"
    );
    assert!(
        offered
            .transfers
            .iter()
            .all(|t| t.state == TransferState::Offered),
        "it was not waiting when it was shown"
    );
    assert!(
        waited >= Duration::from_secs(3),
        "the offer was refused after {waited:?}, long before its wait"
    );
    assert!(
        waited < Duration::from_secs(9),
        "the offer waited {waited:?}: the saved wait was not the one used"
    );
    // An answer after the end is refused in words, and not silently.
    let id = receiver.snapshot().transfers[0].id.clone();
    assert!(
        receiver
            .command(Command::AcceptTransfer {
                transfer: id,
                directory: directory.path().to_path_buf()
            })
            .await
            .is_err()
    );
    stop([(&sender, task_sender), (&receiver, task_receiver)]).await;
}

#[tokio::test]
async fn a_wait_changed_while_running_reaches_the_next_offer_with_no_restart() {
    let directory = tempfile::tempdir().unwrap();
    let (sender, task_sender) = start(&directory.path().join("sender")).await;
    // Started with the default of two minutes: left alone, this offer would wait far longer than the test does.
    let (receiver, task_receiver) = start(&directory.path().join("receiver")).await;
    assert_eq!(receiver.snapshot().settings.offer_timeout_seconds, 120);
    // The same patch the command line (`--offer-timeout`) and a window send.
    let cli =
        Cli::try_parse_from(["peersus", "--offer-timeout", "5", "settings", "--save"]).unwrap();
    cli.execute(&receiver).await.unwrap();
    assert_eq!(
        receiver.snapshot().settings.offer_timeout_seconds,
        5,
        "the change was not kept"
    );
    let payload = directory.path().join("later.bin");
    std::fs::write(&payload, vec![3; 10_000]).unwrap();
    let Reply::FilesInvitation { code, .. } = sender
        .command(Command::CreateFilesInvitation {
            paths: vec![payload],
        })
        .await
        .unwrap()
    else {
        panic!("Expected a files code")
    };
    receiver
        .command(Command::JoinInvitation { invitation: code })
        .await
        .unwrap();
    until(&receiver, |s| {
        s.transfers
            .iter()
            .any(|t| t.incoming && t.state == TransferState::Offered)
    })
    .await;
    let before = std::time::Instant::now();
    until(&receiver, |s| {
        s.transfers
            .iter()
            .all(|t| t.state == TransferState::Cancelled)
    })
    .await;
    assert!(
        before.elapsed() < Duration::from_secs(9),
        "the offer kept the old wait of two minutes"
    );
    // Out of range is refused in words and changes nothing.
    let refused = Cli::try_parse_from(["peersus", "--offer-timeout", "1", "settings", "--save"])
        .unwrap()
        .execute(&receiver)
        .await;
    assert_eq!(
        refused.err(),
        Some(peersus_core::SettingsError::OfferTimeout.to_string())
    );
    assert_eq!(receiver.snapshot().settings.offer_timeout_seconds, 5);
    stop([(&sender, task_sender), (&receiver, task_receiver)]).await;
}

#[tokio::test]
async fn clearing_the_history_forgets_finished_transfers_on_that_device_only_by_command_or_by_the_cli()
 {
    let directory = tempfile::tempdir().unwrap();
    let (sender, task_sender) = start(&directory.path().join("sender")).await;
    let (receiver, task_receiver) = start(&directory.path().join("receiver")).await;
    let payload = directory.path().join("note.bin");
    std::fs::write(&payload, vec![5; 20_000]).unwrap();
    let destination = directory.path().join("received");
    std::fs::create_dir(&destination).unwrap();
    let Reply::FilesInvitation { code, .. } = sender
        .command(Command::CreateFilesInvitation {
            paths: vec![payload.clone()],
        })
        .await
        .unwrap()
    else {
        panic!("Expected a files code")
    };
    receiver
        .command(Command::JoinInvitation { invitation: code })
        .await
        .unwrap();
    let offered = until(&receiver, |s| !s.transfers.is_empty()).await;
    receiver
        .command(Command::AcceptTransfer {
            transfer: offered.transfers[0].id.clone(),
            directory: destination.clone(),
        })
        .await
        .unwrap();
    for client in [&sender, &receiver] {
        until(client, |s| {
            !s.transfers.is_empty()
                && s.transfers
                    .iter()
                    .all(|t| t.state == TransferState::Complete)
        })
        .await;
    }

    // The sender made a code and it was scanned: it is still reported, spent, until the history that came of it is forgotten.
    assert!(
        sender.snapshot().files_code.is_some_and(|code| !code.live),
        "the code that was scanned was not reported as spent before Clear"
    );
    // The sender forgets what is over; the receiver's record is its own and is not touched.
    sender.command(Command::ClearHistory {}).await.unwrap();
    assert!(
        sender.snapshot().transfers.is_empty(),
        "a finished transfer survived Clear"
    );
    assert_eq!(
        sender.snapshot().files_code,
        None,
        "a code that was spent was still on show after Clear, so the circle fell back to scanned"
    );
    assert_eq!(
        receiver.snapshot().transfers.len(),
        1,
        "Clear on one device forgot the other's record"
    );
    // Only the record: the file that arrived is where it was put.
    assert_eq!(
        std::fs::read(destination.join("note.bin")).unwrap().len(),
        20_000
    );
    // Nothing over is not an error, and the same command from the command line does what the command does.
    sender.command(Command::ClearHistory {}).await.unwrap();
    Cli::try_parse_from(["peersus", "clear-history"])
        .unwrap()
        .execute(&receiver)
        .await
        .unwrap();
    assert!(
        receiver.snapshot().transfers.is_empty(),
        "the command line's clear-history did not clear"
    );
    stop([(&sender, task_sender), (&receiver, task_receiver)]).await;
}

#[cfg(unix)]
#[tokio::test]
async fn local_control_updates_the_running_owner_and_survives_malformed_clients() {
    use peersus_app::ipc;
    use tokio::{
        io::AsyncWriteExt,
        net::{UnixListener, UnixStream},
    };
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), Default::default()));
    let mut malformed = UnixStream::connect(&socket).await.unwrap();
    malformed.write_u32(u32::MAX).await.unwrap();
    drop(malformed);
    let mut settings = ipc::snapshot(&socket).await.unwrap().settings;
    settings.background = true;
    let remote = ipc::LocalControl::new(socket.clone());
    let output = Cli::try_parse_from(["peersus", "--background", "settings", "--save"])
        .unwrap()
        .execute(&remote)
        .await
        .unwrap();
    let peersus_app::cli::Output::Settings(saved) = output else {
        panic!("Expected saved preferences");
    };
    assert_eq!(*saved, settings);
    assert_eq!(client.snapshot().settings, settings);
    assert_eq!(
        SettingsStore::open(directory.path().join("preferences.json"))
            .unwrap()
            .current(),
        &settings
    );
    let mut invalid = settings.clone();
    invalid.scale = 0.0;
    assert!(
        ipc::command(
            &socket,
            Command::UpdateSettings {
                settings: Box::new(invalid),
                expected: Box::new(settings.clone())
            }
        )
        .await
        .is_err()
    );
    assert_eq!(ipc::snapshot(&socket).await.unwrap().settings, settings);
    assert!(matches!(
        ipc::command(&socket, Command::Shutdown {}).await.unwrap().0,
        Reply::Applied
    ));
    owner.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn attached_window_tracks_owner_and_closes_without_stopping_background_service() {
    use peersus_app::ipc;
    use peersus_core::config::SettingsPatch;
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), Default::default()));
    let (window, bridge) = ipc::attach(socket.clone()).await.unwrap();
    let background = |value| Command::PatchSettings {
        patch: Box::new(SettingsPatch {
            background: Some(value),
            ..Default::default()
        }),
    };
    window.command(background(true)).await.unwrap();
    assert!(client.snapshot().settings.background);
    // The window's copy follows the owner's on its own pace (a quiet command carries no snapshot),
    // so it is waited for, never read in the same instant.
    let mut changes = window.subscribe();
    tokio::time::timeout(
        Duration::from_secs(3),
        changes.wait_for(|s| s.settings.background),
    )
    .await
    .unwrap()
    .unwrap();
    client.command(background(false)).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(3),
        changes.wait_for(|s| !s.settings.background),
    )
    .await
    .unwrap()
    .unwrap();
    drop(window);
    tokio::time::timeout(Duration::from_secs(2), bridge)
        .await
        .unwrap()
        .unwrap();
    assert!(
        ipc::snapshot(&socket).await.is_ok(),
        "closing a window leaves the owner running"
    );
    let (_window, bridge) = ipc::attach(socket).await.unwrap();
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), bridge)
        .await
        .unwrap()
        .unwrap();
    server.await.unwrap().unwrap();
}

/// A door in front of the owner's socket, which a window is attached through. It counts the connections a
/// command came on, and with `keeps` off it is an owner that does not know a kept connection: one that asks
/// for it is closed with nothing said and nothing passed on, as such an owner does with a request it does not read.
#[cfg(unix)]
fn door(
    directory: &Path,
    owner: std::path::PathBuf,
    keeps: bool,
) -> (std::path::PathBuf, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let path = directory.join("door.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let commands = Arc::new(AtomicUsize::new(0));
    let counted = commands.clone();
    tokio::spawn(async move {
        while let Ok((mut window, _)) = listener.accept().await {
            let (owner, counted) = (owner.clone(), counted.clone());
            tokio::spawn(async move {
                let Ok(length) = window.read_u32().await else {
                    return;
                };
                let mut first = vec![0; length as usize];
                if window.read_exact(&mut first).await.is_err() {
                    return;
                }
                let request: serde_json::Value = serde_json::from_slice(&first).unwrap();
                if request["operation"] == "Command" {
                    counted.fetch_add(1, Ordering::SeqCst);
                    if !keeps && request.get("keep").is_some() {
                        return;
                    }
                }
                let mut behind = tokio::net::UnixStream::connect(&owner).await.unwrap();
                behind.write_u32(length).await.unwrap();
                behind.write_all(&first).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut window, &mut behind).await;
            });
        }
    });
    (path, commands)
}

#[cfg(unix)]
fn background(value: bool) -> Command {
    Command::PatchSettings {
        patch: Box::new(peersus_core::config::SettingsPatch {
            background: Some(value),
            ..Default::default()
        }),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_windows_commands_all_go_on_one_connection() {
    use peersus_app::ipc;
    use std::sync::atomic::Ordering;
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), Default::default()));
    let (door, commands) = door(directory.path(), socket, true);
    let (window, bridge) = ipc::attach(door).await.unwrap();

    // As a finger on a page does: one after another, each answered. The last one is seen at the owner, so
    // every one before it was carried in its turn.
    for turn in 0..40 {
        window.command(background(turn % 2 == 1)).await.unwrap();
    }
    assert!(client.snapshot().settings.background);
    assert_eq!(
        commands.load(Ordering::SeqCst),
        1,
        "a connection was made for more than the first command"
    );
    // What the owner refuses is said on the same connection, which stays good for the next command.
    let settings = client.snapshot().settings.clone();
    let mut invalid = settings.clone();
    invalid.scale = 0.0;
    assert!(
        window
            .command(Command::UpdateSettings {
                settings: Box::new(invalid),
                expected: Box::new(settings),
            })
            .await
            .is_err()
    );
    window.command(background(false)).await.unwrap();
    assert!(!client.snapshot().settings.background);
    assert_eq!(commands.load(Ordering::SeqCst), 1);

    drop(window);
    tokio::time::timeout(Duration::from_secs(2), bridge)
        .await
        .unwrap()
        .unwrap();
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("the control service waited for a kept connection")
        .unwrap()
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn an_owner_that_keeps_no_connection_is_asked_the_older_way_and_each_command_once() {
    use peersus_app::ipc;
    use std::sync::atomic::Ordering;
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), Default::default()));
    let (door, commands) = door(directory.path(), socket, false);
    let (window, _bridge) = ipc::attach(door).await.unwrap();

    window.command(background(true)).await.unwrap();
    assert!(client.snapshot().settings.background);
    // The one that was turned away, and the same command asked again the older way.
    assert_eq!(commands.load(Ordering::SeqCst), 2);
    // From then on the window does not ask to keep one again: a connection for each command, and no more.
    for turn in 0..6 {
        window.command(background(turn % 2 == 1)).await.unwrap();
    }
    assert!(client.snapshot().settings.background);
    assert_eq!(commands.load(Ordering::SeqCst), 8);

    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    server.await.unwrap().unwrap();
}

/// One command on a connection that asks to keep it, and what was answered: nothing, if the owner closed it.
#[cfg(unix)]
async fn kept(stream: &mut tokio::net::UnixStream, command: &Command) -> Option<serde_json::Value> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = serde_json::to_vec(&serde_json::json!({
        "operation": "Command", "command": command, "quiet": true, "keep": true,
    }))
    .unwrap();
    stream.write_u32(body.len() as u32).await.unwrap();
    stream.write_all(&body).await.unwrap();
    let length = tokio::time::timeout(Duration::from_secs(3), stream.read_u32())
        .await
        .expect("a kept connection was neither answered nor closed")
        .ok()?;
    let mut answer = vec![0; length as usize];
    stream.read_exact(&mut answer).await.unwrap();
    Some(serde_json::from_slice(&answer).unwrap())
}

#[cfg(unix)]
#[tokio::test]
async fn only_so_many_connections_are_kept_and_they_take_no_place_from_a_command_line_client() {
    use peersus_app::ipc;
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), Default::default()));
    let nothing = Command::PatchSettings {
        patch: Box::default(),
    };

    // As many as there can be windows, each kept open after its first answer.
    let mut held = Vec::new();
    for _ in 0..8 {
        let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let answer = kept(&mut stream, &nothing).await.expect("an answer");
        assert!(answer["result"].get("Ok").is_some(), "{answer}");
        assert!(
            answer.get("snapshot").is_none(),
            "a quiet answer carried the state"
        );
        held.push(stream);
    }
    // All but one of the places an ordinary request has are taken by clients that began to speak and stopped.
    let mut stalled = Vec::new();
    for _ in 0..15 {
        use tokio::io::AsyncWriteExt;
        let mut slow = tokio::net::UnixStream::connect(&socket).await.unwrap();
        slow.write_u32(10).await.unwrap();
        stalled.push(slow);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::timeout(Duration::from_secs(2), ipc::snapshot(&socket))
        .await
        .expect("a command line client waited behind connections that windows keep")
        .unwrap();
    drop(stalled);

    // One over is closed with nothing said, and what it asked was not done.
    let mut ninth = tokio::net::UnixStream::connect(&socket).await.unwrap();
    assert!(kept(&mut ninth, &background(true)).await.is_none());
    assert!(!client.snapshot().settings.background);
    // Those that are kept go on being answered, and a place given back is there for the next.
    assert!(kept(&mut held[3], &background(true)).await.is_some());
    assert!(client.snapshot().settings.background);
    held.pop();
    let mut next = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let until = tokio::time::Instant::now() + Duration::from_secs(3);
    while kept(&mut next, &nothing).await.is_none() {
        assert!(
            tokio::time::Instant::now() < until,
            "a place given back was not given again"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        next = tokio::net::UnixStream::connect(&socket).await.unwrap();
    }
    // Only a command that keeps the connection may follow on it: anything else ends it.
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let body = br#"{"operation":"Snapshot"}"#;
        next.write_u32(body.len() as u32).await.unwrap();
        next.write_all(body).await.unwrap();
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), next.read_to_end(&mut rest))
            .await
            .expect("a kept connection stayed open after a request that is not a command")
            .unwrap();
        assert!(
            rest.is_empty(),
            "a request that is not a command was answered"
        );
    }

    // The owner stopping does not wait for the windows that keep a connection to close it.
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("the control service waited for kept connections to close before it ended")
        .unwrap()
        .unwrap();
    drop(held);
}

#[cfg(unix)]
#[tokio::test]
async fn a_window_whose_owner_was_replaced_asks_the_new_one_and_asks_once() {
    use peersus_app::ipc;
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), Default::default()));
    let (window, _bridge) = ipc::attach(socket.clone()).await.unwrap();
    window.command(background(true)).await.unwrap();

    // The service ends under the window, closing what it held, and another takes its place at the same path.
    server.abort();
    let _ = server.await;
    std::fs::remove_file(&socket).unwrap();
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), Default::default()));
    // Time for the window's side to have been told that its connection was closed.
    tokio::time::sleep(Duration::from_millis(200)).await;
    window.command(background(false)).await.unwrap();
    assert!(!client.snapshot().settings.background);
    // And the window sees what the new one publishes, though it counts its states from the start again.
    let mut changes = window.subscribe();
    tokio::time::timeout(
        Duration::from_secs(3),
        changes.wait_for(|s| !s.settings.background),
    )
    .await
    .expect("the window did not see the state of the owner that took the place")
    .unwrap();

    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("the control service waited for a watch to run out before it ended")
        .unwrap()
        .unwrap();
}

/// A lease taken the way a window takes it: one request, one answer, and the connection kept open.
#[cfg(unix)]
async fn lease(socket: &std::path::Path) -> tokio::net::UnixStream {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
    let body = br#"{"operation":"Lease"}"#;
    stream.write_u32(body.len() as u32).await.unwrap();
    stream.write_all(body).await.unwrap();
    let length = stream.read_u32().await.unwrap() as usize;
    let mut answer = vec![0; length];
    stream.read_exact(&mut answer).await.unwrap();
    stream
}

/// Wait, for a while, until `windows()` is `expected`.
#[cfg(unix)]
async fn windows_become(presence: &peersus_app::lifetime::Presence, expected: u32) {
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    while presence.windows() != expected {
        assert!(
            tokio::time::Instant::now() < until,
            "{} windows were attached and {expected} were expected",
            presence.windows()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_lease_counts_as_a_window_for_as_long_as_it_is_held_and_not_after() {
    use peersus_app::{ipc, lifetime::Presence};
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let presence = Presence::default();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), presence.clone()));

    assert_eq!(presence.windows(), 0);
    assert!(!presence.ever_had_a_window());
    let first = lease(&socket).await;
    let second = lease(&socket).await;
    windows_become(&presence, 2).await;
    assert!(presence.ever_had_a_window());
    // One window goes: the other still counts.
    drop(first);
    windows_become(&presence, 1).await;
    drop(second);
    windows_become(&presence, 0).await;
    // Having had windows is remembered once they are all gone: that is what the owner ends on.
    assert!(presence.ever_had_a_window());

    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    server.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn an_ordinary_request_and_a_command_line_client_are_never_counted_as_a_window() {
    use peersus_app::{ipc, lifetime::Presence};
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let presence = Presence::default();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), presence.clone()));

    // What the command line does, over and over: a snapshot, a command.
    for _ in 0..5 {
        ipc::snapshot(&socket).await.unwrap();
    }
    ipc::command(
        &socket,
        Command::PatchSettings {
            patch: Box::default(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        presence.windows(),
        0,
        "a request that is not a lease was counted as a window"
    );
    assert!(
        !presence.ever_had_a_window(),
        "the command line made an owner think a window had been there"
    );

    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    server.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_the_owner_is_not_held_up_by_a_window_that_is_still_open() {
    use peersus_app::{ipc, lifetime::Presence};
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let presence = Presence::default();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), presence.clone()));

    // Two windows attached and staying so: `peersus stop` must not wait for them to close.
    let _a = lease(&socket).await;
    let _b = lease(&socket).await;
    windows_become(&presence, 2).await;
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("the control service waited for open windows to close before it ended")
        .unwrap()
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn windows_attached_never_leave_a_command_line_client_waiting_and_only_so_many_are_held() {
    use peersus_app::{ipc, lifetime::Presence};
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let presence = Presence::default();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), presence.clone()));

    // As many windows as are allowed, all held open.
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(lease(&socket).await);
    }
    windows_become(&presence, 8).await;
    // All but one of the places an ordinary request has are taken by clients that have begun to speak and
    // stopped: the deadline for them is far longer than this test. With the windows also open, the last place
    // is still there for a command line client, because the windows are not counted against the places.
    let mut stalled = Vec::new();
    for _ in 0..15 {
        use tokio::io::AsyncWriteExt;
        let mut slow = tokio::net::UnixStream::connect(&socket).await.unwrap();
        slow.write_u32(10).await.unwrap();
        stalled.push(slow);
    }
    // Time for the service to have taken them all.
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::timeout(Duration::from_secs(2), ipc::snapshot(&socket))
        .await
        .expect("a command line client waited behind windows that were attached")
        .unwrap();
    drop(stalled);
    // One over the limit is refused and not counted: it is not a window.
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut ninth = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let body = br#"{"operation":"Lease"}"#;
        ninth.write_u32(body.len() as u32).await.unwrap();
        ninth.write_all(body).await.unwrap();
        let length = ninth.read_u32().await.unwrap() as usize;
        let mut answer = vec![0; length];
        ninth.read_exact(&mut answer).await.unwrap();
        let text = String::from_utf8(answer).unwrap();
        assert!(
            text.contains("Too many windows"),
            "the ninth lease was not refused: {text}"
        );
        // And the connection is closed after the refusal, so it holds nothing.
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), ninth.read_to_end(&mut rest))
            .await
            .expect("a refused lease was kept open")
            .unwrap();
    }
    assert_eq!(presence.windows(), 8, "a refused lease was counted");
    drop(held);
    windows_become(&presence, 0).await;

    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    server.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_window_that_attaches_is_counted_while_it_lives_and_not_after_it_goes() {
    use peersus_app::{ipc, lifetime::Presence};
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let presence = Presence::default();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), presence.clone()));

    // Two windows attach, the way a window does: nothing here takes a lease by hand.
    let (first, first_bridge) = ipc::attach(socket.clone()).await.unwrap();
    let (second, second_bridge) = ipc::attach(socket.clone()).await.unwrap();
    windows_become(&presence, 2).await;
    // A window being open for a while, doing its requests every second, is still one window.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        presence.windows(),
        2,
        "the polling of an open window was counted as more windows, or let go"
    );
    // The first closes: its client is dropped and its task ends. The other is still open and still counted.
    drop(first);
    tokio::time::timeout(Duration::from_secs(3), first_bridge)
        .await
        .unwrap()
        .unwrap();
    windows_become(&presence, 1).await;
    drop(second);
    tokio::time::timeout(Duration::from_secs(3), second_bridge)
        .await
        .unwrap()
        .unwrap();
    windows_become(&presence, 0).await;

    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    server.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_window_learns_that_its_owner_was_told_to_stop_and_not_when_it_ended_any_other_way() {
    use peersus_app::{ipc, lifetime::Presence};
    // Told to stop: the window's bridge ends saying so, and the window closes with the application.
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let presence = Presence::default();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), presence.clone()));
    let (_window, bridge) = ipc::attach(socket.clone()).await.unwrap();
    windows_become(&presence, 1).await;
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    server.await.unwrap().unwrap();
    let told = tokio::time::timeout(Duration::from_secs(15), bridge)
        .await
        .expect("the window never saw its owner go")
        .unwrap();
    assert!(told, "an owner told to stop did not say so to its window");

    // Ended with nobody asking (here its task is cut off, as a crash cuts it): nothing is said, and the window
    // takes it as an owner to start again.
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let presence = Presence::default();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), presence.clone()));
    let (_window, bridge) = ipc::attach(socket.clone()).await.unwrap();
    windows_become(&presence, 1).await;
    owner.abort();
    let _ = owner.await;
    server.await.unwrap().unwrap();
    let told = tokio::time::timeout(Duration::from_secs(15), bridge)
        .await
        .expect("the window never saw its owner go")
        .unwrap();
    assert!(
        !told,
        "an owner that was not told to stop was taken as told"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_window_says_when_it_is_behind_and_a_surface_beside_it_is_never_a_window() {
    use peersus_app::{ipc, lifetime::Presence};
    use peersus_core::config::SettingsPatch;
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let presence = Presence::default();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), presence.clone()));
    let front_becomes = |wanted: bool| {
        let presence = presence.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(3), async {
                while presence.in_front() != wanted {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("the owner never saw a window in front become {wanted}"));
        }
    };

    assert!(!presence.in_front());
    let (window, bridge, front) = ipc::attach_window(socket.clone()).await.unwrap();
    windows_become(&presence, 1).await;
    assert!(
        presence.in_front(),
        "a window that has said nothing is in front"
    );
    front.tell(false);
    front_becomes(false).await;
    // The panel attaches beside the window: it reads the state and may command, and is counted as nothing.
    let (beside, beside_bridge) = ipc::attach_beside(socket.clone()).await.unwrap();
    assert_eq!(beside.snapshot().settings, client.snapshot().settings);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        presence.windows(),
        1,
        "a surface beside the window was counted as a window"
    );
    assert!(
        !presence.in_front(),
        "a surface beside the window was taken as a window in front"
    );
    front.tell(true);
    front_becomes(true).await;
    front.tell(false);
    front_becomes(false).await;
    // The window still commands on its own line while it says where it is.
    window
        .command(Command::PatchSettings {
            patch: Box::new(SettingsPatch {
                background: Some(true),
                ..Default::default()
            }),
        })
        .await
        .unwrap();
    assert!(client.snapshot().settings.background);
    // It goes while behind: nothing is left behind, and nothing in front.
    drop((window, front));
    tokio::time::timeout(Duration::from_secs(3), bridge)
        .await
        .unwrap()
        .unwrap();
    windows_become(&presence, 0).await;
    assert!(!presence.in_front());
    drop(beside);
    tokio::time::timeout(Duration::from_secs(3), beside_bridge)
        .await
        .unwrap()
        .unwrap();

    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    server.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_window_the_owner_has_no_room_to_count_is_not_made_and_is_made_when_there_is() {
    use peersus_app::{ipc, lifetime::Presence};
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let socket = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let presence = Presence::default();
    let server = tokio::spawn(ipc::serve(listener, client.clone(), presence.clone()));

    // As many windows as the owner counts.
    let mut windows = Vec::new();
    for _ in 0..8 {
        windows.push(ipc::attach(socket.clone()).await.unwrap());
    }
    windows_become(&presence, 8).await;
    // One more is not made at all: a window the owner does not count is a window whose owner may end under it
    // when the counted ones go. It is an error, and nothing was counted for it.
    let refused = ipc::attach(socket.clone()).await;
    assert!(
        refused.is_err(),
        "a window was made that the owner did not count"
    );
    assert_eq!(presence.windows(), 8);
    // One of the eight goes, and there is room: the same attach now works, and is counted.
    let (first, first_bridge) = windows.remove(0);
    drop(first);
    tokio::time::timeout(Duration::from_secs(3), first_bridge)
        .await
        .unwrap()
        .unwrap();
    windows_become(&presence, 7).await;
    let ninth = ipc::attach(socket.clone())
        .await
        .expect("there was room and the window was still refused");
    windows_become(&presence, 8).await;
    // The seven others go, and the ninth alone keeps the owner counted as having a window.
    for (client, bridge) in windows.drain(..) {
        drop(client);
        tokio::time::timeout(Duration::from_secs(3), bridge)
            .await
            .unwrap()
            .unwrap();
    }
    windows_become(&presence, 1).await;
    drop(ninth);

    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    server.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn an_owner_too_old_to_count_windows_still_lets_a_window_attach_and_is_not_ended_by_it() {
    use peersus_app::ipc;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{UnixListener, UnixStream},
    };
    let directory = tempfile::tempdir().unwrap();
    // A real owner, with its own socket.
    let (client, owner) = start(directory.path()).await;
    let real = directory.path().join("control.sock");
    let server = tokio::spawn(ipc::serve(
        UnixListener::bind(&real).unwrap(),
        client.clone(),
        Default::default(),
    ));
    // What stands between the window and it is an owner without leases: it reads a request, and one it
    // does not know it cannot make sense of, so it closes the connection. Everything else goes through to
    // the real owner and comes back as the real answer, so nothing here is built by hand.
    let old_socket = directory.path().join("old.sock");
    let listener = UnixListener::bind(&old_socket).unwrap();
    let through = real.clone();
    let old = tokio::spawn(async move {
        loop {
            let Ok((mut window, _)) = listener.accept().await else {
                return;
            };
            let through = through.clone();
            tokio::spawn(async move {
                let length = window.read_u32().await.unwrap() as usize;
                let mut request = vec![0; length];
                window.read_exact(&mut request).await.unwrap();
                if String::from_utf8_lossy(&request).contains("Lease") {
                    return; // does not know it: closes
                }
                let mut owner = UnixStream::connect(through).await.unwrap();
                owner.write_u32(length as u32).await.unwrap();
                owner.write_all(&request).await.unwrap();
                let length = owner.read_u32().await.unwrap() as usize;
                let mut answer = vec![0; length];
                owner.read_exact(&mut answer).await.unwrap();
                window.write_u32(length as u32).await.unwrap();
                window.write_all(&answer).await.unwrap();
            });
        }
    });
    let attached = tokio::time::timeout(Duration::from_secs(3), ipc::attach(old_socket)).await;
    let (window, bridge) = attached
        .expect("attach waited on an owner that does not know leases")
        .expect("a window could not attach to an owner that is too old to count windows");
    drop(window);
    tokio::time::timeout(Duration::from_secs(3), bridge)
        .await
        .unwrap()
        .unwrap();
    old.abort();
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    server.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn an_owner_that_answers_a_lease_with_something_unreadable_is_an_error_and_not_an_owner_that_is_too_old()
 {
    use peersus_app::ipc;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{UnixListener, UnixStream},
    };
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let real = directory.path().join("control.sock");
    let server = tokio::spawn(ipc::serve(
        UnixListener::bind(&real).unwrap(),
        client.clone(),
        Default::default(),
    ));
    // An owner of another version: the snapshot goes through to the real one and is answered as it is, but to a lease it
    // answers with a frame that is there and is not a response (what a changed `Response` looks like to this window).
    let other_socket = directory.path().join("other.sock");
    let listener = UnixListener::bind(&other_socket).unwrap();
    let through = real.clone();
    let other = tokio::spawn(async move {
        loop {
            let Ok((mut window, _)) = listener.accept().await else {
                return;
            };
            let through = through.clone();
            tokio::spawn(async move {
                let length = window.read_u32().await.unwrap() as usize;
                let mut request = vec![0; length];
                window.read_exact(&mut request).await.unwrap();
                if String::from_utf8_lossy(&request).contains("Lease") {
                    let body = br#"{"not":"a response"}"#;
                    window.write_u32(body.len() as u32).await.unwrap();
                    window.write_all(body).await.unwrap();
                    return;
                }
                let mut owner = UnixStream::connect(through).await.unwrap();
                owner.write_u32(length as u32).await.unwrap();
                owner.write_all(&request).await.unwrap();
                let length = owner.read_u32().await.unwrap() as usize;
                let mut answer = vec![0; length];
                owner.read_exact(&mut answer).await.unwrap();
                window.write_u32(length as u32).await.unwrap();
                window.write_all(&answer).await.unwrap();
            });
        }
    });
    let attached = tokio::time::timeout(Duration::from_secs(3), ipc::attach(other_socket))
        .await
        .expect("attach hung on an unreadable answer");
    let error = attached.err().expect("a window was made on an owner it does not understand: the owner would not count it, and would end under it");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    other.abort();
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn preference_patches_preserve_newer_network_policy_and_stale_editors_are_rejected() {
    use peersus_core::config::{BindChange, SettingsPatch};
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    let baseline = client.snapshot().settings.clone();
    let patch = |patch| {
        client.command(Command::PatchSettings {
            patch: Box::new(patch),
        })
    };
    patch(SettingsPatch {
        bind: Some(BindChange::Address("127.0.0.2".parse().unwrap())),
        allow_relay: Some(false),
        ..SettingsPatch::default()
    })
    .await
    .unwrap();
    patch(SettingsPatch {
        background: Some(true),
        ..SettingsPatch::default()
    })
    .await
    .unwrap();
    let merged = client.snapshot().settings.clone();
    assert!(merged.background && !merged.network.allow_relay);
    assert_eq!(merged.network.bind, Some("127.0.0.2".parse().unwrap()));
    let mut stale = baseline.clone();
    stale.scale = 1.5;
    assert!(
        client
            .command(Command::UpdateSettings {
                settings: Box::new(stale),
                expected: Box::new(baseline)
            })
            .await
            .is_err()
    );
    assert_eq!(client.snapshot().settings, merged);
    assert_eq!(
        SettingsStore::open(directory.path().join("preferences.json"))
            .unwrap()
            .current(),
        &merged
    );
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
}

#[tokio::test]
async fn turning_the_relays_on_from_the_switch_is_accepted_when_an_address_was_fixed_and_a_refusal_says_why()
 {
    use peersus_core::config::{BindChange, SettingsPatch};
    let directory = tempfile::tempdir().unwrap();
    let (client, owner) = start(directory.path()).await;
    // The state in the screenshot: routing fixed to one local address, so the relays are off.
    client
        .command(Command::PatchSettings {
            patch: Box::new(SettingsPatch {
                bind: Some(BindChange::Address("127.0.0.2".parse().unwrap())),
                allow_relay: Some(false),
                ..SettingsPatch::default()
            }),
        })
        .await
        .unwrap();
    // Relays on and nothing else: the owner will not hold both, and says which preference is the trouble, not
    // a phrase about "conflict" that tells the person nothing about what to change.
    let refused = match client
        .command(Command::PatchSettings {
            patch: Box::new(SettingsPatch {
                allow_relay: Some(true),
                ..SettingsPatch::default()
            }),
        })
        .await
    {
        Ok(_) => panic!("relays on beside a fixed address were accepted"),
        Err(refused) => refused,
    };
    assert!(
        refused.to_string().contains("relays"),
        "the refusal does not say what is wrong: {refused}"
    );
    assert!(
        !refused.to_string().contains("conflict"),
        "the refusal is the old phrase that says nothing: {refused}"
    );
    assert!(
        !client.snapshot().settings.network.allow_relay,
        "a refused edit changed the settings"
    );
    // What the switch sends: relays on, and routing returned to the system's (see `relay_patch` in the interface,
    // whose own test holds that this is what it builds). Accepted.
    client
        .command(Command::PatchSettings {
            patch: Box::new(SettingsPatch {
                allow_relay: Some(true),
                bind: Some(BindChange::Automatic),
                ..SettingsPatch::default()
            }),
        })
        .await
        .expect("the relay switch was refused with an address fixed");
    let after = client.snapshot().settings.clone();
    assert!(after.network.allow_relay, "the relays are not on");
    assert_eq!(
        after.network.bind, None,
        "routing was not returned to the system's"
    );
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
}
