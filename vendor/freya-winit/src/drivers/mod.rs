// An explicit backend (or the FREYA_RENDERER override of Auto) is tried first.
// The remaining backends follow RendererBackend::compiled() order without duplicates.
// Vulkan's adapter preference is independent of this order.
// Recovery keeps the window and tries OpenGL, then software, without retrying Vulkan.

#[cfg(all(
    any(target_os = "linux", target_os = "windows", target_os = "android"),
    feature = "gpu"
))]
mod gl;
#[cfg(all(target_os = "macos", feature = "gpu"))]
mod metal;
mod software;
mod surface;
#[cfg(all(
    any(target_os = "linux", target_os = "windows", target_os = "android"),
    feature = "gpu"
))]
mod vulkan;
#[cfg(all(target_os = "linux", feature = "gpu"))]
mod wayland_gpu;

use freya_engine::prelude::Surface as SkiaSurface;
use winit::{
    dpi::PhysicalSize,
    event_loop::ActiveEventLoop,
    window::{
        Window,
        WindowAttributes,
    },
};

use crate::config::{
    RendererBackend,
    RendererFailure,
    RendererPreference,
    RendererStatus,
};

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn wait_for_display(window: &Window, last_present: &mut Option<std::time::Instant>) {
    use std::time::{
        Duration,
        Instant,
    };

    #[cfg(target_os = "linux")]
    {
        use raw_window_handle::{
            HasDisplayHandle,
            RawDisplayHandle,
        };

        if !matches!(
            window.display_handle().map(|handle| handle.as_raw()),
            Ok(RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_))
        ) {
            return;
        }
    }

    let refresh_rate = window
        .current_monitor()
        .and_then(|monitor| monitor.refresh_rate_millihertz())
        .filter(|refresh_rate| *refresh_rate > 1_000)
        .unwrap_or(60_000);
    let frame_interval = Duration::from_secs_f64(1000.0 / refresh_rate as f64);

    if let Some(last_present) = last_present
        && let Some(remaining) = frame_interval.checked_sub(last_present.elapsed())
    {
        std::thread::sleep(remaining);
    }

    *last_present = Some(Instant::now());
}

/// Unrecoverable graphics error requiring a driver rebuild.
#[derive(Debug)]
// Used by the Vulkan driver.
#[cfg_attr(
    not(all(
        any(target_os = "linux", target_os = "windows", target_os = "android"),
        feature = "gpu"
    )),
    allow(dead_code)
)]
pub enum DriverError {
    DeviceLost,
    OutOfMemory,
}

/// Shared Vulkan context.
#[derive(Default)]
pub struct GraphicsContext {
    #[cfg(all(
        any(target_os = "linux", target_os = "windows", target_os = "android"),
        feature = "gpu"
    ))]
    vulkan: vulkan::SharedVulkan,
}

#[allow(clippy::large_enum_variant)]
pub enum GraphicsDriver {
    #[cfg(all(
        any(target_os = "linux", target_os = "windows", target_os = "android"),
        feature = "gpu"
    ))]
    OpenGl(gl::OpenGLDriver),
    #[cfg(all(target_os = "macos", feature = "gpu"))]
    Metal(metal::MetalDriver),
    #[cfg(all(
        any(target_os = "linux", target_os = "windows", target_os = "android"),
        feature = "gpu"
    ))]
    Vulkan(vulkan::VulkanDriver),
    Software(software::SoftwareDriver),
}

impl GraphicsDriver {
    #[allow(unused_variables)]
    pub fn new(
        event_loop: &ActiveEventLoop,
        window_attributes: WindowAttributes,
        gpu_resource_cache_limit: usize,
        preference: RendererPreference,
        graphics_context: &mut GraphicsContext,
    ) -> (Self, Window, RendererStatus) {
        let environment_override = preference.environment_override();
        let preferred = environment_override.or(preference.backend());
        let candidates = preferred.into_iter().chain(
            RendererBackend::compiled()
                .iter()
                .copied()
                .filter(|backend| Some(*backend) != preferred),
        );
        let mut startup_failures = Vec::new();
        for backend in candidates {
            let result = match backend {
                #[cfg(all(
                    any(target_os = "linux", target_os = "windows", target_os = "android"),
                    feature = "gpu"
                ))]
                RendererBackend::Vulkan => vulkan::VulkanDriver::new(
                    event_loop,
                    window_attributes.clone(),
                    gpu_resource_cache_limit,
                    preference.gpu_preference(),
                    &mut graphics_context.vulkan,
                )
                .map(|(driver, window)| (Self::Vulkan(driver), window)),
                #[cfg(all(
                    any(target_os = "linux", target_os = "windows", target_os = "android"),
                    feature = "gpu"
                ))]
                RendererBackend::OpenGl => gl::OpenGLDriver::new(
                    event_loop,
                    window_attributes.clone(),
                    gpu_resource_cache_limit,
                )
                .map(|(driver, window)| (Self::OpenGl(driver), window)),
                #[cfg(all(target_os = "macos", feature = "gpu"))]
                RendererBackend::Metal => metal::MetalDriver::new(
                    event_loop,
                    window_attributes.clone(),
                    gpu_resource_cache_limit,
                )
                .map(|(driver, window)| (Self::Metal(driver), window)),
                RendererBackend::Software => software::SoftwareDriver::new(
                    event_loop,
                    window_attributes.clone(),
                )
                .map(|(driver, window)| (Self::Software(driver), window)),
                #[allow(unreachable_patterns)]
                _ => {
                    startup_failures.push((backend, RendererFailure::NotCompiled));
                    continue;
                }
            };
            match result {
                Ok((driver, _))
                    if backend == RendererBackend::OpenGl
                        && preferred != Some(backend)
                        && driver.gpu_name().is_some_and(|name| {
                            name.contains("Microsoft Basic Render Driver")
                        }) =>
                {
                    tracing::warn!("Skipping Microsoft's basic OpenGL software adapter");
                    startup_failures.push((backend, RendererFailure::BasicOpenGl));
                }
                Ok((driver, window)) => {
                    let status = RendererStatus {
                        preference,
                        environment_override,
                        active: driver.backend(),
                        startup_failures,
                        recovered_from: None,
                    };
                    return (driver, window, status);
                }
                Err(error) => {
                    tracing::warn!(?backend, %error, "Graphics backend could not start");
                    startup_failures.push((backend, RendererFailure::Initialization));
                }
            }
        }
        panic!("No graphics backend could start: {startup_failures:?}");
    }

    /// Rebuild the driver on the existing window, skipping Vulkan.
    #[cfg_attr(
        any(target_os = "macos", not(feature = "gpu")),
        allow(unused_variables)
    )]
    pub fn recover_reusing_window(
        event_loop: &ActiveEventLoop,
        window: &Window,
        gpu_resource_cache_limit: usize,
        transparent: bool,
    ) -> Self {
        #[cfg(all(
            any(target_os = "linux", target_os = "windows", target_os = "android"),
            feature = "gpu"
        ))]
        match gl::OpenGLDriver::from_window(
            event_loop,
            window,
            gpu_resource_cache_limit,
            transparent,
        ) {
            Ok(driver) => return Self::OpenGl(driver),
            Err(error) => {
                tracing::warn!("OpenGL recovery failed, falling back to software: {error}");
            }
        }

        let driver = software::SoftwareDriver::from_window(window)
            .expect("Failed to initialize software renderer fallback");
        Self::Software(driver)
    }

    pub fn present(
        &mut self,
        size: PhysicalSize<u32>,
        window: &Window,
        render: impl FnOnce(&mut SkiaSurface),
    ) -> Result<bool, DriverError> {
        match self {
            #[cfg(all(
                any(target_os = "linux", target_os = "windows", target_os = "android"),
                feature = "gpu"
            ))]
            Self::OpenGl(gl) => Ok(gl.present(window, render)),
            #[cfg(all(target_os = "macos", feature = "gpu"))]
            Self::Metal(mtl) => Ok(mtl.present(size, window, render)),
            #[cfg(all(
                any(target_os = "linux", target_os = "windows", target_os = "android"),
                feature = "gpu"
            ))]
            Self::Vulkan(vk) => vk.present(size, window, render),
            Self::Software(sw) => Ok(sw.present(size, window, render)),
        }
    }

    pub(crate) fn gpu_device(&self) -> Option<std::rc::Weak<dyn std::any::Any>> {
        #[cfg(all(
            any(target_os = "linux", target_os = "windows", target_os = "android"),
            feature = "gpu"
        ))]
        if let Self::Vulkan(driver) = self {
            return Some(driver.gpu_device());
        }
        None
    }

    pub fn resource_cache_usage(&self) -> Option<(usize, usize)> {
        match self {
            #[cfg(all(
                any(target_os = "linux", target_os = "windows", target_os = "android"),
                feature = "gpu"
            ))]
            Self::OpenGl(driver) => Some(driver.resource_cache_usage()),
            #[cfg(all(target_os = "macos", feature = "gpu"))]
            Self::Metal(driver) => Some(driver.resource_cache_usage()),
            #[cfg(all(
                any(target_os = "linux", target_os = "windows", target_os = "android"),
                feature = "gpu"
            ))]
            Self::Vulkan(driver) => Some(driver.resource_cache_usage()),
            Self::Software(_) => None,
        }
    }

    /// The name of the active graphics driver.
    pub fn name(&self) -> &'static str {
        self.backend().name()
    }

    pub(crate) fn backend(&self) -> RendererBackend {
        match self {
            #[cfg(all(
                any(target_os = "linux", target_os = "windows", target_os = "android"),
                feature = "gpu"
            ))]
            Self::OpenGl(_) => RendererBackend::OpenGl,
            #[cfg(all(target_os = "macos", feature = "gpu"))]
            Self::Metal(_) => RendererBackend::Metal,
            #[cfg(all(
                any(target_os = "linux", target_os = "windows", target_os = "android"),
                feature = "gpu"
            ))]
            Self::Vulkan(_) => RendererBackend::Vulkan,
            Self::Software(_) => RendererBackend::Software,
        }
    }

    /// The name of the GPU picked by the driver, only known for OpenGL and Vulkan.
    pub fn gpu_name(&self) -> Option<&str> {
        match self {
            #[cfg(all(
                any(target_os = "linux", target_os = "windows", target_os = "android"),
                feature = "gpu"
            ))]
            Self::OpenGl(gl) => gl.gpu_name.as_deref(),
            #[cfg(all(
                any(target_os = "linux", target_os = "windows", target_os = "android"),
                feature = "gpu"
            ))]
            Self::Vulkan(vk) => Some(vk.gpu_name()),
            _ => None,
        }
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), DriverError> {
        match self {
            #[cfg(all(
                any(target_os = "linux", target_os = "windows", target_os = "android"),
                feature = "gpu"
            ))]
            Self::OpenGl(gl) => {
                gl.resize(size);
                Ok(())
            }
            #[cfg(all(target_os = "macos", feature = "gpu"))]
            Self::Metal(mtl) => {
                mtl.resize(size);
                Ok(())
            }
            #[cfg(all(
                any(target_os = "linux", target_os = "windows", target_os = "android"),
                feature = "gpu"
            ))]
            Self::Vulkan(vk) => vk.resize(size),
            Self::Software(sw) => {
                sw.resize(size);
                Ok(())
            }
        }
    }
}
