//! Module payloads are arguments to a trusted helper, never executable fallbacks.
use crate::supervisor::{Bootstrap, Launch, Launcher, Lifecycle, Running};
use peersus_plugin_api::manifest::{Execution, EXECUTION_ABI_VERSION};
use std::{path::{Path, PathBuf}, sync::Arc};

pub struct ModuleLauncher {
    helper: PathBuf,
    module: PathBuf,
}

impl ModuleLauncher {
    /// The application chooses its own helper; an extension cannot supply a host program.
    pub fn new(helper: PathBuf, module: PathBuf) -> std::io::Result<Self> {
        if !helper.is_absolute() || !module.is_absolute() {
            return Err(std::io::Error::other("module and helper paths must be absolute"));
        }
        Ok(Self { helper, module })
    }

    pub fn spawn(&self, bootstrap: Bootstrap) -> std::io::Result<Running> {
        Launch {
            program: self.helper.clone(),
            arguments: Vec::new(),
            socket: bootstrap.socket,
            data_socket: bootstrap.data_socket,
            token: bootstrap.token,
        }.spawn_module(&self.module)
    }
}

impl Launcher for ModuleLauncher {
    fn launch(&self, bootstrap: Bootstrap) -> std::io::Result<Box<dyn Lifecycle>> {
        self.spawn(bootstrap).map(|running| Box::new(running) as Box<dyn Lifecycle>)
    }
}

/// The platform supplies a fresh launcher for each installed module. Packages never name
/// the helper, so a downloaded manifest cannot turn an interpreter into an arbitrary exec.
pub trait ModuleFactory: Send + Sync {
    fn launcher(&self, execution: Execution, module: &Path) -> std::io::Result<Arc<dyn Launcher>>;
}

pub struct DesktopModules {
    wasm: PathBuf,
    native: PathBuf,
}

impl DesktopModules {
    /// Helpers come only from the application's installation, never PATH or package files.
    pub fn beside(executable: &Path) -> std::io::Result<Self> {
        if !executable.is_absolute() { return Err(std::io::Error::other("application path must be absolute")); }
        let directory = executable.parent().ok_or_else(|| std::io::Error::other("application directory unavailable"))?;
        Ok(Self {
            wasm: directory.join(format!("peersus-wasm-host{}", std::env::consts::EXE_SUFFIX)),
            native: directory.join(format!("peersus-native-host{}", std::env::consts::EXE_SUFFIX)),
        })
    }
}

impl ModuleFactory for DesktopModules {
    fn launcher(&self, execution: Execution, module: &Path) -> std::io::Result<Arc<dyn Launcher>> {
        let helper = match execution {
            Execution::Wasm { abi: EXECUTION_ABI_VERSION } => &self.wasm,
            Execution::NativeLibrary { abi: EXECUTION_ABI_VERSION } => &self.native,
            _ => return Err(std::io::Error::other("module executor is not supported")),
        };
        Ok(Arc::new(ModuleLauncher::new(helper.clone(), module.to_owned())?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_abis_and_processes_never_become_module_executions() {
        let application = std::env::current_exe().unwrap();
        let modules = DesktopModules::beside(&application).unwrap();
        assert!(DesktopModules::beside(Path::new("relative/application")).is_err());
        for execution in [Execution::Process, Execution::Wasm { abi: 0 }, Execution::NativeLibrary { abi: EXECUTION_ABI_VERSION + 1 }] {
            assert!(modules.launcher(execution, &application).is_err());
        }
        for execution in [Execution::Wasm { abi: EXECUTION_ABI_VERSION }, Execution::NativeLibrary { abi: EXECUTION_ABI_VERSION }] {
            assert!(modules.launcher(execution, Path::new("relative/payload")).is_err());
            assert!(modules.launcher(execution, &application).is_ok());
        }
    }
}
