//! A folder sent from one host to another that are really paired: every file arrives, in the folders it sat in, however many there are.
//!
//! What this shows that the pieces' own tests do not: that a folder with more files than a connection carries at once is sent all
//! the same (the files wait for room and go as it comes), that nothing is held as a list or kept open while it waits, and that
//! the other side ends with the same tree.

use iroh::SecretKey;
use peersus_core::{
    config::NetworkSettings,
    model::{PeerId, TransferState},
};
use peersus_net::{
    history::History,
    network_host::{Event, NetworkHost},
    peers::Peers,
    transport,
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

struct Paired {
    directory: tempfile::TempDir,
    sender: NetworkHost,
    receiver: NetworkHost,
    receiver_id: PeerId,
}

/// Pump both hosts until `done`, offering the next file of any folder as room comes. The receiver answers NOTHING until the sender has
/// filled every place it has (so that the files past that number have to WAIT, which is what is being shown), and from then on it
/// accepts every file it is offered into `into` as it appears. Returns the most transfers the sender had going at one moment.
/// Fails after a time: a folder that stops going is a failure of this.
async fn run(pair: &mut Paired, into: &Path, mut done: impl FnMut(&NetworkHost, &NetworkHost) -> bool) -> usize {
    let mut most = 0;
    let mut answering = false;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            // One more file of the folder goes out if there is room; asked until it says there is nothing to offer now.
            while pair.sender.feed_folders(101).await {}
            let going = pair.sender.transfers().iter().filter(|t| t.state.is_active()).count();
            most = most.max(going);
            // Once the sender has no more room, or its folder has no more files to offer, the receiver begins to answer.
            if going >= 16 || pair.sender.folder_state().iter().all(|(_, _, _, _, done)| *done) {
                answering = true;
            }
            if answering {
                let waiting: Vec<String> = pair
                    .receiver
                    .transfers()
                    .into_iter()
                    .filter(|t| t.incoming && t.state == TransferState::Offered)
                    .map(|t| t.id)
                    .collect();
                for id in waiting {
                    pair.receiver.accept(&id, into.to_owned()).unwrap();
                }
            }
            if done(&pair.sender, &pair.receiver) {
                return;
            }
            let event = tokio::select! {
                event = pair.sender.next(101) => event,
                event = pair.receiver.next(101) => event,
                // The hosts are quiet while a file goes; look again soon.
                () = tokio::time::sleep(Duration::from_millis(50)) => Event::Changed,
            };
            match event {
                Event::Changed => {}
                Event::Error(reason) => panic!("Unexpected host failure: {reason}"),
                Event::Plugin { .. } => panic!("No plugin stream expected"),
                Event::Picture { .. } => panic!("No picture expected"),
            }
        }
    })
    .await
    .expect("the folder did not finish in time");
    most
}

async fn paired() -> Paired {
    let directory = tempfile::tempdir().unwrap();
    let policy = NetworkSettings { bind: Some("127.0.0.1".parse().unwrap()), allow_relay: false };
    let endpoint_a = transport::bind(SecretKey::generate(), &policy).await.unwrap();
    let endpoint_b = transport::bind(SecretKey::generate(), &policy).await.unwrap();
    let receiver_id = PeerId(endpoint_b.id().to_string());
    let mut sender = NetworkHost::new(
        endpoint_a,
        "Sender".into(),
        Peers::open(directory.path().join("peers-a.json")).unwrap(),
        History::open(directory.path().join("history-a.json")).unwrap(),
    );
    let mut receiver = NetworkHost::new(
        endpoint_b,
        "Receiver".into(),
        Peers::open(directory.path().join("peers-b.json")).unwrap(),
        History::open(directory.path().join("history-b.json")).unwrap(),
    );
    sender.join(receiver.invite(100).unwrap()).unwrap();
    pump(&mut sender, &mut receiver, |a, b| !a.pairing().is_empty() && !b.pairing().is_empty()).await;
    let (pa, pb) = (sender.pairing().remove(0), receiver.pairing().remove(0));
    sender.choose(&pa.id, pa.options[0]).unwrap();
    receiver.choose(&pb.id, pa.options[0]).unwrap();
    pump(&mut sender, &mut receiver, |a, b| a.peers().iter().any(|p| p.online) && b.peers().iter().any(|p| p.online)).await;
    Paired { directory, sender, receiver, receiver_id }
}

async fn pump(a: &mut NetworkHost, b: &mut NetworkHost, done: impl Fn(&NetworkHost, &NetworkHost) -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !done(a, b) {
            let event = tokio::select! { event = a.next(101) => event, event = b.next(101) => event };
            if let Event::Error(reason) = event {
                panic!("Unexpected host failure: {reason}");
            }
        }
    })
    .await
    .unwrap();
}

/// A tree of `count` files, spread over `pictures`, `pictures/2024` and `pictures/2024/summer`, each with content of its own.
fn make_tree(base: &Path, count: usize) -> (PathBuf, Vec<(PathBuf, Vec<u8>)>) {
    let root = base.join("pictures");
    let places = [root.clone(), root.join("2024"), root.join("2024").join("summer")];
    for place in &places {
        std::fs::create_dir_all(place).unwrap();
    }
    let mut expected = Vec::new();
    for n in 0..count {
        let place = &places[n % places.len()];
        let bytes = format!("file number {n} ").repeat(1 + n % 7).into_bytes();
        let path = place.join(format!("f{n:03}.txt"));
        std::fs::write(&path, &bytes).unwrap();
        expected.push((path.strip_prefix(base).unwrap().to_owned(), bytes));
    }
    (root, expected)
}

#[tokio::test]
async fn a_folder_of_more_files_than_go_at_once_arrives_whole_in_the_folders_it_sat_in() {
    let mut pair = paired().await;
    // Forty files, more than the sixteen a connection carries at once, in three levels of folders.
    let (root, expected) = make_tree(pair.directory.path(), 40);
    let received = pair.directory.path().join("received");
    std::fs::create_dir(&received).unwrap();
    let id = pair.sender.send_folder(&pair.receiver_id, &root).unwrap();
    let into = received.clone();
    let most = run(&mut pair, &into, |sender, receiver| {
        let state = sender.folder_state();
        let finished = state.iter().all(|(_, _, _, _, done)| *done);
        finished
            && receiver.transfers().len() == 40
            && receiver.transfers().iter().all(|t| t.state == TransferState::Complete)
            && sender.transfers().iter().all(|t| t.state == TransferState::Complete)
    })
    .await;
    // The limit was FILLED (sixteen went at once, so the other twenty-four had to wait their turn) and never passed.
    assert_eq!(most, 16, "the files did not wait for room: {most} were going at once");
    // Every file is there, with its own content, under the folders it sat in, and the chosen folder is the top.
    for (relative, bytes) in &expected {
        let arrived = std::fs::read(received.join(relative)).unwrap_or_else(|_| panic!("{} did not arrive", relative.display()));
        assert_eq!(&arrived, bytes, "{} arrived with other content", relative.display());
    }
    let state = pair.sender.folder_state();
    let (_, _, sent, passed, done) = state.iter().find(|(folder, ..)| *folder == id).cloned().unwrap();
    assert!(done);
    assert_eq!((sent.started, sent.failed), (40, 0), "a file was lost or failed");
    assert_eq!((passed.not_plain, passed.unreadable, passed.too_deep), (0, 0, 0));
    // Nothing is left half written in what was received: no staging file stayed behind.
    fn leftovers(folder: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(folder).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                leftovers(&path, found);
            } else if path.file_name().is_some_and(|name| name.to_string_lossy().ends_with(".part")) {
                found.push(path);
            }
        }
    }
    let mut stray = Vec::new();
    leftovers(&received, &mut stray);
    assert!(stray.is_empty(), "staging files were left behind: {stray:?}");
    pair.sender.close().await;
    pair.receiver.close().await;
}

#[tokio::test]
async fn a_folder_sent_to_a_device_that_is_not_paired_or_not_a_folder_is_refused_at_once() {
    let mut pair = paired().await;
    let (root, _) = make_tree(pair.directory.path(), 3);
    // A device that is not there.
    let stranger = PeerId("not-a-device".into());
    assert!(pair.sender.send_folder(&stranger, &root).is_err(), "a folder was begun for a device that is not paired");
    // A file is not a folder to send.
    assert!(pair.sender.send_folder(&pair.receiver_id, &root.join("f000.txt")).is_err(), "a file was taken as a folder");
    assert!(pair.sender.folder_state().is_empty(), "a refused folder was kept");
    pair.sender.close().await;
    pair.receiver.close().await;
}
