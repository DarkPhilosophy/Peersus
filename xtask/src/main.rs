use std::{
    env,
    path::Path,
    process::{Command, ExitCode},
};
mod bench;
mod catalog;
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

fn task(name: &str) -> Result<(), String> {
    match name {
        "fmt" => cargo(&["fmt", "--all", "--", "--check"]),
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
            cargo(&[
                "build",
                "--locked",
                "--release",
                "--manifest-path",
                "templates/hello-guest/Cargo.toml",
                "--target",
                "wasm32-unknown-unknown",
                "--target-dir",
                "target/hello-guest",
            ])?;
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
            run("python3", &["scripts/package-metadata.py", "check"])?;
            // Modules of one crate that need each other: Cargo refuses that between crates and nothing does within one.
            run("python3", &["scripts/module-cycles.py"])?;
            run("python3", &["scripts/test-inspect-android.py"])?;
            run("python3", &["scripts/test-stage-linux.py"])?;
            run("python3", &["scripts/test-install-linux-user.py"])?;
            run("python3", &["scripts/test-stage-macos.py"])?;
            run("python3", &["scripts/test-root-helpers.py"])?;
            run("python3", &["scripts/test-dev.py"])?;
            run("python3", &["scripts/test-deploy.py"])?;
            run("python3", &["scripts/test-site-preview.py"])?;
            run("python3", &["scripts/test-catalog-fixtures.py"])?;
            run("python3", &["scripts/test-extension-source.py"])?;
            run("python3", &["scripts/test-extension-admission.py"])?;
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
        "build" | "dev" => {
            let packages = [
                "peersus-ui",
                "peersus-app",
                "peersus-plugin-runtime",
                "peersus-store-fetch",
            ];
            let mut args = vec!["build", "--locked"];
            if name == "build" {
                args.push("--release");
            }
            for package in &packages {
                args.extend(["-p", *package]);
            }
            if name == "dev" && env::var("PEERSUS_PRIVATE_STORE_BUILD").as_deref() == Ok("1") {
                args.extend(["--features", "peersus-ui/private-store,peersus-app/private-store,peersus-store-fetch/private-store"]);
            }
            cargo(&args)?;
            if cfg!(unix) {
                task(if name == "build" {
                    "native-host"
                } else {
                    "native-host-dev"
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
                    "assembleCommunityDebug",
                    "verifyStoreTransport",
                ],
            )?;
            let output = Path::new("target/distribution/android/community/debug");
            std::fs::create_dir_all(output).map_err(|error| error.to_string())?;
            std::fs::copy(
                "target/android/gradle/app/outputs/apk/community/debug/app-community-debug.apk",
                output.join("app-community-debug.apk"),
            )
            .map_err(|error| error.to_string())?;
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
        "help" | "--help" | "-h" => {
            println!(
                "cargo xtask [TASK ...]\nTasks: gate fmt metadata check clippy test audit build dev native-host native-host-dev flatpak android android-dev android-dev-native android-dev-apk upload-wasm check-manifest help\nGate is local/CI identical; build checks compilation, not desktop/device operation.\nAndroid release creates UNSIGNED APKs/AAB; android-dev packages the community DEBUG APK using Gradle's existing local debug signer. No task installs or uninstalls an application.\nupload-wasm builds a manually approved Android development extension archive; never installs, signs or uploads.\nFlatpak exports a LOCAL unsigned repository and bundle; never uploads.\n\ncargo xtask check-manifest <package-folder-or-manifest-file>\n  Scripts and CI: refuses a package whose manifest lacks a valid licence-and-notice agreement, using the application's own validation.\ncargo xtask catalog-keygen --out PATH\n  Maintainer-only. Makes the catalog signing seed (0600, never printed) and prints the PUBLIC key.\ncargo xtask catalog-sign --archive A --report R --out DIR [--previous C]\n  Maintainer-only (protected CI). Signs the extension catalog; the key is read only from CATALOG_SIGNING_KEY."
            );
            println!(
                "\ncargo xtask languages-offer [DIRECTORY]\n  Writes languages.json, the offer of one release's language files (default languages/<version>/ of a local copy of the languages branch): each language, the name it gives itself and how many template messages it translates. Problems are printed; the files are offered for what fits.\ncargo xtask check-languages [DIRECTORY]\n  CI on the languages branch: fails when a file does not fit the template (crates/peersus-ui/languages/messages.pot) or languages.json is not the offer of its files."
            );

            println!(
                "\ncargo xtask bench [--seconds=N] [--save=NAME] [--compare=NAME] [--no-build] [--no-window] [--on-screen]\n  Development only. Builds the measuring programs in release (bench-build does only that), runs them on synthetic local data and writes target/bench/latest.json. --save keeps the run in benchmark-results/ (untracked); --compare prints the change against a kept run and judges nothing. The window is measured on a private in-memory X server unless --on-screen. Numbers compare revisions on one machine; they are not device or acceptance results."
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
    use super::identity_test_directory;
    use std::path::PathBuf;

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
