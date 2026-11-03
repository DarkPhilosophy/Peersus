//! What the store trusts, and the one number that stops it going backwards.
//!
//! THE KEY. [`MAINTAINER_PUBLIC_KEY`] is the Ed25519 PUBLIC key of whoever signs the catalog. A public
//! key is not a secret and is committed. While it is `None` the store is not configured and refuses
//! everything: there is no fallback that accepts an unsigned or self-signed catalog, because a
//! fallback is the one thing an attacker would ask for. Setting it is a deliberate source change,
//! reviewed like any other, made with the output of `cargo xtask catalog-keygen`.
//!
//! THE FLOOR. The highest catalog `sequence` ever accepted is kept in the data directory and only ever
//! raised, written atomically and only AFTER a catalog verified. Without it a replayed older (but
//! validly signed) catalog would be accepted again after every restart. A missing file is a first
//! run (floor 0); a damaged one is refused, never read as 0, because that is the rollback.
use ed25519_dalek::VerifyingKey;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The maintainer's public key, or `None` until one is chosen. 32 bytes.
pub const MAINTAINER_PUBLIC_KEY: Option<[u8; 32]> = None;

/// Why the store cannot be used, in words this application chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unavailable(pub &'static str);

/// The key to verify with, from a candidate (production passes [`MAINTAINER_PUBLIC_KEY`]).
pub fn trust_root(candidate: Option<[u8; 32]>) -> Result<VerifyingKey, Unavailable> {
    let bytes = candidate.ok_or(Unavailable(
        "The extension store is not set up in this build",
    ))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| Unavailable("The extension store key is not valid"))
}

/// The persisted floor.
pub struct Floor {
    path: PathBuf,
}

impl Floor {
    pub fn new(data: &Path) -> Self {
        Self {
            path: data.join("store-sequence"),
        }
    }

    /// The highest sequence accepted so far. Absent is 0; anything unreadable is an error.
    pub fn read(&self) -> Result<u64, Unavailable> {
        match fs::read_to_string(&self.path) {
            Ok(text) => {
                let text = text.trim_end_matches('\n');
                if text.is_empty() || text.len() > 20 || !text.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Unavailable("The saved store state is damaged"));
                }
                text.parse()
                    .map_err(|_| Unavailable("The saved store state is damaged"))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(_) => Err(Unavailable("The saved store state could not be read")),
        }
    }

    /// Raise the floor to `sequence`. Never lowers it. Atomic: a temporary file, then a rename.
    pub fn raise(&self, sequence: u64) -> Result<(), Unavailable> {
        if sequence <= self.read()? {
            return Ok(());
        }
        peersus_net::atomic::write(&self.path, format!("{sequence}\n").as_bytes())
            .map_err(|_| Unavailable("The store state could not be saved"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_key_the_store_refuses_everything() {
        assert_eq!(
            trust_root(None).err(),
            Some(Unavailable(
                "The extension store is not set up in this build"
            ))
        );
        assert!(
            MAINTAINER_PUBLIC_KEY.is_none() || trust_root(MAINTAINER_PUBLIC_KEY).is_ok(),
            "a committed key must be a valid point"
        );
    }

    #[test]
    fn a_key_that_does_not_decode_is_refused_and_a_real_one_is_accepted() {
        // About half of all 32-byte strings are not a curve point, so one is found by search rather
        // than by trusting a hand-picked pattern.
        let invalid = (0u8..=255)
            .map(|first| {
                let mut candidate = [0u8; 32];
                candidate[0] = first;
                candidate[1] = 1;
                candidate
            })
            .find(|candidate| VerifyingKey::from_bytes(candidate).is_err())
            .expect("some encoding in 256 tries is not a curve point");
        assert_eq!(
            trust_root(Some(invalid)).err(),
            Some(Unavailable("The extension store key is not valid"))
        );
        let good = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
            .verifying_key()
            .to_bytes();
        assert!(trust_root(Some(good)).is_ok());
    }

    #[test]
    fn the_floor_starts_at_zero_only_rises_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let floor = Floor::new(dir.path());
        assert_eq!(floor.read(), Ok(0));
        floor.raise(5).unwrap();
        floor.raise(3).unwrap();
        assert_eq!(floor.read(), Ok(5), "a lower sequence never lowers it");
        assert_eq!(
            Floor::new(dir.path()).read(),
            Ok(5),
            "a new instance (a restart) sees it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn raising_the_floor_never_uses_an_unrelated_temporary_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let unrelated = dir.path().join("unrelated");
        fs::write(&unrelated, b"leave this file alone").unwrap();
        let old_temporary = dir.path().join("store-sequence.tmp");
        std::os::unix::fs::symlink(&unrelated, &old_temporary).unwrap();

        let floor = Floor::new(dir.path());
        floor.raise(7).unwrap();

        assert_eq!(fs::read(&unrelated).unwrap(), b"leave this file alone");
        assert_eq!(floor.read(), Ok(7));
        assert!(old_temporary.is_symlink());
    }

    #[test]
    fn a_damaged_floor_is_an_error_and_not_zero() {
        let dir = tempfile::tempdir().unwrap();
        let floor = Floor::new(dir.path());
        for damaged in ["", "abc", "-1", "1 2", "99999999999999999999999", "0x10"] {
            fs::write(dir.path().join("store-sequence"), damaged).unwrap();
            assert!(floor.read().is_err(), "{damaged:?} was read as a number");
            assert!(
                floor.raise(10).is_err(),
                "raising must not overwrite a state it cannot read"
            );
        }
    }
}
