//! Drive the real desktop window, owner and isolated WASM helper with the same wheel input
//! over a sparse and a dense guest scene. No application-side drawing or test-only frame source.
use super::map_zoom::{AnyResult, Cpu, until, worker_pid};
use freya::{
    elements::image::ImageElement,
    prelude::{
        platform::{MouseEventName, PlatformEvent, WheelEventName},
        *,
    },
};
use freya_winit::{
    plugins::{FreyaPlugin, PluginEvent, PluginHandle},
    renderer::{RendererAppMessage, RendererMessage},
};
use peersus_api::{
    contributions::Scope,
    manifest::{EXECUTION_ABI_VERSION, Execution, Manifest},
};
use peersus_app::{
    ipc,
    lifetime::Presence,
    plugin_host::{PluginSpec, Registry},
    runtime::Runtime,
};
use peersus_core::{
    Settings,
    config::{NetworkSettings, Theme},
    model::Command,
    plugins::{PluginId, Value as SettingValue, Values},
};
use peersus_net::{
    grants::StandaloneGrants, history::History, network_host::NetworkHost, peers::Peers,
    settings::SettingsStore, transport,
};
use peersus_plugin::module::{DesktopModules, ModuleFactory, ModuleLauncher};
use serde_json::{Value, json};
use skia_safe::{AlphaType, ColorType, ImageInfo};
use std::{
    any::Any,
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command as Process, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

const PLUGIN: &str = "canvas-bench";
const SOCKET: &str = "owner.sock";
const TARGET_HZ: u64 = 60;
const WHEEL_DELTA: f64 = 3.;
const WAIT: Duration = Duration::from_secs(20);
type Point = (f64, f64);

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

async fn own(
    root: &Path,
    manifest: &Path,
    module: &Path,
    target: &Path,
    canvas: bool,
) -> AnyResult<()> {
    let plugin = PluginId::new(PLUGIN)?;
    let package = peersus_api::package::split(&fs::read(manifest)?)
        .map_err(|_| "invalid benchmark package")?;
    let manifest: Manifest = serde_json::from_slice(&package.manifest)?;
    if manifest.execution
        != (Execution::Wasm {
            abi: EXECUTION_ABI_VERSION,
        })
    {
        return Err("the benchmark starts only a reviewed WASM module".into());
    }
    // An installed extension runs the program the person approved, from its own folder under the manifest's program name;
    // the application's private storage serves only that program. This represents fixture approval in an empty
    // directory, as the agreement below represents fixture consent, never the person's running owner.
    let module = if canvas {
        module.to_owned()
    } else {
        let folder = root.join("extension");
        fs::create_dir(&folder)?;
        let program = folder.join(manifest.program_file());
        fs::copy(module, &program)?;
        let sha256 = peersus_app::plugin_approval::program_digest(&program, u64::MAX)
            .ok_or("the benchmark module could not be measured")?;
        peersus_app::plugin_approval::write(
            &folder,
            &peersus_app::plugin_approval::Approval {
                program: manifest.program_file().to_owned(),
                execution: manifest.execution,
                sha256,
                capabilities: manifest.capabilities.keys().cloned().collect(),
                guards: peersus_app::plugin_approval::guards_of(&manifest),
                approved_at: 0,
            },
        )?;
        program
    };
    let module = module.as_path();
    let launcher: Arc<dyn peersus_plugin::supervisor::Launcher> = if canvas {
        Arc::new(ModuleLauncher::new(target.into(), module.into())?)
    } else {
        DesktopModules::beside(&target.join("app"))?.launcher(&manifest, module)?
    };
    let mut toggles = Values::default();
    if canvas {
        toggles.set(
            &manifest.contributions.settings,
            Scope::Global,
            "show-canvas",
            SettingValue::Toggle(true),
        )?;
    }
    let agreements = manifest
        .agreements
        .as_ref()
        .ok_or("missing benchmark package agreement")?;
    // This represents fixture consent in an empty directory, never the person's running owner.
    peersus_app::plugin_agreements::accept(
        root,
        PLUGIN,
        &peersus_core::agreements::digest(agreements),
        1,
    )?;
    let mut registry = Registry::default();
    registry.register(PluginSpec {
        id: plugin.clone(),
        manifest,
        program: Some(module.to_owned()),
        arguments: Vec::new(),
        restart_policy: None,
        languages: Arc::new([]),
    })?;
    let settings = Settings {
        network: NetworkSettings {
            bind: Some(std::net::Ipv4Addr::LOCALHOST.into()),
            allow_relay: false,
            ..Default::default()
        },
        device_name: "Resource benchmark".into(),
        theme: Theme::Dark,
        reduced_motion: true,
        extension_pages_together: true,
        plugins: [(plugin.clone(), toggles)].into(),
        ..Default::default()
    };
    let endpoint = transport::bind(iroh::SecretKey::generate(), &settings.network).await?;
    let mut store = SettingsStore::open(root.join("preferences.json"))?;
    store.replace(settings.clone())?;
    let network = NetworkHost::new(
        endpoint,
        settings.device_name,
        Peers::open(root.join("peers.json"))?,
        History::open(root.join("history.json"))?,
    );
    let sockets = root.join("worker");
    fs::create_dir(&sockets)?;
    let grants = StandaloneGrants::open(root.join("grants.json"))?;
    let (owner, client) = Runtime::new(network, grants, store, registry, sockets)?;
    // No platform services, peer/device state, permissions or relay are supplied to this owner.
    let owner = owner
        .with_launcher(plugin, launcher)
        .with_packages(peersus_app::plugin_manager::Manager::new(root));
    let mut snapshots = client.subscribe();
    let run = tokio::spawn(owner.run());
    // The Canvas workload needs the guest's primary page; startup/idle only needs an
    // authenticated live helper. Never invoke a submitted extension's actions.
    tokio::time::timeout(
        WAIT,
        snapshots.wait_for(|snapshot| {
            snapshot.plugins.iter().any(|entry| {
                entry.id.as_str() == PLUGIN
                    && entry.agreed
                    && entry.status == peersus_core::model::PluginStatus::Running
                    && (!canvas
                        || entry.manifest.contributions.pages.iter().any(|page| {
                            page.id == "show-canvas"
                                && page.placement == peersus_api::contributions::Placement::Primary
                        }))
            })
        }),
    )
    .await
    .map_err(|_| "the WASM helper did not become ready for the benchmark")??;
    let listener = tokio::net::UnixListener::bind(root.join(SOCKET))?;
    let server = tokio::spawn(ipc::serve(listener, client.clone(), Presence::default()));
    // Closing the benchmark's pipe ends the owner and its supervised worker.
    tokio::task::spawn_blocking(|| io::copy(&mut io::stdin().lock(), &mut io::sink())).await??;
    client.command(Command::Shutdown {}).await?;
    run.await?;
    server.await??;
    Ok(())
}

struct Owner {
    child: Child,
    root: tempfile::TempDir,
    startup: Duration,
}

impl Owner {
    fn start(manifest: &Path, module: &Path, target: &Path, canvas: bool) -> AnyResult<Self> {
        let root = tempfile::Builder::new()
            .prefix("pcanvas-")
            .tempdir_in("/tmp")?;
        let began = Instant::now();
        let mut command = Process::new(std::env::current_exe()?);
        command
            .arg(if canvas {
                "--canvas-owner"
            } else {
                "--extension-bench-owner"
            })
            .arg(root.path())
            .arg(manifest)
            .arg(module)
            .arg(target)
            .stdin(Stdio::piped());
        if !canvas {
            command.env_clear().env("HOME", root.path());
        }
        let child = command.spawn()?;
        let mut owner = Self {
            child,
            root,
            startup: Duration::ZERO,
        };
        until("benchmark owner IPC", || {
            if let Some(status) = owner.child.try_wait()? {
                return Err(format!("benchmark owner exited: {status}").into());
            }
            Ok(owner.socket().exists().then_some(()))
        })?;
        owner.startup = began.elapsed();
        Ok(owner)
    }

    fn socket(&self) -> PathBuf {
        self.root.path().join(SOCKET)
    }

    fn stop(&mut self) -> AnyResult<()> {
        self.child.stdin.take();
        let status = until("canvas owner shutdown", || Ok(self.child.try_wait()?))?;
        if !status.success() {
            return Err(format!("canvas owner ended: {status}").into());
        }
        Ok(())
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.child.stdin.take();
        let end = Instant::now() + Duration::from_secs(3);
        while self.child.try_wait().is_ok_and(|status| status.is_none()) {
            if Instant::now() >= end {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Scene {
    Light,
    Dense,
}

impl Scene {
    fn from_pixels(pixels: &[u8]) -> Option<Self> {
        match pixels.get(..4)? {
            [12, 22, 35, 255] => Some(Self::Dense),
            [12, 22, 36, 255] => Some(Self::Light),
            _ => None,
        }
    }

    fn from_image(element: &ImageElement) -> Option<Self> {
        Self::from_pixels(element.image_handle.image.peek_pixels()?.bytes()?)
    }

    fn index(self) -> usize {
        usize::from(self == Self::Dense)
    }
    fn paths(self) -> usize {
        if self == Self::Dense { 1500 } else { 30 }
    }
    fn label(self) -> &'static str {
        if self == Self::Dense {
            "Dense paths"
        } else {
            "Few paths"
        }
    }
}

struct Window {
    handle: PluginHandle,
    id: freya::winit::window::WindowId,
    driver: &'static str,
    size: (u32, u32),
    scale: f64,
}

impl Window {
    fn mouse(&self, name: MouseEventName, point: Point, clicked: bool) {
        self.handle.send_platform_event(
            PlatformEvent::Mouse {
                name,
                cursor: point.into(),
                button: clicked.then_some(MouseButton::Left),
            },
            self.id,
        );
    }

    fn click(&self, point: Point) {
        self.mouse(MouseEventName::MouseMove, point, false);
        self.mouse(MouseEventName::MouseDown, point, true);
        self.mouse(MouseEventName::MouseUp, point, true);
    }

    fn wheel(&self, point: Point, step: usize) {
        let dy = if step.is_multiple_of(2) {
            -WHEEL_DELTA
        } else {
            WHEEL_DELTA
        };
        self.handle.send_platform_event(
            PlatformEvent::Wheel {
                name: WheelEventName::Wheel,
                scroll: (0., dy * self.scale).into(),
                cursor: point.into(),
                source: WheelSource::Pixel,
                phase: TouchPhase::Moved,
            },
            self.id,
        );
    }

    fn close(&self) {
        self.handle
            .send_renderer_message(RendererMessage::App(RendererAppMessage::Command(
                AppCommand::Exit,
            )));
    }
}

#[derive(Default)]
struct Seen {
    presented: bool,
    id: Option<u32>,
    centres: [Option<Point>; 2],
    canvas: Option<(Point, (u32, u32))>,
    count: [u64; 2],
    screenshot: Option<Result<(), String>>,
}

struct Probe {
    window: Option<mpsc::Sender<Window>>,
    seen: Arc<Mutex<Seen>>,
    screenshot: Option<PathBuf>,
    canvas: bool,
}

impl FreyaPlugin for Probe {
    fn plugin_id(&self) -> &'static str {
        "peersus-canvas-bench"
    }

    fn on_event(&mut self, event: &mut PluginEvent<'_>, handle: PluginHandle) {
        match event {
            PluginEvent::WindowCreated {
                window,
                graphics_driver,
                ..
            } => {
                if let Some(ready) = self.window.take() {
                    let size = window.surface_size();
                    let _ = ready.send(Window {
                        handle,
                        id: window.id(),
                        driver: graphics_driver,
                        size: (size.width, size.height),
                        scale: window.scale_factor(),
                    });
                }
            }
            PluginEvent::AfterPresenting { tree, .. } => {
                let mut seen = self.seen.lock().unwrap();
                seen.presented = true;
                if !self.canvas {
                    return;
                }
                let centre = |id| {
                    let area = tree.layout.get(id)?.visible_area();
                    (area.width() > 0. && area.height() > 0.).then(|| {
                        (
                            f64::from(area.min_x() + area.width() / 2.),
                            f64::from(area.min_y() + area.height() / 2.),
                        )
                    })
                };
                if seen.centres.iter().any(Option::is_none) {
                    for (id, element) in &tree.elements {
                        if let Some(label) = Label::try_downcast(element.as_ref()) {
                            for scene in [Scene::Light, Scene::Dense] {
                                if label.text == scene.label() {
                                    seen.centres[scene.index()] = centre(id);
                                }
                            }
                        }
                    }
                }
                for (id, element) in &tree.elements {
                    let Some(image) = (element.as_ref() as &dyn Any).downcast_ref::<ImageElement>()
                    else {
                        continue;
                    };
                    let Some(scene) = Scene::from_image(image) else {
                        continue;
                    };
                    let Some(point) = centre(id) else {
                        continue;
                    };
                    let picture = &image.image_handle.image;
                    seen.canvas = Some((point, (picture.width() as u32, picture.height() as u32)));
                    let id = picture.unique_id();
                    if seen.id.replace(id) != Some(id) {
                        seen.count[scene.index()] += 1;
                    }
                    break;
                }
            }
            PluginEvent::AfterRender {
                window,
                canvas,
                tree,
                ..
            } if self.screenshot.is_some() => {
                let dense = tree.elements.values().any(|element| {
                    (element.as_ref() as &dyn Any)
                        .downcast_ref::<ImageElement>()
                        .and_then(Scene::from_image)
                        == Some(Scene::Dense)
                });
                if dense && let Some(path) = self.screenshot.take() {
                    let size = window.surface_size();
                    let result = capture(canvas, &path, size.width, size.height)
                        .map_err(|error| error.to_string());
                    self.seen.lock().unwrap().screenshot = Some(result);
                }
            }
            _ => {}
        }
    }
}

fn capture(canvas: &skia_safe::Canvas, path: &Path, width: u32, height: u32) -> AnyResult<()> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err("invalid capture size".into());
    }
    let info = ImageInfo::new(
        (width as i32, height as i32),
        ColorType::RGBA8888,
        AlphaType::Unpremul,
        None,
    );
    let stride = width as usize * 4;
    let mut pixels = vec![0; stride * height as usize];
    if !canvas.read_pixels(&info, &mut pixels, stride, (0, 0)) {
        return Err("window raster could not be read back".into());
    }
    if !pixels
        .chunks_exact(4)
        .any(|rgba| Scene::from_pixels(rgba) == Some(Scene::Dense))
    {
        return Err("the dense canvas pixels were not composited into the window".into());
    }
    ::image::save_buffer(path, &pixels, width, height, ::image::ColorType::Rgba8)?;
    Ok(())
}

const CPU_NAMES: [&str; 4] = [
    "window_main_thread",
    "window_process",
    "owner_process",
    "wasm_helper",
];

fn phase(
    window: &Window,
    seen: &Mutex<Seen>,
    point: Point,
    cpus: &[Cpu; 4],
    seconds: u64,
    mode: Option<Scene>,
) -> AnyResult<Value> {
    let counts = || seen.lock().unwrap().count;
    let cpu = || -> io::Result<[u64; 4]> {
        Ok([
            cpus[0].read()?,
            cpus[1].read()?,
            cpus[2].read()?,
            cpus[3].read()?,
        ])
    };
    let first = counts();
    let before = cpu()?;
    let start = Instant::now();
    let mut inputs = 0usize;
    if mode.is_some() {
        for slot in 0..seconds * TARGET_HZ {
            let deadline = start + Duration::from_nanos(slot * 1_000_000_000 / TARGET_HZ);
            thread::sleep(deadline.saturating_duration_since(Instant::now()));
            if Instant::now().saturating_duration_since(deadline) < Duration::from_millis(16) {
                window.wheel(point, inputs);
                inputs += 1;
            }
        }
    }
    thread::sleep((start + Duration::from_secs(seconds)).saturating_duration_since(Instant::now()));
    let elapsed = start.elapsed().as_secs_f64();
    let last = counts();
    let after = cpu()?;
    let presented = mode.map_or_else(
        || (last[0] - first[0]) + (last[1] - first[1]),
        |mode| last[mode.index()] - first[mode.index()],
    );
    if (mode.is_some() && presented == 0) || (mode.is_none() && presented != 0) {
        return Err("canvas presentation did not match the measurement phase".into());
    }
    let cpu = CPU_NAMES.into_iter().zip(before.into_iter().zip(after)).map(|(name,(before,after))| {
        let ms = after.saturating_sub(before) as f64 / 1_000_000.;
        (name.to_owned(), json!({"cpu_ms":ms,"cpu_ms_per_presented_frame":(presented > 0).then(|| ms / presented as f64),
                                "percent_of_one_core":100. * ms / (elapsed * 1_000.)}))
    }).collect::<serde_json::Map<_,_>>();
    Ok(
        json!({"elapsed_s":elapsed,"stroked_paths":mode.map(Scene::paths),"input_events":inputs,
              "distinct_canvas_frames_presented":presented,"achieved_fps":presented as f64 / elapsed,"cpu":cpu}),
    )
}

fn drive(
    window: &Window,
    seen: &Mutex<Seen>,
    owner: u32,
    worker: &Path,
    seconds: u64,
) -> AnyResult<Value> {
    let (point, pixels) = until("the first composited canvas", || {
        let status = seen.lock().unwrap();
        if let Some(Err(error)) = &status.screenshot {
            return Err(error.clone().into());
        }
        Ok(
            match (&status.screenshot, status.count[Scene::Dense.index()]) {
                (Some(Ok(())), 1..) => status.canvas,
                _ => None,
            },
        )
    })?;
    // A different ImageHandle (not just a preview transform) proves the press + move ran in the guest.
    let id = seen.lock().unwrap().id;
    window.mouse(MouseEventName::MouseDown, point, true);
    window.mouse(
        MouseEventName::MouseMove,
        (point.0 + 14., point.1 + 7.),
        true,
    );
    window.mouse(MouseEventName::MouseUp, (point.0 + 14., point.1 + 7.), true);
    until("a new canvas after dragging", || {
        Ok((seen.lock().unwrap().id != id).then_some(()))
    })?;
    let worker_pid_before = until("canvas WASM helper", || Ok(worker_pid(owner, worker)?))?;
    let cpus = [
        Cpu::main(std::process::id())?,
        Cpu::process(std::process::id())?,
        Cpu::process(owner)?,
        Cpu::process(worker_pid_before)?,
    ];
    thread::sleep(Duration::from_millis(250));
    let idle = phase(window, seen, point, &cpus, seconds, None)?;
    let mut result = json!({ "scenario":"canvas", "target_fps":TARGET_HZ, "window_size":window.size,
        "canvas_pixels":pixels, "window_scale":window.scale, "renderer":window.driver, "pointer_drag_observed":true, "idle":idle });
    for scene in [Scene::Light, Scene::Dense] {
        let button = until(scene.label(), || {
            Ok(seen.lock().unwrap().centres[scene.index()])
        })?;
        let before = seen.lock().unwrap().count[scene.index()];
        window.click(button);
        until("the selected canvas scene", || {
            Ok((seen.lock().unwrap().count[scene.index()] > before).then_some(()))
        })?;
        for step in 0..6 {
            window.wheel(point, step);
            thread::sleep(Duration::from_millis(21));
        }
        // Wait out the warm-up's rolling budget so the same number of frames may enter both intervals.
        thread::sleep(Duration::from_secs(1));
        result[if scene == Scene::Dense {
            "dense"
        } else {
            "light"
        }] = phase(window, seen, point, &cpus, seconds, Some(scene))?;
    }
    if worker_pid(owner, worker)? != Some(worker_pid_before) {
        return Err("WASM helper restarted during measurement".into());
    }
    Ok(result)
}

fn extension_worker(owner: u32, release: &Path) -> AnyResult<Option<u32>> {
    Ok(
        worker_pid(owner, &release.join("peersus-wasm-host"))?.or(worker_pid(
            owner,
            &release.join("peersus-wasm-canvas-host"),
        )?),
    )
}

fn measure_extension(
    seen: &Mutex<Seen>,
    owner: u32,
    release: &Path,
    seconds: u64,
    startup: Duration,
) -> AnyResult<Value> {
    let worker = until("the trusted WASM helper", || {
        extension_worker(owner, release)
    })?;
    until("first window presentation", || {
        Ok(seen.lock().unwrap().presented.then_some(()))
    })?;
    let pids = [std::process::id(), owner, worker];
    let cpus = [
        Cpu::process(pids[0])?,
        Cpu::process(owner)?,
        Cpu::process(worker)?,
    ];
    let main_thread = Cpu::main(pids[0])?;
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut peaks = [[None::<u64>; 2]; 3];
    loop {
        for (pid, peak) in pids.into_iter().zip(&mut peaks) {
            for (index, (file, key)) in [("status", "VmHWM:"), ("smaps_rollup", "Pss:")]
                .into_iter()
                .enumerate()
            {
                let sample = fs::read_to_string(format!("/proc/{pid}/{file}"))
                    .ok()
                    .and_then(|text| {
                        text.lines()
                            .find_map(|line| line.strip_prefix(key))?
                            .split_whitespace()
                            .next()?
                            .parse()
                            .ok()
                    });
                peak[index] = peak[index].max(sample);
            }
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        thread::sleep(left.min(Duration::from_millis(100)));
    }
    if extension_worker(owner, release)? != Some(worker) {
        return Err("the extension's WASM helper exited or restarted during measurement".into());
    }
    let worker_image = fs::read_link(format!("/proc/{worker}/exe"))?;
    let worker_name = worker_image
        .file_name()
        .ok_or("WASM helper has no executable name")?;
    let mut report = json!({
        "helper_program": worker_name.to_string_lossy(),
        "startup": { "startup_ms": startup.as_secs_f64() * 1000.0 },
        "window_main_thread": { "cpu_ms": main_thread.read()? as f64 / 1_000_000.0 },
    });
    for ((name, cpu), [rss, pss]) in ["window_process", "owner_process", "wasm_helper"]
        .into_iter()
        .zip(&cpus)
        .zip(peaks)
    {
        report[name] = json!({
            "cpu_ms": cpu.read()? as f64 / 1_000_000.0,
            "peak_resident_kib": rss.ok_or("no RSS sample for the extension benchmark")?,
            "peak_pss_kib": pss.ok_or("no PSS sample for the extension benchmark")?,
        });
    }
    Ok(report)
}

pub(super) fn run(mode: &str, args: &[String]) -> AnyResult<()> {
    let canvas = mode.starts_with("--canvas");
    if matches!(mode, "--canvas-owner" | "--extension-bench-owner") {
        let [root, manifest, module, target] = args else {
            return Err("expected scratch directory, manifest, WASM and helper location".into());
        };
        if fs::read_dir(root)?.next().is_some() {
            return Err("the benchmark needs an empty scratch directory".into());
        }
        return runtime()?.block_on(own(
            Path::new(root),
            Path::new(manifest),
            Path::new(module),
            Path::new(target),
            canvas,
        ));
    }
    let (manifest, module, target, seconds, screenshot) = match args {
        [manifest, module, worker, seconds, screenshot] if canvas => (
            manifest,
            module,
            worker,
            seconds,
            Some(PathBuf::from(screenshot)),
        ),
        [manifest, module, release, seconds] if !canvas => {
            (manifest, module, release, seconds, None)
        }
        _ => return Err(
            "expected manifest, WASM, helper location and seconds (plus a screenshot for Canvas)"
                .into(),
        ),
    };
    let manifest = fs::canonicalize(manifest)?;
    let (module, target) = (fs::canonicalize(module)?, fs::canonicalize(target)?);
    let seconds: u64 = seconds.parse()?;
    if !(1..=60).contains(&seconds) {
        return Err("each measurement phase needs 1 to 60 seconds".into());
    }
    let mut owner = Owner::start(&manifest, &module, &target, canvas)?;
    let pid = owner.child.id();
    let startup = owner.startup;
    let seen = Arc::new(Mutex::new(Seen::default()));
    let (ready, opened) = mpsc::channel();
    let config = peersus_ui::config_at(owner.socket(), Default::default())?.with_plugin(Probe {
        window: Some(ready),
        seen: seen.clone(),
        screenshot,
        canvas,
    });
    let observer = thread::spawn(move || -> AnyResult<_> {
        let window = opened.recv_timeout(WAIT)?;
        let report = if canvas {
            drive(&window, &seen, pid, &target, seconds)
        } else {
            measure_extension(&seen, pid, &target, seconds, startup)
        };
        window.close();
        report
    });
    launch(config);
    let report = observer
        .join()
        .map_err(|_| "canvas measurement thread panicked")?;
    owner.stop()?;
    println!("{:#}", report?);
    Ok(())
}
