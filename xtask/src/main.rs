use std::{env, path::Path, process::{Command, ExitCode}};
mod catalog;

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    eprintln!("+ {program} {}", args.join(" "));
    let status = Command::new(program).args(args).env("FORCE_SKIA_BUILD", "1").status()
        .map_err(|error| format!("cannot execute {program}: {error}"))?;
    if status.success() { Ok(()) } else { Err(format!("{program} exited with {status}")) }
}
fn cargo(args: &[&str]) -> Result<(), String> { run("cargo", args) }
fn task(name: &str) -> Result<(), String> {
    match name {
        "fmt" => cargo(&["fmt", "--all", "--", "--check"]),
        "check" => cargo(&["check", "--locked", "--workspace", "--all-targets", "--exclude", "peersus-ui-android"]),
        "clippy" => cargo(&["clippy", "--locked", "--workspace", "--all-targets", "--exclude", "peersus-ui-android", "--", "-D", "warnings"]),
        "test" => cargo(&["test", "--locked", "--workspace", "--exclude", "peersus-ui-android"]),
        "audit" => cargo(&["audit", "--deny", "warnings"]),
        "metadata" => {
            run("python3", &["scripts/package-metadata.py", "check"])?;
            run("python3", &["scripts/test-inspect-android.py"])?;
            run("python3", &["scripts/test-extension-tools.py"])
        },
        "gate" => { for gate in ["fmt", "metadata", "check", "clippy", "test", "audit"] { task(gate)?; } Ok(()) },
        "build" => {
            let mut packages = vec!["peersus-ui".to_owned(), "peersus-app".to_owned(), "peersus-plugin-runtime".to_owned(), "peersus-store-fetch".to_owned()];
            let entries = std::fs::read_dir("crates").map_err(|error| error.to_string())?;
            for entry in entries {
                let entry = entry.map_err(|error| error.to_string())?;
                if entry.path().join("plugin.json").is_file() || entry.path().join("plugin.toml").is_file() {
                    packages.push(entry.file_name().into_string().map_err(|_| "non-UTF8 crate directory")?);
                }
            }
            packages.sort();
            let mut args = vec!["build", "--locked", "--release"];
            for package in &packages { args.extend(["-p", package.as_str()]); }
            cargo(&args)?;
            task("native-host")
        },
        "native-host" => {
            if !cfg!(unix) { return Err("the native helper currently requires a Unix platform".to_owned()); }
            std::fs::create_dir_all("target/release").map_err(|error| error.to_string())?;
            let compiler = env::var("CC").unwrap_or_else(|_| "cc".to_owned());
            run(&compiler, &["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-fstack-protector-strong", "-fPIE", "-pie",
                "crates/peersus-plugin-runtime/native/host.c", "-Wl,-z,relro,-z,now", "-ldl", "-o", "target/release/peersus-native-host"])
        },
        "flatpak" => run("python3", &["scripts/package-metadata.py", "flatpak"]),
        "android" => {
            for (flavor, features) in [("play", false), ("community", true)] {
                let destination = format!("android/app/src/{flavor}/jniLibs");
                let mut args = vec!["ndk", "-p", "31", "-t", "arm64-v8a", "-t", "x86_64", "-o", &destination, "build", "--locked", "--release", "-p", "peersus-ui-android", "-p", "peersus-plugin-android"];
                if features { args.extend(["--features", "peersus-ui-android/native-extensions"]); }
                cargo(&args)?;
            }
            run("python3", &["scripts/build-android-native.py", "--output", "android/app/src/community/jniLibs"])?;
            // Separate invocations passed after the combined task failed in buildReleasePreBundle.
            run("sh", &["android/gradlew", "--project-dir", "android", "--no-daemon", "-PpeersusReleaseSplits=true", "assemblePlayRelease", "assembleCommunityRelease"])?;
            run("sh", &["android/gradlew", "--project-dir", "android", "--no-daemon", "bundlePlayRelease", "bundleCommunityRelease"])?;
            run("python3", &["scripts/inspect-android.py", "android/app/build/outputs", "target/distribution/android-size.json"])
        },
        "help" | "--help" | "-h" => { println!("cargo xtask [TASK ...]\nTasks: gate fmt metadata check clippy test audit build native-host flatpak android help\nGate is local/CI identical; build checks compilation, not desktop/device operation.\nAndroid creates UNSIGNED release APKs/AAB; no signing credentials are read.\nFlatpak exports a LOCAL unsigned repository and bundle; never uploads.\n\ncargo xtask catalog-keygen --out PATH\n  Maintainer-only. Makes the catalog signing seed (0600, never printed) and prints the PUBLIC key.\ncargo xtask catalog-sign --archive A --report R --out DIR [--previous C]\n  Maintainer-only (protected CI). Signs the extension catalog; the key is read only from CATALOG_SIGNING_KEY."); Ok(()) }
        other => Err(format!("unknown task {other:?}; use cargo xtask help")),
    }
}
fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("workspace root");
    if let Err(error) = env::set_current_dir(root) { eprintln!("{error}"); return ExitCode::FAILURE; }
    let tasks: Vec<_> = env::args().skip(1).collect();
    // The catalog tasks take options, so they are not one of the name-only tasks below.
    let catalog_task = tasks.first().map(String::as_str);
    if matches!(catalog_task, Some("catalog-sign" | "catalog-keygen")) {
        let result = if catalog_task == Some("catalog-sign") { catalog::run(&tasks[1..]) } else { catalog::keygen(&tasks[1..]) };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => { eprintln!("xtask: {error}"); ExitCode::FAILURE }
        };
    }
    for name in if tasks.is_empty() { vec!["help".to_owned()] } else { tasks } {
        if let Err(error) = task(&name) { eprintln!("xtask: {error}"); return ExitCode::FAILURE; }
    }
    ExitCode::SUCCESS
}
