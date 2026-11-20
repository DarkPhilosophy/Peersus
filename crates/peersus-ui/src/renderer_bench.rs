//! Compare native renderer windows, one isolated child at a time. The workers never attach to
//! the owner and draw only the synthetic transfer replay, with no captured content.
use crate::{font, i18n, receive_page::HistoryRow, replay_fixture, theme, view};
use cpu_time::ThreadTime;
use freya::prelude::*;
use freya_winit::{
    config::{RendererBackend as Backend, RendererStatus},
    plugins::{FreyaPlugin, PluginEvent, PluginHandle},
};
use peersus_core::model::PeerId;
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};
#[cfg(not(target_os = "android"))]
use std::{
    io::{self, Write},
    process::{ExitCode, Stdio},
};
#[cfg(not(target_os = "android"))]
use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{mpsc, oneshot},
};

#[cfg(not(target_os = "android"))]
const ARG: &str = "--renderer-sample";
#[cfg(not(target_os = "android"))]
const LIMIT: Duration = Duration::from_secs(8);
#[cfg(not(target_os = "android"))]
const MAX_OUTPUT: u64 = 4096;
const WARMUP: usize = 12;
const FRAMES: usize = 72;
const SCENE_PADDING: f32 = 18.;
const SWATCH_SIDE: f32 = 12.;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct Metrics {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub cpu_percent: f64,
    pub resident_bytes: usize,
    #[cfg(target_os = "android")]
    pub pss_kib: Option<u64>,
}

#[cfg(not(target_os = "android"))]
impl Metrics {
    pub(crate) fn resident_mib(self) -> f64 {
        self.resident_bytes as f64 / 1_048_576.
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Failure {
    Unavailable,
    TimedOut,
    Interrupted,
    NoCounters,
    SceneMismatch,
}

type Reading = Result<Metrics, Failure>;

#[derive(Serialize, Deserialize)]
struct WorkerReport {
    renderer: String,
    reading: Reading,
}

#[cfg(not(target_os = "android"))]
#[derive(Debug, PartialEq)]
pub(crate) struct Report {
    pub baseline: Backend,
    pub rows: Vec<(Backend, Reading)>,
}

#[cfg(not(target_os = "android"))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Recommendation {
    NoBaseline,
    OnlyBaseline,
    Keep,
    Smoother(Backend),
    LessCpu(Backend),
    LessMemory(Backend),
}

#[cfg(not(target_os = "android"))]
impl Report {
    pub fn recommend(&self) -> Recommendation {
        let Some(base) = self
            .rows
            .iter()
            .find(|(backend, _)| *backend == self.baseline)
            .and_then(|(_, reading)| reading.as_ref().ok())
        else {
            return Recommendation::NoBaseline;
        };
        let mut others = self
            .rows
            .iter()
            .filter(|(backend, _)| *backend != self.baseline)
            .filter_map(|(backend, reading)| {
                reading.as_ref().ok().map(|metrics| (*backend, metrics))
            })
            .peekable();
        if others.peek().is_none() {
            return Recommendation::OnlyBaseline;
        }

        // Don't recommend marginal differences or a faster tail that comes with conspicuously
        // worse typical frames or resource use. Order: smoother frames, less CPU, less RAM.
        let mut smoother = None;
        let mut less_cpu = None;
        let mut less_memory = None;
        let headroom = |value: f64, baseline: f64, fraction: f64, minimum: f64| {
            value - baseline <= (baseline * fraction).max(minimum)
        };
        for (backend, candidate) in others {
            if !headroom(candidate.p50_ms, base.p50_ms, 0.10, 0.5)
                || !headroom(candidate.p95_ms, base.p95_ms, 0.10, 0.5)
                || !headroom(candidate.cpu_percent, base.cpu_percent, 0.20, 5.)
                || !headroom(candidate.resident_mib(), base.resident_mib(), 0.25, 8.)
            {
                continue;
            }
            let consider =
                |best: &mut Option<(Backend, f64)>, value: f64, base: f64, enough: f64| {
                    if base - value >= enough && best.is_none_or(|(_, lowest)| value < lowest) {
                        *best = Some((backend, value));
                    }
                };
            consider(
                &mut smoother,
                candidate.p95_ms,
                base.p95_ms,
                (base.p95_ms * 0.10).max(0.75),
            );
            consider(
                &mut less_cpu,
                candidate.cpu_percent,
                base.cpu_percent,
                (base.cpu_percent * 0.15).max(3.),
            );
            consider(
                &mut less_memory,
                candidate.resident_mib(),
                base.resident_mib(),
                (base.resident_mib() * 0.20).max(16.),
            );
        }
        if let Some((backend, _)) = smoother {
            Recommendation::Smoother(backend)
        } else if let Some((backend, _)) = less_cpu {
            Recommendation::LessCpu(backend)
        } else if let Some((backend, _)) = less_memory {
            Recommendation::LessMemory(backend)
        } else {
            Recommendation::Keep
        }
    }
}

#[cfg(not(target_os = "android"))]
pub(crate) enum Update {
    Started(Backend),
    Finished(Report),
    Cancelled,
}

#[cfg(not(target_os = "android"))]
pub(crate) async fn compare(
    baseline: Backend,
    updates: mpsc::Sender<Update>,
    mut cancel: oneshot::Receiver<()>,
) {
    let candidates = std::iter::once(baseline).chain(
        Backend::compiled()
            .iter()
            .copied()
            .filter(|backend| *backend != baseline),
    );
    let mut rows = Vec::with_capacity(Backend::compiled().len());
    for backend in candidates {
        if updates.send(Update::Started(backend)).await.is_err() {
            return;
        }
        let Some(reading) = measure(backend, &mut cancel).await else {
            let _ = updates.send(Update::Cancelled).await;
            return;
        };
        rows.push((backend, reading));
    }
    let _ = updates
        .send(Update::Finished(Report { baseline, rows }))
        .await;
}

#[cfg(not(target_os = "android"))]
async fn measure(backend: Backend, cancel: &mut oneshot::Receiver<()>) -> Option<Reading> {
    // Check before spawning: cancellation may already have arrived between two samples.
    if !matches!(cancel.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
        return None;
    }
    let child = std::env::current_exe().and_then(|program| {
        Command::new(program)
            .args([ARG, backend.name()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
    });
    let Ok(mut child) = child else {
        return Some(Err(Failure::Interrupted));
    };
    let Some(stdout) = child.stdout.take() else {
        return Some(Err(Failure::Interrupted));
    };
    let read = async {
        let mut output = Vec::new();
        stdout
            .take(MAX_OUTPUT)
            .read_to_end(&mut output)
            .await
            .ok()?;
        let ended = child.wait().await.ok()?;
        if !ended.success() || output.len() as u64 == MAX_OUTPUT {
            return None;
        }
        let report = serde_json::from_slice::<WorkerReport>(&output).ok()?;
        (report.renderer == backend.name()).then_some(report.reading)
    };
    let outcome = tokio::select! {
        biased;
        _ = cancel => None,
        result = tokio::time::timeout(LIMIT, read) => Some(match result {
            Ok(report) => report.unwrap_or(Err(Failure::Interrupted)),
            Err(_) => Err(Failure::TimedOut),
        }),
    };
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
    }
    outcome
}

fn measure_metrics(
    intervals: &mut [Duration],
    cpu: Duration,
    elapsed: Duration,
    memory: usize,
) -> Option<Metrics> {
    if intervals.is_empty() || elapsed.is_zero() || memory == 0 {
        return None;
    }
    intervals.sort_unstable();
    let rank = |percent: usize| intervals[(intervals.len() * percent).div_ceil(100) - 1];
    Some(Metrics {
        p50_ms: rank(50).as_secs_f64() * 1_000.,
        p95_ms: rank(95).as_secs_f64() * 1_000.,
        cpu_percent: 100. * cpu.as_secs_f64() / elapsed.as_secs_f64(),
        resident_bytes: memory,
        #[cfg(target_os = "android")]
        pss_kib: std::fs::read_to_string("/proc/self/smaps_rollup")
            .ok()
            .and_then(|status| {
                status.lines().find_map(|line| {
                    line.strip_prefix("Pss:")?
                        .split_whitespace()
                        .next()?
                        .parse()
                        .ok()
                })
            }),
    })
}

struct Probe {
    requested: Backend,
    outcome: Rc<RefCell<Option<Reading>>>,
    tick: Option<State<usize>>,
    presented: bool,
    verified_scene: bool,
    frames: usize,
    start: Option<(Instant, ThreadTime)>,
    last: Option<Instant>,
    intervals: Vec<Duration>,
}

impl Probe {
    fn finish(&self, result: Reading, handle: PluginHandle) {
        *self.outcome.borrow_mut() = Some(result);
        let _closed = LaunchProxy(handle.proxy).post_callback(|renderer| renderer.exit());
    }

    fn frame(&mut self) -> Option<Reading> {
        self.frames += 1;
        if self.frames == WARMUP {
            if !self.verified_scene {
                return Some(Err(Failure::SceneMismatch));
            }
            let now = Instant::now();
            let Ok(cpu) = ThreadTime::try_now() else {
                return Some(Err(Failure::NoCounters));
            };
            self.start = Some((now, cpu));
            self.last = Some(now);
        } else if let Some(last) = &mut self.last {
            let now = Instant::now();
            self.intervals.push(now.duration_since(*last));
            *last = now;
            if self.intervals.len() == FRAMES {
                return Some(
                    (|| {
                        let (started, cpu) = self.start?;
                        let used = cpu.try_elapsed().ok()?;
                        let memory = memory_stats::memory_stats()?.physical_mem;
                        measure_metrics(
                            &mut self.intervals,
                            used,
                            now.duration_since(started),
                            memory,
                        )
                    })()
                    .ok_or(Failure::NoCounters),
                );
            }
        }
        if let Some(mut tick) = self.tick {
            tick.set(self.frames);
        }
        None
    }
}

impl FreyaPlugin for Probe {
    fn plugin_id(&self) -> &'static str {
        "peersus-renderer-sample"
    }

    fn on_event(&mut self, event: &mut PluginEvent<'_>, handle: PluginHandle) {
        match event {
            PluginEvent::WindowCreated { runner, window, .. } => {
                let reported = runner.with_root_context(|| {
                    try_consume_root_context::<State<RendererStatus>>()
                        .map(|status| status.peek().active)
                });
                if reported != Some(self.requested) {
                    self.finish(Err(Failure::Unavailable), handle);
                    return;
                }
                self.tick = runner.with_root_context(try_consume_root_context::<State<usize>>);
                window.request_redraw();
            }
            PluginEvent::AfterRender { window, canvas, .. }
                if !self.verified_scene
                    && self.frames < WARMUP
                    && self.outcome.borrow().is_none() =>
            {
                // Verify the colored swatch before timing. A swap that succeeded is not
                // evidence that Skia wrote the scene to the right image in the right format.
                use skia_safe::{AlphaType, ColorType, ImageInfo};
                let expected = theme::DARK.send;
                let at = (f64::from(SCENE_PADDING + SWATCH_SIDE / 2.) * window.scale_factor())
                    .round() as i32;
                let info = ImageInfo::new((1, 1), ColorType::RGBA8888, AlphaType::Unpremul, None);
                let mut pixel = [0; 4];
                self.verified_scene = canvas.read_pixels(&info, &mut pixel, 4, (at, at))
                    && pixel == [expected.0, expected.1, expected.2, 255];
            }
            PluginEvent::AfterPresenting { presented, .. } => self.presented = *presented,
            PluginEvent::AfterRedraw { window, .. } if self.outcome.borrow().is_none() => {
                if std::mem::take(&mut self.presented) {
                    if let Some(reading) = self.frame() {
                        self.finish(reading, handle);
                    }
                } else {
                    window.request_redraw();
                }
            }
            PluginEvent::GraphicsDriverChanged { .. } => {
                self.finish(Err(Failure::Interrupted), handle)
            }
            _ => {}
        }
    }
}

fn scene() -> impl IntoElement {
    let frame = use_state(|| 0_usize);
    use_provide_root_context(|| frame);
    let phase = frame() % replay_fixture::REPLAY_FRAMES;
    let peer = use_hook(|| PeerId("synthetic-renderer-peer".to_owned()));
    let palette = theme::DARK;
    rect()
        .expanded()
        .padding(SCENE_PADDING)
        .spacing(10.)
        .background(palette.background)
        .child(
            rect()
                .horizontal()
                .spacing(8.)
                .child(
                    rect()
                        .width(Size::px(SWATCH_SIDE))
                        .height(Size::px(SWATCH_SIDE))
                        .background(palette.send),
                )
                .child(crate::section("Renderer comparison", palette)),
        )
        .child(
            rect()
                .width(Size::fill())
                .horizontal()
                .content(Content::flex())
                .spacing(10.)
                .children((0..2).map(|column| {
                    rect().width(Size::flex(1.)).spacing(7.).children(
                        (column * 7..column * 7 + 7).map(|index| {
                            let transfer = replay_fixture::transfer(index, &peer, None, phase);
                            HistoryRow {
                                item: view::progress(&transfer),
                                palette,
                            }
                        }),
                    )
                })),
        )
}

fn configuration(backend: Backend, outcome: Rc<RefCell<Option<Reading>>>) -> LaunchConfig {
    let title = i18n::translate(i18n::Language::default(), "Renderer comparison");
    LaunchConfig::new()
        .with_font(font::FAMILY, font::BYTES)
        .with_default_font(font::FAMILY)
        .with_gpu_resource_cache_limit(crate::WINDOW_GPU_CACHE)
        .with_plugin(Probe {
            requested: backend,
            outcome,
            tick: None,
            presented: false,
            verified_scene: false,
            frames: 0,
            start: None,
            last: None,
            intervals: Vec::with_capacity(FRAMES),
        })
        .with_window(
            WindowConfig::new(scene)
                .with_title(title)
                .with_size(760., 540.)
                .with_resizable(false)
                .with_decorations(false)
                .with_transparency(!cfg!(target_os = "android"))
                .with_background(Color::TRANSPARENT)
                .with_window_attributes(|attributes, _| attributes.with_active(false))
                .with_renderer(backend.preference()),
        )
}

#[cfg(target_os = "android")]
pub fn run_android_probe(
    events: freya::winit::event_loop::EventLoop<NativeEvent>,
    backend: Backend,
) -> Result<String, serde_json::Error> {
    let outcome = Rc::new(RefCell::new(None));
    launch(configuration(backend, outcome.clone()).with_event_loop(events));
    serde_json::to_string(&WorkerReport {
        renderer: backend.name().to_owned(),
        reading: outcome.take().unwrap_or(Err(Failure::Interrupted)),
    })
}

#[cfg(not(target_os = "android"))]
/// Entry for the desktop's private renderer subprocess, before normal application startup.
/// Other arguments are left for the real window's argument parser.
pub fn run_worker(args: &[String]) -> Option<ExitCode> {
    if args.first().is_none_or(|argument| argument != ARG) {
        return None;
    }
    let [_, name] = args else {
        return Some(ExitCode::FAILURE);
    };
    let Some(backend) = Backend::compiled()
        .iter()
        .copied()
        .find(|backend| backend.name().eq_ignore_ascii_case(name))
    else {
        return Some(ExitCode::FAILURE);
    };
    // Do not let Freya's release-build fatal-error dialog take over the application on a worker panic.
    std::panic::set_hook(Box::new(|error| {
        eprintln!("Renderer sample failed: {error}");
        std::process::exit(1);
    }));
    let outcome = Rc::new(RefCell::new(None));
    launch(configuration(backend, outcome.clone()));
    let report = WorkerReport {
        renderer: backend.name().to_owned(),
        reading: outcome.take().unwrap_or(Err(Failure::Interrupted)),
    };
    let mut output = io::stdout().lock();
    Some(match serde_json::to_writer(&mut output, &report) {
        Ok(()) if writeln!(output).is_ok() && output.flush().is_ok() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    })
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_frame_times_and_one_core_cpu_use_the_measured_interval() {
        let mut samples: Vec<_> = (1..=20).rev().map(Duration::from_millis).collect();
        let observed = measure_metrics(
            &mut samples,
            Duration::from_millis(40),
            Duration::from_millis(200),
            2_097_152,
        )
        .unwrap();
        assert!((observed.p50_ms - 10.).abs() < 0.001);
        assert!((observed.p95_ms - 19.).abs() < 0.001);
        assert!((observed.cpu_percent - 20.).abs() < 0.001);
        assert!((observed.resident_mib() - 2.).abs() < 0.001);
        assert!(measure_metrics(&mut [], Duration::ZERO, Duration::from_secs(1), 10).is_none());
    }

    #[test]
    fn compare_against_the_actual_baseline_and_ignore_small_or_costly_gains() {
        let base = Metrics {
            p50_ms: 8.,
            p95_ms: 20.,
            cpu_percent: 50.,
            resident_bytes: 80 << 20,
        };
        let faster = Metrics {
            p95_ms: 15.,
            ..base
        };
        let costlier = Metrics {
            p95_ms: 10.,
            resident_bytes: 135 << 20,
            ..base
        };
        let nearly_equal = Metrics {
            p95_ms: 19.8,
            cpu_percent: 49.,
            ..base
        };
        let mut report = Report {
            baseline: Backend::OpenGl,
            rows: vec![
                (Backend::Vulkan, Ok(faster)),
                (Backend::OpenGl, Ok(base)),
                (Backend::Software, Ok(costlier)),
            ],
        };
        assert_eq!(
            report.recommend(),
            Recommendation::Smoother(Backend::Vulkan)
        );
        report.rows[0].1 = Ok(nearly_equal);
        assert_eq!(report.recommend(), Recommendation::Keep);
        report.rows[0].1 = Err(Failure::Unavailable);
        report.rows[2].1 = Err(Failure::TimedOut);
        assert_eq!(report.recommend(), Recommendation::OnlyBaseline);
        report.rows[1].1 = Err(Failure::Interrupted);
        assert_eq!(report.recommend(), Recommendation::NoBaseline);
    }
}
