//! Desktop control IPC owns only the canvas lease. A separate local connection carries
//! pixels from the extension straight to this window.
use super::{Client, REQUEST_LIMIT, Request, Stream, connect, write_frame};
use crate::page_frames::{
    DirectWatch, FRAME_PERIOD, Frame, MountError, PooledPixels, READ_WAIT, RasterPool, Relay,
    Traffic, Watch, Window, discard, frame_bytes, lock, read_header, timed, wait_for_rate,
};
use peersus_api::canvas::MAX_FRAME_HEADER_BYTES;
use peersus_plugin::{data::canvas, transport};
use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::OwnedSemaphorePermit,
};

pub use crate::page_frames::Key as FrameKey;

const MAGIC: [u8; 4] = *b"PFB2";
const READY: u8 = 0;
const CLOSED: u8 = 1;
const BUSY: u8 = 2;
const WAIT: Duration = Duration::from_secs(3);
const IO: Duration = Duration::from_secs(2);
// A consumed redirect that never reaches the window must not keep a slot forever.
const MOUNT_WAIT: Duration = Duration::from_secs(6);

/// Pixels received from the renderer, checked and packed for the window's compositor.
pub struct ReceivedFrame {
    pub width: u32,
    pub height: u32,
    pub seq: u64,
    pub settled: u64,
    pub pixels: FramePixels,
}

/// Owns the raster and keeps its memory-budget reservation alive until the compositor releases it.
pub struct FramePixels(PixelSource);

enum PixelSource {
    Ipc(PooledPixels),
    Local(Arc<Frame>),
}

impl AsRef<[u8]> for FramePixels {
    fn as_ref(&self) -> &[u8] {
        match &self.0 {
            PixelSource::Ipc(pixels) => pixels.as_ref(),
            PixelSource::Local(frame) => frame.pixels.as_ref(),
        }
    }
}

pub enum FramePoll {
    /// The canvas is being watched but has no newer frame.
    Empty,
    Frame(ReceivedFrame),
    /// The raster could not be retained within its extension's byte budget.
    Dropped,
    Closed,
    Busy,
}

/// A reader owned by the mounted canvas; dropping it tells the owner this view went away.
pub struct FrameReader(Reader);

enum Reader {
    Ipc(IpcReader),
    Local(LocalReader),
}

impl FrameReader {
    pub fn new(client: &Client, key: FrameKey) -> Option<Self> {
        Some(Self(if let Some(relay) = &client.page_frames {
            Reader::Local(LocalReader {
                relay: relay.clone(),
                key,
                watched: None,
            })
        } else {
            Reader::Ipc(IpcReader::new(client.frame_window.clone()?, key))
        }))
    }

    pub async fn next(&mut self) -> io::Result<FramePoll> {
        match &mut self.0 {
            Reader::Ipc(reader) => reader.next().await,
            Reader::Local(reader) => Ok(reader.next().await),
        }
    }
}

struct LocalReader {
    relay: Relay,
    key: FrameKey,
    watched: Option<Watch>,
}

impl LocalReader {
    async fn next(&mut self) -> FramePoll {
        let first = self.watched.is_none();
        if let Some(watched) = self.watched.as_mut() {
            match tokio::time::timeout(WAIT, watched.latest.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    self.watched = None;
                    return FramePoll::Closed;
                }
                Err(_) => return FramePoll::Empty,
            }
        } else {
            self.watched = self.relay.watch(self.key.clone());
        }
        let Some(watched) = self.watched.as_mut() else {
            return FramePoll::Closed;
        };
        let latest = watched.latest.borrow_and_update().clone();
        match latest {
            Some(frame) => FramePoll::Frame(ReceivedFrame {
                width: frame.layout.width(),
                height: frame.layout.height(),
                seq: frame.seq,
                settled: frame.settled,
                pixels: FramePixels(PixelSource::Local(frame)),
            }),
            None if watched.latest.has_changed().is_err() => {
                self.watched = None;
                FramePoll::Closed
            }
            None if first => FramePoll::Empty, // Mounted, so the guest can now get a claim.
            None => FramePoll::Dropped,        // Release our previous raster to make room.
        }
    }
}

struct IpcReader {
    source: Window,
    key: FrameKey,
    line: Option<Line>,
    traffic: Arc<Mutex<Traffic>>,
    pool: RasterPool,
}

struct Line {
    lease: Stream,
    pixels: PixelLine,
    last_seq: u64,
}

enum PixelLine {
    Waiting {
        listener: transport::Listener,
        _directory: tempfile::TempDir,
        token: canvas::Token,
    },
    Connected(transport::Stream),
}

impl IpcReader {
    fn new(source: Window, key: FrameKey) -> Self {
        Self {
            traffic: source.traffic(&key.plugin),
            source,
            key,
            line: None,
            pool: RasterPool::default(),
        }
    }

    async fn next(&mut self) -> io::Result<FramePoll> {
        let Some(mut line) = self.line.take() else {
            return timed(IO, self.mount()).await;
        };
        let result = tokio::select! {
            // Closing the control lease invalidates even an otherwise idle pixel stream.
            biased;
            _ = line.lease.read_u8() => Ok(FramePoll::Closed),
            frame = self.receive(&mut line.pixels, &mut line.last_seq) => frame,
        };
        if !matches!(result, Ok(FramePoll::Closed | FramePoll::Busy) | Err(_)) {
            self.line = Some(line);
        }
        if !matches!(result, Ok(FramePoll::Frame(_))) {
            // A static canvas need not retain its receive buffer between bursts.
            self.pool.trim();
        }
        result
    }

    async fn mount(&mut self) -> io::Result<FramePoll> {
        // A short path fits even on platforms with a small sockaddr_un. The temporary
        // directory is private, and the Windows transport binds to loopback only.
        #[cfg(unix)]
        let directory = tempfile::tempdir_in("/tmp")?;
        #[cfg(not(unix))]
        let directory = tempfile::tempdir()?;
        let listener = transport::Listener::bind(&directory.path().join("canvas"))?;
        let endpoint = listener
            .endpoint()
            .to_str()
            .ok_or(io::ErrorKind::InvalidData)?
            .to_owned();
        let mut lease = connect(&self.source.path).await?;
        write_frame(
            &mut lease,
            &Request::MountCanvas {
                plugin: self.key.plugin.clone(),
                page: self.key.page.clone(),
                canvas: self.key.canvas.clone(),
                endpoint,
            },
            REQUEST_LIMIT,
        )
        .await?;
        let mut magic = [0; MAGIC.len()];
        lease.read_exact(&mut magic).await?;
        if magic != MAGIC {
            return Err(io::ErrorKind::InvalidData.into());
        }
        match lease.read_u8().await? {
            CLOSED => Ok(FramePoll::Closed),
            BUSY => Ok(FramePoll::Busy),
            READY => {
                let mut token = [0; canvas::TOKEN_BYTES];
                lease.read_exact(&mut token).await?;
                self.line = Some(Line {
                    lease,
                    pixels: PixelLine::Waiting {
                        listener,
                        _directory: directory,
                        token,
                    },
                    last_seq: 0,
                });
                Ok(FramePoll::Empty)
            }
            _ => Err(io::ErrorKind::InvalidData.into()),
        }
    }

    async fn receive(&self, pixels: &mut PixelLine, last_seq: &mut u64) -> io::Result<FramePoll> {
        if let PixelLine::Waiting {
            listener, token, ..
        } = pixels
        {
            let accept = async {
                loop {
                    let mut stream = listener.accept().await?;
                    if timed(canvas::ROUTE_WAIT, canvas::authenticate(&mut stream, token))
                        .await
                        .is_ok()
                    {
                        break io::Result::Ok(stream);
                    }
                }
            };
            match tokio::time::timeout(MOUNT_WAIT, accept).await {
                Ok(stream) => *pixels = PixelLine::Connected(stream?),
                Err(_) => return Ok(FramePoll::Closed),
            }
        }
        let PixelLine::Connected(stream) = pixels else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        // A stalled compositor must not cause a third raster to be buffered. A
        // revoked lease cancels this wait without waiting for pixel activity.
        let place = self.pool.place().await?;
        let first = match tokio::time::timeout(WAIT, stream.read_u8()).await {
            Ok(first) => first?,
            Err(_) => return Ok(FramePoll::Empty),
        };
        self.read_raster(stream, first, last_seq, place).await
    }

    async fn read_raster(
        &self,
        stream: &mut (impl AsyncRead + Unpin),
        first: u8,
        last_seq: &mut u64,
        place: OwnedSemaphorePermit,
    ) -> io::Result<FramePoll> {
        let mut wire = [0; MAX_FRAME_HEADER_BYTES];
        let header = timed(READ_WAIT, read_header(stream, first, &mut wire)).await?;
        if header.id != self.key.canvas {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let publish = timed(
            READ_WAIT + FRAME_PERIOD,
            wait_for_rate(|| lock(&self.traffic).delay(frame_bytes(header))),
        )
        .await?;
        let newer = header.seq > *last_seq;
        *last_seq = (*last_seq).max(header.seq);
        if !publish || !newer {
            timed(READ_WAIT, discard(stream, header.layout.byte_len())).await?;
            return Ok(FramePoll::Empty);
        }
        let Some(mut pixels) = self.pool.reserve(
            &self.source.memory,
            &self.key.plugin,
            header.layout.byte_len(),
        ) else {
            timed(READ_WAIT, discard(stream, header.layout.byte_len())).await?;
            return Ok(FramePoll::Dropped);
        };
        timed(READ_WAIT, pixels.fill(stream)).await?;
        pixels.pack(header.layout);
        Ok(FramePoll::Frame(ReceivedFrame {
            width: header.layout.width(),
            height: header.layout.height(),
            seq: header.seq,
            settled: header.settled,
            pixels: FramePixels(PixelSource::Ipc(self.pool.publish(pixels, place))),
        }))
    }
}

pub(super) async fn serve(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    mounted: Result<DirectWatch, MountError>,
) -> io::Result<()> {
    timed(IO, async {
        stream.write_all(&MAGIC).await?;
        let status = match &mounted {
            Ok(_) => READY,
            Err(MountError::Busy) => BUSY,
            Err(MountError::Closed) => CLOSED,
        };
        stream.write_u8(status).await?;
        if let Ok(mounted) = &mounted {
            stream.write_all(&mounted.token).await?;
        }
        io::Result::Ok(())
    })
    .await?;
    if let Ok(mut mounted) = mounted {
        // No frames or cursors. Removing the declaration ends the lease, and
        // dropping the lease releases an unmounted window's place.
        tokio::select! {
            _ = stream.read_u8() => {}
            _ = &mut mounted.closed => {}
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests;
