//! Two hosts that really paired, and the profile picture each tells the other about.
//!
//! What this shows that the exchange's own tests do not: that it happens by itself when two devices
//! come online, that the picture arrives and is kept under the digest it was announced with, that a
//! second time online sends only the digest, and that changing the picture sends the new one.

use iroh::SecretKey;
use peersus_core::{config::NetworkSettings, model::PeerId, profile::Picture};
use peersus_net::{
    history::History,
    network_host::{Event, NetworkHost},
    peers::Peers,
    pictures::Pictures,
    transport,
};
use std::{path::Path, time::Duration};

fn picture(seed: u8) -> Picture {
    let rgba: Vec<u8> = (0..16 * 16 * 4).map(|n| (n as u8).wrapping_mul(3).wrapping_add(seed)).collect();
    Picture::new(16, 16, rgba).unwrap()
}

async fn drive(a: &mut NetworkHost, b: &mut NetworkHost, done: impl Fn(&NetworkHost, &NetworkHost) -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !done(a, b) {
            let event = tokio::select! { event = a.next(101) => event, event = b.next(101) => event };
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

/// A host that keeps its pictures under `directory`.
async fn host(directory: &Path, name: &str, key: SecretKey) -> NetworkHost {
    let policy = NetworkSettings { bind: Some("127.0.0.1".parse().unwrap()), allow_relay: false };
    let endpoint = transport::bind(key, &policy).await.unwrap();
    NetworkHost::new(
        endpoint,
        name.into(),
        Peers::open(directory.join(format!("peers-{name}.json"))).unwrap(),
        History::open(directory.join(format!("history-{name}.json"))).unwrap(),
    )
    .with_pictures(Pictures::new(&directory.join(format!("pictures-{name}"))))
}

/// Pair `a` with `b` and wait until each is online for the other.
async fn pair(a: &mut NetworkHost, b: &mut NetworkHost) {
    a.join(b.invite(100).unwrap()).unwrap();
    drive(a, b, |a, b| !a.pairing().is_empty() && !b.pairing().is_empty()).await;
    let pa = a.pairing().remove(0);
    let pb = b.pairing().remove(0);
    a.choose(&pa.id, pa.options[0]).unwrap();
    b.choose(&pb.id, pa.options[0]).unwrap();
    drive(a, b, |a, b| a.peers().iter().any(|p| p.online) && b.peers().iter().any(|p| p.online)).await;
}

#[tokio::test]
async fn a_picture_set_before_pairing_reaches_the_other_device_once_they_are_online() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b).await;
    let own = picture(1);
    a.set_picture(Some(own.clone())).unwrap();

    pair(&mut a, &mut b).await;
    drive(&mut a, &mut b, |_, b| b.peer_picture(&peer_a).is_some()).await;

    // B holds A's picture, and holds it under the digest A announced.
    assert_eq!(b.peer_picture(&peer_a).unwrap(), own);
    assert!(Pictures::new(&directory.path().join("pictures-b")).has(&own.digest()));
    // And B can say WHICH picture that is without producing it, which is all a snapshot carries.
    assert_eq!(b.peer_digests().get(&peer_a), Some(&own.digest()));
    // A, for its part, shows no picture for B, which never had one.
    assert!(a.peer_digests().is_empty());
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn coming_online_again_does_not_send_the_picture_again() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let mut a = host(directory.path(), "a", key_a.clone()).await;
    let mut b = host(directory.path(), "b", key_b).await;
    let own = picture(2);
    a.set_picture(Some(own.clone())).unwrap();
    pair(&mut a, &mut b).await;
    drive(&mut a, &mut b, |_, b| b.peer_picture(&peer_a).is_some()).await;
    assert_eq!(a.pictures_sent(), 1, "the first time, the picture is sent once");

    // A goes away and comes back, from what it keeps on disk: the same picture, the same device.
    a.close().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while b.peers().iter().any(|peer| peer.online) {
            b.next(102).await;
        }
    })
    .await
    .unwrap();
    let mut a = host(directory.path(), "a", key_a).await;
    assert_eq!(a.pictures_sent(), 0);
    drive(&mut a, &mut b, |a, b| {
        a.peers().iter().any(|p| p.online) && b.peers().iter().any(|p| p.online)
    })
    .await;
    // Coming online again with the same picture produces NO event, by design: a window is not woken for
    // a picture that did not change. So there is nothing to wait for; the hosts are run for a while, which is
    // enough for the announcement, the answer and the end of the exchange, and then what happened is read.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            tokio::select! { _ = a.next(101) => {}, _ = b.next(101) => {} }
        }
    })
    .await
    .unwrap_err();
    assert!(a.exchanges_finished() >= 1, "the announcement after coming online never finished");
    assert_eq!(a.pictures_sent(), 0, "the picture crossed again though B already held it");
    assert_eq!(b.peer_picture(&peer_a).unwrap(), own);
    a.close().await;
    b.close().await;
}

/// Like [`host`], and it also keeps which picture each device showed, in a file under `directory`.
async fn host_remembering(directory: &Path, name: &str, key: SecretKey) -> NetworkHost {
    host(directory, name, key)
        .await
        .with_picture_owners(peersus_net::picture_owners::PictureOwners::open(directory.join(format!("owners-{name}.json"))).unwrap())
}

#[tokio::test]
async fn a_device_shows_the_picture_it_showed_at_the_next_start_before_it_is_online() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host_remembering(directory.path(), "b", key_b.clone()).await;
    let own = picture(7);
    a.set_picture(Some(own.clone())).unwrap();
    pair(&mut a, &mut b).await;
    drive(&mut a, &mut b, |_, b| b.peer_picture(&peer_a).is_some()).await;
    // B stops, and A goes with it: when B starts again nobody is online, and B must still show A's face.
    b.close().await;
    a.close().await;
    let b = host_remembering(directory.path(), "b", key_b).await;
    assert!(b.peers().iter().all(|peer| !peer.online), "the test needs A offline");
    assert_eq!(b.peer_digests().get(&peer_a), Some(&own.digest()), "an offline device was drawn without the picture it showed");
    assert_eq!(b.peer_picture(&peer_a).as_ref(), Some(&own));
    b.close().await;
}

#[tokio::test]
async fn without_the_file_a_device_has_no_picture_until_it_is_online_as_it_was_before() {
    // The same, but B keeps no file of this: after a restart it knows no one's picture until they are online. This is the
    // behaviour that was there before, and it is what the test above is the difference from.
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b.clone()).await;
    a.set_picture(Some(picture(7))).unwrap();
    pair(&mut a, &mut b).await;
    drive(&mut a, &mut b, |_, b| b.peer_picture(&peer_a).is_some()).await;
    b.close().await;
    a.close().await;
    let b = host(directory.path(), "b", key_b).await;
    assert!(b.peer_digests().is_empty(), "a picture was known without a file that keeps it");
    b.close().await;
}

#[tokio::test]
async fn forgetting_a_device_drops_its_picture_from_memory_and_from_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host_remembering(directory.path(), "b", key_b.clone()).await;
    a.set_picture(Some(picture(9))).unwrap();
    pair(&mut a, &mut b).await;
    drive(&mut a, &mut b, |_, b| b.peer_picture(&peer_a).is_some()).await;
    b.forget(&peer_a).unwrap();
    assert!(!b.peer_digests().contains_key(&peer_a), "a forgotten device's picture was still reported");
    let on_disk = peersus_net::picture_owners::PictureOwners::open(directory.path().join("owners-b.json")).unwrap();
    assert!(!on_disk.all().contains_key(&peer_a), "a forgotten device's picture reference stayed in the file");
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn changing_the_picture_sends_only_the_new_one_and_removing_it_clears_it_on_the_other_side() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b).await;
    pair(&mut a, &mut b).await;

    let (first, second) = (picture(3), picture(4));
    a.set_picture(Some(first.clone())).unwrap();
    drive(&mut a, &mut b, |_, b| b.peer_picture(&peer_a).as_ref() == Some(&first)).await;
    assert_eq!(a.pictures_sent(), 1);

    a.set_picture(Some(second.clone())).unwrap();
    drive(&mut a, &mut b, |_, b| b.peer_picture(&peer_a).as_ref() == Some(&second)).await;
    assert_eq!(a.pictures_sent(), 2, "only the new picture was sent, once");

    // Back to the first, which B still holds: nothing is sent.
    a.set_picture(Some(first.clone())).unwrap();
    drive(&mut a, &mut b, |_, b| b.peer_picture(&peer_a).as_ref() == Some(&first)).await;
    assert_eq!(a.pictures_sent(), 2, "a picture B already held was sent again");

    // No picture at all: B is told, and shows none.
    a.set_picture(None).unwrap();
    drive(&mut a, &mut b, |_, b| b.peer_picture(&peer_a).is_none()).await;
    assert_eq!(a.pictures_sent(), 2);
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn a_damaged_own_picture_on_disk_is_not_quietly_taken_for_having_none() {
    let directory = tempfile::tempdir().unwrap();
    let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
    let peer_a = PeerId(key_a.public().to_string());
    // A picture was chosen once and is on disk...
    {
        let mut a = host(directory.path(), "a", key_a.clone()).await;
        a.set_picture(Some(picture(7))).unwrap();
        a.close().await;
    }
    // ... and then the file is damaged.
    std::fs::write(directory.path().join("pictures-a").join("profile-picture.json"), b"{ not a picture").unwrap();

    let mut a = host(directory.path(), "a", key_a).await;
    let mut b = host(directory.path(), "b", key_b).await;
    // The host says the picture is damaged, and holds none; it does not pretend all is well.
    assert!(a.own_picture().is_none());
    assert!(a.picture_problem(), "a damaged picture was not reported");

    // And it must NOT announce "no picture" to a device that comes online: that would tell B to forget
    // the picture A really has, because of a fault in A's own file.
    pair(&mut a, &mut b).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            tokio::select! { _ = a.next(101) => {}, _ = b.next(101) => {} }
        }
    })
    .await
    .unwrap_err();
    assert_eq!(a.exchanges_finished(), 0, "a host with a damaged picture announced something anyway");
    assert!(b.peer_picture(&peer_a).is_none());

    // Choosing a new picture mends it: the host is no longer in trouble, and the new picture is what
    // the other device is shown. A damaged file must not leave a person unable to pick another picture.
    let fresh = picture(8);
    a.set_picture(Some(fresh.clone())).unwrap();
    assert!(!a.picture_problem());
    drive(&mut a, &mut b, |_, b| b.peer_picture(&peer_a).as_ref() == Some(&fresh)).await;
    assert_eq!(a.pictures_sent(), 1);
    a.close().await;
    b.close().await;
}
