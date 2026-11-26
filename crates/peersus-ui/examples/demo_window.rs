//! The read-only demo window as a program of its own, for measuring.
//!
//! It opens exactly what `peersus-ui --demo …` opens (the same arguments, judged by the same
//! code), and exists so that a benchmark never has to build the product program with the
//! `demo` feature on: the program a person runs stays the one built without it.
//!
//! Run it:
//!     cargo run --release -p peersus-ui --example demo_window --features demo -- \
//!         --demo --demo-probe=10 --demo-replay=30 --demo-page=transfers
//! The report's meaning and limits are in `peersus_ui::probe`. The heap is counted
//! (`benchmark_support/counted_heap.rs`), which the product program's is not.

#[path = "benchmark_support/counted_heap.rs"]
mod counted_heap;

#[cfg(target_os = "linux")]
#[path = "benchmark_support/map_zoom.rs"]
mod map_zoom;

#[cfg(target_os = "linux")]
#[path = "benchmark_support/canvas.rs"]
mod canvas;

fn main() -> std::process::ExitCode {
    peersus_ui::probe::mark_start();
    counted_heap::install();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(target_os = "linux")]
    if let Some(
        mode @ ("--canvas" | "--canvas-owner" | "--extension-bench" | "--extension-bench-owner"),
    ) = arguments.first().map(String::as_str)
    {
        return match canvas::run(mode, &arguments[1..]) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("guest window benchmark: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    #[cfg(target_os = "linux")]
    if let Some(mode @ ("--map-zoom" | "--map-zoom-owner")) = arguments.first().map(String::as_str)
    {
        return match map_zoom::run(mode, &arguments[1..]) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("map zoom: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    if !arguments
        .iter()
        .any(|argument| argument == "--demo" || argument.starts_with("--demo-"))
    {
        eprintln!("demo_window opens only the demo: pass --demo or a --demo-* argument");
        return std::process::ExitCode::FAILURE;
    }
    match peersus_ui::config_from(&arguments) {
        Ok(config) => freya::prelude::launch(config),
        Err(reason) => {
            eprintln!("{reason}");
            return std::process::ExitCode::FAILURE;
        }
    }
    std::process::ExitCode::SUCCESS
}
