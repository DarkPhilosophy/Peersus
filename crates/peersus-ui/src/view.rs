//! What the screens show, worked out from the owner's `Snapshot` and nothing else.
//!
//! This is the only place that decides what a person is told: which devices are listed and how
//! each is described, what is waiting for their approval, how far a transfer has got. It is plain
//! data in and plain data out, so every rule here is tested without a window. The components
//! only draw it.
use crate::i18n::{self, Language, TimeUnit};
use peersus_core::{
    config::Settings,
    model::{
        ActionFailure, ActionState, FilesCode, Peer, PluginInfo, Snapshot, Transfer, TransferState,
    },
    profile::Digest,
};
pub fn action_words(state: Option<ActionState>) -> &'static str {
    match state {
        Some(ActionState::Running) => "Working…",
        Some(ActionState::Done) => "Completed",
        Some(ActionState::Cancelled) => "Cancelled. Check any external result before retrying.",
        Some(ActionState::Failed { reason }) => match reason {
            ActionFailure::NotAllowed => "Not allowed. Check this plugin's permissions.",
            ActionFailure::Unavailable => "Plugin unavailable. Start it and retry.",
            ActionFailure::TimedOut => "The action timed out. Check its result before retrying.",
            ActionFailure::Busy => "Another operation is running. Try again when it finishes.",
            ActionFailure::Other => "The action could not finish. Check the plugin and retry.",
        },
        None => "",
    }
}

/// Where the camera is at the circle of Receive: shut, looking for a code, or told no.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Looking {
    /// The camera is not open: the circle invites a tap.
    #[default]
    Shut,
    /// The camera is open and a code is being looked for.
    Scanning,
    /// The camera said it could not be used (the permission was refused): said in the circle, and a tap tries again.
    Refused,
}

/// What one look of the watcher at the camera makes of the state.
#[derive(Debug, PartialEq, Eq)]
pub struct Tick {
    /// The state after it.
    pub looking: Looking,
    /// An invitation was read: join it, and close the camera.
    pub join: Option<String>,
    /// The camera is to be closed now (a code was read, or it could not be used).
    pub close: bool,
}

/// What the watcher makes of one look: `found` is the invitation the scan read, if it read one, and `refused` is
/// whether the camera said it could not be used. A code read wins over a refusal that came with it: the person did
/// get a code, and that is what they asked for. Nothing found and nothing refused leaves the camera as it is.
pub fn look(now: Looking, found: Option<String>, refused: bool) -> Tick {
    match (now, found, refused) {
        (Looking::Shut, ..) => Tick {
            looking: Looking::Shut,
            join: None,
            close: false,
        },
        (Looking::Scanning | Looking::Refused, Some(code), _) => Tick {
            looking: Looking::Shut,
            join: Some(code),
            close: true,
        },
        (Looking::Scanning, None, true) => Tick {
            looking: Looking::Refused,
            join: None,
            close: true,
        },
        (looking, None, _) => Tick {
            looking,
            join: None,
            close: false,
        },
    }
}

/// What the person's pressing Stop makes of the state: a scan that is on, or one that was told no, is shut and its
/// camera is closed; one that is shut already has nothing to close. Nothing is ever joined by it.
pub fn stop(now: Looking) -> Tick {
    match now {
        Looking::Scanning | Looking::Refused => Tick {
            looking: Looking::Shut,
            join: None,
            close: true,
        },
        Looking::Shut => Tick {
            looking: Looking::Shut,
            join: None,
            close: false,
        },
    }
}

/// What the circle of Receive holds, for each state of the camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Circle {
    /// Words: a headline and a line under it. Shut (an invitation to tap) and refused (what to do about it).
    Words(&'static str, &'static str),
    /// What the camera sees, with the codes it has located marked on it, and a way to stop. No words: the picture is
    /// what tells the person where to aim, and a word over it would only be in the way of it.
    Camera,
    /// The camera was asked for and has not given a frame yet: the circle is clear, with one small line, so that it does
    /// not look as if nothing happened. It lasts the moment the camera takes to start.
    Starting(&'static str),
}

/// What the circle holds: in a scan it is the camera as soon as there is a picture of it, and before that a clear circle
/// with a small line; shut and refused say in words what to do.
pub fn circle_for(looking: Looking, has_picture: bool) -> Circle {
    match (looking, has_picture) {
        (Looking::Shut, _) => Circle::Words("SCAN", "tap to open the camera"),
        (Looking::Scanning, true) => Circle::Camera,
        (Looking::Scanning, false) => Circle::Starting("starting the camera"),
        (Looking::Refused, _) => Circle::Words(
            "NO CAMERA",
            "allow it for Peersus in the phone's settings, then tap",
        ),
    }
}

/// Which side of pairing is open in Peers. Showing a code and taking one are two halves of the same thing and are never open
/// together: two circles on one screen leave nobody knowing which one to hold up or to point at. Pressing the one closes
/// the other, and pressing the one that is open closes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PairMode {
    /// Neither: only the two buttons and what they do.
    #[default]
    Closed,
    /// This device's code is shown, for the other device to scan or to paste.
    Showing,
    /// The other device's code is taken here, by the camera or by pasting it.
    Taking,
}

impl PairMode {
    /// The mode after "Pair" is pressed: the code is shown. Pressing it again while it is shown changes nothing: it stays `Showing`.
    pub fn after_pair(self) -> Self {
        Self::Showing
    }

    /// Whether pressing "Pair" asks the owner for a code: only when none is on show. A code that is on show is still valid (the owner refuses it itself
    /// at its deadline, and the window takes it off the screen then, and makes no other by itself), and one made for every press would put a new picture
    /// under the person's camera while the other device was still scanning the one before it.
    pub fn pair_asks_for_a_code(self, code_on_show: bool) -> bool {
        !(self.shows_code() && code_on_show)
    }

    /// The mode after "Connect" is pressed: the code of the other device is taken, and the one that was shown goes. Pressing
    /// it again closes it.
    pub fn after_connect(self) -> Self {
        match self {
            Self::Taking => Self::Closed,
            Self::Closed | Self::Showing => Self::Taking,
        }
    }

    /// Whether this device's own code is on screen.
    pub fn shows_code(self) -> bool {
        self == Self::Showing
    }

    /// Whether the side that takes the other device's code is on screen.
    pub fn takes_code(self) -> bool {
        self == Self::Taking
    }
}

/// What the two buttons of Peers do, in words, under them. Fixed text: nothing read from a device is repeated in it.
pub const PAIR_EXPLAINS: &str = "Pair: show a code for the other device to scan. Connect: scan or paste the code the other device shows.";

/// What pairing means, said once under the buttons: it is how a device becomes trusted for good, and a file sent by code
/// alone never does that.
pub const PAIR_WARNS: &str =
    "Pairing trusts the other device until you forget it. A file sent by code alone never does.";

/// How the files a picker handed over are sent to a device that was chosen: as commands of at most sixteen files each,
/// which is the most the owner accepts at once, in the order they came, each under a name the owner will accept.
///
/// The name the provider gave is not trusted and is cleaned here ([`crate::outgoing::safe_name`]), because a file with
/// no path has no name of its own to fall back on, and a name the offer's rule refuses would make the whole batch fail
/// for a reason the person cannot see. No file is dropped and none is repeated.
pub fn held_sends(files: &[(u64, String)], _destination: &str) -> Vec<Vec<(u64, String)>> {
    const MOST_AT_ONCE: usize = 16;
    files
        .chunks(MOST_AT_ONCE)
        .map(|batch| {
            batch
                .iter()
                .map(|(number, name)| (*number, crate::outgoing::safe_name(name)))
                .collect()
        })
        .collect()
}

/// Desktop selections use the same bounded commands as provider-owned files, without cloning paths.
pub fn path_sends(paths: Vec<std::path::PathBuf>) -> impl Iterator<Item = Vec<std::path::PathBuf>> {
    let mut paths = paths.into_iter();
    std::iter::from_fn(move || {
        let batch: Vec<_> = paths.by_ref().take(16).collect();
        (!batch.is_empty()).then_some(batch)
    })
}

/// A file a picker handed over: the number it is held under, and the name the provider gave it.
pub type HeldFile = (u64, String);

/// Of the files a picker handed over for a profile picture, the one that is used and those that are to be let go of.
///
/// A picture is one file. The picker is asked for one, but what a provider hands over is not trusted to be one, and a
/// file nobody takes stays open in the registry until it is full, which then turns away new files for a reason the
/// person cannot see. So the first is used, in the order they came, and every other is returned to be released. `None`
/// when nothing was chosen.
pub fn picture_choice(files: &[HeldFile]) -> Option<(HeldFile, Vec<HeldFile>)> {
    let (first, rest) = files.split_first()?;
    Some((first.clone(), rest.to_vec()))
}

/// The edit the relay switch sends, to set the relays to `on`.
///
/// Turning them on also returns routing to the system's own. A device held to one local address cannot use relays
/// at all, and the owner refuses a pair of settings that say both: a switch that only said "relays on" was refused
/// whenever an address had been fixed (from the command line, say), with a message that told the person nothing.
/// The relays are how every network is reached, and what the person asked for is the whole internet: it is the fixed
/// address that gives way. Turning them off changes only that, since routing stays as it is.
pub fn relay_patch(on: bool) -> peersus_core::config::SettingsPatch {
    use peersus_core::config::{BindChange, SettingsPatch};
    let mut patch = SettingsPatch {
        allow_relay: Some(on),
        ..SettingsPatch::default()
    };
    if on {
        patch.bind = Some(BindChange::Automatic);
    }
    patch
}

/// How a paired device is described.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Presence {
    /// Connected and awake now: a send to it can start at once.
    Online,
    /// Connected, and it has said that it sleeps (its screen is off, or it has gone idle). It is there: a send waits for it
    /// and goes when it wakes.
    Sleeping,
    /// Remembered, but not reachable at the moment.
    Offline,
}

impl Presence {
    /// Whether a connection exists at all, awake or not: what lets a send be made.
    pub fn reachable(&self) -> bool {
        !matches!(self, Presence::Offline)
    }
}

/// One paired device in the row of destinations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The identifier a command names it by. Never shown.
    pub id: String,
    /// The name its owner gave it. Shown, but never trusted as an identity.
    pub name: String,
    pub presence: Presence,
    /// What this device has allowed each plugin to do for it. A plugin with nothing allowed has no
    /// entry. Not shown anywhere but the Permissions tab.
    pub grants:
        std::collections::BTreeMap<peersus_core::plugins::PluginId, peersus_core::plugins::Grants>,
    /// The digest of the picture this device last showed, if it showed one.
    pub picture: Option<Digest>,
    /// When the pairing completed, in Unix seconds.
    pub paired_at: u64,
    /// The last time anything authenticated came from it, in Unix seconds, if ever.
    pub last_seen: Option<u64>,
    /// What the last test of the connection to this device found, if one was made and not cleared.
    pub link_test: Option<peersus_core::model::LinkTest>,
    /// Where the person said this device can be found, as they would type it again, if they said.
    pub typed_address: Option<String>,
}

/// A request from another device waiting for the person to decide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub request: String,
    pub name: String,
    /// The three numbers on offer; the right one is the one the other device shows.
    pub options: [u8; 3],
    /// This device already answered; nothing more to choose.
    pub answered: bool,
    pub seconds_left: u64,
}

/// A transfer, described for a progress line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub id: String,
    pub name: String,
    pub folders: Vec<String>,
    /// The length of the file in bytes, as the sender advertised it: what the list of an offer shows beside each name.
    pub bytes: u64,
    pub incoming: bool,
    /// The name the sender gave itself while the offer came in, as a label and nothing more; `None` when it gave none or
    /// the transfer is not one that came in by a code.
    pub from: Option<String>,
    /// The device on the other end, by its identity: what tells two devices of one name apart, and what a device's own page
    /// finds its history by. Never the name it gave itself, which two devices can share and anyone can set.
    pub peer: String,

    /// Whole percent, 0..=100. A transfer of nothing is complete, not a division by zero.
    pub percent: u8,
    /// The generation of the files code this transfer came of, or `None` when it came of none. What the circle that drew that
    /// code matches on: never a position in a list, which pruning and other sends move.
    pub code: Option<u64>,
    /// When an offer that is waiting for an answer ends by itself, in Unix seconds, as the owner reports it. The time left is
    /// worked out from this at the moment of drawing (see [`time_left`]), never kept as a count that would stand still between
    /// two snapshots. `None` when nothing is waiting on a clock.
    pub expires_at: Option<u64>,
    /// Where a received file was written on this device once it arrived whole, as the owner reports it. What a platform that must
    /// move the file on (into a folder the person chose) is told to move.
    pub saved_as: Option<std::path::PathBuf>,
    pub phase: Phase,
}

/// Where a transfer stands, in words the application chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for the receiving person to accept.
    Waiting,
    Moving,
    Done,
    Cancelled,
    /// It did not complete, with the reason the application gave.
    Failed(String),
}

impl Phase {
    /// Whether the transfer is over, done or not: the record of it is only a memory, which is what Clear forgets. The same
    /// line the owner draws for a transfer still going (see `TransferState::is_active`), said in the window's own words.
    pub fn is_over(&self) -> bool {
        !matches!(self, Phase::Waiting | Phase::Moving)
    }
}

/// Everything the screens need.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct View {
    pub devices: Vec<Device>,
    pub approvals: Vec<Approval>,
    pub extension_prompts: Vec<peersus_core::model::ExtensionPrompt>,
    pub transfers: Vec<Progress>,
    /// The ids of the transfers that were already in the record when the window opened: the history, which Send and Receive do not list (see
    /// [`of_this_run`]). Empty until the window says it; the Transfers tab and Settings read `transfers` and are not touched by it.
    pub transfers_at_start: Vec<String>,
    /// What the owner reported going wrong, shown near the work it touches.
    pub errors: Vec<String>,
    /// How many times the owner has said something, so the same failure said again is shown again.
    pub errors_said: u64,
    /// The saved preferences, as the owner holds them now. Not `Eq`: they hold a float.
    pub settings: Settings,
    /// Installed plugins with their declared capabilities and health.
    pub plugins: Vec<PluginInfo>,
    /// Packages the person chose and the owner read, waiting to be confirmed.
    pub staged: Vec<PluginInfo>,
    /// The extension store as the owner reports it: what the maintainer approved, and where the store has got to.
    pub store: peersus_core::model::StoreState,
    /// Plugin-agnostic action outcomes and declared preferences for each paired device.
    pub actions: Vec<peersus_core::model::ActionStatus>,
    pub plugin_values: std::collections::BTreeMap<
        peersus_core::model::PeerId,
        std::collections::BTreeMap<peersus_core::plugins::PluginId, peersus_core::plugins::Values>,
    >,
    /// What each plugin is allowed outright, for no device in particular. A plugin with nothing
    /// allowed has no entry.
    pub grants:
        std::collections::BTreeMap<peersus_core::plugins::PluginId, peersus_core::plugins::Grants>,
    /// The digest of this device's own profile picture. The picture is not here: the window asks for it
    /// by this digest when it does not hold it.
    pub profile_picture: Option<Digest>,
    /// The files code the owner reports as on show, if any. Never the code itself.
    pub files_code: Option<peersus_core::model::FilesCode>,
    /// The port this device listens on now: what another device writes after its address to find this one. Shown in Settings, read only.
    pub listening_port: Option<u16>,
    pub relay: Option<peersus_core::relay::RelayDiagnostics>,
    /// What the last test of each relay found, by its probe key.
    pub relay_probes: std::collections::BTreeMap<String, peersus_core::model::RelayProbe>,
    /// The displays of this device's desktop, and of each connected device that said its own, by device id: what a layout places.
    pub displays: Vec<peersus_core::layout::Display>,
    pub peer_displays: std::collections::BTreeMap<String, Vec<peersus_core::layout::Display>>,
    /// What each extension page shows, as the owner checked it.
    pub page_views: Vec<peersus_core::model::PageView>,
}

impl View {
    /// Whether there is anything for Clear to forget. A button that would do nothing is not shown.
    pub fn has_finished_transfers(&self) -> bool {
        self.transfers.iter().any(|item| item.phase.is_over())
    }

    /// How many files went out whole, how many came in whole, and how many in all. Only a transfer that is done counts: one that was
    /// refused, cancelled or failed moved no file, and one that is still going has not moved it yet. The record is the one that Clear
    /// forgets, so after Clear the counts start again from nothing, and they say so: they are of what the list still holds.
    pub fn file_totals(&self) -> FileTotals {
        let done = |incoming: bool| {
            self.transfers
                .iter()
                .filter(|item| item.incoming == incoming && item.phase == Phase::Done)
                .count()
        };
        let (sent, received) = (done(false), done(true));
        FileTotals {
            sent,
            received,
            total: sent + received,
        }
    }
}

/// What the overview says of the files that moved: out, in, and both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileTotals {
    pub sent: usize,
    pub received: usize,
    pub total: usize,
}

/// How a send will go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendState {
    /// Straight to every paired device the person picked, in the order they picked them, every one of which is reachable now.
    Ready(Vec<String>),
    /// No device was picked: the files wait behind a code, and go to whoever takes it.
    ByCode,
    /// A device that was picked is no longer reachable. Nothing else is chosen in its place, the others picked are not sent to alone,
    /// and no code is made behind the person's back: the person is told, and decides.
    ChosenUnavailable,
}

impl View {
    pub fn of(snapshot: &Snapshot) -> Self {
        let mut approvals: Vec<Approval> = snapshot
            .pairing
            .iter()
            .map(|prompt| Approval {
                request: prompt.id.clone(),
                name: prompt.name.clone(),
                options: prompt.options,
                answered: prompt.selected,
                seconds_left: prompt.remaining_seconds,
            })
            .collect();
        // Every request lives for the same time, so the one with the most left is the newest.
        // It is the one the person just started, so it is listed first; older ones follow. The
        // sort is stable, so requests with equal time keep the order the owner gave them.
        approvals.sort_by_key(|approval| std::cmp::Reverse(approval.seconds_left));
        Self {
            devices: snapshot
                .peers
                .iter()
                .map(|peer| {
                    device(
                        peer,
                        snapshot.peer_pictures.get(&peer.id).copied(),
                        snapshot.link_tests.get(&peer.id).copied(),
                        snapshot.typed_addresses.get(&peer.id).cloned(),
                    )
                })
                .collect(),
            approvals,
            extension_prompts: snapshot.extension_prompts.clone(),
            transfers: snapshot.transfers.iter().map(progress).collect(),
            transfers_at_start: Vec::new(),
            files_code: snapshot.files_code,
            listening_port: snapshot.listening_port,
            relay: snapshot.relay,
            relay_probes: snapshot.relay_probes.clone(),
            page_views: snapshot.page_views.clone(),
            errors: snapshot.errors.clone(),
            errors_said: snapshot.errors_said,
            settings: snapshot.settings.clone(),
            plugins: snapshot.plugins.clone(),
            staged: snapshot.staged.clone(),
            store: snapshot.store.clone(),
            actions: snapshot.actions.clone(),
            plugin_values: snapshot
                .peers
                .iter()
                .map(|peer| (peer.id.clone(), peer.plugin_values.clone()))
                .collect(),
            grants: snapshot.grants.clone(),
            profile_picture: snapshot.profile_picture,
            displays: snapshot.displays.clone(),
            peer_displays: snapshot
                .peer_displays
                .iter()
                .map(|(peer, displays)| (peer.0.clone(), displays.clone()))
                .collect(),
        }
    }

    /// Devices a send can start to now: online ones, in the order they are listed.
    pub fn reachable(&self) -> impl Iterator<Item = &Device> {
        self.devices
            .iter()
            .filter(|device| device.presence == Presence::Online)
    }

    /// The devices a send goes to, or `None`.
    ///
    /// ONLY devices the person picked from their paired devices, and only while EVERY one of them is reachable. Nothing is ever chosen
    /// for them: not when a single device happens to be online, not when one they picked has gone away and another is available, and
    /// not by sending to those of their picks that are left, since what is about to be sent is their own files and part of the choice
    /// is not the choice. With no destination a send is by code.
    pub fn destinations(&self, chosen: &[String]) -> Option<Vec<&Device>> {
        if chosen.is_empty() {
            return None;
        }
        chosen
            .iter()
            .map(|id| self.reachable().find(|device| device.id == *id))
            .collect()
    }

    /// How a send will go. The one place that decides it, so the words shown and the action
    /// taken can never disagree.
    pub fn send_state(&self, chosen: &[String]) -> SendState {
        match (chosen.is_empty(), self.destinations(chosen)) {
            (_, Some(devices)) => {
                SendState::Ready(devices.iter().map(|device| device.id.clone()).collect())
            }
            (false, None) => SendState::ChosenUnavailable,
            (true, None) => SendState::ByCode,
        }
    }

    /// Whether this device still carries the name it was installed with. Two devices that both
    /// say "My device" cannot be told apart by the person choosing where to send, so the first
    /// thing they are asked is a name. Taken from the owner's own default, so it cannot drift.
    pub fn needs_a_name(&self) -> bool {
        self.settings.device_name.trim() == Settings::default().device_name
    }

    /// What the window is called, so that two windows of this program on one screen (one per
    /// device, as when trying it out) can be told apart: the product, then the device's name.
    /// A device still on the default name, or with a blank one, is only the product.
    pub fn window_title(&self) -> String {
        let name = self.settings.device_name.trim();
        if name.is_empty() || self.needs_a_name() {
            "Peersus".to_owned()
        } else {
            format!("Peersus \u{b7} {name}")
        }
    }
}

/// Replace `current` with what `snapshot` shows, and say whether anything actually changed.
///
/// The owner publishes a fresh snapshot every second whether or not anything happened, so a
/// screen redrawn on each one is rebuilt (and its tree changed under the pointer) a thousand
/// times an hour for nothing. Nothing is touched when the new view equals the old one.
///
/// What the window said about its own start (`transfers_at_start`) is not the snapshot's to say: it is kept across every replacement and is
/// not a change. If it were rebuilt from the snapshots, the history would be listed again the first time anything moved.
pub fn refresh(current: &mut View, snapshot: &Snapshot) -> bool {
    let mut next = View::of(snapshot);
    next.transfers_at_start = current.transfers_at_start.clone();
    if *current == next {
        return false;
    }
    *current = next;
    true
}

impl Progress {
    /// Whether the person may still stop it: it is waiting for approval or moving.
    pub fn stoppable(&self) -> bool {
        matches!(self.phase, Phase::Waiting | Phase::Moving)
    }

    /// Whether this device is being asked to accept it. Only an offer that is coming in and has
    /// not been answered: for one going out it is the other device that decides.
    pub fn acceptable(&self) -> bool {
        self.incoming && self.phase == Phase::Waiting
    }
}

/// The offer to show over the whole window, if there is one: the first file coming in that waits for an answer. A decision
/// with a clock on it (an offer that is not answered in two minutes is refused), so it is not left as one row among many in a
/// list under a circle, where it was not seen: it is shown on whatever screen the person is on.
///
/// Only one at a time, the oldest, so that the answer is to a thing and not to a pile; the rest follow as each is answered.
pub fn incoming_offer(transfers: &[Progress]) -> Option<&Progress> {
    transfers.iter().find(|item| item.acceptable())
}

/// What the overlay says of the offer it shows: ONE file, and who sends it. Who sends is the name that device gave itself
/// while the offer arrived: its own word, shown as a label and never trusted as who it is, and when it gave none the words do
/// not pretend it did.
///
/// It says nothing of how many others wait: those may come from other devices, and "Alice wants to send you 3 files" when Alice
/// offered one is false. Accept answers THIS file, so the sentence is about this file; the size of the queue is
/// [`queue_words`], a line of its own.
pub fn offer_words(offer: &Progress) -> String {
    offer_words_in(Language::English, offer)
}

pub fn offer_words_in(language: Language, offer: &Progress) -> String {
    match offer
        .from
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        Some(name) => i18n::format(
            language,
            "{name} wants to send you a file",
            &[("name", name)],
        ),
        None => i18n::translate(language, "A device wants to send you a file").to_owned(),
    }
}

/// How many files wait for an answer in all, said apart from the sentence about the one shown, and only when there is more than
/// the one: nothing to add about a queue of one. `waiting` counts the shown file too, whoever sent which.
pub fn queue_words(waiting: usize) -> Option<String> {
    queue_words_in(Language::English, waiting)
}

pub fn queue_words_in(language: Language, waiting: usize) -> Option<String> {
    (waiting > 1).then(|| {
        i18n::format(
            language,
            "{files} are waiting in all",
            &[("files", &i18n::files(language, waiting))],
        )
    })
}

/// What the circle shows while the files code the window drew is out. Decided from exactly three things the owner reports: the
/// generation of the code that was drawn (kept with the drawing), the status of the code on show ([`View::files_code`]) and the
/// transfers tagged with that generation ([`Progress::code`]). Never from a count or a position in the list: pruning, and sends
/// to other devices, move those.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeCircle {
    /// Nobody has scanned it yet and it can still admit a device: the code is drawn.
    Code,
    /// It was scanned (it admits no more) but no file is reported for it yet: the other device was admitted and the files are about
    /// to be offered. The code is NOT drawn: it is spent.
    Scanned,
    /// The files were offered; the other device has not accepted any yet.
    WaitingForApproval { files: usize },
    /// The other device accepted and the files are crossing, `percent` of the way in all.
    Sending { files: usize, percent: u8 },
    /// Every file that went out for this code arrived.
    Sent { files: usize },
    /// Some did not: refused by the other device, or cancelled, or failed. `done` of `files` arrived.
    Stopped { files: usize, done: usize },
    /// The code is not usable and nothing came of it: it was replaced, revoked with the network, or its time ran out. Nothing
    /// is drawn, so that no picture stands for something that no longer works.
    Gone,
}

impl CodeCircle {
    /// Whether pressing the circle chooses files again. Not while a code is out or work is going on (a second choice would replace
    /// what is being offered or sent), but yes when nothing is: no code, one that is gone, and the two ways a send is OVER,
    /// `Sent` and `Stopped`, so that sending again never needs Clear first.
    pub fn lets_choose(&self) -> bool {
        matches!(self, Self::Gone | Self::Sent { .. } | Self::Stopped { .. })
    }
}

/// The circle for the code of generation `drawn`.
///
/// A code that expires unscanned is not published by the owner as it happens (nothing changes that a snapshot would carry), so
/// the window also stops drawing it at the deadline the reply carried: see [`code_expired`].
pub fn code_circle(
    drawn: u64,
    files_code: Option<FilesCode>,
    transfers: &[Progress],
) -> CodeCircle {
    code_circle_selection(drawn, files_code, transfers, 0)
}

/// The denominator includes the whole choice, not just the descriptors opened so far.
pub fn code_circle_selection(
    drawn: u64,
    files_code: Option<FilesCode>,
    transfers: &[Progress],
    expected: usize,
) -> CodeCircle {
    let sent: Vec<&Progress> = transfers
        .iter()
        .filter(|item| !item.incoming && item.code == Some(drawn))
        .collect();
    let files = sent.len().max(expected);
    if sent.is_empty() {
        return match files_code {
            Some(code) if code.generation == drawn && code.live => CodeCircle::Code,
            Some(code) if code.generation == drawn => CodeCircle::Scanned,
            _ => CodeCircle::Gone,
        };
    }
    let done = sent.iter().filter(|item| item.phase == Phase::Done).count();
    let ended_badly = sent
        .iter()
        .any(|item| matches!(item.phase, Phase::Cancelled | Phase::Failed(_)));
    let pending = files_code.is_some_and(|code| code.generation == drawn && code.pending);
    let still_going = sent
        .iter()
        .any(|item| matches!(item.phase, Phase::Waiting | Phase::Moving));
    if still_going || pending {
        // Waiting for approval until anything has begun to move or has finished; then it is sending.
        if sent.iter().all(|item| item.phase == Phase::Waiting) {
            return CodeCircle::WaitingForApproval { files };
        }
        let percent = (sent.iter().map(|item| u64::from(item.percent)).sum::<u64>() / files as u64)
            .min(if pending { 99 } else { 100 }) as u8;
        return CodeCircle::Sending { files, percent };
    }
    if ended_badly || done < files {
        CodeCircle::Stopped { files, done }
    } else {
        CodeCircle::Sent { files }
    }
}

/// Everything that bears on the circle of Send, gathered once: the files that are listed, where they would go, the code that was made (if any), the
/// transfers of the batch that is chosen now, and the setting that sends at a tap. The circle reads nothing else.
#[derive(Clone, Debug)]
pub struct CircleInputs<'a> {
    /// How many files are listed, waiting for the circle. Zero when none.
    pub listed: usize,
    pub aim: Aim,
    pub send_instantly: bool,
    /// What the code that was made says now, `None` when no code is shown.
    pub code: Option<CodeCircle>,
    /// The transfers of the batch chosen now (see [`of_this_run`]), outgoing and incoming.
    pub batch: &'a [Progress],
    /// How many files were just handed over and not yet answered for by the owner: nothing is listed any more and no transfer is named yet, but the files
    /// are on their way. Zero once the owner has answered (the batch is then the transfers it named).
    pub handed_over: usize,
}

/// What the circle says and does, as one answer. There is no second place that decides any of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CircleView {
    /// What a touch on it does (see [`CircleDoes`]); `None` while it is shut and a touch does nothing.
    pub does: Option<CircleDoes>,
    /// Whether the code itself (its picture) is what is drawn in it.
    pub shows_code: bool,
    /// What a transfer of the batch says while the circle is not the code and nothing is listed: its words.
    pub batch: BatchState,
}

/// The one decision. In order, and each step is the whole answer when it applies:
/// 1. A code that is drawn, or scanned and waiting, is the circle: it shows the code and a touch does nothing else.
/// 2. A transfer of the batch that really goes (moving, or waiting for an answer) shuts the circle: it says what is happening and a touch does nothing.
/// 3. Files that are listed make it what it does with them ([`circle_does`]), whatever finished or failed before.
/// 4. Nothing listed and nothing going: the batch that ended says how it ended, and a touch chooses new files; with no batch at all it asks for files.
pub fn circle_view(input: &CircleInputs) -> CircleView {
    let batch = batch_state(input.batch);
    // The stage is the one decision (see [`stage_of`]); what a touch does follows from it and from nothing else.
    match stage_of(input) {
        Stage::Code { scanned } => CircleView {
            does: None,
            shows_code: !scanned,
            batch,
        },
        Stage::Going { .. } => CircleView {
            does: None,
            shows_code: false,
            batch,
        },
        Stage::Ready { .. } | Stage::Empty | Stage::Ended { .. } => CircleView {
            does: Some(circle_does(
                input.listed > 0,
                &input.aim,
                input.send_instantly,
            )),
            shows_code: false,
            batch,
        },
    }
}

/// What the circle shows in words, from the one decision ([`circle_view`]) and what it is the circle of: the kicker above, the big words, the line under,
/// and whether it is the form of the circle that asks for files (which is drawn larger and with the invitation under it). The page chooses no words
/// itself. `devices` is how many are picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CircleWords {
    pub kicker: String,
    pub headline: String,
    pub line: String,
    /// The circle that only asks for files: its two lines are large and the invitation to tap or click goes under them.
    pub asks_for_files: bool,
}

pub fn circle_view_words_in(
    language: Language,
    circle: &CircleView,
    code: Option<&CodeCircle>,
    devices: usize,
    result_shown: bool,
) -> CircleWords {
    // A transfer of the batch that goes, or a code that is waiting for the other device: what is happening, in words.
    if circle.does.is_none() {
        if let Some(code) = code
            && let Some((kicker, headline, line)) = circle_words_in(language, code)
        {
            return CircleWords {
                kicker: kicker.to_owned(),
                headline,
                line,
                asks_for_files: false,
            };
        }
        if let Some((kicker, headline, line)) = batch_words_in(language, &circle.batch) {
            return CircleWords {
                kicker,
                headline,
                line,
                asks_for_files: false,
            };
        }
    }
    let does = circle.does.unwrap_or(CircleDoes::ChooseFiles);
    // Nothing is listed and a batch ended: how it ended, and a touch chooses new files, for as long as the result is shown (see [`shows_result`]);
    // once it has faded the circle asks for files, as it does with no batch at all.
    if does == CircleDoes::ChooseFiles
        && result_shown
        && let Some((kicker, headline, line)) = batch_words_in(language, &circle.batch)
    {
        return CircleWords {
            kicker,
            headline,
            line,
            asks_for_files: false,
        };
    }
    let (kicker, headline, line) = circle_does_words_in(language, does, devices);
    CircleWords {
        kicker,
        headline,
        line,
        asks_for_files: does == CircleDoes::ChooseFiles,
    }
}

/// Whether the time a code was made for has passed: `expires_at` is the seconds the reply to the command that made it carried,
/// `now` the same clock. The owner does not publish a snapshot at the deadline, so the window decides this itself, and only
/// to stop drawing: whether the code still admits anyone is the owner's to say, and it says no at the same moment.
pub fn code_expired(expires_at: u64, now: u64) -> bool {
    now >= expires_at
}

/// How long a code made to end at `expires_at` still has, at `now`, both in seconds of the same clock. Nothing once it has ended.
pub fn until_expiry(expires_at: u64, now: u64) -> std::time::Duration {
    std::time::Duration::from_secs(expires_at.saturating_sub(now))
}

pub fn circle_words_in(
    language: Language,
    circle: &CodeCircle,
) -> Option<(&'static str, String, String)> {
    let text = |key: &'static str| i18n::translate(language, key);
    let files = |count| i18n::files(language, count);
    match circle {
        CodeCircle::Code | CodeCircle::Gone => None,
        CodeCircle::Scanned => Some((
            text("CODE SCANNED"),
            text("SCANNED").to_owned(),
            text("The other device read the code. Waiting for it to accept.").to_owned(),
        )),
        CodeCircle::WaitingForApproval { files: count } => Some((
            text("WAITING FOR APPROVAL"),
            count.to_string(),
            i18n::format(
                language,
                "Waiting for the other device to accept {files}.",
                &[("files", &files(*count))],
            ),
        )),
        CodeCircle::Sending {
            files: count,
            percent,
        } => Some((
            text("SENDING"),
            format!("{percent}%"),
            if *count == 1 {
                text("Sending 1 file.").to_owned()
            } else {
                i18n::format(language, "Sending {files}.", &[("files", &files(*count))])
            },
        )),
        CodeCircle::Sent { files: count } => Some((
            text("DONE"),
            if *count == 1 {
                text("1 SENT").to_owned()
            } else {
                i18n::format(language, "{count} SENT", &[("count", &count.to_string())])
            },
            if *count == 1 {
                text("1 file arrived.").to_owned()
            } else {
                i18n::format(language, "{files} arrived.", &[("files", &files(*count))])
            },
        )),
        CodeCircle::Stopped { files: count, done } => Some((
            text("STOPPED"),
            format!("{done}/{count}"),
            i18n::format(
                language,
                "{done} of {files} arrived. The rest was refused, cancelled or failed.",
                &[("done", &done.to_string()), ("files", &files(*count))],
            ),
        )),
    }
}

pub fn circle_time_in(
    language: Language,
    circle: &CodeCircle,
    drawn: u64,
    transfers: &[Progress],
    now: u64,
) -> Option<String> {
    if !matches!(circle, CodeCircle::WaitingForApproval { .. }) {
        return None;
    }
    let soonest = transfers
        .iter()
        .filter(|item| !item.incoming && item.code == Some(drawn) && item.phase == Phase::Waiting)
        .filter_map(|item| item.expires_at)
        .min()?;
    time_left(Some(soonest), now).map(|seconds| {
        i18n::format(
            language,
            "ENDS IN {time}",
            &[("time", &clock_words(seconds))],
        )
    })
}

/// How many chosen files are named under the circle before the rest are counted. A bound on what is drawn, not on what is sent:
/// every file chosen goes, however many are named here.
pub const NAMED_UNDER_CIRCLE: usize = 5;

/// The lines that say WHICH files the code under the circle hands over: each name, up to [`NAMED_UNDER_CIRCLE`], then how many more
/// there are. Only the names the files go by, never where they were; empty when nothing was chosen.
pub fn chosen_words(names: &[String]) -> Vec<String> {
    chosen_words_total(names, names.len())
}

/// Names may arrive after the count, without holding every descriptor open.
pub fn chosen_words_total(names: &[String], total: usize) -> Vec<String> {
    chosen_words_total_in(Language::English, names, total)
}

pub fn chosen_words_total_in(language: Language, names: &[String], total: usize) -> Vec<String> {
    if names.is_empty() && total > 0 {
        return vec![if total == 1 {
            i18n::translate(language, "1 file selected").to_owned()
        } else {
            i18n::format(
                language,
                "{files} selected",
                &[("files", &i18n::files(language, total))],
            )
        }];
    }
    let mut lines: Vec<String> = names.iter().take(NAMED_UNDER_CIRCLE).cloned().collect();
    if total > lines.len() {
        lines.push(i18n::format(
            language,
            "+ {count} more",
            &[("count", &(total - lines.len()).to_string())],
        ));
    }
    lines
}

/// The offer shown over the window, together with what Accept needs to know to answer it: whether files are saved on their own
/// and into which folder. One value, so that the window above the pages can keep it, compare it with the next and redraw only
/// when it changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offered {
    pub offer: Progress,
    /// Every file that waits for an answer, the shown one among them, oldest first: the person ticks the ones they want, and each is
    /// answered on its own, since a file of an offer is a transfer.
    pub files: Vec<Progress>,
    /// How many files wait for an answer, the one shown among them: said in the words, so that ten offered are not one.
    pub waiting: usize,
    pub auto_save: bool,
    pub save_folder: Option<std::path::PathBuf>,
}

/// What is offered to this person now, from the view: the oldest file that waits for an answer, with the settings that decide
/// where it goes. `None` when nothing waits.
pub fn offered_now(view: &View) -> Option<Offered> {
    incoming_offer(&view.transfers).map(|offer| Offered {
        offer: offer.clone(),
        files: view
            .transfers
            .iter()
            .filter(|item| item.acceptable())
            .cloned()
            .collect(),
        waiting: view
            .transfers
            .iter()
            .filter(|item| item.acceptable())
            .count(),
        auto_save: view.settings.auto_save,
        save_folder: view.settings.save_folder.clone(),
    })
}

/// An offer that has no owner behind it, for a screenshot run only (`PEERSUS_OFFER=1`): what a window shows when another device
/// offers a file, so that how it is drawn can be seen without a second device. It is made from the same `Progress` the owner's
/// snapshots become and passes through the same [`incoming_offer`] rule, so what it shows is what a real offer shows.
pub fn sample_offer(now: u64) -> Offered {
    Offered {
        offer: Progress {
            id: "sample".into(),
            name: "holiday-photos.zip".into(),
            folders: Vec::new(),
            bytes: 48 * 1024 * 1024,
            incoming: true,
            from: Some("Alex's laptop".into()),
            peer: "sample-peer".into(),

            percent: 0,
            code: None,
            expires_at: Some(now + u64::from(peersus_core::config::DEFAULT_OFFER_TIMEOUT)),
            saved_as: None,
            phase: Phase::Waiting,
        },
        files: Vec::new(),
        waiting: 1,
        auto_save: false,
        save_folder: None,
    }
}

/// How far a finger may stray from where it touched and still be a tap, in pixels, measured in the WINDOW's coordinates. Past
/// it the person is dragging, which scrolls the page and presses nothing.
pub const TAP_SLOP: f32 = 10.;

/// How long a finger must stay on a circle for the touch to count as meant, in milliseconds. The circle is large and a palm, a sleeve or a
/// brush of the screen lands on it for a moment: that is not a press. Far below [`HOLD_MS`], so that a person who means to tap never notices it.
/// The one rule for every circle (Send and Receive alike), decided in the model on the caller's clock.
pub const MIN_TOUCH_MS: u64 = 30;

/// One pointer gesture in window coordinates. Once dragged, returning to the start is not a tap.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tap {
    began: Option<(f32, f32)>,
}
impl Tap {
    pub fn down(&mut self, window: (f32, f32)) {
        self.began = Some(window);
    }
    pub fn moved(&mut self, window: (f32, f32)) {
        if let Some(start) = self.began
            && (window.0 - start.0).hypot(window.1 - start.1) > TAP_SLOP
        {
            self.cancel();
        }
    }
    pub fn up(&mut self, window: (f32, f32)) -> bool {
        self.moved(window);
        self.began.take().is_some()
    }
    pub fn cancel(&mut self) {
        self.began = None;
    }
}

/// How long a finger must stay down, without straying, for the touch to be a hold and not a tap, in milliseconds.
pub const HOLD_MS: u64 = 500;

/// How long the circle must be held, without straying, to fill and send, in milliseconds. Longer than [`HOLD_MS`], which only tells a tap
/// from a hold: a send is not undone, and a finger that rests on the circle for the time a press takes must not send.
pub const CONFIRM_MS: u64 = 700;

/// How a touch on a control ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// Lifted soon, near where it began: the control's own press.
    Tap,
    /// Kept down for [`HOLD_MS`] and lifted near where it began: the control's long press, and NOT also its press.
    Hold,
    /// Dragged (the page was scrolling), cancelled, or lifted with no touch before it: nothing happened.
    Nothing,
}

/// One touch on a control that has a press and a long press, judged from touching to lifting. It is [`Tap`] and the clock: the same
/// straying rule (a drag is for good, and returning to where it began does not undo it), with the time the finger stayed down given in
/// by the caller, so that it is tried without waiting. Exactly one of the two ends a touch, never both: a hold does not also press,
/// which a toolkit that presses on lifting would otherwise do.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hold {
    gesture: Tap,
    began_at: Option<u64>,
}

impl Hold {
    /// The finger went down at `window`, at `now_ms` on the caller's clock.
    pub fn down(&mut self, window: (f32, f32), now_ms: u64) {
        self.gesture.down(window);
        self.began_at = Some(now_ms);
    }

    /// The finger moved to `window`. Straying past the slop ends the touch for good.
    pub fn moved(&mut self, window: (f32, f32)) {
        self.gesture.moved(window);
    }

    /// How long the finger has been down, in milliseconds, while the touch is still a touch: down, and not strayed past the slop. `None`
    /// when there is no touch, or it was dragged or cancelled. The one place that knows whether a touch is still alive.
    pub fn held_for(&self, now_ms: u64) -> Option<u64> {
        if self.gesture == Tap::default() {
            return None;
        }
        self.began_at.map(|began| now_ms.saturating_sub(began))
    }

    /// Whether the touch has now been down long enough, and has not strayed, to be a hold: what lets the control show that the press has
    /// taken, while the finger is still down.
    pub fn is_holding(&self, now_ms: u64) -> bool {
        self.held_for(now_ms).is_some_and(|held| held >= HOLD_MS)
    }

    /// The finger was lifted at `window`, at `now_ms`. The touch is over either way. A contact shorter than [`MIN_TOUCH_MS`] was not meant (a palm,
    /// a sleeve, a brush of the screen on a large circle) and is nothing; so is a touch whose clock ran backwards, which has no duration to trust.
    pub fn up(&mut self, window: (f32, f32), now_ms: u64) -> Ended {
        let began = self.began_at.take();
        if !self.gesture.up(window) {
            return Ended::Nothing;
        }
        match began.and_then(|began| now_ms.checked_sub(began)) {
            Some(held) if held >= HOLD_MS => Ended::Hold,
            Some(held) if held >= MIN_TOUCH_MS => Ended::Tap,
            _ => Ended::Nothing,
        }
    }

    /// The touch was taken away (the system cancelled it): nothing happened.
    pub fn cancel(&mut self) {
        self.gesture.cancel();
        self.began_at = None;
    }
}

/// The gesture of one finger on a round control of `diameter` pixels, judged from touching to lifting.
///
/// A press is a touch that BEGAN inside the circle, never strayed from where it began by more than [`TAP_SLOP`], and was LIFTED
/// inside the circle. Three things follow, each of which a rule over only the two end points gets wrong:
/// * the control's box is a square, and a touch in one of its four corners is outside the circle and presses nothing;
/// * a swipe that goes out and comes back ends where it began, and is still a drag: once the finger has strayed past the slop
///   the gesture is cancelled FOR GOOD, and coming back does not undo it;
/// * the straying is measured in the window's coordinates, because when the page scrolls the circle moves under the finger
///   and its position inside its own box changes while the finger has not moved at all.
///
/// The position inside the circle's box (`(0, 0)` its top left corner) decides only whether a point is in the circle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Touch {
    diameter: f32,
    gesture: Tap,
    began_at: Option<u64>,
}

impl Touch {
    pub fn new(diameter: f32) -> Self {
        Self {
            diameter,
            gesture: Tap::default(),
            began_at: None,
        }
    }

    fn inside(&self, (x, y): (f32, f32)) -> bool {
        let radius = self.diameter / 2.;
        (x - radius).hypot(y - radius) <= radius
    }

    /// The finger went down: `in_box` is where in the control's box, `window` where in the window, at `now_ms` on the caller's clock.
    pub fn down(&mut self, in_box: (f32, f32), window: (f32, f32), now_ms: u64) {
        if self.inside(in_box) {
            self.gesture.down(window);
            self.began_at = Some(now_ms);
        } else {
            self.gesture.cancel();
            self.began_at = None;
        }
    }

    /// The finger moved to `window`. Straying past the slop ends the gesture for good.
    pub fn moved(&mut self, window: (f32, f32)) {
        self.gesture.moved(window);
    }

    /// The finger was lifted at `in_box` (in the control's box) and `window`, at `now_ms`. Whether it is a press: a touch that stayed inside the
    /// round, did not stray, and was pressed for at least [`MIN_TOUCH_MS`], like a key that has to go down to its actuation point. The gesture is
    /// over either way.
    pub fn up(&mut self, in_box: (f32, f32), window: (f32, f32), now_ms: u64) -> bool {
        let began = self.began_at.take();
        let whole = self.gesture.up(window) && self.inside(in_box);
        whole
            && began
                .and_then(|began| now_ms.checked_sub(began))
                .is_some_and(|held| held >= MIN_TOUCH_MS)
    }

    /// Whether the finger was lifted inside the round without having strayed, whatever the time it stayed: the geometry of a press alone. For a
    /// control that judges the duration itself (the circle of the Send page, by its own clock), so that the minimum is never decided twice.
    pub fn lifted_inside(&mut self, in_box: (f32, f32), window: (f32, f32)) -> bool {
        self.began_at = None;
        self.gesture.up(window) && self.inside(in_box)
    }

    /// The gesture was taken away (the system cancelled it): nothing is pressed.
    pub fn cancel(&mut self) {
        self.gesture.cancel();
        self.began_at = None;
    }

    /// Whether a finger is down inside the round and has not strayed: the touch is alive.
    pub fn is_down(&self) -> bool {
        self.gesture != Tap::default()
    }
}

/// What is known of a folder, from looking at the disk. Said as a value so that the rule below stays pure: the looking
/// is done where the disk is touched, and what it found is what is decided on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Folder {
    /// It is there (or can be made) and can be written to.
    Usable,
    /// It is not: gone, not a folder, or not writable.
    Unusable,
}

/// Where an incoming file is to be saved, decided from what the person set and what the disk says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    /// Save it here, without asking.
    Save(std::path::PathBuf),
    /// Ask where. `lost` is the folder that was chosen and can no longer be used, when that is why: the person is told so,
    /// and not only asked, because the place they chose is not the place the file would have gone to.
    Ask { lost: Option<std::path::PathBuf> },
}

/// The rule. Saving on its own is a choice the person makes in Settings, where the folder is shown and set by them, and it is
/// OFF until they do. There is NO default folder: a place nobody chose and nobody can see is where a file goes missing.
///
/// * Off: every file asks where it goes.
/// * On, with a folder chosen that can be used: the file goes there.
/// * On, with a folder chosen that cannot be used any more: the file asks, and the lost folder is named so that the window can
///   say that the place they chose is not where the file would have gone. It is never replaced by some other folder.
/// * On, with no folder chosen: there is nowhere to save to on its own, so it asks.
///
/// `chosen_state` is what the disk says of the chosen folder.
pub fn destination(
    auto_save: bool,
    chosen: Option<&std::path::Path>,
    chosen_state: Folder,
) -> Destination {
    if !auto_save {
        return Destination::Ask { lost: None };
    }
    match chosen {
        Some(folder) if chosen_state == Folder::Usable => Destination::Save(folder.to_path_buf()),
        Some(folder) => Destination::Ask {
            lost: Some(folder.to_path_buf()),
        },
        None => Destination::Ask { lost: None },
    }
}

/// What the disk says of `folder`: [`Folder::Usable`] only if it is a real directory (not a link, which could lead anywhere
/// but where the person chose) in which a file can really be made. The permission bits are not trusted, since they say
/// nothing of a read-only mount: a file is made in it and removed, which is the only proof that one can be saved there.
pub fn look_at(folder: &std::path::Path) -> Folder {
    let Ok(metadata) = std::fs::symlink_metadata(folder) else {
        return Folder::Unusable;
    };
    if !metadata.is_dir() {
        return Folder::Unusable;
    }
    let probe = folder.join(format!(".peersus-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Folder::Usable
        }
        Err(_) => Folder::Unusable,
    }
}

fn device(
    peer: &Peer,
    picture: Option<Digest>,
    link_test: Option<peersus_core::model::LinkTest>,
    typed_address: Option<String>,
) -> Device {
    Device {
        id: peer.id.0.clone(),
        name: peer.name.clone(),
        presence: match peer.presence() {
            peersus_core::model::Presence::Online => Presence::Online,
            peersus_core::model::Presence::Sleeping => Presence::Sleeping,
            peersus_core::model::Presence::Offline => Presence::Offline,
        },
        grants: peer.grants.clone(),
        picture,
        paired_at: peer.paired_at,
        last_seen: peer.last_seen,
        link_test,
        typed_address,
    }
}

/// What a person is told when the connection to a paired device was tested, in their language: a title, what to do about it, and how
/// serious it is. One of the four results, and for the one that says nothing answered, the advice depends on whether the internet relays
/// are on: they are what lets two devices on different networks find each other, and without them a network that does not let a
/// connection in (a phone on a mobile network, most of all) cannot be reached at all, whatever address is known. So the advice there is to
/// turn them on, and never to look for an address, which would not help.
///
/// Plain words and no promise: a test says what happened just now, and a device that answered once may not answer a minute later.
pub fn link_test_words_in(
    language: Language,
    test: peersus_core::model::LinkTest,
    relays_on: bool,
) -> (String, String, Tone) {
    use peersus_core::model::LinkTest;
    let say = |title: &'static str, advice: &'static str, tone| {
        (
            i18n::translate(language, title).to_owned(),
            i18n::translate(language, advice).to_owned(),
            tone,
        )
    };
    match test {
        LinkTest::Trying => say(
            "Trying to reach the device",
            "This can take up to half a minute.",
            Tone::Going,
        ),
        LinkTest::Paired => say(
            "It answers, and it knows this device",
            "The connection works. Nothing needs to be done.",
            Tone::Good,
        ),
        LinkTest::NeedsPairing => say(
            "It answers, but it no longer knows this device",
            "Forget it here and pair the two devices again. Nothing else is wrong.",
            Tone::Bad,
        ),
        LinkTest::Unreachable if relays_on => say(
            "It does not answer",
            "It may be off or out of reach. Try again in a moment. The relays are on, so a change of network is not the cause.",
            Tone::Bad,
        ),
        LinkTest::Unreachable => say(
            "It does not answer",
            "The relays are off, so devices on different networks cannot find each other. Turn on \"Use internet relays\" in Settings.",
            Tone::Bad,
        ),
    }
}

/// The words of the overlay where a person says where a paired device can be found, in their language. One place, so the window only draws
/// them. What it says under the field is true and limited on purpose: an address helps a device that can be reached AT that address (on the
/// same network, or one with an address that does not change), and it cannot help one behind a network that does not let a connection in, so
/// it says that too, and that the relays are what do. A name is worth more than a number, because it is looked up again each time.
pub struct AddressWords {
    pub title: String,
    pub placeholder: String,
    pub explanation: [String; 4],
    pub save: String,
    pub clear: String,
    pub close: String,
}

pub fn address_words_in(language: Language) -> AddressWords {
    let say = |key: &'static str| i18n::translate(language, key).to_owned();
    AddressWords {
        title: say("Where is this device?"),
        placeholder: say("192.168.1.20:5000 or name.example.org:5000"),
        explanation: [
            say(
                "Write where this device can be found when the address it was paired at has changed.",
            ),
            say(
                "A name is better than a number: it is looked up again every time, so it keeps working when the number changes.",
            ),
            say(
                "The port is shown on the other device, in Settings. Without a port, the one it was last seen at is used.",
            ),
            say(
                "This only helps if the device can be reached at that address. A phone on a mobile network usually cannot, and then only the relays connect it.",
            ),
        ],
        save: say("Save and test"),
        clear: say("Remove the address"),
        close: say("Close"),
    }
}

/// When a device was last seen, in words, given the time now (both in Unix seconds). Said as how long ago, in the
/// largest unit that is at least one, so that it reads at a glance: "just now", "5 minutes ago", "3 days ago". A
/// device never seen says so, and one seen in the future (the two clocks disagree) is "just now" and not a negative
/// time. `now` is given and not read here, so that this is tried without waiting for a clock.
pub fn seen_words(last_seen: Option<u64>, now: u64) -> String {
    seen_words_in(Language::English, last_seen, now)
}

pub fn seen_words_in(language: Language, last_seen: Option<u64>, now: u64) -> String {
    let Some(seen) = last_seen else {
        return i18n::translate(language, "never seen").to_owned();
    };
    let elapsed = now.saturating_sub(seen);
    let (count, unit) = match elapsed {
        0..=59 => return i18n::translate(language, "just now").to_owned(),
        60..=3_599 => (elapsed / 60, TimeUnit::Minute),
        3_600..=86_399 => (elapsed / 3_600, TimeUnit::Hour),
        86_400..=2_591_999 => (elapsed / 86_400, TimeUnit::Day),
        2_592_000..=31_535_999 => (elapsed / 2_592_000, TimeUnit::Month),
        _ => (elapsed / 31_536_000, TimeUnit::Year),
    };
    i18n::ago(language, count, unit)
}

/// How long an offer has left, in whole seconds, at `now` (the same clock, Unix seconds). `None` when it has no deadline; `0`
/// once it has passed, never a negative count. Both are whole seconds, so there is nothing to round: it says `0` only when the
/// deadline is now or gone.
pub fn time_left(expires_at: Option<u64>, now: u64) -> Option<u64> {
    expires_at.map(|end| end.saturating_sub(now))
}

/// The time left as it is read on a button or in a circle: `1:05`, `0:09`. Minutes are not capped, so an hour is `60:00`.
pub fn clock_words(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

pub(crate) fn progress(transfer: &Transfer) -> Progress {
    let percent = match (transfer.bytes, &transfer.state) {
        (_, TransferState::Complete) => 100,
        // Nothing to move: it is as far along as it can be, not NaN.
        (0, _) => 100,
        // Widened so a transfer of terabytes cannot overflow the product, and clamped so a
        // peer that reports more than it advertised cannot draw past the end.
        (total, _) => {
            (u128::from(transfer.completed_bytes) * 100 / u128::from(total)).min(100) as u8
        }
    };
    Progress {
        id: transfer.id.clone(),
        name: transfer.name.clone(),
        folders: transfer.folders.clone(),
        bytes: transfer.bytes,
        incoming: transfer.incoming,
        from: transfer.peer_name.clone(),
        peer: transfer.peer.0.clone(),

        percent,
        code: transfer.code,
        expires_at: transfer.expires_at,
        saved_as: transfer.saved_as.clone(),
        phase: match &transfer.state {
            TransferState::Offered => Phase::Waiting,
            TransferState::Running => Phase::Moving,
            TransferState::Complete => Phase::Done,
            TransferState::Cancelled => Phase::Cancelled,
            TransferState::Failed { reason } => Phase::Failed(reason.clone()),
        },
    }
}

/// A size in words: bytes below a kilobyte, then one decimal of the unit, so that the line of a file says how big it is at a glance.
pub fn size_in_words(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// How many finished exchanges a device's own page names. A device that has had more shows its latest and says how many more there
/// are, so what is read there is bounded and never a list that grows for ever.
pub const MOST_UNDER_DEVICE: usize = 5;

/// What passed with the device `peer` that is over, newest first, from the ONE list of transfers (the same that Send, Receive and
/// Settings read, so that forgetting it there forgets it here). Matched on the identity of the device and never on the name it
/// gave itself, which two devices can share and anyone can set. A transfer refused before it was ever shown carries no name and
/// says nothing a person would read, so it is not one.
pub fn history_with<'a>(peer: &str, transfers: &'a [Progress]) -> Vec<&'a Progress> {
    // The list keeps transfers in the order they were made, so the newest is the last: no time is invented to sort them by.
    transfers
        .iter()
        .rev()
        .filter(|item| item.peer == peer && item.phase.is_over() && !item.name.is_empty())
        .collect()
}

/// The part of [`history_with`] a device's page names, and how many more there are than it names.
pub fn history_shown<'a>(peer: &str, transfers: &'a [Progress]) -> (Vec<&'a Progress>, usize) {
    let mut all = history_with(peer, transfers);
    let more = all.len().saturating_sub(MOST_UNDER_DEVICE);
    all.truncate(MOST_UNDER_DEVICE);
    (all, more)
}

/// One line of the history under a device: which way the file went, its name and how it ended.
pub fn history_line(item: &Progress) -> String {
    history_line_in(Language::English, item)
}

pub fn history_line_in(language: Language, item: &Progress) -> String {
    let way = if item.incoming { "Received" } else { "Sent" };
    let state = match &item.phase {
        Phase::Done => "done",
        Phase::Cancelled => "cancelled",
        Phase::Failed(_) => "failed",
        // Only what is over is ever a line of history; a transfer still going is not asked for here.
        Phase::Waiting | Phase::Moving => "going",
    };
    format!(
        "{} \u{b7} {} \u{b7} {}",
        i18n::translate(language, way),
        item.name,
        i18n::translate(language, state)
    )
}

/// What the window says of the hand-overs that finished since it last looked, given what it said before. A file that could not be
/// put in the folder the person chose is kept in the program's own folder, and they are told so; the line stays until the window
/// is closed, and a later success of ANOTHER file never takes it away, since that file is still where the person did not ask.
pub fn delivery_note(before: Option<String>, results: &[(String, bool)]) -> Option<String> {
    if results.iter().any(|(_, done)| !done) {
        Some("A received file could not be put in the folder you chose. It is kept in the app's own folder.".to_owned())
    } else {
        before
    }
}

/// The devices that have finished exchanges in the list but are not saved: those met by a code and never paired, one entry per
/// IDENTITY with the name it last gave itself as a label, in the order they first appear. They are shown only to say what was sent
/// and received; being in this list gives a device no permission and does not make it a saved device. A device is here only while
/// the list holds something over with it, so when that is forgotten (Clear) the device is no longer here.
pub fn unpaired_with_history<'a>(
    devices: &[Device],
    transfers: &'a [Progress],
) -> Vec<(&'a str, Option<&'a str>)> {
    let mut seen: Vec<(&str, Option<&str>)> = Vec::new();
    for item in transfers
        .iter()
        .filter(|item| item.phase.is_over() && !item.name.is_empty())
    {
        if devices.iter().any(|device| device.id == item.peer)
            || seen.iter().any(|(peer, _)| *peer == item.peer)
        {
            continue;
        }
        seen.push((item.peer.as_str(), item.from.as_deref()));
    }
    seen
}

/// The transfers that are still going or waiting for an answer, which is all the Send page lists: what is over is a memory, and it
/// is kept under the device it was with (see [`history_with`]) and on Receive, not as a list on the page where files are chosen.
pub fn going_on(transfers: &[Progress]) -> Vec<Progress> {
    transfers
        .iter()
        .filter(|item| !item.phase.is_over())
        .cloned()
        .collect()
}

/// Show waiting and running files first, in their original order, then the newest history.
/// This is presentation only: no transfer is dropped or removed from the shared snapshot.
pub fn transfer_list(transfers: &[Progress]) -> impl Iterator<Item = &Progress> {
    transfers
        .iter()
        .filter(|item| !item.phase.is_over())
        .chain(transfers.iter().rev().filter(|item| item.phase.is_over()))
}

/// Short lists keep their natural height, so a wrapped offer never hides its answer buttons.
/// More than three rows use a bounded viewport instead of growing the page with history.
pub fn transfer_list_height(items: usize) -> Option<f32> {
    (items > 3).then_some(360.)
}

/// Which transfers the Transfers tab lists. A filter only chooses; it never removes anything from the record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransferFilter {
    #[default]
    All,
    /// Waiting for an answer, or moving.
    Active,
    Done,
    /// Cancelled or failed: what did not arrive.
    Stopped,
    Incoming,
    Outgoing,
}

impl TransferFilter {
    pub const ALL: [TransferFilter; 6] = [
        Self::All,
        Self::Active,
        Self::Done,
        Self::Stopped,
        Self::Incoming,
        Self::Outgoing,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Active => "Active",
            Self::Done => "Done",
            Self::Stopped => "Stopped",
            Self::Incoming => "Incoming",
            Self::Outgoing => "Outgoing",
        }
    }

    fn admits(self, item: &Progress) -> bool {
        match self {
            Self::All => true,
            Self::Active => !item.phase.is_over(),
            Self::Done => item.phase == Phase::Done,
            Self::Stopped => matches!(item.phase, Phase::Cancelled | Phase::Failed(_)),
            Self::Incoming => item.incoming,
            Self::Outgoing => !item.incoming,
        }
    }
}

/// What the Transfers tab lists: the transfers that pass `filter` and whose file name, the name the other device gave itself, or
/// the device it was with contain `query` (letters of either case count the same; an empty query admits all). Waiting and moving
/// first, then the newest of what is over, as everywhere else. Presentation only: the record is untouched.
pub fn find_transfers<'a>(
    transfers: &'a [Progress],
    query: &str,
    filter: TransferFilter,
) -> Vec<&'a Progress> {
    let needle = query.trim().to_lowercase();
    let matches = |item: &Progress| {
        needle.is_empty()
            || item.name.to_lowercase().contains(&needle)
            || item
                .from
                .as_deref()
                .is_some_and(|from| from.to_lowercase().contains(&needle))
            || item.peer.to_lowercase().starts_with(&needle)
    };
    transfer_list(transfers)
        .filter(|item| filter.admits(item) && matches(item))
        .collect()
}

/// The transfers that are over and not yet put away with X. X only hides what is over, for the person's eyes: the record is kept,
/// and the Transfers tab still lists everything. `put_away` holds the ids that X hid; a transfer that is still going is never hidden,
/// and one that was hidden stays hidden only as long as it is the same transfer (a new one has a new id, so it appears).
pub fn not_put_away<'a>(transfers: &'a [Progress], put_away: &[String]) -> Vec<&'a Progress> {
    transfer_list(transfers)
        .filter(|item| !item.phase.is_over() || !put_away.contains(&item.id))
        .collect()
}

/// The ids of the transfers that were already in the record when the window opened: the history, which Send and Receive never list (the
/// Transfers tab does). Said once, at the start, and kept for as long as the window lives.
pub fn ids_at_start(transfers: &[Progress]) -> Vec<String> {
    transfers.iter().map(|item| item.id.clone()).collect()
}

/// What Send and Receive list of the record: the transfers made since the window opened, and whatever is still going (a transfer that was
/// begun before and has not ended is the person's business now). The history that was already there at the start is not, however long. A
/// transfer keeps its id from the first moment it is listed, so one that appeared after the start stays listed while it goes and when it is over.
pub fn of_this_run(transfers: &[Progress], at_start: &[String]) -> Vec<Progress> {
    transfers
        .iter()
        .filter(|item| !item.phase.is_over() || !at_start.contains(&item.id))
        .cloned()
        .collect()
}

/// How long a finger may be down without the toolkit saying anything of it before it is taken to be gone, in milliseconds. A finger that stays
/// still on a screen says nothing, but nobody keeps one down this long on a control: a touch that was never reported lifted (it was lifted
/// over a layer that took the event, or the sheet that closed under it) must not be counted for ever.
pub const FINGER_GONE_MS: u64 = 30_000;

/// The fingers that are down at this moment, by the number the system gave each, and when each was last heard of. What tells a touch of two
/// fingers from one, for the gesture that brings Settings down. A finger that goes down is counted; one that is lifted or cancelled is not;
/// and one that was never reported lifted is not counted for ever, or every touch after it would be a touch of two fingers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fingers {
    down: Vec<(u64, u64)>,
}

impl Fingers {
    /// A finger went down at `now_ms`. Returns whether this makes a touch of two fingers, and only then: said once, as the second finger arrives.
    pub fn went_down(&mut self, id: u64, now_ms: u64) -> bool {
        // Whatever has not been heard of for too long is gone: it can no longer be one of the two.
        self.down
            .retain(|(_, last)| now_ms.saturating_sub(*last) < FINGER_GONE_MS);
        match self.down.iter_mut().find(|(known, _)| *known == id) {
            // The system gave this number again: it is the same finger, down again, and not a second one.
            Some(entry) => entry.1 = now_ms,
            None => self.down.push((id, now_ms)),
        }
        self.down.len() == 2
    }

    /// A finger was moved, said at `now_ms`: it is still down, and heard of.
    pub fn moved(&mut self, id: u64, now_ms: u64) {
        if let Some(entry) = self.down.iter_mut().find(|(known, _)| *known == id) {
            entry.1 = now_ms;
        }
    }

    /// A finger was lifted or its touch was taken away.
    pub fn went_up(&mut self, id: u64) {
        self.down.retain(|(known, _)| *known != id);
    }

    /// Every finger is gone: the window lost the screen, or a layer took every touch.
    pub fn clear(&mut self) {
        self.down.clear();
    }

    pub fn count(&self) -> usize {
        self.down.len()
    }
}

/// A row of the list under the circle: a file that is listed and has not begun to go (prepared), or a real transfer, with its status. One list
/// from the moment files are chosen, so that it holds the status of each file all the way: prepared, going, done, failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListRow {
    /// Listed, and nothing has begun for it: waiting for the circle.
    Prepared(String),
    /// A transfer of this run, whatever its state.
    Transfer(Progress),
}

/// The rows of the list under the circle: the transfers of this run ([`of_this_run`]) with the file that is going first and what is over
/// after it, newest first ([`transfer_list`]); then the files that are only listed, in the order they were chosen, each only until a transfer
/// of that name has begun (that transfer is its row from then on, so a file is never listed twice). A name that appears more than once among
/// the listed files is matched one transfer at a time, so two files with one name are two rows and not one.
pub fn list_under_circle(listed: &[String], batch: &[Progress]) -> Vec<ListRow> {
    let mut rows: Vec<ListRow> = transfer_list(batch)
        .cloned()
        .map(ListRow::Transfer)
        .collect();
    // A file that is still listed has not been sent: sending empties the listed files. So it is a prepared row, whatever transfers share its name
    // (a file chosen again after it was cancelled is a new one), and the transfers keep their own rows.
    rows.extend(listed.iter().cloned().map(ListRow::Prepared));
    rows
}

/// The ids X puts away now: every transfer that is over. Nothing while one is still going, so that the button cannot confuse a person
/// in the middle of a transfer: it is not offered then (see [`can_put_away`]).
pub fn put_away_now(transfers: &[Progress]) -> Vec<String> {
    if transfers.iter().any(|item| !item.phase.is_over()) {
        return Vec::new();
    }
    transfers.iter().map(|item| item.id.clone()).collect()
}

/// Whether X is offered: something is listed that is over, and nothing is going or waiting.
pub fn can_put_away(transfers: &[Progress], put_away: &[String]) -> bool {
    transfers.iter().all(|item| item.phase.is_over())
        && transfers.iter().any(|item| !put_away.contains(&item.id))
}

/// All the transfers still going taken together, for the big circle: how much there is, how much has moved, and the one that is
/// moving now. A transfer of nothing counts as moved, never a division by zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overall {
    pub files: usize,
    pub bytes: u64,
    pub moved: u64,
    /// Whole percent of all the bytes, 0..=100; 100 when there is nothing to move.
    pub percent: u8,
    /// The name of the file moving now, or the first one still waiting; `None` when nothing is going.
    pub current: Option<String>,
}

pub fn overall(transfers: &[Progress]) -> Overall {
    let going: Vec<&Progress> = transfers
        .iter()
        .filter(|item| !item.phase.is_over())
        .collect();
    let bytes: u64 = going.iter().map(|item| item.bytes).sum();
    let moved: u64 = going
        .iter()
        .map(|item| item.bytes.saturating_mul(u64::from(item.percent)) / 100)
        .sum();
    let percent = if bytes == 0 {
        100
    } else {
        u8::try_from((u128::from(moved) * 100 / u128::from(bytes)).min(100)).unwrap_or(100)
    };
    let current = going
        .iter()
        .find(|item| item.phase == Phase::Moving)
        .or_else(|| going.first())
        .map(|item| item.name.clone());
    Overall {
        files: going.len(),
        bytes,
        moved,
        percent,
        current,
    }
}

/// How the transfers of the current batch (see [`of_this_run`]) stand, for the circle: it is the context of the batch, so it says what is happening
/// to it while it goes and how it ended, and only goes back to asking for files when there is no batch at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchState {
    /// Nothing of this batch is in the record: no transfer made of it yet.
    None,
    /// At least one file is going (or waiting to be accepted): `files` of them, `percent` of the way in all, now `current`.
    Going {
        files: usize,
        percent: u8,
        current: Option<String>,
    },
    /// Every transfer of the batch is over and all arrived: `files` of them.
    Sent { files: usize },
    /// Every transfer of the batch is over and some did not arrive (cancelled or failed): `done` of `files` arrived.
    Stopped { files: usize, done: usize },
}

/// The state of the batch from its outgoing transfers. A file received is never a part of what this person sent, so it is not counted. `files` is how
/// many FILES the batch is ([`batch_files`]): a file sent to two devices is one file, and how each device stands is [`batch_by_device`]'s to say.
pub fn batch_state(batch: &[Progress]) -> BatchState {
    let mine: Vec<&Progress> = batch.iter().filter(|item| !item.incoming).collect();
    if mine.is_empty() {
        return BatchState::None;
    }
    let files = batch_files(batch);
    let going: Vec<&Progress> = mine
        .iter()
        .copied()
        .filter(|item| !item.phase.is_over())
        .collect();
    if !going.is_empty() {
        let all = overall(batch);
        return BatchState::Going {
            files,
            percent: all.percent,
            current: all.current,
        };
    }
    // Every transfer is over. It arrived whole only if every one of them did, whichever device it went to; otherwise it stopped, and `done` is how
    // many files arrived at EVERY device (a file that reached one device and was refused by the other did not arrive whole).
    let devices = batch_by_device(batch);
    let whole = mine.iter().all(|item| item.phase == Phase::Done);
    if whole {
        return BatchState::Sent { files };
    }
    let done = devices
        .iter()
        .map(|device| {
            device
                .transfers
                .iter()
                .filter(|item| item.phase == Phase::Done)
                .count()
        })
        .min()
        .unwrap_or(0);
    BatchState::Stopped { files, done }
}

/// How full the circle is, from 0 to 1, from where the circle stands: the one place that decides it, so that the hold and the sending are the same
/// picture and not two. While a transfer of the batch goes it is how far the batch has come (the same percent the circle says in words), so that
/// the circle fills as the files move; a circle that is held to send is as full as the finger has held (`held`, from [`fill_of`]); and in every
/// other state (nothing to send, a batch that ended, a code) it is empty, so that nothing is left filled in a circle nobody is acting on. A
/// percent that goes past 100 is the whole circle and never more.
pub fn circle_fill(batch: &BatchState, held: f32) -> f32 {
    match batch {
        BatchState::Going { percent, .. } => (f32::from(*percent) / 100.).clamp(0., 1.),
        _ if held.is_finite() => held.clamp(0., 1.),
        _ => 0.,
    }
}

/// How one device stands for the files of the batch: its own transfers, with their own states. A file sent to two devices is two transfers, one for
/// each, and each is done or stopped by itself; they are never merged into one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceBatch {
    /// The device, by the id the owner gave it (`Progress::peer`).
    pub peer: String,
    /// The transfers made for this device, in the order the record has them.
    pub transfers: Vec<Progress>,
}

impl DeviceBatch {
    /// How this device's part stands: going, sent whole, or stopped with how many arrived (see [`batch_state`], the same rule for one device).
    pub fn state(&self) -> BatchState {
        batch_state(&self.transfers)
    }
}

/// The batch split by device, in the order each device first appears. One entry for each device that has a transfer in the batch; the transfers
/// keep the states they have, and nothing is counted twice. A received file is never part of it.
pub fn batch_by_device(batch: &[Progress]) -> Vec<DeviceBatch> {
    let mut devices: Vec<DeviceBatch> = Vec::new();
    for item in batch.iter().filter(|item| !item.incoming) {
        match devices.iter_mut().find(|device| device.peer == item.peer) {
            Some(device) => device.transfers.push(item.clone()),
            None => devices.push(DeviceBatch {
                peer: item.peer.clone(),
                transfers: vec![item.clone()],
            }),
        }
    }
    devices
}

/// How many FILES the batch is, however many devices it went to: the distinct files across every device, by what identifies a file on any of them
/// (its name, its size and its folders). A file sent to two devices is one file, and a device that was never given one of them does not make the
/// count smaller; so the circle says the same number before and after the owner answers.
pub fn batch_files(batch: &[Progress]) -> usize {
    let mut seen: Vec<(&str, u64, &[String])> = Vec::new();
    for item in batch.iter().filter(|item| !item.incoming) {
        let key = (item.name.as_str(), item.bytes, item.folders.as_slice());
        if !seen.contains(&key) {
            seen.push(key);
        }
    }
    seen.len()
}

#[cfg(test)]
mod by_device_tests {
    use super::*;

    fn one(id: &str, name: &str, peer: &str, phase: Phase) -> Progress {
        Progress {
            id: id.into(),
            name: name.into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: peer.into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    /// Three files sent to two devices: six transfers, as the owner makes them.
    fn three_files_two_devices() -> Vec<Progress> {
        let mut all = Vec::new();
        for (device, offset) in [("laptop", 0), ("phone", 10)] {
            for n in 0..3 {
                all.push(one(
                    &format!("{device}-{n}"),
                    &format!("f{n}.bin"),
                    device,
                    Phase::Moving,
                ));
                let _ = offset;
            }
        }
        all
    }

    #[test]
    fn three_files_to_two_devices_are_three_files_and_two_devices_of_three_transfers_each() {
        let batch = three_files_two_devices();
        assert_eq!(
            batch.len(),
            6,
            "the owner makes one transfer per file per device"
        );
        assert_eq!(
            batch_files(&batch),
            3,
            "the circle counted a file once for each device"
        );
        let devices = batch_by_device(&batch);
        assert_eq!(devices.len(), 2);
        assert!(devices.iter().all(|device| device.transfers.len() == 3));
    }

    #[test]
    fn each_device_stands_by_itself_one_can_be_done_while_the_other_stopped() {
        let mut batch = three_files_two_devices();
        for item in batch.iter_mut() {
            item.phase = if item.peer == "laptop" {
                Phase::Done
            } else {
                Phase::Cancelled
            };
        }
        let devices = batch_by_device(&batch);
        assert_eq!(devices[0].peer, "laptop");
        assert_eq!(devices[0].state(), BatchState::Sent { files: 3 });
        assert_eq!(devices[1].peer, "phone");
        assert_eq!(
            devices[1].state(),
            BatchState::Stopped { files: 3, done: 0 }
        );
    }

    #[test]
    fn one_device_alone_is_unchanged_and_no_transfer_means_no_files() {
        let one_device: Vec<Progress> = (0..3)
            .map(|n| one(&format!("a{n}"), &format!("f{n}"), "laptop", Phase::Done))
            .collect();
        assert_eq!(batch_files(&one_device), 3);
        assert_eq!(batch_files(&[]), 0);
        assert!(batch_by_device(&[]).is_empty());
    }

    #[test]
    fn a_received_file_is_never_counted_as_a_file_of_the_batch() {
        let mut incoming = one("in", "a.txt", "laptop", Phase::Moving);
        incoming.incoming = true;
        assert_eq!(batch_files(&[incoming.clone()]), 0);
        assert!(batch_by_device(&[incoming]).is_empty());
    }

    #[test]
    fn the_devices_keep_the_order_they_first_appear_in() {
        let batch = vec![
            one("1", "a", "phone", Phase::Moving),
            one("2", "a", "laptop", Phase::Moving),
            one("3", "b", "phone", Phase::Moving),
        ];
        let peers: Vec<String> = batch_by_device(&batch)
            .into_iter()
            .map(|device| device.peer)
            .collect();
        assert_eq!(peers, ["phone", "laptop"]);
    }
}

/// What the circle says of the batch, as the kicker above, the big words and the line under, in the window's language. `None` when there is no batch
/// How one device's part of the batch is said in the list, in the window's language: the words and whether they are good news, bad news or neither. The
/// same catalog texts the circle uses for the same states, so the list and the circle can never say a thing two ways.
pub fn device_state_words_in(language: Language, state: &BatchState) -> (String, Tone) {
    match state {
        BatchState::None => (String::new(), Tone::Quiet),
        BatchState::Going { percent, .. } => (format!("{percent}%"), Tone::Going),
        BatchState::Sent { files } => (
            format!(
                "{} \u{b7} {}",
                i18n::translate(language, "DONE"),
                i18n::files(language, *files)
            ),
            Tone::Good,
        ),
        BatchState::Stopped { files, done } => (
            format!("{} {done}/{files}", i18n::translate(language, "STOPPED")),
            Tone::Bad,
        ),
    }
}

#[cfg(test)]
mod device_state_words_tests {
    use super::*;

    #[test]
    fn each_state_of_a_device_has_words_and_a_tone_that_says_whether_it_is_good_or_bad() {
        assert_eq!(
            device_state_words_in(Language::English, &BatchState::None),
            (String::new(), Tone::Quiet)
        );
        assert_eq!(
            device_state_words_in(
                Language::English,
                &BatchState::Going {
                    files: 3,
                    percent: 40,
                    current: None
                }
            ),
            ("40%".to_owned(), Tone::Going)
        );
        let (done, tone) = device_state_words_in(Language::English, &BatchState::Sent { files: 3 });
        assert!(done.starts_with("DONE") && done.contains('3'), "{done}");
        assert_eq!(tone, Tone::Good);
        assert_eq!(
            device_state_words_in(
                Language::English,
                &BatchState::Stopped { files: 3, done: 1 }
            ),
            ("STOPPED 1/3".to_owned(), Tone::Bad)
        );
    }
}

/// (the circle then says what it does with what is listed, or asks for files). The same words the circle of a code uses for the same states.
pub fn batch_words_in(language: Language, state: &BatchState) -> Option<(String, String, String)> {
    let text = |key: &'static str| i18n::translate(language, key).to_owned();
    match state {
        BatchState::None => None,
        BatchState::Going {
            files,
            percent,
            current,
        } => Some((
            text("SENDING"),
            format!("{percent}%"),
            match current {
                Some(name) => name.clone(),
                None if *files == 1 => text("Sending 1 file."),
                None => i18n::format(
                    language,
                    "Sending {files}.",
                    &[("files", &files.to_string())],
                ),
            },
        )),
        BatchState::Sent { files } => Some((
            text("DONE"),
            if *files == 1 {
                text("1 SENT")
            } else {
                i18n::format(language, "{count} SENT", &[("count", &files.to_string())])
            },
            if *files == 1 {
                text("1 file arrived.")
            } else {
                i18n::format(
                    language,
                    "{files} arrived.",
                    &[("files", &files.to_string())],
                )
            },
        )),
        BatchState::Stopped { files, done } => Some((
            text("STOPPED"),
            format!("{done}/{files}"),
            i18n::format(
                language,
                "{done} of {files} arrived. The rest was refused, cancelled or failed.",
                &[("done", &done.to_string()), ("files", &files.to_string())],
            ),
        )),
    }
}

/// What the button at the head of the list of transfers on Send and Receive does now. Two different things, never one button doing
/// both: while anything is going it only folds the list into a line (the transfer carries on, nothing is lost, and it opens again with
/// one press); once everything is over it is X, which hides what is over for good from this page (see [`not_put_away`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderAction {
    /// Something is going and the list is open: fold it.
    Minimize,
    /// Something is going and the list is folded: open it.
    Expand,
    /// Nothing is going and there is something over that was not put away: hide it.
    Hide,
}

/// The action the head of the list offers, or `None` when there is nothing to act on: a button that does nothing is not shown.
pub fn header_action(
    transfers: &[Progress],
    put_away: &[String],
    folded: bool,
) -> Option<HeaderAction> {
    if transfers.iter().any(|item| !item.phase.is_over()) {
        return Some(if folded {
            HeaderAction::Expand
        } else {
            HeaderAction::Minimize
        });
    }
    can_put_away(transfers, put_away).then_some(HeaderAction::Hide)
}

/// The line a folded list is worth: how many files are going, how far along they are all together, and the one moving now, with the
/// size in words. `None` when nothing is going, for then there is nothing to fold.
pub fn folded_line(transfers: &[Progress]) -> Option<String> {
    let all = overall(transfers);
    if all.files == 0 {
        return None;
    }
    let files = if all.files == 1 {
        "1 file".to_owned()
    } else {
        format!("{} files", all.files)
    };
    let now = all
        .current
        .map(|name| format!(" · {name}"))
        .unwrap_or_default();
    Some(format!(
        "{files} · {}% of {}{now}",
        all.percent,
        size_in_words(all.bytes)
    ))
}

/// The diameter, in pixels, of the accent disc that grows from the centre of a circle of `disc` pixels while it is held, for a `fill` from 0 to 1:
/// nothing at 0 (and for anything that is not a number), the whole disc at 1 and never more, and in between the share of the way that the
/// finger has held, so that the circle is full at the moment it sends. It is a size and not a count of dots, so that it grows without steps.
pub fn fill_disc(fill: f32, disc: f32) -> f32 {
    if !fill.is_finite() || !disc.is_finite() || fill <= 0. || disc <= 0. {
        return 0.;
    }
    fill.min(1.) * disc
}

/// How full the circle is while it is held, from 0 (just touched) to 1 (full, which sends). Given the touch and the time, never the
/// clock: it is tried without waiting. A touch that strayed (the page was scrolling) or is not there is 0, and so is a circle with
/// nowhere to send (`can_send` false), which cannot fill.
pub fn fill_of(touch: &Hold, now_ms: u64, can_send: bool) -> f32 {
    if !can_send {
        return 0.;
    }
    touch
        .held_for(now_ms)
        .map_or(0., |held| (held as f32 / CONFIRM_MS as f32).clamp(0., 1.))
}

/// Where the circle sends, from what the page already holds: the devices that were touched (as `send_state` has judged them: picked, and every
/// one reachable). The devices keep ONE source of truth, the page's chosen list; this only reads it, so that a touch on a card can never both
/// stage a device and mark it some other way.
///
/// With devices picked they are the destination, and there is no code: the circle is held and sends to them. With none picked the only way left
/// is a code, which is made only when the person touches the circle for it. A device that was picked and is no longer reachable holds everything
/// back (the person is told, see `SendState::ChosenUnavailable`): the files are not sent to the others behind their back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Aim {
    /// One or more devices picked: hold the circle and the files go to them.
    Devices(Vec<String>),
    /// No device picked: a touch on the circle makes a code, and only then.
    Code,
    /// A picked device went away: nothing goes anywhere until the person picks again.
    Held,
}

pub fn aim_of(state: &SendState) -> Aim {
    match state {
        SendState::Ready(devices) => Aim::Devices(devices.clone()),
        SendState::ByCode => Aim::Code,
        SendState::ChosenUnavailable => Aim::Held,
    }
}

/// What the big circle on Send does at this moment, which is also what it says. One place decides it, so that the words in the circle and
/// what a touch on it does can never disagree. The circle is the only control: there is no other button to send or to ask for a code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircleDoes {
    /// No files are listed: a touch opens the picker.
    ChooseFiles,
    /// Files are listed and no device is picked: a touch makes a code, when the person touches it for one.
    MakeCode,
    /// Files are listed and one or more devices are picked: holding fills the circle, and when it is full the files go to them.
    HoldToSend,
    /// The same, with Send instantly on: a touch sends at once.
    TapToSend,
    /// Files are listed and a device that was picked went away: nothing happens until the person picks again.
    Held,
}

/// What the circle does, from whether files are listed, where they would go ([`aim_of`]) and whether the person turned Send instantly on.
pub fn circle_does(files_listed: bool, aim: &Aim, send_instantly: bool) -> CircleDoes {
    match (files_listed, aim, send_instantly) {
        (false, _, _) => CircleDoes::ChooseFiles,
        (true, Aim::Held, _) => CircleDoes::Held,
        (true, Aim::Code, _) => CircleDoes::MakeCode,
        (true, Aim::Devices(_), false) => CircleDoes::HoldToSend,
        (true, Aim::Devices(_), true) => CircleDoes::TapToSend,
    }
}

/// What the circle says in each of its states: the kicker above, the big words, and the line under, in the window's language. The same
/// [`CircleDoes`] that decides what a touch does decides this, so the words and the touch cannot disagree. `devices` is how many are picked.
pub fn circle_does_words_in(
    language: Language,
    does: CircleDoes,
    devices: usize,
) -> (String, String, String) {
    let text = |key: &'static str| i18n::translate(language, key).to_owned();
    let who = || {
        if devices > 1 {
            i18n::format(
                language,
                "{count} DEVICES",
                &[("count", &devices.to_string())],
            )
        } else {
            text("1 DEVICE")
        }
    };
    match does {
        CircleDoes::ChooseFiles => (
            text("THE TRANSFER PORTAL"),
            text("CHOOSE FILES"),
            text("TO SEND"),
        ),
        CircleDoes::MakeCode => (text("NO DEVICE PICKED"), text("TAP FOR"), text("A QR CODE")),
        CircleDoes::HoldToSend => (
            text("HOLD TO SEND"),
            who(),
            text("keep holding until it is full"),
        ),
        CircleDoes::TapToSend => (text("TAP TO SEND"), who(), text("it sends at once")),
        CircleDoes::Held => (
            text("A DEVICE WENT AWAY"),
            text("PICK AGAIN"),
            text("nothing is sent until you do"),
        ),
    }
}

/// How long the circle is held to send: [`CONFIRM_MS`]. With Send instantly on there is no holding at all, and a touch that is lifted
/// sends (see [`sends_on_lift`]); this is then not used, and says nothing is waited for.
pub fn confirm_ms(send_instantly: bool) -> u64 {
    if send_instantly { 0 } else { CONFIRM_MS }
}

/// Whether the circle is full: the hold was long enough, and the touch was not a drag, and the circle is for holding (devices picked, not instant).
/// What sends while the finger is still down. Decided on whole milliseconds, not on the float the drawing uses, so that rounding cannot send
/// early or never. The files themselves are not here: the page holds them and sends that selection once, to the devices of the [`Aim`].
pub fn is_full(touch: &Hold, now_ms: u64, does: CircleDoes) -> bool {
    does == CircleDoes::HoldToSend
        && touch
            .held_for(now_ms)
            .is_some_and(|held| held >= CONFIRM_MS)
}

/// Whether lifting the finger sends, with Send instantly on: the touch was a whole tap (down and up near the same place, short of a hold) and
/// the circle is for tapping to send.
pub fn sends_on_lift(ended: Ended, does: CircleDoes) -> bool {
    does == CircleDoes::TapToSend && ended == Ended::Tap
}

/// What a touch on the circle asks the page to do. The page only carries it out; every decision is here, on the caller's clock, so that it is
/// tried without a screen and without waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircleAct {
    /// Nothing: a drag, a touch outside the round, a lift too early, or a send that is held back.
    Nothing,
    /// The circle's own press: choose files.
    Choose,
    /// Make a code for the files that are listed. Said by a whole tap, and only when no device is picked.
    MakeCode,
    /// Send what is listed to the devices picked. Said once for a touch.
    Send,
}

/// One finger on the circle: the gesture that only counts inside the round ([`Touch`]) and the clock of how long it stayed ([`Hold`]), as one
/// thing. Holding fills the circle and sends the moment it is full, without waiting for the lift; with Send instantly on a whole tap sends
/// when it is lifted; with no device picked a whole tap makes a code. A touch does one thing at most.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CircleTouch {
    round: Touch,
    time: Hold,
    sent: bool,
    /// When the system last said anything of this finger, on the caller's clock: what shows it is still there.
    heard_at: Option<u64>,
    /// Whether this touch is a pointer whose release always reaches the circle (a mouse), which is never taken to be gone for being silent.
    steady: bool,
}

/// How long a finger on the circle may go without a sign from the system before it is taken to be gone, in milliseconds. A resting finger
/// still trembles and the toolkit says so, many times a second; this is far longer than the gap between two such signs, and far shorter than the
/// hold (so a finger lifted without the circle being told sends nothing), and the page renews it on every sign it gets.
pub const FINGER_LIVE_MS: u64 = 400;

impl CircleTouch {
    pub fn new(diameter: f32) -> Self {
        Self {
            round: Touch::new(diameter),
            time: Hold::default(),
            sent: false,
            heard_at: None,
            steady: false,
        }
    }

    /// The finger went down: `in_box` is where in the circle's box, `window` where in the window, at `now_ms` on the one clock of this touch.
    /// Outside the round it is not a touch at all, whatever the box's corners are.
    pub fn down(&mut self, in_box: (f32, f32), window: (f32, f32), now_ms: u64) -> bool {
        // A second finger down while the first is alive is not a new touch: the first goes on as it was (its `sent` and its clock are not wiped),
        // or a circle that is being held would start again, and be able to send again, for a finger that only touched it beside the first.
        if self.is_down() && !self.silent_for_too_long(now_ms) {
            return false;
        }
        self.sent = false;
        self.steady = false;
        self.heard_at = Some(now_ms);
        self.round.down(in_box, window, now_ms);
        if self.round.is_down() {
            self.time.down(window, now_ms);
        } else {
            self.time.cancel();
        }
        self.is_down()
    }

    /// Whether a finger that is taken to be down has gone without a sign for longer than a finger may (see [`FINGER_LIVE_MS`]): its lift never reached
    /// the circle, so a new down is a new touch and not a second finger.
    fn silent_for_too_long(&self, now_ms: u64) -> bool {
        !self.steady
            && self
                .heard_at
                .is_some_and(|heard| now_ms.saturating_sub(heard) > FINGER_LIVE_MS)
    }

    /// The finger moved. Straying past the slop ends the touch for good, for the round and for the clock alike.
    pub fn moved(&mut self, window: (f32, f32)) {
        self.round.moved(window);
        self.time.moved(window);
    }

    /// How far the circle is filled, from 0 to 1, at `now_ms`. Only a circle that is held fills, and one that has sent is empty.
    pub fn fill(&self, now_ms: u64, does: CircleDoes) -> f32 {
        if self.sent {
            0.
        } else {
            fill_of(&self.time, now_ms, does == CircleDoes::HoldToSend)
        }
    }

    /// The clock moved on while the finger is down: whether the circle is now full and sends. Said once; the lift then sends nothing more.
    pub fn tick(&mut self, now_ms: u64, does: CircleDoes) -> CircleAct {
        // A finger that has not been heard of for [`FINGER_LIVE_MS`] is not known to be there: its lift may never have reached the circle, and a
        // circle that went on filling for it would send, on its own, files nobody was holding. The touch is over, and sends nothing.
        if self.is_down()
            && !self.steady
            && self
                .heard_at
                .is_some_and(|heard| now_ms.saturating_sub(heard) > FINGER_LIVE_MS)
        {
            self.cancel();
            return CircleAct::Nothing;
        }
        if !self.sent && is_full(&self.time, now_ms, does) {
            self.sent = true;
            return CircleAct::Send;
        }
        CircleAct::Nothing
    }

    /// Whether a finger is down on the circle and has not strayed: what the page runs its clock for.
    pub fn is_down(&self) -> bool {
        self.time.held_for(0).is_some()
    }

    /// Whether the finger has stayed down, unstrayed, for the time of a hold (see [`HOLD_MS`]) at `now_ms`: from then on the circle owns the
    /// touch and the page under it stops following the finger. A touch that was a swipe, was lifted or never began is not.
    pub fn is_holding(&self, now_ms: u64) -> bool {
        self.time.is_holding(now_ms)
    }

    /// The finger was lifted. A whole tap with nothing listed opens the picker; a whole tap with files listed and no device picked makes a code;
    /// with Send instantly on a whole tap sends to the devices picked. A touch that already sent, was a drag, or was a hold that did not fill does
    /// nothing.
    pub fn up(
        &mut self,
        in_box: (f32, f32),
        window: (f32, f32),
        now_ms: u64,
        does: CircleDoes,
    ) -> CircleAct {
        let sent = std::mem::take(&mut self.sent);
        let inside = self.round.lifted_inside(in_box, window);
        let ended = self.time.up(window, now_ms);
        if sent || !inside {
            return CircleAct::Nothing;
        }
        match does {
            CircleDoes::ChooseFiles if ended == Ended::Tap => CircleAct::Choose,
            CircleDoes::MakeCode if ended == Ended::Tap => CircleAct::MakeCode,
            _ if sends_on_lift(ended, does) => CircleAct::Send,
            _ => CircleAct::Nothing,
        }
    }

    /// The system took the touch away: nothing happens.
    pub fn cancel(&mut self) {
        self.round.cancel();
        self.time.cancel();
        self.sent = false;
        self.heard_at = None;
        self.steady = false;
    }

    /// This touch is made by a pointer whose release always reaches the circle (a mouse button: it is down or it is up, and `up` follows it, in or out of
    /// the circle). Such a pointer says nothing while it rests, and that is not a sign it is gone: the silence rule ([`FINGER_LIVE_MS`]) is for a finger,
    /// whose lift can be lost, and applied to a mouse it ended every hold after 400 ms of a hand that did not move. Said after `down`, for this touch only.
    pub fn steady(&mut self) {
        if self.is_down() {
            self.steady = true;
        }
    }

    /// A sign of this finger from the system (it went down, it moved, the toolkit said something of it) at `now_ms`: it is still there.
    pub fn seen(&mut self, now_ms: u64) {
        if self.is_down() {
            self.heard_at = Some(now_ms);
        }
    }
}

/// The finished transfers that are to be handed to a folder the person chose, each once: those that came in, arrived whole (so
/// their file is where `saved_as` says) and for which `tree_for` knows a folder. `tree_for` GIVES the folder up when asked (it
/// is taken once), so a transfer already handed over, or one that went to a path, answers nothing and is not handed twice.
/// Returns each as a [`Handoff`](peersus_net::folder_picked::Handoff).
pub fn handoffs(
    transfers: &[Progress],
    mut tree_for: impl FnMut(&str) -> Option<String>,
) -> Vec<peersus_net::folder_picked::Handoff> {
    let mut out = Vec::new();
    for transfer in transfers {
        if !transfer.incoming || transfer.phase != Phase::Done {
            continue;
        }
        let Some(path) = &transfer.saved_as else {
            continue;
        };
        let Some(tree) = tree_for(&transfer.id) else {
            continue;
        };
        let name = path.file_name().map_or_else(
            || transfer.name.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        out.push(peersus_net::folder_picked::Handoff {
            transfer: transfer.id.clone(),
            path: path.clone(),
            name,
            folders: transfer.folders.clone(),
            tree,
        });
    }
    out
}

/// The tabs of the Settings screen, in the order they are listed. The screen opens on the first:
/// an overview of how things stand, before any of the places where they are changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsTab {
    #[default]
    Overview,
    Device,
    Appearance,
    Network,
    Extensions,
    Permissions,
    About,
}

impl SettingsTab {
    /// Every tab, in the order of the list.
    pub const ALL: [SettingsTab; 7] = [
        SettingsTab::Overview,
        SettingsTab::Device,
        SettingsTab::Appearance,
        SettingsTab::Network,
        SettingsTab::Extensions,
        SettingsTab::Permissions,
        SettingsTab::About,
    ];

    /// The name shown in the list.
    pub fn title(self) -> &'static str {
        match self {
            SettingsTab::Overview => "Overview",
            SettingsTab::Device => "Device",
            SettingsTab::Appearance => "Appearance",
            SettingsTab::Network => "Network",
            SettingsTab::Extensions => "Extensions",
            SettingsTab::Permissions => "Permissions",
            SettingsTab::About => "About",
        }
    }
}

/// The most the page can be scrolled: its height less the window it is seen through, never below 0.
pub fn scroll_range(content: f32, viewport: f32) -> f32 {
    (content - viewport).max(0.)
}

/// A row of Settings that the search can lead to: its title and its note as the page words them (in English, the keys of the
/// catalog, so that they are looked for in the person's language), and the tab it is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingEntry {
    pub title: &'static str,
    pub note: &'static str,
    pub tab: SettingsTab,
}

/// The built-in preference labels. Plugin contributions, owner-offered permissions and history
/// are added from live view data by `settings_search`, not maintained in this list.
pub const SETTING_ENTRIES: &[SettingEntry] = &[
    SettingEntry {
        title: "Device name",
        note: "Device name",
        tab: SettingsTab::Device,
    },
    SettingEntry {
        title: "Language",
        note: "Language",
        tab: SettingsTab::Device,
    },
    SettingEntry {
        title: "Interface size",
        note: "Interface size",
        tab: SettingsTab::Device,
    },
    SettingEntry {
        title: "Run in the background",
        note: "Start without opening a window.",
        tab: SettingsTab::Device,
    },
    SettingEntry {
        title: "Notifications",
        note: "On: an offer of a file is shown even when the program is closed.",
        tab: SettingsTab::Device,
    },
    SettingEntry {
        title: "Camera",
        note: "Choose the camera that reads a code, and press Test to see what it shows.",
        tab: SettingsTab::Device,
    },
    SettingEntry {
        title: "Send instantly",
        note: "Off: after choosing where to send, you hold the circle until it fills, so a wrong choice costs nothing. On: touching the circle sends at once.",
        tab: SettingsTab::Device,
    },
    SettingEntry {
        title: "Save incoming files on their own",
        note: "Off: you are asked where each file goes. On: files go straight to the folder below, without asking.",
        tab: SettingsTab::Device,
    },
    SettingEntry {
        title: "Appearance",
        note: "Appearance",
        tab: SettingsTab::Appearance,
    },
    SettingEntry {
        title: "Keep icons when the bar is closed",
        note: "A closed bar shows only its pictures. Off, a closed bar is not shown at all.",
        tab: SettingsTab::Appearance,
    },
    SettingEntry {
        title: "Reduce motion",
        note: "Turn off decorative animation.",
        tab: SettingsTab::Appearance,
    },
    SettingEntry {
        title: "Use internet relays",
        note: "Lets devices connect across networks. Off keeps traffic on this network.",
        tab: SettingsTab::Network,
    },
];

/// Search the current Settings data; the matcher has no installation-specific list.
pub fn search_settings(
    query: &str,
    view: &View,
    language: Language,
) -> Vec<crate::settings_search::Entry> {
    crate::settings_search::search(
        query,
        view,
        language,
        crate::settings_search::Features::default(),
    )
}

/// How well `query` matches `text`, or `None` when it does not. Every character of the query must be found in the text, in order and
/// without regard to case, so that "bkg" finds "Run in the background". A character found at the start of a word or right after the
/// one before it scores more, so that a match that reads as the words scores higher than one scattered across them. An empty query
/// matches nothing: there is no search yet, and a list of everything would not be a result.
pub fn fuzzy_score(query: &str, text: &str) -> Option<u32> {
    let wanted: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if wanted.is_empty() {
        return None;
    }
    let haystack: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let window = wanted.len().saturating_mul(3).saturating_add(8);
    let mut best = None;
    for start in haystack
        .iter()
        .enumerate()
        .filter_map(|(index, c)| (*c == wanted[0]).then_some(index))
    {
        let end = (start + window).min(haystack.len());
        let (mut at, mut score, mut previous) = (start, 0u32, None);
        let mut matched = true;
        for c in &wanted {
            let Some(found) = haystack[at..end]
                .iter()
                .position(|h| h == c)
                .map(|offset| at + offset)
            else {
                matched = false;
                break;
            };
            let starts_word = found == 0 || !haystack[found - 1].is_alphanumeric();
            score += 1
                + u32::from(starts_word) * 3
                + u32::from(previous.is_some_and(|p| p + 1 == found)) * 2;
            previous = Some(found);
            at = found + 1;
        }
        if matched {
            best = best.max(Some(score));
        }
    }
    best
}

/// The group a reader is in at a scroll position (`scrolled` pixels from the top of the content, 0 or more): the last whose top has been reached,
/// with `lead` pixels of the next one already counting as being in it, so that a title that has just come up under the header is the current one.
/// `None` while nothing has been measured. The last group is also the current one once the content cannot be scrolled further (`at_end`), however
/// short it is: a last group that is shorter than the screen would otherwise never be reached.
pub fn current_group(offsets: &[f32], scrolled: f32, lead: f32, at_end: bool) -> Option<usize> {
    if offsets.is_empty() {
        return None;
    }
    if at_end {
        return Some(offsets.len() - 1);
    }
    Some(
        offsets
            .iter()
            .rposition(|top| *top <= scrolled + lead)
            .unwrap_or(0),
    )
}

/// How long a code of this device lives, in seconds: what the owner gives an invitation.
pub const CODE_LIFE_SECONDS: u64 = 300;

/// The share of the life of a code that is left, from 1 (just shown) to 0 (ended), for a code shown at `made_at` that the owner ends at `ends_at`,
/// at `now`: all three in seconds on the same clock. The life is what the owner's own deadline gives it, never a number of the window's, so that
/// a code of any length of life burns down to the moment the owner ends it. A code whose deadline is not after the moment it was shown has no life.
pub fn code_fuse(made_at: u64, ends_at: u64, now: u64) -> f32 {
    let life = ends_at.saturating_sub(made_at);
    if life == 0 {
        return 0.;
    }
    (ends_at.saturating_sub(now) as f32 / life as f32).clamp(0., 1.)
}

/// The number of sparks that fly off the burning tip of a code's fuse.
pub const FUSE_SPARKS: usize = 7;

/// Where the burning tip of the fuse is, for a share `left` of its life that is left (1 whole, 0 gone), on a ring of `radius` centred on
/// (`centre`, `centre`): it begins at the top and burns clockwise, so what is gone is the part the tip has already gone round.
pub fn fuse_tip(left: f32, radius: f32, centre: f32) -> (f32, f32) {
    let turn = std::f32::consts::TAU * (1. - left.clamp(0., 1.));
    (centre + radius * turn.sin(), centre - radius * turn.cos())
}

/// The part of the ring that is still lit, as the two numbers a drawn arc is made from, in degrees as Skia counts them (0 at three o'clock, growing
/// clockwise): where it starts and how far it goes. The fuse burns clockwise from the top, so what is left runs from the tip (where the burning has
/// got to) round to the top again: it starts at the tip and sweeps the share `left` of the whole turn. The same place as [`fuse_tip`] puts the tip,
/// and no other. `None` for a fuse that is gone (nothing to draw) or a number that is not one; a whole fuse sweeps the whole turn.
pub fn fuse_arc(left: f32) -> Option<(f32, f32)> {
    if !left.is_finite() || left <= 0. {
        return None;
    }
    let left = left.min(1.);
    // The top of the ring is -90 degrees. The burnt part is from the top to the tip, so the lit part begins where that ends.
    Some((-90. + 360. * (1. - left), 360. * left))
}

/// Whether the point (`x`, `y`) lies on the disc of radius `disc` about (`centre`, `centre`), or within `margin` of it: the disc holds the code, and
/// nothing of the fuse may be drawn where this is true, so that a spark that flew inwards is dropped and the code is never covered.
pub fn point_on_disc(x: f32, y: f32, centre: f32, disc: f32, margin: f32) -> bool {
    (x - centre).hypot(y - centre) <= disc + margin
}

/// How many particles are born at the tip of the fuse for each frame, and how many frames the longest of them lives: as in the reference, in
/// which four are born for each frame and each fades by its own part of one until it is gone.
pub const FUSE_BORN_PER_FRAME: usize = 4;
pub const FUSE_PARTICLE_FRAMES: u32 = 66;

/// One particle of the sparks, as offsets in pixels from the tip it was born at, its size, its brightness (1 just born, 0 gone), its colour as
/// (red, green, blue) and how many frames ago it was born.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub alpha: f32,
    pub colour: (u8, u8, u8),
    pub age: u32,
}

/// A number between 0 and 1 from `seed`, the same each time: a hash, so that what looks random is a function and can be tested.
fn hashed(seed: u64) -> f32 {
    let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// The live particles at `frame`: the ones born at the tip in each of the frames before it and still not faded out. Each has its own direction over
/// the whole circle, its own speed, a size and one colour of gold, orange, white and red; gravity pulls it down as it goes, and it fades by its
/// own part of one for each frame until it is gone. Everything is a function of the frame number, so that the same frame gives the same
/// particles, and `age` is how many frames ago it was born (0 is this frame's, on the tip).
pub fn fuse_particles(frame: u32) -> Vec<Particle> {
    const GRAVITY: f32 = 0.04;
    const COLOURS: [(u8, u8, u8); 4] =
        [(255, 230, 0), (255, 136, 0), (255, 255, 255), (255, 51, 0)];
    let mut live = Vec::new();
    for age in 0..FUSE_PARTICLE_FRAMES.min(frame + 1) {
        let born = frame - age;
        for child in 0..FUSE_BORN_PER_FRAME {
            let seed = u64::from(born) * 16 + child as u64;
            let angle = hashed(seed) * std::f32::consts::TAU;
            let speed = hashed(seed ^ 0xA5A5) * 2.5 + 0.5;
            let decay = hashed(seed ^ 0x5A5A) * 0.03 + 0.015;
            let size = hashed(seed ^ 0x3C3C) * 2.5 + 1.;
            let alpha = 1. - decay * age as f32;
            if alpha <= 0. {
                continue;
            }
            let colour = COLOURS[(hashed(seed ^ 0xC3C3) * 4.) as usize % 4];
            let t = age as f32;
            live.push(Particle {
                x: angle.cos() * speed * t,
                y: angle.sin() * speed * t + 0.5 * GRAVITY * t * t,
                size,
                alpha,
                colour,
                age,
            });
        }
    }
    live
}

/// How anything that appears on screen appears: it fades in while growing from a little smaller
/// and rising the last few pixels into its place, and it leaves the same way backwards. One
/// definition for the whole application, so that the menu, the tabs, a page and a dialog all move
/// alike and nothing is ever just there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Popup {
    /// From 0 (not there) to 1 (whole).
    pub opacity: f32,
    /// Pixels it is still below its place; 0 once it has arrived.
    pub lift: f32,
    /// Its size as a share of its full size, growing to 1.
    pub scale: f32,
}

impl Popup {
    /// How far below its place a thing starts, in pixels.
    pub const RISE: f32 = 12.;
    /// The share of its size a thing starts from.
    pub const SMALLEST: f32 = 0.85;
    /// How long it takes, in milliseconds.
    pub const DURATION_MS: u64 = 180;

    /// Where it is when the animation is `progress` of the way (0 to 1). The curve starts fast
    /// and ends slowly (cubic ease-out), the way things settle in the physical world.
    pub fn at(progress: f32) -> Popup {
        let t = progress.clamp(0., 1.);
        let eased = 1. - (1. - t).powi(3);
        Popup {
            opacity: eased,
            lift: Self::RISE * (1. - eased),
            scale: Self::SMALLEST + (1. - Self::SMALLEST) * eased,
        }
    }

    /// Whole and in place: what a thing is once it has arrived, or always when motion is reduced.
    pub fn still() -> Popup {
        Popup {
            opacity: 1.,
            lift: 0.,
            scale: 1.,
        }
    }
}

/// The sheet that comes down from the top of a phone's screen and holds the places of Settings. It moves with the same ease as a popup:
/// the sheet is placed by the distance of its top edge from the top of the usable area, and a dark layer under it grows as it comes.
pub struct TopSheet;

impl TopSheet {
    /// The darkest the layer under the sheet gets, as the alpha of black (of 255): the page is still seen under it.
    pub const SCRIM: u8 = 170;

    /// How strongly the page under the open sheet is blurred, in pixels of radius.
    pub const BLUR: f32 = 14.;

    /// The share of the room the sheet takes at most; the rest is the page, which shows under the layer and closes the sheet when pressed.
    pub const SHARE: f32 = 0.9;

    /// Where the top edge of a sheet `height` tall is when it is `progress` (0 to 1) of the way down, in a usable area that begins `top` from
    /// the top of the window: wholly above the area when closed, in its place when open, and never beyond either.
    pub fn top(progress: f32, height: f32, top: f32) -> f32 {
        let eased = 1. - (1. - progress.clamp(0., 1.)).powi(3);
        top - (1. - eased) * height
    }

    /// The alpha of the layer under the sheet at `progress`: nothing when closed, `SCRIM` when open.
    pub fn scrim(progress: f32) -> u8 {
        (f32::from(Self::SCRIM) * progress.clamp(0., 1.)).round() as u8
    }

    /// The height of the sheet in `room` pixels of usable height: most of it, never negative.
    pub fn height(room: f32) -> f32 {
        room.max(0.) * Self::SHARE
    }
}

/// How long a move takes and when a thing that is going away may finally be taken off the screen.
/// A move is a new animation from wherever the thing is now to where it is going (never a
/// reversal, which would start by jumping to the far end), so one interrupted halfway turns round
/// from that very spot, at the same speed, and nothing ever jumps.
pub struct Motion;

impl Motion {
    /// The time to travel the whole way, in milliseconds.
    pub const DURATION_MS: u64 = Popup::DURATION_MS;

    /// The time to go from `from` to `to` (both 0 to 1): in proportion to the distance, so a turn
    /// in the middle is quick and the speed is always the same. None at all with reduced motion.
    pub fn duration_ms(from: f32, to: f32, reduced_motion: bool) -> u64 {
        if reduced_motion {
            return 0;
        }
        (((to - from).abs() * Self::DURATION_MS as f32).round()) as u64
    }

    /// Whether a thing is on the screen: while it is wanted, and for as long as it has not yet
    /// finished going away. Taking it off earlier would make it disappear at once.
    pub fn mounted(wanted: bool, progress: f32) -> bool {
        wanted || progress > 0.
    }
}

impl Motion {
    /// The progress to start the next move from: where the thing is now, as it was last drawn,
    /// kept within its two ends. Never the end the last move was heading for, which would be a jump.
    pub fn start_from(last_drawn: f32) -> f32 {
        last_drawn.clamp(0., 1.)
    }

    /// Whether a thing that is going toward `target` has arrived, to within a hair.
    pub fn arrived(progress: f32, target: f32) -> bool {
        (progress - target).abs() < 1e-3
    }
}

/// What the menu on the settings button offers: shortcuts that open Settings on one of its tabs.
pub const MENU_SHORTCUTS: [SettingsTab; 3] = [
    SettingsTab::Overview,
    SettingsTab::Extensions,
    SettingsTab::About,
];

/// One row of the Permissions tab: one permission the owner says can be granted, what it lets a
/// plugin do, in words a person can read, and whether it is allowed now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRow {
    pub permission: peersus_api::permission::Permission,
    pub description: String,
    pub allowed: bool,
}

/// What a path dropped on the Extensions page is, as little as the rule needs; taken apart from the
/// file system so that the rule is tried without making files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropKind {
    File { length: u64 },
    Folder,
    Other,
    Unreadable,
}

impl DropKind {
    /// What a path is, from the file system. A link is followed: a person who drops a link to a
    /// package means the package.
    pub fn of(path: &std::path::Path) -> Self {
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => Self::File { length: meta.len() },
            Ok(meta) if meta.is_dir() => Self::Folder,
            Ok(_) => Self::Other,
            Err(_) => Self::Unreadable,
        }
    }
}

/// What dropping paths on the Extensions page comes to. There is one way to install, and a package
/// reaches it as a folder or as an archive without the person saying which: the owner looks at the
/// path. So a drop is one path, taken as it is, or nothing at all with the reason in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    /// Nothing was dropped, so there is nothing to do and nothing to say.
    Nothing,
    /// This is the package: send it to be read and shown.
    Take(std::path::PathBuf),
    /// It cannot be one, and this is what to tell the person.
    Refuse(&'static str),
}

impl Install {
    pub const EMPTY: &'static str = "That file is empty, so there is no package in it.";
    pub const UNREADABLE: &'static str =
        "That could not be read: it is gone, or this program may not open it.";
    pub const NOT_A_PACKAGE: &'static str =
        "That is neither a folder nor a file, so it cannot be a package.";
    pub const ONE_AT_A_TIME: &'static str = "Drop one package at a time.";

    /// Judge `paths`, looking at the one that is left with `kind`.
    pub fn of_drop(
        paths: Vec<std::path::PathBuf>,
        kind: impl Fn(&std::path::Path) -> DropKind,
    ) -> Self {
        let mut distinct: Vec<std::path::PathBuf> = Vec::new();
        for path in paths {
            if !distinct.contains(&path) {
                distinct.push(path);
            }
        }
        match distinct.as_slice() {
            [] => Self::Nothing,
            [only] => match kind(only) {
                DropKind::Folder | DropKind::File { length: 1.. } => Self::Take(only.clone()),
                DropKind::File { length: 0 } => Self::Refuse(Self::EMPTY),
                DropKind::Unreadable => Self::Refuse(Self::UNREADABLE),
                DropKind::Other => Self::Refuse(Self::NOT_A_PACKAGE),
            },
            _ => Self::Refuse(Self::ONE_AT_A_TIME),
        }
    }
}

/// What the Permissions tab shows for a plugin and what it sends when a switch is flipped. Pure,
/// so that the rules are tried without a window.
pub struct PermissionRows;

impl PermissionRows {
    /// The rows for `offered`, the permissions the OWNER says `plugin` can be granted in one scope
    /// (for a device, or outright), on if the permission is in `held`. Nothing else is a row: a name
    /// the plugin merely listed in its manifest is not offered by the owner, so the window never
    /// shows a switch the owner would refuse.
    pub fn of(
        plugin: &PluginInfo,
        offered: &[peersus_api::permission::Permission],
        held: &peersus_core::plugins::Grants,
    ) -> Vec<PermissionRow> {
        Self::of_in(crate::i18n::Language::English, plugin, offered, held)
    }

    pub fn of_in(
        language: crate::i18n::Language,
        plugin: &PluginInfo,
        offered: &[peersus_api::permission::Permission],
        held: &peersus_core::plugins::Grants,
    ) -> Vec<PermissionRow> {
        offered
            .iter()
            .map(|permission| PermissionRow {
                permission: permission.clone(),
                description: Self::words_in(language, plugin, permission),
                allowed: held.allows(permission),
            })
            .collect()
    }

    /// What a permission lets a plugin do, written by whoever owns it: the application wrote the
    /// words for its own, and the plugin wrote the words for its own, in its manifest. A permission
    /// nobody described is said to be that, and is not left blank.
    pub fn words(plugin: &PluginInfo, permission: &peersus_api::permission::Permission) -> String {
        Self::words_in(crate::i18n::Language::English, plugin, permission)
    }

    pub fn words_in(
        language: crate::i18n::Language,
        plugin: &PluginInfo,
        permission: &peersus_api::permission::Permission,
    ) -> String {
        if permission.owner == peersus_api::HOST {
            match permission.name.as_str() {
                peersus_app::runtime::PEERS_SEND_CAPABILITY => {
                    return crate::i18n::translate(
                        language,
                        peersus_app::runtime::PEERS_SEND_DESCRIPTION,
                    )
                    .to_owned();
                }
                peersus_app::runtime::PLUGINS_RESTART_CAPABILITY => {
                    return crate::i18n::translate(
                        language,
                        peersus_app::runtime::PLUGINS_RESTART_DESCRIPTION,
                    )
                    .to_owned();
                }
                peersus_app::platform_services::FILES_CAPABILITY => {
                    return crate::i18n::translate(
                        language,
                        peersus_app::platform_services::FILES_DESCRIPTION,
                    )
                    .to_owned();
                }
                peersus_app::platform_services::HTTP_CAPABILITY => {
                    return crate::i18n::translate(
                        language,
                        peersus_app::platform_services::HTTP_DESCRIPTION,
                    )
                    .to_owned();
                }
                peersus_app::platform_services::CLIPBOARD_READ_CAPABILITY => {
                    return crate::i18n::translate(
                        language,
                        peersus_app::platform_services::CLIPBOARD_READ_DESCRIPTION,
                    )
                    .to_owned();
                }
                peersus_app::platform_services::CLIPBOARD_WRITE_CAPABILITY => {
                    return crate::i18n::translate(
                        language,
                        peersus_app::platform_services::CLIPBOARD_WRITE_DESCRIPTION,
                    )
                    .to_owned();
                }
                peersus_app::platform_services::CLIPBOARD_WATCH_CAPABILITY => {
                    return crate::i18n::translate(
                        language,
                        peersus_app::platform_services::CLIPBOARD_WATCH_DESCRIPTION,
                    )
                    .to_owned();
                }
                peersus_app::platform_services::INPUT_CAPABILITY => {
                    return crate::i18n::translate(
                        language,
                        peersus_app::platform_services::INPUT_DESCRIPTION,
                    )
                    .to_owned();
                }
                _ => {}
            }
        } else if permission.owner == plugin.id.as_str()
            && let Some(words) = plugin.manifest.capabilities.get(&permission.name)
        {
            return words.clone();
        }
        crate::i18n::translate(language, "No description was given for this permission.").to_owned()
    }
}

/// What confirming a package waiting to be installed would do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Staging {
    /// Nothing of that name is installed.
    New,
    /// A plugin of that name is installed already, and this would take its place.
    Replaces,
}

impl Staging {
    pub fn of(staged: &PluginInfo, installed: &[PluginInfo]) -> Self {
        if installed.iter().any(|p| p.id == staged.id) {
            Self::Replaces
        } else {
            Self::New
        }
    }

    /// The word on the button that confirms.
    pub fn button(self) -> &'static str {
        match self {
            Self::New => "Install",
            Self::Replaces => "Replace",
        }
    }
}

/// What the one button on a store row does, from the version installed on this device and the version the
/// maintainer approved. Decided here and only here, so a screen never compares versions itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreAction {
    /// Not installed: one click installs it. It starts with nothing allowed.
    Install,
    /// An older version is installed and the approved one is strictly newer: one click replaces it, and what the
    /// old one was granted is forgotten, as for any replacement.
    Update,
    /// The approved version is installed. The only thing to do is remove it.
    Installed,
    /// A NEWER version than the approved one is installed (a development build). It is never offered as an
    /// update, and never silently downgraded: the person can still remove it.
    NewerInstalled,
}

impl StoreAction {
    /// The word on the button, as an English message id for `i18n::tr`. `None` for a row with no install button.
    pub fn button(&self) -> Option<&'static str> {
        match self {
            Self::Install => Some("Install"),
            Self::Update => Some("Update"),
            Self::Installed | Self::NewerInstalled => None,
        }
    }

    /// Whether the row offers removal: only what is installed.
    pub fn can_remove(&self) -> bool {
        matches!(self, Self::Installed | Self::NewerInstalled | Self::Update)
    }

    /// A line under the row that says what pressing the button costs, where that is not obvious. Fixed words.
    pub fn note(&self) -> Option<&'static str> {
        match self {
            Self::Update => Some(
                "Updating forgets what the installed version was allowed to do. You decide again afterwards.",
            ),
            Self::NewerInstalled => Some(
                "A newer version than the approved one is installed. Remove it to install the approved version.",
            ),
            Self::Install | Self::Installed => None,
        }
    }
}

/// One row of the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreRow {
    pub listing: peersus_core::model::StoreListing,
    pub action: StoreAction,
}

impl StoreRow {
    pub fn of(listing: &peersus_core::model::StoreListing) -> Self {
        use peersus_api::package::Version;
        let action = match listing
            .installed_version
            .as_deref()
            .map(|have| (Version::parse(have), Version::parse(&listing.version)))
        {
            None => StoreAction::Install,
            Some((Some(have), Some(offered))) if offered > have => StoreAction::Update,
            Some((Some(have), Some(offered))) if have > offered => StoreAction::NewerInstalled,
            Some((Some(_), Some(_))) => StoreAction::Installed,
            // A version that cannot be compared is never offered as an update: better to miss one than to offer
            // a replacement on a guess.
            Some(_) => StoreAction::Installed,
        };
        Self {
            listing: listing.clone(),
            action,
        }
    }

    pub fn of_installed(
        listing: &peersus_core::model::StoreListing,
        installed: &PluginInfo,
    ) -> Self {
        let mut row = Self::of(listing);
        if matches!(
            installed.standing,
            peersus_core::model::Standing::Broken | peersus_core::model::Standing::Changed
        ) {
            row.action = StoreAction::Update;
        }
        row
    }
}

/// What needs the person's attention among installed extensions, said when the application opens and again
/// whenever it changes: damaged or changed packages, extensions that stopped working, and signed updates.
/// Each line names the extension as `[id]`, so the notice shows it by its installed name.
pub fn extension_health_notices(
    plugins: &[PluginInfo],
    store: &peersus_core::model::StoreState,
    language: peersus_core::config::Language,
) -> Vec<String> {
    let tr = |english| crate::i18n::translate(language, english);
    use peersus_core::model::{PluginStatus, Standing};
    let mut said = Vec::new();
    for plugin in plugins {
        let id = plugin.id.as_str();
        match (plugin.standing, plugin.status) {
            (Standing::Broken | Standing::Changed, _) => said.push(format!(
                "[{id}] {}",
                tr("Damaged or out of date: install it again to repair it")
            )),
            (_, PluginStatus::Failed) => said.push(format!(
                "[{id}] {}",
                tr("Stopped working: update or install it again")
            )),
            _ => {}
        }
    }
    for listing in &store.listings {
        if StoreRow::of(listing).action == StoreAction::Update {
            said.push(format!(
                "[{}] {} {}",
                listing.plugin.as_str(),
                tr("Update available:"),
                listing.version
            ));
        }
    }
    said
}

/// The rows to show for `query`: every listing whose name, author or description contains it, case-insensitively,
/// in the order the signed catalog gave. An empty query shows everything. The same rule the owner's catalog search
/// uses, so the two cannot disagree about what a word finds.
pub fn store_rows(state: &peersus_core::model::StoreState, query: &str) -> Vec<StoreRow> {
    let needle = query.trim().to_lowercase();
    state
        .listings
        .iter()
        .filter(|listing| {
            needle.is_empty()
                || [
                    listing.plugin.as_str(),
                    listing.author.as_str(),
                    listing.description.as_str(),
                ]
                .iter()
                .any(|field| field.to_lowercase().contains(&needle))
        })
        .map(StoreRow::of)
        .collect()
}

/// One extension, whether installed locally, offered by the catalog, or both.
/// Catalog order is retained across installation/removal; local-only entries follow it. Extensions installed in this session
/// (`recent`, newest first) come before all of them, so what was just installed is the first thing seen.
pub struct ExtensionRow<'a> {
    pub id: &'a peersus_core::plugins::PluginId,
    pub installed: Option<&'a PluginInfo>,
    pub listing: Option<&'a peersus_core::model::StoreListing>,
}

pub fn extension_rows<'a>(
    plugins: &'a [PluginInfo],
    store: &'a peersus_core::model::StoreState,
    query: &str,
    recent: &[peersus_core::plugins::PluginId],
) -> Vec<ExtensionRow<'a>> {
    let needle = query.trim().to_lowercase();
    let matches = |row: &ExtensionRow<'_>| {
        needle.is_empty()
            || row.id.as_str().to_lowercase().contains(&needle)
            || row.listing.is_some_and(|listing| {
                [&listing.author, &listing.description]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&needle))
            })
            || row
                .installed
                .and_then(|info| info.package.as_ref())
                .is_some_and(|package| {
                    [&package.name, &package.author, &package.description]
                        .iter()
                        .any(|field| field.to_lowercase().contains(&needle))
                })
    };
    let mut rows = store
        .listings
        .iter()
        .map(|listing| ExtensionRow {
            id: &listing.plugin,
            installed: plugins.iter().find(|info| info.id == listing.plugin),
            listing: Some(listing),
        })
        .chain(
            plugins
                .iter()
                .filter(|info| {
                    !store
                        .listings
                        .iter()
                        .any(|listing| listing.plugin == info.id)
                })
                .map(|info| ExtensionRow {
                    id: &info.id,
                    installed: Some(info),
                    listing: None,
                }),
        )
        .filter(matches)
        .collect::<Vec<_>>();
    // Stable: everything not installed in this session keeps its place.
    rows.sort_by_key(|row| {
        row.installed
            .and_then(|_| recent.iter().position(|id| id == row.id))
            .unwrap_or(usize::MAX)
    });
    rows
}

/// What to say about the store as a whole, as an English message id for `i18n::tr`, from where it has got to. Fixed
/// words: what a download or a package said is never repeated.
pub fn store_notice(state: &peersus_core::model::StoreState) -> Option<&'static str> {
    use peersus_core::model::StoreStatus;
    match state.status {
        StoreStatus::NotConfigured => Some("The extension store is not set up in this build."),
        StoreStatus::Idle => Some("Press Refresh to see the extensions the maintainer approved."),
        StoreStatus::Loading => Some("Working…"),
        StoreStatus::Failed => Some("The store could not complete that. Nothing was changed."),
        StoreStatus::Ready if state.listings.is_empty() => {
            Some("The maintainer has not approved any extension yet.")
        }
        StoreStatus::Ready => None,
    }
}

/// Whether the store accepts a click now: it is configured and not in the middle of something. A second click
/// while one runs would only be refused by the owner, so the button is not offered.
pub fn store_can_act(state: &peersus_core::model::StoreState) -> bool {
    use peersus_core::model::StoreStatus;
    !matches!(
        state.status,
        StoreStatus::NotConfigured | StoreStatus::Loading
    )
}

/// Why the list the person sees is empty, as an English message id for `i18n::tr`, or `None` when it is not. Aware of
/// the search: a search that finds nothing says so, and never claims the maintainer approved nothing, which is a
/// statement about the store and not about the word typed. Only asked when there are no rows, so a notice about the
/// store (loading, failed) never hides a listing that is still on show.
pub fn store_empty(
    state: &peersus_core::model::StoreState,
    rows: &[StoreRow],
    query: &str,
) -> Option<&'static str> {
    if !rows.is_empty() {
        return None;
    }
    // A store that is not set up, loading or failed has its own words, and they come first.
    use peersus_core::model::StoreStatus;
    if !matches!(state.status, StoreStatus::Ready) || state.listings.is_empty() {
        return store_notice(state);
    }
    // Ready, with listings, and no row survived the search: it is the search that found nothing.
    (!query.trim().is_empty()).then_some("No extension matches your search.")
}

/// What the first button of a plugin does, from what its folder allows and what its process is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginMain {
    /// It may run and does: switch it off.
    Disable,
    /// It may run and does not (switched off, stopped or failed): let it run.
    Enable,
    /// Nobody approved it, or it changed since: approve it as it was shown. Carries the digest the owner reported, which is
    /// given back so that what is approved is what was displayed.
    Approve { program: String },
    /// Nothing can be done with it but remove it: it is broken, or there is nothing to approve it by.
    None,
}

/// The buttons a plugin has, from the state it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginActions {
    pub main: PluginMain,
    /// Restarting only means something for a plugin that is running.
    pub can_restart: bool,
}

impl PluginActions {
    /// Removal is always offered and is not part of this: a plugin must never be something that cannot be removed.
    pub fn for_plugin(info: &PluginInfo) -> Self {
        use peersus_core::model::PluginStatus::{Failed, Restarting, Running, Starting, Stopped};
        use peersus_core::model::Standing;
        match info.standing {
            // Nothing of it runs, so there is nothing to switch or restart. It can only be approved, and only if the owner
            // measured it: without a digest there is nothing for the person to have been shown.
            Standing::NeedsApproval | Standing::Changed => Self {
                main: info
                    .program
                    .clone()
                    .map_or(PluginMain::None, |program| PluginMain::Approve { program }),
                can_restart: false,
            },
            Standing::Broken => Self {
                main: PluginMain::None,
                can_restart: false,
            },
            Standing::Disabled => Self {
                main: PluginMain::Enable,
                can_restart: false,
            },
            Standing::Ready => match info.status {
                Running => Self {
                    main: PluginMain::Disable,
                    can_restart: true,
                },
                Starting | Restarting => Self {
                    main: PluginMain::Disable,
                    can_restart: false,
                },
                Stopped | Failed => Self {
                    main: PluginMain::Enable,
                    can_restart: false,
                },
            },
        }
    }
}

/// How a state is to be coloured: the window maps it to its own palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    /// Something the person is asked to decide.
    Ask,
    Bad,
    /// Neither working nor wrong: switched off.
    Quiet,
    /// Something that is going on now and is neither good nor bad yet: a transfer under way.
    Going,
}

/// The word on a plugin's card. What its folder allows comes first, since a plugin that is not allowed to run says so and is
/// not described as merely stopped; only for one that may run does the state of its process speak.
pub fn plugin_word(info: &PluginInfo) -> (&'static str, Tone) {
    use peersus_core::model::PluginStatus::{Failed, Restarting, Running, Starting, Stopped};
    use peersus_core::model::Standing;
    match info.standing {
        Standing::NeedsApproval => ("NEEDS APPROVAL", Tone::Ask),
        Standing::Changed => ("CHANGED", Tone::Bad),
        Standing::Broken => ("BROKEN", Tone::Bad),
        Standing::Disabled => ("OFF", Tone::Quiet),
        Standing::Ready => match info.status {
            Running => ("RUNNING", Tone::Good),
            Starting => ("STARTING", Tone::Good),
            Restarting => ("RESTARTING", Tone::Bad),
            Failed => ("FAILED", Tone::Bad),
            Stopped => ("STOPPED", Tone::Bad),
        },
    }
}

/// One sentence for the card of a plugin that does not run because of what its folder says, and nothing for the rest: why it
/// is not running and what the person can do. Fixed words: nothing read from the plugin is repeated.
pub fn plugin_note(info: &PluginInfo) -> Option<&'static str> {
    use peersus_core::model::Standing;
    match info.standing {
        Standing::NeedsApproval => Some(
            "Nobody approved this plugin, so it is not running. Approving lets it run with what it asks for, listed below.",
        ),
        Standing::Changed => Some(
            "The program of this plugin is not the one that was approved, so it is not running. Approving it again lets this program run, and what it was allowed to do is forgotten.",
        ),
        Standing::Broken => Some(
            "This plugin cannot be used: it is damaged, or its program cannot be run. It can only be removed.",
        ),
        Standing::Ready | Standing::Disabled => None,
    }
}

/// Removal asks twice, because it also forgets everything the plugin was granted: the first press
/// asks, the second, on the same plugin, does it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RemoveAsk {
    #[default]
    None,
    Asking(peersus_core::plugins::PluginId),
}

impl RemoveAsk {
    /// The state after `plugin`'s remove button is pressed, and whether to remove it now.
    pub fn pressed(self, plugin: &peersus_core::plugins::PluginId) -> (RemoveAsk, bool) {
        match self {
            RemoveAsk::Asking(asked) if &asked == plugin => (RemoveAsk::None, true),
            _ => (RemoveAsk::Asking(plugin.clone()), false),
        }
    }

    /// Given up: nothing is removed.
    pub fn cancelled(self) -> RemoveAsk {
        RemoveAsk::None
    }
}

/// An action that forgets something and has no one thing it is about (Clear) asks twice, as removal does: the first press asks,
/// the second does it. Left alone, or given up with the other button, it does nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Confirm {
    #[default]
    Idle,
    Asking,
}

impl Confirm {
    /// The state after the button is pressed, and whether to do the thing now.
    pub fn pressed(self) -> (Confirm, bool) {
        match self {
            Confirm::Asking => (Confirm::Idle, true),
            Confirm::Idle => (Confirm::Asking, false),
        }
    }

    /// Given up: nothing is done.
    pub fn cancelled(self) -> Confirm {
        Confirm::Idle
    }
}

/// The sidebar's two widths and what happens between them. It is always there: it is wide, with
/// the names of the places, or narrow, with only their icons.
pub struct Sidebar;

impl Sidebar {
    /// Width with names, in pixels.
    pub const WIDE: f32 = 248.;
    /// Width with icons only, in pixels: an icon and its padding.
    pub const NARROW: f32 = 56.;

    /// The entries at the top of the sidebar, in order.
    pub const TOP: [SettingsTab; 6] = [
        SettingsTab::Overview,
        SettingsTab::Device,
        SettingsTab::Appearance,
        SettingsTab::Network,
        SettingsTab::Extensions,
        SettingsTab::Permissions,
    ];

    /// How many characters of the device name fit beside the picture in the profile row.
    pub const PROFILE_CHARACTERS: usize = 14;

    /// The entry at the foot of the sidebar, just above the profile.
    pub const FOOT: SettingsTab = SettingsTab::About;

    /// The tab a click on the profile opens: the one where the name shown in it is edited.
    pub const PROFILE: SettingsTab = SettingsTab::Device;

    /// What the sidebar is, from where the person is and the one option. Outside Settings it is
    /// not there, or only its icons if the person asked for that; inside Settings it is expanded,
    /// and it is the same from hidden and from icons alike.
    pub fn state(inside_settings: bool, keep_icons: bool) -> peersus_core::config::Navigation {
        use peersus_core::config::Navigation;
        match (inside_settings, keep_icons) {
            (true, _) => Navigation::Expanded,
            (false, true) => Navigation::Icons,
            (false, false) => Navigation::Hidden,
        }
    }

    /// The sidebar of the screen that is shown. While the device has no name the only thing on screen is the
    /// question that asks for it, and the sidebar is not there at all, whatever page was asked for: it would
    /// stand beside the question with the page's own menu in it, and a way into a page that is not shown.
    ///
    /// On a phone the sidebar inside Settings is its pictures alone and not a panel of names: at the desktop's
    /// width it takes most of the screen, and the page beside it is left too narrow to read. The pictures
    /// are enough to move between the tabs, and the page's own title says where a person is.
    pub fn state_for(
        named: bool,
        inside_settings: bool,
        keep_icons: bool,
        form: Form,
    ) -> peersus_core::config::Navigation {
        use peersus_core::config::Navigation;
        if !named {
            return Navigation::Hidden;
        }
        match Self::state(inside_settings, keep_icons) {
            // A phone has no room beside the page for a bar, even of pictures, so in Settings it has none: the places come down from the
            // top instead. Whoever chose to keep the pictures keeps them.
            Navigation::Expanded if form == Form::Compact && !keep_icons => Navigation::Hidden,
            Navigation::Expanded if form == Form::Compact => Navigation::Icons,
            other => other,
        }
    }

    /// The page that opening `wanted` from the menu leads to: the page itself once the device has a name, and
    /// none while it has not, since the question is all there is until it is answered.
    pub fn page_opened<P>(named: bool, wanted: P) -> Option<P> {
        named.then_some(wanted)
    }

    /// `name` cut to at most `room` characters, with an ellipsis in place of what was cut, and
    /// counted in characters and not bytes, so that an accented letter is never split. A name
    /// that already fits is left as it is.
    pub fn fit_name(name: &str, room: usize) -> String {
        let count = name.chars().count();
        if count <= room {
            return name.to_owned();
        }
        if room == 0 {
            return String::new();
        }
        let mut kept: String = name.chars().take(room - 1).collect();
        kept.push('\u{2026}');
        kept
    }

    /// The letter shown on the profile picture when there is no photo: the first letter of the
    /// name in capitals, or a question mark when the name has none.
    pub fn initial(name: &str) -> char {
        name.trim()
            .chars()
            .next()
            .and_then(|c| c.to_uppercase().next())
            .unwrap_or('?')
    }

    /// The width a state is heading for: nothing when the bar is hidden, the narrow width for the
    /// pictures alone, the full width when it is expanded. The bar is this one width, followed
    /// from wherever it is drawn, so that every move between states is the same kind of move and
    /// expanded is reached from hidden and from the pictures alike.
    pub fn width_for(state: peersus_core::config::Navigation) -> f32 {
        use peersus_core::config::Navigation;
        match state {
            Navigation::Hidden => 0.,
            Navigation::Icons => Self::NARROW,
            Navigation::Expanded => Self::WIDE,
        }
    }

    /// How visible the names are at a given width: not at all up to the pictures' width, fully at
    /// the full width, and in between as it opens, so that a name never sits on a bar too narrow
    /// to hold it.
    pub fn names_at(width: f32) -> f32 {
        ((width - Self::NARROW) / (Self::WIDE - Self::NARROW)).clamp(0., 1.)
    }

    /// How long a move from width `from` to width `to` takes, in milliseconds: in proportion to the
    /// distance, so that a move turned round in the middle is quick and the speed is always the
    /// same. Nothing at all with reduced motion.
    pub fn duration_ms(from: f32, to: f32, reduced_motion: bool) -> u64 {
        Motion::duration_ms(from / Self::WIDE, to / Self::WIDE, reduced_motion)
    }
}

/// How the interface is arranged for the shape of its window. One interface that adapts, not two: the
/// same screens are drawn, and what changes is where the places are and how much of the chrome stays.
/// The shape decides, portrait or landscape, and not a number of pixels: a phone turned on its side is
/// as wide as a small laptop, and a window dragged tall is a phone-shaped window whatever its width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// Landscape or square: the places across the top, the bar beside the page.
    Wide,
    /// Portrait: the places along the bottom, within thumb's reach, and the profile is its picture alone.
    Compact,
}

impl Form {
    /// The form for a window `width` by `height` pixels. Portrait, strictly taller than wide, is compact.
    /// A size that is not yet known (nothing has been laid out, so it is zero or not a number) is wide:
    /// the desktop arrangement is what is shown until the first measurement, so that nothing flashes the
    /// wrong one.
    pub fn of(width: f32, height: f32) -> Self {
        let known = |side: f32| side.is_finite() && side > 0.;
        if known(width) && known(height) && height > width {
            Self::Compact
        } else {
            Self::Wide
        }
    }

    /// The places in the bottom bar, in order: the same four as across the top on a desktop, in the same
    /// order, so that a person finds each where they know it. Send is the first and the screen a person opens
    /// the application on; without it there would be no way back to it from the others. Empty when wide,
    /// where the places are across the top.
    pub fn bottom_places(self) -> &'static [BottomPlace] {
        match self {
            Self::Compact => &[
                BottomPlace::Send,
                BottomPlace::Receive,
                BottomPlace::Peers,
                BottomPlace::Transfers,
            ],
            Self::Wide => &[],
        }
    }

    /// Whether the Send screen is the circle alone: no heading, no sentence beside it. The circle is
    /// already the button that chooses files, and shows the code once there is something to send.
    pub fn send_is_the_circle_alone(self) -> bool {
        self == Self::Compact
    }

    /// What the circle says under its title. A phone is touched and not clicked, and holds no
    /// "computer" to browse.
    pub fn circle_invitation(self) -> &'static str {
        match self {
            Self::Compact => "tap to choose files",
            Self::Wide => "click to browse your computer",
        }
    }

    /// Whether the profile shows its name beside the picture. A compact bar is the picture alone.
    pub fn profile_shows_name(self) -> bool {
        self == Self::Wide
    }

    /// Whether the header has the window's own minimise, maximise and close buttons. A phone has no such
    /// window to shrink or close: the system draws and owns that, so they are not drawn at all there.
    pub fn header_has_window_buttons(self) -> bool {
        self == Self::Wide
    }

    /// Whether the header says "connected" in words. On a phone the coloured dot alone says it, and the
    /// words would only crowd the wordmark and the menu button on a narrow screen.
    pub fn header_says_connected_in_words(self) -> bool {
        self == Self::Wide
    }

    /// Whether the header gives way as the page scrolls (see [`HeaderHide`]). On a phone the screen is short and the bar takes a
    /// twelfth of it, so it goes while the person reads down and returns when they scroll up; on a desktop the room is there and
    /// the bar holds the places and the window's own buttons, so it stays.
    pub fn header_hides_on_scroll(self) -> bool {
        self == Self::Compact
    }

    /// Whether the places of Settings are a sheet that comes down from the top, as on a phone, where a bar at the side takes a quarter of
    /// the width of a screen that is already narrow; the desktop has the room, and keeps its bar.
    pub fn settings_places_come_from_the_top(self) -> bool {
        self == Self::Compact
    }

    /// The space at each side of the header, in pixels: less on a phone, where every pixel is needed.
    pub fn header_side_padding(self) -> f32 {
        match self {
            Self::Compact => 14.,
            Self::Wide => 28.,
        }
    }

    /// The largest the portal may be, from the width it has: its own size, or less if the width is smaller,
    /// never past the width. A width not yet known keeps the portal at its own size.
    pub fn portal_extent(own: f32, width: f32) -> f32 {
        if width.is_finite() && width > 0. {
            own.min(width)
        } else {
            own
        }
    }

    /// The width of the line round the disc, in the toolkit's logical pixels, for a screen that has `scale` device pixels to each: a
    /// whole number of DEVICE pixels, and never fewer than two. A line one logical pixel thick is 3.125 device pixels on some
    /// phones, and it does not stay where it is drawn: at a fraction of a pixel it is spread over four rows and shows at a third of its
    /// colour, and sometimes it is not seen at all, which is a ring that comes and goes as the page above it changes. A scale that is
    /// not a positive number gives the two logical pixels that were measured to hold.
    pub fn portal_line(scale: f64) -> f32 {
        if !(scale.is_finite() && scale > 0.) {
            return 2.;
        }
        let device = (scale * 1.5).round().max(2.);
        (device / scale) as f32
    }

    /// The size of the title of a screen that asks for one thing, in pixels. Smaller on a phone, where the
    /// desktop's size would be wider than the screen and break into lines of single words.
    pub fn ask_title_size(self) -> f32 {
        match self {
            Self::Compact => 36.,
            Self::Wide => 56.,
        }
    }

    /// Whether the field and its button stand one above the other. Side by side they are wider than a phone.
    pub fn ask_stacks_field_and_button(self) -> bool {
        self == Self::Compact
    }

    /// The space left at each side of a screen that asks for one thing, in pixels.
    pub fn ask_side_margin(self) -> f32 {
        match self {
            Self::Compact => 24.,
            Self::Wide => 0.,
        }
    }

    /// The space a page keeps at each side, in pixels. The desktop's is wide, which is what makes a page read
    /// as a page; on a phone the same space would take a third of the screen, so it is small there.
    pub fn page_side_margin(self) -> f32 {
        match self {
            Self::Compact => 20.,
            Self::Wide => 56.,
        }
    }

    /// The space a page keeps above its first line and below its last, in pixels. Every pixel of height on a phone
    /// is wanted for what the page is for, so it is small there; the desktop's is what it was.
    pub fn page_vertical_padding(self) -> f32 {
        match self {
            Self::Compact => 12.,
            Self::Wide => 28.,
        }
    }

    /// The space between the sections of a page, in pixels: the desktop's is what it was, a phone's is closer, so that
    /// the title, the circle and what is under it are seen together without scrolling.
    pub fn page_section_gap(self) -> f32 {
        match self {
            Self::Compact => 16.,
            Self::Wide => 28.,
        }
    }

    /// The width a card of a device in a list keeps, or none. On a desktop cards stand side by side and keep
    /// their own width; on a phone a card of that width is wider than half the screen and the next one would be
    /// pushed off it, so it keeps none and is as wide as the screen, with the cards one under another.
    pub fn device_card_width(self) -> Option<f32> {
        match self {
            Self::Wide => Some(280.),
            Self::Compact => None,
        }
    }

    /// Whether the buttons of a device's card stand one under the other, below the name. Beside the name they take the width the name
    /// needs: on a phone that leaves it a column of one letter, and a name and a state written downwards, a letter to a line. On the
    /// desktop the card is wide enough for all of them in a row, as it always was.
    pub fn device_card_buttons_stack(self) -> bool {
        self == Self::Compact
    }

    /// Whether a page puts the explanation beside the portal or under it. Side by side they need more width
    /// than a phone has, and the explanation is what is left with none.
    pub fn page_puts_portal_beside_text(self) -> bool {
        self == Self::Wide
    }
}

/// The height of the header, in pixels: the one number that both the bar's drawing and the rule that hides it use.
pub const HEADER_HEIGHT: f32 = 72.;

/// How much of the header is hidden as a page scrolls, so that a phone gives its whole screen to what is being read and the bar comes
/// back as soon as the person scrolls up. The bar follows the finger: it goes up by as much as the page went down and comes down by as
/// much as the page went up, between nothing hidden and all of it, so it is never flipped on or off by a single small move.
///
/// Pure, so it is tried with plain numbers and not through a window. `top` is how far the page is scrolled (0 at the very top).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeaderHide {
    /// Where the page was at the last look, in pixels from its top.
    last: f32,
    /// How much of the bar is out of sight now: 0 shown, [`HEADER_HEIGHT`] gone.
    hidden: f32,
}

impl HeaderHide {
    /// The state after the page is at `top` pixels from its start. At the very top the bar is whole whatever came before (there is
    /// nothing above to read), a page that is not scrolled never hides it, and a position that is not a number changes nothing.
    pub fn after(self, top: f32) -> Self {
        if !top.is_finite() {
            return self;
        }
        let top = top.max(0.);
        if top <= 0. {
            return Self {
                last: 0.,
                hidden: 0.,
            };
        }
        // Down is positive: the bar goes up by what the page went down, and comes back by what it went up.
        let moved = top - self.last;
        Self {
            last: top,
            hidden: (self.hidden + moved).clamp(0., HEADER_HEIGHT),
        }
    }

    /// How much of the bar is out of sight, in pixels, from nothing to all of it.
    pub fn hidden(self) -> f32 {
        self.hidden
    }

    /// The height the bar takes now: the page is given what the bar gave up.
    pub fn height(self) -> f32 {
        HEADER_HEIGHT - self.hidden
    }
}

/// The margins the system keeps free around the interface (the status bar above, the navigation bar below, a
/// notch at a side), in the toolkit's logical pixels. Nothing in it is a number written in this program: it is
/// what the system said, divided by the scale factor of the screen it is on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SafeArea {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl SafeArea {
    /// The margins for a screen of `width` by `height` physical pixels of which the system says only the rectangle
    /// `left, top, right, bottom` (physical pixels, `right` and `bottom` exclusive) is for the application,
    /// at `scale` physical pixels to a logical one.
    ///
    /// Nothing is free when the rectangle is empty (the system has not given one yet), when it is not inside
    /// the screen, when it is upside down, or when the scale is not a positive number: a wrong margin would
    /// cut the interface, and no margin cannot.
    pub fn from_physical(rectangle: (i32, i32, i32, i32), screen: (u32, u32), scale: f64) -> Self {
        let (left, top, right, bottom) = rectangle;
        let (width, height) = (screen.0 as i32, screen.1 as i32);
        let usable = scale.is_finite()
            && scale > 0.
            && right > left
            && bottom > top
            && left >= 0
            && top >= 0
            && right <= width
            && bottom <= height;
        if !usable {
            return Self::default();
        }
        let logical = |pixels: i32| (f64::from(pixels) / scale) as f32;
        Self {
            top: logical(top),
            left: logical(left),
            right: logical(width - right),
            bottom: logical(height - bottom),
        }
    }
}

/// A place that has a tab in the bottom bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BottomPlace {
    Send,
    Receive,
    Peers,
    Transfers,
}

/// How large a screen's title is drawn, from the room it has. It is one line, never broken: a title
/// that wraps puts its second line over the first, so it is made smaller instead.
pub struct Headline;

impl Headline {
    /// The size when there is room to spare, in pixels.
    pub const LARGEST: f32 = 76.;
    /// The size below which a title is no longer a title, and is left to be cut.
    pub const SMALLEST: f32 = 34.;
    /// The width of a character of the bold face, as a share of its size, at the widest of the
    /// words used. Measured from the embedded font (4.6 to 5.7 em for ten characters) with a
    /// margin for the bold weight, which is a little wider than the one measured.
    const EM_PER_CHARACTER: f32 = 0.62;

    /// The width, in pixels, a line of `characters` takes at `size`.
    pub fn width_of(characters: usize, size: f32) -> f32 {
        characters as f32 * size * Self::EM_PER_CHARACTER
    }

    /// The largest size, up to [`Headline::LARGEST`], at which a line of `characters` fits in `room`.
    pub fn size_for(room: f32, characters: usize) -> f32 {
        let fitting = room / (characters.max(1) as f32 * Self::EM_PER_CHARACTER);
        fitting.clamp(Self::SMALLEST, Self::LARGEST)
    }

    /// The largest a title may be on a screen of this form. The desktop's is [`Headline::LARGEST`], which is what a
    /// wide screen has room for. On a phone the same size is as wide as the screen, so every title fills it and the
    /// page is mostly title: it is held to a size that leaves the room to what the page is for.
    pub fn largest_in(form: Form) -> f32 {
        match form {
            Form::Wide => Self::LARGEST,
            Form::Compact => 40.,
        }
    }

    /// The size of a title of `characters` in `room`, on a screen of `form`: what fits, held to what the form allows,
    /// and never under [`Headline::SMALLEST`], so that a title is a title and is cut only when nothing else is left.
    pub fn size_in(form: Form, room: f32, characters: usize) -> f32 {
        Self::size_for(room, characters)
            .min(Self::largest_in(form))
            .max(Self::SMALLEST)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_core::model::{PairingPrompt, PeerId};

    #[test]
    fn a_size_is_said_in_the_unit_that_fits_it() {
        assert_eq!(size_in_words(0), "0 B");
        assert_eq!(size_in_words(1023), "1023 B");
        assert_eq!(size_in_words(1024), "1.0 KB");
        assert_eq!(size_in_words(1536), "1.5 KB");
        assert_eq!(size_in_words(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(size_in_words(3 * 1024 * 1024 * 1024), "3.0 GB");
        // Past the last unit the number grows and the unit stays, never a unit that does not exist.
        assert_eq!(size_in_words(2048 * 1024 * 1024 * 1024 * 1024), "2048.0 TB");
    }

    #[test]
    fn the_list_of_an_offer_holds_every_file_that_waits_and_only_those() {
        let waiting = |id: &str| Progress {
            bytes: 10,
            ..row(id, true, Phase::Waiting, None)
        };
        let view = View {
            transfers: vec![
                waiting("a"),
                Progress {
                    ..row("done", true, Phase::Done, None)
                },
                waiting("b"),
                // Went out and waits for the other device to answer: not something this person answers.
                row("out", false, Phase::Waiting, None),
            ],
            ..View::default()
        };
        let offered = offered_now(&view).expect("two files wait");
        let ids: Vec<&str> = offered.files.iter().map(|file| file.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(offered.waiting, 2);
    }

    #[test]
    fn the_fuse_of_a_code_burns_from_whole_to_nothing_over_the_life_the_owner_gave_it() {
        // A code shown at 1000 that the owner ends at 1300: five minutes, as the owner gives them today.
        assert_eq!(code_fuse(1_000, 1_300, 1_000), 1.);
        assert_eq!(code_fuse(1_000, 1_300, 1_150), 0.5);
        assert_eq!(code_fuse(1_000, 1_300, 1_300), 0.);
        // The life is the owner's, not a number of the window's: a code of ten minutes burns over ten, and one of a minute over one.
        assert_eq!(code_fuse(0, 600, 300), 0.5);
        assert_eq!(code_fuse(0, 60, 15), 0.75);
    }

    #[test]
    fn the_fuse_never_goes_below_nothing_nor_above_whole_and_never_divides_by_nothing() {
        // A code long past its end, or a clock that went back before the code was shown: nothing, and a whole, and never outside them.
        assert_eq!(
            code_fuse(1_000, 1_300, 9_999),
            0.,
            "an old code gave a negative share"
        );
        assert_eq!(
            code_fuse(1_000, 1_300, 500),
            1.,
            "a clock that went back gave more than a whole"
        );
        // A deadline that is not after the moment of showing is a code with no life, never a division by zero or a whole fuse.
        assert_eq!(code_fuse(1_000, 1_000, 1_000), 0.);
        assert_eq!(
            code_fuse(1_000, 500, 1_000),
            0.,
            "a deadline before the code was shown gave a fuse"
        );
        // It only ever burns down as the clock goes on.
        let mut last = 1.;
        for now in 1_000..=1_300 {
            let left = code_fuse(1_000, 1_300, now);
            assert!(left <= last, "the fuse grew back at {now}");
            last = left;
        }
    }

    #[test]
    fn the_tip_of_the_fuse_is_at_the_top_when_whole_and_goes_round_clockwise_as_it_burns() {
        let near =
            |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
        // Whole: the tip is where it began, at the top of a ring of radius 100 centred on (100, 100).
        assert!(near(fuse_tip(1., 100., 100.), (100., 0.)));
        // Three quarters left: a quarter has burned, so the tip has gone a quarter round clockwise from the top, to the right.
        assert!(near(fuse_tip(0.75, 100., 100.), (200., 100.)));
        // Half left: at the bottom. A quarter left: at the left.
        assert!(near(fuse_tip(0.5, 100., 100.), (100., 200.)));
        assert!(near(fuse_tip(0.25, 100., 100.), (0., 100.)));
    }

    #[test]
    fn the_lit_arc_starts_exactly_where_the_tip_is_and_sweeps_the_share_that_is_left() {
        let near =
            |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-2 && (a.1 - b.1).abs() < 1e-2;
        // On a ring of radius 100 about (100, 100), the point a Skia angle (0 at three o'clock, clockwise) is at.
        let at = |degrees: f32| {
            let radians = degrees.to_radians();
            (100. + 100. * radians.cos(), 100. + 100. * radians.sin())
        };
        for left in [1., 0.9, 0.75, 0.5, 0.25, 0.1, 0.01] {
            let (start, sweep) = fuse_arc(left).unwrap();
            // The line begins on the tip: the same place the sparks fly from, for any share left.
            assert!(
                near(at(start), fuse_tip(left, 100., 100.)),
                "the arc began away from the tip at {left}"
            );
            assert!(
                (sweep - 360. * left).abs() < 1e-3,
                "it swept {sweep} degrees for a share of {left}"
            );
            // And its end is the top, where the burning began, for every share.
            assert!(
                near(at(start + sweep), (100., 0.)),
                "the arc did not end at the top at {left}"
            );
        }
    }

    #[test]
    fn the_lit_arc_is_the_whole_turn_when_whole_and_nothing_when_gone_or_not_a_number() {
        assert_eq!(fuse_arc(1.), Some((-90., 360.)));
        // More than whole is no more than whole.
        assert_eq!(fuse_arc(7.), Some((-90., 360.)));
        for gone in [0., -0.0, -1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(fuse_arc(gone), None, "{gone} drew a line");
        }
    }

    #[test]
    fn particles_leave_the_tip_in_every_direction_fall_and_fade_until_gone_and_are_the_same_for_the_same_frame()
     {
        let a = fuse_particles(300);
        let b = fuse_particles(300);
        assert_eq!(a, b, "the same frame gave other particles");
        assert!(!a.is_empty());
        for particle in &a {
            assert!(
                (0. ..=1.).contains(&particle.alpha),
                "alpha out of range: {}",
                particle.alpha
            );
            assert!(particle.size > 0.);
            assert!(particle.age < FUSE_PARTICLE_FRAMES);
        }
        // One that was born this very frame is on the tip, whole.
        let newborn: Vec<_> = a.iter().filter(|p| p.age == 0).collect();
        assert_eq!(newborn.len(), FUSE_BORN_PER_FRAME);
        assert!(
            newborn
                .iter()
                .all(|p| p.x == 0. && p.y == 0. && p.alpha == 1.)
        );
        // Over many frames they go off in every direction, not all one way: both sides and both above and below the tip are used.
        let all: Vec<_> = (0..40)
            .flat_map(|frame| fuse_particles(frame + 100))
            .collect();
        assert!(
            all.iter().any(|p| p.x > 5.) && all.iter().any(|p| p.x < -5.),
            "particles went only one way sideways"
        );
        assert!(
            all.iter().any(|p| p.y > 5.) && all.iter().any(|p| p.y < -5.),
            "particles went only one way up and down"
        );
        // Gravity: an older particle is lower than a straight flight would put it.
        assert!(a.iter().filter(|p| p.age > 20).count() > 0);
    }

    #[test]
    fn nothing_of_the_fuse_can_be_drawn_on_the_disc_that_holds_the_code() {
        // A portal of the size of the prototype: the disc is 384 across, the ring of the fuse sits at 418 / 2 less 3.
        let (disc, ring, centre) = (384. / 2., 418. / 2. - 3., 420. / 2.);
        for step in 0..=100 {
            let left = step as f32 / 100.;
            let (tip_x, tip_y) = fuse_tip(left, ring, centre);
            // The tip itself is on the ring, which lies outside the disc by more than the size of the dot drawn on it.
            assert!(
                !point_on_disc(tip_x, tip_y, centre, disc, 8.),
                "the tip lay on the disc at {left}"
            );
            // And of the sparks that are born at it, none that would lie on the disc is let through.
            for frame in [66, 100, 150, 400] {
                for particle in fuse_particles(frame) {
                    let (x, y) = (tip_x + particle.x, tip_y + particle.y);
                    let drawn = !point_on_disc(x, y, centre, disc, 2.);
                    if !drawn {
                        continue;
                    }
                    assert!(
                        (x - centre).hypot(y - centre) > disc,
                        "a spark that is drawn lay on the disc at {left}, frame {frame}"
                    );
                }
            }
        }
    }

    fn transfer(bytes: u64, done: u64, state: TransferState) -> Transfer {
        Transfer {
            id: "t".into(),
            peer: PeerId("p".into()),
            peer_name: None,
            name: "file.bin".into(),
            folders: Vec::new(),
            bytes,
            completed_bytes: done,
            incoming: false,
            state,
            started_at: 0,
            code: None,
            expires_at: None,
            saved_as: None,
        }
    }
    fn percent(bytes: u64, done: u64, state: TransferState) -> u8 {
        progress(&transfer(bytes, done, state)).percent
    }

    /// A transfer with its own id, name and device, for the lists of the tests below.
    fn listed(
        id: &str,
        name: &str,
        peer: &str,
        incoming: bool,
        bytes: u64,
        done: u64,
        state: TransferState,
    ) -> Progress {
        let mut made = transfer(bytes, done, state);
        made.id = id.into();
        made.name = name.into();
        made.peer = PeerId(peer.into());
        made.incoming = incoming;
        progress(&made)
    }
    fn ids(items: &[&Progress]) -> Vec<String> {
        items.iter().map(|item| item.id.clone()).collect()
    }
    fn history() -> Vec<Progress> {
        vec![
            listed(
                "a",
                "Holiday.mp4",
                "peer-laptop",
                false,
                100,
                100,
                TransferState::Complete,
            ),
            listed(
                "b",
                "invoice.pdf",
                "peer-phone",
                true,
                10,
                10,
                TransferState::Complete,
            ),
            listed(
                "c",
                "notes.txt",
                "peer-laptop",
                false,
                5,
                0,
                TransferState::Cancelled,
            ),
            listed(
                "d",
                "backup.zip",
                "peer-phone",
                true,
                50,
                0,
                TransferState::Failed {
                    reason: "lost".into(),
                },
            ),
            listed(
                "e",
                "photo.jpg",
                "peer-phone",
                false,
                40,
                10,
                TransferState::Running,
            ),
        ]
    }

    #[test]
    fn the_transfers_tab_lists_what_is_going_first_then_the_newest_of_what_is_over() {
        let items = history();
        assert_eq!(
            ids(&find_transfers(&items, "", TransferFilter::All)),
            ["e", "d", "c", "b", "a"]
        );
    }

    #[test]
    fn each_filter_keeps_only_its_own_and_none_removes_anything_from_the_record() {
        let items = history();
        assert_eq!(
            ids(&find_transfers(&items, "", TransferFilter::Active)),
            ["e"]
        );
        assert_eq!(
            ids(&find_transfers(&items, "", TransferFilter::Done)),
            ["b", "a"]
        );
        assert_eq!(
            ids(&find_transfers(&items, "", TransferFilter::Stopped)),
            ["d", "c"]
        );
        assert_eq!(
            ids(&find_transfers(&items, "", TransferFilter::Incoming)),
            ["d", "b"]
        );
        assert_eq!(
            ids(&find_transfers(&items, "", TransferFilter::Outgoing)),
            ["e", "c", "a"]
        );
        assert_eq!(items.len(), 5, "a filter changed the record");
    }

    #[test]
    fn every_filter_has_a_label_and_the_list_of_them_is_complete() {
        for filter in TransferFilter::ALL {
            assert!(!filter.label().is_empty());
        }
        // A new filter that is not in ALL could never be chosen: the match below stops compiling when one is added.
        for filter in TransferFilter::ALL {
            match filter {
                TransferFilter::All
                | TransferFilter::Active
                | TransferFilter::Done
                | TransferFilter::Stopped
                | TransferFilter::Incoming
                | TransferFilter::Outgoing => {}
            }
        }
        assert_eq!(TransferFilter::default(), TransferFilter::All);
    }

    #[test]
    fn search_finds_by_file_name_whatever_the_case_and_ignores_the_spaces_around_it() {
        let items = history();
        assert_eq!(
            ids(&find_transfers(&items, "  HOLIDAY ", TransferFilter::All)),
            ["a"]
        );
        assert_eq!(
            ids(&find_transfers(&items, ".pdf", TransferFilter::All)),
            ["b"]
        );
        assert!(find_transfers(&items, "no such file", TransferFilter::All).is_empty());
    }

    #[test]
    fn search_finds_by_the_name_the_sender_gave_and_by_the_start_of_the_device_identity() {
        let mut items = history();
        items[1].from = Some("Maria's phone".into());
        assert_eq!(
            ids(&find_transfers(&items, "maria", TransferFilter::All)),
            ["b"]
        );
        // The identity is matched from its start only: a letter in the middle of one is not a reason to list a device's files.
        assert_eq!(
            ids(&find_transfers(&items, "peer-lap", TransferFilter::All)),
            ["c", "a"]
        );
        assert!(find_transfers(&items, "laptop", TransferFilter::All).is_empty());
    }

    #[test]
    fn search_and_filter_together_keep_only_what_passes_both() {
        let items = history();
        assert_eq!(
            ids(&find_transfers(
                &items,
                "peer-phone",
                TransferFilter::Incoming
            )),
            ["d", "b"]
        );
        assert_eq!(
            ids(&find_transfers(&items, "peer-phone", TransferFilter::Done)),
            ["b"]
        );
        assert!(find_transfers(&items, "holiday", TransferFilter::Incoming).is_empty());
    }

    #[test]
    fn x_puts_away_what_is_over_and_only_when_nothing_is_going() {
        let items = history();
        assert!(
            put_away_now(&items).is_empty(),
            "X put things away in the middle of a transfer"
        );
        assert!(
            !can_put_away(&items, &[]),
            "X was offered in the middle of a transfer"
        );
        let over: Vec<Progress> = items
            .iter()
            .filter(|item| item.phase.is_over())
            .cloned()
            .collect();
        assert!(can_put_away(&over, &[]));
        assert_eq!(put_away_now(&over), ["a", "b", "c", "d"]);
    }

    #[test]
    fn what_x_put_away_is_not_listed_but_is_never_removed_and_a_new_transfer_appears() {
        let over: Vec<Progress> = history()
            .into_iter()
            .filter(|item| item.phase.is_over())
            .collect();
        let put_away = put_away_now(&over);
        assert!(not_put_away(&over, &put_away).is_empty());
        assert!(
            !can_put_away(&over, &put_away),
            "X was offered with nothing left to put away"
        );
        // The record is still all there: the Transfers tab lists it.
        assert_eq!(find_transfers(&over, "", TransferFilter::All).len(), 4);
        // A new transfer has a new id, so it is listed, and X is not offered while it goes.
        let mut more = over.clone();
        more.push(listed(
            "f",
            "new.bin",
            "peer-laptop",
            false,
            8,
            2,
            TransferState::Running,
        ));
        assert_eq!(ids(&not_put_away(&more, &put_away)), ["f"]);
        assert!(!can_put_away(&more, &put_away));
        // When it ends it is over and not yet put away: listed, and X is offered again.
        more[4] = listed(
            "f",
            "new.bin",
            "peer-laptop",
            false,
            8,
            8,
            TransferState::Complete,
        );
        assert_eq!(ids(&not_put_away(&more, &put_away)), ["f"]);
        assert!(can_put_away(&more, &put_away));
    }

    #[test]
    fn a_transfer_that_is_still_going_is_never_hidden_even_if_its_id_was_put_away() {
        let items = vec![listed(
            "x",
            "again.bin",
            "p",
            false,
            10,
            5,
            TransferState::Running,
        )];
        assert_eq!(ids(&not_put_away(&items, &["x".to_owned()])), ["x"]);
    }

    #[test]
    fn the_circle_sums_only_what_is_going_and_names_the_one_moving_now() {
        let items = history();
        let all = overall(&items);
        // Only "photo.jpg" is going: 40 bytes, 25 % moved.
        assert_eq!(
            all,
            Overall {
                files: 1,
                bytes: 40,
                moved: 10,
                percent: 25,
                current: Some("photo.jpg".into())
            }
        );
    }

    #[test]
    fn the_circle_adds_the_bytes_of_every_file_not_the_average_of_their_percentages() {
        let items = vec![
            listed("1", "small.bin", "p", false, 10, 10, TransferState::Running),
            listed("2", "large.bin", "p", false, 990, 0, TransferState::Running),
        ];
        let all = overall(&items);
        assert_eq!((all.files, all.bytes, all.moved), (2, 1000, 10));
        assert_eq!(
            all.percent, 1,
            "a tiny file that is done made the whole look half done"
        );
        assert_eq!(
            all.current.as_deref(),
            Some("small.bin"),
            "the first one that moves is the current one"
        );
    }

    #[test]
    fn the_circle_names_the_first_waiting_file_when_none_is_moving_yet() {
        let items = vec![
            listed("1", "first.bin", "p", true, 10, 0, TransferState::Offered),
            listed("2", "second.bin", "p", true, 10, 0, TransferState::Offered),
        ];
        assert_eq!(overall(&items).current.as_deref(), Some("first.bin"));
    }

    #[test]
    fn with_nothing_going_the_circle_has_nothing_to_say_and_never_divides_by_zero() {
        assert_eq!(
            overall(&[]),
            Overall {
                files: 0,
                bytes: 0,
                moved: 0,
                percent: 100,
                current: None
            }
        );
        let over = vec![listed(
            "a",
            "done.bin",
            "p",
            false,
            10,
            10,
            TransferState::Complete,
        )];
        assert_eq!(overall(&over).files, 0);
        // Files of nothing, going: complete, not NaN.
        let empty = vec![listed(
            "e",
            "empty.bin",
            "p",
            false,
            0,
            0,
            TransferState::Running,
        )];
        assert_eq!(overall(&empty).percent, 100);
    }

    #[test]
    fn progress_is_the_share_of_the_bytes_that_have_moved() {
        assert_eq!(percent(200, 50, TransferState::Running), 25);
        assert_eq!(percent(200, 0, TransferState::Running), 0);
        assert_eq!(
            percent(3, 1, TransferState::Running),
            33,
            "rounds down, never up"
        );
    }

    fn going() -> Vec<Progress> {
        vec![
            listed("a", "done.bin", "p", false, 10, 10, TransferState::Complete),
            listed(
                "b",
                "moving.bin",
                "p",
                false,
                2048,
                1024,
                TransferState::Running,
            ),
        ]
    }
    fn all_over() -> Vec<Progress> {
        vec![
            listed("a", "one.bin", "p", false, 10, 10, TransferState::Complete),
            listed("b", "two.bin", "p", false, 10, 0, TransferState::Cancelled),
        ]
    }

    #[test]
    fn while_something_is_going_the_button_only_folds_and_unfolds_and_never_hides() {
        let items = going();
        assert_eq!(
            header_action(&items, &[], false),
            Some(HeaderAction::Minimize)
        );
        assert_eq!(header_action(&items, &[], true), Some(HeaderAction::Expand));
        // Whatever was put away before changes nothing in the middle of a transfer.
        assert_eq!(
            header_action(&items, &["a".to_owned()], false),
            Some(HeaderAction::Minimize)
        );
    }

    #[test]
    fn when_everything_is_over_the_button_is_x_and_it_is_not_there_once_all_is_put_away() {
        let items = all_over();
        assert_eq!(header_action(&items, &[], false), Some(HeaderAction::Hide));
        // Folded or not makes no difference when there is nothing going to fold.
        assert_eq!(header_action(&items, &[], true), Some(HeaderAction::Hide));
        let put_away = put_away_now(&items);
        assert_eq!(
            header_action(&items, &put_away, false),
            None,
            "a button that does nothing was shown"
        );
    }

    #[test]
    fn with_nothing_at_all_there_is_no_button() {
        assert_eq!(header_action(&[], &[], false), None);
        assert_eq!(header_action(&[], &[], true), None);
    }

    #[test]
    fn a_new_transfer_after_x_brings_the_fold_button_back_and_the_old_ones_stay_away() {
        let mut items = all_over();
        let put_away = put_away_now(&items);
        items.push(listed(
            "c",
            "new.bin",
            "p",
            false,
            100,
            10,
            TransferState::Running,
        ));
        assert_eq!(
            header_action(&items, &put_away, false),
            Some(HeaderAction::Minimize)
        );
        assert_eq!(ids(&not_put_away(&items, &put_away)), ["c"]);
        items[2] = listed(
            "c",
            "new.bin",
            "p",
            false,
            100,
            100,
            TransferState::Complete,
        );
        // The new one is over and not put away: X is offered again, for it alone.
        assert_eq!(
            header_action(&items, &put_away, false),
            Some(HeaderAction::Hide)
        );
        assert_eq!(ids(&not_put_away(&items, &put_away)), ["c"]);
    }

    #[test]
    fn the_folded_line_says_how_many_how_far_how_big_and_which_one_is_moving() {
        assert_eq!(
            folded_line(&going()).as_deref(),
            Some("1 file · 50% of 2.0 KB · moving.bin")
        );
        let two = vec![
            listed(
                "a",
                "first.bin",
                "p",
                false,
                1024,
                0,
                TransferState::Running,
            ),
            listed(
                "b",
                "second.bin",
                "p",
                false,
                1024,
                0,
                TransferState::Offered,
            ),
        ];
        assert_eq!(
            folded_line(&two).as_deref(),
            Some("2 files · 0% of 2.0 KB · first.bin")
        );
    }

    #[test]
    fn there_is_no_folded_line_when_nothing_is_going() {
        assert_eq!(folded_line(&[]), None);
        assert_eq!(folded_line(&all_over()), None);
    }

    #[test]
    fn a_finished_transfer_reads_as_complete_whatever_the_counters_say() {
        assert_eq!(percent(200, 199, TransferState::Complete), 100);
    }

    #[test]
    fn a_transfer_of_nothing_is_not_a_division_by_zero() {
        assert_eq!(percent(0, 0, TransferState::Running), 100);
    }

    #[test]
    fn a_peer_that_reports_more_than_it_advertised_cannot_draw_past_the_end() {
        assert_eq!(percent(100, 5_000, TransferState::Running), 100);
    }

    #[test]
    fn enormous_sizes_do_not_overflow() {
        // 2^63 bytes done of 2^63: a naive `done * 100` overflows u64.
        let huge = 1u64 << 63;
        assert_eq!(percent(huge, huge, TransferState::Running), 100);
        assert_eq!(percent(huge, huge / 2, TransferState::Running), 50);
    }

    fn ready(ids: &[&str]) -> SendState {
        SendState::Ready(ids.iter().map(|id| (*id).to_owned()).collect())
    }

    #[test]
    fn no_device_picked_means_a_code_and_devices_picked_mean_those_devices_and_never_a_code() {
        assert_eq!(aim_of(&SendState::ByCode), Aim::Code);
        // The devices go in the order the person picked them, as `send_state` judged them.
        assert_eq!(
            aim_of(&ready(&["laptop", "phone"])),
            Aim::Devices(vec!["laptop".into(), "phone".into()])
        );
        assert_eq!(
            aim_of(&ready(&["phone"])),
            Aim::Devices(vec!["phone".into()])
        );
    }

    #[test]
    fn a_picked_device_that_went_away_holds_everything_back_and_the_others_are_not_sent_to_behind_the_persons_back()
     {
        assert_eq!(aim_of(&SendState::ChosenUnavailable), Aim::Held);
        for instantly in [false, true] {
            assert_eq!(circle_does(true, &Aim::Held, instantly), CircleDoes::Held);
        }
    }

    #[test]
    fn the_real_send_state_of_a_view_drives_the_aim_for_every_case_of_who_is_online() {
        // End to end over the real rule, not a hand-made state: online devices are a destination, an offline picked one holds the send back.
        let view = view_of(vec![peer("laptop", true), peer("phone", false)]);
        let owned =
            |ids: &[&str]| -> Vec<String> { ids.iter().map(|id| (*id).to_owned()).collect() };
        assert_eq!(
            aim_of(&view.send_state(&owned(&["id-laptop"]))),
            Aim::Devices(vec!["id-laptop".into()])
        );
        assert_eq!(
            aim_of(&view.send_state(&owned(&["id-phone"]))),
            Aim::Held,
            "an offline device was a destination"
        );
        assert_eq!(
            aim_of(&view.send_state(&owned(&["id-laptop", "id-phone"]))),
            Aim::Held,
            "one offline device must hold the send back"
        );
        assert_eq!(aim_of(&view.send_state(&[])), Aim::Code);
    }

    #[test]
    fn the_circle_says_what_it_does_from_one_place_in_each_of_its_states() {
        let devices = Aim::Devices(vec!["laptop".into()]);
        // Nothing listed: the picker, whatever else is true.
        for aim in [&Aim::Code, &devices, &Aim::Held] {
            assert_eq!(circle_does(false, aim, false), CircleDoes::ChooseFiles);
            assert_eq!(circle_does(false, aim, true), CircleDoes::ChooseFiles);
        }
        // Files listed and no device picked: a tap makes a code, however Send instantly is set. A hold is never asked for here.
        assert_eq!(circle_does(true, &Aim::Code, false), CircleDoes::MakeCode);
        assert_eq!(circle_does(true, &Aim::Code, true), CircleDoes::MakeCode);
        // Files listed and devices picked: held to send, or one tap with Send instantly.
        assert_eq!(circle_does(true, &devices, false), CircleDoes::HoldToSend);
        assert_eq!(circle_does(true, &devices, true), CircleDoes::TapToSend);
    }

    #[test]
    fn a_tap_never_opens_the_picker_again_once_files_are_listed() {
        let devices = Aim::Devices(vec!["laptop".into()]);
        for aim in [&Aim::Code, &devices, &Aim::Held] {
            for instantly in [false, true] {
                assert_ne!(circle_does(true, aim, instantly), CircleDoes::ChooseFiles);
            }
        }
    }

    #[test]
    fn the_circle_fills_while_held_and_sends_exactly_when_full_not_before() {
        let mut touch = Hold::default();
        touch.down((50., 50.), 1_000);
        assert_eq!(fill_of(&touch, 1_000, true), 0.);
        let half = fill_of(&touch, 1_000 + CONFIRM_MS / 2, true);
        assert!((half - 0.5).abs() < 0.01, "half the time was {half}");
        assert!(!is_full(
            &touch,
            1_000 + CONFIRM_MS - 1,
            CircleDoes::HoldToSend
        ));
        assert!(
            is_full(&touch, 1_000 + CONFIRM_MS, CircleDoes::HoldToSend),
            "the boundary itself is full"
        );
        assert_eq!(
            fill_of(&touch, 1_000 + CONFIRM_MS * 4, true),
            1.,
            "the fill went past the end"
        );
    }

    #[test]
    fn only_a_circle_that_is_held_ever_fills_or_sends_by_waiting() {
        let mut touch = Hold::default();
        touch.down((50., 50.), 0);
        for does in [
            CircleDoes::ChooseFiles,
            CircleDoes::MakeCode,
            CircleDoes::TapToSend,
            CircleDoes::Held,
        ] {
            assert!(
                !is_full(&touch, CONFIRM_MS * 10, does),
                "{does:?} sent by waiting"
            );
        }
    }

    #[test]
    fn a_press_as_long_as_a_hold_takes_does_not_send_for_that_is_not_the_time_to_confirm() {
        let mut touch = Hold::default();
        touch.down((50., 50.), 0);
        assert!(touch.is_holding(HOLD_MS), "the hold boundary moved");
        assert!(
            !is_full(&touch, HOLD_MS, CircleDoes::HoldToSend),
            "resting a finger for the time of a press sent the files"
        );
        const { assert!(CONFIRM_MS > HOLD_MS) };
    }

    #[test]
    fn dragging_empties_the_circle_for_good_however_long_the_finger_stays_down() {
        let mut touch = Hold::default();
        touch.down((50., 50.), 0);
        touch.moved((50., 50. + TAP_SLOP + 1.));
        assert_eq!(touch.held_for(CONFIRM_MS * 10), None);
        assert_eq!(fill_of(&touch, CONFIRM_MS * 10, true), 0.);
        // Coming back to where it began does not undo it.
        touch.moved((50., 50.));
        assert!(
            !is_full(&touch, CONFIRM_MS * 10, CircleDoes::HoldToSend),
            "a drag sent the files"
        );
    }

    #[test]
    fn lifting_early_empties_the_circle_and_sends_nothing() {
        let mut touch = Hold::default();
        touch.down((50., 50.), 0);
        assert_eq!(touch.up((50., 50.), CONFIRM_MS - 1), Ended::Hold);
        assert_eq!(
            touch.held_for(CONFIRM_MS - 1),
            None,
            "a lifted finger kept filling the circle"
        );
        assert!(!is_full(&touch, CONFIRM_MS * 3, CircleDoes::HoldToSend));
    }

    #[test]
    fn with_send_instantly_a_whole_tap_sends_on_lifting_and_nothing_else_does() {
        assert!(sends_on_lift(Ended::Tap, CircleDoes::TapToSend));
        // A touch that was dragged, cancelled, or held long enough to be a hold is not a tap: no instant send from it.
        assert!(
            !sends_on_lift(Ended::Nothing, CircleDoes::TapToSend),
            "a drag or a cancelled touch sent"
        );
        assert!(
            !sends_on_lift(Ended::Hold, CircleDoes::TapToSend),
            "a long press sent instantly"
        );
    }

    #[test]
    fn only_the_instant_circle_sends_on_a_tap_so_a_hold_circle_a_code_circle_and_a_held_back_one_do_not()
     {
        for does in [
            CircleDoes::HoldToSend,
            CircleDoes::MakeCode,
            CircleDoes::ChooseFiles,
            CircleDoes::Held,
        ] {
            assert!(!sends_on_lift(Ended::Tap, does), "{does:?} sent on a tap");
        }
    }

    #[test]
    fn how_long_the_circle_is_held_depends_only_on_whether_sending_is_instant() {
        assert_eq!(confirm_ms(false), CONFIRM_MS);
        assert_eq!(confirm_ms(true), 0);
    }

    #[test]
    fn the_disc_that_fills_the_circle_grows_with_the_hold_and_is_the_whole_circle_exactly_when_it_sends()
     {
        const DISC: f32 = 300.;
        let mut touch = Hold::default();
        touch.down((50., 50.), 0);
        let size = |ms: u64| fill_disc(fill_of(&touch, ms, true), DISC);
        assert_eq!(
            size(0),
            0.,
            "a touch that has just begun already showed a fill"
        );
        assert_eq!(size(CONFIRM_MS / 2), DISC / 2.);
        assert!(
            size(CONFIRM_MS - 1) < DISC,
            "the circle was full before it sent"
        );
        assert_eq!(size(CONFIRM_MS), DISC);
        assert!(
            is_full(&touch, CONFIRM_MS, CircleDoes::HoldToSend),
            "the disc is whole and the circle did not send"
        );
        // Never more than the circle however long it is held, and growing all the way (never going back).
        assert_eq!(size(CONFIRM_MS * 10), DISC);
        let mut last = 0.;
        for ms in (0..=CONFIRM_MS).step_by(10) {
            assert!(size(ms) >= last, "the fill went back at {ms} ms");
            last = size(ms);
        }
    }

    #[test]
    fn the_disc_shows_nothing_for_anything_that_is_not_a_fill() {
        for bad in [f32::NAN, f32::NEG_INFINITY, f32::INFINITY, -1., -0.0, 0.] {
            assert_eq!(fill_disc(bad, 300.), 0., "{bad} drew a fill");
        }
        assert_eq!(fill_disc(0.5, 0.), 0.);
        assert_eq!(fill_disc(0.5, f32::NAN), 0.);
        assert_eq!(fill_disc(1.5, 300.), 300.);
    }

    #[test]
    fn while_files_go_the_circle_fills_with_how_far_the_batch_has_come_and_not_with_the_finger() {
        let going = |percent| BatchState::Going {
            files: 2,
            percent,
            current: None,
        };
        assert_eq!(
            circle_fill(&going(0), 0.9),
            0.,
            "a finger that is no longer the circle's business filled it"
        );
        assert_eq!(circle_fill(&going(40), 0.), 0.4);
        assert_eq!(circle_fill(&going(100), 0.), 1.);
        // A percent that went past the end is the whole circle, and never more.
        assert_eq!(circle_fill(&going(250), 0.), 1.);
        // It never goes back as the batch comes on.
        let mut last = 0.;
        for percent in 0..=100u8 {
            let now = circle_fill(&going(percent), 0.);
            assert!(now >= last, "the circle emptied at {percent}%");
            last = now;
        }
    }

    #[test]
    fn a_circle_that_is_held_fills_with_the_finger_and_every_other_state_is_empty() {
        let quiet = [
            BatchState::None,
            BatchState::Sent { files: 2 },
            BatchState::Stopped { files: 2, done: 1 },
        ];
        for state in &quiet {
            // Held to send: the finger fills it.
            assert_eq!(circle_fill(state, 0.5), 0.5, "{state:?}");
            assert_eq!(circle_fill(state, 1.), 1.);
            // Not held: nothing is left filled in a circle nobody is acting on.
            assert_eq!(circle_fill(state, 0.), 0., "{state:?}");
        }
        // A number that went wrong upstream fills nothing, and a fill past the end is the whole circle.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.] {
            assert_eq!(circle_fill(&BatchState::None, bad), 0., "{bad}");
        }
        assert_eq!(circle_fill(&BatchState::None, 5.), 1.);
    }

    #[test]
    fn every_state_has_a_phase_and_a_failure_keeps_its_reason() {
        let phase = |state| progress(&transfer(10, 1, state)).phase;
        assert_eq!(phase(TransferState::Offered), Phase::Waiting);
        assert_eq!(phase(TransferState::Running), Phase::Moving);
        assert_eq!(phase(TransferState::Complete), Phase::Done);
        assert_eq!(phase(TransferState::Cancelled), Phase::Cancelled);
        assert_eq!(
            phase(TransferState::Failed {
                reason: "disk full".into()
            }),
            Phase::Failed("disk full".into())
        );
    }

    #[test]
    fn an_offer_already_waiting_when_the_window_opens_is_found_from_that_snapshot_with_the_settings_that_decide_where_it_goes()
     {
        let mut waiting = transfer(10, 0, TransferState::Offered);
        waiting.incoming = true;
        waiting.peer_name = Some("Alex's phone".into());
        let mut snapshot = snapshot_with(
            vec![],
            vec![transfer(10, 10, TransferState::Complete), waiting],
        );
        snapshot.settings.auto_save = true;
        snapshot.settings.save_folder = Some(std::path::PathBuf::from("/home/me/Received"));
        // From the snapshot alone, with no change since: this is what the window opens on.
        let found =
            offered_now(&View::of(&snapshot)).expect("an offer that was waiting was not shown");
        assert!(found.offer.acceptable());
        assert_eq!(found.offer.from.as_deref(), Some("Alex's phone"));
        assert!(found.auto_save);
        assert_eq!(
            found.save_folder.as_deref(),
            Some(std::path::Path::new("/home/me/Received"))
        );
        // And nothing is shown when nothing waits.
        assert_eq!(
            offered_now(&View::of(&snapshot_with(
                vec![],
                vec![transfer(10, 10, TransferState::Complete)]
            ))),
            None
        );
        assert_eq!(offered_now(&View::default()), None);
    }

    #[test]
    fn only_transfers_still_going_can_be_stopped() {
        let snapshot = snapshot_with(
            vec![],
            vec![
                transfer(10, 1, TransferState::Offered),
                transfer(10, 1, TransferState::Running),
                transfer(10, 10, TransferState::Complete),
                transfer(10, 1, TransferState::Cancelled),
                transfer(10, 1, TransferState::Failed { reason: "x".into() }),
            ],
        );
        let stoppable: Vec<bool> = View::of(&snapshot)
            .transfers
            .iter()
            .map(Progress::stoppable)
            .collect();
        // Offered, running, complete, cancelled, failed.
        assert_eq!(stoppable, [true, true, false, false, false]);
    }

    #[test]
    fn only_an_incoming_offer_still_waiting_can_be_accepted() {
        let make = |incoming: bool, state: TransferState| {
            let mut t = transfer(10, 0, state);
            t.incoming = incoming;
            progress(&t)
        };
        // Waiting and coming in: the person decides.
        assert!(make(true, TransferState::Offered).acceptable());
        // Waiting but going out: it is the OTHER device that decides, not this one.
        assert!(!make(false, TransferState::Offered).acceptable());
        // Already moving, finished, stopped or failed: nothing left to accept.
        assert!(!make(true, TransferState::Running).acceptable());
        assert!(!make(true, TransferState::Complete).acceptable());
        assert!(!make(true, TransferState::Cancelled).acceptable());
        assert!(!make(true, TransferState::Failed { reason: "x".into() }).acceptable());
    }

    fn peer(name: &str, online: bool) -> Peer {
        Peer {
            id: PeerId(format!("id-{name}")),
            name: name.into(),
            paired_at: 0,
            last_seen: None,
            online,
            sleeping: false,
            statuses: Vec::new(),
            grants: Default::default(),
            plugin_values: Default::default(),
        }
    }
    /// A snapshot with nothing in it: the one place a test builds a whole `Snapshot` by hand, so a field
    /// added to it is added here and nowhere else.
    fn empty_snapshot() -> Snapshot {
        Snapshot {
            settings: Default::default(),
            peers: vec![],
            grants: Default::default(),
            plugins: vec![],
            staged: vec![],
            store: Default::default(),
            actions: vec![],
            transfers: vec![],
            pairing: vec![],
            extension_prompts: vec![],
            errors: vec![],
            errors_said: 0,
            profile_picture: None,
            peer_pictures: Default::default(),
            displays: Vec::new(),
            peer_displays: Default::default(),
            files_code: None,
            link_tests: Default::default(),
            typed_addresses: Default::default(),
            listening_port: None,
            relay: None,
            relay_probes: Default::default(),
            page_views: Vec::new(),
        }
    }
    fn snapshot_with(peers: Vec<Peer>, transfers: Vec<Transfer>) -> Snapshot {
        Snapshot {
            peers,
            transfers,
            ..empty_snapshot()
        }
    }

    #[test]
    fn devices_keep_their_order_and_say_whether_they_are_reachable() {
        let snapshot = snapshot_with(
            vec![
                peer("laptop", true),
                peer("phone", false),
                peer("tablet", true),
            ],
            vec![],
        );
        let view = View::of(&snapshot);
        assert_eq!(
            view.devices
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            ["laptop", "phone", "tablet"]
        );
        assert_eq!(
            view.reachable()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            ["laptop", "tablet"],
            "an offline device was offered as a destination"
        );
    }

    #[test]
    fn a_device_is_named_to_commands_by_its_identifier_and_shown_by_its_name() {
        let view = View::of(&snapshot_with(vec![peer("laptop", true)], vec![]));
        assert_eq!(view.devices[0].id, "id-laptop");
        assert_eq!(view.devices[0].name, "laptop");
    }

    #[test]
    fn a_pairing_request_carries_what_is_needed_to_decide_it() {
        let mut snapshot = snapshot_with(vec![], vec![]);
        snapshot.pairing = vec![PairingPrompt {
            id: "req-1".into(),
            name: "Alex's phone".into(),
            options: [11, 22, 33],
            selected: false,
            remaining_seconds: 42,
        }];
        let approval = &View::of(&snapshot).approvals[0];
        assert_eq!(approval.request, "req-1");
        assert_eq!(approval.name, "Alex's phone");
        assert_eq!(approval.options, [11, 22, 33]);
        assert!(!approval.answered);
        assert_eq!(approval.seconds_left, 42);
    }

    #[test]
    fn the_newest_pairing_request_comes_first_whatever_order_the_owner_listed_them_in() {
        let prompt = |id: &str, seconds: u64| PairingPrompt {
            id: id.into(),
            name: "My device".into(),
            options: [1, 2, 3],
            selected: false,
            remaining_seconds: seconds,
        };
        let mut snapshot = snapshot_with(vec![], vec![]);
        // The old one is listed first, as the owner did in a real run.
        snapshot.pairing = vec![prompt("old", 17), prompt("new", 117), prompt("middle", 60)];
        let order: Vec<_> = View::of(&snapshot)
            .approvals
            .iter()
            .map(|a| a.request.clone())
            .collect();
        assert_eq!(order, ["new", "middle", "old"]);
    }

    #[test]
    fn a_device_still_on_the_default_name_has_not_been_named_by_its_person() {
        let mut view = View::default();
        assert!(view.needs_a_name(), "a fresh install has the default name");
        view.settings.device_name = "Alex's laptop".into();
        assert!(!view.needs_a_name());
        // Only the exact default counts; a name that merely contains it was chosen.
        view.settings.device_name = "My device 2".into();
        assert!(!view.needs_a_name());
        // And the default spelled with spaces around it is still the default.
        view.settings.device_name = format!("  {}  ", Settings::default().device_name);
        assert!(view.needs_a_name());
    }

    #[test]
    fn the_window_is_titled_with_the_name_of_the_device_it_belongs_to() {
        let mut view = View::default();
        // A device that has not been named yet is just "Peersus": the default name says nothing.
        assert_eq!(view.window_title(), "Peersus");
        view.settings.device_name = "  Alex laptop ".into();
        assert_eq!(view.window_title(), "Peersus \u{b7} Alex laptop");
        // A name of only spaces is no name.
        view.settings.device_name = "   ".into();
        assert_eq!(view.window_title(), "Peersus");
    }

    #[test]
    fn what_the_owner_reported_is_passed_on_unchanged() {
        let mut snapshot = snapshot_with(vec![], vec![]);
        snapshot.errors = vec!["A plugin stopped working and was turned off".into()];
        assert_eq!(
            View::of(&snapshot).errors,
            ["A plugin stopped working and was turned off"]
        );
    }

    #[test]
    fn nothing_known_is_an_empty_view_not_an_error() {
        assert_eq!(View::of(&snapshot_with(vec![], vec![])), View::default());
    }

    #[test]
    fn a_snapshot_that_shows_nothing_new_is_not_a_change() {
        let snapshot = snapshot_with(vec![peer("laptop", true)], vec![]);
        let mut current = View::of(&snapshot);
        // The owner republishes the same state every second.
        assert!(!refresh(&mut current, &snapshot));
        assert!(!refresh(&mut current, &snapshot.clone()));
        assert_eq!(current, View::of(&snapshot));
    }

    #[test]
    fn a_snapshot_that_shows_something_new_replaces_the_view() {
        let mut current = View::of(&snapshot_with(vec![peer("laptop", true)], vec![]));
        let later = snapshot_with(vec![peer("laptop", false)], vec![]);
        assert!(refresh(&mut current, &later));
        assert_eq!(current, View::of(&later));
        assert_eq!(current.devices[0].presence, Presence::Offline);
    }

    fn destination_of(view: &View, chosen: &[&str]) -> Option<Vec<String>> {
        let chosen: Vec<String> = chosen.iter().map(|id| (*id).to_owned()).collect();
        view.destinations(&chosen)
            .map(|devices| devices.iter().map(|device| device.name.clone()).collect())
    }
    fn view_of(peers: Vec<Peer>) -> View {
        View::of(&snapshot_with(peers, vec![]))
    }

    #[test]
    fn with_nothing_picked_there_is_no_destination_however_many_devices_are_reachable() {
        // One reachable, one not.
        let view = view_of(vec![peer("laptop", true), peer("phone", false)]);
        assert_eq!(destination_of(&view, &[]), None);
        // Several reachable.
        let view = view_of(vec![peer("laptop", true), peer("tablet", true)]);
        assert_eq!(destination_of(&view, &[]), None);
        // None reachable, and none paired.
        assert_eq!(
            destination_of(&view_of(vec![peer("laptop", false)]), &[]),
            None
        );
        assert_eq!(destination_of(&view_of(vec![]), &[]), None);
    }

    #[test]
    fn picked_devices_are_the_destinations_in_the_order_they_were_picked_while_all_are_reachable() {
        let view = view_of(vec![
            peer("laptop", true),
            peer("tablet", true),
            peer("phone", true),
        ]);
        assert_eq!(
            destination_of(&view, &["id-tablet"]),
            Some(vec!["tablet".to_owned()])
        );
        // The order is the person's, not the order the devices are listed in.
        assert_eq!(
            destination_of(&view, &["id-phone", "id-laptop"]),
            Some(vec!["phone".to_owned(), "laptop".to_owned()])
        );
    }

    #[test]
    fn a_picked_device_that_goes_offline_is_never_swapped_for_another_nor_left_out() {
        // The person picked the laptop. It drops off the network while the tablet is online:
        // the next files must NOT quietly go to the tablet.
        let before = view_of(vec![peer("laptop", true), peer("tablet", true)]);
        assert_eq!(
            destination_of(&before, &["id-laptop"]),
            Some(vec!["laptop".to_owned()])
        );
        let after = view_of(vec![peer("laptop", false), peer("tablet", true)]);
        assert_eq!(
            destination_of(&after, &["id-laptop"]),
            None,
            "the person's choice was replaced by another device"
        );
        // With two picked and one gone, the one that is left does not get the files alone.
        assert_eq!(
            destination_of(&after, &["id-laptop", "id-tablet"]),
            None,
            "part of the choice was sent while the rest was dropped"
        );
        // And with nothing picked, nothing is chosen for the person either.
        assert_eq!(destination_of(&after, &[]), None);
    }

    #[test]
    fn a_picked_device_that_was_forgotten_is_not_a_destination() {
        let view = view_of(vec![peer("tablet", true)]);
        assert_eq!(destination_of(&view, &["id-laptop"]), None);
        assert_eq!(destination_of(&view, &["id-tablet", "id-laptop"]), None);
    }

    #[test]
    fn a_send_goes_to_devices_only_when_the_person_picked_them_otherwise_it_is_by_code() {
        use SendState::*;
        let owned =
            |ids: &[&str]| -> Vec<String> { ids.iter().map(|id| (*id).to_owned()).collect() };
        let state = |peers: Vec<Peer>, chosen: &[&str]| view_of(peers).send_state(&owned(chosen));
        // Nothing picked: always a code, whatever is paired or online. Even a single online
        // device is NOT chosen for the person: what is being sent is their own files.
        assert_eq!(state(vec![], &[]), ByCode);
        assert_eq!(state(vec![peer("a", false)], &[]), ByCode);
        assert_eq!(state(vec![peer("a", true)], &[]), ByCode);
        assert_eq!(state(vec![peer("a", true), peer("b", true)], &[]), ByCode);
        // Picked and reachable: straight to it, or to all of them.
        assert_eq!(
            state(vec![peer("a", true), peer("b", true)], &["id-b"]),
            Ready(owned(&["id-b"]))
        );
        assert_eq!(
            state(vec![peer("a", true), peer("b", true)], &["id-b", "id-a"]),
            Ready(owned(&["id-b", "id-a"]))
        );
        // One of those picked went away while another is online: say THAT, do not pick for
        // the person and do not quietly switch to a code.
        assert_eq!(
            state(vec![peer("a", false), peer("b", true)], &["id-a"]),
            ChosenUnavailable
        );
        assert_eq!(
            state(vec![peer("a", false), peer("b", true)], &["id-a", "id-b"]),
            ChosenUnavailable
        );
        // Picked, then forgotten altogether: the same, there is nothing to send to.
        assert_eq!(state(vec![peer("b", true)], &["id-a"]), ChosenUnavailable);
    }

    #[test]
    fn settings_has_these_tabs_in_this_order_with_distinct_names_and_opens_on_the_first() {
        let titles: Vec<&str> = SettingsTab::ALL.iter().map(|tab| tab.title()).collect();
        assert_eq!(
            titles,
            [
                "Overview",
                "Device",
                "Appearance",
                "Network",
                "Extensions",
                "Permissions",
                "About"
            ]
        );
        let mut sorted = titles.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), titles.len(), "two tabs share a name");
        assert_eq!(SettingsTab::default(), SettingsTab::ALL[0]);
    }

    #[test]
    fn a_popup_opens_from_transparent_and_a_little_high_to_opaque_and_in_place() {
        let start = Popup::at(0.);
        let end = Popup::at(1.);
        assert_eq!((start.opacity, start.lift), (0., Popup::RISE));
        assert_eq!((end.opacity, end.lift), (1., 0.));
        // Never beyond its ends, whatever the animation hands over.
        assert_eq!(Popup::at(-3.), start);
        assert_eq!(Popup::at(9.), end);
        let middle = Popup::at(0.5);
        assert!(middle.opacity > 0. && middle.opacity < 1.);
        assert!(middle.lift > 0. && middle.lift < Popup::RISE);
    }

    #[test]
    fn a_thing_that_is_still_is_whole_and_in_place_from_the_first_frame() {
        assert_eq!(
            Popup::still(),
            Popup {
                opacity: 1.,
                lift: 0.,
                scale: 1.
            }
        );
        // And it is exactly where a finished opening ends, so reduced motion shows the same picture.
        assert_eq!(Popup::still(), Popup::at(1.));
    }

    #[test]
    fn a_popup_also_grows_from_a_little_smaller_to_its_full_size() {
        assert_eq!(Popup::at(0.).scale, Popup::SMALLEST);
        assert_eq!(Popup::at(1.).scale, 1.);
        assert_eq!(Popup::still().scale, 1.);
        let middle = Popup::at(0.5).scale;
        assert!(middle > Popup::SMALLEST && middle < 1.);
    }

    #[test]
    fn the_dashboard_menu_in_the_sidebar_has_these_seven_entries_once_each_in_this_order() {
        assert_eq!(
            SettingsTab::ALL,
            [
                SettingsTab::Overview,
                SettingsTab::Device,
                SettingsTab::Appearance,
                SettingsTab::Network,
                SettingsTab::Extensions,
                SettingsTab::Permissions,
                SettingsTab::About,
            ]
        );
        // It opens on its first entry, an overview of how things stand.
        assert_eq!(SettingsTab::default(), SettingsTab::ALL[0]);
        // The places of the header are not in it: Send, Receive and Peers stay where they are.
        for tab in SettingsTab::ALL {
            assert!(!matches!(tab.title(), "Send" | "Receive" | "Peers"));
        }
    }

    #[test]
    fn about_sits_at_the_foot_of_the_sidebar_and_the_other_six_stay_at_the_top() {
        assert_eq!(
            Sidebar::TOP,
            [
                SettingsTab::Overview,
                SettingsTab::Device,
                SettingsTab::Appearance,
                SettingsTab::Network,
                SettingsTab::Extensions,
                SettingsTab::Permissions,
            ]
        );
        assert_eq!(Sidebar::FOOT, SettingsTab::About);
        // Together they are exactly the eight entries, in the same order, none lost and none twice.
        let mut all = Sidebar::TOP.to_vec();
        all.push(Sidebar::FOOT);
        assert_eq!(all, SettingsTab::ALL);
    }

    #[test]
    fn permission_rows_are_what_the_owner_says_can_be_granted_never_what_a_manifest_lists() {
        use peersus_api::{manifest::Manifest, permission::Permission};
        use peersus_core::model::{Grantable, PluginInfo, PluginStatus, Standing};
        use peersus_core::plugins::{Grants, PluginId};
        let mut manifest = Manifest::default();
        manifest
            .capabilities
            .insert("send.use".into(), "Send things for you".into());
        // Listed in the manifest, and not grantable: the owner does not offer it, so it is no row.
        manifest
            .capabilities
            .insert("just.listed".into(), "Only listed".into());
        let info = PluginInfo {
            id: PluginId::new("syncer").unwrap(),
            manifest,
            standing: Standing::Ready,
            status: PluginStatus::Running,
            grantable: Grantable {
                per_device: vec![Permission::new("syncer", "send.use").unwrap()],
                outright: vec![],
            },
            program: None,
            package: None,
        };

        let rows = PermissionRows::of(&info, &info.grantable.per_device, &Grants::default());
        assert_eq!(rows.len(), 1, "a name that was only listed became a row");
        assert_eq!(
            rows[0].permission,
            Permission::new("syncer", "send.use").unwrap()
        );
        assert_eq!(rows[0].description, "Send things for you");
        assert!(!rows[0].allowed);
        // Granted: the row is on.
        let held = Grants::new([Permission::new("syncer", "send.use").unwrap()]).unwrap();
        assert!(PermissionRows::of(&info, &info.grantable.per_device, &held)[0].allowed);
    }

    #[test]
    fn a_permission_of_the_application_is_described_in_the_applications_own_words() {
        use peersus_api::permission::Permission;
        use peersus_core::model::{Grantable, PluginInfo, PluginStatus, Standing};
        use peersus_core::plugins::{Grants, PluginId};
        let mut info = PluginInfo {
            id: PluginId::new("sender").unwrap(),
            manifest: Default::default(),
            standing: Standing::Ready,
            status: PluginStatus::Running,
            grantable: Grantable::default(),
            program: None,
            package: None,
        };
        // Not in the plugin's manifest at all: its owner is the application, which wrote the words.
        let send = Permission::new("host", "peers.send").unwrap();
        let rows = PermissionRows::of(&info, std::slice::from_ref(&send), &Grants::default());
        assert_eq!(
            rows[0].description,
            peersus_app::runtime::PEERS_SEND_DESCRIPTION
        );
        let restart = Permission::new("host", "plugins.restart").unwrap();
        let rows = PermissionRows::of(&info, std::slice::from_ref(&restart), &Grants::default());
        assert_eq!(
            rows[0].description,
            peersus_app::runtime::PLUGINS_RESTART_DESCRIPTION
        );
        // An imported host permission must never borrow a same-named description
        // from the extension asking for it, including in translated search results.
        for (capability, description) in [
            (
                peersus_app::platform_services::FILES_CAPABILITY,
                peersus_app::platform_services::FILES_DESCRIPTION,
            ),
            (
                peersus_app::platform_services::HTTP_CAPABILITY,
                peersus_app::platform_services::HTTP_DESCRIPTION,
            ),
        ] {
            info.manifest.capabilities.insert(
                capability.to_owned(),
                "Extension-supplied description, not the host's consent wording".to_owned(),
            );
            let permission = Permission::new("host", capability).unwrap();
            for language in [
                crate::i18n::Language::English,
                crate::i18n::Language::Romanian,
            ] {
                let rows = PermissionRows::of_in(
                    language,
                    &info,
                    std::slice::from_ref(&permission),
                    &Grants::default(),
                );
                assert_eq!(
                    rows[0].description,
                    crate::i18n::translate(language, description),
                    "host-owned consent wording was missing or replaced by extension text",
                );
            }
        }
        // A permission nobody described is still shown, and says so, rather than nothing at all.
        let mystery = Permission::new("sender", "unknown.use").unwrap();
        let rows = PermissionRows::of(&info, std::slice::from_ref(&mystery), &Grants::default());
        assert!(!rows[0].description.trim().is_empty());
    }

    #[test]
    fn each_device_in_the_view_keeps_what_it_granted_each_plugin() {
        use peersus_api::permission::Permission;
        use peersus_core::model::Peer;
        use peersus_core::plugins::{Grants, PluginId};
        let plugin = PluginId::new("syncer").unwrap();
        let mut peer = Peer {
            id: peersus_core::model::PeerId("peer-a".into()),
            name: "Phone".into(),
            paired_at: 1,
            last_seen: None,
            online: true,
            sleeping: false,
            statuses: Vec::new(),
            grants: Default::default(),
            plugin_values: Default::default(),
        };
        peer.grants.insert(
            plugin.clone(),
            Grants::new([Permission::new("syncer", "send.use").unwrap()]).unwrap(),
        );
        let snapshot = Snapshot {
            peers: vec![peer],
            ..empty_snapshot()
        };
        let view = View::of(&snapshot);
        assert_eq!(view.devices.len(), 1);
        let held = view.devices[0]
            .grants
            .get(&plugin)
            .expect("what it granted is kept");
        assert!(held.allows(&Permission::new("syncer", "send.use").unwrap()));
        // A device that granted a plugin nothing has no entry for it.
        assert!(
            !view.devices[0]
                .grants
                .contains_key(&PluginId::new("other").unwrap())
        );
    }

    #[test]
    fn the_view_keeps_what_each_plugin_is_allowed_outright() {
        use peersus_api::permission::Permission;
        use peersus_core::plugins::{Grants, PluginId};
        let mut snapshot = empty_snapshot();
        let id = PluginId::new("syncer").unwrap();
        snapshot.grants.insert(
            id.clone(),
            Grants::new([Permission::new("syncer", "send.use").unwrap()]).unwrap(),
        );
        let view = View::of(&snapshot);
        assert!(
            view.grants
                .get(&id)
                .is_some_and(|g| g.allows(&Permission::new("syncer", "send.use").unwrap()))
        );
        // A plugin with nothing allowed has no entry, and that is what the window shows as all off.
        assert!(!view.grants.contains_key(&PluginId::new("nothing").unwrap()));
    }

    #[test]
    fn a_click_on_the_profile_opens_the_device_tab_where_the_name_is_edited() {
        assert_eq!(Sidebar::PROFILE, SettingsTab::Device);
        // It is one of the entries of the dashboard, so the page always has somewhere to go.
        assert!(SettingsTab::ALL.contains(&Sidebar::PROFILE));
        // And it is not the entry at the foot: the profile is not About.
        assert_ne!(Sidebar::PROFILE, Sidebar::FOOT);
    }

    #[test]
    fn the_view_carries_the_own_picture_digest_and_one_for_each_device_that_showed_one() {
        use peersus_core::profile::Picture;
        let own = Picture::new(1, 1, vec![1, 2, 3, 255]).unwrap().digest();
        let theirs = Picture::new(1, 1, vec![9, 8, 7, 255]).unwrap().digest();
        let mut snapshot = snapshot_with(vec![peer("laptop", true), peer("phone", true)], vec![]);
        snapshot.profile_picture = Some(own);
        snapshot
            .peer_pictures
            .insert(snapshot.peers[0].id.clone(), theirs);
        let view = View::of(&snapshot);
        assert_eq!(view.profile_picture, Some(own));
        assert_eq!(
            view.devices[0].picture,
            Some(theirs),
            "the device that showed a picture does not carry its digest"
        );
        assert_eq!(
            view.devices[1].picture, None,
            "a device that showed none was given one"
        );
        // A picture for a device that is not in the list names nobody and is not carried anywhere.
        snapshot
            .peer_pictures
            .insert(PeerId("stranger".into()), own);
        let view = View::of(&snapshot);
        assert!(view.devices.iter().all(|d| d.id != "stranger"));
        assert_eq!(
            view.devices.iter().filter(|d| d.picture.is_some()).count(),
            1
        );
    }

    #[test]
    fn a_view_refreshes_when_only_a_picture_changed_and_not_when_nothing_did() {
        use peersus_core::profile::Picture;
        let mut snapshot = snapshot_with(vec![peer("laptop", true)], vec![]);
        let mut view = View::of(&snapshot);
        assert!(
            !refresh(&mut view, &snapshot),
            "a quiet second redrew the window"
        );
        snapshot.profile_picture = Some(Picture::new(1, 1, vec![1, 1, 1, 255]).unwrap().digest());
        assert!(
            refresh(&mut view, &snapshot),
            "a new own picture did not refresh the view"
        );
        assert!(!refresh(&mut view, &snapshot));
        let id = snapshot.peers[0].id.clone();
        snapshot
            .peer_pictures
            .insert(id, Picture::new(1, 1, vec![2, 2, 2, 255]).unwrap().digest());
        assert!(
            refresh(&mut view, &snapshot),
            "a device's new picture did not refresh the view"
        );
    }

    #[test]
    fn a_snapshot_makes_the_window_ask_once_for_each_picture_it_names_and_for_no_other() {
        use crate::shelf::{Shelf, named_by};
        use peersus_core::profile::Picture;
        let (own, theirs) = (
            Picture::new(1, 1, vec![1, 2, 3, 255]).unwrap(),
            Picture::new(1, 1, vec![9, 8, 7, 255]).unwrap(),
        );
        let mut snapshot = snapshot_with(vec![peer("laptop", true), peer("phone", true)], vec![]);
        snapshot.profile_picture = Some(own.digest());
        // Two devices showing the same picture name it once: it is fetched once.
        for device in &snapshot.peers.clone() {
            snapshot
                .peer_pictures
                .insert(device.id.clone(), theirs.digest());
        }
        let named = named_by(&View::of(&snapshot));
        assert_eq!(named, [own.digest(), theirs.digest()].into_iter().collect());

        let mut shelf = Shelf::default();
        let mut asked = shelf.follow(&named);
        asked.sort();
        let mut both = vec![own.digest(), theirs.digest()];
        both.sort();
        assert_eq!(asked, both);
        assert!(
            shelf.follow(&named).is_empty(),
            "the same snapshot made the window ask again while it waited"
        );

        // One arrives; the same snapshot again asks only for the other.
        shelf.arrived(own.digest(), own.clone());
        assert_eq!(
            shelf.follow(&named),
            Vec::<peersus_core::profile::Digest>::new()
        );

        // A picture dropped by the snapshot and named again later is asked for again: it was let go first.
        shelf.arrived(theirs.digest(), theirs.clone());
        assert!(shelf.get(&theirs.digest()).is_some());
        let without: std::collections::BTreeSet<_> = [own.digest()].into_iter().collect();
        assert!(shelf.follow(&without).is_empty());
        assert!(
            shelf.get(&theirs.digest()).is_none(),
            "a picture no snapshot names was kept"
        );
        assert_eq!(
            shelf.follow(&named),
            vec![theirs.digest()],
            "a picture named again was not asked for again"
        );
    }

    #[test]
    fn a_package_waiting_to_be_confirmed_says_whether_it_is_new_or_replaces_one_installed() {
        use peersus_core::model::{PluginInfo, PluginStatus, Standing};
        use peersus_core::plugins::PluginId;
        let info = |name: &str| PluginInfo {
            id: PluginId::new(name).unwrap(),
            manifest: Default::default(),
            standing: Standing::Ready,
            status: PluginStatus::Stopped,
            grantable: Default::default(),
            program: None,
            package: None,
        };
        let installed = vec![info("alpha")];
        assert_eq!(Staging::of(&info("beta"), &installed), Staging::New);
        // The same name as one installed: confirming would replace it, and the person is told.
        assert_eq!(Staging::of(&info("alpha"), &installed), Staging::Replaces);
        assert_eq!(Staging::New.button(), "Install");
        assert_eq!(Staging::Replaces.button(), "Replace");
    }

    #[test]
    fn what_a_plugin_card_offers_follows_what_its_folder_allows_and_then_what_its_process_does() {
        use peersus_core::model::{PluginInfo, PluginStatus, PluginStatus::*, Standing};
        use peersus_core::plugins::PluginId;
        let card = |standing, status, program: Option<&str>| {
            PluginActions::for_plugin(&PluginInfo {
                id: PluginId::new("thing").unwrap(),
                manifest: Default::default(),
                standing,
                status,
                grantable: Default::default(),
                program: program.map(str::to_owned),
                package: None,
            })
        };
        let main = |standing, status: PluginStatus, program| card(standing, status, program).main;
        // May run: its process decides. One that runs can be disabled; one that does not can be enabled.
        for status in [Running, Starting, Restarting] {
            assert_eq!(
                main(Standing::Ready, status, None),
                PluginMain::Disable,
                "{status:?}"
            );
        }
        for status in [Stopped, Failed] {
            assert_eq!(
                main(Standing::Ready, status, None),
                PluginMain::Enable,
                "{status:?}"
            );
        }
        // Only a plugin that is running has anything to restart.
        assert!(card(Standing::Ready, Running, None).can_restart);
        assert!(!card(Standing::Ready, Stopped, None).can_restart);
        assert!(!card(Standing::Ready, Failed, None).can_restart);
        // Switched off: it is enabled, whatever its (stopped) process says.
        assert_eq!(main(Standing::Disabled, Stopped, None), PluginMain::Enable);
        assert!(!card(Standing::Disabled, Stopped, None).can_restart);
        // Waiting or changed: approved, as the digest it was shown, and nothing is switched or restarted.
        for standing in [Standing::NeedsApproval, Standing::Changed] {
            assert_eq!(
                main(standing, Stopped, Some("abc")),
                PluginMain::Approve {
                    program: "abc".into()
                },
                "{standing:?}"
            );
            assert!(!card(standing, Stopped, Some("abc")).can_restart);
            // Nothing was measured: nothing was shown, so there is nothing to approve, and it is not offered.
            assert_eq!(
                main(standing, Stopped, None),
                PluginMain::None,
                "{standing:?} without a digest"
            );
        }
        // Broken: only to be removed, even if a digest happens to be there.
        assert_eq!(
            main(Standing::Broken, Stopped, Some("abc")),
            PluginMain::None
        );
        assert!(
            !card(Standing::Broken, Running, None).can_restart,
            "a broken plugin cannot be restarted"
        );
    }

    #[test]
    fn a_plugin_that_is_not_allowed_to_run_says_so_and_is_never_called_merely_stopped() {
        use peersus_core::model::{PluginInfo, PluginStatus, PluginStatus::*, Standing};
        use peersus_core::plugins::PluginId;
        let info = |standing, status: PluginStatus| PluginInfo {
            id: PluginId::new("thing").unwrap(),
            manifest: Default::default(),
            standing,
            status,
            grantable: Default::default(),
            program: None,
            package: None,
        };
        // What the folder says wins over the process, whatever the process reports.
        for status in [Running, Starting, Restarting, Failed, Stopped] {
            assert_eq!(
                plugin_word(&info(Standing::NeedsApproval, status)),
                ("NEEDS APPROVAL", Tone::Ask),
                "{status:?}"
            );
            assert_eq!(
                plugin_word(&info(Standing::Changed, status)),
                ("CHANGED", Tone::Bad),
                "{status:?}"
            );
            assert_eq!(
                plugin_word(&info(Standing::Broken, status)),
                ("BROKEN", Tone::Bad),
                "{status:?}"
            );
            assert_eq!(
                plugin_word(&info(Standing::Disabled, status)),
                ("OFF", Tone::Quiet),
                "{status:?}"
            );
        }
        // One that may run is described by its process.
        assert_eq!(
            plugin_word(&info(Standing::Ready, Running)),
            ("RUNNING", Tone::Good)
        );
        assert_eq!(
            plugin_word(&info(Standing::Ready, Starting)),
            ("STARTING", Tone::Good)
        );
        assert_eq!(
            plugin_word(&info(Standing::Ready, Restarting)),
            ("RESTARTING", Tone::Bad)
        );
        assert_eq!(
            plugin_word(&info(Standing::Ready, Failed)),
            ("FAILED", Tone::Bad)
        );
        assert_eq!(
            plugin_word(&info(Standing::Ready, Stopped)),
            ("STOPPED", Tone::Bad)
        );
        // A sentence is given exactly where the plugin is not running because of its folder, and each says its own thing.
        let notes: Vec<_> = [Standing::NeedsApproval, Standing::Changed, Standing::Broken]
            .into_iter()
            .map(|standing| {
                plugin_note(&info(standing, Stopped)).expect("no sentence for a plugin held back")
            })
            .collect();
        assert!(
            notes[0].contains("approved")
                && notes[1].contains("forgotten")
                && notes[2].contains("removed"),
            "{notes:?}"
        );
        assert_eq!(
            notes
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3,
            "two states share a sentence"
        );
        assert_eq!(plugin_note(&info(Standing::Ready, Running)), None);
        assert_eq!(plugin_note(&info(Standing::Disabled, Stopped)), None);
    }

    #[test]
    fn with_saving_on_its_own_off_every_file_asks_and_nothing_is_said_to_be_lost() {
        let folder = std::path::Path::new("/home/me/Received");
        // Whatever the folder is like, off asks, and says nothing about a folder: nothing was lost, it is only asked.
        for (chosen, state) in [
            (Some(folder), Folder::Usable),
            (Some(folder), Folder::Unusable),
            (None, Folder::Usable),
        ] {
            assert_eq!(
                destination(false, chosen, state),
                Destination::Ask { lost: None }
            );
        }
    }

    #[test]
    fn a_chosen_folder_that_can_be_used_is_where_the_file_goes() {
        let folder = std::path::Path::new("/home/me/Received");
        assert_eq!(
            destination(true, Some(folder), Folder::Usable),
            Destination::Save(folder.to_path_buf())
        );
    }

    #[test]
    fn a_chosen_folder_that_is_lost_is_asked_about_by_name_and_never_replaced_by_another_place() {
        let folder = std::path::Path::new("/home/me/Received");
        // The file does not go anywhere on its own, and the folder that was chosen is named so the person is told.
        assert_eq!(
            destination(true, Some(folder), Folder::Unusable),
            Destination::Ask {
                lost: Some(folder.to_path_buf())
            }
        );
    }

    #[test]
    fn with_saving_on_but_no_folder_chosen_there_is_nowhere_to_save_to_and_the_file_asks() {
        // There is no default folder: a place nobody chose and nobody can see. Whatever the disk says of "no folder".
        for state in [Folder::Usable, Folder::Unusable] {
            assert_eq!(
                destination(true, None, state),
                Destination::Ask { lost: None }
            );
        }
    }

    #[test]
    fn a_real_writable_folder_is_usable_and_nothing_is_left_behind_in_it() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(look_at(folder.path()), Folder::Usable);
        // Looking made a file to prove it can, and took it away: nothing of the looking stays in the person's folder.
        assert_eq!(
            std::fs::read_dir(folder.path()).unwrap().count(),
            0,
            "the probe was left behind"
        );
    }

    #[test]
    fn a_folder_that_is_gone_or_is_not_a_folder_is_unusable() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(look_at(&folder.path().join("never-made")), Folder::Unusable);
        let file = folder.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            look_at(&file),
            Folder::Unusable,
            "a file was taken for a folder"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_unusable_even_when_it_leads_to_a_folder_that_can_be_written() {
        // A link could lead anywhere but where the person chose, and a file saved through it would land somewhere else.
        let (real, links) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let link = links.path().join("chosen");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        assert_eq!(look_at(real.path()), Folder::Usable);
        assert_eq!(
            look_at(&link),
            Folder::Unusable,
            "a link was used as the chosen folder"
        );
        assert_eq!(
            std::fs::read_dir(real.path()).unwrap().count(),
            0,
            "the link was written through"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_written_to_is_unusable_whatever_it_looks_like() {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().unwrap();
        std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        // Running as the owner without special rights, a folder with no write bit refuses a new file. (As root it would not,
        // so the claim is only made where a file really cannot be made.)
        let can_write = std::fs::File::create(folder.path().join("probe")).is_ok();
        if !can_write {
            assert_eq!(look_at(folder.path()), Folder::Unusable);
        }
        std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn showing_a_code_and_taking_one_are_never_on_screen_together() {
        // Whatever sequence of presses, at most one of the two circles is ever wanted.
        let all = [PairMode::Closed, PairMode::Showing, PairMode::Taking];
        for start in all {
            for next in [start.after_pair(), start.after_connect()] {
                assert!(
                    !(next.shows_code() && next.takes_code()),
                    "{start:?} led to both open"
                );
            }
        }
    }

    #[test]
    fn each_button_closes_the_other_side_and_connect_is_a_switch() {
        // Pair shows the code, and closes the taking side if it was open.
        assert_eq!(PairMode::Closed.after_pair(), PairMode::Showing);
        assert_eq!(PairMode::Taking.after_pair(), PairMode::Showing);
        // Pressed again while shown, it stays shown and hides nothing (whether it asks for a new code is `pair_asks_for_a_code`'s to say).
        assert_eq!(PairMode::Showing.after_pair(), PairMode::Showing);
        // Connect takes a code, and closes the shown one if it was open.
        assert_eq!(PairMode::Closed.after_connect(), PairMode::Taking);
        assert_eq!(PairMode::Showing.after_connect(), PairMode::Taking);
        // Pressed again while open, it closes: a way back to the two buttons.
        assert_eq!(PairMode::Taking.after_connect(), PairMode::Closed);
        // Nothing is open to begin with.
        assert_eq!(PairMode::default(), PairMode::Closed);
        assert!(!PairMode::Closed.shows_code() && !PairMode::Closed.takes_code());
    }

    #[test]
    fn pair_asks_for_a_code_only_when_none_is_on_show_and_a_second_press_does_not_replace_a_valid_one()
     {
        // Nothing shown yet, whichever side was open: the press asks for the code.
        assert!(PairMode::Closed.pair_asks_for_a_code(false));
        assert!(PairMode::Taking.pair_asks_for_a_code(false));
        // Showing and a code is on screen: pressing again changes nothing. This is the press that used to put a new picture under a camera.
        assert!(
            !PairMode::Showing.pair_asks_for_a_code(true),
            "a valid code on show was replaced by a press"
        );
        // Showing, but the code is not there (it ran out, or the owner has not answered yet): it asks, or the circle would stay empty.
        assert!(PairMode::Showing.pair_asks_for_a_code(false));
        // Back from the other side with the old code still held: the mode was not showing, so a new one is asked for, and never the stale one reused.
        assert!(PairMode::Taking.pair_asks_for_a_code(true));
        assert!(PairMode::Closed.pair_asks_for_a_code(true));
    }

    #[test]
    fn the_words_under_the_buttons_name_both_and_say_what_a_file_by_code_is_not() {
        assert!(
            PAIR_EXPLAINS.contains("Pair") && PAIR_EXPLAINS.contains("Connect"),
            "{PAIR_EXPLAINS}"
        );
        assert!(
            PAIR_EXPLAINS.to_lowercase().contains("scan")
                && PAIR_EXPLAINS.to_lowercase().contains("paste")
        );
        // Pairing is the trust; sending a file by code is not.
        assert!(
            PAIR_WARNS.to_lowercase().contains("trust")
                && PAIR_WARNS.to_lowercase().contains("file"),
            "{PAIR_WARNS}"
        );
    }

    #[test]
    fn removing_a_plugin_takes_two_presses_and_the_second_must_be_for_the_same_plugin() {
        use peersus_core::plugins::PluginId;
        let a = PluginId::new("alpha").unwrap();
        let b = PluginId::new("beta").unwrap();
        // Nothing is being removed until the first press.
        assert_eq!(
            RemoveAsk::None.pressed(&a),
            (RemoveAsk::Asking(a.clone()), false)
        );
        // Pressing the same plugin again, while it is being asked about, removes it.
        assert_eq!(
            RemoveAsk::Asking(a.clone()).pressed(&a),
            (RemoveAsk::None, true)
        );
        // Pressing another plugin changes the question to that one and removes nothing.
        assert_eq!(
            RemoveAsk::Asking(a.clone()).pressed(&b),
            (RemoveAsk::Asking(b.clone()), false)
        );
        // Asking can be given up without removing anything.
        assert_eq!(RemoveAsk::Asking(a).cancelled(), RemoveAsk::None);
    }

    #[test]
    fn a_profile_name_that_is_too_long_is_cut_with_an_ellipsis_and_a_short_one_is_left_alone() {
        assert_eq!(Sidebar::fit_name("Alex", 14), "Alex");
        assert_eq!(Sidebar::fit_name("exactly-14-chrs", 15), "exactly-14-chrs");
        // Cut to fit, with the ellipsis counted in the room.
        let cut = Sidebar::fit_name("A very long device name indeed", 14);
        assert_eq!(cut.chars().count(), 14);
        assert!(cut.ends_with('\u{2026}'));
        assert!(cut.starts_with("A very long "));
        // Counted in characters, not bytes: an accented name is not cut in the middle of a letter.
        let accented = Sidebar::fit_name("\u{218}tefan \u{218}tefanescu Popescu", 10);
        assert_eq!(accented.chars().count(), 10);
        assert!(accented.ends_with('\u{2026}'));
        // Nothing at all to show, and a room too small for anything, give an empty name, not a panic.
        assert_eq!(Sidebar::fit_name("", 8), "");
        assert_eq!(Sidebar::fit_name("Alex", 0), "");
        // The initial for the picture: the first letter, in capitals, or a question mark.
        assert_eq!(Sidebar::initial("alex"), 'A');
        assert_eq!(Sidebar::initial("  \u{219}tefan"), '\u{218}');
        assert_eq!(Sidebar::initial(""), '?');
    }

    #[test]
    fn the_sidebar_is_hidden_outside_settings_expands_inside_it_and_keeps_its_icons_if_asked() {
        use peersus_core::config::Navigation::{Expanded, Hidden, Icons};
        // By default it is not there: Send, Receive and Peers are pages without a sidebar.
        assert_eq!(Sidebar::state(false, false), Hidden);
        // Inside Settings it is expanded, whatever the option says.
        assert_eq!(Sidebar::state(true, false), Expanded);
        assert_eq!(Sidebar::state(true, true), Expanded);
        // The one option: outside Settings the sidebar stays as its icons instead of going away.
        assert_eq!(Sidebar::state(false, true), Icons);
    }

    #[test]
    fn the_menu_on_the_settings_button_is_a_shortcut_into_tabs_that_exist() {
        // Not pages of their own: each entry opens Settings on one of its tabs.
        assert_eq!(
            MENU_SHORTCUTS,
            [
                SettingsTab::Overview,
                SettingsTab::Extensions,
                SettingsTab::About
            ]
        );
        for tab in MENU_SHORTCUTS {
            assert!(
                SettingsTab::ALL.contains(&tab),
                "{tab:?} is not a tab of Settings"
            );
        }
    }

    #[test]
    fn the_bar_is_one_width_and_each_state_names_where_it_is_heading() {
        use peersus_core::config::Navigation;
        // Hidden is no width at all, the pictures are the narrow width, expanded is the full one.
        assert_eq!(Sidebar::width_for(Navigation::Hidden), 0.);
        assert_eq!(Sidebar::width_for(Navigation::Icons), Sidebar::NARROW);
        assert_eq!(Sidebar::width_for(Navigation::Expanded), Sidebar::WIDE);
        // Every move is toward one of these, so going from any state to any is one continuous move
        // of the same value, and expanded is reached from hidden and from the pictures alike.
        // The names show only past the pictures' width, and fully at the full one.
        assert_eq!(Sidebar::names_at(0.), 0.);
        assert_eq!(Sidebar::names_at(Sidebar::NARROW), 0.);
        assert_eq!(Sidebar::names_at(Sidebar::WIDE), 1.);
        let between = Sidebar::names_at((Sidebar::NARROW + Sidebar::WIDE) / 2.);
        assert!(between > 0. && between < 1.);
        // Whatever width is handed over, a name is never more or less than fully there or not.
        assert_eq!(Sidebar::names_at(-50.), 0.);
        assert_eq!(Sidebar::names_at(10_000.), 1.);
        // An icon alone fits the narrow width with room around it.
        const { assert!(Sidebar::NARROW >= 48.) };
    }

    #[test]
    fn a_move_of_the_bar_takes_time_in_proportion_to_the_distance_it_has_to_go() {
        let ms = |from: f32, to: f32| Sidebar::duration_ms(from, to, false);
        // The whole way, from nothing to the full width, takes the whole time.
        assert_eq!(ms(0., Sidebar::WIDE), Motion::DURATION_MS);
        assert_eq!(ms(Sidebar::WIDE, 0.), Motion::DURATION_MS);
        // A shorter way is shorter, at the same speed: the pictures to the full width is part of it.
        let part = ms(Sidebar::NARROW, Sidebar::WIDE);
        assert!(part > 0 && part < Motion::DURATION_MS);
        // Turned round in the middle, only the distance that is left is travelled.
        assert_eq!(
            ms(Sidebar::WIDE / 2., Sidebar::WIDE),
            Motion::DURATION_MS / 2
        );
        // Already there is nothing to do, and reduced motion is always at once.
        assert_eq!(ms(Sidebar::NARROW, Sidebar::NARROW), 0);
        assert_eq!(Sidebar::duration_ms(0., Sidebar::WIDE, true), 0);
    }

    #[test]
    fn a_headline_shrinks_with_the_room_it_has_and_never_goes_below_a_readable_size() {
        // The longest line of a headline is "Make files" (10 characters) in a bold, wide face.
        let at = |room: f32| Headline::size_for(room, 10);
        assert_eq!(at(10_000.), Headline::LARGEST);
        // The narrower the room, the smaller: never larger than with more room.
        assert!(at(500.) <= at(700.));
        assert!(at(300.) <= at(500.));
        // The line fits the room it is given, as far as that is possible at all.
        for room in [320., 420., 520., 640., 780.] {
            let size = at(room);
            assert!(
                Headline::width_of(10, size) <= room || size == Headline::SMALLEST,
                "{size} in {room}"
            );
        }
        // And there is a floor, so a very narrow window gets a small title, not an invisible one.
        assert_eq!(at(10.), Headline::SMALLEST);
    }

    #[test]
    fn a_title_on_a_phone_is_held_below_what_the_room_alone_would_allow_and_a_desktop_is_unchanged()
    {
        // The Asus: a page 346 wide minus its margins. "Ready when" has 10 characters.
        let room = 346. - 2. * Form::Compact.page_side_margin();
        let by_room = Headline::size_for(room, 10);
        let on_phone = Headline::size_in(Form::Compact, room, 10);
        assert!(
            on_phone < by_room,
            "the phone's title ({on_phone}) is as large as the room alone makes it ({by_room})"
        );
        assert!(on_phone <= Headline::largest_in(Form::Compact));
        // The desktop is exactly what it was: the ceiling is the old one, so the rule gives the old answer.
        for room in [320., 640., 900., 10_000.] {
            assert_eq!(
                Headline::size_in(Form::Wide, room, 10),
                Headline::size_for(room, 10),
                "room {room}"
            );
        }
        assert_eq!(Headline::largest_in(Form::Wide), Headline::LARGEST);
        // A phone's ceiling is lower than the desktop's, and not under the floor, or a title would not be one.
        assert!(Headline::largest_in(Form::Compact) < Headline::largest_in(Form::Wide));
        assert!(Headline::largest_in(Form::Compact) >= Headline::SMALLEST);
        // A phone turned narrower than its ceiling's room still shrinks the title, down to the floor.
        assert!(
            Headline::size_in(Form::Compact, 150., 10)
                <= Headline::size_in(Form::Compact, 300., 10)
        );
        assert_eq!(
            Headline::size_in(Form::Compact, 10., 10),
            Headline::SMALLEST
        );
    }
    #[test]
    fn a_turn_in_the_middle_takes_only_the_time_of_the_distance_that_is_left() {
        // Open or close all the way: the whole duration.
        assert_eq!(Motion::duration_ms(0., 1., false), Motion::DURATION_MS);
        assert_eq!(Motion::duration_ms(1., 0., false), Motion::DURATION_MS);
        // Turned back at the half: half the distance, half the time, so the speed is the same.
        assert_eq!(Motion::duration_ms(0.5, 0., false), Motion::DURATION_MS / 2);
        assert_eq!(
            Motion::duration_ms(0.25, 1., false),
            Motion::DURATION_MS * 3 / 4
        );
        // Already there: nothing to do.
        assert_eq!(Motion::duration_ms(1., 1., false), 0);
        // And with motion reduced it is always at once, whatever the distance.
        assert_eq!(Motion::duration_ms(0., 1., true), 0);
    }

    #[test]
    fn a_menu_is_mounted_while_it_is_open_or_still_closing_and_only_then() {
        // Open: shown. Closing and not yet at its end: still shown, or it would vanish at once.
        assert!(Motion::mounted(true, 0.0));
        assert!(Motion::mounted(false, 0.4));
        // Closed and arrived at nothing: gone.
        assert!(!Motion::mounted(false, 0.0));
    }

    #[test]
    fn what_is_dropped_on_the_extensions_page_is_one_package_or_nothing_and_says_why() {
        use std::path::PathBuf;
        let one = |kind| Install::of_drop(vec![PathBuf::from("/a/clip")], |_| kind);
        // A folder and a file are both packages: the owner tells which by looking at the path.
        assert_eq!(
            one(DropKind::Folder),
            Install::Take(PathBuf::from("/a/clip"))
        );
        assert_eq!(
            one(DropKind::File { length: 10 }),
            Install::Take(PathBuf::from("/a/clip"))
        );
        // Nothing to install in an empty file, a path that is gone, or something that is neither.
        assert_eq!(
            one(DropKind::File { length: 0 }),
            Install::Refuse(Install::EMPTY)
        );
        assert_eq!(
            one(DropKind::Unreadable),
            Install::Refuse(Install::UNREADABLE)
        );
        assert_eq!(
            one(DropKind::Other),
            Install::Refuse(Install::NOT_A_PACKAGE)
        );
        // Nothing dropped is not an error to shout about: there is simply nothing to do.
        assert_eq!(
            Install::of_drop(Vec::new(), |_| DropKind::Folder),
            Install::Nothing
        );
        // Two or more at once is ambiguous (which one is the package?), so none is taken.
        let two = vec![PathBuf::from("/a/one"), PathBuf::from("/a/two")];
        assert_eq!(
            Install::of_drop(two, |_| DropKind::Folder),
            Install::Refuse(Install::ONE_AT_A_TIME)
        );
        // The same path twice is still one package.
        let same = vec![PathBuf::from("/a/clip"), PathBuf::from("/a/clip")];
        assert_eq!(
            Install::of_drop(same, |_| DropKind::Folder),
            Install::Take(PathBuf::from("/a/clip"))
        );
    }

    #[test]
    fn the_interface_follows_orientation_portrait_is_compact_and_landscape_or_square_is_wide() {
        // A phone held upright, in the sizes phones come in.
        for (width, height) in [
            (360., 780.),
            (393., 852.),
            (412., 915.),
            (430., 932.),
            (320., 568.),
        ] {
            assert_eq!(
                Form::of(width, height),
                Form::Compact,
                "{width} x {height} is upright"
            );
        }
        // The same phone on its side is landscape, and is not given the portrait arrangement: the
        // width alone (900 here) would have said wide too, but 360 on its side would not.
        for (width, height) in [(780., 360.), (915., 412.), (932., 430.)] {
            assert_eq!(
                Form::of(width, height),
                Form::Wide,
                "{width} x {height} is on its side"
            );
        }
        // The desktop window, at its smallest and its usual size, is landscape.
        for (width, height) in [(760., 560.), (1100., 720.), (1920., 1080.)] {
            assert_eq!(
                Form::of(width, height),
                Form::Wide,
                "{width} x {height} is a desktop window"
            );
        }
        // Square is not taller than wide, so it is not portrait.
        assert_eq!(Form::of(600., 600.), Form::Wide);
        // A narrow window is not a phone unless it is also taller than it is wide, and the one that is
        // taller than wide is given the arrangement made for that shape, narrow or not.
        assert_eq!(Form::of(500., 400.), Form::Wide);
        assert_eq!(Form::of(800., 900.), Form::Compact);
        // Not yet measured, or not numbers: the desktop arrangement, never a flash of the other.
        for unknown in [0., -1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                Form::of(unknown, 800.),
                Form::Wide,
                "width {unknown} was taken for a phone"
            );
            assert_eq!(
                Form::of(400., unknown),
                Form::Wide,
                "height {unknown} was taken for a phone"
            );
        }
    }

    #[test]
    fn the_phone_shows_the_circle_alone_with_all_four_places_at_the_bottom_and_the_desktop_none_of_that()
     {
        // On a phone: Send is the circle, and every place has a tab at the bottom, in the order the desktop has
        // them across the top. Without Send there would be no way back to it from the others.
        assert_eq!(
            Form::Compact.bottom_places(),
            &[
                BottomPlace::Send,
                BottomPlace::Receive,
                BottomPlace::Peers,
                BottomPlace::Transfers
            ]
        );
        let places = Form::Compact.bottom_places();
        for (index, place) in places.iter().enumerate() {
            assert!(
                !places[..index].contains(place),
                "{place:?} is in the bar twice"
            );
        }
        assert!(Form::Compact.send_is_the_circle_alone());
        assert!(!Form::Compact.profile_shows_name());
        assert_eq!(Form::Compact.circle_invitation(), "tap to choose files");
        // On the desktop nothing changes: the places stay across the top.
        assert!(Form::Wide.bottom_places().is_empty());
        assert!(!Form::Wide.send_is_the_circle_alone());
        assert!(Form::Wide.profile_shows_name());
        assert_eq!(
            Form::Wide.circle_invitation(),
            "click to browse your computer"
        );
        // A phone is touched, so it never says "click" or names a computer.
        let phone = Form::Compact.circle_invitation();
        assert!(!phone.contains("click") && !phone.contains("computer"));
    }

    #[test]
    fn the_phone_header_keeps_only_what_a_phone_has_room_and_use_for() {
        // A phone has no window of its own to minimise or close, and says connected by its dot alone.
        assert!(!Form::Compact.header_has_window_buttons());
        assert!(!Form::Compact.header_says_connected_in_words());
        // The desktop header is exactly what it was.
        assert!(Form::Wide.header_has_window_buttons());
        assert!(Form::Wide.header_says_connected_in_words());
        // Only a phone gives its short screen back by hiding the bar while reading down; the desktop keeps it.
        assert!(Form::Compact.header_hides_on_scroll());
        assert!(!Form::Wide.header_hides_on_scroll());
        // Less room at the sides on a phone, and a side never negative or empty.
        assert!(Form::Compact.header_side_padding() < Form::Wide.header_side_padding());
        assert!(Form::Compact.header_side_padding() > 0.);
        assert_eq!(Form::Wide.header_side_padding(), 28.);
    }

    #[test]
    fn the_portal_is_never_wider_than_the_room_it_has_and_is_its_own_size_when_there_is_room() {
        // The narrowest phones, and one wide enough: the portal is cut to the width, or left as it is.
        assert_eq!(Form::portal_extent(420., 360.), 360.);
        assert_eq!(Form::portal_extent(420., 412.), 412.);
        assert_eq!(Form::portal_extent(420., 430.), 420.);
        assert_eq!(Form::portal_extent(420., 1100.), 420.);
        // The width itself is the limit, not a little under it: equal means it fits exactly.
        assert_eq!(Form::portal_extent(420., 420.), 420.);
        // A width that is not yet known, or not a number: the portal keeps its own size.
        for unknown in [0., -1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                Form::portal_extent(420., unknown),
                420.,
                "width {unknown} changed the portal"
            );
        }
    }

    #[test]
    fn the_line_round_the_disc_is_whole_device_pixels_and_never_thinner_than_two() {
        // Whatever the screen, what is drawn is a whole number of device pixels, so it lands on the pixels and not between them.
        for scale in [1., 1.5, 2., 2.625, 3., 3.125, 3.5, 4.] {
            let device = f64::from(Form::portal_line(scale)) * scale;
            assert!(
                (device - device.round()).abs() < 1e-3,
                "at {scale} the line is {device} device pixels, not whole"
            );
            assert!(
                device.round() >= 2.,
                "at {scale} the line is {device} device pixels, thinner than two"
            );
        }
        // The phone this was measured on (a scale of 3.125): one logical pixel there is 3.125 device pixels and went missing at
        // times; the line is now five.
        assert!((f64::from(Form::portal_line(3.125)) * 3.125 - 5.).abs() < 1e-3);
        // A screen of the plain kind keeps it at two device pixels.
        assert_eq!(Form::portal_line(1.), 2.);
        // A scale that cannot be used gives what was measured to hold, not nothing and not a number that is not one.
        for unusable in [0., -2., f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                Form::portal_line(unusable),
                2.,
                "scale {unusable} changed the line"
            );
        }
    }

    #[test]
    fn the_naming_screen_is_smaller_and_stacked_on_a_phone_and_unchanged_on_a_desktop() {
        // The title is smaller on a phone, where 56 px does not fit "Name this device" on a line.
        assert!(Form::Compact.ask_title_size() < Form::Wide.ask_title_size());
        assert_eq!(Form::Wide.ask_title_size(), 56.);
        // Field and button stand one above the other on a phone only; the desktop keeps them side by side.
        assert!(Form::Compact.ask_stacks_field_and_button());
        assert!(!Form::Wide.ask_stacks_field_and_button());
        assert_eq!(Form::Wide.ask_side_margin(), 0.);
        assert!(Form::Compact.ask_side_margin() > 0.);
    }

    #[test]
    fn a_page_keeps_a_small_margin_on_a_phone_and_its_wide_one_on_a_desktop_and_stacks_its_portal()
    {
        // 56 px at each side takes 112 of a 346-wide screen: a third. On a phone the margin is a fraction of it.
        assert!(
            Form::Compact.page_side_margin() * 2. < 346. / 6.,
            "a phone's margins take too much of its width"
        );
        assert!(Form::Compact.page_side_margin() > 0.);
        // The desktop is exactly what it was.
        assert_eq!(Form::Wide.page_side_margin(), 56.);
        // Text and portal side by side only where there is the width for both.
        assert!(Form::Wide.page_puts_portal_beside_text());
        assert!(!Form::Compact.page_puts_portal_beside_text());
    }

    #[test]
    fn the_safe_area_is_what_the_system_leaves_free_divided_by_the_screens_own_scale() {
        // The Asus, measured on the device: 1080 x 2400 and a status bar of 105 px at the top. The scale is a
        // parameter, as it is in the program: here an example of one, not the phone's.
        let scale = 3.125;
        let area = SafeArea::from_physical((0, 105, 1080, 2400), (1080, 2400), scale);
        assert!(
            (area.top - 105. / scale as f32).abs() < 1e-3,
            "the status bar was {} logical px",
            area.top
        );
        assert_eq!((area.left, area.right, area.bottom), (0., 0., 0.));
        // The same status bar on a screen of lower density is taller in logical pixels: the answer follows
        // the scale and is not a number kept in the program.
        let coarse = SafeArea::from_physical((0, 105, 1080, 2400), (1080, 2400), 1.0);
        assert!((coarse.top - 105.).abs() < 1e-3);
        assert!(coarse.top > area.top);
        // A navigation bar below and a notch at one side are carried the same way.
        let both = SafeArea::from_physical((60, 105, 1080, 2300), (1080, 2400), 2.0);
        assert_eq!(
            (both.top, both.left, both.right, both.bottom),
            (52.5, 30.0, 0.0, 50.0)
        );
    }

    #[test]
    fn a_rectangle_that_cannot_be_right_leaves_no_margin_rather_than_a_wrong_one() {
        let none = SafeArea::default();
        let screen = (1080, 2400);
        // Not yet given by the system: all zero, which is what the Android entry reads at its very start.
        assert_eq!(SafeArea::from_physical((0, 0, 0, 0), screen, 3.0), none);
        // Empty, inverted, outside the screen, or negative.
        assert_eq!(
            SafeArea::from_physical((0, 105, 1080, 105), screen, 3.0),
            none
        );
        assert_eq!(
            SafeArea::from_physical((500, 105, 100, 2400), screen, 3.0),
            none
        );
        assert_eq!(
            SafeArea::from_physical((0, 105, 1081, 2400), screen, 3.0),
            none
        );
        assert_eq!(
            SafeArea::from_physical((0, 105, 1080, 2401), screen, 3.0),
            none
        );
        assert_eq!(
            SafeArea::from_physical((-1, 105, 1080, 2400), screen, 3.0),
            none
        );
        assert_eq!(
            SafeArea::from_physical((0, -5, 1080, 2400), screen, 3.0),
            none
        );
        // A scale that is not a positive number cannot divide anything.
        for scale in [0., -1., f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                SafeArea::from_physical((0, 105, 1080, 2400), screen, scale),
                none,
                "scale {scale}"
            );
        }
        // And the whole screen is no margin at all.
        assert_eq!(
            SafeArea::from_physical((0, 0, 1080, 2400), screen, 3.0),
            none
        );
    }

    #[test]
    fn while_the_device_has_no_name_there_is_no_sidebar_and_no_way_into_a_page() {
        use peersus_core::config::Navigation;
        // Inside Settings, asked for, with every option, on either form: without a name the sidebar is not
        // there at all. This is the state in which the menu's Settings stood beside the question and covered it.
        for form in [Form::Wide, Form::Compact] {
            for keep_icons in [false, true] {
                for inside_settings in [false, true] {
                    assert_eq!(
                        Sidebar::state_for(false, inside_settings, keep_icons, form),
                        Navigation::Hidden,
                        "a sidebar was shown beside the question ({form:?}, settings {inside_settings}, icons {keep_icons})"
                    );
                }
            }
        }
        // With a name, on a desktop, nothing is different from before: it is exactly the rule that was.
        for keep_icons in [false, true] {
            for inside_settings in [false, true] {
                assert_eq!(
                    Sidebar::state_for(true, inside_settings, keep_icons, Form::Wide),
                    Sidebar::state(inside_settings, keep_icons)
                );
            }
        }
        // Opening a page from the menu leads nowhere until there is a name, and to the page after.
        assert_eq!(Sidebar::page_opened(false, "settings"), None);
        assert_eq!(Sidebar::page_opened(true, "settings"), Some("settings"));
    }

    #[test]
    fn a_phones_page_is_closer_above_and_between_its_sections_and_a_desktops_is_what_it_was() {
        // The desktop is exactly what it was before this rule existed.
        assert_eq!(Form::Wide.page_vertical_padding(), 28.);
        assert_eq!(Form::Wide.page_section_gap(), 28.);
        // A phone is closer, but never touching and never negative: a page is still a page.
        assert!(Form::Compact.page_vertical_padding() < Form::Wide.page_vertical_padding());
        assert!(Form::Compact.page_section_gap() < Form::Wide.page_section_gap());
        assert!(Form::Compact.page_vertical_padding() > 0.);
        assert!(Form::Compact.page_section_gap() > 0.);
    }

    #[test]
    fn on_a_phone_settings_has_no_bar_at_the_side_and_its_places_come_down_from_the_top() {
        use peersus_core::config::Navigation;
        // The desktop shows names beside the pictures in Settings.
        assert_eq!(
            Sidebar::state_for(true, true, false, Form::Wide),
            Navigation::Expanded
        );
        assert_eq!(
            Sidebar::state_for(true, true, true, Form::Wide),
            Navigation::Expanded
        );
        // A phone has no room for a bar at the side, and so none: its places are a sheet that comes down from the top.
        assert_eq!(
            Sidebar::state_for(true, true, false, Form::Compact),
            Navigation::Hidden
        );
        assert!(Form::Compact.settings_places_come_from_the_top());
        assert!(!Form::Wide.settings_places_come_from_the_top());
        // Whoever turned the option for icons on keeps them, on the phone as on the desktop: that was asked for.
        assert_eq!(
            Sidebar::state_for(true, true, true, Form::Compact),
            Navigation::Icons
        );
        // Outside Settings the phone has no sidebar, as the desktop has none: nothing else is touched.
        assert_eq!(
            Sidebar::state_for(true, false, false, Form::Compact),
            Navigation::Hidden
        );
        assert_eq!(
            Sidebar::state_for(true, false, true, Form::Compact),
            Navigation::Icons
        );
        // What the phone's bar is wide enough for, and what the page keeps of a 346-wide screen.
        assert!(Sidebar::width_for(Navigation::Icons) < 346. / 4.);
        assert!(
            Sidebar::width_for(Navigation::Expanded) > 346. / 2.5,
            "the desktop's bar is no longer too wide for a phone, so this rule is not needed"
        );
    }

    #[test]
    fn the_sheet_from_the_top_starts_wholly_above_the_screen_and_ends_in_its_place() {
        let (height, top) = (500., 24.);
        // Closed, its lower edge is at the top of the usable area and nothing of it shows.
        assert_eq!(TopSheet::top(0., height, top), top - height);
        // Open, it is exactly in place.
        assert_eq!(TopSheet::top(1., height, top), top);
        // It never goes above the closed position nor below the open one, whatever progress it is handed.
        assert_eq!(TopSheet::top(-4., height, top), top - height);
        assert_eq!(TopSheet::top(7., height, top), top);
        // And it only ever goes one way: down while opening.
        let mut last = f32::MIN;
        for step in 0..=20 {
            let now = TopSheet::top(step as f32 / 20., height, top);
            assert!(now >= last, "the sheet went back up at step {step}");
            last = now;
        }
    }

    #[test]
    fn the_dark_layer_under_the_sheet_grows_with_it_and_never_hides_the_page_completely() {
        assert_eq!(TopSheet::scrim(0.), 0);
        assert!(
            TopSheet::scrim(1.) > 0 && TopSheet::scrim(1.) < 255,
            "the page must stay visible under the layer"
        );
        assert!(TopSheet::scrim(0.5) < TopSheet::scrim(1.));
        assert_eq!(TopSheet::scrim(-3.), 0);
        assert_eq!(TopSheet::scrim(9.), TopSheet::scrim(1.));
    }

    #[test]
    fn the_sheet_is_never_taller_than_most_of_the_screen_it_comes_down_on() {
        for room in [320., 640., 900.] {
            let height = TopSheet::height(room);
            assert!(height <= room * 0.9 + 0.01, "{height} of {room}");
            assert!(height > 0.);
        }
        assert_eq!(TopSheet::height(0.), 0.);
        assert_eq!(TopSheet::height(-5.), 0.);
    }

    #[test]
    fn turning_the_relays_on_is_accepted_even_when_an_address_was_fixed_and_returns_routing_to_the_systems()
     {
        use peersus_core::config::SettingsPatch;
        // The case in the screenshot: routing fixed to one local address (as the command line can do), so relays
        // are off. Asking for them on with only "relays on" was refused: the owner will not hold both.
        let mut fixed = Settings::default();
        fixed.network.bind = Some("192.168.1.20".parse().unwrap());
        fixed.network.allow_relay = false;
        assert!(
            fixed.validate().is_ok(),
            "the starting point is not itself valid"
        );
        let only_relays = SettingsPatch {
            allow_relay: Some(true),
            ..SettingsPatch::default()
        };
        assert!(
            only_relays.apply(&fixed).is_err(),
            "the premise is gone: relays alone are no longer refused"
        );
        // What the switch sends now is accepted, and leaves routing the system's and the relays on.
        let accepted = relay_patch(true)
            .apply(&fixed)
            .expect("the relay switch was refused with an address fixed");
        assert!(accepted.network.allow_relay);
        assert_eq!(
            accepted.network.bind, None,
            "routing was not returned to the system's"
        );
        // On a device with nothing fixed it changes nothing but the relays, which are on by default.
        let plain = relay_patch(true).apply(&Settings::default()).unwrap();
        assert_eq!(plain, Settings::default());
        // Off changes only the relays, and leaves an address that was fixed exactly where it was.
        let mut routed = Settings::default();
        routed.network.bind = Some("192.168.1.20".parse().unwrap());
        routed.network.allow_relay = false;
        let off = relay_patch(false).apply(&routed).unwrap();
        assert!(!off.network.allow_relay);
        assert_eq!(
            off.network.bind, routed.network.bind,
            "turning the relays off moved the address"
        );
        // And the relays are on by default: the internet is what a new device reaches.
        assert!(Settings::default().network.allow_relay);
        assert_eq!(Settings::default().network.bind, None);
    }

    #[test]
    fn desktop_batches_preserve_every_selected_path_in_owner_sized_commands() {
        let paths: Vec<_> = (0..40)
            .map(|number| std::path::PathBuf::from(format!("selected-{number}.bin")))
            .collect();
        let batches: Vec<_> = path_sends(paths.clone()).collect();
        assert!(
            batches.iter().all(|batch| (1..=16).contains(&batch.len())),
            "desktop selection exceeded command capacity"
        );
        assert_eq!(
            batches.into_iter().flatten().collect::<Vec<_>>(),
            paths,
            "desktop selection was dropped, duplicated or reordered"
        );
        assert!(path_sends(Vec::new()).next().is_none());
    }

    #[test]
    fn the_files_a_picker_hands_over_are_sent_to_the_chosen_device_in_batches_under_cleaned_names()
    {
        let held: Vec<(u64, String)> = (0..40).map(|n| (n, format!("../f{n}.bin"))).collect();
        let plan = held_sends(&held, "laptop");
        // Never more in one command than the owner accepts, and every file is in exactly one command.
        assert!(
            plan.iter().all(|files| (1..=16).contains(&files.len())),
            "a batch the owner would refuse"
        );
        assert_eq!(plan.iter().map(Vec::len).sum::<usize>(), 40);
        let numbers: Vec<u64> = plan.iter().flatten().map(|(number, _)| *number).collect();
        assert_eq!(
            numbers,
            (0..40).collect::<Vec<_>>(),
            "files were lost, repeated or put out of order"
        );
        // The name that goes is the cleaned one: the provider's own name is not trusted, and a path in it is dropped.
        assert!(
            plan.iter()
                .flatten()
                .all(|(_, name)| !name.contains('/') && name.ends_with(".bin"))
        );
        assert_eq!(plan[0][0].1, "f0.bin");
        // One file is one batch of one; none is none.
        assert_eq!(held_sends(&[(7, "a.jpg".to_owned())], "x").len(), 1);
        assert!(held_sends(&[], "x").is_empty());
    }

    #[test]
    fn a_name_the_provider_did_not_give_is_a_plain_one_and_not_an_empty_offer() {
        let plan = held_sends(
            &[
                (1, String::new()),
                (2, "..".to_owned()),
                (3, "  ".to_owned()),
            ],
            "x",
        );
        let names: Vec<&str> = plan
            .iter()
            .flatten()
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(
            names,
            ["file", "file", "file"],
            "a file with no name was offered under an empty or dotted one"
        );
    }

    #[test]
    fn a_device_card_keeps_its_width_where_cards_stand_side_by_side_and_none_where_they_would_not_fit()
     {
        // The desktop is as it was.
        assert_eq!(Form::Wide.device_card_width(), Some(280.));
        // A phone keeps none: the card is as wide as the screen, with the cards one under another.
        assert_eq!(Form::Compact.device_card_width(), None);
        // Why: two cards of the desktop's width, with the gap between them, are wider than the narrowest phones
        // in common use, so the second would be pushed off the screen.
        let own = Form::Wide.device_card_width().unwrap();
        for screen in [320., 345.6, 360., 412.] {
            assert!(
                own * 2. + 14. + 2. * Form::Compact.page_side_margin() > screen - 1.,
                "two cards fit {screen}: the rule is no longer needed"
            );
        }
    }

    #[test]
    fn the_buttons_of_a_device_card_stand_under_its_name_on_a_phone_and_beside_it_on_the_desktop() {
        // The desktop is exactly as it was: the buttons in the row of the card.
        assert!(!Form::Wide.device_card_buttons_stack());
        // A phone has no room for them beside the name, which would be left a column of one letter.
        assert!(Form::Compact.device_card_buttons_stack());
        // The same shape as the rule for a field and its button, so the two cannot disagree about what a phone is.
        for form in [Form::Wide, Form::Compact] {
            assert_eq!(
                form.device_card_buttons_stack(),
                form.ask_stacks_field_and_button()
            );
        }
    }

    #[test]
    fn every_look_of_the_watcher_at_the_camera_makes_the_state_it_should() {
        let code = || Some("peersus:1:ab".to_owned());
        // Shut: nothing the camera says matters, since no one asked it anything. Nothing is joined, nothing closed.
        for (found, refused) in [(None, false), (None, true), (code(), false), (code(), true)] {
            let tick = look(Looking::Shut, found, refused);
            assert_eq!(
                tick,
                Tick {
                    looking: Looking::Shut,
                    join: None,
                    close: false
                }
            );
        }
        // Scanning, nothing yet: it goes on, and the camera stays open.
        assert_eq!(
            look(Looking::Scanning, None, false),
            Tick {
                looking: Looking::Scanning,
                join: None,
                close: false
            }
        );
        // Scanning and a code is read: joined, and the camera is closed, so that it does not read the same code again.
        assert_eq!(
            look(Looking::Scanning, code(), false),
            Tick {
                looking: Looking::Shut,
                join: code(),
                close: true
            }
        );
        // Scanning and refused: said in the circle, and the camera is closed.
        assert_eq!(
            look(Looking::Scanning, None, true),
            Tick {
                looking: Looking::Refused,
                join: None,
                close: true
            }
        );
        // A code and a refusal together: the code wins, since the person did get a code.
        assert_eq!(
            look(Looking::Scanning, code(), true),
            Tick {
                looking: Looking::Shut,
                join: code(),
                close: true
            }
        );
        // Refused already: it stays so with nothing new, and a code that comes late is still joined.
        assert_eq!(
            look(Looking::Refused, None, true),
            Tick {
                looking: Looking::Refused,
                join: None,
                close: false
            }
        );
        assert_eq!(
            look(Looking::Refused, code(), false),
            Tick {
                looking: Looking::Shut,
                join: code(),
                close: true
            }
        );
    }

    #[test]
    fn while_the_camera_is_shown_the_circle_has_no_words_and_the_other_states_say_what_to_do() {
        // A scan with a picture of the camera: the camera, and no words over it; whatever the state is before.
        assert_eq!(circle_for(Looking::Scanning, true), Circle::Camera);
        // A scan with no picture yet: a clear circle with one small line, never a headline like "looking".
        match circle_for(Looking::Scanning, false) {
            Circle::Starting(line) => {
                assert!(!line.is_empty() && !line.to_lowercase().contains("looking"))
            }
            other => panic!("a scan with no picture yet was {other:?}"),
        }
        // Shut: an invitation to tap, in words, whether or not a picture is left over from before.
        for has_picture in [false, true] {
            match circle_for(Looking::Shut, has_picture) {
                Circle::Words(headline, line) => {
                    assert!(!headline.is_empty() && line.contains("tap"))
                }
                other => panic!("a shut camera was {other:?}"),
            }
            // Told no: where to put it right, and a picture left from before does not hide it.
            match circle_for(Looking::Refused, has_picture) {
                Circle::Words(_, line) => assert!(line.contains("settings")),
                other => panic!("a refused camera was {other:?}"),
            }
        }
        // The shut and the refused words are not the same words.
        assert_ne!(
            circle_for(Looking::Shut, false),
            circle_for(Looking::Refused, false)
        );
        assert_eq!(Looking::default(), Looking::Shut);
    }

    #[test]
    fn of_the_files_chosen_for_a_picture_the_first_is_used_and_every_other_is_let_go_of() {
        let file = |number: u64| (number, format!("{number}.png"));
        // One: it is used and nothing is left over.
        assert_eq!(
            picture_choice(&[file(7)]),
            Some(((7, "7.png".to_owned()), Vec::new()))
        );
        // Several (a provider may hand over more than was asked for): the first in the order they came is used, and
        // each of the others, which nobody will take, is to be released, none is lost and none is used twice.
        let (used, rest) = picture_choice(&[file(3), file(9), file(4)]).unwrap();
        assert_eq!(used, file(3));
        assert_eq!(rest, vec![file(9), file(4)]);
        // None: nothing to use and nothing to let go of.
        assert_eq!(picture_choice(&[]), None);
    }

    #[test]
    fn stopping_ends_a_scan_that_is_on_and_does_nothing_to_one_that_is_not() {
        // On: back to shut, with the camera closed, and nothing joined.
        assert_eq!(
            stop(Looking::Scanning),
            Tick {
                looking: Looking::Shut,
                join: None,
                close: true
            }
        );
        // Told no: stop is also the way back, closing what may still be open and returning to the invitation to tap.
        assert_eq!(
            stop(Looking::Refused),
            Tick {
                looking: Looking::Shut,
                join: None,
                close: true
            }
        );
        // Shut already: there is nothing to close, and closing a camera that is not open is not asked of the platform.
        assert_eq!(
            stop(Looking::Shut),
            Tick {
                looking: Looking::Shut,
                join: None,
                close: false
            }
        );
        // Nothing is ever joined by stopping.
        for now in [Looking::Shut, Looking::Scanning, Looking::Refused] {
            assert_eq!(stop(now).join, None);
        }
    }

    fn row(id: &str, incoming: bool, phase: Phase, from: Option<&str>) -> Progress {
        Progress {
            id: id.into(),
            peer: format!("peer-{id}"),
            name: format!("{id}.bin"),
            folders: Vec::new(),
            bytes: 0,
            incoming,
            from: from.map(str::to_owned),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn the_totals_count_only_files_that_moved_whole_and_say_which_way() {
        let view = View {
            transfers: vec![
                row("a", false, Phase::Done, None),
                row("b", false, Phase::Done, None),
                row("c", true, Phase::Done, None),
                // Not moved: refused, cancelled, failed, or still going. None of them is a file that went anywhere.
                row("d", false, Phase::Cancelled, None),
                row("e", true, Phase::Failed("x".into()), None),
                row("f", true, Phase::Moving, None),
                row("g", true, Phase::Waiting, None),
            ],
            ..View::default()
        };
        assert_eq!(
            view.file_totals(),
            FileTotals {
                sent: 2,
                received: 1,
                total: 3
            }
        );
        assert_eq!(
            View::default().file_totals(),
            FileTotals {
                sent: 0,
                received: 0,
                total: 0
            }
        );
    }

    #[test]
    fn every_row_of_the_search_index_is_a_message_of_the_catalog() {
        // The index is looked in what a person reads, so each title and each note must be a text that the catalog has; one that is not is
        // looked for in English in every language, and drifts from the page that says it.
        for entry in SETTING_ENTRIES {
            assert!(
                i18n::has(entry.title),
                "the title {:?} is not in the catalog",
                entry.title
            );
            assert!(
                i18n::has(entry.note),
                "the note of {:?} is not in the catalog: {:?}",
                entry.title,
                entry.note
            );
        }
    }

    #[test]
    fn a_few_letters_in_order_find_a_row_and_a_wrong_order_does_not() {
        assert!(fuzzy_score("bkg", "Run in the background").is_some());
        assert!(fuzzy_score("RUN", "Run in the background").is_some());
        assert!(
            fuzzy_score("gkb", "Run in the background").is_none(),
            "letters out of order matched"
        );
        assert!(fuzzy_score("zzz", "Run in the background").is_none());
        // Nothing typed is no search: not a list of everything.
        assert_eq!(fuzzy_score("", "Run in the background"), None);
        assert_eq!(fuzzy_score("   ", "Run in the background"), None);
    }

    #[test]
    fn a_match_that_reads_as_the_words_beats_one_scattered_across_them() {
        // "round" read straight off "background", and the same five letters found one here and one there inside other words: none of the
        // scattered ones begins a word, so only the straight run is rewarded.
        let straight = fuzzy_score("round", "Run in the background").unwrap();
        let scattered = fuzzy_score("round", "aaroaubanadoo").unwrap();
        assert!(straight > scattered, "{straight} should beat {scattered}");
    }

    #[test]
    fn the_search_leads_to_the_tab_of_the_row_and_puts_a_title_before_a_note() {
        let found = search_settings("reduce", &View::default(), Language::English);
        assert_eq!(
            found.first().map(|entry| entry.title.as_str()),
            Some("Reduce motion")
        );
        assert_eq!(
            found.first().map(|entry| entry.target.tab()),
            Some(SettingsTab::Appearance)
        );
        // Nothing typed: nothing found.
        assert!(search_settings("", &View::default(), Language::English).is_empty());
        let romanian = search_settings("limb", &View::default(), Language::Romanian);
        assert!(
            romanian.iter().any(|entry| entry.title == "Limbă"),
            "the translated language row was not found"
        );
    }

    #[test]
    fn settings_search_can_reach_every_group() {
        for tab in SettingsTab::ALL {
            assert!(
                search_settings(tab.title(), &View::default(), Language::English)
                    .iter()
                    .any(|entry| entry.target.tab() == tab),
                "search could not reach {tab:?}"
            );
        }
    }

    /// An outgoing transfer that came of the files code of generation `code`, `percent` of the way.
    fn of_code(id: &str, code: u64, phase: Phase, percent: u8) -> Progress {
        Progress {
            code: Some(code),
            percent,
            ..row(id, false, phase, None)
        }
    }
    fn on_show(generation: u64, live: bool) -> Option<FilesCode> {
        Some(FilesCode {
            generation,
            live,
            pending: false,
        })
    }

    fn arrived(id: &str, at: &str) -> Progress {
        Progress {
            saved_as: Some(std::path::PathBuf::from(at)),
            ..row(id, true, Phase::Done, None)
        }
    }

    /// A transfer of the device `peer`, in the one list that every page reads.
    fn with(id: &str, peer: &str, name_of_peer: &str, phase: Phase) -> Progress {
        Progress {
            peer: peer.into(),
            from: Some(name_of_peer.into()),
            name: format!("{id}.txt"),
            ..row(id, false, phase, None)
        }
    }

    #[test]
    fn the_history_of_a_device_is_what_was_with_that_identity_newest_first_and_not_with_one_of_the_same_name()
     {
        let list = [
            with("a", "peer-1", "Phone", Phase::Done),
            with("b", "peer-2", "Phone", Phase::Done),
            with("c", "peer-1", "Phone", Phase::Done),
        ];
        let names = |peer: &str| {
            history_with(peer, &list)
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names("peer-1"),
            ["c", "a"],
            "two devices of one name were mixed together, or the newest was not first"
        );
        assert_eq!(names("peer-2"), ["b"]);
        assert!(
            names("peer-3").is_empty(),
            "a device with no history got someone else's"
        );
    }

    #[test]
    fn a_device_has_no_history_of_its_own_once_the_list_forgets_what_was_with_it() {
        // ONE list: Send, Receive, Settings and a device's page all read it. When Clear forgets what is over there is nothing left
        // under the device either, and a device met only by a code is not listed any more.
        let before = [
            with("a", "peer-1", "Phone", Phase::Done),
            with("b", "peer-1", "Phone", Phase::Failed("x".into())),
        ];
        assert_eq!(history_with("peer-1", &before).len(), 2);
        assert_eq!(unpaired_with_history(&[], &before).len(), 1);
        let after: Vec<Progress> = Vec::new();
        assert!(
            history_with("peer-1", &after).is_empty(),
            "a device kept a history the list had forgotten"
        );
        assert!(
            unpaired_with_history(&[], &after).is_empty(),
            "a device with nothing in the list was still listed"
        );
    }

    #[test]
    fn what_is_still_going_with_a_device_is_not_a_line_of_its_history() {
        let list = [
            with("a", "peer-1", "Phone", Phase::Done),
            with("b", "peer-1", "Phone", Phase::Moving),
            with("c", "peer-1", "Phone", Phase::Waiting),
        ];
        let ids: Vec<_> = history_with("peer-1", &list)
            .iter()
            .map(|item| item.id.clone())
            .collect();
        assert_eq!(ids, ["a"], "a transfer still going was listed as history");
        // A device with only something going is not a device met by a code with a history yet.
        assert!(unpaired_with_history(&[], &list[1..]).is_empty());
    }

    #[test]
    fn a_device_page_names_its_latest_five_and_says_how_many_more_there_are() {
        let many: Vec<Progress> = (0..8)
            .map(|n| with(&format!("t{n}"), "peer-1", "Phone", Phase::Done))
            .collect();
        let (shown, more) = history_shown("peer-1", &many);
        let ids: Vec<_> = shown.iter().map(|item| item.id.clone()).collect();
        assert_eq!(
            ids,
            ["t7", "t6", "t5", "t4", "t3"],
            "the latest were not the ones named, newest first"
        );
        assert_eq!(more, 3);
        let (few, none) = history_shown("peer-1", &many[..2]);
        assert_eq!(
            (few.len(), none),
            (2, 0),
            "a device with fewer than the cap was cut or said it had more"
        );
    }

    #[test]
    fn every_result_of_a_link_test_is_told_in_every_language_and_only_a_working_connection_is_told_as_good()
     {
        use peersus_core::model::LinkTest;
        for (test, tone) in [
            (LinkTest::Trying, Tone::Going),
            (LinkTest::Paired, Tone::Good),
            (LinkTest::NeedsPairing, Tone::Bad),
            (LinkTest::Unreachable, Tone::Bad),
        ] {
            for relays in [true, false] {
                for language in Language::ALL {
                    let (title, advice, said) = link_test_words_in(language, test, relays);
                    assert!(
                        !title.is_empty() && !advice.is_empty(),
                        "{test:?} says nothing in {language:?}"
                    );
                    assert_eq!(said, tone, "{test:?} with relays {relays} in {language:?}");
                    // A sentence that is not in the catalog comes back as it was written, in English: that is a missing translation.
                    if language != Language::English {
                        let (english_title, english_advice, _) =
                            link_test_words_in(Language::English, test, relays);
                        assert_ne!(
                            title, english_title,
                            "the title of {test:?} is not translated into {language:?}"
                        );
                        assert_ne!(
                            advice, english_advice,
                            "the advice of {test:?} is not translated into {language:?}"
                        );
                    }
                }
            }
        }
        // The two results that need something done ask for different things.
        let (_, pair_again, _) =
            link_test_words_in(Language::English, LinkTest::NeedsPairing, true);
        let (_, wait, _) = link_test_words_in(Language::English, LinkTest::Unreachable, true);
        assert_ne!(pair_again, wait);
        assert!(pair_again.contains("pair"), "{pair_again}");
    }

    #[test]
    fn a_device_that_does_not_answer_is_told_to_turn_the_relays_on_only_when_they_are_off_and_never_to_look_for_an_address()
     {
        use peersus_core::model::LinkTest;
        let (_, with_relays, _) =
            link_test_words_in(Language::English, LinkTest::Unreachable, true);
        let (_, without, _) = link_test_words_in(Language::English, LinkTest::Unreachable, false);
        // Off: the cause that is probably true is named, and so is what mends it.
        assert!(
            without.contains("relays") && without.contains("Settings"),
            "{without}"
        );
        // On: it must not tell a person to turn on what is on.
        assert!(!with_relays.contains("Turn on"), "{with_relays}");
        // Neither sends anyone to type an address: across networks a known address does not let a connection in.
        for words in [&with_relays, &without] {
            assert!(
                !words.to_lowercase().contains("ip address")
                    && !words.to_lowercase().contains("type an"),
                "{words}"
            );
        }
    }

    #[test]
    fn the_result_of_a_link_test_in_the_snapshot_reaches_the_device_it_is_about_and_no_other() {
        use peersus_core::model::{LinkTest, PeerId};
        let mut snapshot = snapshot_with(vec![peer("Alex", true), peer("Claude", false)], vec![]);
        let tested = snapshot.peers[1].id.clone();
        snapshot.link_tests.insert(tested, LinkTest::NeedsPairing);
        snapshot
            .link_tests
            .insert(PeerId("nobody".into()), LinkTest::Paired);
        let view = View::of(&snapshot);
        assert_eq!(
            view.devices[0].link_test, None,
            "a device that was not tested was given a result"
        );
        assert_eq!(view.devices[1].link_test, Some(LinkTest::NeedsPairing));
    }

    #[test]
    fn a_device_met_only_by_code_is_listed_once_per_identity_and_a_saved_one_is_not() {
        let saved = Device {
            id: "peer-saved".into(),
            name: "Laptop".into(),
            presence: Presence::Offline,
            grants: Default::default(),
            picture: None,
            paired_at: 0,
            last_seen: None,
            link_test: None,
            typed_address: None,
        };
        let list = [
            with("a", "peer-code-1", "Phone", Phase::Done),
            with("b", "peer-saved", "Laptop", Phase::Done),
            // Same name as the first, another identity: a different device, not merged.
            with("c", "peer-code-2", "Phone", Phase::Done),
            with("d", "peer-code-1", "Phone", Phase::Done),
        ];
        let met = unpaired_with_history(std::slice::from_ref(&saved), &list);
        assert_eq!(
            met,
            vec![
                ("peer-code-1", Some("Phone")),
                ("peer-code-2", Some("Phone"))
            ]
        );
        assert!(unpaired_with_history(&[saved], &[]).is_empty());
    }

    #[test]
    fn the_send_page_lists_only_what_is_still_going_and_not_what_is_over() {
        let list = [
            row("done", false, Phase::Done, None),
            row("moving", false, Phase::Moving, None),
            row("cancelled", false, Phase::Cancelled, None),
            row("waiting", false, Phase::Waiting, None),
            row("failed", false, Phase::Failed("x".into()), None),
        ];
        let ids: Vec<_> = going_on(&list).into_iter().map(|item| item.id).collect();
        assert_eq!(
            ids,
            ["moving", "waiting"],
            "a finished transfer was listed on the page where files are chosen"
        );
        assert!(going_on(&[]).is_empty());
    }

    #[test]
    fn the_bounded_transfer_list_keeps_every_row_and_puts_active_files_before_newest_history() {
        let list = [
            row("old", false, Phase::Done, None),
            row("waiting", true, Phase::Waiting, None),
            row("cancelled", false, Phase::Cancelled, None),
            row("moving", false, Phase::Moving, None),
            row("failed", true, Phase::Failed("offline".into()), None),
        ];
        let shown: Vec<_> = transfer_list(&list).map(|item| item.id.as_str()).collect();
        assert_eq!(shown, ["waiting", "moving", "failed", "cancelled", "old"]);
        assert_eq!(
            list[0].id, "old",
            "presentation must not reorder the shared model"
        );
        assert!(transfer_list(&[]).next().is_none());
    }

    #[test]
    fn more_history_never_extends_the_transfer_viewport() {
        for count in 0..=3 {
            assert_eq!(transfer_list_height(count), None);
        }
        assert_eq!(transfer_list_height(4), Some(360.));
        assert_eq!(transfer_list_height(4), transfer_list_height(usize::MAX));
    }

    #[test]
    fn a_history_line_says_which_way_the_file_went_and_how_it_ended() {
        let sent = Progress {
            name: "a.txt".into(),
            ..row("a", false, Phase::Done, None)
        };
        assert_eq!(history_line(&sent), "Sent \u{b7} a.txt \u{b7} done");
        let failed = Progress {
            name: "a.txt".into(),
            ..row("a", true, Phase::Failed("x".into()), None)
        };
        assert_eq!(history_line(&failed), "Received \u{b7} a.txt \u{b7} failed");
        let cancelled = Progress {
            name: "a.txt".into(),
            ..row("a", true, Phase::Cancelled, None)
        };
        assert_eq!(
            history_line(&cancelled),
            "Received \u{b7} a.txt \u{b7} cancelled"
        );
    }

    #[test]
    fn a_file_that_was_not_delivered_stays_said_whatever_other_file_arrives_with_it_or_after() {
        let ok = ("b".to_owned(), true);
        let bad = ("a".to_owned(), false);
        let said = delivery_note(None, &[bad.clone(), ok.clone()]);
        assert!(
            said.is_some(),
            "a failure was lost when a success came in the same look"
        );
        assert_eq!(
            delivery_note(None, &[ok.clone(), bad]),
            said,
            "the order changed what was said"
        );
        assert_eq!(
            delivery_note(said.clone(), std::slice::from_ref(&ok)),
            said,
            "another file's success took the failure away"
        );
        assert_eq!(
            delivery_note(None, &[ok]),
            None,
            "a success alone said something went wrong"
        );
    }

    #[test]
    fn a_received_file_that_arrived_whole_is_handed_to_the_folder_chosen_for_it_and_only_then() {
        let list = [
            arrived("a", "/stage/a.bin"),
            // Not done yet: its file is partial or not there, so it is not handed over.
            Progress {
                saved_as: Some("/stage/b.bin".into()),
                ..row("b", true, Phase::Moving, None)
            },
            // Went out, not in: nothing of ours to put anywhere.
            Progress {
                saved_as: Some("/stage/c.bin".into()),
                ..row("c", false, Phase::Done, None)
            },
            // Done but no file reported: nothing to hand over.
            row("d", true, Phase::Done, None),
            // Done, with a file, but accepted to a path the program can write: no tree, so nothing to do.
            arrived("e", "/stage/e.bin"),
        ];
        let trees = |id: &str| (id != "e").then(|| format!("tree-{id}"));
        let handed = handoffs(&list, trees);
        assert_eq!(
            handed,
            vec![peersus_net::folder_picked::Handoff {
                transfer: "a".into(),
                path: "/stage/a.bin".into(),
                name: "a.bin".into(),
                folders: Vec::new(),
                tree: "tree-a".into()
            }]
        );
    }

    #[test]
    fn nested_delivery_keeps_offer_directories_instead_of_flattening_the_staged_file() {
        let mut record = transfer(6, 6, TransferState::Complete);
        record.incoming = true;
        record.folders = vec!["pictures".into(), "nested".into(), "deeper".into()];
        record.saved_as = Some("/private/staging/pictures/nested/deeper/file.bin".into());
        let list = [progress(&record)];
        let handed = handoffs(&list, |_| Some("chosen-tree".into()));
        assert_eq!(handed[0].folders, ["pictures", "nested", "deeper"]);
        assert_eq!(handed[0].name, "file.bin");
        assert_eq!(handed[0].tree, "chosen-tree");
    }

    #[test]
    fn a_transfer_is_handed_over_once_because_its_folder_is_given_up_when_asked() {
        let picked = peersus_net::folder_picked::FolderPicked::default();
        picked.destined("a", "tree-a".into());
        let list = [arrived("a", "/stage/a.bin")];
        assert_eq!(handoffs(&list, |id| picked.tree_for(id)).len(), 1);
        assert!(
            handoffs(&list, |id| picked.tree_for(id)).is_empty(),
            "the same finished transfer was handed over again"
        );
    }

    #[test]
    fn the_time_left_counts_down_to_the_deadline_and_stops_at_zero_and_says_nothing_without_one() {
        assert_eq!(
            time_left(None, 1_000),
            None,
            "an offer with no deadline was given one"
        );
        assert_eq!(time_left(Some(1_120), 1_000), Some(120));
        assert_eq!(time_left(Some(1_120), 1_119), Some(1));
        assert_eq!(
            time_left(Some(1_120), 1_120),
            Some(0),
            "the deadline itself still said there was time"
        );
        // Gone by: never a negative count, and never a wrapped one.
        assert_eq!(time_left(Some(1_120), 5_000), Some(0));
        assert_eq!(time_left(Some(0), u64::MAX), Some(0));
    }

    #[test]
    fn the_clock_is_read_as_minutes_and_two_digit_seconds_and_an_hour_is_sixty_minutes() {
        for (seconds, words) in [
            (0, "0:00"),
            (9, "0:09"),
            (59, "0:59"),
            (60, "1:00"),
            (65, "1:05"),
            (120, "2:00"),
            (3_600, "60:00"),
            (86_400, "1440:00"),
        ] {
            assert_eq!(clock_words(seconds), words, "{seconds} s");
        }
    }

    #[test]
    fn the_window_carries_the_owners_deadline_as_it_is_and_not_a_count() {
        let offered = Transfer {
            expires_at: Some(1_700_000_120),
            ..transfer(10, 0, TransferState::Offered)
        };
        assert_eq!(progress(&offered).expires_at, Some(1_700_000_120));
        assert_eq!(
            progress(&transfer(10, 0, TransferState::Offered)).expires_at,
            None
        );
    }

    #[test]
    fn only_a_file_coming_in_that_waits_for_an_answer_is_shown_over_the_window() {
        // Going out and waiting: it is the OTHER device that decides, so there is nothing for this person to answer.
        assert!(incoming_offer(&[row("a", false, Phase::Waiting, None)]).is_none());
        // Coming in, but already answered, finished or stopped: nothing left to ask.
        for phase in [
            Phase::Moving,
            Phase::Done,
            Phase::Cancelled,
            Phase::Failed("x".into()),
        ] {
            assert!(incoming_offer(&[row("a", true, phase, None)]).is_none());
        }
        // Coming in and waiting: the one thing that is asked.
        assert_eq!(
            incoming_offer(&[row("a", true, Phase::Waiting, None)]).map(|offer| offer.id.as_str()),
            Some("a")
        );
        assert!(incoming_offer(&[]).is_none());
    }

    #[test]
    fn clear_is_offered_only_when_something_is_over_and_never_for_what_is_still_going() {
        let view = |phases: Vec<Phase>| View {
            transfers: phases
                .into_iter()
                .enumerate()
                .map(|(n, phase)| row(&format!("t{n}"), n % 2 == 0, phase, None))
                .collect(),
            ..View::default()
        };
        assert!(
            !view(vec![]).has_finished_transfers(),
            "nothing at all, and there was something to clear"
        );
        assert!(
            !view(vec![Phase::Waiting, Phase::Moving]).has_finished_transfers(),
            "what is still going was offered to be cleared"
        );
        for over in [Phase::Done, Phase::Cancelled, Phase::Failed("x".into())] {
            assert!(over.is_over(), "{over:?} is over");
            assert!(
                view(vec![Phase::Moving, over]).has_finished_transfers(),
                "one over among those going was not found"
            );
        }
        assert!(!Phase::Waiting.is_over() && !Phase::Moving.is_over());
    }

    #[test]
    fn the_sample_offer_is_one_the_window_would_show_for_a_real_one_and_has_the_default_wait() {
        let sample = sample_offer(1_000);
        // The rule that picks what to show over the window picks it, and it can be answered.
        let list = [sample.offer.clone()];
        assert_eq!(
            incoming_offer(&list).map(|offer| offer.id.as_str()),
            Some("sample")
        );
        assert!(sample.offer.acceptable());
        assert_eq!(sample.waiting, 1);
        assert_eq!(
            time_left(sample.offer.expires_at, 1_000),
            Some(u64::from(peersus_core::config::DEFAULT_OFFER_TIMEOUT))
        );
    }

    #[test]
    fn an_action_with_no_target_is_done_on_the_second_press_and_never_on_the_first() {
        let (asked, now) = Confirm::default().pressed();
        assert_eq!(
            (asked, now),
            (Confirm::Asking, false),
            "the first press did it"
        );
        let (done, now) = asked.pressed();
        assert_eq!(
            (done, now),
            (Confirm::Idle, true),
            "the second press did not do it"
        );
        // After it is done, the next press asks again: one confirmation is not a standing permission.
        assert_eq!(done.pressed(), (Confirm::Asking, false));
        // Given up, then pressed: asks again and does nothing.
        assert_eq!(asked.cancelled().pressed(), (Confirm::Asking, false));
    }

    #[test]
    fn with_several_waiting_the_oldest_comes_first_and_the_next_follows_when_it_is_answered() {
        let mut list = vec![
            row("done", true, Phase::Done, None),
            row("first", true, Phase::Waiting, None),
            row("second", true, Phase::Waiting, None),
        ];
        assert_eq!(
            incoming_offer(&list).map(|offer| offer.id.as_str()),
            Some("first"),
            "a finished one, or a later one, came first"
        );
        list[1].phase = Phase::Moving;
        assert_eq!(
            incoming_offer(&list).map(|offer| offer.id.as_str()),
            Some("second")
        );
        list[2].phase = Phase::Cancelled;
        assert!(incoming_offer(&list).is_none());
    }

    /// A finger on a circle 200 px across, as the toolkit reports it: where it is in the circle's box, and in the window.
    /// The box sits at (50, 300) in the window, so the two differ by that.
    fn at_box(x: f32, y: f32) -> ((f32, f32), (f32, f32)) {
        ((x, y), (x + 50., y + 300.))
    }

    #[test]
    fn a_circle_is_pressed_to_its_actuation_point_like_a_key_and_a_brush_of_the_screen_is_not_a_press()
     {
        let (b, w) = at_box(100., 100.);
        let mut touch = Touch::new(200.);
        touch.down(b, w, 1_000);
        assert!(
            !touch.up(b, w, 1_000),
            "a contact of no time pressed the circle"
        );
        touch.down(b, w, 1_000);
        assert!(
            !touch.up(b, w, 1_000 + MIN_TOUCH_MS - 1),
            "a brush pressed the circle"
        );
        touch.down(b, w, 1_000);
        assert!(
            touch.up(b, w, 1_000 + MIN_TOUCH_MS),
            "the actuation point itself is a press"
        );
        // Resting on it long is still a press for a control that has no hold of its own: the circle of Receive.
        touch.down(b, w, 1_000);
        assert!(
            touch.up(b, w, 1_000 + HOLD_MS * 4),
            "a long rest was not a press"
        );
    }

    #[test]
    fn a_clock_that_runs_backwards_does_not_press_a_circle() {
        let (b, w) = at_box(100., 100.);
        let mut touch = Touch::new(200.);
        touch.down(b, w, 10_000);
        assert!(
            !touch.up(b, w, 5),
            "a clock that ran backwards pressed the circle"
        );
    }

    #[test]
    fn the_circle_of_send_asks_the_geometry_alone_for_its_own_clock_judges_the_time() {
        let (b, w) = at_box(100., 100.);
        let mut touch = Touch::new(200.);
        touch.down(b, w, 0);
        assert!(
            touch.lifted_inside(b, w),
            "a lift inside the round was refused"
        );
        let (corner, corner_win) = at_box(2., 2.);
        touch.down(b, w, 0);
        assert!(
            !touch.lifted_inside(corner, corner_win),
            "a lift from a corner counted"
        );
    }

    #[test]
    fn a_touch_in_the_middle_that_hardly_moves_is_a_press() {
        let mut touch = Touch::new(200.);
        let (down_box, down_win) = at_box(100., 100.);
        touch.down(down_box, down_win, 0);
        let (up_box, up_win) = at_box(104., 103.);
        assert!(
            touch.up(up_box, up_win, MIN_TOUCH_MS),
            "a finger that moved a little was not a tap"
        );
        // A move of exactly the slop is still a tap.
        let mut touch = Touch::new(200.);
        touch.down(down_box, down_win, 0);
        let (up_box, up_win) = at_box(100. + TAP_SLOP, 100.);
        assert!(
            touch.up(up_box, up_win, MIN_TOUCH_MS),
            "a move of exactly the slop was refused"
        );
    }

    #[test]
    fn a_touch_that_begins_in_a_corner_of_the_box_is_outside_the_circle_and_presses_nothing() {
        for corner in [(2., 2.), (198., 2.), (2., 198.), (198., 198.)] {
            let mut touch = Touch::new(200.);
            let (b, w) = at_box(corner.0, corner.1);
            touch.down(b, w, 0);
            assert!(
                !touch.up(b, w, MIN_TOUCH_MS),
                "{corner:?} pressed the circle from its corner"
            );
        }
        // Down in the circle and lifted in a corner is not a press either.
        let mut touch = Touch::new(200.);
        let (db, dw) = at_box(100., 100.);
        touch.down(db, dw, 0);
        let (ub, uw) = at_box(2., 2.);
        assert!(!touch.up(ub, uw, MIN_TOUCH_MS));
    }

    #[test]
    fn a_drag_presses_nothing_even_when_it_begins_and_ends_on_the_circle() {
        let mut touch = Touch::new(200.);
        let (db, dw) = at_box(100., 60.);
        touch.down(db, dw, 0);
        let (ub, uw) = at_box(100., 140.);
        assert!(
            !touch.up(ub, uw, MIN_TOUCH_MS),
            "a swipe was taken for a tap"
        );
        // Just past the slop is already a drag.
        let mut touch = Touch::new(200.);
        let (db, dw) = at_box(100., 100.);
        touch.down(db, dw, 0);
        let (ub, uw) = at_box(100. + TAP_SLOP + 1., 100.);
        assert!(
            !touch.up(ub, uw, MIN_TOUCH_MS),
            "a move just past the slop was a tap"
        );
    }

    #[test]
    fn a_swipe_that_goes_out_and_comes_back_is_still_a_drag_and_presses_nothing() {
        let mut touch = Touch::new(200.);
        let (db, dw) = at_box(100., 100.);
        touch.down(db, dw, 0);
        // Down 70 px (a swipe), then back to exactly where it began, and lifted there: both ends are the same point.
        touch.moved(at_box(100., 170.).1);
        let (ub, uw) = at_box(100., 100.);
        assert!(
            !touch.up(ub, uw, MIN_TOUCH_MS),
            "an out-and-back swipe ended where it began and was taken for a tap"
        );
    }

    #[test]
    fn a_page_scrolling_under_a_still_finger_is_not_the_finger_moving_but_the_finger_moving_is() {
        // The page scrolls 80 px: the circle's box moves up under a finger that stays still. In the window the finger has not
        // moved; inside the box it appears to have moved 80 px. That is not a drag by the finger, and it must not decide.
        let mut touch = Touch::new(200.);
        touch.down((100., 100.), (150., 400.), 0);
        touch.moved((150., 400.));
        // The finger is where it was, in the window. In the box it reads 80 px further up, which is still inside the circle.
        assert!(
            touch.up((100., 20.), (150., 400.), MIN_TOUCH_MS),
            "scrolling under a still finger cancelled a tap"
        );
        // The same distance moved by the FINGER, in the window, is a drag.
        let mut touch = Touch::new(200.);
        touch.down((100., 100.), (150., 400.), 0);
        touch.moved((150., 480.));
        assert!(
            !touch.up((100., 100.), (150., 480.), MIN_TOUCH_MS),
            "the finger moved 80 px and it was a tap"
        );
    }

    #[test]
    fn the_edge_of_the_circle_is_where_the_press_ends() {
        // On the rim (distance from the middle equal to the radius) is in; a hair past it is out.
        let mut touch = Touch::new(200.);
        let (b, w) = at_box(200., 100.);
        touch.down(b, w, 0);
        assert!(touch.up(b, w, MIN_TOUCH_MS), "the rim itself was refused");
        let mut touch = Touch::new(200.);
        let (b, w) = at_box(200.5, 100.);
        touch.down(b, w, 0);
        assert!(
            !touch.up(b, w, MIN_TOUCH_MS),
            "a point past the rim pressed"
        );
    }

    #[test]
    fn both_ends_of_the_touch_must_be_in_the_circle_so_one_end_outside_with_a_tiny_move_is_not_a_press()
     {
        // Down on the rim (inside), up 3 px outward (outside): the move is inside the slop, so only the place where the
        // finger came up can refuse it.
        let mut touch = Touch::new(200.);
        let (db, dw) = at_box(200., 100.);
        touch.down(db, dw, 0);
        let (ub, uw) = at_box(203., 100.);
        assert!(
            !touch.up(ub, uw, MIN_TOUCH_MS),
            "a touch that lifted outside the circle pressed"
        );
        // The other way: down just outside, up on the rim. Only the place where it began can refuse it.
        let mut touch = Touch::new(200.);
        let (db, dw) = at_box(203., 100.);
        touch.down(db, dw, 0);
        let (ub, uw) = at_box(200., 100.);
        assert!(
            !touch.up(ub, uw, MIN_TOUCH_MS),
            "a touch that began outside the circle pressed"
        );
    }

    #[test]
    fn a_cancelled_gesture_presses_nothing_and_a_lift_with_no_touch_before_it_presses_nothing() {
        let mut touch = Touch::new(200.);
        let (b, w) = at_box(100., 100.);
        touch.down(b, w, 0);
        touch.cancel();
        assert!(!touch.up(b, w, MIN_TOUCH_MS), "a cancelled gesture pressed");
        // A lift that nothing began: the finger came onto the circle from outside and was lifted there.
        let mut touch = Touch::new(200.);
        assert!(
            !touch.up(b, w, MIN_TOUCH_MS),
            "a lift with no touch before it pressed"
        );
        // And one gesture does not carry into the next: after a press the next lift alone is not one.
        let mut touch = Touch::new(200.);
        touch.down(b, w, 0);
        assert!(touch.up(b, w, MIN_TOUCH_MS));
        assert!(
            !touch.up(b, w, MIN_TOUCH_MS),
            "a second lift pressed from the first touch"
        );
    }

    #[test]
    fn the_words_name_who_sends_and_never_pretend_when_it_gave_no_name() {
        assert_eq!(
            offer_words(&row("a", true, Phase::Waiting, Some("Alex's phone"))),
            "Alex's phone wants to send you a file"
        );
        // Nothing, or only blanks: it is not made up, and no empty name leaves a sentence that begins with a space.
        for none in [None, Some(""), Some("   ")] {
            assert_eq!(
                offer_words(&row("a", true, Phase::Waiting, none)),
                "A device wants to send you a file"
            );
        }
    }

    #[test]
    fn the_queue_is_said_on_a_line_of_its_own_and_only_when_there_is_more_than_one() {
        assert_eq!(queue_words(0), None);
        assert_eq!(queue_words(1), None, "a queue of one was announced");
        assert_eq!(
            queue_words(2).as_deref(),
            Some("2 files are waiting in all")
        );
        assert_eq!(
            queue_words(16).as_deref(),
            Some("16 files are waiting in all")
        );
    }

    #[test]
    fn a_count_is_never_put_to_the_name_of_one_sender_when_the_files_come_from_several() {
        // Alice offered one file and Bob two. The prompt is about the FIRST file, which is what Accept answers, so its
        // sentence names Alice and says "a file"; the total is a separate line, and it is three because that is what waits.
        let mut view = View {
            transfers: vec![
                row("done", true, Phase::Done, Some("Carol")),
                row("out", false, Phase::Waiting, Some("Dan")),
                row("alice-1", true, Phase::Waiting, Some("Alice")),
                row("bob-1", true, Phase::Waiting, Some("Bob")),
                row("bob-2", true, Phase::Waiting, Some("Bob")),
            ],
            ..View::default()
        };
        let offered = offered_now(&view).expect("three were waiting");
        assert_eq!(offered.offer.id, "alice-1");
        assert_eq!(
            offered.waiting, 3,
            "a finished or an outgoing file was counted as waiting"
        );
        assert_eq!(
            offer_words(&offered.offer),
            "Alice wants to send you a file",
            "the count was put to one sender's name"
        );
        assert_eq!(
            queue_words(offered.waiting).as_deref(),
            Some("3 files are waiting in all")
        );
        // Accept answers the first one; when it is answered the next is about Bob, with two waiting.
        view.transfers[2].phase = Phase::Moving;
        let next = offered_now(&view).expect("two were waiting");
        assert_eq!((next.offer.id.as_str(), next.waiting), ("bob-1", 2));
        assert_eq!(offer_words(&next.offer), "Bob wants to send you a file");
    }

    #[test]
    fn the_circle_can_be_pressed_to_choose_again_when_nothing_is_going_on_and_when_a_send_is_over_without_clear()
     {
        let can = |circle: CodeCircle| circle.lets_choose();
        // Nothing out, or finished: choosing is open.
        assert!(can(CodeCircle::Gone));
        assert!(
            can(CodeCircle::Sent { files: 2 }),
            "a finished send could not be followed by another without Clear"
        );
        assert!(
            can(CodeCircle::Stopped { files: 3, done: 1 }),
            "a stopped send could not be followed by another without Clear"
        );
        // A code out, or work going on: a second choice would replace what is being offered or sent.
        assert!(!can(CodeCircle::Code));
        assert!(!can(CodeCircle::Scanned));
        assert!(!can(CodeCircle::WaitingForApproval { files: 1 }));
        assert!(!can(CodeCircle::Sending {
            files: 1,
            percent: 40
        }));
        // The same through the rule that makes the circle: a finished transfer of the drawn code is a circle that may be pressed.
        let finished = [of_code("a", 3, Phase::Done, 100)];
        assert!(code_circle(3, on_show(3, false), &finished).lets_choose());
        let going = [of_code("a", 3, Phase::Moving, 40)];
        assert!(!code_circle(3, on_show(3, false), &going).lets_choose());
    }

    #[test]
    fn the_code_is_drawn_only_while_it_is_the_one_on_show_and_can_still_admit_a_device() {
        assert_eq!(code_circle(3, on_show(3, true), &[]), CodeCircle::Code);
        // Scanned: the owner says it admits no more, and no file is reported for it yet. NOT drawn.
        assert_eq!(
            code_circle(3, on_show(3, false), &[]),
            CodeCircle::Scanned,
            "a scanned code stayed on the screen"
        );
        // Another code is on show, or none (revoked with the network): what was drawn is gone.
        assert_eq!(
            code_circle(3, on_show(4, true), &[]),
            CodeCircle::Gone,
            "a replaced code stayed on the screen"
        );
        assert_eq!(
            code_circle(3, None, &[]),
            CodeCircle::Gone,
            "a revoked code stayed on the screen"
        );
    }

    #[test]
    fn a_file_that_came_of_the_code_says_where_it_stands_whatever_the_owner_says_of_the_code() {
        let waiting = [
            of_code("a", 3, Phase::Waiting, 0),
            of_code("b", 3, Phase::Waiting, 0),
        ];
        assert_eq!(
            code_circle(3, on_show(3, false), &waiting),
            CodeCircle::WaitingForApproval { files: 2 }
        );
        // One accepted and is crossing: sending, the mean of the two.
        let sending = [
            of_code("a", 3, Phase::Moving, 80),
            of_code("b", 3, Phase::Waiting, 0),
        ];
        assert_eq!(
            code_circle(3, on_show(3, false), &sending),
            CodeCircle::Sending {
                files: 2,
                percent: 40
            }
        );
        // Another code may have been made since: the files of the first still say where THEY stand.
        assert_eq!(
            code_circle(3, on_show(4, true), &sending),
            CodeCircle::Sending {
                files: 2,
                percent: 40
            }
        );
        let sent = [
            of_code("a", 3, Phase::Done, 100),
            of_code("b", 3, Phase::Done, 100),
        ];
        assert_eq!(code_circle(3, None, &sent), CodeCircle::Sent { files: 2 });
        let stopped = [
            of_code("a", 3, Phase::Done, 100),
            of_code("b", 3, Phase::Cancelled, 10),
        ];
        assert_eq!(
            code_circle(3, None, &stopped),
            CodeCircle::Stopped { files: 2, done: 1 }
        );
        let failed = [of_code("a", 3, Phase::Failed("x".into()), 0)];
        assert_eq!(
            code_circle(3, None, &failed),
            CodeCircle::Stopped { files: 1, done: 0 }
        );
        // Some still crossing while another failed: still going, not stopped.
        let mixed = [
            of_code("a", 3, Phase::Moving, 50),
            of_code("b", 3, Phase::Failed("x".into()), 0),
        ];
        assert_eq!(
            code_circle(3, None, &mixed),
            CodeCircle::Sending {
                files: 2,
                percent: 25
            }
        );
    }

    #[test]
    fn what_did_not_come_of_the_code_never_moves_its_circle() {
        // Files sent to a device that is paired (no code), files of another code, and files coming IN, in any number and in
        // any order, are not this code's.
        let others = [
            row("paired", false, Phase::Moving, None),
            of_code("older", 2, Phase::Done, 100),
            of_code("newer", 4, Phase::Waiting, 0),
            Progress {
                code: Some(3),
                ..row("in", true, Phase::Done, Some("Alex"))
            },
        ];
        assert_eq!(
            code_circle(3, on_show(3, true), &others),
            CodeCircle::Code,
            "a transfer of no code was taken for this one's"
        );
        assert_eq!(
            code_circle(3, on_show(3, false), &others),
            CodeCircle::Scanned
        );
    }

    #[test]
    fn a_code_is_taken_off_the_screen_at_its_deadline_without_waiting_for_the_owner() {
        assert!(!code_expired(1_000, 999));
        assert!(
            code_expired(1_000, 1_000),
            "a code was still drawn at its deadline"
        );
        assert!(code_expired(1_000, 5_000));
    }

    #[test]
    fn the_circle_says_what_is_happening_to_the_files_in_words_and_says_nothing_while_it_is_the_code()
     {
        // The code is the picture, and a code that is gone asks for files again: neither has words of its own.
        assert_eq!(circle_words_in(Language::English, &CodeCircle::Code), None);
        assert_eq!(circle_words_in(Language::English, &CodeCircle::Gone), None);
        // Scanned: said at once, before any file is reported, and it says it is the other device that is to answer.
        let (_, headline, line) = circle_words_in(Language::English, &CodeCircle::Scanned).unwrap();
        assert_eq!(headline, "SCANNED");
        assert!(line.contains("Waiting for it to accept"), "{line}");
        let (kicker, headline, line) = circle_words_in(
            Language::English,
            &CodeCircle::WaitingForApproval { files: 1 },
        )
        .unwrap();
        assert_eq!((kicker, headline.as_str()), ("WAITING FOR APPROVAL", "1"));
        assert_eq!(line, "Waiting for the other device to accept 1 file.");
        assert_eq!(
            circle_words_in(
                Language::English,
                &CodeCircle::WaitingForApproval { files: 3 }
            )
            .unwrap()
            .2,
            "Waiting for the other device to accept 3 files."
        );
        // Sending: the percent is the headline, so that how far it is can be read at a glance.
        let (kicker, headline, line) = circle_words_in(
            Language::English,
            &CodeCircle::Sending {
                files: 2,
                percent: 40,
            },
        )
        .unwrap();
        assert_eq!(
            (kicker, headline.as_str(), line.as_str()),
            ("SENDING", "40%", "Sending 2 files.")
        );
        let (_, headline, line) =
            circle_words_in(Language::English, &CodeCircle::Sent { files: 2 }).unwrap();
        assert_eq!(
            (headline.as_str(), line.as_str()),
            ("2 SENT", "2 files arrived.")
        );
        // Stopped says how many got there, not just that something went wrong.
        let (_, headline, line) = circle_words_in(
            Language::English,
            &CodeCircle::Stopped { files: 3, done: 1 },
        )
        .unwrap();
        assert_eq!(headline, "1/3");
        assert!(line.starts_with("1 of 3 files arrived."), "{line}");
    }

    #[test]
    fn the_circle_counts_down_to_the_earliest_deadline_of_the_files_of_its_code_that_still_wait_and_only_then()
     {
        let waiting = CodeCircle::WaitingForApproval { files: 2 };
        let with_deadline = |id: &str, code: u64, phase: Phase, at: Option<u64>| Progress {
            expires_at: at,
            ..of_code(id, code, phase, 0)
        };
        let list = [
            with_deadline("late", 3, Phase::Waiting, Some(1_200)),
            with_deadline("soon", 3, Phase::Waiting, Some(1_090)),
            // Another code's file, a file already moving and one with no deadline say nothing about this circle's time.
            with_deadline("other", 4, Phase::Waiting, Some(1_005)),
            with_deadline("moving", 3, Phase::Moving, Some(1_001)),
            with_deadline("none", 3, Phase::Waiting, None),
        ];
        assert_eq!(
            circle_time_in(Language::English, &waiting, 3, &list, 1_000).as_deref(),
            Some("ENDS IN 1:30"),
            "not the earliest deadline of this code"
        );
        assert_eq!(
            circle_time_in(Language::English, &waiting, 3, &list, 1_085).as_deref(),
            Some("ENDS IN 0:05")
        );
        // At the deadline the clock stops at zero (the same contract as the offer's): the owner ends the offer then, and the circle
        // leaves this state by itself. A deadline long past is not a negative number.
        assert_eq!(
            circle_time_in(Language::English, &waiting, 3, &list, 1_090).as_deref(),
            Some("ENDS IN 0:00"),
            "the clock did not stop at zero"
        );
        assert_eq!(
            circle_time_in(Language::English, &waiting, 3, &list, 9_999).as_deref(),
            Some("ENDS IN 0:00"),
            "a deadline long past went below zero"
        );
        // Only while waiting: every other state has its own words and no clock.
        for circle in [
            CodeCircle::Code,
            CodeCircle::Scanned,
            CodeCircle::Sending {
                files: 1,
                percent: 5,
            },
            CodeCircle::Sent { files: 1 },
            CodeCircle::Gone,
        ] {
            assert_eq!(
                circle_time_in(Language::English, &circle, 3, &list, 1_000),
                None,
                "{circle:?} showed a countdown"
            );
        }
        assert_eq!(
            circle_time_in(Language::English, &waiting, 3, &[], 1_000),
            None,
            "a circle with no waiting file showed a time"
        );
    }

    #[test]
    fn the_circle_names_the_chosen_files_up_to_a_bound_and_counts_the_rest_and_says_nothing_for_none()
     {
        let names = |count: usize| (0..count).map(|n| format!("f{n}.txt")).collect::<Vec<_>>();
        assert!(
            chosen_words(&[]).is_empty(),
            "something was said when nothing was chosen"
        );
        assert_eq!(chosen_words(&names(1)), ["f0.txt"]);
        // Exactly the bound: all named, nothing counted.
        assert_eq!(
            chosen_words(&names(NAMED_UNDER_CIRCLE)).len(),
            NAMED_UNDER_CIRCLE
        );
        assert!(
            !chosen_words(&names(NAMED_UNDER_CIRCLE))
                .last()
                .unwrap()
                .starts_with('+'),
            "a file that fits was counted instead of named"
        );
        // One past the bound: the first ones are named and the rest are counted, so that nothing chosen is hidden without a word.
        let many = chosen_words(&names(NAMED_UNDER_CIRCLE + 3));
        assert_eq!(many.len(), NAMED_UNDER_CIRCLE + 1);
        assert_eq!(many[0], "f0.txt");
        assert_eq!(many.last().unwrap(), "+ 3 more");
    }

    #[test]
    fn what_is_left_of_a_codes_life_runs_to_its_deadline_and_never_goes_below_nothing() {
        assert_eq!(
            until_expiry(1_300, 1_000),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(until_expiry(1_000, 1_000), std::time::Duration::ZERO);
        // A clock that is ahead of the deadline is not a debt.
        assert_eq!(until_expiry(1_000, 9_000), std::time::Duration::ZERO);
        // The same rule as `code_expired`: no time left exactly when it is expired.
        for now in [0, 999, 1_000, 1_001] {
            assert_eq!(
                until_expiry(1_000, now).is_zero(),
                code_expired(1_000, now),
                "at {now}"
            );
        }
    }

    #[test]
    fn the_bar_goes_up_as_the_page_goes_down_and_comes_back_as_it_goes_up() {
        let start = HeaderHide::default();
        assert_eq!(start.hidden(), 0., "a page not yet scrolled hid the bar");
        // Scrolling down: the bar goes up by what the page went down, and no further than its own height.
        let down = start.after(30.);
        assert_eq!(down.hidden(), 30.);
        assert_eq!(down.height(), HEADER_HEIGHT - 30.);
        let far = down.after(400.);
        assert_eq!(
            far.hidden(),
            HEADER_HEIGHT,
            "the bar did not go all the way"
        );
        assert_eq!(far.height(), 0.);
        // Scrolling up, anywhere on the page and not only at its top: the bar comes back by what the page went up.
        let up = far.after(380.);
        assert_eq!(
            up.hidden(),
            HEADER_HEIGHT - 20.,
            "the bar did not come back as the page went up"
        );
        let back = up.after(300.);
        assert_eq!(
            back.hidden(),
            0.,
            "scrolling up did not bring the whole bar back"
        );
    }

    #[test]
    fn the_bar_is_whole_at_the_very_top_whatever_came_before() {
        let hidden = HeaderHide::default().after(500.);
        assert_eq!(hidden.hidden(), HEADER_HEIGHT);
        // Back at the top in one jump (a tap on the status bar, a page that was left and entered again): nothing above to read.
        assert_eq!(hidden.after(0.).hidden(), 0.);
        // And a position that went past the top (a pull that stretches the page) is the top, not a negative one.
        assert_eq!(hidden.after(-40.).hidden(), 0.);
    }

    #[test]
    fn a_small_move_is_a_small_change_and_a_position_that_is_not_a_number_changes_nothing() {
        // From a bar that is only partly hidden: a bar that is already all the way up cannot go further whatever the move.
        let at = HeaderHide::default().after(20.);
        let nudged = at.after(23.);
        assert_eq!(
            nudged.hidden() - at.hidden(),
            3.,
            "a small move was not a small change"
        );
        // And a bar that is all the way up stays so for a small move down; it comes back only by a move up.
        let gone = HeaderHide::default().after(500.);
        assert_eq!(gone.after(503.).hidden(), HEADER_HEIGHT);
        assert_eq!(
            gone.after(498.).hidden(),
            HEADER_HEIGHT - 2.,
            "a small move up did not bring a small part of the bar back"
        );
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(at.after(bad), at, "{bad} changed the bar");
        }
    }

    #[test]
    fn completed_first_batches_never_finish_a_selection_that_is_still_being_opened() {
        let early = [of_code("first", 3, Phase::Done, 100)];
        let producing = Some(FilesCode {
            generation: 3,
            live: false,
            pending: true,
        });
        assert_eq!(
            code_circle_selection(3, producing, &early, 80),
            CodeCircle::Sending {
                files: 80,
                percent: 1
            }
        );
        assert!(!code_circle_selection(3, producing, &early, 80).lets_choose());
        // Expiry, closure or producer failure must not turn the partial result into success.
        assert_eq!(
            code_circle_selection(3, on_show(3, false), &early, 80),
            CodeCircle::Stopped { files: 80, done: 1 }
        );
        // Once unreadable choices were accounted for, the successful remainder can finish.
        assert_eq!(
            code_circle_selection(3, on_show(3, false), &early, 1),
            CodeCircle::Sent { files: 1 }
        );
    }

    #[test]
    fn pending_source_work_cannot_show_one_hundred_percent_even_if_all_visible_rows_are_done() {
        let complete = [of_code("first", 3, Phase::Done, 100)];
        let producing = Some(FilesCode {
            generation: 3,
            live: false,
            pending: true,
        });
        assert_eq!(
            code_circle_selection(3, producing, &complete, 1),
            CodeCircle::Sending {
                files: 1,
                percent: 99
            }
        );
        assert_eq!(
            code_circle_selection(3, on_show(3, false), &complete, 1),
            CodeCircle::Sent { files: 1 }
        );
    }

    fn listing(
        name: &str,
        version: &str,
        installed: Option<&str>,
    ) -> peersus_core::model::StoreListing {
        peersus_core::model::StoreListing {
            plugin: peersus_core::plugins::PluginId::new(name).unwrap(),
            version: version.into(),
            author: "Someone".into(),
            link: "https://example.org/x".into(),
            description: "Copies text between devices".into(),
            installed_version: installed.map(str::to_owned),
        }
    }

    #[test]
    fn opening_the_application_names_damaged_failed_and_outdated_extensions() {
        use peersus_core::model::{PluginStatus, Standing, StoreState, StoreStatus};
        let plugin = |name: &str, standing, status| PluginInfo {
            id: peersus_core::plugins::PluginId::new(name).unwrap(),
            manifest: Default::default(),
            standing,
            status,
            grantable: Default::default(),
            program: None,
            package: None,
        };
        let plugins = [
            plugin("healthy", Standing::Ready, PluginStatus::Running),
            plugin("damaged", Standing::Broken, PluginStatus::Stopped),
            plugin("crashed", Standing::Ready, PluginStatus::Failed),
        ];
        let store = StoreState {
            status: StoreStatus::Ready,
            listings: vec![
                listing("old", "2.0.0", Some("1.0.0")),
                listing("current", "1.0.0", Some("1.0.0")),
            ],
            error: None,
        };
        let said = extension_health_notices(
            &plugins,
            &store,
            peersus_core::config::Language::English,
        );
        let named: Vec<&str> = said
            .iter()
            .map(|line| &line[1..line.find(']').unwrap()])
            .collect();
        assert_eq!(named, ["damaged", "crashed", "old"]);
        assert!(said[2].ends_with("2.0.0"));
    }

    #[test]
    fn damaged_extensions_offer_reinstallation_even_at_the_catalog_version() {
        use peersus_core::model::{PluginStatus, Standing};
        let listing = listing("repairme", "1.0.0", Some("1.0.0"));
        let mut plugin = PluginInfo {
            id: listing.plugin.clone(),
            manifest: Default::default(),
            standing: Standing::Ready,
            status: PluginStatus::Stopped,
            grantable: Default::default(),
            program: None,
            package: None,
        };
        assert_eq!(StoreRow::of_installed(&listing, &plugin).action, StoreAction::Installed);
        for standing in [Standing::Broken, Standing::Changed] {
            plugin.standing = standing;
            assert_eq!(StoreRow::of_installed(&listing, &plugin).action, StoreAction::Update);
        }
    }

    #[test]
    fn unified_extensions_keep_catalog_positions_across_install_and_removal() {
        use peersus_core::model::{PluginStatus, Standing, StoreState, StoreStatus};
        let plugin = |name: &str| PluginInfo {
            id: peersus_core::plugins::PluginId::new(name).unwrap(),
            manifest: Default::default(),
            standing: Standing::Ready,
            status: PluginStatus::Stopped,
            grantable: Default::default(),
            program: None,
            package: None,
        };
        let store = StoreState {
            status: StoreStatus::Ready,
            listings: vec![
                listing("echo", "1.0.0", None),
                listing("other", "1.0.0", None),
            ],
            error: None,
        };
        let plugins = [plugin("local"), plugin("echo")];
        let rows = extension_rows(&plugins, &store, "", &[]);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["echo", "other", "local"]
        );
        assert!(rows[0].installed.is_some());
        assert!(rows[0].listing.is_some());
        assert!(rows[1].installed.is_none());
        assert!(rows[2].listing.is_none());
        let after = extension_rows(&plugins[..1], &store, "", &[]);
        assert_eq!(
            after.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["echo", "other", "local"]
        );
        assert!(after[0].installed.is_none());
        assert!(after[0].listing.is_some());
        for status in [StoreStatus::Loading, StoreStatus::Failed] {
            let cached = StoreState {
                status,
                ..store.clone()
            };
            assert_eq!(
                extension_rows(&plugins, &cached, "", &[])
                    .iter()
                    .map(|row| row.id.as_str())
                    .collect::<Vec<_>>(),
                ["echo", "other", "local"]
            );
        }
        assert_eq!(
            extension_rows(&plugins, &store, " ECHO ", &[])
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["echo"]
        );
        assert_eq!(
            extension_rows(&plugins, &store, "local", &[])
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["local"]
        );
        assert_eq!(
            extension_rows(&plugins, &store, "someone", &[])
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["echo", "other"]
        );
        assert!(extension_rows(&plugins, &store, "missing", &[]).is_empty());
    }

    #[test]
    fn extensions_installed_this_session_come_first_newest_first_and_nothing_else_moves() {
        use peersus_core::model::{PluginStatus, Standing, StoreState, StoreStatus};
        let id = |name: &str| peersus_core::plugins::PluginId::new(name).unwrap();
        let plugin = |name: &str| PluginInfo {
            id: id(name),
            manifest: Default::default(),
            standing: Standing::Ready,
            status: PluginStatus::Stopped,
            grantable: Default::default(),
            program: None,
            package: None,
        };
        let store = StoreState {
            status: StoreStatus::Ready,
            listings: vec![
                listing("echo", "1.0.0", None),
                listing("other", "1.0.0", None),
            ],
            error: None,
        };
        let plugins = [plugin("local"), plugin("echo"), plugin("upload")];
        let order = |recent: &[peersus_core::plugins::PluginId]| {
            extension_rows(&plugins, &store, "", recent)
                .iter()
                .map(|row| row.id.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(order(&[]), ["echo", "other", "local", "upload"]);
        assert_eq!(order(&[id("upload")]), ["upload", "echo", "other", "local"]);
        assert_eq!(
            order(&[id("local"), id("upload")]),
            ["local", "upload", "echo", "other"]
        );
        // A catalog entry that is not installed is never pulled forward, even if it was named.
        assert_eq!(order(&[id("other")]), ["echo", "other", "local", "upload"]);
    }

    #[test]
    fn a_store_row_offers_install_update_or_nothing_from_the_two_versions() {
        use StoreAction::{Install, Installed, NewerInstalled, Update};
        let action = |offered: &str, installed: Option<&str>| {
            StoreRow::of(&listing("echo", offered, installed)).action
        };
        assert_eq!(action("1.2.0", None), Install);
        assert_eq!(action("1.2.0", Some("1.1.9")), Update);
        assert_eq!(
            action("1.2.0", Some("1.2.0")),
            Installed,
            "the same version is not an update"
        );
        assert_eq!(
            action("1.2.0", Some("1.3.0")),
            NewerInstalled,
            "a newer one is never downgraded by a click"
        );
        // By number and not by text: 1.10.0 is later than 1.9.0 even though "1.10.0" sorts first as text.
        assert_eq!(action("1.10.0", Some("1.9.0")), Update);
        assert_eq!(action("1.9.0", Some("1.10.0")), NewerInstalled);
        // A version that cannot be compared is never turned into an offered replacement.
        assert_eq!(action("1.2.0", Some("not-a-version")), Installed);
        assert_eq!(
            action("1.2.0", Some("01.2.0")),
            Installed,
            "a leading zero is not a version"
        );
    }

    #[test]
    fn only_install_and_update_have_a_button_and_only_what_is_installed_can_be_removed() {
        use StoreAction::{Install, Installed, NewerInstalled, Update};
        assert_eq!(
            (Install.button(), Update.button()),
            (Some("Install"), Some("Update"))
        );
        assert_eq!((Installed.button(), NewerInstalled.button()), (None, None));
        assert_eq!(
            [
                Install.can_remove(),
                Update.can_remove(),
                Installed.can_remove(),
                NewerInstalled.can_remove()
            ],
            [false, true, true, true]
        );
        // The cost of an update is said where it is pressed: what the old version was allowed is forgotten.
        assert!(Update.note().is_some_and(|words| words.contains("forgets")));
        assert!(Install.note().is_none() && Installed.note().is_none());
    }

    #[test]
    fn searching_the_store_matches_name_author_and_description_and_ignores_case_and_edges() {
        let state = peersus_core::model::StoreState {
            status: peersus_core::model::StoreStatus::Ready,
            listings: vec![listing("echo", "1.0.0", None), {
                let mut other = listing("zeta", "2.0.0", None);
                other.author = "Maria".into();
                other.description = "Uploads files".into();
                other
            }],
            error: None,
        };
        let names = |query: &str| {
            store_rows(&state, query)
                .iter()
                .map(|row| row.listing.plugin.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(""),
            ["echo", "zeta"],
            "an empty search shows everything, in the catalog's order"
        );
        assert_eq!(
            names("  ECHO "),
            ["echo"],
            "case and surrounding space are ignored"
        );
        assert_eq!(names("maria"), ["zeta"], "by author");
        assert_eq!(names("uploads"), ["zeta"], "by description");
        assert_eq!(
            names("copies"),
            ["echo"],
            "echo's description only: zeta's was changed"
        );
        assert!(names("nothing like this").is_empty());
    }

    #[test]
    fn the_store_says_where_it_has_got_to_in_fixed_words_and_accepts_a_click_only_when_it_can() {
        use peersus_core::model::{StoreState, StoreStatus::*};
        let state = |status, listings: usize| StoreState {
            status,
            listings: (0..listings)
                .map(|n| listing(&format!("p{n}"), "1.0.0", None))
                .collect(),
            error: Some("what a download said".into()),
        };
        assert_eq!(
            store_notice(&state(NotConfigured, 0)),
            Some("The extension store is not set up in this build.")
        );
        assert!(
            store_notice(&state(Idle, 0)).is_some() && store_notice(&state(Loading, 0)).is_some()
        );
        assert_eq!(
            store_notice(&state(Ready, 0)),
            Some("The maintainer has not approved any extension yet.")
        );
        assert_eq!(
            store_notice(&state(Ready, 2)),
            None,
            "a list needs no sentence above it"
        );
        // Whatever a download or a package said (`error`) is never repeated: only the application's own words.
        for status in [NotConfigured, Idle, Loading, Ready, Failed] {
            assert!(
                store_notice(&state(status, 0))
                    .is_none_or(|words| !words.contains("download said"))
            );
        }
        assert_eq!(
            [NotConfigured, Idle, Loading, Ready, Failed].map(|s| store_can_act(&state(s, 0))),
            [false, true, false, true, true]
        );
    }

    #[test]
    fn an_empty_list_says_why_and_a_search_that_finds_nothing_never_blames_the_maintainer() {
        use peersus_core::model::{StoreState, StoreStatus::*};
        let state = |status, listings: usize| StoreState {
            status,
            listings: (0..listings)
                .map(|n| listing(&format!("p{n}"), "1.0.0", None))
                .collect(),
            error: None,
        };
        let ready = state(Ready, 2);
        // Ready, with approved extensions, and the search matched none: it is the SEARCH that found nothing.
        let none = store_rows(&ready, "zzz");
        assert!(none.is_empty());
        assert_eq!(
            store_empty(&ready, &none, "zzz"),
            Some("No extension matches your search.")
        );
        // No search, and rows to show: nothing to explain.
        let all = store_rows(&ready, "");
        assert_eq!(store_empty(&ready, &all, ""), None);
        // The maintainer approved nothing: that is about the store, whatever is typed.
        let empty = state(Ready, 0);
        assert_eq!(
            store_empty(&empty, &[], ""),
            Some("The maintainer has not approved any extension yet.")
        );
        assert_eq!(
            store_empty(&empty, &[], "zzz"),
            Some("The maintainer has not approved any extension yet.")
        );
        // Not set up, loading, failed: the store's own words come first, with or without a search.
        for (status, words) in [
            (
                NotConfigured,
                "The extension store is not set up in this build.",
            ),
            (Loading, "Working…"),
            (
                Failed,
                "The store could not complete that. Nothing was changed.",
            ),
        ] {
            assert_eq!(store_empty(&state(status, 0), &[], "zzz"), Some(words));
        }
        // A notice about the store (loading, failed) never hides rows that are still on show.
        let loading = state(Loading, 2);
        assert_eq!(
            store_empty(&loading, &store_rows(&loading, ""), ""),
            None,
            "the last verified listing stays visible"
        );
    }

    /// The words the store rules RETURN are translated where they are drawn, so a scan of the call sites cannot see them.
    /// Every one the rules can produce, for every state, must be a message of the catalog.
    #[test]
    fn every_word_the_store_rules_can_return_has_a_row_in_the_catalog() {
        use peersus_core::model::{StoreState, StoreStatus::*};
        let mut ids: Vec<&'static str> = Vec::new();
        for status in [NotConfigured, Idle, Loading, Ready, Failed] {
            for listings in [0, 2] {
                let state = StoreState {
                    status,
                    listings: (0..listings)
                        .map(|n| listing(&format!("p{n}"), "1.0.0", None))
                        .collect(),
                    error: None,
                };
                ids.extend(store_notice(&state));
                for query in ["", "zzz"] {
                    ids.extend(store_empty(&state, &store_rows(&state, query), query));
                }
            }
        }
        for action in [
            StoreAction::Install,
            StoreAction::Update,
            StoreAction::Installed,
            StoreAction::NewerInstalled,
        ] {
            ids.extend(action.button());
            ids.extend(action.note());
        }
        assert!(
            ids.len() >= 12,
            "the rules produced too few words to be exercising them: {ids:?}"
        );
        let missing: Vec<&str> = ids.into_iter().filter(|id| !crate::i18n::has(id)).collect();
        assert!(
            missing.is_empty(),
            "add these to the catalog in every language: {missing:?}"
        );
    }
}

#[cfg(test)]
mod circle_view_tests {
    use super::*;

    fn one(id: &str, phase: Phase) -> Progress {
        Progress {
            id: id.into(),
            name: format!("{id}.txt"),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn devices() -> Aim {
        Aim::Devices(vec!["laptop".into()])
    }
    fn view_of(
        listed: usize,
        aim: Aim,
        code: Option<CodeCircle>,
        batch: &[Progress],
    ) -> CircleView {
        circle_view(&CircleInputs {
            listed,
            aim,
            send_instantly: false,
            code,
            batch,
            handed_over: 0,
        })
    }

    #[test]
    fn with_nothing_at_all_it_asks_for_files_and_with_files_listed_it_does_what_they_call_for() {
        assert_eq!(
            view_of(0, Aim::Code, None, &[]).does,
            Some(CircleDoes::ChooseFiles)
        );
        assert_eq!(
            view_of(2, Aim::Code, None, &[]).does,
            Some(CircleDoes::MakeCode)
        );
        assert_eq!(
            view_of(2, devices(), None, &[]).does,
            Some(CircleDoes::HoldToSend)
        );
    }

    #[test]
    fn while_a_transfer_goes_the_circle_is_shut_and_says_what_goes_and_never_asks_for_files() {
        let batch = [one("a", Phase::Moving)];
        let shut = view_of(0, Aim::Code, None, &batch);
        assert_eq!(shut.does, None);
        assert!(
            matches!(shut.batch, BatchState::Going { files: 1, .. }),
            "{:?}",
            shut.batch
        );
    }

    #[test]
    fn after_a_cancel_the_circle_is_open_again_and_still_says_how_it_ended_and_a_touch_chooses_new_files()
     {
        let batch = [one("a", Phase::Cancelled)];
        let open = view_of(0, Aim::Code, None, &batch);
        assert_eq!(
            open.does,
            Some(CircleDoes::ChooseFiles),
            "a circle with nothing going was dead"
        );
        assert_eq!(open.batch, BatchState::Stopped { files: 1, done: 0 });
    }

    #[test]
    fn new_files_chosen_after_a_cancel_make_the_circle_what_they_call_for_whatever_failed_before() {
        let batch = [one("a", Phase::Cancelled)];
        // The failure: a stale code said Sending and the circle stayed on what it saw before, instead of what is chosen now.
        let stale = Some(CodeCircle::Sending {
            files: 1,
            percent: 99,
        });
        assert_eq!(
            view_of(1, devices(), stale.clone(), &batch).does,
            Some(CircleDoes::HoldToSend)
        );
        assert_eq!(
            view_of(1, Aim::Code, stale, &batch).does,
            Some(CircleDoes::MakeCode)
        );
    }

    #[test]
    fn a_code_that_is_drawn_is_the_circle_and_a_scanned_one_waits_and_neither_takes_a_new_choice() {
        let drawn = view_of(3, devices(), Some(CodeCircle::Code), &[]);
        assert!(drawn.shows_code);
        assert_eq!(drawn.does, None);
        let scanned = view_of(3, devices(), Some(CodeCircle::Scanned), &[]);
        assert!(!scanned.shows_code);
        assert_eq!(scanned.does, None);
    }

    #[test]
    fn a_code_that_is_still_sending_with_no_transfer_yet_holds_the_circle_shut_until_the_transfers_say_otherwise()
     {
        let sending = Some(CodeCircle::Sending {
            files: 2,
            percent: 10,
        });
        assert_eq!(view_of(0, Aim::Code, sending.clone(), &[]).does, None);
        // Once its transfers have all ended, the code's word no longer holds it shut.
        let ended = [one("a", Phase::Done), one("b", Phase::Cancelled)];
        assert_eq!(
            view_of(0, Aim::Code, sending, &ended).does,
            Some(CircleDoes::ChooseFiles)
        );
    }

    #[test]
    fn a_batch_that_arrived_whole_says_so_and_a_touch_chooses_the_next() {
        let batch = [one("a", Phase::Done)];
        let done = view_of(0, Aim::Code, None, &batch);
        assert_eq!(done.batch, BatchState::Sent { files: 1 });
        assert_eq!(done.does, Some(CircleDoes::ChooseFiles));
    }

    #[test]
    fn the_same_inputs_always_give_the_same_answer() {
        let batch = [one("a", Phase::Moving), one("b", Phase::Done)];
        let a = view_of(
            2,
            devices(),
            Some(CodeCircle::Sending {
                files: 2,
                percent: 50,
            }),
            &batch,
        );
        let b = view_of(
            2,
            devices(),
            Some(CodeCircle::Sending {
                files: 2,
                percent: 50,
            }),
            &batch,
        );
        assert_eq!(a, b);
    }
}

#[cfg(test)]
mod circle_view_words_tests {
    use super::*;

    fn one(phase: Phase) -> Progress {
        Progress {
            id: "t".into(),
            name: "a.txt".into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 40,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn words(listed: usize, aim: Aim, code: Option<CodeCircle>, batch: &[Progress]) -> CircleWords {
        let circle = circle_view(&CircleInputs {
            listed,
            aim,
            send_instantly: false,
            code: code.clone(),
            batch,
            handed_over: 0,
        });
        circle_view_words_in(Language::English, &circle, code.as_ref(), 1, true)
    }

    #[test]
    fn with_nothing_at_all_it_asks_for_files_in_the_large_form() {
        let w = words(0, Aim::Code, None, &[]);
        assert!(w.asks_for_files);
        assert_eq!(w.headline, "CHOOSE FILES");
    }

    #[test]
    fn while_a_file_goes_it_says_so_and_never_asks_for_files() {
        let w = words(0, Aim::Code, None, &[one(Phase::Moving)]);
        assert!(
            !w.asks_for_files,
            "a circle with a transfer going asked for files"
        );
        assert_eq!(w.kicker, "SENDING");
        assert_eq!(w.headline, "40%");
    }

    #[test]
    fn after_a_cancel_it_says_how_it_ended_and_does_not_ask_for_files_in_the_large_form() {
        let w = words(0, Aim::Code, None, &[one(Phase::Cancelled)]);
        assert!(!w.asks_for_files);
        assert_eq!(w.kicker, "STOPPED");
        assert_eq!(w.headline, "0/1");
    }

    #[test]
    fn files_listed_say_what_the_circle_will_do_whatever_ended_before() {
        let hold = words(
            2,
            Aim::Devices(vec!["x".into()]),
            None,
            &[one(Phase::Cancelled)],
        );
        assert_eq!(hold.kicker, "HOLD TO SEND");
        assert!(!hold.asks_for_files);
        let code = words(2, Aim::Code, None, &[one(Phase::Cancelled)]);
        assert_eq!(code.kicker, "NO DEVICE PICKED");
    }

    #[test]
    fn a_code_that_waits_for_the_other_device_says_so_in_the_codes_own_words() {
        let w = words(0, Aim::Code, Some(CodeCircle::Scanned), &[]);
        assert!(!w.asks_for_files);
        assert!(!w.headline.is_empty());
    }
}

#[cfg(test)]
mod tap_tests {
    use super::*;
    #[test]
    fn tap_and_small_jitter_select_but_swiping_and_returning_never_select() {
        let mut tap = Tap::default();
        tap.down((20., 20.));
        assert!(tap.up((20. + TAP_SLOP, 20.)));
        assert!(!tap.up((20., 20.)), "one gesture selected twice");
        tap.down((20., 20.));
        tap.moved((20., 200.));
        tap.moved((20., 20.));
        assert!(!tap.up((20., 20.)), "scrolling selected a search result");
        tap.down((20., 20.));
        tap.cancel();
        assert!(!tap.up((20., 20.)));
    }

    #[test]
    fn a_short_touch_is_a_tap_and_a_long_one_is_a_hold_and_never_both() {
        let mut hold = Hold::default();
        hold.down((50., 50.), 1_000);
        assert_eq!(hold.up((50., 50.), 1_000 + HOLD_MS - 1), Ended::Tap);
        hold.down((50., 50.), 1_000);
        assert_eq!(
            hold.up((50., 50.), 1_000 + HOLD_MS),
            Ended::Hold,
            "the boundary itself is a hold"
        );
        // One touch ends once: a second lift with no touch before it is nothing.
        assert_eq!(hold.up((50., 50.), 9_999), Ended::Nothing);
    }

    #[test]
    fn a_touch_that_strays_is_a_drag_and_never_a_hold_however_long_it_stays_down() {
        let mut hold = Hold::default();
        hold.down((50., 50.), 0);
        hold.moved((50., 50. + TAP_SLOP + 1.));
        assert!(!hold.is_holding(HOLD_MS * 10), "a drag was held");
        // Coming back to where it began does not undo it, as for a tap.
        hold.moved((50., 50.));
        assert_eq!(hold.up((50., 50.), HOLD_MS * 10), Ended::Nothing);
    }

    #[test]
    fn small_jitter_while_holding_is_still_a_hold() {
        let mut hold = Hold::default();
        hold.down((50., 50.), 0);
        hold.moved((50. + TAP_SLOP, 50.));
        assert!(hold.is_holding(HOLD_MS));
        assert_eq!(hold.up((50. + TAP_SLOP, 50.), HOLD_MS + 40), Ended::Hold);
    }

    #[test]
    fn a_finger_is_not_holding_until_the_time_is_up_and_not_at_all_without_a_touch() {
        let mut hold = Hold::default();
        assert!(!hold.is_holding(HOLD_MS * 10), "held with no touch");
        hold.down((10., 10.), 100);
        assert!(!hold.is_holding(100 + HOLD_MS - 1));
        assert!(hold.is_holding(100 + HOLD_MS));
    }

    #[test]
    fn a_cancelled_touch_is_nothing_and_does_not_leak_into_the_next() {
        let mut hold = Hold::default();
        hold.down((10., 10.), 0);
        hold.cancel();
        assert!(!hold.is_holding(HOLD_MS * 2));
        assert_eq!(hold.up((10., 10.), HOLD_MS * 2), Ended::Nothing);
        hold.down((10., 10.), 5_000);
        assert_eq!(
            hold.up((10., 10.), 5_000 + MIN_TOUCH_MS),
            Ended::Tap,
            "the cancelled touch's clock was kept"
        );
    }

    #[test]
    fn a_clock_that_runs_backwards_never_makes_a_hold() {
        let mut hold = Hold::default();
        hold.down((10., 10.), 10_000);
        assert_eq!(
            hold.up((10., 10.), 5),
            Ended::Nothing,
            "a clock that ran backwards made a touch"
        );
    }

    #[test]
    fn a_brush_of_the_screen_is_not_a_touch_and_a_meant_one_is_whatever_the_circle() {
        let mut hold = Hold::default();
        hold.down((50., 50.), 1_000);
        assert_eq!(
            hold.up((50., 50.), 1_000),
            Ended::Nothing,
            "a contact of no time was a tap"
        );
        hold.down((50., 50.), 1_000);
        assert_eq!(
            hold.up((50., 50.), 1_000 + MIN_TOUCH_MS - 1),
            Ended::Nothing,
            "a brush was a tap"
        );
        hold.down((50., 50.), 1_000);
        assert_eq!(
            hold.up((50., 50.), 1_000 + MIN_TOUCH_MS),
            Ended::Tap,
            "the boundary itself is a meant touch"
        );
        const {
            assert!(
                MIN_TOUCH_MS * 5 < HOLD_MS,
                "the guard is so long that a person tapping would feel it"
            )
        };
    }
}

#[cfg(test)]
mod list_under_the_circle_tests {
    use super::*;

    fn one(id: &str, phase: Phase) -> Progress {
        Progress {
            id: id.to_owned(),
            name: format!("{id}.bin"),
            folders: Vec::new(),
            bytes: 10,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn the_file_that_is_going_is_first_so_nobody_has_to_scroll_for_it_and_what_is_over_follows_newest_first()
     {
        // The record keeps transfers in the order they were made: the oldest first.
        let all = vec![
            one("a", Phase::Done),
            one("b", Phase::Done),
            one("c", Phase::Moving),
            one("d", Phase::Done),
            one("e", Phase::Waiting),
        ];
        let ids: Vec<&str> = transfer_list(&all).map(|item| item.id.as_str()).collect();
        assert_eq!(
            &ids[..2],
            ["c", "e"],
            "a file that is going was not on top: {ids:?}"
        );
        assert_eq!(
            &ids[2..],
            ["d", "b", "a"],
            "what is over was not newest first: {ids:?}"
        );
    }

    #[test]
    fn after_the_transfer_ends_the_file_stays_with_its_status_until_it_is_hidden_and_then_it_is_gone_from_the_list_not_from_the_record()
     {
        let start: Vec<String> = Vec::new();
        let mut all = vec![one("now", Phase::Moving)];
        assert_eq!(of_this_run(&all, &start).len(), 1);
        all[0].phase = Phase::Failed("the other device left".into());
        let kept = of_this_run(&all, &start);
        assert_eq!(kept.len(), 1, "a failed file vanished by itself");
        assert_eq!(
            kept[0].phase,
            Phase::Failed("the other device left".into()),
            "the status was lost"
        );
        // X puts away what is over; the record itself is untouched.
        let hidden = put_away_now(&all);
        assert!(
            not_put_away(&all, &hidden).is_empty(),
            "the hidden file was still listed"
        );
        assert_eq!(all.len(), 1, "hiding changed the record");
    }
}

#[cfg(test)]
mod fingers_tests {
    use super::*;

    #[test]
    fn one_finger_is_not_two_and_the_second_finger_is_two_once() {
        let mut fingers = Fingers::default();
        assert!(!fingers.went_down(1, 0));
        assert!(
            fingers.went_down(2, 10),
            "the second finger was not a touch of two"
        );
        assert!(!fingers.went_down(3, 20), "a third finger said two again");
    }

    #[test]
    fn a_finger_that_was_never_reported_lifted_does_not_make_every_later_touch_a_touch_of_two() {
        // The failure: Settings closes under a finger, its lift never reaches the window, and the ghost is counted for ever.
        let mut fingers = Fingers::default();
        assert!(!fingers.went_down(7, 0));
        // A later, ordinary touch, long after, with one finger only.
        let later = FINGER_GONE_MS + 1;
        assert!(
            !fingers.went_down(9, later),
            "a ghost finger made an ordinary touch open Settings"
        );
        assert_eq!(fingers.count(), 1);
    }

    #[test]
    fn a_lifted_finger_is_gone_at_once_so_the_next_touch_is_alone() {
        let mut fingers = Fingers::default();
        fingers.went_down(1, 0);
        fingers.went_up(1);
        assert!(!fingers.went_down(2, 5));
        assert_eq!(fingers.count(), 1);
    }

    #[test]
    fn the_system_giving_a_number_again_is_one_finger_and_not_two() {
        let mut fingers = Fingers::default();
        fingers.went_down(4, 0);
        assert!(
            !fingers.went_down(4, 100),
            "the same finger twice was a touch of two"
        );
        assert_eq!(fingers.count(), 1);
    }

    #[test]
    fn a_finger_that_keeps_moving_is_still_down_however_long_it_stays() {
        let mut fingers = Fingers::default();
        fingers.went_down(1, 0);
        for step in 1..=5 {
            fingers.moved(1, step * (FINGER_GONE_MS - 1));
        }
        // Alive: a second finger now is a touch of two.
        assert!(fingers.went_down(2, 5 * (FINGER_GONE_MS - 1) + 1));
    }

    #[test]
    fn clearing_forgets_every_finger() {
        let mut fingers = Fingers::default();
        fingers.went_down(1, 0);
        fingers.went_down(2, 1);
        fingers.clear();
        assert_eq!(fingers.count(), 0);
        assert!(!fingers.went_down(3, 2));
    }
}

#[cfg(test)]
mod one_batch_tests {
    use super::*;

    fn one(id: &str, name: &str, phase: Phase) -> Progress {
        Progress {
            id: id.to_owned(),
            name: name.to_owned(),
            folders: Vec::new(),
            bytes: 10,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn choosing_new_files_starts_a_new_list_and_the_cancelled_or_sent_batch_before_it_is_not_in_it()
    {
        let mut all = vec![
            one("t1", "a.txt", Phase::Cancelled),
            one("t2", "b.txt", Phase::Done),
        ];
        // The person chooses c.txt: the batch before is no longer the circle's list.
        let before = ids_at_start(&all);
        let rows = list_under_circle(&["c.txt".into()], &of_this_run(&all, &before));
        assert_eq!(
            rows,
            [ListRow::Prepared("c.txt".into())],
            "an earlier batch stayed under the circle: {rows:?}"
        );
        // c.txt is sent and goes: its transfer is the batch's, and only it.
        all.push(one("t3", "c.txt", Phase::Moving));
        let rows = list_under_circle(&[], &of_this_run(&all, &before));
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], ListRow::Transfer(item) if item.id == "t3"));
    }

    #[test]
    fn a_file_chosen_again_after_it_was_cancelled_is_one_prepared_row_and_not_a_cancelled_one_beside_it()
     {
        let all = vec![one("t1", "a.txt", Phase::Cancelled)];
        let before = ids_at_start(&all);
        let rows = list_under_circle(&["a.txt".into()], &of_this_run(&all, &before));
        assert_eq!(
            rows,
            [ListRow::Prepared("a.txt".into())],
            "the cancelled file stayed beside the prepared one: {rows:?}"
        );
    }

    #[test]
    fn a_transfer_still_going_from_the_batch_before_stays_in_the_list_until_it_ends() {
        let all = vec![one("t1", "a.txt", Phase::Moving)];
        let before = ids_at_start(&all);
        let rows = list_under_circle(&["b.txt".into()], &of_this_run(&all, &before));
        assert_eq!(rows.len(), 2, "a transfer in progress was hidden: {rows:?}");
    }

    #[test]
    fn the_list_never_holds_more_than_the_batch_however_many_were_sent_before() {
        let history: Vec<Progress> = (0..5_000)
            .map(|n| one(&format!("old-{n}"), &format!("f{n}.bin"), Phase::Done))
            .collect();
        let before = ids_at_start(&history);
        assert!(list_under_circle(&[], &of_this_run(&history, &before)).is_empty());
        assert_eq!(
            list_under_circle(
                &["x.txt".into(), "y.txt".into()],
                &of_this_run(&history, &before)
            )
            .len(),
            2
        );
    }
}

#[cfg(test)]
mod list_of_the_circle_tests {
    use super::*;

    fn sent(id: &str, name: &str, phase: Phase) -> Progress {
        Progress {
            id: id.to_owned(),
            name: name.to_owned(),
            folders: Vec::new(),
            bytes: 10,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn names(rows: &[ListRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                ListRow::Prepared(name) => format!("prepared:{name}"),
                ListRow::Transfer(item) => format!("{:?}:{}", item.phase, item.name),
            })
            .collect()
    }

    #[test]
    fn files_that_are_only_listed_are_rows_with_their_status_prepared_before_anything_is_sent() {
        let rows = list_under_circle(&["a.txt".into(), "b.txt".into()], &of_this_run(&[], &[]));
        assert_eq!(names(&rows), ["prepared:a.txt", "prepared:b.txt"]);
    }

    #[test]
    fn a_transfer_that_has_begun_has_its_own_row_and_the_files_still_listed_stay_prepared() {
        // a.txt was sent (it is no longer listed); b.txt is still listed.
        let all = vec![sent("t1", "a.txt", Phase::Moving)];
        let rows = list_under_circle(&["b.txt".into()], &of_this_run(&all, &[]));
        assert_eq!(names(&rows), ["Moving:a.txt", "prepared:b.txt"]);
    }

    #[test]
    fn the_file_that_is_going_is_first_then_what_is_over_then_what_is_still_to_go() {
        let all = vec![
            sent("t1", "a.txt", Phase::Done),
            sent("t2", "b.txt", Phase::Moving),
        ];
        let rows = list_under_circle(&["c.txt".into()], &of_this_run(&all, &[]));
        assert_eq!(
            names(&rows),
            ["Moving:b.txt", "Done:a.txt", "prepared:c.txt"]
        );
    }

    #[test]
    fn two_listed_files_with_one_name_are_two_rows() {
        let rows = list_under_circle(
            &["same.txt".into(), "same.txt".into()],
            &of_this_run(&[], &[]),
        );
        assert_eq!(names(&rows), ["prepared:same.txt", "prepared:same.txt"]);
    }

    #[test]
    fn the_history_of_the_start_is_never_in_it_and_a_failed_file_keeps_its_reason() {
        let old: Vec<Progress> = (0..50)
            .map(|n| sent(&format!("old-{n}"), "old.bin", Phase::Done))
            .collect();
        let start = ids_at_start(&old);
        let mut all = old;
        all.push(sent(
            "now",
            "a.txt",
            Phase::Failed("the other device left".into()),
        ));
        // Sent, so no longer listed: the file is the transfer, with its reason.
        let rows = list_under_circle(&[], &of_this_run(&all, &start));
        assert_eq!(rows.len(), 1, "history was listed: {rows:?}");
        assert_eq!(names(&rows), ["Failed(\"the other device left\"):a.txt"]);
    }

    #[test]
    fn a_file_that_is_chosen_again_after_its_transfer_was_cancelled_is_listed_again() {
        // The person chose a.txt, cancelled its transfer, and chose a.txt again: the new choice is a file that has not been sent.
        let all = vec![sent("t1", "a.txt", Phase::Cancelled)];
        let rows = list_under_circle(&["a.txt".into()], &of_this_run(&all, &[]));
        assert_eq!(
            names(&rows),
            ["Cancelled:a.txt", "prepared:a.txt"],
            "a file chosen again was taken for the cancelled one"
        );
    }

    #[test]
    fn a_file_chosen_while_others_are_going_or_over_is_listed_after_them_and_is_never_lost() {
        let all = vec![
            sent("t1", "a.txt", Phase::Cancelled),
            sent("t2", "c.txt", Phase::Moving),
        ];
        let rows = list_under_circle(&["b.txt".into(), "d.txt".into()], &of_this_run(&all, &[]));
        assert_eq!(
            names(&rows),
            [
                "Moving:c.txt",
                "Cancelled:a.txt",
                "prepared:b.txt",
                "prepared:d.txt"
            ]
        );
    }

    #[test]
    fn a_file_that_came_in_is_not_a_file_that_was_listed_to_go_out() {
        let mut incoming = sent("in", "a.txt", Phase::Moving);
        incoming.incoming = true;
        let rows = list_under_circle(&["a.txt".into()], &of_this_run(&[incoming], &[]));
        assert_eq!(
            names(&rows),
            ["Moving:a.txt", "prepared:a.txt"],
            "a file received took the place of one to be sent"
        );
    }
}

#[cfg(test)]
mod this_run_tests {
    use super::*;

    fn transfer(id: &str, phase: Phase) -> Progress {
        Progress {
            id: id.to_owned(),
            name: format!("{id}.bin"),
            folders: Vec::new(),
            bytes: 0,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn the_history_that_was_there_at_the_start_is_never_listed_however_long_it_is() {
        let history: Vec<Progress> = (0..5_000)
            .map(|n| transfer(&format!("old-{n}"), Phase::Done))
            .collect();
        let start = ids_at_start(&history);
        assert!(
            of_this_run(&history, &start).is_empty(),
            "the old record was listed on Send or Receive"
        );
    }

    #[test]
    fn one_file_sent_now_is_one_row_not_the_record_and_it_stays_when_it_is_over() {
        let mut all: Vec<Progress> = (0..200)
            .map(|n| transfer(&format!("old-{n}"), Phase::Done))
            .collect();
        let start = ids_at_start(&all);
        all.push(transfer("now", Phase::Moving));
        let going = of_this_run(&all, &start);
        assert_eq!(going.len(), 1);
        assert_eq!(going[0].id, "now");
        all.last_mut().unwrap().phase = Phase::Done;
        let over = of_this_run(&all, &start);
        assert_eq!(over.len(), 1, "the transfer vanished the moment it ended");
        assert_eq!(over[0].id, "now");
    }

    #[test]
    fn a_transfer_begun_before_the_start_and_still_going_is_listed_until_it_ends_and_then_it_is_history()
     {
        let mut all = vec![transfer("early", Phase::Moving)];
        let start = ids_at_start(&all);
        assert_eq!(
            of_this_run(&all, &start).len(),
            1,
            "a transfer in progress was hidden"
        );
        all[0].phase = Phase::Done;
        assert!(of_this_run(&all, &start).is_empty());
    }
}

#[cfg(test)]
mod batch_state_tests {
    use super::*;

    fn one(id: &str, name: &str, phase: Phase, percent: u8) -> Progress {
        Progress {
            id: id.to_owned(),
            name: name.to_owned(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn no_transfer_is_no_batch_and_a_received_file_is_not_a_batch_of_this_persons() {
        assert_eq!(batch_state(&[]), BatchState::None);
        let mut incoming = one("in", "a.txt", Phase::Moving, 10);
        incoming.incoming = true;
        assert_eq!(
            batch_state(&[incoming]),
            BatchState::None,
            "a file that came in made a batch that went out"
        );
    }

    #[test]
    fn while_a_file_goes_the_batch_is_going_with_its_percent_and_the_file_that_is_going() {
        let batch = [
            one("a", "a.txt", Phase::Done, 100),
            one("b", "b.txt", Phase::Moving, 40),
        ];
        let BatchState::Going {
            files,
            percent,
            current,
        } = batch_state(&batch)
        else {
            panic!("not going")
        };
        assert_eq!(files, 2);
        assert_eq!(current.as_deref(), Some("b.txt"));
        assert!(percent > 0 && percent < 100, "{percent}");
    }

    #[test]
    fn a_file_still_waiting_to_be_accepted_is_going_too_so_the_circle_does_not_ask_for_files() {
        let batch = [one("a", "a.txt", Phase::Waiting, 0)];
        assert!(matches!(
            batch_state(&batch),
            BatchState::Going { files: 1, .. }
        ));
    }

    #[test]
    fn when_all_arrived_it_is_sent_and_when_some_did_not_it_says_how_many_did() {
        assert_eq!(
            batch_state(&[
                one("a", "a", Phase::Done, 100),
                one("b", "b", Phase::Done, 100)
            ]),
            BatchState::Sent { files: 2 }
        );
        assert_eq!(
            batch_state(&[
                one("a", "a", Phase::Done, 100),
                one("b", "b", Phase::Cancelled, 10)
            ]),
            BatchState::Stopped { files: 2, done: 1 }
        );
        assert_eq!(
            batch_state(&[one("a", "a", Phase::Failed("x".into()), 0)]),
            BatchState::Stopped { files: 1, done: 0 }
        );
    }
}

#[cfg(test)]
mod batch_words_tests {
    use super::*;

    #[test]
    fn every_state_of_a_batch_has_words_and_no_batch_has_none() {
        assert_eq!(batch_words_in(Language::English, &BatchState::None), None);
        for state in [
            BatchState::Going {
                files: 2,
                percent: 40,
                current: Some("b.txt".into()),
            },
            BatchState::Going {
                files: 2,
                percent: 40,
                current: None,
            },
            BatchState::Sent { files: 1 },
            BatchState::Sent { files: 3 },
            BatchState::Stopped { files: 3, done: 1 },
        ] {
            let (kicker, headline, line) =
                batch_words_in(Language::English, &state).expect("a batch had no words");
            assert!(
                !kicker.is_empty() && !headline.is_empty() && !line.is_empty(),
                "{state:?} left a line empty"
            );
        }
    }

    #[test]
    fn while_it_goes_the_percent_and_the_file_that_is_going_are_said() {
        let (kicker, headline, line) = batch_words_in(
            Language::English,
            &BatchState::Going {
                files: 3,
                percent: 40,
                current: Some("b.txt".into()),
            },
        )
        .unwrap();
        assert_eq!(
            (kicker.as_str(), headline.as_str(), line.as_str()),
            ("SENDING", "40%", "b.txt")
        );
    }

    #[test]
    fn when_it_stopped_it_says_how_many_arrived_and_not_a_bare_error() {
        let (kicker, headline, line) = batch_words_in(
            Language::English,
            &BatchState::Stopped { files: 1, done: 0 },
        )
        .unwrap();
        assert_eq!((kicker.as_str(), headline.as_str()), ("STOPPED", "0/1"));
        assert!(line.contains("0 of 1"), "{line}");
    }
}

#[cfg(test)]
mod circle_words_tests {
    use super::*;

    #[test]
    fn every_thing_the_circle_can_do_has_its_own_words_and_none_is_left_empty() {
        for does in [
            CircleDoes::ChooseFiles,
            CircleDoes::MakeCode,
            CircleDoes::HoldToSend,
            CircleDoes::TapToSend,
            CircleDoes::Held,
        ] {
            let (kicker, headline, line) = circle_does_words_in(Language::English, does, 2);
            assert!(
                !kicker.is_empty() && !headline.is_empty() && !line.is_empty(),
                "{does:?} left a line empty"
            );
        }
    }

    #[test]
    fn a_code_a_hold_an_instant_send_and_a_held_back_circle_each_say_something_different() {
        let words = |does| circle_does_words_in(Language::English, does, 1);
        let all = [
            CircleDoes::MakeCode,
            CircleDoes::HoldToSend,
            CircleDoes::TapToSend,
            CircleDoes::Held,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(words(*a), words(*b), "{a:?} and {b:?} say the same");
            }
        }
    }

    #[test]
    fn the_number_of_devices_is_said_when_there_is_more_than_one() {
        let (_, one, _) = circle_does_words_in(Language::English, CircleDoes::HoldToSend, 1);
        let (_, three, _) = circle_does_words_in(Language::English, CircleDoes::HoldToSend, 3);
        assert!(three.contains('3'), "{three}");
        assert_ne!(one, three);
    }
}

#[cfg(test)]
mod circle_touch_tests {
    use super::*;
    const MID: (f32, f32) = (50., 50.);
    const HOLD: CircleDoes = CircleDoes::HoldToSend;

    fn down_at(touch: &mut CircleTouch, in_box: (f32, f32), at: u64) {
        touch.down(in_box, (200., 300.), at);
    }

    /// A finger that went down at `from_ms` and rests on the circle until `until_ms`, heard of every 100 ms as a real one is (it trembles, and the
    /// toolkit says so): the last thing the clock said.
    fn held_until(
        touch: &mut CircleTouch,
        from_ms: u64,
        until_ms: u64,
        does: CircleDoes,
    ) -> CircleAct {
        let mut last = CircleAct::Nothing;
        let mut at = from_ms;
        while at < until_ms {
            at = (at + 100).min(until_ms);
            touch.seen(at);
            let act = touch.tick(at, does);
            if act != CircleAct::Nothing {
                last = act;
            }
        }
        last
    }

    #[test]
    fn holding_fills_and_sends_once_when_full_without_waiting_for_the_lift_and_the_lift_then_sends_nothing()
     {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 1_000);
        assert_eq!(
            held_until(&mut touch, 1_000, 1_000 + CONFIRM_MS - 1, HOLD),
            CircleAct::Nothing
        );
        assert_eq!(
            held_until(&mut touch, 1_000 + CONFIRM_MS - 1, 1_000 + CONFIRM_MS, HOLD),
            CircleAct::Send
        );
        assert_eq!(
            touch.tick(1_000 + CONFIRM_MS + 50, HOLD),
            CircleAct::Nothing,
            "a touch sent twice"
        );
        assert_eq!(
            touch.up(MID, (200., 300.), 1_000 + CONFIRM_MS + 80, HOLD),
            CircleAct::Nothing,
            "the lift sent a second time"
        );
    }

    #[test]
    fn lifting_before_it_is_full_sends_nothing_and_leaves_no_fill() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert!(touch.fill(CONFIRM_MS / 2, HOLD) > 0.4);
        assert_eq!(
            touch.up(MID, (200., 300.), CONFIRM_MS - 1, HOLD),
            CircleAct::Nothing
        );
        assert_eq!(
            touch.fill(CONFIRM_MS * 3, HOLD),
            0.,
            "a lifted finger kept the circle filling"
        );
        assert!(!touch.is_down());
    }

    #[test]
    fn a_touch_in_the_corner_of_the_box_outside_the_round_never_starts_a_hold() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, (2., 2.), 0);
        assert!(!touch.is_down(), "a corner of the square started a hold");
        assert_eq!(touch.tick(CONFIRM_MS * 5, HOLD), CircleAct::Nothing);
        assert_eq!(
            touch.up(MID, (200., 300.), CONFIRM_MS * 5, CircleDoes::TapToSend),
            CircleAct::Nothing
        );
    }

    #[test]
    fn a_drag_ends_the_hold_for_good_even_if_the_finger_comes_back() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        touch.moved((200., 300. + TAP_SLOP + 1.));
        touch.moved((200., 300.));
        assert!(!touch.is_down());
        assert_eq!(
            touch.tick(CONFIRM_MS * 2, HOLD),
            CircleAct::Nothing,
            "a drag sent the files"
        );
        assert_eq!(
            touch.up(MID, (200., 300.), CONFIRM_MS * 2, HOLD),
            CircleAct::Nothing
        );
    }

    #[test]
    fn with_no_device_picked_a_whole_tap_makes_a_code_and_nothing_fills_or_waits() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.fill(CONFIRM_MS, CircleDoes::MakeCode),
            0.,
            "a circle for a code filled"
        );
        assert_eq!(
            touch.tick(CONFIRM_MS * 3, CircleDoes::MakeCode),
            CircleAct::Nothing,
            "a code was made by waiting"
        );
        // The person has to touch it for the code: a whole tap.
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.up(MID, (200., 300.), 80, CircleDoes::MakeCode),
            CircleAct::MakeCode
        );
        // A long press is not the tap that asks for it, and a drag never is.
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.up(MID, (200., 300.), HOLD_MS + 10, CircleDoes::MakeCode),
            CircleAct::Nothing,
            "a long press made a code"
        );
    }

    #[test]
    fn with_devices_picked_a_tap_does_not_send_for_holding_is_the_confirmation() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.up(MID, (200., 300.), 80, HOLD),
            CircleAct::Nothing,
            "a tap sent without the hold the person kept on"
        );
    }

    #[test]
    fn a_circle_held_back_by_a_device_that_went_away_does_nothing_at_all() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(touch.fill(CONFIRM_MS, CircleDoes::Held), 0.);
        assert_eq!(
            touch.tick(CONFIRM_MS * 3, CircleDoes::Held),
            CircleAct::Nothing
        );
        assert_eq!(
            touch.up(MID, (200., 300.), 80, CircleDoes::Held),
            CircleAct::Nothing
        );
    }

    #[test]
    fn with_nothing_listed_a_tap_chooses_files_and_a_long_hold_does_not() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.up(MID, (200., 300.), 100, CircleDoes::ChooseFiles),
            CircleAct::Choose
        );
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.up(MID, (200., 300.), HOLD_MS + 10, CircleDoes::ChooseFiles),
            CircleAct::Nothing
        );
    }

    #[test]
    fn send_instantly_sends_on_a_whole_tap_only_and_never_fills() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(touch.fill(CONFIRM_MS * 4, CircleDoes::TapToSend), 0.);
        assert_eq!(
            held_until(&mut touch, 0, CONFIRM_MS * 4, CircleDoes::TapToSend),
            CircleAct::Nothing,
            "a finger resting on the circle sent in the instant mode"
        );
        // The touch above went on for so long that it was a hold: a new one is the tap.
        down_at(&mut touch, MID, 10_000);
        assert_eq!(
            touch.up(MID, (200., 300.), 10_080, CircleDoes::TapToSend),
            CircleAct::Send
        );
        down_at(&mut touch, MID, 20_000);
        assert_eq!(
            touch.up(
                MID,
                (200., 300.),
                20_000 + HOLD_MS + 10,
                CircleDoes::TapToSend
            ),
            CircleAct::Nothing,
            "a long press sent instantly"
        );
    }

    #[test]
    fn a_new_touch_after_a_send_can_send_again_and_a_cancel_sends_nothing() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(held_until(&mut touch, 0, CONFIRM_MS, HOLD), CircleAct::Send);
        touch.up(MID, (200., 300.), CONFIRM_MS + 1, HOLD);
        down_at(&mut touch, MID, 5_000);
        touch.cancel();
        assert_eq!(
            touch.tick(5_000 + CONFIRM_MS * 2, HOLD),
            CircleAct::Nothing,
            "a cancelled touch sent"
        );
        down_at(&mut touch, MID, 9_000);
        assert_eq!(
            held_until(&mut touch, 9_000, 9_000 + CONFIRM_MS, HOLD),
            CircleAct::Send
        );
    }
}

#[cfg(test)]
mod finger_life_tests {
    use super::*;
    const MID: (f32, f32) = (50., 50.);

    #[test]
    fn a_finger_that_is_no_longer_heard_of_never_sends_however_long_the_clock_runs() {
        // The finger went down and its lift never reached the circle: nothing says it is gone but the silence.
        let mut touch = CircleTouch::new(100.);
        touch.down(MID, (200., 300.), 0);
        assert_eq!(
            touch.tick(FINGER_LIVE_MS + 1, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
        assert!(
            !touch.is_down(),
            "a silent finger was still taken to be down"
        );
        assert_eq!(
            touch.tick(CONFIRM_MS * 5, CircleDoes::HoldToSend),
            CircleAct::Nothing,
            "the circle sent files nobody was holding"
        );
    }

    #[test]
    fn a_silent_finger_leaves_the_circle_empty_so_it_does_not_go_on_charging() {
        let mut touch = CircleTouch::new(100.);
        touch.down(MID, (200., 300.), 0);
        touch.seen(100);
        assert!(touch.fill(100 + FINGER_LIVE_MS / 2, CircleDoes::HoldToSend) > 0.);
        // Silence: the next tick ends it, and what the circle shows is empty, for good.
        touch.tick(100 + FINGER_LIVE_MS + 1, CircleDoes::HoldToSend);
        assert_eq!(
            touch.fill(100 + FINGER_LIVE_MS + 2, CircleDoes::HoldToSend),
            0.,
            "the circle went on charging for a finger that was gone"
        );
        assert!(!touch.is_down());
    }

    #[test]
    fn a_sign_of_a_finger_that_is_not_down_brings_nothing_back() {
        let mut touch = CircleTouch::new(100.);
        touch.seen(1_000);
        assert!(!touch.is_down());
        assert_eq!(
            touch.tick(1_000 + CONFIRM_MS, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
    }

    #[test]
    fn the_life_of_a_finger_is_shorter_than_the_hold_so_a_lost_lift_is_noticed_before_it_could_send()
     {
        const { assert!(FINGER_LIVE_MS < HOLD_MS && HOLD_MS < CONFIRM_MS) };
    }

    #[test]
    fn a_mouse_held_perfectly_still_fills_and_sends_for_it_says_nothing_while_it_rests_and_that_is_not_a_lost_lift()
     {
        let mut mouse = CircleTouch::new(100.);
        mouse.down(MID, (200., 300.), 0);
        mouse.steady();
        // No sign of it at all, from the moment it went down to the moment the circle is full, as with a hand that does not move.
        let mut sent_at = None;
        for ms in (0..=CONFIRM_MS + 100).step_by(33) {
            if mouse.tick(ms, CircleDoes::HoldToSend) == CircleAct::Send {
                sent_at = Some(ms);
                break;
            }
            assert!(
                mouse.is_down(),
                "a mouse that rests was taken to be gone at {ms} ms"
            );
        }
        let sent_at = sent_at.expect("a mouse held for the whole time never sent");
        assert!(
            (CONFIRM_MS..CONFIRM_MS + 100).contains(&sent_at),
            "it sent at {sent_at} ms, not when it was full"
        );
    }

    #[test]
    fn a_mouse_hold_still_ends_when_the_button_is_lifted_and_when_it_strays() {
        let mut lifted = CircleTouch::new(100.);
        lifted.down(MID, (200., 300.), 0);
        lifted.steady();
        lifted.cancel();
        assert!(!lifted.is_down(), "a released mouse was still down");
        // After a release the mark is gone: the next touch is judged on its own, by the pointer that makes it.
        lifted.down(MID, (200., 300.), 5_000);
        assert_eq!(
            lifted.tick(5_000 + FINGER_LIVE_MS + 1, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
        assert!(
            !lifted.is_down(),
            "the mark of a mouse outlived its touch and spared a finger"
        );
        let mut strayed = CircleTouch::new(100.);
        strayed.down(MID, (200., 300.), 0);
        strayed.steady();
        strayed.moved((900., 900.));
        assert!(
            !strayed.is_down(),
            "a mouse that left the circle was still held"
        );
        assert_eq!(
            strayed.tick(CONFIRM_MS * 5, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
    }

    #[test]
    fn a_finger_is_still_judged_by_its_silence_for_only_a_finger_can_lose_its_lift() {
        let mut finger = CircleTouch::new(100.);
        finger.down(MID, (200., 300.), 0);
        assert_eq!(
            finger.tick(FINGER_LIVE_MS + 1, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
        assert!(!finger.is_down(), "a finger that went silent was kept");
    }

    #[test]
    fn a_second_down_on_a_circle_whose_mouse_is_held_is_not_a_new_touch_and_steady_needs_a_touch() {
        let mut mouse = CircleTouch::new(100.);
        mouse.down(MID, (200., 300.), 0);
        mouse.steady();
        assert!(
            !mouse.down(MID, (200., 300.), 10_000),
            "a second pointer restarted a hold that was in progress"
        );
        // Said of a touch that is not down, it marks nothing: a later finger is not spared by it.
        let mut none = CircleTouch::new(100.);
        none.steady();
        none.down(MID, (200., 300.), 0);
        assert_eq!(
            none.tick(FINGER_LIVE_MS + 1, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
        assert!(
            !none.is_down(),
            "steady, said before any touch, spared the finger that came after"
        );
    }
}

#[cfg(test)]
mod nothing_to_send_tests {
    use super::*;

    fn one(phase: Phase) -> Progress {
        Progress {
            id: "t".into(),
            name: "a.txt".into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn with_nothing_listed_the_circle_is_never_held_to_send_or_asked_for_a_code_whoever_is_picked_and_whatever_ended()
     {
        let devices = Aim::Devices(vec!["laptop".into()]);
        for aim in [Aim::Code, devices, Aim::Held] {
            for batch in [
                vec![],
                vec![one(Phase::Cancelled)],
                vec![one(Phase::Done)],
                vec![one(Phase::Failed("x".into()))],
            ] {
                for send_instantly in [false, true] {
                    let view = circle_view(&CircleInputs {
                        listed: 0,
                        aim: aim.clone(),
                        send_instantly,
                        code: None,
                        batch: &batch,
                        handed_over: 0,
                    });
                    assert!(
                        !matches!(
                            view.does,
                            Some(
                                CircleDoes::HoldToSend
                                    | CircleDoes::TapToSend
                                    | CircleDoes::MakeCode
                                    | CircleDoes::Held
                            )
                        ),
                        "a circle with nothing listed offered to send or make a code ({aim:?}, {batch:?}, instantly={send_instantly}): {:?}",
                        view.does
                    );
                }
            }
        }
    }

    #[test]
    fn after_a_cancel_with_devices_still_picked_a_touch_chooses_files_and_does_not_start_charging()
    {
        let view = circle_view(&CircleInputs {
            listed: 0,
            aim: Aim::Devices(vec!["laptop".into()]),
            send_instantly: false,
            code: None,
            batch: &[one(Phase::Cancelled)],
            handed_over: 0,
        });
        assert_eq!(view.does, Some(CircleDoes::ChooseFiles));
        // And nothing in the touch model charges for it: only a circle that is held to send ever fills.
        let mut touch = CircleTouch::new(100.);
        touch.down((50., 50.), (200., 300.), 0);
        for at in (100..=3_000).step_by(100) {
            touch.seen(at);
            assert_eq!(
                touch.tick(at, view.does.unwrap()),
                CircleAct::Nothing,
                "it sent with nothing to send at {at}"
            );
            assert_eq!(
                touch.fill(at, view.does.unwrap()),
                0.,
                "the circle charged with nothing to send at {at}"
            );
        }
    }
}

#[cfg(test)]
mod second_down_tests {
    use super::*;

    #[test]
    fn a_second_down_while_the_first_is_alive_is_refused_and_leaves_the_touch_as_it_was() {
        let mut touch = CircleTouch::new(100.);
        assert!(
            touch.down((50., 50.), (200., 300.), 0),
            "the first down was refused"
        );
        touch.seen(100);
        assert!(
            !touch.down((50., 50.), (200., 300.), 150),
            "a second finger down on a live touch restarted it"
        );
        // The hold goes on from the FIRST down: it sends at the confirmation time counted from 0, and only once.
        let mut sent = Vec::new();
        for at in (200..=CONFIRM_MS + 300).step_by(100) {
            touch.seen(at);
            if touch.tick(at, CircleDoes::HoldToSend) == CircleAct::Send {
                sent.push(at);
            }
        }
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(
            sent[0] >= CONFIRM_MS && sent[0] < CONFIRM_MS + 150,
            "{sent:?}"
        );
    }

    #[test]
    fn after_the_touch_ended_the_next_down_is_taken() {
        let mut touch = CircleTouch::new(100.);
        assert!(touch.down((50., 50.), (200., 300.), 0));
        touch.up((50., 50.), (200., 300.), 200, CircleDoes::HoldToSend);
        assert!(
            touch.down((50., 50.), (200., 300.), 300),
            "a down after the lift was refused"
        );
    }

    #[test]
    fn a_down_outside_the_round_is_not_taken_so_it_never_starts_anything() {
        let mut touch = CircleTouch::new(100.);
        assert!(!touch.down((2., 2.), (200., 300.), 0));
        assert!(!touch.is_down());
    }
}

/// Where the circle of Send is in its sequence. One stage at any moment, decided from what the application already knows (the files listed, the code that
/// was made, the transfers of the batch chosen now); the circle keeps no state of its own about it. The stages follow one another: Empty, then Ready
/// when files are chosen, then Going when they are sent, then Ended when every transfer of the batch is over, and Ready again when the person chooses
/// new files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Nothing is listed and there is no batch: the circle asks for files.
    Empty,
    /// `files` are listed and wait for the circle; `aim` says where they would go.
    Ready { files: usize, aim: Aim },
    /// A code is drawn (or scanned and waiting): the circle is the code.
    Code { scanned: bool },
    /// The batch goes: `files` of it, `percent` of the way, `current` the file that is moving now.
    Going {
        files: usize,
        percent: u8,
        current: Option<String>,
    },
    /// Every transfer of the batch is over: how it ended.
    Ended { files: usize, arrived: usize },
}

impl Stage {
    /// How many files this stage is about: what the circle says of the count, from the one place that knows it for the stage.
    pub fn files(&self) -> usize {
        match self {
            Stage::Empty | Stage::Code { .. } => 0,
            Stage::Ready { files, .. }
            | Stage::Going { files, .. }
            | Stage::Ended { files, .. } => *files,
        }
    }
}

/// The stage, from the inputs. The order is the sequence: a code that is shown first, then a batch that goes, then files that are listed (a new
/// choice after a batch ended starts the next lap, whatever ended), then a batch that ended, then nothing.
pub fn stage_of(input: &CircleInputs) -> Stage {
    match &input.code {
        Some(CodeCircle::Code) => return Stage::Code { scanned: false },
        Some(CodeCircle::Scanned) => return Stage::Code { scanned: true },
        _ => {}
    }
    let batch = batch_state(input.batch);
    let code_goes = matches!(
        input.code,
        Some(CodeCircle::WaitingForApproval { .. } | CodeCircle::Sending { .. })
    ) && !matches!(batch, BatchState::Sent { .. } | BatchState::Stopped { .. });
    if let BatchState::Going {
        files,
        percent,
        current,
    } = &batch
    {
        return Stage::Going {
            files: *files,
            percent: *percent,
            current: current.clone(),
        };
    }
    if code_goes {
        let (files, percent) = match input.code {
            Some(CodeCircle::WaitingForApproval { files }) => (files, 0),
            Some(CodeCircle::Sending { files, percent }) => (files, percent),
            _ => (0, 0),
        };
        return Stage::Going {
            files,
            percent,
            current: None,
        };
    }
    if input.listed > 0 {
        return Stage::Ready {
            files: input.listed,
            aim: input.aim.clone(),
        };
    }
    match batch {
        BatchState::Sent { files } => Stage::Ended {
            files,
            arrived: files,
        },
        BatchState::Stopped { files, done } => Stage::Ended {
            files,
            arrived: done,
        },
        // Nothing of the batch is in the record yet, but files were just handed over and the owner has not answered: they are on their way, and the
        // circle says so instead of going back to asking for files. The owner's own transfers, once there, take over from this count.
        BatchState::None | BatchState::Going { .. } if input.handed_over > 0 => Stage::Going {
            files: input.handed_over,
            percent: 0,
            current: None,
        },
        BatchState::None | BatchState::Going { .. } => Stage::Empty,
    }
}

#[cfg(test)]
mod stage_tests {
    use super::*;

    fn one(id: &str, phase: Phase, percent: u8) -> Progress {
        Progress {
            id: id.into(),
            name: format!("{id}.txt"),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn stage(listed: usize, aim: Aim, code: Option<CodeCircle>, batch: &[Progress]) -> Stage {
        stage_of(&CircleInputs {
            listed,
            aim,
            send_instantly: false,
            code,
            batch,
            handed_over: 0,
        })
    }

    #[test]
    fn one_whole_lap_the_circle_goes_empty_ready_going_ended_and_ready_again_each_with_its_own_count()
     {
        let devices = Aim::Devices(vec!["laptop".into()]);
        assert_eq!(stage(0, devices.clone(), None, &[]), Stage::Empty);
        let ready = stage(2, devices.clone(), None, &[]);
        assert_eq!(
            ready,
            Stage::Ready {
                files: 2,
                aim: devices.clone()
            }
        );
        assert_eq!(ready.files(), 2);
        let going = stage(
            0,
            devices.clone(),
            None,
            &[one("a", Phase::Moving, 50), one("b", Phase::Waiting, 0)],
        );
        assert!(matches!(going, Stage::Going { files: 2, .. }), "{going:?}");
        assert_eq!(going.files(), 2);
        let ended = stage(
            0,
            devices.clone(),
            None,
            &[one("a", Phase::Cancelled, 10), one("b", Phase::Done, 100)],
        );
        assert_eq!(
            ended,
            Stage::Ended {
                files: 2,
                arrived: 1
            }
        );
        let again = stage(
            3,
            devices.clone(),
            None,
            &[one("a", Phase::Cancelled, 10), one("b", Phase::Done, 100)],
        );
        assert_eq!(
            again,
            Stage::Ready {
                files: 3,
                aim: devices
            }
        );
        assert_eq!(again.files(), 3);
    }

    #[test]
    fn the_count_is_the_one_of_the_stage_and_never_the_one_of_another_stage() {
        let batch = [one("a", Phase::Done, 100), one("b", Phase::Cancelled, 10)];
        let ready = stage(1, Aim::Code, None, &batch);
        assert_eq!(
            ready.files(),
            1,
            "the circle counted the files of the batch that ended"
        );
        let going = stage(0, Aim::Code, None, &[one("c", Phase::Moving, 20)]);
        assert_eq!(going.files(), 1);
    }

    #[test]
    fn a_cancel_with_nothing_listed_ends_the_lap_and_never_goes_back_to_ready_with_nothing_to_send()
    {
        let ended = stage(
            0,
            Aim::Devices(vec!["x".into()]),
            None,
            &[one("a", Phase::Cancelled, 5)],
        );
        assert_eq!(
            ended,
            Stage::Ended {
                files: 1,
                arrived: 0
            }
        );
        assert!(
            !matches!(ended, Stage::Ready { .. }),
            "a circle with nothing listed was Ready to send"
        );
    }

    #[test]
    fn a_code_is_its_own_stage_and_beats_everything_while_it_is_shown() {
        assert_eq!(
            stage(4, Aim::Code, Some(CodeCircle::Code), &[]),
            Stage::Code { scanned: false }
        );
        assert_eq!(
            stage(4, Aim::Code, Some(CodeCircle::Scanned), &[]),
            Stage::Code { scanned: true }
        );
    }

    #[test]
    fn a_code_still_producing_is_going_even_before_its_transfers_are_in_the_record_and_not_after_they_all_ended()
     {
        let sending = Some(CodeCircle::Sending {
            files: 5,
            percent: 20,
        });
        assert!(matches!(
            stage(0, Aim::Code, sending.clone(), &[]),
            Stage::Going {
                files: 5,
                percent: 20,
                ..
            }
        ));
        let ended = [one("a", Phase::Done, 100), one("b", Phase::Done, 100)];
        assert_eq!(
            stage(0, Aim::Code, sending, &ended),
            Stage::Ended {
                files: 2,
                arrived: 2
            }
        );
    }
}

/// The transfers that are the batch sent now: those the owner named when it took the files (`ids`), and those that came of the code the batch was
/// made behind (`code`, the generation the owner gave it). Taken from the record as it is now, so each carries its own state. Nothing is deduced from what
/// was there before: a transfer is the batch's because the owner said so or because it came of the batch's code, and for no other reason.
pub fn batch_of(transfers: &[Progress], ids: &[String], code: Option<u64>) -> Vec<Progress> {
    transfers
        .iter()
        .filter(|item| {
            !item.incoming && (ids.contains(&item.id) || (code.is_some() && item.code == code))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod batch_of_tests {
    use super::*;

    fn one(id: &str, name: &str, phase: Phase, code: Option<u64>) -> Progress {
        Progress {
            id: id.into(),
            name: name.into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn the_batch_is_exactly_the_transfers_the_owner_named_and_nothing_older_or_newer() {
        let record = vec![
            one("old", "old.txt", Phase::Done, None),
            one("a", "a.txt", Phase::Moving, None),
            one("b", "b.txt", Phase::Cancelled, None),
            one("later", "later.txt", Phase::Moving, None),
        ];
        let batch = batch_of(&record, &["a".into(), "b".into()], None);
        let ids: Vec<&str> = batch.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            ["a", "b"],
            "the batch took a transfer the owner did not name: {ids:?}"
        );
    }

    #[test]
    fn each_transfer_of_the_batch_carries_the_state_it_has_now() {
        let record = vec![one("a", "a.txt", Phase::Cancelled, None)];
        let batch = batch_of(&record, &["a".into()], None);
        assert_eq!(
            batch[0].phase,
            Phase::Cancelled,
            "the batch kept a state from when it was named"
        );
    }

    #[test]
    fn the_transfers_of_the_batchs_code_are_its_own_by_the_generation_and_another_codes_are_not() {
        let record = vec![
            one("a", "a.txt", Phase::Moving, Some(3)),
            one("b", "b.txt", Phase::Moving, Some(4)),
            one("c", "c.txt", Phase::Done, None),
        ];
        let batch = batch_of(&record, &[], Some(3));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, "a");
        assert!(
            batch_of(&record, &[], None).is_empty(),
            "no ids and no code made a batch out of the record"
        );
    }

    #[test]
    fn a_file_received_is_never_part_of_the_batch_that_was_sent() {
        let mut incoming = one("in", "a.txt", Phase::Moving, None);
        incoming.incoming = true;
        assert!(batch_of(&[incoming], &["in".into()], None).is_empty());
    }

    #[test]
    fn a_record_of_thousands_costs_nothing_the_batch_is_only_what_was_named() {
        let record: Vec<Progress> = (0..5_000)
            .map(|n| one(&format!("old-{n}"), "x.bin", Phase::Done, None))
            .collect();
        assert!(batch_of(&record, &[], None).is_empty());
        assert_eq!(batch_of(&record, &["old-7".into()], None).len(), 1);
    }
}

/// The ids of the batch that stay when new files are chosen: only the transfers that still go (moving, or waiting for an answer). What is over (done,
/// cancelled, failed) is the lap that ended and is let go of; what still goes is not forgotten, so the circle and the list never lose a send in flight
/// because the person began choosing the next files.
pub fn ids_kept_on_new_choice(
    transfers: &[Progress],
    ids: &[String],
    code: Option<u64>,
) -> Vec<String> {
    batch_of(transfers, ids, code)
        .into_iter()
        .filter(|item| !item.phase.is_over())
        .map(|item| item.id)
        .collect()
}

#[cfg(test)]
mod new_choice_tests {
    use super::*;

    fn one(id: &str, phase: Phase) -> Progress {
        Progress {
            id: id.into(),
            name: format!("{id}.txt"),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn choosing_new_files_keeps_what_still_goes_and_lets_go_of_what_is_over() {
        let record = vec![
            one("a", Phase::Moving),
            one("b", Phase::Cancelled),
            one("c", Phase::Done),
            one("d", Phase::Waiting),
        ];
        let kept = ids_kept_on_new_choice(
            &record,
            &["a".into(), "b".into(), "c".into(), "d".into()],
            None,
        );
        assert_eq!(
            kept,
            ["a", "d"],
            "a send in flight was forgotten, or one that was over was kept: {kept:?}"
        );
    }

    #[test]
    fn a_batch_that_is_all_over_is_let_go_of_entirely() {
        let record = vec![
            one("a", Phase::Cancelled),
            one("b", Phase::Failed("x".into())),
        ];
        assert!(ids_kept_on_new_choice(&record, &["a".into(), "b".into()], None).is_empty());
    }
}

#[cfg(test)]
mod handed_over_not_yet_answered_tests {
    use super::*;

    #[test]
    fn between_the_send_and_the_owners_answer_the_circle_is_going_and_never_asks_for_files() {
        // Nothing is listed any more (the files were taken), no transfer is named yet, no code: only the knowledge that N files were handed over.
        let inputs = CircleInputs {
            listed: 0,
            aim: Aim::Code,
            send_instantly: false,
            code: None,
            batch: &[],
            handed_over: 3,
        };
        let stage = stage_of(&inputs);
        assert_eq!(
            stage,
            Stage::Going {
                files: 3,
                percent: 0,
                current: None
            },
            "the circle fell back to asking for files in the gap"
        );
        assert_eq!(
            circle_view(&inputs).does,
            None,
            "a circle in the gap answered a touch"
        );
    }

    #[test]
    fn once_the_transfers_are_named_the_real_batch_takes_over_from_the_count() {
        let moving = Progress {
            id: "a".into(),
            name: "a.txt".into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 40,
            code: None,
            expires_at: None,
            saved_as: None,
            phase: Phase::Moving,
        };
        let batch = [moving];
        let inputs = CircleInputs {
            listed: 0,
            aim: Aim::Code,
            send_instantly: false,
            code: None,
            batch: &batch,
            handed_over: 3,
        };
        assert!(
            matches!(
                stage_of(&inputs),
                Stage::Going {
                    files: 1,
                    percent: 40,
                    ..
                }
            ),
            "{:?}",
            stage_of(&inputs)
        );
    }

    #[test]
    fn nothing_handed_over_and_nothing_else_is_empty() {
        let inputs = CircleInputs {
            listed: 0,
            aim: Aim::Code,
            send_instantly: false,
            code: None,
            batch: &[],
            handed_over: 0,
        };
        assert_eq!(stage_of(&inputs), Stage::Empty);
    }
}

#[cfg(test)]
mod three_files_two_devices_tests {
    use super::*;

    fn transfer(device: &str, n: usize, phase: Phase, percent: u8) -> Progress {
        Progress {
            id: format!("{device}-{n}"),
            name: format!("f{n}.bin"),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: device.into(),
            percent,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn batch_of_six(phase_of: impl Fn(&str) -> Phase, percent: u8) -> Vec<Progress> {
        ["laptop", "phone"]
            .iter()
            .flat_map(|device| (0..3).map(move |n| (*device, n)))
            .map(|(device, n)| transfer(device, n, phase_of(device), percent))
            .collect()
    }
    fn inputs<'a>(batch: &'a [Progress], handed_over: usize) -> CircleInputs<'a> {
        CircleInputs {
            listed: 0,
            aim: Aim::Devices(vec!["laptop".into(), "phone".into()]),
            send_instantly: false,
            code: None,
            batch,
            handed_over,
        }
    }

    #[test]
    fn the_circle_says_three_files_in_the_gap_while_it_goes_and_when_it_ends_never_six() {
        assert!(matches!(
            stage_of(&inputs(&[], 3)),
            Stage::Going { files: 3, .. }
        ));
        let going = batch_of_six(|_| Phase::Moving, 40);
        let stage = stage_of(&inputs(&going, 0));
        assert!(
            matches!(
                stage,
                Stage::Going {
                    files: 3,
                    percent: 40,
                    ..
                }
            ),
            "the circle counted a file once for each device: {stage:?}"
        );
        let done = batch_of_six(|_| Phase::Done, 100);
        assert_eq!(
            stage_of(&inputs(&done, 0)),
            Stage::Ended {
                files: 3,
                arrived: 3
            }
        );
    }

    #[test]
    fn when_one_device_refused_the_batch_stopped_and_says_how_many_files_arrived_everywhere() {
        let mixed = batch_of_six(
            |device| {
                if device == "laptop" {
                    Phase::Done
                } else {
                    Phase::Cancelled
                }
            },
            100,
        );
        assert_eq!(
            stage_of(&inputs(&mixed, 0)),
            Stage::Ended {
                files: 3,
                arrived: 0
            },
            "a file refused by one device was counted as arrived"
        );
        let devices = batch_by_device(&mixed);
        assert_eq!(devices[0].state(), BatchState::Sent { files: 3 });
        assert_eq!(
            devices[1].state(),
            BatchState::Stopped { files: 3, done: 0 }
        );
    }
}

#[cfg(test)]
mod uneven_devices_tests {
    use super::*;

    fn transfer(device: &str, name: &str) -> Progress {
        Progress {
            id: format!("{device}-{name}"),
            name: name.into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: device.into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase: Phase::Moving,
        }
    }

    #[test]
    fn a_device_that_was_given_fewer_files_does_not_make_the_batch_smaller() {
        // Three files; the laptop got all three, the phone only one (the others were refused before a transfer was made for them).
        let batch = vec![
            transfer("laptop", "a"),
            transfer("laptop", "b"),
            transfer("laptop", "c"),
            transfer("phone", "a"),
        ];
        assert_eq!(
            batch_files(&batch),
            3,
            "the count followed the device that had the fewest transfers or the most"
        );
        // And it does not follow the device with the MOST either: the phone alone had three different files.
        let other = vec![
            transfer("phone", "x"),
            transfer("phone", "y"),
            transfer("laptop", "x"),
        ];
        assert_eq!(batch_files(&other), 2);
    }

    #[test]
    fn two_different_files_with_one_name_and_different_sizes_are_two_files() {
        let mut small = transfer("laptop", "same.bin");
        small.bytes = 10;
        let mut large = transfer("laptop", "same.bin");
        large.id = "other".into();
        large.bytes = 20;
        assert_eq!(batch_files(&[small, large]), 2);
    }
}

/// One row of the list under the circle, as the model orders it: a device's own heading (who, and how that device stands), or a transfer of the batch.
/// The drawing only walks these and draws each; it decides nothing about grouping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchRow {
    /// The heading of one device's part. Only when the batch went to more than one device: with one, the device is not news. `name` is what the device
    /// can be shown by (the name its own transfers carry, or its id when none does).
    Device {
        peer: String,
        name: String,
        state: BatchState,
    },
    /// A transfer of the batch, for the device named just above it when there is one.
    Transfer(Progress),
}

impl DeviceBatch {
    /// What the device is shown by: the name its transfers carry (the device's own word for itself, kept on each), else the id the owner gave it.
    pub fn shown_name(&self) -> String {
        self.transfers
            .iter()
            .find_map(|item| item.from.clone().filter(|name| !name.is_empty()))
            .unwrap_or_else(|| self.peer.clone())
    }
}

/// The rows of the batch in the order the list shows them: with one device, its transfers as [`transfer_list`] has them and no heading; with more than
/// one, a heading for each device (in the order they first appear) followed by that device's own transfers, going first.
pub fn batch_rows(batch: &[Progress]) -> Vec<BatchRow> {
    let devices = batch_by_device(batch);
    if devices.len() < 2 {
        return transfer_list(batch)
            .cloned()
            .map(BatchRow::Transfer)
            .collect();
    }
    let mut rows = Vec::new();
    for device in &devices {
        rows.push(BatchRow::Device {
            peer: device.peer.clone(),
            name: device.shown_name(),
            state: device.state(),
        });
        rows.extend(
            transfer_list(&device.transfers)
                .cloned()
                .map(BatchRow::Transfer),
        );
    }
    rows
}

#[cfg(test)]
mod batch_rows_tests {
    use super::*;

    fn transfer(device: &str, name: &str, phase: Phase) -> Progress {
        Progress {
            id: format!("{device}-{name}"),
            name: name.into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: device.into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn shape(rows: &[BatchRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                BatchRow::Device { name, .. } => format!("[{name}]"),
                BatchRow::Transfer(item) => item.name.clone(),
            })
            .collect()
    }

    #[test]
    fn with_two_devices_each_gets_a_heading_and_its_own_files_together() {
        let batch = vec![
            transfer("laptop", "a", Phase::Done),
            transfer("phone", "a", Phase::Moving),
            transfer("laptop", "b", Phase::Done),
            transfer("phone", "b", Phase::Moving),
        ];
        // Inside each device the order is the list's own everywhere (going first, then what is over, newest first).
        assert_eq!(
            shape(&batch_rows(&batch)),
            ["[laptop]", "b", "a", "[phone]", "a", "b"]
        );
    }

    #[test]
    fn a_heading_says_how_that_device_stands_by_itself() {
        let batch = vec![
            transfer("laptop", "a", Phase::Done),
            transfer("phone", "a", Phase::Cancelled),
        ];
        let rows = batch_rows(&batch);
        assert_eq!(
            rows[0],
            BatchRow::Device {
                peer: "laptop".into(),
                name: "laptop".into(),
                state: BatchState::Sent { files: 1 }
            }
        );
        assert!(
            matches!(&rows[2], BatchRow::Device { peer, state: BatchState::Stopped { files: 1, done: 0 }, .. } if peer == "phone"),
            "{:?}",
            rows[2]
        );
    }

    #[test]
    fn with_one_device_the_rows_are_as_they_always_were_with_no_heading() {
        let batch = vec![
            transfer("laptop", "a", Phase::Done),
            transfer("laptop", "b", Phase::Moving),
        ];
        assert_eq!(shape(&batch_rows(&batch)), ["b", "a"]);
    }

    #[test]
    fn a_heading_shows_the_name_the_device_gave_itself_and_falls_back_to_its_id() {
        let mut named = transfer("id-1", "a", Phase::Done);
        named.from = Some("Alex's laptop".into());
        let batch = vec![named, transfer("id-2", "a", Phase::Done)];
        let rows = batch_rows(&batch);
        assert!(
            matches!(&rows[0], BatchRow::Device { name, .. } if name == "Alex's laptop"),
            "{:?}",
            rows[0]
        );
        assert!(
            matches!(&rows[2], BatchRow::Device { name, .. } if name == "id-2"),
            "no name known, so the id: {:?}",
            rows[2]
        );
    }

    #[test]
    fn every_transfer_is_in_the_rows_exactly_once_and_no_batch_has_no_rows() {
        let batch = vec![
            transfer("laptop", "a", Phase::Done),
            transfer("phone", "a", Phase::Cancelled),
            transfer("tablet", "a", Phase::Moving),
        ];
        let transfers = batch_rows(&batch)
            .into_iter()
            .filter(|row| matches!(row, BatchRow::Transfer(_)))
            .count();
        assert_eq!(transfers, 3);
        assert!(batch_rows(&[]).is_empty());
    }
}

/// The number of a touch on the circle: every finger that goes down is given the next one, and everything that belongs to that finger (its clock,
/// what it sends) carries it and acts only while it is still the number of the touch that is current. A clock of an earlier touch that is still
/// running when the next one begins is therefore of no consequence: it finds that it is not the current touch, and does nothing and ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TouchNumber(u64);

impl TouchNumber {
    /// The touch that comes after this one.
    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }

    /// Whether `claimed` is still the touch that is current: the one a clock was started for.
    pub fn is_current(self, claimed: Self) -> bool {
        self == claimed
    }
}

#[cfg(test)]
mod touch_number_tests {
    use super::*;

    /// What the circle does with a clock: it ticks the touch it finds, but only while the number it started with is still the current one.
    fn tick_as_the_page_does(
        current: TouchNumber,
        touch: &mut CircleTouch,
        claimed: TouchNumber,
        now: u64,
    ) -> Option<CircleAct> {
        if !current.is_current(claimed) {
            return None;
        }
        Some(touch.tick(now, CircleDoes::HoldToSend))
    }

    #[test]
    fn a_clock_of_an_earlier_touch_does_nothing_to_the_touch_that_came_after_it() {
        let mut current = TouchNumber::default();
        let mut touch = CircleTouch::new(100.);
        // First touch, held a while; its clock is still running when the person lifts and touches again at once.
        current = current.next();
        let first = current;
        touch.down((50., 50.), (200., 300.), 0);
        current = current.next();
        touch.cancel();
        touch.down((50., 50.), (200., 300.), 600);
        // The old clock beats at 900 ms and later: it must not fill, send or empty the new touch.
        assert_eq!(
            tick_as_the_page_does(current, &mut touch, first, 900),
            None,
            "a clock of the earlier touch acted on the new one"
        );
        assert_eq!(
            tick_as_the_page_does(current, &mut touch, first, 1_500),
            None
        );
        // The new touch's own clock does act, from its own start (held 400 ms at 1,000 ms: not full yet).
        touch.seen(1_000);
        assert_eq!(
            tick_as_the_page_does(current, &mut touch, current, 1_000),
            Some(CircleAct::Nothing)
        );
    }

    #[test]
    fn each_touch_has_its_own_number_and_only_the_latest_is_current() {
        let one = TouchNumber::default().next();
        let two = one.next();
        assert!(two.is_current(two));
        assert!(!two.is_current(one));
        assert_ne!(one, two);
    }

    #[test]
    fn the_number_wraps_rather_than_failing_after_a_very_long_run() {
        let last = TouchNumber(u64::MAX);
        assert_ne!(
            last,
            last.next(),
            "the number did not move on at the end of its range"
        );
    }
}

/// The files that wait for the circle, as one list the person can change: more can be added, one can be taken away, all can be cleared. Held by
/// path (a desktop selection) and in the same order as shown; the page keeps no rule about it of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Waiting {
    files: Vec<std::path::PathBuf>,
}

impl Waiting {
    pub fn new(files: Vec<std::path::PathBuf>) -> Self {
        Self { files }
    }

    pub fn files(&self) -> &[std::path::PathBuf] {
        &self.files
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// More files were chosen: they join the list, after what is there, and a file that is already in it is not listed twice. Returns how many
    /// were really new, for the person to be told how many were added.
    pub fn add(&mut self, more: Vec<std::path::PathBuf>) -> usize {
        let before = self.files.len();
        for path in more {
            if !self.files.contains(&path) {
                self.files.push(path);
            }
        }
        self.files.len() - before
    }

    /// The file at `index` is taken away. An index that is not there changes nothing (the list may have changed between the touch and the press).
    pub fn remove(&mut self, index: usize) -> bool {
        if index < self.files.len() {
            self.files.remove(index);
            true
        } else {
            false
        }
    }

    /// Everything is taken away.
    pub fn clear(&mut self) {
        self.files.clear();
    }

    /// Whether the circle may open the picker for a first choice: only when nothing waits. With something waiting, more files are added through the
    /// list's own control and the circle is for sending.
    pub fn lets_the_circle_choose(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether the list offers to add more files, and to clear: only when there is at least one file to add to or clear.
    pub fn offers_more_and_clear(&self) -> bool {
        !self.files.is_empty()
    }
}

#[cfg(test)]
mod waiting_tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<std::path::PathBuf> {
        names
            .iter()
            .map(|name| std::path::PathBuf::from(format!("/files/{name}")))
            .collect()
    }

    #[test]
    fn more_files_join_the_list_after_what_is_there_and_do_not_replace_it() {
        let mut waiting = Waiting::new(paths(&["a", "b"]));
        assert_eq!(waiting.add(paths(&["c", "d"])), 2);
        assert_eq!(waiting.files(), paths(&["a", "b", "c", "d"]).as_slice());
    }

    #[test]
    fn a_file_chosen_again_is_not_listed_twice_and_is_not_counted_as_added() {
        let mut waiting = Waiting::new(paths(&["a", "b"]));
        assert_eq!(
            waiting.add(paths(&["b", "c", "a"])),
            1,
            "a file already in the list was counted as new"
        );
        assert_eq!(waiting.files(), paths(&["a", "b", "c"]).as_slice());
    }

    #[test]
    fn taking_a_file_away_keeps_the_order_of_the_rest_and_a_missing_index_changes_nothing() {
        let mut waiting = Waiting::new(paths(&["a", "b", "c"]));
        assert!(waiting.remove(1));
        assert_eq!(waiting.files(), paths(&["a", "c"]).as_slice());
        assert!(
            !waiting.remove(9),
            "an index that is not there removed something"
        );
        assert_eq!(waiting.files().len(), 2);
    }

    #[test]
    fn when_the_last_file_is_taken_away_the_circle_may_choose_again_and_the_list_offers_nothing() {
        let mut waiting = Waiting::new(paths(&["only"]));
        assert!(!waiting.lets_the_circle_choose());
        assert!(waiting.offers_more_and_clear());
        assert!(waiting.remove(0));
        assert!(waiting.is_empty());
        assert!(
            waiting.lets_the_circle_choose(),
            "an empty list did not let the circle choose files"
        );
        assert!(!waiting.offers_more_and_clear());
    }

    #[test]
    fn clearing_empties_everything_at_once() {
        let mut waiting = Waiting::new(paths(&["a", "b", "c"]));
        waiting.clear();
        assert!(waiting.is_empty());
        assert!(waiting.lets_the_circle_choose());
    }

    #[test]
    fn adding_to_an_empty_list_is_the_first_choice_and_adding_nothing_changes_nothing() {
        let mut waiting = Waiting::default();
        assert_eq!(waiting.add(Vec::new()), 0);
        assert!(waiting.is_empty());
        assert_eq!(waiting.add(paths(&["a"])), 1);
    }
}

#[cfg(test)]
mod every_circle_answers_alike_tests {
    use super::*;
    const MID: (f32, f32) = (50., 50.);

    /// What a lift does on the circle after a touch of `held_ms`, for a circle whose act is `does`.
    fn lift_after(held_ms: u64, does: CircleDoes) -> CircleAct {
        let mut touch = CircleTouch::new(100.);
        touch.down(MID, (200., 300.), 0);
        touch.up(MID, (200., 300.), held_ms, does)
    }

    #[test]
    fn a_short_tap_on_a_press_only_circle_answers_at_the_actuation_point_and_not_later() {
        // The circle of Receive: its one act is its press. It answers a tap as soon as it is a meant touch, with no waiting for anything else.
        assert_eq!(
            lift_after(MIN_TOUCH_MS - 1, CircleDoes::ChooseFiles),
            CircleAct::Nothing,
            "a brush pressed it"
        );
        assert_eq!(
            lift_after(MIN_TOUCH_MS, CircleDoes::ChooseFiles),
            CircleAct::Choose,
            "a meant tap did not press it"
        );
        assert_eq!(
            lift_after(HOLD_MS - 1, CircleDoes::ChooseFiles),
            CircleAct::Choose,
            "a tap just short of a hold did not press it"
        );
    }

    #[test]
    fn a_press_that_lasts_as_long_as_a_hold_is_not_a_tap_for_any_circle_that_only_chooses() {
        assert_eq!(
            lift_after(HOLD_MS, CircleDoes::ChooseFiles),
            CircleAct::Nothing
        );
        assert_eq!(
            lift_after(HOLD_MS + 200, CircleDoes::ChooseFiles),
            CircleAct::Nothing
        );
    }
}

/// The height of one row of the history, in pixels: the same for every row. A row is one line of text and its state, never a card with buttons.
pub const HISTORY_ROW_HEIGHT: f32 = 56.;

/// The space between two rows of the history, in pixels.
pub const HISTORY_ROW_GAP: f32 = 6.;

/// The most rows of the history that are made at once, newest first, however long the history is: the cost of the page does not grow with it.
pub const HISTORY_ROWS_MADE: usize = 100;

/// The transfers the Transfers page shows, split into the two things it is: what is still going (offered, or moving), kept as full cards because
/// each has buttons (cancel, accept) and there are few; and the history, what is over, newest first, in rows of one fixed height. Both come from
/// the ONE list of transfers, after the search and the filter, and nothing is copied: a transfer is in exactly one of the two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sections<'a> {
    pub going: Vec<&'a Progress>,
    pub history: Vec<&'a Progress>,
}

pub fn sections<'a>(found: &'a [Progress]) -> Sections<'a> {
    let going: Vec<&Progress> = transfer_list(found)
        .filter(|item| !item.phase.is_over())
        .collect();
    let history: Vec<&Progress> = transfer_list(found)
        .filter(|item| item.phase.is_over())
        .collect();
    Sections { going, history }
}

#[cfg(test)]
mod history_sections_tests {
    use super::*;

    fn one(id: &str, phase: Phase) -> Progress {
        Progress {
            id: id.into(),
            name: format!("{id}.bin"),
            folders: Vec::new(),
            bytes: 10,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn a_transfer_is_in_exactly_one_of_going_and_history_and_none_is_lost() {
        let all = vec![
            one("a", Phase::Done),
            one("b", Phase::Moving),
            one("c", Phase::Cancelled),
            one("d", Phase::Waiting),
            one("e", Phase::Failed("x".into())),
        ];
        let parts = sections(&all);
        assert_eq!(
            parts.going.len() + parts.history.len(),
            all.len(),
            "a transfer was lost or counted twice"
        );
        let mut going: Vec<&str> = parts.going.iter().map(|item| item.id.as_str()).collect();
        going.sort_unstable();
        assert_eq!(going, ["b", "d"]);
        assert!(parts.history.iter().all(|item| item.phase.is_over()));
    }

    #[test]
    fn the_history_is_newest_first_and_the_going_ones_never_sit_in_it() {
        let all = vec![
            one("old", Phase::Done),
            one("mid", Phase::Done),
            one("new", Phase::Done),
            one("now", Phase::Moving),
        ];
        let ids: Vec<&str> = sections(&all)
            .history
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, ["new", "mid", "old"]);
    }

    #[test]
    fn however_long_the_history_is_no_more_than_the_bound_of_rows_are_made() {
        let all: Vec<Progress> = (0..5_000)
            .map(|n| one(&format!("t{n}"), Phase::Done))
            .collect();
        let made = sections(&all)
            .history
            .into_iter()
            .take(HISTORY_ROWS_MADE)
            .count();
        assert_eq!(made, HISTORY_ROWS_MADE, "the cost grew with the history");
        let few: Vec<Progress> = (0..3).map(|n| one(&format!("t{n}"), Phase::Done)).collect();
        assert_eq!(
            sections(&few)
                .history
                .into_iter()
                .take(HISTORY_ROWS_MADE)
                .count(),
            3
        );
    }

    #[test]
    fn nothing_going_and_nothing_over_is_two_empty_sections() {
        let parts = sections(&[]);
        assert!(parts.going.is_empty() && parts.history.is_empty());
    }
}

/// How long a result (SENT, STOPPED 0/1) stays on the circle once everything of the batch is over, in milliseconds, before it begins to fade.
pub const RESULT_SHOWN_MS: u64 = 4_000;
/// How long the fade takes, in milliseconds: the words go from clear to gone, and then the circle asks for files again.
pub const FADE_MS: u64 = 600;

/// The number of a lap of the circle: files chosen, sent, ended. A fade belongs to the lap it was started for, and does nothing once a newer lap
/// has begun (new files chosen, or a new send): the same rule as a touch's number, for the same reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lap(u64);

impl Lap {
    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// How clear the result on the circle is at `now_ms`, from 1 (clear) to 0 (gone), for a lap that ended at `ended_ms`. Clear while it is shown, then a
/// straight fade, then gone. A clock that ran backwards leaves it clear: nothing disappears by a wrong time.
pub fn result_opacity(ended_ms: u64, now_ms: u64) -> f32 {
    let Some(since) = now_ms.checked_sub(ended_ms) else {
        return 1.;
    };
    if since <= RESULT_SHOWN_MS {
        return 1.;
    }
    let faded = since - RESULT_SHOWN_MS;
    if faded >= FADE_MS {
        return 0.;
    }
    1. - faded as f32 / FADE_MS as f32
}

/// Whether the circle shows the result of the lap that ended, or has given way to asking for files: only while the lap is still the current one and
/// the result has not faded out. A newer lap always wins.
pub fn shows_result(lap_of_result: Lap, current: Lap, ended_ms: u64, now_ms: u64) -> bool {
    lap_of_result == current && result_opacity(ended_ms, now_ms) > 0.
}

#[cfg(test)]
mod fade_tests {
    use super::*;

    #[test]
    fn the_result_is_clear_for_the_time_it_is_shown_then_fades_in_a_straight_line_and_is_gone() {
        assert_eq!(result_opacity(1_000, 1_000), 1.);
        assert_eq!(
            result_opacity(1_000, 1_000 + RESULT_SHOWN_MS),
            1.,
            "it began to fade before its time was up"
        );
        let half = result_opacity(1_000, 1_000 + RESULT_SHOWN_MS + FADE_MS / 2);
        assert!(
            (half - 0.5).abs() < 0.01,
            "half way through the fade it was {half}"
        );
        assert_eq!(result_opacity(1_000, 1_000 + RESULT_SHOWN_MS + FADE_MS), 0.);
        assert_eq!(
            result_opacity(1_000, 1_000 + RESULT_SHOWN_MS * 10),
            0.,
            "a long gone result came back"
        );
    }

    #[test]
    fn a_clock_that_runs_backwards_never_makes_a_result_disappear() {
        assert_eq!(result_opacity(10_000, 5), 1.);
    }

    #[test]
    fn the_opacity_only_ever_goes_down_as_time_goes_on() {
        let mut last = 1.;
        for step in (0..=(RESULT_SHOWN_MS + FADE_MS + 500)).step_by(50) {
            let now = result_opacity(0, step);
            assert!(
                now <= last + f32::EPSILON,
                "it grew again at {step} ms: {last} then {now}"
            );
            last = now;
        }
    }

    #[test]
    fn a_fade_of_an_earlier_lap_never_touches_a_newer_one() {
        // The first lap ended long ago and has faded; the person chose new files (a newer lap): the old result is not shown.
        let first = Lap::default().next();
        let second = first.next();
        assert!(
            shows_result(first, first, 0, 100),
            "the result of the current lap was not shown while it is clear"
        );
        assert!(
            !shows_result(first, second, 0, 100),
            "an earlier lap's result was shown over a newer lap"
        );
        // And a newer lap's own result is shown from its own start, whatever the old one did.
        assert!(shows_result(second, second, 50_000, 50_100));
    }

    #[test]
    fn once_faded_the_result_is_not_shown_even_for_the_current_lap() {
        let lap = Lap::default().next();
        assert!(!shows_result(lap, lap, 0, RESULT_SHOWN_MS + FADE_MS));
    }
}
