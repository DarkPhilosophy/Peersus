fn main() {
    peersus_plugin_runtime::desktop::run_main(|bytes, transport| {
        peersus_plugin_runtime::run(bytes, transport).is_ok()
    });
}
