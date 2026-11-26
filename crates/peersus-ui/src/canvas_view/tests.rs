use super::*;
use freya_testing::TestingRunner;
use peersus_api::canvas::CanvasRect;
use std::cell::RefCell;

fn image_at(left: u32, settled: u64) -> Visual {
    let (width, height) = (48, 32);
    let mut pixels = vec![0; (width * height * 4) as usize];
    for y in 10..20 {
        for x in left..left + 8 {
            let at = ((y * width + x) * 4) as usize;
            pixels[at..at + 4].copy_from_slice(&[100, 20, 0, 128]);
        }
    }
    Visual {
        image: raster(width, height, pixels).unwrap(),
        settled,
    }
}

#[test]
fn only_skias_last_reference_releases_the_original_raster() {
    struct Raster {
        pixels: Vec<u8>,
        _owner: std::sync::Arc<()>,
    }
    impl AsRef<[u8]> for Raster {
        fn as_ref(&self) -> &[u8] {
            &self.pixels
        }
    }
    let owner = std::sync::Arc::new(());
    let live = std::sync::Arc::downgrade(&owner);
    let tracked = Raster {
        pixels: vec![10, 20, 30, 255],
        _owner: owner,
    };
    let storage = tracked.pixels.as_ptr();
    let image = raster(1, 1, tracked).unwrap();
    let last = image.image.clone();
    drop(image);
    {
        let pixmap = last.peek_pixels().unwrap();
        let bytes = pixmap.bytes().unwrap();
        assert_eq!(bytes, &[10, 20, 30, 255]);
        assert_eq!(bytes.as_ptr(), storage);
    }
    assert!(live.upgrade().is_some());
    drop(last);
    assert!(live.upgrade().is_none());
}

#[test]
fn the_canvas_composites_premultiplied_pixels_and_settles_its_bitmap_preview() {
    let motion = follow::Motion::default();
    let canvas = motion.clone();
    #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
    let timing = Arc::new(FirstFrame::mount());
    #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
    let observed = Arc::clone(&timing);
    let (mut runner, (mut frame, mut moved)) = TestingRunner::new(
        move || {
            let (frame, moved): (State<Option<Visual>>, State<u32>) = use_consume();
            rect()
                .width(Size::fill())
                .height(Size::fill())
                .overflow(Overflow::Clip)
                .background((0, 0, 0))
                .child(Bitmap {
                    frame,
                    preview: Some((canvas.clone(), moved)),
                    a11y: Rc::from([]),
                    send: Rc::new(EventHandler::new(|_| {})),
                    #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
                    first_frame: Some(Arc::clone(&timing)),
                })
        },
        (48., 32.).into(),
        |runner| {
            runner.provide_root_context(|| {
                (State::create(Some(image_at(10, 0))), State::create(0_u32))
            })
        },
        1.,
    );
    let draw = |runner: &mut TestingRunner| {
        runner.sync_and_update();
        ::image::load_from_memory(runner.render().as_bytes())
            .unwrap()
            .to_rgba8()
    };
    #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
    assert!(!observed.drawn.load(Relaxed));
    let initial = draw(&mut runner);
    #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
    assert!(observed.drawn.load(Relaxed));
    assert_eq!(initial.get_pixel(13, 13).0, [100, 20, 0, 255]);
    assert_eq!(initial.get_pixel(23, 13).0, [0, 0, 0, 255]);
    motion.gestures().press(10_000, 15_000);
    motion.gestures().drag(9, 20_000, 15_000);
    moved.set(1);
    let preview = draw(&mut runner);
    assert_eq!(preview.get_pixel(13, 13).0, [0, 0, 0, 255]);
    assert_eq!(preview.get_pixel(23, 13).0, [100, 20, 0, 255]);
    frame.set(Some(image_at(20, 9)));
    let settled = draw(&mut runner);
    assert_eq!(
        settled, preview,
        "the replacement frame must not apply the already-rendered gesture again"
    );
    assert_eq!(motion.gestures().steps(), 0);

    motion.gestures().wheel(
        10,
        peersus_api::elements::Follow {
            wheel_doubling: 100,
            ..Default::default()
        },
        -100.,
        motion.about(16., 16.),
    );
    moved.set(2);
    let zoomed = draw(&mut runner);
    assert_eq!(zoomed.get_pixel(21, 13).0, [0, 0, 0, 255]);
    assert_eq!(zoomed.get_pixel(33, 13).0, [100, 20, 0, 255]);
}

#[test]
fn the_canvas_action_overlay_forwards_press_and_size_uses_the_extension_accent() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let (mut runner, ()) = TestingRunner::new(
        move || {
            let sink = sink.clone();
            let frame = use_state(|| Some(image_at(10, 0)));
            let metrics = Metrics::of(crate::theme::LIGHT);
            use_hook(|| {
                sink.borrow_mut().push(metrics.event(
                    "canvas".into(),
                    (2_000, 3_000, 48_000, 32_000),
                    0,
                ))
            });
            rect()
                .width(Size::px(48.))
                .height(Size::px(32.))
                .child(Bitmap {
                    frame,
                    preview: None,
                    a11y: Rc::from([CanvasA11y {
                        id: "zoom".into(),
                        role: CanvasRole::Button,
                        label: "Zoom".into(),
                        rect: CanvasRect {
                            x: 2_000,
                            y: 2_000,
                            width: 15_000,
                            height: 15_000,
                        },
                    }]),
                    send: Rc::new(EventHandler::new(move |event| {
                        sink.borrow_mut().push(event)
                    })),
                    #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
                    first_frame: None,
                })
        },
        (48., 32.).into(),
        |_| (),
        1.5,
    );
    let button = runner
        .find(|_, element| {
            let accessibility = element.accessibility();
            (accessibility.builder.role() == AccessibilityRole::Button)
                .then(|| accessibility.into_owned())
        })
        .unwrap();
    assert_eq!(button.builder.label(), Some("Zoom"));
    assert!(button.builder.supports_action(AccessibilityAction::Click));
    runner.click_cursor((8., 8.));
    runner.handle_events_immediately();
    runner.sync_and_update();
    let events = events.borrow();
    let Event::CanvasSize {
        scale,
        palette,
        width,
        ..
    } = events.first().unwrap()
    else {
        panic!("missing size event");
    };
    let (r, g, b) = crate::theme::LIGHT.receive;
    assert_eq!(
        (*scale, *width, palette.accent),
        (1_500, 48_000, [r, g, b, 255])
    );
    assert_eq!(events.get(1), Some(&Event::Press { id: "zoom".into() }));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires an isolated graphical display with a Ganesh renderer"]
fn native_gpu_reuses_one_canvas_texture_and_purges_it_on_unmount() {
    use freya::winit::{event_loop::EventLoop, platform::x11::EventLoopBuilderExtX11};
    use skia_safe::{
        gpu::{ganesh::BackendApi, surfaces::get_backend_texture},
        surface::BackendHandleAccess,
    };

    const WIDTH: u32 = 1024;
    const HEIGHT: u32 = 768;
    const RASTER_BYTES: usize = (WIDTH * HEIGHT * 4) as usize;
    const CACHE_BYTES: usize = 64 * 1024 * 1024;

    fn frame(color: u8) -> ImageHandle {
        raster(WIDTH, HEIGHT, vec![color; RASTER_BYTES]).unwrap()
    }

    async fn sample(window: &PlatformWindow) -> (u8, usize, usize) {
        window
            .post_render_callback(|surface| {
                let mut context = surface.direct_context().expect("window must use Ganesh");
                context.flush_submit_and_sync_cpu().unwrap();
                let info = ImageInfo::new((1, 1), ColorType::RGBA8888, AlphaType::Premul, None);
                let mut pixel = [0; 4];
                assert!(surface.canvas().read_pixels(&info, &mut pixel, 4, (16, 16)));
                assert_eq!(pixel, [pixel[0], pixel[0], pixel[0], 255]);
                (
                    pixel[0],
                    context.resource_cache_usage().resource_bytes,
                    context.resource_cache_limit(),
                )
            })
            .await
            .unwrap()
    }

    let vulkan = std::env::var("FREYA_RENDERER").as_deref() == Ok("vulkan");
    let renderer = if vulkan {
        RendererPreference::Vulkan {
            gpu_preference: GpuPreference::Integrated,
        }
    } else {
        RendererPreference::OpenGl
    };
    let report = Rc::new(RefCell::new(None));
    let outcome = report.clone();
    let mut events = EventLoop::builder();
    let events = events.with_any_thread(true).build().unwrap();
    launch(
        LaunchConfig::new()
            .with_event_loop(events)
            .with_gpu_resource_cache_limit(CACHE_BYTES)
            .with_window(
                WindowConfig::new(move || {
                    let mut visual = use_state(|| None::<ImageHandle>);
                    let mut legacy = use_state(|| true);
                    let outcome = outcome.clone();
                    use_hook(|| {
                        let platform = Platform::get();
                        let window = platform.current_window();
                        let window_id = Platform::window_id();
                        assert_eq!(
                            try_consume_root_context::<WindowGpuDevice>()
                                .and_then(|device| device.lease())
                                .is_some(),
                            vulkan,
                        );
                        spawn(async move {
                            let (_, cold, limit) = sample(&window).await;
                            assert_eq!(limit, CACHE_BYTES);
                            let mut legacy_peak = 0;
                            for color in 1..=30 {
                                visual.set(Some(frame(color)));
                                let (red, bytes, _) = sample(&window).await;
                                assert_eq!(red, color);
                                legacy_peak = legacy_peak.max(bytes);
                            }
                            visual.set(None);
                            let (red, legacy_closed, _) = sample(&window).await;
                            assert_eq!(red, 0);
                            let backend = window
                                .post_render_callback(|surface| {
                                    let mut context = surface.direct_context().unwrap();
                                    context.purge_unlocked_resources(
                                        PurgeResourceOptions::AllResources,
                                    );
                                    let mut upload = CanvasTexture::default();
                                    let mut texture = None;
                                    for color in 1..=30 {
                                        let source = frame(color);
                                        assert!(
                                            upload
                                                .image(surface.canvas(), &source.image)
                                                .unwrap()
                                                .is_texture_backed()
                                        );
                                        let target =
                                            &mut upload.texture.as_mut().unwrap().target.surface;
                                        let mut handle = get_backend_texture(
                                            target,
                                            BackendHandleAccess::FlushRead,
                                        )
                                        .unwrap();
                                        if let Some(previous) = &texture {
                                            assert!(handle.is_same_texture(previous));
                                        } else {
                                            texture = Some(handle);
                                        }
                                    }
                                    context.flush_submit_and_sync_cpu().unwrap();
                                    upload.release(surface);
                                    context.backend()
                                })
                                .await
                                .unwrap();

                            legacy.set(false);
                            let mut peak = 0;
                            for color in 1..=30 {
                                visual.set(Some(frame(color)));
                                let (red, bytes, _) = sample(&window).await;
                                assert_eq!(red, color);
                                peak = peak.max(bytes);
                            }
                            visual.set(None);
                            // The unmount queues GPU cleanup for the next render, not the
                            // runner pass that removed the picture.
                            sample(&window).await;
                            let (red, closed, _) = sample(&window).await;
                            assert_eq!(red, 0);
                            *outcome.borrow_mut() =
                                Some((backend, cold, legacy_peak, legacy_closed, peak, closed));
                            // Also close the whole window with a live GPU texture.
                            visual.set(Some(frame(63)));
                            assert_eq!(sample(&window).await.0, 63);
                            platform.close_window(window_id);
                        })
                    });
                    let current = visual.read().clone().map(|image_handle| {
                        if *legacy.read() {
                            image(image_handle)
                                .width(Size::fill())
                                .height(Size::fill())
                                .aspect_ratio(AspectRatio::None)
                                .sampling_mode(SamplingMode::Bilinear)
                                .into_element()
                        } else {
                            Picture(
                                image_handle,
                                #[cfg(any(debug_assertions, feature = "development-diagnostics"))]
                                None,
                            )
                            .into_element()
                        }
                    });
                    rect()
                        .width(Size::fill())
                        .height(Size::fill())
                        .background((0, 0, 0))
                        .maybe_child(current)
                })
                .with_size(320., 240.)
                .with_renderer(renderer),
            ),
    );
    let (backend, cold, legacy_peak, legacy_closed, peak, closed) =
        report.borrow_mut().take().unwrap();
    assert_eq!(
        backend,
        if vulkan {
            BackendApi::Vulkan
        } else {
            BackendApi::OpenGL
        }
    );
    eprintln!(
        "{backend:?}: budgeted bytes cold={cold} legacy peak={legacy_peak} closed={legacy_closed} new peak={peak} closed={closed}",
    );
    assert!(peak >= RASTER_BYTES, "the canvas really reached the GPU");
    assert!(
        peak < cold + 3 * RASTER_BYTES,
        "the cache must not grow with frames"
    );
    assert!(
        closed < RASTER_BYTES,
        "the canvas texture must not stay cached"
    );
}
