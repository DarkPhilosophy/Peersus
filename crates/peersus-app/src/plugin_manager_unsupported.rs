//! The plugin store where plugins cannot run (every system but Unix; see
//! [`crate::discovery::check_directory`]). Nothing can be installed, so nothing is: every change is
//! refused in this application's own words, and what is listed is the truth, which is nothing.

use std::path::{Path, PathBuf};

/// Most entries one package may hold: the same bound as on Unix.
pub const MAX_FILES: usize = 512;
/// Most bytes all the files of one package may hold together: the same bound as on Unix.
pub const MAX_BYTES: u64 = 128 * 1024 * 1024;

const UNSUPPORTED: &str = "Plugins cannot be installed on this system";

/// Why something was not done, in words this application chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal(pub &'static str);

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

/// What an install did, in the same shape as on Unix. Never made here: nothing is installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    New(String),
    Unchanged(String),
}

/// Where an installed package stands, in the same shape as on Unix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    Ready,
    Disabled,
    Changed,
    NeedsApproval,
    Broken,
}

/// One installed package. There are none here.
pub struct Entry {
    pub id: String,
    pub standing: Standing,
}

/// A store that holds nothing and takes nothing.
pub struct Manager {
    store: PathBuf,
}

impl Manager {
    pub fn new(data: &Path) -> Self {
        Self {
            store: data.join("plugins"),
        }
    }

    pub fn store(&self) -> &Path {
        &self.store
    }

    /// Nothing is installed: nothing is found, and nothing is said to be broken.
    pub fn load(&self) -> crate::discovery::Discovered {
        crate::discovery::Discovered::default()
    }

    pub fn list(&self) -> Result<Vec<Entry>, Refusal> {
        Ok(Vec::new())
    }

    pub fn install(&self, _source: &Path) -> Result<Installed, Refusal> {
        Err(Refusal(UNSUPPORTED))
    }

    pub fn remove(&self, _id: &str) -> Result<(), Refusal> {
        Err(Refusal(UNSUPPORTED))
    }

    pub fn retire(&self, _id: &str) -> Result<PathBuf, Refusal> {
        Err(Refusal(UNSUPPORTED))
    }

    pub fn restore(&self, _id: &str, _retired: &Path) -> Result<(), Refusal> {
        Err(Refusal(UNSUPPORTED))
    }

    pub fn drop_retired(&self, _retired: &Path) {}

    pub fn set_disabled(&self, _id: &str, _disabled: bool) -> Result<(), Refusal> {
        Err(Refusal(UNSUPPORTED))
    }

    pub fn approve(&self, _id: &str, _shown: &str) -> Result<(), Refusal> {
        Err(Refusal(UNSUPPORTED))
    }
}

pub(crate) fn package_id(_source: &Path) -> Result<String, Refusal> {
    Err(Refusal(UNSUPPORTED))
}

pub fn package_digest(_source: &Path) -> Result<String, Refusal> {
    Err(Refusal(UNSUPPORTED))
}

/// The same rule as on Unix: an empty or failed measurement is never a match.
pub(crate) fn still_what_was_shown(shown: &str, now: Result<String, Refusal>) -> bool {
    !shown.is_empty() && now.is_ok_and(|now| now == shown)
}

/// Nothing was ever installed here, so nothing installed is known and nothing is forgotten.
pub fn forget_removed(
    _found: &crate::discovery::Discovered,
    _devices: &mut peersus_net::peers::Peers,
    _standalone: &mut peersus_net::grants::StandaloneGrants,
) -> std::io::Result<usize> {
    Ok(0)
}

/// Forget what one plugin was granted, on every device and outright: the same as on Unix.
pub fn forget_one(
    plugin: &peersus_core::plugins::PluginId,
    devices: &mut peersus_net::peers::Peers,
    standalone: &mut peersus_net::grants::StandaloneGrants,
) -> std::io::Result<usize> {
    Ok(devices.forget_plugin(plugin)? + standalone.forget_plugin(plugin)?)
}
