//! Private extension namespaces and short-lived local data claims. The owner never performs disk
//! I/O on its dispatch loop, and the contract never accepts a path or exposes a descriptor.
use crate::{platform_services::Snapshot, private_fs};
use peersus_api::{extension_data as api, message::Outcome};
use peersus_core::{model::PeerId, plugins::PluginId};
use rand::RngExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) const CLAIM_BIT: u64 = 1 << 62;
const CALLS: usize = 8;
const CLAIMS: usize = 32;
const STAGES: usize = 10;
const CLAIM_TTL: Duration = Duration::from_secs(5);
const STAGE_TTL: Duration = Duration::from_secs(300);
const INDEX_LIMIT: u64 = 4 * 1024 * 1024;
const INVALID: Fault = Fault("invalid", "Invalid extension data request");
const IO: Fault = Fault("unavailable", "Private extension storage is unavailable");
const STALE: Fault = Fault("stale", "The extension data revision changed");
const QUOTA: Fault = Fault("quota", "The private extension storage limit was reached");
const REVOKED: Fault = Fault("cancelled", "The operation is no longer authorized");
const BUSY: Fault = Fault("busy", "Too many extension data operations are in progress");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fault(&'static str, &'static str);
impl Fault {
    pub(crate) fn outcome(self) -> Outcome {
        Outcome::error(self.0, self.1)
    }
}
impl From<std::io::Error> for Fault {
    fn from(_: std::io::Error) -> Self {
        IO
    }
}
fn parse<T: DeserializeOwned>(args: Value) -> Result<T, Fault> {
    serde_json::from_value(args).map_err(|_| INVALID)
}
fn value(value: impl Serialize) -> Result<Value, Fault> {
    serde_json::to_value(value).map_err(|_| IO)
}
fn lock<T>(value: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, Fault> {
    value.lock().map_err(|_| IO)
}
fn sha(input: &[u8]) -> String {
    hex::encode(Sha256::digest(input))
}
fn fresh() -> [u8; 32] {
    rand::rng().random()
}
pub(crate) fn random_scope() -> api::ScopeId {
    // Encoded 32 bytes are exactly the grammar of ScopeId.
    api::ScopeId::try_from(hex::encode(fresh())).expect("hex encoding has a fixed grammar")
}

/// Publication linearizes after the last live check; retiring a lease never waits on disk.
#[derive(Clone)]
pub(crate) struct Fence(Arc<AtomicBool>);
impl Fence {
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
    pub(crate) fn revoke(&self) {
        self.0.store(false, Ordering::Release);
    }
    fn check(&self) -> Result<(), Fault> {
        self.0.load(Ordering::Acquire).then_some(()).ok_or(REVOKED)
    }
}

struct CancelOnDrop(Fence);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.revoke();
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Index {
    revision: u64,
    records: BTreeMap<api::Key, Record>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    object: api::ScopeId,
    bytes: u64,
}
struct Pending {
    file: tempfile::NamedTempFile,
    bytes: u64,
    written: u64,
    hash: Sha256,
    sealed: Option<api::ScopeId>,
    created: Instant,
    fence: Fence,
}
#[derive(Default)]
struct State {
    index: Option<Index>,
    stages: BTreeMap<u64, Pending>,
    approved: Option<Fence>,
}
struct Slot {
    path: PathBuf,
    state: Mutex<State>,
}
#[derive(Clone)]
pub(crate) struct Access {
    caller: PluginId,
    slot: Arc<Slot>,
    fence: Fence,
    approval: Option<PathBuf>,
}
struct Claim {
    access: Access,
    data: ClaimData,
    expiry: Instant,
}
enum ClaimData {
    Read(Vec<u8>),
    Append(api::Append),
}
#[derive(Default)]
struct Owners {
    slots: BTreeMap<PluginId, Access>,
    claims: BTreeMap<u64, Claim>,
}

#[derive(Clone)]
pub(crate) struct Storage {
    root: PathBuf,
    owners: Arc<Mutex<Owners>>,
    workers: Arc<tokio::sync::Semaphore>,
}
impl Storage {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            owners: Arc::new(Mutex::new(Owners::default())),
            workers: Arc::new(tokio::sync::Semaphore::new(CALLS)),
        }
    }
    pub(crate) fn access(&self, caller: PluginId) -> Result<Access, Fault> {
        let mut owners = lock(&self.owners)?;
        if !owners.slots.contains_key(&caller) && owners.slots.len() >= 128 {
            return Err(QUOTA);
        }
        let current = owners
            .slots
            .entry(caller.clone())
            .or_insert_with(|| Access {
                slot: Arc::new(Slot {
                    path: self.root.join(sha(caller.as_str().as_bytes())),
                    state: Mutex::new(State::default()),
                }),
                caller,
                fence: Fence::new(),
                approval: None,
            });
        Ok(current.clone())
    }
    /// Retire handles immediately. A shared disk slot serializes a previous publisher with its successor.
    pub(crate) fn revoke(&self, caller: &PluginId) {
        if let Ok(mut owners) = self.owners.lock() {
            if let Some(access) = owners.slots.get_mut(caller) {
                access.fence.revoke();
                access.fence = Fence::new();
                // Old stage handles must not survive to the new generation. A concurrent disk
                // operation sees the old fence; cleanup runs on its own bounded stock of stages.
                if let Ok(mut state) = access.slot.state.try_lock() {
                    state.stages.clear();
                }
            }
            owners
                .claims
                .retain(|_, entry| &entry.access.caller != caller);
        }
    }
    /// Package removal already retired the on-disk namespace under the package lock.
    pub(crate) fn forget(&self, caller: &PluginId) {
        self.revoke(caller);
        if let Ok(mut owners) = self.owners.lock() {
            owners.slots.remove(caller);
        }
    }
    async fn work<T: Send + 'static>(
        &self,
        run: impl FnOnce() -> Result<T, Fault> + Send + 'static,
    ) -> Result<T, Fault> {
        let permit = self.workers.clone().try_acquire_owned().map_err(|_| BUSY)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            run()
        })
        .await
        .map_err(|_| IO)?
    }
    pub(crate) async fn dispatch(
        self,
        access: Access,
        method: String,
        args: Value,
        receive: Option<Fence>,
    ) -> Outcome {
        let store = self.clone();
        let live = Fence::new();
        let _cancel = CancelOnDrop(live.clone());
        let result = self
            .work(move || store.execute(access, &method, args, receive, &live))
            .await;
        result.map_or_else(Fault::outcome, Outcome::ok)
    }
    /// The whole of one of `access`'s committed objects or sealed stages, which must be exactly `bytes` long: what
    /// `peersus.pictures` keeps. Read on a worker, never on the owner's dispatch loop.
    pub(crate) async fn read_whole(
        &self,
        access: Access,
        source: api::Source,
        bytes: usize,
    ) -> Result<Vec<u8>, Fault> {
        self.work(move || {
            let mut state = access.state()?;
            let (mut file, total) = state.source(&access, &access.slot.path, source)?;
            if total != bytes as u64 {
                return Err(INVALID);
            }
            let mut data = vec![0; bytes];
            file.read_exact(&mut data)?;
            Ok(data)
        })
        .await
    }
    fn execute(
        &self,
        access: Access,
        method: &str,
        args: Value,
        receive: Option<Fence>,
        live: &Fence,
    ) -> Result<Value, Fault> {
        let mut state = access.state()?;
        live.check()?;
        let path = &access.slot.path;
        match method {
            "list" => {
                let args: api::List = parse(args)?;
                if !(1..=api::MAX_LIST).contains(&args.limit) {
                    return Err(INVALID);
                }
                let index = state.index.as_ref().ok_or(IO)?;
                let mut keys: Vec<_> = index
                    .records
                    .keys()
                    .filter(|key| {
                        args.after.as_ref().is_none_or(|after| *key > after)
                            && args.prefix.as_ref().is_none_or(|prefix| key.under(prefix))
                    })
                    .take(args.limit + 1)
                    .cloned()
                    .collect();
                let more = keys.len() > args.limit;
                keys.truncate(args.limit);
                value(api::Listing {
                    revision: index.revision,
                    keys,
                    more,
                })
            }
            "read" => {
                let args: api::Read = parse(args)?;
                if !(1..=api::MAX_CHUNK).contains(&args.length) {
                    return Err(INVALID);
                }
                let (mut file, total) = state.source(&access, path, args.source)?;
                let left = total.checked_sub(args.offset).ok_or(INVALID)?;
                let len = left.min(u64::from(args.length)) as usize;
                file.seek(SeekFrom::Start(args.offset))?;
                let mut data = vec![0; len];
                file.read_exact(&mut data)?;
                self.claim(access.clone(), ClaimData::Read(data), len as u64)
            }
            "stage" => {
                let request: api::Stage = parse(args)?;
                let stage = state.begin(&access, request.bytes)?;
                value(api::Staged { stage })
            }
            "append" => {
                let request: api::Append = parse(args)?;
                state.can_append(&access, &request)?;
                let bytes = u64::from(request.length);
                self.claim(access.clone(), ClaimData::Append(request), bytes)
            }
            "seal" => {
                let request: api::Staged = parse(args)?;
                value(state.seal(&access, request.stage)?)
            }
            "abort" => {
                let request: api::Staged = parse(args)?;
                state.stage(&access, request.stage)?;
                state.stages.remove(&request.stage);
                Ok(json!({}))
            }
            "commit" => {
                let request: api::Commit = parse(args)?;
                if !request.valid() || request.receive_ticket.is_some() != receive.is_some() {
                    return Err(INVALID);
                }
                let revision = state.commit(
                    &access,
                    request,
                    receive.as_ref().into_iter().chain(Some(live)),
                )?;
                Ok(json!({"revision":revision}))
            }
            _ => Err(INVALID),
        }
    }
    fn claim(&self, access: Access, data: ClaimData, bytes: u64) -> Result<Value, Fault> {
        let mut owners = lock(&self.owners)?;
        let now = Instant::now();
        owners
            .claims
            .retain(|_, entry| entry.expiry > now && entry.access.fence.check().is_ok());
        access.fence.check()?;
        if owners.claims.len() >= CLAIMS
            || owners
                .claims
                .values()
                .filter(|entry| entry.access.caller == access.caller)
                .count()
                >= 4
        {
            return Err(BUSY);
        }
        let claim = rand::rng().random::<u64>() & (CLAIM_BIT - 1) | CLAIM_BIT;
        if owners.claims.contains_key(&claim) {
            return Err(BUSY);
        }
        owners.claims.insert(
            claim,
            Claim {
                access,
                data,
                expiry: now + CLAIM_TTL,
            },
        );
        value(api::DataClaim { claim, bytes })
    }
    pub(crate) fn accept(
        &self,
        caller: &PluginId,
        id: u64,
        mut stream: peersus_plugin::transport::Stream,
    ) {
        let Ok(mut owners) = self.owners.lock() else {
            return;
        };
        if owners
            .claims
            .get(&id)
            .is_none_or(|claim| &claim.access.caller != caller)
        {
            return;
        }
        let Some(claim) = owners.claims.remove(&id) else {
            return;
        };
        drop(owners);
        if claim.expiry <= Instant::now() || claim.access.fence.check().is_err() {
            return;
        }
        let Ok(permit) = self.workers.clone().try_acquire_owned() else {
            return;
        };
        tokio::spawn(async move {
            let _ = tokio::time::timeout(CLAIM_TTL, async move {
                match claim.data {
                    ClaimData::Read(data) => {
                        let _permit = permit;
                        if claim.access.fence.check().is_ok() {
                            let _ = stream.write_all(&data).await;
                        }
                    }
                    ClaimData::Append(request) => {
                        let mut data = Vec::new();
                        if (&mut stream)
                            .take(u64::from(request.length) + 1)
                            .read_to_end(&mut data)
                            .await
                            .is_err()
                            || data.len() != request.length as usize
                        {
                            return;
                        }
                        let stored = tokio::task::spawn_blocking(move || {
                            let _permit = permit;
                            claim.access.state()?.append(&claim.access, &request, &data)
                        })
                        .await;
                        if matches!(stored, Ok(Ok(()))) {
                            let _ = stream.write_all(&[1]).await;
                        }
                    }
                }
                let _ = stream.shutdown().await;
            })
            .await;
        });
    }
    pub(crate) async fn import_selected(
        &self,
        access: Access,
        selected: Arc<Snapshot>,
    ) -> Result<api::Sealed, Fault> {
        let (bytes, digest) = (selected.bytes(), hex::encode(selected.sha256()));
        self.import(access, bytes, digest, move |pos, len| {
            selected.read_part(pos, len).map_err(|_| IO)
        })
        .await
    }
    /// Stage bytes the application made for the caller itself (a recording), sealed as an object the caller then commits
    /// or aborts like any other stage.
    pub(crate) async fn import_bytes(
        &self,
        access: Access,
        bytes: Vec<u8>,
    ) -> Result<api::Sealed, Fault> {
        let digest = hex::encode(Sha256::digest(&bytes));
        let length = bytes.len() as u64;
        self.import(access, length, digest, move |pos, len| {
            Ok(bytes[pos as usize..(pos + len) as usize].to_vec())
        })
        .await
    }
    async fn import(
        &self,
        access: Access,
        bytes: u64,
        digest: String,
        read_part: impl Fn(u64, u64) -> Result<Vec<u8>, Fault> + Send + 'static,
    ) -> Result<api::Sealed, Fault> {
        let live = Fence::new();
        let _cancel = CancelOnDrop(live.clone());
        self.work(move || {
            let mut state = access.state()?;
            live.check()?;
            let id = state.begin(&access, bytes)?;
            let result = (|| {
                let mut pos = 0;
                while pos < bytes {
                    live.check()?;
                    let len = (bytes - pos).min(u64::from(api::MAX_CHUNK)) as u32;
                    let part = read_part(pos, u64::from(len))?;
                    state.append(
                        &access,
                        &api::Append {
                            stage: id,
                            offset: pos,
                            length: len,
                        },
                        &part,
                    )?;
                    pos += u64::from(len);
                }
                live.check()?;
                let sealed = state.seal(&access, id)?;
                if sealed.sha256.as_str() != digest {
                    return Err(IO);
                }
                Ok(sealed)
            })();
            if result.is_err() {
                state.stages.remove(&id);
            }
            result
        })
        .await
    }
    pub(crate) async fn export_source(
        &self,
        access: Access,
        key: api::Key,
        revision: u64,
    ) -> Result<(File, u64), Fault> {
        self.work(move || access.state()?.open(&access.slot.path, &key, revision))
            .await
    }
    pub(crate) async fn export_copy(
        &self,
        mut source: File,
        mut destination: File,
        bytes: u64,
        guard: Fence,
    ) -> Result<(), Fault> {
        use std::io::Read as _;
        self.work(move || {
            let mut remaining = bytes;
            let mut buffer = [0; 32 * 1024];
            while remaining != 0 {
                guard.check()?;
                let len = remaining.min(buffer.len() as u64) as usize;
                source.read_exact(&mut buffer[..len])?;
                destination.write_all(&buffer[..len])?;
                remaining -= len as u64;
            }
            destination.flush()?;
            destination.sync_all()?;
            Ok(())
        })
        .await
    }
    pub(crate) async fn references(
        &self,
        access: Access,
        peers: Vec<(String, PeerId, u64)>,
        local: Option<(PeerId, String)>,
        scope: Option<api::ScopeId>,
    ) -> Result<api::ReferenceSet, Fault> {
        let root = self.root.clone();
        self.work(move || {
            let _state = access.state()?;
            let key = secret(&access.slot.path, "reference-key")?;
            let pair_dir = root.join("pair-incarnations");
            private_dir(&pair_dir)?;
            let scoped = |id: &PeerId, scope: &api::ScopeId| {
                keyed(
                    scope.as_str().as_bytes(),
                    b"scoped-peer-v1",
                    &[access.caller.as_str().as_bytes(), id.0.as_bytes()],
                )
            };
            let mut result = Vec::with_capacity(peers.len());
            for (device, id, paired_at) in peers {
                let incarnation = secret(&pair_dir, &sha(id.0.as_bytes()))?;
                result.push(api::PeerReference {
                    device,
                    local_ref: keyed(
                        &key,
                        b"local-peer-v1",
                        &[&incarnation, id.0.as_bytes(), &paired_at.to_be_bytes()],
                    )?,
                    scoped_ref: scope.as_ref().map(|scope| scoped(&id, scope)).transpose()?,
                });
            }
            let local = match local.zip(scope) {
                Some(((id, name), scope)) => Some(api::LocalReference {
                    scoped_ref: scoped(&id, &scope)?,
                    name,
                }),
                None => None,
            };
            access.fence.check()?;
            Ok(api::ReferenceSet {
                peers: result,
                local,
            })
        })
        .await
    }
    pub(crate) async fn forget_peer(&self, peer: &PeerId) -> Result<(), Fault> {
        let path = self
            .root
            .join("pair-incarnations")
            .join(sha(peer.0.as_bytes()));
        self.work(move || match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        })
        .await
    }
}

impl Access {
    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>, Fault> {
        self.fence.check()?;
        let mut state = lock(&self.slot.state)?;
        self.fence.check()?;
        let now = Instant::now();
        if state.index.is_none() {
            private_dir(self.slot.path.parent().ok_or(IO)?)?;
            private_dir(&self.slot.path)?;
        }
        if let Some(program) = &self.approval
            && state
                .approved
                .as_ref()
                .is_none_or(|approved| !Arc::ptr_eq(&approved.0, &self.fence.0))
        {
            let folder = program.parent().ok_or(IO)?;
            let approval = crate::plugin_approval::read(folder).ok_or(IO)?;
            if program.file_name().and_then(|name| name.to_str()) != Some(&approval.program) {
                return Err(IO);
            }
            let bound = sha(format!(
                "{}:{}:{}",
                approval.sha256, approval.approved_at, approval.program
            )
            .as_bytes());
            bind(&self.slot.path, &bound)?;
            state.approved = Some(self.fence.clone());
        }
        if state.index.is_none() {
            // The program identity was checked before a private index is opened.
            state.index = Some(open_index(&self.slot.path)?);
        }
        state
            .stages
            .retain(|_, s| s.fence.check().is_ok() && now.duration_since(s.created) < STAGE_TTL);
        Ok(state)
    }
    pub(crate) fn for_program(mut self, program: PathBuf) -> Self {
        self.approval = Some(program);
        self
    }
}
fn private_dir(path: &Path) -> Result<(), Fault> {
    match private_fs::private_folder().create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let m = fs::symlink_metadata(path)?;
            if m.is_dir() && !private_fs::is_link(&m) && !private_fs::others_can_change(&m) {
                Ok(())
            } else {
                Err(IO)
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// A successful package removal can never leave a live namespace at this installed name.
pub(crate) fn retire_namespace(data: &Path, id: &str) -> std::io::Result<()> {
    let root = data.join("extension-data");
    match fs::symlink_metadata(&root) {
        Ok(meta)
            if meta.is_dir()
                && !private_fs::is_link(&meta)
                && !private_fs::others_can_change(&meta) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(_) => return Err(std::io::Error::other("Invalid extension data directory")),
    }
    let path = root.join(sha(id.as_bytes()));
    let retired = root.join(format!(".retired-{}", hex::encode(fresh())));
    match fs::rename(path, &retired) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    // A retired directory is never opened as a namespace, even if deletion fails.
    if fs::remove_dir_all(&retired).is_err() {
        tracing::warn!("Retired extension data could not be removed");
    }
    Ok(())
}

/// An approved update of the extension at `id` keeps its private data: the namespace was bound to the
/// program approved before, and the next access binds it to the program approved now. Called only by the
/// installer, after the new approval is in place and before the new program can start.
pub(crate) fn rebind_namespace(data: &Path, id: &str) -> std::io::Result<()> {
    let root = data.join("extension-data");
    let dir = root.join(sha(id.as_bytes()));
    for path in [&root, &dir] {
        match fs::symlink_metadata(path) {
            Ok(meta)
                if meta.is_dir()
                    && !private_fs::is_link(&meta)
                    && !private_fs::others_can_change(&meta) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
            Ok(_) => return Err(std::io::Error::other("Invalid extension data directory")),
        }
    }
    match fs::remove_file(dir.join("program-binding")) {
        Ok(()) => {
            #[cfg(unix)]
            File::open(&dir)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn bind(dir: &Path, approved: &str) -> Result<(), Fault> {
    let path = dir.join("program-binding");
    match private_fs::read_no_follow(&path) {
        Ok(mut file) => {
            let mut bytes = [0; 64];
            if !file.metadata()?.is_file() || file.metadata()?.len() != 64 {
                return Err(IO);
            }
            file.read_exact(&mut bytes)?;
            if bytes != approved.as_bytes() {
                return Err(REVOKED);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut temp = tempfile::NamedTempFile::new_in(dir)?;
            temp.write_all(approved.as_bytes())?;
            temp.as_file().sync_all()?;
            temp.persist_noclobber(path).map_err(|_| IO)?;
            sync_directory(dir)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
fn secret(dir: &Path, filename: &str) -> Result<[u8; 32], Fault> {
    let path = dir.join(filename);
    let read = || -> Result<[u8; 32], Fault> {
        let mut saved = [0; 32];
        let mut file = private_fs::read_no_follow(&path)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() != 32 {
            return Err(IO);
        }
        file.read_exact(&mut saved)?;
        Ok(saved)
    };
    match fs::symlink_metadata(&path) {
        Ok(_) => return read(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let data = fresh();
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&data)?;
    tmp.as_file().sync_all()?;
    match tmp.persist_noclobber(&path) {
        Ok(_) => {
            sync_directory(dir)?;
            Ok(data)
        }
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => read(),
        Err(_) => Err(IO),
    }
}
fn keyed(key: &[u8], domain: &[u8], parts: &[&[u8]]) -> Result<api::ScopeId, Fault> {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| IO)?;
    mac.update(domain);
    for part in parts {
        mac.update(&(part.len() as u64).to_be_bytes());
        mac.update(part);
    }
    api::ScopeId::try_from(hex::encode(mac.finalize().into_bytes())).map_err(|_| IO)
}
fn sync_directory(_dir: &Path) -> Result<(), Fault> {
    #[cfg(unix)]
    File::open(_dir)?.sync_all()?;
    Ok(())
}
fn object_path(dir: &Path, id: &api::ScopeId) -> PathBuf {
    dir.join(format!("o-{}", id.as_str()))
}
fn open_index(dir: &Path) -> Result<Index, Fault> {
    let index: Index = match private_fs::read_no_follow(&dir.join("index")) {
        Ok(file) => {
            if !file.metadata()?.is_file() || file.metadata()?.len() > INDEX_LIMIT {
                return Err(IO);
            }
            serde_json::from_reader(file.take(INDEX_LIMIT + 1)).map_err(|_| IO)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Index::default(),
        Err(error) => return Err(error.into()),
    };
    let live: BTreeSet<_> = index.records.values().map(|r| &r.object).collect();
    if live.len() != index.records.len() || !fits(&index.records) {
        return Err(IO);
    }
    // Reclaim only files created by this protocol and not reachable from the durable index.
    // No stage or scratch index is durable authority. Recover their bounded peak allocation
    // after an owner crash rather than letting leftovers accumulate outside the stage quota.
    for entry in fs::read_dir(dir)?.take(api::MAX_KEYS + 512) {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("stage-") || name.starts_with("index-"))
            && entry.file_type()?.is_file()
        {
            fs::remove_file(entry.path())?;
            continue;
        }
        if let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.strip_prefix("o-"))
            .and_then(|s| api::ScopeId::try_from(s.to_owned()).ok())
            && !live.contains(&id)
            && entry.file_type()?.is_file()
        {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(index)
}
fn fits(records: &BTreeMap<api::Key, Record>) -> bool {
    records.len() <= api::MAX_KEYS
        && records.values().all(|r| r.bytes <= api::MAX_OBJECT)
        && records.values().map(|r| r.bytes).sum::<u64>() <= api::MAX_BYTES
}
impl State {
    fn open(&self, dir: &Path, key: &api::Key, revision: u64) -> Result<(File, u64), Fault> {
        let index = self.index.as_ref().ok_or(IO)?;
        if index.revision != revision {
            return Err(STALE);
        }
        let row = index
            .records
            .get(key)
            .ok_or(Fault("missing", "The object does not exist"))?;
        let file = private_fs::read_no_follow(&object_path(dir, &row.object))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != row.bytes {
            return Err(IO);
        }
        Ok((file, row.bytes))
    }
    /// A committed object at an exact revision, or a sealed stage, opened for reading, with its length.
    fn source(
        &mut self,
        access: &Access,
        dir: &Path,
        source: api::Source,
    ) -> Result<(File, u64), Fault> {
        match source {
            api::Source::Key { key, revision } => self.open(dir, &key, revision),
            api::Source::Stage { stage } => {
                let stage = self.stage(access, stage)?;
                if stage.sealed.is_none() {
                    return Err(INVALID);
                }
                Ok((stage.file.reopen()?, stage.bytes))
            }
        }
    }
    fn begin(&mut self, access: &Access, bytes: u64) -> Result<u64, Fault> {
        access.fence.check()?;
        if bytes > api::MAX_OBJECT
            || self.stages.len() >= STAGES
            || self.stages.values().map(|s| s.bytes).sum::<u64>() + bytes > api::MAX_STAGED
        {
            return Err(QUOTA);
        }
        let id = rand::rng().random::<u64>();
        if id == 0 || self.stages.contains_key(&id) {
            return Err(BUSY);
        }
        self.stages.insert(
            id,
            Pending {
                file: tempfile::Builder::new()
                    .prefix("stage-")
                    .tempfile_in(&access.slot.path)?,
                bytes,
                written: 0,
                hash: Sha256::new(),
                sealed: None,
                created: Instant::now(),
                fence: access.fence.clone(),
            },
        );
        Ok(id)
    }
    fn stage(&mut self, access: &Access, id: u64) -> Result<&mut Pending, Fault> {
        access.fence.check()?;
        self.stages
            .get_mut(&id)
            .filter(|s| s.created.elapsed() < STAGE_TTL)
            .ok_or(INVALID)
    }
    fn can_append(&mut self, access: &Access, args: &api::Append) -> Result<(), Fault> {
        let stage = self.stage(access, args.stage)?;
        if !(1..=api::MAX_CHUNK).contains(&args.length)
            || stage.sealed.is_some()
            || args.offset != stage.written
            || args
                .offset
                .checked_add(u64::from(args.length))
                .is_none_or(|end| end > stage.bytes)
        {
            return Err(INVALID);
        }
        Ok(())
    }
    fn append(&mut self, access: &Access, args: &api::Append, data: &[u8]) -> Result<(), Fault> {
        self.can_append(access, args)?;
        if args.length as usize != data.len() {
            return Err(INVALID);
        }
        let stage = self.stage(access, args.stage)?;
        if stage.file.as_file_mut().write_all(data).is_err() {
            self.stages.remove(&args.stage);
            return Err(IO);
        }
        stage.hash.update(data);
        stage.written += u64::from(args.length);
        Ok(())
    }
    fn seal(&mut self, access: &Access, id: u64) -> Result<api::Sealed, Fault> {
        let stage = self.stage(access, id)?;
        if stage.written != stage.bytes {
            return Err(INVALID);
        }
        let hash =
            api::ScopeId::try_from(hex::encode(stage.hash.clone().finalize())).map_err(|_| IO)?;
        stage.file.as_file().sync_all()?;
        stage.sealed = Some(hash.clone());
        Ok(api::Sealed {
            stage: id,
            bytes: stage.bytes,
            sha256: hash,
        })
    }
    fn commit<'a>(
        &mut self,
        access: &Access,
        args: api::Commit,
        fences: impl IntoIterator<Item = &'a Fence>,
    ) -> Result<u64, Fault> {
        let dir = &access.slot.path;
        let mut removes: BTreeSet<_> = args.remove.iter().cloned().collect();
        if let Some(id) = args.remove_from {
            let stage = self.stage(access, id)?;
            if stage.sealed.is_none() || stage.bytes > api::MAX_REMOVE_BYTES {
                return Err(INVALID);
            }
            let list: Vec<api::Key> =
                serde_json::from_reader(stage.file.reopen()?.take(api::MAX_REMOVE_BYTES))
                    .map_err(|_| INVALID)?;
            if list.len() > api::MAX_REMOVE_KEYS
                || list.into_iter().any(|key| !removes.insert(key))
                || args.put.iter().any(|put| removes.contains(put.key()))
            {
                return Err(INVALID);
            }
        }
        let (revision, old) = {
            let index = self.index.as_ref().ok_or(IO)?;
            if index.revision != args.if_revision {
                return Err(STALE);
            }
            (
                index.revision.checked_add(1).ok_or(QUOTA)?,
                index.records.clone(),
            )
        };
        let mut records = old.clone();
        records.retain(|key, _| {
            !removes.contains(key) && !args.remove_prefix.iter().any(|p| key.under(p))
        });
        let mut allocated = Unpublished(Vec::new());
        let mut stages: Vec<_> = args.remove_from.into_iter().collect();
        let mut moved = BTreeSet::new();
        for put in args.put {
            let id = random_scope();
            let path = object_path(dir, &id);
            let (key, bytes) = match put {
                api::Put::Text { key, text } => {
                    let mut file = private_fs::new_file(0o600).open(&path)?;
                    allocated.0.push(Allocated::Created(path));
                    file.write_all(text.as_bytes())?;
                    file.sync_all()?;
                    (key, text.len() as u64)
                }
                api::Put::Object { key, stage } => {
                    let pending = self.stage(access, stage)?;
                    // One stage becomes one object: the file itself is moved, not copied.
                    if pending.sealed.is_none() || !moved.insert(stage) {
                        return Err(INVALID);
                    }
                    // Android's app storage refuses hard links, so the sealed stage itself is moved into place. Until
                    // the index is published a failure moves it back, and the stage stays as usable as before.
                    let back = pending.file.path().to_owned();
                    fs::rename(&back, &path)?;
                    allocated.0.push(Allocated::Moved { path, back });
                    stages.push(stage);
                    (key, pending.bytes)
                }
            };
            records.insert(key, Record { object: id, bytes });
        }
        if !fits(&records) {
            return Err(QUOTA);
        }
        let mut index_file = tempfile::Builder::new().prefix("index-").tempfile_in(dir)?;
        let index = Index { revision, records };
        serde_json::to_writer(&mut index_file, &index).map_err(|_| IO)?;
        index_file.as_file().sync_all()?;
        sync_directory(dir)?;
        access.fence.check()?;
        for fence in fences {
            fence.check()?;
        }
        index_file.persist(dir.join("index")).map_err(|_| IO)?;
        // At this point the new namespace is authoritative even if a subsequent sync fails.
        allocated.0.clear();
        let synced = sync_directory(dir);
        for (key, record) in old {
            if index
                .records
                .get(&key)
                .is_none_or(|new| new.object != record.object)
            {
                let _ = fs::remove_file(object_path(dir, &record.object));
            }
        }
        for stage in stages {
            self.stages.remove(&stage);
        }
        self.index = Some(index);
        synced?;
        Ok(revision)
    }
}
/// What a commit has put in the namespace before its index is published, undone if it is not.
enum Allocated {
    /// A new object file: removed.
    Created(PathBuf),
    /// A sealed stage moved to `path`: moved back to `back`, or removed if it cannot be.
    Moved { path: PathBuf, back: PathBuf },
}
struct Unpublished(Vec<Allocated>);
impl Drop for Unpublished {
    fn drop(&mut self) {
        for entry in &self.0 {
            match entry {
                Allocated::Created(path) => {
                    let _ = fs::remove_file(path);
                }
                Allocated::Moved { path, back } => {
                    if fs::rename(path, back).is_err() {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(id: &str) -> PluginId {
        PluginId::new(id).unwrap()
    }
    fn object(name: &str) -> api::Key {
        api::Key::try_from(name.to_owned()).unwrap()
    }
    fn change(revision: u64, key: &str, text: &str) -> api::Commit {
        api::Commit {
            if_revision: revision,
            put: vec![api::Put::Text {
                key: object(key),
                text: text.to_owned(),
            }],
            remove: Vec::new(),
            remove_prefix: Vec::new(),
            receive_ticket: None,
            remove_from: None,
        }
    }

    #[test]
    fn revision_publish_is_private_durable_and_fenced() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().join("extension-data"));
        let a = storage.access(plugin("first")).unwrap();
        let other = storage.access(plugin("second")).unwrap();
        let gate = Fence::new();
        gate.revoke();
        assert_eq!(
            a.state()
                .unwrap()
                .commit(&a, change(0, "notes/a", "not admitted"), Some(&gate)),
            Err(REVOKED)
        );
        assert_eq!(
            a.state()
                .unwrap()
                .commit(&a, change(0, "notes/a", "only first"), None),
            Ok(1)
        );
        assert_eq!(
            a.state()
                .unwrap()
                .commit(&a, change(0, "notes/a", "stale"), None),
            Err(STALE)
        );
        assert!(
            other
                .state()
                .unwrap()
                .open(&other.slot.path, &object("notes/a"), 0)
                .is_err()
        );
        drop(storage);
        let reopened = Storage::new(root.path().join("extension-data"))
            .access(plugin("first"))
            .unwrap();
        let (mut file, _) = reopened
            .state()
            .unwrap()
            .open(&reopened.slot.path, &object("notes/a"), 1)
            .unwrap();
        let mut data = String::new();
        file.read_to_string(&mut data).unwrap();
        assert_eq!(data, "only first");
    }

    #[test]
    fn sealed_stages_validate_offsets_and_do_not_survive_the_generation() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::new(root.path().join("extension-data"));
        let id = plugin("first");
        let a = store.access(id.clone()).unwrap();
        let stage = a.state().unwrap().begin(&a, 4).unwrap();
        let mut state = a.state().unwrap();
        assert_eq!(
            state.append(
                &a,
                &api::Append {
                    stage,
                    offset: 1,
                    length: 3
                },
                b"bad"
            ),
            Err(INVALID)
        );
        assert_eq!(
            state.append(
                &a,
                &api::Append {
                    stage,
                    offset: 0,
                    length: 4
                },
                b"bad"
            ),
            Err(INVALID)
        );
        assert!(state.seal(&a, stage).is_err());
        state
            .append(
                &a,
                &api::Append {
                    stage,
                    offset: 0,
                    length: 4,
                },
                b"data",
            )
            .unwrap();
        assert_eq!(state.seal(&a, stage).unwrap().sha256.as_str(), sha(b"data"));
        drop(state);
        store.revoke(&id);
        assert!(a.state().is_err());
        let fresh = store.access(id).unwrap();
        assert!(fresh.state().unwrap().seal(&fresh, stage).is_err());
    }

    #[tokio::test]
    async fn a_whole_read_takes_a_sealed_stage_or_a_committed_object_of_exactly_the_promised_length()
     {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::new(root.path().join("extension-data"));
        let a = store.access(plugin("first")).unwrap();
        let stage = a.state().unwrap().begin(&a, 4).unwrap();
        let source = || api::Source::Stage { stage };
        // Not sealed yet: nothing to read.
        assert_eq!(store.read_whole(a.clone(), source(), 4).await, Err(INVALID));
        {
            let mut state = a.state().unwrap();
            let append = api::Append {
                stage,
                offset: 0,
                length: 4,
            };
            state.append(&a, &append, b"rgba").unwrap();
            state.seal(&a, stage).unwrap();
        }
        assert_eq!(
            store.read_whole(a.clone(), source(), 4).await.unwrap(),
            b"rgba"
        );
        // Another length than the one promised is refused, never read short or long.
        assert_eq!(store.read_whole(a.clone(), source(), 8).await, Err(INVALID));
        assert_eq!(store.read_whole(a.clone(), source(), 3).await, Err(INVALID));
        a.state()
            .unwrap()
            .commit(&a, change(0, "thumb/one", "pixels"), None)
            .unwrap();
        let key = api::Source::Key {
            key: object("thumb/one"),
            revision: 1,
        };
        assert_eq!(
            store.read_whole(a.clone(), key, 6).await.unwrap(),
            b"pixels"
        );
        // Another extension's storage is not this one's.
        let other = store.access(plugin("second")).unwrap();
        assert!(store.read_whole(other, source(), 4).await.is_err());
    }

    #[tokio::test]
    async fn a_sealed_stage_becomes_the_object_and_a_refused_commit_leaves_the_stage_usable() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::new(root.path().join("extension-data"));
        let a = store.access(plugin("first")).unwrap();
        let stage = a.state().unwrap().begin(&a, 4).unwrap();
        {
            let mut state = a.state().unwrap();
            let append = api::Append {
                stage,
                offset: 0,
                length: 4,
            };
            state.append(&a, &append, b"rgba").unwrap();
            state.seal(&a, stage).unwrap();
        }
        let put = |key: &str| api::Put::Object {
            key: object(key),
            stage,
        };
        let commit = |put: Vec<api::Put>| api::Commit {
            if_revision: 0,
            put,
            remove: Vec::new(),
            remove_prefix: Vec::new(),
            receive_ticket: None,
            remove_from: None,
        };
        // One stage cannot become two objects.
        assert_eq!(
            a.state()
                .unwrap()
                .commit(&a, commit(vec![put("pictures/a"), put("pictures/b")]), None),
            Err(INVALID)
        );
        // Refused after the stage was moved into place: it is moved back, still sealed and whole.
        let gate = Fence::new();
        gate.revoke();
        assert!(
            a.state()
                .unwrap()
                .commit(&a, commit(vec![put("pictures/a")]), Some(&gate))
                .is_err()
        );
        let staged = || api::Source::Stage { stage };
        assert_eq!(
            store.read_whole(a.clone(), staged(), 4).await.unwrap(),
            b"rgba"
        );
        // Accepted: the object holds the bytes and the stage is gone.
        assert_eq!(
            a.state()
                .unwrap()
                .commit(&a, commit(vec![put("pictures/a")]), None),
            Ok(1)
        );
        let key = api::Source::Key {
            key: object("pictures/a"),
            revision: 1,
        };
        assert_eq!(store.read_whole(a.clone(), key, 4).await.unwrap(), b"rgba");
        assert_eq!(store.read_whole(a.clone(), staged(), 4).await, Err(INVALID));
    }

    #[test]
    fn a_refused_package_removal_preserves_data_and_a_real_removal_retires_it() {
        let data = tempfile::tempdir().unwrap();
        let manager = crate::plugin_manager::Manager::new(data.path());
        let store = Storage::new(data.path().join("extension-data"));
        let a = store.access(plugin("first")).unwrap();
        a.state()
            .unwrap()
            .commit(&a, change(0, "history/local", "local"), None)
            .unwrap();
        let stage = a.state().unwrap().begin(&a, 32).unwrap();
        let staged_path = a.state().unwrap().stages[&stage].file.path().to_path_buf();
        assert!(staged_path.is_file());
        fs::create_dir(manager.store()).unwrap();
        assert!(manager.remove("first").is_err());
        assert!(
            a.state()
                .unwrap()
                .open(&a.slot.path, &object("history/local"), 1)
                .is_ok()
        );
        assert!(staged_path.is_file());
        fs::create_dir(manager.store().join("first")).unwrap();
        manager.remove("first").unwrap();
        store.forget(&plugin("first"));
        assert!(!a.slot.path.exists());
        assert!(!staged_path.exists());
        let next = store.access(plugin("first")).unwrap();
        assert!(
            next.state()
                .unwrap()
                .open(&next.slot.path, &object("history/local"), 0)
                .is_err()
        );
        assert!(a.state().is_err());
    }

    #[test]
    fn a_new_approval_is_refused_the_old_data_until_an_update_rebinds_it() {
        let data = tempfile::tempdir().unwrap();
        let folder = data.path().join("plugins").join("first");
        fs::create_dir_all(&folder).unwrap();
        let program = folder.join("first.wasm");
        let approve = |digest: char, at: u64| {
            let approval = serde_json::from_value(json!({
                "program": "first.wasm",
                "execution": {"kind": "wasm", "abi": 1},
                "sha256": digest.to_string().repeat(64),
                "capabilities": [],
                "guards": {},
                "approved_at": at,
            }))
            .unwrap();
            crate::plugin_approval::write(&folder, &approval).unwrap();
        };
        let open = || {
            let store = Storage::new(data.path().join("extension-data"));
            let access = store
                .access(plugin("first"))
                .unwrap()
                .for_program(program.clone());
            let readable = access.state().map(|state| {
                state
                    .open(&access.slot.path, &object("history/local"), 1)
                    .is_ok()
            });
            (store, access, readable)
        };
        approve('a', 1);
        {
            let (_store, access, _) = open();
            access
                .state()
                .unwrap()
                .commit(&access, change(0, "history/local", "local"), None)
                .unwrap();
        }
        approve('b', 2);
        assert_eq!(open().2.err(), Some(REVOKED));
        rebind_namespace(data.path(), "first").unwrap();
        assert_eq!(open().2, Ok(true));
        // Bound again, now to the update: the earlier approval is the stranger.
        approve('a', 1);
        assert_eq!(open().2.err(), Some(REVOKED));
    }

    #[test]
    fn a_clear_unlinks_its_live_objects_without_touching_another_prefix() {
        let root = tempfile::tempdir().unwrap();
        let data = Storage::new(root.path().join("extension-data"));
        let a = data.access(plugin("first")).unwrap();
        let mut state = a.state().unwrap();
        state
            .commit(&a, change(0, "rooms/one/body", "discard"), None)
            .unwrap();
        state
            .commit(&a, change(1, "rooms/two/body", "retain"), None)
            .unwrap();
        let old_path = object_path(
            &a.slot.path,
            &state.index.as_ref().unwrap().records[&object("rooms/one/body")].object,
        );
        let mut purge = change(2, "rooms/one/body", "ignored");
        purge.put.clear();
        purge.remove_prefix.push(object("rooms/one"));
        assert!(purge.valid());
        assert_eq!(state.commit(&a, purge, None), Ok(3));
        assert!(!old_path.exists());
        assert!(
            state
                .open(&a.slot.path, &object("rooms/one/body"), 3)
                .is_err()
        );
        assert!(
            state
                .open(&a.slot.path, &object("rooms/two/body"), 3)
                .is_ok()
        );
    }

    #[test]
    fn recovery_removes_incomplete_stage_and_index_without_removing_live_bytes() {
        let root = tempfile::tempdir().unwrap();
        let data = Storage::new(root.path().join("extension-data"));
        let a = data.access(plugin("first")).unwrap();
        a.state()
            .unwrap()
            .commit(&a, change(0, "data/live", "committed"), None)
            .unwrap();
        let left = a.slot.path.join("stage-abandoned");
        let index = a.slot.path.join("index-abandoned");
        fs::write(&left, b"part").unwrap();
        fs::write(&index, b"part").unwrap();
        let again = Storage::new(root.path().join("extension-data"))
            .access(plugin("first"))
            .unwrap();
        assert!(
            again
                .state()
                .unwrap()
                .open(&again.slot.path, &object("data/live"), 1)
                .is_ok()
        );
        assert!(!left.exists());
        assert!(!index.exists());
    }

    #[tokio::test]
    async fn stable_references_are_pair_local_and_scope_refs_agree_across_owners() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = Storage::new(a_dir.path().join("extension-data"));
        let b = Storage::new(b_dir.path().join("extension-data"));
        let first = PeerId("first".into());
        let second = PeerId("second".into());
        let s = random_scope();
        let aa = a.access(plugin("feature")).unwrap();
        let bb = b.access(plugin("feature")).unwrap();
        let from_a = a
            .references(
                aa,
                vec![("alias1".into(), second.clone(), 23)],
                Some((first.clone(), "First".into())),
                Some(s.clone()),
            )
            .await
            .unwrap();
        let from_b = b
            .references(
                bb,
                vec![("alias2".into(), first, 12)],
                Some((second.clone(), "Second".into())),
                Some(s.clone()),
            )
            .await
            .unwrap();
        assert_eq!(
            from_a.peers[0].scoped_ref.as_ref(),
            from_b.local.as_ref().map(|l| &l.scoped_ref)
        );
        assert_eq!(
            from_b.peers[0].scoped_ref.as_ref(),
            from_a.local.as_ref().map(|l| &l.scoped_ref)
        );
        let restart = Storage::new(a_dir.path().join("extension-data"));
        let access = restart.access(plugin("feature")).unwrap();
        let restored = restart
            .references(
                access.clone(),
                vec![("new1".into(), second.clone(), 23)],
                None,
                Some(s.clone()),
            )
            .await
            .unwrap();
        assert_eq!(restored.peers[0].local_ref, from_a.peers[0].local_ref);
        restart.forget_peer(&second).await.unwrap();
        let paired_again = restart
            .references(access, vec![("new1".into(), second, 23)], None, Some(s))
            .await
            .unwrap();
        assert_ne!(paired_again.peers[0].local_ref, from_a.peers[0].local_ref);
        assert_eq!(paired_again.peers[0].scoped_ref, from_a.peers[0].scoped_ref);
    }
}
