fn main() {
    peersus_plugin_runtime::desktop::run_main(env!("CARGO_BIN_NAME"), peersus_plugin_canvas::run);
}
