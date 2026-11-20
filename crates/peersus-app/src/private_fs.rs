//! The few file operations the plugin store's safety rests on, once per platform.
//!
//! On Unix the guarantees are the owner and mode bits, `O_NOFOLLOW` and `flock`. Windows has no
//! mode bits: everything here lives in this user's own data folder in the profile, whose inherited
//! access list gives this user (and the system and administrators, who own the machine anyway)
//! control and nobody else any, and that inheritance is what stands in for 0700/0600. What can
//! still redirect a path there is a reparse point (symbolic link or junction), so those are
//! treated as the links they are and never followed.
use std::{fs, io, path::Path};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

/// `FILE_ATTRIBUTE_REPARSE_POINT`: a symbolic link, junction or other redirection.
#[cfg(windows)]
const REPARSE_POINT: u32 = 0x400;
/// `FILE_FLAG_OPEN_REPARSE_POINT`: open a reparse point itself instead of what it points to.
#[cfg(windows)]
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Make `path` private to this user.
pub(crate) fn make_private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// A folder builder that makes private folders (where the system has modes).
pub(crate) fn private_folder() -> fs::DirBuilder {
    #[allow(unused_mut)]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder
}

/// Folders made with every missing parent, private to this user where the system has modes.
pub(crate) fn private_folders() -> fs::DirBuilder {
    let mut builder = private_folder();
    builder.recursive(true);
    builder
}

/// Apply `mode` to a file about to be created (where the system has modes).
pub(crate) fn with_mode(options: &mut fs::OpenOptions, mode: u32) -> &mut fs::OpenOptions {
    #[cfg(unix)]
    options.mode(mode);
    #[cfg(not(unix))]
    let _ = mode;
    options
}

/// A new file that replaces nothing, with `mode` where the system has modes.
pub(crate) fn new_file(mode: u32) -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    with_mode(&mut options, mode);
    options
}

/// Open for reading without following a final link. On Windows a reparse point is opened as
/// itself and refused.
pub(crate) fn read_no_follow(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    #[cfg(windows)]
    options.custom_flags(OPEN_REPARSE_POINT);
    let file = options.open(path)?;
    // On Unix the open already failed for a link; on Windows the reparse point itself opened.
    if is_link(&file.metadata()?) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a link"));
    }
    Ok(file)
}

/// Whether `metadata` (from `symlink_metadata`) is a link of any kind.
pub(crate) fn is_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    if metadata.file_attributes() & REPARSE_POINT != 0 {
        return true;
    }
    false
}

/// Whether the file may be run as a program. Windows decides that by what the file is, so a
/// regular file counts; the Unix bit is what is recorded and kept there.
pub(crate) fn is_runnable(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// Whether someone other than this user could change what is at this path's place. On Windows
/// the inherited access list rules that out (see the module), so only a redirection is.
pub(crate) fn others_can_change(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o022 != 0
    }
    #[cfg(not(unix))]
    {
        is_link(metadata)
    }
}

/// Hold an exclusive lock on `file` without waiting, for as long as it stays open.
pub(crate) fn try_lock(file: &fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        fs2::FileExt::try_lock_exclusive(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_is_never_opened_through() {
        let folder = tempfile::tempdir().unwrap();
        let target = folder.path().join("target");
        fs::write(&target, b"x").unwrap();
        let link = folder.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(&target, &link).is_err() {
            // Creating links needs a privilege an ordinary Windows account may lack.
            return;
        }
        assert!(read_no_follow(&target).is_ok());
        assert!(read_no_follow(&link).is_err(), "a link was followed");
        assert!(is_link(&fs::symlink_metadata(&link).unwrap()));
    }

    #[test]
    fn the_lock_is_exclusive() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("lock");
        let first = new_file(0o600).open(&path).unwrap();
        try_lock(&first).unwrap();
        let second = fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert!(try_lock(&second).is_err());
        drop(first);
        try_lock(&second).unwrap();
    }
}
