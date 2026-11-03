//! Which picture each device last showed, kept in a file of its own.
//!
//! The pictures themselves are kept by [`crate::pictures`], by digest, and survive a restart. What that module does not know is
//! WHICH DEVICE a picture belongs to: that is the host's, and it was kept only in memory. So at every start the host knew no one's
//! picture until that device came online again and showed it, and a device that was offline (or a card opened before it
//! reconnected) was drawn with its initial instead of its face. This is that memory, on disk.
//!
//! It holds an identity and a digest and nothing else: no name, no path, no picture. A digest is not trusted for what it names:
//! the host asks [`crate::pictures::Pictures::has`] before it reports one, so a digest whose picture is gone shows nothing.
//! It is bounded (a count and a size), written whole and moved into place (a stop in the middle leaves the old file), and a file
//! that cannot be read is an error that is said, never replaced by an empty one.

use peersus_core::{model::PeerId, profile::Digest};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    path::PathBuf,
};

/// The most devices remembered. A person's devices are a handful; this keeps the file under a few hundred kilobytes however many pass.
const MOST_DEVICES: usize = 1024;
/// The most bytes the file may be, read or written.
const MAX_BYTES: u64 = 512 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    owners: BTreeMap<PeerId, Digest>,
}

pub struct PictureOwners {
    path: PathBuf,
    owners: BTreeMap<PeerId, Digest>,
}

impl PictureOwners {
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let owners = match File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_BYTES {
                    return Err(invalid());
                }
                let document: Document = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                if document.version != 1 || document.owners.len() > MOST_DEVICES {
                    return Err(invalid());
                }
                document.owners
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error),
        };
        Ok(Self { path, owners })
    }

    pub fn all(&self) -> &BTreeMap<PeerId, Digest> {
        &self.owners
    }

    /// Remember that `peer` showed the picture `digest`, or that it shows none (`None`). Writes only when something changes, so
    /// a device that comes online with the picture it already had costs nothing.
    pub fn set(&mut self, peer: &PeerId, digest: Option<Digest>) -> io::Result<bool> {
        let mut candidate = self.owners.clone();
        match digest {
            Some(digest) => {
                if candidate.get(peer) == Some(&digest) {
                    return Ok(false);
                }
                // A new device past the limit is not remembered: the ones already here are people's own, and are not pushed out by a stranger.
                if !candidate.contains_key(peer) && candidate.len() >= MOST_DEVICES {
                    return Ok(false);
                }
                candidate.insert(peer.clone(), digest);
            }
            None => {
                if candidate.remove(peer).is_none() {
                    return Ok(false);
                }
            }
        }
        self.write(&candidate)?;
        self.owners = candidate;
        Ok(true)
    }

    /// Forget what `peer` showed: its device was forgotten, and nothing of it is kept.
    pub fn forget(&mut self, peer: &PeerId) -> io::Result<bool> {
        self.set(peer, None)
    }

    fn write(&self, owners: &BTreeMap<PeerId, Digest>) -> io::Result<()> {
        let bytes = serde_json::to_vec(&Document { version: 1, owners: owners.clone() }).map_err(|_| invalid())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        crate::atomic::write(&self.path, &bytes)
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Device pictures list is invalid; original preserved")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str) -> PeerId {
        PeerId(name.into())
    }
    fn digest(n: u8) -> Digest {
        peersus_core::profile::Picture::new(1, 1, vec![n, n, n, 255]).unwrap().digest()
    }

    #[test]
    fn what_a_device_showed_is_there_when_the_file_is_opened_again() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("picture-owners.json");
        let mut owners = PictureOwners::open(&path).unwrap();
        assert!(owners.set(&peer("a"), Some(digest(1))).unwrap());
        assert!(owners.set(&peer("b"), Some(digest(2))).unwrap());
        // A restart: the file is opened again, and every device has its picture before any of them is online.
        let again = PictureOwners::open(&path).unwrap();
        assert_eq!(again.all().get(&peer("a")), Some(&digest(1)));
        assert_eq!(again.all().get(&peer("b")), Some(&digest(2)));
    }

    #[test]
    fn the_same_picture_again_writes_nothing_and_a_new_one_replaces_the_old() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("picture-owners.json");
        let mut owners = PictureOwners::open(&path).unwrap();
        assert!(owners.set(&peer("a"), Some(digest(1))).unwrap());
        assert!(!owners.set(&peer("a"), Some(digest(1))).unwrap(), "the same picture was written again");
        assert!(owners.set(&peer("a"), Some(digest(2))).unwrap());
        assert_eq!(owners.all().get(&peer("a")), Some(&digest(2)));
        // A device that shows none is remembered as having none, not as having its old one.
        assert!(owners.set(&peer("a"), None).unwrap());
        assert!(owners.all().is_empty());
        assert!(!owners.set(&peer("a"), None).unwrap(), "forgetting what is not there wrote the file");
    }

    #[test]
    fn a_forgotten_device_is_gone_from_the_file_too() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("picture-owners.json");
        let mut owners = PictureOwners::open(&path).unwrap();
        owners.set(&peer("a"), Some(digest(1))).unwrap();
        owners.set(&peer("b"), Some(digest(2))).unwrap();
        assert!(owners.forget(&peer("a")).unwrap());
        let again = PictureOwners::open(&path).unwrap();
        assert!(!again.all().contains_key(&peer("a")), "a forgotten device's picture stayed on disk");
        assert_eq!(again.all().get(&peer("b")), Some(&digest(2)), "another device's picture went with it");
    }

    #[test]
    fn a_file_that_is_not_a_list_is_an_error_and_is_never_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("picture-owners.json");
        std::fs::write(&path, b"not a list").unwrap();
        assert!(PictureOwners::open(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not a list", "an unreadable file was replaced");
    }

    #[test]
    fn a_stranger_past_the_limit_is_not_remembered_and_does_not_push_a_known_device_out() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("picture-owners.json");
        let mut owners = PictureOwners::open(&path).unwrap();
        for n in 0..MOST_DEVICES {
            owners.owners.insert(peer(&format!("p{n}")), digest(1));
        }
        assert!(!owners.set(&peer("stranger"), Some(digest(2))).unwrap(), "a device past the limit was remembered");
        assert!(!owners.all().contains_key(&peer("stranger")));
        // A device already known can still change its picture at the limit.
        assert!(owners.set(&peer("p0"), Some(digest(3))).unwrap());
    }
}
