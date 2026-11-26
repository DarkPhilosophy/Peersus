//! Supervised guest loop: peer stream handles live here; only the domain state is persisted.
use crate::{
    core::{self, *},
    host::{self, DeliveryAck, Handle, Wire},
    sdk::extension_data::{self as api, ScopeId},
    store::{self, Link, Refusal},
    ui::{self, Draft, Features, Locale, Peer},
    wire::*,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

#[path = "page.rs"]
mod page;
use page::FileJob;
#[path = "sound.rs"]
mod sound;

const POLL_MS: i32 = 100;
const CHECK_MS: u64 = 2000;
const IDLE_STAGE: u64 = 30_000;
const TOTAL_STAGE: u64 = 300_000;
const DATA_WAIT: u64 = 6000;
const MAX_ACTIVE_IN: usize = 4;
const MAX_REPLIES: usize = 32;
/// How long a message that arrived in a conversation open on a page waits to be seen there before it is notified.
const NOTICE_WAIT: u64 = 3000;
/// Most messages waiting to be notified; past it the oldest is notified at once.
const MAX_NOTICES: usize = 16;

struct Incoming {
    peer: ScopeId,
    ticket: u64,
    expires: u64,
    bytes: Vec<u8>,
    cleared: std::collections::BTreeSet<RoomKey>,
}
struct Outgoing {
    handle: Handle,
    key: Option<Key>,
    bytes: u64,
    expires: u64,
}
struct Upload {
    key: Key,
    scope: Scope,
    id: Id,
    order: u64,
    body: Body,
    stage: u64,
    offset: u32,
    began: u64,
    idle: u64,
}
struct Reply {
    peer: ScopeId,
    key: Key,
    verdict: Verdict,
}
/// How often the date and time are read again.
const WALL_EVERY_MS: u64 = 10_000;
struct Runner {
    engine: Engine,
    revision: u64,
    owner: String,
    peers: BTreeMap<ScopeId, Peer>,
    alias: BTreeMap<String, ScopeId>,
    incoming: BTreeMap<Handle, Incoming>,
    outgoing: Option<Outgoing>,
    uploads: BTreeMap<ScopeId, Upload>,
    replies: VecDeque<Reply>,
    due: u64,
    last_peer: Option<ScopeId>,
    fault: bool,
    pages: BTreeMap<String, ui::Page>,
    file_job: Option<FileJob>,
    features: Features,
    locale: Locale,
    primary: Option<bool>,
    /// The photo pictures made in this process and kept for the page.
    thumbs: ui::Thumbs,
    waves: ui::Waves,
    /// Messages that arrived in a conversation open on a page, by storage key, and when: notified unless seen there first.
    notices: VecDeque<(String, u64)>,
    /// The audio message playing, decoded here a few seconds ahead of the device.
    listening: Option<ui::Listening>,
    /// The conversation of the notification the person last tapped, for the page the application opens for it.
    opening: Option<RoomKey>,
    /// When to read the date and time again (`clock.wall`), on the monotonic clock.
    wall_due: u64,
}

pub(crate) fn run<H: Wire + host::Data>(host: &mut H) -> i32 {
    let mut link = Link::new(host);
    if link.host.send(host::subscribe_config()).is_err() {
        return 1;
    }
    if link.host.send(json!({"op":"subscribe","provider":"host","service":"peersus.language","version":1,"event":"changed"})).is_err() {
        return 1;
    }
    // A tapped notification of a message opens its conversation.
    if link.host.send(json!({"op":"subscribe","provider":"host","service":page::NOTIFY,"version":1,"event":page::OPENED})).is_err() {
        return 1;
    }
    let Ok((revision, data)) = store::load(&mut link) else {
        return 1;
    };
    let mut state = Runner {
        engine: Engine::new(data),
        revision,
        owner: String::new(),
        peers: BTreeMap::new(),
        alias: BTreeMap::new(),
        incoming: BTreeMap::new(),
        outgoing: None,
        uploads: BTreeMap::new(),
        replies: VecDeque::new(),
        due: 0,
        last_peer: None,
        fault: false,
        pages: BTreeMap::new(),
        file_job: None,
        features: Features::default(),
        locale: Locale::default(),
        primary: None,
        thumbs: ui::Thumbs::default(),
        waves: ui::Waves::default(),
        notices: VecDeque::new(),
        listening: None,
        opening: None,
        wall_due: 0,
    };
    loop {
        if link.stopped() || state.fault {
            return 1;
        }
        let Ok(now) = link.clock() else {
            return 1;
        };
        state.expire(&mut link, now);
        // A change that may or may not have been kept stops everything at once: nothing more is handled or sent.
        if state.fault {
            return 1;
        }
        if now >= state.wall_due {
            // Often enough that a message's minute is right, rarely enough to cost nothing; without the grant the
            // answer is `None` and messages show no time.
            state.engine.wall = link.wall();
            state.wall_due = now.saturating_add(WALL_EVERY_MS);
        }
        state.expire_drafts(&mut link, now);
        state.notify_waiting(&mut link, now);
        state.pump_sound(&mut link, now);
        let stalled = (state.due == 0 || state.due <= now && !state.engine.data.jobs.is_empty())
            && !state.refresh(&mut link, now)
            && link.stopped();
        // A refresh keeps what it learned too: one that may not have been kept stops here as well.
        if stalled || state.fault {
            return 1;
        }
        if let Some(message) = link.deferred.pop_front()
            && !state.control(&mut link, message, now)
        {
            return 1;
        }
        if state.fault {
            return 1;
        }
        state.dispatch(&mut link, now);
        if !link.deferred.is_empty() {
            continue;
        }
        let mut handles: Vec<_> = state.incoming.keys().copied().collect();
        if let Some(out) = &state.outgoing {
            handles.push(out.handle);
        }
        let pending = !handles.is_empty()
            || !state.engine.data.jobs.is_empty()
            || !state.uploads.is_empty()
            || state.pages.values().any(|page| page.draft.is_some())
            || !state.replies.is_empty()
            || !state.notices.is_empty()
            || state.listening.is_some();
        let Ok(ready) = link.host.poll(&handles, if pending { POLL_MS } else { -1 }) else {
            return 0;
        };
        // The wait may have been long: what woke it is handled at the time it came.
        let Ok(now) = link.clock() else {
            return 1;
        };
        if ready == Some(0) {
            let Ok(message) = link.host.receive() else {
                return 0;
            };
            if !state.control(&mut link, message, now) {
                return 1;
            }
        } else if let Some(handle) = ready {
            if state
                .outgoing
                .as_ref()
                .is_some_and(|out| out.handle == handle)
            {
                state.completed(&mut link, now);
            } else {
                state.read(&mut link, handle, now);
            }
        }
    }
}

impl Runner {
    fn commit<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        previous: core::Data,
        asset: Option<(String, u64)>,
        ticket: Option<u64>,
    ) -> Result<(), Fault> {
        // After a commit that may or may not have been kept, nothing more is kept or acknowledged from a guessed revision.
        if self.fault {
            return Err(Fault::Storage);
        }
        match store::commit(
            link,
            self.revision,
            &previous,
            &self.engine.data,
            asset,
            ticket,
        ) {
            Ok(revision) => {
                self.revision = revision;
                Ok(())
            }
            Err(Fault::Conflict) => {
                self.engine.data = previous;
                Err(Fault::Conflict)
            }
            Err(error) => {
                // An unanswered commit may already have succeeded. Reload on supervised restart,
                // never keep emitting traffic from a guessed revision or acknowledge a guess.
                self.fault = true;
                Err(error)
            }
        }
    }
    fn refresh<H: Wire + host::Data>(&mut self, link: &mut Link<'_, H>, now: u64) -> bool {
        self.due = now.saturating_add(CHECK_MS);
        let Ok(config) = link.call("peersus.config", "get", json!({}), 2000) else {
            self.invalidate_routes();
            return false;
        };
        let Some(owner) = config["plugin"].as_str() else {
            self.invalidate_routes();
            return false;
        };
        self.owner = owner.to_owned();
        self.configure_pages(link, &config);
        let config: BTreeMap<_, _> = config["peers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| Some((p["device"].as_str()?, p)))
            .collect();
        let Ok(Value::Array(roster)) = link.call(api::PEERS, "list", json!({}), 2000) else {
            self.invalidate_routes();
            return false;
        };
        let roster: BTreeMap<_, _> = roster
            .iter()
            .filter_map(|row| Some((row["device"].as_str()?, row)))
            .take(api::MAX_REFERENCES)
            .collect();
        let Ok(references) =
            self.references(link, roster.keys().map(|s| (*s).to_owned()).collect(), None)
        else {
            self.invalidate_routes();
            return false;
        };
        let mut peers = BTreeMap::new();
        let mut aliases = BTreeMap::new();
        for entry in references.peers {
            let Some(device) = roster.get(entry.device.as_str()) else {
                continue;
            };
            let Some(config) = config.get(entry.device.as_str()) else {
                continue;
            };
            let allowed = |owner: &str, name: &str| {
                config["permissions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|p| p["owner"] == owner && p["name"] == name)
            };
            let can_reply = allowed("host", "peers.send")
                && allowed("host", "peers.watch")
                && allowed(&self.owner, GUARD);
            let route = if !can_reply {
                Route::Denied
            } else if device["online"] != true {
                Route::Offline
            } else if !device["accepts"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == SERVICE))
            {
                Route::Unavailable
            } else {
                Route::Ready
            };
            let policy = Policy {
                delete: config["settings"]["allow-message-deletion"] == true,
                clear: config["settings"]["allow-chat-clear"] == true,
            };
            aliases.insert(entry.device.clone(), entry.local_ref.clone());
            peers.insert(
                entry.local_ref,
                Peer {
                    alias: entry.device,
                    route,
                    policy,
                    can_reply,
                    name: device["name"].as_str().unwrap_or("Device").to_owned(),
                    sleeping: device["presence"] == "sleeping",
                },
            );
        }
        if self.engine.data.jobs.values().any(|job| {
            !job.held
                && job
                    .peer
                    .as_ref()
                    .is_some_and(|id| !peers.get(id).is_some_and(|peer| peer.can_reply))
        }) {
            let previous = self.engine.data.clone();
            self.engine
                .hold_revoked(|id| peers.get(id).is_some_and(|peer| peer.can_reply));
            if self.commit(link, previous, None, None).is_err() {
                return false;
            }
        }
        for old in self.peers.keys() {
            if !peers.contains_key(old) {
                self.engine.set_route(old.clone(), Route::Denied);
            }
        }
        for (id, peer) in &peers {
            self.engine.set_route(id.clone(), peer.route);
        }
        self.peers = peers;
        self.alias = aliases;
        let groups: Vec<_> = self
            .engine
            .data
            .rooms
            .keys()
            .filter_map(|room| match room {
                RoomKey::Group(g) => Some(g.clone()),
                _ => None,
            })
            .collect();
        for group in groups {
            if self.group_map(link, &group).is_err() {
                self.engine.clear_group_routes(&group);
            }
        }
        true
    }
    fn invalidate_routes(&mut self) {
        for peer in self.peers.keys() {
            self.engine.set_route(peer.clone(), Route::Denied);
        }
        self.peers.clear();
        self.alias.clear();
    }
    fn references<H: Wire + host::Data>(
        &self,
        link: &mut Link<'_, H>,
        devices: Vec<String>,
        scope: Option<ScopeId>,
    ) -> Result<api::ReferenceSet, Fault> {
        if devices.is_empty() && scope.is_none() {
            return Ok(api::ReferenceSet {
                peers: Vec::new(),
                local: None,
            });
        }
        let include_self = scope.is_some();
        link.ask(
            api::PEERS,
            api::REFERENCES,
            api::References {
                devices,
                scope,
                include_self,
            },
            2000,
        )
    }
    fn group_map<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        group: &ScopeId,
    ) -> Result<api::LocalReference, Fault> {
        let found = self.references(
            link,
            self.alias.keys().cloned().collect(),
            Some(group.clone()),
        )?;
        let local = found.local.ok_or(Fault::Storage)?;
        self.engine.set_group_routes(
            group.clone(),
            local.scoped_ref.clone(),
            found
                .peers
                .into_iter()
                .filter_map(|p| Some((p.local_ref, p.scoped_ref?)))
                .collect(),
        );
        Ok(local)
    }
    fn control<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        message: Value,
        now: u64,
    ) -> bool {
        if host::config_changed(&message) {
            self.due = 0;
            return true;
        }
        if message["type"] == "event"
            && message["provider"] == "host"
            && message["service"] == "peersus.language"
            && message["version"] == 1
            && message["event"] == "changed"
        {
            self.locale = message["data"]["language"]
                .as_str()
                .map_or(Locale::En, Locale::from_tag);
            return true;
        }
        if message["type"] == "event"
            && message["provider"] == "host"
            && message["service"] == page::NOTIFY
            && message["version"] == 1
            && message["event"] == page::OPENED
        {
            self.notices_opened(link);
            return true;
        }
        if message["type"] == "result" {
            self.file_result(link, &message, now);
            return true;
        }
        if message["type"] == "cancelled" {
            if let Some(ticket) = message["ticket"].as_u64()
                && let Some(handle) = self
                    .incoming
                    .iter()
                    .find(|(_, entry)| entry.ticket == ticket)
                    .map(|(h, _)| *h)
            {
                self.incoming.remove(&handle);
                link.host.close(handle);
            }
            return true;
        }
        if message["type"] != "call" {
            return true;
        }
        let Some(ticket) = message["ticket"].as_u64().filter(|id| *id != 0) else {
            return false;
        };
        if link.cancelled(ticket) {
            return true;
        }
        let service = message["service"].as_str();
        let method = message["method"].as_str();
        if message["caller"] != "host" || message["version"] != VERSION {
            return reply(link, ticket, Err(Fault::Invalid));
        }
        match (service, method) {
            (Some(SERVICE), Some("receive")) => {
                if self.incoming.len() >= MAX_ACTIVE_IN {
                    return reply(link, ticket, Err(Fault::Rate));
                }
                let Some(alias) = message["args"]["peer"].as_str() else {
                    return reply(link, ticket, Err(Fault::Invalid));
                };
                let peer = self
                    .alias
                    .get(alias)
                    .and_then(|p| self.peers.get(p).filter(|r| r.can_reply).map(|_| p.clone()));
                if peer.is_none() {
                    // A legitimate inbound call can race discovery/grants or a worker restart.
                    self.refresh(link, now);
                    if self.fault {
                        return false;
                    }
                }
                let Some(peer) = self
                    .alias
                    .get(alias)
                    .and_then(|p| self.peers.get(p).filter(|r| r.can_reply).map(|_| p.clone()))
                else {
                    return reply(link, ticket, Err(Fault::Invalid));
                };
                let Ok(handle) = link.host.open(false, ticket) else {
                    return reply(link, ticket, Err(Fault::Invalid));
                };
                // The time to send the data counts from the stream's opening, after any discovery above.
                let Ok(opened) = link.clock() else {
                    link.host.close(handle);
                    return false;
                };
                self.incoming.insert(
                    handle,
                    Incoming {
                        peer,
                        ticket,
                        expires: opened.saturating_add(DATA_WAIT),
                        bytes: Vec::new(),
                        cleared: Default::default(),
                    },
                );
                true
            }
            (Some("chat.local"), Some("view")) => {
                let result = self.page_event(link, &message["args"], now);
                reply(link, ticket, result)
            }
            _ => reply(link, ticket, Err(Fault::Invalid)),
        }
    }
    fn read<H: Wire + host::Data>(&mut self, link: &mut Link<'_, H>, handle: Handle, now: u64) {
        let Some(entry) = self.incoming.get_mut(&handle) else {
            return;
        };
        let mut buffer = [0; 8192];
        let room = (FRAME + 1)
            .saturating_sub(entry.bytes.len())
            .min(buffer.len());
        let got = link.host.read(handle, &mut buffer[..room]);
        if let Ok(n) = got
            && n > 0
            && n <= room
            && entry.bytes.len() + n <= FRAME
        {
            entry.bytes.extend_from_slice(&buffer[..n]);
            return;
        }
        let Some(entry) = self.incoming.remove(&handle) else {
            return;
        };
        link.host.close(handle);
        let Ok((packet, bytes)) = decode(&entry.bytes).filter_err(got == Ok(0)) else {
            let _ = reply(link, entry.ticket, Err(Fault::Invalid));
            return;
        };
        if let Packet::Mutation { scope, .. } = &packet {
            let room = match scope {
                Scope::Direct => RoomKey::Direct(entry.peer.clone()),
                Scope::Group(g) => RoomKey::Group(g.clone()),
            };
            if entry.cleared.contains(&room) {
                let _ = reply(link, entry.ticket, Err(Fault::Conflict));
                return;
            }
        }
        let result = self.accept(link, &entry, packet, bytes, now);
        if !link.cancelled(entry.ticket) {
            let _ = reply(link, entry.ticket, result.map(|_| json!({})));
        }
    }
    fn accept<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        entry: &Incoming,
        packet: Packet,
        bytes: &[u8],
        now: u64,
    ) -> Result<(), Fault> {
        let peer = &entry.peer;
        self.engine.admit(peer, &packet, bytes, now)?;
        let key = packet.key().clone();
        let does_reply = !matches!(packet, Packet::Result { .. });
        if let Packet::Mutation {
            scope: Scope::Group(group),
            change: Op::Define { .. },
            ..
        } = &packet
        {
            self.group_map(link, group)?;
        }
        if matches!(
            packet,
            Packet::Mutation {
                change: Op::Clear { .. } | Op::Delete { .. },
                ..
            }
        ) {
            self.refresh(link, now);
            if self.fault {
                return Err(Fault::Storage);
            }
        }
        let policy = self
            .peers
            .get(peer)
            .filter(|p| p.can_reply)
            .ok_or(Fault::Invalid)?
            .policy
            .clone();
        if link.cancelled(entry.ticket) {
            return Err(Fault::Conflict);
        }
        let previous = self.engine.data.clone();
        let mut promotion = None;
        let mut abort = None;
        let result = match &packet {
            Packet::Mutation {
                scope,
                change: Op::Begin { id, order, body },
                ..
            } => {
                let r = self
                    .engine
                    .apply(peer, packet.clone(), bytes, now, policy, None)?;
                if r != Verdict::Offset(0) {
                    r
                } else if let Some(held) = self.uploads.get(peer)
                    && held.key == key
                {
                    if &held.scope == scope
                        && &held.id == id
                        && held.order == *order
                        && &held.body == body
                    {
                        Verdict::Offset(held.offset)
                    } else {
                        Verdict::Invalid
                    }
                } else {
                    if let Some(old) = self.uploads.remove(peer) {
                        link.abort(old.stage);
                    }
                    if self.uploads.len() >= 2 {
                        return Err(Fault::Rate);
                    }
                    let file = body.attachments.first().ok_or(Fault::Invalid)?;
                    let stage = link.start(u64::from(file.bytes))?;
                    self.uploads.insert(
                        peer.clone(),
                        Upload {
                            key: key.clone(),
                            scope: scope.clone(),
                            id: id.clone(),
                            order: *order,
                            body: body.clone(),
                            stage,
                            offset: 0,
                            began: now,
                            idle: now,
                        },
                    );
                    Verdict::Offset(0)
                }
            }
            Packet::Mutation {
                scope,
                change: Op::Chunk { offset, len },
                ..
            } => {
                let check = self
                    .engine
                    .apply(peer, packet.clone(), bytes, now, policy, None)?;
                if check.terminal() {
                    check
                } else if let Some(held) = self
                    .uploads
                    .get_mut(peer)
                    .filter(|s| s.key == key && &s.scope == scope)
                {
                    let end = offset.checked_add(*len).ok_or(Fault::Invalid)?;
                    if end > held.body.attachments.first().ok_or(Fault::Invalid)?.bytes {
                        return Err(Fault::Invalid);
                    }
                    if *offset == held.offset {
                        link.append(held.stage, u64::from(*offset), bytes)?;
                        held.offset = end;
                        held.idle = now;
                    }
                    Verdict::Offset(held.offset)
                } else {
                    Verdict::Unknown
                }
            }
            Packet::Mutation {
                scope,
                change: Op::Finish,
                ..
            } => {
                if let Some(known) = self.engine.cursor(peer, &key) {
                    known
                } else if let Some(held) = self
                    .uploads
                    .get(peer)
                    .filter(|s| s.key == key && &s.scope == scope)
                {
                    let file = held.body.attachments.first().ok_or(Fault::Invalid)?;
                    if held.offset != file.bytes {
                        Verdict::Offset(held.offset)
                    } else {
                        let stage = held.stage;
                        let sealed = link.seal(stage)?;
                        let size = file.bytes;
                        let digest = file.sha256.clone();
                        let held = self.uploads.remove(peer).ok_or(Fault::Invalid)?;
                        abort = Some(stage);
                        if sealed.bytes != u64::from(size) || sealed.sha256 != digest {
                            link.abort(stage);
                            return Err(Fault::Invalid);
                        }
                        let room = match scope {
                            Scope::Direct => RoomKey::Direct(peer.clone()),
                            Scope::Group(g) => RoomKey::Group(g.clone()),
                        };
                        let reference = format!(
                            "assets/{}/recv_{}_{:016x}",
                            room.namespace(),
                            key.epoch.as_str(),
                            key.seq
                        );
                        // A finish refused here commits nothing, and its upload is already let go of: so is its stage.
                        let answer = match self.engine.finish_attachment(
                            peer,
                            key.clone(),
                            held.scope,
                            crate::core::IncomingMessage {
                                id: held.id,
                                order: held.order,
                                body: held.body,
                            },
                            now,
                            reference.clone(),
                        ) {
                            Ok(answer) => answer,
                            Err(fault) => {
                                link.abort(stage);
                                return Err(fault);
                            }
                        };
                        if answer == Verdict::Applied {
                            promotion = Some((reference, stage));
                        }
                        answer
                    }
                } else {
                    Verdict::Unknown
                }
            }
            Packet::Status { .. } => {
                let r = self.engine.apply(peer, packet, bytes, now, policy, None)?;
                if !r.terminal() {
                    self.uploads
                        .get(peer)
                        .filter(|s| s.key == key)
                        .map_or(r, |s| Verdict::Offset(s.offset))
                } else {
                    r
                }
            }
            _ => self.engine.apply(peer, packet, bytes, now, policy, None)?,
        };
        // The messages this packet brought from someone else: told of once they are kept.
        let fresh: Vec<String> = self
            .engine
            .data
            .rows
            .iter()
            .filter(|(key, row)| {
                !row.mine && row.body.is_some() && !previous.rows.contains_key(*key)
            })
            .map(|(key, _)| key.clone())
            .collect();
        let promoted = promotion.is_some();
        let committed = self.commit(link, previous, promotion, Some(entry.ticket));
        if let Some(stage) = abort
            && (!promoted || committed.is_err())
        {
            link.abort(stage);
        }
        committed?;
        self.notice(link, fresh, now);
        if result.terminal()
            && let Some(held) = self.uploads.get(peer)
            && held.key.epoch == key.epoch
            && held.key.seq <= key.seq
            && let Some(held) = self.uploads.remove(peer)
        {
            link.abort(held.stage);
        }
        self.engine.collect_groups();
        if does_reply
            && self.replies.len() < MAX_REPLIES
            && !self
                .replies
                .iter()
                .any(|r| &r.peer == peer && r.key == key && r.verdict == result)
        {
            self.replies.push_back(Reply {
                peer: peer.clone(),
                key,
                verdict: result,
            });
        }
        Ok(())
    }
    fn expire<H: Wire + host::Data>(&mut self, link: &mut Link<'_, H>, now: u64) {
        self.incoming.retain(|handle, entry| {
            if entry.expires <= now {
                link.host.close(*handle);
                let _ = reply(link, entry.ticket, Err(Fault::Rate));
                false
            } else {
                true
            }
        });
        let expired: Vec<_> = self
            .uploads
            .iter()
            .filter(|(_, s)| {
                s.idle.saturating_add(IDLE_STAGE) <= now
                    || s.began.saturating_add(TOTAL_STAGE) <= now
            })
            .map(|(p, _)| p.clone())
            .collect();
        for peer in expired {
            if let Some(s) = self.uploads.remove(&peer) {
                link.abort(s.stage);
            }
        }
        if self.outgoing.as_ref().is_some_and(|out| out.expires <= now)
            && let Some(out) = self.outgoing.take()
        {
            link.host.close(out.handle);
            self.note_transport(link, out.key.as_ref(), now, false);
        }
    }
    fn dispatch<H: Wire + host::Data>(&mut self, link: &mut Link<'_, H>, now: u64) {
        if self.outgoing.is_some() {
            return;
        }
        while let Some(result) = self.replies.pop_front() {
            if self
                .peers
                .get(&result.peer)
                .is_some_and(|p| p.route == Route::Ready)
            {
                self.send(
                    link,
                    result.peer,
                    Packet::Result {
                        key: result.key,
                        verdict: result.verdict,
                    },
                    &[],
                    None,
                    now,
                );
                return;
            }
        }
        if self.engine.data.jobs.is_empty() {
            return;
        }
        let peers: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, p)| p.route == Route::Ready)
            .map(|(id, _)| id.clone())
            .collect();
        let split = self
            .last_peer
            .as_ref()
            .map_or(0, |previous| peers.partition_point(|id| id <= previous));
        for peer in peers.iter().skip(split).chain(peers.iter().take(split)) {
            if !self.engine.can_dispatch(peer, now) {
                continue;
            }
            let lane = match self
                .engine
                .lane_epoch(peer)
                .cloned()
                .map_or_else(|| link.scope(), Ok)
            {
                Ok(epoch) => epoch,
                Err(_) => return,
            };
            let previous = self.engine.data.clone();
            let Ok(Some(offer)) = self.engine.next(peer, now, lane) else {
                self.engine.data = previous;
                continue;
            };
            if self.commit(link, previous, None, None).is_err() {
                return;
            }
            self.last_peer = Some(peer.clone());
            let mut bytes = offer.clear;
            if let Some((asset, offset, len)) = offer.asset {
                match link.read_asset(&asset, self.revision, offset, len) {
                    Ok(content) => bytes = content,
                    Err(_) => {
                        self.note_transport(link, Some(&offer.key), now, true);
                        return;
                    }
                }
            }
            self.send(
                link,
                peer.clone(),
                offer.packet,
                &bytes,
                Some(offer.key),
                now,
            );
            return;
        }
    }
    fn send<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        peer: ScopeId,
        packet: Packet,
        data: &[u8],
        key: Option<Key>,
        now: u64,
    ) {
        let Some(route) = self.peers.get(&peer) else {
            return;
        };
        let Ok(bytes) = encode(&packet, data) else {
            self.note_transport(link, key.as_ref(), now, true);
            return;
        };
        let claim = link.call(
            api::PEERS,
            "send",
            json!({"device":route.alias,"service":SERVICE,"version":VERSION}),
            2000,
        );
        let opened = match claim {
            Ok(v) => v["claim"]
                .as_u64()
                .and_then(|id| link.host.open(true, id).ok()),
            Err(error) => {
                self.note_transport(link, key.as_ref(), now, error == Refusal::Blocked);
                return;
            }
        };
        let Some(handle) = opened else {
            self.note_transport(link, key.as_ref(), now, false);
            return;
        };
        // The bounded local stream can buffer one frame; read its checked terminal even on an early write stop.
        let _ = host::write_all(link.host, handle, &bytes);
        let _ = link.host.shutdown(handle);
        self.outgoing = Some(Outgoing {
            handle,
            key,
            bytes: bytes.len() as u64,
            expires: now.saturating_add(DATA_WAIT),
        });
    }
    fn completed<H: Wire + host::Data>(&mut self, link: &mut Link<'_, H>, now: u64) {
        if let Some(out) = self.outgoing.take() {
            let answer = host::read_ack(link.host, out.handle);
            link.host.close(out.handle);
            let blocked = matches!(answer, Ok(DeliveryAck::NotAccepted));
            if matches!(answer,Ok(DeliveryAck::Delivered(n)) if n==out.bytes) {
                // Transport success neither promotes a row nor consumes its outbox job.
            }
            self.note_transport(link, out.key.as_ref(), now, blocked);
        }
    }
    fn note_transport<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        key: Option<&Key>,
        now: u64,
        blocked: bool,
    ) {
        if let Some(key) = key {
            let previous = self.engine.data.clone();
            self.engine.after_transport(key, now, blocked);
            let _ = self.commit(link, previous, None, None);
        }
    }
    /// The thread a reply joins, named the way the other devices name it: the first message of the thread the answered
    /// message is in. One that is gone, deleted or from another conversation, is not answered: the message is still sent,
    /// on its own.
    fn reply_target(&self, room: &RoomKey, key: Option<&str>) -> Option<Target> {
        let answered = self
            .engine
            .data
            .rows
            .get(key?)
            .filter(|row| &row.room == room && row.body.is_some())?;
        Some(self.engine.root(answered).unwrap_or(answered).target())
    }
}

enum Local<'a> {
    Open(RoomKey),
    /// A reply names the message it answers by that message's row key.
    Text {
        room: RoomKey,
        text: String,
        reply: Option<String>,
    },
    Attach {
        room: RoomKey,
        draft: &'a Draft,
        reply: Option<String>,
    },
    Group {
        aliases: Vec<String>,
        title: String,
    },
    OfferGroup {
        room: RoomKey,
        member: ScopeId,
    },
    Delete {
        room: RoomKey,
        id: Id,
        mode: Delete,
    },
    Clear {
        room: RoomKey,
        remote: bool,
        purge: bool,
    },
    React {
        room: RoomKey,
        row: String,
        emoji: Option<String>,
    },
    Retry {
        peer: ScopeId,
    },
    /// Delivered only from the local page adapter with its current visible row ID.
    Observe {
        room: RoomKey,
        row: String,
        visible: bool,
        foreground: bool,
    },
}
impl Runner {
    fn local<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        action: Local<'_>,
    ) -> Result<Value, Fault> {
        let previous = self.engine.data.clone();
        let mut promotion = None;
        let mut scope_changed = None;
        let value = match action {
            Local::Open(room) => {
                self.engine.open_room(room)?;
                json!({})
            }
            Local::Observe {
                room,
                row,
                visible,
                foreground,
            } => {
                self.engine
                    .observe_rows(&room, &[row], visible, foreground)?;
                json!({})
            }
            Local::Text { room, text, reply } => {
                let epoch = self
                    .engine
                    .data
                    .rooms
                    .get(&room)
                    .and_then(|r| r.origin.clone())
                    .map_or_else(|| link.scope(), Ok)?;
                let body = Body {
                    reply: self.reply_target(&room, reply.as_deref()),
                    ..Body::text(text)
                };
                json!({"id":self.engine.queue(room,epoch,body,None)?})
            }
            Local::Attach { room, draft, reply } => {
                let nonce = link.scope()?;
                let path = store::asset(&room, &nonce);
                let epoch = self
                    .engine
                    .data
                    .rooms
                    .get(&room)
                    .and_then(|r| r.origin.clone())
                    .unwrap_or(nonce);
                let (kind, media_type) = draft
                    .media_type
                    .map_or(("file", "application/octet-stream"), |media| {
                        (draft.selected.kind(), media)
                    });
                let body = Body {
                    kind: kind.into(),
                    text: None,
                    attachments: vec![Attachment {
                        name: draft.name.clone(),
                        media_type: media_type.into(),
                        bytes: u32::try_from(draft.stage.bytes).map_err(|_| Fault::Invalid)?,
                        sha256: draft.stage.sha256.clone(),
                        wave: draft.wave.clone(),
                    }],
                    reply: self.reply_target(&room, reply.as_deref()),
                    sent: None,
                };
                self.engine.queue(room, epoch, body, Some(path.clone()))?;
                promotion = Some((path, draft.stage.stage));
                json!({})
            }
            Local::Group { aliases, title } => {
                if !(2..=7).contains(&aliases.len()) {
                    return Err(Fault::Invalid);
                }
                let id = link.scope()?;
                let found = self.references(link, aliases, Some(id.clone()))?;
                let local = found.local.ok_or(Fault::Invalid)?;
                let mut roster = vec![Member {
                    id: local.scoped_ref.clone(),
                    label: local.name,
                }];
                for reference in found.peers {
                    let label = self
                        .peers
                        .get(&reference.local_ref)
                        .ok_or(Fault::Invalid)?
                        .name
                        .clone();
                    roster.push(Member {
                        id: reference.scoped_ref.ok_or(Fault::Invalid)?,
                        label,
                    });
                }
                roster.sort_by(|a, b| a.id.cmp(&b.id));
                self.group_map(link, &id)?;
                let room = self.engine.create_group(
                    Group {
                        id: id.clone(),
                        title,
                        creator: local.scoped_ref.clone(),
                        roster,
                    },
                    local.scoped_ref,
                )?;
                json!({"room":room})
            }
            Local::OfferGroup { room, member } => {
                self.engine.offer_group(&room, member)?;
                json!({})
            }
            Local::Delete { room, id, mode } => {
                self.engine.delete(&room, &id, mode)?;
                json!({})
            }
            Local::Clear {
                room,
                remote,
                purge,
            } => {
                self.engine.clear(&room, remote, purge)?;
                scope_changed = Some(room);
                json!({})
            }
            Local::React { room, row, emoji } => {
                self.engine.react(&room, &row, emoji)?;
                json!({})
            }
            Local::Retry { peer } => {
                self.engine.retry(&peer);
                json!({})
            }
        };
        // What the change lets go of in transfers under way is let go of only once the change is kept: one that could not
        // be kept leaves them as they were.
        self.commit(link, previous, promotion, None)?;
        if self
            .outgoing
            .as_ref()
            .and_then(|out| out.key.as_ref())
            .is_some_and(|key| !self.engine.data.jobs.contains_key(&key.operation))
            && let Some(out) = self.outgoing.take()
        {
            link.host.close(out.handle);
        }
        if let Some(scope) = scope_changed {
            for incoming in self.incoming.values_mut() {
                // Only streams that were already admitted can be invalidated by this clear.
                if incoming.cleared.len() < 64 {
                    incoming.cleared.insert(scope.clone());
                }
            }
            let scope = match scope {
                RoomKey::Direct(p) => self
                    .uploads
                    .get(&p)
                    .is_some_and(|s| s.scope == Scope::Direct)
                    .then_some(p),
                RoomKey::Group(g) => {
                    let peers: Vec<_> = self
                        .uploads
                        .iter()
                        .filter(|(_, s)| s.scope == Scope::Group(g.clone()))
                        .map(|(p, _)| p.clone())
                        .collect();
                    for peer in peers {
                        if let Some(old) = self.uploads.remove(&peer) {
                            link.abort(old.stage);
                        }
                    }
                    None
                }
            };
            if let Some(peer) = scope
                && let Some(old) = self.uploads.remove(&peer)
            {
                link.abort(old.stage);
            }
        }
        Ok(value)
    }
}
fn reply<H: Wire + host::Data>(
    link: &mut Link<'_, H>,
    ticket: u64,
    result: Result<Value, Fault>,
) -> bool {
    let result = match result {
        Ok(value) => json!({"status":"ok","value":value}),
        Err(error) => {
            let code = match error {
                Fault::Quota => "quota",
                Fault::Rate => "rate-limited",
                Fault::Storage => "unavailable",
                Fault::Conflict => "cancelled",
                Fault::Missing => "missing",
                Fault::Invalid => "invalid",
                Fault::NotDelivered => "not-delivered",
            };
            json!({"status":"error","code":code,"message":""})
        }
    };
    link.host
        .send(json!({"op":"reply","ticket":ticket,"result":result}))
        .is_ok()
}
trait WithComplete<T> {
    fn filter_err(self, keep: bool) -> Result<T, Fault>;
}
impl<T> WithComplete<T> for Result<T, Fault> {
    fn filter_err(self, keep: bool) -> Result<T, Fault> {
        if keep { self } else { Err(Fault::Invalid) }
    }
}
