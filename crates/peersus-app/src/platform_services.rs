//! Asynchronous facilities offered by the platform to sandboxed extensions.
//!
//! Provider endpoints and upload protocols belong to extensions. A platform adapter
//! owns selection/consent UI, private file snapshots, origin-bound authorization and
//! opaque credential handles. None of these authorities is an extension-supplied path.
use peersus_api::message::Outcome;
use peersus_core::plugins::PluginId;
use std::{future::Future, pin::Pin};

pub mod file_http;

/// Staged inputs from different facilities must never redeem each other's IDs.
pub fn next_context() -> Result<u64, &'static str> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_update(
        std::sync::atomic::Ordering::Relaxed,
        std::sync::atomic::Ordering::Relaxed,
        |value| value.checked_add(1),
    )
    .map_err(|_| "Selected input identifiers exhausted")
}

mod snapshot;
pub use snapshot::{MAX_FILE_BYTES, MAX_PART_BYTES, Snapshot};

pub const FILES_SERVICE: &str = "peersus.files";
pub const HTTP_SERVICE: &str = "peersus.http";
pub const CLIPBOARD_SERVICE: &str = "peersus.clipboard";
pub const FILES_CAPABILITY: &str = "files.select";
pub const HTTP_CAPABILITY: &str = "network.http";
pub const CLIPBOARD_READ_CAPABILITY: &str = "clipboard.read";
pub const CLIPBOARD_WRITE_CAPABILITY: &str = "clipboard.write";
pub const CLIPBOARD_WATCH_CAPABILITY: &str = "clipboard.watch";
/// The event that says the clipboard changed. It never carries text: only `read` does.
pub const CLIPBOARD_CHANGED: &str = "changed";
pub const FILES_DESCRIPTION: &str =
    "Ask you to select a file for this extension; no ambient filesystem access.";
pub const HTTP_DESCRIPTION: &str =
    "Request explicit destination approval for HTTPS requests; credentials remain in the host.";
pub const CLIPBOARD_DESCRIPTION: &str = "Read or replace this device's clipboard text in bounded pieces, each under its own permission.";
pub const CLIPBOARD_READ_DESCRIPTION: &str = "Read this device's clipboard text in bounded pieces; the platform decides when it is readable and excludes sensitive contents.";
pub const CLIPBOARD_WRITE_DESCRIPTION: &str =
    "Replace this device's clipboard text with text the extension supplies in bounded pieces.";
pub const CLIPBOARD_WATCH_DESCRIPTION: &str = "Be told when this device's clipboard changes, without receiving its contents; reading still needs the read permission.";
/// Most text one clipboard handle may carry, in bytes.
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
/// Largest piece moved by one call, so control messages stay far below their frame limit.
pub const CLIPBOARD_CHUNK_BYTES: usize = 8 * 1024;
pub const INPUT_SERVICE: &str = "peersus.input";
pub const INPUT_CAPABILITY: &str = "input.inject";
pub const INPUT_DESCRIPTION: &str = "Move the pointer, press buttons and keys, and type text on this computer, as if from a mouse and keyboard plugged into it.";

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
];

/// Events a platform service publishes: (service, event, capability, description). Declared in one place
/// like `METHODS`, so the runtime derives exports and permissions without naming any service.
pub(crate) const EVENTS: &[(&str, &str, &str, &str)] = &[(
    CLIPBOARD_SERVICE,
    CLIPBOARD_CHANGED,
    CLIPBOARD_WATCH_CAPABILITY,
    "The clipboard changed; carries no text. Read it with `read`, which needs its own permission.",
)];

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

pub type ActionProgress = std::collections::HashMap<(PluginId, String), (u64, u64)>;

/// Supplied by the application platform, never installed from an extension package.
///
/// `dispatch` must not block: selection, consent and network work live in the returned
/// future. Dropping it must cancel pending UI and I/O without an automatic retry.
/// Returned handles must be caller-scoped; request authorization must bind the exact
/// origin and selected snapshot, deny redirects, and never return secret plaintext.
pub trait PlatformServices: Send + Sync {
    fn extension_prompts(
        &self,
    ) -> Option<std::sync::Arc<crate::extension_prompts::ExtensionPrompts>> {
        None
    }
    /// Host-observed file progress, keyed by the action that owns the selected input.
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
    /// Bind one short-lived, single-use input to the caller's current generation.
    fn select(&self, _caller: &PluginId, _input: SelectedInput) -> Result<u64, &'static str> {
        Err("Selected input is unavailable on this platform")
    }
    /// Withdraw an input when action admission fails.
    fn discard_selection(&self, _caller: &PluginId, _context: u64) {}
    /// A platform facility that can notice its own changes exposes one counter per service, raised on every change.
    /// The counter says only that something changed, never what: contents stay behind the guarded methods.
    fn changes(&self, _service: &str) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }
    /// End a plugin generation, releasing all its snapshots and transient secrets.
    fn revoke(&self, caller: &PluginId);
}
