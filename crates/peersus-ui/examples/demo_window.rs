//! The read-only demo window as a program of its own, for measuring.
//!
//! It opens exactly what `peersus-ui --demo …` opens (the same arguments, judged by the same
//! code), and exists so that a benchmark never has to build the product program with the
//! `demo` feature on: the program a person runs stays the one built without it.
//!
//! Run it:
//!     cargo run --release -p peersus-ui --example demo_window --features demo -- \
//!         --demo --demo-probe=10 --demo-replay=30 --demo-page=transfers
//! The report's meaning and limits are in `peersus_ui::probe`.

fn main() -> std::process::ExitCode {
    peersus_ui::probe::mark_start();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
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
