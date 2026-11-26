//! What a device sent in and what is sent out: routing streams to the extension that exports them and holding chosen files.
use super::*;
use crate::receive::{Delivery, Terminal};

impl Runtime {
    /// Route an inbound peer stream to the plugin whose manifest exports its service.
    ///
    /// The stream is not handed to the plugin: the plugin is a separate process that must be
    /// told about it and then connects back to fetch the bytes. So it is set aside under the
    /// number of a call to the plugin's `receive`, and the plugin names that call when it
    /// connects. If the hub refuses the call nobody will ever come for it, so it is taken
    /// back at once and dropped, which also ends the peer's side, instead of waiting for
    /// the deadline.
    pub(super) fn route_inbound(
        &mut self,
        peer: &PeerId,
        service: &str,
        version: u32,
        stream: PluginStream,
        connection: usize,
        Stream { send, receive }: Stream,
    ) -> Result<(), &'static str> {
        // Everything that may refuse comes before anything is kept, and the peer list is
        // read fresh: a device forgotten after transport routing must not be served.
        let admitted = match admit(&self.plugins, &self.network.peers(), peer, service, version) {
            Ok(admitted) => admitted,
            Err(admission::Refusal::MissingPermission { plugin, permission }) => {
                self.warn_missing_permission(&plugin, permission, Some(peer.clone()));
                drop(receive);
                self.reject_stream(send);
                return Ok(());
            }
            Err(admission::Refusal::ExtensionAbsent) => {
                self.notifications
                    .unavailable_recipient(peer, service, version);
                drop(receive);
                self.reject_stream(send);
                return Ok(());
            }
            Err(admission::Refusal::Failed(reason)) => return Err(reason),
        };
        if admitted.replays_input {
            let guard = Permission::new(HOST, crate::platform_services::INPUT_CAPABILITY)
                .ok_or("Invalid input permission")?;
            if !self
                .standalone
                .get(&admitted.plugin)
                .is_some_and(|given| given.allows(&guard))
            {
                self.warn_missing_permission(&admitted.plugin, guard, None);
                drop(receive);
                self.reject_stream(send);
                return Ok(());
            }
        }
        let owner = crate::parked::Owner {
            peer: peer.clone(),
            plugin: admitted.plugin.clone(),
        };
        let visit = match stream {
            PluginStream::Visit => {
                if !admitted.replays_input {
                    self.reject_stream(send);
                    return Ok(());
                }
                self.keep_inbound_visit(owner, admitted, connection, Stream { send, receive });
                return Ok(());
            }
            PluginStream::Data {
                visit: Some(stream),
            } => {
                let key = VisitKey { connection, stream };
                if !admitted.replays_input
                    || !self.inbound_visits.get(&owner).is_some_and(|visit| {
                        visit.key == key
                            && visit.service.service == service
                            && visit.service.version == version
                            && visit.guard == admitted.guard
                            && visit.lease.active()
                    })
                {
                    self.reject_stream(send);
                    return Ok(());
                }
                Some(key)
            }
            PluginStream::Data { visit: None } => None,
        };
        if self.parked_stops.len() >= 2 * crate::parked::MAX_PARKED {
            return Err("Too many transfers are waiting for plugins");
        }
        let remote_stop = send.stopped();
        self.next_call = self.next_call.checked_add(1).ok_or("Too many calls")?;
        let id = self.next_call;
        let (host, aliases) = (&self.host, &self.aliases);
        keep_and_announce(
            &mut self.parked,
            &mut self.receiving,
            id,
            owner,
            Stream { send, receive },
            std::time::Instant::now(),
            || {
                host.submit(peersus_api::message::Operation::Call {
                    id,
                    provider: admitted.plugin.as_str().to_owned(),
                    service: admitted.service,
                    version: admitted.version,
                    method: crate::plugin_host::RECEIVE_METHOD.to_owned(),
                    args: peer_argument(aliases, &admitted.plugin, peer),
                    timeout_ms: 15_000,
                })
                .map_err(|error| refusal_text(&error))
            },
        )?;
        if let Some(receive) = self.receiving.get_mut(&id) {
            receive.guard = admitted.guard;
            receive.replays_input = admitted.replays_input;
            receive.visit = visit;
            receive.parked_stop = Some(self.parked_stops.spawn(async move {
                matches!(within(crate::parked::MAX_WAIT, remote_stop).await,
                    Some(Ok(Some(code))) if code == crate::receive::PERMISSION_REVOKED.into())
                .then_some(id)
            }));
        }
        Ok(())
    }

    fn keep_inbound_visit(
        &mut self,
        owner: crate::parked::Owner,
        admitted: admission::Admitted,
        connection: usize,
        Stream {
            mut send,
            mut receive,
        }: Stream,
    ) {
        let key = VisitKey {
            connection,
            stream: send.id().into(),
        };
        let previous = self.inbound_visits.get(&owner);
        if (previous.is_none() && self.inbound_visits.len() >= MAX_INPUT_VISITS)
            || self.inbound_visit_ends.len() >= 2 * MAX_INPUT_VISITS
            || previous.is_some_and(|previous| {
                previous.key.connection == key.connection && previous.key.stream >= key.stream
            })
        {
            self.reject_stream(send);
            return;
        }
        self.close_inbound_visits(|candidate, _| candidate == &owner);
        let lease = crate::platform_services::InputLease::new();
        let (close, mut closed) = oneshot::channel();
        self.inbound_visits.insert(
            owner.clone(),
            InboundVisit {
                key,
                service: peersus_net::streams::ServiceRef {
                    service: admitted.service,
                    version: admitted.version,
                },
                guard: admitted.guard,
                lease: lease.clone(),
                close,
            },
        );
        self.inbound_visit_ends.spawn(async move {
            let ready = tokio::select! {
                biased;
                _ = &mut closed => false,
                ready = crate::receive::send_terminal(&mut send, Terminal::received(0)) => ready,
            };
            if ready {
                let mut invalid_payload = [0];
                // EOF, a reset, data on this empty half, or loss of the authenticated
                // connection all end the same visit. QUIC already bounds peer silence.
                tokio::select! {
                    biased;
                    _ = &mut closed => {}
                    _ = receive.read(&mut invalid_payload) => {}
                }
            }
            lease.revoke();
            let _ = receive.stop(crate::receive::PERMISSION_REVOKED.into());
            (owner, key)
        });
    }

    pub(super) fn close_inbound_visits(
        &mut self,
        matches: impl FnMut(&crate::parked::Owner, &mut InboundVisit) -> bool,
    ) {
        for (owner, visit) in self.inbound_visits.extract_if(matches).collect::<Vec<_>>() {
            visit.lease.revoke();
            let _ = visit.close.send(());
            self.release_replays(|candidate, replay| {
                candidate == &owner && replay.visit == Some(visit.key)
            });
            let calls = self.decline_receives(&owner.plugin, Some(&owner.peer), |entry| {
                entry.owner == owner && entry.visit == Some(visit.key)
            });
            self.cancel_calls(calls);
        }
    }

    pub(super) fn prune_inbound_visits(&mut self) {
        let closed: Vec<_> = self
            .inbound_visits
            .iter()
            .filter_map(|(owner, visit)| {
                (!self.network.shown_to(&owner.peer)
                    || self
                        .network
                        .connection(&owner.peer)
                        .is_none_or(|connection| connection.stable_id() != visit.key.connection))
                .then_some(visit.key)
            })
            .collect();
        self.close_inbound_visits(|_, visit| closed.contains(&visit.key));
    }

    pub(super) fn hold_outbound_visit(
        &mut self,
        connection: &iroh::endpoint::Connection,
        destination: &RemoteDestination,
        grant: Option<&Permission>,
    ) -> Result<watch::Receiver<VisitStatus>, SendRefusal> {
        if let Some(held) = &self.outbound_visit {
            if held.connection != connection.stable_id()
                || held.service.service != destination.service
                || held.service.version != destination.version
            {
                // This says nothing about the other device's grants. Report the local route
                // failure instead of teaching the carrier to remember a remote refusal.
                return Err("The input visit no longer matches its destination".into());
            }
            return Ok(held.ready.clone());
        }
        if self.outbound_visit_ends.len() >= MAX_INPUT_VISITS {
            return Err("Too many input visits are waiting to close".into());
        }

        let service = peersus_net::streams::ServiceRef {
            service: destination.service.clone(),
            version: destination.version,
        };
        let request = peersus_net::streams::Request::PluginVisit {
            service: service.service.clone(),
            version: service.version,
        };
        let (hold, mut ended) = oneshot::channel();
        let (phase, ready) = watch::channel(VisitStatus::Opening);
        self.outbound_visit = Some(OutboundVisit {
            connection: connection.stable_id(),
            service,
            grant: grant.cloned(),
            ready: ready.clone(),
            _hold: hold,
        });
        let connection = connection.clone();
        let serial = self.last_visit;
        self.outbound_visit_ends.spawn(async move {
            let opening = async {
                let (mut send, receive) = connection.open_bi().await.ok()?;
                peersus_net::streams::send(&mut send, &request).await.ok()?;
                let terminal = crate::receive::read_terminal(receive).await.ok()?;
                Some((send, terminal))
            };
            let open = tokio::select! {
                biased;
                _ = &mut ended => return None,
                open = within(OPEN_LIMIT, opening) => open.flatten(),
            };
            let mut send = match open {
                Some((send, Terminal::Received { bytes: 0, .. })) => send,
                Some((_, Terminal::NotAccepted { .. })) => {
                    phase.send_replace(VisitStatus::Declined);
                    return None;
                }
                _ => {
                    phase.send_replace(VisitStatus::Failed);
                    return None;
                }
            };
            phase.send_replace(VisitStatus::Live(send.id().into()));
            let stopped = send.stopped();
            let blocked = tokio::select! {
                biased;
                _ = &mut ended => {
                    let _ = send.reset(crate::receive::PERMISSION_REVOKED.into());
                    return None;
                }
                result = stopped => matches!(result,
                    Ok(Some(code)) if code == crate::receive::PERMISSION_REVOKED.into()),
            };
            phase.send_replace(if blocked {
                VisitStatus::Declined
            } else {
                VisitStatus::Failed
            });
            Some((serial, blocked))
        });
        Ok(ready)
    }

    fn reject_stream(&mut self, send: iroh::endpoint::SendStream) {
        if self.rejection_replies.len() < crate::parked::MAX_PARKED {
            self.rejection_replies.spawn(crate::receive::write_terminal(
                send,
                Terminal::not_accepted(),
            ));
        }
    }

    pub(super) fn prune_remote_refusals(&mut self) {
        self.remote_refusals
            .prune(|destination| destination.readiness(&self.network));
    }

    /// `visit` is the person's crossing the send belongs to, `None` for a send that belongs to none.
    pub(super) fn reserve_remote(
        &mut self,
        destination: &RemoteDestination,
        readiness: PeerReadiness,
        visit: Option<u64>,
    ) -> Result<watch::Receiver<bool>, SendRefusal> {
        self.remote_refusals
            .reserve(destination, readiness, visit, |destination| {
                destination.readiness(&self.network)
            })
    }

    pub(super) fn record_remote_no(&mut self, no: &RemoteNo) -> bool {
        let believed = self
            .remote_refusals
            .record(no, self.last_visit, |destination| {
                destination.readiness(&self.network)
            });
        if !believed {
            return false;
        }
        let destination = &no.destination;
        let retired = self.carrying_out.signal(
            &destination.owner.plugin,
            Some(&destination.owner.peer),
            |_, handle| {
                handle
                    .refusal
                    .as_ref()
                    .is_some_and(|reservation| reservation.same_channel(&no.reservation))
            },
        );
        self.retire_outbound(&retired);
        true
    }

    pub(super) fn retire_outbound(&mut self, stopped: &[(u64, crate::parked::Owner)]) {
        for (claim, owner) in stopped {
            self.outbound.replace_for(*claim, owner, Outbound::Declined);
        }
    }

    /// Whether another claim may be issued. A claim keeps its place after a withdrawal, and
    /// again while the neutral reply to its late presenter is still being written, so that no
    /// reply is ever left without room.
    pub(super) fn has_claim_room(&self) -> bool {
        self.outbound.len() + self.claim_replies.len() < crate::parked::MAX_PARKED
    }

    /// A guest result cannot authorize an owner warning or decide the paired wire outcome.
    pub(super) fn finish_receive(
        &mut self,
        id: u64,
        mut receiving: admission::Receiving,
        result: &peersus_api::message::Outcome,
    ) -> PluginId {
        receiving.retire();
        if let Some(stop) = receiving.stop {
            let _ = stop.send(());
        } else {
            drop(self.parked.claim(id, std::time::Instant::now()));
        }
        if receiving.visit.is_none() || !matches!(result, peersus_api::message::Outcome::Ok { .. })
        {
            self.release_replays(|_, controller| controller.call == id);
        }
        receiving.owner.plugin
    }

    /// A peer's live sending lease ended. Drop the guest call rather than letting it
    /// interpret a partial data copy or keep any held platform input.
    pub(super) fn finish_revoked_receive(&mut self, call: u64) {
        self.release_replays(|_, controller| controller.call == call);
        self.cancel_calls(vec![call]);
    }

    fn decline_receives(
        &mut self,
        plugin: &PluginId,
        peer: Option<&PeerId>,
        depends: impl Fn(&admission::Receiving) -> bool,
    ) -> Vec<u64> {
        let cancelled: BTreeSet<_> = self
            .receiving
            .iter_mut()
            .filter(|(_, entry)| depends(entry))
            .map(|(&id, entry)| {
                entry.retire();
                id
            })
            .collect();
        self.carrying.signal(plugin, peer, |id, handle| {
            cancelled.contains(&id) && handle.revoke()
        });
        for (_, _, stream) in self
            .parked
            .take_for(plugin, peer, |id| cancelled.contains(&id))
        {
            drop(stream.receive);
            self.reject_stream(stream.send);
        }
        cancelled.into_iter().collect()
    }

    pub(super) fn withdraw_grant_work(
        &mut self,
        plugin: &PluginId,
        peer: Option<&PeerId>,
        permission: &Permission,
    ) {
        let send_permission =
            peer.is_some() && permission.owner == HOST && permission.name == PEERS_SEND_CAPABILITY;
        let replay_permission = peer.is_none()
            && permission.owner == HOST
            && permission.name == crate::platform_services::INPUT_CAPABILITY;
        let no_entry = peer
            .and_then(|peer| self.peer(peer))
            .is_some_and(|peer| !peer.grants.contains_key(plugin));
        let capture_permission = peer.is_some()
            && permission.owner == plugin.as_str()
            && self
                .plugins
                .find(plugin)
                .and_then(|index| self.plugins.get(index))
                .and_then(|spec| spec.manifest.contributions.input.as_ref())
                .is_some_and(|input| input.capability == permission.name);
        let stops_capture = self
            .capture
            .as_ref()
            .is_some_and(|active| &active.plugin == plugin && peer == active.device.as_ref())
            && self
                .outbound_visit
                .as_ref()
                .is_some_and(|visit| visit.grant.as_ref() == Some(permission));
        let stopped_sends = self.carrying_out.signal(plugin, peer, |_, handle| {
            (send_permission
                || handle.grant.as_ref() == Some(permission)
                || handle.capture_grant.as_ref() == Some(permission))
                && handle.revoke()
        });
        self.retire_outbound(&stopped_sends);
        self.close_inbound_visits(|owner, visit| {
            &owner.plugin == plugin
                && peer.is_none_or(|peer| peer == &owner.peer)
                && (replay_permission
                    || visit.guard.as_ref() == Some(permission)
                    || (no_entry && visit.guard.is_none()))
        });
        let depends = |entry: &admission::Receiving| {
            &entry.owner.plugin == plugin
                && peer.is_none_or(|peer| peer == &entry.owner.peer)
                && ((replay_permission && entry.replays_input)
                    || entry.guard.as_ref() == Some(permission)
                    || (no_entry && entry.guard.is_none()))
        };
        let mut cancelled = self.decline_receives(plugin, peer, depends);
        self.release_replays(|owner, controller| {
            &owner.plugin == plugin
                && peer.is_none_or(|peer| peer == &owner.peer)
                && (replay_permission
                    || controller.guard.as_ref() == Some(permission)
                    || (no_entry && controller.guard.is_none()))
        });
        if let Some(peer) = peer
            && no_entry
        {
            // The pairing entry itself admits actions that have no named capability.
            cancelled.extend(self.in_flight.take_for_device(plugin, peer));
            self.board.forget_for(peer, Some(plugin));
        } else if let Some(peer) = peer
            && permission.owner == plugin.as_str()
            && let Some(spec) = self
                .plugins
                .find(plugin)
                .and_then(|index| self.plugins.get(index))
        {
            for action in spec
                .manifest
                .contributions
                .actions
                .iter()
                .filter(|action| action.capability.as_ref() == Some(&permission.name))
            {
                let key = Key {
                    plugin: plugin.clone(),
                    action: action.id.clone(),
                    device: Some(peer.clone()),
                };
                if let Some(id) = self.in_flight.take_action(&key) {
                    self.board.finished(&key, ActionState::Cancelled);
                    cancelled.push(id);
                }
            }
        }
        if send_permission || capture_permission || stops_capture {
            self.halt_capture(
                plugin,
                peer,
                peersus_api::message::Outcome::MissingPermission {
                    permission: permission.clone(),
                },
            );
        }
        self.cancel_calls(cancelled);
    }

    fn release_peer_input(&self, owner: &crate::parked::Owner) {
        if let Some(platform) = &self.platform_services {
            platform.release_peer_input(
                &owner.plugin,
                &self.aliases.alias(&owner.plugin, &owner.peer),
            );
        }
    }

    pub(super) fn release_replays(
        &mut self,
        mut matches: impl FnMut(&crate::parked::Owner, &admission::ReplayController) -> bool,
    ) {
        for (owner, controller) in self
            .replay_controllers
            .extract_if(|owner, lease| matches(owner, lease))
            .collect::<Vec<_>>()
        {
            controller.lease.revoke();
            self.release_peer_input(&owner);
        }
    }

    /// The receive that may answer a platform call the hub admitted as `ticket`: the oldest live
    /// copy of this extension's receives from `peer` whose data was claimed before that call was
    /// admitted. A call queued by an earlier visit can reach the owner after the next visit's data
    /// connection was claimed; it must never take that newer copy's gate.
    pub(super) fn replay_source(
        &self,
        plugin: &PluginId,
        peer: &PeerId,
        ticket: u64,
    ) -> Option<(u64, &admission::Receiving)> {
        self.receiving
            .iter()
            .filter(|(_, entry)| {
                &entry.owner.plugin == plugin
                    && &entry.owner.peer == peer
                    && entry
                        .replay
                        .as_ref()
                        .is_some_and(|(claimed, lease)| *claimed < ticket && lease.active())
            })
            .min_by_key(|(call, _)| **call)
            .map(|(call, entry)| (*call, entry))
    }

    /// Bind a queued platform request to the oldest claimed, still-live receive.
    pub(super) fn replay_lease(
        &mut self,
        caller: &PluginId,
        alias: &str,
        ticket: u64,
    ) -> Option<crate::platform_services::InputLease> {
        let peers = self.network.peers();
        let peer = self
            .aliases
            .resolve(caller, alias, peers.iter().map(|peer| &peer.id))?;
        let (call, entry) = self.replay_source(caller, peer, ticket)?;
        let peer = peers.iter().find(|entry| &entry.id == peer)?;
        if !permitted(
            peer,
            caller,
            entry.guard.as_ref().map(|guard| guard.name.as_str()),
        ) {
            return None;
        }
        let owner = entry.owner.clone();
        let (_, lease) = entry.replay.clone()?;
        let current = admission::ReplayController {
            call,
            visit: entry.visit,
            guard: entry.guard.clone(),
            lease,
        };
        self.keep_replay(owner, current)
    }

    pub(super) fn keep_replay(
        &mut self,
        owner: crate::parked::Owner,
        current: admission::ReplayController,
    ) -> Option<crate::platform_services::InputLease> {
        const MAX_REPLAY_CONTROLLERS: usize = 256;
        let call = current.call;
        let lease = current.lease.clone();
        if !lease.active() {
            return None;
        }
        if let Some(previous) = self.replay_controllers.get(&owner) {
            if previous.call > call {
                return None;
            }
            if previous.call == call {
                return Some(lease);
            }
            previous.lease.revoke();
            // A live, explicitly admitted visit authorizes rotation independently
            // of when the old task next polls the transport acknowledgment.
            if current.visit.is_none()
                || previous.visit != current.visit
                || previous.guard != current.guard
            {
                self.release_peer_input(&owner);
            }
        } else if self.replay_controllers.len() >= MAX_REPLAY_CONTROLLERS {
            return None;
        }
        for (&older, entry) in &self.receiving {
            if older < call
                && entry.owner == owner
                && let Some((_, lease)) = &entry.replay
            {
                lease.revoke();
            }
        }
        self.replay_controllers.insert(owner, current);
        Some(lease)
    }

    /// A plugin has connected back for the bytes of a call. Pair its connection with what
    /// was set aside for that call and carry them across, in the background.
    ///
    /// The pairing is by the number the application gave the call, but that number alone
    /// names no one: every participant numbers its own calls, so a plugin can call itself
    /// with the very number the application used, and hold a perfectly genuine right for
    /// that call. So the hub's stamp of who made the call is checked first, and only the
    /// application's own calls may collect what the application set aside. The stream can
    /// be claimed once. If nothing is parked under that number (it expired, was already
    /// taken, or the device was forgotten meanwhile) the plugin's connection is simply
    /// dropped, and the plugin learns it got nothing.
    pub(super) fn begin_transfer(&mut self, accepted: Accepted) {
        let Accepted {
            plugin,
            right,
            stream,
        } = accepted;
        let right = match right {
            peersus_plugin::data::Right::Hub(right) => right,
            // A claim names a send the application opened for this plugin. Who is presenting
            // it is the channel the connection arrived on, never anything the plugin wrote,
            // and the entry is taken only if it was issued to exactly that plugin, so one
            // plugin cannot collect another's send or use up its number by trying.
            peersus_plugin::data::Right::Claim(claim) => {
                let Ok(plugin) = PluginId::new(&plugin) else {
                    return;
                };
                if claim & crate::page_frames::CLAIM_BIT != 0 {
                    self.page_frames.accept(&plugin, claim, stream);
                    return;
                }
                if claim & crate::extension_data::CLAIM_BIT != 0 {
                    if let Some(storage) = &self.extension_data {
                        storage.accept(&plugin, claim, stream);
                    }
                    return;
                }
                match self
                    .outbound
                    .claim_for(claim, &plugin, std::time::Instant::now())
                {
                    Some((_, Outbound::Waiting(waiting))) => {
                        let _ = waiting.send(stream);
                    }
                    Some((_, Outbound::Declined)) => {
                        self.claim_replies.spawn(async move {
                            let mut stream = stream;
                            acknowledge_no(&mut stream).await;
                        });
                    }
                    None => {}
                }
                return;
            }
        };
        if !may_collect(right.caller()) {
            return;
        }
        let Some((owner, parked)) = self
            .parked
            .claim_owned(right.call_id(), std::time::Instant::now())
        else {
            return;
        };
        let call = right.call_id();
        let Some(receiving) = self.receiving.get_mut(&call) else {
            return;
        };
        if let Some(watcher) = receiving.parked_stop.take() {
            watcher.abort();
        }
        let (stop, ended) = oneshot::channel();
        let (revoke, mut revoked) = watch::channel(false);
        let replay = if let Some(key) = receiving.visit {
            let Some(held) = self
                .inbound_visits
                .get(&owner)
                .filter(|held| held.key == key)
            else {
                self.cancel_calls(vec![call]);
                self.reject_stream(parked.send);
                return;
            };
            Some(held.lease.for_copy())
        } else {
            receiving
                .replays_input
                .then(crate::platform_services::InputLease::new)
        };
        receiving.replay = replay.clone().map(|lease| (right.ticket(), lease));
        let data_commit = self
            .extension_data
            .as_ref()
            .map(|_| crate::extension_data::Fence::new());
        receiving.data_commit = data_commit.clone().map(|fence| (right.ticket(), fence));
        receiving.stop = Some(stop);
        let handle = self.transfers.spawn(async move {
            let Stream { mut send, mut receive } = parked;
            let peer_stop = send.stopped();
            let copied = tokio::select! {
                biased;
                true = async { revoked.wait_for(|revoked| *revoked).await.is_ok() } => {
                    if let Some(fence) = &data_commit { fence.revoke(); }
                    if let Some(lease) = &replay { lease.revoke(); }
                    crate::receive::write_terminal(send, Terminal::not_accepted()).await;
                    return (Direction::Inbound, call, Some(Delivery::NotAccepted));
                }
                stopped = peer_stop => {
                    if let Some(fence) = &data_commit { fence.revoke(); }
                    if let Some(lease) = &replay { lease.revoke(); }
                    return (Direction::Inbound, call, Some(match stopped {
                        Ok(Some(code)) if code == crate::receive::PERMISSION_REVOKED.into() => Delivery::RemoteRevoked,
                        _ => Delivery::Failed("A transfer to a plugin was interrupted"),
                    }));
                }
                delivery = crate::receive::deliver(
                    &mut receive, stream, crate::receive::DEFAULT_LIMIT, peersus_plugin::pump::IDLE_LIMIT,
                ) => Some(delivery),
                _ = ended => None,
            };
            // RESET_STREAM can reach us ahead of the matching reverse STOP_SENDING.
            if matches!(copied, Some(Delivery::Failed(_)))
                && matches!(within(Duration::ZERO, receive.received_reset()).await,
                    Some(Ok(Some(code))) if code == crate::receive::PERMISSION_REVOKED.into())
            {
                if let Some(fence) = &data_commit { fence.revoke(); }
                if let Some(lease) = &replay { lease.revoke(); }
                return (Direction::Inbound, call, Some(Delivery::RemoteRevoked));
            }
            if let Some(Delivery::Done(bytes)) = copied {
                if let Some(lease) = &replay {
                    if !crate::receive::send_terminal(&mut send, Terminal::received(bytes)).await {
                        lease.revoke();
                        return (Direction::Inbound, call, Some(Delivery::Failed("A transfer to a plugin was interrupted")));
                    }
                    match within(crate::receive::TERMINAL_WAIT, send.stopped()).await {
                        Some(Ok(None)) => {}
                        Some(Ok(Some(code))) if code == crate::receive::PERMISSION_REVOKED.into() => {
                            lease.revoke();
                            return (Direction::Inbound, call, Some(Delivery::RemoteRevoked));
                        }
                        _ => {
                            lease.revoke();
                            return (Direction::Inbound, call, Some(Delivery::Failed("A transfer to a plugin was interrupted")));
                        }
                    }
                } else {
                    crate::receive::write_terminal(send, Terminal::received(bytes)).await;
                }
            } else {
                if let Some(fence) = &data_commit { fence.revoke(); }
                if let Some(lease) = &replay { lease.revoke(); }
                let _ = send.finish();
            }
            (Direction::Inbound, call, copied)
        });
        // Recorded under the device and the plugin, or a permission withdrawn later could
        // not reach a transfer that is already carrying bytes.
        self.carrying.started(
            call,
            owner,
            TransferStop {
                abort: handle,
                revoke,
                grant: None,
                refusal: None,
                capture_grant: None,
                capture_visit: None,
            },
        );
    }

    /// The most files one command may offer.
    pub(super) const MOST_FILES_AT_ONCE: usize = 16;

    /// How many files of the folders being sent are offered at each turn of the loop, before the events get theirs.
    pub(super) const FOLDER_FILES_PER_TURN: usize = 8;

    /// Offer `files` to `peer`, each under the name that goes with it. The one place that judges a send, so that
    /// every way of sending is judged the same: between one and sixteen files, every source opened before any is
    /// offered (so one bad file cannot make a partial offer of a command that asked for several), and what was
    /// already offered withdrawn if a later offer fails.
    pub(super) fn offer_sources(
        &mut self,
        peer: &PeerId,
        sources: Vec<Source>,
    ) -> Result<Reply, &'static str> {
        // A send is the person's own act: while this device keeps to itself it is what shows it to that device.
        self.opened_by_person(peer, None);
        let mut transfers = Vec::with_capacity(sources.len());
        for source in sources {
            match self.network.send(peer, source, now()) {
                Ok(id) => transfers.push(id),
                Err(_) => {
                    // A later offer failed: what was offered before it is withdrawn, so that the person does not
                    // find half of what they chose on the other device.
                    for id in &transfers {
                        let _ = self.network.cancel(id);
                    }
                    return Err("Could not offer selected files");
                }
            }
        }
        Ok(Reply::Transfers(transfers))
    }

    /// Every file the interface kept open by number, taken and judged BEFORE anything is offered, so that one that is unknown or
    /// fails (a name the rule refuses) never leaves a partial offer of a command that asked for several. Whatever is not taken
    /// when the command is refused (too many, a number that is gone, a file that fails) is let go of here, by every number of the
    /// command: the interface cannot know which the owner reached, and a number kept would fill the registry for the next choice.
    pub(super) async fn take_held(
        &mut self,
        files: Vec<(u64, String)>,
    ) -> Result<Vec<Source>, &'static str> {
        let numbers: Vec<u64> = files.iter().map(|(number, _)| *number).collect();
        let result = self.take_held_sources(files).await;
        if result.is_err() {
            for number in numbers {
                self.held.release(number);
            }
        }
        result
    }

    pub(super) async fn take_held_sources(
        &mut self,
        files: Vec<(u64, String)>,
    ) -> Result<Vec<Source>, &'static str> {
        if files.is_empty() || files.len() > Self::MOST_FILES_AT_ONCE {
            return Err("Choose between one and sixteen files");
        }
        let mut sources = Vec::with_capacity(files.len());
        for (number, name) in files {
            let Some(file) = self.held.take(number) else {
                // Said where it can be read by whoever debugs this, never to the person and never with a name. A number
                // this registry never gave out came from another registry; one it gave and no longer holds was taken or let
                // go by something else first.
                tracing::warn!(
                    number,
                    reason = ?self.held.why_missing(number),
                    "A chosen file's number is not held"
                );
                return Err("A chosen file is no longer available");
            };
            sources.push(
                Source::from_file(file, &name)
                    .await
                    .map_err(|_| "Could not open selected file")?,
            );
        }
        Ok(sources)
    }

    /// The code that hands over `sources`, as text. The files wait with the code, in the network host.
    pub(super) fn files_code(&mut self, sources: Vec<Source>) -> Result<Reply, &'static str> {
        let invitation = self
            .network
            .invite_for_files(sources, now())
            .map_err(|_| "Could not create invitation")?;
        self.files_invitation_reply(invitation)
    }

    pub(super) fn files_invitation_reply(
        &self,
        invitation: Invitation,
    ) -> Result<Reply, &'static str> {
        let generation = self
            .network
            .files_code()
            .map(|code| code.generation)
            .ok_or("Could not create invitation")?;
        let expires_at = invitation.expires_at;
        let code = invitation
            .encode()
            .map_err(|_| "Could not create invitation")?;
        Ok(Reply::FilesInvitation {
            code,
            generation,
            expires_at,
        })
    }

    pub(super) fn can_queue_destination(
        &self,
        destination: &FileDestination,
        count: usize,
    ) -> Result<bool, &'static str> {
        match destination {
            FileDestination::Peer { peer } => self
                .network
                .can_queue_files(peer, count)
                .map_err(|_| "Device is offline"),
            FileDestination::Invitation { generation } => self
                .network
                .can_queue_selection(*generation, count, now())
                .map_err(|_| "Files selection is no longer active"),
            FileDestination::Nearby { peer } => {
                self.nearby_invitation(peer)?;
                Ok(true)
            }
        }
    }

    pub(super) fn queue_destination(
        &mut self,
        destination: FileDestination,
        sources: Vec<Source>,
    ) -> Result<Reply, &'static str> {
        match destination {
            FileDestination::Peer { peer } => self.offer_sources(&peer, sources),
            FileDestination::Invitation { generation } => {
                self.network
                    .queue_selection(generation, sources, now())
                    .map_err(|_| "Could not queue selected files")?;
                Ok(Reply::Transfers(Vec::new()))
            }
            FileDestination::Nearby { peer } => {
                let invitation = self.nearby_invitation(&peer)?;
                let promised = self
                    .network
                    .send_nearby(&invitation, sources)
                    .map_err(|_| "Could not offer selected files")?;
                // What was announced admits once: once it was used it is not kept, and the next one that device
                // announces is heard as a new one. One that could not be used stays for another try.
                self.nearby_offers.retain(|(id, _)| id != &peer);
                // The transfers these will be once that device admits this one: none exists yet, and none ever does
                // when it does not.
                Ok(Reply::Transfers(promised))
            }
        }
    }

    /// The invitation the device nearby `peer` announced, if it is heard now, said it takes files, and is brought
    /// near by the strength the person set: the same rule the page proposes it by, kept here so that no window or
    /// tool can send to a device that is merely in range.
    pub(super) fn nearby_invitation(&self, peer: &PeerId) -> Result<Invitation, &'static str> {
        const GONE: &str = "That device is no longer nearby";
        if !self.settings.current().nearby_send {
            return Err(GONE);
        }
        if !self
            .nearby
            .strangers
            .iter()
            .any(|stranger| &stranger.id == peer && stranger.receives && stranger.near)
        {
            return Err(GONE);
        }
        self.nearby_offers
            .iter()
            .find(|(id, _)| id == peer)
            .and_then(|(_, text)| Invitation::decode(text, now()).ok())
            .ok_or(GONE)
    }
}
