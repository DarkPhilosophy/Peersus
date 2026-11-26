//! Asynchronous facilities offered by the platform to sandboxed extensions.
//!
//! Provider endpoints and upload protocols belong to extensions. A platform adapter
//! owns selection/consent UI, private file snapshots, origin-bound authorization and
//! opaque credential handles. None of these authorities is an extension-supplied path.
use peersus_api::message::Outcome;
use peersus_core::plugins::PluginId;
use std::{future::Future, pin::Pin};

pub mod file_http;
pub mod location;

/// Staged inputs from different facilities must never redeem each other's IDs.
pub fn next_context() -> Result<u64, &'static str> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_update(
        std::sync::atomic::Ordering::Relaxed,
        std::sync::atomic::Ordering::Relaxed,
        |value| value.checked_add(1),
    )
    // Not reachable in practice (2^64 selections); said to the person as any input that cannot be staged.
    .map_err(|_| "Selected input is unavailable")
}

mod snapshot;
pub use snapshot::{MAX_FILE_BYTES, MAX_PART_BYTES, OUTSIDE_LIMIT, Snapshot};

pub const FILES_SERVICE: &str = "peersus.files";

/// The kind of file `peersus.files.choose` asks the person for (`kind`): each platform opens the picker it has for that
/// kind (the photo picker for an image on Android, a dialog filtered to those files on a computer). It narrows what is
/// offered, never what is accepted: the file the person picks is the file the extension gets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileKind {
    #[default]
    Any,
    Image,
    Audio,
    Video,
}
impl FileKind {
    /// The `kind` argument; absent means any file, and an unknown word is refused.
    pub fn from_arg(value: Option<&serde_json::Value>) -> Result<Self, &'static str> {
        match value.map(|value| value.as_str()) {
            None => Ok(Self::Any),
            Some(Some("any")) => Ok(Self::Any),
            Some(Some("image")) => Ok(Self::Image),
            Some(Some("audio")) => Ok(Self::Audio),
            Some(Some("video")) => Ok(Self::Video),
            Some(_) => Err("Invalid file kind"),
        }
    }
    /// The MIME pattern a system picker filters by.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Any => "*/*",
            Self::Image => "image/*",
            Self::Audio => "audio/*",
            Self::Video => "video/*",
        }
    }
    /// Filename extensions a dialog without MIME filtering offers, with the filter's name; none for any file.
    pub fn extensions(self) -> Option<(&'static str, &'static [&'static str])> {
        match self {
            Self::Any => None,
            Self::Image => Some((
                "Images",
                &[
                    "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "avif", "bmp",
                ],
            )),
            Self::Audio => Some((
                "Audio",
                &[
                    "mp3", "m4a", "aac", "ogg", "oga", "opus", "wav", "flac", "amr", "3gp",
                ],
            )),
            Self::Video => Some(("Video", &["mp4", "m4v", "webm", "mkv", "mov", "3gp"])),
        }
    }
}
pub const HTTP_SERVICE: &str = "peersus.http";
pub const CLIPBOARD_SERVICE: &str = "peersus.clipboard";
pub const FILES_CAPABILITY: &str = "files.select";
pub const HTTP_CAPABILITY: &str = "network.http";
pub const PUBLIC_HTTP_CAPABILITY: &str = "network.public";
pub const PUBLIC_HTTP_DESCRIPTION: &str = "Download bounded public data, without cookies or credentials, from an HTTPS address you approve.";
pub const CLIPBOARD_READ_CAPABILITY: &str = "clipboard.read";
pub const CLIPBOARD_WRITE_CAPABILITY: &str = "clipboard.write";
pub const CLIPBOARD_WATCH_CAPABILITY: &str = "clipboard.watch";
/// The event that says the clipboard changed. It never carries text: only `read` does.
pub const CLIPBOARD_CHANGED: &str = "changed";
pub const FILES_DESCRIPTION: &str =
    "Ask you to select a file for this extension; no ambient filesystem access.";
pub const FILES_EXPORT_DESCRIPTION: &str = "Ask you where to save one file this extension already holds. It cannot choose a destination by itself.";
pub const HTTP_DESCRIPTION: &str =
    "Request explicit destination approval for HTTPS requests; credentials remain in the host.";
pub const CLIPBOARD_DESCRIPTION: &str = "Read or replace this device's clipboard text in bounded pieces, each under its own permission.";
pub const CLIPBOARD_READ_DESCRIPTION: &str = "Read this device's clipboard text in bounded pieces; the platform decides when it is readable and excludes sensitive contents.";
pub const CLIPBOARD_WRITE_DESCRIPTION: &str =
    "Replace this device's clipboard text with text the extension supplies in bounded pieces.";
pub const CLIPBOARD_WATCH_DESCRIPTION: &str = "Be told when this device's clipboard changes, without receiving its contents; reading still needs the read permission.";
/// Most text one clipboard handle may carry, in bytes.
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
/// Maximum decoded clipboard bytes per chunk or append. A read piece can be shorter to fit the JSON reply.
pub const CLIPBOARD_CHUNK_BYTES: usize = 8 * 1024;
pub const INPUT_SERVICE: &str = "peersus.input";
pub const INPUT_CAPABILITY: &str = "input.inject";
pub const INPUT_DESCRIPTION: &str = "Move the pointer, press buttons and keys, and type text on this computer, as if from a mouse and keyboard plugged into it.";
/// Notifications an extension asks the application to show a person, in the shape of the contract
/// (`peersus_api::notification`). Answered by the application's own list on every platform: no adapter is asked.
pub const NOTIFY_SERVICE: &str = "peersus.notify";
pub const NOTIFY_CAPABILITY: &str = "notify.show";
pub const NOTIFY_DESCRIPTION: &str = "Show you notifications under its name, with its own words and buttons, in the window and outside it.";
/// Where this device is (`location`): what the service is in the application's manifest, and what its permission lets an
/// extension see, in the words a person reads when granting it. They are here, not beside the service, because this file's
/// sentences are the ones checked to be in the catalogue a translator fills.
pub const LOCATION_DESCRIPTION: &str =
    "Where this device is now, as its system finds it, behind its own permission.";
pub const LOCATION_PERMISSION: &str = "Know where this device is now, as its system finds it. To find it the system may ask an online service of its own with the wireless networks it sees nearby; the extension receives only the position.";
/// The names of the input service's arguments, declared here once: the service that reads them and the runtime, which
/// writes the name tag before a call reaches it, both use these, and `docs/api/service-messages.md` describes them.
/// Desktop-only arguments stay internal to the desktop adapter.
pub mod input_arg {
    /// The batch of input to replay.
    pub const EVENTS: &str = "events";
    /// The device the input is replayed for, by the alias the calling extension was given.
    pub const OWNER: &str = "owner";
    /// Which pointer is moved; absent for the shared one.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub(crate) const POINTER: &str = "pointer";
    /// The value of `POINTER` that asks for a pointer of the device's own.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub(crate) const POINTER_OWN: &str = "own";
    /// Asks for the device's name beside its pointer; the application decides the words.
    pub(crate) const SHOW_NAME: &str = "show_name";
    /// The words beside the pointer. Only the application writes it: one an extension wrote is dropped.
    pub(crate) const LABEL: &str = "label";
}

pub(crate) const METHODS: &[(&str, &str, &str, &str)] = &[
    (
        FILES_SERVICE,
        "choose",
        FILES_CAPABILITY,
        "Open the system picker for one private file snapshot.",
    ),
    (
        FILES_SERVICE,
        "selected",
        FILES_CAPABILITY,
        "Redeem one caller-bound file staged on Send without opening a picker.",
    ),
    (
        FILES_SERVICE,
        "release",
        FILES_CAPABILITY,
        "Release a file selected by this caller.",
    ),
    (
        FILES_SERVICE,
        peersus_api::extension_data::STAGE_SELECTED,
        FILES_CAPABILITY,
        "Copy the caller's selected snapshot into a private storage stage.",
    ),
    (
        FILES_SERVICE,
        peersus_api::extension_data::SAVE,
        peersus_api::extension_data::EXPORT,
        "Ask the person to save one exact revision of a private object.",
    ),
    (
        HTTP_SERVICE,
        "authorize",
        HTTP_CAPABILITY,
        "Ask for one origin-bound file and credential authorization.",
    ),
    (
        HTTP_SERVICE,
        "request",
        HTTP_CAPABILITY,
        "Perform one bounded HTTPS request using caller-owned handles.",
    ),
    (
        HTTP_SERVICE,
        "present",
        HTTP_CAPABILITY,
        "Display a validated result link within this caller's approved HTTPS origin.",
    ),
    (
        HTTP_SERVICE,
        "release",
        HTTP_CAPABILITY,
        "Discard this caller's transient HTTPS authorization.",
    ),
    (
        HTTP_SERVICE,
        "configure",
        HTTP_CAPABILITY,
        "Manage an origin-bound encrypted credential in trusted platform settings.",
    ),
    (
        HTTP_SERVICE,
        "saved",
        HTTP_CAPABILITY,
        "Say whether a credential is saved for an origin, or report that the store could not confirm it.",
    ),
    (
        HTTP_SERVICE,
        "check",
        HTTP_CAPABILITY,
        "Ask an origin, with the saved credential, whether it accepts it; only the status code comes back.",
    ),
    (
        HTTP_SERVICE,
        "authorize-read",
        PUBLIC_HTTP_CAPABILITY,
        "Restore a public HTTPS path approved in Settings (legacy method name).",
    ),
    (
        HTTP_SERVICE,
        "restore-read",
        PUBLIC_HTTP_CAPABILITY,
        "Restore a previously approved public HTTPS path without showing a prompt.",
    ),
    (
        HTTP_SERVICE,
        "get-resource",
        PUBLIC_HTTP_CAPABILITY,
        "Download one bounded public resource from the approved path.",
    ),
    (
        HTTP_SERVICE,
        "read-resource",
        PUBLIC_HTTP_CAPABILITY,
        "Read a bounded piece of the downloaded data.",
    ),
    (
        HTTP_SERVICE,
        "release-read",
        PUBLIC_HTTP_CAPABILITY,
        "Release the caller's public HTTPS authorization and data.",
    ),
    (
        HTTP_SERVICE,
        "resource-cache",
        PUBLIC_HTTP_CAPABILITY,
        "Say how many downloaded public resources this device keeps for the caller, and their bytes.",
    ),
    (
        HTTP_SERVICE,
        "clear-resource-cache",
        PUBLIC_HTTP_CAPABILITY,
        "Remove every downloaded public resource this device keeps for the caller.",
    ),
    (
        CLIPBOARD_SERVICE,
        "read",
        CLIPBOARD_READ_CAPABILITY,
        "Capture the current clipboard text, if readable now, as a caller-owned handle.",
    ),
    (
        CLIPBOARD_SERVICE,
        "observe",
        CLIPBOARD_READ_CAPABILITY,
        "Capture the current clipboard text if readable and report whether it is a new copy; returns a caller-owned handle.",
    ),
    (
        CLIPBOARD_SERVICE,
        "selected",
        CLIPBOARD_READ_CAPABILITY,
        "Redeem one caller-bound text staged on Send; the clipboard itself is not read.",
    ),
    (
        CLIPBOARD_SERVICE,
        "chunk",
        CLIPBOARD_READ_CAPABILITY,
        "Read one bounded piece of a captured text.",
    ),
    (
        CLIPBOARD_SERVICE,
        "write",
        CLIPBOARD_WRITE_CAPABILITY,
        "Begin replacing the clipboard text with a declared size.",
    ),
    (
        CLIPBOARD_SERVICE,
        "append",
        CLIPBOARD_WRITE_CAPABILITY,
        "Add one bounded piece to a pending replacement.",
    ),
    (
        CLIPBOARD_SERVICE,
        "commit",
        CLIPBOARD_WRITE_CAPABILITY,
        "Replace the clipboard text with the completed piece sequence.",
    ),
    (
        CLIPBOARD_SERVICE,
        "abort",
        CLIPBOARD_WRITE_CAPABILITY,
        "Discard a pending write owned by this caller without reading or changing the clipboard.",
    ),
    (
        CLIPBOARD_SERVICE,
        "reset",
        CLIPBOARD_READ_CAPABILITY,
        "Forget earlier copies so only later copies are reported.",
    ),
    (
        CLIPBOARD_SERVICE,
        "release",
        CLIPBOARD_READ_CAPABILITY,
        "Discard this caller's pending clipboard handles; needs no more than reading.",
    ),
    (
        INPUT_SERVICE,
        "inspect",
        INPUT_CAPABILITY,
        "Report supported input modes without opening or replaying an input device.",
    ),
    (
        INPUT_SERVICE,
        "inject",
        INPUT_CAPABILITY,
        "Replay a batch of pointer and keyboard input on this computer.",
    ),
    (
        INPUT_SERVICE,
        "release",
        INPUT_CAPABILITY,
        "Let go of every button and key this extension is holding down on this computer.",
    ),
    (
        NOTIFY_SERVICE,
        "show",
        NOTIFY_CAPABILITY,
        "Show one notification; answers at once for one that passes, and with the person's answer for one that waits.",
    ),
    (
        NOTIFY_SERVICE,
        peersus_api::notification::OPENED,
        NOTIFY_CAPABILITY,
        "Take the words of this extension's notifications the person tapped, oldest first.",
    ),
    (
        location::SERVICE,
        location::CURRENT,
        location::CAPABILITY,
        "Where this device is now: `{latitude, longitude, accuracy, age}`, degrees, metres (null when the system does not say) and seconds since the system found it.",
    ),
];

/// Events a platform service publishes: (service, event, capability, description). Declared in one place
/// like `METHODS`, so the runtime derives exports and permissions without naming any service.
pub(crate) const EVENTS: &[(&str, &str, &str, &str)] = &[(
    CLIPBOARD_SERVICE,
    CLIPBOARD_CHANGED,
    CLIPBOARD_WATCH_CAPABILITY,
    "The clipboard changed; carries no text. Read it with `read`, which needs its own permission.",
)];

/// Events the application raises itself, to the one extension something happened to, and never again when grants, settings
/// or devices change: (service, event, capability, description), declared like `EVENTS` for the manifest and permissions.
pub(crate) const RAISED: &[(&str, &str, &str, &str)] = &[(
    NOTIFY_SERVICE,
    peersus_api::notification::OPENED,
    NOTIFY_CAPABILITY,
    "The person tapped one of this extension's notifications; carries nothing. Take its word with `opened`.",
)];

/// The services that say when they change, each once: what the runtime watches, and what says so again when grants,
/// settings or the devices change. Read from `EVENTS`, so no caller names a service.
pub(crate) fn watched() -> std::collections::BTreeSet<&'static str> {
    EVENTS.iter().map(|(service, ..)| *service).collect()
}

/// How long a platform method may take unless its service says otherwise: a person may be choosing a file or answering a
/// consent screen, so the ceiling is generous.
const DEFAULT_CEILING: std::time::Duration = std::time::Duration::from_secs(600);

/// A service that never waits for a person says so here, with how long its methods may take. Declared in one place like
/// `METHODS`, so no condition anywhere names a service. The clipboard waits only for the platform, which answers or
/// refuses within seconds, so a call that has taken ten has hung and should be ended rather than held for ten minutes.
/// Input is replayed at once or not at all: a batch that has taken two seconds has hung, and a
/// controller waiting longer would pile up motion that lands all together.
pub(crate) const CEILINGS: &[(&str, std::time::Duration)] = &[
    (CLIPBOARD_SERVICE, std::time::Duration::from_secs(10)),
    (INPUT_SERVICE, std::time::Duration::from_secs(2)),
    (location::SERVICE, location::CEILING),
];

/// Interactive host facilities use an explicit bounded deadline, not a relaxation
/// of the ordinary 30-second plugin-to-plugin broker limit.
pub(crate) fn method_timeouts(
    manifest: &peersus_api::manifest::Manifest,
) -> std::collections::BTreeMap<(String, u32, String), std::time::Duration> {
    METHODS
        .iter()
        .filter(|(service, method, _, _)| {
            manifest.exports.iter().any(|export| {
                export.service == *service
                    && export.version == 1
                    && export.methods.contains_key(*method)
            })
        })
        .map(|(service, method, _, _)| {
            let ceiling = CEILINGS
                .iter()
                .find(|(declared, _)| declared == service)
                .map_or(DEFAULT_CEILING, |(_, ceiling)| *ceiling);
            (((*service).to_owned(), 1, (*method).to_owned()), ceiling)
        })
        .collect()
}

/// Identity is stamped by the authenticated hub, never deserialized from guest args.
/// Deliberately not Debug: arguments can contain private filenames or response data.
pub struct Call {
    pub caller: PluginId,
    pub service: String,
    pub method: String,
    pub args: serde_json::Value,
}

pub type Response = Pin<Box<dyn Future<Output = Outcome> + Send + 'static>>;

/// An authenticated receive's right to replay input. The adapter holds the
/// gates only during the side effect, never across a wait or queued call.
#[derive(Clone)]
pub struct InputLease {
    active: InputGate,
    visit: Option<InputGate>,
}

type InputGate = std::sync::Arc<std::sync::Mutex<bool>>;

impl InputLease {
    pub(crate) fn new() -> Self {
        Self {
            active: InputGate::new(std::sync::Mutex::new(true)),
            visit: None,
        }
    }

    /// A copy can retire independently; closing the visit fences all its copies.
    pub(crate) fn for_copy(&self) -> Self {
        Self {
            visit: Some(self.active.clone()),
            ..Self::new()
        }
    }

    pub(crate) fn revoke(&self) {
        if let Ok(mut active) = self.active.lock() {
            *active = false;
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.apply(|| ()).is_some()
    }

    pub fn apply<T>(&self, replay: impl FnOnce() -> T) -> Option<T> {
        let visit = match &self.visit {
            Some(visit) => Some(visit.lock().ok()?),
            None => None,
        };
        if visit.as_deref().is_some_and(|active| !*active) {
            return None;
        }
        let active = self.active.lock().ok()?;
        active.then(replay)
    }
}

pub fn replay_input<T>(lease: Option<&InputLease>, replay: impl FnOnce() -> T) -> Option<T> {
    match lease {
        Some(lease) => lease.apply(replay),
        None => Some(replay()),
    }
}
/// Input registered only by the trusted owner, never decoded from guest arguments.
/// Held files stay with Send; the adapter obtains an independent reader on redemption.
/// Text is held by the owner too, bounded by `MAX_CLIPBOARD_BYTES`, and redeemed once.
pub enum SelectedInput {
    Path(std::path::PathBuf),
    Held {
        registry: peersus_net::held::Held,
        number: u64,
        name: String,
    },
    Text(String),
}

/// Host-issued selected-input context to its live HTTP payload counter.
pub type ActionProgress = std::collections::HashMap<u64, std::sync::Arc<file_http::FileProgress>>;

/// Supplied by the application platform, never installed from an extension package.
///
/// `dispatch` must not block: selection, consent and network work live in the returned
/// future. Dropping it must cancel pending UI and I/O without an automatic retry.
/// Returned handles must be caller-scoped; request authorization must bind the exact
/// origin and selected snapshot, deny redirects, and never return secret plaintext.
pub trait PlatformServices: Send + Sync {
    /// Where the HTTP implementation actually restores public read approvals on this platform.
    fn public_read_directory(&self) -> Option<&std::path::Path> {
        None
    }
    fn extension_prompts(
        &self,
    ) -> Option<std::sync::Arc<crate::extension_prompts::ExtensionPrompts>> {
        None
    }
    /// The application's list of notifications, when this platform's services show something through it: the engine then
    /// publishes and answers that same list.
    fn notifications(&self) -> Option<std::sync::Arc<crate::notifications::Notifications>> {
        None
    }
    /// Latest file counters. The watch wakes on membership and the first unsampled change.
    fn action_progress(&self) -> Option<tokio::sync::watch::Receiver<ActionProgress>> {
        None
    }
    fn select_for_action(
        &self,
        caller: &PluginId,
        input: SelectedInput,
        _action: &str,
    ) -> Result<u64, &'static str> {
        self.select(caller, input)
    }
    fn dispatch(&self, call: Call) -> Response;
    /// A stricter platform foreground sensor, when there is one; desktop uses window focus.
    fn extension_foreground(&self) -> Option<bool> {
        None
    }
    /// The immutable snapshot already chosen for this caller; not accessible to the guest.
    fn file_snapshot(&self, _caller: &PluginId, _file: &str) -> Option<std::sync::Arc<Snapshot>> {
        None
    }
    /// Open a destination chosen by the user. The guest receives no path or descriptor.
    fn save_extension_file(
        &self,
        _caller: PluginId,
        _name: String,
    ) -> file_http::UiResponse<std::fs::File> {
        Box::pin(async { Err("Saving extension files is not supported here") })
    }
    /// Whether the system lets the application use the microphone now, asking the person where the system asks (Android's
    /// runtime permission). A desktop has no such question: what `audio.record` allows is the whole answer there.
    fn microphone_allowed(&self) -> file_http::UiResponse<()> {
        Box::pin(async { Ok(()) })
    }
    /// Replay on provision of a live paired-session lease (None for device-independent input).
    fn dispatch_input(&self, call: Call, _lease: Option<InputLease>) -> Response {
        self.dispatch(call)
    }
    /// Let go only of this authenticated controller's held inputs.
    fn release_peer_input(&self, _caller: &PluginId, _owner: &str) {}
    /// Release resources guarded by exactly this removed standalone grant.
    fn revoke_permission(
        &self,
        caller: &PluginId,
        _permission: &peersus_api::permission::Permission,
    ) {
        self.revoke(caller);
    }
    /// Bind one short-lived, single-use input to the caller's current generation.
    fn select(&self, _caller: &PluginId, _input: SelectedInput) -> Result<u64, &'static str> {
        Err("Selected input is unavailable")
    }
    /// Withdraw an input when action admission fails.
    fn discard_selection(&self, _caller: &PluginId, _context: u64) {}
    /// A platform facility that can notice its own changes exposes one counter per service, raised on every change.
    /// The counter says only that something changed, never what: contents stay behind the guarded methods.
    fn changes(&self, _service: &str) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }
    /// What this platform's own interface says of the device where the files [`crate::system_readings`] reads are closed to
    /// an application (a phone's battery and network). `None` reads the files.
    fn readings(&self) -> Option<&dyn crate::system_readings::PlatformReadings> {
        None
    }
    /// End a plugin generation, releasing all its snapshots and transient secrets.
    fn revoke(&self, caller: &PluginId);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_services_that_say_when_they_change_are_those_of_the_table_each_once() {
        let watched = watched();
        assert!(!watched.is_empty());
        for (service, event, _, _) in EVENTS {
            assert!(
                watched.contains(service),
                "{service} {event} is not watched"
            );
        }
        // Every one of them is a service the application offers methods of.
        for service in watched {
            assert!(METHODS.iter().any(|(offered, ..)| *offered == service));
        }
    }

    #[test]
    fn a_file_kind_is_one_of_the_closed_words_and_absent_means_any() {
        let kind = |value: serde_json::Value| FileKind::from_arg(Some(&value));
        assert_eq!(FileKind::from_arg(None), Ok(FileKind::Any));
        assert_eq!(kind(serde_json::json!("image")), Ok(FileKind::Image));
        assert_eq!(
            kind(serde_json::json!("audio")).map(FileKind::mime),
            Ok("audio/*")
        );
        for refused in [
            serde_json::json!("pdf"),
            serde_json::json!("Image"),
            serde_json::json!(1),
        ] {
            assert!(kind(refused).is_err());
        }
        assert_eq!(FileKind::Any.extensions(), None);
    }
}
