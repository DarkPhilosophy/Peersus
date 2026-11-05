//! Isolated desktop clipboard plugin. arboard obtains local text before it can be bounded;
//! malicious local applications can therefore exhaust this plugin, never the owner process.
#[cfg(not(target_os = "android"))]
fn main() {
    use peersus_plugin::supervisor::Bootstrap;
    let (Ok(socket), Ok(token), Ok(data_socket)) = (
        std::env::var("PEERSUS_PLUGIN_SOCKET"),
        std::env::var("PEERSUS_PLUGIN_TOKEN"),
        std::env::var("PEERSUS_PLUGIN_DATA_SOCKET"),
    ) else {
        return;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        return;
    };
    let clipboard = peersus_clipboard_platform::desktop::open();
    runtime.block_on(peersus_clipboard::session::run(
        clipboard,
        Bootstrap {
            socket: socket.into(),
            data_socket: data_socket.into(),
            token,
        },
    ));
}

// Android's bundled plugin is launched by PluginService's JNI entry, never an executable.
#[cfg(target_os = "android")]
fn main() {}
