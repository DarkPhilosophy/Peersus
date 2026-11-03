//! Files that the system opened and handed over, held until a transfer takes them.
//!
//! On Android a picked file is not a path: the system opens it and gives the program a descriptor. The descriptor
//! is the file, so it is kept as a [`File`], which owns it and closes it when it is dropped, and is read where it
//! is. Nothing is copied, and nothing is opened a second time: a path such as `/proc/self/fd/N` would be an
//! opening of its own, with its own access check, and a provider that hands over a pipe has no path to open.
//!
//! The interface that takes the descriptor and the owner that sends from it are in one process, but the command
//! that asks for the send travels as text, so a file cannot be in it. What travels is a number, and this is where
//! the number is turned back into the file: the interface [`Held::keep`]s it and says the number, the owner
//! [`Held::take`]s it. A file is taken once: a number that was used is gone, and a number that was never given
//! is nothing, so a command cannot be made to send a file twice or one it was never handed.

use std::{
    collections::BTreeMap,
    fs::File,
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// The most files held at once. A person picks a handful; more than this is not a person, and each one is an open
/// descriptor, of which a process has a limited number.
pub const MOST_HELD: usize = 64;

/// Files held under a number each. Cheap to clone: the clones share what is held, so the interface and the owner,
/// each given one, see the same files.
#[derive(Clone, Default)]
pub struct Held {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    next: u64,
    files: BTreeMap<u64, File>,
    /// A folder only this program can write, where a copy of a file is made when the system will not open it a second time. `None`: the
    /// system's own temporary folder, which a desktop has and a phone does not give a program.
    copies: Option<PathBuf>,
}

impl Held {
    /// Where a second copy of a held file is made when it cannot be opened again: a folder of the program's own. Set once, by what knows
    /// where the program may write. The copies are anonymous files, which the system removes when they are closed, so nothing is left in it.
    #[must_use]
    pub fn with_copies_in(self, folder: PathBuf) -> Self {
        if let Ok(mut inner) = self.inner.lock() {
            inner.copies = Some(folder);
        }
        self
    }

    /// Hold `file`, and say the number to take it by. `Err` with the file itself when [`MOST_HELD`] are held already
    /// (or the registry cannot be locked): the file is given back unheld, to be closed when the caller drops it,
    /// rather than a descriptor kept without bound.
    pub fn keep(&self, file: File) -> Result<u64, File> {
        let Ok(mut inner) = self.inner.lock() else {
            return Err(file);
        };
        if inner.files.len() >= MOST_HELD {
            return Err(file);
        }
        let number = inner.next;
        inner.next = inner.next.wrapping_add(1);
        inner.files.insert(number, file);
        Ok(number)
    }

    /// Take the file held under `number`, once. A number that was taken, or never given, is `None`.
    pub fn take(&self, number: u64) -> Option<File> {
        self.inner.lock().ok()?.files.remove(&number)
    }

    /// How many files are held now.
    pub fn len(&self) -> usize {
        self.inner.lock().map_or(0, |inner| inner.files.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Let go of the file held under `number`, closing it, without sending it: what is done when a person picked a
    /// file and then did not send it. A number that is not held is nothing to let go of.
    pub fn release(&self, number: u64) {
        drop(self.take(number));
    }

    /// Hold another handle on the file held under `number`, with a read position of its own, under a new number. A file sent to
    /// several devices is read once for each of them, and a handle that is only duplicated shares its position with the first, so
    /// two readers would take the bytes between them.
    ///
    /// The file is opened again through the descriptor that is already open, which gives a new open file with the position at the
    /// start and copies nothing. On a phone the system forbids that for a file a provider handed over (the open is refused), and then
    /// the bytes are copied, from the start and without moving the position of the first, into an anonymous file of the program's own
    /// (see [`Held::with_copies_in`]); a reader of that file is as good as a reader of the first. `Err` with the reason when no file is
    /// held under `number`, when the registry is full, or when neither way gives a copy: the caller then has no second copy and says
    /// so, and is never given the first one's handle instead.
    pub fn reopen(&self, number: u64) -> Result<u64, String> {
        use std::os::fd::AsRawFd;
        let (again, folder) = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| "the registry cannot be locked".to_owned())?;
            let file = inner
                .files
                .get(&number)
                .ok_or_else(|| "no file is held under that number".to_owned())?;
            match File::open(format!("/proc/self/fd/{}", file.as_raw_fd())) {
                Ok(again) => (Ok(again), None),
                Err(refused) => (
                    Err(refused),
                    Some((
                        file.try_clone()
                            .map_err(|error| format!("the file cannot be duplicated: {error}"))?,
                        inner.copies.clone(),
                    )),
                ),
            }
        };
        let again = match again {
            Ok(again) => again,
            Err(refused) => {
                let Some((source, copies)) = folder else {
                    return Err(format!("the system will not open it again: {refused}"));
                };
                copy_of(&source, copies.as_deref()).map_err(|error| {
                    format!("it could not be opened again ({refused}) nor copied ({error})")
                })?
            }
        };
        self.keep(again)
            .map_err(|_| "too many files are held".to_owned())
    }
}

/// An anonymous file holding the bytes of `source` from its start, read without moving the position of `source` (a positional read), so
/// that whoever else reads `source` is not disturbed. In `folder` when one is given, else in the system's temporary folder.
fn copy_of(source: &File, folder: Option<&std::path::Path>) -> std::io::Result<File> {
    use std::os::unix::fs::FileExt;
    let mut copy = match folder {
        Some(folder) => tempfile::tempfile_in(folder)?,
        None => tempfile::tempfile()?,
    };
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut at = 0_u64;
    loop {
        let count = source.read_at(&mut buffer, at)?;
        if count == 0 {
            break;
        }
        std::io::Write::write_all(&mut copy, &buffer[..count])?;
        at += count as u64;
    }
    std::io::Seek::rewind(&mut copy)?;
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn file_with(bytes: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        std::io::Seek::rewind(&mut file).unwrap();
        file
    }

    #[test]
    fn a_held_file_comes_back_as_the_same_file_and_only_once() {
        let held = Held::default();
        let number = held.keep(file_with(b"the file")).unwrap();
        let mut taken = held
            .take(number)
            .expect("a file that was kept was not found");
        let mut text = String::new();
        taken.read_to_string(&mut text).unwrap();
        assert_eq!(text, "the file");
        // Taken once: the number is gone, and a command cannot send the same file twice.
        assert!(held.take(number).is_none());
        assert!(held.is_empty());
    }

    #[test]
    fn a_number_never_given_is_nothing_and_numbers_are_not_reused() {
        let held = Held::default();
        assert!(held.take(0).is_none());
        assert!(held.take(u64::MAX).is_none());
        let first = held.keep(file_with(b"a")).unwrap();
        held.release(first);
        let second = held.keep(file_with(b"b")).unwrap();
        // A number that was let go is not given to the next file: a stale command cannot reach a file that is not
        // the one it was made for.
        assert_ne!(first, second);
        assert!(held.take(first).is_none());
        assert!(held.take(second).is_some());
    }

    #[test]
    fn the_clones_share_what_is_held_so_that_the_interface_and_the_owner_see_the_same_files() {
        let interface = Held::default();
        let owner = interface.clone();
        let number = interface.keep(file_with(b"shared")).unwrap();
        assert_eq!(owner.len(), 1);
        assert!(owner.take(number).is_some());
        assert!(
            interface.is_empty(),
            "the interface still held a file that the owner took"
        );
    }

    #[test]
    fn no_more_than_the_most_are_held_and_the_one_over_is_given_back_to_be_closed() {
        let held = Held::default();
        let mut numbers = Vec::new();
        for _ in 0..MOST_HELD {
            numbers.push(held.keep(file_with(b"x")).unwrap());
        }
        assert_eq!(held.len(), MOST_HELD);
        let over = held.keep(file_with(b"one too many"));
        assert!(over.is_err(), "a file was held past the limit");
        assert_eq!(held.len(), MOST_HELD);
        // Room again once one is let go.
        held.release(numbers[0]);
        assert!(held.keep(file_with(b"fits now")).is_ok());
    }

    #[test]
    fn letting_go_of_a_file_closes_it_and_a_number_not_held_is_nothing_to_let_go_of() {
        let held = Held::default();
        let number = held.keep(file_with(b"gone")).unwrap();
        held.release(number);
        assert!(held.is_empty());
        held.release(number);
        held.release(12345);
        assert!(held.is_empty());
    }

    #[test]
    fn a_reopened_file_has_a_read_position_of_its_own_and_is_taken_by_its_own_number() {
        let held = Held::default();
        let first = held.keep(file_with(b"0123456789")).unwrap();
        let second = held
            .reopen(first)
            .expect("a held file could not be opened again");
        assert_ne!(first, second);
        assert_eq!(held.len(), 2);
        let mut one = held.take(first).unwrap();
        let mut two = held.take(second).unwrap();
        // Each reads the whole file from its start, whichever is read first and however far the other has gone.
        let mut head = [0_u8; 4];
        one.read_exact(&mut head).unwrap();
        assert_eq!(&head, b"0123");
        let mut all = String::new();
        two.read_to_string(&mut all).unwrap();
        assert_eq!(
            all, "0123456789",
            "the second copy began where the first one was read to"
        );
    }

    #[test]
    fn a_number_not_held_cannot_be_reopened_and_a_full_registry_gives_no_copy() {
        let held = Held::default();
        assert!(held.reopen(0).is_err());
        let mut numbers = Vec::new();
        for _ in 0..MOST_HELD {
            numbers.push(held.keep(file_with(b"x")).unwrap());
        }
        assert!(
            held.reopen(numbers[0]).is_err(),
            "a copy was held past the limit"
        );
        assert_eq!(held.len(), MOST_HELD);
    }

    /// A desktop lets a file be opened again, so the refusal of a phone cannot be made here with a real file: what is tried is the copy that
    /// is made in its place, which is what `reopen` hands over once the system has said no.
    #[test]
    fn a_copy_is_the_whole_file_from_its_start_whatever_the_first_has_read_and_does_not_move_the_first()
     {
        let mut original = file_with(b"0123456789");
        let mut head = [0_u8; 4];
        original.read_exact(&mut head).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let mut copy = copy_of(&original, Some(folder.path())).unwrap();
        let mut all = String::new();
        copy.read_to_string(&mut all).unwrap();
        assert_eq!(
            all, "0123456789",
            "the copy began where the first one was read to"
        );
        // The first one is where it was: the copy was made without moving it.
        let mut rest = String::new();
        original.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "456789");
    }

    #[test]
    fn a_copy_of_a_file_longer_than_one_read_is_whole() {
        let bytes: Vec<u8> = (0..200_000_u32).map(|n| (n % 251) as u8).collect();
        let original = file_with(&bytes);
        let mut copy = copy_of(&original, None).unwrap();
        let mut read = Vec::new();
        copy.read_to_end(&mut read).unwrap();
        assert_eq!(read, bytes);
    }

    #[test]
    fn a_copy_is_left_nowhere_once_it_is_closed() {
        let folder = tempfile::tempdir().unwrap();
        let held = Held::default().with_copies_in(folder.path().to_path_buf());
        let original = file_with(b"left nowhere");
        let copy = copy_of(&original, Some(folder.path())).unwrap();
        let number = held.keep(copy).unwrap();
        held.release(number);
        assert_eq!(
            std::fs::read_dir(folder.path()).unwrap().count(),
            0,
            "a copy stayed in the folder after it was let go of"
        );
    }
}
