//! Deterministic guest-owned state. The runner publishes mutations only after committing them.
use crate::{
    sdk::extension_data::{ScopeId, WallTime},
    serde::{self, Deserialize, Serialize},
    wire::*,
};
use std::collections::{BTreeMap, BTreeSet};

const MAX_ROOMS: usize = 64;
const MAX_GROUPS: usize = 32;
const MAX_ROWS: usize = 4096;
const MAX_PENDING: usize = 128;
const MAX_JOBS: usize = 384;
const MAX_PER_DEST: usize = 64;
const CONTROL_RESERVE: usize = 16;
const MAX_PEERS: usize = 128;
const MAX_ASSETS: u64 = 64 * 1024 * 1024;
const MAX_PEER_ASSETS: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(crate = "serde", try_from = "String", into = "String")]
pub(crate) enum RoomKey {
    Direct(ScopeId),
    Group(ScopeId),
}
impl RoomKey {
    pub fn namespace(&self) -> String {
        match self {
            Self::Direct(p) => format!("d{}", p.as_str()),
            Self::Group(g) => format!("g{}", g.as_str()),
        }
    }
    fn scope(&self) -> Scope {
        match self {
            Self::Direct(_) => Scope::Direct,
            Self::Group(g) => Scope::Group(g.clone()),
        }
    }
}
impl From<RoomKey> for String {
    fn from(key: RoomKey) -> Self {
        key.namespace()
    }
}
impl TryFrom<String> for RoomKey {
    type Error = &'static str;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        let id = ScopeId::try_from(s.get(1..).ok_or("Invalid room key")?.to_owned())?;
        match s.as_bytes().first() {
            Some(b'd') => Ok(Self::Direct(id)),
            Some(b'g') => Ok(Self::Group(id)),
            _ => Err("Invalid room key"),
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Room {
    pub group: Option<Group>,
    pub member: Option<ScopeId>,
    pub origin: Option<ScopeId>,
    pub sequence: u64,
    pub order: u64,
    pub cleared: BTreeMap<ScopeId, Removal>,
    /// Members that acknowledged this exact immutable descriptor.
    pub introduced: BTreeSet<ScopeId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", rename_all = "kebab-case")]
pub(crate) enum Delivery {
    Queued,
    Unconfirmed,
    Rejected,
    Sent,
    Seen,
}
impl Delivery {
    fn delivered(self) -> bool {
        matches!(self, Self::Sent | Self::Seen)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", rename_all = "kebab-case")]
pub(crate) enum Removal {
    Pending,
    Applied,
    Absent,
    LocalOnly,
    Unconfirmed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delete {
    Unsent,
    Local,
    Everyone,
}
#[derive(Clone, Default)]
pub(crate) struct Policy {
    pub delete: bool,
    pub clear: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Ready,
    Offline,
    Unavailable,
    Denied,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Reaction {
    pub revision: u64,
    pub emoji: Option<String>,
}

pub(crate) struct IncomingMessage {
    pub id: Id,
    pub order: u64,
    pub body: Body,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Row {
    pub room: RoomKey,
    pub key: Id,
    pub order: u64,
    pub mine: bool,
    pub member: Option<ScopeId>,
    pub from: Option<ScopeId>,
    pub body: Option<Body>,
    pub asset: Option<String>,
    pub delivery: BTreeMap<ScopeId, Delivery>,
    pub removed: BTreeMap<ScopeId, Removal>,
    pub reactions: BTreeMap<String, Reaction>,
    pub seen: bool,
    /// When this device wrote it or received it, by this device's clock, when the person let Chat read the date and time
    /// (`clock.wall`). Another device's clock is never trusted for it, so nothing about time goes on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<WallTime>,
    /// A peer asked to clear it while this device still owed another device work that names it: it is removed as soon as
    /// that work is done.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clearing: bool,
}
impl Row {
    /// This message as another device names it, in a reaction or a reply this device sends.
    pub(crate) fn target(&self) -> Target {
        Target {
            actor: self
                .member
                .clone()
                .map(Actor::Member)
                .unwrap_or(if self.mine {
                    Actor::Sender
                } else {
                    Actor::Receiver
                }),
            id: self.key.clone(),
        }
    }
    pub fn storage_key(&self) -> String {
        row_key(&self.room, self.member.as_ref(), self.mine, &self.key)
    }
}
/// Whether `work` names a row: its delivery or deletion when this device wrote it (`mine`, by its `id`), or a receipt or
/// reaction for it (by its `target`).
fn names(work: &Work, mine: bool, id: &Id, target: &Target) -> bool {
    match work {
        Work::Message(named) | Work::Delete(named) => mine && named == id,
        Work::Seen(named) | Work::React { target: named, .. } => named == target,
        _ => false,
    }
}
fn row_key(room: &RoomKey, member: Option<&ScopeId>, mine: bool, id: &Id) -> String {
    let author = member
        .map(ScopeId::as_str)
        .unwrap_or(if mine { "self" } else { "other" });
    format!(
        "{}/{}/{}/{:016x}",
        room.namespace(),
        author,
        id.epoch.as_str(),
        id.no
    )
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", rename_all = "kebab-case")]
enum Work {
    Define,
    Message(Id),
    React {
        target: Target,
        revision: u64,
        emoji: Option<String>,
    },
    Seen(Target),
    Delete(Id),
    Clear(Vec<Target>),
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Job {
    pub room: RoomKey,
    pub to: ScopeId,
    work: Work,
    pub peer: Option<ScopeId>,
    key: Option<Key>,
    pub held: bool,
    offset: Option<u32>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Lane {
    epoch: ScopeId,
    sequence: u64,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Cursor {
    key: Key,
    verdict: Verdict,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Data {
    pub rooms: BTreeMap<RoomKey, Room>,
    pub rows: BTreeMap<String, Row>,
    pub jobs: BTreeMap<u64, Job>,
    pub lanes: BTreeMap<ScopeId, Lane>,
    pub cursors: BTreeMap<ScopeId, Cursor>,
    pub next_job: u64,
}
#[derive(Default)]
pub(crate) struct Engine {
    pub data: Data,
    /// The date and time on this device, as last read by the runner; `None` until it is read or while it may not be.
    pub wall: Option<WallTime>,
    routes: BTreeMap<ScopeId, Route>,
    group_routes: BTreeMap<ScopeId, BTreeMap<ScopeId, ScopeId>>,
    group_self: BTreeMap<ScopeId, ScopeId>,
    limits: BTreeMap<(ScopeId, u8), Bucket>,
    aggregate: [Bucket; 3],
    schedule: BTreeMap<u64, Retry>,
}
#[derive(Default)]
struct Bucket {
    credit: u64,
    at: Option<u64>,
}
impl Bucket {
    fn charge(&mut self, now: u64, rate: u64, burst: u64) -> bool {
        let capacity = burst * 1000;
        self.credit = self.at.map_or(capacity, |at| {
            self.credit
                .saturating_add(now.saturating_sub(at).saturating_mul(rate))
                .min(capacity)
        });
        self.at = Some(now);
        if self.credit < 1000 {
            return false;
        }
        self.credit -= 1000;
        true
    }
}
#[derive(Default)]
struct Retry {
    due: u64,
    failed: u8,
}
pub(crate) struct Offer {
    pub key: Key,
    pub packet: Packet,
    pub clear: Vec<u8>,
    pub asset: Option<(String, u32, u32)>,
}

impl Engine {
    pub fn new(data: Data) -> Self {
        Self {
            data,
            ..Self::default()
        }
    }
    pub fn set_route(&mut self, peer: ScopeId, route: Route) {
        if self.routes.len() >= MAX_PEERS && !self.routes.contains_key(&peer) {
            return;
        }
        if self.routes.insert(peer.clone(), route) != Some(Route::Ready) && route == Route::Ready {
            for (number, job) in &self.data.jobs {
                if job.peer.as_ref() == Some(&peer) || job.to == peer {
                    self.schedule.remove(number);
                }
            }
        }
    }
    /// A withdrawn grant holds previously attempted work, not a never-started offline message.
    pub fn hold_revoked(&mut self, admitted: impl Fn(&ScopeId) -> bool) {
        for job in self.data.jobs.values_mut() {
            if job.peer.as_ref().is_some_and(|peer| !admitted(peer)) {
                job.held = true;
            }
        }
    }
    pub fn set_group_routes(
        &mut self,
        group: ScopeId,
        own: ScopeId,
        known: BTreeMap<ScopeId, ScopeId>,
    ) {
        if self.group_routes.len() > MAX_GROUPS && !self.group_routes.contains_key(&group) {
            return;
        }
        self.group_self.insert(group.clone(), own);
        self.group_routes.insert(group, known);
    }
    pub fn clear_group_routes(&mut self, group: &ScopeId) {
        self.group_routes.remove(group);
        self.group_self.remove(group);
    }
    pub fn collect_groups(&mut self) {
        self.group_routes
            .retain(|g, _| self.data.rooms.contains_key(&RoomKey::Group(g.clone())));
        self.group_self
            .retain(|g, _| self.group_routes.contains_key(g));
    }
    fn auth_group(&self, group: &ScopeId, peer: &ScopeId) -> Option<&ScopeId> {
        self.group_routes.get(group).and_then(|map| map.get(peer))
    }
    pub fn group_member(&self, group: &ScopeId, peer: &ScopeId) -> Option<&ScopeId> {
        self.auth_group(group, peer)
    }
    pub fn group_peer(&self, group: &ScopeId, member: &ScopeId) -> Option<&ScopeId> {
        self.group_routes
            .get(group)?
            .iter()
            .find_map(|(peer, id)| (id == member).then_some(peer))
    }
    pub fn open_room(&mut self, room: RoomKey) -> Result<(), Fault> {
        if self.data.rooms.contains_key(&room) {
            return Ok(());
        }
        if !matches!(room, RoomKey::Direct(_)) {
            return Err(Fault::Missing);
        }
        if !self.rooms_cap(&room) {
            return Err(Fault::Quota);
        }
        self.data.rooms.insert(room, Room::default());
        Ok(())
    }
    fn destinations(&self, room: &RoomKey) -> Result<Vec<ScopeId>, Fault> {
        match room {
            RoomKey::Direct(peer) => Ok(vec![peer.clone()]),
            RoomKey::Group(_) => {
                let state = self.data.rooms.get(room).ok_or(Fault::Missing)?;
                let group = state.group.as_ref().ok_or(Fault::Missing)?;
                Ok(group
                    .roster
                    .iter()
                    .filter(|m| Some(&m.id) != state.member.as_ref())
                    .map(|m| m.id.clone())
                    .collect())
            }
        }
    }
    fn local_row(&self, room: &RoomKey, id: &Id) -> Option<&Row> {
        let member = self.data.rooms.get(room)?.member.as_ref();
        self.data
            .rows
            .get(&row_key(room, member, true, id))
            .filter(|row| row.mine)
    }
    fn rooms_cap(&self, key: &RoomKey) -> bool {
        self.data.rooms.contains_key(key)
            || self.data.rooms.len() < MAX_ROOMS
                && (!matches!(key, RoomKey::Group(_))
                    || self
                        .data
                        .rooms
                        .keys()
                        .filter(|r| matches!(r, RoomKey::Group(_)))
                        .count()
                        < MAX_GROUPS)
    }
    fn rows_cap(&self, room: &RoomKey) -> bool {
        self.data.rows.len() < MAX_ROWS
            && self.data.rows.values().filter(|r| &r.room == room).count() < MAX_ROOM_ROWS
    }
    fn asset_fits(&self, body: &Body, peer: Option<&ScopeId>) -> bool {
        let bytes = |b: &Body| {
            b.attachments
                .iter()
                .map(|a| u64::from(a.bytes))
                .sum::<u64>()
        };
        if body.attachments.is_empty() {
            return true;
        }
        let mut total = bytes(body);
        let mut received = bytes(body);
        for r in self.data.rows.values() {
            let n = r.body.as_ref().map_or(0, bytes);
            total += n;
            if peer.is_some_and(|p| r.from.as_ref() == Some(p)) {
                received += n;
            }
        }
        total <= MAX_ASSETS && received <= MAX_PEER_ASSETS
    }
    fn add(
        &mut self,
        room: &RoomKey,
        batch: Vec<(ScopeId, Work)>,
        control: bool,
    ) -> Result<(), Fault> {
        let limit = MAX_JOBS + if control { CONTROL_RESERVE } else { 0 };
        if self.data.jobs.len() + batch.len() > limit
            || self.data.next_job.checked_add(batch.len() as u64).is_none()
        {
            return Err(Fault::Quota);
        }
        let mut counts = BTreeMap::new();
        for job in self.data.jobs.values() {
            *counts.entry(&job.to).or_insert(0usize) += 1;
        }
        for (to, _) in &batch {
            let count = counts.entry(to).or_default();
            *count += 1;
            if *count > MAX_PER_DEST + if control { 4 } else { 0 } {
                return Err(Fault::Quota);
            }
        }
        for (to, work) in batch {
            self.data.next_job += 1;
            self.data.jobs.insert(
                self.data.next_job,
                Job {
                    room: room.clone(),
                    to,
                    work,
                    peer: None,
                    key: None,
                    held: false,
                    offset: None,
                },
            );
        }
        Ok(())
    }
    pub fn create_group(&mut self, group: Group, own: ScopeId) -> Result<RoomKey, Fault> {
        let room = RoomKey::Group(group.id.clone());
        if !group.valid() || group.creator != own || self.data.rooms.contains_key(&room) {
            return Err(Fault::Invalid);
        }
        if !self.rooms_cap(&room) {
            return Err(Fault::Quota);
        }
        self.add(
            &room,
            group
                .roster
                .iter()
                .filter(|m| m.id != own)
                .map(|m| (m.id.clone(), Work::Define))
                .collect(),
            false,
        )?;
        self.data.rooms.insert(
            room.clone(),
            Room {
                group: Some(group),
                member: Some(own),
                ..Room::default()
            },
        );
        Ok(room)
    }
    pub fn offer_group(&mut self, room: &RoomKey, to: ScopeId) -> Result<(), Fault> {
        let info = self.data.rooms.get(room).ok_or(Fault::Missing)?;
        let group = info.group.as_ref().ok_or(Fault::Invalid)?;
        if info.member.as_ref() != Some(&group.creator) || to == group.creator || !group.has(&to) {
            return Err(Fault::Invalid);
        }
        if self
            .data
            .jobs
            .values()
            .any(|j| &j.room == room && j.to == to && matches!(j.work, Work::Define))
        {
            return Ok(());
        }
        self.add(room, vec![(to, Work::Define)], true)
    }
    pub fn queue(
        &mut self,
        room: RoomKey,
        origin: ScopeId,
        mut body: Body,
        asset: Option<String>,
    ) -> Result<Id, Fault> {
        // The minute it is sent, so the other device shows when it was written, not when it got there.
        body.sent = self.wall.map(|wall| wall.unix_minute);
        if !body.valid() || body.attachments.is_empty() == asset.is_some() {
            return Err(Fault::Invalid);
        }
        if !self.rooms_cap(&room) || !self.rows_cap(&room) || !self.asset_fits(&body, None) {
            return Err(Fault::Quota);
        }
        if self
            .data
            .jobs
            .values()
            .filter_map(|j| match &j.work {
                Work::Message(id) => Some((&j.room, id)),
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .len()
            >= MAX_PENDING
        {
            return Err(Fault::Quota);
        }
        let destinations = self.destinations(&room)?;
        let mut info = self.data.rooms.get(&room).cloned().unwrap_or_default();
        info.sequence = info.sequence.checked_add(1).ok_or(Fault::Quota)?;
        info.order = info
            .order
            .checked_add(1)
            .filter(|&v| v <= MAX_ORDER)
            .ok_or(Fault::Quota)?;
        let key = Id {
            epoch: info.origin.get_or_insert(origin).clone(),
            no: info.sequence,
        };
        let mut jobs = Vec::new();
        for to in &destinations {
            if info
                .group
                .as_ref()
                .is_some_and(|group| info.member.as_ref() == Some(&group.creator))
                && !info.introduced.contains(to)
                && !self
                    .data
                    .jobs
                    .values()
                    .any(|j| j.room == room && &j.to == to && matches!(j.work, Work::Define))
            {
                jobs.push((to.clone(), Work::Define));
            }
            jobs.push((to.clone(), Work::Message(key.clone())));
        }
        self.add(&room, jobs, false)?;
        let row = Row {
            room: room.clone(),
            key: key.clone(),
            order: info.order,
            mine: true,
            member: info.member.clone(),
            from: None,
            body: Some(body),
            asset,
            delivery: destinations
                .into_iter()
                .map(|to| (to, Delivery::Queued))
                .collect(),
            removed: BTreeMap::new(),
            reactions: BTreeMap::new(),
            seen: false,
            at: self.wall,
            clearing: false,
        };
        self.data.rows.insert(row.storage_key(), row);
        self.data.rooms.insert(room, info);
        Ok(key)
    }
    pub fn options(&self, room: &RoomKey, id: &Id) -> &'static [Delete] {
        let Some(row) = self.local_row(room, id).filter(|row| row.body.is_some()) else {
            return &[];
        };
        if row.delivery.values().any(|d| d.delivered()) {
            &[Delete::Local, Delete::Everyone]
        } else if row.delivery.values().any(|d| *d == Delivery::Unconfirmed) {
            &[]
        } else {
            &[Delete::Unsent]
        }
    }
    pub fn delete(&mut self, room: &RoomKey, id: &Id, choice: Delete) -> Result<(), Fault> {
        if !self.options(room, id).contains(&choice) {
            return Err(Fault::NotDelivered);
        }
        let row = self.local_row(room, id).ok_or(Fault::Missing)?;
        let key = row.storage_key();
        let former = row.target();
        let recipients: Vec<_> = row
            .delivery
            .iter()
            .filter(|(_, d)| d.delivered() || **d == Delivery::Unconfirmed)
            .map(|(p, _)| p.clone())
            .collect();
        if choice == Delete::Everyone {
            self.add(
                room,
                recipients
                    .iter()
                    .map(|to| (to.clone(), Work::Delete(id.clone())))
                    .collect(),
                true,
            )?;
        }
        self.data.jobs.retain(|_, job| {
            &job.room != room
                || !matches!(&job.work, Work::Message(message) if message == id)
                    && !matches!(&job.work, Work::React {target,..} if target == &former)
        });
        if choice == Delete::Unsent {
            self.data.rows.remove(&key);
        } else if let Some(row) = self.data.rows.get_mut(&key) {
            row.body = None;
            row.asset = None;
            row.reactions.clear();
            row.removed = recipients
                .into_iter()
                .map(|to| {
                    (
                        to,
                        if choice == Delete::Everyone {
                            Removal::Pending
                        } else {
                            Removal::LocalOnly
                        },
                    )
                })
                .collect();
        }
        Ok(())
    }
    pub fn clear(&mut self, room: &RoomKey, remote: bool, purge: bool) -> Result<(), Fault> {
        if !self.data.rooms.contains_key(room) {
            return Err(Fault::Missing);
        }
        let mut batch = Vec::new();
        if remote && !purge {
            for to in self.destinations(room)? {
                let targets: Vec<_> = self
                    .data
                    .rows
                    .values()
                    .filter(|row| {
                        &row.room == room
                            && if row.mine {
                                row.delivery.get(&to).is_some_and(|d| d.delivered())
                            } else {
                                matches!(room, RoomKey::Group(_)) || row.from.as_ref() == Some(&to)
                            }
                    })
                    .map(Row::target)
                    .collect();
                if !targets.is_empty() {
                    batch.push((to, Work::Clear(targets)));
                }
            }
        }
        let mut old = std::mem::take(&mut self.data.jobs);
        self.data.jobs = old.extract_if(.., |_, job| &job.room != room).collect();
        let pending = batch
            .iter()
            .map(|(to, _)| (to.clone(), Removal::Pending))
            .collect();
        if let Err(error) = self.add(room, batch, true) {
            self.data.jobs.append(&mut old);
            return Err(error);
        }
        self.data.rows.retain(|_, row| &row.room != room);
        if purge {
            self.data.rooms.remove(room);
            self.collect_groups();
        } else if let Some(info) = self.data.rooms.get_mut(room) {
            info.cleared = pending;
        }
        Ok(())
    }
    pub fn observe_rows(
        &mut self,
        room: &RoomKey,
        keys: &[String],
        visible: bool,
        foreground: bool,
    ) -> Result<(), Fault> {
        if !visible || !foreground {
            return Ok(());
        }
        if keys.len() > 8 {
            return Err(Fault::Invalid);
        }
        let mut selected = BTreeSet::new();
        let mut batch = Vec::new();
        for key in keys {
            let Some(row) =
                self.data.rows.get(key).filter(|row| {
                    &row.room == room && !row.mine && !row.seen && row.body.is_some()
                })
            else {
                continue;
            };
            if !selected.insert(key) {
                continue;
            }
            let to = row
                .member
                .as_ref()
                .or(row.from.as_ref())
                .ok_or(Fault::Invalid)?;
            batch.push((to.clone(), Work::Seen(row.target())));
            // Callers report the exact row, including its author, not just an origin counter.
        }
        self.add(room, batch, true)?;
        for key in selected {
            if let Some(row) = self.data.rows.get_mut(key) {
                row.seen = true;
            }
        }
        Ok(())
    }
    pub fn react(
        &mut self,
        room: &RoomKey,
        row_key: &str,
        emoji: Option<String>,
    ) -> Result<(), Fault> {
        if emoji
            .as_deref()
            .is_some_and(|emoji| !EMOJI.contains(&emoji))
        {
            return Err(Fault::Invalid);
        }
        let row = self
            .data
            .rows
            .get(row_key)
            .filter(|r| &r.room == room && r.body.is_some())
            .ok_or(Fault::Missing)?;
        let target = row.target();
        let actor = self
            .data
            .rooms
            .get(room)
            .and_then(|state| state.member.as_ref())
            .map(ScopeId::as_str)
            .unwrap_or("self")
            .to_owned();
        let revision = row
            .reactions
            .get(&actor)
            .map_or(Some(1), |old| old.revision.checked_add(1))
            .ok_or(Fault::Quota)?;
        let targets = self.destinations(room)?;
        self.add(
            room,
            targets
                .into_iter()
                .map(|to| {
                    (
                        to,
                        Work::React {
                            target: target.clone(),
                            revision,
                            emoji: emoji.clone(),
                        },
                    )
                })
                .collect(),
            true,
        )?;
        self.data
            .rows
            .get_mut(row_key)
            .ok_or(Fault::Missing)?
            .reactions
            .insert(actor, Reaction { revision, emoji });
        Ok(())
    }

    fn matches_destination(&self, job: &Job, peer: &ScopeId) -> bool {
        if let Some(p) = &job.peer {
            return p == peer;
        }
        match &job.room {
            RoomKey::Direct(local) => local == peer,
            RoomKey::Group(g) => self.auth_group(g, peer) == Some(&job.to),
        }
    }
    fn next_job(&self, peer: &ScopeId) -> Option<(&u64, &Job)> {
        self.data
            .jobs
            .iter()
            .filter(|(_, job)| {
                self.matches_destination(job, peer) && (!job.held || job.key.is_some())
            })
            .min_by_key(|(n, job)| (job.key.is_none(), **n))
    }
    pub fn lane_epoch(&self, peer: &ScopeId) -> Option<&ScopeId> {
        self.data.lanes.get(peer).map(|lane| &lane.epoch)
    }
    pub fn can_dispatch(&self, peer: &ScopeId, now: u64) -> bool {
        self.routes.get(peer) == Some(&Route::Ready)
            && self.next_job(peer).is_some_and(|(n, job)| {
                !job.held && self.schedule.get(n).is_none_or(|retry| retry.due <= now)
            })
    }
    pub fn next(
        &mut self,
        peer: &ScopeId,
        now: u64,
        fresh: ScopeId,
    ) -> Result<Option<Offer>, Fault> {
        if self.routes.get(peer) != Some(&Route::Ready) {
            return Ok(None);
        }
        self.schedule.retain(|n, _| self.data.jobs.contains_key(n));
        let Some((&number, job)) = self.next_job(peer) else {
            return Ok(None);
        };
        if job.held || self.schedule.get(&number).is_some_and(|r| r.due > now) {
            return Ok(None);
        }
        let (room, to, work, offset) = (
            job.room.clone(),
            job.to.clone(),
            job.work.clone(),
            job.offset,
        );
        let key = if let Some(key) = &job.key {
            key.clone()
        } else {
            if self.data.lanes.len() >= MAX_PEERS && !self.data.lanes.contains_key(peer) {
                return Err(Fault::Quota);
            }
            let lane = self.data.lanes.entry(peer.clone()).or_insert(Lane {
                epoch: fresh,
                sequence: 0,
            });
            lane.sequence = lane.sequence.checked_add(1).ok_or(Fault::Quota)?;
            let key = Key {
                epoch: lane.epoch.clone(),
                seq: lane.sequence,
                operation: number,
            };
            let job = self.data.jobs.get_mut(&number).ok_or(Fault::Missing)?;
            job.peer = Some(peer.clone());
            job.key = Some(key.clone());
            key
        };
        let mut payload = Vec::new();
        let mut asset = None;
        let change = match work {
            Work::Define => Op::Define {
                group: self
                    .data
                    .rooms
                    .get(&room)
                    .and_then(|r| r.group.clone())
                    .ok_or(Fault::Missing)?,
            },
            Work::Message(id) => {
                let row = self.local_row(&room, &id).ok_or(Fault::Missing)?;
                let body = row.body.clone().ok_or(Fault::Missing)?;
                let change = match (body.attachments.first(), offset) {
                    (None, _) => Op::Message {
                        id,
                        order: row.order,
                        body,
                    },
                    (Some(_), None) => Op::Begin {
                        id,
                        order: row.order,
                        body,
                    },
                    (Some(file), Some(offset)) if offset < file.bytes => {
                        let len = (file.bytes - offset).min(CHUNK as u32);
                        asset = Some((row.asset.clone().ok_or(Fault::Storage)?, offset, len));
                        Op::Chunk { offset, len }
                    }
                    (Some(file), Some(offset)) if offset == file.bytes => Op::Finish,
                    _ => return Err(Fault::Invalid),
                };
                let row_key = row.storage_key();
                let row = self.data.rows.get_mut(&row_key).ok_or(Fault::Missing)?;
                if matches!(
                    row.delivery.get(&to),
                    Some(Delivery::Queued | Delivery::Rejected)
                ) {
                    row.delivery.insert(to, Delivery::Unconfirmed);
                }
                change
            }
            Work::React {
                target,
                revision,
                emoji,
            } => Op::React {
                target,
                revision,
                emoji,
            },
            Work::Seen(target) => Op::Seen { target },
            Work::Delete(id) => Op::Delete { id },
            Work::Clear(ids) => {
                payload = clear_ids(
                    &ids,
                    self.data.rooms.get(&room).and_then(|r| r.group.as_ref()),
                )?;
                Op::Clear {
                    count: (payload.len() / 41) as u16,
                }
            }
        };
        Ok(Some(Offer {
            key: key.clone(),
            packet: Packet::Mutation {
                key,
                scope: room.scope(),
                change,
            },
            clear: payload,
            asset,
        }))
    }
    pub fn after_transport(&mut self, key: &Key, now: u64, blocked: bool) {
        let Some(job) = self
            .data
            .jobs
            .get_mut(&key.operation)
            .filter(|j| j.key.as_ref() == Some(key))
        else {
            return;
        };
        if blocked {
            job.held = true;
            if let Work::Delete(id) = &job.work {
                if let Some(row) = self
                    .data
                    .rows
                    .values_mut()
                    .find(|row| row.mine && row.room == job.room && &row.key == id)
                {
                    row.removed.insert(job.to.clone(), Removal::Unconfirmed);
                }
            } else if matches!(job.work, Work::Clear(_))
                && let Some(room) = self.data.rooms.get_mut(&job.room)
            {
                room.cleared.insert(job.to.clone(), Removal::Unconfirmed);
            }
        }
        let retry = self.schedule.entry(key.operation).or_default();
        retry.due = now.saturating_add(1000u64 << retry.failed.min(4));
        retry.failed = retry.failed.saturating_add(1);
    }
    pub fn retry(&mut self, peer: &ScopeId) {
        let numbers: Vec<_> = self
            .data
            .jobs
            .iter()
            .filter(|(_, j)| self.matches_destination(j, peer))
            .map(|(n, _)| *n)
            .collect();
        for n in numbers {
            if let Some(job) = self.data.jobs.get_mut(&n) {
                job.held = false;
            }
            self.schedule.remove(&n);
        }
    }
    pub fn answer(
        &mut self,
        peer: &ScopeId,
        key: Key,
        answer: Verdict,
        now: u64,
    ) -> Result<(), Fault> {
        let Some(job) = self
            .data
            .jobs
            .get(&key.operation)
            .filter(|j| j.peer.as_ref() == Some(peer) && j.key.as_ref() == Some(&key))
            .cloned()
        else {
            return Ok(());
        };
        if let Verdict::Offset(offset) = answer {
            let Work::Message(id) = &job.work else {
                return Err(Fault::Invalid);
            };
            let len = self
                .local_row(&job.room, id)
                .and_then(|r| r.body.as_ref())
                .and_then(|b| b.attachments.first())
                .ok_or(Fault::Invalid)?
                .bytes;
            let previous = job.offset.unwrap_or(0);
            if offset > len
                || offset > previous.saturating_add(CHUNK as u32)
                || job.offset.is_none() && offset != 0
            {
                return Err(Fault::Invalid);
            }
            if let Some(active) = self.data.jobs.get_mut(&key.operation) {
                active.offset = Some(previous.max(offset));
            }
            self.schedule.remove(&key.operation);
            return Ok(());
        }
        if answer == Verdict::Unknown {
            if job.offset.is_some()
                && let Some(active) = self.data.jobs.get_mut(&key.operation)
            {
                active.offset = None;
            }
            self.schedule.remove(&key.operation);
            return Ok(());
        }
        if matches!(answer, Verdict::NeedGroup | Verdict::NeedTarget) {
            // Yield the pair's lane. Retry is explicit if this dependency isn't known to be ready.
            let n = self.data.next_job.checked_add(1).ok_or(Fault::Quota)?;
            if answer == Verdict::NeedGroup
                && let Some(room) = self.data.rooms.get_mut(&job.room)
            {
                room.introduced.remove(&job.to);
            }
            if let Some(mut old) = self.data.jobs.remove(&key.operation) {
                old.key = None;
                old.offset = None;
                old.held = true;
                self.data.next_job = n;
                self.data.jobs.insert(n, old);
            }
            if let Work::Message(id) = &job.work
                && let Some(storage_key) = self.local_row(&job.room, id).map(Row::storage_key)
                && let Some(row) = self.data.rows.get_mut(&storage_key)
            {
                row.delivery.insert(job.to, Delivery::Rejected);
            }
            self.schedule.remove(&key.operation);
            return Ok(());
        }
        self.data.jobs.remove(&key.operation);
        self.schedule.remove(&key.operation);
        let state = match answer {
            Verdict::Applied => Removal::Applied,
            Verdict::Absent => Removal::Absent,
            Verdict::Declined => Removal::LocalOnly,
            _ => Removal::Unconfirmed,
        };
        match job.work {
            Work::Define if answer == Verdict::Applied => {
                if let Some(room) = self.data.rooms.get_mut(&job.room) {
                    room.introduced.insert(job.to.clone());
                }
                for pending in self.data.jobs.values_mut().filter(|pending| {
                    pending.room == job.room
                        && pending.to == job.to
                        && pending.key.is_none()
                        && matches!(pending.work, Work::Message(_))
                }) {
                    pending.held = false;
                }
            }
            Work::Message(id) => {
                if let Some(row) = self.local_row(&job.room, &id) {
                    let storage_key = row.storage_key();
                    if let Some(row) = self.data.rows.get_mut(&storage_key) {
                        let disposition = match answer {
                            Verdict::Applied => Delivery::Sent,
                            Verdict::Stale => Delivery::Unconfirmed,
                            _ => Delivery::Rejected,
                        };
                        if row.delivery.get(&job.to) != Some(&Delivery::Seen) {
                            row.delivery.insert(job.to, disposition);
                        }
                    }
                }
            }
            Work::Delete(id) => {
                if let Some(row) = self.local_row(&job.room, &id) {
                    let key = row.storage_key();
                    if let Some(row) = self.data.rows.get_mut(&key) {
                        row.removed.insert(job.to, state);
                    }
                }
            }
            Work::Clear(_) => {
                if let Some(room) = self.data.rooms.get_mut(&job.room) {
                    room.cleared.insert(job.to, state);
                }
            }
            _ => {}
        }
        self.settle_clears(&job.room);
        let _ = now;
        Ok(())
    }

    /// Whether this device still owes a device other than `except` work that names `row`.
    fn owed(&self, row: &Row, except: Option<&ScopeId>) -> bool {
        let target = row.target();
        self.data.jobs.values().any(|job| {
            job.room == row.room
                && Some(&job.to) != except
                && names(&job.work, row.mine, &row.key, &target)
        })
    }
    /// Remove the rows of `room` a peer asked to clear whose owed work is now done.
    fn settle_clears(&mut self, room: &RoomKey) {
        let done: Vec<_> = self
            .data
            .rows
            .iter()
            .filter(|(_, row)| row.clearing && &row.room == room && !self.owed(row, None))
            .map(|(key, _)| key.clone())
            .collect();
        for key in done {
            self.data.rows.remove(&key);
        }
    }

    fn rate(&mut self, peer: &ScopeId, category: u8, now: u64) -> bool {
        let (rate, burst) = match category {
            0 => (6, 12),
            1 => (32, 64),
            _ => (10, 20),
        };
        if !self.aggregate[usize::from(category)].charge(now, rate * 2, burst * 2) {
            return false;
        }
        let key = (peer.clone(), category);
        if self.limits.len() >= 3 * MAX_PEERS && !self.limits.contains_key(&key) {
            return false;
        }
        self.limits.entry(key).or_default().charge(now, rate, burst)
    }
    pub fn cursor(&self, peer: &ScopeId, key: &Key) -> Option<Verdict> {
        let found = self.data.cursors.get(peer)?;
        if found.key.epoch != key.epoch {
            return Some(Verdict::Invalid);
        }
        if key.seq < found.key.seq {
            return Some(Verdict::Stale);
        }
        if key.seq == found.key.seq {
            return Some(if &found.key == key {
                found.verdict
            } else {
                Verdict::Invalid
            });
        }
        None
    }
    pub fn admit(
        &mut self,
        peer: &ScopeId,
        packet: &Packet,
        payload: &[u8],
        now: u64,
    ) -> Result<(), Fault> {
        if !packet.valid(payload) {
            return Err(Fault::Invalid);
        }
        let category = match packet {
            Packet::Mutation {
                change: Op::Chunk { .. },
                ..
            } => 1,
            Packet::Mutation { .. } => 0,
            _ => 2,
        };
        self.rate(peer, category, now)
            .then_some(())
            .ok_or(Fault::Rate)
    }
    #[cfg(test)]
    pub fn receive(
        &mut self,
        peer: &ScopeId,
        packet: Packet,
        payload: &[u8],
        now: u64,
        policy: Policy,
        attachment: Option<String>,
    ) -> Result<Verdict, Fault> {
        self.admit(peer, &packet, payload, now)?;
        self.apply(peer, packet, payload, now, policy, attachment)
    }
    pub fn finish_attachment(
        &mut self,
        peer: &ScopeId,
        key: Key,
        scope: Scope,
        message: IncomingMessage,
        now: u64,
        asset: String,
    ) -> Result<Verdict, Fault> {
        let IncomingMessage { id, order, body } = message;
        self.apply(
            peer,
            Packet::Mutation {
                key,
                scope,
                change: Op::Begin { id, order, body },
            },
            &[],
            now,
            Policy::default(),
            Some(asset),
        )
    }
    pub fn apply(
        &mut self,
        peer: &ScopeId,
        packet: Packet,
        payload: &[u8],
        now: u64,
        policy: Policy,
        attachment: Option<String>,
    ) -> Result<Verdict, Fault> {
        let (key, scope, change) = match packet {
            Packet::Status { key } => {
                return Ok(self.cursor(peer, &key).unwrap_or(Verdict::Unknown));
            }
            Packet::Result { key, verdict } => {
                self.answer(peer, key, verdict, now)?;
                return Ok(Verdict::Unknown);
            }
            Packet::Mutation { key, scope, change } => (key, scope, change),
        };
        if let Some(previous) = self.cursor(peer, &key) {
            return Ok(previous);
        }
        if self.data.cursors.len() >= MAX_PEERS && !self.data.cursors.contains_key(peer) {
            return Err(Fault::Quota);
        }
        let room = match &scope {
            Scope::Direct => RoomKey::Direct(peer.clone()),
            Scope::Group(g) => RoomKey::Group(g.clone()),
        };
        let actor = if let Scope::Group(group) = &scope {
            self.auth_group(group, peer).cloned()
        } else {
            None
        };
        let membership = match (&scope, &change) {
            (Scope::Direct, Op::Define { .. }) => Err(Verdict::Invalid),
            (Scope::Direct, _) => Ok(()),
            (Scope::Group(g), Op::Define { group }) => {
                let ours = self.group_self.get(g);
                if actor.as_ref() != Some(&group.creator)
                    || !ours.is_some_and(|me| me != &group.creator && group.has(me))
                {
                    Err(Verdict::Invalid)
                } else {
                    Ok(())
                }
            }
            (Scope::Group(_), _) => match self.data.rooms.get(&room).and_then(|r| r.group.as_ref())
            {
                Some(group) if actor.as_ref().is_some_and(|a| group.has(a)) => Ok(()),
                Some(_) => Err(Verdict::Invalid),
                None => Err(Verdict::NeedGroup),
            },
        };
        let verdict = if let Err(no) = membership {
            no
        } else {
            match change {
                Op::Define { group } => {
                    if let Some(existing) = self.data.rooms.get(&room) {
                        if existing.group.as_ref() == Some(&group) {
                            Verdict::Applied
                        } else {
                            Verdict::Invalid
                        }
                    } else if !self.rooms_cap(&room)
                        || self
                            .data
                            .rooms
                            .values()
                            .filter(|r| {
                                r.group
                                    .as_ref()
                                    .is_some_and(|g| Some(&g.creator) == actor.as_ref())
                            })
                            .count()
                            >= 4
                    {
                        Verdict::Quota
                    } else {
                        self.data.rooms.insert(
                            room.clone(),
                            Room {
                                member: self.group_self.get(&group.id).cloned(),
                                group: Some(group),
                                ..Room::default()
                            },
                        );
                        Verdict::Applied
                    }
                }
                Op::Message { id, order, body } => self.insert(
                    &room,
                    peer,
                    actor,
                    IncomingMessage { id, order, body },
                    None,
                ),
                Op::Begin { id, order, body } => {
                    if attachment.is_some() {
                        self.insert(
                            &room,
                            peer,
                            actor,
                            IncomingMessage { id, order, body },
                            attachment,
                        )
                    } else if !self.rooms_cap(&room)
                        || !self.rows_cap(&room)
                        || !self.asset_fits(&body, Some(peer))
                    {
                        Verdict::Quota
                    } else {
                        Verdict::Offset(0)
                    }
                }
                Op::Chunk { .. } | Op::Finish => Verdict::Unknown,
                Op::Delete { id } => {
                    if !policy.delete {
                        Verdict::Declined
                    } else {
                        let key = row_key(&room, actor.as_ref(), false, &id);
                        match self
                            .data
                            .rows
                            .get_mut(&key)
                            .filter(|row| row.from.as_ref() == Some(peer))
                        {
                            Some(row) => {
                                let former = row.target();
                                row.body = None;
                                row.asset = None;
                                row.reactions.clear();
                                self.data.jobs.retain(|_,job| job.room != room
                                    || !matches!(&job.work, Work::Seen(target) | Work::React {target,..} if target == &former));
                                Verdict::Applied
                            }
                            None => Verdict::Absent,
                        }
                    }
                }
                Op::Clear { .. } => {
                    if !policy.clear {
                        Verdict::Declined
                    } else {
                        let group = self.data.rooms.get(&room).and_then(|r| r.group.as_ref());
                        let ids = read_clear(payload, group)?;
                        let actor = actor.as_ref().unwrap_or(peer);
                        // A row this device still owes another device work for (a group message not yet delivered to
                        // another member, a deletion or receipt it waits for) is removed once that work is done: one
                        // recipient's clear never takes a message away from another.
                        let named: Vec<_> = ids
                            .iter()
                            .filter_map(|target| {
                                self.target(&room, target).filter(|row| {
                                    !row.mine
                                        || row.delivery.get(actor).is_some_and(|d| d.delivered())
                                })
                            })
                            .map(|row| {
                                let owed = self.owed(row, Some(actor));
                                (
                                    row.storage_key(),
                                    row.mine,
                                    row.key.clone(),
                                    row.target(),
                                    owed,
                                )
                            })
                            .collect();
                        // Nothing more is sent to the requester about any of them, nor to anyone about a removed one.
                        self.data.jobs.retain(|_, job| {
                            job.room != room
                                || !named.iter().any(|(_, mine, id, target, owed)| {
                                    names(&job.work, *mine, id, target)
                                        && (!owed || &job.to == actor)
                                })
                        });
                        for (key, _, _, _, owed) in named {
                            if owed {
                                if let Some(row) = self.data.rows.get_mut(&key) {
                                    row.clearing = true;
                                }
                            } else {
                                self.data.rows.remove(&key);
                            }
                        }
                        Verdict::Applied
                    }
                }
                Op::Seen { target } => {
                    let actor = actor.as_ref().unwrap_or(peer);
                    let found = self
                        .target(&room, &target)
                        .filter(|r| r.mine)
                        .map(Row::storage_key);
                    if let Some(row) = found.and_then(|key| self.data.rows.get_mut(&key)) {
                        if let Some(status) = row.delivery.get_mut(actor).filter(|d| {
                            matches!(**d, Delivery::Sent | Delivery::Seen | Delivery::Unconfirmed)
                        }) {
                            *status = Delivery::Seen;
                            self.data.jobs.retain(|_, job| {
                                job.room != room
                                    || &job.to != actor
                                    || !matches!(&job.work,Work::Message(id) if id == &target.id)
                            });
                        }
                        Verdict::Applied
                    } else {
                        Verdict::Absent
                    }
                }
                Op::React {
                    target,
                    revision,
                    emoji,
                } => {
                    let actor = actor
                        .as_ref()
                        .map(ScopeId::as_str)
                        .unwrap_or("other")
                        .to_owned();
                    let found = self.target(&room, &target).map(Row::storage_key);
                    match found.and_then(|key| self.data.rows.get_mut(&key)) {
                        Some(row) if row.body.is_none() => Verdict::Absent,
                        Some(row) => {
                            if row
                                .reactions
                                .get(&actor)
                                .is_none_or(|item| item.revision < revision)
                            {
                                row.reactions.insert(actor, Reaction { revision, emoji });
                            }
                            Verdict::Applied
                        }
                        None => Verdict::NeedTarget,
                    }
                }
            }
        };
        if verdict.terminal() {
            self.data
                .cursors
                .insert(peer.clone(), Cursor { key, verdict });
        }
        Ok(verdict)
    }
    fn target(&self, room: &RoomKey, target: &Target) -> Option<&Row> {
        let (member, mine) = match &target.actor {
            Actor::Sender => (None, false),
            Actor::Receiver => (None, true),
            Actor::Member(member) => (Some(member), false),
        };
        self.data.rows.get(&row_key(room, member, mine, &target.id))
    }
    /// The message `row` answers, as this device holds it. A reply names its target the way the device that wrote it saw
    /// it, so on this device's own message in a direct room the sender and the receiver trade places.
    pub fn replied(&self, row: &Row) -> Option<&Row> {
        let target = row.body.as_ref()?.reply.as_ref()?;
        let (member, mine) = match &target.actor {
            Actor::Sender => (None, row.mine),
            Actor::Receiver => (None, !row.mine),
            Actor::Member(member) => (Some(member), false),
        };
        self.data
            .rows
            .get(&row_key(&row.room, member, mine, &target.id))
    }
    /// The first message of the thread `row` is in, when it answers a message this device holds: a reply to a reply belongs
    /// to the same thread. A chain is at most a conversation's rows long, and replies that name each other end where they
    /// come back round; never `row` itself.
    pub fn root(&self, row: &Row) -> Option<&Row> {
        let mut root = self.replied(row)?;
        let mut seen = BTreeSet::from([row.storage_key(), root.storage_key()]);
        while seen.len() <= MAX_ROOM_ROWS {
            match self.replied(root) {
                Some(up) if seen.insert(up.storage_key()) => root = up,
                _ => break,
            }
        }
        (root.storage_key() != row.storage_key()).then_some(root)
    }
    fn insert(
        &mut self,
        room: &RoomKey,
        peer: &ScopeId,
        actor: Option<ScopeId>,
        message: IncomingMessage,
        asset: Option<String>,
    ) -> Verdict {
        let IncomingMessage { id, order, body } = message;
        let key = row_key(room, actor.as_ref(), false, &id);
        if let Some(row) = self.data.rows.get(&key) {
            // A message placed at the bound keeps a lower order here than the one it was sent with.
            return if row.body.as_ref() == Some(&body)
                && row.order <= order
                && row.from.as_ref() == Some(peer)
            {
                Verdict::Applied
            } else {
                Verdict::Invalid
            };
        }
        if !self.rooms_cap(room) || !self.rows_cap(room) || !self.asset_fits(&body, Some(peer)) {
            return Verdict::Quota;
        }
        if body.attachments.is_empty() == asset.is_some() {
            return Verdict::Invalid;
        }
        let known = self.data.rooms.get(room);
        if matches!(room, RoomKey::Direct(_))
            && known.and_then(|info| info.origin.as_ref()) == Some(&id.epoch)
        {
            return Verdict::Invalid;
        }
        // How far a peer moves a conversation's order is bounded, so no one pushes it to its end. A message past the
        // bound is still taken, placed at it: refusing it would refuse a member whose order moved with a message this
        // device has not seen yet.
        let current = known.map_or(0, |info| info.order);
        let order = order.min(if current == 0 {
            (1 << 48) - 1
        } else {
            current.saturating_add(1 << 24)
        });
        let info = self.data.rooms.entry(room.clone()).or_default();
        info.order = info.order.max(order);
        let sent = body.sent;
        self.data.rows.insert(
            key,
            Row {
                room: room.clone(),
                key: id,
                order,
                mine: false,
                member: actor,
                from: Some(peer.clone()),
                body: Some(body),
                asset,
                delivery: BTreeMap::new(),
                removed: BTreeMap::new(),
                reactions: BTreeMap::new(),
                seen: false,
                at: self.wall.map(|wall| WallTime {
                    unix_minute: sent_minute(sent, wall.unix_minute),
                    ..wall
                }),
                clearing: false,
            },
        );
        Verdict::Applied
    }
}

/// The minute a received message is shown at: the minute its writer says it sent it, when that is no later than a few
/// minutes past this device's own clock and no earlier than the longest a message waits in a queue; else the minute it got
/// here. The writer's clock is its word only, and a wrong one moves nothing far.
fn sent_minute(sent: Option<i64>, here: i64) -> i64 {
    const AHEAD: i64 = 5;
    const QUEUED: i64 = 30 * 24 * 60;
    sent.filter(|minute| (here - QUEUED..=here + AHEAD).contains(minute))
        .map_or(here, |minute| minute.min(here))
}
