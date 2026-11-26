//! The window never reads a display list. Each mounted canvas watches one raster line and the
//! UI observes only the latest image handle, independently of the application's snapshot loop.
use crate::{follow, link::Link, theme::Palette};
use freya::{
    elements::image::{ImageHandle, ImagePainter},
    engine::prelude::{
        AlphaType, Budgeted, Canvas, ColorType, Data, ImageInfo, SkImage, SkSurface, SurfaceOrigin,
        gpu::PurgeResourceOptions, raster_from_data, render_target,
    },
    prelude::*,
};
use peersus_api::{
    canvas::{CanvasA11y, CanvasPalette, CanvasRole, MAX_CANVAS_SCALE},
    elements::Event,
};
use peersus_app::ipc::{FrameKey, FramePoll, FrameReader, ReceivedFrame};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
#[cfg(any(debug_assertions, feature = "development-diagnostics"))]
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
    },
    time::Instant,
};

#[cfg(any(debug_assertions, feature = "development-diagnostics"))]
struct FirstFrame {
    showing: u64,
    mounted: Instant,
    drawn: AtomicBool,
    refused_rasters: AtomicU64,
    read_errors: AtomicU64,
}

#[cfg(any(debug_assertions, feature = "development-diagnostics"))]
impl PartialEq for FirstFrame {
    fn eq(&self, other: &Self) -> bool {
        self.showing == other.showing
    }
}

#[cfg(any(debug_assertions, feature = "development-diagnostics"))]
impl FirstFrame {
    fn mount() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let showing = NEXT.fetch_add(1, Relaxed);
        let mounted = Instant::now();
        peersus_diagnostics::dev_trace!(stage = "canvas-mount", showing, "Canvas mounted");
        Self {
            showing,
            mounted,
            drawn: AtomicBool::new(false),
            refused_rasters: AtomicU64::new(0),
            read_errors: AtomicU64::new(0),
        }
    }

    fn take(&self) -> Option<(u64, u64, u64)> {
        (!self.drawn.load(Relaxed) && !self.drawn.swap(true, Relaxed)).then(|| {
            (
                self.mounted.elapsed().as_millis() as u64,
                self.refused_rasters.load(Relaxed),
                self.read_errors.load(Relaxed),
            )
        })
    }

    fn trace_draw(&self) {
        if let Some((elapsed_ms, refused_rasters, read_errors)) = self.take() {
            peersus_diagnostics::dev_trace!(
                stage = "canvas-first-frame",
                showing = self.showing,
                elapsed_ms,
                refused_rasters,
                read_errors,
                "Compositor drew the first canvas frame"
            );
        }
    }
}

#[derive(Clone)]
pub(crate) struct Source(pub Link);

#[derive(Clone, PartialEq)]
struct Visual {
    image: ImageHandle,
    settled: u64,
}

impl Visual {
    fn from_frame(frame: ReceivedFrame) -> Option<Self> {
        Some(Self {
            image: raster(frame.width, frame.height, frame.pixels)?,
            settled: frame.settled,
        })
    }
}

fn raster(
    width: u32,
    height: u32,
    pixels: impl AsRef<[u8]> + Send + 'static,
) -> Option<ImageHandle> {
    let info = ImageInfo::new(
        (width as i32, height as i32),
        ColorType::RGBA8888,
        AlphaType::Premul,
        None,
    );
    // Skia owns the original IPC allocation (and its reservation) until its final
    // image reference is released. ImageHandle needs no second byte owner or copy.
    let data = Data::new_with_owner(pixels);
    let image = raster_from_data(&info, data, width as usize * 4)?;
    Some(ImageHandle::new(image, Bytes::new()))
}

#[derive(Default)]
struct CanvasTexture {
    device: Option<WindowGpuDevice>,
    texture: Option<Texture>,
    used_gpu: bool,
}

struct Texture {
    image: SkImage,
    target: TextureTarget,
    frame_id: u32,
}

struct TextureTarget {
    surface: SkSurface,
    // Drop the Skia image and surface before the Vulkan device they reference.
    _lease: Option<Rc<dyn Any>>,
}

impl CanvasTexture {
    fn image(&mut self, canvas: &Canvas, source: &SkImage) -> Option<&SkImage> {
        let Some(mut context) = canvas.recording_context() else {
            self.texture = None;
            return None;
        };
        self.used_gpu = true;
        let reusable = self.texture.as_ref().is_some_and(|texture| {
            texture.image.dimensions() == source.dimensions()
                && texture.image.is_valid(Some(context.as_recorder()))
        });
        let frame_id = source.unique_id();
        if reusable
            && self
                .texture
                .as_ref()
                .is_some_and(|texture| texture.frame_id == frame_id)
        {
            return self.texture.as_ref().map(|texture| &texture.image);
        }

        let pixels = source.peek_pixels()?;
        let mut target = match self.texture.take() {
            Some(Texture { image, target, .. }) if reusable => {
                // Release the previous temporary image before overwriting its backing texture.
                drop(image);
                target
            }
            retired => {
                drop(retired);
                let surface = render_target(
                    &mut context,
                    Budgeted::Yes,
                    pixels.info(),
                    None,
                    SurfaceOrigin::TopLeft,
                    None,
                    None,
                    None,
                )?;
                TextureTarget {
                    surface,
                    _lease: self.device.as_ref().and_then(WindowGpuDevice::lease),
                }
            }
        };
        if !target.surface.canvas().write_pixels(
            pixels.info(),
            pixels.bytes()?,
            pixels.row_bytes(),
            (0, 0),
        ) {
            return None;
        }
        self.texture = Some(Texture {
            image: target.surface.make_temporary_image()?,
            target,
            frame_id,
        });
        self.texture.as_ref().map(|texture| &texture.image)
    }

    fn release(&mut self, window: &SkSurface) {
        self.texture = None;
        if let Some(mut context) = window.direct_context() {
            context.check_async_work_completion();
            context.purge_unlocked_resources(PurgeResourceOptions::AllResources);
        }
    }
}

#[derive(Clone, Default)]
struct Update {
    ready: bool,
    visual: Option<Visual>,
}

/// A checked, already-received raster for a headless layout fixture. It exercises the normal Bitmap.
#[cfg(all(test, feature = "headless", not(target_os = "android")))]
#[derive(Clone)]
pub(crate) struct TestFrame {
    key: FrameKey,
    visual: Visual,
}

#[cfg(all(test, feature = "headless", not(target_os = "android")))]
impl TestFrame {
    pub(crate) fn new(key: FrameKey, width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        let layout = peersus_api::canvas::FrameLayout::tight(width, height).ok()?;
        if !peersus_api::canvas::valid_id(&key.canvas) || pixels.len() != layout.byte_len() {
            return None;
        }
        Some(Self {
            key,
            visual: Visual {
                image: raster(layout.width(), layout.height(), pixels)?,
                settled: 0,
            },
        })
    }
}

pub(crate) fn mount(key: &FrameKey, a11y: Rc<[CanvasA11y]>) -> Mounted {
    let source = try_consume_context::<Source>();
    let mut frame = use_state(|| {
        #[cfg(all(test, feature = "headless", not(target_os = "android")))]
        if let Some(fixture) = try_consume_root_context::<TestFrame>()
            && &fixture.key == key
        {
            return Some(fixture.visual);
        }
        None
    });
    let mut ready = use_state(|| {
        #[cfg(all(test, feature = "headless", not(target_os = "android")))]
        {
            frame.peek().is_some()
        }
        #[cfg(not(all(test, feature = "headless", not(target_os = "android"))))]
        {
            false
        }
    });
    let last_metrics = use_hook(|| Rc::new(Cell::new(None)));
    #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
    let first_frame = use_hook(|| {
        use peersus_diagnostics::tracing::{Level, enabled};
        (source.is_some() && enabled!(target: "peersus::dev", Level::TRACE))
            .then(|| Arc::new(FirstFrame::mount()))
    });
    let worker = use_hook(|| {
        let source = source?;
        let reader = FrameReader::new(source.0.client(), key.clone())?;
        let (updates, mut latest) = tokio::sync::watch::channel(Update::default());
        #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
        let timing = first_frame.clone();
        spawn(async move {
            while latest.changed().await.is_ok() {
                let update = latest.borrow_and_update().clone();
                ready.set_if_modified(update.ready);
                frame.set_if_modified(update.visual);
            }
        });
        Some(
            source
                .0
                .executor()
                .spawn(fetch(
                    reader,
                    updates,
                    #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
                    timing,
                ))
                .abort_handle(),
        )
    });
    use_drop(move || {
        if let Some(worker) = worker {
            worker.abort();
        }
    });
    Mounted {
        ready,
        frame,
        last_metrics,
        a11y,
        #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
        first_frame,
    }
}

async fn fetch(
    mut reader: FrameReader,
    updates: tokio::sync::watch::Sender<Update>,
    #[cfg(any(debug_assertions, feature = "development-diagnostics"))] timing: Option<
        Arc<FirstFrame>,
    >,
) {
    loop {
        match reader.next().await {
            Ok(FramePoll::Frame(frame)) => {
                let Some(visual) = Visual::from_frame(frame) else {
                    #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
                    if let Some(timing) = &timing {
                        timing.refused_rasters.fetch_add(1, Relaxed);
                    }
                    continue;
                };
                updates.send_replace(Update {
                    ready: true,
                    visual: Some(visual),
                });
            }
            Ok(FramePoll::Empty) => {
                updates.send_if_modified(|current| {
                    let changed = !current.ready;
                    current.ready = true;
                    changed
                });
            }
            Ok(FramePoll::Dropped) => {
                #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
                if let Some(timing) = &timing {
                    timing.refused_rasters.fetch_add(1, Relaxed);
                }
                // Releasing this canvas's previous image may make room for its replacement.
                // A direct stream cannot replay the dropped raster; leave room for its next frame.
                updates.send_if_modified(|current| current.visual.take().is_some());
            }
            _result @ (Ok(FramePoll::Closed | FramePoll::Busy) | Err(_)) => {
                #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
                if _result.is_err()
                    && let Some(timing) = &timing
                {
                    timing.read_errors.fetch_add(1, Relaxed);
                }
                updates.send_if_modified(|current| {
                    if !current.ready {
                        return false;
                    }
                    *current = Update::default();
                    true
                });
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

/// References to the mounted raster, separate from the viewport's input and layout state.
#[derive(Clone, PartialEq)]
pub(crate) struct Mounted {
    ready: State<bool>,
    frame: State<Option<Visual>>,
    last_metrics: Rc<Cell<Option<Metrics>>>,
    a11y: Rc<[CanvasA11y]>,
    #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
    first_frame: Option<Arc<FirstFrame>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Metrics {
    scale: u32,
    palette: CanvasPalette,
}

impl Mounted {
    /// Theme/DPI changes require a new drawing even when logical layout did not change.
    pub fn measure(&self, palette: Palette, changed: impl FnOnce()) -> Option<Metrics> {
        let now = (*self.ready.read()).then(|| Metrics::of(palette));
        if self.last_metrics.replace(now) != now {
            changed();
        }
        now
    }

    pub fn bitmap(
        &self,
        preview: Option<(follow::Motion, State<u32>)>,
        send: Rc<EventHandler<Event>>,
    ) -> Element {
        Bitmap {
            frame: self.frame,
            preview,
            a11y: self.a11y.clone(),
            send,
            #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
            first_frame: self.first_frame.clone(),
        }
        .into()
    }
}

impl Metrics {
    fn of(p: Palette) -> Self {
        let scale = *Platform::get().current_window().scale_factor.read();
        let rgba = |(r, g, b)| [r, g, b, 255];
        let (r, g, b) = p.background;
        Self {
            scale: ((1000. * scale).round() as u32).clamp(1, MAX_CANVAS_SCALE),
            palette: CanvasPalette {
                dark: 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b) < 128_000,
                surface: rgba(p.surface),
                raised: rgba(p.raised),
                text: rgba(p.text),
                muted: rgba(p.muted),
                border: rgba(p.border),
                scrim: [0, 0, 0, 180],
                accent: rgba(p.receive),
                success: rgba(p.good),
                danger: rgba(p.bad),
            },
        }
    }

    pub fn event(
        self,
        id: String,
        (x, y, width, height): (i32, i32, u32, u32),
        padding: u16,
    ) -> Event {
        let padding = u32::from(padding) * 1000;
        Event::CanvasSize {
            id,
            x: x.saturating_add(padding as i32),
            y: y.saturating_add(padding as i32),
            width: width.saturating_sub(padding * 2),
            height: height.saturating_sub(padding * 2),
            scale: self.scale,
            palette: self.palette,
        }
    }
}

#[derive(PartialEq)]
struct Bitmap {
    frame: State<Option<Visual>>,
    preview: Option<(follow::Motion, State<u32>)>,
    a11y: Rc<[CanvasA11y]>,
    send: Rc<EventHandler<Event>>,
    #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
    first_frame: Option<Arc<FirstFrame>>,
}

impl Component for Bitmap {
    fn render(&self) -> impl IntoElement {
        let frame = self.frame.read();
        let viewport = rect().width(Size::fill()).height(Size::fill());
        let Some(frame) = frame.as_ref() else {
            return viewport;
        };
        let overlay = (!self.a11y.is_empty()).then(|| Overlay {
            items: self.a11y.clone(),
            send: self.send.clone(),
        });
        let picture = Picture(
            frame.image.clone(),
            #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
            self.first_frame.clone(),
        );
        if let Some((motion, moved)) = &self.preview {
            let _ = moved.read();
            let pose = motion.for_frame(frame.settled);
            let motion = motion.clone();
            viewport
                .on_sized(move |e: freya::prelude::Event<SizedEventData>| {
                    motion.set_origin((e.area.min_x(), e.area.min_y()));
                })
                .child(
                    rect()
                        .width(Size::fill())
                        .height(Size::fill())
                        .maybe(pose != follow::Pose::IDENTITY, |el| {
                            el.offset_x(pose.x / pose.scale)
                                .offset_y(pose.y / pose.scale)
                                .scale((pose.scale, pose.scale))
                                .transform_origin(TransformOrigin::top_left())
                        })
                        .child(picture)
                        .maybe_child(overlay),
                )
        } else {
            viewport.child(picture).maybe_child(overlay)
        }
    }
}

#[derive(PartialEq)]
struct Picture(
    ImageHandle,
    #[cfg(any(debug_assertions, feature = "development-diagnostics"))] Option<Arc<FirstFrame>>,
);

impl Component for Picture {
    fn render(&self) -> impl IntoElement {
        let texture = use_hook(|| {
            Rc::new(RefCell::new(CanvasTexture {
                device: try_consume_root_context(),
                ..CanvasTexture::default()
            }))
        });
        let painter = use_hook(|| {
            let texture = texture.clone();
            #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
            let first_frame = self.1.clone();
            ImagePainter::new(move |canvas, source, rect, sampling, paint| {
                let mut texture = texture.borrow_mut();
                let image = texture.image(canvas, source).unwrap_or(source);
                canvas.draw_image_rect_with_sampling_options(image, None, rect, sampling, paint);
                #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
                if let Some(timing) = &first_frame {
                    timing.trace_draw();
                }
            })
        });
        use_drop(move || {
            if !texture.borrow().used_gpu {
                return;
            }
            // The window may be closed already, or it may have a new ID after Android resumes.
            // When still live, release on its render pass with the correct GL context current.
            if let Some(id) = try_consume_root_context::<CurrentWindowId>()
                && let Some(window) = Platform::get().try_window(id.0)
            {
                drop(window.post_render_callback(move |surface| {
                    texture.borrow_mut().release(surface);
                }));
            }
        });
        image(self.0.clone())
            .with_painter(painter)
            .width(Size::fill())
            .height(Size::fill())
            .position(Position::new_absolute().left(0.).top(0.))
            .aspect_ratio(AspectRatio::None)
            .sampling_mode(SamplingMode::Bilinear)
            .a11y_role(AccessibilityRole::GenericContainer)
    }
}

#[derive(PartialEq)]
struct Overlay {
    items: Rc<[CanvasA11y]>,
    send: Rc<EventHandler<Event>>,
}

impl Component for Overlay {
    fn render(&self) -> impl IntoElement {
        rect()
            .position(Position::new_absolute().left(0.).top(0.))
            .width(Size::fill())
            .height(Size::fill())
            .children(self.items.iter().map(|item| Semantic {
                item: item.clone(),
                send: self.send.clone(),
            }))
    }
}

#[derive(PartialEq)]
struct Semantic {
    item: CanvasA11y,
    send: Rc<EventHandler<Event>>,
}

impl Component for Semantic {
    fn render_key(&self) -> DiffKey {
        DiffKey::from(&(std::any::TypeId::of::<Self>(), &self.item.id))
    }

    fn render(&self) -> impl IntoElement {
        let item = &self.item;
        let action = matches!(item.role, CanvasRole::Button | CanvasRole::Link);
        let role = match item.role {
            CanvasRole::Button => AccessibilityRole::Button,
            CanvasRole::Link => AccessibilityRole::Link,
            CanvasRole::Image => AccessibilityRole::Image,
            CanvasRole::Text => AccessibilityRole::Label,
            CanvasRole::Group => AccessibilityRole::Group,
        };
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        rect()
            .position(
                Position::new_absolute()
                    .left(item.rect.x as f32 / 1000.)
                    .top(item.rect.y as f32 / 1000.),
            )
            .width(Size::px(item.rect.width as f32 / 1000.))
            .height(Size::px(item.rect.height as f32 / 1000.))
            .a11y_id(a11y)
            .a11y_role(role)
            .a11y_focusable(action)
            .interactive(action)
            .a11y_builder(|node| {
                if role == AccessibilityRole::Label {
                    node.set_value(item.label.clone());
                } else {
                    node.set_label(item.label.clone());
                }
            })
            .maybe(action, |el| {
                let (send, id) = (self.send.clone(), item.id.clone());
                el.on_press(move |_| send.call(Event::Press { id: id.clone() }))
            })
            .maybe(action && focus() == Focus::Keyboard, |el| {
                el.border(Border::new().fill(crate::theme::DARK.text).width(2.))
            })
    }
}

#[cfg(all(test, feature = "headless", not(target_os = "android")))]
mod tests;

#[cfg(all(test, any(debug_assertions, feature = "development-diagnostics")))]
#[test]
fn first_draw_drains_only_this_showings_observed_refusals() {
    let first = FirstFrame::mount();
    first.refused_rasters.fetch_add(2, Relaxed);
    first.read_errors.fetch_add(1, Relaxed);
    assert!(matches!(first.take(), Some((_, 2, 1))));
    assert!(first.take().is_none());
    let reopened = FirstFrame::mount();
    assert_ne!(first.showing, reopened.showing);
    assert!(matches!(reopened.take(), Some((_, 0, 0))));
}
