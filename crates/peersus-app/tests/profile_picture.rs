//! The profile picture, driven the way a window drives it: by commands to an owner, read back from its
//! snapshots, between two owners that really pair over the network.
//!
//! The picture itself is never in a snapshot, only its digest; a window asks for the pixels with
//! `FetchPicture` when it does not hold that digest. That is what is shown here, end to end.
// Speaks to owners over Unix sockets; the Windows owner is reached by named pipe and is not covered here.
#![cfg(unix)]

use iroh::SecretKey;
use peersus_app::runtime::{Client, Reply, Runtime};
use peersus_core::{
    Settings,
    config::NetworkSettings,
    model::{Command, PeerId, Snapshot},
    profile::{Digest, Picture},
};
use peersus_net::{
    history::History, network_host::NetworkHost, peers::Peers, pictures::Pictures,
    settings::SettingsStore, transport,
};
use std::{path::Path, sync::Arc, time::Duration};

fn picture(seed: u8) -> Picture {
    let rgba: Vec<u8> = (0..16 * 16 * 4)
        .map(|n| (n as u8).wrapping_mul(5).wrapping_add(seed))
        .collect();
    Picture::new(16, 16, rgba).unwrap()
}

/// A short private directory for the sockets of plugin processes.
fn sockets() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::Builder::new()
        .prefix("pp")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

/// An owner that keeps its state, pictures included, under `root/name`.
async fn start(
    root: &Path,
    name: &str,
    key: SecretKey,
) -> (Client, tokio::task::JoinHandle<()>, PeerId) {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
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
    )
    .with_pictures(Pictures::new(&path));
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
    let task = tokio::spawn(async move {
        runtime.run().await;
        drop(directory);
    });
    (client, task, id)
}

async fn until(client: &Client, predicate: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    let mut updates = client.subscribe();
    tokio::time::timeout(Duration::from_secs(15), async {
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

/// Pair `a` with `b` the way two people do: one makes an invitation, the other joins, both choose the
/// number that is shown.
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

async fn fetch(client: &Client, digest: Digest) -> Result<Picture, &'static str> {
    match client.command(Command::FetchPicture { digest }).await? {
        Reply::Picture(picture) => Ok(picture),
        _ => panic!("FetchPicture answered with something that is not a picture"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picture_that_is_set_is_held_by_the_owner_and_asked_for_by_its_digest() {
    let root = tempfile::tempdir().unwrap();
    let (client, _task, _) = start(root.path(), "alone", SecretKey::generate()).await;
    assert_eq!(client.snapshot().profile_picture, None);

    let own = picture(1);
    client
        .command(Command::SetProfilePicture {
            picture: own.clone(),
        })
        .await
        .unwrap();
    let snapshot = until(&client, |s| s.profile_picture.is_some()).await;
    assert_eq!(snapshot.profile_picture, Some(own.digest()));
    // The snapshot names the picture and does not carry it.
    assert!(!serde_json::to_string(&*snapshot).unwrap().contains("rgba"));
    // The window that does not hold that digest asks for it, once.
    assert_eq!(fetch(&client, own.digest()).await.unwrap(), own);

    // Another picture replaces it, and the old digest is no longer one the owner holds as its own.
    let other = picture(2);
    client
        .command(Command::SetProfilePicture {
            picture: other.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| s.profile_picture == Some(other.digest())).await;
    assert_eq!(fetch(&client, other.digest()).await.unwrap(), other);
    assert!(
        fetch(&client, own.digest()).await.is_err(),
        "a picture that is no longer anyone's was produced"
    );

    // Giving it up clears it.
    client
        .command(Command::ClearProfilePicture {})
        .await
        .unwrap();
    until(&client, |s| s.profile_picture.is_none()).await;
    assert!(fetch(&client, other.digest()).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_digest_the_owner_does_not_hold_is_refused_whoever_asks_and_however_it_looks() {
    let root = tempfile::tempdir().unwrap();
    let (client, _task, _) = start(root.path(), "alone", SecretKey::generate()).await;
    client
        .command(Command::SetProfilePicture {
            picture: picture(1),
        })
        .await
        .unwrap();
    until(&client, |s| s.profile_picture.is_some()).await;
    for stranger in [
        picture(9).digest(),
        Picture::digest_of_none(),
        Digest::from_bytes([0; 32]),
        Digest::from_bytes([255; 32]),
    ] {
        assert!(
            fetch(&client, stranger).await.is_err(),
            "{stranger} was answered"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_own_picture_is_still_there_when_the_owner_is_started_again() {
    let root = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();
    let own = picture(3);
    {
        let (client, task, _) = start(root.path(), "durable", key.clone()).await;
        client
            .command(Command::SetProfilePicture {
                picture: own.clone(),
            })
            .await
            .unwrap();
        until(&client, |s| s.profile_picture.is_some()).await;
        client.command(Command::Shutdown {}).await.unwrap();
        task.await.unwrap();
    }
    let (client, _task, _) = start(root.path(), "durable", key).await;
    let snapshot = until(&client, |s| s.profile_picture.is_some()).await;
    assert_eq!(snapshot.profile_picture, Some(own.digest()));
    assert_eq!(fetch(&client, own.digest()).await.unwrap(), own);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_owner_shows_the_other_ones_picture_by_digest_and_hands_it_over_when_asked() {
    let root = tempfile::tempdir().unwrap();
    let (a, _task_a, a_id) = start(root.path(), "a", SecretKey::generate()).await;
    let (b, _task_b, _) = start(root.path(), "b", SecretKey::generate()).await;
    let own = picture(4);
    a.command(Command::SetProfilePicture {
        picture: own.clone(),
    })
    .await
    .unwrap();
    pair(&a, &b).await;

    // B names A's picture in its snapshot, and can hand the pixels to its window when asked.
    let shown = until(&b, |s| s.peer_pictures.contains_key(&a_id)).await;
    assert_eq!(shown.peer_pictures[&a_id], own.digest());
    assert!(
        !serde_json::to_string(&*shown).unwrap().contains("rgba"),
        "pixels travelled in a snapshot"
    );
    assert_eq!(fetch(&b, own.digest()).await.unwrap(), own);
    // A, which showed its own picture, has nothing to show for B.
    assert!(a.snapshot().peer_pictures.is_empty());

    // A changes it: B follows, and the new one is what B hands over.
    let new = picture(5);
    a.command(Command::SetProfilePicture {
        picture: new.clone(),
    })
    .await
    .unwrap();
    until(&b, |s| s.peer_pictures.get(&a_id) == Some(&new.digest())).await;
    assert_eq!(fetch(&b, new.digest()).await.unwrap(), new);

    // A gives it up: B shows none for A any more.
    a.command(Command::ClearProfilePicture {}).await.unwrap();
    until(&b, |s| !s.peer_pictures.contains_key(&a_id)).await;
}

/// The largest picture there can be: the very command a window sends for a full-size photograph.
fn largest() -> Picture {
    use peersus_core::profile::MAX_EDGE;
    let rgba: Vec<u8> = (0..MAX_EDGE * MAX_EDGE * 4)
        .map(|n| (n as u8).wrapping_mul(13).wrapping_add((n >> 8) as u8))
        .collect();
    Picture::new(MAX_EDGE, MAX_EDGE, rgba).unwrap()
}

/// The window does not hold the owner's `Client`: it is attached to the owner by the local control socket,
/// and every command it sends, and every answer it gets, crosses that socket. So a picture set from a
/// window is judged by the socket's limits before the owner ever sees it.
#[tokio::test(flavor = "multi_thread")]
async fn a_full_size_picture_set_and_fetched_through_the_control_socket_as_a_window_does() {
    let root = tempfile::tempdir().unwrap();
    let (owner, _task, _) = start(root.path(), "owner", SecretKey::generate()).await;
    let socket = root.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let _serving = tokio::spawn(peersus_app::ipc::serve(
        listener,
        owner.clone(),
        Default::default(),
    ));
    let (window, _attached) = peersus_app::ipc::attach(socket).await.unwrap();

    let big = largest();
    // The command's own size: this is what the socket has to carry.
    let size = serde_json::to_vec(&Command::SetProfilePicture {
        picture: big.clone(),
    })
    .unwrap()
    .len();
    assert!(
        size > 128 * 1024,
        "the picture is not the largest there is, so this proves nothing ({size} bytes)"
    );

    window
        .command(Command::SetProfilePicture {
            picture: big.clone(),
        })
        .await
        .expect("a full-size picture was refused");
    let named = until(&window, |s| s.profile_picture == Some(big.digest())).await;
    assert_eq!(named.profile_picture, Some(big.digest()));
    // The pixels come back the same way: asked for by digest, over the same socket.
    assert_eq!(fetch(&window, big.digest()).await.unwrap(), big);
    // And the owner behind it holds exactly that picture.
    assert_eq!(owner.snapshot().profile_picture, Some(big.digest()));
}

/// A local request is judged by its size before it is read. It is as large as the largest command that
/// can be valid and no larger. The request here is a complete and valid command, a picture that would be
/// set if it were read, padded with blanks past the limit: only its size makes it refused.
#[tokio::test(flavor = "multi_thread")]
async fn the_control_socket_turns_away_a_valid_command_only_because_it_is_larger_than_any_can_be() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = tempfile::tempdir().unwrap();
    let (owner, _task, _) = start(root.path(), "owner", SecretKey::generate()).await;
    let socket = root.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let _serving = tokio::spawn(peersus_app::ipc::serve(
        listener,
        owner.clone(),
        Default::default(),
    ));

    let command = Command::SetProfilePicture {
        picture: picture(1),
    };
    let mut body =
        serde_json::to_vec(&serde_json::json!({ "operation": "Command", "command": command }))
            .unwrap();
    body.resize(peersus_app::ipc::REQUEST_LIMIT + 1, b' ');
    assert!(
        serde_json::from_slice::<serde_json::Value>(&body).is_ok(),
        "the padded request is not valid JSON"
    );

    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    stream.write_u32(body.len() as u32).await.unwrap();
    // The service refuses on the length alone and closes, so a client still writing may be told the
    // connection is gone: that is the refusal arriving, and not something to be asserted against.
    let _ = stream.write_all(&body).await;
    let mut answer = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut answer),
    )
    .await;
    assert_eq!(
        owner.snapshot().profile_picture,
        None,
        "a request past the limit was read and carried out"
    );

    // The same command, one blank shorter, is within the limit and is carried out: it was the size.
    body.pop();
    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    stream.write_u32(body.len() as u32).await.unwrap();
    stream.write_all(&body).await.unwrap();
    let mut answer = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut answer),
    )
    .await;
    assert_eq!(
        owner.snapshot().profile_picture,
        Some(picture(1).digest()),
        "the same command at the limit was refused"
    );
}
