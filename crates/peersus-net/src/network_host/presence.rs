//! What this device tells the others and learns of them: pictures, displays, the services it accepts, extensions' states, the
//! link tests, relays and typed addresses, and connecting again to the devices it remembers.
use super::*;

impl NetworkHost {
    /// Give this host a place for the pictures it keeps. Without one it takes no part in profile
    /// pictures at all, which is how it was before they existed.
    pub fn with_pictures(mut self, pictures: Pictures) -> Self {
        // A picture that cannot be read is not the same as having none, and is not treated as it.
        match pictures.own() {
            Ok(own) => self.own = own,
            Err(_) => self.damaged = true,
        }
        self.pictures = Some(pictures);
        self
    }

    /// Give this host the memory of which picture each device showed, kept in a file. What it holds is what this host knows of
    /// every device's picture from the first moment, before any of them is online. A file that cannot be read is an error for the
    /// caller to say: it is not replaced by an empty one.
    pub fn with_picture_owners(mut self, owners: PictureOwners) -> Self {
        self.held = owners.all().clone();
        self.picture_owners = Some(owners);
        self
    }

    /// The access token the transport was already bound with, when there is one, so that the host knows what is in use and a later change
    /// of token is told apart from no change. It binds nothing: the endpoint given to [`NetworkHost::new`] is the one in use.
    #[must_use]
    pub fn with_relay_access(mut self, access: Option<crate::transport::RelayAccess>) -> Self {
        self.relay_access = access;
        self
    }

    /// Whether this device's own picture is on disk but cannot be read. It is then not announced to
    /// anyone, so a fault in a file here never changes what other devices show for this one.
    pub fn picture_problem(&self) -> bool {
        self.damaged
    }

    /// Make `picture` this device's own (or give it up, with `None`), keep it, and tell every device
    /// that is online. What each of them already holds is not sent again: they are told the digest.
    pub fn set_picture(&mut self, picture: Option<Picture>) -> io::Result<()> {
        let Some(pictures) = &self.pictures else {
            return Err(io::Error::other("No place for pictures"));
        };
        match &picture {
            Some(picture) => pictures.set_own(picture)?,
            None => pictures.clear_own()?,
        }
        self.own = picture;
        // A picture that was just written replaces whatever was damaged.
        self.damaged = false;
        let online: Vec<PeerId> = self.sessions.keys().cloned().collect();
        for peer in online {
            self.announce(peer);
        }
        Ok(())
    }

    /// Tell iroh that the network may have changed, so it redoes its sockets, its relay connection and the paths to peers. At most
    /// once in [`NUDGE_EVERY`]: it is cheap, but it is not free, and nothing more is learnt by doing it again at once.
    pub(super) fn nudge_network(&mut self) {
        let now = tokio::time::Instant::now();
        if self
            .last_nudge
            .is_some_and(|last| now.duration_since(last) < NUDGE_EVERY)
        {
            return;
        }
        self.last_nudge = Some(now);
        tracing::debug!("Telling the transport the network may have changed");
        let endpoint = self.endpoint.clone();
        self.work.spawn(async move {
            endpoint.network_change().await;
            Work::Nudged
        });
    }

    /// A relay that stays lost is a network that changed unnoticed (Android reports no changes to iroh): after [`RELAY_LOST_FOR`]
    /// without one, the transport is told to look again.
    pub(super) fn watch_relays(&mut self, connected: bool) {
        if connected {
            self.relay_lost_since = None;
            return;
        }
        let now = tokio::time::Instant::now();
        let since = *self.relay_lost_since.get_or_insert(now);
        if now.duration_since(since) >= RELAY_LOST_FOR {
            self.nudge_network();
        }
    }

    /// This device sleeps (its screen went off, or it has gone idle) or is awake again. Every device that is connected is told, and
    /// so is each one that connects later while this holds. An unchanged state is not told again: nothing would differ.
    pub fn set_asleep(&mut self, asleep: bool) {
        if self.own_asleep == asleep {
            return;
        }
        self.own_asleep = asleep;
        if !asleep {
            // Android tells iroh nothing about networks (its route monitor is empty there), so sockets and relay connections that
            // died while the device slept are never redone: it stays reachable by nobody. Waking is a network change.
            self.nudge_network();
        }
        let online: Vec<PeerId> = self.sessions.keys().cloned().collect();
        for peer in online {
            self.tell_presence(&peer);
        }
    }

    /// Tell one connected device whether this one sleeps, in the background. One small request, never answered. A device that does
    /// not know it (an older one) drops it and keeps the connection, so telling is always safe.
    pub(super) fn tell_presence(&mut self, peer: &PeerId) {
        if !self.shown_to(peer) {
            return;
        }
        let request = Request::Presence {
            asleep: self.own_asleep,
            statuses: self.own_statuses.clone(),
            name: Some(self.name.clone()),
        };
        self.say(peer, request);
    }

    /// Put `request` on the way to `peer`, after everything this device said to it before on this connection (see
    /// `Session::say`). Nothing for a device that is not connected: it is told everything when it connects.
    fn say(&self, peer: &PeerId, request: Request) {
        if let Some(session) = self.sessions.get(peer) {
            // The one that reads ends with the connection; what is said to a connection that is gone is not missed.
            let _ = session.say.send(request);
        }
    }

    /// The displays this device's desktop has now, replacing what was said before. Every connected device is told, and so is each
    /// one that connects later. The same set again says nothing; a set that cannot be believed is refused.
    pub fn set_displays(&mut self, displays: Vec<peersus_core::layout::Display>) -> bool {
        if !peersus_core::layout::valid_displays(&displays) || self.own_displays == displays {
            return false;
        }
        self.own_displays = displays;
        let online: Vec<PeerId> = self.sessions.keys().cloned().collect();
        for peer in online {
            self.tell_displays(&peer);
        }
        true
    }

    /// The displays of this device's desktop, as last set.
    pub fn own_displays(&self) -> &[peersus_core::layout::Display] {
        &self.own_displays
    }

    /// The displays each connected device said its desktop has. A device that said nothing, or is not connected, is not listed.
    pub fn peer_displays(&self) -> BTreeMap<PeerId, Vec<peersus_core::layout::Display>> {
        self.sessions
            .iter()
            .filter(|(_, session)| {
                session.said
                    && session.connection.close_reason().is_none()
                    && !session.displays.is_empty()
            })
            .map(|(peer, session)| (peer.clone(), session.displays.clone()))
            .collect()
    }

    /// Tell one connected device the displays of this one, in the background. Nothing is told while there are none.
    pub(super) fn tell_displays(&mut self, peer: &PeerId) {
        if self.own_displays.is_empty() || !self.shown_to(peer) {
            return;
        }
        let request = Request::Displays {
            displays: self.own_displays.clone(),
        };
        self.say(peer, request);
    }

    /// The extension services this device takes from each remembered device now, replacing the whole of what was set before: the
    /// application gives the full map, a device with nothing taken included (as an empty set). Each connected device whose own set
    /// changed is told it, an emptied one too, and only it; a device told nothing new hears nothing. A set is cut to what the wire takes.
    pub fn set_accepts(&mut self, accepts: BTreeMap<PeerId, Vec<streams::ServiceRef>>) {
        let next: BTreeMap<PeerId, Vec<streams::ServiceRef>> = accepts
            .into_iter()
            .map(|(peer, mut services)| {
                services.sort();
                services.dedup();
                services.truncate(streams::MAX_ACCEPTED);
                (peer, services)
            })
            .collect();
        let changed: Vec<PeerId> = next
            .iter()
            .filter(|(peer, services)| self.own_accepts.get(*peer) != Some(*services))
            .map(|(peer, _)| peer.clone())
            .collect();
        self.own_accepts = next;
        for peer in changed {
            self.tell_accepts(&peer);
        }
    }

    /// What each connected device said it takes from this one. A device that said nothing (an older one), or is not connected, is not
    /// listed: not known.
    pub fn peer_accepts(&self) -> BTreeMap<PeerId, Vec<streams::ServiceRef>> {
        self.sessions
            .iter()
            .filter(|(_, session)| session.said && session.connection.close_reason().is_none())
            .filter_map(|(peer, session)| Some((peer.clone(), session.accepts.clone()?)))
            .collect()
    }

    /// Tell one connected device the services this one takes from it, in the background. Nothing is told before the application
    /// set it: a device is never told "none" for a set that was not worked out yet.
    pub(super) fn tell_accepts(&mut self, peer: &PeerId) {
        let Some(services) = self.own_accepts.get(peer).cloned() else {
            return;
        };
        if !self.shown_to(peer) {
            return;
        }
        self.say(peer, Request::Accepts { services });
    }

    /// The statuses of plugins this device says it has, replacing what it said before: the set is always the whole of it. Every device
    /// that is connected is told, and so is each one that connects later. The same set again says nothing. More than the limit, or one
    /// status twice, is refused and changes nothing.
    pub fn set_statuses(&mut self, statuses: Vec<StatusInfo>) -> Result<(), PluginStateError> {
        let mut seen = std::collections::BTreeSet::new();
        if statuses.len() > MAX_ACTIVE_STATUSES
            || !statuses
                .iter()
                .all(|status| seen.insert(status.key.clone()))
        {
            return Err(PluginStateError::TooManyStatuses);
        }
        if self.own_statuses == statuses {
            return Ok(());
        }
        self.own_statuses = statuses;
        let online: Vec<PeerId> = self.sessions.keys().cloned().collect();
        for peer in online {
            self.tell_presence(&peer);
        }
        Ok(())
    }

    /// The statuses of plugins this device says it has.
    pub fn own_statuses(&self) -> &[StatusInfo] {
        &self.own_statuses
    }

    /// Whether this device keeps to itself (see [`Self::set_private`]).
    pub fn is_private(&self) -> bool {
        self.private
    }

    /// Whether `peer` is told how this device is and may start something here: always, unless this device keeps to itself
    /// and was not opened to it.
    pub fn shown_to(&self, peer: &PeerId) -> bool {
        !self.private || self.opened.contains(peer)
    }

    /// This device keeps to itself, or no longer does. Keeping to itself, it stays connected and hears what the others say,
    /// but tells them nothing of itself and takes no file and no extension stream from them: every connected device is told
    /// that it says nothing more, and shows it as not there. Giving that up, every connected device is told how it is again.
    /// The same choice again does nothing.
    pub fn set_private(&mut self, private: bool) {
        if self.private == private {
            return;
        }
        self.private = private;
        self.opened.clear();
        let connected: Vec<PeerId> = self.sessions.keys().cloned().collect();
        for peer in connected {
            if private {
                self.withdraw(&peer);
            } else {
                self.tell_all(&peer);
            }
        }
    }

    /// While this device keeps to itself, show it to `peer` alone: that device is told how this one is, and may start things
    /// here, until [`Self::close_to`]. Whether anything changed: nothing does for a device already open, or while not private.
    pub fn open_to(&mut self, peer: &PeerId) -> bool {
        if !self.private || !self.opened.insert(peer.clone()) {
            return false;
        }
        self.tell_all(peer);
        true
    }

    /// Take back [`Self::open_to`]: `peer` is told this device says nothing more. Whether anything changed.
    pub fn close_to(&mut self, peer: &PeerId) -> bool {
        if !self.private || !self.opened.remove(peer) {
            return false;
        }
        self.withdraw(peer);
        true
    }

    /// Everything a device is told of this one when it connects, said again.
    pub(super) fn tell_all(&mut self, peer: &PeerId) {
        self.announce(peer.clone());
        self.tell_presence(peer);
        self.tell_displays(peer);
        self.tell_accepts(peer);
    }

    /// Tell one connected device that this one says nothing of itself from now on, in the background.
    fn withdraw(&mut self, peer: &PeerId) {
        self.say(peer, Request::Withdrawn {});
    }

    /// This device's own picture, as it holds it.
    pub fn own_picture(&self) -> Option<&Picture> {
        self.own.as_ref()
    }

    /// The picture a paired device last showed, if it showed one and it is still kept.
    pub fn peer_picture(&self, peer: &PeerId) -> Option<Picture> {
        let digest = self.held.get(peer)?;
        self.pictures.as_ref()?.get(digest).ok().flatten()
    }

    /// The digest of the picture each paired device last showed, only for those that showed one and
    /// whose picture is still kept: a digest is never reported for a picture that cannot be produced.
    pub fn peer_digests(&self) -> BTreeMap<PeerId, Digest> {
        let Some(pictures) = self.pictures.as_ref() else {
            return BTreeMap::new();
        };
        self.held
            .iter()
            .filter(|(_, digest)| pictures.has(digest))
            .map(|(peer, digest)| (peer.clone(), *digest))
            .collect()
    }

    /// How many times this host has sent its picture, as opposed to announcing it. Measured by the
    /// tests that show a picture crosses only when the other side lacked it.
    pub fn pictures_sent(&self) -> usize {
        self.sent
    }

    /// How many announcements this host has finished, whatever they came to.
    pub fn exchanges_finished(&self) -> usize {
        self.finished
    }

    /// Tell one online device which picture this device holds, in the background.
    pub(super) fn announce(&mut self, peer: PeerId) {
        // A device whose own picture is damaged on disk says nothing about it: announcing "no picture"
        // would tell every other device to forget the picture it really has, over a fault in a file here.
        if self.pictures.is_none() || self.damaged {
            return;
        }
        if !self.shown_to(&peer) {
            return;
        }
        let Some(session) = self.sessions.get(&peer) else {
            return;
        };
        let connection = session.connection.clone();
        let own = self.own.clone();
        self.work.spawn(async move {
            let result = async {
                let (mut send, mut receive) = connection.open_bi().await.map_err(|_| ())?;
                streams::send(&mut send, &Request::Profile {})
                    .await
                    .map_err(|_| ())?;
                profile_exchange::announce(&mut send, &mut receive, own.as_ref())
                    .await
                    .map_err(|_| ())
            }
            .await;
            Work::Announced(result)
        });
    }

    /// Test the connection to a remembered device now, in the background, and say what was found in [`Self::link_tests`] when it is known. A
    /// device that is connected already is found paired at once, without opening another connection beside the one it is on. Only a device
    /// that is remembered can be tested, and a test that is going on is not started again.
    pub fn test_link(&mut self, peer: &PeerId) -> Result<(), &'static str> {
        let remembered = self.peers.get(peer).ok_or("Device is not paired")?.clone();
        if self.link_tests.get(peer) == Some(&LinkTest::Trying) {
            return Ok(());
        }
        if self
            .sessions
            .get(peer)
            .is_some_and(|session| session.connection.close_reason().is_none())
        {
            self.link_tests.insert(peer.clone(), LinkTest::Paired);
            return Ok(());
        }
        self.link_tests.insert(peer.clone(), LinkTest::Trying);
        let endpoint = self.endpoint.clone();
        let peer = peer.clone();
        self.work.spawn(async move {
            Work::Probed(peer, reconnect::probe(&endpoint, &remembered).await)
        });
        Ok(())
    }

    /// Forget what the last test of a device found, so that it is not shown again. A test that is still going on is let finish and is
    /// not forgotten: its answer has not come.
    pub fn clear_link_test(&mut self, peer: &PeerId) {
        if self.link_tests.get(peer) != Some(&LinkTest::Trying) {
            self.link_tests.remove(peer);
        }
    }

    /// What the last test of each device found, only for devices that were tested.
    pub fn link_tests(&self) -> BTreeMap<PeerId, LinkTest> {
        self.link_tests.clone()
    }

    /// Test how quickly each relay of `policy` answers; the answers arrive in [`Self::relay_probes`]. A relay already being tested is not
    /// tested twice at once.
    pub fn probe_relays(&mut self, policy: &peersus_core::relay::RelayPolicy) {
        use peersus_core::model::RelayProbe;
        for target in &policy.targets {
            let key = target.probe_key();
            if self.relay_probes.get(&key) == Some(&RelayProbe::Trying) {
                continue;
            }
            self.relay_probes.insert(key.clone(), RelayProbe::Trying);
            let target = target.clone();
            self.work.spawn(async move {
                Work::RelayProbed(key, crate::relay_probe::probe(&target).await)
            });
        }
    }

    /// What the last test of each relay found.
    pub fn relay_probes(&self) -> BTreeMap<String, peersus_core::model::RelayProbe> {
        self.relay_probes.clone()
    }

    /// Say where a remembered device can be found, as typed, or clear it with `None`. Read here, at the boundary: what is not a place is
    /// refused with a sentence that says why. A device is only ever found at an address with a PORT, and the port of a device changes each time
    /// it starts, so an address typed without one is taken only when the device has been seen at a port (that is the one used); with none seen
    /// the person is asked to write it, instead of saving something that could never be tried. A test of the connection from before is
    /// cleared: it was about the old place.
    pub fn set_address(&mut self, peer: &PeerId, text: Option<&str>) -> Result<(), &'static str> {
        use peersus_core::address::{AddressError, TypedAddress};
        let remembered = self.peers.get(peer).ok_or("Device is not paired")?;
        let typed = match text.map(str::trim).filter(|text| !text.is_empty()) {
            None => None,
            Some(text) => {
                let read = TypedAddress::parse(text).map_err(|error| match error {
                    AddressError::Empty => "Write an address",
                    AddressError::NotAnAddress => "That is not an IP address or a name",
                    AddressError::Port => "The port must be a number from 1 to 65535",
                    AddressError::NotRoutable => {
                        "That address cannot be reached from another device"
                    }
                })?;
                if read.port.is_none() && remembered.address.ip_addrs().next().is_none() {
                    return Err(
                        "This device has not been seen at any port yet: write the port as well, like 192.168.1.20:5000",
                    );
                }
                Some(read)
            }
        };
        self.peers
            .set_typed(peer, typed)
            .map_err(|_| "Could not save the address")?;
        self.link_tests.remove(peer);
        Ok(())
    }

    /// Where the person said a device can be found, as they would type it again, if they said.
    pub fn typed_address(&self, peer: &PeerId) -> Option<String> {
        self.peers
            .get(peer)
            .and_then(|remembered| remembered.typed.as_ref())
            .map(|typed| typed.shown())
    }

    /// Every address a person typed, by device, as they would type it again: only for devices that were given one.
    pub fn typed_addresses(&self) -> BTreeMap<PeerId, String> {
        self.peers
            .entries()
            .filter_map(|remembered| {
                remembered
                    .typed
                    .as_ref()
                    .map(|typed| (remembered.peer.id.clone(), typed.shown()))
            })
            .collect()
    }

    /// The port this device listens on, the one another device writes after its address to find this one. It is chosen by the system when
    /// the device starts and is a different one each time, so it is read from the endpoint and not kept. `None` when it is not bound.
    pub fn listening_port(&self) -> Option<u16> {
        self.endpoint
            .bound_sockets()
            .iter()
            .find(|socket| socket.is_ipv4())
            .map(|socket| socket.port())
    }

    /// Home-relay health, not a claim about any individual peer's selected traffic path.
    pub fn relay_diagnostics(&self) -> peersus_core::relay::RelayDiagnostics {
        self.relay_policy
            .as_ref()
            .map(|policy| policy.diagnostics(&self.endpoint))
            .unwrap_or_default()
    }

    pub(super) fn reconnect(&mut self) {
        let entries: Vec<_> = self.peers.entries().collect();
        if entries.is_empty() {
            return;
        }
        let count = entries.len();
        for offset in 0..count {
            if self.connecting.len() >= 8 {
                break;
            }
            let index = (self.reconnect_cursor + offset) % count;
            let remembered = entries[index];
            let peer = remembered.peer.id.clone();
            if self.sessions.contains_key(&peer) || self.connecting.contains(&peer) {
                continue;
            }
            self.connecting.insert(peer.clone());
            let remembered = remembered.clone();
            let endpoint = self.endpoint.clone();
            self.work.spawn(async move {
                Work::Resumed(
                    peer,
                    reconnect::connect_remembered(&endpoint, &remembered)
                        .await
                        .map_err(|_| ()),
                    true,
                )
            });
        }
        self.reconnect_cursor = (self.reconnect_cursor + 8) % count;
    }
}

/// Say to one device, one after the other and in the order they were decided, what this device says of itself over one
/// connection: how it is, its displays, what it takes, and that it says nothing more. Each is tried a few times while the
/// connection lives: whether a device is shown as there, and with what, hangs on these arriving, and on the last one decided
/// being the last one to arrive. Ends when the session that feeds it is gone.
pub(super) async fn speak(
    connection: Connection,
    mut said: tokio::sync::mpsc::UnboundedReceiver<Request>,
) {
    const TRIES: usize = 3;
    while let Some(request) = said.recv().await {
        for attempt in 0..TRIES {
            if connection.close_reason().is_some() {
                return;
            }
            if let Ok((mut send, _receive)) = connection.open_bi().await
                && streams::send(&mut send, &request).await.is_ok()
            {
                let _ = send.finish();
                break;
            }
            if attempt + 1 < TRIES {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}
