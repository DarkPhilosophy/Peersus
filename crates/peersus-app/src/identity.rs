//! Desktop identity storage. No plaintext fallback and no credential error payloads.
//! Call off the UI thread, once at startup. Mobile supplies its native keystore.
use fs2::FileExt;
use iroh::SecretKey;
use keyring::{Entry, Error};
use std::{fs::OpenOptions, path::Path};
use zeroize::Zeroizing;

/// Serialize first-use creation across processes sharing the application directory.
/// The lock file contains no key material. Only the dedicated credential is accessed.
pub fn load(directory: &Path) -> Result<SecretKey, &'static str> {
    std::fs::create_dir_all(directory).map_err(|_| "Cannot create application directory")?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(directory.join("identity.lock"))
        .map_err(|_| "Cannot open identity lock")?;
    lock.try_lock_exclusive()
        .map_err(|_| "Identity is being opened by another process")?;
    let entry = Entry::new("Peersus", "transport-identity")
        .map_err(|_| "System credential store is unavailable")?;
    load_entry(&entry)
}

fn decode(bytes: &[u8]) -> Result<SecretKey, &'static str> {
    let bytes: &[u8; 32] = bytes
        .try_into()
        .map_err(|_| "Stored identity is invalid; it has not been replaced")?;
    Ok(SecretKey::from_bytes(bytes))
}

fn load_entry(entry: &Entry) -> Result<SecretKey, &'static str> {
    match entry.get_secret() {
        Ok(bytes) => decode(&Zeroizing::new(bytes)),
        Err(Error::NoEntry) => {
            let key = SecretKey::generate();
            let bytes = Zeroizing::new(key.to_bytes());
            entry
                .set_secret(bytes.as_ref())
                .map_err(|_| "Cannot save identity in system credential store")?;
            let saved = Zeroizing::new(
                entry
                    .get_secret()
                    .map_err(|_| "Cannot verify saved identity; retry reading on next startup")?,
            );
            let loaded = decode(&saved)?;
            if loaded.public() != key.public() {
                return Err("Stored identity changed during creation; startup refused");
            }
            Ok(loaded)
        }
        Err(_) => Err("Cannot read system identity; existing credentials were not changed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry() -> Entry {
        Entry::new_with_credential(Box::new(keyring::mock::MockCredential::default()))
    }
    #[test]
    fn identity_survives_repeated_load_without_rotation() {
        let entry = entry();
        let first = load_entry(&entry).unwrap().public();
        let second = load_entry(&entry).unwrap().public();
        assert_eq!(first, second);
    }
    #[test]
    fn corrupt_identity_is_preserved_instead_of_silently_rotated() {
        let entry = entry();
        entry.set_secret(&[1, 2, 3]).unwrap();
        assert!(load_entry(&entry).is_err());
        assert_eq!(entry.get_secret().unwrap(), [1, 2, 3]);
    }
    #[test]
    #[ignore = "Requires explicit permission to access the dedicated system credential"]
    fn native_store_retains_identity() {
        let directory = tempfile::tempdir().unwrap();
        let first = load(directory.path()).unwrap().public();
        let second = load(directory.path()).unwrap().public();
        assert!(
            first == second,
            "Identity changed between native store reads"
        );
    }
}
