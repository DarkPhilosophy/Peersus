//! Commands and immutable snapshots shared by the CLI, every frontend and platform hosts.
//!
//! Nothing here names a concrete plugin. Plugin interaction is expressed with three
//! generic commands (grants, setting values, declared actions) keyed by an opaque
//! [`PluginId`], and plugins appear in the snapshot with the manifest they declared.
use crate::{
    Settings,
    config::SettingsPatch,
    plugins::{Grants, PluginId, Value},
    profile::{Digest, Picture},
};
use peersus_plugin_api::manifest::Manifest;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

/// A peer's authenticated public identity, never its display name or IP address.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerId(pub String);

/// Remembered device; presence is derived from live authenticated connections.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Peer {
    /// Stable authenticated identifier.
    pub id: PeerId,
    /// User-facing display name.
    pub name: String,
    /// Time the mutual pairing completed, in Unix seconds.
    pub paired_at: u64,
    /// Last successful authenticated communication.
    pub last_seen: Option<u64>,
    /// Current connection state, never inferred merely from stored history.
    pub online: bool,
    /// Capabilities the user granted each plugin for this device. Default-deny: a
    /// plugin without an entry may do nothing for this device.
    #[serde(default)]
    pub grants: BTreeMap<PluginId, Grants>,
    /// Per-device plugin setting values, by plugin id.
    #[serde(default)]
    pub plugin_values: BTreeMap<PluginId, crate::plugins::Values>,
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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

/// Human confirmation request, never the remote user's selection.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub per_device: Vec<peersus_plugin_api::permission::Permission>,
    /// Granted once, for no device in particular.
    #[serde(default)]
    pub outright: Vec<peersus_plugin_api::permission::Permission>,
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
    /// and never what decides who the plugin is (that is `id`). Absent for a bundled plugin and for a
    /// package that carries none; missing from an owner that predates it.
    #[serde(default)]
    pub package: Option<peersus_plugin_api::package::Package>,
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
        Self::new(path).ok_or_else(|| serde::de::Error::custom("a directory must be an absolute path"))
    }
}

/// A producer names either an existing paired connection or one files-code generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum FileDestination {
    Peer { peer: PeerId },
    Invitation { generation: u64 },
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
    /// Authorize one displayed number; never a remote suggested selection.
    ChoosePairing { request: String, number: u8 },
    /// Refuse a pending pairing request.
    RejectPairing { request: String },
    /// Remove a remembered device and close its active connections.
    ForgetPeer { peer: PeerId },
    /// Offer local files; a transfer never implies pairing.
    SendFiles { peer: PeerId, paths: Vec<PathBuf> },
    /// Offer files that the system handed over already open (on Android a picked file has no path, only a
    /// descriptor). Each is named by the number it was kept under, and by the name to offer it as. The owner takes
    /// each file once by its number; a number that is unknown or was already used is refused. Judged as
    /// [`Command::SendFiles`] is, and a transfer never implies pairing.
    SendHeld { peer: PeerId, files: Vec<(u64, String)> },
    /// Bounded producer handoff. QueueBusy leaves every source untouched so the caller can
    /// wait for an owner update and retry the same batch (at most sixteen files).
    QueueFiles { destination: FileDestination, paths: Vec<PathBuf> },
    /// The held-file counterpart of QueueFiles; QueueBusy never takes or releases a number.
    QueueHeld { destination: FileDestination, files: Vec<(u64, String)> },
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
    PatchSettings { patch: SettingsPatch },
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
        permission: peersus_plugin_api::permission::Permission,
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
    /// Run an action the plugin declared. `peer` is required exactly when the
    /// declaration says the action needs a device.
    InvokeAction {
        plugin: PluginId,
        action: String,
        peer: Option<PeerId>,
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
    /// Withdraw precisely one pending user action; already published external effects cannot be undone.
    CancelAction { plugin: PluginId, action: String, peer: Option<PeerId> },
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
    /// Install the package that was shown for `plugin`, and only that one. It starts with no
    /// permissions.
    ConfirmInstall { plugin: PluginId },
    /// Let go of the package that was shown for `plugin` and not confirmed. Nothing is installed,
    /// and nothing of it is kept.
    CancelInstall { plugin: PluginId },
    /// Fetch the maintainer-approved extension catalog and check its signature. Nothing is installed.
    StoreRefresh {},
    /// Download the approved package for `plugin` from the store, check that it is exactly what the
    /// maintainer approved, and install it. Choosing to install IS the confirmation: what the person
    /// pressed on showed the author, link and version. The plugin starts with nothing allowed.
    StoreInstall { plugin: PluginId },
    /// Make this the picture that stands for this device. It is told to every paired device that is
    /// online, which asks for the picture only if it does not already hold it.
    SetProfilePicture { picture: Picture },
    /// Stop having a profile picture.
    ClearProfilePicture {},
    /// Ask for the picture with this digest, as the owner holds it: this device's own, or one a
    /// paired device showed. The answer is the picture; a digest the owner does not hold is refused.
    FetchPicture { digest: Digest },
    /// Gracefully stop owned work.
    Shutdown {},
}

/// Latest bounded state; consuming it performs no network or disk work.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// Where each action a user started has got to, in the application's own words.
    #[serde(default)]
    pub actions: Vec<ActionStatus>,
    /// Recent and active transfers, bounded by the host.
    pub transfers: Vec<Transfer>,
    /// Unfinished consent ceremonies.
    pub pairing: Vec<PairingPrompt>,
    /// Sanitized errors to show near affected operations.
    pub errors: Vec<String>,
    /// The digest of this device's own profile picture, or none when it has none. The picture itself
    /// never travels in a snapshot: a window that does not hold that digest asks for it once, with
    /// `FetchPicture`, so a change to anything else never carries sixty kilobytes of pixels again.
    #[serde(default)]
    pub profile_picture: Option<Digest>,
    /// The digest of the picture each paired device last showed this one, by device. Only devices
    /// that showed one are here.
    #[serde(default)]
    pub peer_pictures: BTreeMap<PeerId, Digest>,
    /// The files code on show, if there is one: what a screen needs to draw it and nothing of the code itself. Absent in a snapshot
    /// from an older owner, and when no code is out.
    #[serde(default)]
    pub files_code: Option<FilesCode>,
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
/// text a plugin wrote has no place in what the application shows. Whatever a plugin or the
/// hub answered is reduced to one of these, and anything not recognised becomes `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionFailure {
    /// The plugin, or the device, has not been allowed to do this.
    NotAllowed,
    /// The plugin is not running, or went away before it answered.
    Unavailable,
    /// It did not answer in time.
    TimedOut,
    /// It could not take the request now.
    Busy,
    /// It failed for a reason that is not one of the above.
    Other,
}

/// Where one action a user started has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ActionState {
    /// Sent to the plugin and not answered yet.
    Running,
    /// The plugin answered that it did it.
    Done,
    /// Stopped by the user. An external operation already submitted may still have completed.
    Cancelled,
    /// It did not complete.
    Failed { reason: ActionFailure },
}

/// The latest state of one action, named only by ids the application already knows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionStatus {
    pub plugin: PluginId,
    pub action: String,
    /// The device it was for, if the action takes one.
    pub peer: Option<PeerId>,
    pub state: ActionState,
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
            ActionFailure::TimedOut,
            ActionFailure::Busy,
            ActionFailure::Other,
        ] {
            // No wildcard arm: this only compiles while the list above covers the type.
            match reason {
                ActionFailure::NotAllowed
                | ActionFailure::Unavailable
                | ActionFailure::TimedOut
                | ActionFailure::Busy
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
                    && word.len() <= 16
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
        let command = Command::CancelInstall { plugin: PluginId::new("clipboard").unwrap() };
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(json, r#"{"command":"cancel-install","plugin":"clipboard"}"#);
        assert_eq!(serde_json::to_string(&roundtrip(&command)).unwrap(), json);
        for hostile in [
            r#"{"command":"cancel-install"}"#,
            r#"{"command":"cancel-install","plugin":"clipboard","source":"/evil"}"#,
            r#"{"command":"cancel-install","plugin":"a/b"}"#,
        ] {
            assert!(serde_json::from_str::<Command>(hostile).is_err(), "{hostile}");
        }
    }

    #[test]
    fn store_commands_name_only_a_plugin_and_nothing_may_ride_along() {
        let refresh = Command::StoreRefresh {};
        assert_eq!(serde_json::to_string(&refresh).unwrap(), r#"{"command":"store-refresh"}"#);
        let install = Command::StoreInstall { plugin: PluginId::new("echo").unwrap() };
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
            assert!(serde_json::from_str::<Command>(hostile).is_err(), "{hostile}");
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
                installed_version: Some("0.9.0".into()),
            }],
            error: None,
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains(r#""status":"ready""#), "{json}");
        assert!(json.contains(r#""installed_version":"0.9.0""#), "{json}");
        assert_eq!(serde_json::from_str::<StoreState>(&json).unwrap(), state);
        assert_eq!(serde_json::to_string(&StoreStatus::NotConfigured).unwrap(), r#""not-configured""#);
        // A listing from an owner that did not report versions reads as not installed.
        let older = r#"{"status":"ready","listings":[{"plugin":"echo","version":"1.0.0","author":"A","link":"https://example.org/e"}]}"#;
        assert_eq!(serde_json::from_str::<StoreState>(older).unwrap().listings[0].installed_version, None);
    }

    #[test]
    fn removing_enabling_and_disabling_a_plugin_name_it_by_id_and_nothing_else_may_ride_along() {
        for (name, build) in [
            ("remove-package", Command::RemovePackage { plugin: PluginId::new("any-plugin").unwrap() }),
            ("enable-plugin", Command::EnablePlugin { plugin: PluginId::new("any-plugin").unwrap() }),
            ("disable-plugin", Command::DisablePlugin { plugin: PluginId::new("any-plugin").unwrap() }),
        ] {
            let json = serde_json::to_string(&build).unwrap();
            assert_eq!(json, format!(r#"{{"command":"{name}","plugin":"any-plugin"}}"#));
            assert_eq!(serde_json::to_string(&roundtrip(&build)).unwrap(), json);
            for hostile in [
                format!(r#"{{"command":"{name}","plugin":"a/b"}}"#),
                format!(r#"{{"command":"{name}","plugin":""}}"#),
                format!(r#"{{"command":"{name}"}}"#),
                format!(r#"{{"command":"{name}","plugin":"ok","extra":1}}"#),
            ] {
                assert!(serde_json::from_str::<Command>(&hostile).is_err(), "{hostile}");
            }
        }
    }

    #[test]
    fn installing_a_package_is_two_steps_and_the_second_names_only_the_plugin_that_was_shown() {
        // Step one reads a package and shows it; it installs nothing.
        let stage = Command::StageInstall { source: SourceDir::new("/home/me/clipboard").unwrap() };
        let json = serde_json::to_string(&stage).unwrap();
        assert_eq!(json, r#"{"command":"stage-install","source":"/home/me/clipboard"}"#);
        assert!(matches!(
            roundtrip(&stage),
            Command::StageInstall { source } if source.as_path() == std::path::Path::new("/home/me/clipboard")
        ));
        // Step two names a plugin and nothing else, so no path can be smuggled in after the look.
        let confirm = Command::ConfirmInstall { plugin: PluginId::new("clipboard").unwrap() };
        let json = serde_json::to_string(&confirm).unwrap();
        assert_eq!(json, r#"{"command":"confirm-install","plugin":"clipboard"}"#);
        assert_eq!(serde_json::to_string(&roundtrip(&confirm)).unwrap(), json);
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
            assert!(serde_json::from_str::<Command>(hostile).is_err(), "{hostile}");
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
        let set = Command::SetProfilePicture { picture: picture.clone() };
        assert_eq!(
            serde_json::to_string(&set).unwrap(),
            r#"{"command":"set-profile-picture","picture":{"width":2,"height":1,"rgba":"010203ff040506ff"}}"#
        );
        assert!(matches!(roundtrip(&set), Command::SetProfilePicture { picture: p } if p == picture));

        assert_eq!(serde_json::to_string(&Command::ClearProfilePicture {}).unwrap(), r#"{"command":"clear-profile-picture"}"#);
        assert!(matches!(roundtrip(&Command::ClearProfilePicture {}), Command::ClearProfilePicture {}));

        let digest = picture.digest();
        let fetch = Command::FetchPicture { digest };
        assert_eq!(serde_json::to_string(&fetch).unwrap(), format!(r#"{{"command":"fetch-picture","digest":"{digest}"}}"#));
        assert!(matches!(roundtrip(&fetch), Command::FetchPicture { digest: d } if d == digest));

        for bad in [
            r#"{"command":"clear-profile-picture","x":1}"#,
            r#"{"command":"set-profile-picture"}"#,
            r#"{"command":"set-profile-picture","picture":{"width":2,"height":1,"rgba":"00"}}"#,
            r#"{"command":"set-profile-picture","picture":{"width":2,"height":1,"rgba":"010203ff040506ff"},"x":1}"#,
            r#"{"command":"fetch-picture","digest":"00"}"#,
            r#"{"command":"fetch-picture"}"#,
        ] {
            assert!(serde_json::from_str::<Command>(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn a_picture_over_the_limit_cannot_arrive_as_a_command() {
        use crate::profile::MAX_EDGE;
        let rgba = "00".repeat((MAX_EDGE as usize + 1) * 4);
        let json = format!(r#"{{"command":"set-profile-picture","picture":{{"width":{},"height":1,"rgba":"{rgba}"}}}}"#, MAX_EDGE + 1);
        assert!(serde_json::from_str::<Command>(&json).is_err());
    }

    fn empty_snapshot() -> Snapshot {
        Snapshot {
            settings: Settings::default(),
            peers: vec![],
            grants: Default::default(),
            plugins: vec![],
            staged: vec![],
            store: Default::default(),
            actions: vec![],
            transfers: vec![],
            pairing: vec![],
            errors: vec![],
            profile_picture: None,
            peer_pictures: Default::default(),
            files_code: None,
        }
    }

    #[test]
    fn a_snapshot_carries_digests_and_never_pixels_and_one_from_before_still_reads() {
        use crate::profile::Picture;
        let picture = Picture::new(1, 1, vec![9, 9, 9, 255]).unwrap();
        let mut snapshot = empty_snapshot();
        snapshot.profile_picture = Some(picture.digest());
        snapshot.peer_pictures.insert(PeerId("peer".into()), picture.digest());
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains(&picture.digest().to_string()));
        assert!(!json.contains("rgba"), "pixels travelled in a snapshot: {json}");

        // A snapshot from an owner that knew nothing of pictures has neither field and still reads.
        let mut older = serde_json::to_value(empty_snapshot()).unwrap();
        older.as_object_mut().unwrap().remove("profile_picture");
        older.as_object_mut().unwrap().remove("peer_pictures");
        let read: Snapshot = serde_json::from_value(older).expect("an older snapshot still reads");
        assert!(read.profile_picture.is_none() && read.peer_pictures.is_empty());
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
        use peersus_plugin_api::permission::Permission;
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
        let json = r#"{"id":"p","standing":"ready","manifest":{"manifest_version":1,"capabilities":{},"imports":[],"exports":[]}}"#;
        let info: PluginInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.status, PluginStatus::Stopped);
        assert_ne!(PluginStatus::default(), PluginStatus::Running);
    }

    #[test]
    fn a_plugin_that_does_not_say_what_its_folder_allows_is_not_read_at_all() {
        // There is no default standing, on purpose. A default would have to be one of the five, and any of them would be a
        // claim about a plugin nobody has looked at: "ready" says it may run, and the others say something has been found
        // wrong. So it is required, and a record without it is refused.
        let json = r#"{"id":"p","manifest":{"manifest_version":1,"capabilities":{},"imports":[],"exports":[]}}"#;
        assert!(serde_json::from_str::<PluginInfo>(json).is_err(), "a plugin with no standing was read");
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
        for bad in ["\"Ready\"", "\"running\"", "\"ok\"", "\"\"", "null", "0", "\"ready \"", "\"needs_approval\""] {
            assert!(serde_json::from_str::<Standing>(bad).is_err(), "{bad} was read as a standing");
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
    fn the_snapshot_carries_the_packages_waiting_to_be_confirmed_and_an_older_one_without_them_reads() {
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
        use peersus_plugin_api::permission::Permission;
        let info = PluginInfo {
            id: PluginId::new("syncer").unwrap(),
            manifest: Default::default(),
            standing: Standing::Ready,
            status: PluginStatus::Running,
            grantable: Grantable {
                per_device: vec![Permission::new("syncer", "send.use").unwrap()],
                outright: vec![Permission::new("host", "plugins.restart").unwrap()],
            },
            program: None,
            package: None,
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
        use peersus_plugin_api::permission::Permission;
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
        assert_eq!(serde_json::to_string(&roundtrip(&outright)).unwrap(), written);
        for hostile in [
            r#"{"command":"allow","plugin":"syncer","permission":{"owner":"a","name":"b"}}"#,
            r#"{"command":"allow","plugin":"syncer","on":true}"#,
            r#"{"command":"allow","plugin":"syncer","permission":{"owner":"a","name":"b"},"on":true,"grants":[]}"#,
            r#"{"command":"allow","plugin":"a/b","permission":{"owner":"a","name":"b"},"on":true}"#,
        ] {
            assert!(serde_json::from_str::<Command>(hostile).is_err(), "{hostile}");
        }
    }

    #[test]
    fn a_settings_update_is_written_exactly_as_it_was_before_its_settings_were_boxed() {
        // A window and an owner of different builds talk through this form, and a box must not show in it.
        let settings = Settings::default();
        let command = Command::UpdateSettings { settings: Box::new(settings.clone()), expected: Box::new(settings.clone()) };
        let written = serde_json::to_string(&command).unwrap();
        let by_hand = format!(
            r#"{{"command":"update-settings","settings":{},"expected":{}}}"#,
            serde_json::to_string(&settings).unwrap(),
            serde_json::to_string(&settings).unwrap()
        );
        assert_eq!(written, by_hand);
        assert_eq!(serde_json::to_string(&roundtrip(&command)).unwrap(), written);
    }

    #[test]
    fn the_commands_for_a_files_code_have_one_form_and_nothing_may_ride_along() {
        let files = Command::CreateFilesInvitation { paths: vec![PathBuf::from("/home/me/a.bin")] };
        let written = serde_json::to_string(&files).unwrap();
        assert_eq!(written, r#"{"command":"create-files-invitation","paths":["/home/me/a.bin"]}"#);
        assert_eq!(serde_json::to_string(&roundtrip(&files)).unwrap(), written);
        let held = Command::CreateHeldFilesInvitation { files: vec![(7, "a.bin".into())] };
        let written = serde_json::to_string(&held).unwrap();
        assert_eq!(written, r#"{"command":"create-held-files-invitation","files":[[7,"a.bin"]]}"#);
        assert_eq!(serde_json::to_string(&roundtrip(&held)).unwrap(), written);
        for hostile in [
            r#"{"command":"create-files-invitation","paths":["/a"],"peer":"p"}"#,
            r#"{"command":"create-files-invitation"}"#,
            r#"{"command":"create-held-files-invitation","files":[[1,"a"]],"x":1}"#,
            r#"{"command":"create-files-invitation","paths":"/a"}"#,
        ] {
            assert!(serde_json::from_str::<Command>(hostile).is_err(), "{hostile} was accepted");
        }
    }

    #[test]
    fn clearing_the_history_has_one_form_and_nothing_may_ride_along() {
        let clear = Command::ClearHistory {};
        let written = serde_json::to_string(&clear).unwrap();
        assert_eq!(written, r#"{"command":"clear-history"}"#);
        assert_eq!(serde_json::to_string(&roundtrip(&clear)).unwrap(), written);
        // A transfer named in it would make it a cancel, which has its own command and its own rules.
        assert!(serde_json::from_str::<Command>(r#"{"command":"clear-history","transfer":"x"}"#).is_err());
    }

    #[test]
    fn an_older_owners_files_metadata_decodes_without_inventing_pending_source_work() {
        let old: FilesCode = serde_json::from_str(r#"{"generation":7,"live":false}"#).unwrap();
        assert!(!old.pending, "an older owner must not keep a background service alive for nonexistent source work");
        let current = FilesCode { generation: 7, live: false, pending: true };
        let recovered: FilesCode = serde_json::from_str(&serde_json::to_string(&current).unwrap()).unwrap();
        assert!(recovered.pending, "source work disappeared on the actual IPC representation");
    }
}

#[cfg(test)]
mod context_boundary_tests {
    use super::*;

    #[test]
    fn selected_input_context_is_internal_only_not_an_external_command() {
        let command = Command::InvokeActionWithContext {
            plugin: PluginId::new("example").unwrap(), action:"share".into(),
            peer:Some(PeerId("peer".into())), context:42,
        };
        assert!(serde_json::to_value(command).is_err());
        assert!(serde_json::from_value::<Command>(serde_json::json!({
            "command":"invoke-action-with-context", "plugin":"example", "action":"share",
            "peer":"peer", "context":42
        })).is_err());
        assert!(serde_json::from_value::<Command>(serde_json::json!({
            "command":"invoke-action", "plugin":"example", "action":"share",
            "peer":"peer", "context":42
        })).is_err());
    }
}
