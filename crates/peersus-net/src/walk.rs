//! Walk a folder the person chose, one file at a time, to send everything in it with the folders it sits in.
//!
//! WHAT IT HOLDS. A stack of open folders, one for each level the walk is inside, and nothing else: at most [`MAX_FOLDER_DEPTH`] + 1 of them.
//! Each folder is read ONE ENTRY AT A TIME, from the system's own iterator, so a folder with a billion files in it is not read into a list,
//! not sorted and not counted before the first file is given: the first file is given after the first entry is read. What is held does not
//! grow with the number of files, in one folder or in all of them; it grows with how deep the walk is.
//!
//! ITS ORDER is the one the system gives the entries of a folder. It is not sorted, because sorting a folder means holding all of it; the
//! same folder is walked in the same order as long as it does not change, and nothing here depends on the order.
//!
//! WHAT IT NEVER FOLLOWS. A link. Someone can put a link inside a folder (to a private key, to the system's password file), and a
//! person who sends the folder never meant to send what the link points at. A link, a socket, a device and everything else that is
//! not a plain file or a plain folder is passed over and counted, never opened and never followed. (A file the person chose
//! on its own is another thing: they chose it, and [`crate::transfer::Source::open`] follows a link they dropped. This is about what
//! was NOT chosen, which is everything inside a folder.)
//!
//! WHAT IT KEEPS. The folders a file sits in, as a list of names starting from the folder that was chosen (which is the first name),
//! so that the other side builds the same folders.

use crate::transfer::{MAX_FOLDER_DEPTH, Offer};
#[cfg(windows)]
use cap_fs_ext::DirExt;
#[cfg(unix)]
use rustix::fs::{Dir, FileType, Mode, OFlags, openat};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

/// An open folder that names are reached relative to: a descriptor on Unix, a `cap-std` folder on
/// Windows (which has no `openat`).
#[cfg(unix)]
type Handle = fs::File;
#[cfg(windows)]
type Handle = cap_std::fs::Dir;

/// A file reached by the walk. Its display path is never used to open it.
#[derive(Clone, Debug)]
pub struct Found {
    pub path: PathBuf,
    pub folders: Vec<String>,
    parent: Arc<Handle>,
}

impl Found {
    /// Open relative to the directory actually visited, not its replaceable path.
    #[cfg(unix)]
    pub(crate) fn open(&self) -> io::Result<fs::File> {
        let name = self.path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        Ok(openat(&*self.parent, name, flags, Mode::empty())?.into())
    }

    /// Open relative to the directory actually visited, never through a link.
    #[cfg(windows)]
    pub(crate) fn open(&self) -> io::Result<fs::File> {
        use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
        let name = self.path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        Ok(self.parent.open_with(name, &options)?.into_std())
    }
}

/// What an entry of a folder is, judged without following it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Folder,
    Other,
}

/// Entries skipped without silently treating a partial folder as complete.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Passed {
    pub not_plain: usize,
    pub unreadable: usize,
    pub too_deep: usize,
}

struct Counted {
    #[cfg(unix)]
    entries: Dir,
    #[cfg(windows)]
    entries: cap_std::fs::ReadDir,
    taken: Arc<AtomicUsize>,
}

impl Iterator for Counted {
    /// The entry's name (`None` when it is not text) and what it is.
    type Item = io::Result<(Option<String>, Kind)>;

    #[cfg(unix)]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let entry = self.entries.next()?;
            // Directory APIs expose these entries; they are not children.
            if let Ok(entry) = &entry
                && matches!(entry.file_name().to_bytes(), b"." | b"..")
            {
                continue;
            }
            self.taken.fetch_add(1, Ordering::Relaxed);
            return Some(entry.map_err(Into::into).map(|entry| {
                let kind = match entry.file_type() {
                    FileType::RegularFile => Kind::File,
                    FileType::Directory => Kind::Folder,
                    _ => Kind::Other,
                };
                (entry.file_name().to_str().ok().map(str::to_owned), kind)
            }));
        }
    }

    #[cfg(windows)]
    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.entries.next()?;
        self.taken.fetch_add(1, Ordering::Relaxed);
        Some(entry.and_then(|entry| {
            // `cap-std` judges the entry itself: a link or junction is neither a file nor a folder.
            let kind = entry.file_type()?;
            let kind = if kind.is_symlink() {
                Kind::Other
            } else if kind.is_file() {
                Kind::File
            } else if kind.is_dir() {
                Kind::Folder
            } else {
                Kind::Other
            };
            Ok((entry.file_name().into_string().ok(), kind))
        }))
    }
}

struct Level {
    entries: Counted,
    handle: Arc<Handle>,
    path: PathBuf,
    names: Vec<String>,
}

impl Level {
    fn new(
        handle: Handle,
        path: PathBuf,
        names: Vec<String>,
        taken: &Arc<AtomicUsize>,
    ) -> io::Result<Self> {
        let entries = Counted {
            #[cfg(unix)]
            entries: Dir::read_from(&handle)?,
            #[cfg(windows)]
            entries: handle.entries()?,
            taken: Arc::clone(taken),
        };
        Ok(Self {
            entries,
            handle: Arc::new(handle),
            path,
            names,
        })
    }
}

/// Lazy depth-first traversal retaining directory capabilities, not reopenable paths.
pub struct Walk {
    levels: Vec<Level>,
    passed: Passed,
    taken: Arc<AtomicUsize>,
}

impl Walk {
    /// Open the chosen directory once, refusing a symlink at the selected root.
    pub fn new(root: &Path) -> io::Result<Self> {
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| Offer::is_one_name(name))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a folder with a name that cannot be sent",
                )
            })?
            .to_owned();
        #[cfg(unix)]
        let handle: fs::File = {
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            rustix::fs::open(root, flags, Mode::empty())?.into()
        };
        // The chosen folder itself must not be a link or junction, as on Unix.
        #[cfg(windows)]
        let handle = {
            if fs::symlink_metadata(root)?.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a link is not walked",
                ));
            }
            let parent = root.parent().ok_or(io::ErrorKind::InvalidInput)?;
            cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())?
                .open_dir_nofollow(&name)?
        };
        let taken = Arc::new(AtomicUsize::new(0));
        let level = Level::new(handle, root.to_path_buf(), vec![name], &taken)?;
        Ok(Self {
            levels: vec![level],
            passed: Passed::default(),
            taken,
        })
    }

    pub fn passed(&self) -> Passed {
        self.passed
    }

    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    pub fn read(&self) -> usize {
        self.taken.load(Ordering::Relaxed)
    }
}

impl Iterator for Walk {
    type Item = Found;

    fn next(&mut self) -> Option<Found> {
        loop {
            let level = self.levels.last_mut()?;
            let Some(entry) = level.entries.next() else {
                self.levels.pop();
                continue;
            };
            let Ok(entry) = entry else {
                self.passed.unreadable += 1;
                continue;
            };
            let (Some(name), kind) = entry else {
                self.passed.unreadable += 1;
                continue;
            };
            if kind == Kind::Other {
                self.passed.not_plain += 1;
                continue;
            }
            if !Offer::is_one_name(&name) {
                self.passed.unreadable += 1;
                continue;
            }
            let path = level.path.join(&name);
            let mut names = level.names.clone();
            if kind == Kind::File {
                return Some(Found {
                    path,
                    folders: names,
                    parent: Arc::clone(&level.handle),
                });
            }
            if names.len() >= MAX_FOLDER_DEPTH {
                self.passed.too_deep += 1;
                continue;
            }
            #[cfg(unix)]
            let child = {
                let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
                openat(&*level.handle, name.as_str(), flags, Mode::empty())
                    .map(fs::File::from)
                    .map_err(io::Error::from)
            };
            #[cfg(windows)]
            let child = level.handle.open_dir_nofollow(&name);
            names.push(name);
            match child.and_then(|handle| Level::new(handle, path, names, &self.taken)) {
                Ok(level) => self.levels.push(level),
                Err(_) => self.passed.unreadable += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("pictures");
        fs::create_dir_all(root.join("2024").join("summer")).unwrap();
        fs::create_dir_all(root.join("2023")).unwrap();
        fs::write(root.join("cover.jpg"), b"c").unwrap();
        fs::write(root.join("2024").join("a.jpg"), b"a").unwrap();
        fs::write(root.join("2024").join("summer").join("b.jpg"), b"b").unwrap();
        fs::write(root.join("2023").join("z.jpg"), b"z").unwrap();
        base
    }

    fn listed(base: &Path) -> Vec<(String, Vec<String>)> {
        Walk::new(&base.join("pictures"))
            .unwrap()
            .map(|found| {
                (
                    found
                        .path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    found.folders,
                )
            })
            .collect()
    }

    #[test]
    fn every_file_of_the_folder_is_found_with_the_folders_it_sits_in_starting_from_the_chosen_one()
    {
        let base = tree();
        let found = listed(base.path());
        let s = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        assert_eq!(found.len(), 4);
        assert!(found.contains(&("cover.jpg".into(), s(&["pictures"]))));
        assert!(found.contains(&("a.jpg".into(), s(&["pictures", "2024"]))));
        assert!(found.contains(&("b.jpg".into(), s(&["pictures", "2024", "summer"]))));
        assert!(found.contains(&("z.jpg".into(), s(&["pictures", "2023"]))));
    }

    #[test]
    fn every_file_is_given_once_and_a_folder_is_finished_before_its_next_neighbour_is_entered() {
        let base = tree();
        let first = listed(base.path());
        // The order is the system's own and is not promised; what is promised is that each file is given exactly once.
        let mut names: Vec<_> = first.iter().map(|(name, _)| name.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            ["a.jpg", "b.jpg", "cover.jpg", "z.jpg"],
            "a file was given twice or not at all"
        );
        // Depth first: the files of `summer` come right after `a.jpg` or right before it, never with another folder's files between.
        let place = |name: &str| first.iter().position(|(file, _)| file == name).unwrap();
        assert_eq!(
            place("a.jpg").abs_diff(place("b.jpg")),
            1,
            "a folder was left half read to go into another"
        );
    }

    #[test]
    fn the_first_files_of_a_very_wide_folder_are_given_after_the_entries_up_to_them_and_not_after_the_whole_folder()
     {
        // A folder of twenty thousand files, and a walk that is stopped after ten. A walk that read the folder into a list first, to
        // sort it or to count it, would have taken all twenty thousand entries from the system by the time it gave the first file: it
        // is the number taken, and not how many levels are open, that says whether a folder is held.
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("wide");
        fs::create_dir(&root).unwrap();
        for n in 0..20_000 {
            fs::write(root.join(format!("f{n:05}")), b"").unwrap();
        }
        let mut walk = Walk::new(&root).unwrap();
        assert_eq!(
            walk.read(),
            0,
            "the walk took entries before the first was asked for"
        );
        assert!(walk.next().is_some());
        assert_eq!(
            walk.read(),
            1,
            "the first file was given after more than its own entry was read"
        );
        let more: Vec<_> = walk.by_ref().take(9).collect();
        assert_eq!(more.len(), 9);
        assert_eq!(
            walk.read(),
            10,
            "the walk had read ahead of the files it gave"
        );
        assert_eq!(walk.depth(), 1);
        assert_eq!(walk.passed(), Passed::default());
        // And every one of the twenty thousand is given, once each, in the end.
        assert_eq!(walk.count() + 10, 20_000);
    }

    #[test]
    fn what_the_walk_holds_grows_with_how_deep_it_is_and_never_with_how_many_files_there_are() {
        let base = tempfile::tempdir().unwrap();
        let mut folder = base.path().join("top");
        fs::create_dir(&folder).unwrap();
        for level in 0..5 {
            // Many files at every level, and one folder inside.
            for n in 0..300 {
                fs::write(folder.join(format!("f{n:03}")), b"x").unwrap();
            }
            folder = folder.join(format!("d{level}"));
            fs::create_dir(&folder).unwrap();
        }
        let mut walk = Walk::new(&base.path().join("top")).unwrap();
        let mut deepest = 0;
        let mut count = 0;
        while walk.next().is_some() {
            count += 1;
            deepest = deepest.max(walk.depth());
        }
        assert_eq!(count, 1500);
        // Six folders deep at the most (the top and five more), and never more, for fifteen hundred files.
        assert!(
            deepest <= 6,
            "the walk held {deepest} open folders for a tree six deep"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_the_folder_is_passed_over_and_what_it_points_at_is_never_sent() {
        let base = tree();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.key"), b"private").unwrap();
        let root = base.path().join("pictures");
        // A link to a file and a link to a folder, both pointing outside what was chosen, and one pointing nowhere.
        std::os::unix::fs::symlink(outside.path().join("secret.key"), root.join("innocent.jpg"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("2024").join("more")).unwrap();
        std::os::unix::fs::symlink(root.join("nothing"), root.join("dangling")).unwrap();
        let mut walk = Walk::new(&root).unwrap();
        let found: Vec<_> = walk.by_ref().collect();
        assert_eq!(
            found.len(),
            4,
            "a link was taken as a file or a folder: {found:?}"
        );
        assert!(
            found.iter().all(|f| !f.path.starts_with(outside.path())),
            "the walk went outside the chosen folder"
        );
        assert!(
            found
                .iter()
                .all(|f| f.path.file_name().unwrap() != "secret.key"
                    && f.path.file_name().unwrap() != "innocent.jpg")
        );
        assert_eq!(
            walk.passed().not_plain,
            3,
            "the links were not counted as passed over"
        );
    }

    #[test]
    fn a_folder_deeper_than_the_other_side_takes_is_cut_and_said_and_nothing_is_lost_above_it() {
        let base = tempfile::tempdir().unwrap();
        let mut deep = base.path().join("top");
        fs::create_dir(&deep).unwrap();
        fs::write(deep.join("kept.txt"), b"k").unwrap();
        for level in 0..(MAX_FOLDER_DEPTH + 2) {
            deep = deep.join(format!("d{level}"));
            fs::create_dir(&deep).unwrap();
            fs::write(deep.join("f.txt"), b"x").unwrap();
        }
        let mut walk = Walk::new(&base.path().join("top")).unwrap();
        let found: Vec<_> = walk.by_ref().collect();
        assert!(
            found.iter().all(|f| f.folders.len() <= MAX_FOLDER_DEPTH),
            "a path deeper than the limit was produced"
        );
        assert!(
            found.iter().any(|f| f.path.ends_with("kept.txt")),
            "a file above the cut was lost"
        );
        assert!(walk.passed().too_deep >= 1, "a cut was not counted");
    }

    // Windows drops a trailing space when a file is made, so the name cannot exist there.
    #[cfg(unix)]
    #[test]
    fn a_name_the_other_side_would_refuse_is_passed_over_and_not_turned_into_another() {
        let base = tree();
        let root = base.path().join("pictures");
        // A name the rule refuses and that a file system allows: a trailing space, and a colon on a system where it is allowed.
        fs::write(root.join("bad "), b"x").unwrap();
        let mut walk = Walk::new(&root).unwrap();
        let found: Vec<_> = walk.by_ref().collect();
        assert!(found.iter().all(|f| f.path.file_name().unwrap() != "bad "));
        assert_eq!(walk.passed().unreadable, 1);
    }

    #[test]
    fn only_a_plain_folder_is_walked_and_a_file_or_a_missing_path_is_not() {
        let base = tree();
        assert!(
            Walk::new(&base.path().join("pictures").join("cover.jpg")).is_err(),
            "a file was walked"
        );
        assert!(Walk::new(&base.path().join("nowhere")).is_err());
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), base.path().join("link")).unwrap();
            assert!(
                Walk::new(&base.path().join("link")).is_err(),
                "a link to a folder was walked as if it were the chosen folder"
            );
        }
    }

    #[test]
    fn a_folder_with_nothing_in_it_walks_to_nothing() {
        let base = tempfile::tempdir().unwrap();
        fs::create_dir(base.path().join("empty")).unwrap();
        assert_eq!(Walk::new(&base.path().join("empty")).unwrap().count(), 0);
    }

    /// A junction is a link anyone can make on Windows (`mklink /J`, no privilege needed).
    #[cfg(windows)]
    fn junction(target: &Path, at: &Path) {
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(at)
            .arg(target)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_inside_the_folder_or_as_the_folder_is_never_walked() {
        let base = tree();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.key"), b"private").unwrap();
        let root = base.path().join("pictures");
        junction(outside.path(), &root.join("2024").join("more"));
        let mut walk = Walk::new(&root).unwrap();
        let found: Vec<_> = walk.by_ref().collect();
        assert_eq!(found.len(), 4, "a junction was walked: {found:?}");
        assert!(
            found
                .iter()
                .all(|f| f.path.file_name().unwrap() != "secret.key")
        );
        assert_eq!(walk.passed().not_plain, 1);
        junction(outside.path(), &base.path().join("link"));
        assert!(
            Walk::new(&base.path().join("link")).is_err(),
            "a junction was walked as if it were the chosen folder"
        );
    }
}
