//! Android's store transport: a private Java worker writes untrusted bytes to a
//! fresh private file. The shared owner alone verifies catalogs and packages.
use peersus_app::{catalog::Refusal, store_runtime::StoreFetcher};
use std::{future::Future, io::Read, path::PathBuf, pin::Pin};

pub struct AndroidFetcher(pub PathBuf);

const FAILED: Refusal = Refusal("The extension store could not be reached");

/// The platform's worker carries one download at a time and ends when its caller is done: a second
/// one started meanwhile is refused at once, and one still running is ended with the first. The
/// store and the application's own updates share this transport, so their downloads take turns.
static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl StoreFetcher for AndroidFetcher {
    fn branch_backed(&self) -> bool {
        true
    }

    fn fetch(
        &self,
        name: String,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Refusal>> + Send>> {
        let scratch = self.0.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                if limit as u64 > peersus_app::catalog::MAX_ARCHIVE_BYTES {
                    return Err(FAILED);
                }
                // NamedTempFile creates 0600; the worker receives an already-open
                // descriptor, not this path or permission to choose a destination.
                let mut output = tempfile::NamedTempFile::new_in(scratch).map_err(|_| FAILED)?;
                {
                    // Nothing a holder does can leave the transport in a state the next one must not use.
                    let _turn = TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    crate::bridge::store_download(&name, limit, output.path())
                        .map_err(|_| FAILED)?;
                }
                let mut bytes = Vec::new();
                output
                    .as_file_mut()
                    .take(limit as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| FAILED)?;
                if bytes.len() > limit {
                    return Err(Refusal("What the store sent is larger than allowed"));
                }
                Ok(bytes)
            })
            .await
            .map_err(|_| FAILED)?
        })
    }
}

#[cfg(feature = "private-store")]
pub struct PrivatePlatform(pub PathBuf);

#[cfg(feature = "private-store")]
impl peersus_app::private_store::Platform for PrivatePlatform {
    fn load(&self) -> Result<Option<peersus_core::private_store::PrivateCatalog>, &'static str> {
        let bytes = zeroize::Zeroizing::new(
            crate::bridge::private_store_access(None)?
                .ok_or("Cannot read private catalog configuration")?,
        );
        if bytes.is_empty() {
            return Ok(None);
        }
        let source: peersus_core::private_store::PrivateCatalog = serde_json::from_slice(&bytes)
            .map_err(|_| "The saved private catalog configuration is not usable")?;
        source.validate()?;
        Ok(Some(source))
    }
    fn save(
        &self,
        source: &peersus_core::private_store::PrivateCatalog,
    ) -> Result<(), &'static str> {
        source.validate()?;
        let bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(source)
                .map_err(|_| "Cannot encode private catalog configuration")?,
        );
        crate::bridge::private_store_access(Some(&bytes))?;
        Ok(())
    }
    fn fetcher(
        &self,
        _source: &peersus_core::private_store::PrivateCatalog,
    ) -> Result<std::sync::Arc<dyn StoreFetcher>, &'static str> {
        Ok(std::sync::Arc::new(AndroidFetcher(self.0.clone())))
    }
    fn data(&self) -> &std::path::Path {
        &self.0
    }
}
