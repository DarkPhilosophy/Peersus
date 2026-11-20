use super::*;
use peersus_api::canvas::{FRAME_PREFIX_BYTES, FrameHeader, FrameLayout, MAX_CANVAS_FPS};
use peersus_core::plugins::PluginId;
use std::{path::PathBuf, time::Instant};
use tokio::{net::UnixListener, task::JoinHandle};

fn key(canvas: &str) -> FrameKey {
    FrameKey {
        plugin: PluginId::new("fixture").unwrap(),
        page: "page".into(),
        canvas: canvas.into(),
    }
}

struct Fixture {
    reader: IpcReader,
    relay: Relay,
    server: JoinHandle<io::Result<()>>,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn open() -> Self {
        let key = key("map");
        let relay = Relay::default();
        relay.set_page(&key.plugin, &key.page, vec![key.canvas.clone()]);
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        let path = directory.path().join("ipc");
        let listener = UnixListener::bind(&path).unwrap();
        let mut reader = IpcReader::new(Window::new(path), key.clone());
        let server = tokio::spawn({
            let relay = relay.clone();
            async move {
                let (mut stream, _) = listener.accept().await?;
                let request =
                    super::super::read_frame::<Request>(&mut stream, REQUEST_LIMIT).await?;
                let Request::MountCanvas {
                    plugin,
                    page,
                    canvas,
                    endpoint,
                } = request
                else {
                    panic!("expected mount request")
                };
                assert_eq!(
                    FrameKey {
                        plugin,
                        page,
                        canvas
                    },
                    key
                );
                serve(&mut stream, relay.mount_direct(key, endpoint)).await
            }
        });
        assert!(matches!(reader.next().await.unwrap(), FramePoll::Empty));
        Self {
            reader,
            relay,
            server,
            _directory: directory,
        }
    }

    async fn writer(&self) -> transport::Stream {
        let key = &self.reader.key;
        let claim = self.relay.claim(key, Instant::now()).unwrap();
        let (owner, guest) = std::os::unix::net::UnixStream::pair().unwrap();
        owner.set_nonblocking(true).unwrap();
        let owner = transport::Stream::from_std(owner).unwrap();
        self.relay.accept(&key.plugin, claim, owner);
        let guest = tokio::task::spawn_blocking(|| canvas::Redirect::follow(guest))
            .await
            .unwrap()
            .unwrap();
        guest.set_nonblocking(true).unwrap();
        transport::Stream::from_std(guest).unwrap()
    }
}

async fn present(stream: &mut transport::Stream, header: FrameHeader<'_>, body: &[u8]) {
    let mut wire = [0; MAX_FRAME_HEADER_BYTES];
    stream
        .write_all(header.encode(&mut wire).unwrap())
        .await
        .unwrap();
    stream.write_all(body).await.unwrap();
}

#[tokio::test]
async fn the_direct_window_reuses_two_rasters_and_releases_them_on_unmount() {
    let fixture = Fixture::open().await;
    let mut guest = fixture.writer().await;
    let Fixture {
        mut reader,
        relay,
        server,
        _directory,
    } = fixture;
    let key = reader.key.clone();
    assert!(relay.claim(&key, Instant::now()).is_none());
    let header = FrameHeader {
        id: &key.canvas,
        layout: FrameLayout::new(1, 2, 8).unwrap(),
        seq: 1,
        settled: 4,
    };
    present(
        &mut guest,
        header,
        &[3, 4, 0, 255, 9, 9, 9, 9, 2, 9, 0, 128, 8, 8, 8, 8],
    )
    .await;
    let FramePoll::Frame(first) = reader.next().await.unwrap() else {
        panic!("missing frame")
    };
    assert_eq!(
        (first.width, first.height, first.seq, first.settled),
        (1, 2, 1, 4)
    );
    assert_eq!(first.pixels.as_ref(), &[3, 4, 0, 255, 2, 9, 0, 128]);
    present(&mut guest, FrameHeader { seq: 2, ..header }, &[2; 16]).await;
    let FramePoll::Frame(second) = reader.next().await.unwrap() else {
        panic!("missing frame")
    };
    let addresses = [
        first.pixels.as_ref().as_ptr(),
        second.pixels.as_ref().as_ptr(),
    ];
    present(&mut guest, FrameHeader { seq: 3, ..header }, &[3; 16]).await;
    let mut pending = Box::pin(reader.next());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err()
    );
    drop(first);
    let FramePoll::Frame(mut latest) = pending.await.unwrap() else {
        panic!("missing reused frame")
    };
    assert_eq!(latest.pixels.as_ref().as_ptr(), addresses[0]);
    drop(second);
    for seq in 4..=40 {
        present(&mut guest, FrameHeader { seq, ..header }, &[seq as u8; 16]).await;
        let FramePoll::Frame(frame) = reader.next().await.unwrap() else {
            panic!("missing replacement")
        };
        assert_eq!(frame.seq, seq);
        assert_eq!(frame.pixels.as_ref(), &[seq as u8; 8]);
        assert_eq!(
            frame.pixels.as_ref().as_ptr(),
            addresses[(seq as usize - 1) % 2]
        );
        latest = frame;
    }
    assert_eq!(latest.seq, 40);
    let memory = reader.source.memory.clone();
    assert_eq!(memory.used(&key.plugin), 2 * header.layout.byte_len());
    drop(reader);
    assert_eq!(memory.used(&key.plugin), header.layout.byte_len());
    drop(latest);
    assert_eq!(memory.used(&key.plugin), 0);
    timed(IO, async { server.await.unwrap() }).await.unwrap();
    assert_eq!(guest.read(&mut [0]).await.unwrap(), 0);
    assert!(relay.claim(&key, Instant::now()).is_none());
}

#[tokio::test]
async fn a_consumed_redirect_without_a_matching_connection_releases_the_slot() {
    let mut fixture = Fixture::open().await;
    let key = fixture.reader.key.clone();
    let claim = fixture.relay.claim(&key, Instant::now()).unwrap();
    let (owner, mut unused) = tokio::io::duplex(512);
    fixture.relay.accept(&key.plugin, claim, owner);
    let mut redirected = Vec::new();
    timed(IO, unused.read_to_end(&mut redirected))
        .await
        .unwrap();
    assert!(fixture.relay.claim(&key, Instant::now()).is_none());

    let Some(Line {
        pixels: PixelLine::Waiting {
            listener, token, ..
        },
        ..
    }) = &fixture.reader.line
    else {
        panic!("expected an unconnected listener");
    };
    let mut unrelated = transport::connect(listener.endpoint()).await.unwrap();
    let mut bad_token = *token;
    bad_token[0] ^= 1;
    unrelated.write_all(b"PSC1").await.unwrap();
    unrelated.write_all(&bad_token).await.unwrap();
    assert!(matches!(
        fixture.reader.next().await.unwrap(),
        FramePoll::Closed
    ));
    fixture.server.await.unwrap().unwrap();
    assert_eq!(unrelated.read(&mut [0]).await.unwrap(), 0);
    assert!(fixture.relay.claim(&key, Instant::now()).is_none());

    let listener =
        transport::Listener::bind(&fixture._directory.path().join("replacement")).unwrap();
    let endpoint = listener.endpoint().to_str().unwrap().to_owned();
    let mounted = fixture.relay.mount_direct(key.clone(), endpoint);
    assert!(
        mounted.is_ok(),
        "the failed redirect must not block a fresh view"
    );
    assert_ne!(fixture.relay.claim(&key, Instant::now()), Some(claim));
}

#[tokio::test]
async fn the_window_refuses_a_wrong_canvas_or_oversized_header_without_reading_a_body() {
    for (wrong_id, oversized) in [(true, false), (false, true)] {
        let mut fixture = Fixture::open().await;
        let mut guest = fixture.writer().await;
        let id = if wrong_id { "unclaimed" } else { "map" };
        let mut wire = [0; MAX_FRAME_HEADER_BYTES];
        let size = FrameHeader {
            id,
            layout: FrameLayout::tight(1, 1).unwrap(),
            seq: 1,
            settled: 0,
        }
        .encode(&mut wire)
        .unwrap()
        .len();
        if oversized {
            let at = FRAME_PREFIX_BYTES + id.len();
            wire[at..at + 4].copy_from_slice(&4097_u32.to_le_bytes());
        }
        guest.write_all(&wire[..size]).await.unwrap();
        let result = tokio::time::timeout(Duration::from_millis(250), fixture.reader.next())
            .await
            .unwrap();
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidData));
        assert_eq!(
            fixture
                .reader
                .source
                .memory
                .used(&fixture.reader.key.plugin),
            0
        );
        fixture.server.await.unwrap().unwrap();
        assert_eq!(guest.read(&mut [0]).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn the_window_throttles_a_direct_writer_without_an_owner_payload_reader() {
    let mut fixture = Fixture::open().await;
    let mut guest = fixture.writer().await;
    let mut header = FrameHeader {
        id: "map",
        layout: FrameLayout::tight(1, 1).unwrap(),
        seq: 1,
        settled: 0,
    };
    for seq in 1..=MAX_CANVAS_FPS as u64 {
        header.seq = seq;
        present(&mut guest, header, &[3; 4]).await;
        assert!(
            matches!(fixture.reader.next().await.unwrap(), FramePoll::Frame(frame) if frame.seq == seq)
        );
    }
    header.seq += 1;
    present(&mut guest, header, &[4; 4]).await;
    assert!(matches!(
        fixture.reader.next().await.unwrap(),
        FramePoll::Empty
    ));
    header.seq += 1;
    present(&mut guest, header, &[5; 4]).await;
    assert!(
        matches!(fixture.reader.next().await.unwrap(), FramePoll::Frame(frame) if frame.pixels.as_ref() == [5; 4])
    );
    drop(fixture.reader);
    fixture.server.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_desktop_windows_byte_budget_is_shared_by_all_of_an_extensions_canvases() {
    let source = Window::new(PathBuf::from("unused"));
    let readers = (1..=4)
        .map(|n| IpcReader::new(source.clone(), key(&format!("canvas-{n}"))))
        .collect::<Vec<_>>();
    let layout = FrameLayout::tight(4096, 1536).unwrap(); // 24 MiB each
    async fn simulated_frame(reader: &IpcReader, layout: FrameLayout) -> FramePoll {
        let mut wire = [0; MAX_FRAME_HEADER_BYTES];
        let header = FrameHeader {
            id: &reader.key.canvas,
            layout,
            seq: 1,
            settled: 0,
        }
        .encode(&mut wire)
        .unwrap();
        let mut contents = header[1..].chain(tokio::io::repeat(7).take(layout.byte_len() as u64));
        let place = reader.pool.place().await.unwrap();
        reader
            .read_raster(&mut contents, header[0], &mut 0, place)
            .await
            .unwrap()
    }
    let mut kept = Vec::new();
    for (i, reader) in readers.iter().enumerate() {
        match simulated_frame(reader, layout).await {
            FramePoll::Frame(frame) if i < 2 => kept.push(frame.pixels),
            FramePoll::Dropped if i >= 2 => {}
            _ => panic!("the shared budget must refuse the third and fourth allocations"),
        }
    }
    assert_eq!(source.memory.used(&readers[0].key.plugin), 48 * 1024 * 1024);
    drop(kept.pop());
    assert!(matches!(
        simulated_frame(&readers[2], layout).await,
        FramePoll::Frame(_)
    ));
}
