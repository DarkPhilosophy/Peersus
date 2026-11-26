//! Private development distribution, isolated from production trust and rollback state.
use crate::{store_runtime::StoreFetcher, store_service::StoreService};
use peersus_core::private_store::PrivateCatalog;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use std::path::PathBuf;
use std::{path::Path, sync::Arc};

pub trait Platform: Send + Sync + 'static {
    fn load(&self) -> Result<Option<PrivateCatalog>, &'static str>;
    fn save(&self, source: &PrivateCatalog) -> Result<(), &'static str>;
    fn fetcher(&self, source: &PrivateCatalog) -> Result<Arc<dyn StoreFetcher>, &'static str>;
    fn data(&self) -> &Path;
}

pub struct Setup {
    pub service: StoreService,
    pub fetcher: Arc<dyn StoreFetcher>,
}

pub fn prepare(platform: &dyn Platform, source: &PrivateCatalog) -> Result<Setup, &'static str> {
    use sha2::{Digest, Sha256};
    source.validate()?;
    crate::store_trust::trust_root(Some(source.public_key))
        .map_err(|_| "The private catalog signing key is not valid")?;
    let fetcher = platform.fetcher(source)?;
    // Source/trust changes must not reuse another catalog's rollback floor.
    let mut digest = Sha256::new();
    digest.update(source.public_key);
    for part in [&source.repository, &source.directory, &source.revision] {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    let directory = platform
        .data()
        .join("private-store")
        .join(hex::encode(digest.finalize()));
    std::fs::create_dir_all(&directory).map_err(|_| "Cannot prepare private catalog state")?;
    #[cfg(unix)]
    std::fs::set_permissions(
        &directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .map_err(|_| "Cannot protect private catalog state")?;
    Ok(Setup {
        service: StoreService::new(Some(source.public_key), &directory),
        fetcher,
    })
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub struct Desktop {
    pub executable: PathBuf,
    pub scratch: PathBuf,
    pub directory: PathBuf,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl Desktop {
    fn entry() -> Result<keyring::Entry, &'static str> {
        keyring::Entry::new("Peersus", "private-development-catalog")
            .map_err(|_| "The system credential store is unavailable")
    }

    fn load_entry(entry: &keyring::Entry) -> Result<Option<PrivateCatalog>, &'static str> {
        match entry.get_password() {
            Ok(value) => {
                let value = zeroize::Zeroizing::new(value);
                let source: PrivateCatalog = serde_json::from_str(&value)
                    .map_err(|_| "The saved private catalog configuration is not usable")?;
                source.validate()?;
                Ok(Some(source))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err("Cannot read the private catalog configuration"),
        }
    }

    fn save_entry(entry: &keyring::Entry, source: &PrivateCatalog) -> Result<(), &'static str> {
        source.validate()?;
        let encoded = zeroize::Zeroizing::new(
            serde_json::to_string(source)
                .map_err(|_| "Cannot encode private catalog configuration")?,
        );
        entry
            .set_password(&encoded)
            .map_err(|_| "Cannot save private catalog configuration securely")?;
        let read_back = zeroize::Zeroizing::new(
            entry
                .get_password()
                .map_err(|_| "Cannot verify the saved private catalog configuration")?,
        );
        if read_back.as_str() != encoded.as_str() {
            return Err("The private catalog configuration changed while saving");
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl Platform for Desktop {
    fn load(&self) -> Result<Option<PrivateCatalog>, &'static str> {
        Self::load_entry(&Self::entry()?)
    }
    fn save(&self, source: &PrivateCatalog) -> Result<(), &'static str> {
        Self::save_entry(&Self::entry()?, source)
    }
    fn fetcher(&self, source: &PrivateCatalog) -> Result<Arc<dyn StoreFetcher>, &'static str> {
        Ok(Arc::new(
            crate::store_runtime::HelperFetcher::beside(&self.executable, &self.scratch)
                .map_err(|_| "Cannot locate the private catalog helper")?
                .with_private_source(crate::store_runtime::PrivateSource {
                    repository: source.repository.clone(),
                    directory: source.directory.clone(),
                    revision: source.revision.clone(),
                    token: source.token.clone(),
                }),
        ))
    }
    fn data(&self) -> &Path {
        &self.directory
    }
}

#[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
mod tests {
    use super::*;
    use peersus_core::model::SecretText;

    #[test]
    fn private_source_is_verified_after_secure_save_and_invalid_replacement_keeps_old_source() {
        let entry =
            keyring::Entry::new_with_credential(Box::new(keyring::mock::MockCredential::default()));
        assert!(Desktop::load_entry(&entry).unwrap().is_none());
        let mut source = PrivateCatalog {
            repository: "example/private".into(),
            directory: "store".into(),
            revision: "main".into(),
            token: SecretText::new("fake-test-only").unwrap(),
            public_key: [0; 32],
        };
        Desktop::save_entry(&entry, &source).unwrap();
        assert_eq!(
            Desktop::load_entry(&entry).unwrap().unwrap().token.expose(),
            "fake-test-only"
        );
        source.repository = "../other".into();
        assert!(Desktop::save_entry(&entry, &source).is_err());
        assert_eq!(
            Desktop::load_entry(&entry).unwrap().unwrap().repository,
            "example/private"
        );
    }
}
