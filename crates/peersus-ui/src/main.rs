#![cfg_attr(windows, windows_subsystem = "windows")]
// The shipped window is not a console program: on Windows it would otherwise open a terminal
// beside it. A failure is still a nonzero exit code; the diagnostics line it writes only has
// nowhere to show itself there, which is right for a window.
fn main() -> std::process::ExitCode {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if peersus_ui::prepare_window_allocator().is_err() {
        eprintln!(
            "Could not restart the Peersus window to apply its allocator default; continuing"
        );
    }
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    // This private worker has no owner, profile, diagnostics log or network session.
    if let Some(result) = peersus_ui::run_renderer_worker(&arguments) {
        return result;
    }
    #[cfg(target_os = "linux")]
    let event_loop = match freya::winit::event_loop::EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(_) => {
            eprintln!("Cannot open the Peersus window: no usable graphical display");
            return std::process::ExitCode::FAILURE;
        }
    };
    let _diagnostics = match peersus_diagnostics::init("ui") {
        Ok(guard) => Some(guard),
        Err(reason) => {
            eprintln!("{reason}");
            None
        }
    };
    // Development builds: a text report of a panic goes to `<data folder>/diagnostics/`, and the window's own event
    // loop is watched from its root component. Inert in every other build.
    if peersus_diagnostics::DEVELOPMENT
        && let Some(directory) = peersus_app::instance::data_directory()
    {
        peersus_diagnostics::incident::install("peersus-ui", &directory);
    }
    match peersus_ui::config_from(&arguments) {
        Ok(config) => {
            #[cfg(target_os = "linux")]
            let config = config.with_event_loop(event_loop);
            freya::prelude::launch(config);
        }
        Err(reason) => {
            eprintln!("{reason}");
            return std::process::ExitCode::FAILURE;
        }
    }
    std::process::ExitCode::SUCCESS
}
