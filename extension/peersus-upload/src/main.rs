//! The uploader is a separate process, never loaded into the host UI.
fn main() {
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
    runtime.block_on(peersus_upload::desktop::run(
        peersus_plugin::supervisor::Bootstrap {
            socket: socket.into(),
            token,
            data_socket: data_socket.into(),
        },
    ));
}
