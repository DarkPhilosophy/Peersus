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
            ActionFailure::Stopped => {
                "The plugin stopped before it finished. Retry when it runs again."
            }
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

/// The most files that are offered to a device nearby that is not paired: what one such connection carries, and one command.
pub const NEARBY_FILES_MOST: usize = 16;

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
    /// Live, plugin-owned words this peer says about itself. No stale words for an offline peer.
    pub statuses: Vec<peersus_core::plugins::StatusInfo>,
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

impl Device {
    /// What the device's extensions say of it, for a place with room for one line under how it is reachable: the first
    /// status in its author's words, and how many more there are. `None` when it says nothing. Never instead of the
    /// presence: that is the application's own word and decides what can be sent.
    pub fn status_line(&self) -> Option<String> {
        let first = self.statuses.first()?.label.as_str();
        Some(match self.statuses.len() - 1 {
            0 => first.to_owned(),
            more => format!("{first} +{more}"),
        })
    }
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
    /// Who the offer is from, as a label: a paired device by the name it is kept under here (it is the device the
    /// connection authenticated, whatever it calls itself now); any other by the name it gave itself while the offer came
    /// in, which nobody checked; `None` when it gave none.
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

/// Whether the person accepted an incoming file between two views of the owner: one that waited for an answer in `before` is on its way or
/// has arrived in `after`. Read from what the owner reports and not from a button, so that it is the same whichever way the file was accepted
/// (the offer over the window, a card of a list, a notification or a panel outside the window). It is what takes the window to Receive, to the
/// list of the files that are arriving ([`ReceiveJump`]). A file that was refused, cancelled or failed is not one, and neither is one going out.
pub fn accepted_between(before: &[Progress], after: &[Progress]) -> bool {
    after.iter().any(|now| {
        now.incoming
            && matches!(now.phase, Phase::Moving | Phase::Done)
            && before
                .iter()
                .any(|was| was.id == now.id && was.acceptable())
    })
}

/// Where the window is on its way to after a file was accepted (see [`accepted_between`]): first to the page of Receive, and on it to the list of
/// the files that are arriving. Each step is carried out once by what owns it (the root has the page, Receive has its scroll) and passes on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReceiveJump {
    #[default]
    Nowhere,
    /// A file was accepted: the root shows Receive.
    ToPage,
    /// Receive is shown: it scrolls to its list once it knows where the list is.
    ToList,
}

/// Everything the screens need.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct View {
    pub devices: Vec<Device>,
    pub approvals: Vec<Approval>,
    pub extension_prompts: Vec<peersus_core::model::ExtensionPrompt>,
    /// The notifications to show now, as cards (see [`notification_cards`]).
    pub notifications: Vec<NotificationCard>,
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
    /// Updates of the application itself as the owner reports them.
    pub update: peersus_core::model::UpdateState,
    /// Languages as the owner reports them: the device's, the one shown, the files held and what is offered.
    pub languages: peersus_core::model::LanguageState,
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
    /// The paired devices that were brought near, by id, and why none can be found, if the platform said (see `peersus_core::nearby`).
    pub near: Vec<String>,
    /// Those of them whose person has Receive open now, by id.
    pub receiving: Vec<String>,
    pub nearby_trouble: Option<peersus_core::nearby::Trouble>,
    /// The devices nearby that offer to pair and are not paired, as the owner tells them: named by themselves, checked by nobody.
    pub strangers: Vec<peersus_core::nearby::Stranger>,
    /// Whether a person is setting how near "near" is, and the strength in dBm of the device heard strongest while they do.
    pub nearby_measuring: bool,
    pub nearby_strongest: Option<i16>,
}

impl View {
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
            notifications: notification_cards(&snapshot.notifications, &snapshot.plugins),
            transfers: snapshot
                .transfers
                .iter()
                .map(|transfer| {
                    let mut row = progress(transfer);
                    // A paired device is named as it is known here; `progress` alone knows only what the sender said of itself.
                    if let Some(paired) =
                        snapshot.peers.iter().find(|peer| peer.id == transfer.peer)
                    {
                        row.from = Some(paired.name.clone());
                    }
                    row
                })
                .collect(),
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
            languages: snapshot.languages.clone(),
            update: snapshot.update.clone(),
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
            near: snapshot
                .nearby
                .near
                .iter()
                .map(|peer| peer.0.clone())
                .collect(),
            receiving: snapshot
                .nearby
                .receiving
                .iter()
                .map(|peer| peer.0.clone())
                .collect(),
            nearby_trouble: snapshot.nearby.trouble,
            strangers: snapshot.nearby.strangers.clone(),
            nearby_measuring: snapshot.nearby.measuring,
            nearby_strongest: snapshot.nearby.strongest,
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

    /// The paired device the listed files go to at once because it is held near AND its person has Receive open (experimental), or none.
    ///
    /// Opening Receive on the other device is what asks for the files, so nothing more is pressed here. Only while the person's switch is
    /// on, files are listed and they picked no device themselves (what they picked is never replaced); only a device a send can start to
    /// now; and only when exactly ONE such device is near with Receive open, since two say nothing of which was meant. Each file is still
    /// that device's person's to accept. A device that is near with any other page open is given nothing (see [`Self::near_waiting`]).
    pub fn receiving_near(&self, chosen: &[String], files_listed: bool) -> Option<&Device> {
        if !self.settings.nearby_send || !files_listed || !chosen.is_empty() {
            return None;
        }
        let mut receiving = self
            .reachable()
            .filter(|device| self.receiving.contains(&device.id));
        let only = receiving.next()?;
        receiving.next().is_none().then_some(only)
    }

    /// The paired device that is held near but does not have Receive open, when it is the only one near and the listed files go nowhere
    /// by nearness: named on the page, so that the person knows what is missing. Nothing is sent to it and nothing is chosen by it.
    pub fn near_waiting(&self, chosen: &[String], files_listed: bool) -> Option<&Device> {
        if !self.settings.nearby_send
            || !files_listed
            || !chosen.is_empty()
            || !self.receiving.is_empty()
        {
            return None;
        }
        let mut near = self
            .reachable()
            .filter(|device| self.near.contains(&device.id));
        let only = near.next()?;
        near.next().is_none().then_some(only)
    }

    /// The devices nearby that are not paired and said they are there to be paired with: what Peers lists. One that said it takes
    /// files said another thing, and cannot be paired with by what it announced.
    pub fn pairable(&self) -> Vec<peersus_core::nearby::Stranger> {
        self.strangers
            .iter()
            .filter(|stranger| !stranger.receives)
            .cloned()
            .collect()
    }

    /// The devices nearby that are not paired, take files and are near now, by id: what the page remembers of the moment files were listed
    /// (see [`Self::brought_near`]).
    pub fn strangers_near(&self) -> Vec<String> {
        self.strangers
            .iter()
            .filter(|stranger| stranger.receives && stranger.near)
            .map(|stranger| stranger.id.0.clone())
            .collect()
    }

    /// The device that is not paired and that the listed files are offered to because it was BROUGHT near with Receive open
    /// (experimental), or none.
    ///
    /// Bringing it near is the gesture, so only a device that was not near and taking files already when the files were listed
    /// (`near_before`): one that lay there all along is given nothing by a choice of files, and has to be taken away and brought again, or
    /// have its Receive closed and opened. Only while the person's switch is on, they picked no device, no paired device is near (that one
    /// is sent to or waited for, see [`Self::receiving_near`]), and the files are no more than one such offer carries; only a device that
    /// said it takes files, which it says only while its person has Receive open; and only when exactly ONE was brought, since two say
    /// nothing of which was meant. Its person is still asked about every file.
    pub fn brought_near(
        &self,
        chosen: &[String],
        near_before: &[String],
        files: usize,
    ) -> Option<&peersus_core::nearby::Stranger> {
        if !self.settings.nearby_send
            || !chosen.is_empty()
            || !(1..=NEARBY_FILES_MOST).contains(&files)
            || !self.near.is_empty()
        {
            return None;
        }
        let mut brought = self.strangers.iter().filter(|stranger| {
            stranger.receives && stranger.near && !near_before.contains(&stranger.id.0)
        });
        let only = brought.next()?;
        brought.next().is_none().then_some(only)
    }

    /// What the page keeps of `near_before` from one draw to the next: while no file is listed, every device that is near now; once files
    /// are listed, only those of them that are still near, so that one taken away and brought again counts as brought.
    pub fn near_before_kept(&self, near_before: &[String], files_listed: bool) -> Vec<String> {
        let near = self.strangers_near();
        if files_listed {
            near_before
                .iter()
                .filter(|id| near.contains(id))
                .cloned()
                .collect()
        } else {
            near
        }
    }

    /// Whether this device still carries the name it was installed with. Two devices that both
    /// say "My device" cannot be told apart by the person choosing where to send, so the first
    /// thing they are asked is a name. Taken from the owner's own default, so it cannot drift.
    pub fn needs_a_name(&self) -> bool {
        self.settings.device_name.trim() == Settings::default().device_name
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
    queue_words_in(Language::ENGLISH, waiting)
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

/// The names the listed files go by, for the list under the circle: the names of an Android selection, or the last part of each path of a
/// desktop one (they are one list seen in two places, and only one of the two is ever filled).
pub fn names_listed(paths: &[std::path::PathBuf], names: &[String]) -> Vec<String> {
    if !names.is_empty() {
        return names.to_vec();
    }
    paths
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect()
}

/// The names the list under the circle shows as prepared. The files that are listed, when any are. With none listed, the files a code was
/// made of: they went behind the code when the circle was touched and are no longer the page's to send, but they have not begun to go
/// either, so the list still says which they are, from that touch (`making`, while the code is being made) for as long as the code is what
/// the circle draws (`drawn`). A code that was scanned, ran out or was taken back names nothing: the transfers made of it are the rows then.
pub fn names_under_circle(
    listed: Vec<String>,
    making: Option<&[String]>,
    drawn: Option<&[String]>,
) -> Vec<String> {
    if !listed.is_empty() {
        return listed;
    }
    making.or(drawn).map(<[String]>::to_vec).unwrap_or_default()
}

/// The lines that say WHICH files the code under the circle hands over: each name, up to [`NAMED_UNDER_CIRCLE`], then how many more
/// there are. Only the names the files go by, never where they were; empty when nothing was chosen. Names may arrive after the
/// count, without holding every descriptor open.
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
    /// control that judges the duration itself (the circle of the Send page, by its own clock), so that the minimum is never decided twice, and
    /// for one that asks for no duration at all (the header's Settings button, which only opens a place, as the tabs do).
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
        statuses: peer.custom_statuses().to_vec(),
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

/// What a window that is only now drawn takes as already said: the owner's own messages, the first `owned` of `said`. Each of
/// them is something that happened before this window looked, and the owner keeps its last few for as long as it runs, so a
/// window that opens later, or that is drawn again after it was away (the connection dropped, the app was left and entered
/// again), must not say them all over again. What follows them in `said` is how things ARE (an extension that is stopped, a
/// file kept elsewhere), which a new window does say.
pub fn notices_already_said(said: &[String], owned: usize) -> Vec<String> {
    said[..owned.min(said.len())].to_vec()
}

/// The messages to show now, given what the window had seen (`before`, and the owner's count of everything it said then).
/// New texts are those not in the list as it was last seen; a text that left the list and comes back is new again. The owner
/// also counts every time it says something, even a sentence its list already ends with, so the newest of its own messages, as
/// many as were said since, are shown again though the list did not change.
pub fn fresh_notices(
    before: &[String],
    before_count: u64,
    said: &[String],
    owned: usize,
    count: u64,
) -> Vec<String> {
    let owned = owned.min(said.len());
    let mut fresh: Vec<String> = said
        .iter()
        .filter(|text| !before.contains(text))
        .cloned()
        .collect();
    let again = usize::try_from(count.saturating_sub(before_count))
        .unwrap_or(usize::MAX)
        .min(owned);
    for text in &said[owned - again..owned] {
        if !fresh.contains(text) {
            fresh.push(text.clone());
        }
    }
    fresh
}

/// Most posted notifications drawn at once.
pub const MOST_NOTIFICATION_CARDS: usize = 3;
/// The most characters all the words of a card's buttons may have together and still stand side by side; with more, each button
/// gets a line of its own, so that a long answer is never cut and never leaves the card.
pub const BUTTONS_IN_A_ROW_CHARS: usize = 28;

/// A posted notification as the window draws it: one card in the stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationCard {
    /// What an answer names.
    pub id: u64,
    /// The extension it is from, by the name it is installed under; none for the application's own. Never a name the sender wrote.
    pub from: Option<String>,
    /// The sender's words. The application's own are in its language's table; an extension's are shown as written.
    pub text: String,
    /// The answers offered, in the sender's order and under the sender's words.
    pub buttons: Vec<peersus_api::notification::Button>,
}

impl NotificationCard {
    /// Whether the buttons stand side by side (see [`BUTTONS_IN_A_ROW_CHARS`]).
    pub fn buttons_in_a_row(&self) -> bool {
        self.buttons
            .iter()
            .map(|button| button.label.chars().count())
            .sum::<usize>()
            <= BUTTONS_IN_A_ROW_CHARS
    }
}

/// The cards to draw for what is on show, oldest first, at most [`MOST_NOTIFICATION_CARDS`]. One that waits for the person keeps
/// its place before any that only passes: a question is never pushed off the screen by sentences that came after it. Of those
/// that pass, the newest are kept. How far a notification reaches does not matter here: whatever is on show is drawn in the
/// window, and what else is done with one that reaches outside is not the window's.
pub fn notification_cards(
    posted: &[peersus_core::model::PostedNotification],
    plugins: &[PluginInfo],
) -> Vec<NotificationCard> {
    use peersus_core::model::NotificationSource;
    let waiting: Vec<_> = posted
        .iter()
        .filter(|posted| posted.notification.waits())
        .take(MOST_NOTIFICATION_CARDS)
        .collect();
    let passing = posted
        .iter()
        .rev()
        .filter(|posted| !posted.notification.waits())
        .take(MOST_NOTIFICATION_CARDS - waiting.len());
    let mut shown: Vec<_> = waiting.iter().copied().chain(passing).collect();
    shown.sort_by_key(|posted| posted.id);
    shown
        .into_iter()
        .map(|posted| NotificationCard {
            id: posted.id,
            from: match &posted.source {
                NotificationSource::App => None,
                NotificationSource::Plugin(id) => Some(
                    plugins
                        .iter()
                        .find(|plugin| &plugin.id == id)
                        .and_then(|plugin| plugin.package.as_ref())
                        .map_or_else(|| id.as_str().to_owned(), |package| package.name.clone()),
                ),
            },
            text: posted.notification.text.clone(),
            buttons: posted.notification.buttons.clone(),
        })
        .collect()
}

/// The cards the panel outside the window draws: only what its sender asked to reach outside, chosen and named as in the window.
pub fn outside_cards(snapshot: &peersus_core::model::Snapshot) -> Vec<NotificationCard> {
    let outside: Vec<_> = snapshot
        .notifications
        .iter()
        .filter(|posted| posted.notification.reach == peersus_api::notification::Reach::Outside)
        .cloned()
        .collect();
    notification_cards(&outside, &snapshot.plugins)
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

/// Whether an installed extension asks the application for `service`: its manifest imports it from the application itself.
///
/// A platform setting that only serves that service to extensions (see `link::ServiceSetting`) is shown, and found by search, only
/// then. The rule knows no extension and no service by name: what is installed and what it imports is all it reads, so a row
/// appears with the first extension that needs it, whichever that is, and is gone with the last.
pub fn wants_service(plugins: &[PluginInfo], service: &str) -> bool {
    plugins.iter().any(|plugin| {
        plugin
            .manifest
            .imports
            .iter()
            .any(|import| import.provider == peersus_api::HOST && import.service == service)
    })
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
            device_state_words_in(Language::ENGLISH, &BatchState::None),
            (String::new(), Tone::Quiet)
        );
        assert_eq!(
            device_state_words_in(
                Language::ENGLISH,
                &BatchState::Going {
                    files: 3,
                    percent: 40,
                    current: None
                }
            ),
            ("40%".to_owned(), Tone::Going)
        );
        let (done, tone) = device_state_words_in(Language::ENGLISH, &BatchState::Sent { files: 3 });
        assert!(done.starts_with("DONE") && done.contains('3'), "{done}");
        assert_eq!(tone, Tone::Good);
        assert_eq!(
            device_state_words_in(
                Language::ENGLISH,
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

/// Whether the code on show gives way to the devices the person picked: it does while nobody has scanned it and it could still admit a device
/// ([`CodeCircle::Code`]), and one or more devices are picked. The code is then taken back and the files it was for are listed again, to be sent
/// to those devices by the circle. A code that was scanned is the business of the device that scanned it, and picking a device changes
/// nothing of it; a device that was picked and went away takes nothing back either, since the files would have nowhere to go.
pub fn code_gives_way(code: Option<&CodeCircle>, aim: &Aim) -> bool {
    matches!(code, Some(CodeCircle::Code)) && matches!(aim, Aim::Devices(_))
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

#[cfg(test)]
mod tests;

// The send circle's own rules (its batches, stages, touches and what waits under it), with their tests.
mod circle;
pub use circle::*;

// The Settings screen's rules.
mod settings;
pub use settings::*;

// How things move and appear.
mod motion;
pub use motion::*;

// The Extensions screen's rules.
mod extensions;
pub use extensions::*;

// How the interface is arranged for its window.
mod layout;
pub use layout::*;

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
