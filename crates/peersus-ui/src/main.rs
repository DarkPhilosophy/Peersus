fn main() {
    match peersus_ui::config() {
        Ok(config) => freya::prelude::launch(config),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}
