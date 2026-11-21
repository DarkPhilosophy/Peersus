//! Writing a file so that it is either entirely the old content or entirely the new, never
//! something in between.
//!
//! The stores of what the user decided (devices, grants) are read back on the next start and
//! trusted. A write that is cut short by a crash or a full disk must therefore never leave a
//! half-written file where the real one was. The bytes go to a temporary file in the SAME
//! directory (so the final step is a rename within one filesystem), are forced to disk, and only
//! then replace the real file; the directory entry is forced as well, so the replacement itself
//! survives a power cut.
use std::{
    io::{self, Write},
    path::Path,
};

/// Replace `path` with `bytes`, creating its directory if needed.
///
/// On any error the previous content of `path`, if there was any, is left exactly as it was.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    replace(path, bytes, false)
}

/// [`write`], for a file only its owner may read (an approval, a ledger, a token): on Unix the temporary file is made the owner's
/// alone before anything is written to it, so no other user can read the content at any moment. Windows has no mode bits:
/// the folder is the user's own profile, whose ACL the file inherits.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    replace(path, bytes, true)
}

fn replace(path: &Path, bytes: &[u8], private: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = private;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    if let Ok(directory) = std::fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_writes_the_bytes_and_replaces_what_was_there() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        write(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        write(&path, b"second, and longer").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second, and longer");
        // Shorter than before: nothing of the old content may remain at the end.
        write(&path, b"x").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"x");
    }

    #[test]
    fn it_leaves_nothing_but_the_file_behind() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        write(&path, b"data").unwrap();
        write(&path, b"more data").unwrap();
        let names: Vec<String> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["state.json"], "a temporary file was left behind");
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_is_the_owners_alone_whatever_was_there_before() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("approval.json");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn it_creates_the_directory_it_writes_into() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a").join("b").join("state.json");
        write(&path, b"data").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"data");
    }

    #[test]
    fn a_failed_write_leaves_the_previous_content_exactly_as_it_was() {
        // The target is a directory, so the final replacement must fail; whatever was at the
        // path (here, a directory holding a file) must be untouched by the attempt.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("inside"), b"keep me").unwrap();
        assert!(write(&path, b"data").is_err());
        assert_eq!(std::fs::read(path.join("inside")).unwrap(), b"keep me");
        // And the failed attempt did not leave a temporary file next to it either.
        let names: Vec<String> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(
            names,
            ["state.json"],
            "a failed write left a temporary file"
        );
    }
}
