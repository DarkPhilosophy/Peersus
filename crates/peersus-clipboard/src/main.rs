//! Isolated desktop clipboard plugin. arboard obtains local text before it can be bounded;
//! malicious local applications can therefore exhaust this plugin, never the owner process.
#[cfg(not(target_os = "android"))]
fn main() {
    use arboard::Clipboard as System;
    use peersus_clipboard::owner::{Owner, Store};
    use peersus_plugin_host::supervisor::Bootstrap;
    struct Desktop(System);
    impl Store for Desktop {
        fn get(&mut self) -> Option<String> {
            self.0.get_text().ok()
        }
        fn set(&mut self, text: &str) -> bool {
            self.0.set_text(text).is_ok()
        }
        fn listen(
            &mut self,
            signals: std::sync::mpsc::SyncSender<peersus_clipboard::watch::Signal>,
        ) -> bool {
            peersus_clipboard::watch::start(signals)
        }
    }
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
    let clipboard = Owner::start(|| System::new().ok().map(Desktop));
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
