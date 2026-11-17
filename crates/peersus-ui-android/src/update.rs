//! How this application is replaced on Android: the system installs one package, whole.
//!
//! The shared owner finds, verifies and pins the update; this is only the last step. Which way the
//! package reaches the system is the distribution's (see its `AppUpdate.java`): one replaces itself
//! without a question through a shell provider, the other is kept current by its store and says so.
use peersus_app::app_update::{Installer, Kind, Target, Verified};
use std::{io::Write, path::PathBuf};

/// The folder the verified package waits in while the system reads it.
pub struct AndroidInstaller(PathBuf);

impl AndroidInstaller {
    /// A folder of its own inside the application's private `directory`, emptied at every start: a
    /// replacement that succeeds ends the process before the package it was read from is removed.
    pub fn new(directory: &std::path::Path) -> Self {
        let folder = directory.join("update");
        let _ = std::fs::remove_dir_all(&folder);
        Self(folder)
    }
}

const UNAVAILABLE: &str = "The update could not be handed to Android";

impl Installer for AndroidInstaller {
    fn target(&self) -> Result<Target, &'static str> {
        match crate::bridge::update_managed() {
            Some(false) => Ok(Target {
                platform: "android",
                architecture: std::env::consts::ARCH,
                kind: Kind::Package,
            }),
            Some(true) => Err("This installation is updated by Google Play"),
            None => Err(UNAVAILABLE),
        }
    }

    fn ends_application(&self) -> bool {
        true
    }

    fn install(&self, update: Verified, reopen: bool) -> Result<(), &'static str> {
        self.target()?;
        if update.kind != Kind::Package {
            return Err("This kind of update cannot be applied to this installation");
        }
        // 0600 and removed when this returns; the system is given its bytes, never this path.
        std::fs::create_dir_all(&self.0).map_err(|_| UNAVAILABLE)?;
        let mut package = tempfile::NamedTempFile::new_in(&self.0).map_err(|_| UNAVAILABLE)?;
        package
            .write_all(&update.bytes)
            .and_then(|()| package.flush())
            .map_err(|_| UNAVAILABLE)?;
        drop(update);
        // A replacement that succeeds ends this process inside the call: every answer is a reason
        // the update is not in place, except the one the system gives just before it does so.
        match crate::bridge::install_update(package.path(), reopen) {
            Some(0) => Ok(()),
            Some(1) => Err("Start Stellar/Shizuku in shell mode to update without being asked"),
            Some(2) => Err("Allow Peersus in Stellar/Shizuku, then update again"),
            Some(4) => Err("Android refused the update"),
            Some(5) => Err("Android took too long to install the update"),
            _ => Err(UNAVAILABLE),
        }
    }
}
