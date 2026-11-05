//! The access token of a personal relay kept in a file of its own, for a platform that has no system credential store to keep it in
//! (Android keeps it in the application's private directory, as it does the device's key: see [`crate::key_file`]).
//!
//! Unlike the key, a token is meant to be replaced and forgotten, so the rules are about what can NEVER happen:
//!
//! - A file that is there but cannot be used is never taken for "no token", and never replaced by a read. It is an error: sending the next
//!   request without the token would be refused by the relay and never say why, and the person decides what to do with the file.
//! - What is written is never seen half written. A private temporary file with a name of its own is written, flushed to disk, and moved over
//!   the destination in one step, so a reader finds the old token or the new one, and a crash in between leaves the old one.
//! - The file is for its owner alone from the first byte (mode 0600, made with `create_new`), and no refusal ever carries the token.
use crate::transport::RelayAccess;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{io::Write, path::Path};

/// The file's name in the directory it is kept in.
pub const FILE: &str = "relay.token";

const UNUSABLE: &str = "The saved relay token is not usable; it has not been changed";
const UNREADABLE: &str = "Cannot read the relay token; the saved one was not changed";
const NOT_SAVED: &str = "Cannot save the relay token";
const NOT_FORGOTTEN: &str = "Cannot forget the relay token";
const TOO_LONG: &str = "The relay token must be one line of at most 512 characters";

/// The token saved in `directory`, or `None` when none is.
pub fn load(directory: &Path) -> Result<Option<RelayAccess>, &'static str> {
    // Not through a link: a file of the application's folder is read as it is, or not at all.
    let path = directory.join(FILE);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(UNREADABLE),
        Ok(found) if !found.file_type().is_file() => return Err(UNUSABLE),
        Ok(_) => {}
    }
    let text = zeroize::Zeroizing::new(std::fs::read_to_string(&path).map_err(|_| UNREADABLE)?);
    RelayAccess::new(&text).map(Some).ok_or(UNUSABLE)
}

/// Keep this token, or forget the saved one when it is empty. What is on disk afterwards is read back before this says it worked.
pub fn save(directory: &Path, token: &str) -> Result<(), &'static str> {
    let token = token.trim();
    let path = directory.join(FILE);
    if token.is_empty() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(NOT_FORGOTTEN),
        };
    }
    // The same rules as the token that will be used, so that what is saved is what can be loaded.
    if RelayAccess::new(token).is_none() {
        return Err(TOO_LONG);
    }
    let temporary = directory.join(format!(
        "{FILE}.new.{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    // Windows has no mode bits: the folder is the user's own profile, whose ACL the file inherits.
    #[cfg(unix)]
    options.mode(0o600);
    let written = options
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(token.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| {
            #[cfg(unix)]
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
            Ok(())
        })
        // `rename` replaces the destination in one step, which is the point: this is a token that is changed, not an identity.
        .and_then(|()| std::fs::rename(&temporary, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return Err(NOT_SAVED);
    }
    match load(directory) {
        Ok(saved) if saved == RelayAccess::new(token) => Ok(()),
        _ => Err("The saved relay token changed while it was being saved"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn nothing_saved_is_none_and_a_saved_token_comes_back_from_a_file_only_its_owner_can_use() {
        let directory = tempfile::tempdir().unwrap();
        assert!(load(directory.path()).unwrap().is_none());
        save(directory.path(), "  s3cret-token  ").unwrap();
        assert_eq!(
            load(directory.path()).unwrap(),
            RelayAccess::new("s3cret-token")
        );
        #[cfg(unix)]
        assert_eq!(mode(&directory.path().join(FILE)), 0o600);
    }

    #[test]
    fn a_new_token_replaces_the_old_one_whole_and_leaves_no_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        save(directory.path(), "first").unwrap();
        save(directory.path(), "second-and-longer").unwrap();
        assert_eq!(
            load(directory.path()).unwrap(),
            RelayAccess::new("second-and-longer")
        );
        let names: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            names,
            vec![std::ffi::OsString::from(FILE)],
            "a temporary file was left behind"
        );
    }

    #[test]
    fn an_empty_token_forgets_the_saved_one_and_forgetting_twice_is_fine() {
        let directory = tempfile::tempdir().unwrap();
        save(directory.path(), "s3cret-token").unwrap();
        save(directory.path(), "   ").unwrap();
        assert!(load(directory.path()).unwrap().is_none());
        assert!(!directory.path().join(FILE).exists());
        save(directory.path(), "").unwrap();
    }

    #[test]
    fn a_token_that_could_never_be_loaded_is_not_saved_and_the_old_one_stays() {
        let directory = tempfile::tempdir().unwrap();
        save(directory.path(), "good-token").unwrap();
        for bad in ["two\nlines", &"x".repeat(513)] {
            assert!(save(directory.path(), bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            load(directory.path()).unwrap(),
            RelayAccess::new("good-token")
        );
    }

    #[test]
    fn a_file_that_cannot_be_used_is_reported_and_left_alone_never_taken_for_no_token() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(FILE);
        std::fs::write(&path, "bad\nvalue").unwrap();
        assert!(load(directory.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "bad\nvalue",
            "a read changed the file"
        );
        // A folder, or a link to somewhere else, is not a token either.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(load(directory.path()).is_err());
        std::fs::remove_dir(&path).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", &path).unwrap();
            assert!(load(directory.path()).is_err(), "a link was followed");
        }
    }

    #[test]
    fn no_refusal_carries_the_token() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(FILE), "bad\nsupersecret").unwrap();
        let said = load(directory.path()).unwrap_err();
        assert!(!said.contains("supersecret"), "{said}");
        let said = save(directory.path(), "two\nsupersecret").unwrap_err();
        assert!(!said.contains("supersecret"), "{said}");
    }
}
