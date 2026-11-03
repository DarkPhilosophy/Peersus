//! Toolkit-independent network owner. No network future runs on a UI thread.
use crate::{
    file_session::Incoming,
    handshake::{self, Admitted},
    folder_sender::FolderSender,
    history::History,
    invitation::Invitation,
    invitations::{Invitations, Purpose},
    pairing_tasks::{self, PairingTasks},
    peers::Peers,
    reconnect,
    picture_owners::PictureOwners,
    pictures::Pictures,
    profile_exchange::{self, Outcome},
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
    model::{FilesCode, PairingPrompt, Peer, PeerId, Transfer},
    pairing::Role,
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

struct Session {
    connection: Connection,
    generation: u64,
    preferred: bool,
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
        Self { taken: 0, limit: Self::MOST_OFFERS, deadline: now + Self::LIFE }
    }

    fn for_selection(now: tokio::time::Instant, files: u32) -> Self {
        Self { limit: files as usize, ..Self::new(now) }
    }

    /// The allowance after one more offer is taken at `now`, or `None` when it may not be: the time is up, or that many have
    /// been taken already.
    fn after_one_more(self, now: tokio::time::Instant) -> Option<Self> {
        (now < self.deadline && self.taken < self.limit).then_some(Self { taken: self.taken + 1, ..self })
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
    /// A connection the gates admitted, and for a files code the generation of the code it was admitted with (0 when this
    /// host joined someone else's code, which has no plan here).
    Admitted(Result<(Admitted, u64), &'static str>, Role),
    /// The next stream on a connection that carries files only, the name the other device gave in its handshake, and how much
    /// of the connection's allowance is left.
    FileStream(Connection, String, FileLink, Routed),
    Resumed(PeerId, Result<Connection, ()>, bool),
    Stream(PeerId, u64, Routed),
    /// A profile exchange this host started, and whether the picture itself was SENT.
    Announced(Result<bool, ()>),
    /// A profile exchange another device started with this host, finished.
    Picture(PeerId, Result<Outcome, ()>),
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
}

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
        }
    }

    pub fn invite(&self, now: u64) -> Result<Invitation, crate::invitation::Error> {
        self.invitations.create(&self.endpoint, now)
    }

    /// A code to hand over files, and the files it is for. The sources are parked with the code: when a device that was
    /// shown it is admitted, THIS host offers them on that connection by itself, so nothing waits on a window. A new files
    /// code replaces the previous one, with what was parked for it. Never a pairing: it has its own gate.
    pub fn invite_for_files(&mut self, sources: Vec<Source>, now: u64) -> Result<Invitation, crate::invitation::Error> {
        let (invitation, generation) = self.invitations.create_for_file_selection(&self.endpoint, now, sources.len())?;
        // Kept with the generation of THIS code: an admission that was made with an earlier one, and has not finished yet,
        // finds a plan that is not its own and takes nothing.
        self.parked = Some((generation, sources));
        self.selection = None;
        Ok(invitation)
    }

    /// Show one code before opening files. Batches cannot renew its count or lifetime.
    pub fn begin_files_selection(&mut self, count: usize, now: u64) -> Result<Invitation, crate::invitation::Error> {
        if count == 0 { return Err(crate::invitation::Error::Invalid); }
        let (invitation, generation) = self.invitations.create_for_file_selection(&self.endpoint, now, count)?;
        self.parked = None;
        self.selection = Some(FileSelection {
            generation,
            remaining: count,
            until: tokio::time::Instant::now() + Duration::from_secs(invitation.expires_at.saturating_sub(now)),
            admitted: None,
            pending: VecDeque::new(),
            finished: false,
        });
        Ok(invitation)
    }

    /// False means backpressure, with no source consumed. An invalid target is an error.
    pub fn can_queue_selection(&self, generation: u64, count: usize, _now: u64) -> io::Result<bool> {
        let selection = self.selection.as_ref()
            .filter(|s| s.generation == generation && !s.finished && tokio::time::Instant::now() < s.until)
            .ok_or_else(|| io::Error::other("Files selection is no longer active"))?;
        if count == 0 || count > selection.remaining {
            return Err(io::Error::other("Files selection allowance exceeded"));
        }
        if let Some((connection, _)) = &selection.admitted
            && connection.close_reason().is_some()
        {
            return Err(io::Error::other("Files selection connection closed"));
        }
        let reserved = selection.pending.len().saturating_add(count);
        Ok(reserved <= crate::transfer_tasks::QUEUED_LIMIT && self.transfers.can_queue(reserved))
    }

    pub fn queue_selection(&mut self, generation: u64, sources: Vec<Source>, now: u64) -> io::Result<()> {
        if !self.can_queue_selection(generation, sources.len(), now)? {
            return Err(io::Error::other("The transfer queue is full"));
        }
        let selection = self.selection.as_mut().expect("validated selection");
        selection.remaining -= sources.len();
        selection.pending.extend(sources);
        Ok(())
    }

    /// Finishing cannot revoke files already queued or reopen a producer.
    pub fn finish_files_selection(&mut self, generation: u64, refused: usize) -> io::Result<()> {
        let selection = self.selection.as_mut().filter(|s| s.generation == generation && !s.finished)
            .ok_or_else(|| io::Error::other("Files selection is no longer active"))?;
        if selection.remaining != refused {
            return Err(io::Error::other("Files selection is not fully accounted for"));
        }
        selection.remaining = 0;
        selection.finished = true;
        Ok(())
    }

    fn feed_file_selection(&mut self, now: u64) -> io::Result<bool> {
        let Some(selection) = self.selection.as_mut() else { return Ok(false) };
        if tokio::time::Instant::now() >= selection.until
            || selection.admitted.as_ref().is_some_and(|(connection, _)| connection.close_reason().is_some())
            || (selection.finished && selection.pending.is_empty()) {
            self.selection = None;
            return Ok(true);
        }
        let Some((connection, name)) = &selection.admitted else { return Ok(false) };
        let mut changed = false;
        for _ in 0..16 {
            if !self.transfers.can_queue(1) { break; }
            let Some(source) = selection.pending.pop_front() else { break };
            self.transfers.send(connection.clone(), Some(name.clone()), Some(selection.generation), source, &mut self.history, now)?;
            changed = true;
        }
        Ok(changed)
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

    /// The sentence that says why an invitation did not become a connection. A fixed text for each kind of failure: it never holds the code, an address or
    /// the name of a device, so that it can be written into the diagnostics and shown to the person as it is.
    fn why(error: handshake::Error) -> &'static str {
        match error {
            handshake::Error::Connection => "The other device could not be reached with this invitation",
            handshake::Error::Refused => "The invitation was refused: it is expired, already used, or not the one this device showed",
            handshake::Error::Name => "The other device sent a name that is not valid",
            handshake::Error::Transport(_) => "The connection to the other device failed while it was being set up",
        }
    }

    /// The fixed sentence for the reason a pairing ceremony gave. The reasons come from this crate and are a few fixed texts, none of which holds a
    /// code, an address or a device's name, but a sentence is chosen here and not passed on as it came, so that nothing else can ever be written.
    fn why_pairing(reason: &str) -> &'static str {
        match reason {
            "Pairing task stopped unexpectedly" => "Pairing stopped unexpectedly before the two devices agreed",
            "Could not save paired device" => "The two devices agreed, but the paired device could not be saved",
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
        self.invitations.files_code().map(|(generation, live)| FilesCode {
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
                peer.online = self
                    .sessions
                    .get(&peer.id)
                    .is_some_and(|session| session.connection.close_reason().is_none());
                peer
            })
            .collect()
    }

    /// Test a producer batch before opening or taking its sources. Unknown and disconnected
    /// peers are errors, not backpressure: a producer must not wait forever for a stale target.
    pub fn can_queue_files(&self, peer: &PeerId, count: usize) -> io::Result<bool> {
        let session = self.sessions.get(peer).ok_or_else(|| io::Error::other("Device is offline"))?;
        if session.connection.close_reason().is_some() {
            return Err(io::Error::other("Device is offline"));
        }
        let reserved = self.selection.as_ref().map_or(0, |selection| selection.pending.len());
        Ok(self.transfers.can_queue(reserved.saturating_add(count)))
    }

    pub fn send(&mut self, peer: &PeerId, source: Source, now: u64) -> io::Result<String> {
        let session = self
            .sessions
            .get(peer)
            .ok_or_else(|| io::Error::other("Device is offline"))?;
        self.transfers
            .send(session.connection.clone(), None, None, source, &mut self.history, now)
    }

    /// Begin sending the folder `root` to the paired device `peer`: every file in it, with the folders it sits in. Nothing is opened and
    /// nothing is offered here: the folder is walked as the files go (see [`crate::folder_sender`]) and [`Self::feed_folders`] offers one
    /// whenever there is room, so a folder of any size is never held as a list and never as open files.
    ///
    /// Refused for a device that is offline or not paired, and for a `root` that is not a plain folder. Returns the id of this
    /// folder's sending, to ask how it stands and to stop it.
    pub fn send_folder(&mut self, peer: &PeerId, root: &Path) -> io::Result<u64> {
        if !self.sessions.contains_key(peer) {
            return Err(io::Error::other("Device is offline"));
        }
        // Directory walks keep capabilities open. Bound active walks separately from file-transfer slots.
        if self.folders.iter().filter(|run| !run.sender.is_done()).count() >= 16 {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "Folder queue is full"));
        }
        let sender = FolderSender::new(root)?;
        // Bound recent completed results; callers may also dismiss one with stop_folder.
        let mut completed = self.folders.iter().filter(|run| run.sender.is_done()).count();
        self.folders.retain(|run| {
            if run.sender.is_done() && completed >= 16 {
                completed -= 1;
                false
            } else {
                true
            }
        });
        self.folder_serial += 1;
        self.folders.push(FolderRun { id: self.folder_serial, peer: peer.clone(), sender });
        Ok(self.folder_serial)
    }

    /// Offer ONE more file of the folders being sent, if there is room and a folder has one. Returns whether a file was offered, so a
    /// caller that is not told "no more" calls it again. One file per call keeps each step short, which the loop that pumps events
    /// needs: a folder of a million files is a million short steps, not one long one.
    ///
    /// A folder whose device is gone is let go of. Finished walks remain as bounded recent results in
    /// [`Self::folder_state`]; starting another folder evicts the oldest completed results.
    pub async fn feed_folders(&mut self, now: u64) -> bool {
        self.folders.retain(|run| self.sessions.contains_key(&run.peer));
        if !self.transfers.has_room() {
            return false;
        }
        let Some(index) = self.folders.iter().position(|run| !run.sender.is_done()) else { return false };
        let peer = self.folders[index].peer.clone();
        let Some(source) = self.folders[index].sender.next().await else {
            // The walk is complete; preserve its counters among the bounded recent results.
            return true;
        };
        // A send that is refused (no room after all, the device just went) is not lost silently: the file was opened and is dropped,
        // and it is counted as one that could not be sent.
        if self.send(&peer, source, now).is_err() {
            self.folders[index].sender.count_failed();
        }
        true
    }

    /// How the folders being sent stand: for each, its id, what has been offered and what could not be, and what the walk left out.
    /// At most thirty-two retained walks, no more than sixteen of them still active.
    pub fn folder_state(&self) -> Vec<(u64, PeerId, crate::folder_sender::Sent, crate::walk::Passed, bool)> {
        self.folders
            .iter()
            .map(|run| (run.id, run.peer.clone(), run.sender.sent(), run.sender.passed(), run.sender.is_done()))
            .collect()
    }

    /// Stop sending a folder: no more of its files are offered. What was already offered stays offered, and is the person's to cancel.
    pub fn stop_folder(&mut self, id: u64) -> bool {
        let before = self.folders.len();
        self.folders.retain(|run| run.id != id);
        self.folders.len() != before
    }
    pub fn accept(&mut self, id: &str, directory: PathBuf) -> io::Result<()> {
        self.transfers.accept(id, directory, &mut self.history)
    }
    pub fn cancel(&mut self, id: &str) -> io::Result<()> {
        self.transfers.cancel(id, &mut self.history)?;
        // A cancelled exchange is over: it stays in the list until the person clears it.
        Ok(())
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
        declared: &[peersus_plugin_api::contributions::Setting],
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
        let network_changed = candidate.network != store.current().network;
        let replacement = if network_changed {
            if !self.waiting_files.is_empty() || self.transfers().iter().any(|transfer| {
                matches!(
                    transfer.state,
                    peersus_core::model::TransferState::Offered
                        | peersus_core::model::TransferState::Running
                )
            }) {
                return Err("Finish or cancel transfers before changing the network");
            }
            Some(
                transport::bind(self.endpoint.secret_key().clone(), &candidate.network)
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
            return Err("Could not save preferences");
        }
        self.name = candidate.device_name;
        self.set_offer_wait(candidate.offer_timeout_seconds);
        if let Some(endpoint) = replacement {
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
                Some(incoming) = self.endpoint.accept() => {
                    if self.admissions >= 8 {
                        // Refused with no word to anyone. If this counter ever stays high (a connection that is counted in and never out), every
                        // new invitation is refused from then on, so each refusal says so in the owner's log, with how many are counted.
                        eprintln!("peersus: refusing a new connection, {} are still counted as being admitted", self.admissions);
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
                            }
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
                                Request::Profile {} => {
                                    self.listen(peer.clone(), generation);
                                    // Taken only by a host that keeps pictures: another is not asked to
                                    // read what it has no place for, and the stream is simply let go.
                                    if let Some(pictures) = self.pictures.clone() {
                                        self.work.spawn(async move {
                                            let (mut send, mut receive) = (send, receive);
                                            let result = profile_exchange::serve(&mut send, &mut receive, &pictures).await.map_err(|_| ());
                                            Work::Picture(peer, result)
                                        });
                                    }
                                }
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
            let Some(waiting) = self.waiting_files.pop_front() else { break };
            if waiting.connection.close_reason().is_some() {
                continue;
            }
            if let FileOrigin::Paired(generation) = &waiting.origin
                && !self.sessions.get(&waiting.peer).is_some_and(|session| session.generation == *generation)
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
            let WaitingFile { peer, incoming, connection, origin } = waiting;
            if !cancelled {
                let name = match &origin {
                    FileOrigin::Paired(_) => None,
                    FileOrigin::Files { name, .. } => Some(name.clone()),
                };
                self.transfers.receive(peer.clone(), name, incoming, connection.clone(), now)?;
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
        if let Some(existing) = self.sessions.get(&peer)
            && existing.connection.close_reason().is_none()
            && (existing.preferred || !preferred)
        {
            connection.close(0_u32.into(), b"duplicate session");
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        if let Some(old) = self.sessions.insert(
            peer.clone(),
            Session {
                connection,
                generation,
                preferred,
            },
        ) {
            old.connection.close(0_u32.into(), b"replaced session");
            self.waiting_files.retain(|waiting| waiting.peer != peer || !matches!(waiting.origin, FileOrigin::Paired(_)));
        }
        self.listen(peer.clone(), generation);
        // A device that is online is told which picture this one holds: its digest, and the picture
        // itself only if the device turns out not to have it.
        self.announce(peer);
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
        let Some(pictures) = self.pictures.as_ref() else { return BTreeMap::new() };
        self.held.iter().filter(|(_, digest)| pictures.has(digest)).map(|(peer, digest)| (peer.clone(), *digest)).collect()
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
    fn announce(&mut self, peer: PeerId) {
        // A device whose own picture is damaged on disk says nothing about it: announcing "no picture"
        // would tell every other device to forget the picture it really has, over a fault in a file here.
        if self.pictures.is_none() || self.damaged {
            return;
        }
        let Some(session) = self.sessions.get(&peer) else { return };
        let connection = session.connection.clone();
        let own = self.own.clone();
        self.work.spawn(async move {
            let result = async {
                let (mut send, mut receive) = connection.open_bi().await.map_err(|_| ())?;
                streams::send(&mut send, &Request::Profile {}).await.map_err(|_| ())?;
                profile_exchange::announce(&mut send, &mut receive, own.as_ref()).await.map_err(|_| ())
            }
            .await;
            Work::Announced(result)
        });
    }

    fn reconnect(&mut self) {
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

    #[test]
    fn an_admission_takes_the_files_of_its_own_code_and_they_are_taken_once() {
        let mut parked = Some((3, vec!["a", "b"]));
        assert_eq!(plan_for(&mut parked, 3), vec!["a", "b"]);
        assert!(parked.is_none(), "what was taken stayed parked");
        // A second admission with the same generation finds nothing: a code is single use.
        assert!(plan_for(&mut parked, 3).is_empty());
    }

    #[test]
    fn an_admission_made_with_an_older_code_takes_nothing_and_leaves_the_newer_plan_for_its_own_code() {
        // The plan on hand is the one of generation 2: the code of generation 1 was replaced after it was used.
        let mut parked = Some((2, vec!["private to the second code"]));
        assert!(plan_for(&mut parked, 1).is_empty(), "files of a newer code went to a device admitted with an older one");
        assert_eq!(parked, Some((2, vec!["private to the second code"])), "the newer plan was spent by the older admission");
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
            link = link.after_one_more(now).unwrap_or_else(|| panic!("offer {taken} was refused before the limit"));
        }
        assert!(link.after_one_more(now).is_none(), "an offer past the most one code carries was taken");
    }

    #[test]
    fn a_files_connection_takes_nothing_at_or_after_its_deadline_even_with_offers_to_spare() {
        use super::FileLink;
        let now = tokio::time::Instant::now();
        let link = FileLink::new(now);
        // Still inside its time: an offer is taken.
        assert!(link.after_one_more(now + FileLink::LIFE - std::time::Duration::from_secs(1)).is_some());
        // At the moment the code stops being good, and after: nothing, however few were taken. A sender that keeps its
        // connection open past this offers nothing the receiver will take.
        assert!(link.after_one_more(now + FileLink::LIFE).is_none(), "an offer was taken at the deadline");
        assert!(link.after_one_more(now + FileLink::LIFE * 3).is_none(), "an offer was taken long after the deadline");
    }

    #[test]
    fn a_larger_selection_is_bounded_by_its_declared_count_and_the_original_deadline() {
        use super::FileLink;
        let now = tokio::time::Instant::now();
        let mut link = FileLink::for_selection(now, 40);
        for _ in 0..40 {
            link = link.after_one_more(now).expect("a selected file was refused");
        }
        assert!(link.after_one_more(now).is_none(), "the authenticated selection allowance was exceeded");
        let fresh = FileLink::for_selection(now, 40);
        assert!(fresh.after_one_more(now + FileLink::LIFE).is_none(),
            "larger selections extended the files-only authorization window");
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
