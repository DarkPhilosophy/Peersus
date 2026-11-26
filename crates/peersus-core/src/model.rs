//! Commands and immutable snapshots shared by the CLI, every frontend and platform hosts.
//!
//! Nothing here names a concrete plugin. Plugin interaction is expressed with three
//! generic commands (grants, setting values, declared actions) keyed by an opaque
//! [`PluginId`], and plugins appear in the snapshot with the manifest they declared.
use crate::{
    Settings,
    config::{Language, SettingsPatch},
    plugins::{Grants, PluginId, Value},
    profile::{Digest, Picture},
};

pub use crate::{
    peer::PeerId,
    secret::{MAX_SECRET_BYTES, SecretText},
};
use peersus_api::manifest::Manifest;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
use zeroize::Zeroize;

/// What this device knows of another one right now. Never stored: it is read from a live authenticated connection and from
/// what that device last said of itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Presence {
    /// No live connection. Nothing can be sent.
    #[default]
    Offline,
    /// Connected, and the device has said that it sleeps (the screen is off, or it has gone idle): it is there, and it answers
    /// when it wakes, which may take a while.
    Sleeping,
    /// Connected and awake: a send to it starts at once.
    Online,
}

impl Presence {
    /// Whether there is a live connection at all. Both `Online` and `Sleeping` are reachable; this is what a plugin or an
    /// older reader of the saved file knows as `online`.
    pub fn reachable(self) -> bool {
        self != Self::Offline
    }
}

/// Remembered device; presence is derived from live authenticated connections.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Peer {
    /// Stable authenticated identifier.
    pub id: PeerId,
    /// User-facing display name.
    pub name: String,
    /// Time the mutual pairing completed, in Unix seconds.
    pub paired_at: u64,
    /// Last successful authenticated communication.
    pub last_seen: Option<u64>,
    /// Whether a live authenticated connection exists (the device may be asleep: see `sleeping`). Kept as the bool it always
    /// was, because the saved file and the plugins' device lists read it by this name.
    pub online: bool,
    /// The device has said that it sleeps. Only meaningful with `online`; read through [`Peer::presence`], which never lets
    /// the two disagree. Never stored: a saved file does not have it and reads it as awake.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sleeping: bool,
    /// The statuses of plugins that the device says it has now, each with whose it is and the words its author gave it. Open: the
    /// application does not list what a device may say. Never stored, and only what a live connection brought: a device that is not
    /// connected says nothing, and a saved file reads it as none. At most [`crate::plugins::MAX_ACTIVE_STATUSES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<crate::plugins::StatusInfo>,
    /// Capabilities the user granted each plugin for this device. Default-deny: a
    /// plugin without an entry may do nothing for this device.
    #[serde(default)]
    pub grants: BTreeMap<PluginId, Grants>,
    /// Per-device plugin setting values, by plugin id.
    #[serde(default)]
    pub plugin_values: BTreeMap<PluginId, crate::plugins::Values>,
}

impl Peer {
    /// How this device is reachable now, from the two flags. A sleeping device that is not connected is offline: there is
    /// nothing asleep to wake.
    pub fn presence(&self) -> Presence {
        match (self.online, self.sleeping) {
            (false, _) => Presence::Offline,
            (true, true) => Presence::Sleeping,
            (true, false) => Presence::Online,
        }
    }

    /// What the device says of itself besides being awake or asleep: nothing unless there is a live connection to say it on.
    pub fn custom_statuses(&self) -> &[crate::plugins::StatusInfo] {
        if self.online { &self.statuses } else { &[] }
    }
}

/// Transfer lifecycle visible to all clients.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum TransferState {
    /// Awaiting explicit recipient consent.
    Offered,
    /// Actively streaming verified data.
    Running,
    /// Integrity validated and destination atomically published.
    Complete,
    /// Cancelled by either user.
    Cancelled,
    /// Failed with a user-safe reason.
    Failed { reason: String },
}

impl TransferState {
    /// Whether the transfer is still being worked on: waiting for consent or moving. Everything else is over, and a record of it
    /// is only a memory. The one meaning of "still going", so that no rule repeats the list of states.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Offered | Self::Running)
    }
}

/// One transfer's metadata and progress; never file content.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Transfer {
    /// Host-generated transfer identifier.
    pub id: String,
    /// Remote identity.
    pub peer: PeerId,
    /// The name the other device gave itself while this transfer crossed, kept as a label for the history and nothing more:
    /// it is the device's own word (never trusted as who it is, which is `peer`), it is not fetched again after the transfer
    /// even if the device is online, and a new scan or a pairing is what refreshes it. `None` for a record made before.
    #[serde(default)]
    pub peer_name: Option<String>,
    /// User-visible basename, not a remotely controlled destination path.
    pub name: String,
    /// Validated relative directory components of a folder transfer, excluding the file name.
    /// Older history records and individually selected files have no directory components.
    #[serde(default)]
    pub folders: Vec<String>,
    /// Total length advertised and checked by the receiver.
    pub bytes: u64,
    /// Successfully processed bytes.
    pub completed_bytes: u64,
    /// Whether this device receives the transfer.
    pub incoming: bool,
    pub state: TransferState,
    /// Start timestamp for history ordering.
    pub started_at: u64,
    /// The generation of the files code this transfer came of: it was offered on the connection that code admitted. `None` for
    /// a transfer that came of no code (sent to a device that is paired, or received), and for a record made before this existed.
    /// The generation is no secret: it is a count of codes made, and the capability itself never leaves the host.
    #[serde(default)]
    pub code: Option<u64>,
    /// When an offer that came in and waits for an answer ends by itself, in Unix seconds: the moment it was shown plus the wait
    /// the preferences give. A screen draws the time left from it and a plugin reads the same number. `None` once it is answered
    /// or over, for a transfer that went out, and for a record made before this existed. Never a time the screen chose.
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// Where a received file was written on this device, set only once it arrived whole and checked: the path inside the folder the
    /// transfer was accepted into. `None` while it is not done, for a transfer that went out, and for a record made before this existed.
    /// A plugin reads the same place the person's files are, and a platform that must move the file on (Android: from its own
    /// staging into the folder the person chose) knows by it what to move.
    #[serde(default)]
    pub saved_as: Option<std::path::PathBuf>,
}

impl Transfer {
    /// Whether this is a file offered to this device that nobody has answered yet: the one thing a person is asked to decide.
    /// One going out waits too, but for the other device.
    pub fn awaits_answer(&self) -> bool {
        self.incoming && self.state == TransferState::Offered
    }
}

/// Human confirmation request, never the remote user's selection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PairingPrompt {
    /// Ephemeral request identifier.
    pub id: String,
    /// Untrusted friendly name; identity comes from the transport.
    pub name: String,
    /// Locally derived numeric choices.
    pub options: [u8; 3],
    /// Whether this device already submitted a choice.
    pub selected: bool,
    /// Remaining lifetime in seconds.
    pub remaining_seconds: u64,
}

/// An owner-issued, ephemeral request for the key an extension uses at `origin`: answered in the extension's settings, with the
/// key, with its removal, or refused. Never contains credentials. What an extension's work asks the person otherwise (may this go
/// there, here is the result) is a notification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionPrompt {
    pub id: u64,
    pub plugin: PluginId,
    pub origin: String,
}

/// Who asked the application to show a notification. Never a name the sender wrote: an
/// extension is known by the id the application gave it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotificationSource {
    /// The application itself.
    App,
    /// An extension, by its id.
    Plugin(PluginId),
    /// The owner checked a required local grant; an extension cannot post this kind.
    MissingPermission(MissingGrant),
    /// No extension routes a paired device's request here; the request's text is never shown.
    UnavailableRecipient,
}

/// A local permission warning, keyed by exactly what the owner checked.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissingGrant {
    pub plugin: PluginId,
    pub permission: peersus_api::permission::Permission,
    pub peer: Option<PeerId>,
}

/// A notification on show: what its sender asked for, under the number the answer names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostedNotification {
    /// Given by the owner, never used twice while it runs.
    pub id: u64,
    pub source: NotificationSource,
    pub notification: peersus_api::notification::Notification,
}

/// Credentials have the same JSON representation as strings but cannot leak through Debug,
/// failed validation, abandoned IPC commands or a dropped prompt answer.
#[derive(Clone, Serialize, Deserialize, Zeroize)]
#[serde(try_from = "String")]
pub struct ExtensionSecret(String);

impl ExtensionSecret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ExtensionSecret {
    type Error = &'static str;
    fn try_from(mut value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.len() > 8192 || !value.bytes().all(|b| b.is_ascii_graphic()) {
            zeroize::Zeroize::zeroize(&mut value);
            return Err("The API key must contain 1–8192 printable ASCII bytes without spaces");
        }
        Ok(Self(value))
    }
}
impl std::fmt::Debug for ExtensionSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExtensionSecret([redacted])")
    }
}
impl Drop for ExtensionSecret {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

/// What the application knows about a plugin process right now. Fixed vocabulary: nothing a
/// plugin prints ever reaches this, so a frontend can render it without sanitising.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginStatus {
    /// Not started, or stopped by the user. The default, so that a record from a version
    /// that did not report health never claims a plugin is running.
    #[default]
    Stopped,
    /// The process was launched but has not yet connected. A plugin that never connects
    /// stays here, which is how it differs from one the user stopped.
    Starting,
    /// Running and connected to the hub, so it can answer.
    Running,
    /// It ended unexpectedly and the supervisor will start it again shortly.
    Restarting,
    /// It failed too often and was turned off. It stays off until the user restarts it.
    Failed,
}

/// What a plugin's FOLDER allows, worked out from what is on disk and nothing else. A separate thing from
/// [`PluginStatus`], which is about the process: a plugin can be `Ready` and not running (the person stopped it, or it has not
/// started yet), or running status for one that is `Changed` can never happen, because only `Ready` is started. Fixed
/// vocabulary, like the status: nothing a plugin prints reaches this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Standing {
    /// What is on disk is what was approved, and nothing is switched off: it may run.
    Ready,
    /// It was never approved, or its approval was lost, or what it asks for has grown past what was approved. It is not run
    /// until the person looks at what it asks for and approves. Not a fault: a question waiting for an answer.
    NeedsApproval,
    /// The program on disk is not the one that was approved. It is not run, and the person may trust it again or remove it.
    Changed,
    /// The person switched it off. It stays, keeps what it was granted, and is not run.
    Disabled,
    /// It cannot run whatever the person decides: no usable manifest, or no program fit to run. The one way out is to remove
    /// it.
    Broken,
}

/// What a plugin can be granted, as the OWNER works it out from the roles the plugin plays, and the
/// only definition of it: the window shows exactly this and decides nothing of its own. A name a
/// plugin merely listed in its manifest is not here. Split by whether a permission is granted for
/// one paired device at a time or once, for no device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grantable {
    /// Granted for one paired device at a time.
    #[serde(default)]
    pub per_device: Vec<peersus_api::permission::Permission>,
    /// Granted once, for no device in particular.
    #[serde(default)]
    pub outright: Vec<peersus_api::permission::Permission>,
}

/// A plugin as a frontend sees it: its host-assigned id, declared manifest, what its folder allows, and the health of its
/// process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: PluginId,
    pub manifest: Manifest,
    /// What its folder allows. Always said, never guessed: there is no default, so that an owner cannot report a plugin
    /// without saying whether it may run.
    pub standing: Standing,
    #[serde(default)]
    pub status: PluginStatus,
    /// What it can be granted. Missing from an owner that predates it, which reads as nothing.
    #[serde(default)]
    pub grantable: Grantable,
    /// The digest of the program as the owner measured it, for a plugin that waits for approval or has changed, and nothing
    /// otherwise. A frontend gives it back with `ApprovePlugin`, which binds the approval to what the person was shown.
    #[serde(default)]
    pub program: Option<String>,
    /// What the package says about itself: name, version, author and link. Text to SHOW, never a right,
    /// and never what decides who the plugin is (that is `id`). Absent for a plugin without a package and for a
    /// package that carries none; missing from an owner that predates it.
    #[serde(default)]
    pub package: Option<peersus_api::package::Package>,
    /// Whether this person accepted the agreement this package carries now. False for a package that carries none, for
    /// one whose agreement changed since, and from an owner that predates it.
    #[serde(default)]
    pub agreed: bool,
    /// Whether it came through the signed store. For a package waiting to be confirmed: false for one the person chose from
    /// somewhere else, which the window warns about before it can be installed. For an installed one: whether it was installed
    /// from the store, which is what decides if an update keeps what the person allowed (`plugins::Installation`); told to the
    /// window only, never to another plugin. False from an owner that predates it.
    #[serde(default)]
    pub from_store: bool,
}

/// A directory the person chose, as a path that is checked when it is READ: it must be absolute and
/// must not be empty. A value that is not cannot get past the protocol, so nothing after it has to
/// ask. (An empty path reads without error as a `PathBuf`, and would mean the current directory.)
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SourceDir(PathBuf);

impl SourceDir {
    pub fn new(path: impl Into<PathBuf>) -> Option<Self> {
        let path = path.into();
        (path.is_absolute() && path.as_os_str().len() > 1).then_some(Self(path))
    }

    pub fn as_path(&self) -> &std::path::Path {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SourceDir {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let path = PathBuf::deserialize(deserializer)?;
        Self::new(path)
            .ok_or_else(|| serde::de::Error::custom("a directory must be an absolute path"))
    }
}

/// A producer names either an existing paired connection or one files-code generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum FileDestination {
    Peer {
        peer: PeerId,
    },
    Invitation {
        generation: u64,
    },
    /// A device nearby that is not paired and said it takes files (see `nearby::Stranger`): each file is offered to
    /// it, and is its person's to accept. Sixteen files at most, in one command; never a pairing.
    Nearby {
        peer: PeerId,
    },
}

/// Commands use the same authorization and validation regardless of client.
///
/// Field-less commands are written `Name {}` rather than as unit variants on purpose:
/// serde does not apply `deny_unknown_fields` to unit variants of an internally
/// tagged enum, so `{"command":"shutdown","anything":1}` would be accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Command {
    /// Create a fresh invitation for explicit mutual pairing.
    CreateInvitation {},
    /// Join an invitation supplied by the user or QR scanner.
    JoinInvitation { invitation: String },
    /// Create a code to HAND OVER files, for the files chosen on Send. A device that scans it is offered exactly these files,
    /// each of which it must accept, and nothing else: no pairing, no trust, and the device is not remembered as a peer.
    /// A new code replaces the previous files code, and what was chosen for it. Never touches a code shown in Peers.
    CreateFilesInvitation { paths: Vec<PathBuf> },
    /// Create a files code for files the system handed over already open (Android); each is named as in
    /// [`Command::SendHeld`] and taken once by its number.
    CreateHeldFilesInvitation { files: Vec<(u64, String)> },
    /// Start one files-only code before opening a potentially large native selection.
    BeginFilesInvitation { count: u32 },
    /// All choices have been accounted for; refused files never entered the queue.
    FinishFilesInvitation { generation: u64, refused: u32 },
    /// Take back the files code of this generation, with the files that wait behind it, while no device has scanned it: the person chose a
    /// paired device for those files instead. Refused when the code was scanned, ran out or is not the one on show; nothing is sent by this.
    WithdrawFilesInvitation { generation: u64 },
    /// Authorize one displayed number; never a remote suggested selection.
    ChoosePairing { request: String, number: u8 },
    /// Refuse a pending pairing request.
    RejectPairing { request: String },
    /// Answer only the exact prompt issued by this owner.
    AnswerExtensionPrompt {
        id: u64,
        accepted: bool,
        #[serde(default)]
        secret: Option<ExtensionSecret>,
        #[serde(default)]
        remove: bool,
    },
    /// Answer the notification of this number: with the id of one of its buttons, or with none, which closes it. Refused for a
    /// number that is not on show and for a button it does not have.
    AnswerNotification {
        id: u64,
        #[serde(default)]
        button: Option<String>,
    },
    /// The person tapped the notification of this number itself (not a button). Refused for a number that is not on show and
    /// for one that opens nothing. The windows are told to show its sender's page ([`Snapshot::opened`]).
    OpenNotification { id: u64 },
    /// Remove a remembered device and close its active connections.
    ForgetPeer { peer: PeerId },
    /// Offer local files; a transfer never implies pairing.
    SendFiles { peer: PeerId, paths: Vec<PathBuf> },
    /// Offer files that the system handed over already open (on Android a picked file has no path, only a
    /// descriptor). Each is named by the number it was kept under, and by the name to offer it as. The owner takes
    /// each file once by its number; a number that is unknown or was already used is refused. Judged as
    /// [`Command::SendFiles`] is, and a transfer never implies pairing.
    SendHeld {
        peer: PeerId,
        files: Vec<(u64, String)>,
    },
    /// Test the connection to a paired device now, and say what was found in the snapshot (`link_tests`). It asks for nothing about the
    /// device but whether it answers and knows this one, and changes nothing about it.
    TestLink { peer: PeerId },
    /// Forget what the last test of a device found, so that its answer is not shown again.
    ClearLinkTest { peer: PeerId },
    /// Measure how quickly every configured relay answers, and say what was found in the snapshot (`relay_probes`). Nothing is sent to
    /// a relay beyond opening a connection, and nothing about the relays is changed.
    ProbeRelays {
        /// Test only this relay (its [`probe_key`](crate::relay::RelayTarget::probe_key)); every relay when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        only: Option<String>,
    },
    /// Say where a paired device can be found: an IP address or a name, with a port or without one, as the person typed it. `None` clears
    /// what was said. The OWNER reads the text (see [`crate::address::TypedAddress::parse`]) and refuses what is not a place, so no window
    /// has to be trusted to have done it; the answer to "does it work" is a test of the connection, asked for separately. It only says where
    /// to look: which device answers is decided by its key. Only a paired device can be given one.
    SetAddress {
        peer: PeerId,
        address: Option<String>,
    },
    /// Bounded producer handoff. QueueBusy leaves every source untouched so the caller can
    /// wait for an owner update and retry the same batch (at most sixteen files).
    QueueFiles {
        destination: FileDestination,
        paths: Vec<PathBuf>,
    },
    /// The held-file counterpart of QueueFiles; QueueBusy never takes or releases a number.
    QueueHeld {
        destination: FileDestination,
        files: Vec<(u64, String)>,
    },
    /// Offer a whole folder, with every folder and file in it, to a paired device. It carries the ONE folder the person chose and
    /// never a list of what is in it: the owner walks the folder as the files go, so that a folder of any size costs a command the
    /// size of a path. What the walk leaves out (a link, a file that cannot be read) is said in the reply, not lost. A transfer
    /// never implies pairing.
    SendFolder { peer: PeerId, root: PathBuf },
    /// Consent to one offered file, into a locally selected directory.
    AcceptTransfer {
        transfer: String,
        directory: PathBuf,
    },
    /// Refuse or cancel a transfer.
    CancelTransfer { transfer: String },
    /// Forget every transfer that is over (done, cancelled or failed); those still waiting or moving are kept. Never touches a
    /// file that was received or sent: only the record of it.
    ClearHistory {},
    /// Replace preferences only if the editor's baseline is still current.
    UpdateSettings {
        settings: Box<Settings>,
        expected: Box<Settings>,
    },
    /// Merge explicit edits into the owner's latest preferences atomically.
    PatchSettings { patch: Box<SettingsPatch> },
    /// Replace all key bindings at once (an empty map clears them). Each extension action must be installed, runnable by a key,
    /// and name a paired device exactly when it needs one. Desktop only; refused on Android.
    SetBindings { bindings: crate::bindings::Bindings },

    /// Replace what one plugin may do: for one paired device when `peer` is given, and
    /// outright, for no device, when it is not. Which permissions belong to which is decided
    /// by the application; the other kind is refused.
    SetGrants {
        plugin: PluginId,
        #[serde(default)]
        peer: Option<PeerId>,
        grants: Grants,
    },
    /// Turn ONE permission of one plugin on or off, for one paired device when `peer` is given and
    /// outright when it is not. The owner applies it to what it holds at that moment, so two flips
    /// made in quick succession cannot undo each other, which a replacement of the whole set
    /// ([`Command::SetGrants`]) does when both start from the same picture. The same rules apply
    /// as there: a permission that cannot be granted to this plugin is refused.
    Allow {
        plugin: PluginId,
        #[serde(default)]
        peer: Option<PeerId>,
        permission: peersus_api::permission::Permission,
        on: bool,
    },
    /// Set one declared plugin setting. `peer` is required exactly for per-device
    /// settings and forbidden for global ones.
    SetPluginSetting {
        plugin: PluginId,
        peer: Option<PeerId>,
        setting: String,
        value: Value,
    },
    /// Apply the choice on the disclosed public HTTPS toggle, for the exact scope shown.
    SetPublicReadSetting {
        plugin: PluginId,
        setting: String,
        resource: peersus_api::contributions::PublicRead,
        enabled: bool,
    },
    /// Run an action the plugin declared. `peer` is required exactly when the
    /// declaration says the action needs a device.
    InvokeAction {
        plugin: PluginId,
        action: String,
        peer: Option<PeerId>,
    },
    /// The displays of this device's desktop changed: trusted composition only, from the system's own
    /// account of them, never from a window or a guest.
    #[serde(skip)]
    SetDisplays {
        displays: Vec<crate::layout::Display>,
    },
    /// What nearness says now (see `nearby`): trusted composition only, from the platform's Bluetooth adapter, never
    /// from a window or a guest. The owner keeps of `near` only devices that are paired and of `strangers` only ones
    /// that are not. `offers` is the invitation each stranger announced, as text: kept by the owner for
    /// [`Command::PairNearby`] and never told to a window.
    #[serde(skip)]
    SetNearby {
        found: crate::nearby::Found,
        offers: Vec<(PeerId, String)>,
    },
    /// What this device announces to devices nearby now: trusted composition only, asked by what drives the Bluetooth
    /// adapter, which says with it whether a window is in `front` now. Answered with whether a window is open (devices
    /// nearby are listened for) and, when that window is in front and lets this device be found for something (see
    /// [`Command::SeekNearby`]), with the invitation to announce and what it is for; what was out for anything else is
    /// taken back.
    #[serde(skip)]
    NearbyOffer { front: bool },
    /// A window is open. Said again every few seconds for as long as it is: only then are devices nearby listened
    /// for. `takes` is what the page it shows lets this device be found for, and only that is announced: to be paired
    /// with while the list of devices nearby is shown, to be sent files while Receive is, and nothing on any other
    /// page. Nothing happens while the person's switch for nearness is off.
    SeekNearby {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        takes: Option<crate::nearby::Takes>,
    },
    /// Begin pairing with a device of that list. What follows is the pairing a code begins: both people choose.
    PairNearby { peer: PeerId },
    /// A person sets how near "near" is. Said again every few seconds for as long as they do: only then does the
    /// snapshot tell the strength of the strongest device of this application that is heard (`nearby.strongest`).
    MeasureNearby {},
    /// Captured platform input sent through the extension's declared input transport, independently
    /// of any page. The owner validates every event and the destination's per-device permission.
    ForwardInput {
        plugin: PluginId,
        peer: PeerId,
        events: Vec<serde_json::Value>,
        end: bool,
    },
    /// What a person did on a page whose content an extension gives (`contributions::Surface::Elements`):
    /// a `elements::Event`, checked by the owner and sent on to the page's source, whose answer is the
    /// page's content after it (see `Snapshot::page_views`).
    PageEvent {
        plugin: PluginId,
        page: String,
        event: peersus_api::elements::Event,
    },
    /// Desktop Send supplies its own selected path, never an existing context or held
    /// handle. The owner snapshots it and creates a fresh caller-bound context.
    /// HTTP authorization is still required separately before any upload.
    InvokeSelectedPathAction {
        plugin: PluginId,
        action: String,
        path: PathBuf,
    },
    /// Internal composition-only action token. It is deliberately not an IPC command:
    /// a serialized local request must never acquire another operation's selected input.
    #[serde(skip)]
    InvokeActionWithContext {
        plugin: PluginId,
        action: String,
        peer: Option<PeerId>,
        context: u64,
    },
    /// Trusted Send composition only; never accepted from serialized IPC or a guest.
    #[serde(skip)]
    InvokeSelectedAction {
        plugin: PluginId,
        action: String,
        path: Option<PathBuf>,
        held: Option<(u64, String)>,
    },
    /// Trusted composition only; never accepted from serialized IPC or a guest. The text is staged once, bound to
    /// the plugin that declared the action, and redeemed by that plugin through the platform.
    #[serde(skip)]
    InvokeSharedText {
        plugin: PluginId,
        action: String,
        peer: PeerId,
        text: String,
    },
    /// Withdraw precisely one pending user action; already published external effects cannot be undone.
    CancelAction {
        plugin: PluginId,
        action: String,
        peer: Option<PeerId>,
    },
    /// Stop one plugin's process and start a new one, whether or not anything is wrong with it.
    /// What the old process was doing is withdrawn, as when one of its settings changes.
    RestartPlugin { plugin: PluginId },
    /// Take an installed plugin out of the store and forget everything it was granted, on every
    /// device and outright, in the same step: the owner is the only writer of those records, so a
    /// package installed afterwards under the same name starts with nothing.
    RemovePackage { plugin: PluginId },
    /// Let an installed plugin run again. It keeps what it was granted.
    EnablePlugin { plugin: PluginId },
    /// Stop an installed plugin and keep it from running until it is enabled. It stays installed
    /// and keeps what it was granted.
    DisablePlugin { plugin: PluginId },
    /// Approve the plugin `plugin` as it is on disk and as it was shown: `program` is the digest the owner reported for it
    /// ([`PluginInfo::program`]). If the program is not that one any more, nothing is approved. Only a plugin that waits for
    /// approval or has changed can be approved; the capabilities approved are what its manifest asks for now, which the person
    /// was shown together with the digest.
    ApprovePlugin { plugin: PluginId, program: String },
    /// Read the package in `source`, a directory the person chose, and show what it is and what it
    /// asks for. Nothing is installed by this.
    StageInstall { source: SourceDir },
    /// Review a private unpacked archive for development replacement. Retention is
    /// decided from the owner's registered predecessor, never from client proof.
    StageDevelopmentInstall {
        source: SourceDir,
        replacement_only: bool,
    },
    /// Install the package that was shown for `plugin`, and only that one. It starts with no
    /// permissions. `agreement` is the digest ([`crate::agreements::digest`]) of the licences and notice the person
    /// accepted on the window's overlay: the owner installs only if it is the agreement of the package it holds, and a
    /// package that has none cannot be installed. `risk_acknowledged` is what the person did on the warning the window
    /// shows for a package that did not come through the store (read to the end, then held): the owner installs such a
    /// package only if it is true, and a package from the store does not need it.
    ConfirmInstall {
        plugin: PluginId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agreement: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        risk_acknowledged: bool,
    },
    /// Let go of the package that was shown for `plugin` and not confirmed. Nothing is installed,
    /// and nothing of it is kept.
    CancelInstall { plugin: PluginId },
    /// Fetch the maintainer-approved extension catalog and check its signature. Nothing is installed.
    StoreRefresh {},
    /// Download the approved package for `plugin` from the store, check that it is exactly what the
    /// maintainer approved, and install it. Choosing to install IS the confirmation: what the person
    /// pressed on showed the author, link and version. The plugin starts with nothing allowed.
    StoreInstall { plugin: PluginId },
    /// Show `plugin`'s entry in the store, in every open window, as a store link asked (`peersus://store/<id>`, see
    /// [`store_link_plugin`]): the window opens the store at it and, when it is listed and not installed, starts its Install,
    /// which still asks the person to accept its licence and notice. Nothing is installed by the link itself.
    ShowStoreEntry { plugin: PluginId },
    /// Ask whether a newer release of the application was published, and check its signature.
    /// Nothing is downloaded or changed.
    UpdateCheck {},
    /// Download the release the last check verified, check it byte for byte, and put it in place.
    /// Names nothing: what is fetched is decided by the signed release, never by this command.
    UpdateInstall {},
    /// Start the application again so that the update that was put in place runs.
    UpdateRestart {},
    /// The device's own language, as the platform says it now (the system's first preferred language), or none when it names
    /// none. Sent by a frontend, never by a person: it is what a person who chose no language is shown in.
    SetDeviceLanguage { language: Option<Language> },
    /// Look again at which languages are offered for this release, and fetch the one shown if it is not held yet.
    LanguagesRefresh {},
    /// Download the file of `language` for this release from the languages branch and hold it. Choosing it is a
    /// separate step (`patch-settings`).
    LanguageDownload { language: Language },
    /// Hold a language file a person wrote or was given, as it is (a gettext PO file, at most
    /// [`peersus_api::language::MAX_FILE_BYTES`]). It must name its language; it replaces an imported file of the same language
    /// and is shown before a downloaded one of it. Never overwritten by a download or an update.
    LanguageImport { text: String },
    /// Let go of the file of `language` this device holds: the imported one when `imported`, else the downloaded one.
    LanguageRemove { language: Language, imported: bool },
    /// The text of the language file the windows are shown in (`languages.shown_file`), as held: what a window reads its words
    /// from. Refused while they are shown in English. A window asks once for each `languages.revision`, as for a picture.
    LanguageText {},
    /// Development builds only: save a private catalog source and apply it immediately.
    #[cfg(feature = "private-store")]
    ConfigurePrivateStore {
        source: crate::private_store::PrivateCatalog,
    },
    /// Make this the picture that stands for this device. It is told to every paired device that is
    /// online, which asks for the picture only if it does not already hold it.
    SetProfilePicture { picture: Picture },
    /// Stop having a profile picture.
    ClearProfilePicture {},
    /// Ask for the picture with this digest, as the owner holds it: this device's own, or one a
    /// paired device showed. The answer is the picture; a digest the owner does not hold is refused.
    FetchPicture { digest: Digest },
    /// Ask for the picture an extension keeps for its pages (`peersus.pictures`) with this digest, which a window learns only
    /// from a page that places it. The answer is the picture; a digest no extension keeps is refused.
    FetchKeptPicture { digest: Digest },
    /// What `plugin` keeps for its drawings now (`peersus.drawings`), by layer id, each with the stamp of the content kept under it: what a
    /// window compares with what it holds when a snapshot's `kept_layers` names a stamp of that extension it does not hold. Never refused:
    /// an extension that keeps nothing keeps nothing.
    LayerIndex { plugin: PluginId },
    /// The content of the layers `ids` of `plugin`, as kept now: as many of them, in the order asked, as one answer may carry (a window asks
    /// again for the rest), and not an id that is no longer kept. Never refused.
    FetchLayers { plugin: PluginId, ids: Vec<String> },
    /// This device sleeps (the screen went off, or the system went idle) or is awake again. Sent by a platform, never by a
    /// person: it is what lets the devices that are connected show this one as sleeping instead of reading its silence as gone.
    /// Saying what is already so changes nothing.
    SetAsleep { asleep: bool },
    /// Set the access token of the personal relay, or forget it when empty. It is a secret: kept apart from every
    /// preference, never shown in a snapshot, a log or a `Debug`, and it only ever travels to the owner on this one
    /// local, same-user connection.
    SetRelayToken { token: SecretText },
    /// Read the diagnostics folder again: which crash reports it holds now (a window that crashed left one the owner did not
    /// see written). Sent when the Diagnostics page is shown.
    RefreshDiagnostics {},
    /// Delete every crash report in the diagnostics folder, and nothing else in it.
    DeleteCrashReports {},
    /// Gracefully stop owned work.
    Shutdown {},
}

/// What a test of the connection to a paired device found, as a closed set. A person is told which of three things it is, because each
/// means doing something different: nothing, pairing again, or looking at the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkTest {
    /// The test is going on and has not ended.
    Trying,
    /// The device answered and knows this one: it is paired and the connection works.
    Paired,
    /// The device answered and does not know this one: it must be paired again. Nothing else is wrong.
    NeedsPairing,
    /// There was no answer: no route to it, or it did not reply in time. It may be off, asleep out of reach, or at another address.
    Unreachable,
}

/// Latest bounded state; consuming it performs no network or disk work.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub settings: Settings,
    pub peers: Vec<Peer>,
    /// What plugins were granted that is about no device, by plugin. What is about a device is
    /// on that device, in `peers`.
    #[serde(default)]
    pub grants: std::collections::BTreeMap<PluginId, Grants>,
    /// Registered plugins and their declared contributions, for generic rendering.
    #[serde(default)]
    pub plugins: Vec<PluginInfo>,
    /// Packages the person chose and the owner read, waiting to be confirmed: what each is and what
    /// it asks for, shown before anything is installed. Absent in a snapshot from an older owner.
    #[serde(default)]
    pub staged: Vec<PluginInfo>,
    /// The extension store: what the maintainer approved, as the last verified catalog said.
    #[serde(default)]
    pub store: StoreState,
    /// Updates of the application itself: what the last verified release said. Absent from an older owner.
    #[serde(default)]
    pub update: UpdateState,
    /// Where each action a user started has got to, in the application's own words.
    #[serde(default)]
    pub actions: Vec<ActionStatus>,
    /// Recent and active transfers, bounded by the host.
    pub transfers: Vec<Transfer>,
    /// Unfinished consent ceremonies.
    pub pairing: Vec<PairingPrompt>,
    #[serde(default)]
    pub extension_prompts: Vec<ExtensionPrompt>,
    /// Notifications on show, oldest first: the application's own and extensions', in one list. Absent from an older owner.
    #[serde(default)]
    pub notifications: Vec<PostedNotification>,
    /// The last extension whose page a tapped notification asked every window to show ([`Command::OpenNotification`]). A
    /// window acts on a `serial` it has not acted on yet; one that opens later does not act on an old one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened: Option<PageAsked>,
    /// Sanitized errors to show near affected operations.
    pub errors: Vec<String>,
    /// How many times the owner has said something since it started, counting a sentence its list already ends with:
    /// the same failure again is news, though the list does not change. Absent from an older owner, read as nothing.
    #[serde(default)]
    pub errors_said: u64,
    /// The digest of this device's own profile picture, or none when it has none. The picture itself
    /// never travels in a snapshot: a window that does not hold that digest asks for it once, with
    /// `FetchPicture`, so a change to anything else never carries sixty kilobytes of pixels again.
    #[serde(default)]
    pub profile_picture: Option<Digest>,
    /// The digest of the picture each paired device last showed this one, by device. Only devices
    /// that showed one are here.
    #[serde(default)]
    pub peer_pictures: BTreeMap<PeerId, Digest>,
    /// The displays of this device's desktop, as the system says now: what a layout places it as.
    /// Empty where there is no desktop to place (a phone) or the system did not say.
    #[serde(default)]
    pub displays: Vec<crate::layout::Display>,
    /// The displays each connected device said its desktop has, by device.
    #[serde(default)]
    pub peer_displays: BTreeMap<PeerId, Vec<crate::layout::Display>>,
    /// The files code on show, if there is one: what a screen needs to draw it and nothing of the code itself. Absent in a snapshot
    /// from an older owner, and when no code is out.
    #[serde(default)]
    pub files_code: Option<FilesCode>,
    /// What the last test of a connection to a paired device found, by device: only devices that were tested, and only until a window
    /// clears it. A test is asked for and the answer comes later, so it is told here as a state and not as the answer to the command.
    #[serde(default)]
    pub link_tests: BTreeMap<PeerId, LinkTest>,
    /// Where the person said each paired device can be found, as they would type it again: only for devices that were given one. Told so that
    /// the window can show what is saved and fill the field with it, without reading any file.
    #[serde(default)]
    pub typed_addresses: BTreeMap<PeerId, String>,
    /// The port this device listens on: what another device writes after its address to find this one. A different one each time the device
    /// starts, so it is told and not kept. `None` from an owner from before, and when none is bound.
    #[serde(default)]
    pub listening_port: Option<u16>,
    /// Sanitized home-relay health; absent when connected to an older owner.
    #[serde(default)]
    pub relay: Option<crate::relay::RelayDiagnostics>,
    /// What the last test of each relay found, by the relay's address (`n0` for the public pool): only relays that were tested.
    #[serde(default)]
    pub relay_probes: BTreeMap<String, RelayProbe>,
    /// What each extension's page shows now, as the extension last answered: checked by the owner, and
    /// gone with the extension when it stops. Only pages that were opened are here.
    #[serde(default)]
    pub page_views: Vec<PageView>,
    /// What each extension keeps for its drawings (`peersus.drawings`), by stamp only: the shapes never travel in a snapshot, but are
    /// fetched, once for each change, by a window that does not hold the stamp (see [`KeptLayers`]). Only extensions that keep something
    /// are here. Nothing from an older owner.
    #[serde(default)]
    pub kept_layers: Vec<KeptLayers>,
    /// The paired devices that were brought near, where that is looked for (see `nearby`). Nothing from an older owner,
    /// and nothing while the switch is off.
    #[serde(default)]
    pub nearby: crate::nearby::Found,
    /// Languages: the device's own, the one shown, the files held and what is offered for this release. Absent from an older
    /// owner, read as English with nothing held.
    #[serde(default)]
    pub languages: LanguageState,
    /// Crash capture of the owner and the crash reports this device holds. Nothing from an older owner.
    #[serde(default)]
    pub diagnostics: DiagnosticsState,
}

/// How crash capture of one program stands (see `peersus_diagnostics::crash_watch`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureState {
    /// Not switched on.
    #[default]
    Off,
    /// Switched on; the helper is starting.
    Starting,
    /// The helper watches the program.
    Watching,
    /// Switched on, and this system has no capture from outside (Linux only).
    NotThisSystem,
    /// Switched on, and the helper program is not installed beside this one.
    Missing,
    /// Switched on, and the helper did not start or answer.
    NotStarted,
    /// Switched on, and the system refuses tracing.
    Refused,
}

/// One crash report in the diagnostics folder: its minidump's file name, size and when it was written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashReport {
    pub file: String,
    pub bytes: u64,
    pub unix_seconds: u64,
}

/// Crash capture and its reports, as the Diagnostics page shows them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsState {
    /// How capture of the owner stands. Each window knows its own.
    #[serde(default)]
    pub owner: CaptureState,
    /// The crash reports held, newest first.
    #[serde(default)]
    pub reports: Vec<CrashReport>,
    /// The folder they are in, for a window to open; none where there is no data directory.
    #[serde(default)]
    pub folder: Option<std::path::PathBuf>,
}

/// One language file, as a list of languages shows it: the language, the name it gives itself, how many of the
/// application's messages it translates (a window knows how many there are, and says how far along it is), and its size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageFile {
    pub language: Language,
    pub name: String,
    pub translated: u32,
    /// The file's size in bytes: what downloading it costs. Zero from an offer written before sizes were.
    #[serde(default)]
    pub bytes: u64,
    /// Whether the file says a machine translated it (`X-Translation: ai`), so that a person knows before choosing it. False from
    /// an offer written before files said so.
    #[serde(default)]
    pub machine: bool,
}

/// A language file this device holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldLanguage {
    #[serde(flatten)]
    pub file: LanguageFile,
    /// The release whose languages it was downloaded with, or none when a person imported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
}

/// Where fetching languages has got to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LanguageStatus {
    #[default]
    Idle,
    Fetching,
    /// The last fetch failed; see [`LanguageState::error`]. Tried again later on its own.
    Failed,
}

/// Languages as a window draws and uses them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageState {
    /// The device's own language, as its platform said; none when it named none.
    #[serde(default)]
    pub device: Option<Language>,
    /// What the windows are shown in: the language wanted (the chosen one, else the device's) when a file of it, or of its
    /// primary language, is held; English otherwise.
    #[serde(default)]
    pub shown: Language,
    /// The held file `shown` is read from; none for English.
    #[serde(default)]
    pub shown_file: Option<HeldLanguage>,
    #[serde(default)]
    pub held: Vec<HeldLanguage>,
    /// What the languages branch offers for this release, as last fetched.
    #[serde(default)]
    pub offered: Vec<LanguageFile>,
    #[serde(default)]
    pub status: LanguageStatus,
    /// A sentence this application chose, when `status` is `failed`.
    #[serde(default)]
    pub error: Option<String>,
    /// Changes whenever a held file changes, so that a window reads again the one it shows.
    #[serde(default)]
    pub revision: u64,
}

/// The content of one extension page, as the owner checked it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageView {
    pub plugin: PluginId,
    pub page: String,
    pub view: peersus_api::elements::Node,
}

/// What one extension keeps for its drawings, as a snapshot tells of it: that it changed, and nothing of what it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptLayers {
    pub plugin: PluginId,
    /// Changes whenever any layer of the extension does, and is never the same twice from one owner (nor, but for chance, from two).
    /// Opaque: only whether it is the same as the one a window holds means anything. Never zero.
    pub stamp: u64,
}

/// What an extension keeps for its drawings, as [`Command::LayerIndex`] answers: every layer's id with the stamp of the content kept
/// under it, and the stamp of the whole, as `Snapshot::kept_layers` says it (zero when nothing is kept).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerIndex {
    pub stamp: u64,
    pub layers: Vec<(String, u64)>,
}

/// The content of one kept layer, as [`Command::FetchLayers`] answers: the stamp it is kept under now, and its shapes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerContent {
    pub id: String,
    pub stamp: u64,
    pub shapes: Vec<peersus_api::elements::Shape>,
}

/// What a test of a relay found: how long it took to answer, or that it did not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state")]
pub enum RelayProbe {
    /// The test is going on.
    Trying,
    /// The relay accepted a connection after this many milliseconds.
    Answered { ms: u32 },
    /// It did not answer in time.
    Unreachable,
}

/// Where the store has got to, as a closed set: what the application says about it is its own text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoreStatus {
    /// This build has no maintainer key, so nothing can be verified and the store refuses everything.
    NotConfigured,
    /// Not asked yet.
    #[default]
    Idle,
    Loading,
    /// A verified catalog is held; see [`StoreState::listings`].
    Ready,
    /// The last refresh or install failed; see [`StoreState::error`].
    Failed,
}

/// One extension the maintainer approved. Only what the SIGNED catalog said, never what a package says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreListing {
    /// The plugin's name, which is the id it will have when installed.
    pub plugin: PluginId,
    pub version: String,
    pub author: String,
    pub link: String,
    #[serde(default)]
    pub description: String,
    /// The size in bytes of the archive this device would download, as the signed catalog pinned it: what installing or
    /// updating costs, said before a person chooses. Zero from an owner written before listings carried it.
    #[serde(default)]
    pub bytes: u64,
    /// The version installed under this name now, or none when it is not installed. Read by the owner
    /// from the installed folder, never from the catalog: it is what is on this device.
    #[serde(default)]
    pub installed_version: Option<String>,
}

/// The store as a screen draws it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreState {
    pub status: StoreStatus,
    #[serde(default)]
    pub listings: Vec<StoreListing>,
    /// A sentence this application chose, when `status` is `failed`.
    #[serde(default)]
    pub error: Option<String>,
    /// The last entry a store link asked to be shown ([`Command::ShowStoreEntry`]). A window acts on a `serial` it has not
    /// acted on yet; one that opens later does not act on an old one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<StoreAsked>,
}

/// One request of a store link to show an entry, numbered so that each is acted on once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreAsked {
    pub serial: u64,
    pub plugin: PluginId,
}

/// One request to show an extension's page, numbered so that each is acted on once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageAsked {
    pub serial: u64,
    pub plugin: PluginId,
}

/// The start of a store link: the address a web page gives to open one extension of the store in Peersus. The system hands
/// the address to the application (the desktop entry's `x-scheme-handler/peersus`, Android's `VIEW` intent filter).
pub const STORE_LINK: &str = "peersus://store/";

/// The extension a store link names, or none for anything that is not exactly `peersus://store/<id>` (one trailing `/`
/// allowed) with a valid extension id. Nothing else of an address is read: no query, no fragment, no other host.
pub fn store_link_plugin(link: &str) -> Option<PluginId> {
    let id = link.strip_prefix(STORE_LINK)?;
    let id = id.strip_suffix('/').unwrap_or(id);
    PluginId::new(id).ok()
}

/// Where an update of the application has got to, as a closed set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateStatus {
    /// This build has no publisher key for updates, so none can be verified and every step is refused.
    #[default]
    NotConfigured,
    /// This installation cannot replace itself; see [`UpdateState::error`] for why.
    Unsupported,
    /// Not asked yet.
    Idle,
    Checking,
    /// The last check found nothing newer than what runs.
    Current,
    /// A verified, newer release is waiting; see [`UpdateState::offered`].
    Available,
    Downloading,
    Installing,
    /// The update is in place and runs after a restart.
    Installed,
    /// The application is ending so that the update runs: whatever started it starts it again.
    Restarting,
    /// The last check, download or install failed; see [`UpdateState::error`].
    Failed,
}

/// Application updates as a screen draws them. Only what a SIGNED release said, and this build's own name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    pub status: UpdateStatus,
    /// The build that is running, as it names itself.
    #[serde(default)]
    pub current: String,
    /// The build a verified release offers, when it is newer.
    #[serde(default)]
    pub offered: Option<String>,
    /// The size in bytes of the offered release's file for this installation, as the release listing gave it: what the
    /// update downloads, at most. Zero when nothing is offered.
    #[serde(default)]
    pub offered_bytes: u64,
    /// A sentence this application chose, when `status` is `failed` or `unsupported`.
    #[serde(default)]
    pub error: Option<String>,
}

/// A files code as a screen may know it. The code is a bearer capability and is never in a snapshot: a screen asks for it in the
/// reply to the command that made it, and keeps it only as long as it draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesCode {
    /// Higher for every code made: what a transfer's [`Transfer::code`] is compared with, and what a screen remembers about the
    /// code it drew.
    pub generation: u64,
    /// Whether the code could still admit a device: false once it was scanned, once it expired, and once it was revoked.
    pub live: bool,
    /// The selection still has unopened or queued sources; completed first batches
    /// do not mean the whole selection is done.
    #[serde(default)]
    pub pending: bool,
}

/// Why an action a user started did not complete, in words this application chose.
///
/// A closed set on purpose: what a plugin says about its own failure is its own text, and
/// text a plugin wrote has no place in what the application shows. An unrecognised failure
/// code becomes `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionFailure {
    /// The plugin, or the device, has not been allowed to do this.
    NotAllowed,
    /// The plugin is not running.
    Unavailable,
    /// It stopped (ended or was restarted) while the request was still outstanding for it, before it answered: it may
    /// have done part of the work.
    Stopped,
    /// It did not answer in time.
    TimedOut,
    /// It could not take the request now.
    Busy,
    /// The destination limited further requests.
    RateLimited,
    /// The destination cannot accept something this large.
    TooLarge,
    /// The request was refused, locally or at its destination.
    Refused,
    /// The destination did not accept the saved credential.
    CredentialRejected,
    /// It failed for a reason that is not one of the above.
    Other,
}

impl ActionFailure {
    /// The application's own words for the owner notice and log.
    pub const fn notice(self) -> &'static str {
        match self {
            Self::NotAllowed => "A plugin was not allowed to complete the action",
            Self::Unavailable => {
                "A plugin could not complete the action: what it needs is not available"
            }
            Self::Stopped => "A plugin stopped before it finished the action",
            Self::TimedOut => "A plugin did not complete the action in time",
            Self::Busy => "A plugin could not complete the action: it is busy",
            Self::RateLimited => "The destination's limit was reached. Try again later.",
            Self::TooLarge => "What was sent is larger than the destination accepts.",
            Self::Refused => "The request was refused",
            Self::CredentialRejected => "The saved credential was not accepted. Save a valid one.",
            Self::Other => "A plugin could not complete the action",
        }
    }
}

/// Where one action a user started has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ActionState {
    /// Sent to the plugin and not answered yet.
    Running,
    /// The plugin answered that it did it.
    Done,
    /// The plugin completed the action, but could not present its result.
    DoneUnshown,
    /// Stopped by the user. An external operation already submitted may still have completed.
    Cancelled,
    /// A required permission was missing, so the action was not carried out.
    Blocked,
    /// It did not complete.
    Failed { reason: ActionFailure },
}

/// The application-owned notice shared by the action row and the owner.
pub const ACTION_RESULT_NOT_SHOWN: &str = "The action finished, but its result could not be shown. Check what happened before repeating it.";

/// The latest state of one action, named only by ids the application already knows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionStatus {
    pub plugin: PluginId,
    pub action: String,
    /// The device it was for, if the action takes one.
    pub peer: Option<PeerId>,
    pub state: ActionState,
    /// Distinct file payload bytes consumed by the request transport and total snapshot bytes.
    /// While running, even the full count does not imply a successful action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<(u64, u64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(command: &Command) -> Command {
        serde_json::from_str(&serde_json::to_string(command).unwrap()).unwrap()
    }

    /// Every reason the application can give, listed by an exhaustive `match`: a new variant is
    /// a compile error here, so adding one is a decision somebody has to make on purpose.
    fn every_failure() -> Vec<ActionFailure> {
        let mut all = Vec::new();
        for reason in [
            ActionFailure::NotAllowed,
            ActionFailure::Unavailable,
            ActionFailure::Stopped,
            ActionFailure::TimedOut,
            ActionFailure::Busy,
            ActionFailure::RateLimited,
            ActionFailure::TooLarge,
            ActionFailure::Refused,
            ActionFailure::CredentialRejected,
            ActionFailure::Other,
        ] {
            // No wildcard arm: this only compiles while the list above covers the type.
            match reason {
                ActionFailure::NotAllowed
                | ActionFailure::Unavailable
                | ActionFailure::Stopped
                | ActionFailure::TimedOut
                | ActionFailure::Busy
                | ActionFailure::RateLimited
                | ActionFailure::TooLarge
                | ActionFailure::Refused
                | ActionFailure::CredentialRejected
                | ActionFailure::Other => all.push(reason),
            }
        }
        all
    }

    #[test]
    fn every_failure_reason_is_a_bare_word_on_the_wire_so_none_can_carry_text() {
        for reason in every_failure() {
            let value = serde_json::to_value(reason).unwrap();
            let word = value
                .as_str()
                .unwrap_or_else(|| panic!("{reason:?} is not a bare string: {value}"));
            assert!(
                !word.is_empty()
                    && word.len() <= 24
                    && word.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
                "{reason:?} serialises as {word:?}, which is not a fixed lowercase word"
            );
            // And it only reads back from exactly that word.
            let back: ActionFailure = serde_json::from_value(value).unwrap();
            assert_eq!(back, reason);
        }
    }
    #[test]
    fn an_action_state_serialises_as_a_small_closed_vocabulary() {
        let cases = [
            (ActionState::Running, r#"{"state":"running"}"#),
            (ActionState::Done, r#"{"state":"done"}"#),
            (ActionState::DoneUnshown, r#"{"state":"done-unshown"}"#),
            (ActionState::Cancelled, r#"{"state":"cancelled"}"#),
            (
                ActionState::Failed {
                    reason: ActionFailure::NotAllowed,
                },
                r#"{"state":"failed","reason":"not-allowed"}"#,
            ),
            (
                ActionState::Failed {
                    reason: ActionFailure::Unavailable,
                },
                r#"{"state":"failed","reason":"unavailable"}"#,
            ),
            (
                ActionState::Failed {
                    reason: ActionFailure::Stopped,
                },
                r#"{"state":"failed","reason":"stopped"}"#,
            ),
            (
                ActionState::Failed {
                    reason: ActionFailure::TimedOut,
                },
                r#"{"state":"failed","reason":"timed-out"}"#,
            ),
            (
                ActionState::Failed {
                    reason: ActionFailure::Busy,
                },
                r#"{"state":"failed","reason":"busy"}"#,
            ),
            (
                ActionState::Failed {
                    reason: ActionFailure::RateLimited,
                },
                r#"{"state":"failed","reason":"rate-limited"}"#,
            ),
            (
                ActionState::Failed {
                    reason: ActionFailure::TooLarge,
                },
                r#"{"state":"failed","reason":"too-large"}"#,
            ),
            (
                ActionState::Failed {
                    reason: ActionFailure::Refused,
                },
                r#"{"state":"failed","reason":"refused"}"#,
            ),
            (
                ActionState::Failed {
                    reason: ActionFailure::CredentialRejected,
                },
                r#"{"state":"failed","reason":"credential-rejected"}"#,
            ),
            (
                ActionState::Failed {
                    reason: ActionFailure::Other,
                },
                r#"{"state":"failed","reason":"other"}"#,
            ),
        ];
        for (state, wire) in cases {
            assert_eq!(serde_json::to_string(&state).unwrap(), wire);
            assert_eq!(serde_json::from_str::<ActionState>(wire).unwrap(), state);
        }
    }

    #[test]
    fn a_failure_reason_that_is_not_in_the_vocabulary_is_refused_not_carried() {
        // Whatever a plugin writes in its own words has no place to live in this type.
        for reason in [
            r#"{"state":"failed","reason":"disk full on /home/alex"}"#,
            r#"{"state":"failed","reason":""}"#,
            r#"{"state":"failed","reason":{"text":"hello"}}"#,
            r#"{"state":"failed"}"#,
            r#"{"state":"failed","reason":"other","detail":"secret"}"#,
        ] {
            assert!(
                serde_json::from_str::<ActionState>(reason).is_err(),
                "{reason} must be refused"
            );
        }
    }

    #[test]
    fn the_state_of_an_action_names_it_by_opaque_ids_only() {
        let status = ActionStatus {
            plugin: PluginId::new("clip").unwrap(),
            action: "share".into(),
            peer: Some(PeerId("peer-1".into())),
            state: ActionState::Done,
            progress: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(
            json,
            r#"{"plugin":"clip","action":"share","peer":"peer-1","state":{"state":"done"}}"#
        );
        assert_eq!(serde_json::from_str::<ActionStatus>(&json).unwrap(), status);
    }
    #[test]
    fn restarting_a_plugin_names_it_by_its_id_and_nothing_else_may_ride_along() {
        let command = Command::RestartPlugin {
            plugin: PluginId::new("any-plugin").unwrap(),
        };
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(
            json,
            r#"{"command":"restart-plugin","plugin":"any-plugin"}"#
        );
        assert!(matches!(
            roundtrip(&command),
            Command::RestartPlugin { plugin } if plugin.as_str() == "any-plugin"
        ));
        // An id that is not an identifier is refused when read, like every other plugin id.
        for hostile in [
            r#"{"command":"restart-plugin","plugin":"a/b"}"#,
            r#"{"command":"restart-plugin","plugin":""}"#,
            r#"{"command":"restart-plugin"}"#,
            r#"{"command":"restart-plugin","plugin":"ok","extra":1}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(hostile).is_err(),
                "{hostile}"
            );
        }
    }

    #[test]
    fn giving_up_a_package_names_only_the_plugin_that_was_shown() {
        let command = Command::CancelInstall {
            plugin: PluginId::new("clipboard").unwrap(),
        };
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(json, r#"{"command":"cancel-install","plugin":"clipboard"}"#);
        assert_eq!(serde_json::to_string(&roundtrip(&command)).unwrap(), json);
        for hostile in [
            r#"{"command":"cancel-install"}"#,
            r#"{"command":"cancel-install","plugin":"clipboard","source":"/evil"}"#,
            r#"{"command":"cancel-install","plugin":"a/b"}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(hostile).is_err(),
                "{hostile}"
            );
        }
    }

    #[test]
    fn store_commands_name_only_a_plugin_and_nothing_may_ride_along() {
        let refresh = Command::StoreRefresh {};
        assert_eq!(
            serde_json::to_string(&refresh).unwrap(),
            r#"{"command":"store-refresh"}"#
        );
        let install = Command::StoreInstall {
            plugin: PluginId::new("echo").unwrap(),
        };
        let json = serde_json::to_string(&install).unwrap();
        assert_eq!(json, r#"{"command":"store-install","plugin":"echo"}"#);
        assert_eq!(serde_json::to_string(&roundtrip(&install)).unwrap(), json);
        for hostile in [
            r#"{"command":"store-install"}"#,
            r#"{"command":"store-install","plugin":"a/b"}"#,
            // No address, path or digest can be supplied: what is fetched is decided by the signed catalog.
            r#"{"command":"store-install","plugin":"echo","url":"https://evil.example/x"}"#,
            r#"{"command":"store-install","plugin":"echo","sha256":"00"}"#,
            r#"{"command":"store-refresh","url":"https://evil.example/c"}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(hostile).is_err(),
                "{hostile}"
            );
        }
    }

    #[test]
    fn a_store_link_names_one_extension_and_nothing_else_of_an_address_is_read() {
        let test = PluginId::new("test").unwrap();
        assert_eq!(
            store_link_plugin("peersus://store/test"),
            Some(test.clone())
        );
        assert_eq!(store_link_plugin("peersus://store/test/"), Some(test));
        for refused in [
            "peersus://store/",
            "peersus://store/test/x",
            "peersus://store/test?install=1",
            "peersus://store/test#x",
            "peersus://store/../test",
            "peersus://other/test",
            "https://peersus.axxa.dev/store.html#test",
            "PEERSUS://store/test",
            " peersus://store/test",
        ] {
            assert_eq!(store_link_plugin(refused), None, "{refused}");
        }
        let shown = Command::ShowStoreEntry {
            plugin: PluginId::new("echo").unwrap(),
        };
        let json = serde_json::to_string(&shown).unwrap();
        assert_eq!(json, r#"{"command":"show-store-entry","plugin":"echo"}"#);
        assert!(
            serde_json::from_str::<Command>(
                r#"{"command":"show-store-entry","plugin":"echo","install":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn update_commands_name_nothing_and_nothing_may_ride_along() {
        for (command, json) in [
            (Command::UpdateCheck {}, r#"{"command":"update-check"}"#),
            (Command::UpdateInstall {}, r#"{"command":"update-install"}"#),
            (Command::UpdateRestart {}, r#"{"command":"update-restart"}"#),
        ] {
            assert_eq!(serde_json::to_string(&command).unwrap(), json);
        }
        for hostile in [
            r#"{"command":"update-check","channel":"other"}"#,
            r#"{"command":"update-install","url":"https://evil.example/x"}"#,
            r#"{"command":"update-install","version":"9.9.9"}"#,
            r#"{"command":"update-restart","program":"/tmp/x"}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(hostile).is_err(),
                "{hostile}"
            );
        }
    }

    #[test]
    fn a_snapshot_from_before_the_store_still_reads_as_idle_and_empty() {
        let mut value = serde_json::to_value(empty_snapshot()).unwrap();
        value.as_object_mut().unwrap().remove("store");
        let snapshot: Snapshot = serde_json::from_value(value).unwrap();
        assert_eq!(snapshot.store.status, StoreStatus::Idle);
        assert!(snapshot.store.listings.is_empty() && snapshot.store.error.is_none());
    }

    #[test]
    fn the_store_state_round_trips_in_a_stable_shape() {
        let state = StoreState {
            status: StoreStatus::Ready,
            listings: vec![StoreListing {
                plugin: PluginId::new("echo").unwrap(),
                version: "1.0.0".into(),
                author: "Someone".into(),
                link: "https://example.org/echo".into(),
                description: String::new(),
                bytes: 4096,
                installed_version: Some("0.9.0".into()),
            }],
            error: None,
            asked: None,
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains(r#""status":"ready""#), "{json}");
        assert!(json.contains(r#""installed_version":"0.9.0""#), "{json}");
        assert_eq!(serde_json::from_str::<StoreState>(&json).unwrap(), state);
        assert_eq!(
            serde_json::to_string(&StoreStatus::NotConfigured).unwrap(),
            r#""not-configured""#
        );
        // A listing from an owner that did not report versions reads as not installed.
        let older = r#"{"status":"ready","listings":[{"plugin":"echo","version":"1.0.0","author":"A","link":"https://example.org/e"}]}"#;
        assert_eq!(
            serde_json::from_str::<StoreState>(older).unwrap().listings[0].installed_version,
            None
        );
    }

    #[test]
    fn removing_enabling_and_disabling_a_plugin_name_it_by_id_and_nothing_else_may_ride_along() {
        for (name, build) in [
            (
                "remove-package",
                Command::RemovePackage {
                    plugin: PluginId::new("any-plugin").unwrap(),
                },
            ),
            (
                "enable-plugin",
                Command::EnablePlugin {
                    plugin: PluginId::new("any-plugin").unwrap(),
                },
            ),
            (
                "disable-plugin",
                Command::DisablePlugin {
                    plugin: PluginId::new("any-plugin").unwrap(),
                },
            ),
        ] {
            let json = serde_json::to_string(&build).unwrap();
            assert_eq!(
                json,
                format!(r#"{{"command":"{name}","plugin":"any-plugin"}}"#)
            );
            assert_eq!(serde_json::to_string(&roundtrip(&build)).unwrap(), json);
            for hostile in [
                format!(r#"{{"command":"{name}","plugin":"a/b"}}"#),
                format!(r#"{{"command":"{name}","plugin":""}}"#),
                format!(r#"{{"command":"{name}"}}"#),
                format!(r#"{{"command":"{name}","plugin":"ok","extra":1}}"#),
            ] {
                assert!(
                    serde_json::from_str::<Command>(&hostile).is_err(),
                    "{hostile}"
                );
            }
        }
    }

    // Its wire examples are Unix paths; what counts as a full path on Windows is a different spelling.
    #[cfg(unix)]
    #[test]
    fn installing_a_package_is_two_steps_and_the_second_names_only_the_plugin_that_was_shown() {
        // Step one reads a package and shows it; it installs nothing.
        let stage = Command::StageInstall {
            source: SourceDir::new("/home/me/clipboard").unwrap(),
        };
        let json = serde_json::to_string(&stage).unwrap();
        assert_eq!(
            json,
            r#"{"command":"stage-install","source":"/home/me/clipboard"}"#
        );
        assert!(matches!(
            roundtrip(&stage),
            Command::StageInstall { source } if source.as_path() == std::path::Path::new("/home/me/clipboard")
        ));
        // Step two names a plugin and nothing else, so no path can be smuggled in after the look.
        let confirm = Command::ConfirmInstall {
            plugin: PluginId::new("clipboard").unwrap(),
            agreement: None,
            risk_acknowledged: false,
        };
        let json = serde_json::to_string(&confirm).unwrap();
        assert_eq!(
            json,
            r#"{"command":"confirm-install","plugin":"clipboard"}"#
        );
        assert_eq!(serde_json::to_string(&roundtrip(&confirm)).unwrap(), json);
        // The agreement the person accepted is named by its digest, and a command without one still reads: the owner decides.
        let accepted = Command::ConfirmInstall {
            plugin: PluginId::new("clipboard").unwrap(),
            agreement: Some("ab".repeat(32)),
            risk_acknowledged: true,
        };
        let accepted_json = serde_json::to_string(&accepted).unwrap();
        assert!(accepted_json.contains(r#""agreement":"abab"#));
        assert!(accepted_json.contains(r#""risk_acknowledged":true"#));
        assert_eq!(
            serde_json::to_string(&roundtrip(&accepted)).unwrap(),
            accepted_json
        );
        for hostile in [
            r#"{"command":"stage-install"}"#,
            r#"{"command":"stage-install","source":"/x","extra":1}"#,
            r#"{"command":"stage-install","source":""}"#,
            r#"{"command":"stage-install","source":"relative/dir"}"#,
            r#"{"command":"stage-install","source":"/"}"#,
            r#"{"command":"confirm-install","plugin":"clipboard","source":"/evil"}"#,
            r#"{"command":"confirm-install","plugin":"a/b"}"#,
            r#"{"command":"confirm-install"}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(hostile).is_err(),
                "{hostile}"
            );
        }
    }

    #[test]
    fn plugin_commands_carry_only_opaque_ids_and_never_a_plugin_name_in_the_protocol() {
        let plugin = PluginId::new("any-plugin").unwrap();
        let peer = PeerId("peer-1".into());
        let command = Command::InvokeAction {
            plugin: plugin.clone(),
            action: "share".into(),
            peer: Some(peer.clone()),
        };
        let json = serde_json::to_string(&command).unwrap();
        assert!(json.contains(r#""command":"invoke-action""#));
        assert!(
            matches!(roundtrip(&command), Command::InvokeAction { plugin: p, .. } if p == plugin)
        );
    }

    #[test]
    fn the_picture_commands_have_one_exact_form_and_carry_nothing_else() {
        use crate::profile::Picture;
        let picture = Picture::new(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]).unwrap();
        let set = Command::SetProfilePicture {
            picture: picture.clone(),
        };
        assert_eq!(
            serde_json::to_string(&set).unwrap(),
            r#"{"command":"set-profile-picture","picture":{"width":2,"height":1,"rgba":"010203ff040506ff"}}"#
        );
        assert!(
            matches!(roundtrip(&set), Command::SetProfilePicture { picture: p } if p == picture)
        );

        assert_eq!(
            serde_json::to_string(&Command::ClearProfilePicture {}).unwrap(),
            r#"{"command":"clear-profile-picture"}"#
        );
        assert!(matches!(
            roundtrip(&Command::ClearProfilePicture {}),
            Command::ClearProfilePicture {}
        ));

        let digest = picture.digest();
        let fetch = Command::FetchPicture { digest };
        assert_eq!(
            serde_json::to_string(&fetch).unwrap(),
            format!(r#"{{"command":"fetch-picture","digest":"{digest}"}}"#)
        );
        assert!(matches!(roundtrip(&fetch), Command::FetchPicture { digest: d } if d == digest));

        for bad in [
            r#"{"command":"clear-profile-picture","x":1}"#,
            r#"{"command":"set-profile-picture"}"#,
            r#"{"command":"set-profile-picture","picture":{"width":2,"height":1,"rgba":"00"}}"#,
            r#"{"command":"set-profile-picture","picture":{"width":2,"height":1,"rgba":"010203ff040506ff"},"x":1}"#,
            r#"{"command":"fetch-picture","digest":"00"}"#,
            r#"{"command":"fetch-picture"}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(bad).is_err(),
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn a_picture_over_the_limit_cannot_arrive_as_a_command() {
        use crate::profile::MAX_EDGE;
        let rgba = "00".repeat((MAX_EDGE as usize + 1) * 4);
        let json = format!(
            r#"{{"command":"set-profile-picture","picture":{{"width":{},"height":1,"rgba":"{rgba}"}}}}"#,
            MAX_EDGE + 1
        );
        assert!(serde_json::from_str::<Command>(&json).is_err());
    }

    #[test]
    fn settings_patch_keeps_its_existing_wire_shape_and_unknown_fields_are_refused() {
        let wire = r#"{"command":"patch-settings","patch":{"background":true}}"#;
        let command: Command = serde_json::from_str(wire).unwrap();
        let Command::PatchSettings { patch } = &command else {
            panic!("a settings patch");
        };
        let current = Settings::default();
        let changed = patch.apply(&current).unwrap();
        assert!(changed.background);
        assert_eq!(
            Settings {
                background: false,
                ..changed
            },
            current
        );
        assert_eq!(serde_json::to_string(&command).unwrap(), wire);
        assert!(
            serde_json::from_str::<Command>(
                r#"{"command":"patch-settings","patch":{"background":true,"unknown":1}}"#
            )
            .is_err()
        );
    }

    fn empty_snapshot() -> Snapshot {
        Snapshot {
            settings: Settings::default(),
            peers: vec![],
            grants: Default::default(),
            plugins: vec![],
            staged: vec![],
            store: Default::default(),
            update: Default::default(),
            actions: vec![],
            transfers: vec![],
            pairing: vec![],
            extension_prompts: vec![],
            notifications: vec![],
            opened: None,
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
            kept_layers: Vec::new(),
            nearby: Default::default(),
            languages: Default::default(),
            diagnostics: Default::default(),
        }
    }

    #[test]
    fn relay_diagnostics_are_optional_for_old_owners_and_round_trip_live_health() {
        use crate::relay::{RelayDiagnostics, RelayHealth};
        let mut snapshot = empty_snapshot();
        let mut legacy = serde_json::to_value(&snapshot).unwrap();
        legacy.as_object_mut().unwrap().remove("relay");
        assert!(
            serde_json::from_value::<Snapshot>(legacy)
                .unwrap()
                .relay
                .is_none()
        );
        snapshot.relay = Some(RelayDiagnostics {
            active_target: Some(1),
            health: RelayHealth::Refused,
        });
        let decoded: Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        assert_eq!(decoded.relay, snapshot.relay);
    }

    #[test]
    fn a_snapshot_carries_digests_and_never_pixels_and_one_from_before_still_reads() {
        use crate::profile::Picture;
        let picture = Picture::new(1, 1, vec![9, 9, 9, 255]).unwrap();
        let mut snapshot = empty_snapshot();
        snapshot.profile_picture = Some(picture.digest());
        snapshot
            .peer_pictures
            .insert(PeerId("peer".into()), picture.digest());
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains(&picture.digest().to_string()));
        assert!(
            !json.contains("rgba"),
            "pixels travelled in a snapshot: {json}"
        );

        // A snapshot from an owner that knew nothing of pictures has neither field and still reads.
        let mut older = serde_json::to_value(empty_snapshot()).unwrap();
        older.as_object_mut().unwrap().remove("profile_picture");
        older.as_object_mut().unwrap().remove("peer_pictures");
        let read: Snapshot = serde_json::from_value(older).expect("an older snapshot still reads");
        assert!(read.profile_picture.is_none() && read.peer_pictures.is_empty());
    }

    #[test]
    fn what_extensions_keep_for_drawings_is_told_by_stamp_and_fetched_and_never_carried() {
        let plugin = PluginId::new("map").unwrap();
        let index = Command::LayerIndex {
            plugin: plugin.clone(),
        };
        assert_eq!(
            serde_json::to_string(&index).unwrap(),
            r#"{"command":"layer-index","plugin":"map"}"#
        );
        let fetch = Command::FetchLayers {
            plugin: plugin.clone(),
            ids: vec!["t/1".into(), "t/2".into()],
        };
        assert_eq!(
            serde_json::to_string(&fetch).unwrap(),
            r#"{"command":"fetch-layers","plugin":"map","ids":["t/1","t/2"]}"#
        );
        assert!(
            matches!(roundtrip(&fetch), Command::FetchLayers { ids, .. } if ids == ["t/1", "t/2"])
        );
        for bad in [
            r#"{"command":"layer-index"}"#,
            r#"{"command":"layer-index","plugin":"map","x":1}"#,
            r#"{"command":"fetch-layers","plugin":"map"}"#,
            r#"{"command":"fetch-layers","plugin":"map","ids":"t"}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(bad).is_err(),
                "{bad} was accepted"
            );
        }
        // The snapshot says only that something changed, by a stamp.
        let mut snapshot = empty_snapshot();
        snapshot.kept_layers = vec![KeptLayers { plugin, stamp: 77 }];
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(
            json.contains(r#""kept_layers":[{"plugin":"map","stamp":77}]"#),
            "{json}"
        );
        // One from an owner that knew nothing of layers has no such field and still reads, as keeping none.
        let mut older = serde_json::to_value(empty_snapshot()).unwrap();
        older.as_object_mut().unwrap().remove("kept_layers");
        let read: Snapshot = serde_json::from_value(older).expect("an older snapshot still reads");
        assert!(read.kept_layers.is_empty());
    }

    #[test]
    fn a_link_test_is_told_in_the_snapshot_as_one_of_four_words_and_one_from_before_still_reads() {
        // One closed word each: what a plugin or another window reads is exactly these.
        for (test, word) in [
            (LinkTest::Trying, "trying"),
            (LinkTest::Paired, "paired"),
            (LinkTest::NeedsPairing, "needs-pairing"),
            (LinkTest::Unreachable, "unreachable"),
        ] {
            assert_eq!(serde_json::to_string(&test).unwrap(), format!("\"{word}\""));
            assert_eq!(
                serde_json::from_str::<LinkTest>(&format!("\"{word}\"")).unwrap(),
                test
            );
        }
        assert!(
            serde_json::from_str::<LinkTest>("\"ok\"").is_err(),
            "a word that is not one of the four was accepted"
        );
        // It is told by device, and an owner from before had none: the snapshot reads as having tested nothing.
        let mut snapshot = empty_snapshot();
        snapshot
            .link_tests
            .insert(PeerId("p".into()), LinkTest::NeedsPairing);
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            json["link_tests"],
            serde_json::json!({ "p": "needs-pairing" })
        );
        let mut older = json.clone();
        older.as_object_mut().unwrap().remove("link_tests");
        assert!(
            serde_json::from_value::<Snapshot>(older)
                .unwrap()
                .link_tests
                .is_empty()
        );
    }

    #[test]
    fn the_commands_that_test_a_link_name_a_device_and_nothing_rides_along() {
        let test: Command = serde_json::from_str(r#"{"command":"test-link","peer":"p"}"#).unwrap();
        assert!(matches!(&test, Command::TestLink { peer } if peer.0 == "p"));
        let clear: Command =
            serde_json::from_str(r#"{"command":"clear-link-test","peer":"p"}"#).unwrap();
        assert!(matches!(&clear, Command::ClearLinkTest { peer } if peer.0 == "p"));
        for bad in [
            r#"{"command":"test-link"}"#,
            r#"{"command":"test-link","peer":"p","address":"10.0.0.1"}"#,
            r#"{"command":"clear-link-test","peer":5}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(bad).is_err(),
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn hostile_plugin_state_is_rejected_when_decoded_from_the_wire_or_disk() {
        let command = |body: &str| format!(r#"{{"command":"set-grants","peer":"p",{body}}}"#);
        for hostile in [
            r#""plugin":"a/b","grants":[]"#,
            r#""plugin":"ok","grants":[{"owner":"host","name":"bad name"}]"#,
            r#""plugin":"ok","grants":[{"owner":"host","name":"é"}]"#,
            // A bad OWNER is as hostile as a bad name.
            r#""plugin":"ok","grants":[{"owner":"a b","name":"x"}]"#,
            r#""plugin":"ok","grants":[{"owner":"","name":"x"}]"#,
            // The old shape, a bare name with no owner, says nothing about whose it is.
            r#""plugin":"ok","grants":["a.b"]"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(&command(hostile)).is_err(),
                "{hostile}"
            );
        }
        assert!(
            serde_json::from_str::<Command>(&command(
                r#""plugin":"ok","grants":[{"owner":"host","name":"a.b"}]"#
            ))
            .is_ok()
        );
        // A setting id that is not a valid identifier cannot enter stored values.
        assert!(serde_json::from_str::<crate::plugins::Values>(r#"{"bad key":true}"#).is_err());
        assert!(serde_json::from_str::<crate::plugins::Values>(r#"{"good.key":true}"#).is_ok());
        for name in ["shutdown", "create-invitation"] {
            let exact = format!(r#"{{"command":"{name}"}}"#);
            let extra = format!(r#"{{"command":"{name}","extra":1}}"#);
            assert!(serde_json::from_str::<Command>(&exact).is_ok(), "{name}");
            assert!(serde_json::from_str::<Command>(&extra).is_err(), "{name}");
        }
    }

    #[test]
    fn grant_and_value_collections_are_bounded() {
        use peersus_api::permission::Permission;
        let permissions =
            |count: usize| (0..count).map(|i| Permission::new("host", format!("c{i}")).unwrap());
        assert!(crate::plugins::Grants::new(permissions(crate::plugins::MAX_GRANTS)).is_ok());
        assert!(crate::plugins::Grants::new(permissions(crate::plugins::MAX_GRANTS + 1)).is_err());
        let json = format!(
            "{{{}}}",
            (0..=crate::plugins::MAX_VALUES)
                .map(|i| format!("\"k{i}\":true"))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(serde_json::from_str::<crate::plugins::Values>(&json).is_err());
    }

    #[test]
    fn a_peer_without_grants_has_none_by_default() {
        let json = r#"{"id":"p","name":"n","paired_at":1,"last_seen":null,"online":false}"#;
        let peer: Peer = serde_json::from_str(json).unwrap();
        assert!(peer.grants.is_empty() && peer.plugin_values.is_empty());
    }

    #[test]
    fn a_plugin_record_without_health_is_never_reported_as_running() {
        // A record that says nothing about health must decode, and it must not claim the plugin is running, because
        // nothing says that it is.
        let json = r#"{"id":"p","standing":"ready","manifest":{"manifest_version":1,"execution":{"kind":"process"},"capabilities":{},"imports":[],"exports":[]}}"#;
        let info: PluginInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.status, PluginStatus::Stopped);
        assert_ne!(PluginStatus::default(), PluginStatus::Running);
    }

    #[test]
    fn a_plugin_that_does_not_say_what_its_folder_allows_is_not_read_at_all() {
        // There is no default standing, on purpose. A default would have to be one of the five, and any of them would be a
        // claim about a plugin nobody has looked at: "ready" says it may run, and the others say something has been found
        // wrong. So it is required, and a record without it is refused.
        let json = r#"{"id":"p","manifest":{"manifest_version":1,"execution":{"kind":"process"},"capabilities":{},"imports":[],"exports":[]}}"#;
        assert!(
            serde_json::from_str::<PluginInfo>(json).is_err(),
            "a plugin with no standing was read"
        );
    }

    #[test]
    fn standing_has_a_fixed_wire_vocabulary_and_rejects_anything_else() {
        for (standing, text) in [
            (Standing::Ready, "\"ready\""),
            (Standing::NeedsApproval, "\"needs-approval\""),
            (Standing::Changed, "\"changed\""),
            (Standing::Disabled, "\"disabled\""),
            (Standing::Broken, "\"broken\""),
        ] {
            assert_eq!(serde_json::to_string(&standing).unwrap(), text);
            assert_eq!(serde_json::from_str::<Standing>(text).unwrap(), standing);
        }
        // Nothing a plugin can print is a standing: a word that is not one of the five does not read.
        for bad in [
            "\"Ready\"",
            "\"running\"",
            "\"ok\"",
            "\"\"",
            "null",
            "0",
            "\"ready \"",
            "\"needs_approval\"",
        ] {
            assert!(
                serde_json::from_str::<Standing>(bad).is_err(),
                "{bad} was read as a standing"
            );
        }
    }

    #[test]
    fn plugin_status_has_a_fixed_wire_vocabulary_and_rejects_anything_else() {
        for (status, text) in [
            (PluginStatus::Stopped, "\"stopped\""),
            (PluginStatus::Starting, "\"starting\""),
            (PluginStatus::Running, "\"running\""),
            (PluginStatus::Restarting, "\"restarting\""),
            (PluginStatus::Failed, "\"failed\""),
        ] {
            assert_eq!(serde_json::to_string(&status).unwrap(), text);
            assert_eq!(serde_json::from_str::<PluginStatus>(text).unwrap(), status);
        }
        // Text a plugin could try to smuggle in is not a status.
        assert!(serde_json::from_str::<PluginStatus>("\"running\\nok\"").is_err());
        assert!(serde_json::from_str::<PluginStatus>("\"anything\"").is_err());
    }

    #[test]
    fn the_snapshot_carries_the_packages_waiting_to_be_confirmed_and_an_older_one_without_them_reads()
     {
        let json = serde_json::to_value(empty_snapshot()).unwrap();
        assert_eq!(json["staged"], serde_json::json!([]));
        // What an owner that predates the field sends: no `staged` at all.
        let mut older = json.clone();
        older.as_object_mut().unwrap().remove("staged");
        let read: Snapshot = serde_json::from_value(older).expect("an older snapshot still reads");
        assert!(read.staged.is_empty());
    }

    #[test]
    fn a_plugin_carries_what_it_can_be_granted_split_by_device_and_an_older_one_reads() {
        use peersus_api::permission::Permission;
        let info = PluginInfo {
            id: PluginId::new("syncer").unwrap(),
            manifest: peersus_api::manifest::Manifest::new(
                peersus_api::manifest::Execution::Process,
            ),
            standing: Standing::Ready,
            status: PluginStatus::Running,
            grantable: Grantable {
                per_device: vec![Permission::new("syncer", "send.use").unwrap()],
                outright: vec![Permission::new("host", "plugins.restart").unwrap()],
            },
            program: None,
            package: None,
            agreed: false,
            from_store: false,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["grantable"]["per_device"][0]["name"], "send.use");
        assert_eq!(json["grantable"]["outright"][0]["owner"], "host");
        // What an owner that predates the field sends: no `grantable` at all.
        let mut older = json.clone();
        older.as_object_mut().unwrap().remove("grantable");
        let read: PluginInfo = serde_json::from_value(older).expect("an older plugin still reads");
        assert!(read.grantable.per_device.is_empty() && read.grantable.outright.is_empty());
    }

    #[test]
    fn allowing_or_withdrawing_names_one_permission_and_nothing_may_ride_along() {
        use peersus_api::permission::Permission;
        let allow = Command::Allow {
            plugin: PluginId::new("syncer").unwrap(),
            peer: Some(PeerId("peer-1".into())),
            permission: Permission::new("syncer", "send.use").unwrap(),
            on: true,
        };
        let json = serde_json::to_string(&allow).unwrap();
        assert_eq!(
            json,
            r#"{"command":"allow","plugin":"syncer","peer":"peer-1","permission":{"owner":"syncer","name":"send.use"},"on":true}"#
        );
        assert_eq!(serde_json::to_string(&roundtrip(&allow)).unwrap(), json);
        // Outright, with no device: the peer is simply absent.
        let outright = Command::Allow {
            plugin: PluginId::new("syncer").unwrap(),
            peer: None,
            permission: Permission::new("host", "plugins.restart").unwrap(),
            on: false,
        };
        let written = serde_json::to_string(&outright).unwrap();
        assert_eq!(
            serde_json::to_string(&roundtrip(&outright)).unwrap(),
            written
        );
        for hostile in [
            r#"{"command":"allow","plugin":"syncer","permission":{"owner":"a","name":"b"}}"#,
            r#"{"command":"allow","plugin":"syncer","on":true}"#,
            r#"{"command":"allow","plugin":"syncer","permission":{"owner":"a","name":"b"},"on":true,"grants":[]}"#,
            r#"{"command":"allow","plugin":"a/b","permission":{"owner":"a","name":"b"},"on":true}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(hostile).is_err(),
                "{hostile}"
            );
        }
    }

    #[test]
    fn a_settings_update_is_written_exactly_as_it_was_before_its_settings_were_boxed() {
        // A window and an owner of different builds talk through this form, and a box must not show in it.
        let settings = Settings::default();
        let command = Command::UpdateSettings {
            settings: Box::new(settings.clone()),
            expected: Box::new(settings.clone()),
        };
        let written = serde_json::to_string(&command).unwrap();
        let by_hand = format!(
            r#"{{"command":"update-settings","settings":{},"expected":{}}}"#,
            serde_json::to_string(&settings).unwrap(),
            serde_json::to_string(&settings).unwrap()
        );
        assert_eq!(written, by_hand);
        assert_eq!(
            serde_json::to_string(&roundtrip(&command)).unwrap(),
            written
        );
    }

    #[test]
    fn the_commands_for_a_files_code_have_one_form_and_nothing_may_ride_along() {
        let files = Command::CreateFilesInvitation {
            paths: vec![PathBuf::from("/home/me/a.bin")],
        };
        let written = serde_json::to_string(&files).unwrap();
        assert_eq!(
            written,
            r#"{"command":"create-files-invitation","paths":["/home/me/a.bin"]}"#
        );
        assert_eq!(serde_json::to_string(&roundtrip(&files)).unwrap(), written);
        let held = Command::CreateHeldFilesInvitation {
            files: vec![(7, "a.bin".into())],
        };
        let written = serde_json::to_string(&held).unwrap();
        assert_eq!(
            written,
            r#"{"command":"create-held-files-invitation","files":[[7,"a.bin"]]}"#
        );
        assert_eq!(serde_json::to_string(&roundtrip(&held)).unwrap(), written);
        for hostile in [
            r#"{"command":"create-files-invitation","paths":["/a"],"peer":"p"}"#,
            r#"{"command":"create-files-invitation"}"#,
            r#"{"command":"create-held-files-invitation","files":[[1,"a"]],"x":1}"#,
            r#"{"command":"create-files-invitation","paths":"/a"}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(hostile).is_err(),
                "{hostile} was accepted"
            );
        }
    }

    #[test]
    fn clearing_the_history_has_one_form_and_nothing_may_ride_along() {
        let clear = Command::ClearHistory {};
        let written = serde_json::to_string(&clear).unwrap();
        assert_eq!(written, r#"{"command":"clear-history"}"#);
        assert_eq!(serde_json::to_string(&roundtrip(&clear)).unwrap(), written);
        // A transfer named in it would make it a cancel, which has its own command and its own rules.
        assert!(
            serde_json::from_str::<Command>(r#"{"command":"clear-history","transfer":"x"}"#)
                .is_err()
        );
    }

    #[test]
    fn an_older_owners_files_metadata_decodes_without_inventing_pending_source_work() {
        let old: FilesCode = serde_json::from_str(r#"{"generation":7,"live":false}"#).unwrap();
        assert!(
            !old.pending,
            "an older owner must not keep a background service alive for nonexistent source work"
        );
        let current = FilesCode {
            generation: 7,
            live: false,
            pending: true,
        };
        let recovered: FilesCode =
            serde_json::from_str(&serde_json::to_string(&current).unwrap()).unwrap();
        assert!(
            recovered.pending,
            "source work disappeared on the actual IPC representation"
        );
    }
}

#[cfg(test)]
mod context_boundary_tests {
    use super::*;
    #[test]
    fn desktop_selected_path_cannot_redeem_a_context_or_held_file() {
        let value = serde_json::json!({
            "command":"invoke-selected-path-action","plugin":"example",
            "action":"upload","path":"synthetic.txt"
        });
        let command: Command = serde_json::from_value(value.clone()).unwrap();
        assert!(
            matches!(command, Command::InvokeSelectedPathAction { path, .. } if path == std::path::Path::new("synthetic.txt"))
        );
        for (field, payload) in [
            ("context", serde_json::json!(42)),
            ("held", serde_json::json!([1, "private.txt"])),
        ] {
            let mut malicious = value.clone();
            malicious[field] = payload;
            assert!(serde_json::from_value::<Command>(malicious).is_err());
        }
    }

    #[test]
    fn selected_file_authority_cannot_arrive_from_external_ipc() {
        let command = Command::InvokeSelectedAction {
            plugin: PluginId::new("example").unwrap(),
            action: "upload".into(),
            path: None,
            held: Some((1, "synthetic.txt".into())),
        };
        assert!(serde_json::to_value(command).is_err());
        assert!(
            serde_json::from_value::<Command>(serde_json::json!({
                "command":"invoke-selected-action","plugin":"example","action":"upload",
                "path":null,"held":[1,"synthetic.txt"]
            }))
            .is_err()
        );
    }

    #[test]
    fn selected_input_context_is_internal_only_not_an_external_command() {
        let command = Command::InvokeActionWithContext {
            plugin: PluginId::new("example").unwrap(),
            action: "share".into(),
            peer: Some(PeerId("peer".into())),
            context: 42,
        };
        assert!(serde_json::to_value(command).is_err());
        assert!(
            serde_json::from_value::<Command>(serde_json::json!({
                "command":"invoke-action-with-context", "plugin":"example", "action":"share",
                "peer":"peer", "context":42
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<Command>(serde_json::json!({
                "command":"invoke-action", "plugin":"example", "action":"share",
                "peer":"peer", "context":42
            }))
            .is_err()
        );
    }
}
