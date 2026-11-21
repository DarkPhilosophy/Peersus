//! The pictures a device keeps: its own, and the ones other devices have shown it.
//!
//! WHY THEY ARE KEPT UNDER THEIR DIGESTS. A picture is announced by its digest, and a device that
//! already holds that digest has no reason to be sent the picture again. So what a device has
//! received is kept in a folder with one file for each picture, named by the digest it was received
//! under, and "do I have it?" is answered by looking at a file name, without reading a picture.
//!
//! WHAT A DIGEST IS NEVER TAKEN FROM. A picture is stored under the digest this module computes from
//! its pixels, never under one that came with it. A device that announced one picture and sent
//! another would otherwise get its second picture kept under the first one's name, and every later
//! device that was told the first would be shown the second.
//!
//! WHAT IT BOUNDS. At most [`MOST_RECEIVED`] received pictures are kept, the oldest given up first, so
//! that devices coming and going never fill the disk; each file is at most a picture's largest size
//! as text; and what is read back is checked as a picture again, because a file is something a
//! person or a crash can change.
//!
//! WHAT IT DOES NOT KNOW. Which device a picture belongs to. That is the host's: the same picture
//! held by two devices is kept once.

use peersus_core::profile::{Digest, MAX_BYTES, Picture};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

/// Most received pictures kept. A paired device is a person's own, a handful at a time; this is far
/// more than that and keeps the folder well under a hundred megabytes however many devices pass.
pub const MOST_RECEIVED: usize = 256;
/// Largest file read: the largest picture as hexadecimal text, plus the few fields around it.
const MAX_FILE: u64 = (MAX_BYTES as u64) * 2 + 256;

/// Where the pictures live: the own picture in one file, the received ones in a folder. Two paths and
/// nothing else, so it is cheap to copy into a task that works on its own.
#[derive(Clone)]
pub struct Pictures {
    own: PathBuf,
    received: PathBuf,
}

impl Pictures {
    /// The pictures kept under `directory`. Nothing is created until something is stored.
    pub fn new(directory: &Path) -> Self {
        Self {
            own: directory.join("profile-picture.json"),
            received: directory.join("device-pictures"),
        }
    }

    /// This device's own picture, or `None` when it has none. A file that is there but is not a valid
    /// picture is an error and not "none": a picture the person chose must not silently vanish.
    pub fn own(&self) -> io::Result<Option<Picture>> {
        read_picture(&self.own)
    }

    /// Make `picture` this device's own.
    pub fn set_own(&self, picture: &Picture) -> io::Result<()> {
        crate::atomic::write(
            &self.own,
            &serde_json::to_vec(picture).map_err(|_| invalid())?,
        )
    }

    /// Give up this device's own picture. Having none already is not an error.
    pub fn clear_own(&self) -> io::Result<()> {
        match fs::remove_file(&self.own) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    #[cfg(test)]
    /// The digest this device announces: its picture's, or the digest of having none.
    pub fn own_digest(&self) -> io::Result<Digest> {
        Ok(self
            .own()?
            .map_or_else(Picture::digest_of_none, |picture| picture.digest()))
    }

    /// Whether a picture with this digest is already kept: answered by a file name, reading nothing.
    pub fn has(&self, digest: &Digest) -> bool {
        self.received.join(file_name(digest)).is_file()
    }

    /// The received picture with this digest, if it is kept, checked again as a picture and checked to
    /// be the picture its file name says it is: a file put there by hand under a name that is not its
    /// own is refused, not shown.
    pub fn get(&self, digest: &Digest) -> io::Result<Option<Picture>> {
        let Some(picture) = read_picture(&self.received.join(file_name(digest)))? else {
            return Ok(None);
        };
        if picture.digest() != *digest {
            return Err(invalid());
        }
        Ok(Some(picture))
    }

    /// Keep a received picture, under the digest computed from it, and give that digest back. Keeping
    /// the same picture again changes nothing. When more than [`MOST_RECEIVED`] would be kept, the
    /// oldest are given up.
    pub fn keep(&self, picture: &Picture) -> io::Result<Digest> {
        let digest = picture.digest();
        let path = self.received.join(file_name(&digest));
        if !path.is_file() {
            crate::atomic::write(&path, &serde_json::to_vec(picture).map_err(|_| invalid())?)?;
        }
        self.trim(&digest)?;
        Ok(digest)
    }

    /// Give up the received picture with this digest. Not having it is not an error.
    pub fn forget(&self, digest: &Digest) -> io::Result<()> {
        match fs::remove_file(self.received.join(file_name(digest))) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// The received pictures kept, oldest first, by the time each file was last written.
    fn kept(&self) -> io::Result<Vec<(std::time::SystemTime, PathBuf)>> {
        let read = match fs::read_dir(&self.received) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut kept = Vec::new();
        for entry in read {
            let entry = entry?;
            let name = entry.file_name();
            // Only what this module made: a file named by a digest. Anything else is not touched.
            if name.to_str().is_some_and(is_picture_file) {
                kept.push((entry.metadata()?.modified()?, entry.path()));
            }
        }
        kept.sort();
        Ok(kept)
    }

    /// Give up the oldest received pictures above the limit, never the one just kept.
    fn trim(&self, keep: &Digest) -> io::Result<()> {
        let kept = self.kept()?;
        let extra = kept.len().saturating_sub(MOST_RECEIVED);
        let protected = self.received.join(file_name(keep));
        for (_, path) in kept
            .into_iter()
            .filter(|(_, path)| *path != protected)
            .take(extra)
        {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }
}

/// The file a received picture is kept in: its digest and nothing else, so a digest can never name
/// anything but a picture file.
fn file_name(digest: &Digest) -> String {
    format!("{digest}.json")
}

/// Whether `name` is exactly what [`file_name`] makes: 64 lowercase digits and `.json`.
fn is_picture_file(name: &str) -> bool {
    name.strip_suffix(".json")
        .is_some_and(|stem| stem.parse::<Digest>().is_ok())
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid profile picture")
}

/// A picture read from `path`, bounded and checked; `None` only when there is no such file.
fn read_picture(path: &Path) -> io::Result<Option<Picture>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(invalid());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(seed: u8) -> Picture {
        let rgba: Vec<u8> = (0..4 * 4 * 4)
            .map(|n| (n as u8).wrapping_add(seed))
            .collect();
        Picture::new(4, 4, rgba).unwrap()
    }

    fn store() -> (tempfile::TempDir, Pictures) {
        let directory = tempfile::tempdir().unwrap();
        let pictures = Pictures::new(directory.path());
        (directory, pictures)
    }

    #[test]
    fn a_device_has_no_picture_until_it_chooses_one_and_then_has_that_one() {
        let (_directory, pictures) = store();
        assert_eq!(pictures.own().unwrap(), None);
        assert_eq!(pictures.own_digest().unwrap(), Picture::digest_of_none());

        pictures.set_own(&picture(1)).unwrap();
        assert_eq!(pictures.own().unwrap(), Some(picture(1)));
        assert_eq!(pictures.own_digest().unwrap(), picture(1).digest());

        // Choosing another replaces it.
        pictures.set_own(&picture(2)).unwrap();
        assert_eq!(pictures.own().unwrap(), Some(picture(2)));
        assert_ne!(pictures.own_digest().unwrap(), picture(1).digest());

        // And giving it up goes back to having none; giving up none is not an error.
        pictures.clear_own().unwrap();
        assert_eq!(pictures.own().unwrap(), None);
        assert_eq!(pictures.own_digest().unwrap(), Picture::digest_of_none());
        pictures.clear_own().unwrap();
    }

    #[test]
    fn the_own_picture_survives_the_program_being_started_again() {
        let directory = tempfile::tempdir().unwrap();
        Pictures::new(directory.path())
            .set_own(&picture(7))
            .unwrap();
        assert_eq!(
            Pictures::new(directory.path()).own().unwrap(),
            Some(picture(7))
        );
    }

    #[test]
    fn an_own_picture_that_is_damaged_on_disk_is_an_error_and_not_quietly_none() {
        let directory = tempfile::tempdir().unwrap();
        let pictures = Pictures::new(directory.path());
        pictures.set_own(&picture(3)).unwrap();
        for damaged in [
            &b"not json"[..],
            br#"{"width":2,"height":2,"rgba":"00"}"#,
            b"",
            b"{}",
        ] {
            fs::write(directory.path().join("profile-picture.json"), damaged).unwrap();
            assert!(
                pictures.own().is_err(),
                "{:?} was read as no picture or as a picture",
                damaged
            );
        }
    }

    #[test]
    fn a_received_picture_is_kept_under_the_digest_made_from_it_and_found_by_that_digest() {
        let (directory, pictures) = store();
        let digest = picture(5).digest();
        assert!(!pictures.has(&digest));
        assert_eq!(pictures.get(&digest).unwrap(), None);

        assert_eq!(pictures.keep(&picture(5)).unwrap(), digest);
        assert!(pictures.has(&digest));
        assert_eq!(pictures.get(&digest).unwrap(), Some(picture(5)));
        // The file is named by the digest and by nothing else.
        let names: Vec<String> = fs::read_dir(directory.path().join("device-pictures"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, [format!("{digest}.json")]);
        // Keeping it again changes nothing.
        assert_eq!(pictures.keep(&picture(5)).unwrap(), digest);
        assert_eq!(
            fs::read_dir(directory.path().join("device-pictures"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn a_picture_cannot_be_kept_under_a_digest_that_is_not_its_own() {
        let (directory, pictures) = store();
        // Someone puts picture 9 in the file that is named for picture 8, by hand or by a fault.
        let name = picture(8).digest();
        let folder = directory.path().join("device-pictures");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(format!("{name}.json")),
            serde_json::to_vec(&picture(9)).unwrap(),
        )
        .unwrap();
        assert!(pictures.has(&name), "it is there by name");
        assert!(
            pictures.get(&name).is_err(),
            "a picture was shown under a digest that is not its own"
        );
    }

    #[test]
    fn what_is_kept_is_given_up_oldest_first_above_the_limit_and_never_the_one_just_kept() {
        let (directory, pictures) = store();
        let folder = directory.path().join("device-pictures");
        // Fill to the limit, with each file a little older than the next.
        let mut digests = Vec::new();
        for n in 0..MOST_RECEIVED {
            let p = Picture::new(1, 1, vec![(n % 256) as u8, (n / 256) as u8, 7, 255]).unwrap();
            digests.push(pictures.keep(&p).unwrap());
            let at = std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(1_000 + n as u64);
            fs::File::options()
                .write(true)
                .open(folder.join(file_name(&digests[n])))
                .unwrap()
                .set_modified(at)
                .unwrap();
        }
        assert_eq!(fs::read_dir(&folder).unwrap().count(), MOST_RECEIVED);

        // One more: the oldest goes, the rest stay, and the new one is there.
        let newest = Picture::new(1, 1, vec![200, 200, 200, 255]).unwrap();
        let kept = pictures.keep(&newest).unwrap();
        assert_eq!(fs::read_dir(&folder).unwrap().count(), MOST_RECEIVED);
        assert!(!pictures.has(&digests[0]), "the oldest was not given up");
        assert!(pictures.has(&digests[1]) && pictures.has(&digests[MOST_RECEIVED - 1]));
        assert!(pictures.has(&kept), "the picture just kept was given up");
    }

    #[test]
    fn keeping_a_picture_that_is_the_oldest_of_all_does_not_give_it_up_even_over_the_limit() {
        // The folder holds exactly the most there may be, none of them the picture. Keeping the picture
        // makes one too many, and the oldest is given up: which would be the picture itself, when its
        // file is older than all the rest. It is the picture just asked for, so it must be another.
        let (directory, pictures) = store();
        let folder = directory.path().join("device-pictures");
        let mut others = Vec::new();
        for n in 0..MOST_RECEIVED {
            let p = Picture::new(1, 1, vec![(n % 256) as u8, (n / 256) as u8, 2, 255]).unwrap();
            let digest = pictures.keep(&p).unwrap();
            let later = std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(5_000 + n as u64);
            fs::File::options()
                .write(true)
                .open(folder.join(file_name(&digest)))
                .unwrap()
                .set_modified(later)
                .unwrap();
            others.push(digest);
        }
        assert_eq!(fs::read_dir(&folder).unwrap().count(), MOST_RECEIVED);

        // The picture's file is written, and made older than every other, as one left from long ago is.
        let wanted = Picture::new(1, 1, vec![9, 9, 9, 255]).unwrap();
        let wanted_digest = wanted.digest();
        fs::write(
            folder.join(file_name(&wanted_digest)),
            serde_json::to_vec(&wanted).unwrap(),
        )
        .unwrap();
        let long_ago = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10);
        fs::File::options()
            .write(true)
            .open(folder.join(file_name(&wanted_digest)))
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        assert_eq!(fs::read_dir(&folder).unwrap().count(), MOST_RECEIVED + 1);

        // Asked for again: it is over the limit, it is the oldest, and it must still be there.
        assert_eq!(pictures.keep(&wanted).unwrap(), wanted_digest);
        assert!(
            pictures.has(&wanted_digest),
            "the picture just asked for was given up"
        );
        assert_eq!(
            fs::read_dir(&folder).unwrap().count(),
            MOST_RECEIVED,
            "the folder was not brought back to the limit"
        );
        // What went was the oldest of the others.
        assert!(!pictures.has(&others[0]));
    }

    #[test]
    fn trimming_touches_only_what_this_module_made() {
        let (directory, pictures) = store();
        let folder = directory.path().join("device-pictures");
        fs::create_dir_all(&folder).unwrap();
        // Things that are not picture files: not counted against the limit, and never removed.
        for name in [
            "notes.txt",
            "README",
            ".hidden",
            "0123.json",
            &format!("{}.json", "A".repeat(64)),
        ] {
            fs::write(folder.join(name), b"mine").unwrap();
        }
        for n in 0..MOST_RECEIVED + 3 {
            pictures
                .keep(&Picture::new(1, 1, vec![(n % 256) as u8, (n / 256) as u8, 1, 255]).unwrap())
                .unwrap();
        }
        for name in ["notes.txt", "README", ".hidden", "0123.json"] {
            assert_eq!(
                fs::read(folder.join(name)).unwrap(),
                b"mine",
                "{name} was touched"
            );
        }
        assert!(
            folder.join(format!("{}.json", "A".repeat(64))).exists(),
            "a name that is not a digest was removed"
        );
    }

    #[test]
    fn forgetting_a_picture_removes_only_it_and_not_having_it_is_not_an_error() {
        let (_directory, pictures) = store();
        let a = pictures.keep(&picture(1)).unwrap();
        let b = pictures.keep(&picture(2)).unwrap();
        pictures.forget(&a).unwrap();
        assert!(!pictures.has(&a) && pictures.has(&b));
        pictures.forget(&a).unwrap();
        pictures.forget(&Picture::digest_of_none()).unwrap();
    }

    #[test]
    fn a_file_larger_than_any_picture_can_be_is_refused_without_being_read_whole() {
        let directory = tempfile::tempdir().unwrap();
        let pictures = Pictures::new(directory.path());
        let path = directory.path().join("profile-picture.json");
        // Valid JSON that is only too long: a string of spaces inside it, so that nothing but the size
        // can be what refuses it (a file of zeros would be refused as not being JSON at all).
        let mut text = br#"{"width":1,"height":1,"rgba":"00000000"}"#.to_vec();
        text.extend(std::iter::repeat_n(b' ', MAX_FILE as usize));
        fs::write(&path, &text).unwrap();
        assert!(text.len() as u64 > MAX_FILE);
        assert!(pictures.own().is_err(), "a file past the limit was read");
        // Exactly at the limit, the same picture is read.
        text.truncate(MAX_FILE as usize);
        fs::write(&path, &text).unwrap();
        assert!(
            pictures.own().unwrap().is_some(),
            "a file at the limit was refused"
        );
    }
}
