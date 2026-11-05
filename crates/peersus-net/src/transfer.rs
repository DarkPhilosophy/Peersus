//! File data plane. Receiving is called only after explicit local acceptance.
use crate::transport;
use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CHUNK: usize = 64 * 1024;
const STALL: Duration = Duration::from_secs(30);
pub const MAX_FILE_BYTES: u64 = 1 << 40;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub id: String,
    pub name: String,
    pub bytes: u64,
    /// The folders this file sits in, inside what was sent, outermost first, each ONE name (see [`Offer::folders`]). Left out of an
    /// offer for a file sent alone, so that such an offer is exactly what it was before folders existed and a device that does not
    /// know folders still takes it; only an offer that has folders carries the field, and such a device refuses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<Vec<String>>,
}
impl Offer {
    pub fn validate(&self) -> Result<(), Error> {
        if self.id.len() != 32 || !self.id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::InvalidOffer);
        }
        if self.bytes > MAX_FILE_BYTES {
            return Err(Error::InvalidOffer);
        }
        self.validate_name()?;
        self.validate_folders()
    }

    /// The rule for ONE name, a file's or a folder's: the same text as before folders existed, so what a file may be called did not
    /// change.
    fn validate_name(&self) -> Result<(), Error> {
        // A portable basename is stricter than Path::file_name on any single OS.
        if self.name.is_empty()
            || self.name.len() > 240
            || self.name == "."
            || self.name == ".."
            || self.name.ends_with(['.', ' '])
            || self.name.chars().any(|c| {
                c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            })
        {
            return Err(Error::InvalidOffer);
        }
        let stem = self
            .name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(Error::InvalidOffer);
        }
        Ok(())
    }
}

/// The deepest a folder may be inside what was sent. A sender that nests without end is a sender writing paths for the receiver.
pub const MAX_FOLDER_DEPTH: usize = 32;
/// The most characters in all the names of one path, so that what is built from them stays a path every system accepts.
pub const MAX_FOLDER_CHARS: usize = 1024;

impl Offer {
    /// Whether `name` is one name of a path: judged by exactly the rule a file's own name is, so that a folder is never named with
    /// anything a file could not be (no separator, no `.` or `..`, no device name, no trailing dot or space).
    pub(crate) fn is_one_name(name: &str) -> bool {
        Offer {
            id: "0".repeat(32),
            name: name.to_owned(),
            bytes: 0,
            folder: None,
        }
        .validate_name()
        .is_ok()
    }

    /// The folders this file sits in, inside what was sent, as a list of names and never as one string with separators: there is
    /// then nothing to split and no way to write a step up. Empty for a file that was sent alone.
    pub fn folders(&self) -> &[String] {
        self.folder.as_deref().unwrap_or(&[])
    }

    /// Whether the folders of this offer can be built under a folder the person chose: each is one name by the rule of a file's
    /// name, there are not too many, and they are not too long all together.
    fn validate_folders(&self) -> Result<(), Error> {
        let folders = self.folders();
        if folders.len() > MAX_FOLDER_DEPTH
            || folders
                .iter()
                .map(|name| name.chars().count())
                .sum::<usize>()
                > MAX_FOLDER_CHARS
            || !folders.iter().all(|name| Self::is_one_name(name))
        {
            return Err(Error::InvalidOffer);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid file offer")]
    InvalidOffer,
    #[error("file changed while being sent")]
    Changed,
    #[error("file transfer was interrupted or timed out")]
    Interrupted,
    #[error("file checksum did not match; nothing was published")]
    Integrity,
    #[error("destination already exists; it was not overwritten")]
    Exists,
    #[error("could not read or write the selected file")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Control(#[from] transport::Error),
}

/// Keep the opened source handle, so later path replacement cannot redirect it.
pub struct Source {
    file: tokio::fs::File,
    pub offer: Offer,
}
impl Source {
    pub async fn open(path: &Path) -> Result<Self, Error> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Error::InvalidOffer)?
            .to_owned();
        Self::from_file(std::fs::File::open(path)?, &name).await
    }

    /// A file that is already open, offered under `name`. The `File` is kept and read where it is: nothing is
    /// copied and nothing is opened a second time, so it does not matter that it has no path, as a file the system
    /// handed over as a descriptor has none (and a provider may hand over a pipe, which no path could open).
    /// `name` is judged by the same rule as any other offer, and a handle on anything but a file is refused.
    pub async fn from_file(file: std::fs::File, name: &str) -> Result<Self, Error> {
        Self::from_file_in(file, name, None).await
    }

    pub(crate) async fn from_file_in(
        file: std::fs::File,
        name: &str,
        folder: Option<Vec<String>>,
    ) -> Result<Self, Error> {
        let file = tokio::fs::File::from_std(file);
        let metadata = file.metadata().await?;
        if !metadata.is_file() {
            return Err(Error::InvalidOffer);
        }
        let offer = Offer {
            id: hex::encode(rand::random::<[u8; 16]>()),
            name: name.to_owned(),
            bytes: metadata.len(),
            // None for a file sent alone, so that its offer is what it always was.
            folder: folder.filter(|names| !names.is_empty()),
        };
        offer.validate()?;
        Ok(Self { file, offer })
    }

    /// Transfer exactly the offered bytes, followed by their SHA-256 digest.
    pub async fn send(
        mut self,
        stream: &mut SendStream,
        mut progress: impl FnMut(u64),
    ) -> Result<(), Error> {
        let mut remaining = self.offer.bytes;
        let mut buffer = vec![0; CHUNK];
        let mut digest = Sha256::new();
        while remaining > 0 {
            let wanted = remaining.min(CHUNK as u64) as usize;
            let count = self.file.read(&mut buffer[..wanted]).await?;
            if count == 0 {
                return Err(Error::Changed);
            }
            tokio::time::timeout(STALL, stream.write_all(&buffer[..count]))
                .await
                .map_err(|_| Error::Interrupted)?
                .map_err(|_| Error::Interrupted)?;
            digest.update(&buffer[..count]);
            remaining -= count as u64;
            progress(self.offer.bytes - remaining);
        }
        if self.file.read(&mut buffer[..1]).await? != 0 {
            return Err(Error::Changed);
        }
        let hash: [u8; 32] = digest.finalize().into();
        tokio::time::timeout(STALL, stream.write_all(&hash))
            .await
            .map_err(|_| Error::Interrupted)?
            .map_err(|_| Error::Interrupted)?;
        stream.finish().map_err(|_| Error::Interrupted)?;
        Ok(())
    }
}

/// A folder that is open, and that every step after it is made RELATIVE to.
///
/// WHY A HANDLE AND NOT A PATH. A path is looked at and then used, and in between anyone who can write in the folder the person chose
/// can put a link where a level was: a path checked one moment is another path the next. A handle on an open folder does not
/// change when its name does. So the chosen folder is opened once, and each level is then opened inside the one before, with the
/// link refused at that very open (`NOFOLLOW`), and what is made, written and published is made inside the last handle: there is
/// no path left to swap. The answer a person is given at the end is a path, for them to read; nothing is done with it.
struct Folder {
    #[cfg(unix)]
    handle: std::os::fd::OwnedFd,
    /// Windows has no `openat`: `cap-std` reaches every name relative to this handle and never
    /// out of it, and the levels are opened without following a link or junction.
    #[cfg(windows)]
    handle: cap_std::fs::Dir,
}

#[cfg(unix)]
impl Folder {
    /// Open `base`, the folder the person chose, and make and enter each of `names` inside it, one level at a time. A level that is
    /// there must be a real folder, and a link is refused where it is opened. A folder that is there is used, not refused: two
    /// folders sent into the same place meet in it, and what they hold stays protected, since a file is published without replacing
    /// one that exists. The names were judged by [`Offer::validate`] and are judged again here, because this is what touches the disk.
    fn open(base: &Path, names: &[String]) -> Result<Self, Error> {
        use rustix::fs::{Mode, OFlags, mkdirat, openat};
        let directory = OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::RDONLY;
        // The chosen folder itself is followed if it is a link: the person chose it, and it is the one thing not named by the sender.
        let chosen = std::fs::File::open(base)?;
        if !chosen.metadata()?.is_dir() {
            return Err(Error::InvalidOffer);
        }
        let mut handle: std::os::fd::OwnedFd = chosen.into();
        for name in names {
            if !Offer::is_one_name(name) {
                return Err(Error::InvalidOffer);
            }
            match mkdirat(&handle, name.as_str(), Mode::from_raw_mode(0o755)) {
                Ok(()) => {}
                Err(error) if error == rustix::io::Errno::EXIST => {}
                Err(error) => return Err(Error::Io(error.into())),
            }
            // Opened as a folder and without following: whatever is at this name now (a link, a file, another thing) is refused
            // here, on the way in, and the handle that comes back is a real folder.
            handle = openat(&handle, name.as_str(), directory, Mode::empty())
                .map_err(|_| Error::InvalidOffer)?;
        }
        Ok(Self { handle })
    }

    /// A new file for the bytes as they arrive, with a name of its own that nothing else has: made inside this folder and refused if
    /// there is anything by that name already, and never through a link.
    fn stage(&self) -> Result<Staged<'_>, Error> {
        use rustix::fs::{Mode, OFlags, openat};
        let name = format!(".peersus-{}.part", hex::encode(rand::random::<[u8; 8]>()));
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file = openat(
            &self.handle,
            name.as_str(),
            flags,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| Error::Io(error.into()))?;
        Ok(Staged {
            folder: self,
            name,
            file: Some(std::fs::File::from(file)),
            published: false,
        })
    }
}

#[cfg(windows)]
impl Folder {
    /// As on Unix: the chosen folder followed, each level made and entered inside the one before,
    /// a link or junction at a level refused where it is opened.
    fn open(base: &Path, names: &[String]) -> Result<Self, Error> {
        use cap_fs_ext::DirExt;
        use cap_std::{ambient_authority, fs::Dir};
        if !std::fs::metadata(base)?.is_dir() {
            return Err(Error::InvalidOffer);
        }
        let mut handle = Dir::open_ambient_dir(base, ambient_authority())?;
        for name in names {
            if !Offer::is_one_name(name) {
                return Err(Error::InvalidOffer);
            }
            match handle.create_dir(name) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(Error::Io(error)),
            }
            handle = handle
                .open_dir_nofollow(name)
                .map_err(|_| Error::InvalidOffer)?;
        }
        Ok(Self { handle })
    }

    /// A new file of its own name, refused if anything is there already, never through a link.
    fn stage(&self) -> Result<Staged<'_>, Error> {
        use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
        let name = format!(".peersus-{}.part", hex::encode(rand::random::<[u8; 8]>()));
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        let file = self
            .handle
            .open_with(&name, &options)
            .map_err(Error::Io)?
            .into_std();
        Ok(Staged {
            folder: self,
            name,
            file: Some(file),
            published: false,
        })
    }
}

/// A file being received, inside a folder. Unless it was published it is removed when this goes away, for whatever reason (an error,
/// a cancel, a task that is dropped), so that a transfer that does not finish leaves nothing behind.
struct Staged<'a> {
    folder: &'a Folder,
    name: String,
    file: Option<std::fs::File>,
    published: bool,
}

impl Staged<'_> {
    /// The file to write to, handed over once.
    fn take_file(&mut self) -> Result<std::fs::File, Error> {
        self.file.take().ok_or(Error::Interrupted)
    }

    /// Publish without replacing an existing file, relative to the directory already opened.
    /// Android app storage forbids hard links; Linux's atomic no-replace rename preserves the same guarantee.
    #[cfg(unix)]
    fn publish(&mut self, name: &str) -> Result<(), Error> {
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        use rustix::fs::{AtFlags, unlinkat};
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let published = rustix::fs::renameat_with(
            &self.folder.handle,
            self.name.as_str(),
            &self.folder.handle,
            name,
            rustix::fs::RenameFlags::NOREPLACE,
        );
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let published = rustix::fs::linkat(
            &self.folder.handle,
            self.name.as_str(),
            &self.folder.handle,
            name,
            AtFlags::empty(),
        );
        match published {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::EXIST => return Err(Error::Exists),
            Err(error) => return Err(Error::Io(error.into())),
        }
        self.published = true;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let _ = unlinkat(&self.folder.handle, self.name.as_str(), AtFlags::empty());
        Ok(())
    }

    /// Windows: a hard link inside the open folder, which is refused when the name is taken, then
    /// the staging name removed. (A volume without hard links, such as FAT, refuses to publish.)
    #[cfg(windows)]
    fn publish(&mut self, name: &str) -> Result<(), Error> {
        let folder = &self.folder.handle;
        match folder.hard_link(&self.name, folder, name) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Error::Exists);
            }
            Err(error) => return Err(Error::Io(error)),
        }
        self.published = true;
        let _ = folder.remove_file(&self.name);
        Ok(())
    }
}

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        // Published: the staging name is gone already. Not published: the file is removed, and a name that is not there is no error.
        if !self.published {
            #[cfg(unix)]
            let _ = rustix::fs::unlinkat(
                &self.folder.handle,
                self.name.as_str(),
                rustix::fs::AtFlags::empty(),
            );
            #[cfg(windows)]
            {
                // Windows cannot remove a file that is still open: close it first.
                drop(self.file.take());
                let _ = self.folder.handle.remove_file(&self.name);
            }
        }
    }
}

/// Stage in the destination filesystem, validate digest and FIN, publish without
/// replacing existing files. Dropping/cancelling removes the owned staging file.
pub async fn receive(
    stream: &mut RecvStream,
    offer: &Offer,
    directory: &Path,
    mut progress: impl FnMut(u64),
) -> Result<PathBuf, Error> {
    offer.validate()?;
    // The folder the file goes in, as an open handle: the chosen one with the offer's folders made inside it (none for a file sent
    // alone). It touches the disk, so it is done where blocking is allowed.
    let (base, names) = (directory.to_owned(), offer.folders().to_vec());
    let folder = tokio::task::spawn_blocking(move || Folder::open(&base, &names))
        .await
        .map_err(|_| Error::Interrupted)??;
    let mut staged = folder.stage()?;
    let mut file = tokio::fs::File::from_std(staged.take_file()?);
    let mut remaining = offer.bytes;
    let mut buffer = vec![0; CHUNK];
    let mut digest = Sha256::new();
    while remaining > 0 {
        let wanted = remaining.min(CHUNK as u64) as usize;
        tokio::time::timeout(STALL, stream.read_exact(&mut buffer[..wanted]))
            .await
            .map_err(|_| Error::Interrupted)?
            .map_err(|_| Error::Interrupted)?;
        digest.update(&buffer[..wanted]);
        file.write_all(&buffer[..wanted]).await?;
        remaining -= wanted as u64;
        progress(offer.bytes - remaining);
    }
    let mut expected = [0; 32];
    tokio::time::timeout(STALL, stream.read_exact(&mut expected))
        .await
        .map_err(|_| Error::Interrupted)?
        .map_err(|_| Error::Interrupted)?;
    let actual: [u8; 32] = digest.finalize().into();
    if actual != expected {
        return Err(Error::Integrity);
    }
    // Reject extra data: the offered size is a hard framing contract.
    let extra = tokio::time::timeout(STALL, stream.read(&mut buffer[..1]))
        .await
        .map_err(|_| Error::Interrupted)?
        .map_err(|_| Error::Interrupted)?;
    if extra.is_some() {
        return Err(Error::InvalidOffer);
    }
    file.sync_all().await?;
    drop(file);
    staged.publish(&offer.name)?;
    // What a person reads: the chosen folder, the folders of the offer, and the name. Nothing is done with this path.
    let mut destination = directory.to_owned();
    destination.extend(offer.folders());
    destination.push(&offer.name);
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{self, ALPN};
    use iroh::SecretKey;
    use peersus_core::NetworkSettings;

    #[test]
    fn remote_names_cannot_escape_or_target_platform_devices() {
        for name in [
            "../a", "a/b", "a\\b", "C:foo", "CON", "nul.txt", "COM1.log", "a.", "a ",
        ] {
            assert!(
                Offer {
                    id: "a".repeat(32),
                    name: name.into(),
                    bytes: 0,
                    folder: None,
                }
                .validate()
                .is_err()
            );
        }
    }

    fn offer_in(folder: Option<Vec<&str>>) -> Offer {
        Offer {
            id: "a".repeat(32),
            name: "photo.jpg".into(),
            bytes: 1,
            folder: folder.map(|names| names.into_iter().map(str::to_owned).collect()),
        }
    }

    #[test]
    fn a_folder_of_an_offer_is_judged_name_by_name_and_none_can_step_out_or_name_a_device() {
        assert!(offer_in(Some(vec!["pictures", "2024"])).validate().is_ok());
        // Every trick that a file's name is refused for, in any place of the path: a step up, a separator inside one name (so that
        // one name could be a whole path), a drive, a device, a trailing dot or space, nothing at all, a control character.
        for bad in [
            "..", ".", "a/b", "a\\b", "C:", "CON", "nul", "COM1", "a.", "a ", "", "x\0y", "a*b",
            "a?b",
        ] {
            for place in 0..3 {
                let mut names = vec!["one", "two", "three"];
                names[place] = bad;
                assert!(
                    offer_in(Some(names)).validate().is_err(),
                    "{bad:?} was taken as a folder name at place {place}"
                );
            }
        }
    }

    #[test]
    fn a_path_cannot_be_made_of_endless_or_endlessly_long_folders() {
        let names = |count: usize| vec!["d".to_owned(); count];
        let with = |names: Vec<String>| Offer {
            folder: Some(names),
            ..offer_in(None)
        };
        assert!(with(names(MAX_FOLDER_DEPTH)).validate().is_ok());
        assert!(
            with(names(MAX_FOLDER_DEPTH + 1)).validate().is_err(),
            "a path deeper than the limit was taken"
        );
        // Few names, but together past the length that is allowed: each is a good name alone.
        let long = vec!["n".repeat(200); MAX_FOLDER_CHARS / 200 + 1];
        assert!(
            with(long).validate().is_err(),
            "a path longer than the limit was taken"
        );
    }

    #[test]
    fn a_file_sent_alone_is_offered_exactly_as_before_folders_and_an_offer_from_before_still_reads()
    {
        // On the wire a file with no folder has NO field for it, so a device that does not know folders takes it as it always did.
        let alone = serde_json::to_string(&offer_in(None)).unwrap();
        assert!(
            !alone.contains("folder"),
            "a file sent alone carries a field an older device would refuse: {alone}"
        );
        assert_eq!(
            alone,
            format!(
                r#"{{"id":"{}","name":"photo.jpg","bytes":1}}"#,
                "a".repeat(32)
            )
        );
        // An offer written before folders existed reads as a file with none.
        let before: Offer = serde_json::from_str(&alone).unwrap();
        assert!(before.folders().is_empty());
        assert!(before.validate().is_ok());
        // A file in folders does carry them, as a list and never as one string with separators.
        let inside = serde_json::to_string(&offer_in(Some(vec!["a", "b"]))).unwrap();
        assert!(
            inside.contains(r#""folder":["a","b"]"#),
            "the folders are not a list on the wire: {inside}"
        );
    }

    #[test]
    fn an_offer_still_refuses_a_field_it_does_not_know() {
        // A new field must not open a way for anything else to ride along: the offer is still strict about what it holds.
        let wrong = format!(
            r#"{{"id":"{}","name":"a.txt","bytes":1,"path":"../x"}}"#,
            "a".repeat(32)
        );
        assert!(
            serde_json::from_str::<Offer>(&wrong).is_err(),
            "an unknown field was accepted in an offer"
        );
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    /// Write `bytes` as a file called `name` inside `folder`, the way `receive` does, and publish it.
    fn put(folder: &Folder, name: &str, bytes: &[u8]) -> Result<(), Error> {
        use std::io::Write;
        let mut staged = folder.stage()?;
        staged.take_file()?.write_all(bytes)?;
        staged.publish(name)
    }

    #[test]
    fn the_folders_of_an_offer_are_made_inside_the_chosen_one_and_a_real_folder_that_is_there_is_used()
     {
        let base = tempfile::tempdir().unwrap();
        put(
            &Folder::open(base.path(), &names(&["pictures", "2024"])).unwrap(),
            "a.jpg",
            b"a",
        )
        .unwrap();
        assert_eq!(
            std::fs::read(base.path().join("pictures").join("2024").join("a.jpg")).unwrap(),
            b"a"
        );
        // A second file in the same folders finds them and uses them.
        put(
            &Folder::open(base.path(), &names(&["pictures", "2024"])).unwrap(),
            "b.jpg",
            b"b",
        )
        .unwrap();
        assert_eq!(
            std::fs::read_dir(base.path().join("pictures").join("2024"))
                .unwrap()
                .count(),
            2
        );
        // A file sent alone has no folders: it goes in the chosen one itself.
        put(&Folder::open(base.path(), &[]).unwrap(), "alone.txt", b"x").unwrap();
        assert!(base.path().join("alone.txt").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_where_a_folder_should_be_is_refused_and_nothing_is_written_through_it() {
        let base = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(base.path().join("pictures"))
            .arg(elsewhere.path())
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        assert!(
            Folder::open(base.path(), &names(&["pictures", "2024"])).is_err(),
            "a path went through a junction"
        );
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_where_a_folder_should_be_is_refused_and_nothing_is_written_through_it() {
        let base = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        // A link to a real folder somewhere the person never chose, placed in the folder they chose.
        std::os::unix::fs::symlink(elsewhere.path(), base.path().join("pictures")).unwrap();
        assert!(
            Folder::open(base.path(), &names(&["pictures", "2024"])).is_err(),
            "a path went through a link"
        );
        assert_eq!(
            std::fs::read_dir(elsewhere.path()).unwrap().count(),
            0,
            "something was made on the other side of the link"
        );
        // A link to a file, and a link that points nowhere: refused as well.
        let file = base.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        std::os::unix::fs::symlink(&file, base.path().join("to-file")).unwrap();
        std::os::unix::fs::symlink(base.path().join("not-there"), base.path().join("dangling"))
            .unwrap();
        for link in ["to-file", "dangling"] {
            assert!(
                Folder::open(base.path(), &names(&[link])).is_err(),
                "{link} was taken as a folder"
            );
        }
        // And a link in the MIDDLE of a path, under a folder that is real.
        std::fs::create_dir(base.path().join("real")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), base.path().join("real").join("inner"))
            .unwrap();
        assert!(Folder::open(base.path(), &names(&["real", "inner", "deeper"])).is_err());
        assert_eq!(
            std::fs::read_dir(elsewhere.path()).unwrap().count(),
            0,
            "something was made through a link in the middle"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_level_replaced_by_a_link_after_it_is_open_changes_where_nothing_is_written() {
        // The case a checked path cannot hold: the folder is looked at and opened, and THEN someone swaps it for a link to somewhere
        // else, before the file is made and published. The handle is on the folder that was opened, so the file lands in it, and
        // not on the other side of the link that stands where it was.
        let base = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let folder = Folder::open(base.path(), &names(&["pictures", "2024"])).unwrap();
        let moved = base.path().join("moved-away");
        std::fs::rename(base.path().join("pictures"), &moved).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), base.path().join("pictures")).unwrap();
        put(&folder, "a.jpg", b"for the real folder").unwrap();
        assert_eq!(
            std::fs::read_dir(elsewhere.path()).unwrap().count(),
            0,
            "a file was written through a link that was put there after the open"
        );
        assert_eq!(
            std::fs::read(moved.join("2024").join("a.jpg")).unwrap(),
            b"for the real folder",
            "the file did not land in the folder that was opened"
        );
    }

    #[test]
    fn a_file_where_a_folder_should_be_is_refused_and_a_name_that_is_not_one_name_is_never_followed()
     {
        let base = tempfile::tempdir().unwrap();
        std::fs::write(base.path().join("taken"), b"x").unwrap();
        assert!(
            Folder::open(base.path(), &names(&["taken"])).is_err(),
            "a file was taken for a folder"
        );
        // The disk is touched only with names that are one name: a step up or a separator is refused before any level is made.
        for bad in ["..", "a/b", "a\\b", "", "."] {
            assert!(
                Folder::open(base.path(), &names(&["fine", bad])).is_err(),
                "{bad:?} reached the disk"
            );
        }
        assert_eq!(
            std::fs::read_dir(base.path()).unwrap().count(),
            2,
            "something was made outside what was asked"
        );
    }

    #[test]
    fn a_file_is_published_without_replacing_one_and_a_file_that_is_not_finished_leaves_nothing() {
        let base = tempfile::tempdir().unwrap();
        let folder = Folder::open(base.path(), &[]).unwrap();
        put(&folder, "x.txt", b"first").unwrap();
        assert!(
            matches!(put(&folder, "x.txt", b"second"), Err(Error::Exists)),
            "a file was replaced"
        );
        assert_eq!(std::fs::read(base.path().join("x.txt")).unwrap(), b"first");
        // A file begun and dropped (a cancel, an error, a task that is dropped) is removed with nothing else left in the folder.
        {
            let mut staged = folder.stage().unwrap();
            std::io::Write::write_all(&mut staged.take_file().unwrap(), b"half").unwrap();
        }
        assert_eq!(
            std::fs::read_dir(base.path()).unwrap().count(),
            1,
            "a file that was not finished was left behind"
        );
    }

    #[tokio::test]
    async fn a_file_of_a_folder_is_offered_with_the_folders_it_sits_in_and_a_file_alone_with_none()
    {
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("a.jpg");
        std::fs::write(&path, b"abc").unwrap();
        let inside = Source::from_file_in(
            std::fs::File::open(&path).unwrap(),
            "a.jpg",
            Some(names(&["pictures", "2024"])),
        )
        .await
        .unwrap();
        assert_eq!(inside.offer.folders(), ["pictures", "2024"]);
        assert_eq!(inside.offer.bytes, 3);
        // A file chosen on its own has no folders, and no field for them on the wire.
        let alone = Source::open(&path).await.unwrap();
        assert!(alone.offer.folders().is_empty());
        assert!(
            !serde_json::to_string(&alone.offer)
                .unwrap()
                .contains("folder")
        );
        // An empty list is the same as none: nothing is written for it.
        let none = Source::from_file_in(
            std::fs::File::open(&path).unwrap(),
            "a.jpg",
            Some(Vec::new()),
        )
        .await
        .unwrap();
        assert!(
            !serde_json::to_string(&none.offer)
                .unwrap()
                .contains("folder")
        );
        // And a folder name that cannot be one is refused before anything is sent.
        assert!(
            Source::from_file_in(
                std::fs::File::open(&path).unwrap(),
                "a.jpg",
                Some(names(&["fine", ".."]))
            )
            .await
            .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_put_where_a_file_was_is_refused_at_the_open_and_what_it_points_at_is_not_sent()
    {
        let base = tempfile::tempdir().unwrap();
        let secret = tempfile::tempdir().unwrap();
        std::fs::write(secret.path().join("private.key"), b"do not send").unwrap();
        let path = base.path().join("photo.jpg");
        // The walk saw a plain file here; before it is opened someone replaces it with a link to something private.
        std::fs::write(&path, b"photo").unwrap();
        let found = crate::walk::Walk::new(base.path()).unwrap().next().unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(secret.path().join("private.key"), &path).unwrap();
        assert!(found.open().is_err(), "a link was followed at the open");
        // A file chosen on its own that IS a link is still followed, because the person chose it: that is the other rule.
        assert!(Source::open(&path).await.is_ok());
        // And a folder in the place of a file is refused by its handle.
        let folder = base.path().join("was-a-file");
        std::fs::create_dir(&folder).unwrap();
        assert!(
            Source::from_file_in(
                std::fs::File::open(&folder).unwrap(),
                "was-a-file",
                Some(names(&["pictures"]))
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn encrypted_file_delivery_checks_content_and_refuses_overwrite() {
        let source_dir = tempfile::tempdir().unwrap();
        let destination_dir = tempfile::tempdir().unwrap();
        let path = source_dir.path().join("sample.bin");
        let data: Vec<u8> = (0..180_000).map(|n| (n % 251) as u8).collect();
        tokio::fs::write(&path, &data).await.unwrap();
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        };
        let a = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let b = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let (a_conn, b_conn) = tokio::join!(a.connect(b.addr(), ALPN), async {
            b.accept().await.unwrap().await
        });
        let a_conn = a_conn.unwrap();
        let b_conn = b_conn.unwrap();
        for attempt in 0..2 {
            let source = Source::open(&path).await.unwrap();
            let offer = source.offer.clone();
            let (sent, received) = tokio::join!(
                async {
                    let mut stream = a_conn.open_uni().await.unwrap();
                    source.send(&mut stream, |_| {}).await
                },
                async {
                    let mut stream = b_conn.accept_uni().await.unwrap();
                    receive(&mut stream, &offer, destination_dir.path(), |_| {}).await
                }
            );
            sent.unwrap();
            if attempt == 0 {
                received.unwrap();
            } else {
                assert!(matches!(received, Err(Error::Exists)));
            }
            assert_eq!(
                tokio::fs::read(destination_dir.path().join("sample.bin"))
                    .await
                    .unwrap(),
                data
            );
        }
        a_conn.close(0_u32.into(), b"done");
        b_conn.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn a_file_with_no_path_is_sent_as_it_is_held_and_nothing_is_copied() {
        use std::io::{Seek, Write};
        let destination_dir = tempfile::tempdir().unwrap();
        // A file the system could have handed over: it exists only as an open file. It has no name in any directory,
        // so nothing can open it a second time, and the only way to send it is to read this very handle.
        let mut held = tempfile::tempfile().unwrap();
        let data: Vec<u8> = (0..180_000).map(|n| (n % 241) as u8).collect();
        held.write_all(&data).unwrap();
        held.rewind().unwrap();
        let source = Source::from_file(held, "holiday.bin").await.unwrap();
        assert_eq!(source.offer.bytes, data.len() as u64);
        assert_eq!(
            source.offer.name, "holiday.bin",
            "the name is the one given"
        );
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        };
        let a = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let b = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let (a_conn, b_conn) = tokio::join!(a.connect(b.addr(), ALPN), async {
            b.accept().await.unwrap().await
        });
        let (a_conn, b_conn) = (a_conn.unwrap(), b_conn.unwrap());
        let offer = source.offer.clone();
        let (sent, received) = tokio::join!(
            async {
                let mut stream = a_conn.open_uni().await.unwrap();
                source.send(&mut stream, |_| {}).await
            },
            async {
                let mut stream = b_conn.accept_uni().await.unwrap();
                receive(&mut stream, &offer, destination_dir.path(), |_| {}).await
            }
        );
        sent.unwrap();
        received.unwrap();
        assert_eq!(
            tokio::fs::read(destination_dir.path().join("holiday.bin"))
                .await
                .unwrap(),
            data
        );
        a_conn.close(0_u32.into(), b"done");
        b_conn.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn an_offer_from_an_open_file_is_refused_for_a_name_the_rule_refuses_and_for_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.bin");
        std::fs::write(&path, b"data").unwrap();
        for name in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "a\\b",
            "a:b",
            "a*b",
            "trailing.",
            "trailing ",
            &"n".repeat(241),
        ] {
            let file = std::fs::File::open(&path).unwrap();
            assert!(
                Source::from_file(file, name).await.is_err(),
                "{name:?} was accepted as a name"
            );
        }
        // A handle on a folder is not a file, whatever it is called. (Windows opens a folder as a
        // file only when asked to with a flag `File::open` does not pass.)
        #[cfg(unix)]
        {
            let folder = std::fs::File::open(dir.path()).unwrap();
            assert!(matches!(
                Source::from_file(folder, "folder").await,
                Err(Error::InvalidOffer)
            ));
        }
        // And an ordinary name is accepted.
        assert!(
            Source::from_file(std::fs::File::open(&path).unwrap(), "ok.bin")
                .await
                .is_ok()
        );
        // `open` is the case where the name is the path's own, and judges it the same way.
        assert_eq!(Source::open(&path).await.unwrap().offer.name, "x.bin");
    }

    #[tokio::test]
    async fn malformed_payloads_leave_no_published_or_staged_file() {
        let directory = tempfile::tempdir().unwrap();
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        };
        let a = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let b = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let (left, right) = tokio::join!(a.connect(b.addr(), ALPN), async {
            b.accept().await.unwrap().await
        });
        let left = left.unwrap();
        let right = right.unwrap();
        let offer = Offer {
            id: "a".repeat(32),
            name: "payload.bin".into(),
            bytes: 4,
            folder: None,
        };
        let digest: [u8; 32] = Sha256::digest(b"data").into();
        let valid = [b"data".as_slice(), digest.as_slice()].concat();
        let cases = [
            b"dat".to_vec(),
            [b"data".as_slice(), &[0; 32]].concat(),
            [valid.as_slice(), b"extra"].concat(),
        ];
        for (index, payload) in cases.into_iter().enumerate() {
            let ((), received) = tokio::join!(
                async {
                    let mut stream = left.open_uni().await.unwrap();
                    stream.write_all(&payload).await.unwrap();
                    stream.finish().unwrap();
                },
                async {
                    let mut stream = right.accept_uni().await.unwrap();
                    receive(&mut stream, &offer, directory.path(), |_| {}).await
                }
            );
            match index {
                0 => assert!(matches!(received, Err(Error::Interrupted))),
                1 => assert!(matches!(received, Err(Error::Integrity))),
                _ => assert!(matches!(received, Err(Error::InvalidOffer))),
            }
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
        left.close(0_u32.into(), b"done");
        right.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }
}
