//! Module payloads are arguments to a trusted helper, never executable fallbacks.
use crate::supervisor::{Bootstrap, HelperKind, Launch, Launcher, Lifecycle, Running};
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
    kind: HelperKind,
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
            kind: HelperKind::Other,
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
        launch.spawn_helper(&self.module, self.kind)
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
    window_mmap_default: bool,
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
            window_mmap_default: false,
        })
    }

    /// Only the desktop owner's window-launch handoff can mark the packaged allocator
    /// default as removable. An independent owner leaves user settings inherited.
    pub fn with_window_mmap_default(mut self, enabled: bool) -> Self {
        self.window_mmap_default = enabled;
        self
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

    fn helper(&self, manifest: &Manifest) -> std::io::Result<(&Path, HelperKind)> {
        match manifest.execution {
            Execution::Wasm {
                abi: EXECUTION_ABI_VERSION,
            } => {
                // A canvas may appear only after the guest answers or offers a page.
                // Its declared host import, not a static page shape, selects Skia; so does
                // declaring that it decodes pictures or sounds in its own process.
                let canvas = manifest.imports.iter().any(|import| {
                    import.provider == crate::hub::HOST
                        && import.version == 1
                        && (import.service == canvas::SERVICE
                            && import.methods.contains(canvas::OPEN)
                            || import.service == peersus_api::pictures::SERVICE
                                && import.methods.contains(peersus_api::pictures::DECODE)
                            || import.service == peersus_api::audio::SERVICE
                                && import.methods.contains(peersus_api::audio::DECODE))
                });
                Ok(if canvas {
                    (
                        &self.wasm_canvas,
                        HelperKind::CanvasWasm {
                            window_mmap_default: self.window_mmap_default,
                        },
                    )
                } else {
                    (
                        &self.wasm,
                        HelperKind::LightWasm {
                            window_mmap_default: self.window_mmap_default,
                        },
                    )
                })
            }
            Execution::NativeLibrary {
                abi: EXECUTION_ABI_VERSION,
            } => Ok((&self.native, HelperKind::Other)),
            _ => Err(std::io::Error::other("module executor is not supported")),
        }
    }
}

impl ModuleFactory for DesktopModules {
    fn launcher(&self, manifest: &Manifest, module: &Path) -> std::io::Result<Arc<dyn Launcher>> {
        let (helper, kind) = self.helper(manifest)?;
        let mut launcher = ModuleLauncher::new(helper.to_owned(), module.to_owned())?;
        launcher.kind = kind;
        Ok(Arc::new(launcher))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas_import(version: u32) -> peersus_api::manifest::Import {
        peersus_api::manifest::Import {
            provider: crate::hub::HOST.into(),
            service: canvas::SERVICE.into(),
            version,
            methods: [canvas::OPEN.into()].into(),
            events: Default::default(),
        }
    }

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
        assert_eq!(modules.helper(&manifest).unwrap().0, modules.wasm.as_path());
        manifest.imports.push(canvas_import(2));
        assert_eq!(modules.helper(&manifest).unwrap().0, modules.wasm.as_path());
        manifest.imports[0].version = 1;
        assert_eq!(
            modules.helper(&manifest).unwrap().0,
            modules.wasm_canvas.as_path()
        );
        manifest.execution = Execution::NativeLibrary {
            abi: EXECUTION_ABI_VERSION,
        };
        assert_eq!(
            modules.helper(&manifest).unwrap().0,
            modules.native.as_path()
        );
    }

    #[test]
    fn a_wasm_guest_that_decodes_pictures_runs_in_the_helper_with_the_decoder() {
        let modules = DesktopModules::beside(&std::env::current_exe().unwrap()).unwrap();
        let mut manifest = Manifest::new(Execution::Wasm {
            abi: EXECUTION_ABI_VERSION,
        });
        manifest.imports.push(peersus_api::manifest::Import {
            provider: crate::hub::HOST.into(),
            service: peersus_api::pictures::SERVICE.into(),
            version: 1,
            methods: [peersus_api::pictures::KEEP.into()].into(),
            events: Default::default(),
        });
        // Keeping pixels it made itself needs no decoder.
        assert_eq!(modules.helper(&manifest).unwrap().0, modules.wasm.as_path());
        manifest.imports[0]
            .methods
            .insert(peersus_api::pictures::DECODE.into());
        assert_eq!(
            modules.helper(&manifest).unwrap().0,
            modules.wasm_canvas.as_path()
        );
    }

    #[test]
    fn a_wasm_guest_that_decodes_sounds_runs_in_the_helper_with_the_decoder() {
        let modules = DesktopModules::beside(&std::env::current_exe().unwrap()).unwrap();
        let mut manifest = Manifest::new(Execution::Wasm {
            abi: EXECUTION_ABI_VERSION,
        });
        manifest.imports.push(peersus_api::manifest::Import {
            provider: crate::hub::HOST.into(),
            service: peersus_api::audio::SERVICE.into(),
            version: 1,
            methods: [peersus_api::audio::PLAY.into()].into(),
            events: Default::default(),
        });
        // Playing samples it made itself needs no decoder.
        assert_eq!(modules.helper(&manifest).unwrap().0, modules.wasm.as_path());
        manifest.imports[0]
            .methods
            .insert(peersus_api::audio::DECODE.into());
        assert_eq!(
            modules.helper(&manifest).unwrap().0,
            modules.wasm_canvas.as_path()
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn desktop_helpers_receive_their_own_allocator_policy() {
        use std::{os::unix::fs::PermissionsExt, process::Command, time::Duration};

        const PROBE: &str = "PEERSUS_TEST_ALLOCATOR_PROBE";
        const WINDOW_POLICY: &str = "PEERSUS_TEST_FROM_WINDOW";
        const THRESHOLD: &str = "MALLOC_MMAP_THRESHOLD_";
        const MARKER: &str = "PEERSUS_LAUNCH_MMAP_DEFAULT";
        const DEFAULT: &str = "4194304";
        const HELPERS: [&str; 3] = [
            "peersus-wasm-host",
            "peersus-wasm-canvas-host",
            "peersus-native-host",
        ];

        // Use a separate test process so each fixture gets a real inherited environment without
        // mutating this process's environment while other tests are running.
        if let Some(directory) = std::env::var_os(PROBE) {
            let directory = PathBuf::from(directory);
            let modules = DesktopModules::beside(&directory.join("peersus"))
                .unwrap()
                .with_window_mmap_default(std::env::var_os(WINDOW_POLICY).is_some());
            let wasm = Execution::Wasm {
                abi: EXECUTION_ABI_VERSION,
            };
            let native = Execution::NativeLibrary {
                abi: EXECUTION_ABI_VERSION,
            };
            let mut manifests = [wasm, wasm, native].map(Manifest::new);
            manifests[1].imports.push(canvas_import(1));
            for (name, manifest) in HELPERS.into_iter().zip(manifests) {
                let helper = directory.join(name);
                std::fs::write(
                    &helper,
                    "#!/bin/sh\nprintf '%s\\n' \"${MALLOC_MMAP_THRESHOLD_-unset}\" \
                     \"${PEERSUS_LAUNCH_MMAP_DEFAULT-unset}\" > \"$0.out\"\n",
                )
                .unwrap();
                std::fs::set_permissions(helper, std::fs::Permissions::from_mode(0o700)).unwrap();
                let launcher = modules
                    .launcher(&manifest, &directory.join("fixture"))
                    .unwrap();
                let mut child = launcher
                    .launch(Bootstrap {
                        socket: directory.join("control"),
                        data_socket: directory.join("data"),
                        token: String::new(),
                    })
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), child.wait())
                    .await
                    .unwrap();
            }
            return;
        }

        let exe = std::env::current_exe().unwrap();
        for (scenario, threshold, marker, window_policy, expected) in [
            ("direct owner", None, None, false, ["unset"; 3]),
            (
                "launcher default",
                Some(DEFAULT),
                Some("window-owner"),
                true,
                ["unset", "unset", DEFAULT],
            ),
            (
                "explicit same value",
                Some(DEFAULT),
                None,
                false,
                [DEFAULT; 3],
            ),
            (
                "independent owner with a stale matching marker",
                Some(DEFAULT),
                Some(DEFAULT),
                false,
                [DEFAULT; 3],
            ),
            (
                "independent owner with a stale non-default marker",
                Some("8388608"),
                Some("8388608"),
                false,
                ["8388608"; 3],
            ),
            (
                "overridden after launch",
                Some("8388608"),
                Some("window-owner"),
                true,
                ["8388608"; 3],
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut command = Command::new(&exe);
            command
                .args([
                    "--exact",
                    "module::tests::desktop_helpers_receive_their_own_allocator_policy",
                ])
                .env_clear()
                .env(PROBE, directory.path());
            if let Some(threshold) = threshold {
                command.env(THRESHOLD, threshold);
            }
            if let Some(marker) = marker {
                command.env(MARKER, marker);
            }
            if window_policy {
                command.env(WINDOW_POLICY, "1");
            }
            let result = command.output().unwrap();
            assert!(
                result.status.success(),
                "{scenario}: {}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr),
            );
            for (name, expected) in HELPERS.into_iter().zip(expected) {
                let recorded =
                    std::fs::read_to_string(directory.path().join(format!("{name}.out"))).unwrap();
                assert_eq!(
                    recorded,
                    format!("{expected}\nunset\n"),
                    "{scenario}: {name}"
                );
            }
        }
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
