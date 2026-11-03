//! Stand-in for platforms where plugin processes are not supported yet.
//!
//! It has the same surface as the real service so the shared application code compiles
//! everywhere, but it refuses to start, so a missing platform shows up as an explicit
//! error at the moment a plugin would have run, never as a plugin that silently does
//! nothing. The local socket and the job-object containment a Windows host needs are not
//! built; this is where they will replace it.
use peersus_plugin_api::{hub::Hub, manifest::Manifest};
use std::{collections::BTreeSet, io, path::PathBuf, time::Duration};
use tokio::sync::{mpsc, watch};

use crate::supervisor::Policy;

/// What the application can learn about a plugin's health.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Started,
    Connected,
    Failed { restart_in: Duration },
    GaveUp,
    Stopped,
}

/// Same type name as the real service has, so shared code builds everywhere. Nothing can
/// ever produce one here, because no plugin process can be started on this platform.
pub struct Accepted {
    pub plugin: String,
    pub right: crate::data::Right,
}

/// Everything needed to host one plugin.
pub struct Plugin {
    pub id: String,
    pub manifest: Manifest,
    pub grants: BTreeSet<String>,
    pub program: Option<PathBuf>,
    pub arguments: Vec<String>,
    pub directory: PathBuf,
    pub policy: Policy,
    /// Same field as the real service has, so shared code builds everywhere.
    pub handoff: Option<mpsc::Sender<Accepted>>,
}

/// A plugin service that can never be started on this platform.
pub struct Service {
    events: watch::Receiver<Event>,
}

impl Service {
    /// Always fails: plugin processes are not supported on this platform yet.
    pub fn start(_hub: Hub, _plugin: Plugin) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Plugins are not supported on this platform yet",
        ))
    }

    pub fn start_with_launcher(_hub: Hub, _plugin: Plugin, _launcher: std::sync::Arc<dyn crate::supervisor::Launcher>) -> io::Result<Self> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "Plugin sockets are not supported on this platform yet"))
    }

    pub fn events(&self) -> watch::Receiver<Event> {
        self.events.clone()
    }

    /// Same method as the real service has, so shared code builds everywhere. There is never
    /// a supervisor here to ask, so it always says so, which is what makes the application
    /// start the plugin from scratch and thereby report that it cannot be started.
    pub fn restart(&self) -> bool {
        false
    }

    pub async fn shutdown(self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn starting_is_an_explicit_unsupported_error_not_a_silent_success() {
        let plugin = Plugin {
            id: "p".into(),
            manifest: Manifest::default(),
            grants: BTreeSet::new(),
            program: Some(PathBuf::from("/nonexistent")),
            arguments: Vec::new(),
            directory: PathBuf::from("/nonexistent"),
            policy: Policy::default(),
            handoff: None,
        };
        let error = Service::start(Hub::default(), plugin)
            .err()
            .expect("must refuse to start");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }
}
