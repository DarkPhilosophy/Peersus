//! Canvas claims authorize a direct desktop connection or an in-process raster slot.
use peersus_api::canvas::{
    FRAME_PREFIX_BYTES, FrameHeader, FrameLayout, MAX_CANVAS_BYTES_PER_EXTENSION,
    MAX_CANVAS_BYTES_PER_SECOND, MAX_CANVAS_FPS, MAX_CANVASES_PER_EXTENSION,
    MAX_FRAME_HEADER_BYTES, valid_id,
};
pub(crate) use peersus_api::canvas::{OPEN, SERVICE};
use peersus_core::plugins::PluginId;
use peersus_plugin::data::canvas::{ROUTE_WAIT, Redirect, Token};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    io,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, Weak},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    sync::{OwnedSemaphorePermit, Semaphore, oneshot, watch},
    task::AbortHandle,
};

pub(crate) fn open(relay: &Relay, plugin: PluginId, args: &serde_json::Value) -> Option<u64> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Open {
        page: String,
        canvas: String,
    }
    let Open { page, canvas } = Open::deserialize(args).ok()?;
    let key = Key {
        plugin,
        page,
        canvas,
    };
    if !key.valid() {
        return None;
    }
    relay.claim(&key, Instant::now())
}

/// Canvas and outbound file claims share DataHello, never a number space.
pub(crate) const CLAIM_BIT: u64 = 1 << 63;
const CLAIM_WAIT: Duration = Duration::from_secs(5);
pub(crate) const READ_WAIT: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub plugin: PluginId,
    pub page: String,
    pub canvas: String,
}

impl Key {
    pub(crate) fn valid(&self) -> bool {
        PluginId::new(self.plugin.as_str()).is_ok()
            && peersus_api::name::valid(&self.page)
            && valid_id(&self.canvas)
    }
}

/// Tracks each raster allocation until it is freed, including spare buffers and frames held
/// by the window after they have stopped being the latest.
#[derive(Clone, Default)]
pub(crate) struct Memory(Arc<Mutex<HashMap<PluginId, usize>>>);

impl Memory {
    fn reserve(&self, plugin: &PluginId, len: usize) -> Option<Charge> {
        let mut all = lock(&self.0);
        let used = all.get(plugin).copied().unwrap_or(0);
        if len > MAX_CANVAS_BYTES_PER_EXTENSION - used {
            return None;
        }
        all.insert(plugin.clone(), used + len);
        Some(Charge {
            memory: self.clone(),
            plugin: plugin.clone(),
            len,
        })
    }

    #[cfg(test)]
    pub(crate) fn used(&self, plugin: &PluginId) -> usize {
        lock(&self.0).get(plugin).copied().unwrap_or_default()
    }
}

struct Charge {
    memory: Memory,
    plugin: PluginId,
    len: usize,
}

impl Drop for Charge {
    fn drop(&mut self) {
        let mut all = lock(&self.memory.0);
        if let Some(used) = all.get_mut(&self.plugin) {
            *used -= self.len;
            if *used == 0 {
                all.remove(&self.plugin);
            }
        }
    }
}

pub struct Pixels {
    bytes: Vec<u8>,
    charge: Charge,
}

impl AsRef<[u8]> for Pixels {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl Pixels {
    fn capacity(&self) -> usize {
        self.charge.len
    }

    pub async fn fill(&mut self, stream: &mut (impl AsyncRead + Unpin)) -> io::Result<()> {
        let len = self.capacity();
        self.bytes
            .try_reserve_exact(len - self.bytes.len())
            .map_err(|_| io::ErrorKind::OutOfMemory)?;
        self.bytes.resize(len, 0);
        stream.read_exact(&mut self.bytes).await.map(|_| ())
    }

    /// The raster upload consumes tight rows. Normal helper frames are already tight.
    pub(crate) fn pack(&mut self, layout: FrameLayout) {
        let width = layout.width() as usize * 4;
        let stride = layout.stride() as usize;
        if width != stride {
            for y in 1..layout.height() as usize {
                self.bytes
                    .copy_within(y * stride..y * stride + width, y * width);
            }
            self.bytes.truncate(width * layout.height() as usize);
        }
    }
}

/// A published raster and one in flight. The last reader of a raster can return its
/// storage, but never keeps the canvas's pool alive after unmount.
#[derive(Clone)]
pub(crate) struct RasterPool {
    places: Arc<Semaphore>,
    spare: Arc<Mutex<Option<Pixels>>>,
}

impl Default for RasterPool {
    fn default() -> Self {
        Self {
            places: Arc::new(Semaphore::new(2)),
            spare: Arc::default(),
        }
    }
}

impl RasterPool {
    pub async fn place(&self) -> io::Result<OwnedSemaphorePermit> {
        self.places
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| io::ErrorKind::BrokenPipe.into())
    }

    pub fn reserve(&self, memory: &Memory, plugin: &PluginId, len: usize) -> Option<Pixels> {
        let reusable = lock(&self.spare).take();
        reusable
            .filter(|pixels| pixels.capacity() == len)
            .or_else(|| {
                memory.reserve(plugin, len).map(|charge| Pixels {
                    bytes: Vec::new(),
                    charge,
                })
            })
    }

    pub fn publish(&self, pixels: Pixels, place: OwnedSemaphorePermit) -> PooledPixels {
        PooledPixels {
            pixels: Some(pixels),
            spare: Arc::downgrade(&self.spare),
            place,
        }
    }

    pub fn trim(&self) {
        lock(&self.spare).take();
    }
}

pub(crate) struct PooledPixels {
    pixels: Option<Pixels>,
    spare: Weak<Mutex<Option<Pixels>>>,
    // Released after Drop returns the storage, so the next read can reuse it.
    place: OwnedSemaphorePermit,
}

impl AsRef<[u8]> for PooledPixels {
    fn as_ref(&self) -> &[u8] {
        self.pixels
            .as_ref()
            .expect("pooled raster storage is present until drop")
            .as_ref()
    }
}

impl Drop for PooledPixels {
    fn drop(&mut self) {
        if self.place.semaphore().available_permits() == 0
            && let Some(pool) = self.spare.upgrade()
        {
            let mut spare = lock(&pool);
            if spare.is_none() {
                *spare = self.pixels.take();
            }
        }
    }
}

pub(crate) async fn discard(stream: &mut (impl AsyncRead + Unpin), len: usize) -> io::Result<()> {
    if tokio::io::copy(&mut stream.take(len as u64), &mut tokio::io::sink()).await? != len as u64 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

/// What the window needs to open independent kept frame lines sharing one memory budget.
#[derive(Clone)]
pub(crate) struct Window {
    pub path: Arc<PathBuf>,
    pub memory: Memory,
    rates: Arc<Mutex<HashMap<PluginId, Weak<Mutex<Traffic>>>>>,
}

impl Window {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: Arc::new(path),
            memory: Memory::default(),
            rates: Arc::default(),
        }
    }

    pub fn traffic(&self, plugin: &PluginId) -> Arc<Mutex<Traffic>> {
        let mut rates = lock(&self.rates);
        if let Some(traffic) = rates.get(plugin).and_then(Weak::upgrade) {
            return traffic;
        }
        rates.retain(|_, weak| weak.strong_count() != 0);
        let traffic = Arc::new(Mutex::new(Traffic::default()));
        rates.insert(plugin.clone(), Arc::downgrade(&traffic));
        traffic
    }
}

#[derive(Default, Clone)]
pub(crate) struct Relay(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    declared: HashMap<(PluginId, String), Vec<String>>,
    open: HashMap<Key, Slot>,
    traffic: HashMap<PluginId, Traffic>,
    memory: Memory,
    nonce: u64,
}

impl State {
    fn next(&mut self) -> Option<u64> {
        self.nonce = self.nonce.checked_add(1).filter(|&n| n < CLAIM_BIT)?;
        Some(self.nonce)
    }

    fn declares(&self, key: &Key) -> bool {
        key.valid()
            && self
                .declared
                .get(&(key.plugin.clone(), key.page.clone()))
                .is_some_and(|ids| ids.contains(&key.canvas))
    }

    fn has_room(&self, key: &Key) -> bool {
        self.open
            .keys()
            .filter(|other| other.plugin == key.plugin)
            .count()
            < MAX_CANVASES_PER_EXTENSION
    }

    fn local(&self, key: &Key, epoch: u64) -> io::Result<&Local> {
        match self.open.get(key).filter(|slot| slot.epoch == epoch) {
            Some(Slot {
                route: Route::Local(local),
                ..
            }) => Ok(local),
            _ => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
}

struct Slot {
    epoch: u64,
    views: usize,
    claim: Option<(u64, Instant)>,
    writer: Option<AbortHandle>,
    route: Route,
}

enum Route {
    Local(Local),
    Direct {
        redirect: Option<Redirect>,
        _mounted: oneshot::Sender<()>,
    },
}

struct Local {
    latest: watch::Sender<Option<Arc<Frame>>>,
    pool: RasterPool,
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Route::Local(local) = &self.route {
            local.latest.send_replace(None);
            local.pool.trim();
        }
        if let Some(writer) = &self.writer {
            writer.abort();
        }
    }
}

pub(crate) struct Frame {
    pub layout: FrameLayout,
    pub seq: u64,
    pub settled: u64,
    pub pixels: PooledPixels,
}

pub(crate) struct Watch {
    _lease: Lease,
    pub latest: watch::Receiver<Option<Arc<Frame>>>,
}

pub(crate) struct DirectWatch {
    _lease: Lease,
    pub closed: oneshot::Receiver<()>,
    pub token: Token,
}

pub(crate) enum MountError {
    Closed,
    Busy,
}

struct Lease {
    relay: Relay,
    key: Key,
    epoch: u64,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = lock(&self.relay.0);
        if let Some(slot) = state
            .open
            .get_mut(&self.key)
            .filter(|slot| slot.epoch == self.epoch)
        {
            slot.views -= 1;
            if slot.views == 0 {
                state.open.remove(&self.key);
            }
        }
    }
}

impl Relay {
    /// Synchronize claims and live slots before publishing the page tree to clients.
    pub fn set_page(&self, plugin: &PluginId, page: &str, ids: Vec<String>) {
        let mut state = lock(&self.0);
        state.open.retain(|key, _| {
            key.plugin != *plugin || key.page != page || ids.contains(&key.canvas)
        });
        let page = (plugin.clone(), page.to_owned());
        if ids.is_empty() {
            state.declared.remove(&page);
        } else {
            state.declared.insert(page, ids);
        }
    }

    pub fn forget(&self, plugin: &PluginId) -> bool {
        let mut state = lock(&self.0);
        state.open.retain(|key, _| key.plugin != *plugin);
        let before = state.declared.len();
        state.declared.retain(|(id, _), _| id != plugin);
        state.traffic.remove(plugin);
        state.declared.len() != before
    }

    pub fn watch(&self, key: Key) -> Option<Watch> {
        let mut state = lock(&self.0);
        if !state.declares(&key) {
            return None;
        }
        if !state.open.contains_key(&key) {
            if !state.has_room(&key) {
                return None;
            }
            let epoch = state.next()?;
            state.open.insert(
                key.clone(),
                Slot {
                    epoch,
                    views: 0,
                    claim: None,
                    writer: None,
                    route: Route::Local(Local {
                        latest: watch::channel(None).0,
                        pool: RasterPool::default(),
                    }),
                },
            );
        }
        let slot = state.open.get_mut(&key)?;
        let Route::Local(local) = &slot.route else {
            return None;
        };
        slot.views += 1;
        Some(Watch {
            _lease: Lease {
                relay: self.clone(),
                key,
                epoch: slot.epoch,
            },
            latest: local.latest.subscribe(),
        })
    }

    pub fn mount_direct(&self, key: Key, endpoint: String) -> Result<DirectWatch, MountError> {
        let mut state = lock(&self.0);
        if !state.declares(&key) {
            return Err(MountError::Closed);
        }
        // One producer stream cannot be delivered to two processes without reintroducing
        // a pixel relay. A second window waits for the current view to unmount.
        if state.open.contains_key(&key) || !state.has_room(&key) {
            return Err(MountError::Busy);
        }
        let token = rand::random::<Token>();
        let redirect = Redirect::new(endpoint, token).ok_or(MountError::Closed)?;
        let epoch = state.next().ok_or(MountError::Closed)?;
        let (mounted, closed) = oneshot::channel();
        state.open.insert(
            key.clone(),
            Slot {
                epoch,
                views: 1,
                claim: None,
                writer: None,
                route: Route::Direct {
                    redirect: Some(redirect),
                    _mounted: mounted,
                },
            },
        );
        Ok(DirectWatch {
            _lease: Lease {
                relay: self.clone(),
                key,
                epoch,
            },
            closed,
            token,
        })
    }

    pub fn claim(&self, key: &Key, now: Instant) -> Option<u64> {
        let mut state = lock(&self.0);
        let slot = state.open.get(key)?;
        if slot.writer.is_some() || matches!(slot.route, Route::Direct { redirect: None, .. }) {
            return None;
        }
        if let Some((claim, _)) = slot.claim.filter(|&(_, expires)| now < expires) {
            return Some(claim);
        }
        let claim = state.next()? | CLAIM_BIT;
        state.open.get_mut(key)?.claim = Some((claim, now + CLAIM_WAIT));
        Some(claim)
    }

    pub fn accept(
        &self,
        plugin: &PluginId,
        claim: u64,
        mut stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    ) {
        let now = Instant::now();
        let mut state = lock(&self.0);
        let Some((key, slot)) = state.open.iter_mut().find(|(key, slot)| {
            key.plugin == *plugin
                && slot
                    .claim
                    .is_some_and(|(number, expires)| number == claim && now < expires)
        }) else {
            return;
        };
        slot.claim = None;
        let (key, epoch, relay) = (key.clone(), slot.epoch, self.clone());
        if let Route::Direct { redirect, .. } = &mut slot.route {
            if let Some(redirect) = redirect.take() {
                // Only the small redirect travels on this owner connection. Closing it
                // after the answer also refuses a worker trying to put pixels on it.
                tokio::spawn(async move {
                    if timed(ROUTE_WAIT, redirect.write(&mut stream))
                        .await
                        .is_err()
                    {
                        let mut state = lock(&relay.0);
                        if state.open.get(&key).is_some_and(|slot| slot.epoch == epoch) {
                            state.open.remove(&key);
                        }
                    }
                });
            }
            return;
        }
        slot.writer = Some(
            tokio::spawn(async move {
                let _ = relay.receive(&key, epoch, stream).await;
                if let Some(slot) = lock(&relay.0)
                    .open
                    .get_mut(&key)
                    .filter(|slot| slot.epoch == epoch)
                {
                    slot.writer = None;
                }
            })
            .abort_handle(),
        );
    }

    async fn allow_frame(&self, key: &Key, epoch: u64, size: usize) -> io::Result<bool> {
        wait_for_rate(|| {
            let mut state = lock(&self.0);
            if state.open.get(key).is_none_or(|slot| slot.epoch != epoch) {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            state
                .traffic
                .entry(key.plugin.clone())
                .or_default()
                .delay(size)
        })
        .await
    }

    async fn receive(
        &self,
        key: &Key,
        epoch: u64,
        mut stream: impl AsyncRead + Unpin,
    ) -> io::Result<()> {
        let pool = lock(&self.0).local(key, epoch)?.pool.clone();
        let mut previous = 0;
        let mut wire = [0; MAX_FRAME_HEADER_BYTES];
        loop {
            // A static canvas may send nothing for a long time. Bounded reads begin when
            // it starts a frame; closing the slot cancels an idle or backpressured task.
            let first = stream.read_u8().await?;
            let header = timed(READ_WAIT, read_header(&mut stream, first, &mut wire)).await?;
            if header.id != key.canvas {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let len = header.layout.byte_len();
            let publish = timed(
                READ_WAIT + FRAME_PERIOD,
                self.allow_frame(key, epoch, frame_bytes(header)),
            )
            .await?;
            let newer = header.seq > previous;
            previous = previous.max(header.seq);
            if !publish || !newer {
                timed(READ_WAIT, discard(&mut stream, len)).await?;
                continue;
            }
            // Backpressure is not a slow body read: a busy window may release the raster
            // later, or unmount and cancel the writer. Do not lose its last update to a timer.
            let place = pool.place().await?;
            timed(READ_WAIT, async {
                let pixels = {
                    let state = lock(&self.0);
                    let local = state.local(key, epoch)?;
                    pool.reserve(&state.memory, &key.plugin, len).or_else(|| {
                        // Give up the slot's reference if it is blocking the shared byte budget.
                        // A compositor still drawing the old frame owns its charge.
                        local.latest.send_replace(None);
                        pool.reserve(&state.memory, &key.plugin, len)
                    })
                };
                let Some(mut pixels) = pixels else {
                    return discard(&mut stream, len).await;
                };
                let mut layout = header.layout;
                pixels.fill(&mut stream).await?;
                // An in-process compositor can take the frame by reference. Its raster image
                // requires tight rows; worker frames are already tight, so no pixels move there.
                if layout.stride() != layout.width() * 4 {
                    pixels.pack(layout);
                    layout = FrameLayout::tight(layout.width(), layout.height())
                        .map_err(|_| io::ErrorKind::InvalidData)?;
                }
                let state = lock(&self.0);
                let local = state.local(key, epoch)?;
                local.latest.send_replace(Some(Arc::new(Frame {
                    layout,
                    seq: header.seq,
                    settled: header.settled,
                    pixels: pool.publish(pixels, place),
                })));
                Ok(())
            })
            .await?;
        }
    }
}

pub(crate) const FRAME_PERIOD: Duration = Duration::from_secs(1);

pub(crate) fn frame_bytes(header: FrameHeader<'_>) -> usize {
    MAX_FRAME_HEADER_BYTES - peersus_api::canvas::MAX_CANVAS_ID_BYTES
        + header.id.len()
        + header.layout.byte_len()
}

pub(crate) async fn wait_for_rate(
    mut retry_after: impl FnMut() -> io::Result<Option<Duration>>,
) -> io::Result<bool> {
    let mut publish = true;
    while let Some(delay) = retry_after()? {
        // Backpressure before consuming a payload, even one that will be dropped.
        publish = false;
        tokio::time::sleep(delay).await;
    }
    Ok(publish)
}

#[derive(Default)]
pub(crate) struct Traffic {
    frames: VecDeque<(Instant, usize)>,
    bytes: usize,
}

impl Traffic {
    pub fn delay(&mut self, size: usize) -> io::Result<Option<Duration>> {
        let now = Instant::now();
        if self.admit(now, size) {
            return Ok(None);
        }
        let (oldest, _) = self.frames.front().ok_or(io::ErrorKind::InvalidData)?;
        Ok(Some(
            FRAME_PERIOD.saturating_sub(now.duration_since(*oldest)),
        ))
    }

    fn admit(&mut self, now: Instant, len: usize) -> bool {
        while let Some(&(at, size)) = self.frames.front() {
            if now.duration_since(at) < FRAME_PERIOD {
                break;
            }
            self.frames.pop_front();
            self.bytes -= size;
        }
        if self.frames.len() >= MAX_CANVAS_FPS || len > MAX_CANVAS_BYTES_PER_SECOND - self.bytes {
            return false;
        }
        self.frames.push_back((now, len));
        self.bytes += len;
        true
    }
}

pub(crate) async fn read_header<'a>(
    stream: &mut (impl AsyncRead + Unpin),
    first: u8,
    wire: &'a mut [u8; MAX_FRAME_HEADER_BYTES],
) -> io::Result<FrameHeader<'a>> {
    wire[0] = first;
    stream.read_exact(&mut wire[1..FRAME_PREFIX_BYTES]).await?;
    let prefix = wire
        .first_chunk::<FRAME_PREFIX_BYTES>()
        .ok_or(io::ErrorKind::InvalidData)?;
    let end = FrameHeader::encoded_len(prefix).map_err(|_| io::ErrorKind::InvalidData)?;
    stream
        .read_exact(&mut wire[FRAME_PREFIX_BYTES..end])
        .await?;
    FrameHeader::decode(&wire[..end]).map_err(|_| io::ErrorKind::InvalidData.into())
}

pub(crate) async fn timed<T>(
    limit: Duration,
    f: impl Future<Output = io::Result<T>>,
) -> io::Result<T> {
    tokio::time::timeout(limit, f)
        .await
        .map_err(|_| io::ErrorKind::TimedOut)?
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
