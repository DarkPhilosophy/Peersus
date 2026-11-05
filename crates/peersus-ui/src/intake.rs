//! The one rule for files that arrive to be sent, whichever way they arrived: chosen in the
//! operating system's dialog, dropped on the window, or pasted from the clipboard. All three give a
//! list of paths, and all three go through [`accept`], so a file is judged the same whatever
//! brought it and no way in has a rule of its own.
//!
//! Nothing here reads a file's contents or sends anything: it decides which of the paths offered
//! are worth handing on, and says why the others are not, so that the person is told.

use std::path::{Path, PathBuf};

/// The most files taken at once. A drop of a whole folder tree's worth of paths, or a paste of a
/// selection of thousands, is far more than anyone means to send in one go.
pub const MOST_AT_ONCE: usize = 500;

/// Why a path offered was not taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Left {
    /// It is not a file: a folder, or something that is neither.
    NotAFile(PathBuf),
    /// It could not be looked at: it is gone, or this program may not read it.
    Unreadable(PathBuf),
    /// It is empty: there is nothing to send.
    Empty(PathBuf),
    /// It was offered twice.
    Twice(PathBuf),
    /// There were more than [`MOST_AT_ONCE`].
    TooMany(PathBuf),
}

/// What came of a list of paths: the ones to send, in the order given, and the ones that were left
/// out with the reason for each.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Intake {
    pub taken: Vec<PathBuf>,
    pub left: Vec<Left>,
}

impl Intake {
    /// Whether anything is worth sending.
    pub fn is_empty(&self) -> bool {
        self.taken.is_empty()
    }
}

/// What is known about a path, as little as the rule needs. Taken apart from the file system so
/// that the rule can be tried on any combination without making files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File { length: u64 },
    Folder,
    Other,
    Unreadable,
}

/// The kind of a path, from the file system. A link is followed, because a person who drops a link
/// to a file means the file.
pub fn kind_of(path: &Path) -> Kind {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => Kind::File { length: meta.len() },
        Ok(meta) if meta.is_dir() => Kind::Folder,
        Ok(_) => Kind::Other,
        Err(_) => Kind::Unreadable,
    }
}

/// Judge `paths` using `kind` to look at each. The order is kept, a path offered twice is taken
/// once (the first time), and the number taken is never more than [`MOST_AT_ONCE`].
pub fn accept_with(
    paths: impl IntoIterator<Item = PathBuf>,
    kind: impl Fn(&Path) -> Kind,
) -> Intake {
    let mut intake = Intake::default();
    for path in paths {
        if intake.taken.contains(&path) {
            intake.left.push(Left::Twice(path));
            continue;
        }
        match kind(&path) {
            Kind::File { length: 0 } => intake.left.push(Left::Empty(path)),
            Kind::File { .. } if intake.taken.len() >= MOST_AT_ONCE => {
                intake.left.push(Left::TooMany(path))
            }
            Kind::File { .. } => intake.taken.push(path),
            Kind::Folder | Kind::Other => intake.left.push(Left::NotAFile(path)),
            Kind::Unreadable => intake.left.push(Left::Unreadable(path)),
        }
    }
    intake
}

/// [`accept_with`] on the real file system.
pub fn accept(paths: impl IntoIterator<Item = PathBuf>) -> Intake {
    accept_with(paths, kind_of)
}

/// Counts only: notices can be redrawn in another language without retaining rejected paths.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary([usize; 4]);

impl From<&Intake> for Summary {
    fn from(intake: &Intake) -> Self {
        let mut counts = [0; 4];
        for reason in &intake.left {
            let index = match reason {
                Left::NotAFile(_) => 0,
                Left::Unreadable(_) => 1,
                Left::Empty(_) => 2,
                Left::TooMany(_) => 3,
                Left::Twice(_) => continue,
            };
            counts[index] += 1;
        }
        Self(counts)
    }
}

impl Summary {
    pub fn words_in(self, language: crate::i18n::Language) -> Option<String> {
        use crate::i18n::{self, Language};
        let mut parts = Vec::new();
        for (index, count) in self
            .0
            .into_iter()
            .enumerate()
            .filter(|(_, count)| *count > 0)
        {
            let sentence = if language == Language::English {
                match index {
                    0 => format!(
                        "{count} {} not a file",
                        if count == 1 { "is" } else { "are" }
                    ),
                    1 => format!("{count} could not be read"),
                    2 => format!("{count} {} empty", if count == 1 { "is" } else { "are" }),
                    _ => format!("{count} more than {MOST_AT_ONCE} at once"),
                }
            } else {
                let template = [
                    "Not files: {count}",
                    "Could not be read: {count}",
                    "Empty files: {count}",
                    "Beyond the {limit}-file selection limit: {count}",
                ][index];
                i18n::format(
                    language,
                    template,
                    &[
                        ("count", &count.to_string()),
                        ("limit", &MOST_AT_ONCE.to_string()),
                    ],
                )
            };
            parts.push(sentence);
        }
        if parts.is_empty() {
            return None;
        }
        Some(i18n::format(
            language,
            "Left out: {items}.",
            &[("items", &parts.join(", "))],
        ))
    }
}

/// English compatibility entry point for callers without interface language state.
pub fn words(intake: &Intake) -> Option<String> {
    Summary::from(intake).words_in(crate::i18n::Language::English)
}

/// The files a person pasted: the list of files that a file manager put on the clipboard, and
/// nothing else. A file manager puts them there as `text/uri-list`, which the clipboard reader
/// gives as a list of paths on every platform; text is a different thing and is never read as a
/// path, whatever it says. A web page or another program can put the address of a private file on
/// the clipboard as text, and pasting must not send it: taking a file the person did not choose is
/// not a convenience that can be allowed.
pub fn pasted(files: Option<Vec<PathBuf>>) -> Vec<PathBuf> {
    files.unwrap_or_default()
}

/// Whether a key press is the command to paste: V with control (Windows and Linux) or with the
/// command key (macOS). Shift does not matter; Alt turns it into another command, so it does not
/// count.
pub fn is_paste(code: freya::prelude::Code, modifiers: freya::prelude::Modifiers) -> bool {
    use freya::prelude::Modifiers;
    let control_or_command =
        modifiers.contains(Modifiers::CONTROL) || modifiers.contains(Modifiers::META);
    code == freya::prelude::Code::KeyV && control_or_command && !modifiers.contains(Modifiers::ALT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> PathBuf {
        PathBuf::from(text)
    }

    fn by_name(path: &Path) -> Kind {
        match path.to_str().unwrap() {
            name if name.ends_with('/') => Kind::Folder,
            name if name.contains("gone") => Kind::Unreadable,
            name if name.contains("empty") => Kind::File { length: 0 },
            name if name.contains("device") => Kind::Other,
            _ => Kind::File { length: 10 },
        }
    }

    #[test]
    fn files_are_taken_in_the_order_given() {
        let taken = accept_with([p("/b"), p("/a"), p("/c")], by_name).taken;
        assert_eq!(taken, [p("/b"), p("/a"), p("/c")]);
    }

    #[test]
    fn a_path_offered_twice_is_taken_once_and_the_first_time_counts() {
        let intake = accept_with([p("/a"), p("/b"), p("/a")], by_name);
        assert_eq!(intake.taken, [p("/a"), p("/b")]);
        assert_eq!(intake.left, [Left::Twice(p("/a"))]);
    }

    #[test]
    fn a_folder_a_device_and_a_missing_file_are_left_out_each_for_its_own_reason() {
        let intake = accept_with(
            [p("/dir/"), p("/dev/device"), p("/gone"), p("/ok")],
            by_name,
        );
        assert_eq!(intake.taken, [p("/ok")]);
        assert_eq!(
            intake.left,
            [
                Left::NotAFile(p("/dir/")),
                Left::NotAFile(p("/dev/device")),
                Left::Unreadable(p("/gone"))
            ]
        );
    }

    #[test]
    fn an_empty_file_is_left_out_because_there_is_nothing_in_it_to_send() {
        let intake = accept_with([p("/empty"), p("/ok")], by_name);
        assert_eq!(intake.taken, [p("/ok")]);
        assert_eq!(intake.left, [Left::Empty(p("/empty"))]);
    }

    #[test]
    fn no_more_than_the_limit_is_taken_and_the_rest_are_said_to_be_too_many() {
        let many: Vec<PathBuf> = (0..MOST_AT_ONCE + 3)
            .map(|n| p(&format!("/f{n}")))
            .collect();
        let intake = accept_with(many, by_name);
        assert_eq!(intake.taken.len(), MOST_AT_ONCE);
        assert_eq!(intake.left.len(), 3);
        assert!(intake.left.iter().all(|l| matches!(l, Left::TooMany(_))));
        // What was kept is the first ones, in order, not an arbitrary choice.
        assert_eq!(intake.taken[0], p("/f0"));
        assert_eq!(
            intake.taken[MOST_AT_ONCE - 1],
            p(&format!("/f{}", MOST_AT_ONCE - 1))
        );
    }

    #[test]
    fn nothing_offered_is_nothing_taken_and_nothing_to_say() {
        let intake = accept_with(Vec::<PathBuf>::new(), by_name);
        assert!(intake.is_empty());
        assert_eq!(words(&intake), None);
    }

    #[test]
    fn the_sentence_counts_what_was_left_out_and_never_lists_names() {
        let intake = accept_with([p("/a/"), p("/b/"), p("/gone"), p("/ok")], by_name);
        let said = words(&intake).expect("something was left out");
        assert_eq!(said, "Left out: 2 are not a file, 1 could not be read.");
        assert!(!said.contains("/a/"), "a name would be in the message");
    }

    #[test]
    fn a_path_offered_twice_alone_is_not_worth_a_message() {
        let intake = accept_with([p("/a"), p("/a")], by_name);
        assert_eq!(words(&intake), None);
    }

    #[test]
    fn only_a_real_list_of_files_is_a_paste_and_nothing_else_is() {
        // A web page or another program can put text on the clipboard, including the address of a
        // file the person never chose, such as a private key: that is never a file to send, and
        // the function does not so much as look at text. Only the list of files a file manager put
        // there is a choice.
        assert_eq!(pasted(Some(vec![p("/a"), p("/b")])), [p("/a"), p("/b")]);
        assert_eq!(pasted(Some(vec![])), Vec::<PathBuf>::new());
        assert_eq!(pasted(None), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_paste_is_the_v_key_with_control_or_with_the_command_key_and_nothing_else() {
        use freya::prelude::{Code, Modifiers};
        assert!(is_paste(Code::KeyV, Modifiers::CONTROL));
        assert!(is_paste(Code::KeyV, Modifiers::META));
        // V alone is a letter being typed; another key with control is another command.
        assert!(!is_paste(Code::KeyV, Modifiers::empty()));
        assert!(!is_paste(Code::KeyC, Modifiers::CONTROL));
        // Control and shift together is the terminal's paste, and Alt makes it another command.
        assert!(!is_paste(Code::KeyV, Modifiers::CONTROL | Modifiers::ALT));
    }

    #[test]
    fn on_the_real_file_system_a_file_is_taken_and_a_folder_and_a_missing_path_are_not() {
        let dir = std::env::temp_dir().join(format!("peersus-intake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, b"hello").unwrap();
        let empty = dir.join("empty.txt");
        std::fs::write(&empty, b"").unwrap();
        let intake = accept([
            file.clone(),
            dir.clone(),
            dir.join("missing"),
            empty.clone(),
        ]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(intake.taken, [file]);
        assert_eq!(intake.left.len(), 3);
        assert!(intake.left.contains(&Left::NotAFile(dir)));
        assert!(intake.left.contains(&Left::Empty(empty)));
    }
}
