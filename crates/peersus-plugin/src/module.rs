//! Module payloads are arguments to a trusted helper, never executable fallbacks.
use crate::supervisor::{Bootstrap, Launch, Launcher, Lifecycle, Running};
use peersus_api::{
    canvas,
    manifest::{EXECUTION_ABI_VERSION, Execution, Manifest},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct ModuleLauncher {
    helper: PathBuf,
    module: PathBuf,
    reviewed_diagnostics: bool,
}

impl ModuleLauncher {
    /// The application chooses its own helper; an extension cannot supply a host program.
    pub fn new(helper: PathBuf, module: PathBuf) -> std::io::Result<Self> {
        if !helper.is_absolute() || !module.is_absolute() {
            return Err(std::io::Error::other(
                "module and helper paths must be absolute",
            ));
        }
        Ok(Self {
            helper,
            module,
            reviewed_diagnostics: false,
        })
    }

    pub fn spawn(&self, bootstrap: Bootstrap) -> std::io::Result<Running> {
        let launch = Launch {
            program: self.helper.clone(),
            arguments: Vec::new(),
            socket: bootstrap.socket,
            data_socket: bootstrap.data_socket,
            token: bootstrap.token,
        };
        if self.reviewed_diagnostics {
            launch.spawn_wasm_module(&self.module)
        } else {
            launch.spawn_module(&self.module)
        }
    }
}

impl Launcher for ModuleLauncher {
    fn launch(&self, bootstrap: Bootstrap) -> std::io::Result<Box<dyn Lifecycle>> {
        self.spawn(bootstrap)
            .map(|running| Box::new(running) as Box<dyn Lifecycle>)
    }
}

/// The platform supplies a fresh launcher for each installed module. Packages never name
/// the helper, so a downloaded manifest cannot turn an interpreter into an arbitrary exec.
pub trait ModuleFactory: Send + Sync {
    fn launcher(&self, manifest: &Manifest, module: &Path) -> std::io::Result<Arc<dyn Launcher>>;
}

pub struct DesktopModules {
    wasm: PathBuf,
    wasm_canvas: PathBuf,
    native: PathBuf,
}

impl DesktopModules {
    /// Helpers come only from the application's installation, never PATH or package files.
    pub fn beside(executable: &Path) -> std::io::Result<Self> {
        if !executable.is_absolute() {
            return Err(std::io::Error::other("application path must be absolute"));
        }
        let directory = executable
            .parent()
            .ok_or_else(|| std::io::Error::other("application directory unavailable"))?;
        Ok(Self {
            wasm: directory.join(format!("peersus-wasm-host{}", std::env::consts::EXE_SUFFIX)),
            wasm_canvas: directory.join(format!(
                "peersus-wasm-canvas-host{}",
                std::env::consts::EXE_SUFFIX
            )),
            native: directory.join(format!(
                "peersus-native-host{}",
                std::env::consts::EXE_SUFFIX
            )),
        })
    }

    /// The helpers that are not beside the application. An installation that lacks one cannot run
    /// the extensions that need it; the application says so once, where each would only fail.
    pub fn missing(&self) -> impl Iterator<Item = &Path> {
        [
            self.wasm.as_path(),
            self.wasm_canvas.as_path(),
            self.native.as_path(),
        ]
        .into_iter()
        .filter(|helper| !helper.is_file())
    }

    fn helper(&self, manifest: &Manifest) -> std::io::Result<&Path> {
        match manifest.execution {
            Execution::Wasm {
                abi: EXECUTION_ABI_VERSION,
            } => {
                // A canvas may appear only after the guest answers or offers a page.
                // Its declared host import, not a static page shape, selects Skia.
                let canvas = manifest.imports.iter().any(|import| {
                    import.provider == crate::hub::HOST
                        && import.service == canvas::SERVICE
                        && import.version == 1
                        && import.methods.contains(canvas::OPEN)
                });
                Ok(if canvas {
                    &self.wasm_canvas
                } else {
                    &self.wasm
                })
            }
            Execution::NativeLibrary {
                abi: EXECUTION_ABI_VERSION,
            } => Ok(&self.native),
            _ => Err(std::io::Error::other("module executor is not supported")),
        }
    }
}

impl ModuleFactory for DesktopModules {
    fn launcher(&self, manifest: &Manifest, module: &Path) -> std::io::Result<Arc<dyn Launcher>> {
        let helper = self.helper(manifest)?;
        let mut launcher = ModuleLauncher::new(helper.to_owned(), module.to_owned())?;
        launcher.reviewed_diagnostics = matches!(manifest.execution, Execution::Wasm { .. });
        Ok(Arc::new(launcher))
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
        for execution in [
            Execution::Process,
            Execution::Wasm { abi: 0 },
            Execution::NativeLibrary {
                abi: EXECUTION_ABI_VERSION + 1,
            },
        ] {
            let manifest = Manifest::new(execution);
            assert!(modules.launcher(&manifest, &application).is_err());
        }
        for execution in [
            Execution::Wasm {
                abi: EXECUTION_ABI_VERSION,
            },
            Execution::NativeLibrary {
                abi: EXECUTION_ABI_VERSION,
            },
        ] {
            let manifest = Manifest::new(execution);
            assert!(
                modules
                    .launcher(&manifest, Path::new("relative/payload"))
                    .is_err()
            );
            assert!(modules.launcher(&manifest, &application).is_ok());
        }
    }

    #[test]
    fn only_a_wasm_guest_importing_the_canvas_host_method_needs_the_renderer() {
        let modules = DesktopModules::beside(&std::env::current_exe().unwrap()).unwrap();
        let mut manifest = Manifest::new(Execution::Wasm {
            abi: EXECUTION_ABI_VERSION,
        });
        assert_eq!(modules.helper(&manifest).unwrap(), modules.wasm.as_path());
        manifest.imports.push(peersus_api::manifest::Import {
            provider: crate::hub::HOST.into(),
            service: canvas::SERVICE.into(),
            version: 2,
            methods: [canvas::OPEN.into()].into(),
            events: Default::default(),
        });
        assert_eq!(modules.helper(&manifest).unwrap(), modules.wasm.as_path());
        manifest.imports[0].version = 1;
        assert_eq!(
            modules.helper(&manifest).unwrap(),
            modules.wasm_canvas.as_path()
        );
        manifest.execution = Execution::NativeLibrary {
            abi: EXECUTION_ABI_VERSION,
        };
        assert_eq!(modules.helper(&manifest).unwrap(), modules.native.as_path());
    }

    #[test]
    fn a_helper_that_is_not_beside_the_application_is_named_and_one_that_is_there_is_not() {
        let directory = tempfile::tempdir().unwrap();
        let modules = DesktopModules::beside(&directory.path().join("application")).unwrap();
        assert_eq!(modules.missing().count(), 3);
        std::fs::write(&modules.wasm, b"").unwrap();
        assert_eq!(
            modules.missing().collect::<Vec<_>>(),
            [modules.wasm_canvas.as_path(), modules.native.as_path()]
        );
    }
}
