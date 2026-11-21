//! NativeActivity entry for a disposable Android benchmark package, not the Peersus APK.
#![cfg(target_os = "android")]

use std::{fs, path::Path};

use freya::{
    prelude::NativeEvent,
    winit::{
        event_loop::EventLoop,
        platform::android::{EventLoopBuilderExtAndroid, activity::AndroidApp},
    },
};
use freya_winit::config::RendererBackend;

fn record_report(directory: &Path, report: &str) -> std::io::Result<()> {
    let staged = directory.join("renderer-probe.json.tmp");
    fs::write(&staged, report)?;
    fs::rename(staged, directory.join("renderer-probe.json"))
}

// SAFETY: this example is a separate cdylib. Its NativeActivity loads only this entry library,
// never the product's shared library or the other renderer probe in the same process.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    let _diagnostics = peersus_diagnostics::init("renderer-probe");
    let data = app.internal_data_path();
    let backend = if option_env!("PEERSUS_RENDERER_PROBE_GLES").is_some() {
        RendererBackend::OpenGl
    } else {
        RendererBackend::Vulkan
    };
    let events = EventLoop::<NativeEvent>::with_user_event()
        .with_android_app(app)
        .build();
    match events {
        Ok(events) => match peersus_ui::run_android_renderer_probe(events, backend) {
            Ok(report) => {
                // Some vendor builds omit application Logcat messages. Keep the synthetic
                // reading in the disposable app's own files for a debug harness to retrieve.
                if let Some(path) = data
                    && let Err(error) = record_report(&path, &report)
                {
                    tracing::warn!(target: "peersus::dev", kind = ?error.kind(), "Renderer probe could not save its reading");
                }
                tracing::warn!(target: "peersus::dev", %report, "Renderer probe result");
            }
            Err(error) => {
                tracing::warn!(target: "peersus::dev", %error, "Renderer probe could not encode results")
            }
        },
        Err(error) => {
            tracing::warn!(target: "peersus::dev", %error, "Renderer probe could not start")
        }
    }
}
