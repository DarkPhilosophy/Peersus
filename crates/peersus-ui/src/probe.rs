//! The demo window measuring itself: a `--demo-probe=SECONDS` run (see [`crate::demo`]).
//!
//! The window is the real one — the same event loop, window system, graphics driver and
//! frame pacing a person gets — on the demo's synthetic snapshot. The toolkit tells a plugin
//! when each stage of a frame starts and ends; this one notes the times and nothing else. Once
//! the first frame is on the screen it measures for the seconds asked, writes one JSON report
//! and asks the window to close.
//!
//! What a report is evidence of, and what it is not:
//!
//! * It IS the time to the first presented frame, the cost of each later frame by stage, and
//!   what the process used while idle or replaying: CPU time, resident memory, context
//!   switches and disk bytes, as the operating system counts them for this process.
//! * It is NOT a product run: no owner process, socket, extension, network or disk state
//!   takes part. Allocations are not counted (the workspace forbids the `unsafe` a counting
//!   allocator needs). The process counters are read on Linux only; elsewhere they are absent
//!   from the report, not zero. A report compares revisions on one machine and one display
//!   server; it says nothing of another device.
//! * A `whole_frame` is one redraw: the layout when the tree changed (a frame of an unchanged
//!   tree has none), the render and the present, ending when the frame is on the screen.
//!   `update_tree` is the work between frames that decides whether one is needed, and is not
//!   inside it; the accessibility tree the toolkit updates after presenting is not measured.
//! * `frames_per_second` is how often the window chose to redraw. A display server that does
//!   not pace frames (the private in-memory one) lets a running animation redraw as fast as
//!   the CPU allows: there the number is an upper bound and shows THAT something redraws,
//!   while on a paced display the same animation redraws at the refresh rate.
//!
//! Development only, behind the `demo` feature. The report holds numbers, the name of the
//! graphics driver and of the GPU, and nothing of the person's.

use freya::prelude::{GlobalUserEvent, NativeEvent};
use freya_winit::{
    plugins::{FreyaPlugin, PluginEvent, PluginHandle},
    renderer::NativeGenericEvent,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError},
    time::{Duration, Instant},
};

/// The longest a probe may be asked to measure for: ten minutes shows a slow leak, and a
/// mistaken number cannot leave a window open for a day.
pub const MAX_SECONDS: usize = 600;
/// How long a window that never presents a frame is waited for before the run is given up
/// as failed: no display, or a driver that could not start.
const FIRST_FRAME_PATIENCE: Duration = Duration::from_secs(60);
/// How often the waiting thread looks whether the first frame has come.
const LOOK_EVERY: Duration = Duration::from_millis(10);
/// How long the window is given to close once asked, before the process is ended anyway.
const END_PATIENCE: Duration = Duration::from_secs(5);
/// How many samples a stage keeps at most; past it they are counted and not kept, so that a
/// long run cannot grow the very memory it reports.
const MAX_SAMPLES: usize = 100_000;

/// What one probe run was told (see [`crate::demo::parse`]).
pub struct Run {
    pub(crate) seconds: u64,
    pub(crate) out: Option<PathBuf>,
    pub(crate) peers: usize,
    pub(crate) transfers: usize,
    pub(crate) replay_hz: usize,
}

static STARTED: OnceLock<Instant> = OnceLock::new();

/// Note the moment the program began. A program that wants its start-up measured from its
/// first statement calls this there; without it the clock starts when the arguments are read.
pub fn mark_start() {
    STARTED.get_or_init(Instant::now);
}

/// The numbers of one metric, in microseconds: a middle that one slow sample cannot move, a
/// tail, and the ends. No sample at all is said as a count of zero, never as zeros.
pub fn stats(samples: &mut [f64]) -> serde_json::Value {
    samples.sort_unstable_by(f64::total_cmp);
    let (Some(least), Some(most)) = (samples.first(), samples.last()) else {
        return serde_json::json!({ "count": 0 });
    };
    let tail = ((samples.len() as f64) * 0.95).ceil() as usize;
    serde_json::json!({
        "count": samples.len(),
        "median_us": samples[samples.len() / 2],
        "p95_us": samples[tail.clamp(1, samples.len()) - 1],
        "min_us": least,
        "max_us": most,
    })
}

/// What the operating system counts for this process, now. Each is absent where the platform
/// has no such counter this module reads.
#[derive(Clone, Copy, Default)]
struct Counters {
    cpu: Option<Duration>,
    resident_kib: Option<u64>,
    peak_resident_kib: Option<u64>,
    threads: Option<u64>,
    voluntary_switches: Option<u64>,
    involuntary_switches: Option<u64>,
    read_bytes: Option<u64>,
    written_bytes: Option<u64>,
}

/// The number after `key` at the start of a line of one of the kernel's `key: value` files.
#[cfg(target_os = "linux")]
fn field(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(key))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(target_os = "linux")]
fn counters() -> Counters {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    // Bytes that reached or left a block device for this process; absent in a sandbox that
    // hides the file.
    let io = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
    let cpu = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
    Counters {
        cpu: u64::try_from(cpu.tv_sec)
            .ok()
            .zip(u32::try_from(cpu.tv_nsec).ok())
            .map(|(seconds, nanos)| Duration::new(seconds, nanos)),
        resident_kib: field(&status, "VmRSS:"),
        peak_resident_kib: field(&status, "VmHWM:"),
        threads: field(&status, "Threads:"),
        voluntary_switches: field(&status, "voluntary_ctxt_switches:"),
        involuntary_switches: field(&status, "nonvoluntary_ctxt_switches:"),
        read_bytes: field(&io, "read_bytes:"),
        written_bytes: field(&io, "write_bytes:"),
    }
}

#[cfg(not(target_os = "linux"))]
fn counters() -> Counters {
    Counters::default()
}

/// One stage of a frame: when the one now running began, and how long each finished one took.
#[derive(Default)]
struct Stage {
    began: Option<Instant>,
    micros: Vec<f64>,
    not_kept: u64,
}

impl Stage {
    fn begin(&mut self) {
        self.began = Some(Instant::now());
    }

    /// Begin unless already begun: for a stage that more than one event can open.
    fn begin_once(&mut self) {
        self.began.get_or_insert_with(Instant::now);
    }

    /// End the stage now and keep its time; `None` when it was never begun.
    fn end(&mut self) -> Option<f64> {
        let micros = self.began.take()?.elapsed().as_secs_f64() * 1_000_000.0;
        if self.micros.len() < MAX_SAMPLES {
            self.micros.push(micros);
        } else {
            self.not_kept += 1;
        }
        Some(micros)
    }

    fn forget(&mut self) {
        self.micros.clear();
        self.not_kept = 0;
    }

    fn report(&mut self) -> serde_json::Value {
        let mut report = stats(&mut self.micros);
        if self.not_kept > 0 {
            report["not_kept"] = self.not_kept.into();
        }
        report
    }
}

/// What the first frame's arrival fixed: when, what it cost, and what the process had used.
struct First {
    at: Instant,
    since_start: Duration,
    frame_us: f64,
    counters: Counters,
}

/// Everything the plugin has seen, shared with the thread that writes the report.
#[derive(Default)]
struct Seen {
    driver: Option<&'static str>,
    gpu: Option<String>,
    window_after: Option<Duration>,
    /// The way to ask the window's event loop to end, kept from the window's creation.
    handle: Option<PluginHandle>,
    first: Option<First>,
    update: Stage,
    layout: Stage,
    render: Stage,
    present: Stage,
    frame: Stage,
    /// Frames presented in the second half of the run: what the window still draws once
    /// whatever opening it set off has had half the run to end.
    settled_frames: u64,
}

/// The toolkit plugin of a probe run.
pub(crate) struct Probe {
    started: Instant,
    /// How long after the first frame the settled half begins.
    settles_after: Duration,
    seen: Arc<Mutex<Seen>>,
}

/// A stage that panicked mid-update leaves numbers that are still numbers: the report is
/// written all the same.
fn locked(seen: &Mutex<Seen>) -> MutexGuard<'_, Seen> {
    seen.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Probe {
    /// Start measuring: the plugin to hand the toolkit, with the thread that will write the
    /// report and end the process already waiting.
    pub(crate) fn begin(run: Run) -> Result<Self, String> {
        let from_main = STARTED.get().is_some();
        let settles_after = Duration::from_secs(run.seconds) / 2;
        let started = *STARTED.get_or_init(Instant::now);
        let seen = Arc::new(Mutex::new(Seen::default()));
        let shared = Arc::clone(&seen);
        std::thread::Builder::new()
            .name("probe".to_owned())
            .spawn(move || {
                let opened = Instant::now();
                let first_at = loop {
                    std::thread::sleep(LOOK_EVERY);
                    if let Some(first) = &locked(&shared).first {
                        break Some(first.at);
                    }
                    if opened.elapsed() > FIRST_FRAME_PATIENCE {
                        break None;
                    }
                };
                if let Some(at) = first_at {
                    let until = at + Duration::from_secs(run.seconds);
                    std::thread::sleep(until.saturating_duration_since(Instant::now()));
                }
                let report = report(&run, from_main, &mut locked(&shared), counters());
                let written = match &run.out {
                    Some(path) => std::fs::write(path, format!("{report:#}\n")),
                    None => {
                        println!("{report:#}");
                        Ok(())
                    }
                };
                if let Err(error) = &written {
                    eprintln!("The probe's report could not be written: {}", error.kind());
                }
                let failed = first_at.is_none() || written.is_err();
                // The window is asked to end as a person closing it would end it. Ending the
                // process from this thread instead would run its exit handlers under a
                // frame that is still being drawn.
                //
                // The handle is taken in a statement of its own: the lock must be let go
                // before the wait below. Held through it, the window's next frame waits on
                // this lock inside the plugin and the loop that was asked to end cannot.
                let handle = locked(&shared).handle.take();
                if let (false, Some(handle)) = (failed, handle) {
                    handle.send_event_loop_event(NativeEvent::Generic(NativeGenericEvent::User(
                        GlobalUserEvent::Exit,
                    )));
                    std::thread::sleep(END_PATIENCE);
                    // The process ends when the loop does, so this is reached only when the
                    // window did not end when asked. That is a finding, said and not hidden.
                    eprintln!("The probe's window did not end when asked; the process is ended");
                    std::process::exit(2);
                }
                // No window to ask, or a failed run.
                std::process::exit(i32::from(failed));
            })
            .map_err(|_| "Cannot start the probe's thread".to_owned())?;
        Ok(Self {
            started,
            settles_after,
            seen,
        })
    }
}

/// `later - earlier` of two counters, when the platform gave both.
fn grown(earlier: Option<u64>, later: Option<u64>) -> Option<u64> {
    Some(later?.saturating_sub(earlier?))
}

fn report(run: &Run, from_main: bool, seen: &mut Seen, end: Counters) -> serde_json::Value {
    let workload = serde_json::json!({
        "fixture": "peersus_ui::demo::replay (synthetic, deterministic)",
        "peers": run.peers,
        "transfers": run.transfers,
        "replay_hz": run.replay_hz,
        "seconds_asked": run.seconds,
    });
    let window = serde_json::json!({
        "graphics_driver": seen.driver,
        "gpu": seen.gpu,
        "created_ms": seen.window_after.map(|after| after.as_secs_f64() * 1000.0),
    });
    let build = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let Some(first) = &seen.first else {
        return serde_json::json!({
            "tool": "demo_probe",
            "build": build,
            "workload": workload,
            "window": window,
            "error": "No frame was presented: no display, or the graphics driver did not start",
        });
    };
    let measured = first.at.elapsed().as_secs_f64();
    let cpu = end
        .cpu
        .zip(first.counters.cpu)
        .map(|(end, first)| end.saturating_sub(first).as_secs_f64());
    let frames = seen.frame.micros.len() as u64 + seen.frame.not_kept;
    let settled = (measured - run.seconds as f64 / 2.0).max(f64::MIN_POSITIVE);
    serde_json::json!({
        "tool": "demo_probe",
        "build": build,
        "workload": workload,
        "window": window,
        "startup": {
            "clock_from": if from_main { "first statement of main" } else { "arguments read" },
            "first_frame_ms": first.since_start.as_secs_f64() * 1000.0,
            "first_frame_work_us": first.frame_us,
            "cpu_ms": first.counters.cpu.map(|cpu| cpu.as_secs_f64() * 1000.0),
            "resident_kib": first.counters.resident_kib,
            "threads": first.counters.threads,
            "read_bytes": first.counters.read_bytes,
            "written_bytes": first.counters.written_bytes,
        },
        "steady": {
            "seconds": measured,
            "frames": frames,
            "frames_per_second": frames as f64 / measured,
            // The same, for the second half of the run only: a screen that opens with a
            // short burst of frames and then rests shows it here, where the figure above
            // would spread the burst over the whole run.
            "settled": {
                "seconds": settled,
                "frames": seen.settled_frames,
                "frames_per_second": seen.settled_frames as f64 / settled,
            },
            "cpu_ms": cpu.map(|cpu| cpu * 1000.0),
            "cpu_percent_of_one_core": cpu.map(|cpu| cpu / measured * 100.0),
            "voluntary_switches": grown(first.counters.voluntary_switches, end.voluntary_switches),
            "involuntary_switches": grown(
                first.counters.involuntary_switches,
                end.involuntary_switches,
            ),
            "read_bytes": grown(first.counters.read_bytes, end.read_bytes),
            "written_bytes": grown(first.counters.written_bytes, end.written_bytes),
            "resident_kib_grown": end
                .resident_kib
                .zip(first.counters.resident_kib)
                .map(|(end, first)| end as i64 - first as i64),
        },
        "end": {
            "resident_kib": end.resident_kib,
            "peak_resident_kib": end.peak_resident_kib,
            "threads": end.threads,
        },
        "frame_stages": {
            "update_tree": seen.update.report(),
            "layout": seen.layout.report(),
            "render": seen.render.report(),
            "present": seen.present.report(),
            "whole_frame": seen.frame.report(),
        },
        "not_measured": [
            "allocations",
            "the owner process, extensions, network and saved state",
            "input latency",
            "process counters on a platform other than Linux",
        ],
    })
}

impl FreyaPlugin for Probe {
    fn plugin_id(&self) -> &'static str {
        "peersus-demo-probe"
    }

    fn on_event(&mut self, event: &mut PluginEvent, handle: PluginHandle) {
        let mut guard = locked(&self.seen);
        // The fields are borrowed apart below, which a guard does not allow and its inside does.
        let seen = &mut *guard;
        match event {
            PluginEvent::WindowCreated {
                graphics_driver,
                gpu_name,
                ..
            } => {
                seen.driver = Some(graphics_driver);
                seen.gpu = gpu_name.map(str::to_owned);
                seen.window_after = Some(self.started.elapsed());
                seen.handle = Some(handle);
            }
            PluginEvent::GraphicsDriverChanged {
                graphics_driver,
                gpu_name,
                ..
            } => {
                seen.driver = Some(graphics_driver);
                seen.gpu = gpu_name.map(str::to_owned);
            }
            PluginEvent::StartedUpdatingTree { .. } => seen.update.begin(),
            PluginEvent::FinishedUpdatingTree { .. } => {
                seen.update.end();
            }
            PluginEvent::StartedMeasuringLayout { .. } => {
                seen.frame.begin_once();
                seen.layout.begin();
            }
            PluginEvent::FinishedMeasuringLayout { .. } => {
                seen.layout.end();
            }
            PluginEvent::BeforeRender { .. } => {
                seen.frame.begin_once();
                seen.render.begin();
            }
            PluginEvent::AfterRender { .. } => {
                seen.render.end();
            }
            PluginEvent::BeforePresenting { .. } => seen.present.begin(),
            // A frame is layout, render and present: it ends when it is on the screen. What
            // the toolkit does after that in the same turn (the accessibility tree) is not
            // part of it, and is not measured.
            PluginEvent::AfterPresenting { .. } => {
                seen.present.end();
                let frame_us = seen.frame.end();
                if let (Some(first), Some(_)) = (&seen.first, frame_us) {
                    if first.at.elapsed() >= self.settles_after {
                        seen.settled_frames += 1;
                    }
                } else if let Some(frame_us) = frame_us {
                    // What came before belongs to start-up; the steady numbers begin clean.
                    for stage in [
                        &mut seen.update,
                        &mut seen.layout,
                        &mut seen.render,
                        &mut seen.present,
                        &mut seen.frame,
                    ] {
                        stage.forget();
                    }
                    seen.first = Some(First {
                        at: Instant::now(),
                        since_start: self.started.elapsed(),
                        frame_us,
                        counters: counters(),
                    });
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_say_the_middle_the_tail_and_the_ends_and_nothing_of_no_samples() {
        let mut samples: Vec<f64> = (1..=100).rev().map(f64::from).collect();
        let report = stats(&mut samples);
        assert_eq!(report["count"], 100);
        assert_eq!(report["median_us"], 51.0);
        assert_eq!(report["p95_us"], 95.0);
        assert_eq!(report["min_us"], 1.0);
        assert_eq!(report["max_us"], 100.0);
        assert_eq!(stats(&mut [7.0])["p95_us"], 7.0);
        assert_eq!(stats(&mut []), serde_json::json!({ "count": 0 }));
    }

    #[test]
    fn a_stage_keeps_what_ended_and_only_what_began() {
        let mut stage = Stage::default();
        assert!(stage.end().is_none(), "nothing began");
        stage.begin();
        assert!(stage.end().is_some());
        assert!(stage.end().is_none(), "one beginning ends once");
        assert_eq!(stage.report()["count"], 1);
        stage.forget();
        assert_eq!(stage.report()["count"], 0);
    }

    #[test]
    fn a_run_with_no_frame_reports_the_failure_and_no_numbers() {
        let run = Run {
            seconds: 1,
            out: None,
            peers: 3,
            transfers: 4,
            replay_hz: 0,
        };
        let report = report(&run, false, &mut Seen::default(), Counters::default());
        assert!(report["error"].is_string());
        assert!(report.get("steady").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_process_counters_are_read_on_linux() {
        let now = counters();
        assert!(now.cpu.is_some());
        assert!(now.resident_kib.is_some_and(|kib| kib > 0));
        assert!(now.threads.is_some_and(|threads| threads >= 1));
        assert_eq!(
            field("VmRSS:\t  1234 kB\nThreads:\t7\n", "Threads:"),
            Some(7)
        );
        assert_eq!(field("VmRSS:\t  1234 kB\n", "VmHWM:"), None);
    }
}
