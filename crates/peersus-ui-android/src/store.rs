//! Android's store transport: a private Java worker writes untrusted bytes to a
//! fresh private file. The shared owner alone verifies catalogs and packages.
use peersus_app::{catalog::Refusal, store_runtime::StoreFetcher};
use std::{future::Future, io::Read, path::PathBuf, pin::Pin};

pub struct AndroidFetcher(pub PathBuf);

const FAILED: Refusal = Refusal("The extension store could not be reached");

impl StoreFetcher for AndroidFetcher {
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
                crate::bridge::store_download(&name, limit, output.path()).map_err(|_| FAILED)?;
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
