//! What a device sent in and what is sent out: routing streams to the extension that exports them and holding chosen files.
use super::*;

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
        send: iroh::endpoint::SendStream,
        receive: iroh::endpoint::RecvStream,
    ) -> Result<(), &'static str> {
        // Everything that may refuse comes before anything is kept, and the peer list is
        // read fresh: a device forgotten after transport routing must not be served.
        let admitted = admit(&self.plugins, &self.network.peers(), peer, service, version)?;
        self.next_call = self.next_call.checked_add(1).ok_or("Too many calls")?;
        let id = self.next_call;
        let (host, aliases) = (&self.host, &self.aliases);
        keep_and_announce(
            &mut self.parked,
            &mut self.receiving,
            id,
            crate::parked::Owner {
                peer: peer.clone(),
                plugin: admitted.plugin.clone(),
            },
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
        )
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
                if let Some((_owner, waiting)) =
                    self.outbound
                        .claim_for(claim, &plugin, std::time::Instant::now())
                {
                    // The task that waits on the other end carries it from here. If it is
                    // already gone (withdrawn, or too late) the connection is simply dropped.
                    let _ = waiting.send(stream);
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
        let handle = self.transfers.spawn(async move {
            let Stream { send, receive } = parked;
            let outcome = crate::receive::deliver(
                receive,
                stream,
                crate::receive::DEFAULT_LIMIT,
                peersus_plugin::pump::IDLE_LIMIT,
            )
            .await;
            // The peer's sending half is finished either way, so its side ends cleanly
            // instead of waiting on a transfer that is over.
            let mut send = send;
            let _ = send.finish();
            (Direction::Inbound, call, outcome)
        });
        // Recorded under the device and the plugin, or a permission withdrawn later could
        // not reach a transfer that is already carrying bytes.
        self.carrying.started(call, owner, handle);
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
