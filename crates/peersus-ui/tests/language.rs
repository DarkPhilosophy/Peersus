use peersus_core::config::Language;
use peersus_ui::view::{self, CodeCircle};

/// The test language files, the same the window's own tests read: a few messages of each language.
const FIXTURES: [&str; 5] = [
    include_str!("../src/i18n/fixtures/ro.po"),
    include_str!("../src/i18n/fixtures/de.po"),
    include_str!("../src/i18n/fixtures/pl.po"),
    include_str!("../src/i18n/fixtures/cs.po"),
    include_str!("../src/i18n/fixtures/sv.po"),
];

/// Installs the test language of `tag` and gives its language.
fn language(tag: &str) -> Language {
    let wanted = Language::parse(tag).unwrap();
    for text in FIXTURES {
        assert!(peersus_ui::i18n::install(text).is_ok());
    }
    wanted
}

#[test]
fn selection_counts_are_localized_but_filenames_are_not() {
    assert_eq!(
        view::chosen_words_total_in(language("ro"), &[], 22),
        ["22 de fișiere selectate"]
    );
    let names = vec!["Send".to_owned(), "Cancel {count}".to_owned()];
    assert_eq!(
        view::chosen_words_total_in(language("ro"), &names, 22),
        ["Send", "Cancel {count}", "+ încă 20"]
    );
    assert_eq!(
        view::queue_words_in(language("ro"), 20).as_deref(),
        Some("În total așteaptă 20 de fișiere")
    );
    assert_eq!(view::queue_words_in(language("de"), 1), None);
}

#[test]
fn progress_keeps_counts_and_uses_singular_and_plural_sentences() {
    let one = view::circle_words_in(
        language("ro"),
        &CodeCircle::Sending {
            files: 1,
            percent: 20,
        },
    )
    .unwrap();
    assert_eq!(
        one,
        ("SE TRIMITE", "20%".into(), "Se trimite 1 fișier.".into())
    );
    let many = view::circle_words_in(
        language("ro"),
        &CodeCircle::Sending {
            files: 20,
            percent: 60,
        },
    )
    .unwrap();
    assert_eq!(
        many,
        (
            "SE TRIMITE",
            "60%".into(),
            "Se trimit 20 de fișiere.".into()
        )
    );
    assert!(view::circle_words_in(language("ro"), &CodeCircle::Code).is_none());
}

#[test]
fn last_seen_is_formatted_in_the_requested_language() {
    assert_eq!(
        view::seen_words_in(language("ro"), Some(0), 20 * 60),
        "acum 20 de minute"
    );
    assert_eq!(
        view::seen_words_in(language("de"), None, 100),
        "noch nie gesehen"
    );
    assert_eq!(
        view::seen_words_in(language("pl"), Some(110), 100),
        "przed chwilą"
    );
}

#[test]
fn rejected_selection_can_change_language_without_retaining_paths() {
    use peersus_ui::intake::{Intake, Left, Summary};
    let intake = Intake {
        taken: vec![],
        left: vec![
            Left::NotAFile("private-folder".into()),
            Left::Unreadable("private-file".into()),
            Left::Twice("private-duplicate".into()),
        ],
    };
    let notice = Summary::from(&intake);
    assert_eq!(
        notice.words_in(Language::ENGLISH).as_deref(),
        Some("Left out: 1 is not a file, 1 could not be read.")
    );
    assert_eq!(
        notice.words_in(language("ro")).as_deref(),
        Some("Omise: Nu sunt fișiere: 1, Nu s-au putut citi: 1.")
    );
    let duplicates = Intake {
        taken: vec![],
        left: vec![Left::Twice("private-duplicate".into())],
    };
    assert!(
        Summary::from(&duplicates)
            .words_in(language("ro"))
            .is_none()
    );
}

#[test]
fn every_supported_language_preserves_file_labels_and_numeric_progress() {
    let names = vec!["Send".to_owned(), "Cancel {count}".to_owned()];
    for language in std::iter::once(Language::ENGLISH)
        .chain(["ro", "de", "pl", "cs", "sv"].into_iter().map(language))
    {
        let chosen = view::chosen_words_total_in(language, &names, 22);
        assert_eq!(
            &chosen[..2],
            names.as_slice(),
            "file labels are opaque in {language:?}"
        );
        let (_, percent, _) = view::circle_words_in(
            language,
            &CodeCircle::Sending {
                files: 22,
                percent: 37,
            },
        )
        .expect("an active transfer has progress");
        assert_eq!(percent, "37%", "progress must not change with {language:?}");
    }
}
