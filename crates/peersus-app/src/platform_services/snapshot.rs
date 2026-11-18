//! File authority starts with an already selected descriptor, never a guest path.
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Mutex,
};

pub const MAX_FILE_BYTES: u64 = 250 * 1024 * 1024;
pub const MAX_PART_BYTES: u64 = 6 * 1024 * 1024;

/// An unnamed private copy of exactly the bytes the user selected. No path is exposed.
/// Deliberately not Debug: file names are private user data.
pub struct Snapshot {
    file: Mutex<File>,
    name: String,
    bytes: u64,
    digest: [u8; 32],
}

impl Snapshot {
    /// Copy on a platform I/O worker, not the owner or Android UI thread.
    /// The already-open descriptor is consumed even when validation fails.
    pub fn from_selected(
        mut source: File,
        name: String,
        limit: u64,
        scratch: &Path,
    ) -> Result<Self, &'static str> {
        if limit == 0
            || limit > MAX_FILE_BYTES
            || name.is_empty()
            || name.len() > 255
            || name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\'))
        {
            return Err("Invalid selected file metadata");
        }
        let before = source
            .metadata()
            .map_err(|_| "The selected file is unavailable")?;
        if !before.is_file() || before.len() == 0 || before.len() > limit {
            return Err("Choose a regular file within the upload size limit");
        }
        // A provider can hand over a descriptor with a nonzero cursor. Selection means
        // the complete file, not whatever bytes happen to follow that cursor.
        source
            .seek(SeekFrom::Start(0))
            .map_err(|_| "The selected file cannot be read")?;
        // Android has no application-writable global /tmp. The trusted platform
        // supplies its private scratch directory; the guest cannot choose a path.
        let mut file = tempfile::tempfile_in(scratch)
            .map_err(|_| "A private snapshot could not be created")?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut bytes = 0u64;
        loop {
            let count = source
                .read(&mut buffer)
                .map_err(|_| "The selected file cannot be read")?;
            if count == 0 {
                break;
            }
            bytes = bytes
                .checked_add(count as u64)
                .ok_or("The selected file is too large")?;
            if bytes > limit {
                return Err("The selected file changed during selection");
            }
            digest.update(&buffer[..count]);
            file.write_all(&buffer[..count])
                .map_err(|_| "The private snapshot could not be written")?;
        }
        let after = source
            .metadata()
            .map_err(|_| "The selected file is unavailable")?;
        if bytes != before.len()
            || before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
        {
            return Err("The selected file changed during selection");
        }
        file.flush()
            .map_err(|_| "The private snapshot could not be written")?;
        Ok(Self {
            file: Mutex::new(file),
            name,
            bytes,
            digest: digest.finalize().into(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn sha256(&self) -> &[u8; 32] {
        &self.digest
    }

    /// One bounded part from the private copy. Concurrent readers cannot race cursors.
    pub fn read_part(&self, offset: u64, length: u64) -> Result<Vec<u8>, &'static str> {
        let end = offset.checked_add(length).ok_or("Invalid upload part")?;
        if length == 0 || length > MAX_PART_BYTES || end > self.bytes {
            return Err("Invalid upload part");
        }
        let mut file = self
            .file
            .lock()
            .map_err(|_| "The private snapshot is unavailable")?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| "The private snapshot cannot be read")?;
        let mut data = vec![0; length as usize];
        file.read_exact(&mut data)
            .map_err(|_| "The private snapshot cannot be read")?;
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(bytes: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file
    }

    fn capture(source: File, name: String, limit: u64) -> Result<Snapshot, &'static str> {
        let scratch = tempfile::tempdir().unwrap();
        Snapshot::from_selected(source, name, limit, scratch.path())
    }

    #[test]
    fn the_snapshot_is_the_complete_selected_file_not_the_descriptors_current_offset() {
        let snapshot = capture(selected(b"selected bytes"), "test.txt".into(), 100).unwrap();
        assert_eq!(snapshot.read_part(0, 14).unwrap(), b"selected bytes");
        assert_eq!(
            snapshot.sha256().as_slice(),
            Sha256::digest(b"selected bytes").as_slice()
        );
    }

    #[test]
    fn later_source_changes_cannot_change_the_consent_snapshot() {
        let source = selected(b"before");
        let mut writer = source.try_clone().unwrap();
        let snapshot = capture(source, "test.txt".into(), 100).unwrap();
        writer.seek(SeekFrom::Start(0)).unwrap();
        writer.write_all(b"after!").unwrap();
        assert_eq!(snapshot.read_part(0, 6).unwrap(), b"before");
    }

    #[test]
    fn parts_are_bounded_and_do_not_share_a_visible_read_cursor() {
        let snapshot = capture(selected(b"abcdef"), "test.txt".into(), 100).unwrap();
        assert_eq!(snapshot.read_part(3, 3).unwrap(), b"def");
        assert_eq!(snapshot.read_part(0, 3).unwrap(), b"abc");
        for (offset, length) in [(0, 0), (6, 1), (u64::MAX, 2), (0, MAX_PART_BYTES + 1)] {
            assert!(snapshot.read_part(offset, length).is_err());
        }
    }

    #[test]
    fn selection_rejects_empty_oversize_and_header_unsafe_names() {
        for name in ["", "../other", "bad\r\nname", "other\\name"] {
            assert!(capture(selected(b"x"), name.into(), 100).is_err());
        }
        assert!(capture(selected(b""), "test.txt".into(), 100).is_err());
        assert!(capture(selected(b"123"), "test.txt".into(), 2).is_err());
        assert!(capture(selected(b"123"), "test.txt".into(), MAX_FILE_BYTES + 1).is_err());
    }
}
