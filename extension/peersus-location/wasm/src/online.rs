//! An opt-in public browsing session. Only explicitly explored viewport tiles are requested.
use crate::{
    tile,
    world::{Camera, Frame, View},
};
use serde_json::{Value, json};
use std::{cell::RefCell, collections::VecDeque};

pub const SERVICE: &str = "peersus.http";
const ORIGIN: &str = "https://tiles.openfreemap.org";
const ROOT: &str = "/planet";
const MAX_BYTES: usize = 4 * 1024 * 1024;
// During zoom, the last complete level and its replacement can coexist.
const MAX_CACHED_TILES: usize = 2 * tile::MAX_TILES;
const CACHED_SOURCE_BYTES: usize = 8 * 1024 * 1024;
const FIRST_DETAIL: f64 = crate::world::REGIONAL_SPAN;
const RETRY_AFTER: u64 = 8_000;
const DOWNLOAD_GAP: u64 = 250;
const INDEX_AGE: u64 = 24 * 60 * 60;
const TILE_AGE: u64 = 30 * INDEX_AGE;
pub struct Call {
    pub method: &'static str,
    pub args: Value,
    pub timeout: u32,
}
fn call(method: &'static str, args: Value, timeout: u32) -> Call {
    Call {
        method,
        args,
        timeout,
    }
}

#[derive(Clone, Copy)]
enum Resource {
    Index(IndexFetch),
    Tile(TileLoad),
    ConfirmCopy(TileId),
}

#[derive(Clone, Copy)]
enum IndexFetch {
    Open,
    Check,
    Save,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fetch {
    Normal,
    Kept,
    Refresh,
}

#[derive(Clone, Copy)]
struct TileLoad {
    id: TileId,
    previous: bool,
    fetch: Fetch,
    quiet: bool,
}

#[derive(Clone, Copy)]
struct Loaded {
    tile: TileLoad,
    refresh_at: Option<u64>,
    saved: bool,
}

type TileId = tile::Key;

struct Cached {
    id: TileId,
    previous: bool,
    refresh_at: Option<u64>,
    saved: bool,
    tile: tile::Tile,
    source_bytes: usize,
}

struct Metadata {
    prefix: String,
    zoom: u8,
}

impl Metadata {
    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 512 * 1024 {
            return None;
        }
        let v: Value = serde_json::from_slice(bytes).ok()?;
        let prefix = v["tiles"]
            .as_array()?
            .first()?
            .as_str()?
            .strip_prefix(ORIGIN)?
            .strip_suffix("/{z}/{x}/{y}.pbf")?;
        let version = prefix.strip_prefix("/planet/")?;
        if version.is_empty()
            || version.len() > 64
            || !version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return None;
        }
        let zoom = u8::try_from(v["maxzoom"].as_u64()?).ok()?;
        (1..=20).contains(&zoom).then(|| Self {
            prefix: prefix.into(),
            zoom,
        })
    }
}

enum Pending {
    Restore,
    Get(Resource),
    Read {
        resource: Resource,
        age: Option<u64>,
        key: String,
        total: usize,
        bytes: Vec<u8>,
    },
}

struct CachedDrawing {
    at: (View, Frame),
    drawing: tile::Drawing,
    complete: bool,
    used: Vec<TileId>,
}

#[derive(Default)]
pub struct Online {
    current_view_only: bool,
    authorization: Option<String>,
    pending: Option<Pending>,
    decoding: Option<(Loaded, tile::Decode)>,
    request: u64,
    metadata: Option<Metadata>,
    previous: Option<Metadata>,
    supports_kept: bool,
    index_refresh_at: Option<u64>,
    refreshed_index: bool,
    index_needs_save: bool,
    // Only the current viewport's probes, for at most two source versions.
    probed: Vec<(TileId, bool)>,
    drawing_stamp: u64,
    published: Option<(View, Frame, u64)>,
    displayed: Vec<TileId>,
    tiles: VecDeque<Cached>,
    cached: RefCell<Option<CachedDrawing>>,
    /// Failed tiles are retried independently, and never more quickly than the retry window.
    failed: Vec<(TileId, u64)>,
    retry: u64,
    next_download: u64,
    retry_refresh: u64,
    notice: Option<&'static str>,
}

impl Online {
    pub fn ready(&self) -> bool {
        self.authorization.is_some()
    }
    pub fn notice(&self) -> Option<&'static str> {
        self.notice
    }

    /// Clear both forms of tile cache and cancel any in-flight tile read. Metadata and approval stay.
    pub fn forget_tiles(&mut self) -> u64 {
        while !self.tiles.is_empty() {
            self.evict(0);
        }
        self.failed.clear();
        self.probed.clear();
        self.decoding = None;
        if matches!(self.pending, Some(Pending::Get(_) | Pending::Read { .. })) {
            self.pending = None;
            std::mem::take(&mut self.request)
        } else {
            0
        }
    }

    #[cfg(test)]
    pub fn stage_fixture(&mut self, fixtures: &std::path::Path, view: View, frame: Frame) -> usize {
        self.authorization = Some("fixture".into());
        self.metadata = Some(Metadata {
            prefix: "/planet/fixture".into(),
            zoom: 14,
        });
        let visible = self.visible(view, frame);
        for &id in &visible {
            let filename = fixtures.join(format!("{}-{}-{}.pbf", id.z, id.x, id.y));
            let bytes =
                std::fs::read(&filename).unwrap_or_else(|error| panic!("{filename:?}: {error}"));
            let mut decoder = tile::Decode::start(id, bytes).expect("valid public fixture");
            assert!(
                (0..1024).any(|_| decoder.step().expect("valid fixture geometry")),
                "{filename:?}: fixture did not finish decoding"
            );
            self.store(
                Loaded {
                    tile: TileLoad {
                        id,
                        previous: false,
                        fetch: Fetch::Kept,
                        quiet: true,
                    },
                    refresh_at: None,
                    saved: true,
                },
                decoder.finish(),
                &visible,
                0,
            );
        }
        assert_eq!(
            self.tiles.len(),
            visible.len(),
            "keep every tile covering the fixture camera"
        );
        self.tiles.iter().map(|tile| tile.source_bytes).sum()
    }

    #[cfg(test)]
    pub fn cached_keys(&self) -> Vec<tile::Key> {
        self.tiles.iter().map(|tile| tile.id).collect()
    }

    #[cfg(test)]
    pub fn shown_keys(&self, view: View, frame: Frame) -> Vec<tile::Key> {
        self.select(&self.visible(view, frame))
            .0
            .iter()
            .map(|tile| tile.id)
            .collect()
    }

    pub fn revision(&self) -> u64 {
        self.drawing_stamp
    }

    pub fn awaiting_coverage(&self, view: View, frame: Frame, now: u64) -> bool {
        if !self.ready() {
            return false;
        }
        let visible = self.visible(view, frame);
        !visible.is_empty()
            && !self
                .failed
                .iter()
                .any(|&(id, retry)| now < retry && visible.contains(&id))
            && !self.select(&visible).1
    }

    /// Only a successful Canvas presentation authorizes us to save a replacement index.
    pub fn frame_shown(&mut self, view: View, frame: Frame) {
        self.displayed.clear();
        self.published = self
            .cached
            .get_mut()
            .as_ref()
            .filter(|cached| cached.at == (view, frame) && cached.complete)
            .map(|cached| {
                self.displayed.clone_from(&cached.used);
                (view, frame, self.drawing_stamp)
            });
    }

    fn still_covers(visible: &[tile::Key], id: TileId) -> bool {
        visible
            .iter()
            .any(|&key| contains(id, key) || contains(key, id))
    }

    fn on_screen(&self, id: TileId) -> bool {
        self.displayed.contains(&id)
    }

    fn eviction(&self, visible: &[tile::Key]) -> Option<usize> {
        let candidates = || {
            self.tiles
                .iter()
                .enumerate()
                .filter(|(_, tile)| !self.on_screen(tile.id))
        };
        candidates()
            .find(|(_, tile)| !Self::still_covers(visible, tile.id))
            .or_else(|| candidates().find(|(_, tile)| !visible.contains(&tile.id)))
            .map(|(index, _)| index)
    }

    fn evict(&mut self, index: usize) {
        if self.tiles.remove(index).is_some() {
            self.changed();
        }
    }

    fn changed(&mut self) {
        *self.cached.get_mut() = None;
        self.drawing_stamp = self.drawing_stamp.wrapping_add(1);
    }

    fn store(&mut self, loaded: Loaded, tile: tile::Tile, visible: &[tile::Key], now: u64) {
        let id = loaded.tile.id;
        let bytes = tile.source_bytes();
        loop {
            let old = self.tiles.iter().find(|cached| cached.id == id);
            let occupied = self
                .tiles
                .iter()
                .map(|tile| tile.source_bytes)
                .sum::<usize>();
            let need = occupied + bytes - old.map_or(0, |tile| tile.source_bytes);
            if need <= CACHED_SOURCE_BYTES && (old.is_some() || self.tiles.len() < MAX_CACHED_TILES)
            {
                break;
            }
            if bytes > CACHED_SOURCE_BYTES {
                self.unavailable(Resource::Tile(loaded.tile), now);
                return;
            }
            let Some(index) = self.eviction(visible) else {
                self.unavailable(Resource::Tile(loaded.tile), now);
                return;
            };
            self.evict(index);
        }
        if let Some(old) = self.tiles.iter().position(|cached| cached.id == id) {
            self.evict(old);
        }
        self.tiles.push_back(Cached {
            id,
            previous: loaded.tile.previous,
            saved: loaded.saved,
            refresh_at: loaded.refresh_at,
            tile,
            source_bytes: bytes,
        });
        self.changed();
        self.notice = None;
    }

    pub fn cache_recent(&mut self, save: bool) {
        if self.current_view_only == save {
            self.current_view_only = !save;
            self.changed();
        }
    }
    pub fn request(&self, id: u64) -> bool {
        self.request != 0 && self.request == id
    }
    pub fn sent(&mut self, id: u64) {
        self.request = id;
    }

    pub fn restore(&mut self) -> Call {
        self.pending = Some(Pending::Restore);
        call("restore-read", json!({"origin":ORIGIN,"root":ROOT}), 4000)
    }

    pub fn disable(&mut self) -> (u64, Option<Call>) {
        let release = self.authorization.take().map(|authorization| {
            call("release-read", json!({"authorization":authorization}), 3000)
        });
        let request = self.request;
        self.forget_tiles();
        *self = Self::default();
        (request, release)
    }

    pub fn busy(&self, now: u64, view: View, frame: Frame) -> bool {
        if view.span > FIRST_DETAIL {
            return false;
        }
        if self.pending.is_some() || self.decoding.is_some() {
            return true;
        }
        if !self.ready() || now < self.retry {
            return false;
        }
        let Some(meta) = &self.metadata else {
            return true;
        };
        let visible = tile::visible(view, frame, meta.zoom);
        self.next_probe(&visible).is_some()
            || self.next_tile(now, &visible).is_some()
            || self.refresh(now, view, frame, &visible).is_some()
    }

    fn current(&self, id: TileId) -> Option<&Cached> {
        self.tiles
            .iter()
            .find(|cached| cached.id == id && !cached.previous)
    }

    fn next_tile(&self, now: u64, visible: &[tile::Key]) -> Option<TileLoad> {
        visible
            .iter()
            .copied()
            .filter(|&id| {
                self.current(id).is_none()
                    && !self
                        .failed
                        .iter()
                        .any(|&(failed, after)| failed == id && after > now)
            })
            .map(|id| TileLoad {
                id,
                previous: false,
                fetch: Fetch::Normal,
                quiet: self.tiles.iter().any(|tile| contains(tile.id, id)),
            })
            .min_by_key(|load| self.previous.is_some() && load.quiet)
    }

    fn next_probe(&self, visible: &[tile::Key]) -> Option<TileLoad> {
        if !self.supports_kept || self.current_view_only {
            return None;
        }
        let old_zoom = self.previous.as_ref().map(|index| index.zoom);
        // Probe every current copy first; a miss is local and never blocks another saved tile.
        for previous in [false, true] {
            for id in visible.iter().copied() {
                if self.current(id).is_some() {
                    continue;
                }
                let id = if previous {
                    let zoom = old_zoom?;
                    let key = at_most_zoom(id, zoom);
                    if self.tiles.iter().any(|cached| cached.id == key) {
                        continue;
                    }
                    key
                } else {
                    id
                };
                if !self.probed.contains(&(id, previous)) {
                    return Some(TileLoad {
                        id,
                        previous,
                        fetch: Fetch::Kept,
                        quiet: true,
                    });
                }
            }
        }
        None
    }

    fn refresh(
        &self,
        now: u64,
        view: View,
        frame: Frame,
        visible: &[tile::Key],
    ) -> Option<Resource> {
        if self.current_view_only
            || !self.supports_kept
            || now < self.retry_refresh
            || self.published != Some((view, frame, self.drawing_stamp))
            || visible.is_empty()
            || visible.iter().copied().any(|id| self.current(id).is_none())
        {
            return None;
        }
        if self.index_needs_save {
            // A downloaded tile may have been drawn even if the host could not write it.
            // Verify every new copy before the saved index can point at it.
            return Some(
                visible
                    .iter()
                    .copied()
                    .find(|&id| self.current(id).is_some_and(|tile| !tile.saved))
                    .map_or(Resource::Index(IndexFetch::Save), Resource::ConfirmCopy),
            );
        }
        if !self.refreshed_index && self.index_refresh_at.is_some_and(|at| now >= at) {
            return Some(Resource::Index(IndexFetch::Check));
        }
        visible
            .iter()
            .copied()
            .find(|&id| {
                self.current(id)
                    .is_some_and(|tile| tile.refresh_at.is_some_and(|at| now >= at))
            })
            .map(|id| {
                Resource::Tile(TileLoad {
                    id,
                    previous: false,
                    fetch: Fetch::Refresh,
                    quiet: true,
                })
            })
    }

    fn visible(&self, view: View, frame: Frame) -> Vec<tile::Key> {
        self.metadata
            .as_ref()
            .filter(|_| view.span <= FIRST_DETAIL)
            .map_or_else(Vec::new, |meta| tile::visible(view, frame, meta.zoom))
    }

    pub fn decode_pending(&self) -> bool {
        self.decoding.is_some()
    }

    pub fn tick(&mut self, now: u64, view: View, frame: Frame) {
        let Some((loaded, mut decoder)) = self.decoding.take() else {
            return;
        };
        match decoder.step() {
            Ok(false) => self.decoding = Some((loaded, decoder)),
            Ok(true) => self.store(loaded, decoder.finish(), &self.visible(view, frame), now),
            Err(()) => self.unavailable(Resource::Tile(loaded.tile), now),
        }
    }

    pub fn advance(&mut self, now: u64, view: View, frame: Frame) -> Option<Call> {
        let visible = self.visible(view, frame);
        if self.current_view_only {
            let wanted = |id| visible.contains(&id);
            for i in (0..self.tiles.len()).rev() {
                if !wanted(self.tiles[i].id) {
                    self.evict(i);
                }
            }
            if self
                .decoding
                .as_ref()
                .is_some_and(|(loaded, _)| !wanted(loaded.tile.id))
            {
                self.decoding = None;
            }
        }
        if view.span > FIRST_DETAIL || !self.available() || now < self.retry {
            return None;
        }
        if self.metadata.is_none() {
            return (now >= self.next_download)
                .then(|| self.get(now, Resource::Index(IndexFetch::Open)))
                .flatten();
        }
        if visible.is_empty() {
            self.notice = Some("The online source does not cover this area.");
            return None;
        }
        if let Some(request) = self.probe(now, &visible) {
            return Some(request);
        }
        let network = now >= self.next_download;
        self.failed
            .retain(|(id, after)| *after > now && visible.contains(id));
        let missing = network.then(|| self.next_tile(now, &visible)).flatten();
        let resource = missing.map(Resource::Tile).or_else(|| {
            self.refresh(now, view, frame, &visible)
                .filter(|resource| network || matches!(resource, Resource::ConfirmCopy(_)))
        })?;
        self.get(now, resource)
    }

    fn available(&self) -> bool {
        self.ready() && self.pending.is_none() && self.decoding.is_none()
    }

    /// Between page ticks, use ordinary host results to drain the local cache promptly. Only
    /// a page event can start network I/O, and cache reads never use the download pacing slot.
    pub fn advance_cached(&mut self, now: u64, view: View, frame: Frame) -> Option<Call> {
        if !self.supports_kept || self.current_view_only || !self.available() {
            return None;
        }
        let visible = self.visible(view, frame);
        self.probe(now, &visible)
    }

    fn probe(&mut self, now: u64, visible: &[tile::Key]) -> Option<Call> {
        let old_zoom = self.previous.as_ref().map(|index| index.zoom);
        self.probed.retain(|(id, previous)| {
            visible.iter().any(|&key| {
                if *previous {
                    old_zoom.is_some_and(|zoom| at_most_zoom(key, zoom) == *id)
                } else {
                    key == *id
                }
            })
        });
        let load = self.next_probe(visible)?;
        self.probed.push((load.id, load.previous));
        self.get(now, Resource::Tile(load))
    }

    /// A network result for the previous viewport must not delay reading a newly visible copy.
    /// Cancelling only the get (never a local read) preserves the host's single resource slot.
    pub fn cached_first(&mut self, now: u64, view: View, frame: Frame) -> u64 {
        let network = match self.pending {
            Some(Pending::Get(Resource::Index(IndexFetch::Check | IndexFetch::Save))) => true,
            Some(Pending::Get(Resource::Tile(load))) => load.fetch != Fetch::Kept,
            _ => false,
        };
        if network && self.next_probe(&self.visible(view, frame)).is_some() {
            if matches!(
                self.pending,
                Some(Pending::Get(Resource::Index(IndexFetch::Check)))
            ) {
                // A preempted check did not tell us anything. Try again after the view is
                // covered, with a cooldown so a pan cannot repeatedly cancel network work.
                self.retry_refresh = now.saturating_add(RETRY_AFTER);
            }
            self.pending = None;
            return std::mem::take(&mut self.request);
        }
        0
    }

    fn get(&mut self, now: u64, resource: Resource) -> Option<Call> {
        let (path, fetch, quiet, keep) = match resource {
            Resource::Index(IndexFetch::Open) => {
                (ROOT.into(), Fetch::Normal, false, !self.current_view_only)
            }
            Resource::Index(IndexFetch::Check) => (ROOT.into(), Fetch::Refresh, true, false),
            Resource::Index(IndexFetch::Save) => (ROOT.into(), Fetch::Refresh, true, true),
            Resource::ConfirmCopy(id) => (self.tile_path(id, false)?, Fetch::Kept, true, true),
            Resource::Tile(load) => {
                let path = self.tile_path(load.id, load.previous)?;
                if load.fetch == Fetch::Refresh
                    && let Some(tile) = self.tiles.iter_mut().find(|tile| tile.id == load.id)
                {
                    tile.refresh_at = None;
                }
                (path, load.fetch, load.quiet, !self.current_view_only)
            }
        };
        let authorization = self.authorization.as_ref()?;
        let mut args = json!({"authorization":authorization,"path":path,"keep":keep});
        match fetch {
            Fetch::Kept => args["kept_only"] = json!(true),
            Fetch::Refresh => args["refresh"] = json!(true),
            Fetch::Normal => {}
        }
        if fetch != Fetch::Kept {
            self.next_download = now.saturating_add(DOWNLOAD_GAP);
        }
        if !quiet {
            self.notice = Some("Loading online streets…");
        }
        self.pending = Some(Pending::Get(resource));
        Some(call("get-resource", args, 15_000))
    }

    fn tile_path(&self, key: tile::Key, previous: bool) -> Option<String> {
        let meta = if previous {
            self.previous.as_ref()
        } else {
            self.metadata.as_ref()
        }?;
        let tile::Key { z, x, y } = key;
        Some(format!("{}/{z}/{x}/{y}.pbf", meta.prefix))
    }

    pub fn result(&mut self, result: &Value, now: u64) -> Option<Call> {
        self.request = 0;
        let stage = self.pending.take()?;
        if result["status"] != "ok" {
            let detail = match stage {
                Pending::Restore => return None,
                Pending::Get(r) | Pending::Read { resource: r, .. } => r,
            };
            self.unavailable(detail, now);
            return None;
        }
        let value = &result["value"];
        match stage {
            Pending::Restore => {
                self.authorization = handle(&value["authorization"]).map(str::to_owned);
                None
            }
            Pending::Get(resource) => {
                if let Resource::ConfirmCopy(id) = resource {
                    if value["age"].as_u64().is_some()
                        && let Some(tile) = self
                            .tiles
                            .iter_mut()
                            .find(|tile| tile.id == id && !tile.previous)
                    {
                        tile.saved = true;
                    } else {
                        self.unavailable(resource, now);
                    }
                    return None;
                }
                if matches!(resource, Resource::Index(IndexFetch::Open)) {
                    // Older owners do not report age and ignore the newer request flags.
                    self.supports_kept = value.get("age").is_some();
                }
                let valid = handle(&value["resource"]).zip(
                    value["bytes"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|n| *n <= MAX_BYTES),
                );
                let Some((key, total)) = valid else {
                    self.unavailable(resource, now);
                    return None;
                };
                self.read(
                    resource,
                    value["age"].as_u64(),
                    key.to_owned(),
                    total,
                    Vec::new(),
                )
            }
            Pending::Read {
                resource,
                age,
                key,
                total,
                mut bytes,
            } => {
                let read = value["hex"]
                    .as_str()
                    .filter(|s| s.len() <= 2 * 16 * 1024)
                    .and_then(|s| hex::decode(s).ok());
                let Some(read) = read else {
                    self.unavailable(resource, now);
                    return None;
                };
                if read.len() > total.saturating_sub(bytes.len())
                    || (read.is_empty() && bytes.len() != total)
                {
                    self.unavailable(resource, now);
                    return None;
                }
                bytes.extend_from_slice(&read);
                if value["end"].as_bool() != Some(bytes.len() == total) {
                    self.unavailable(resource, now);
                    return None;
                }
                if bytes.len() != total {
                    return self.read(resource, age, key, total, bytes);
                }
                match resource {
                    Resource::Index(fetch) => {
                        let Some(meta) = Metadata::decode(&bytes) else {
                            self.unavailable(resource, now);
                            return None;
                        };
                        match fetch {
                            IndexFetch::Open => {
                                self.index_refresh_at = refresh_at(now, age, INDEX_AGE);
                                self.metadata = Some(meta);
                            }
                            IndexFetch::Check => {
                                self.refreshed_index = true;
                                if self
                                    .metadata
                                    .as_ref()
                                    .is_some_and(|old| old.prefix != meta.prefix)
                                {
                                    for tile in &mut self.tiles {
                                        tile.previous = true;
                                    }
                                    self.previous = self.metadata.take();
                                    self.probed.clear();
                                    self.failed.clear();
                                    self.changed();
                                }
                                self.metadata = Some(meta);
                                self.index_needs_save = true;
                            }
                            IndexFetch::Save => {
                                // Continue using the revision we staged. The provider can
                                // change again between the two HTTP requests.
                                self.index_needs_save = false;
                            }
                        }
                    }
                    Resource::Tile(load) => {
                        if let Some(decoder) = tile::Decode::start(load.id, bytes) {
                            let loaded = Loaded {
                                tile: load,
                                refresh_at: refresh_at(now, age, TILE_AGE),
                                saved: age.is_some() || load.fetch == Fetch::Kept,
                            };
                            self.decoding = Some((loaded, decoder));
                        } else {
                            self.unavailable(resource, now);
                        }
                    }
                    Resource::ConfirmCopy(_) => {}
                }
                None
            }
        }
    }

    fn read(
        &mut self,
        resource: Resource,
        age: Option<u64>,
        key: String,
        total: usize,
        bytes: Vec<u8>,
    ) -> Option<Call> {
        let args = json!({"resource":key,"offset":bytes.len()});
        self.pending = Some(Pending::Read {
            resource,
            age,
            key,
            total,
            bytes,
        });
        Some(call("read-resource", args, 4000))
    }

    fn unavailable(&mut self, resource: Resource, now: u64) {
        let after = now.saturating_add(RETRY_AFTER);
        let quiet = match resource {
            Resource::Index(IndexFetch::Check | IndexFetch::Save) => {
                self.retry_refresh = after;
                true
            }
            Resource::Index(IndexFetch::Open) => {
                self.retry = after;
                false
            }
            Resource::ConfirmCopy(_) => {
                // Do not replace the saved index in this session if a tile could not be saved.
                self.index_needs_save = false;
                true
            }
            Resource::Tile(TileLoad {
                fetch: Fetch::Kept, ..
            }) => true,
            Resource::Tile(TileLoad {
                fetch: Fetch::Refresh,
                ..
            }) => {
                self.retry_refresh = after;
                true
            }
            Resource::Tile(load) => {
                if self.failed.len() >= MAX_CACHED_TILES {
                    self.failed.remove(0);
                }
                self.failed.push((load.id, after));
                load.quiet
            }
        };
        if !quiet {
            self.notice =
                Some("Some online detail is unavailable. The offline globe is still shown.");
        }
    }

    /// Reproject only the previously accepted level, never the in-flight replacement.
    pub fn preview_drawing(
        &self,
        view: View,
        frame: Frame,
        camera: &Camera,
    ) -> Option<tile::Drawing> {
        if !self.ready() || self.displayed.is_empty() || view.span > FIRST_DETAIL {
            return None;
        }
        let displayed = self.displayed.iter().filter_map(|id| {
            self.tiles
                .iter()
                .find(|cached| cached.id == *id)
                .map(|cached| &cached.tile)
        });
        Some(tile::Drawing::render_on(
            displayed,
            view,
            frame,
            camera,
            crate::map_label_bounds(frame),
        ))
    }

    pub fn with_drawing<T>(
        &self,
        view: View,
        frame: Frame,
        draw: impl FnOnce(Option<(&tile::Drawing, bool)>) -> T,
    ) -> T {
        if view.span > FIRST_DETAIL || !self.ready() || self.tiles.is_empty() {
            return draw(None);
        }
        let mut cache = self.cached.borrow_mut();
        if !cache
            .as_ref()
            .is_some_and(|cached| cached.at == (view, frame))
        {
            let visible = self.visible(view, frame);
            let (tiles, complete) = self.select(&visible);
            let camera = Camera::new(view, frame);
            let drawing = tile::Drawing::render_on(
                tiles.iter().map(|cached| &cached.tile),
                view,
                frame,
                &camera,
                crate::map_label_bounds(frame),
            );
            let used = tiles.into_iter().map(|cached| cached.id).collect();
            *cache = Some(CachedDrawing {
                at: (view, frame),
                drawing,
                complete,
                used,
            });
        }
        draw(
            cache
                .as_ref()
                .map(|cached| (&cached.drawing, cached.complete)),
        )
    }

    fn select(&self, visible: &[tile::Key]) -> (Vec<&Cached>, bool) {
        let Some(first) = visible.first() else {
            return (Vec::new(), false);
        };
        // A finer tile covers a fraction of its ancestor; count those fractions exactly.
        // This preserves a complete parent/child level until its replacement covers the view.
        const CELL: u64 = 1 << 40;
        let mut levels = [0u64; 21];
        for tile in &self.tiles {
            let id = tile.id;
            for &key in visible {
                if contains(id, key) {
                    levels[usize::from(id.z)] += CELL;
                } else if contains(key, id) && !self.current_view_only {
                    levels[usize::from(id.z)] += CELL >> (2 * (id.z - key.z));
                }
            }
        }
        let filtered = |z: usize| !self.current_view_only || z == usize::from(first.z);
        let Some((z, coverage)) = levels
            .into_iter()
            .enumerate()
            .filter(|&(z, area)| area > 0 && filtered(z))
            .max_by_key(|&(z, area)| (area, std::cmp::Reverse(z.abs_diff(usize::from(first.z)))))
        else {
            return (Vec::new(), false);
        };
        let mut tiles: Vec<_> = self
            .tiles
            .iter()
            .filter(|tile| usize::from(tile.id.z) == z && Self::still_covers(visible, tile.id))
            .collect();
        // Preserve the centre-first request order independent of cache insertion order.
        tiles.sort_by_key(|tile| {
            visible
                .iter()
                .position(|&key| contains(tile.id, key) || contains(key, tile.id))
        });
        (tiles, coverage >= CELL * visible.len() as u64)
    }
}

fn refresh_at(now: u64, age: Option<u64>, limit: u64) -> Option<u64> {
    age.map(|age| now.saturating_add((limit + 1).saturating_sub(age) * 1000))
}

fn at_most_zoom(key: tile::Key, zoom: u8) -> tile::Key {
    let shift = key.z.saturating_sub(zoom);
    tile::Key {
        z: key.z - shift,
        x: key.x >> shift,
        y: key.y >> shift,
    }
}

fn contains(parent: tile::Key, child: tile::Key) -> bool {
    let Some(shift) = child.z.checked_sub(parent.z) else {
        return false;
    };
    (child.x >> shift, child.y >> shift) == (parent.x, parent.y)
}

fn handle(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty() && s.len() <= 80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cached_parent_stays_visible_while_the_new_zoom_downloads() {
        let parent = tile::Key {
            z: 13,
            x: 4096,
            y: 4096,
        };
        let view = View {
            lon: 0.015,
            lat: -0.015,
            span: 0.045,
            roll: 0.,
        };
        let frame = Frame(888, 449);
        let requested = tile::visible(view, frame, 14);
        assert!(requested.iter().all(|tile| tile.z == 14));
        let mut online = Online {
            authorization: Some("approved".into()),
            metadata: Some(Metadata {
                prefix: "/planet/test".into(),
                zoom: 14,
            }),
            ..Online::default()
        };
        // One primary way inside the parent, crossing the requested viewport.
        let mut layer = b"\x0a\x0etransportation".to_vec();
        layer.extend(
            b"\x12\x11\x12\x02\x00\x00\x18\x02\x22\x09\x09\xc8\x01\x80\x20\x0a\x80\x38\x00",
        );
        layer.extend(b"\x1a\x05class\x22\x09\x0a\x07primary\x28\x80\x20");
        let mut data = vec![0x1a, layer.len() as u8];
        data.extend(layer);
        let mut decoder = tile::Decode::start(parent, data).unwrap();
        while !decoder.step().unwrap() {}
        online.store(
            Loaded {
                tile: TileLoad {
                    id: parent,
                    previous: false,
                    fetch: Fetch::Normal,
                    quiet: false,
                },
                refresh_at: None,
                saved: false,
            },
            decoder.finish(),
            &[],
            0,
        );
        let shows_roads =
            |d: Option<(&tile::Drawing, bool)>| d.is_some_and(|(drawing, _)| drawing.has_roads());
        assert!(online.with_drawing(view, frame, shows_roads));
        online.cache_recent(false);
        assert!(
            !online.with_drawing(view, frame, shows_roads),
            "with current-view-only enabled no old tile is displayed"
        );
    }

    const OLD: &[u8] =
        br#"{"tiles":["https://tiles.openfreemap.org/planet/old/{z}/{x}/{y}.pbf"],"maxzoom":14}"#;
    const NEW: &[u8] =
        br#"{"tiles":["https://tiles.openfreemap.org/planet/new/{z}/{x}/{y}.pbf"],"maxzoom":14}"#;
    const ROAD: &[u8] = b"\x1a\x38\x0a\x0etransportation\x12\x11\x12\x02\x00\x00\x18\x02\x22\x09\x09\xc8\x01\x80\x20\x0a\x80\x38\x00\x1a\x05class\x22\x09\x0a\x07primary\x28\x80\x20";
    const VIEW: View = View {
        lon: 0.011,
        lat: -0.011,
        span: 0.002,
        roll: 0.,
    };
    const FRAME: Frame = Frame(640, 500);

    fn delivery(bytes: &[u8], age: Option<u64>, now: u64, online: &mut Online) {
        let read = online
            .result(
                &json!({"status":"ok","value":{"resource":"fixture",
            "bytes":bytes.len(),"age":age}}),
                now,
            )
            .unwrap();
        assert_eq!(read.method, "read-resource");
        assert!(
            online
                .result(
                    &json!({"status":"ok","value":{"hex":hex::encode(bytes),
            "end":true}}),
                    now
                )
                .is_none()
        );
    }

    fn decode_tile(online: &mut Online, now: u64) {
        for _ in 0..12 {
            if !online.decode_pending() {
                return;
            }
            online.tick(now, VIEW, FRAME);
        }
        panic!("tile did not decode in the turn budget");
    }

    fn shown_ids(online: &Online) -> Vec<tile::Key> {
        online.with_drawing(VIEW, FRAME, |_| ());
        online.shown_keys(VIEW, FRAME)
    }

    fn warm(index_age: u64, tile_age: u64) -> Online {
        let mut online = Online {
            authorization: Some("fixture".into()),
            ..Online::default()
        };
        let index = online.advance(0, VIEW, FRAME).unwrap();
        assert_eq!(index.args["path"], ROOT);
        assert_eq!(index.args["keep"], true);
        delivery(OLD, Some(index_age), 0, &mut online);
        let tile = online.advance_cached(0, VIEW, FRAME).unwrap();
        assert_eq!(tile.args["kept_only"], true);
        delivery(ROAD, Some(tile_age), 0, &mut online);
        decode_tile(&mut online, 0);
        assert!(!shown_ids(&online).is_empty());
        online
    }

    #[test]
    fn a_new_index_is_saved_only_after_the_replacement_tile_reaches_the_canvas() {
        let mut online = warm(INDEX_AGE + 1, TILE_AGE + 1);
        let old = shown_ids(&online);
        assert!(
            online.advance(1_000, VIEW, FRAME).is_none(),
            "no refresh before publishing"
        );
        online.frame_shown(VIEW, FRAME);
        let refresh = online.advance(1_000, VIEW, FRAME).unwrap();
        assert_eq!(refresh.args["path"], ROOT);
        assert_eq!(refresh.args["refresh"], true);
        assert_eq!(
            refresh.args["keep"], false,
            "an interrupted migration must leave the saved index intact"
        );
        delivery(NEW, None, 1_000, &mut online);
        assert_eq!(
            shown_ids(&online),
            old,
            "changing the index does not blank the viewport"
        );
        let probe = online.advance_cached(1_000, VIEW, FRAME).unwrap();
        assert!(
            probe.args["path"]
                .as_str()
                .unwrap()
                .starts_with("/planet/new/")
        );
        assert_eq!(probe.args["kept_only"], true);
        online.result(&json!({"status":"error","code":"unavailable"}), 1_000);
        assert!(
            online.advance(1_100, VIEW, FRAME).is_none(),
            "network pacing stays in effect"
        );
        let replacement = online.advance(1_250, VIEW, FRAME).unwrap();
        assert_eq!(replacement.args["path"], probe.args["path"]);
        assert!(replacement.args.get("kept_only").is_none());
        delivery(ROAD, None, 1_250, &mut online);
        assert_eq!(
            shown_ids(&online),
            old,
            "the decoded replacement is not ready yet"
        );
        assert!(online.tiles.front().unwrap().previous);
        decode_tile(&mut online, 1_250);
        assert_eq!(
            shown_ids(&online),
            old,
            "the same geographic footprint is preserved"
        );
        assert!(!online.tiles.front().unwrap().previous);
        assert!(
            online.advance(1_500, VIEW, FRAME).is_none(),
            "the replacement has not reached the canvas yet"
        );
        online.frame_shown(VIEW, FRAME);
        let check = online.advance(1_500, VIEW, FRAME).unwrap();
        assert_eq!(check.args["path"], replacement.args["path"]);
        assert_eq!(check.args["kept_only"], true);
        assert!(online.result(&json!({"status":"ok","value":{"resource":"fixture","bytes":ROAD.len(),"age":0}}), 1_500).is_none());
        let save = online.advance(1_500, VIEW, FRAME).unwrap();
        assert_eq!(save.args["path"], ROOT);
        assert_eq!(save.args["refresh"], true);
        assert_eq!(save.args["keep"], true);
        delivery(NEW, None, 1_500, &mut online);
        assert!(
            online.advance(5_000, VIEW, FRAME).is_none(),
            "the index is refreshed only once"
        );
    }

    #[test]
    fn a_new_version_cache_miss_checks_the_previous_version_without_the_network() {
        let mut online = Online {
            authorization: Some("fixture".into()),
            metadata: Metadata::decode(NEW),
            previous: Metadata::decode(OLD),
            supports_kept: true,
            refreshed_index: true,
            ..Online::default()
        };
        let probe = online.advance(0, VIEW, FRAME).unwrap();
        assert!(
            probe.args["path"]
                .as_str()
                .unwrap()
                .starts_with("/planet/new/")
        );
        assert_eq!(probe.args["kept_only"], true);
        online.result(&json!({"status":"error","code":"unavailable"}), 0);
        let fallback = online.advance_cached(0, VIEW, FRAME).unwrap();
        assert!(
            fallback.args["path"]
                .as_str()
                .unwrap()
                .starts_with("/planet/old/")
        );
        assert_eq!(fallback.args["kept_only"], true);
        delivery(ROAD, Some(TILE_AGE + 1), 0, &mut online);
        decode_tile(&mut online, 0);
        let shown = shown_ids(&online);
        assert!(!shown.is_empty());
        let replacement = online.advance(0, VIEW, FRAME).unwrap();
        assert_eq!(replacement.args["path"], probe.args["path"]);
        online.result(&json!({"status":"error","code":"unavailable"}), 0);
        assert_eq!(shown_ids(&online), shown);
        assert!(
            online.notice().is_none(),
            "the cached tile remains usable without a refresh"
        );
    }

    #[test]
    fn an_old_tile_refresh_is_quiet_and_new_visible_copies_have_priority() {
        let mut online = warm(10, TILE_AGE + 1);
        let initial = shown_ids(&online);
        online.frame_shown(VIEW, FRAME);
        let refresh = online.advance(250, VIEW, FRAME).unwrap();
        assert_eq!(refresh.args["refresh"], true);
        assert!(refresh.args["path"].as_str().unwrap().ends_with(".pbf"));
        assert!(
            online.advance(500, VIEW, FRAME).is_none(),
            "only one refresh in flight"
        );
        online.result(&json!({"status":"error","code":"unavailable"}), 500);
        assert!(online.notice().is_none());
        assert_eq!(shown_ids(&online), initial);
        assert!(
            online.advance(1_000, VIEW, FRAME).is_none(),
            "no immediate retry"
        );
        let visited = View { lon: 0.035, ..VIEW };
        let pending = online.advance(1_000, visited, FRAME).unwrap();
        assert_eq!(pending.args["kept_only"], true);
        online.result(&json!({"status":"error","code":"unavailable"}), 1_000);
        let pending = online.advance(1_000, visited, FRAME).unwrap();
        assert!(pending.args.get("kept_only").is_none());
        online.sent(42);
        assert_eq!(
            online.cached_first(1_000, View { lon: 0.06, ..VIEW }, FRAME),
            42
        );
        assert_eq!(shown_ids(&online), initial);
    }

    #[test]
    fn a_cache_lookup_preempts_the_index_without_losing_the_refresh() {
        let mut online = warm(INDEX_AGE + 1, 10);
        online.frame_shown(VIEW, FRAME);
        let staged = online.advance(250, VIEW, FRAME).unwrap();
        assert_eq!(staged.args["refresh"], true);
        assert_eq!(staged.args["keep"], false);
        online.sent(42);
        assert_eq!(
            online.cached_first(260, View { lon: 0.06, ..VIEW }, FRAME),
            42
        );
        assert!(online.advance(300, VIEW, FRAME).is_none());
        let retry = online.advance(260 + RETRY_AFTER, VIEW, FRAME).unwrap();
        assert_eq!(retry.args, staged.args);
    }

    #[test]
    fn a_failed_index_check_retries_and_can_stage_the_new_revision() {
        let mut online = warm(INDEX_AGE + 1, 10);
        online.frame_shown(VIEW, FRAME);
        let first = online.advance(250, VIEW, FRAME).unwrap();
        assert_eq!(first.args["path"], ROOT);
        assert_eq!(first.args["refresh"], true);
        assert_eq!(first.args["keep"], false);
        let old = shown_ids(&online);
        online.result(&json!({"status":"error","code":"unavailable"}), 260);
        assert!(online.advance(259 + RETRY_AFTER, VIEW, FRAME).is_none());
        assert_eq!(shown_ids(&online), old);
        let retry_at = 260 + RETRY_AFTER;
        let retry = online.advance(retry_at, VIEW, FRAME).unwrap();
        assert_eq!(retry.args, first.args);
        delivery(NEW, None, retry_at, &mut online);
        assert_eq!(shown_ids(&online), old);
        let staged = online.advance_cached(retry_at, VIEW, FRAME).unwrap();
        assert!(
            staged.args["path"]
                .as_str()
                .unwrap()
                .starts_with("/planet/new/")
        );
    }

    #[test]
    fn a_painted_exact_tile_does_not_pin_its_hidden_parent_or_child() {
        let mut online = warm(10, 10);
        let exact = online.tiles.front().unwrap().id;
        let parent = at_most_zoom(exact, exact.z - 1);
        let child = tile::Key {
            z: exact.z + 1,
            x: exact.x * 2,
            y: exact.y * 2,
        };
        online.frame_shown(VIEW, FRAME);
        assert!(online.on_screen(exact));
        assert!(!online.on_screen(parent));
        assert!(!online.on_screen(child));
    }
}
