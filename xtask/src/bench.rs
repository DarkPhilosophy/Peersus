//! `cargo xtask bench`: build the measuring programs in release, run them, and keep what they
//! report in one file that a later run can be compared with.
//!
//! What is run, each a program of its own that says in its report what it does not measure:
//!
//! * `replay_bench` — the interface's per-snapshot work and each screen built and drawn by
//!   the CPU with no display;
//! * `benchmark` (the network crate's) — encrypted loopback transfer and history updates;
//! * `demo_window` three times — the real window on the synthetic demo: quiet (a screen with
//!   nothing moving, which is what an idle window costs), still (a screen whose rows show
//!   work going on, on a snapshot that does not change) and with a snapshot that changes 30
//!   times a second. First frame, frame stages, CPU, memory, context switches, disk bytes.
//!
//! Everything is synthetic and local: no owner process, no saved state, no network beyond
//! loopback, no device. The numbers compare revisions on ONE machine; a comparison prints the
//! change and judges nothing, because noise between two runs is not known to this tool.

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Where a run's reports go; inside the build directory, so never tracked.
const OUTPUT: &str = "target/bench";
/// Where a run kept on purpose goes (`--save`); ignored by version control.
const KEPT: &str = "benchmark-results";
/// The leaves of a report that a comparison shows. Everything else is context.
const COMPARED: [&str; 14] = [
    "median_us",
    "p95_us",
    "median",
    "endpoint_pair_ms",
    "connect_ms",
    "first_frame_ms",
    "first_frame_work_us",
    "cpu_ms",
    "cpu_percent_of_one_core",
    "frames_per_second",
    "resident_kib",
    "peak_resident_kib",
    "voluntary_switches",
    "written_bytes",
];

struct Options {
    seconds: u64,
    build: bool,
    window: bool,
    on_screen: bool,
    save: Option<String>,
    compare: Option<String>,
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
        save: None,
        compare: None,
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
        } else {
            match argument.as_str() {
                "--no-build" => options.build = false,
                "--no-window" => options.window = false,
                "--on-screen" => options.on_screen = true,
                other => {
                    return Err(format!(
                        "unknown bench option {other:?}; it takes --seconds=N --save=NAME \
                         --compare=NAME --no-build --no-window --on-screen"
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

fn program(name: &str) -> Result<PathBuf, String> {
    let path = crate::output_directory("release")
        .join("examples")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "{name} is not built for this layout; run the bench without --no-build, or \
             `cargo xtask bench-build` under the same CARGO_TARGET_DIR and CARGO_BUILD_TARGET"
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

/// Run a program that prints one JSON report on standard output.
fn printed(name: &str, args: &[&str]) -> Result<serde_json::Value, String> {
    eprintln!("+ {name} {}", args.join(" "));
    let output = Command::new(program(name)?)
        .args(args)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run {name}: {}", error.kind()))?;
    if !output.status.success() {
        return Err(format!("{name} exited with {}", output.status));
    }
    serde_json::from_slice(&output.stdout).map_err(|_| format!("{name} printed no JSON report"))
}

/// Run the demo window's probe and read the report it wrote. The probe ends itself; a
/// window that does not is ended here, after longer than it could rightly take.
fn probed(
    label: &str,
    args: &[&str],
    seconds: u64,
    display: Option<&PrivateDisplay>,
) -> Result<serde_json::Value, String> {
    let report = Path::new(OUTPUT).join(format!("{label}.json"));
    let _ = std::fs::remove_file(&report);
    let mut command = Command::new(program("demo_window")?);
    command
        .arg("--demo")
        .arg(format!("--demo-probe={seconds}"))
        .arg(format!("--demo-probe-out={}", report.display()))
        .args(args)
        .stdin(Stdio::null());
    if let Some(display) = display {
        command
            .env("DISPLAY", &display.name)
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET");
    }
    eprintln!(
        "+ demo_window --demo --demo-probe={seconds} {}",
        args.join(" ")
    );
    let mut window = command
        .spawn()
        .map_err(|error| format!("cannot run demo_window: {}", error.kind()))?;
    // The probe gives a window a minute to present its first frame, then measures.
    let limit = Duration::from_secs(seconds + 90);
    let began = Instant::now();
    let status = loop {
        match window.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if began.elapsed() < limit => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = window.kill();
                let _ = window.wait();
                return Err(format!(
                    "{label}: the window did not end by itself; it was ended"
                ));
            }
        }
    };
    let text = std::fs::read(&report)
        .map_err(|_| format!("{label}: the window ended ({status}) without a report"))?;
    let report: serde_json::Value =
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
    Ok(report)
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

/// Every compared number of a report, by its path in it.
fn compared(value: &serde_json::Value, path: &str, key: &str, into: &mut Vec<(String, f64)>) {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, value) in fields {
                let path = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}.{name}")
                };
                compared(value, &path, name, into);
            }
        }
        // A list of records is walked; a list of raw samples is not what is compared.
        serde_json::Value::Array(items) => {
            for (index, item) in items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.is_object())
            {
                compared(item, &format!("{path}[{index}]"), key, into);
            }
        }
        serde_json::Value::Number(number) if COMPARED.contains(&key) => {
            if let Some(number) = number.as_f64() {
                into.push((path.to_owned(), number));
            }
        }
        _ => {}
    }
}

/// The lines of a comparison: each number both runs have, before, now and the change.
fn comparison(before: &serde_json::Value, now: &serde_json::Value) -> Vec<String> {
    let (mut earlier, mut later) = (Vec::new(), Vec::new());
    compared(before, "", "", &mut earlier);
    compared(now, "", "", &mut later);
    let mut lines = vec![format!(
        "{:<62} {:>14} {:>14} {:>9}",
        "metric", "before", "now", "change"
    )];
    for (path, now) in &later {
        let Some((_, before)) = earlier.iter().find(|(earlier, _)| earlier == path) else {
            continue;
        };
        let change = if *before == 0.0 {
            "n/a".to_owned()
        } else {
            format!("{:+.1}%", (now - before) / before * 100.0)
        };
        lines.push(format!("{path:<62} {before:>14.2} {now:>14.2} {change:>9}"));
    }
    lines
}

pub fn run(args: &[String]) -> Result<(), String> {
    let options = options(args)?;
    // A comparison that cannot be made is said before minutes are spent measuring.
    let before = match &options.compare {
        Some(name) => {
            let text = std::fs::read(Path::new(KEPT).join(format!("{name}.json")))
                .map_err(|_| format!("no kept run named {name:?} in {KEPT}/"))?;
            Some(
                serde_json::from_slice::<serde_json::Value>(&text)
                    .map_err(|_| format!("the kept run {name:?} is not JSON"))?,
            )
        }
        None => None,
    };
    if options.build {
        build()?;
    }
    std::fs::create_dir_all(OUTPUT).map_err(|error| format!("{OUTPUT}: {}", error.kind()))?;

    let mut report = serde_json::json!({
        "schema": 1,
        "taken_unix": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
        "revision": revision(),
        "replay": printed("replay_bench", &[])?,
        "transport": printed("benchmark", &[])?,
    });
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
                label,
                &[&sized[..], more].concat(),
                options.seconds,
                private.as_ref(),
            )?;
        }
    }

    let text = format!("{report:#}\n");
    let latest = Path::new(OUTPUT).join("latest.json");
    std::fs::write(&latest, &text).map_err(|error| format!("{OUTPUT}: {}", error.kind()))?;
    eprintln!("report: {}", latest.display());
    if let Some(name) = &options.save {
        std::fs::create_dir_all(KEPT).map_err(|error| format!("{KEPT}: {}", error.kind()))?;
        let kept = Path::new(KEPT).join(format!("{name}.json"));
        std::fs::write(&kept, &text).map_err(|error| format!("{KEPT}: {}", error.kind()))?;
        eprintln!("kept: {}", kept.display());
    }
    if let Some(before) = &before {
        for line in comparison(before, &report) {
            println!("{line}");
        }
    }
    Ok(())
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
        assert!(!taken.build && taken.window && !taken.on_screen);
        assert_eq!(taken.save.as_deref(), Some("before-fix_1.2"));
        for bad in [
            "--seconds=0",
            "--seconds=601",
            "--save=",
            "--save=../outside",
            "--save=.hidden",
            "--compare=a/b",
            "--fast",
        ] {
            assert!(options(&[bad.to_owned()]).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn a_comparison_shows_shared_numbers_and_skips_samples_and_context() {
        let before = serde_json::json!({
            "replay": {"metrics": {"view.of": {"count": 200, "median_us": 10.0}}},
            "transport": {
                "encrypted_loopback_mib_s": {"samples": [1.0, 2.0], "median": 100.0},
                "history": [{"records": 1, "update_ms": {"median": 0.0}}],
            },
            "gone": {"median_us": 1.0},
        });
        let now = serde_json::json!({
            "replay": {"metrics": {"view.of": {"count": 300, "median_us": 12.0}}},
            "transport": {
                "encrypted_loopback_mib_s": {"samples": [3.0], "median": 50.0},
                "history": [{"records": 1, "update_ms": {"median": 2.0}}],
            },
            "new": {"median_us": 1.0},
        });
        let lines = comparison(&before, &now);
        // The heading, and the three numbers both runs have.
        assert_eq!(lines.len(), 4, "{lines:#?}");
        assert!(lines.iter().any(|line| {
            line.starts_with("replay.metrics.view.of.median_us") && line.ends_with("+20.0%")
        }));
        assert!(lines.iter().any(|line| {
            line.starts_with("transport.encrypted_loopback_mib_s.median")
                && line.ends_with("-50.0%")
        }));
        // A change from nothing has no percentage.
        assert!(lines.iter().any(|line| {
            line.starts_with("transport.history[0].update_ms.median") && line.ends_with("n/a")
        }));
    }
}
