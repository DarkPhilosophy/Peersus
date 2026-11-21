#![cfg_attr(windows, windows_subsystem = "windows")]
// The shipped window is not a console program: on Windows it would otherwise open a terminal
// beside it. A failure is still a nonzero exit code; the diagnostics line it writes only has
// nowhere to show itself there, which is right for a window.
fn main() -> std::process::ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    // This private worker has no owner, profile, diagnostics log or network session.
    if let Some(result) = peersus_ui::run_renderer_worker(&arguments) {
        return result;
    }
    let _diagnostics = match peersus_diagnostics::init("ui") {
        Ok(guard) => Some(guard),
        Err(reason) => {
            eprintln!("{reason}");
            None
        }
    };
    match peersus_ui::config_from(&arguments) {
        Ok(config) => freya::prelude::launch(config),
        Err(reason) => {
            eprintln!("{reason}");
            return std::process::ExitCode::FAILURE;
        }
    }
    std::process::ExitCode::SUCCESS
}
