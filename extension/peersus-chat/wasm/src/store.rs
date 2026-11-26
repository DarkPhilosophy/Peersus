//! Map guest records to opaque objects; no host code sees a room or message schema.
use crate::{
    core::{Data, Job, RoomKey},
    host::{self, Wire},
    sdk::extension_data as api,
    serde::{Serialize, de::DeserializeOwned},
    wire::Fault,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

pub(crate) type Result<T, E = Fault> = std::result::Result<T, E>;
/// Where job `n` is kept: under its room, so a conversation's work is found with it.
fn job_key(n: u64, job: &Job) -> String {
    format!("jobs/{}/{n:016x}", job.room.namespace())
}
pub(crate) fn parse<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| Fault::Storage)
}
pub(crate) fn key(value: String) -> Result<api::Key> {
    api::Key::try_from(value).map_err(|_| Fault::Storage)
}
pub(crate) fn asset(room: &RoomKey, id: &api::ScopeId) -> String {
    format!("assets/{}/{}", room.namespace(), id.as_str())
}

pub(crate) struct Link<'a, H> {
    pub host: &'a mut H,
    pub deferred: VecDeque<Value>,
    next: u64,
    stopped: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    Blocked,
    Other,
    Cancelled,
    Quota,
    Closed,
    /// The chosen file is past the size the call allowed.
    TooLarge,
}
pub(crate) fn outcome(result: &Value) -> std::result::Result<Value, Refusal> {
    match result["status"].as_str() {
        Some("ok") => Ok(result["value"].clone()),
        Some("missing-permission" | "not-accepted") => Err(Refusal::Blocked),
        Some("error") if result["code"] == "cancelled" => Err(Refusal::Cancelled),
        Some("error") if result["code"] == "quota" => Err(Refusal::Quota),
        Some("error") if result["code"] == "too-large" => Err(Refusal::TooLarge),
        _ => Err(Refusal::Other),
    }
}
impl<'a, H: Wire + host::Data> Link<'a, H> {
    pub fn new(host: &'a mut H) -> Self {
        Self {
            host,
            deferred: VecDeque::new(),
            next: 0,
            stopped: false,
        }
    }
    pub fn request(
        &mut self,
        service: &str,
        method: &str,
        args: Value,
        timeout_ms: u32,
    ) -> std::result::Result<u64, Refusal> {
        self.next = self.next.checked_add(1).ok_or(Refusal::Closed)?;
        let id = self.next;
        self.host.send(json!({"op":"call","provider":"host","service":service,"version":1,"method":method,"args":args,"id":id,"timeout_ms":timeout_ms})).map_err(|_|Refusal::Closed)?;
        Ok(id)
    }
    pub fn cancel(&mut self, id: u64) {
        let _ = self.host.send(json!({"op":"cancel","id":id}));
    }
    pub fn call(
        &mut self,
        service: &str,
        method: &str,
        args: Value,
        timeout_ms: u32,
    ) -> std::result::Result<Value, Refusal> {
        let id = self.request(service, method, args, timeout_ms)?;
        loop {
            let item = self.host.receive().map_err(|_| Refusal::Closed)?;
            if item["type"] == "result" && item["id"].as_u64() == Some(id) {
                return outcome(&item["result"]);
            }
            self.defer(item)?;
        }
    }
    pub fn defer(&mut self, item: Value) -> std::result::Result<(), Refusal> {
        if self.deferred.len() >= 64 {
            self.stopped = true;
            return Err(Refusal::Closed);
        }
        self.deferred.push_back(item);
        Ok(())
    }
    pub fn stopped(&self) -> bool {
        self.stopped
    }
    pub fn cancelled(&self, ticket: u64) -> bool {
        self.deferred
            .iter()
            .any(|d| d["type"] == "cancelled" && d["ticket"].as_u64() == Some(ticket))
    }
    pub fn ask<T: DeserializeOwned>(
        &mut self,
        service: &str,
        method: &str,
        args: impl Serialize,
        timeout: u32,
    ) -> Result<T> {
        let args = serde_json::to_value(args).map_err(|_| Fault::Storage)?;
        parse(
            self.call(service, method, args, timeout)
                .map_err(|_| Fault::Storage)?,
        )
    }
    pub fn scope(&mut self) -> Result<api::ScopeId> {
        parse(
            self.call(api::PEERS, api::NEW_SCOPE, json!({}), 2000)
                .map_err(|_| Fault::Storage)?["scope"]
                .clone(),
        )
    }
    pub fn clock(&mut self) -> Result<u64> {
        self.call(api::CLOCK, api::NOW, json!({}), 2000)
            .map_err(|_| Fault::Storage)?["monotonic_ms"]
            .as_u64()
            .ok_or(Fault::Storage)
    }
    /// The date and time on this device, or `None` when Chat may not read it (no `clock.wall` grant) or the answer is not
    /// one the application gives. Never a fault: messages simply show no time.
    pub fn wall(&mut self) -> Option<api::WallTime> {
        let answer: api::WallTime =
            parse(self.call(api::CLOCK, api::WALL, json!({}), 2000).ok()?).ok()?;
        answer.valid().then_some(answer)
    }
    fn storage<T: DeserializeOwned>(&mut self, method: &str, args: impl Serialize) -> Result<T> {
        self.ask(api::STORAGE, method, args, 5000)
    }
    pub fn start(&mut self, bytes: u64) -> Result<u64> {
        Ok(self
            .storage::<api::Staged>("stage", api::Stage { bytes })?
            .stage)
    }
    pub fn append(&mut self, stage: u64, offset: u64, bytes: &[u8]) -> Result<()> {
        let length = u32::try_from(bytes.len()).map_err(|_| Fault::Storage)?;
        let grant: api::DataClaim = self.storage(
            "append",
            api::Append {
                stage,
                offset,
                length,
            },
        )?;
        if grant.bytes != u64::from(length) {
            return Err(Fault::Storage);
        }
        let stream = self
            .host
            .open(true, grant.claim)
            .map_err(|_| Fault::Storage)?;
        let result = (|| {
            host::write_all(self.host, stream, bytes).map_err(|_| Fault::Storage)?;
            self.host.shutdown(stream).map_err(|_| Fault::Storage)?;
            (host::read_limited(self.host, stream, 1).map_err(|_| Fault::Storage)? == [1])
                .then_some(())
                .ok_or(Fault::Storage)
        })();
        self.host.close(stream);
        result
    }
    pub fn seal(&mut self, stage: u64) -> Result<api::Sealed> {
        self.storage("seal", api::Staged { stage })
    }
    pub fn abort(&mut self, stage: u64) {
        let _ = self.storage::<Value>("abort", api::Staged { stage });
    }
    pub fn stage_bytes(&mut self, bytes: &[u8]) -> Result<u64> {
        let stage = self.start(bytes.len() as u64)?;
        let result = (|| {
            let mut offset = 0;
            for chunk in bytes.chunks(api::MAX_CHUNK as usize) {
                self.append(stage, offset, chunk)?;
                offset += chunk.len() as u64;
            }
            self.seal(stage)?;
            Ok(stage)
        })();
        if result.is_err() {
            self.abort(stage);
        }
        result
    }
    fn read(
        &mut self,
        name: &api::Key,
        revision: u64,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>> {
        self.read_source(
            api::Source::Key {
                key: name.clone(),
                revision,
            },
            offset,
            length,
        )
    }
    pub fn read_source(
        &mut self,
        source: api::Source,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>> {
        let claim: api::DataClaim = self.storage(
            "read",
            api::Read {
                source,
                offset,
                length,
            },
        )?;
        if claim.bytes > u64::from(length) {
            return Err(Fault::Storage);
        }
        let handle = self
            .host
            .open(true, claim.claim)
            .map_err(|_| Fault::Storage)?;
        let data =
            host::read_limited(self.host, handle, claim.bytes as usize).map_err(|_| Fault::Storage);
        self.host.close(handle);
        data.and_then(|data| {
            (data.len() as u64 == claim.bytes)
                .then_some(data)
                .ok_or(Fault::Storage)
        })
    }
    pub fn read_asset(
        &mut self,
        name: &str,
        revision: u64,
        offset: u32,
        length: u32,
    ) -> Result<Vec<u8>> {
        let data = self.read(&key(name.to_owned())?, revision, u64::from(offset), length)?;
        (data.len() == length as usize)
            .then_some(data)
            .ok_or(Fault::Storage)
    }
    /// The whole of a committed object, which must be exactly `bytes` long, read in the largest chunks storage gives.
    pub fn read_whole(&mut self, name: &str, revision: u64, bytes: u32) -> Result<Vec<u8>> {
        let key = key(name.to_owned())?;
        self.read_all(
            || api::Source::Key {
                key: key.clone(),
                revision,
            },
            u64::from(bytes),
        )
    }
    /// The whole of a sealed stage, which must be exactly `bytes` long.
    pub fn read_stage(&mut self, stage: u64, bytes: u64) -> Result<Vec<u8>> {
        self.read_all(|| api::Source::Stage { stage }, bytes)
    }
    fn read_all(&mut self, source: impl Fn() -> api::Source, bytes: u64) -> Result<Vec<u8>> {
        let mut whole = Vec::with_capacity(usize::try_from(bytes).map_err(|_| Fault::Storage)?);
        while (whole.len() as u64) < bytes {
            let offset = whole.len() as u64;
            let length = (bytes - offset).min(u64::from(api::MAX_CHUNK)) as u32;
            let data = self.read_source(source(), offset, length)?;
            if data.len() != length as usize {
                return Err(Fault::Storage);
            }
            whole.extend(data);
        }
        Ok(whole)
    }
    /// Keep `rgba` (`width` by `height`) as the picture `id` this extension's pages place: staged, kept by the application,
    /// and the stage let go of at once, since the application keeps its own copy.
    pub fn keep_picture(&mut self, id: &str, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
        let stage = self.stage_bytes(rgba)?;
        let kept = self.call(
            crate::sdk::pictures::SERVICE,
            crate::sdk::pictures::KEEP,
            json!({"id": id, "width": width, "height": height,
                "source": {"kind": "stage", "stage": stage}}),
            5000,
        );
        self.abort(stage);
        kept.map(|_| ()).map_err(|_| Fault::Storage)
    }
    pub fn forget_pictures(&mut self, ids: Vec<String>) {
        let _ = self.call(
            crate::sdk::pictures::SERVICE,
            crate::sdk::pictures::FORGET,
            json!({ "ids": ids }),
            2000,
        );
    }
}

pub(crate) fn load<H: Wire + host::Data>(link: &mut Link<'_, H>) -> Result<(u64, Data)> {
    let mut after = None;
    let mut revision = None;
    let mut found = 0;
    let mut data = Data::default();
    let mut schema = false;
    let mut initialized = false;
    loop {
        let listing: api::Listing = link.storage(
            "list",
            api::List {
                prefix: None,
                after,
                limit: api::MAX_LIST,
            },
        )?;
        if revision
            .replace(listing.revision)
            .is_some_and(|old| old != listing.revision)
        {
            return Err(Fault::Conflict);
        }
        found += listing.keys.len();
        if found > api::MAX_KEYS || listing.more && listing.keys.is_empty() {
            return Err(Fault::Storage);
        }
        for key in &listing.keys {
            let name = key.as_str();
            if name.starts_with("assets/") {
                continue;
            }
            let mut bytes = Vec::new();
            loop {
                let chunk = link.read(key, listing.revision, bytes.len() as u64, api::MAX_CHUNK)?;
                let len = chunk.len();
                bytes.extend(chunk);
                if bytes.len() > 128 * 1024 {
                    return Err(Fault::Storage);
                }
                if len < api::MAX_CHUNK as usize {
                    break;
                }
            }
            fn decoded<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
                serde_json::from_slice(bytes).map_err(|_| Fault::Storage)
            }
            initialized = true;
            let (kind, suffix) = name.split_once('/').ok_or(Fault::Storage)?;
            match kind {
                "meta" if suffix == "schema" => {
                    schema = decoded::<u64>(&bytes)? == 1;
                }
                "meta" if suffix == "next" => {
                    data.next_job = decoded(&bytes)?;
                }
                "rooms" => {
                    data.rooms.insert(
                        RoomKey::try_from(suffix.to_owned()).map_err(|_| Fault::Storage)?,
                        decoded(&bytes)?,
                    );
                }
                "rows" => {
                    data.rows.insert(suffix.to_owned(), decoded(&bytes)?);
                }
                "jobs" => {
                    let (_, number) = suffix.split_once('/').ok_or(Fault::Storage)?;
                    let number = u64::from_str_radix(number, 16).map_err(|_| Fault::Storage)?;
                    let job: Job = decoded(&bytes)?;
                    // Kept under another room's name, it would be removed from where it is not and come back as work.
                    if job_key(number, &job) != name {
                        return Err(Fault::Storage);
                    }
                    data.jobs.insert(number, job);
                }
                "lanes" => {
                    data.lanes.insert(
                        api::ScopeId::try_from(suffix.to_owned()).map_err(|_| Fault::Storage)?,
                        decoded(&bytes)?,
                    );
                }
                "cursors" => {
                    data.cursors.insert(
                        api::ScopeId::try_from(suffix.to_owned()).map_err(|_| Fault::Storage)?,
                        decoded(&bytes)?,
                    );
                }
                _ => return Err(Fault::Storage),
            }
        }
        if !listing.more {
            break;
        }
        after = listing.keys.last().cloned();
    }
    if initialized && !schema
        || data
            .rows
            .iter()
            .any(|(key, row)| key != &row.storage_key() || !data.rooms.contains_key(&row.room))
        || data
            .jobs
            .iter()
            .any(|(n, job)| *n > data.next_job || !data.rooms.contains_key(&job.room))
    {
        return Err(Fault::Storage);
    }
    Ok((revision.unwrap_or(0), data))
}

struct Batch {
    put: Vec<api::Put>,
    remove: Vec<api::Key>,
    stages: Vec<u64>,
}
impl Batch {
    fn put<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        name: String,
        value: &impl Serialize,
    ) -> Result<()> {
        let key = key(name)?;
        let text = serde_json::to_string(value).map_err(|_| Fault::Storage)?;
        if text.len() <= api::MAX_INLINE {
            self.put.push(api::Put::Text { key, text });
        } else {
            let stage = link.stage_bytes(text.as_bytes())?;
            self.stages.push(stage);
            self.put.push(api::Put::Object { key, stage });
        }
        Ok(())
    }
    fn diff<H: Wire + host::Data, K: Ord, V: PartialEq + Serialize>(
        &mut self,
        link: &mut Link<'_, H>,
        old: &BTreeMap<K, V>,
        new: &BTreeMap<K, V>,
        path: impl Fn(&K, &V) -> String,
    ) -> Result<()> {
        for (k, v) in new {
            if old.get(k) != Some(v) {
                self.put(link, path(k, v), v)?;
            }
        }
        for (k, v) in old {
            if !new.contains_key(k) {
                self.remove.push(key(path(k, v))?);
            }
        }
        Ok(())
    }
}

pub(crate) fn commit<H: Wire + host::Data>(
    link: &mut Link<'_, H>,
    revision: u64,
    before: &Data,
    after: &Data,
    new_asset: Option<(String, u64)>,
    receive_ticket: Option<u64>,
) -> Result<u64> {
    let mut batch = Batch {
        put: Vec::new(),
        remove: Vec::new(),
        stages: Vec::new(),
    };
    let result = (|| {
        batch.diff(link, &before.rooms, &after.rooms, |k, _| {
            format!("rooms/{}", k.namespace())
        })?;
        batch.diff(link, &before.rows, &after.rows, |k, _| format!("rows/{k}"))?;
        batch.diff(link, &before.jobs, &after.jobs, |n, job| job_key(*n, job))?;
        batch.diff(link, &before.lanes, &after.lanes, |id, _| {
            format!("lanes/{}", id.as_str())
        })?;
        batch.diff(link, &before.cursors, &after.cursors, |id, _| {
            format!("cursors/{}", id.as_str())
        })?;
        for (k, row) in &before.rows {
            if let Some(asset) = &row.asset
                && after.rows.get(k).and_then(|r| r.asset.as_ref()) != Some(asset)
            {
                batch.remove.push(key(asset.clone())?);
            }
        }
        if before.next_job != after.next_job {
            batch.put(link, "meta/next".into(), &after.next_job)?;
        }
        if let Some((name, stage)) = new_asset
            && after
                .rows
                .values()
                .any(|row| row.asset.as_deref() == Some(&name))
        {
            batch.put.push(api::Put::Object {
                key: key(name)?,
                stage,
            });
        }
        let mut remove_prefix = Vec::new();
        for room in before
            .rooms
            .keys()
            .filter(|room| !after.rooms.contains_key(room))
        {
            for kind in ["rows", "assets", "jobs"] {
                remove_prefix.push(key(format!("{kind}/{}", room.namespace()))?);
            }
        }
        batch
            .remove
            .retain(|item| !remove_prefix.iter().any(|prefix| item.under(prefix)));
        if batch.put.is_empty() && batch.remove.is_empty() {
            return Ok(revision);
        }
        if revision == 0 {
            batch.put(link, "meta/schema".into(), &1u64)?;
        }
        let remove_from = if batch.remove.len() > 24 {
            let bytes = serde_json::to_vec(&batch.remove).map_err(|_| Fault::Storage)?;
            let stage = link.stage_bytes(&bytes)?;
            batch.stages.push(stage);
            batch.remove.clear();
            Some(stage)
        } else {
            None
        };
        if receive_ticket.is_some_and(|ticket| link.cancelled(ticket)) {
            return Err(Fault::Conflict);
        }
        let args = api::Commit {
            if_revision: revision,
            put: std::mem::take(&mut batch.put),
            remove: std::mem::take(&mut batch.remove),
            remove_prefix,
            receive_ticket,
            remove_from,
        };
        let answer: Value = link.storage("commit", args)?;
        answer["revision"]
            .as_u64()
            .filter(|v| revision.checked_add(1) == Some(*v))
            .ok_or(Fault::Storage)
    })();
    if result.is_err() {
        for stage in batch.stages {
            link.abort(stage);
        }
    }
    result
}
