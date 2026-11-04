use std::{
    env,
    path::Path,
    process::{Command, ExitCode},
};
mod catalog;

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    eprintln!("+ {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .env("FORCE_SKIA_BUILD", "1")
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
    run(
        "python3",
        &["scripts/package-upload-wasm.py", "--build-only"],
    )?;
    let destination = format!("target/android/jniLibs/{flavor}");
    let mut args = vec![
        "ndk",
        "-p",
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
    let features = match (extensions, development) {
        (true, true) => {
            Some("peersus-ui-android/native-extensions,peersus-ui-android/development-diagnostics")
        }
        (true, false) => Some("peersus-ui-android/native-extensions"),
        (false, true) => Some("peersus-ui-android/development-diagnostics"),
        (false, false) => None,
    };
    if let Some(features) = features {
        args.extend(["--features", features]);
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

fn task(name: &str) -> Result<(), String> {
    match name {
        "fmt" => cargo(&["fmt", "--all", "--", "--check"]),
        "check" => cargo(&[
            "check",
            "--locked",
            "--workspace",
            "--all-targets",
            "--exclude",
            "peersus-ui-android",
        ]),
        "clippy" => cargo(&[
            "clippy",
            "--locked",
            "--workspace",
            "--all-targets",
            "--exclude",
            "peersus-ui-android",
            "--",
            "-D",
            "warnings",
        ]),
        "test" => {
            cargo(&[
                "test",
                "--locked",
                "--workspace",
                "--exclude",
                "peersus-ui-android",
            ])?;
            // Keep test-only identity and plugin fixtures out of product builds,
            // but never silently omit their process/store integration coverage.
            cargo(&[
                "test",
                "--locked",
                "-p",
                "peersus-app",
                "--features",
                "test-plugin,test-identity",
            ])
        }
        "audit" => cargo(&["audit", "--deny", "warnings"]),
        "metadata" => {
            run("python3", &["scripts/test-gen-docs.py"])?;
            run("python3", &["scripts/gen-docs.py", "--check"])?;
            run("python3", &["scripts/package-metadata.py", "check"])?;
            run("python3", &["scripts/test-inspect-android.py"])?;
            run("python3", &["scripts/test-stage-linux.py"])?;
            run("python3", &["scripts/test-root-helpers.py"])?;
            run("python3", &["scripts/test-dev.py"])?;
            run("python3", &["scripts/test-site-preview.py"])?;
            run("python3", &["scripts/test-extension-source.py"])?;
            run("python3", &["scripts/test-extension-admission.py"])?;
            run("python3", &["scripts/test-extension-ownership.py"])?;
            run("python3", &["scripts/test-prepare-extension-branch.py"])?;
            run("python3", &["scripts/test-package-native-extension.py"])?;
            run("python3", &["scripts/test-triage-pull-request.py"])?;
            run("python3", &["scripts/test-extension-tools.py"])
        }
        "gate" => {
            for gate in [
                "fmt",
                "metadata",
                "check",
                "clippy",
                "test",
                "audit",
                "upload-wasm",
            ] {
                task(gate)?;
            }
            Ok(())
        }
        "build" | "dev" => {
            let mut packages = vec![
                "peersus-ui".to_owned(),
                "peersus-app".to_owned(),
                "peersus-plugin-runtime".to_owned(),
                "peersus-store-fetch".to_owned(),
            ];
            let entries = std::fs::read_dir("crates").map_err(|error| error.to_string())?;
            for entry in entries {
                let entry = entry.map_err(|error| error.to_string())?;
                if entry.path().join("plugin.json").is_file()
                    || entry.path().join("plugin.toml").is_file()
                {
                    packages.push(
                        entry
                            .file_name()
                            .into_string()
                            .map_err(|_| "non-UTF8 crate directory")?,
                    );
                }
            }
            packages.sort();
            let mut args = vec!["build", "--locked"];
            if name == "build" {
                args.push("--release");
            }
            for package in &packages {
                args.extend(["-p", package.as_str()]);
            }
            cargo(&args)?;
            task(if name == "build" {
                "native-host"
            } else {
                "native-host-dev"
            })
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
            run(
                &compiler,
                &[
                    "-std=c11",
                    "-O2",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-fstack-protector-strong",
                    "-fPIE",
                    "-pie",
                    "crates/peersus-plugin-runtime/native/host.c",
                    "-Wl,-z,relro,-z,now",
                    "-ldl",
                    "-o",
                    output,
                ],
            )
        }
        "flatpak" => run("python3", &["scripts/package-metadata.py", "flatpak"]),
        "upload-wasm" => run("python3", &["scripts/package-upload-wasm.py"]),
        "android-dev-native" => android_native("community", true, true),
        "android-dev-apk" => {
            run(
                "sh",
                &[
                    "android/gradlew",
                    "--project-dir",
                    "android",
                    "--no-daemon",
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
            // Separate invocations passed after the combined task failed in buildReleasePreBundle.
            run(
                "sh",
                &[
                    "android/gradlew",
                    "--project-dir",
                    "android",
                    "--no-daemon",
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
        "help" | "--help" | "-h" => {
            println!(
                "cargo xtask [TASK ...]\nTasks: gate fmt metadata check clippy test audit build dev native-host native-host-dev flatpak android android-dev android-dev-native android-dev-apk upload-wasm help\nGate is local/CI identical; build checks compilation, not desktop/device operation.\nAndroid release creates UNSIGNED APKs/AAB; android-dev packages the community DEBUG APK using Gradle's existing local debug signer. No task installs or uninstalls an application.\nupload-wasm builds a manually approved Android development extension archive; never installs, signs or uploads.\nFlatpak exports a LOCAL unsigned repository and bundle; never uploads.\n\ncargo xtask catalog-keygen --out PATH\n  Maintainer-only. Makes the catalog signing seed (0600, never printed) and prints the PUBLIC key.\ncargo xtask catalog-sign --archive A --report R --out DIR [--previous C]\n  Maintainer-only (protected CI). Signs the extension catalog; the key is read only from CATALOG_SIGNING_KEY."
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
    // The catalog tasks take options, so they are not one of the name-only tasks below.
    let catalog_task = tasks.first().map(String::as_str);
    if matches!(catalog_task, Some("catalog-sign" | "catalog-keygen")) {
        let result = if catalog_task == Some("catalog-sign") {
            catalog::run(&tasks[1..])
        } else {
            catalog::keygen(&tasks[1..])
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
