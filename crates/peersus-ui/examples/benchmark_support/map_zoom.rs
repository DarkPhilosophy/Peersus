//! Opt-in Linux scenario for the existing `demo_window` benchmark. Run on a private X server:
//! `demo_window --map-zoom regional|street MANIFEST WASM WASM_HOST [--steps=60] [--interval-ms=80]`.
//! On the same Location page it measures a matched idle interval and a paced, reversible wheel
//! sequence. It uses the normal window, local IPC, owner and Wasm helper, never a running owner.
#[path = "map_zoom_fixture.rs"]
mod fixture;

use freya::elements::image::ImageElement;
use freya::prelude::{
    name::EventName,
    platform::{MouseEventName, PlatformEvent, WheelEventName},
    *,
};
use freya_winit::{
    plugins::{FreyaPlugin, PluginEvent, PluginHandle},
    renderer::NativeGenericEvent,
};
use peersus_api::elements::Node;
use peersus_app::{cli::Control, ipc::LocalControl};
use peersus_ui::probe::{heap, stats};
use rustix::time::{ClockId, clock_gettime};
use serde_json::{Value, json};
use std::{
    any::Any,
    fs::{self, File},
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    process::{Child, Command as Process, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(super) type AnyResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const WAIT: Duration = Duration::from_secs(20);
const CYCLE: usize = 6;
const WHEEL_LINE: f64 = 53.; // Freya's Winit conversion for one line, at scale 1.
const TITLES: [&str; 3] = ["Location", "Options", "Locate this device"];
const STAGES: [&str; 8] = [
    "tree",
    "layout",
    "render",
    "present",
    "input",
    "tasks",
    "accessibility",
    "frame",
];

type Point = (f64, f64);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Span {
    Regional,
    Street,
}

impl Span {
    fn parse(span: &str) -> AnyResult<Self> {
        match span {
            "regional" => Ok(Self::Regional),
            "street" => Ok(Self::Street),
            _ => Err("span must be regional or street".into()),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Regional => "regional",
            Self::Street => "street",
        }
    }
}

struct Run {
    span: Span,
    manifest: PathBuf,
    module: PathBuf,
    worker: PathBuf,
    steps: usize,
    interval: Duration,
}

impl Run {
    fn parse(args: &[String]) -> AnyResult<Self> {
        let [span, manifest, module, worker, options @ ..] = args else {
            return Err("--map-zoom needs SPAN MANIFEST WASM WASM_HOST".into());
        };
        let (mut steps, mut interval) = (60, 80);
        for option in options {
            if let Some(value) = option.strip_prefix("--steps=") {
                steps = value.parse()?;
            } else if let Some(value) = option.strip_prefix("--interval-ms=") {
                interval = value.parse()?;
            } else {
                return Err("expected --steps=N or --interval-ms=N".into());
            }
        }
        if !(CYCLE..=600).contains(&steps) || steps % CYCLE != 0 || !(40..=250).contains(&interval)
        {
            return Err("steps must be a multiple of six (6–600) and interval 40–250 ms".into());
        }
        Ok(Self {
            span: Span::parse(span)?,
            manifest: fs::canonicalize(manifest)?,
            module: fs::canonicalize(module)?,
            worker: fs::canonicalize(worker)?,
            steps,
            interval: Duration::from_millis(interval),
        })
    }
}

fn clock_ns(clock: ClockId) -> u64 {
    let time = clock_gettime(clock);
    time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

pub(super) fn now_ns() -> u64 {
    clock_ns(ClockId::Monotonic)
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

struct Owner {
    child: Child,
    scratch: tempfile::TempDir,
}

impl Owner {
    fn start(run: &Run) -> AnyResult<Self> {
        let scratch = tempfile::Builder::new().prefix("peersus-map-").tempdir()?;
        let child = Process::new(std::env::current_exe()?)
            .arg("--map-zoom-owner")
            .arg(scratch.path())
            .arg(run.span.name())
            .arg(&run.manifest)
            .arg(&run.module)
            .arg(&run.worker)
            .stdin(Stdio::piped())
            .spawn()?;
        let mut owner = Self { child, scratch };
        until("owner readiness", || {
            if let Some(status) = owner.child.try_wait()? {
                return Err(format!("benchmark owner ended: {status}").into());
            }
            Ok(owner.socket().exists().then_some(()))
        })?;
        Ok(owner)
    }

    fn socket(&self) -> PathBuf {
        self.scratch.path().join(fixture::SOCKET)
    }

    fn stop(&mut self) -> AnyResult<fixture::Progress> {
        self.child.stdin.take();
        let status = until("owner shutdown", || Ok(self.child.try_wait()?))?;
        if !status.success() {
            return Err(format!("benchmark owner ended: {status}").into());
        }
        Ok(serde_json::from_slice(&fs::read(
            self.scratch.path().join("progress.json"),
        )?)?)
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.child.stdin.take();
        let until = Instant::now() + Duration::from_secs(3);
        while self.child.try_wait().is_ok_and(|status| status.is_none()) {
            if Instant::now() >= until {
                // The Child is owned by this trial. The worker sees EOF on its control socket.
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

pub(super) fn until<T>(what: &str, mut read: impl FnMut() -> AnyResult<Option<T>>) -> AnyResult<T> {
    let end = Instant::now() + WAIT;
    loop {
        if let Some(result) = read()? {
            return Ok(result);
        }
        if Instant::now() >= end {
            return Err(format!("timed out waiting for {what}").into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// `schedstat` gives scheduled ns, not the jiffy-rounded `/proc/PID/stat` CPU time.
/// Open the active threads after warmup. A sleeping worker may retire before the
/// trial ends; retain its final observation instead of losing its accrued CPU.
pub(super) struct Cpu(Vec<(File, std::cell::Cell<u64>)>);

fn task_gone(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
        || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error())
}

impl Cpu {
    pub(super) fn main(pid: u32) -> io::Result<Self> {
        Ok(Self(vec![(
            File::open(format!("/proc/{pid}/schedstat"))?,
            std::cell::Cell::new(0),
        )]))
    }

    pub(super) fn process(pid: u32) -> io::Result<Self> {
        let mut files = Vec::new();
        for entry in fs::read_dir(format!("/proc/{pid}/task"))? {
            match File::open(entry?.path().join("schedstat")) {
                Ok(file) => files.push((file, std::cell::Cell::new(0))),
                Err(error) if task_gone(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(Self(files))
    }

    pub(super) fn read(&self) -> io::Result<u64> {
        self.0.iter().try_fold(0, |sum, (file, last)| {
            let mut buf = [0; 160];
            let n = match file.read_at(&mut buf, 0) {
                Ok(n) if n > 0 => n,
                Ok(_) => return Ok(sum + last.get()),
                Err(error) if task_gone(&error) => return Ok(sum + last.get()),
                Err(error) => return Err(error),
            };
            let ns = std::str::from_utf8(&buf[..n])
                .ok()
                .and_then(|line| line.split_whitespace().next())
                .and_then(|word| word.parse::<u64>().ok())
                .ok_or_else(|| io::Error::other("invalid schedstat counter"))?;
            last.set(ns);
            Ok(sum + ns)
        })
    }
}

pub(super) fn worker_pid(owner: u32, executable: &Path) -> io::Result<Option<u32>> {
    for task in fs::read_dir(format!("/proc/{owner}/task"))? {
        let children = match fs::read_to_string(task?.path().join("children")) {
            Ok(children) => children,
            Err(error) if task_gone(&error) => continue,
            Err(error) => return Err(error),
        };
        for child in children.split_whitespace() {
            if let Ok(pid) = child.parse()
                && fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|path| path == executable)
            {
                return Ok(Some(pid));
            }
        }
    }
    Ok(None)
}

struct Window {
    handle: PluginHandle,
    id: freya::winit::window::WindowId,
    driver: &'static str,
    size: (u32, u32),
    scale: f64,
}

impl Window {
    fn click(&self, point: Point) {
        for name in [
            MouseEventName::MouseMove,
            MouseEventName::MouseDown,
            MouseEventName::MouseUp,
        ] {
            self.handle.send_platform_event(
                PlatformEvent::Mouse {
                    name,
                    cursor: point.into(),
                    button: (name != MouseEventName::MouseMove).then_some(MouseButton::Left),
                },
                self.id,
            );
        }
    }

    fn wheel(&self, point: Point, step: usize) {
        let dy = if step % CYCLE < CYCLE / 2 {
            -WHEEL_LINE
        } else {
            WHEEL_LINE
        };
        self.handle.send_platform_event(
            PlatformEvent::Wheel {
                name: WheelEventName::Wheel,
                scroll: (0., dy * self.scale).into(),
                cursor: point.into(),
                source: WheelSource::Line,
                phase: TouchPhase::Moved,
            },
            self.id,
        );
    }

    fn close(&self) {
        self.handle
            .send_event_loop_event(NativeEvent::Generic(NativeGenericEvent::User(
                GlobalUserEvent::Exit,
            )));
    }
}

struct Stage {
    start: Option<(u64, Instant)>,
    cpu_ns: u64,
    frames_us: Vec<f64>,
}

impl Stage {
    fn new(steps: usize) -> Self {
        Self {
            start: None,
            cpu_ns: 0,
            frames_us: Vec::with_capacity(steps * 4),
        }
    }

    fn begin(&mut self) {
        self.start
            .get_or_insert_with(|| (clock_ns(ClockId::ThreadCPUTime), Instant::now()));
    }

    fn end(&mut self) {
        if let Some((cpu, wall)) = self.start.take() {
            self.cpu_ns += clock_ns(ClockId::ThreadCPUTime).saturating_sub(cpu);
            self.frames_us
                .push(wall.elapsed().as_secs_f64() * 1_000_000.);
        }
    }

    fn report(mut self) -> Value {
        json!({"cpu_us_total": self.cpu_ns as f64 / 1_000., "wall": stats(&mut self.frames_us)})
    }
}

#[derive(Default)]
struct Watch {
    labels: [Option<Point>; TITLES.len()],
    viewport: Option<(Point, Point)>,
    image_id: Option<u32>,
    composited: usize,
    stages: Option<[Stage; STAGES.len()]>,
}

struct Probe {
    ready: Option<mpsc::Sender<Window>>,
    watch: Arc<Mutex<Watch>>,
}

impl FreyaPlugin for Probe {
    fn plugin_id(&self) -> &'static str {
        "peersus-map-zoom"
    }

    fn on_event(&mut self, event: &mut PluginEvent<'_>, handle: PluginHandle) {
        use PluginEvent::*;
        if let WindowCreated {
            window,
            graphics_driver,
            ..
        } = event
            && let Some(ready) = self.ready.take()
        {
            let size = window.inner_size();
            let _ = ready.send(Window {
                handle,
                id: window.id(),
                driver: graphics_driver,
                size: (size.width, size.height),
                scale: window.scale_factor(),
            });
        }
        let mut watch = self.watch.lock().unwrap();
        if let AfterPresenting { tree, .. } = event {
            let center = |id| {
                let area = tree.layout.get(id)?.area;
                (area.width() > 0. && area.height() > 0.).then_some((
                    (
                        f64::from(area.min_x() + area.width() / 2.),
                        f64::from(area.min_y() + area.height() / 2.),
                    ),
                    (f64::from(area.width()), f64::from(area.height())),
                ))
            };
            if watch.stages.is_none() {
                watch.labels = [None; TITLES.len()];
                for (id, element) in &tree.elements {
                    if let Some(label) = Label::try_downcast(element.as_ref())
                        && let Some(index) = TITLES.iter().position(|title| label.text == *title)
                        && let Some((point, _)) = center(id)
                    {
                        watch.labels[index] = Some(point);
                    }
                }
                watch.viewport = tree.listeners.get(&EventName::Wheel).and_then(|wheel| {
                    let sized = tree.listeners.get(&EventName::Sized)?;
                    wheel
                        .iter()
                        .filter(|id| sized.contains(id))
                        .filter_map(center)
                        .max_by(|(_, a), (_, b)| (a.0 * a.1).total_cmp(&(b.0 * b.1)))
                });
            }
            if let Some((point, size)) = watch.viewport {
                for (id, element) in &tree.elements {
                    let Some(image) = (element.as_ref() as &dyn Any).downcast_ref::<ImageElement>()
                    else {
                        continue;
                    };
                    if center(id) == Some((point, size)) {
                        let unique = image.image_handle.image.unique_id();
                        if watch.image_id.replace(unique) != Some(unique) {
                            watch.composited += 1;
                        }
                        break;
                    }
                }
            }
        }
        let Some(stages) = &mut watch.stages else {
            return;
        };
        let (stage, begin) = match event {
            StartedUpdatingTree { .. } => (0, true),
            FinishedUpdatingTree { .. } => (0, false),
            StartedMeasuringLayout { .. } => {
                stages[7].begin();
                (1, true)
            }
            FinishedMeasuringLayout { .. } => (1, false),
            BeforeRender { .. } => {
                stages[7].begin();
                (2, true)
            }
            AfterRender { .. } => (2, false),
            BeforePresenting { .. } => (3, true),
            AfterPresenting { .. } => {
                stages[7].end();
                (3, false)
            }
            StartedMeasuringEvents { .. } => (4, true),
            FinishedMeasuringEvents { .. } => (4, false),
            StartedPollingTasks { .. } => (5, true),
            FinishedPollingTasks { .. } => (5, false),
            BeforeAccessibility { .. } => (6, true),
            AfterAccessibility { .. } => (6, false),
            _ => return,
        };
        if begin {
            stages[stage].begin()
        } else {
            stages[stage].end()
        }
    }
}

#[derive(Default)]
struct Canvas {
    caption: bool,
    online: bool,
}

impl Canvas {
    fn inspect(page: &Node) -> Option<Self> {
        let mut scene = Self::default();
        let mut declared = false;
        fixture::visit(page, &mut |node| match node {
            Node::Canvas { id, style, .. } if id == "map" => declared = style.follow.is_none(),
            Node::Text { text, .. } => {
                scene.caption |= text.contains("± 10 m");
                scene.online |= text.contains("© OpenMapTiles");
            }
            Node::Drawing { .. } => panic!("Location sent declarative drawing geometry"),
            _ => {}
        });
        declared.then_some(scene)
    }
}

fn tap(window: &Window, watch: &Mutex<Watch>, label: usize) -> AnyResult<()> {
    let point = until(TITLES[label], || Ok(watch.lock().unwrap().labels[label]))?;
    window.click(point);
    Ok(())
}

fn canvas(
    rt: &tokio::runtime::Runtime,
    client: &LocalControl,
    mut ready: impl FnMut(&Canvas) -> bool,
) -> AnyResult<Canvas> {
    until("a populated map page", || {
        let snapshot = rt.block_on(client.state())?;
        Ok(fixture::page(&snapshot)
            .and_then(Canvas::inspect)
            .filter(&mut ready))
    })
}

fn pause_until(deadline: Instant) {
    thread::sleep(deadline.saturating_duration_since(Instant::now()));
}

struct Phase {
    start_ns: u64,
    end_ns: u64,
    unix_start_ms: f64,
    samples: Vec<[u64; 3]>,
    allocated: Vec<usize>,
    stages: [Stage; STAGES.len()],
    alloc: Option<usize>,
    composited: usize,
}

impl Phase {
    fn report(self, progress: &fixture::Progress) -> Value {
        let count = |events: &[u64]| {
            events
                .iter()
                .filter(|at| (self.start_ns..self.end_ns).contains(*at))
                .count()
        };
        let answers = count(&progress.answers);
        let elapsed_us = (self.end_ns - self.start_ns) as f64 / 1_000.;
        let processes = ["window_main_thread", "guest_process", "owner_process"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let mut bins = self
                    .samples
                    .iter()
                    .map(|bin| bin[index] as f64 / 1_000.)
                    .collect::<Vec<_>>();
                let by_slot = bins.clone();
                let total = bins.iter().sum::<f64>();
                (
                    name.to_owned(),
                    json!({"cpu_us_total":total,"cpu_us_per_slot":total / bins.len() as f64,
                    "cpu_us_per_settled_publish":(answers != 0).then(|| total / answers as f64),
                    "cpu_percent_of_one_core":100. * total / elapsed_us,"slots":stats(&mut bins),
                    "cpu_us_by_slot":by_slot}),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let stages = STAGES
            .into_iter()
            .zip(self.stages)
            .map(|(name, stage)| (name.to_owned(), stage.report()))
            .collect::<serde_json::Map<_, _>>();
        json!({
            "elapsed_us": elapsed_us, "unix_start_ms": self.unix_start_ms,
            "monotonic_start_ns": self.start_ns, "monotonic_end_ns": self.end_ns,
            "settled_publications": answers, "published_frames": count(&progress.frames),
            "composited_frames": self.composited,
            "rust_heap_allocated_bytes": self.alloc,
            "rust_heap_allocated_bytes_by_slot":self.allocated,
            "frames":stages, "cpu":processes,
        })
    }
}

fn phase(
    run: &Run,
    window: &Window,
    watch: &Mutex<Watch>,
    cpus: &[Cpu; 3],
    zoom: Option<Point>,
) -> AnyResult<Phase> {
    let read = || -> io::Result<_> { Ok([cpus[0].read()?, cpus[1].read()?, cpus[2].read()?]) };
    let mut samples = Vec::with_capacity(run.steps);
    let mut allocated = Vec::with_capacity(run.steps);
    let images_before = {
        let mut watch = watch.lock().unwrap();
        watch.stages = Some(std::array::from_fn(|_| Stage::new(run.steps)));
        watch.composited
    };
    let unix_start_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64() * 1_000.;
    let start_ns = now_ns();
    let mut deadline = Instant::now();
    let mut previous = read()?;
    let heap_start = heap();
    let mut previous_heap = heap_start;
    for step in 0..run.steps {
        if let Some(point) = zoom {
            window.wheel(point, step);
        }
        deadline += run.interval;
        pause_until(deadline);
        let next = read()?;
        for index in 0..next.len() {
            if next[index] < previous[index] {
                return Err("process CPU counter went backwards".into());
            }
        }
        samples.push(std::array::from_fn(|i| next[i] - previous[i]));
        previous = next;
        let next_heap = heap();
        if let Some((before, after)) = previous_heap.zip(next_heap) {
            allocated.push(after.allocated - before.allocated);
        }
        previous_heap = next_heap;
    }
    let end_ns = now_ns();
    let alloc = previous_heap
        .zip(heap_start)
        .map(|(end, start)| end.allocated - start.allocated);
    let (stages, composited) = {
        let mut watch = watch.lock().unwrap();
        (
            watch.stages.take().ok_or("missing phase counters")?,
            watch.composited - images_before,
        )
    };
    Ok(Phase {
        start_ns,
        end_ns,
        unix_start_ms,
        samples,
        allocated,
        stages,
        alloc,
        composited,
    })
}

fn drive(
    run: &Run,
    window: &Window,
    watch: &Mutex<Watch>,
    owner_pid: u32,
    socket: PathBuf,
) -> AnyResult<(Value, Phase, Phase)> {
    let rt = runtime()?;
    let client = LocalControl::new(socket);
    tap(window, watch, 0)?;
    tap(window, watch, 1)?;
    tap(window, watch, 2)?;
    canvas(&rt, &client, |map| map.caption)?;
    until("the real map bitmap", || Ok(watch.lock().unwrap().image_id))?;
    tap(window, watch, 1)?;
    let (point, viewport) = until("the laid-out viewport", || {
        Ok(watch.lock().unwrap().viewport)
    })?;

    let ready = |map: &Canvas| map.online == (run.span == Span::Street);
    canvas(&rt, &client, ready)?;
    // Fill the same caches both halves will use and let the local tile pipeline settle.
    for step in 0..CYCLE {
        window.wheel(point, step);
        thread::sleep(run.interval);
    }
    thread::sleep(Duration::from_secs(1));
    canvas(&rt, &client, ready)?;
    let guest = until("the supervised Wasm worker", || {
        Ok(worker_pid(owner_pid, &run.worker)?)
    })?;
    let cpus = [
        Cpu::main(std::process::id())?,
        Cpu::process(guest)?,
        Cpu::process(owner_pid)?,
    ];
    let idle = phase(run, window, watch, &cpus, None)?;
    let zoom = phase(run, window, watch, &cpus, Some(point))?;
    if worker_pid(owner_pid, &run.worker)? != Some(guest) {
        return Err("the Wasm worker restarted during measurement".into());
    }
    let description = json!({
        "window_size":window.size, "window_scale":window.scale, "viewport":viewport,
        "renderer":window.driver, "online_detail":(run.span == Span::Street),
        "render_path":"extension canvas",
    });
    Ok((description, idle, zoom))
}

pub(super) fn run(mode: &str, args: &[String]) -> AnyResult<()> {
    if mode == "--map-zoom-owner" {
        let [root, span, manifest, module, worker] = args else {
            return Err("invalid benchmark child arguments".into());
        };
        if fs::read_dir(root)?.next().is_some() {
            return Err("benchmark child requires an empty scratch directory".into());
        }
        return runtime()?.block_on(fixture::owner(
            Path::new(root),
            Span::parse(span)?,
            Path::new(manifest),
            Path::new(module),
            Path::new(worker),
        ));
    }
    let run = Run::parse(args)?;
    let mut owner = Owner::start(&run)?;
    let socket = owner.socket();
    let watch = Arc::new(Mutex::new(Watch::default()));
    let (ready, opened) = mpsc::channel();
    let config = peersus_ui::config_at(socket.clone(), Default::default())?.with_plugin(Probe {
        ready: Some(ready),
        watch: Arc::clone(&watch),
    });
    let (owner_pid, steps, interval, span) = (owner.child.id(), run.steps, run.interval, run.span);
    let driver = thread::spawn(move || -> AnyResult<_> {
        let window = opened.recv_timeout(WAIT)?;
        let result = drive(&run, &window, &watch, owner_pid, socket);
        window.close();
        result
    });
    launch(config);
    let measured = driver.join().map_err(|_| "benchmark driver panicked")?;
    let progress = owner.stop()?;
    let (fixture, idle, zoom) = measured?;
    let idle = idle.report(&progress);
    let zoom = zoom.report(&progress);
    // A frame can settle multiple coalesced input steps.
    if idle["settled_publications"] != 0 || zoom["settled_publications"].as_u64() == Some(0) {
        return Err("the idle page was not settled or zoom never settled a camera".into());
    }
    if zoom["composited_frames"].as_u64().unwrap_or_default() < (steps as u64 * 9 / 10)
        || zoom["frames"]["frame"]["wall"]["count"]
            .as_u64()
            .unwrap_or(0)
            == 0
    {
        return Err("zoom did not composite enough distinct camera frames".into());
    }
    println!(
        "{:#}",
        json!({
            "tool":"map_zoom", "span":span.name(), "steps":steps, "interval_ms":interval.as_millis(),
            "wheel_line_px":WHEEL_LINE, "cycle":CYCLE, "fixture":fixture, "idle":idle, "zoom":zoom,
            "not_measured":["Skia C++ heap", "interpreter fuel (test-only hook)", "real tile network and GPS"],
        })
    );
    Ok(())
}
