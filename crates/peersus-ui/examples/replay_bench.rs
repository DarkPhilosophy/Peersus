//! Replays the interface's own per-snapshot data path against deterministic synthetic fixtures
//! and prints the timings as JSON on stdout.
//!
//! What it measures, all in-process, all on the same fixture data the read-only demo shows
//! (`peersus_ui::demo::snapshot_sized`, a `demo` feature build):
//!
//! * `view.of` — `View::of(&Snapshot)`, what every snapshot's arrival costs in view work;
//! * `view.refresh_same` / `view.refresh_changed` — `view::refresh` when the new snapshot is
//!   identical (the quiet-second case the window spends most of its life in) and when it
//!   differs;
//! * `serde.encode` / `serde.decode` — the JSON codec the owner/window socket's frames use;
//! * `screen.open_and_draw.<page>` — the demo's real component tree for that screen built,
//!   laid out and drawn once by the CPU into memory, at the desktop size, with no display
//!   (`peersus_ui::headless`). Each sample is a whole new window, so it is the cost of a
//!   screen from nothing, not of one more frame of it.
//!
//! What it does NOT measure, said here so a number is never taken for more than it is: a GPU,
//! a window system or frame pacing (the `demo_window` probe measures the real window), memory
//! or RSS, disk, the real transport, device identity, or anything a network touches. It is
//! not an acceptance benchmark, and it proves nothing about a real device; it compares
//! revisions of this code path under the same synthetic workload.
//!
//! A development tool, like the demo that shares its fixture: off the product path, and only
//! built with `--features headless`.
//!
//! Run it:
//!     cargo run --release -p peersus-ui --example replay_bench --features headless
//! Bounded knobs (defaults are the reasonable run):
//!     --warmup=25 --iters=200 --peers=16 --transfers=128

use peersus_core::model::Snapshot;
use peersus_ui::{demo, headless, probe::stats};
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

const MAX_ITERS: usize = 100_000;
/// How many windows are opened per screen at most: each is a whole tree built and drawn, so
/// a few dozen say what two hundred would.
const MAX_SCREEN_SAMPLES: usize = 30;
const SCREENS: [&str; 5] = ["send", "receive", "peers", "transfers", "settings"];

struct Run {
    warmup: usize,
    iters: usize,
    peers: usize,
    transfers: usize,
}

fn bounded(name: &str, value: &str, lo: usize, hi: usize) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(number) if (lo..=hi).contains(&number) => Ok(number),
        _ => Err(format!(
            "{name} takes a number from {lo} to {hi}, not {value:?}"
        )),
    }
}

fn parse(args: &[String]) -> Result<Run, String> {
    let mut run = Run {
        warmup: 25,
        iters: 200,
        peers: 16,
        transfers: 128,
    };
    for argument in args {
        if let Some(value) = argument.strip_prefix("--warmup=") {
            run.warmup = bounded("--warmup", value, 0, MAX_ITERS)?;
        } else if let Some(value) = argument.strip_prefix("--iters=") {
            run.iters = bounded("--iters", value, 1, MAX_ITERS)?;
        } else if let Some(value) = argument.strip_prefix("--peers=") {
            run.peers = bounded("--peers", value, 0, demo::MAX_PEERS)?;
        } else if let Some(value) = argument.strip_prefix("--transfers=") {
            run.transfers = bounded("--transfers", value, 0, demo::MAX_TRANSFERS)?;
        } else {
            return Err(format!(
                "Unknown argument {argument:?}: replay_bench takes only \
                 --warmup, --iters, --peers and --transfers"
            ));
        }
    }
    Ok(run)
}

/// Microseconds one call of `step` takes.
fn time_us(mut step: impl FnMut()) -> f64 {
    let began = Instant::now();
    step();
    began.elapsed().as_secs_f64() * 1_000_000.0
}

/// The same snapshot, with one small real change: the pair `refresh_same`/`refresh_changed`
/// needs. The fixture is the demo's, not a captured device: nothing private can leak into a
/// report by being measured.
fn changed(fixture: &Arc<Snapshot>) -> Snapshot {
    let mut next = (**fixture).clone();
    next.errors.push("synthetic replay change".to_owned());
    next
}

/// The claims every measurement is only worth as much as: encode/decode is lossless, and
/// refresh says the truth about whether the view changed.
fn check_invariants(fixture: &Arc<Snapshot>) -> Result<(), String> {
    let encoded = serde_json::to_vec(fixture).map_err(|error| error.to_string())?;
    let decoded: Snapshot = serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
    if decoded != **fixture {
        return Err("a decoded fixture is not the fixture".to_owned());
    }
    let mut view = peersus_ui::view::View::of(fixture);
    if peersus_ui::view::refresh(&mut view, fixture) {
        return Err("refresh reported a change on an unchanged snapshot".to_owned());
    }
    if !peersus_ui::view::refresh(&mut view, &changed(fixture)) {
        return Err("refresh reported no change on a changed snapshot".to_owned());
    }
    Ok(())
}

fn report(run: &Run, fixture: &Arc<Snapshot>, encoded_len: usize) -> Result<String, String> {
    // The paths being measured, exercised once before the clock so a first-call cost is not
    // mistaken for the steady one.
    for _ in 0..run.warmup {
        let snapshot = (**fixture).clone();
        let mut view = peersus_ui::view::View::of(black_box(&snapshot));
        black_box(peersus_ui::view::refresh(&mut view, black_box(&snapshot)));
        black_box(peersus_ui::view::refresh(
            &mut view,
            black_box(&changed(fixture)),
        ));
        let encoded = serde_json::to_vec(black_box(fixture)).map_err(|e| e.to_string())?;
        let decoded: Snapshot =
            serde_json::from_slice(black_box(&encoded)).map_err(|e| e.to_string())?;
        black_box(decoded);
    }

    let changed_snapshot = changed(fixture);
    let mut of = Vec::with_capacity(run.iters);
    let mut refresh_same = Vec::with_capacity(run.iters);
    let mut refresh_changed = Vec::with_capacity(run.iters);
    let mut encode = Vec::with_capacity(run.iters);
    let mut decode = Vec::with_capacity(run.iters);
    for _ in 0..run.iters {
        let snapshot = (**fixture).clone();
        of.push(time_us(|| {
            black_box(peersus_ui::view::View::of(black_box(&snapshot)));
        }));
        let mut view = peersus_ui::view::View::of(&snapshot);
        refresh_same.push(time_us(|| {
            black_box(peersus_ui::view::refresh(&mut view, black_box(&snapshot)));
        }));
        refresh_changed.push(time_us(|| {
            black_box(peersus_ui::view::refresh(
                &mut view,
                black_box(&changed_snapshot),
            ));
        }));
        let mut bytes = Vec::new();
        encode.push(time_us(|| {
            bytes = serde_json::to_vec(black_box(fixture)).expect("encode");
        }));
        decode.push(time_us(|| {
            black_box(serde_json::from_slice::<Snapshot>(black_box(&bytes)).expect("decode"));
        }));
    }

    let mut screens = serde_json::Map::new();
    for page in SCREENS {
        let arguments = [
            format!("--demo-page={page}"),
            format!("--demo-peers={}", run.peers),
            format!("--demo-transfers={}", run.transfers),
        ];
        let mut samples = Vec::new();
        for _ in 0..run.iters.min(MAX_SCREEN_SAMPLES) {
            let began = Instant::now();
            let mut window = headless::Window::open(&arguments, headless::DESKTOP)?;
            window.frame();
            samples.push(began.elapsed().as_secs_f64() * 1_000_000.0);
        }
        screens.insert(format!("screen.open_and_draw.{page}"), stats(&mut samples));
    }

    let mut metrics = serde_json::json!({
        "view.of": stats(&mut of),
        "view.refresh_same": stats(&mut refresh_same),
        "view.refresh_changed": stats(&mut refresh_changed),
        "serde.encode": stats(&mut encode),
        "serde.decode": stats(&mut decode),
    });
    if let Some(metrics) = metrics.as_object_mut() {
        metrics.extend(screens);
    }
    serde_json::to_string_pretty(&serde_json::json!({
        "tool": "replay_bench",
        "workload": {
            "fixture": "peersus_ui::demo::snapshot_sized (synthetic, deterministic)",
            "peers": run.peers,
            "transfers": run.transfers,
            "snapshot_json_bytes": encoded_len,
            "warmup": run.warmup,
        },
        "metrics": metrics,
        "not_measured": [
            "GPU, window system and frame pacing",
            "memory or RSS",
            "disk I/O",
            "network and transport",
            "device identity or keyring",
            "end-to-end transfer latency"
        ]
    }))
    .map_err(|error| error.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = match parse(&args) {
        Ok(run) => run,
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(2);
        }
    };
    let fixture = Arc::new(demo::snapshot_sized(run.peers, run.transfers));
    let encoded_len = serde_json::to_vec(&fixture).map_or(0, |bytes| bytes.len());
    match check_invariants(&fixture).and_then(|()| report(&run, &fixture, encoded_len)) {
        Ok(json) => println!("{json}"),
        Err(reason) => {
            eprintln!("replay_bench: {reason}");
            std::process::exit(1);
        }
    }
}
