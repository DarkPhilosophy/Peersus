//! The product window, a release owner built with the ephemeral identity, and real WASM
//! interpreters. Only the checked-in synthetic fixtures may be staged for this workload.

use super::{LOOK_EVERY, MEMORY_EVERY, PrivateDisplay, Usage, configure_private_window};
use serde_json::{Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const READY: Duration = Duration::from_secs(25);
const FIXTURES: [(&str, &str); 2] = [
    ("hello", "peersus-wasm-host"),
    ("canvas", "peersus-wasm-canvas-host"),
];

struct Profile(PathBuf);

impl Profile {
    fn open() -> Result<Self, String> {
        let output = Command::new("/usr/bin/mktemp")
            .args(["-d", "/tmp/peersus-bench.XXXXXXXX"])
            .output()
            .map_err(|error| format!("could not create a benchmark profile: {error}"))?;
        let path =
            String::from_utf8(output.stdout).map_err(|_| "invalid temporary profile path")?;
        let path = PathBuf::from(path.trim_end());
        if !output.status.success()
            || path.parent() != Some(Path::new("/tmp"))
            || !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("peersus-bench."))
            || !fs::symlink_metadata(&path).is_ok_and(|entry| entry.is_dir())
        {
            return Err("could not create a benchmark profile".into());
        }
        Ok(Self(path))
    }

    fn command(&self, executable: &Path) -> Command {
        let mut command = Command::new(executable);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.0)
            .env("XDG_DATA_HOME", &self.0)
            .env("XDG_CONFIG_HOME", &self.0)
            .env("XDG_RUNTIME_DIR", &self.0)
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/dev/null")
            .stdin(Stdio::null());
        command
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Running(Child);

impl Running {
    fn start(mut command: Command, name: &str) -> Result<Self, String> {
        command
            .stdout(Stdio::null())
            .spawn()
            .map(Self)
            .map_err(|error| format!("cannot start {name}: {error}"))
    }

    fn live(&mut self, name: &str) -> Result<(), String> {
        match self.0.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => Err(format!("{name} exited before its sample ended ({status})")),
            Err(error) => Err(format!("cannot observe {name}: {error}")),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            // An owner handles TERM and cooperatively stops its worker process groups.
            let _ = Command::new("kill")
                .args(["-TERM", &self.0.id().to_string()])
                .status();
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.0.try_wait().is_ok_and(|status| status.is_none())
                && Instant::now() < deadline
            {
                std::thread::sleep(LOOK_EVERY);
            }
            if self.0.try_wait().is_ok_and(|status| status.is_none()) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
}

fn until(label: &str, mut condition: impl FnMut() -> Result<bool, String>) -> Result<(), String> {
    let end = Instant::now() + READY;
    loop {
        if condition()? {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(format!("timed out waiting for {label}"));
        }
        std::thread::sleep(LOOK_EVERY);
    }
}

fn install_fixture(
    profile: &Profile,
    owner: &Path,
    source: &Path,
    fixture: &str,
) -> Result<(), String> {
    let output = profile
        .command(owner)
        .arg("plugin-install")
        .arg(source)
        .output()
        .map_err(|error| format!("cannot check {fixture} fixture: {error}"))?;
    // Use the real application's agreement identity. Acceptance is for this synthetic,
    // repository fixture in a new temporary profile, never a person's installation.
    let preview = String::from_utf8_lossy(&output.stdout);
    let digest = preview
        .lines()
        .rev()
        .find_map(|line| {
            let value = line.split_once(" --accept ")?.1.split_whitespace().next()?;
            (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then_some(value)
        })
        .ok_or_else(|| {
            format!(
                "{fixture} fixture returned no agreement digest: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })?;
    let installed = profile
        .command(owner)
        .arg("plugin-install")
        .arg(source)
        .args(["--accept", digest, "--accept-outside-store-risk"])
        .output()
        .map_err(|error| format!("cannot install {fixture} fixture: {error}"))?;
    if !installed.status.success() {
        return Err(format!(
            "could not install the {fixture} benchmark fixture: {}",
            String::from_utf8_lossy(&installed.stderr).trim()
        ));
    }
    Ok(())
}

fn helper(owner: u32, executable: &Path) -> Option<u32> {
    fs::read_dir(format!("/proc/{owner}/task"))
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_to_string(entry.path().join("children")).ok())
        .find_map(|children| {
            children
                .split_whitespace()
                .filter_map(|id| id.parse().ok())
                .find(|pid| {
                    fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|path| path == executable)
                })
        })
}

fn stage_program(release: &Path, stage: &Path, name: &str) -> Result<PathBuf, String> {
    let source =
        fs::canonicalize(release.join(name)).map_err(|error| format!("{name}: {error}"))?;
    let target = stage.join(name);
    fs::hard_link(&source, &target)
        .or_else(|_| fs::copy(source, &target).map(|_| ()))
        .map_err(|error| format!("cannot stage {name}: {error}"))?;
    Ok(target)
}

pub(super) fn measure(
    release: &Path,
    fixtures: &Path,
    display: &PrivateDisplay,
    seconds: u64,
) -> Result<Value, String> {
    let release =
        fs::canonicalize(release).map_err(|error| format!("program directory: {error}"))?;
    let profile = Profile::open()?;
    // The owner finds its fetcher beside its executable. Deliberately leave the
    // store/update fetcher out of this stack, even if the release has a catalog key.
    let stage = profile.0.join("programs");
    fs::create_dir(&stage).map_err(|error| format!("cannot stage benchmark programs: {error}"))?;
    let owner_bin = stage_program(&release, &stage, "peersus-test-identity")?;
    let window_bin = stage_program(&release, &stage, "peersus-ui")?;
    let mut workers = Vec::new();
    for (fixture, program) in FIXTURES {
        if fixture == "canvas" && !fixtures.join(fixture).is_dir() {
            // CI stages this fixture only when both revisions have the canvas worker.
            continue;
        }
        let executable = stage_program(&release, &stage, program)?;
        install_fixture(&profile, &owner_bin, &fixtures.join(fixture), fixture)?;
        workers.push((program, executable, None));
    }
    let mut launch = profile.command(&owner_bin);
    launch.args(["--background", "--bind=127.0.0.1", "--allow-relay=false"]);
    let mut owner = Running::start(launch, "peersus test-identity owner")?;
    let owner_pid = owner.0.id();
    until("the isolated owner", || {
        owner.live("peersus test-identity owner")?;
        Ok(profile.0.join("peersus/control.sock").exists())
    })?;
    for (name, path, pid) in &mut workers {
        until(name, || {
            owner.live("peersus test-identity owner")?;
            *pid = helper(owner_pid, path);
            Ok(pid.is_some())
        })?;
    }
    let mut launch = profile.command(&window_bin);
    configure_private_window(&mut launch, display);
    let mut window = Running::start(launch, "peersus-ui")?;
    until("the real peersus-ui window", || {
        owner.live("peersus test-identity owner")?;
        window.live("peersus-ui")?;
        let output = Command::new("xwininfo")
            .env_clear()
            .env("DISPLAY", &display.name)
            .args(["-root", "-tree"])
            .output()
            .map_err(|error| format!("xwininfo is required: {error}"))?;
        Ok(output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("\"Peersus\""))
    })?;
    for (pid, binary) in [(owner_pid, &owner_bin), (window.0.id(), &window_bin)] {
        if !fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|running| running == *binary) {
            return Err("the benchmark process is not the staged executable".into());
        }
    }
    let mut subjects = vec![
        ("peersus-ui", window.0.id(), Usage::default()),
        ("peersus-test-identity", owner_pid, Usage::default()),
    ];
    for (name, _, pid) in &workers {
        subjects.push((
            name,
            pid.ok_or("a WASM helper disappeared before measurement")?,
            Usage::default(),
        ));
    }
    for (name, pid, beginning) in &mut subjects {
        beginning.look(*pid, false, *name == "peersus-ui");
    }
    let beginnings: Vec<_> = subjects
        .iter_mut()
        .map(|(_, _, usage)| {
            let initial = (usage.cpu_ticks, usage.main_thread_ns);
            *usage = Usage::default();
            initial
        })
        .collect();
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut looks = 0u32;
    loop {
        for (name, pid, usage) in &mut subjects {
            usage.look(
                *pid,
                looks.is_multiple_of(MEMORY_EVERY),
                *name == "peersus-ui",
            );
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        looks = looks.wrapping_add(1);
        std::thread::sleep(left.min(LOOK_EVERY));
    }
    owner.live("peersus test-identity owner")?;
    window.live("peersus-ui")?;
    for (name, path, pid) in workers {
        if helper(owner_pid, &path) != pid {
            return Err(format!("{name} stopped or restarted during the sample"));
        }
    }
    let mut report = Map::new();
    for ((name, _, mut usage), initial) in subjects.into_iter().zip(beginnings) {
        usage.cpu_ticks = usage
            .cpu_ticks
            .zip(initial.0)
            .and_then(|(end, start)| end.checked_sub(start));
        usage.main_thread_ns = usage
            .main_thread_ns
            .zip(initial.1)
            .and_then(|(end, start)| end.checked_sub(start));
        let mut values = usage.report();
        if name == "peersus-ui" {
            values["main_thread_ms"] = usage
                .main_thread_ns
                .map(|ns| ns as f64 / 1_000_000.0)
                .into();
        }
        report.insert(name.to_owned(), values);
    }
    Ok(Value::Object(report))
}
