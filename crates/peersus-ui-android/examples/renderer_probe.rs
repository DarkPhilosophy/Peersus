//! NativeActivity entry for a disposable Android benchmark package, not the Peersus APK.
#![cfg(target_os = "android")]

use std::{fs, io::ErrorKind, path::Path, time::Instant};

use freya::winit::{
    event_loop::EventLoop,
    platform::android::{EventLoopBuilderExtAndroid, activity::AndroidApp},
};
use freya_winit::config::RendererBackend;

fn record_report(directory: &Path, report: &str) -> std::io::Result<()> {
    let staged = directory.join("renderer-probe.json.tmp");
    fs::write(&staged, report)?;
    fs::rename(staged, directory.join("renderer-probe.json"))
}

// A private run-as file selects the workload and renderer without rebuilding
// the probe. Absence keeps the original compile-time selection for local runs.
fn settings(directory: &Path) -> Result<(RendererBackend, bool), &'static str> {
    use RendererBackend::{OpenGl, Vulkan};
    match fs::read_to_string(directory.join("renderer-probe-mode")) {
        Ok(contents) => match contents.trim() {
            "Vulkan animated" => Ok((Vulkan, true)),
            "Vulkan idle" => Ok((Vulkan, false)),
            "OpenGL animated" => Ok((OpenGl, true)),
            "OpenGL idle" => Ok((OpenGl, false)),
            _ => Err("Invalid renderer probe mode"),
        },
        Err(error) if error.kind() == ErrorKind::NotFound => Ok((
            if option_env!("PEERSUS_RENDERER_PROBE_GLES").is_some() {
                OpenGl
            } else {
                Vulkan
            },
            true,
        )),
        Err(_) => Err("Renderer probe mode could not be read"),
    }
}

// SAFETY: this example is a separate cdylib. Its NativeActivity loads only this entry library,
// never the product's shared library or the other renderer probe in the same process.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    let started = Instant::now();
    let _diagnostics = peersus_diagnostics::init("renderer-probe");
    let Some(data) = app.internal_data_path() else {
        tracing::warn!(target: "peersus::dev", "Renderer probe has no private files directory");
        return;
    };
    let (backend, animated) = match settings(&data) {
        Ok(settings) => settings,
        Err(reason) => {
            tracing::warn!(target: "peersus::dev", reason, "Renderer probe could not start");
            return;
        }
    };
    let events = EventLoop::builder().with_android_app(app).build();
    match events {
        Ok(events) => peersus_ui::run_android_renderer_probe(
            events,
            backend,
            animated,
            started,
            move |report| {
                // The disposable package has no real account data. Only the
                // fixed measurement schema is written, not scene content.
                if let Err(error) = record_report(&data, report) {
                    tracing::warn!(target: "peersus::dev", kind = ?error.kind(), "Renderer probe could not save its reading");
                }
            },
        ),
        Err(error) => {
            tracing::warn!(target: "peersus::dev", %error, "Renderer probe could not start")
        }
    }
}
