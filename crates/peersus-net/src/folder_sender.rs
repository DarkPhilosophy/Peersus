//! A folder being sent: its files handed out one at a time, each OPENED ONLY WHEN IT IS ASKED FOR.
//!
//! WHY NOT ALL AT ONCE. A folder can hold more files than a system keeps open, and than a list can hold: it is walked as it is sent
//! ([`crate::walk::Walk`]) and a file is opened when there is room for it to go and not before. What this keeps is the walk, which is a
//! stack of folders, and two counts. Not one file is kept open while it waits.
//!
//! WHAT IS LOST, AND WHAT IS NOT. Nothing is lost without being counted. A file that cannot be opened when its turn comes (it was
//! deleted since the walk saw it, it became a link, it is no longer a plain file) is counted in [`Sent::failed`] and the folder goes
//! on with the next: one file does not stop a folder. What the walk passed over is in [`FolderSender::passed`]. The person is told
//! both, so a folder sent with files missing is never taken for a folder sent whole.
//!
//! WHAT IT DOES NOT DECIDE. When there is room: that is the host's. This is asked for the next file and answers; it starts nothing.

use crate::{
    transfer::Source,
    walk::{Passed, Walk},
};
use std::path::Path;

/// How a folder's sending stands: what has been handed out and what could not be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sent {
    /// Files opened and handed over to be offered.
    pub started: usize,
    /// Files that were there when the walk saw them and could not be opened when their turn came.
    pub failed: usize,
}

/// A folder being handed out, file by file.
pub struct FolderSender {
    walk: Walk,
    sent: Sent,
    done: bool,
}

impl FolderSender {
    /// Begin a folder. `root` must be a plain folder (see [`Walk::new`]).
    pub fn new(root: &Path) -> std::io::Result<Self> {
        Ok(Self {
            walk: Walk::new(root)?,
            sent: Sent::default(),
            done: false,
        })
    }

    /// Whether every file was handed out.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// What has been handed out and what could not be.
    pub fn sent(&self) -> Sent {
        self.sent
    }

    /// What the walk passed over so far (links, names the other side would refuse, folders too deep, folders that could not be read).
    pub fn passed(&self) -> Passed {
        self.walk.passed()
    }

    /// The next file the walk reaches, NOT yet opened, or `None` at the end (which is then remembered: [`Self::is_done`]). Between this and
    /// [`Self::open_found`] a file can change, since the walk looked at it and before it is opened: that gap is what opening without
    /// following a link and judging on the open file is for.
    pub fn next_found(&mut self) -> Option<crate::walk::Found> {
        let found = self.walk.next();
        if found.is_none() {
            self.done = true;
        }
        found
    }

    /// Open a file the walk reached, now. One that cannot be opened (gone, a link now, no longer a plain file) is counted in
    /// [`Sent::failed`] and is `None`; one that opens is counted in [`Sent::started`].
    pub async fn open_found(&mut self, found: crate::walk::Found) -> Option<Source> {
        let opened = match found.open() {
            Ok(file) => {
                let name = found.path.file_name()?.to_str()?;
                Source::from_file_in(file, name, Some(found.folders)).await
            }
            Err(error) => Err(crate::transfer::Error::Io(error)),
        };
        match opened {
            Ok(source) => {
                self.sent.started += 1;
                Some(source)
            }
            Err(_) => {
                self.sent.failed += 1;
                None
            }
        }
    }

    /// The next file, opened now, or `None` when there are no more. A file that cannot be opened is counted and the next is tried, in
    /// this call, so that a caller asking for a file gets one if any is left.
    pub async fn next(&mut self) -> Option<Source> {
        loop {
            let found = self.next_found()?;
            if let Some(source) = self.open_found(found).await {
                return Some(source);
            }
        }
    }

    /// A file this handed out was not offered after all (the device went, there was no room): it is counted as one that could not be
    /// sent and no longer as one that started, so that a file is never counted as both and the person is told the true number.
    pub fn count_failed(&mut self) {
        self.sent.started = self.sent.started.saturating_sub(1);
        self.sent.failed += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree(count: usize) -> tempfile::TempDir {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("pictures");
        fs::create_dir_all(root.join("2024")).unwrap();
        for n in 0..count {
            let folder = if n % 2 == 0 {
                root.clone()
            } else {
                root.join("2024")
            };
            fs::write(folder.join(format!("f{n:03}.txt")), format!("{n}")).unwrap();
        }
        base
    }

    #[tokio::test]
    async fn every_file_is_handed_out_once_with_its_folders_and_then_there_are_no_more() {
        let base = tree(5);
        let mut sender = FolderSender::new(&base.path().join("pictures")).unwrap();
        let mut got = Vec::new();
        while let Some(source) = sender.next().await {
            got.push((source.offer.name.clone(), source.offer.folders().to_vec()));
        }
        assert_eq!(got.len(), 5);
        assert!(sender.is_done());
        assert_eq!(
            sender.sent(),
            Sent {
                started: 5,
                failed: 0
            }
        );
        assert!(
            got.iter()
                .all(|(_, folders)| folders.first().map(String::as_str) == Some("pictures")),
            "a file lost the chosen folder"
        );
        assert!(
            got.iter()
                .any(|(_, folders)| folders == &["pictures", "2024"])
        );
        // Asked again after the end, it stays at the end.
        assert!(sender.next().await.is_none());
    }

    #[tokio::test]
    async fn a_file_is_opened_only_when_asked_for_and_a_waiting_folder_holds_none_open() {
        // What this guards: a folder of more files than a system keeps open. After the walk has been made and before a file is asked
        // for, nothing is open; each file is opened by `next` and is the caller's to drop.
        let base = tree(50);
        let mut sender = FolderSender::new(&base.path().join("pictures")).unwrap();
        assert_eq!(
            sender.sent().started,
            0,
            "a file was opened before it was asked for"
        );
        let first = sender.next().await.unwrap();
        assert_eq!(
            sender.sent().started,
            1,
            "more than one file was opened for one asked"
        );
        drop(first);
        // Only the asked-for ones are ever opened.
        for _ in 0..4 {
            drop(sender.next().await.unwrap());
        }
        assert_eq!(sender.sent().started, 5);
        assert!(!sender.is_done());
    }

    /// A folder `pictures` with three files in it.
    fn three() -> (tempfile::TempDir, std::path::PathBuf) {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("pictures");
        fs::create_dir(&root).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(root.join(name), name).unwrap();
        }
        (base, root)
    }

    #[tokio::test]
    async fn a_file_that_is_gone_between_being_reached_and_being_opened_is_counted_and_the_folder_goes_on()
     {
        let (_base, root) = three();
        let mut sender = FolderSender::new(&root).unwrap();
        // The walk reaches a file; before it is opened, that file is deleted. This is the window a walk that looks at a folder and sends
        // it a file at a time always has, and what is reached next must still go.
        let reached = sender.next_found().unwrap();
        fs::remove_file(&reached.path).unwrap();
        assert!(
            sender.open_found(reached).await.is_none(),
            "a file that was gone was opened"
        );
        assert_eq!(
            sender.sent(),
            Sent {
                started: 0,
                failed: 1
            },
            "the file that was gone was not counted"
        );
        // The other two are still handed out, and nothing is counted twice.
        let mut after = 0;
        while sender.next().await.is_some() {
            after += 1;
        }
        assert_eq!(
            after, 2,
            "the folder did not go on past the file that was gone"
        );
        assert_eq!(
            sender.sent(),
            Sent {
                started: 2,
                failed: 1
            }
        );
        assert!(sender.is_done());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_turned_into_a_link_between_being_reached_and_being_opened_is_refused_and_what_it_pointed_at_is_not_sent()
     {
        let (_base, root) = three();
        let secret = tempfile::tempdir().unwrap();
        fs::write(secret.path().join("private.key"), b"do not send").unwrap();
        let mut sender = FolderSender::new(&root).unwrap();
        let reached = sender.next_found().unwrap();
        // The walk saw a plain file. Before it is opened it becomes a link to something private.
        fs::remove_file(&reached.path).unwrap();
        std::os::unix::fs::symlink(secret.path().join("private.key"), &reached.path).unwrap();
        assert!(
            sender.open_found(reached).await.is_none(),
            "a link was opened and its target sent"
        );
        assert_eq!(
            sender.sent(),
            Sent {
                started: 0,
                failed: 1
            },
            "the link was not counted as a file that could not be sent"
        );
    }

    #[tokio::test]
    async fn a_folder_in_which_every_file_fails_still_ends_and_counts_each_one_without_growing_a_stack()
     {
        // Many files, each deleted after the walk reached it and before it is opened: each fails. This is a loop, not a recursion, so how
        // many fail cannot grow what is kept; it must end at the end of the folder and count every one.
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("pictures");
        fs::create_dir(&root).unwrap();
        let count = 3000;
        for n in 0..count {
            fs::write(root.join(format!("f{n:05}.txt")), b"x").unwrap();
        }
        let mut sender = FolderSender::new(&root).unwrap();
        let mut failed = 0;
        while let Some(reached) = sender.next_found() {
            fs::remove_file(&reached.path).unwrap();
            assert!(sender.open_found(reached).await.is_none());
            failed += 1;
        }
        assert_eq!(failed, count);
        assert!(sender.is_done());
        assert_eq!(
            sender.sent(),
            Sent {
                started: 0,
                failed: count
            },
            "a file that failed was not counted"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replacing_an_ancestor_after_discovery_cannot_redirect_the_open_file() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("chosen");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("file.txt"), b"chosen").unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("file.txt"), b"not selected by the user").unwrap();
        let mut sender = FolderSender::new(&root).unwrap();
        let found = sender.next_found().unwrap();
        fs::rename(&root, base.path().join("original")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &root).unwrap();
        let source = sender.open_found(found).await.unwrap();
        assert_eq!(
            source.offer.bytes, 6,
            "the replacement ancestor redirected the file open"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replacing_the_root_before_iteration_cannot_redirect_nested_traversal() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("chosen");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/file.txt"), b"chosen").unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(outside.path().join("nested")).unwrap();
        fs::write(
            outside.path().join("nested/file.txt"),
            b"not selected by the user",
        )
        .unwrap();
        let mut sender = FolderSender::new(&root).unwrap();
        fs::rename(&root, base.path().join("original")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &root).unwrap();
        let source = sender.next().await.unwrap();
        assert_eq!(
            source.offer.bytes, 6,
            "the replacement root redirected traversal"
        );
        assert_eq!(source.offer.folders(), ["chosen", "nested"]);
    }

    #[tokio::test]
    async fn a_folder_with_nothing_in_it_is_done_at_once_and_a_file_is_not_a_folder() {
        let base = tempfile::tempdir().unwrap();
        fs::create_dir(base.path().join("empty")).unwrap();
        let mut sender = FolderSender::new(&base.path().join("empty")).unwrap();
        assert!(sender.next().await.is_none());
        assert!(sender.is_done());
        fs::write(base.path().join("a-file"), b"x").unwrap();
        assert!(
            FolderSender::new(&base.path().join("a-file")).is_err(),
            "a file was taken as a folder to send"
        );
    }
}
