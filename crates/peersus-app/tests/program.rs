//! The `peersus` program itself, started as a process the way a user starts it.
//!
//! Everything else tests the runtime with a harness that builds it by hand. This runs the
//! program: its argument parsing, the choice between being the owner and being a client of
//! one, the private directory, plugin discovery, and the end of it all on `stop`.
//!
//! EVERY RUN IS CONTAINED. Each program is given its own data directory under a temporary one
//! (never the user's), is bound to the loopback address only, and has relays switched off:
//! by default the program contacts public relay and discovery servers, which a test must
//! never do. The environment is cleared of anything that could point it elsewhere.
#![cfg(unix)]

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
#[cfg(feature = "test-identity")]
use std::{
    process::Child,
    time::{Duration, Instant},
};

fn program() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_peersus"))
}

/// A data directory the program may use, and nothing else of the user's.
fn data() -> tempfile::TempDir {
    let directory = tempfile::Builder::new()
        .prefix("pp")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

/// The program, with an environment that cannot reach anything of the user's.
fn command(data: &Path) -> Command {
    let mut command = Command::new(program());
    command
        .env_clear()
        .env("XDG_DATA_HOME", data)
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null());
    command
}

/// Loopback only, no relay: nothing the program does leaves this machine.
#[cfg(feature = "test-identity")]
const CONTAINED: [&str; 3] = ["--bind", "127.0.0.1", "--allow-relay=false"];

/// A running owner that is killed if the test ends without stopping it.
#[cfg(feature = "test-identity")]
struct Owner {
    child: Option<Child>,
}

#[cfg(feature = "test-identity")]
impl Owner {
    fn start(data: &Path) -> Self {
        let child = command(data)
            .args(CONTAINED)
            .arg("--foreground")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the program did not start");
        let mut owner = Self { child: Some(child) };
        // Ready when its control socket exists. Probing with the program itself would race
        // the owner for the instance lock, which every invocation takes before it decides
        // whether it is only a client: a probe that won would be the owner for a moment, and
        // the intended owner would take the client branch and leave.
        let socket = data.join("peersus").join("control.sock");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = owner.child.as_mut().unwrap().try_wait().unwrap() {
                let mut why = String::new();
                if let Some(mut stderr) = owner.child.as_mut().unwrap().stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut stderr, &mut why);
                }
                panic!("the owner ended at start with {status}: {why}");
            }
            if socket.exists() {
                return owner;
            }
            assert!(Instant::now() < deadline, "the owner never made its socket");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait for the owner to end by itself, as it does after `stop`.
    fn ended(self) -> std::process::ExitStatus {
        self.ended_with_output().0
    }

    /// Like `ended`, and also what the owner wrote to its error output.
    fn ended_with_output(mut self) -> (std::process::ExitStatus, String) {
        let mut child = self.child.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let mut written = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut stderr, &mut written);
                }
                return (status, written);
            }
            if Instant::now() > deadline {
                stop_gracefully(&mut child);
                panic!("the owner did not end after being told to stop");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(feature = "test-identity")]
impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            stop_gracefully(&mut child);
        }
    }
}

/// End an owner so that its supervisor can stop the plugins it started. A plain kill would
/// end only the owner: the plugins are in groups of their own and are stopped by the owner,
/// which a kill gives no chance to do, so they would be left running after the test.
///
/// Returns whether a signal was sent. A child that has already ended has been collected, and
/// its number may by now belong to an unrelated process, so it is never signalled: whether it
/// has ended is asked first, and only a child that is still ours is told to stop.
#[cfg(feature = "test-identity")]
fn stop_gracefully(child: &mut Child) -> bool {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return false;
    }
    if let Some(pid) = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    true
}

fn run(data: &Path, args: &[&str]) -> std::process::Output {
    command(data).args(args).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn settings_are_answered_from_the_saved_file_with_no_owner_running() {
    let data = data();
    let out = run(data.path(), &["settings"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let settings: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(settings["device_name"], "My device");
}

#[test]
fn saved_preferences_are_kept_for_the_next_run() {
    let data = data();
    let saved = run(
        data.path(),
        &["--device-name", "Kept name", "settings", "--save"],
    );
    assert!(saved.status.success(), "{}", text(&saved.stderr));
    let out = run(data.path(), &["settings"]);
    let settings: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(settings["device_name"], "Kept name");
}

#[test]
fn previewing_preferences_without_save_changes_nothing_on_disk() {
    let data = data();
    let preview = run(
        data.path(),
        &["--device-name", "Only a preview", "settings"],
    );
    let shown: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(shown["device_name"], "Only a preview");
    let after = run(data.path(), &["settings"]);
    let kept: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(kept["device_name"], "My device");
}

#[test]
fn a_control_command_with_no_owner_running_is_refused_in_words() {
    let data = data();
    let out = run(data.path(), &["peers"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("Start the application before sending control commands"),
        "{}",
        text(&out.stderr)
    );
}

/// A plugin package on disk the way an author would ship it: a directory named for the plugin.
fn package_on_disk(parent: &Path, name: &str, program: &str) -> PathBuf {
    let directory = parent.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("plugin.json"),
        r#"{"manifest_version":1,"capabilities":{},"imports":[],"exports":[]}"#,
    )
    .unwrap();
    std::fs::write(directory.join("plugin"), program).unwrap();
    std::fs::set_permissions(directory.join("plugin"), PermissionsExt::from_mode(0o755)).unwrap();
    directory
}

#[test]
fn a_plugin_is_installed_listed_and_removed_with_no_owner_running() {
    let (data, parent) = (data(), tempfile::tempdir().unwrap());
    let source = package_on_disk(parent.path(), "quiet", "#!/bin/sh\nexec sleep 300\n");
    let source = source.to_str().unwrap();
    let installed = run(data.path(), &["plugin-install", source]);
    assert!(installed.status.success(), "{}", text(&installed.stderr));
    assert!(text(&installed.stdout).contains("Installed quiet"));
    // It is where the application will look for it, and nothing was needed to put it there.
    assert!(data.path().join("peersus/plugins/quiet/plugin").exists());
    let listed = run(data.path(), &["plugin-list"]);
    assert!(
        text(&listed.stdout).contains("quiet  installed"),
        "{}",
        text(&listed.stdout)
    );
    let again = run(data.path(), &["plugin-install", source]);
    assert!(text(&again.stdout).contains("already installed"));
    let removed = run(data.path(), &["plugin-remove", "quiet"]);
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    assert!(!data.path().join("peersus/plugins/quiet").exists());
    assert!(text(&run(data.path(), &["plugin-list"]).stdout).contains("No plugins are installed"));
}

#[test]
fn a_package_that_cannot_run_is_refused_in_words_and_leaves_nothing() {
    let (data, parent) = (data(), tempfile::tempdir().unwrap());
    let source = package_on_disk(parent.path(), "thing", "x");
    std::fs::set_permissions(source.join("plugin"), PermissionsExt::from_mode(0o644)).unwrap();
    let out = run(data.path(), &["plugin-install", source.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("cannot be run"),
        "{}",
        text(&out.stderr)
    );
    assert!(text(&run(data.path(), &["plugin-list"]).stdout).contains("No plugins are installed"));
}

#[test]
fn a_plugin_installed_by_the_command_is_found_by_discovery_in_the_same_directory() {
    // The command and the application must agree on where plugins live, or an installed
    // plugin would never be seen: this ties the two to one location.
    let (data, parent) = (data(), tempfile::tempdir().unwrap());
    let source = package_on_disk(parent.path(), "quiet", "#!/bin/sh\nexit 0\n");
    let out = run(data.path(), &["plugin-install", source.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let found = peersus_app::discovery::discover(&data.path().join("peersus/plugins"));
    assert!(found.problems.is_empty(), "{:?}", found.problems);
    assert_eq!(found.registry.iter().count(), 1);
}

#[test]
fn a_directory_put_in_the_store_by_hand_is_listed_by_what_it_is_and_never_as_installed() {
    let data = data();
    let store = data.path().join("peersus/plugins");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Nothing a plugin has: no manifest, no program. It cannot be used, and the listing says it can only be removed.
    std::fs::create_dir_all(store.join("empty")).unwrap();
    // A complete package, put there by hand: good in every way, and nobody approved it.
    let complete = package_on_disk(&store, "complete", "#!/bin/sh\nexit 0\n");
    std::fs::set_permissions(&complete, std::fs::Permissions::from_mode(0o700)).unwrap();
    let listed = text(&run(data.path(), &["plugin-list"]).stdout);
    assert!(listed.contains("empty  BROKEN"), "{listed}");
    assert!(listed.contains("complete  NOT APPROVED"), "{listed}");
    // The state words of a plugin that really is installed (and switched off), which neither of these may have.
    for line in listed.lines() {
        let state = line.split_once("  ").map_or("", |(_, state)| state);
        assert!(
            state != "installed" && !state.starts_with("installed,"),
            "a folder nobody installed was listed as installed: {line}"
        );
    }
}

#[test]
fn an_option_nobody_defined_is_refused_before_anything_starts() {
    let data = data();
    let out = run(data.path(), &["--no-such-option"]);
    assert!(!out.status.success());
    // Nothing was created: refusing the arguments comes before the data directory.
    assert!(
        !data.path().join("peersus").exists(),
        "the program made its directory for arguments it refused"
    );
}

#[cfg(feature = "test-identity")]
#[test]
fn the_first_run_is_the_owner_and_a_second_is_only_its_client_until_it_is_stopped() {
    let data = data();
    let owner = Owner::start(data.path());
    // Its directory is private to the user, and holds the lock and the control socket.
    let directory = data.path().join("peersus");
    let mode = std::fs::metadata(&directory).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "the application directory is not private");
    assert!(directory.join("control.sock").exists());
    // A second program answers from the owner instead of starting a second one.
    let peers = run(data.path(), &["peers"]);
    assert!(peers.status.success(), "{}", text(&peers.stderr));
    assert_eq!(text(&peers.stdout).trim(), "[]");
    // `stop` is carried out by the owner, which then ends by itself and cleanly.
    let stop = run(data.path(), &["stop"]);
    assert!(stop.status.success(), "{}", text(&stop.stderr));
    let status = owner.ended();
    assert!(status.success(), "the owner ended with {status}");
    // Its control socket does not outlive it.
    assert!(
        !directory.join("control.sock").exists(),
        "the control socket was left behind"
    );
}

#[cfg(feature = "test-identity")]
#[test]
fn only_a_plugin_that_was_installed_and_is_unchanged_is_started() {
    let (data, parent) = (data(), tempfile::tempdir().unwrap());
    // One plugin installed the way a user does: with the command.
    let good = package_on_disk(parent.path(), "quiet", "#!/bin/sh\nexec sleep 300\n");
    let installed = run(data.path(), &["plugin-install", good.to_str().unwrap()]);
    assert!(installed.status.success(), "{}", text(&installed.stderr));
    // A second, installed the same way, and then changed on disk afterwards.
    let tampered = package_on_disk(parent.path(), "swapped", "#!/bin/sh\nexec sleep 300\n");
    let installed = run(data.path(), &["plugin-install", tampered.to_str().unwrap()]);
    assert!(installed.status.success(), "{}", text(&installed.stderr));
    let store = data.path().join("peersus").join("plugins");
    let program = store.join("swapped").join("plugin");
    std::fs::write(&program, "#!/bin/sh\nexec sleep 301\n").unwrap();
    // A third, a perfectly good package that nobody installed, put in the store by hand.
    let by_hand = store.join("byhand");
    std::fs::create_dir(&by_hand).unwrap();
    std::fs::set_permissions(&by_hand, PermissionsExt::from_mode(0o700)).unwrap();
    std::fs::write(
        by_hand.join("plugin.json"),
        r#"{"manifest_version":1,"capabilities":{},"imports":[],"exports":[]}"#,
    )
    .unwrap();
    std::fs::write(by_hand.join("plugin"), "#!/bin/sh\nexec sleep 302\n").unwrap();
    std::fs::set_permissions(by_hand.join("plugin"), PermissionsExt::from_mode(0o755)).unwrap();

    let owner = Owner::start(data.path());
    // The control socket is ready before every asynchronous supervisor has
    // reported its first event. Wait for the installed plugin's startup state,
    // not an arbitrary delay or the socket's existence alone.
    let deadline = Instant::now() + Duration::from_secs(10);
    let listed: serde_json::Value = loop {
        let out = run(data.path(), &["plugins"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        let listed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let started = listed.as_array().unwrap().iter().any(|plugin| {
            plugin["id"] == "quiet"
                && matches!(plugin["status"].as_str(), Some("starting" | "running"))
        });
        if started {
            break listed;
        }
        assert!(
            Instant::now() < deadline,
            "installed plugin did not start: {listed}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    // A plugin that is blocked is never missing from the list (it can be approved or removed from the window), so
    // what this proves is WHICH ones may run: by what each says of its standing, and that a blocked one has no process.
    let state = |id: &str| {
        let found = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap_or_else(|| panic!("{id} is not listed: {listed}"));
        (
            found["standing"].as_str().unwrap().to_owned(),
            found["status"].as_str().unwrap().to_owned(),
        )
    };
    // The person is still told, in fixed words, which blocked plugin is blocked and why: by the list command.
    let described = run(data.path(), &["plugin-list"]);
    assert!(described.status.success(), "{}", text(&described.stderr));
    let described = text(&described.stdout);
    let stop = run(data.path(), &["stop"]);
    assert!(stop.status.success());
    let (_status, stderr) = owner.ended_with_output();
    assert_eq!(
        listed.as_array().unwrap().len(),
        3,
        "every plugin in the store is listed: {listed}"
    );
    assert_eq!(
        state("quiet").0,
        "ready",
        "what was installed and is unchanged may run"
    );
    assert_ne!(state("quiet").1, "stopped", "and it was started: {listed}");
    assert_eq!(
        state("byhand"),
        ("needs-approval".to_owned(), "stopped".to_owned()),
        "nobody installed it"
    );
    assert_eq!(
        state("swapped"),
        ("changed".to_owned(), "stopped".to_owned()),
        "it is not what was installed"
    );
    assert!(
        described.contains("byhand  NOT APPROVED: put there by hand, not installed"),
        "{described}"
    );
    assert!(
        described.contains("swapped  CHANGED since it was approved"),
        "{described}"
    );
    // A blocked plugin is a state to show and no longer a start-up problem, so the owner has nothing to say about
    // it on its error output: and in particular nothing the plugin wrote.
    assert!(
        !stderr.contains("sleep"),
        "text from a plugin reached the owner's output: {stderr}"
    );
}

/// The production default, through the REAL program: this build carries no maintainer key, so the extension store
/// says it is not set up and refuses every store command, whatever is asked. No test key can be given to the real
/// binary, so what it does here is exactly what a user's build does today.
#[cfg(feature = "test-identity")]
#[test]
fn a_build_without_a_maintainer_key_reports_the_store_as_not_set_up_and_refuses_it() {
    let data = data();
    let owner = Owner::start(data.path());
    let shown = run(data.path(), &["store"]);
    assert!(shown.status.success(), "{}", text(&shown.stderr));
    let state: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(state["status"], "not-configured", "{state}");
    assert_eq!(state["listings"], serde_json::json!([]));
    for arguments in [
        &["store-refresh"][..],
        &["store-install", "echo"],
        &["store-install", "../etc"],
    ] {
        let refused = run(data.path(), arguments);
        assert!(!refused.status.success(), "{arguments:?} must be refused");
    }
    // Refusing is not a failure state that invites a retry: it still reads as not set up.
    let again: serde_json::Value =
        serde_json::from_slice(&run(data.path(), &["store"]).stdout).unwrap();
    assert_eq!(again["status"], "not-configured");
    let stop = run(data.path(), &["stop"]);
    assert!(stop.status.success());
    let (_status, stderr) = owner.ended_with_output();
    // The helper was never started: nothing was fetched.
    assert!(!stderr.contains("peersus-store-fetch"), "{stderr}");
}

/// The process id of the one running instance of the plugin whose program sleeps for
/// `seconds`, or `None` if there is not exactly one. Found by what it runs, not by name.
#[cfg(feature = "test-identity")]
fn sleeper(seconds: u32) -> Option<u32> {
    let needle = format!("sleep {seconds}");
    let out = Command::new("pgrep")
        .args(["-f", &format!("^{needle}$")])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let ids: Vec<u32> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
    (ids.len() == 1).then(|| ids[0])
}

#[cfg(feature = "test-identity")]
#[test]
fn restart_plugin_replaces_the_running_process_with_a_new_one() {
    let (data, parent) = (data(), tempfile::tempdir().unwrap());
    // A distinctive duration so the process can be found by what it runs.
    let package = package_on_disk(parent.path(), "quiet", "#!/bin/sh\nexec sleep 411\n");
    let installed = run(data.path(), &["plugin-install", package.to_str().unwrap()]);
    assert!(installed.status.success(), "{}", text(&installed.stderr));
    let owner = Owner::start(data.path());

    let wait_for = |what: &str| {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(pid) = sleeper(411) {
                return pid;
            }
            assert!(Instant::now() < deadline, "{what}: the plugin never ran");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let before = wait_for("at start");

    let restarted = run(data.path(), &["restart-plugin", "quiet"]);
    assert!(restarted.status.success(), "{}", text(&restarted.stderr));

    // A DIFFERENT process runs it now, and only one: the old one was stopped, not kept.
    let deadline = Instant::now() + Duration::from_secs(15);
    let after = loop {
        if let Some(pid) = sleeper(411)
            && pid != before
        {
            break pid;
        }
        assert!(Instant::now() < deadline, "the plugin was not restarted");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_ne!(before, after);

    // Something that does not exist is refused in words, and nothing else was disturbed.
    let missing = run(data.path(), &["restart-plugin", "nothing"]);
    assert!(!missing.status.success());
    assert_eq!(sleeper(411), Some(after));

    let stop = run(data.path(), &["stop"]);
    assert!(stop.status.success());
    owner.ended();
    assert_eq!(sleeper(411), None, "the plugin outlived the application");
}

#[cfg(feature = "test-identity")]
#[test]
fn at_start_the_program_forgets_what_was_granted_to_plugins_that_are_not_installed() {
    let (data, parent) = (data(), tempfile::tempdir().unwrap());
    let package = package_on_disk(parent.path(), "quiet", "#!/bin/sh\nexec sleep 412\n");
    let installed = run(data.path(), &["plugin-install", package.to_str().unwrap()]);
    assert!(installed.status.success(), "{}", text(&installed.stderr));
    // What the user once allowed: to a plugin that is installed, and to one that was removed.
    let state = data.path().join("peersus");
    let granted = |who: &str| format!(r#""{who}":[{{"owner":"host","name":"plugins.restart"}}]"#);
    std::fs::write(
        state.join("grants.json"),
        format!(
            r#"{{"version":1,"grants":{{{},{}}}}}"#,
            granted("quiet"),
            granted("removed-long-ago")
        ),
    )
    .unwrap();
    let owner = Owner::start(data.path());
    let stop = run(data.path(), &["stop"]);
    assert!(stop.status.success(), "{}", text(&stop.stderr));
    owner.ended();
    let saved = std::fs::read_to_string(state.join("grants.json")).unwrap();
    assert!(
        !saved.contains("removed-long-ago"),
        "what a removed plugin was allowed was kept: {saved}"
    );
    assert!(
        saved.contains("quiet"),
        "an installed plugin lost its consent: {saved}"
    );
}

#[cfg(feature = "test-identity")]
#[test]
fn a_child_that_has_already_ended_is_never_signalled() {
    // It ended and was collected, so its number may belong to someone else by now.
    let mut child = Command::new("true").spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "`true` did not end");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !stop_gracefully(&mut child),
        "a signal was sent to a child that had already been collected"
    );
}

#[cfg(feature = "test-identity")]
#[test]
fn a_child_that_is_still_running_is_stopped() {
    let mut child = Command::new("sleep").arg("1000").spawn().unwrap();
    assert!(stop_gracefully(&mut child));
    assert!(
        child.try_wait().unwrap().is_some(),
        "the child was still running after it was told to stop"
    );
}
