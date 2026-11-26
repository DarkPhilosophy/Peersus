use std::{
    env,
    path::Path,
    process::{Command, ExitCode},
};
mod bench;
mod catalog;
mod judge;
mod languages;
mod manifest;
mod update;

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    run_with(program, args, &[])
}
/// [`run`] with more environment for this one command, so that a demand made of one test run does not reach the others.
fn run_with(program: &str, args: &[&str], environment: &[(&str, &str)]) -> Result<(), String> {
    eprintln!("+ {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .env("FORCE_SKIA_BUILD", "1")
        .envs(environment.iter().copied())
        .status()
        .map_err(|error| format!("cannot execute {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}
fn cargo(args: &[&str]) -> Result<(), String> {
    run("cargo", args)
}

fn identity_test_directory(target_directory: Option<std::ffi::OsString>) -> std::path::PathBuf {
    target_directory
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "target".into())
        .join("test-identity")
}
fn output_directory(profile: &str) -> std::path::PathBuf {
    let mut directory = env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "target".into());
    if let Some(target) = env::var_os("CARGO_BUILD_TARGET").filter(|target| !target.is_empty()) {
        directory.push(target);
    }
    directory.join(profile)
}

fn android_native(flavor: &str, extensions: bool, development: bool) -> Result<(), String> {
    let destination = format!("target/android/jniLibs/{flavor}");
    let mut args = vec![
        "ndk",
        // The long form: cargo-ndk 3 spells the short one `-p`, cargo-ndk 4 (the one CI installs) `-P`, and there `-p`
        // is Cargo's package.
        "--platform",
        "31",
        "-t",
        "arm64-v8a",
        "-t",
        "x86_64",
        "-o",
        &destination,
        "build",
        "--locked",
        "--release",
        "-p",
        "peersus-ui-android",
        "-p",
        "peersus-plugin-android",
    ];
    let mut features = Vec::new();
    if extensions {
        features.push("peersus-ui-android/native-extensions");
    }
    if development {
        features.push("peersus-ui-android/development-diagnostics");
        if env::var("PEERSUS_PRIVATE_STORE_BUILD").as_deref() == Ok("1") {
            features.push("peersus-ui-android/private-store");
        }
        features.push(if prerelease()? {
            "peersus-ui-android/prerelease-channel"
        } else {
            "peersus-ui-android/development-channel"
        });
    } else {
        features.push("peersus-ui-android/production-diagnostics");
    }
    let joined_features = features.join(",");
    if !features.is_empty() {
        args.extend(["--features", &joined_features]);
    }
    cargo(&args)?;
    if extensions {
        run(
            "python3",
            &["scripts/build-android-native.py", "--output", &destination],
        )?;
    }
    Ok(())
}

/// The interface with its development-only features on (the read-only demo and its headless
/// window), which no product build has and the workspace-wide steps therefore never compile.
const DEVELOPMENT_INTERFACE: &[&str] = &[
    "--locked",
    "-p",
    "peersus-ui",
    "--all-targets",
    "--features",
    "headless",
];

/// Whether a development build follows the prerelease channel: `PEERSUS_UPDATE_CHANNEL=pre`. Unset or `dev` is the
/// development channel; any other value is refused rather than built as something else. Which store the build reads
/// (`PEERSUS_PRIVATE_STORE_BUILD`) does not matter.
fn prerelease() -> Result<bool, String> {
    match env::var("PEERSUS_UPDATE_CHANNEL").as_deref() {
        Err(_) | Ok("" | "dev") => Ok(false),
        Ok("pre") => Ok(true),
        Ok(other) => Err(format!(
            "unknown PEERSUS_UPDATE_CHANNEL {other:?}: expected dev or pre"
        )),
    }
}

fn desktop_build_args(task: &str, private_store: bool, prerelease: bool) -> Vec<&'static str> {
    let mut args = vec![
        "build",
        "--locked",
        "-p",
        "peersus-ui",
        "-p",
        "peersus-app",
        "-p",
        "peersus-plugin-runtime",
        "-p",
        "peersus-plugin-canvas",
        "-p",
        "peersus-store-fetch",
        "-p",
        "peersus-diag",
    ];
    if task != "dev" {
        args.push("--release");
    }
    match task {
        "build" => args.extend(["--features", "peersus-app/production-diagnostics"]),
        "dev" | "dev-deploy" => args.extend([
            "--features",
            "peersus-app/development-diagnostics,peersus-ui/development-diagnostics,peersus-plugin-runtime/development-diagnostics,peersus-plugin-canvas/development-diagnostics",
        ]),
        _ => unreachable!(),
    }
    // Every development build follows an update channel, whichever store it reads: the optimized development build too,
    // so a development release and a development deploy verify their updates with the same key.
    if matches!(task, "dev" | "dev-deploy") {
        if private_store {
            args.extend([
                "--features",
                "peersus-ui/private-store,peersus-app/private-store,peersus-store-fetch/private-store",
            ]);
        }
        args.extend([
            "--features",
            if prerelease {
                "peersus-app/prerelease-channel"
            } else {
                "peersus-app/development-channel"
            },
        ]);
    }
    args
}

fn task(name: &str) -> Result<(), String> {
    match name {
        "fmt" => {
            cargo(&["fmt", "--all", "--", "--check"])?;
            cargo(&[
                "fmt",
                "--manifest-path",
                "templates/canvas-guest/Cargo.toml",
                "--",
                "--check",
            ])
        }
        "check" => {
            cargo(&[
                "check",
                "--locked",
                "--workspace",
                "--all-targets",
                "--exclude",
                "peersus-ui-android",
            ])?;
            cargo(&[&["check"], DEVELOPMENT_INTERFACE].concat())
        }
        "clippy" => {
            cargo(&[
                "clippy",
                "--locked",
                "--workspace",
                "--all-targets",
                "--exclude",
                "peersus-ui-android",
                "--",
                "-D",
                "warnings",
            ])?;
            cargo(
                &[
                    &["clippy"],
                    DEVELOPMENT_INTERFACE,
                    &["--", "-D", "warnings"],
                ]
                .concat(),
            )
        }
        "test" => {
            // The host gate exercises the generic SDK guest. Independent extension
            // sources and their acceptance gates live on the extension branch.
            bench::build_hello_guest()?;
            bench::build_canvas_guest()?;
            cargo(&[
                "test",
                "--locked",
                "--workspace",
                "--exclude",
                "peersus-ui-android",
            ])?;
            // The demo and its headless window: their own tests, and the screens opened,
            // pressed and drawn with no display; and the vendored toolkit's own tests of the
            // components it carries local changes to (`toolkit_`, see peersus-ui's lib.rs).
            // Library tests only: an integration test would make Cargo build the program with
            // the feature on, in the place where the ordinary development build is expected.
            cargo(&[
                "test",
                "--locked",
                "-p",
                "peersus-ui",
                "--features",
                "headless",
                "--lib",
                "--",
                "demo",
                "headless::",
                "canvas_view::",
                "toolkit_",
            ])?;
            // Integration tests build an executable with an ephemeral identity. Cargo would
            // otherwise replace the ordinary executable in the shared development directory.
            let isolated = identity_test_directory(env::var_os("CARGO_TARGET_DIR"));
            let isolated = isolated
                .to_str()
                .ok_or("test output directory is not valid UTF-8")?;
            cargo(&[
                "test",
                "--locked",
                "-p",
                "peersus-app",
                "--features",
                "test-plugin,test-identity",
                "--target-dir",
                isolated,
            ])
        }
        "audit" => cargo(&["audit", "--deny", "warnings"]),
        "metadata" => {
            run("python3", &["scripts/test-gen-docs.py"])?;
            run("python3", &["scripts/gen-docs.py", "--check"])?;
            run("python3", &["scripts/test-gen-architecture-map.py"])?;
            run("python3", &["scripts/package-metadata.py", "check"])?;
            // Modules of one crate that need each other: Cargo refuses that between crates and nothing does within one.
            run("python3", &["scripts/module-cycles.py"])?;
            run("python3", &["scripts/test-inspect-android.py"])?;
            run("python3", &["scripts/test-stage-linux.py"])?;
            run("python3", &["scripts/test-stage-macos.py"])?;
            run("python3", &["scripts/test-root-helpers.py"])?;
            run("python3", &["scripts/test-dev.py"])?;
            run("python3", &["scripts/test-deploy.py"])?;
            run("python3", &["scripts/test-sync-github.py"])?;
            run("python3", &["scripts/test-memory-gate.py"])?;
            run("python3", &["scripts/test-site-preview.py"])?;
            run("python3", &["scripts/test-catalog-fixtures.py"])?;
            run("python3", &["scripts/test-extension-source.py"])?;
            run("python3", &["scripts/test-first-party-extensions.py"])?;
            run("python3", &["scripts/test-extension-admission.py"])?;
            run("python3", &["scripts/test-prune-extension-releases.py"])?;
            run("python3", &["scripts/test-extension-impact.py"])?;
            run("python3", &["scripts/test-update-resource-impact.py"])?;
            run("python3", &["scripts/test-review-readiness.py"])?;
            run("python3", &["scripts/test-extension-ownership.py"])?;
            run("python3", &["scripts/test-prepare-extension-branch.py"])?;
            run("python3", &["scripts/test-package-native-extension.py"])?;
            run("python3", &["scripts/test-triage-pull-request.py"])?;
            run("python3", &["scripts/test-dependency-news.py"])?;
            run("python3", &["scripts/test-dependency-roll.py"])?;
            run("python3", &["scripts/test-extension-tools.py"])
        }
        "gate" => {
            for gate in ["fmt", "metadata", "check", "clippy", "test", "audit"] {
                task(gate)?;
            }
            Ok(())
        }
        "build" | "dev" | "dev-deploy" => {
            let args = desktop_build_args(
                name,
                env::var("PEERSUS_PRIVATE_STORE_BUILD").as_deref() == Ok("1"),
                prerelease()?,
            );
            cargo(&args)?;
            if cfg!(unix) {
                task(if name == "dev" {
                    "native-host-dev"
                } else {
                    "native-host"
                })?;
            }
            Ok(())
        }
        "native-host" | "native-host-dev" => {
            if !cfg!(unix) {
                return Err("the native helper currently requires a Unix platform".to_owned());
            }
            let output = output_directory(if name == "native-host" {
                "release"
            } else {
                "debug"
            })
            .join("peersus-native-host");
            std::fs::create_dir_all(output.parent().expect("release directory"))
                .map_err(|error| error.to_string())?;
            let output = output
                .to_str()
                .ok_or("non-UTF8 native helper output path")?;
            let compiler = env::var("CC").unwrap_or_else(|_| "cc".to_owned());
            let mut arguments = vec![
                "-std=c11",
                "-O2",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-fstack-protector-strong",
                "-fPIE",
            ];
            // ELF linkers take `-pie` and the read-only relocations; Apple's links every program position independent and has
            // neither option (`-pie` unused is an error under `-Werror`, `-z` unknown to its linker).
            if !cfg!(target_os = "macos") {
                arguments.push("-pie");
            }
            arguments.push("crates/peersus-plugin-runtime/native/host.c");
            if !cfg!(target_os = "macos") {
                arguments.push("-Wl,-z,relro,-z,now");
            }
            arguments.extend(["-ldl", "-o", output]);
            run(&compiler, &arguments)
        }
        "flatpak" => run("python3", &["scripts/package-metadata.py", "flatpak"]),
        "upload-wasm" => run("python3", &["scripts/package-upload-wasm.py"]),
        "android-dev-native" => android_native("community", true, true),
        "android-dev-apk" => {
            let private_store = env::var("PEERSUS_PRIVATE_STORE_BUILD").as_deref() == Ok("1");
            // One package per ABI instead of one holding both: what a release carries, since each must fit the
            // updater's single-artifact bound. A device install still takes the default universal package.
            let split = env::var("PEERSUS_ANDROID_ABI_SPLIT").as_deref() == Ok("1");
            run(
                "sh",
                &[
                    "android/gradlew",
                    "--project-dir",
                    "android",
                    "--no-daemon",
                    if private_store {
                        "-PpeersusPrivateStore=true"
                    } else {
                        "-PpeersusPrivateStore=false"
                    },
                    if split {
                        "-PpeersusReleaseSplits=true"
                    } else {
                        "-PpeersusReleaseSplits=false"
                    },
                    "assembleCommunityDebug",
                    "verifyStoreTransport",
                ],
            )?;
            let output = Path::new("target/distribution/android/community/debug");
            std::fs::create_dir_all(output).map_err(|error| error.to_string())?;
            let built = Path::new("target/android/gradle/app/outputs/apk/community/debug");
            let names: &[&str] = if split {
                &[
                    "app-community-arm64-v8a-debug.apk",
                    "app-community-x86_64-debug.apk",
                ]
            } else {
                &["app-community-debug.apk"]
            };
            for name in names {
                std::fs::copy(built.join(name), output.join(name))
                    .map_err(|error| format!("{name}: {error}"))?;
            }
            Ok(())
        }
        "android-dev" => {
            task("android-dev-native")?;
            task("android-dev-apk")
        }
        "android" => {
            for (flavor, features) in [("play", false), ("community", true)] {
                android_native(flavor, features, false)?;
            }
            // Separate invocations passed after the combined task failed in buildReleasePreBundle. `--stacktrace` prints the cause
            // of a failure, which Gradle otherwise leaves out when it has no message (CI's `packagePlayRelease`, E-147); a run that
            // passes prints nothing more.
            run(
                "sh",
                &[
                    "android/gradlew",
                    "--project-dir",
                    "android",
                    "--no-daemon",
                    "--stacktrace",
                    "-PpeersusReleaseSplits=true",
                    "assemblePlayRelease",
                    "assembleCommunityRelease",
                    "verifyStoreTransport",
                ],
            )?;
            run(
                "sh",
                &[
                    "android/gradlew",
                    "--project-dir",
                    "android",
                    "--no-daemon",
                    "--stacktrace",
                    "bundlePlayRelease",
                    "bundleCommunityRelease",
                ],
            )?;
            run(
                "python3",
                &[
                    "scripts/inspect-android.py",
                    "target/android/gradle/app/outputs",
                    "target/distribution/android-size.json",
                ],
            )
        }
        "bench-build" => bench::build(),
        "bench-fixture" => bench::build_canvas_guest(),
        "bench-fixtures" => {
            bench::build_hello_guest()?;
            bench::build_canvas_guest()
        }
        "bench-extension-build" => bench::build_guest_window(),
        "help" | "--help" | "-h" => {
            println!(
                "cargo xtask [TASK ...]\nTasks: gate fmt metadata check clippy test audit build dev native-host native-host-dev flatpak android android-dev android-dev-native android-dev-apk upload-wasm check-manifest help\nGate is local/CI identical; build checks compilation, not desktop/device operation.\nAndroid release creates UNSIGNED APKs/AAB; android-dev packages the community DEBUG APK using Gradle's existing local debug signer. No task installs or uninstalls an application.\nupload-wasm builds a manually approved Android development extension archive; never installs, signs or uploads.\nFlatpak exports a LOCAL unsigned repository and bundle; never uploads.\n\ncargo xtask check-manifest <package-folder-or-manifest-file>\n  Scripts and CI: refuses a package whose manifest lacks a valid licence-and-notice agreement, using the application's own validation.\ncargo xtask catalog-keygen --out PATH\n  Maintainer-only. Makes the catalog signing seed (0600, never printed) and prints the PUBLIC key.\ncargo xtask catalog-sign --archive A --report R --out DIR [--previous C]\n  Maintainer-only (protected CI). Signs the extension catalog; the key is read only from CATALOG_SIGNING_KEY."
            );
            println!(
                "\ncargo xtask languages-offer [DIRECTORY]\n  Writes languages.json, the offer of one release's language files (default languages/<version>/ of a local copy of the languages branch): each language, the name it gives itself and how many template messages it translates. Problems are printed; the files are offered for what fits.\ncargo xtask check-languages [DIRECTORY]\n  CI on the languages branch: fails when a file does not fit the template (crates/peersus-ui/languages/messages.pot) or languages.json is not the offer of its files."
            );

            println!(
                "\ncargo xtask bench [--seconds=N] [--save=NAME] [--compare=NAME] [--sizes] [--programs=DIR] [--no-build] [--no-window] [--on-screen] [--with-surface=DIR] [--measure-programs=DIR]\n  Builds and measures synthetic application workloads (bench-build only builds). --sizes adds desktop executable sizes; --with-surface uses the same prepared Canvas fixture for each revision. --measure-programs measures actual staged desktop executables on Linux against the fixed repository fixtures; it requires a staged test-identity owner and private Xvfb.\ncargo xtask bench-compare (--base=NAME[,NAME…] | --no-baseline) --head=NAME[,NAME…] [--markdown=PATH]\n  Compares kept samples from the same runner. Fails for a blocking regression or an inconclusive comparison."
            );
            println!(
                "\ncargo xtask bench --map-zoom=MANIFEST --map-wasm=MODULE [--programs=DIR] [--steps=N] [--interval-ms=N]\n  Linux only. Measures the real window, owner and Wasm helper on regional and street Location pages on private Xvfb. Uses a fixed position and tile fixture without installing an extension. Writes target/bench/map-zoom.json."
            );
            println!(
                "\ncargo xtask bench --surface [--seconds=N] [--programs=DIR|--no-build] [--on-screen] [--save=NAME]\n  Linux only. Measures the real Canvas window, owner and WASM helper. Writes target/bench/canvas.json and canvas.png. bench-fixture builds just the Canvas module.\ncargo xtask bench --extension=INPUT [--programs=DIR|--no-build] [--seconds=N] [--save=NAME] [--package-report=JSON]\n  Linux only. Measures startup and idle on a disposable owner/window and a reviewed WASM module without a live session or grants. bench-extension-build builds the trusted window and interpreters."
            );
            Ok(())
        }
        other => Err(format!("unknown task {other:?}; use cargo xtask help")),
    }
}
fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    if let Err(error) = env::set_current_dir(root) {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    let tasks: Vec<_> = env::args().skip(1).collect();
    // The catalog and bench tasks take options, so they are not one of the name-only tasks below.
    let optioned = tasks.first().map(String::as_str);
    if matches!(
        optioned,
        Some(
            "catalog-sign"
                | "catalog-keygen"
                | "update-sign"
                | "update-keygen"
                | "bench"
                | "bench-compare"
                | "check-manifest"
                | "languages-offer"
                | "check-languages"
        )
    ) {
        let result = match optioned {
            Some("catalog-sign") => catalog::run(&tasks[1..]),
            Some("catalog-keygen") => catalog::keygen(&tasks[1..]),
            Some("update-sign") => update::run(&tasks[1..]),
            Some("update-keygen") => update::keygen(&tasks[1..]),
            Some("check-manifest") => manifest::run(&tasks[1..]),
            Some("bench-compare") => bench::compare(&tasks[1..]),
            Some("languages-offer") => languages::write(&tasks[1..]),
            Some("check-languages") => languages::check(&tasks[1..]),
            _ => bench::run(&tasks[1..]),
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        };
    }
    for name in if tasks.is_empty() {
        vec!["help".to_owned()]
    } else {
        tasks
    } {
        if let Err(error) = task(&name) {
            eprintln!("xtask: {error}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{desktop_build_args, identity_test_directory};
    use std::path::PathBuf;

    #[test]
    fn desktop_release_checks_that_diagnostics_are_off_and_development_tasks_enable_them() {
        let release = desktop_build_args("build", true, true);
        assert!(release.contains(&"--release"));
        assert!(release.contains(&"peersus-app/production-diagnostics"));
        assert!(
            !release
                .iter()
                .any(|arg| arg.contains("/development-diagnostics"))
        );
        for (task, optimized) in [("dev", false), ("dev-deploy", true)] {
            let args = desktop_build_args(task, false, false);
            assert_eq!(args.contains(&"--release"), optimized);
            assert!(!args.contains(&"peersus-app/production-diagnostics"));
            let features = args
                .windows(2)
                .find_map(|pair| (pair[0] == "--features").then_some(pair[1]))
                .unwrap();
            for package in [
                "peersus-app",
                "peersus-ui",
                "peersus-plugin-runtime",
                "peersus-plugin-canvas",
            ] {
                assert!(
                    features
                        .split(',')
                        .any(|feature| feature == format!("{package}/development-diagnostics"))
                );
            }
        }
        for task in ["dev", "dev-deploy"] {
            assert!(
                desktop_build_args(task, true, false)
                    .contains(&"peersus-ui/private-store,peersus-app/private-store,peersus-store-fetch/private-store")
            );
            assert!(
                !desktop_build_args(task, false, false)
                    .iter()
                    .any(|arg| arg.contains("private-store"))
            );
        }
        // A product build follows no channel and reads only the public store.
        assert!(
            !desktop_build_args("build", true, true)
                .iter()
                .any(|arg| arg.contains("private-store") || arg.contains("-channel"))
        );
        // Every development build follows exactly one channel, with or without the private catalog.
        for private_store in [false, true] {
            for (prerelease, channel) in [
                (false, "peersus-app/development-channel"),
                (true, "peersus-app/prerelease-channel"),
            ] {
                let args = desktop_build_args("dev-deploy", private_store, prerelease);
                assert!(args.contains(&channel));
                assert_eq!(
                    args.iter().filter(|arg| arg.contains("-channel")).count(),
                    1
                );
            }
        }
    }

    #[test]
    fn ephemeral_identity_outputs_never_replace_the_product_executable() {
        for root in [None, Some("target/ui".into()), Some("custom cache".into())] {
            let product = root
                .clone()
                .map(PathBuf::from)
                .unwrap_or_else(|| "target".into());
            let isolated = identity_test_directory(root);
            assert_eq!(isolated.parent(), Some(product.as_path()));
            for triple in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-gnu"] {
                for profile in ["debug", "release"] {
                    assert_ne!(
                        product.join(triple).join(profile).join("peersus"),
                        isolated.join(triple).join(profile).join("peersus")
                    );
                }
            }
        }
    }
}
