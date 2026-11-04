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
}

mod bridge {
    pub fn present_extension_result(_caller: &str, link: &str) -> Result<(), &'static str> {
        if !link.starts_with("https://example.invalid/") {
            return Err("Invalid fixture result origin");
        }
        Ok(())
    }
}
