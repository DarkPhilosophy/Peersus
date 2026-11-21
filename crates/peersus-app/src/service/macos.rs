//! The bundled LaunchAgent runs as the signed-in user. Service Management owns registration,
//! consent, and removal. This module never installs a daemon, writes a system plist or elevates.
use crate::instance::Instance;
use smappservice_rs::{AppService, ServiceManagementError, ServiceStatus, ServiceType};
use std::{path::Path, time::Duration};

const PLIST: &str = "io.github.DarkPhilosophy.Peersus.session.plist";

#[derive(Debug, Clone, Copy)]
pub struct Manager(());

#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub enabled: bool,
    pub requires_approval: bool,
}

impl Manager {
    pub fn detect() -> Result<Self, String> {
        if !objc2::available!(macos = 13.0) {
            return Err("Starting with your session requires macOS 13 or later.".into());
        }
        let path = std::env::current_exe().ok();
        if !path
            .as_deref()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .is_some_and(|contents| {
                contents.ends_with("Contents")
                    && contents.join("Library/LaunchAgents").join(PLIST).is_file()
            })
        {
            return Err("Open the installed application bundle to manage startup.".into());
        }
        Ok(Self(()))
    }

    fn agent(&self) -> AppService {
        AppService::new(ServiceType::Agent { plist_name: PLIST })
    }

    pub fn status(&self) -> Result<Status, String> {
        let status = self.agent().status();
        if status == ServiceStatus::NotFound {
            return Err("macOS could not find the bundled user service.".into());
        }
        Ok(Status {
            enabled: status == ServiceStatus::Enabled,
            requires_approval: status == ServiceStatus::RequiresApproval,
        })
    }

    pub fn enable(&self, _owner: &Path) -> Result<(), String> {
        // BundleProgram, not a path supplied by an extension or caller, selects the executable.
        match self.agent().register() {
            Ok(()) | Err(ServiceManagementError::AlreadyRegistered) => Ok(()),
            Err(ServiceManagementError::LaunchDeniedByUser) => {
                Err("macOS did not approve the user service.".into())
            }
            Err(ServiceManagementError::InvalidSignature) => {
                Err("macOS rejected the application's signature.".into())
            }
            Err(_) => Err("macOS could not register the user service.".into()),
        }
    }

    pub fn disable(&self) -> Result<(), String> {
        self.agent()
            .unregister()
            .map_err(|_| "macOS could not unregister the user service.".to_owned())
    }
}

/// A LaunchAgent can be bootstrapped as soon as it is registered. Do not interrupt a window's
/// existing owner. launchd terminates the waiting process on unregister or sign-out.
pub async fn acquire(directory: &Path) -> Result<Option<Instance>, String> {
    loop {
        if let Some(instance) = Instance::acquire(directory)
            .map_err(|_| "Cannot acquire the application's private directory")?
        {
            return Ok(Some(instance));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
