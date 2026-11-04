//! Asynchronous facilities offered by the platform to sandboxed extensions.
//!
//! Provider endpoints and upload protocols belong to extensions. A platform adapter
//! owns selection/consent UI, private file snapshots, origin-bound authorization and
//! opaque credential handles. None of these authorities is an extension-supplied path.
use peersus_core::plugins::PluginId;
use peersus_plugin_api::message::Outcome;
use std::{future::Future, pin::Pin};

mod snapshot;
pub use snapshot::{MAX_FILE_BYTES, MAX_PART_BYTES, Snapshot};

pub const FILES_SERVICE: &str = "peersus.files";
pub const HTTP_SERVICE: &str = "peersus.http";
pub const FILES_CAPABILITY: &str = "files.select";
pub const HTTP_CAPABILITY: &str = "network.http";
pub const FILES_DESCRIPTION: &str =
    "Ask you to select a file for this extension; no ambient filesystem access.";
pub const HTTP_DESCRIPTION: &str =
    "Request explicit destination approval for HTTPS requests; credentials remain in the host.";

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
];

/// Interactive host facilities use an explicit bounded deadline, not a relaxation
/// of the ordinary 30-second plugin-to-plugin broker limit.
pub(crate) fn method_timeouts(
    manifest: &peersus_plugin_api::manifest::Manifest,
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
            (
                ((*service).to_owned(), 1, (*method).to_owned()),
                std::time::Duration::from_secs(600),
            )
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
pub enum SelectedInput {
    Path(std::path::PathBuf),
    Held {
        registry: peersus_net::held::Held,
        number: u64,
        name: String,
    },
}

/// Supplied by the application platform, never installed from an extension package.
///
/// `dispatch` must not block: selection, consent and network work live in the returned
/// future. Dropping it must cancel pending UI and I/O without an automatic retry.
/// Returned handles must be caller-scoped; request authorization must bind the exact
/// origin and selected snapshot, deny redirects, and never return secret plaintext.
pub trait PlatformServices: Send + Sync {
    fn dispatch(&self, call: Call) -> Response;
    /// Bind one short-lived, single-use input to the caller's current generation.
    fn select(&self, _caller: &PluginId, _input: SelectedInput) -> Result<u64, &'static str> {
        Err("Selected input is unavailable on this platform")
    }
    /// Withdraw an input when action admission fails.
    fn discard_selection(&self, _caller: &PluginId, _context: u64) {}
    /// End a plugin generation, releasing all its snapshots and transient secrets.
    fn revoke(&self, caller: &PluginId);
}
