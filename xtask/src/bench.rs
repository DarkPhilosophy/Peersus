//! `cargo xtask bench`: build the measuring programs in release, run them, and keep what they
//! report in one file that a later run can be compared with. `cargo xtask bench-compare`:
//! compare kept runs of a base with kept runs of a change and judge the difference.
//!
//! What is run, each a program of its own that says in its report what it does not measure:
//!
//! * `replay_bench` — the interface's per-snapshot work and each screen built and drawn by
//!   the CPU with no display, with what each asks of the heap;
//! * `benchmark` (the network crate's) — encrypted loopback transfer and history updates;
//! * `demo_window` three times — the real window on the synthetic demo: quiet (a screen with
//!   nothing moving, which is what an idle window costs), still (a screen whose rows show
//!   work going on, on a snapshot that does not change) and with a snapshot that changes 30
//!   times a second. First frame, frame stages, CPU, memory, heap, context switches, disk
//!   bytes.
//!
//! Beside what a program says of itself, each is watched from outside while it runs
//! (`process` in its report): CPU time, peak resident and anonymous memory, threads and,
//! where a graphics driver accounts for it, GPU memory and GPU time. A change cannot alter
//! those by reporting differently. `--sizes` adds the size of each product program.
//!
//! Everything uses synthetic data, with no real profile, device or network beyond loopback.
//! `--measure-programs` also samples a real window, helpers and an ephemeral-identity owner.
//! The numbers compare revisions on ONE machine; what a comparison makes
//! of them, and the limits it judges by, are in [`crate::judge`].

use crate::judge::Comparison;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::OnceLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "linux")]
mod programs;

/// Where a run's reports go; inside the build directory, so never tracked.
const OUTPUT: &str = "target/bench";
/// Kept samples (`--save`). CI can select a job-owned directory outside the checkout.
fn kept_dir() -> PathBuf {
    std::env::var_os("PEERSUS_BENCH_RESULTS")
        .map_or_else(|| PathBuf::from("benchmark-results"), PathBuf::from)
}
/// The programs of a desktop release (the ones `scripts/stage-linux.py` stages), whose sizes
/// `--sizes` reports.
const PRODUCT: [&str; 6] = [
    "peersus",
    "peersus-ui",
    "peersus-wasm-host",
    "peersus-wasm-canvas-host",
    "peersus-native-host",
    "peersus-store-fetch",
];
/// How often a running program is looked at from outside.
const LOOK_EVERY: Duration = Duration::from_millis(20);
/// Every how many looks the memory and the GPU are read too: they cost more to read than
/// the CPU time and move more slowly.
const MEMORY_EVERY: u32 = 5;

struct Options {
    seconds: u64,
    build: bool,
    window: bool,
    on_screen: bool,
    sizes: bool,
    programs: Option<PathBuf>,
    save: Option<String>,
    compare: Option<String>,
    surface_fixture: Option<PathBuf>,
    package_report: Option<PathBuf>,
    program_fixtures: Option<PathBuf>,
}

/// A name of a kept run: a file name of ours and nothing that could leave the folder.
fn kept_name(flag: &str, value: &str) -> Result<String, String> {
    let plain = !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('.')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if plain {
        Ok(value.to_owned())
    } else {
        Err(format!(
            "{flag} takes a name of letters, digits, '-', '_' and '.', not {value:?}"
        ))
    }
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        seconds: 10,
        build: true,
        window: true,
        on_screen: false,
        sizes: false,
        programs: None,
        save: None,
        compare: None,
        surface_fixture: None,
        package_report: None,
        program_fixtures: None,
    };
    for argument in args {
        if let Some(value) = argument.strip_prefix("--seconds=") {
            options.seconds = match value.parse::<u64>() {
                Ok(seconds) if (1..=600).contains(&seconds) => seconds,
                _ => return Err(format!("--seconds takes 1 to 600, not {value:?}")),
            };
        } else if let Some(value) = argument.strip_prefix("--save=") {
            options.save = Some(kept_name("--save", value)?);
        } else if let Some(value) = argument.strip_prefix("--compare=") {
            options.compare = Some(kept_name("--compare", value)?);
        } else if let Some(value) = argument.strip_prefix("--programs=") {
            if value.is_empty() {
                return Err("--programs takes the folder of a release build".to_owned());
            }
            // Programs built elsewhere are measured as they are: nothing is built over them.
            options.programs = Some(PathBuf::from(value));
            options.build = false;
        } else if let Some(value) = argument.strip_prefix("--with-surface=") {
            if value.is_empty() {
                return Err("--with-surface needs the shared Canvas fixture directory".into());
            }
            options.surface_fixture = Some(PathBuf::from(value));
        } else if let Some(value) = argument.strip_prefix("--package-report=") {
            if value.is_empty() {
                return Err("--package-report needs a package measurement JSON file".into());
            }
            options.package_report = Some(PathBuf::from(value));
        } else if let Some(value) = argument.strip_prefix("--measure-programs=") {
            if value.is_empty() {
                return Err("--measure-programs needs the trusted fixture directory".into());
            }
            options.program_fixtures = Some(PathBuf::from(value));
        } else {
            match argument.as_str() {
                "--no-build" => options.build = false,
                "--no-window" => options.window = false,
                "--on-screen" => options.on_screen = true,
                "--sizes" => options.sizes = true,
                other => {
                    return Err(format!(
                        "unknown bench option {other:?}; it takes --seconds=N --save=NAME \
                         --compare=NAME --programs=DIR --sizes --no-build --no-window \
                         --on-screen --with-surface=DIR --measure-programs=DIR"
                    ));
                }
            }
        }
    }
    Ok(options)
}

/// Build the measuring programs, optimized. Separate from running them so that a machine
/// which compiles in a container can still run them where its display is.
pub fn build() -> Result<(), String> {
    crate::cargo(&[
        "build",
        "--release",
        "--locked",
        "-p",
        "peersus-ui",
        "--features",
        "headless",
        "--example",
        "replay_bench",
        "--example",
        "demo_window",
    ])?;
    crate::cargo(&[
        "build",
        "--release",
        "--locked",
        "-p",
        "peersus-net",
        "--example",
        "benchmark",
    ])
}

/// A measuring program of the release build in `release`.
fn program(release: &Path, name: &str) -> Result<PathBuf, String> {
    let path = release
        .join("examples")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "{name} is not in {}; run the bench without --no-build, or `cargo xtask \
             bench-build` under the same CARGO_TARGET_DIR and CARGO_BUILD_TARGET",
            release.display()
        ))
    }
}

/// An X server of the run's own, drawing into memory: every run sees the same screen, and
/// the person's session is neither shown a window nor asked for one. Ended when dropped.
struct PrivateDisplay {
    server: Child,
    name: String,
}

impl PrivateDisplay {
    fn open() -> Result<Self, String> {
        let mut server = Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "1280x800x24",
                "-nolisten",
                "tcp",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| {
                "no private display: Xvfb is not installed (use --on-screen for the session's \
                 display, or --no-window to leave the window out)"
                    .to_owned()
            })?;
        // The server writes its display number, once it listens, and a line end.
        let mut number = String::new();
        let read = server
            .stdout
            .take()
            .map(|stdout| BufReader::new(stdout).read_line(&mut number));
        let number = number.trim();
        if !matches!(read, Some(Ok(_))) || number.is_empty() || number.parse::<u32>().is_err() {
            let _ = server.kill();
            let _ = server.wait();
            return Err("the private display did not start".to_owned());
        }
        Ok(Self {
            name: format!(":{number}"),
            server,
        })
    }
}

impl Drop for PrivateDisplay {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}

fn configure_private_window(command: &mut Command, display: &PrivateDisplay) {
    command
        .env("DISPLAY", &display.name)
        .env("XAUTHORITY", "/dev/null")
        .env("FREYA_RENDERER", "software")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/dev/null")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET");
}

/// The number after `key` at the start of a line of one of the kernel's `key: value` files.
fn field(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(key))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The CPU time of a whole process in the kernel's ticks, from its `stat` line. The name of
/// the program, which may hold anything, ends at the last `)`.
fn cpu_ticks(stat: &str) -> Option<u64> {
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
    // After the name: the state, then ten fields, then the user and the system time.
    let user: u64 = fields.nth(11)?.parse().ok()?;
    let system: u64 = fields.next()?.parse().ok()?;
    Some(user + system)
}

/// How many of the kernel's ticks make a second, as the system says (`getconf CLK_TCK`), asked
/// once. Not assumed: where it cannot be asked, a CPU time read in ticks is left out.
fn ticks_per_second() -> Option<u64> {
    static TICKS: OnceLock<Option<u64>> = OnceLock::new();
    *TICKS.get_or_init(|| {
        let asked = Command::new("getconf").arg("CLK_TCK").output().ok()?;
        String::from_utf8_lossy(&asked.stdout)
            .trim()
            .parse()
            .ok()
            .filter(|ticks| *ticks > 0)
    })
}

/// What a graphics driver accounts to one file of a process (`fdinfo`): which of the
/// driver's clients it is, the memory it holds in KiB and the time its engines worked in
/// nanoseconds. A driver names its memory by region; a newer kernel also gives a total for
/// each region, which then is the figure and not a second one to add.
fn gpu_client(fdinfo: &str) -> Option<(String, Option<u64>, Option<u64>)> {
    let mut client = None;
    let mut regions = BTreeMap::<&str, (Option<u64>, Option<u64>)>::new();
    let mut engines: Option<u64> = None;
    for line in fdinfo.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let mut words = value.split_whitespace();
        let number = words.next().and_then(|number| number.parse::<u64>().ok());
        if key == "drm-client-id" {
            client = Some(value.trim().to_owned());
            continue;
        }
        let Some(number) = number else { continue };
        let unit = words.next();
        if key.starts_with("drm-engine-") && !key.starts_with("drm-engine-capacity-") {
            if unit == Some("ns") {
                engines = Some(engines.unwrap_or(0).saturating_add(number));
            }
        } else {
            let kib = match unit {
                Some("KiB") => number,
                Some("MiB") => number.saturating_mul(1024),
                Some("B") | None => number / 1024,
                _ => continue,
            };
            if let Some(region) = key.strip_prefix("drm-total-") {
                regions.entry(region).or_default().0 = Some(kib);
            } else if let Some(region) = key.strip_prefix("drm-memory-") {
                regions.entry(region).or_default().1 = Some(kib);
            }
        }
    }
    let memory = regions
        .into_values()
        .filter_map(|(total, used)| total.or(used))
        .reduce(u64::saturating_add);
    Some((client?, memory, engines))
}

/// What the operating system counted for a measured program, read from outside it while it
/// ran. Each is absent where the platform, or a driver, has no such count: never a zero.
#[derive(Default)]
struct Usage {
    cpu_ticks: Option<u64>,
    main_thread_ns: Option<u64>,
    peak_resident_kib: Option<u64>,
    peak_pss_kib: Option<u64>,
    peak_anonymous_kib: Option<u64>,
    threads: Option<u64>,
    gpu_memory_kib: Option<u64>,
    gpu_nanoseconds: Option<u64>,
}

impl Usage {
    /// Look at the process once. Its CPU time is exact at the last look: a process that
    /// ended keeps its total until it is waited for. Its memory is the most any look saw.
    fn look(&mut self, process: u32, memory_too: bool, main_thread: bool) {
        let folder = PathBuf::from(format!("/proc/{process}"));
        if let Some(ticks) = std::fs::read_to_string(folder.join("stat"))
            .ok()
            .as_deref()
            .and_then(cpu_ticks)
        {
            self.cpu_ticks = Some(ticks);
        }
        if main_thread {
            self.main_thread_ns = std::fs::read_to_string(folder.join("schedstat"))
                .ok()
                .and_then(|text| text.split_whitespace().next()?.parse().ok())
                .max(self.main_thread_ns);
        }
        if !memory_too {
            return;
        }
        let status = std::fs::read_to_string(folder.join("status")).unwrap_or_default();
        for (count, key) in [
            (&mut self.peak_resident_kib, "VmHWM:"),
            (&mut self.peak_anonymous_kib, "RssAnon:"),
            (&mut self.threads, "Threads:"),
        ] {
            *count = field(&status, key).max(*count);
        }
        self.peak_pss_kib = std::fs::read_to_string(folder.join("smaps_rollup"))
            .ok()
            .and_then(|text| field(&text, "Pss:"))
            .max(self.peak_pss_kib);
        // One client of a driver may be open through several files: counted once.
        let mut clients = BTreeMap::new();
        for file in std::fs::read_dir(folder.join("fdinfo"))
            .into_iter()
            .flatten()
            .flatten()
        {
            if let Some((client, kib, nanoseconds)) = std::fs::read_to_string(file.path())
                .ok()
                .as_deref()
                .and_then(gpu_client)
            {
                clients.insert(client, (kib, nanoseconds));
            }
        }
        if !clients.is_empty() {
            // A missing counter is not zero. Do not report a partial sum across clients.
            let kib = clients
                .values()
                .try_fold(0u64, |sum, (kib, _)| Some(sum.saturating_add((*kib)?)));
            let ns = clients
                .values()
                .try_fold(0u64, |sum, (_, ns)| Some(sum.saturating_add((*ns)?)));
            self.gpu_memory_kib = kib.max(self.gpu_memory_kib);
            self.gpu_nanoseconds = ns.max(self.gpu_nanoseconds);
        }
    }

    fn report(&self) -> serde_json::Value {
        serde_json::json!({
            "read_from": "outside the program, by the bench",
            "cpu_ms": self
                .cpu_ticks
                .zip(ticks_per_second())
                .map(|(ticks, per_second)| ticks as f64 * 1000.0 / per_second as f64),
            "peak_resident_kib": self.peak_resident_kib,
            "peak_pss_kib": self.peak_pss_kib,
            "peak_anonymous_kib": self.peak_anonymous_kib,
            "threads": self.threads,
            "gpu_memory_kib": self.gpu_memory_kib,
            "gpu_ms": self.gpu_nanoseconds.map(|nanoseconds| nanoseconds as f64 / 1_000_000.0),
        })
    }
}

/// Wait for a measured program to end, looking at it from outside meanwhile. A program that
/// runs longer than it could rightly take is ended.
fn watched(
    label: &str,
    program: &mut Child,
    limit: Duration,
    window_main_thread: bool,
) -> Result<(ExitStatus, Usage), String> {
    let began = Instant::now();
    let mut usage = Usage::default();
    let mut looks = 0u32;
    loop {
        usage.look(
            program.id(),
            looks.is_multiple_of(MEMORY_EVERY),
            window_main_thread,
        );
        looks = looks.wrapping_add(1);
        match program.try_wait() {
            Ok(Some(status)) => return Ok((status, usage)),
            Ok(None) if began.elapsed() < limit => std::thread::sleep(LOOK_EVERY),
            _ => {
                let _ = program.kill();
                let _ = program.wait();
                return Err(format!("{label}: it did not end by itself; it was ended"));
            }
        }
    }
}

/// Run a program that prints one JSON report on standard output.
fn printed(release: &Path, label: &str, name: &str) -> Result<serde_json::Value, String> {
    eprintln!("+ {name}");
    // Into a file, not a pipe: a program is never made to wait for its report to be read.
    let report = Path::new(OUTPUT).join(format!("{label}.json"));
    let output =
        File::create(&report).map_err(|error| format!("{}: {}", report.display(), error.kind()))?;
    // No desktop is given to these programs: none of them shows anything, and the toolkit's test runner would otherwise
    // connect a clipboard for every window it builds and keep it (a thread and two connections, about 59 KB of heap each)
    // until the program ends, which would be counted as heap the application did not give back.
    let mut running = Command::new(program(release, name)?)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .stdin(Stdio::null())
        .stdout(output)
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("cannot run {name}: {}", error.kind()))?;
    let (status, usage) = watched(label, &mut running, Duration::from_secs(600), false)?;
    if !status.success() {
        return Err(format!("{name} exited with {status}"));
    }
    let mut report: serde_json::Value = std::fs::read(&report)
        .ok()
        .and_then(|text| serde_json::from_slice(&text).ok())
        .filter(serde_json::Value::is_object)
        .ok_or_else(|| format!("{name} printed no JSON report"))?;
    report["process"] = usage.report();
    Ok(report)
}

/// Run the demo window's probe and read the report it wrote. The probe ends itself; a
/// window that does not is ended here, after longer than it could rightly take.
fn probed(
    release: &Path,
    label: &str,
    args: &[&str],
    seconds: u64,
    display: Option<&PrivateDisplay>,
) -> Result<serde_json::Value, String> {
    let report = Path::new(OUTPUT).join(format!("{label}.json"));
    let _ = std::fs::remove_file(&report);
    let mut command = Command::new(program(release, "demo_window")?);
    command
        .arg("--demo")
        .arg(format!("--demo-probe={seconds}"))
        .arg(format!("--demo-probe-out={}", report.display()))
        .args(args)
        .stdin(Stdio::null());
    if let Some(display) = display {
        configure_private_window(&mut command, display);
    }
    eprintln!(
        "+ demo_window --demo --demo-probe={seconds} {}",
        args.join(" ")
    );
    let mut window = command
        .spawn()
        .map_err(|error| format!("cannot run demo_window: {}", error.kind()))?;
    // The probe gives a window a minute to present its first frame, then measures.
    let (status, usage) = watched(label, &mut window, Duration::from_secs(seconds + 90), true)?;
    let text = std::fs::read(&report)
        .map_err(|_| format!("{label}: the window ended ({status}) without a report"))?;
    let mut report: serde_json::Value =
        serde_json::from_slice(&text).map_err(|_| format!("{label}: the report is not JSON"))?;
    if let Some(error) = report.get("error").and_then(serde_json::Value::as_str) {
        return Err(format!("{label}: {error}"));
    }
    // A window that wrote its report and then crashed is not a clean run: its numbers are
    // not kept as if it were.
    if !status.success() {
        return Err(format!(
            "{label}: the window wrote its report and then ended badly ({status})"
        ));
    }
    report["process"] = usage.report();
    report["process"]["main_thread_ms"] = usage
        .main_thread_ns
        .map(|ns| ns as f64 / 1_000_000.0)
        .into();
    Ok(report)
}

/// The size of each product program in `release`. One that is not there is said, not left
/// out: a size nobody measured must not read as a size that did not change.
fn sizes(release: &Path) -> serde_json::Value {
    let mut sizes = serde_json::Map::new();
    for name in PRODUCT {
        let path = release.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        let bytes = std::fs::metadata(path).ok().map(|file| file.len());
        sizes.insert(name.to_owned(), serde_json::json!({ "file_bytes": bytes }));
    }
    sizes.into()
}

/// The revision the numbers are of, and whether the working tree differs from it.
fn revision() -> serde_json::Value {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    serde_json::json!({
        "commit": git(&["rev-parse", "--short=12", "HEAD"]),
        "uncommitted_changes": git(&["status", "--porcelain"]).map(|status| !status.is_empty()),
    })
}

/// A kept run, read back.
fn kept(name: &str) -> Result<serde_json::Value, String> {
    let text = std::fs::read(kept_dir().join(format!("{name}.json"))).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!(
                "sample `{name}` was not saved: its measurement failed before writing a result (see the sampling step's log)"
            )
        } else {
            format!("kept run `{name}` could not be read ({})", error.kind())
        }
    })?;
    let report: serde_json::Value = serde_json::from_slice(&text)
        .map_err(|_| format!("kept run `{name}` is not valid JSON"))?;
    if !report.is_object() {
        return Err(format!("kept run `{name}` is not a benchmark object"));
    }
    Ok(report)
}

/// An opt-in scenario of the native window, separate from the default synthetic replay.
/// `--programs` lets the private display run on a host with binaries built in a container.
fn map_zoom(manifest: &str, args: &[String]) -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("the map zoom scenario needs Linux CPU counters and Xvfb".to_owned());
    }
    let mut module = None;
    let mut programs = None;
    let mut pacing = Vec::new();
    for arg in args {
        if let Some(value) = arg.strip_prefix("--map-wasm=") {
            module = Some(value);
        } else if let Some(value) = arg.strip_prefix("--programs=") {
            programs = Some(PathBuf::from(value));
        } else if arg.starts_with("--steps=") || arg.starts_with("--interval-ms=") {
            pacing.push(arg);
        } else {
            return Err(format!("unknown map zoom option {arg:?}"));
        }
    }
    let module = module.ok_or("--map-zoom=MANIFEST also needs --map-wasm=MODULE")?;
    if !Path::new(manifest).is_file() || !Path::new(module).is_file() {
        return Err("the map bench needs a Location manifest and built Wasm file".to_owned());
    }
    if programs.is_none() {
        crate::cargo(&[
            "build",
            "--release",
            "--locked",
            "-p",
            "peersus-ui",
            "-p",
            "peersus-plugin-canvas",
            "--features",
            "peersus-ui/demo",
            "--example",
            "demo_window",
            "--bin",
            "peersus-wasm-canvas-host",
        ])?;
    }
    let release = programs.unwrap_or_else(|| crate::output_directory("release"));
    let window = program(&release, "demo_window")?;
    let worker = release.join("peersus-wasm-canvas-host");
    if !worker.is_file() {
        return Err(
            "the map bench needs peersus-wasm-canvas-host in the release build directory"
                .to_owned(),
        );
    }
    let private = PrivateDisplay::open()?;
    std::fs::create_dir_all(OUTPUT).map_err(|error| format!("{OUTPUT}: {}", error.kind()))?;
    let mut summary = serde_json::Map::new();
    for span in ["regional", "street"] {
        let label = format!("map-zoom-{span}");
        let report = Path::new(OUTPUT).join(format!("{label}.json"));
        let output = File::create(&report).map_err(|error| format!("{label}: {}", error.kind()))?;
        let mut child = Command::new(&window)
            .args(["--map-zoom", span, manifest, module])
            .arg(&worker)
            .args(&pacing)
            .env("DISPLAY", &private.name)
            .env("XAUTHORITY", "/dev/null")
            .env("FREYA_RENDERER", "software")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/dev/null")
            .stdin(Stdio::null())
            .stdout(output)
            .spawn()
            .map_err(|error| format!("cannot run {label}: {}", error.kind()))?;
        let (status, usage) = watched(&label, &mut child, Duration::from_secs(420), false)?;
        if !status.success() {
            return Err(format!("{label} exited with {status}"));
        }
        let contents =
            std::fs::read(&report).map_err(|error| format!("{label}: {}", error.kind()))?;
        let mut data: serde_json::Value = serde_json::from_slice(&contents)
            .map_err(|_| format!("{label} produced no valid report"))?;
        data["window_process"] = usage.report();
        summary.insert(span.to_owned(), data);
    }
    let path = Path::new(OUTPUT).join("map-zoom.json");
    std::fs::write(&path, format!("{:#}\n", serde_json::Value::Object(summary)))
        .map_err(|error| format!("map zoom report: {}", error.kind()))?;
    eprintln!("report: {}", path.display());
    Ok(())
}

const CANVAS_MANIFEST: &str = "templates/canvas-guest/plugin.json";
const CANVAS_GUEST: &str = "target/canvas-guest/wasm32-unknown-unknown/release/canvas_guest.wasm";

pub(crate) fn build_canvas_guest() -> Result<(), String> {
    crate::cargo(&[
        "build",
        "--locked",
        "--release",
        "--manifest-path",
        "templates/canvas-guest/Cargo.toml",
        "--target",
        "wasm32-unknown-unknown",
        "--target-dir",
        "target/canvas-guest",
    ])
}

pub(crate) fn build_hello_guest() -> Result<(), String> {
    crate::cargo(&[
        "build",
        "--locked",
        "--release",
        "--manifest-path",
        "templates/hello-guest/Cargo.toml",
        "--target",
        "wasm32-unknown-unknown",
        "--target-dir",
        "target/hello-guest",
    ])
}

pub fn build_guest_window() -> Result<(), String> {
    crate::cargo(&[
        "build",
        "--release",
        "--locked",
        "-p",
        "peersus-ui",
        "-p",
        "peersus-plugin-canvas",
        "-p",
        "peersus-plugin-runtime",
        "--features",
        "peersus-ui/demo",
        "--example",
        "demo_window",
        "--bin",
        "peersus-wasm-host",
        "--bin",
        "peersus-wasm-canvas-host",
    ])
}

fn measure_guest_window(
    mut command: Command,
    label: &str,
    limit: Duration,
    display: Option<&PrivateDisplay>,
) -> Result<serde_json::Value, String> {
    std::fs::create_dir_all(OUTPUT).map_err(|error| format!("{OUTPUT}: {}", error.kind()))?;
    let path = Path::new(OUTPUT).join(format!("{label}.json"));
    let output =
        File::create(&path).map_err(|error| format!("{label} report: {}", error.kind()))?;
    command
        .env("FREYA_RENDERER", "software")
        .stdin(Stdio::null())
        .stdout(output);
    if let Some(display) = display {
        configure_private_window(&mut command, display);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot launch {label} bench: {}", error.kind()))?;
    let (status, _) = watched(label, &mut child, limit, false)?;
    if !status.success() {
        return Err(format!("{label} bench exited with {status}"));
    }
    let data = std::fs::read(path).map_err(|error| format!("{label}: {}", error.kind()))?;
    serde_json::from_slice(&data)
        .ok()
        .filter(serde_json::Value::is_object)
        .ok_or_else(|| format!("{label} produced no valid report"))
}

fn measure_canvas(
    release: &Path,
    seconds: u64,
    fixture: Option<&Path>,
    display: Option<&PrivateDisplay>,
) -> Result<serde_json::Value, String> {
    let (manifest, module) = fixture.map_or_else(
        || (PathBuf::from(CANVAS_MANIFEST), PathBuf::from(CANVAS_GUEST)),
        |dir| (dir.join("plugin.json"), dir.join("canvas_guest.wasm")),
    );
    let worker = release.join("peersus-wasm-canvas-host");
    if !module.is_file() || !manifest.is_file() || !worker.is_file() {
        return Err("missing Canvas fixture or WASM Canvas helper".into());
    }
    let screenshot = Path::new(OUTPUT).join("canvas.png");
    let mut command = Command::new(program(release, "demo_window")?);
    command
        .arg("--canvas")
        .arg(manifest)
        .arg(module)
        .arg(worker)
        .arg(seconds.to_string())
        .arg(&screenshot)
        .env("PEERSUS_PAGE", "extensions");
    let report = measure_guest_window(
        command,
        "canvas",
        Duration::from_secs(3 * seconds + 90),
        display,
    )?;
    eprintln!(
        "canvas report: {OUTPUT}/canvas.json; composed window: {}",
        screenshot.display()
    );
    Ok(report)
}

fn canvas(args: &[String]) -> Result<(), String> {
    let options = options(args)?;
    if !cfg!(target_os = "linux")
        || options.seconds > 60
        || !options.window
        || options.sizes
        || options.compare.is_some()
        || options.package_report.is_some()
        || options.program_fixtures.is_some()
    {
        return Err(
            "--surface needs Linux and takes --seconds=1..60, --programs=DIR, \
                    --with-surface=FIXTURE, --save=NAME, --no-build or --on-screen"
                .into(),
        );
    }
    if options.build {
        if options.surface_fixture.is_none() {
            build_canvas_guest()?;
        }
        build_guest_window()?;
    }
    let release = options
        .programs
        .unwrap_or_else(|| crate::output_directory("release"));
    let private = (!options.on_screen)
        .then(PrivateDisplay::open)
        .transpose()?;
    let report = measure_canvas(
        &release,
        options.seconds,
        options.surface_fixture.as_deref(),
        private.as_ref(),
    )?;
    if let Some(name) = options.save {
        save(
            &name,
            &serde_json::json!({
                "canvas": report,
                "workload": {"profile": "canvas", "duration_s": options.seconds, "private_display": private.is_some()},
            }),
        )?;
    }
    println!("{report:#}");
    Ok(())
}

fn read_package_measurements(path: &Path) -> Result<serde_json::Value, String> {
    let raw =
        std::fs::read(path).map_err(|error| format!("package measurement: {}", error.kind()))?;
    let mut report: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| "the package measurement is not valid JSON")?;
    let package = report
        .get_mut("package")
        .filter(|package| {
            package
                .as_object()
                .is_some_and(|packages| !packages.is_empty())
        })
        .ok_or("the package measurement has no packages")?;
    Ok(package.take())
}

fn agreement_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let raw = std::fs::read(path).map_err(|error| format!("manifest: {}", error.kind()))?;
    let package = peersus_api::package::split(&raw).map_err(|_| "invalid package manifest")?;
    let manifest: peersus_api::manifest::Manifest =
        serde_json::from_slice(&package.manifest).map_err(|_| "invalid package manifest")?;
    let agreement = manifest.agreements.ok_or("missing licence and notice")?;
    let canonical = serde_json::to_vec(&agreement).map_err(|_| "invalid licence and notice")?;
    Ok(hex::encode(Sha256::digest(canonical)))
}

fn extension(inputs: &Path, args: &[String]) -> Result<(), String> {
    let options = options(args)?;
    if !cfg!(target_os = "linux")
        || !options.window
        || options.on_screen
        || options.sizes
        || options.seconds > 60
        || options.surface_fixture.is_some()
        || options.compare.is_some()
        || options.program_fixtures.is_some()
    {
        return Err(
            "--extension needs Linux and private Xvfb; it takes --seconds=1..60, \
                    --save=NAME, --package-report=JSON, --programs=DIR or --no-build"
                .into(),
        );
    }
    let (manifest, module) = (inputs.join("plugin.json"), inputs.join("guest.wasm"));
    if inputs.as_os_str().is_empty() || !manifest.is_file() || !module.is_file() {
        return Err(
            "--extension needs a directory with the reviewed plugin.json and guest.wasm".into(),
        );
    }
    let agreement = agreement_sha256(&manifest)?;
    let package = options
        .package_report
        .as_deref()
        .map(read_package_measurements)
        .transpose()?;
    if options.build {
        build_guest_window()?;
    }
    let release = options
        .programs
        .unwrap_or_else(|| crate::output_directory("release"));
    let private = PrivateDisplay::open()?;
    // The process that interprets a submitted guest must see neither CI credentials nor a
    // local session, even when the caller happens to have them in its environment.
    let mut command = Command::new(program(&release, "demo_window")?);
    command
        .env_clear()
        .arg("--extension-bench")
        .arg(manifest)
        .arg(module)
        .arg(release)
        .arg(options.seconds.to_string());
    let mut measured = measure_guest_window(
        command,
        "extension",
        Duration::from_secs(options.seconds + 90),
        Some(&private),
    )?;
    let helper = match measured["helper_program"].as_str() {
        Some(name @ ("peersus-wasm-host" | "peersus-wasm-canvas-host")) => name.to_owned(),
        _ => return Err("the extension sample did not identify its executed WASM helper".into()),
    };
    let helper_usage = measured["wasm_helper"].take();
    let mut report = serde_json::json!({
        "schema": 3,
        "workload": {"profile": "extension-startup-idle", "duration_s": options.seconds, "private_display": true},
        "agreement_sha256": agreement,
        "extension": measured,
    });
    report["programs"][helper] = helper_usage;
    if let Some(package) = package {
        report["package"] = package;
    }
    if let Some(name) = options.save {
        save(&name, &report)?;
    }
    println!("{report:#}");
    Ok(())
}

fn save(name: &str, report: &serde_json::Value) -> Result<(), String> {
    let folder = kept_dir();
    std::fs::create_dir_all(&folder)
        .map_err(|error| format!("kept directory: {}", error.kind()))?;
    let path = folder.join(format!("{name}.json"));
    std::fs::write(&path, format!("{report:#}\n"))
        .map_err(|error| format!("kept run {name}: {}", error.kind()))?;
    eprintln!("kept: {}", path.display());
    Ok(())
}

pub fn run(args: &[String]) -> Result<(), String> {
    if let Some((selection, args)) = args.split_first() {
        if selection == "--surface" {
            return canvas(args);
        }
        if let Some(manifest) = selection.strip_prefix("--map-zoom=") {
            return map_zoom(manifest, args);
        }
        if let Some(inputs) = selection.strip_prefix("--extension=") {
            return extension(Path::new(inputs), args);
        }
    }
    let options = options(args)?;
    if options.package_report.is_some() {
        return Err("--package-report is only for --extension".into());
    }
    if options.surface_fixture.is_some()
        && (!cfg!(target_os = "linux") || !options.window || options.seconds > 60)
    {
        return Err("--with-surface needs a Linux window and --seconds=1..60".into());
    }
    if options.program_fixtures.is_some()
        && (!cfg!(target_os = "linux")
            || !options.window
            || options.on_screen
            || options.programs.is_none())
    {
        return Err(
            "--measure-programs requires Linux, private Xvfb and staged --programs=DIR".into(),
        );
    }
    // A comparison that cannot be made is said before minutes are spent measuring.
    let before = options.compare.as_deref().map(kept).transpose()?;
    if options.build {
        build()?;
        if options.sizes {
            crate::task("build")?;
        }
        if options.surface_fixture.is_some() && !options.sizes {
            build_guest_window()?;
        }
    }
    std::fs::create_dir_all(OUTPUT).map_err(|error| format!("{OUTPUT}: {}", error.kind()))?;
    let release = options
        .programs
        .clone()
        .unwrap_or_else(|| crate::output_directory("release"));

    let mut report = serde_json::json!({
        "schema": 3,
        "workload": {
            "profile": "application",
            "duration_s": options.window.then_some(options.seconds),
            "private_display": options.window && cfg!(target_os = "linux") && !options.on_screen,
            "canvas": options.surface_fixture.is_some(),
            "programs": options.program_fixtures.is_some(),
            "sizes": options.sizes,
        },
        "taken_unix": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
        // Programs built elsewhere are of the revision that built them, which is not known here.
        "revision": if options.programs.is_some() { serde_json::Value::Null } else { revision() },
        "replay": printed(&release, "replay", "replay_bench")?,
        "transport": printed(&release, "transport", "benchmark")?,
    });
    if options.sizes {
        report["size"] = sizes(&release);
    }
    if options.window {
        let private = if options.on_screen || !cfg!(target_os = "linux") {
            None
        } else {
            Some(PrivateDisplay::open()?)
        };
        report["display"] = if private.is_some() {
            "private X server drawing into memory"
        } else {
            "the session's own display"
        }
        .into();
        let sized = ["--demo-peers=16", "--demo-transfers=128"];
        for (key, label, more) in [
            ("window_quiet", "window-quiet", &["--demo-page=send"][..]),
            (
                "window_still",
                "window-still",
                &["--demo-page=transfers"][..],
            ),
            (
                "window_replay",
                "window-replay",
                &["--demo-page=transfers", "--demo-replay=30"][..],
            ),
        ] {
            report[key] = probed(
                &release,
                label,
                &[&sized[..], more].concat(),
                options.seconds,
                private.as_ref(),
            )?;
        }
        if let Some(fixture) = options.surface_fixture.as_deref() {
            report["canvas"] =
                measure_canvas(&release, options.seconds, Some(fixture), private.as_ref())?;
        }
        #[cfg(target_os = "linux")]
        if let Some(fixtures) = options.program_fixtures.as_deref() {
            let display = private
                .as_ref()
                .ok_or("program measurements require private Xvfb")?;
            report["programs"] = programs::measure(&release, fixtures, display, options.seconds)?;
        }
    }

    let text = format!("{report:#}\n");
    let latest = Path::new(OUTPUT).join("latest.json");
    std::fs::write(&latest, &text).map_err(|error| format!("{OUTPUT}: {}", error.kind()))?;
    eprintln!("report: {}", latest.display());
    if let Some(name) = &options.save {
        save(name, &report)?;
    }
    if let Some(before) = before {
        // One run against one kept run shows the verdict and fails nothing: with the noise
        // unknown it is a reading, and `bench-compare` is what decides.
        for line in Comparison::of(&[before], &[report]).lines() {
            println!("{line}");
        }
    }
    Ok(())
}

/// `cargo xtask bench-compare --base=NAME[,NAME…] --head=NAME[,NAME…] [--markdown=PATH]`:
/// judge kept runs of a change against kept runs of its base. Regression and incomplete
/// samples are reported even when the command cannot pass.
pub fn compare(args: &[String]) -> Result<(), String> {
    let (mut base, mut head, mut markdown) = (Vec::new(), Vec::new(), None);
    let mut no_baseline = false;
    for argument in args {
        let names = |flag: &str, names: &str| {
            names
                .split(',')
                .map(|name| kept_name(flag, name))
                .collect::<Result<Vec<_>, _>>()
        };
        if let Some(runs) = argument.strip_prefix("--base=") {
            base = names("--base", runs)?;
        } else if let Some(runs) = argument.strip_prefix("--head=") {
            head = names("--head", runs)?;
        } else if let Some(path) = argument.strip_prefix("--markdown=") {
            markdown = Some(PathBuf::from(path));
        } else if argument == "--no-baseline" {
            no_baseline = true;
        } else {
            return Err(format!(
                "unknown bench-compare option {argument:?}; it takes --base=NAME[,NAME…] \
                 or --no-baseline, --head=NAME[,NAME…], and --markdown=PATH"
            ));
        }
    }
    if base.is_empty() != no_baseline || head.is_empty() {
        return Err("bench-compare needs --head= and either --base= or --no-baseline".into());
    }
    let mut names = BTreeSet::new();
    if !base.iter().chain(&head).all(|name| names.insert(name)) {
        return Err("each sample must have a distinct kept run name".into());
    }
    let mut failures = Vec::new();
    if no_baseline {
        failures.push(
            "No baseline exists for this comparison; submitted measurements are absolute only."
                .into(),
        );
    }
    let mut workload = None;
    let mut load = |names: &[String]| -> Vec<_> {
        names
            .iter()
            .map(|name| match kept(name) {
                Ok(report) => {
                    let profile = report.get("workload").cloned().unwrap_or_default();
                    if let Some(workload) = &workload {
                        if *workload != profile {
                            failures.push(format!(
                                "Kept run `{name}` used a different workload configuration."
                            ));
                            return serde_json::Value::Null;
                        }
                    } else {
                        workload = Some(profile);
                    }
                    report
                }
                Err(error) => {
                    failures.push(error);
                    serde_json::Value::Null
                }
            })
            .collect()
    };
    let (base, head) = (load(&base), load(&head));
    let comparison = Comparison::of(&base, &head).with_failures(failures);
    for line in comparison.lines() {
        println!("{line}");
    }
    if let Some(path) = markdown {
        std::fs::write(&path, comparison.markdown())
            .map_err(|error| format!("{}: {}", path.display(), error.kind()))?;
    }
    if comparison.blocks() {
        Err(comparison.summary())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_known_options_and_plain_names_are_taken() {
        let taken = options(&[
            "--seconds=3".to_owned(),
            "--no-build".to_owned(),
            "--save=before-fix_1.2".to_owned(),
        ])
        .expect("options");
        assert_eq!(taken.seconds, 3);
        assert!(!taken.build && taken.window && !taken.on_screen && !taken.sizes);
        assert_eq!(taken.save.as_deref(), Some("before-fix_1.2"));
        // Programs built elsewhere are never built over.
        let elsewhere =
            options(&["--programs=/tmp/base".to_owned(), "--sizes".to_owned()]).expect("options");
        assert!(!elsewhere.build && elsewhere.sizes);
        assert_eq!(elsewhere.programs.as_deref(), Some(Path::new("/tmp/base")));
        for bad in [
            "--seconds=0",
            "--seconds=601",
            "--save=",
            "--save=../outside",
            "--save=.hidden",
            "--compare=a/b",
            "--programs=",
            "--fast",
        ] {
            assert!(options(&[bad.to_owned()]).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn the_cpu_time_of_a_process_is_read_past_any_program_name() {
        // A name may hold spaces and brackets; the fields are counted from the last one.
        let stat = "4242 (a b) c) S 1 4242 4242 0 -1 4194304 900 0 3 0 150 25 0 0 20 0 9 0 77";
        assert_eq!(cpu_ticks(stat), Some(175));
        assert_eq!(cpu_ticks("4242 (cut"), None);
    }

    #[test]
    fn a_graphics_client_is_its_memory_once_and_its_engine_time() {
        // A driver that names regions only.
        let regions = "pos:\t0\ndrm-driver:\tamdgpu\ndrm-client-id:\t206\n\
                       drm-memory-vram:\t100908 KiB\ndrm-memory-gtt: \t14852 KiB\n\
                       drm-resident-vram:\t100908 KiB\ndrm-engine-gfx:\t1109960259 ns\n";
        assert_eq!(
            gpu_client(regions),
            Some(("206".to_owned(), Some(115_760), Some(1_109_960_259)))
        );
        // A kernel that also totals each region: the total is the figure, not added to it.
        let totals = "drm-client-id: 7\ndrm-memory-vram: 10 MiB\ndrm-total-vram: 10 MiB\n\
                      drm-total-system0: 2097152\ndrm-engine-capacity-gfx: 2\n";
        assert_eq!(
            gpu_client(totals),
            Some(("7".to_owned(), Some(12_288), None))
        );
        let mixed = "drm-client-id: 7\ndrm-total-vram: 0 KiB\ndrm-memory-vram: 10 MiB\ndrm-memory-system: 2 MiB\ndrm-engine-render: 0 ns\n";
        assert_eq!(
            gpu_client(mixed),
            Some(("7".to_owned(), Some(2048), Some(0)))
        );
        assert_eq!(
            gpu_client("drm-client-id: 7\n"),
            Some(("7".to_owned(), None, None))
        );
        // A file that is not a graphics client's.
        assert_eq!(gpu_client("pos:\t0\nflags:\t02\n"), None);
    }

    #[test]
    fn a_comparison_takes_plain_names_of_both_sides_and_nothing_else() {
        for bad in [
            &["--base=a"][..],
            &["--head=a"][..],
            &["--base=../a", "--head=b"][..],
            &["--base=a,", "--head=b"][..],
            &["--base=a,a", "--head=b"][..],
            &["--base=a", "--head=a"][..],
            &["--base=a", "--no-baseline", "--head=b"][..],
            &["--base=a", "--head=b", "--strict"][..],
        ] {
            let args: Vec<String> = bad.iter().map(|&argument| argument.to_owned()).collect();
            let refused = compare(&args).expect_err("refused");
            // Refused for what was asked, before any kept run is looked for.
            assert!(!refused.starts_with("INCONCLUSIVE"), "{bad:?}: {refused}");
        }
    }

    #[test]
    fn a_missing_run_still_writes_an_inconclusive_markdown_report() {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(format!("peersus-bench-{unique}.md"));
        let status = compare(&[
            format!("--base=absent-base-{unique}"),
            format!("--head=absent-head-{unique}"),
            format!("--markdown={}", path.display()),
        ]);
        let report = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(status.is_err());
        assert!(report.starts_with("## Resource impact: INCONCLUSIVE"));
        assert!(report.contains(&format!(
            "sample `absent-base-{unique}` was not saved: its measurement failed"
        )));
        assert!(report.contains(&format!(
            "sample `absent-head-{unique}` was not saved: its measurement failed"
        )));
        assert!(report.contains("### No numbers"));
        assert!(!report.contains("| Metric / method |"));
        assert!(!report.contains("not measured — this executable"));
    }
}
