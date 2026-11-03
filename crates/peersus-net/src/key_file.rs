//! A device's key kept in a file of its own: read if it is there, made once if it is not, and never replaced.
//!
//! For a platform that has no system credential store to keep it in (Android keeps it in the application's private directory).
//! One device must be one identity for good: a second key under the same name is a second device to everyone who met this one, and
//! they cannot tell the two apart. So the rules are about what can NEVER happen:
//!
//! - A file that is there but is not a key is never replaced. It is an error, and the person decides.
//! - Two starts at once end as ONE device. Each writes a private temporary file, then publishes without replacement.
//!   Linux and Android use `renameat2(RENAME_NOREPLACE)` (Android forbids hard links in app storage); other Unix
//!   platforms use a hard link. The start that finds the destination taken reads that identity and drops its own.
use iroh::SecretKey;
use std::{
    io,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

/// The file's name in the directory it is kept in.
pub const FILE: &str = "identity.key";

const INVALID: &str = "The stored identity is not valid; it has not been replaced";
const UNREADABLE: &str = "The stored identity could not be read; it has not been replaced";
const NOT_SAVED: &str = "The identity could not be saved";

fn read(path: &Path) -> Result<Option<SecretKey>, &'static str> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let bytes: [u8; 32] = bytes.try_into().map_err(|_| INVALID)?;
            Ok(Some(SecretKey::from_bytes(&bytes)))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(UNREADABLE),
    }
}

/// Publish atomically without replacing another process's already-published identity.
fn publish(temporary: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        rustix::fs::renameat_with(
            rustix::fs::CWD, temporary, rustix::fs::CWD, destination,
            rustix::fs::RenameFlags::NOREPLACE,
        ).map_err(Into::into)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        std::fs::hard_link(temporary, destination)
    }
}

/// The key of `directory`, made and kept the first time and the same for every start after it, including starts at the same moment.
pub fn load(directory: &Path) -> Result<SecretKey, &'static str> {
    let path = directory.join(FILE);
    if let Some(key) = read(&path)? {
        return Ok(key);
    }
    let key = SecretKey::generate();
    // A name of its own for each start (two starts must not write one file), made with `create_new` so nothing is reused.
    let temporary = directory.join(format!("{FILE}.new.{}", hex::encode(rand::random::<[u8; 8]>())));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut file| {
            io::Write::write_all(&mut file, &key.to_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600)));
    let published = written.and_then(|()| publish(&temporary, &path));
    let _ = std::fs::remove_file(&temporary);
    match published {
        Ok(()) => Ok(key),
        // Another start published first: its key is the device's, and this one's is dropped.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => read(&path)?.ok_or(NOT_SAVED),
        Err(_) => Err(NOT_SAVED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn the_key_is_made_once_and_every_later_start_has_it() {
        let directory = tempfile::tempdir().unwrap();
        let first = load(directory.path()).unwrap().public();
        let second = load(directory.path()).unwrap().public();
        assert_eq!(first, second, "a start after the first got another identity");
    }

    #[test]
    fn starts_at_the_same_moment_end_as_one_device() {
        // Every thread finds no file, makes a key of its own and tries to publish it; they are held at a barrier so that they do.
        let directory = tempfile::tempdir().unwrap();
        let threads = 16;
        let barrier = Arc::new(Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (path, barrier) = (directory.path().to_path_buf(), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    load(&path).unwrap().public()
                })
            })
            .collect();
        let keys: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
        assert!(keys.windows(2).all(|pair| pair[0] == pair[1]), "starts at once ended as more than one device");
        // And it is the key the file holds, so the next start is the same device too.
        assert_eq!(load(directory.path()).unwrap().public(), keys[0], "the file holds another key than the starts ran with");
    }

    #[test]
    fn a_file_that_is_not_a_key_is_never_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(FILE);
        std::fs::write(&path, [1u8, 2, 3]).unwrap();
        assert_eq!(load(directory.path()).unwrap_err(), INVALID);
        assert_eq!(std::fs::read(&path).unwrap(), [1u8, 2, 3], "a bad file was replaced with a new key");
    }

    #[test]
    fn the_file_is_private_and_no_temporary_is_left() {
        let directory = tempfile::tempdir().unwrap();
        load(directory.path()).unwrap();
        let mode = std::fs::metadata(directory.path().join(FILE)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the key file is readable by others");
        let names: Vec<_> = std::fs::read_dir(directory.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "a temporary file was left behind: {names:?}");
    }
}
