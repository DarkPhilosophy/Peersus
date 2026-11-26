use super::*;
use tokio::io::{AsyncWriteExt, DuplexStream};

fn key(name: &str) -> Key {
    Key {
        plugin: PluginId::new("fixture").unwrap(),
        page: "page".into(),
        canvas: name.into(),
    }
}

fn declare(relay: &Relay, keys: &[Key]) {
    let key = &keys[0];
    relay.set_page(
        &key.plugin,
        &key.page,
        keys.iter().map(|key| key.canvas.clone()).collect(),
    );
}

fn writer(relay: &Relay, key: &Key) -> DuplexStream {
    let claim = relay
        .claim(key, Instant::now())
        .expect("canvas was watched");
    let (owner, guest) = tokio::io::duplex(64 * 1024);
    relay.accept(&key.plugin, claim, owner);
    guest
}

async fn header(guest: &mut DuplexStream, id: &str, seq: u64, layout: FrameLayout) {
    let mut wire = [0; MAX_FRAME_HEADER_BYTES];
    let bytes = FrameHeader {
        id,
        layout,
        seq,
        settled: seq * 2,
    }
    .encode(&mut wire)
    .unwrap();
    guest.write_all(bytes).await.unwrap();
}

async fn send(guest: &mut DuplexStream, key: &Key, seq: u64, layout: FrameLayout) {
    header(guest, &key.canvas, seq, layout).await;
    tokio::io::copy(
        &mut tokio::io::repeat(seq as u8).take(layout.byte_len() as u64),
        guest,
    )
    .await
    .unwrap();
}

async fn latest(view: &mut Watch, seq: u64) -> Arc<Frame> {
    let frame = tokio::time::timeout(
        READ_WAIT,
        view.latest
            .wait_for(|frame| frame.as_ref().is_some_and(|f| f.seq == seq)),
    )
    .await
    .unwrap()
    .unwrap();
    frame.as_ref().unwrap().clone()
}

fn retained(relay: &Relay, owner: &PluginId) -> usize {
    lock(&lock(&relay.0).memory.0)
        .get(owner)
        .copied()
        .unwrap_or(0)
}

#[tokio::test]
async fn claim_is_one_shot_and_cannot_retarget_the_extension_page_canvas() {
    let relay = Relay::default();
    let key = key("map");
    declare(&relay, std::slice::from_ref(&key));
    assert!(
        relay.claim(&key, Instant::now()).is_none(),
        "no mounted view"
    );
    let watched = relay.watch(key.clone()).unwrap();
    let claim = relay.claim(&key, Instant::now()).unwrap();
    let (owner, mut guest) = tokio::io::duplex(128);
    relay.accept(&PluginId::new("other").unwrap(), claim, owner);
    assert_eq!(guest.read(&mut [0]).await.unwrap(), 0);

    let (owner, mut guest) = tokio::io::duplex(128);
    relay.accept(&key.plugin, claim, owner);
    let (second, mut duplicate) = tokio::io::duplex(128);
    relay.accept(&key.plugin, claim, second);
    assert_eq!(duplicate.read(&mut [0]).await.unwrap(), 0);

    // Only a header is sent: a receiver that tried to read or allocate the body would stall.
    header(
        &mut guest,
        "other-canvas",
        1,
        FrameLayout::tight(4096, 2048).unwrap(),
    )
    .await;
    assert_eq!(timed(READ_WAIT, guest.read(&mut [0])).await.unwrap(), 0);
    assert!(watched.latest.borrow().is_none());
    assert_eq!(retained(&relay, &key.plugin), 0);
}

#[tokio::test]
async fn finished_frames_over_the_dimension_or_byte_limit_are_refused_at_the_header() {
    let relay = Relay::default();
    let key = key("map");
    declare(&relay, std::slice::from_ref(&key));
    let view = relay.watch(key.clone()).unwrap();
    let max = peersus_api::canvas::MAX_CANVAS_DIMENSION;
    for (width, height) in [(max + 1, 1), (max, max)] {
        let mut wire = [0; MAX_FRAME_HEADER_BYTES];
        let end = FrameHeader {
            id: &key.canvas,
            layout: FrameLayout::tight(1, 1).unwrap(),
            seq: 1,
            settled: 0,
        }
        .encode(&mut wire)
        .unwrap()
        .len();
        let start = FRAME_PREFIX_BYTES + key.canvas.len();
        for (field, value) in
            wire[start..start + 12]
                .chunks_exact_mut(4)
                .zip([width, height, width * 4])
        {
            field.copy_from_slice(&value.to_le_bytes());
        }
        // No payload: accepting the layout would read past the header instead of rejecting it.
        let error = relay
            .receive(&key, view._lease.epoch, &wire[..end])
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(view.latest.borrow().is_none());
    }
}

#[tokio::test]
async fn latest_slot_drops_stale_frames_and_last_view_closes_stream() {
    let relay = Relay::default();
    let key = key("map");
    declare(&relay, std::slice::from_ref(&key));
    let mut first = relay.watch(key.clone()).unwrap();
    let final_view = relay.watch(key.clone()).unwrap();
    let mut guest = writer(&relay, &key);
    let layout = FrameLayout::tight(2, 2).unwrap();
    send(&mut guest, &key, 2, layout).await;
    let old = latest(&mut first, 2).await;
    drop(first);
    // An out-of-order frame cannot replace frame 2 or stop the healthy connection.
    send(&mut guest, &key, 1, layout).await;
    send(&mut guest, &key, 3, layout).await;
    let mut final_view = final_view;
    let frame = latest(&mut final_view, 3).await;
    assert_eq!(
        (frame.seq, frame.settled, frame.pixels.as_ref()),
        (3, 6, &[3; 16][..])
    );
    assert_eq!(
        retained(&relay, &key.plugin),
        32,
        "in-flight old frame remains charged"
    );
    drop(old);
    drop(frame);
    assert_eq!(
        retained(&relay, &key.plugin),
        32,
        "retired raster is the spare"
    );
    drop(final_view);
    assert_eq!(timed(READ_WAIT, guest.read(&mut [0])).await.unwrap(), 0);
    assert_eq!(retained(&relay, &key.plugin), 0);
    assert!(relay.claim(&key, Instant::now()).is_none());
}

#[tokio::test]
async fn an_in_process_canvas_reuses_an_unheld_raster_until_unmounted() {
    let relay = Relay::default();
    let key = key("map");
    declare(&relay, std::slice::from_ref(&key));
    let mut view = relay.watch(key.clone()).unwrap();
    let mut guest = writer(&relay, &key);
    let layout = FrameLayout::tight(1024, 512).unwrap();
    send(&mut guest, &key, 1, layout).await;
    let first = latest(&mut view, 1).await.pixels.as_ref().as_ptr();
    send(&mut guest, &key, 2, layout).await;
    let second = latest(&mut view, 2).await.pixels.as_ref().as_ptr();
    assert_ne!(first, second);
    for seq in 3..=32 {
        send(&mut guest, &key, seq, layout).await;
        let frame = latest(&mut view, seq).await;
        assert_eq!(
            frame.pixels.as_ref().as_ptr(),
            if seq % 2 == 0 { second } else { first }
        );
        assert_eq!(frame.pixels.as_ref()[0], seq as u8);
        assert_eq!(retained(&relay, &key.plugin), 2 * layout.byte_len());
    }
    // Unmount during a payload as well as after complete frames. No partial frame can stay
    // in the slot, and cancelling the reader must release its in-flight allocation.
    header(&mut guest, &key.canvas, 33, layout).await;
    guest.write_all(&[33; 4]).await.unwrap();
    drop(view);
    assert_eq!(timed(READ_WAIT, guest.read(&mut [0])).await.unwrap(), 0);
    assert_eq!(retained(&relay, &key.plugin), 0);
    assert!(relay.claim(&key, Instant::now()).is_none());
}

#[tokio::test]
async fn an_in_process_canvas_keeps_two_rasters_when_the_compositor_lags() {
    let relay = Relay::default();
    let key = key("map");
    declare(&relay, std::slice::from_ref(&key));
    let mut view = relay.watch(key.clone()).unwrap();
    let mut guest = writer(&relay, &key);
    // The payload exceeds the duplex socket buffer, so a send must make progress in the owner.
    let layout = FrameLayout::tight(512, 128).unwrap();
    send(&mut guest, &key, 1, layout).await;
    let first = latest(&mut view, 1).await;
    send(&mut guest, &key, 2, layout).await;
    let second = latest(&mut view, 2).await;
    let storage = first.pixels.as_ref().as_ptr();
    {
        let pending = send(&mut guest, &key, 3, layout);
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut pending)
                .await
                .is_err()
        );
        assert_eq!(retained(&relay, &key.plugin), 2 * layout.byte_len());
        assert_eq!(view.latest.borrow().as_ref().unwrap().seq, 2);
        assert_eq!(first.pixels.as_ref()[0], 1);
        assert_eq!(second.pixels.as_ref()[0], 2);
        drop(first);
        pending.await;
    }
    let third = latest(&mut view, 3).await;
    assert_eq!(third.pixels.as_ref().as_ptr(), storage);
    assert_eq!(third.pixels.as_ref()[0], 3);
    let storage = second.pixels.as_ref().as_ptr();
    drop(second);
    drop(third);
    send(&mut guest, &key, 4, layout).await;
    assert_eq!(latest(&mut view, 4).await.pixels.as_ref().as_ptr(), storage);
    drop(view);
    assert_eq!(timed(READ_WAIT, guest.read(&mut [0])).await.unwrap(), 0);
    assert_eq!(retained(&relay, &key.plugin), 0);
}

#[tokio::test]
async fn a_single_extension_cannot_fill_more_than_four_slots_or_sixty_four_mib() {
    let relay = Relay::default();
    let keys: Vec<_> = (0..=MAX_CANVASES_PER_EXTENSION)
        .map(|i| key(&format!("s-{i}")))
        .collect();
    declare(&relay, &keys);
    let layout = FrameLayout::tight(4096, 1536).unwrap(); // 24 MiB
    let mut views = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        if i == MAX_CANVASES_PER_EXTENSION {
            assert!(relay.watch(key.clone()).is_none());
            break;
        }
        let mut view = relay.watch(key.clone()).unwrap();
        let mut guest = writer(&relay, key);
        send(&mut guest, key, 1, layout).await;
        guest.shutdown().await.unwrap();
        assert_eq!(timed(READ_WAIT, guest.read(&mut [0])).await.unwrap(), 0);
        if i < 2 {
            assert_eq!(
                latest(&mut view, 1).await.pixels.as_ref().len(),
                layout.byte_len()
            );
        } else {
            assert!(
                view.latest.borrow().is_none(),
                "cap applies before retaining this payload"
            );
        }
        views.push(view);
    }
    assert_eq!(retained(&relay, &keys[0].plugin), 2 * layout.byte_len());
    assert!(relay.forget(&keys[0].plugin));
    assert!(views.iter().all(|view| view.latest.borrow().is_none()));
    assert_eq!(retained(&relay, &keys[0].plugin), 0);
}

#[tokio::test]
async fn rate_limited_frames_are_dropped_without_losing_the_canvas_connection() {
    let relay = Relay::default();
    let key = key("map");
    declare(&relay, std::slice::from_ref(&key));
    let mut view = relay.watch(key.clone()).unwrap();
    let mut guest = writer(&relay, &key);
    let started = Instant::now() - Duration::from_millis(500);
    {
        let mut state = lock(&relay.0);
        let traffic = state.traffic.entry(key.plugin.clone()).or_default();
        for _ in 0..MAX_CANVAS_FPS {
            assert!(traffic.admit(started, 4));
        }
    }
    let layout = FrameLayout::tight(1, 1).unwrap();
    send(&mut guest, &key, 1, layout).await; // Must wait for headroom, then drop.
    send(&mut guest, &key, 2, layout).await;
    latest(&mut view, 2).await;
}

#[tokio::test]
async fn the_desktop_route_owns_no_raster_and_hands_the_canvas_to_the_next_window() {
    let relay = Relay::default();
    let key = key("map");
    declare(&relay, std::slice::from_ref(&key));
    let root = tempfile::tempdir().unwrap();
    let listener = peersus_plugin::transport::Listener::bind(&root.path().join("route")).unwrap();
    let endpoint = listener.endpoint().to_str().unwrap().to_owned();
    let Ok(first) = relay.mount_direct(key.clone(), endpoint.clone()) else {
        panic!("first view refused");
    };
    assert!(matches!(
        relay.mount_direct(key.clone(), endpoint.clone()),
        Err(MountError::Busy)
    ));
    assert!(relay.watch(key.clone()).is_none());
    let claim = relay.claim(&key, Instant::now()).unwrap();
    let (owner, mut foreign) = tokio::io::duplex(128);
    relay.accept(&PluginId::new("other").unwrap(), claim, owner);
    assert_eq!(foreign.read(&mut [0; 1]).await.unwrap(), 0);
    let (owner, mut guest) = tokio::io::duplex(512);
    relay.accept(&key.plugin, claim, owner);
    let mut redirect = Vec::new();
    timed(READ_WAIT, guest.read_to_end(&mut redirect))
        .await
        .unwrap();
    assert!(redirect.ends_with(&first.token));
    assert!(
        relay.claim(&key, Instant::now()).is_none(),
        "one-use redirect"
    );
    assert_eq!(retained(&relay, &key.plugin), 0);
    drop(first);
    let Ok(next) = relay.mount_direct(key.clone(), endpoint) else {
        panic!("replacement view refused");
    };
    let second_claim = relay.claim(&key, Instant::now()).unwrap();
    assert_ne!(second_claim, claim);
    assert!(relay.forget(&key.plugin));
    assert!(
        next.closed.await.is_err(),
        "removal revokes the window lease"
    );
}

#[test]
fn extension_traffic_is_counted_in_one_shared_rolling_window() {
    let now = Instant::now();
    let mut frames = Traffic::default();
    for _ in 0..MAX_CANVAS_FPS {
        assert!(frames.admit(now, 4));
    }
    assert!(!frames.admit(now + Duration::from_millis(999), 4));
    assert!(frames.admit(now + Duration::from_secs(1), 4));
    let mut bytes = Traffic::default();
    assert!(bytes.admit(now, MAX_CANVAS_BYTES_PER_SECOND - 1));
    assert!(!bytes.admit(now, 2));
    assert!(bytes.admit(now, 1));
}
