//! What a platform's own folder picker hands over: the folder a person chose to put a received file in.
//!
//! On a phone the program cannot write into a folder the person chose: the system's picker returns a handle (a tree address)
//! that only the system's own calls can write through, never a path. So the answer is kept here as that text, which nothing in
//! the program reads into a path, and the platform layer is what writes into it. Like [`crate::picked::Picked`], the answer
//! comes on a thread that is not the interface's, and only when the person has chosen; everything that decides what is kept is
//! here, with no platform code, so that it is tried without a phone.
//!
//! WHAT IS DECIDED. One answer is kept for the one transfer it was asked for, and is taken once. An answer that arrives when
//! none was asked for is dropped, so that an old choice cannot decide where a later file goes.

use std::sync::{Arc, Mutex};

/// What the interface asked of the folder picker and what came back, kept together so that they cannot disagree.
#[derive(Clone, Default)]
pub struct FolderPicked {
    inner: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    /// The transfer the picker was opened for, until the person answers or backs out.
    asked_for: Option<String>,
    /// The answer: the transfer it is for, and the folder, or `None` if the person backed out.
    answer: Option<(String, Option<String>)>,
    /// A picker whose waiter gave up while the system's picker was still open: it still has an answer coming, which belongs to
    /// nobody. It is kept so that a question asked in the meantime is not given that late answer, and is cleared by it.
    draining: bool,
    /// The tree each accepted transfer is to be put in when it arrives whole, by transfer.
    trees: std::collections::BTreeMap<String, String>,
    /// How each delivery went since the interface last asked: the transfer, and whether the file reached the folder.
    delivered: Vec<(String, bool)>,
}

/// What the interface takes when it asks.
#[derive(Debug, PartialEq, Eq)]
pub enum Taken {
    /// The picker is open, or was never opened: nothing to take yet.
    Waiting,
    /// The person answered for this transfer: with a folder, or with `None` for backing out.
    Answered { transfer: String, folder: Option<String> },
}

/// A received file that arrived whole and is to be put in the folder the person chose for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    /// The transfer it came of.
    pub transfer: String,
    /// Where the program wrote it, in its own staging.
    pub path: std::path::PathBuf,
    /// What it is called.
    pub name: String,
    /// Validated directory components beneath the destination tree.
    pub folders: Vec<String>,
    /// The folder, as the platform gave it: text the program never reads as a path.
    pub tree: String,
}

impl FolderPicked {
    /// The interface is opening the picker for `transfer`. Returns whether it may: the system shows ONE folder picker at a time, and
    /// the picker is busy from the question until its answer has been TAKEN by the one who asked. A second question in that time
    /// would take the place of the first, or erase an answer its waiter has not yet read, so that the first file would never learn
    /// its folder. It is refused instead (`false`), and the caller says that a folder is being chosen already.
    pub fn opened(&self, transfer: &str) -> bool {
        let Ok(mut state) = self.inner.lock() else { return false };
        if state.asked_for.is_some() || state.answer.is_some() || state.draining {
            return false;
        }
        state.asked_for = Some(transfer.to_owned());
        true
    }

    /// The person chose `folder`, or backed out (`None`). Called by the platform layer, on its own thread. Kept only if the
    /// picker was asked for; an answer nobody asked for is dropped.
    pub fn answered(&self, folder: Option<String>) {
        let Ok(mut state) = self.inner.lock() else { return };
        if state.draining {
            // The answer of a picker whose waiter gave up: it is for nobody, and it is the end of that picker.
            state.draining = false;
            return;
        }
        if let Some(transfer) = state.asked_for.take() {
            state.answer = Some((transfer, folder));
        }
    }

    /// The answer, taken once and only if it was made for `transfer`: the interface asks again and again while the picker is open,
    /// and an answer for another file stays where it is for that file's own waiter.
    pub fn take_for(&self, transfer: &str) -> Taken {
        let Ok(mut state) = self.inner.lock() else { return Taken::Waiting };
        if state.answer.as_ref().is_some_and(|(asked, _)| asked == transfer)
            && let Some((transfer, folder)) = state.answer.take()
        {
            return Taken::Answered { transfer, folder };
        }
        Taken::Waiting
    }

    /// The waiter for `transfer` gave up (the picker never answered, or nobody waits for the answer any more). An answer already
    /// made for it is let go of. A question still waiting on the SYSTEM's picker is not freed outright: that picker is still open
    /// and will answer, and the answer must not be taken for the next question, so the picker stays busy ("draining") until that
    /// answer comes and is dropped. What belongs to another transfer is left alone.
    pub fn abandon(&self, transfer: &str) {
        let Ok(mut state) = self.inner.lock() else { return };
        if state.asked_for.as_deref() == Some(transfer) {
            state.asked_for = None;
            state.draining = true;
        }
        if state.answer.as_ref().is_some_and(|(asked, _)| asked == transfer) {
            state.answer = None;
        }
    }

    /// `transfer` was accepted to go to `tree`: remembered until it arrives whole and is put there.
    pub fn destined(&self, transfer: &str, tree: String) {
        if let Ok(mut state) = self.inner.lock() {
            state.trees.insert(transfer.to_owned(), tree);
        }
    }

    /// The folder `transfer` goes to, taken once: asked when it is complete and is to be handed over. A transfer that went to
    /// no tree (a path the program can write) has none.
    pub fn tree_for(&self, transfer: &str) -> Option<String> {
        self.inner.lock().ok()?.trees.remove(transfer)
    }

    /// A transfer that was not completed (refused, cancelled, failed) goes nowhere: what was remembered for it is let go of.
    pub fn forget(&self, transfer: &str) {
        if let Ok(mut state) = self.inner.lock() {
            state.trees.remove(transfer);
        }
    }

    /// A file was put in the folder (`true`) or could not be (`false`). Called by the platform layer, on its own thread.
    pub fn delivered(&self, transfer: String, done: bool) {
        if let Ok(mut state) = self.inner.lock() {
            state.delivered.push((transfer, done));
        }
    }

    /// The deliveries that finished, taken once each.
    pub fn take_delivered(&self) -> Vec<(String, bool)> {
        self.inner.lock().map(|mut state| std::mem::take(&mut state.delivered)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_taken_once_and_for_the_transfer_it_was_asked_for() {
        let picked = FolderPicked::default();
        assert_eq!(picked.take_for("one"), Taken::Waiting, "nothing was asked, so nothing is there");
        assert!(picked.opened("one"));
        assert_eq!(picked.take_for("one"), Taken::Waiting, "asked, not yet answered");
        picked.answered(Some("tree-1".into()));
        assert_eq!(picked.take_for("one"), Taken::Answered { transfer: "one".into(), folder: Some("tree-1".into()) });
        assert_eq!(picked.take_for("one"), Taken::Waiting, "given once");
    }

    #[test]
    fn backing_out_is_an_answer_and_is_not_a_folder() {
        let picked = FolderPicked::default();
        assert!(picked.opened("one"));
        picked.answered(None);
        assert_eq!(picked.take_for("one"), Taken::Answered { transfer: "one".into(), folder: None });
    }

    #[test]
    fn an_answer_nobody_asked_for_is_dropped() {
        let picked = FolderPicked::default();
        picked.answered(Some("stray".into()));
        assert_eq!(picked.take_for("one"), Taken::Waiting);
        // And one answer cannot serve two questions: the second arrives with nothing asked.
        assert!(picked.opened("one"));
        picked.answered(Some("a".into()));
        picked.answered(Some("b".into()));
        assert_eq!(picked.take_for("one"), Taken::Answered { transfer: "one".into(), folder: Some("a".into()) });
    }

    #[test]
    fn a_second_question_is_refused_while_one_is_open_so_no_answer_is_taken_for_the_wrong_file() {
        let picked = FolderPicked::default();
        assert!(picked.opened("one"));
        assert!(!picked.opened("two"), "a second question took the place of the first");
        picked.answered(Some("for-one".into()));
        // The answer belongs to "one": "two" must not be able to take it, and "one" still gets it.
        assert_eq!(picked.take_for("two"), Taken::Waiting, "an answer was taken for the file it was not made for");
        assert_eq!(picked.take_for("one"), Taken::Answered { transfer: "one".into(), folder: Some("for-one".into()) });
    }

    #[test]
    fn after_an_answer_is_taken_the_next_question_may_be_asked() {
        let picked = FolderPicked::default();
        assert!(picked.opened("one"));
        picked.answered(Some("a".into()));
        assert!(picked.take_for("one") != Taken::Waiting);
        assert!(picked.opened("two"), "a finished question still blocked the next");
    }

    #[test]
    fn an_answer_that_was_not_yet_read_is_kept_against_the_next_question_until_it_is_read_or_given_up() {
        let picked = FolderPicked::default();
        assert!(picked.opened("a"));
        picked.answered(Some("tree-a".into()));
        // The person has answered, its waiter has not looked yet: a second Accept must not erase the answer.
        assert!(!picked.opened("b"), "a second question was allowed while the first answer was unread");
        assert_eq!(
            picked.take_for("a"),
            Taken::Answered { transfer: "a".into(), folder: Some("tree-a".into()) },
            "the first file lost its folder to the second question"
        );
        assert!(picked.opened("b"), "reading the answer did not free the picker");
        // And an answer that nobody will read is let go of by the one it was made for, which frees the picker.
        picked.answered(Some("tree-b".into()));
        assert!(!picked.opened("c"));
        picked.abandon("c");
        assert!(!picked.opened("c"), "another file's giving up freed an answer that was not its own");
        picked.abandon("b");
        assert!(picked.opened("c"), "an answer nobody read blocked every later question");
    }

    #[test]
    fn giving_up_on_a_question_only_the_one_who_asked_may_do_and_the_picker_stays_busy_until_the_system_answers() {
        let picked = FolderPicked::default();
        assert!(picked.opened("one"));
        picked.abandon("two");
        assert!(!picked.opened("three"), "another file's giving up freed a question that was not its own");
        picked.abandon("one");
        // The system's picker is still open and will answer: until it does, nothing may be asked.
        assert!(!picked.opened("three"), "a picker that was given up on but is still open was handed to another question");
        picked.answered(Some("late".into()));
        assert!(picked.opened("three"), "the late answer of a given-up picker did not end it");
    }

    #[test]
    fn the_late_answer_of_a_picker_that_was_given_up_is_never_taken_for_the_question_asked_after_it() {
        let picked = FolderPicked::default();
        assert!(picked.opened("a"));
        picked.abandon("a");
        // B cannot be asked while A's picker is still open, so A's answer cannot be filed under B.
        assert!(!picked.opened("b"));
        picked.answered(Some("folder-a".into()));
        assert_eq!(picked.take_for("a"), Taken::Waiting, "the late answer was kept for the file that gave up");
        assert!(picked.opened("b"));
        assert_eq!(picked.take_for("b"), Taken::Waiting, "B was given the answer that A's picker made");
        picked.answered(Some("folder-b".into()));
        assert_eq!(picked.take_for("b"), Taken::Answered { transfer: "b".into(), folder: Some("folder-b".into()) });
    }

    #[test]
    fn the_folder_of_a_transfer_is_remembered_for_that_transfer_and_given_once() {
        let picked = FolderPicked::default();
        picked.destined("one", "tree-1".into());
        picked.destined("two", "tree-2".into());
        assert_eq!(picked.tree_for("one"), Some("tree-1".into()));
        assert_eq!(picked.tree_for("one"), None, "a transfer is handed over once");
        assert_eq!(picked.tree_for("two"), Some("tree-2".into()), "another transfer keeps its own folder");
        assert_eq!(picked.tree_for("never"), None, "a transfer that went to a path has no tree");
    }

    #[test]
    fn a_transfer_that_does_not_finish_is_let_go_of() {
        let picked = FolderPicked::default();
        picked.destined("one", "tree-1".into());
        picked.forget("one");
        assert_eq!(picked.tree_for("one"), None);
    }

    #[test]
    fn deliveries_are_taken_once_each_with_how_they_went() {
        let picked = FolderPicked::default();
        picked.delivered("one".into(), true);
        picked.delivered("two".into(), false);
        assert_eq!(picked.take_delivered(), vec![("one".to_owned(), true), ("two".to_owned(), false)]);
        assert!(picked.take_delivered().is_empty());
    }
}
