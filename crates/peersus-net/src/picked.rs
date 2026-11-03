//! What a platform's own file picker hands over, turned into files the owner can send.
//!
//! The picker belongs to the platform (on Android, the system's document picker, started from Java), and it answers
//! when the person has chosen, not when it was asked. The answer comes as events on a thread that is not the
//! interface's: one for each file chosen, an open descriptor and the name the provider gave it, and then one that
//! says the picker is done. Everything that decides what happens to them is here, with no platform code, so that it
//! is tried without a phone; the layer that talks to the platform only passes the events in.
//!
//! WHAT IS DECIDED. A file is kept in [`Held`] and what is said is its number, so that it can be sent by number
//! and never copied or opened twice. A picker that is not asked for does not fill the registry: files that arrive
//! when none was wanted are closed. The name is made acceptable by the same rule as any other (see
//! `outgoing::safe_name` in the interface); this module only passes it on.

use crate::held::Held;
use std::{
    fs::File,
    sync::{Arc, Mutex},
};

/// What the interface asked of the picker and what came back, kept together so that they cannot disagree.
#[derive(Clone, Default)]
pub struct Picked {
    held: Held,
    inner: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    /// The picker was opened and has not said it is done.
    waiting: bool,
    /// The files that arrived since the interface last took them, by the number each is held under, with the name.
    arrived: Vec<(u64, String)>,
    /// How many files were turned away (the registry was full) since the interface last asked.
    refused: u32,
    generation: u64,
    streaming: bool,
    expected: Option<u32>,
    received: u32,
    /// A selection that came from outside the interface (another application sharing files) and that the interface has not yet
    /// been told about. Its generation, taken once by [`Picked::take_external`].
    external: Option<u64>,
}

/// What the interface takes when it asks: the files, and whether the picker is still open.
#[derive(Debug, PartialEq, Eq)]
pub struct Taken {
    /// Each file chosen, as the number it is held under and the name the provider gave it.
    pub files: Vec<(u64, String)>,
    /// The picker is still open: more may come, and the interface should not treat what it has as everything.
    pub still_open: bool,
    /// How many files were not kept because too many were already held.
    pub refused: u32,
}

impl Picked {
    /// A picker that keeps its files in `held`, the same registry the owner takes them from.
    pub fn new(held: Held) -> Self {
        Self {
            held,
            inner: Arc::default(),
        }
    }

    /// Begin a selection: whatever an earlier one left untaken is let go of, so that an old choice cannot be sent along with a new one,
    /// and a notice of an earlier selection from outside is dropped, for it points at a generation that this one has replaced. One
    /// transition under the caller's lock, so that no other opener can come between the check and the change.
    fn begin(&self, state: &mut State, streaming: bool) {
        for (number, _) in state.arrived.drain(..) {
            self.held.release(number);
        }
        state.refused = 0;
        state.expected = None;
        state.received = 0;
        state.generation = state.generation.wrapping_add(1);
        state.streaming = streaming;
        state.external = None;
        state.waiting = true;
    }

    /// The interface is opening the picker. Anything left from an earlier one that was never taken is let go of
    /// first, so that an old choice cannot be sent along with a new one.
    pub fn opened(&self) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        self.begin(&mut state, false);
    }

    /// Paired sends consume descriptors as they arrive, so a producer can wait for registry space.
    pub fn opened_streaming(&self) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        self.begin(&mut state, true);
    }

    /// Files arrive from outside the interface (another application shared them): open a selection for them, as the interface does when
    /// a person asks for the picker, and leave a notice for the interface to take (see [`Picked::take_external`]). Returns the
    /// generation the files are to be sent under, or `None` when a picker is already open or is being waited on, or an earlier share has
    /// not been taken yet: then what is there is the person's or is waiting for them, and a share is never allowed to replace it or be
    /// mixed into it. The caller refuses the share. Checked and begun under one lock, so a picker the person opens at the same moment
    /// either comes first (and this refuses) or after (and replaces this, as any newer choice does).
    pub fn opened_externally(&self) -> Option<u64> {
        let mut state = self.inner.lock().ok()?;
        if state.waiting || state.external.is_some() {
            return None;
        }
        self.begin(&mut state, true);
        let generation = state.generation;
        state.external = Some(generation);
        Some(generation)
    }

    /// The generation of a selection that came from outside and that nobody has been told about yet, taken once. What the interface asks
    /// as it draws, to start what it does with files the person has already chosen (a code, or a device).
    pub fn take_external(&self) -> Option<u64> {
        self.inner.lock().ok()?.external.take()
    }

    /// Whether files shared from outside are waiting for the interface to be told, without telling it. What the window asks to bring the
    /// page that handles them forward: the page itself takes the notice (see [`Picked::take_external`]), so that it is taken once and by
    /// the one that does something with it.
    pub fn has_external(&self) -> bool {
        self.inner
            .lock()
            .is_ok_and(|state| state.external.is_some())
    }

    pub fn generation(&self) -> u64 {
        self.inner.lock().map_or(0, |state| state.generation)
    }

    /// -1 cancels an obsolete producer; 0 applies backpressure; 1 permits another provider open.
    pub fn producer_state(&self, generation: u64) -> i32 {
        let Ok(state) = self.inner.lock() else {
            return -1;
        };
        if !state.waiting || state.generation != generation {
            -1
        } else if state.streaming && self.held.len() >= crate::held::MOST_HELD {
            0
        } else {
            1
        }
    }

    /// The native picker knows the complete selection before opening its first file.
    pub fn selected_for(&self, generation: u64, count: u32) {
        if let Ok(mut state) = self.inner.lock()
            && state.waiting
            && state.generation == generation
            && state.expected.is_none()
        {
            state.expected = Some(count);
        }
    }

    pub fn selected_count(&self, generation: u64) -> Option<u32> {
        self.inner
            .lock()
            .ok()
            .filter(|state| state.generation == generation)
            .and_then(|state| state.expected)
    }

    /// A file was chosen: kept, unless the picker was not asked for (then it is closed, since nobody will take it)
    /// or the registry is full (then it is closed and counted). Called by the platform layer, on its own thread.
    pub fn file(&self, file: File, name: String) {
        self.file_for(self.generation(), file, name);
    }

    /// A late result from a replaced picker is closed, never attached to the newer choice.
    pub fn file_for(&self, generation: u64, file: File, name: String) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        if !state.waiting || state.generation != generation {
            return;
        }
        state.received = state.received.saturating_add(1);
        match self.held.keep(file) {
            Ok(number) => state.arrived.push((number, name)),
            Err(_closed) => state.refused = state.refused.saturating_add(1),
        }
    }

    /// The picker is done, with files or without. Called by the platform layer.
    pub fn finished(&self) {
        self.finished_for(self.generation());
    }

    pub fn finished_for(&self, generation: u64) {
        if let Ok(mut state) = self.inner.lock()
            && state.generation == generation
            && state.waiting
        {
            if let Some(expected) = state.expected {
                state.refused = state
                    .refused
                    .saturating_add(expected.saturating_sub(state.received));
            }
            state.waiting = false;
        }
    }

    /// What has arrived, taken: the interface asks again and again while the picker is open, and each file is
    /// given once. The files are still in the registry; taking them here only means that they were seen.
    pub fn take(&self) -> Taken {
        self.take_for(self.generation())
    }

    /// A watcher for a replaced picker must not consume the new picker's files.
    pub fn take_for(&self, generation: u64) -> Taken {
        let Ok(mut state) = self.inner.lock() else {
            return Taken {
                files: Vec::new(),
                still_open: false,
                refused: 0,
            };
        };
        if state.generation != generation {
            return Taken {
                files: Vec::new(),
                still_open: false,
                refused: 0,
            };
        }
        Taken {
            files: std::mem::take(&mut state.arrived),
            still_open: state.waiting,
            refused: std::mem::take(&mut state.refused),
        }
    }

    /// Let go of files that were taken and then not sent (the page was left, or the choice was replaced): each is
    /// closed. Files that were taken are no longer in the picker's own list, so `opened` cannot reach them: without
    /// this, every page that was abandoned with files waiting would leave them open, until the registry was full and
    /// the picker turned every new file away for a reason nobody could see. A number that is not held is nothing.
    pub fn release(&self, files: &[(u64, String)]) {
        for (number, _) in files {
            self.held.release(*number);
        }
    }

    /// Another set of the same files, each held under a new number with a read position of its own, for a second device to be
    /// sent them: the first set's numbers are taken by the first send, and a handle that was only duplicated would share its
    /// position with it. All or none: when a file cannot be held again (the registry is full, or the system will not open it
    /// again) the copies already made are let go of and the reason is returned, so that no device is sent part of a choice and no
    /// descriptor is left open for a send that did not happen.
    pub fn reopened(&self, files: &[(u64, String)]) -> Result<Vec<(u64, String)>, String> {
        let mut made: Vec<(u64, String)> = Vec::with_capacity(files.len());
        for (number, name) in files {
            match self.held.reopen(*number) {
                Ok(copy) => made.push((copy, name.clone())),
                Err(reason) => {
                    self.release(&made);
                    return Err(reason);
                }
            }
        }
        Ok(made)
    }

    /// A file that was chosen, taken out of the registry by the number it was told under, to be read by the interface
    /// itself (a profile picture is decoded from it, where a file to send is sent from where it is held). It leaves
    /// the registry, so the file is closed when the caller drops it; a number that is not held, or was taken, is None.
    pub fn take_file(&self, number: u64) -> Option<File> {
        self.held.take(number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::held::MOST_HELD;
    use std::io::Write;

    fn pathless(bytes: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        std::io::Seek::rewind(&mut file).unwrap();
        file
    }

    #[test]
    fn selection_count_is_fenced_and_unreadable_choices_are_reported_once() {
        let picked = Picked::new(Held::default());
        picked.opened_streaming();
        let old = picked.generation();
        picked.selected_for(old, 80);
        assert_eq!(picked.selected_count(old), Some(80));
        picked.file_for(old, pathless(b"one readable choice"), "one.bin".into());
        picked.finished_for(old);
        picked.finished_for(old);
        let taken = picked.take_for(old);
        assert_eq!(taken.files.len(), 1);
        assert_eq!(taken.refused, 79);
        picked.release(&taken.files);
        picked.opened_streaming();
        let current = picked.generation();
        picked.selected_for(old, 999);
        assert_eq!(picked.selected_count(current), None);
        picked.selected_for(current, 5);
        assert_eq!(picked.selected_count(current), Some(5));
        assert_eq!(picked.selected_count(old), None);
    }

    #[test]
    fn looking_for_a_share_does_not_take_it_so_the_page_that_handles_it_still_gets_it_once() {
        let picked = Picked::new(Held::default());
        assert!(!picked.has_external());
        let generation = picked.opened_externally().unwrap();
        for _ in 0..3 {
            assert!(picked.has_external(), "looking took the notice");
        }
        assert_eq!(picked.take_external(), Some(generation));
        assert!(!picked.has_external());
        assert_eq!(picked.take_external(), None);
    }

    #[test]
    fn files_shared_from_outside_open_a_selection_and_leave_one_notice_for_the_interface() {
        let picked = Picked::new(Held::default());
        let generation = picked
            .opened_externally()
            .expect("nothing was open, so the share is taken");
        assert_eq!(generation, picked.generation());
        assert_eq!(picked.take_external(), Some(generation));
        assert_eq!(picked.take_external(), None, "the notice was given twice");
        // The files then arrive exactly as a picker's do.
        picked.selected_for(generation, 2);
        picked.file_for(generation, pathless(b"one"), "one.bin".into());
        picked.file_for(generation, pathless(b"two"), "two.bin".into());
        picked.finished_for(generation);
        let taken = picked.take_for(generation);
        assert_eq!(
            taken
                .files
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>(),
            ["one.bin", "two.bin"]
        );
        assert_eq!(taken.refused, 0);
        assert!(!taken.still_open);
    }

    #[test]
    fn a_share_never_replaces_or_joins_a_picker_the_person_has_open() {
        let picked = Picked::new(Held::default());
        picked.opened_streaming();
        let theirs = picked.generation();
        picked.file_for(
            theirs,
            pathless(b"the person's own choice"),
            "mine.bin".into(),
        );
        assert_eq!(
            picked.opened_externally(),
            None,
            "a share was taken while a picker was open"
        );
        assert_eq!(picked.generation(), theirs, "the open picker was replaced");
        assert_eq!(picked.take_external(), None);
        let kept = picked.take_for(theirs);
        assert_eq!(kept.files.len(), 1, "the person's own choice was dropped");
        assert!(kept.still_open);
    }

    #[test]
    fn a_second_share_waits_until_the_first_has_been_taken() {
        let picked = Picked::new(Held::default());
        let first = picked.opened_externally().unwrap();
        picked.finished_for(first);
        assert_eq!(
            picked.opened_externally(),
            None,
            "a second share replaced one nobody had been told about"
        );
        assert_eq!(picked.take_external(), Some(first));
        let second = picked
            .opened_externally()
            .expect("the first was taken, so the next share is");
        assert_ne!(second, first);
    }

    #[test]
    fn a_picker_the_person_opens_drops_the_notice_of_a_share_it_replaces_and_lets_go_of_its_files()
    {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        let shared = picked.opened_externally().unwrap();
        picked.file_for(shared, pathless(b"shared"), "shared.bin".into());
        picked.finished_for(shared);
        picked.opened_streaming();
        assert_eq!(
            picked.take_external(),
            None,
            "a notice pointed at a generation that was replaced"
        );
        assert!(
            held.is_empty(),
            "a shared file the person never saw was left open"
        );
    }

    #[test]
    fn openers_racing_for_one_picker_never_both_win_the_same_generation() {
        // A share and a person's picker opened from two threads at once: whichever way it falls, a share that was
        // taken holds a generation of its own, and the picker's is a later one that replaced it.
        for _ in 0..200 {
            let picked = Picked::new(Held::default());
            let (for_share, for_person) = (picked.clone(), picked.clone());
            let share = std::thread::spawn(move || for_share.opened_externally());
            let person = std::thread::spawn(move || {
                for_person.opened_streaming();
                for_person.generation()
            });
            let shared = share.join().unwrap();
            let theirs = person.join().unwrap();
            if let Some(shared) = shared {
                assert_ne!(
                    shared, theirs,
                    "a share and a picker were given the same selection"
                );
            }
        }
    }

    #[test]
    fn streaming_picker_waits_for_descriptor_space_and_resumes_without_losing_a_choice() {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        picked.opened_streaming();
        let generation = picked.generation();
        for number in 0..MOST_HELD {
            assert_eq!(picked.producer_state(generation), 1);
            picked.file_for(generation, pathless(b"content"), format!("{number}"));
        }
        assert_eq!(picked.producer_state(generation), 0);
        let first = picked.take();
        for (number, _) in &first.files {
            assert!(held.take(*number).is_some());
        }
        assert_eq!(picked.producer_state(generation), 1);
        picked.file_for(generation, pathless(b"last"), "last".into());
        picked.finished_for(generation);
        let last = picked.take();
        assert_eq!(
            last.files
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>(),
            ["last"]
        );
        assert_eq!(first.refused + last.refused, 0);
        assert!(!last.still_open);
        assert_eq!(picked.producer_state(generation), -1);
    }

    #[test]
    fn replaced_picker_rejects_late_files_and_completion_without_closing_the_new_picker() {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        picked.opened_streaming();
        let old = picked.generation();
        picked.opened_streaming();
        let current = picked.generation();
        assert_ne!(old, current);
        assert_eq!(picked.producer_state(old), -1);
        picked.file_for(old, pathless(b"obsolete"), "obsolete".into());
        picked.finished_for(old);
        assert!(held.is_empty());
        assert!(picked.take().still_open);
        picked.file_for(current, pathless(b"current"), "current".into());
        picked.finished_for(current);
        let result = picked.take();
        assert_eq!(
            result
                .files
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>(),
            ["current"]
        );
        assert!(!result.still_open);
    }

    #[test]
    fn files_chosen_while_the_picker_is_open_are_held_and_told_by_number_with_their_names() {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        picked.opened();
        picked.file(pathless(b"a"), "a.jpg".into());
        picked.file(pathless(b"b"), "b.pdf".into());
        let first = picked.take();
        assert_eq!(first.files.len(), 2);
        assert_eq!(first.files[0].1, "a.jpg");
        assert_eq!(first.files[1].1, "b.pdf");
        assert!(
            first.still_open,
            "the picker was said to be done before it said so"
        );
        // Both are in the registry, by the numbers that were told, and the numbers differ.
        assert_ne!(first.files[0].0, first.files[1].0);
        assert_eq!(held.len(), 2);
        assert!(held.take(first.files[0].0).is_some());
        // Given once: asking again finds nothing new.
        assert!(picked.take().files.is_empty());
        picked.finished();
        assert!(!picked.take().still_open);
    }

    #[test]
    fn a_chosen_file_is_taken_out_of_the_registry_by_its_number_once() {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        picked.opened();
        picked.file(pathless(b"the picture"), "me.png".into());
        let number = picked.take().files[0].0;
        let mut file = picked
            .take_file(number)
            .expect("a chosen file was not given back");
        let mut text = String::new();
        std::io::Read::read_to_string(&mut file, &mut text).unwrap();
        assert_eq!(text, "the picture");
        // It left the registry, so none is held and a second ask finds nothing.
        assert!(
            held.is_empty(),
            "the file stayed held after it was taken out"
        );
        assert!(picked.take_file(number).is_none());
        // A number that was never held is nothing.
        assert!(picked.take_file(9_999).is_none());
    }

    #[test]
    fn a_picker_that_was_backed_out_of_says_it_is_done_with_no_files() {
        let picked = Picked::new(Held::default());
        picked.opened();
        picked.finished();
        let taken = picked.take();
        assert!(taken.files.is_empty());
        assert!(
            !taken.still_open,
            "the interface would wait for ever on a picker that was cancelled"
        );
    }

    #[test]
    fn a_second_set_of_the_chosen_files_is_read_from_the_start_whatever_the_first_has_read() {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        picked.opened();
        picked.file(pathless(b"0123456789"), "a.bin".into());
        let first = picked.take().files;
        let second = picked
            .reopened(&first)
            .expect("the chosen files could not be held again");
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].0, second[0].0);
        assert_eq!(second[0].1, "a.bin");
        let mut one = held.take(first[0].0).unwrap();
        let mut head = [0_u8; 4];
        std::io::Read::read_exact(&mut one, &mut head).unwrap();
        let mut two = held.take(second[0].0).unwrap();
        let mut all = String::new();
        std::io::Read::read_to_string(&mut two, &mut all).unwrap();
        assert_eq!(all, "0123456789");
    }

    #[test]
    fn when_one_file_cannot_be_held_again_none_is_left_open_and_no_part_of_a_set_is_given() {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        picked.opened();
        // Room for exactly one copy more than is held: the set of two cannot be made.
        for index in 0..(MOST_HELD - 1) {
            picked.file(pathless(b"x"), format!("{index}"));
        }
        let files = picked.take().files;
        let before = held.len();
        assert_eq!(before, MOST_HELD - 1);
        assert!(picked.reopened(&files).is_err());
        assert_eq!(
            held.len(),
            before,
            "copies made before the one that failed were left open"
        );
    }

    #[test]
    fn a_file_that_arrives_when_no_picker_was_asked_for_is_closed_and_not_held() {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        // A late answer, or one from a picker the interface did not open: nobody will take it.
        picked.file(pathless(b"stray"), "stray.bin".into());
        assert!(held.is_empty(), "a file nobody asked for was kept open");
        assert!(picked.take().files.is_empty());
        // And after the picker is done, a straggler is the same.
        picked.opened();
        picked.finished();
        picked.file(pathless(b"late"), "late.bin".into());
        assert!(held.is_empty());
    }

    #[test]
    fn opening_the_picker_again_lets_go_of_what_the_last_one_left_so_an_old_choice_is_not_sent_with_a_new_one()
     {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        picked.opened();
        picked.file(pathless(b"old"), "old.bin".into());
        picked.finished();
        // Never taken. The person opens the picker again.
        picked.opened();
        assert!(
            held.is_empty(),
            "the old file was still held when a new picker was opened"
        );
        picked.file(pathless(b"new"), "new.bin".into());
        let taken = picked.take();
        assert_eq!(taken.files.len(), 1);
        assert_eq!(taken.files[0].1, "new.bin");
    }

    #[test]
    fn files_past_the_most_held_are_closed_and_counted_and_the_others_still_arrive() {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        picked.opened();
        for n in 0..MOST_HELD + 3 {
            picked.file(pathless(b"x"), format!("f{n}.bin"));
        }
        let taken = picked.take();
        assert_eq!(taken.files.len(), MOST_HELD);
        assert_eq!(
            taken.refused, 3,
            "the files that did not fit were not counted"
        );
        assert_eq!(held.len(), MOST_HELD);
        // Said once: asking again does not say it again.
        assert_eq!(picked.take().refused, 0);
    }

    #[test]
    fn files_taken_and_then_abandoned_do_not_fill_the_registry_once_they_are_released() {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        // A person opens Send, picks files with no device chosen, leaves the page, and does it again, many times.
        // Without the release, each round would leave its files open: the registry fills, and the picker then refuses
        // every new file for a reason nobody can see.
        for _ in 0..MOST_HELD * 2 {
            picked.opened();
            picked.file(pathless(b"a"), "a.bin".into());
            picked.file(pathless(b"b"), "b.bin".into());
            picked.finished();
            let taken = picked.take();
            assert_eq!(
                taken.refused, 0,
                "the registry was full of files nobody would send"
            );
            assert_eq!(taken.files.len(), 2);
            // The page was left with these waiting: they are let go of.
            picked.release(&taken.files);
        }
        assert!(held.is_empty(), "files that were abandoned were still open");
        // And a picker opened after all that still has room.
        picked.opened();
        picked.file(pathless(b"fresh"), "fresh.bin".into());
        assert_eq!(picked.take().files.len(), 1);
    }

    #[test]
    fn releasing_files_that_are_not_held_is_nothing_and_does_not_touch_the_ones_that_are() {
        let held = Held::default();
        let picked = Picked::new(held.clone());
        picked.opened();
        picked.file(pathless(b"keep"), "keep.bin".into());
        let kept = picked.take().files;
        picked.release(&[(999_999, "never.bin".into())]);
        assert_eq!(
            held.len(),
            1,
            "releasing a number that was never given let go of another file"
        );
        picked.release(&kept);
        picked.release(&kept);
        assert!(held.is_empty());
    }
}
