fn main() -> std::process::ExitCode {
    let _diagnostics = match peersus_diagnostics::init("ui") {
        Ok(guard) => Some(guard),
        Err(reason) => {
            eprintln!("{reason}");
            None
        }
    };
    match peersus_ui::config() {
        Ok(config) => freya::prelude::launch(config),
        Err(reason) => {
            eprintln!("{reason}");
            return std::process::ExitCode::FAILURE;
        }
    }
    std::process::ExitCode::SUCCESS
}
