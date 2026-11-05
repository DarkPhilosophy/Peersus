//! Exercise the actual adapter on the host with synthetic picker/consent results.
//! No Android UI, credential store or external upload is accessed by these tests.
#[path = "../src/extension_services.rs"]
mod extension_services;

mod extension_files {
    use std::{fs::File, io::Write};
    pub async fn choose() -> Result<(File, String), &'static str> {
        let mut file = tempfile::tempfile().map_err(|_| "Fixture unavailable")?;
        file.write_all(b"selected fixture bytes")
            .map_err(|_| "Fixture unavailable")?;
        Ok((file, "fixture.txt".into()))
    }
}
mod extension_authorization {
    use zeroize::Zeroizing;
    pub async fn request(
        _caller: &str,
        origin: &str,
        filename: &str,
        bytes: u64,
    ) -> Result<Zeroizing<Vec<u8>>, &'static str> {
        if origin == "https://example.invalid/" && filename.is_empty() && bytes == 0 {
            return Ok(Zeroizing::new(b"configured".to_vec()));
        }
        if origin != "https://example.invalid/" || filename != "fixture.txt" || bytes != 22 {
            return Err("Consent did not bind the selected snapshot");
        }
        Ok(Zeroizing::new(b"synthetic-test-credential".to_vec()))
    }
    /// The credential store: nothing is ever kept by these tests, so a check finds no saved key.
    pub async fn access(
        _caller: peersus_core::plugins::PluginId,
        _origin: reqwest::Url,
        _operation: i32,
        _secret: Zeroizing<Vec<u8>>,
    ) -> Result<Zeroizing<Vec<u8>>, &'static str> {
        Ok(Zeroizing::new(Vec::new()))
    }
}

/// The platform clipboard stand-in: every write is accepted and nothing is read, so no real clipboard is touched.
mod extension_clipboard {
    pub const MAX_BYTES: usize = 64 * 1024;
    pub struct Answer {
        pub ok: bool,
    }
    pub async fn write(_text: &str) -> Result<Answer, &'static str> {
        Ok(Answer { ok: true })
    }
    pub fn changes() -> tokio::sync::watch::Receiver<u64> {
        tokio::sync::watch::channel(0).1
    }
    pub struct AndroidBoard;
    impl peersus_app::clipboard_handles::Board for AndroidBoard {
        async fn read(&self, _observe: bool) -> peersus_app::clipboard_handles::Reading {
            peersus_app::clipboard_handles::Reading {
                text: None,
                changed: false,
            }
        }
        async fn write(&self, _text: String) -> bool {
            true
        }
        async fn reset(&self) -> bool {
            true
        }
    }
}

mod bridge {
    pub fn present_extension_result(_caller: &str, link: &str) -> Result<(), &'static str> {
        if !link.starts_with("https://example.invalid/") {
            return Err("Invalid fixture result origin");
        }
        Ok(())
    }
    /// No phone here: a batch is taken as replayed, so only the checks before it are tried.
    pub fn inject_remote_input(_batch: &str) -> Result<(), String> {
        Ok(())
    }
    pub fn release_remote_input() {}
}
