//! Toolkit-independent network owner. No network future runs on a UI thread.
use crate::{
    file_session::Incoming,
    folder_sender::FolderSender,
    handshake::{self, Admitted},
    history::History,
    invitation::Invitation,
    invitations::{Invitations, Purpose},
    pairing_tasks::{self, PairingTasks},
    peers::Peers,
    picture_owners::PictureOwners,
    pictures::Pictures,
    profile_exchange::{self, Outcome},
    reconnect,
    streams::{self, Request},
    transfer::Source,
    transfer_tasks::TransferTasks,
    transport,
};
use iroh::{
    Endpoint,
    endpoint::{Connection, RecvStream, SendStream},
};
use peersus_core::{
    model::{FilesCode, LinkTest, PairingPrompt, Peer, PeerId, Transfer},
    pairing::Role,
    plugins::{MAX_ACTIVE_STATUSES, PluginStateError, StatusInfo},
    profile::{Digest, Picture},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::task::JoinSet;

mod files;
mod presence;

/// One folder being sent: its id, the device it goes to, and the walk that hands out its files.
struct FolderRun {
    id: u64,
    peer: PeerId,
    sender: FolderSender,
}

pub enum Event {
    Changed,
    Error(&'static str),
    Plugin {
        peer: PeerId,
        service: String,
        version: u32,
        send: SendStream,
        receive: RecvStream,
    },
    /// A paired device's profile picture changed, or it has none now: read it with `peer_picture`.
    Picture {
        peer: PeerId,
    },
}

/// How long after a session was made a second connection of the same two devices still counts as made at the same moment.
const SAME_MOMENT: Duration = Duration::from_secs(10);

/// Whether a connection from a device that already has a session here takes that session's place.
///
/// Two devices that find each other at the same moment each dial the other, and both must keep the same one of the two
/// connections: the one the device with the lower id dialed (`preferred`). So a connection that is not the preferred one,
/// arriving moments after the preferred one was installed, is that duplicate, and is the one to close on both sides.
///
/// Anything else is the device connecting again. It dials only when it has no connection of its own, so the session held
/// here is one it no longer has, whatever this side still believes of it: it slept through the end of it, or its application
/// started again. Refusing the new connection for as long as the old one has not been noticed dead left the device shown
/// as it last was (asleep, or online while it saw this one offline) until the old connection ran out.
fn takes_the_place(alive: bool, held_preferred: bool, held_for: Duration, preferred: bool) -> bool {
    !(alive && held_preferred && !preferred && held_for < SAME_MOMENT)
}

struct Session {
    connection: Connection,
    generation: u64,
    preferred: bool,
    /// When this session was installed.
    since: tokio::time::Instant,
    /// What the device said of itself last: that it sleeps. Starts awake and is only ever set by that device over this very
    /// connection, so a new connection (it woke, or came back) is awake until it says otherwise.
    asleep: bool,
    /// The statuses of plugins that the device said it has, the whole set it last said over this connection. Empty until it says
    /// otherwise, and gone with the connection.
    statuses: Vec<StatusInfo>,
    /// The displays the device last said its desktop has, over this connection. Empty until it says so.
    displays: Vec<peersus_core::layout::Display>,
    /// The extension services the device last said it takes from THIS device, over this connection. `None` until it says so (an
    /// older device never does): not known, which is not the same as none.
    accepts: Option<Vec<streams::ServiceRef>>,
    /// Whether the device has said how it is over this connection and has not taken it back. Until it does it is not shown as
    /// there: a device whose person keeps it private holds the connection and says nothing, and is shown as offline.
    said: bool,
    /// Where this device puts what it says of itself to that one (how it is, its displays, what it takes, that it says
    /// nothing more), read by one task that writes them one after the other: in the order they were decided, so the last
    /// one decided is the last one to arrive.
    say: tokio::sync::mpsc::UnboundedSender<Request>,
}
type Routed = Result<(SendStream, RecvStream, Request), ()>;

/// A files-only connection retains its time bound and the authenticated host's
/// selection allowance. Legacy codes allow sixteen files; larger selections
/// carry their explicit count, independently of the bounded active window.
///
/// Pure, so the rule is tried with plain values and not through a clock and a socket.
#[derive(Clone, Copy, Debug)]
struct FileLink {
    taken: usize,
    limit: usize,
    deadline: tokio::time::Instant,
}

impl FileLink {
    const MOST_OFFERS: usize = 16;
    const LIFE: Duration = Duration::from_secs(300);

    fn new(now: tokio::time::Instant) -> Self {
        Self {
            taken: 0,
            limit: Self::MOST_OFFERS,
            deadline: now + Self::LIFE,
        }
    }

    fn for_selection(now: tokio::time::Instant, files: u32) -> Self {
        Self {
            limit: files as usize,
            ..Self::new(now)
        }
    }

    /// The allowance after one more offer is taken at `now`, or `None` when it may not be: the time is up, or that many have
    /// been taken already.
    fn after_one_more(self, now: tokio::time::Instant) -> Option<Self> {
        (now < self.deadline && self.taken < self.limit).then_some(Self {
            taken: self.taken + 1,
            ..self
        })
    }
}

/// The files an admission may offer, taken out of what is parked, and only if they were parked for the files code that
/// admission was made with. A code shown after it is another generation, and what was kept for that one is not for a device
/// that scanned this one: it stays parked for the code it belongs to and this admission is given nothing. What is taken is
/// gone from `parked`, so a code is used once.
///
/// Pure, and generic over what the files are, so that the rule is tried with plain values: the race it closes (an owner
/// asking for a new code while an admission is being handled) cannot be made to happen on demand with real connections.
fn plan_for<T>(parked: &mut Option<(u64, Vec<T>)>, generation: u64) -> Vec<T> {
    match parked.take() {
        Some((parked_for, sources)) if parked_for == generation => sources,
        other => {
            *parked = other;
            Vec::new()
        }
    }
}

enum FileOrigin {
    Paired(u64),
    Files { name: String, link: FileLink },
}

/// At most one parsed file waits per connection: its listener is not rearmed
/// until an active transfer slot is available. QUIC bounds the remaining streams.
struct WaitingFile {
    peer: PeerId,
    incoming: Incoming,
    connection: Connection,
    origin: FileOrigin,
}

/// A generation-fenced producer retains its admitted connection across empty batches.
struct FileSelection {
    generation: u64,
    remaining: usize,
    until: tokio::time::Instant,
    admitted: Option<(Connection, String)>,
    pending: VecDeque<Source>,
    finished: bool,
}

enum Work {
    Transport(Result<Connection, ()>),
    /// The transport was told the network may have changed; nothing to do with the answer.
    Nudged,
    /// A connection the gates admitted, and for a files code the generation of the code it was admitted with (0 when this
    /// host joined someone else's code, which has no plan here).
    Admitted(Result<(Admitted, u64), &'static str>, Role),
    /// The answer of a device nearby that was asked to take files, with the files that were to go to it. They travel
    /// with the asking, so that files meant for one device are never on hand when another answers.
    /// Each file with the identifier its transfer was promised under when the files were taken.
    NearbyAdmitted(Result<Admitted, &'static str>, Vec<(String, Source)>),
    /// The next stream on a connection that carries files only, the name the other device gave in its handshake, and how much
    /// of the connection's allowance is left.
    FileStream(Connection, String, FileLink, Routed),
    Resumed(PeerId, Result<Connection, ()>, bool),
    Stream(PeerId, u64, Routed),
    /// A profile exchange this host started, and whether the picture itself was SENT.
    Announced(Result<bool, ()>),
    /// A profile exchange another device started with this host, finished; `None` when the connection went away mid-way.
    Picture(PeerId, Result<Option<Outcome>, ()>),
    /// This host told a device that it sleeps or is awake again, and nothing came back: there is nothing to wait for or to count.
    Told,
    /// A connection settled on a direct path: where the device is, for the session of that generation. `None` when the connection ended, or
    /// it never left the relay, before it did.
    Located(PeerId, u64, Option<std::net::SocketAddr>),
    /// A test of the connection to a device ended, and what it found.
    Probed(PeerId, LinkTest),
    /// A test of a relay ended: its key and how long it took to answer, if it did.
    RelayProbed(String, Option<u32>),
}

/// How long a connection is given to move from a relay to a direct path before it is taken as staying on the relay: finding a way through
/// takes seconds, and a minute is far past it. And how often it is looked at meanwhile.
const LOCATE_FOR: Duration = Duration::from_secs(60);
const LOCATE_EVERY: Duration = Duration::from_millis(500);

/// The direct (IP) address that a connection is using, from its paths given as `(is it the selected one, where it goes)`; `None` when the
/// selected path is a relay or there is none yet. Only the SELECTED path says where the device is reached now: another one that is open
/// may be a route that is about to be dropped. Pure, so it is tried with plain values.
fn selected_direct(
    paths: impl IntoIterator<Item = (bool, iroh::TransportAddr)>,
) -> Option<std::net::SocketAddr> {
    paths
        .into_iter()
        .find_map(|(selected, address)| match address {
            iroh::TransportAddr::Ip(address) if selected => Some(address),
            _ => None,
        })
}

pub struct NetworkHost {
    endpoint: Endpoint,
    name: String,
    invitations: Arc<Invitations>,
    pairing: PairingTasks,
    transfers: TransferTasks,
    peers: Peers,
    history: History,
    sessions: BTreeMap<PeerId, Session>,
    connecting: BTreeSet<PeerId>,
    work: JoinSet<Work>,
    waiting_files: VecDeque<WaitingFile>,
    admissions: usize,
    generation: u64,
    reconnect_tick: tokio::time::Interval,
    reconnect_cursor: usize,
    /// Where the pictures this device keeps are, once it has been given a place: without one, no
    /// profile picture is announced, asked for or kept, and a host behaves exactly as it always did.
    pictures: Option<Pictures>,
    /// This device's own profile picture, which it tells every device that is online.
    own: Option<Picture>,
    /// The digest of the picture each paired device last showed, so a device's picture can be found
    /// again without asking it, and forgotten when it says it has none.
    held: BTreeMap<PeerId, Digest>,
    /// The file that keeps `held` between starts, once the host has been given one.
    picture_owners: Option<PictureOwners>,
    /// How many times this host has SENT its picture (not just announced it), for what a test
    /// measures: that a picture crosses only when the other side lacked it.
    sent: usize,
    /// How many announcements this host has finished, whatever they came to.
    finished: usize,
    /// Whether the own picture is on disk and unreadable, which is not the same as having none.
    damaged: bool,
    /// The files that go out when the device shown the files code is admitted, parked with that code.
    parked: Option<(u64, Vec<Source>)>,
    selection: Option<FileSelection>,
    /// The folders being sent, each walked as its files go. Not a list of files: each holds a walk, which is a stack of folders.
    folders: Vec<FolderRun>,
    /// How many folders this host has begun, to give each an id.
    folder_serial: u64,
    /// Whether THIS device sleeps (its screen is off, or it has gone idle), which it tells every device that is connected, and
    /// each one that connects later. Awake until a platform says otherwise.
    own_asleep: bool,
    /// The statuses of plugins this device says it has, told to every device that is connected and to each one that connects later.
    /// None until a plugin sets one.
    own_statuses: Vec<StatusInfo>,
    /// The displays of this device's desktop, told to every device that connects. Empty where there is no desktop to place.
    own_displays: Vec<peersus_core::layout::Display>,
    /// The extension services this device takes from each remembered device, told to each one alone while it is connected and again
    /// whenever its set changes. Set by the application, which knows the extensions and what its person allowed each device.
    own_accepts: BTreeMap<PeerId, Vec<streams::ServiceRef>>,
    /// What the last test of the connection to each remembered device found. Only a device that was tested has an entry, and it is gone
    /// when the window clears it or the device is forgotten.
    link_tests: BTreeMap<PeerId, LinkTest>,
    /// What the last test of each relay found, by [`RelayTarget::probe_key`](peersus_core::relay::RelayTarget::probe_key).
    relay_probes: BTreeMap<String, peersus_core::model::RelayProbe>,
    /// The access token of the personal relay, when there is one. A secret: it is not part of any preference, snapshot or
    /// command, and `Debug` never shows it. Only [`NetworkHost::set_relay_access`] changes it, after a transport that uses it was bound.
    relay_access: Option<crate::transport::RelayAccess>,
    relay_policy: Option<crate::relay_policy::Coordinator>,
    relay_tick: tokio::time::Interval,
    /// Since when no relay of the tier in use has been connected; none while one is.
    relay_lost_since: Option<tokio::time::Instant>,
    /// When the transport was last told the network may have changed.
    last_nudge: Option<tokio::time::Instant>,
    /// Whether THIS device keeps to itself: it tells a paired device nothing of itself, and takes no file and no extension
    /// stream from it, unless it is in `opened`. Off until the application says otherwise.
    private: bool,
    /// The devices this one is shown to while it keeps to itself: those its person started something for, until the
    /// application closes them again. Empty and unused while `private` is off.
    opened: BTreeSet<PeerId>,
}

/// How long a relay may stay lost before the transport is told to look at the network again.
const RELAY_LOST_FOR: Duration = Duration::from_secs(15);
/// The least time between two such tellings.
const NUDGE_EVERY: Duration = Duration::from_secs(30);

impl NetworkHost {
    pub fn new(endpoint: Endpoint, name: String, peers: Peers, history: History) -> Self {
        let pairing = PairingTasks::new(endpoint.id());
        let mut reconnect_tick = tokio::time::interval(Duration::from_secs(5));
        reconnect_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Self {
            endpoint,
            name,
            invitations: Arc::new(Invitations::default()),
            pairing,
            transfers: TransferTasks::default(),
            peers,
            history,
            sessions: BTreeMap::new(),
            connecting: BTreeSet::new(),
            work: JoinSet::new(),
            waiting_files: VecDeque::new(),
            admissions: 0,
            generation: 0,
            reconnect_tick,
            reconnect_cursor: 0,
            pictures: None,
            own: None,
            held: BTreeMap::new(),
            picture_owners: None,
            sent: 0,
            finished: 0,
            damaged: false,
            parked: None,
            selection: None,
            folders: Vec::new(),
            folder_serial: 0,
            own_asleep: false,
            own_statuses: Vec::new(),
            own_displays: Vec::new(),
            own_accepts: BTreeMap::new(),
            link_tests: BTreeMap::new(),
            relay_probes: BTreeMap::new(),
            relay_access: None,
            relay_policy: None,
            relay_tick: tokio::time::interval(Duration::from_secs(1)),
            relay_lost_since: None,
            last_nudge: None,
            private: false,
            opened: BTreeSet::new(),
        }
    }

    /// Coordinate the ordered home-relay tiers on this host's supervised event loop.
    pub fn with_network_settings(mut self, settings: &peersus_core::NetworkSettings) -> Self {
        self.relay_policy = crate::relay_policy::Coordinator::new(settings);
        self
    }

    pub fn invite(&self, now: u64) -> Result<Invitation, crate::invitation::Error> {
        self.invitations.create(&self.endpoint, now)
    }

    /// A capability to announce to devices nearby, for one thing; see [`Invitations::create_nearby`].
    pub fn invite_nearby(
        &self,
        now: u64,
        announced: crate::invitations::Announced,
    ) -> Result<Invitation, crate::invitation::Error> {
        self.invitations
            .create_nearby(&self.endpoint, now, announced)
    }

    /// Whether what [`Self::invite_nearby`] last made for `announced` could still admit a device.
    pub fn nearby_invitation_live(&self, announced: crate::invitations::Announced) -> bool {
        self.invitations.nearby_live(announced)
    }

    /// Take back what [`Self::invite_nearby`] made.
    pub fn withdraw_nearby_invitation(&self) {
        self.invitations.withdraw_nearby();
    }

    pub fn join(&mut self, invitation: Invitation) -> io::Result<()> {
        if self.admissions >= 8 {
            return Err(io::Error::other("Too many pending connections"));
        }
        let endpoint = self.endpoint.clone();
        let name = self.name.clone();
        self.admissions += 1;
        self.work.spawn(async move {
            Work::Admitted(
                handshake::join(&endpoint, invitation, name)
                    .await
                    .map(|admitted| (admitted, 0))
                    .map_err(Self::why),
                Role::Initiator,
            )
        });
        Ok(())
    }

    /// Offer `sources` to the device nearby that announced `invitation`, without pairing: it is asked to take files
    /// (see [`Invitation::for_files`]) and, when it admits this device for that, each file is offered on that
    /// connection and is its person's to accept. Nothing is sent when it answers anything else. No more files than
    /// one such connection carries.
    ///
    /// Answers with the identifier each file's transfer will have, in the order of `sources`: the transfers are made
    /// only once the device admits this one, and none is made when it does not.
    pub fn send_nearby(
        &mut self,
        invitation: &Invitation,
        sources: Vec<Source>,
    ) -> io::Result<Vec<String>> {
        if sources.is_empty() || sources.len() > FileLink::MOST_OFFERS {
            return Err(io::Error::other("Too many files"));
        }
        if self.admissions >= 8 {
            return Err(io::Error::other("Too many pending connections"));
        }
        let endpoint = self.endpoint.clone();
        let name = self.name.clone();
        let invitation = invitation.for_files();
        self.admissions += 1;
        let sources: Vec<(String, Source)> = sources
            .into_iter()
            .map(|source| (TransferTasks::fresh_id(), source))
            .collect();
        let promised = sources.iter().map(|(id, _)| id.clone()).collect();
        self.work.spawn(async move {
            Work::NearbyAdmitted(
                handshake::join(&endpoint, invitation, name)
                    .await
                    .map_err(Self::why),
                sources,
            )
        });
        Ok(promised)
    }

    /// The sentence that says why an invitation did not become a connection. A fixed text for each kind of failure: it never holds the code, an address or
    /// the name of a device, so that it can be written into the diagnostics and shown to the person as it is.
    fn why(error: handshake::Error) -> &'static str {
        match error {
            handshake::Error::Connection => {
                "The other device could not be reached with this invitation"
            }
            handshake::Error::Refused => {
                "The invitation was refused: it is expired, already used, or not the one this device showed"
            }
            handshake::Error::Name => "The other device sent a name that is not valid",
            handshake::Error::Transport(_) => {
                "The connection to the other device failed while it was being set up"
            }
        }
    }

    /// The fixed sentence for the reason a pairing ceremony gave. The reasons come from this crate and are a few fixed texts, none of which holds a
    /// code, an address or a device's name, but a sentence is chosen here and not passed on as it came, so that nothing else can ever be written.
    fn why_pairing(reason: &str) -> &'static str {
        match reason {
            "Pairing task stopped unexpectedly" => {
                "Pairing stopped unexpectedly before the two devices agreed"
            }
            "Could not save paired device" => {
                "The two devices agreed, but the paired device could not be saved"
            }
            _ => "Pairing was refused, expired, or the two devices did not show the same numbers",
        }
    }

    pub fn choose(&mut self, request: &str, number: u8) -> Result<(), &'static str> {
        self.pairing.choose(request, number)
    }
    pub fn reject(&mut self, request: &str) -> bool {
        self.pairing.reject(request)
    }
    pub fn pairing(&self) -> Vec<PairingPrompt> {
        self.pairing.prompts()
    }
    pub fn transfers(&self) -> Vec<Transfer> {
        self.transfers.snapshot(&self.history)
    }
    /// The files code on show, for a screen to draw from: see [`Invitations::files_code`]. Never the code itself.
    pub fn files_code(&self) -> Option<FilesCode> {
        self.invitations
            .files_code()
            .map(|(generation, live)| FilesCode {
                generation,
                live,
                pending: self.selection.as_ref().is_some_and(|selection| {
                    selection.generation == generation
                        && tokio::time::Instant::now() < selection.until
                        && (!selection.finished || !selection.pending.is_empty())
                }),
            })
    }
    pub fn peers(&self) -> Vec<Peer> {
        self.peers
            .entries()
            .map(|remembered| {
                let mut peer = remembered.peer.clone();
                // There only once it has said how it is: a connection alone is not that (see `Session::said`).
                peer.online = self.sessions.get(&peer.id).is_some_and(|session| {
                    session.said && session.connection.close_reason().is_none()
                });
                // Asleep only while connected: a device that said it sleeps and then lost its connection is not asleep, it is gone.
                peer.sleeping = peer.online
                    && self
                        .sessions
                        .get(&peer.id)
                        .is_some_and(|session| session.asleep);
                // What a device says of itself is only what its live connection brought: gone with the connection.
                peer.statuses = if peer.online {
                    self.sessions
                        .get(&peer.id)
                        .map(|session| session.statuses.clone())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                peer
            })
            .collect()
    }

    /// Forget the transfers that are over; those still going are kept. How many were forgotten.
    pub fn clear_history(&mut self) -> io::Result<usize> {
        // ONE list: what is over is forgotten from it, and with it from every page that reads it, a device's own included.
        let forgotten = self.history.clear_finished()?;
        // The circle that showed a code that was scanned and sent from reads the transfers it came of; with those forgotten it
        // would fall back to "scanned", which is not true any more. A code that can admit no one is let go of, so it asks again.
        self.invitations.retire_files_if_spent();
        Ok(forgotten)
    }
    pub fn forget(&mut self, peer: &PeerId) -> io::Result<()> {
        self.peers.forget(peer)?;
        self.link_tests.remove(peer);
        // The device is no longer trusted from here on: its connection is closed BEFORE anything else is done, so that nothing
        // that can fail after this point (the metadata below) can leave a device that was forgotten still connected. The
        // caller withdraws its work in flight when this returns Ok, which it must always do once the device is gone.
        if let Some(session) = self.sessions.remove(peer) {
            session.connection.close(0_u32.into(), b"forgotten");
        }
        self.waiting_files.retain(|waiting| &waiting.peer != peer);
        // What passed with a device that is forgotten goes with it; other devices' records stay. This is metadata, not access, so
        // nothing here fails the forgetting. Its transfers still going are stopped FIRST (recorded as cancelled, their tasks
        // aborted), so that none of them writes a last state after this and brings a record of the device back; then the list
        // forgets everything with it. There is ONE list of what passed (the history that Send, Receive and a device's own page all
        // read), so there is nothing else to clean. If it cannot be written the device stays in it, shown under "met by code" as a
        // device with no permission at all, until it is forgotten again.
        if self.transfers.cancel_peer(peer, &mut self.history).is_ok() {
            let _ = self.history.forget_peer(peer);
        }
        // What the device showed goes with it: the reference is not kept for a device that was forgotten.
        self.held.remove(peer);
        if let Some(owners) = self.picture_owners.as_mut() {
            let _ = owners.forget(peer);
        }
        Ok(())
    }

    /// Trusted host access only; never expose a connection handle through IPC.
    pub fn connection(&self, peer: &PeerId) -> Option<Connection> {
        self.sessions
            .get(peer)
            .filter(|session| session.connection.close_reason().is_none())
            .map(|session| session.connection.clone())
    }

    /// Replace what one plugin may do for one paired device.
    pub fn set_grants(
        &mut self,
        peer: &PeerId,
        plugin: peersus_core::plugins::PluginId,
        grants: peersus_core::plugins::Grants,
    ) -> io::Result<()> {
        self.peers.set_grants(peer, plugin, grants)
    }

    /// Forget what ONE plugin was granted on every paired device. Returns how many grants went.
    pub fn forget_plugin(&mut self, plugin: &peersus_core::plugins::PluginId) -> io::Result<usize> {
        self.peers.forget_plugin(plugin)
    }

    /// Store one per-device plugin value checked against the plugin's own manifest.
    pub fn set_plugin_value(
        &mut self,
        peer: &PeerId,
        plugin: peersus_core::plugins::PluginId,
        declared: &[peersus_api::contributions::Setting],
        setting: &str,
        value: peersus_core::plugins::Value,
    ) -> io::Result<()> {
        self.peers
            .set_plugin_value(peer, plugin, declared, setting, value)
    }

    /// How long an offer waits for an answer, for the offers made from now on: from the preferences, at start and whenever
    /// they are applied. An offer already waiting keeps the wait it was made with.
    pub fn set_offer_wait(&mut self, seconds: u32) {
        self.transfers.set_wait(seconds);
    }

    /// Apply the same persisted preference transaction for CLI and GUI callers.
    /// A replacement transport is bound before publication; failures leave the
    /// current listener and saved preferences intact.
    pub async fn update_settings(
        &mut self,
        store: &mut crate::settings::SettingsStore,
        candidate: peersus_core::Settings,
    ) -> Result<(), &'static str> {
        candidate.validate().map_err(|_| "Invalid preferences")?;
        self.rebind_for(store, candidate, None).await
    }

    /// The access token in use for the personal relay, to put back after a change that could not be kept. Borrowed, so a
    /// caller that only looks makes no copy of a secret.
    pub fn relay_access(&self) -> Option<&crate::transport::RelayAccess> {
        self.relay_access.as_ref()
    }

    /// Use this access token (or none) for the personal relay, binding a replacement transport with it the way a change of
    /// network does, and with the same refusals. The token is kept only here, apart from every preference, so no snapshot,
    /// file or command line can carry it. A token that changes nothing is not a reason to disturb the connections.
    pub async fn set_relay_access(
        &mut self,
        store: &mut crate::settings::SettingsStore,
        access: Option<crate::transport::RelayAccess>,
    ) -> Result<(), &'static str> {
        if access == self.relay_access {
            return Ok(());
        }
        let current = store.current().clone();
        // Only a personal relay uses the token. Without one it is only remembered, for when one is chosen.
        if !current.network.has_custom_relay() {
            self.relay_access = access;
            return Ok(());
        }
        self.rebind_for(store, current, Some(access)).await
    }

    /// The one guarded path to a replacement transport, for a change of network or of the token of a personal relay.
    async fn rebind_for(
        &mut self,
        store: &mut crate::settings::SettingsStore,
        candidate: peersus_core::Settings,
        new_access: Option<Option<crate::transport::RelayAccess>>,
    ) -> Result<(), &'static str> {
        let access_changed = new_access
            .as_ref()
            .is_some_and(|access| *access != self.relay_access);
        // A change of only the relay list (order, switches, addresses, timings) is applied to the running endpoint: binding a
        // new transport for it would close every connection and take seconds.
        // The direct wait is built into the transport, so changing it still takes a new one.
        let relays_only = !access_changed
            && candidate.network.effective_relay_policy().direct_wait_ms
                == store
                    .current()
                    .network
                    .effective_relay_policy()
                    .direct_wait_ms
            && {
                let mut same_otherwise = candidate.network.clone();
                same_otherwise.relay_policy = store.current().network.relay_policy.clone();
                same_otherwise == store.current().network
                    && candidate.network != store.current().network
            };
        let network_changed = candidate.network != store.current().network && !relays_only;
        // The relay list applied in place, kept until the preferences are saved.
        let live = if relays_only {
            Some(
                crate::relay_policy::Coordinator::replace(
                    self.relay_policy.as_ref(),
                    &self.endpoint,
                    &candidate.network,
                    self.relay_access.as_ref(),
                )
                .await
                .map_err(|_| "Could not apply the relay list")?,
            )
        } else {
            None
        };
        let replacement = if network_changed || access_changed {
            if !self.waiting_files.is_empty()
                || self.transfers().iter().any(|transfer| {
                    matches!(
                        transfer.state,
                        peersus_core::model::TransferState::Offered
                            | peersus_core::model::TransferState::Running
                    )
                })
            {
                return Err("Finish or cancel transfers before changing the network");
            }
            let access = new_access.as_ref().unwrap_or(&self.relay_access).as_ref();
            Some(
                transport::bind_with(
                    self.endpoint.secret_key().clone(),
                    &candidate.network,
                    access,
                )
                .await
                .map_err(|_| "Could not bind the selected network")?,
            )
        } else {
            None
        };
        if store.replace(candidate.clone()).is_err() {
            if let Some(endpoint) = replacement {
                endpoint.close().await;
            }
            if let Some(next) = &live {
                // Put the running relays back to what is still saved.
                let _ = crate::relay_policy::Coordinator::replace(
                    next.as_ref(),
                    &self.endpoint,
                    &store.current().network.clone(),
                    self.relay_access.as_ref(),
                )
                .await;
            }
            return Err("Could not save preferences");
        }
        if let Some(next) = live {
            self.relay_policy = next;
        }
        let renamed = self.name != candidate.device_name;
        self.name = candidate.device_name;
        // A device that is renamed is told to every device that is connected, the way a changed picture is: the others do not look for it.
        if renamed {
            let online: Vec<PeerId> = self.sessions.keys().cloned().collect();
            for peer in online {
                self.tell_presence(&peer);
            }
        }
        self.set_offer_wait(candidate.offer_timeout_seconds);
        if let Some(access) = new_access {
            self.relay_access = access;
        }
        if let Some(endpoint) = replacement {
            self.relay_policy = crate::relay_policy::Coordinator::new(&candidate.network);
            self.invitations.revoke();
            // The files parked for a revoked code go with it, so that no old plan can attach to a code shown later.
            self.parked = None;
            self.selection = None;
            self.waiting_files.clear();
            for (_, session) in std::mem::take(&mut self.sessions) {
                session.connection.close(0_u32.into(), b"network changed");
            }
            // Drop completed results too: an old admission must never install a
            // connection after the selected interface has changed.
            self.work = JoinSet::new();
            self.pairing = PairingTasks::new(endpoint.id());
            self.connecting.clear();
            self.admissions = 0;
            let previous = std::mem::replace(&mut self.endpoint, endpoint);
            self.reconnect_tick.reset_immediately();
            previous.close().await;
        }
        Ok(())
    }

    /// Cancellation-safe event pumping; command channels can race this future.
    pub async fn next(&mut self, now: u64) -> Event {
        loop {
            match self.feed_file_selection(now) {
                Ok(true) => return Event::Changed,
                Err(_) => return Event::Error("Could not offer selected files"),
                Ok(false) => {}
            }
            if self.promote_waiting_files(now).await.is_err() {
                return Event::Error("Could not admit an incoming transfer");
            }
            tokio::select! {
                _ = self.relay_tick.tick(), if self.relay_policy.is_some() => {
                    let may_retry = self.admissions == 0
                        && self.work.is_empty()
                        && self.waiting_files.is_empty()
                        && self.sessions.values().all(|session| transport::selected_ip(&session.connection))
                        && !self.transfers().iter().any(|transfer| matches!(
                            transfer.state,
                            peersus_core::model::TransferState::Offered | peersus_core::model::TransferState::Running
                        ));
                    let mut ticked = None;
                    if let Some(policy) = &mut self.relay_policy {
                        let result = policy.tick(&self.endpoint, self.relay_access.as_ref(), may_retry).await;
                        let connected = policy.diagnostics(&self.endpoint).health == peersus_core::relay::RelayHealth::Connected;
                        ticked = Some((result, connected));
                    }
                    if let Some((result, connected)) = ticked {
                        self.watch_relays(connected);
                        match result {
                            Ok(true) => return Event::Changed,
                            Ok(false) => {},
                            Err(_) => return Event::Error("Could not update relay fallback"),
                        }
                    }
                }
                Some(incoming) = self.endpoint.accept() => {
                    if self.admissions >= 8 {
                        // Refused with no word to anyone. If this counter ever stays high (a connection that is counted in and never out), every
                        // new invitation is refused from then on, so each refusal says so in the owner's log, with how many are counted.
                        tracing::warn!(admissions = self.admissions, "Connection admission limit reached");
                        incoming.refuse();
                        continue;
                    }
                    self.admissions += 1;
                    self.work.spawn(async move {
                        let result = tokio::time::timeout(transport::CONTROL_TIMEOUT, incoming).await;
                        Work::Transport(result.ok().and_then(Result::ok).ok_or(()))
                    });
                }
                event = self.pairing.next(&mut self.peers, now) => {
                    match event {
                        pairing_tasks::Event::Paired { connection, .. } => self.install(connection, true),
                        pairing_tasks::Event::Failed { reason, .. } => return Event::Error(Self::why_pairing(&reason)),
                        pairing_tasks::Event::Prompt(_) => {},
                    }
                    return Event::Changed;
                }
                result = self.transfers.next(&mut self.history) => {
                    return if result.is_ok() { Event::Changed } else { Event::Error("Could not update transfer history") };
                }
                Some(result) = self.work.join_next(), if !self.work.is_empty() => {
                    if result.as_ref().is_err_and(|error| error.is_cancelled()) { continue }
                    let Ok(work) = result else {
                        // Task panics must not leave a peer displayed as online.
                        for (_, session) in std::mem::take(&mut self.sessions) { session.connection.close(1_u32.into(), b"host task failed"); }
                        self.work.abort_all();
                        self.connecting.clear();
                        self.admissions = 0;
                        return Event::Error("Network task stopped unexpectedly");
                    };
                    match work {
                        Work::Transport(result) => {
                            self.admissions = self.admissions.saturating_sub(1);
                            let Ok(connection) = result else { continue };
                            if connection.alpn() == transport::ALPN {
                                self.admissions += 1;
                                let gate = Arc::clone(&self.invitations);
                                let name = self.name.clone();
                                self.work.spawn(async move { Work::Admitted(gate.accept(connection, name).await.map(|(admitted, _, generation)| (admitted, generation)).map_err(Self::why), Role::Responder) });
                            } else if connection.alpn() == reconnect::ALPN {
                                let peer = PeerId(connection.remote_id().to_string());
                                let identity = self.peers.get(&peer).map(|entry| entry.address.id);
                                self.admissions += 1;
                                self.work.spawn(async move { Work::Resumed(peer, reconnect::accept_known(connection, identity).await.map_err(|_| ()), false) });
                            } else { connection.close(1_u32.into(), b"unknown protocol"); }
                        }
                        Work::Admitted(result, role) => {
                            self.admissions = self.admissions.saturating_sub(1);
                            let (admitted, generation) = match result {
                                Ok(admitted) => admitted,
                                // The reason is the handshake's own sentence (it names no code, address or device), so it can be shown as it is.
                                Err(reason) => return Event::Error(reason),
                            };
                            match (admitted.purpose, role) {
                                (Purpose::Pairing, role) => {
                                    if self.pairing.start(admitted, role).is_err() { return Event::Error("Pairing unavailable") }
                                }
                                // The one that SHOWED the code: what was parked with it goes out on this connection, and
                                // nothing else. It is a trust for these files only, so the device is not made a peer.
                                (Purpose::Files | Purpose::FileSelection { .. }, Role::Responder) => {
                                    let connection = admitted.connection.clone();
                                    let producing = self.selection.as_mut().filter(|s| s.generation == generation);
                                    let retained = producing.is_some();
                                    if let Some(selection) = producing {
                                        selection.admitted = Some((connection.clone(), admitted.name.clone()));
                                    }
                                    let mut offered = false;
                                    // Only the plan of the code this device was admitted with (see `plan_for`).
                                    let own = plan_for(&mut self.parked, generation);
                                    for source in own {
                                        if self.transfers.send(connection.clone(), Some(admitted.name.clone()), Some(generation), source, &mut self.history, now).is_ok() { offered = true; }
                                    }
                                    if !offered && !retained { connection.close(0_u32.into(), b"nothing to send"); }
                                    return Event::Changed;
                                }
                                // The one that SCANNED it: it receives what is offered, each file by its own consent. The
                                // connection is not a session, so nothing but a file can travel on it.
                                (purpose @ (Purpose::Files | Purpose::FileSelection { .. }), Role::Initiator) => {
                                    let name = admitted.name.clone();
                                    let now = tokio::time::Instant::now();
                                    let link = match purpose {
                                        Purpose::FileSelection { files } => FileLink::for_selection(now, files),
                                        _ => FileLink::new(now),
                                    };
                                    self.listen_for_files(admitted.connection, name, link);
                                }
                                // A device nearby asked this one to take files, by the capability for that: what it
                                // offers is taken as from a files code, each file by this person's consent.
                                (Purpose::NearbyFiles, Role::Responder) => {
                                    let name = admitted.name.clone();
                                    self.listen_for_files(admitted.connection, name, FileLink::new(tokio::time::Instant::now()));
                                }
                                // This device asked to pair or for a files code and was answered as one that offers
                                // files: it asked for nothing of the kind, so nothing goes.
                                (Purpose::NearbyFiles, Role::Initiator) => {
                                    admitted.connection.close(1_u32.into(), b"not asked for");
                                }
                            }
                        }
                        Work::NearbyAdmitted(result, sources) => {
                            self.admissions = self.admissions.saturating_sub(1);
                            let admitted = match result {
                                Ok(admitted) => admitted,
                                Err(reason) => return Event::Error(reason),
                            };
                            // Files go only to a device that admitted this one to offer files. One that answers with
                            // anything else (a pairing, most of all) is given nothing and is not paired with.
                            if admitted.purpose != Purpose::NearbyFiles {
                                admitted.connection.close(1_u32.into(), b"not for files");
                                return Event::Error("That device does not take files this way");
                            }
                            let mut offered = false;
                            for (id, source) in sources {
                                if self.transfers.send_as(id, admitted.connection.clone(), Some(admitted.name.clone()), None, source, &mut self.history, now).is_ok() { offered = true; }
                            }
                            if !offered { admitted.connection.close(0_u32.into(), b"nothing to send"); }
                            return Event::Changed;
                        }
                        Work::FileStream(connection, name, link, result) => {
                            let Ok((send, receive, request)) = result else {
                                // The sender is done, went away, or the time for this connection ran out: it is let go,
                                // never kept.
                                connection.close(0_u32.into(), b"done");
                                continue;
                            };
                            // Only a file is taken from a connection that was admitted for files. Anything else is refused.
                            let Request::File { offer } = request else {
                                connection.close(1_u32.into(), b"files only");
                                continue;
                            };
                            // Past the most files one code may carry, or past its time, nothing more is taken: the connection
                            // is closed, and another file needs another scan.
                            let Some(next) = link.after_one_more(tokio::time::Instant::now()) else {
                                connection.close(1_u32.into(), b"no more files");
                                continue;
                            };
                            let peer = PeerId(connection.remote_id().to_string());
                            self.waiting_files.push_back(WaitingFile {
                                peer,
                                incoming: Incoming { tx: send, rx: receive, offer },
                                connection,
                                origin: FileOrigin::Files { name, link: next },
                            });
                        }
                        Work::Nudged => {}
                        Work::Resumed(peer, result, outgoing) => {
                            if outgoing { self.connecting.remove(&peer); } else { self.admissions = self.admissions.saturating_sub(1); }
                            if let Ok(connection) = result {
                                if self.peers.get(&peer).is_none() { connection.close(1_u32.into(), b"forgotten"); }
                                else {
                                    let preferred = outgoing == (self.endpoint.id() < connection.remote_id());
                                    self.install(connection, preferred);
                                    return Event::Changed;
                                }
                            }
                        }
                        Work::Stream(peer, generation, result) => {
                            let Some(session) = self.sessions.get(&peer).filter(|session| session.generation == generation) else { continue };
                            let announcing = session.connection.clone();
                            let Ok((send, receive, request)) = result else {
                                if session.connection.close_reason().is_some() {
                                    self.sessions.remove(&peer);
                                    return Event::Changed;
                                }
                                self.listen(peer, generation);
                                return Event::Error("Invalid incoming stream");
                            };
                            match request {
                                // A device this one is not shown to gets nothing started here: its file and its extension stream
                                // are let go unread, which ends them on its side, and what it says of itself is still heard.
                                Request::File { .. } | Request::Plugin { .. } if !self.shown_to(&peer) => {
                                    self.listen(peer, generation);
                                }
                                Request::File { offer } => {
                                    self.waiting_files.push_back(WaitingFile {
                                        peer,
                                        incoming: Incoming { tx: send, rx: receive, offer },
                                        connection: announcing,
                                        origin: FileOrigin::Paired(generation),
                                    });
                                }
                                Request::Plugin { service, version } => {
                                    self.listen(peer.clone(), generation);
                                    return Event::Plugin { peer, service, version, send, receive };
                                }
                                Request::Presence { asleep, statuses, name } => {
                                    self.listen(peer.clone(), generation);
                                    let mut changed = false;
                                    if let Some(session) = self.sessions.get_mut(&peer)
                                        && session.generation == generation
                                    {
                                        // The whole of what the device says of itself, replacing what was known: never added up from pieces.
                                        changed = !session.said || session.asleep != asleep || session.statuses != statuses;
                                        session.said = true;
                                        session.asleep = asleep;
                                        session.statuses = statuses;
                                        // The name of THIS session's device only, which the connection already proved: one device can never rename
                                        // another. It was judged where it was read; a name the store refuses changes nothing.
                                        if let Some(name) = name
                                            && self.peers.rename(&peer, &name).unwrap_or(false)
                                        {
                                            changed = true;
                                        }
                                    }
                                    if changed {
                                        return Event::Changed;
                                    }
                                }
                                Request::Displays { displays } => {
                                    self.listen(peer.clone(), generation);
                                    if let Some(session) = self.sessions.get_mut(&peer)
                                        && session.generation == generation
                                        && session.displays != displays
                                    {
                                        session.displays = displays;
                                        return Event::Changed;
                                    }
                                }
                                Request::Accepts { services } => {
                                    self.listen(peer.clone(), generation);
                                    if let Some(session) = self.sessions.get_mut(&peer)
                                        && session.generation == generation
                                        && session.accepts.as_ref() != Some(&services)
                                    {
                                        session.accepts = Some(services);
                                        return Event::Changed;
                                    }
                                }
                                Request::Withdrawn {} => {
                                    self.listen(peer.clone(), generation);
                                    if let Some(session) = self.sessions.get_mut(&peer)
                                        && session.generation == generation
                                        && session.said
                                    {
                                        // Everything it said over this connection goes with it. Its picture is what it showed
                                        // last, kept as for a device that went offline.
                                        session.said = false;
                                        session.asleep = false;
                                        session.statuses.clear();
                                        session.displays.clear();
                                        session.accepts = None;
                                        return Event::Changed;
                                    }
                                }
                                // A kind a newer device sent and this build cannot read: let go of the stream and listen again. The
                                // connection is fine and nothing is wrong, so nothing is shown.
                                Request::Unknown => {
                                    self.listen(peer, generation);
                                }
                                Request::Profile {} => {
                                    self.listen(peer.clone(), generation);
                                    // Taken only by a host that keeps pictures: another is not asked to
                                    // read what it has no place for, and the stream is simply let go.
                                    if let Some(pictures) = self.pictures.clone() {
                                        self.work.spawn(async move {
                                            let (mut send, mut receive) = (send, receive);
                                            // A connection that went away mid-way (the other device restarted) is not a refusal: the
                                            // picture is asked for again when it reconnects. Only a picture that was wrong is reported.
                                            let result = match profile_exchange::serve(&mut send, &mut receive, &pictures).await {
                                                Ok(outcome) => Ok(Some(outcome)),
                                                Err(profile_exchange::Error::Transport(_) | profile_exchange::Error::Stream) => Ok(None),
                                                Err(_) => Err(()),
                                            };
                                            Work::Picture(peer, result)
                                        });
                                    }
                                }
                            }
                        }
                        Work::Told => {}
                        Work::Located(peer, generation, found) => {
                            // Only for the session that is still the one this device is connected through: a search that began on a connection
                            // that was since replaced must not write where a device WAS, over where it is. A device that is no longer remembered, or
                            // an address that cannot be dialled, is refused by the store and is not worth an error.
                            if let Some(seen) = found
                                && self.sessions.get(&peer).is_some_and(|session| session.generation == generation)
                                && self.peers.learn_address(&peer, seen).unwrap_or(false)
                            {
                                return Event::Changed;
                            }
                        }
                        Work::RelayProbed(key, answered) => {
                            self.relay_probes.insert(
                                key,
                                answered.map_or(peersus_core::model::RelayProbe::Unreachable, |ms| {
                                    peersus_core::model::RelayProbe::Answered { ms }
                                }),
                            );
                            return Event::Changed;
                        }
                        Work::Probed(peer, found) => {
                            // A device forgotten while its test went on has nothing to be told of: the answer would be about a device that is
                            // not there, and a window must not be shown one.
                            if self.peers.get(&peer).is_some() {
                                self.link_tests.insert(peer, found);
                                return Event::Changed;
                            }
                        }
                        Work::Announced(result) => {
                            self.finished += 1;
                            // Counted only when the picture itself crossed, which is what a test (and a
                            // person with a slow connection) wants to know: the digest alone is nothing.
                            if matches!(result, Ok(true)) { self.sent += 1; }
                        }
                        Work::Picture(peer, result) => {
                            let Ok(outcome) = result else { return Event::Error("A device's picture was refused") };
                            let Some(outcome) = outcome else { continue };
                            let before = self.held.get(&peer).copied();
                            match outcome {
                                Outcome::None => { self.held.remove(&peer); }
                                Outcome::Known(digest) | Outcome::Received(digest) => { self.held.insert(peer.clone(), digest); }
                            }
                            let now = self.held.get(&peer).copied();
                            // What a device showed is kept in a file, so the next start knows it before the device is online. A file that
                            // cannot be written costs the memory of this one change and nothing else: the picture is shown as it was.
                            if now != before && let Some(owners) = self.picture_owners.as_mut() {
                                let _ = owners.set(&peer, now);
                            }
                            // Only a change wakes the window: coming online with the same picture does not.
                            if now != before { return Event::Picture { peer }; }
                        }
                    }
                }
                _ = self.reconnect_tick.tick() => { self.reconnect(); }
            }
        }
    }

    async fn promote_waiting_files(&mut self, now: u64) -> io::Result<()> {
        while self.transfers.can_receive() {
            let Some(waiting) = self.waiting_files.pop_front() else {
                break;
            };
            if waiting.connection.close_reason().is_some() {
                continue;
            }
            if let FileOrigin::Paired(generation) = &waiting.origin
                && self
                    .sessions
                    .get(&waiting.peer)
                    .is_none_or(|session| session.generation != *generation)
            {
                continue;
            }
            // A sender can cancel while capacity is occupied. Never turn that old
            // request into a fresh consent prompt when a slot eventually opens.
            let cancelled = tokio::select! {
                biased;
                _ = waiting.incoming.tx.stopped() => true,
                () = std::future::ready(()) => false,
            };
            let WaitingFile {
                peer,
                incoming,
                connection,
                origin,
            } = waiting;
            if !cancelled {
                let name = match &origin {
                    FileOrigin::Paired(_) => None,
                    FileOrigin::Files { name, .. } => Some(name.clone()),
                };
                self.transfers
                    .receive(peer.clone(), name, incoming, connection.clone(), now)?;
            }
            match origin {
                FileOrigin::Paired(generation) => self.listen(peer, generation),
                FileOrigin::Files { name, link } => self.listen_for_files(connection, name, link),
            }
        }
        Ok(())
    }

    fn install(&mut self, connection: Connection, preferred: bool) {
        let peer = PeerId(connection.remote_id().to_string());
        if let Some(existing) = self.sessions.get_mut(&peer)
            && existing.connection.stable_id() == connection.stable_id()
        {
            // Admission can complete twice for handles to the same transport. Closing either would close the live session.
            existing.preferred |= preferred;
            return;
        }
        if let Some(held) = self.sessions.get(&peer)
            && !takes_the_place(
                held.connection.close_reason().is_none(),
                held.preferred,
                held.since.elapsed(),
                preferred,
            )
        {
            connection.close(0_u32.into(), b"duplicate session");
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let (say, said) = tokio::sync::mpsc::unbounded_channel();
        let speaking = connection.clone();
        self.work.spawn(async move {
            presence::speak(speaking, said).await;
            Work::Told
        });
        if let Some(old) = self.sessions.insert(
            peer.clone(),
            Session {
                connection,
                generation,
                preferred,
                since: tokio::time::Instant::now(),
                asleep: false,
                statuses: Vec::new(),
                displays: Vec::new(),
                accepts: None,
                said: false,
                say,
            },
        ) {
            old.connection.close(0_u32.into(), b"replaced session");
            self.waiting_files.retain(|waiting| {
                waiting.peer != peer || !matches!(waiting.origin, FileOrigin::Paired(_))
            });
        }
        self.listen(peer.clone(), generation);
        // A device that connects is told which picture this one holds (the picture itself only if it lacks it), how it is and
        // what it is called (a device renamed while this one was away would otherwise keep the name it knew), its displays and
        // what it takes. Nothing of it while this device keeps to itself: see `shown_to`.
        self.tell_all(&peer);
        self.learn_where_it_is(&peer);
    }

    /// What a device that has just connected tells of where it is: the direct (IP) address that the connection settled on, remembered so that
    /// this device can find it again if the address it was paired at is gone. It makes a phone that went from one network to another findable
    /// by whichever side still knows where the other is: either of them connects, the connection proves who it is, and both learn the new place
    /// without anything being typed.
    ///
    /// A connection often starts through a relay and only moves to a direct path once the two sides have found a way through to each other,
    /// which can be seconds later: so this looks at once and then now and then, for as long as such a move can take. A connection that never
    /// gets a direct path (it stays on a relay, or it ends first) has no address worth keeping, and the search ends with nothing.
    fn learn_where_it_is(&mut self, peer: &PeerId) {
        let Some(session) = self.sessions.get(peer) else {
            return;
        };
        let (connection, generation, peer) =
            (session.connection.clone(), session.generation, peer.clone());
        self.work.spawn(async move {
            let deadline = tokio::time::Instant::now() + LOCATE_FOR;
            let mut found = None;
            while found.is_none()
                && connection.close_reason().is_none()
                && tokio::time::Instant::now() < deadline
            {
                found = selected_direct(
                    connection
                        .paths()
                        .iter()
                        .map(|path| (path.is_selected(), path.remote_addr().clone())),
                );
                if found.is_none() {
                    tokio::time::sleep(LOCATE_EVERY).await;
                }
            }
            Work::Located(peer, generation, found)
        });
    }

    fn listen(&mut self, peer: PeerId, generation: u64) {
        let Some(session) = self.sessions.get(&peer) else {
            return;
        };
        let connection = session.connection.clone();
        self.work.spawn(async move {
            let result = async {
                let (send, mut receive) = connection.accept_bi().await.map_err(|_| ())?;
                let request = streams::receive(&mut receive).await.map_err(|_| ())?;
                Ok((send, receive, request))
            }
            .await;
            Work::Stream(peer, generation, result)
        });
    }

    /// Listen on a connection that was admitted ONLY to carry files: the next stream on it, until it closes. The device is
    /// not a peer and is not remembered; the connection is not in `sessions`, so nothing else (a plugin, a picture) can
    /// ever travel on it. Only a file offer is taken from it.
    fn listen_for_files(&mut self, connection: Connection, name: String, link: FileLink) {
        self.work.spawn(async move {
            // Waits for the next stream for as long as the connection lives. NO deadline on the connection as a whole: a
            // file that is still crossing on it must never be cut off by a clock that started at admission. How many
            // offers it takes is `FileLink`'s; what ends a batch properly is not settled here and is open work.
            let result = async {
                let (send, mut receive) = connection.accept_bi().await.map_err(|_| ())?;
                let request = streams::receive(&mut receive).await.map_err(|_| ())?;
                Ok((send, receive, request))
            }
            .await;
            Work::FileStream(connection, name, link, result)
        });
    }

    pub async fn close(mut self) {
        self.invitations.revoke();
        self.parked = None;
        self.selection = None;
        self.waiting_files.clear();
        for (_, session) in std::mem::take(&mut self.sessions) {
            session.connection.close(0_u32.into(), b"host stopped");
        }
        self.work.shutdown().await;
        self.endpoint.close().await;
    }
}

impl Drop for NetworkHost {
    fn drop(&mut self) {
        for session in self.sessions.values() {
            session.connection.close(0_u32.into(), b"host stopped");
        }
        self.work.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use super::plan_for;

    #[tokio::test]
    async fn installing_the_same_quic_connection_twice_never_closes_the_live_session() {
        use super::*;
        let directory = tempfile::tempdir().unwrap();
        let policy = peersus_core::NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..Default::default()
        };
        let a = transport::bind(iroh::SecretKey::generate(), &policy)
            .await
            .unwrap();
        let b = transport::bind(iroh::SecretKey::generate(), &policy)
            .await
            .unwrap();
        let (outgoing, incoming) = tokio::join!(a.connect(b.addr(), reconnect::ALPN), async {
            b.accept().await.unwrap().await.unwrap()
        });
        let connection = outgoing.unwrap();
        let (admitted, accepted) = tokio::join!(
            async {
                let (mut send, mut receive) = connection.open_bi().await.unwrap();
                transport::send(&mut send, &"resume").await.unwrap();
                send.finish().unwrap();
                assert_eq!(
                    transport::receive::<String>(&mut receive).await.unwrap(),
                    "accepted"
                );
            },
            reconnect::accept_known(incoming, Some(a.id()))
        );
        let () = admitted;
        let incoming = accepted.unwrap();
        let peer = PeerId(b.id().to_string());
        let mut host = NetworkHost::new(
            a,
            "fixture".into(),
            Peers::open(directory.path().join("peers.json")).unwrap(),
            History::open(directory.path().join("history.json")).unwrap(),
        );
        host.install(connection.clone(), false);
        let generation = host.sessions[&peer].generation;
        host.install(connection.clone(), true);
        assert!(
            connection.close_reason().is_none(),
            "reinstalling a handle closed the connection it still owns"
        );
        assert_eq!(host.sessions[&peer].generation, generation);
        assert!(host.sessions[&peer].preferred);
        host.close().await;
        incoming.close(0_u32.into(), b"fixture done");
        b.close().await;
    }

    #[test]
    fn only_the_same_moments_other_connection_is_a_duplicate() {
        use super::{SAME_MOMENT, takes_the_place};
        use std::time::Duration;
        let (moments, later) = (Duration::from_secs(1), SAME_MOMENT);
        // Both devices dialed at once: each keeps the preferred connection, whichever came first.
        assert!(!takes_the_place(true, true, moments, false));
        assert!(takes_the_place(true, false, moments, true));
        // The device that dialed did so again: it no longer has the one held here.
        assert!(takes_the_place(true, true, moments, true));
        assert!(takes_the_place(true, false, moments, false));
        // Later, any new connection is the device coming back, preferred or not.
        assert!(takes_the_place(true, true, later, false));
        // What is held is known to be gone.
        assert!(takes_the_place(false, true, moments, false));
    }

    #[tokio::test]
    async fn a_device_that_connects_again_takes_its_sessions_place_awake_and_a_duplicate_does_not()
    {
        use super::*;
        let directory = tempfile::tempdir().unwrap();
        let policy = peersus_core::NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..Default::default()
        };
        let a = transport::bind(iroh::SecretKey::generate(), &policy)
            .await
            .unwrap();
        let b = transport::bind(iroh::SecretKey::generate(), &policy)
            .await
            .unwrap();
        let mut made = Vec::new();
        let mut far = Vec::new();
        for _ in 0..3 {
            let (outgoing, incoming) = tokio::join!(a.connect(b.addr(), reconnect::ALPN), async {
                b.accept().await.unwrap().await.unwrap()
            });
            made.push(outgoing.unwrap());
            far.push(incoming);
        }
        let peer = PeerId(b.id().to_string());
        let mut host = NetworkHost::new(
            a,
            "fixture".into(),
            Peers::open(directory.path().join("peers.json")).unwrap(),
            History::open(directory.path().join("history.json")).unwrap(),
        );
        host.install(made[0].clone(), true);
        let first = host.sessions[&peer].generation;
        // The other connection of the same moment: closed, and the session is the one it was.
        host.install(made[1].clone(), false);
        assert!(made[1].close_reason().is_some(), "the duplicate was kept");
        assert!(made[0].close_reason().is_none());
        assert_eq!(host.sessions[&peer].generation, first);
        // The device said it sleeps, and long after connects again, by the connection that is
        // not the preferred one: it has come back, and is awake until it says otherwise.
        let session = host.sessions.get_mut(&peer).unwrap();
        session.asleep = true;
        session.since = session.since.checked_sub(SAME_MOMENT).unwrap();
        host.install(made[2].clone(), false);
        assert!(
            made[0].close_reason().is_some(),
            "the session it left is closed"
        );
        assert!(made[2].close_reason().is_none());
        let session = &host.sessions[&peer];
        assert_eq!(session.connection.stable_id(), made[2].stable_id());
        assert_ne!(session.generation, first);
        assert!(!session.asleep, "a device that came back was left asleep");
        host.close().await;
        for connection in far {
            connection.close(0_u32.into(), b"fixture done");
        }
        b.close().await;
    }

    #[test]
    fn an_admission_takes_the_files_of_its_own_code_and_they_are_taken_once() {
        let mut parked = Some((3, vec!["a", "b"]));
        assert_eq!(plan_for(&mut parked, 3), vec!["a", "b"]);
        assert!(parked.is_none(), "what was taken stayed parked");
        // A second admission with the same generation finds nothing: a code is single use.
        assert!(plan_for(&mut parked, 3).is_empty());
    }

    #[test]
    fn an_admission_made_with_an_older_code_takes_nothing_and_leaves_the_newer_plan_for_its_own_code()
     {
        // The plan on hand is the one of generation 2: the code of generation 1 was replaced after it was used.
        let mut parked = Some((2, vec!["private to the second code"]));
        assert!(
            plan_for(&mut parked, 1).is_empty(),
            "files of a newer code went to a device admitted with an older one"
        );
        assert_eq!(
            parked,
            Some((2, vec!["private to the second code"])),
            "the newer plan was spent by the older admission"
        );
        // The device that scanned the newer code still gets them.
        assert_eq!(plan_for(&mut parked, 2), vec!["private to the second code"]);
    }

    #[test]
    fn nothing_parked_means_nothing_to_take_for_any_generation() {
        let mut parked: Option<(u64, Vec<&str>)> = None;
        assert!(plan_for(&mut parked, 0).is_empty());
        assert!(plan_for(&mut parked, 7).is_empty());
        assert!(parked.is_none());
    }

    #[test]
    fn a_files_connection_takes_offers_up_to_the_most_one_code_carries_and_then_no_more() {
        use super::FileLink;
        let now = tokio::time::Instant::now();
        let mut link = FileLink::new(now);
        for taken in 0..FileLink::MOST_OFFERS {
            link = link
                .after_one_more(now)
                .unwrap_or_else(|| panic!("offer {taken} was refused before the limit"));
        }
        assert!(
            link.after_one_more(now).is_none(),
            "an offer past the most one code carries was taken"
        );
    }

    #[test]
    fn a_files_connection_takes_nothing_at_or_after_its_deadline_even_with_offers_to_spare() {
        use super::FileLink;
        let now = tokio::time::Instant::now();
        let link = FileLink::new(now);
        // Still inside its time: an offer is taken.
        assert!(
            link.after_one_more(now + FileLink::LIFE - std::time::Duration::from_secs(1))
                .is_some()
        );
        // At the moment the code stops being good, and after: nothing, however few were taken. A sender that keeps its
        // connection open past this offers nothing the receiver will take.
        assert!(
            link.after_one_more(now + FileLink::LIFE).is_none(),
            "an offer was taken at the deadline"
        );
        assert!(
            link.after_one_more(now + FileLink::LIFE * 3).is_none(),
            "an offer was taken long after the deadline"
        );
    }

    #[test]
    fn a_larger_selection_is_bounded_by_its_declared_count_and_the_original_deadline() {
        use super::FileLink;
        let now = tokio::time::Instant::now();
        let mut link = FileLink::for_selection(now, 40);
        for _ in 0..40 {
            link = link
                .after_one_more(now)
                .expect("a selected file was refused");
        }
        assert!(
            link.after_one_more(now).is_none(),
            "the authenticated selection allowance was exceeded"
        );
        let fresh = FileLink::for_selection(now, 40);
        assert!(
            fresh.after_one_more(now + FileLink::LIFE).is_none(),
            "larger selections extended the files-only authorization window"
        );
    }

    #[test]
    fn the_allowance_is_the_length_of_a_code_and_the_most_files_the_owner_accepts_at_once() {
        use super::FileLink;
        // Tied to the other two rules so that none can be changed without the others being looked at: a code is good for
        // 300 seconds (`invitation`), and the owner accepts at most sixteen files in one command.
        assert_eq!(FileLink::LIFE, std::time::Duration::from_secs(300));
        assert_eq!(FileLink::MOST_OFFERS, 16);
    }
}
