//! The demo's screens, opened and driven with no display.
//!
//! These run the real root component, layout and drawing code on the demo's synthetic
//! fixture. They are evidence that a screen builds, lays out, draws and answers a press; they
//! are not evidence of the desktop window on a compositor or of any device.
//!
//! Unit tests of the library and not an integration test on purpose: an integration test
//! makes Cargo build the package's program with this feature on, and that would leave a
//! demo-capable `peersus-ui` where the ordinary development build is expected.

use super::{DESKTOP, NARROW, Window};
use freya::prelude::{Code, Key, Modifiers, NamedKey};

/// Each screen `--demo-page` opens, with words only that screen shows.
const PAGES: [(&str, &[&str]); 5] = [
    ("send", &["CHOOSE FILES", "Alice's laptop"]),
    ("receive", &["READY TO RECEIVE", "Paste the code"]),
    ("peers", &["PAIRED PEERS", "Test connection"]),
    ("transfers", &["Search transfers", "fixture-file-01.dat"]),
    ("settings", &["HOW THINGS STAND", "Paired devices"]),
];

fn open(page: &str, size: (f32, f32)) -> Window {
    Window::open(
        &[
            format!("--demo-page={page}"),
            "--demo-peers=5".to_owned(),
            "--demo-transfers=8".to_owned(),
        ],
        size,
    )
    .expect("the demo opens")
}

/// The eight bytes every PNG file begins with.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

#[test]
fn every_screen_opens_with_its_own_words_and_draws_at_both_widths() {
    for size in [DESKTOP, NARROW] {
        let mut pictures = Vec::new();
        for (page, words) in PAGES {
            let mut window = open(page, size);
            let texts = window.texts();
            for word in words {
                assert!(
                    texts.iter().any(|text| text == word),
                    "{page} at {size:?} does not show {word:?}; it shows {texts:?}"
                );
            }
            let picture = window.png();
            assert!(
                picture.starts_with(PNG) && picture.len() > 2_000,
                "{page} at {size:?} drew {} bytes, which is not a picture of a screen",
                picture.len()
            );
            pictures.push(picture);
        }
        // Five screens are five pictures: a root that ignored the page, or drew nothing but
        // its frame, would make them equal.
        for (index, picture) in pictures.iter().enumerate() {
            assert!(
                !pictures[..index].contains(picture),
                "{} at {size:?} drew the same picture as an earlier screen",
                PAGES[index].0
            );
        }
    }
}

#[test]
fn peers_and_send_keep_presence_above_the_custom_status() {
    for size in [DESKTOP, NARROW] {
        for (page, messages) in [
            ("peers", &["In a meeting", "Muted"][..]),
            ("send", &["In a meeting +1"][..]),
        ] {
            let mut window = open(page, size);
            window.frame();
            let texts = window.texts();
            let first = texts
                .iter()
                .position(|text| text == "Alice's laptop")
                .expect("peer card");
            assert_eq!(texts[first + 1], "ONLINE", "{page} {size:?}");
            assert_eq!(
                &texts[first + 2..first + 2 + messages.len()],
                messages,
                "{page} {size:?}"
            );
        }
    }
}

#[test]
fn the_fixture_marks_the_window_as_a_demo_on_the_screen_itself() {
    // The marker is how nobody takes a picture of this window for a real device: it has to
    // be among what is drawn, not only in the data.
    let window = open("settings", DESKTOP);
    assert!(
        window.shows("Synthetic Demo (read-only)"),
        "{:?}",
        window.texts()
    );
}

#[test]
fn pressing_a_tab_changes_the_screen_at_both_widths() {
    for size in [DESKTOP, NARROW] {
        let mut window = open("send", size);
        assert!(window.shows("CHOOSE FILES"));
        for (tab, word) in [
            ("PEERS", "PAIRED PEERS"),
            ("TRANSFERS", "Search transfers"),
            ("RECEIVE", "READY TO RECEIVE"),
            ("SEND", "CHOOSE FILES"),
        ] {
            assert!(window.click_text(tab), "no {tab} tab at {size:?}");
            assert!(
                window.shows(word),
                "pressing {tab} at {size:?} did not show {word:?}; it shows {:?}",
                window.texts()
            );
        }
    }
}

#[test]
fn a_command_pressed_in_the_demo_changes_nothing_it_shows() {
    // Every command is refused by the demo's client. What the screen must not do is act as
    // though it had been carried out: the device is still listed after "Forget".
    let mut window = open("peers", DESKTOP);
    assert!(window.shows("Alice's laptop"));
    assert!(window.click_text("Forget"), "{:?}", window.texts());
    // A confirmation, if the screen asks for one, is answered too.
    for confirm in ["Forget", "Forget device", "Yes"] {
        if window.click_text(confirm) {
            break;
        }
    }
    assert!(
        window.shows("Alice's laptop"),
        "a refused command removed the device from the screen: {:?}",
        window.texts()
    );
}

#[test]
fn a_window_opened_with_a_store_link_shows_that_extension_in_the_extensions_of_settings() {
    for size in [DESKTOP, NARROW] {
        let mut window = Window::open(
            &[
                "--demo-page=send".to_owned(),
                "--demo-link=peersus://store/synthetic-maps".to_owned(),
            ],
            size,
        )
        .expect("the demo opens");
        window.settle();
        window.frame();
        window.settle();
        // Settings, on its Extensions tab (the page on a desktop, the sheet over Send on a phone), with the linked extension's row
        // and its Install.
        assert!(
            window.shows("Search settings"),
            "{size:?}: {:?}",
            window.texts()
        );
        assert!(
            window.shows("synthetic-maps") && window.shows("Install"),
            "{size:?}: {:?}",
            window.texts()
        );
        // What Install downloads is on the card before it is pressed, and the line is whole on a phone too.
        let credit = format!(
            "by Synthetic authors · version 1.2.0 · {}",
            crate::view::size_in_words(71_253)
        );
        assert!(window.shows(&credit), "{size:?}: {:?}", window.texts());
        assert!(
            !window.cut_off_at_the_side(size.0).contains(&credit),
            "{size:?}: the credit line runs past the window"
        );
        if size == DESKTOP {
            assert!(
                !window.shows("CHOOSE FILES"),
                "the page is Settings, not Send: {:?}",
                window.texts()
            );
        }
    }
    // Without a link the same window opens where it was asked to.
    let mut plain =
        Window::open(&["--demo-page=send".to_owned()], DESKTOP).expect("the demo opens");
    plain.settle();
    assert!(
        plain.shows("CHOOSE FILES") && !plain.shows("synthetic-maps"),
        "{:?}",
        plain.texts()
    );
}

#[test]
fn back_closes_what_is_open_on_top_first_and_then_nothing_more() {
    // On a phone the store link opens Settings as a sheet over Send; Back closes it, and the page under it is as it was.
    let mut window = Window::open(
        &[
            "--demo-page=send".to_owned(),
            "--demo-link=peersus://store/synthetic-maps".to_owned(),
        ],
        NARROW,
    )
    .expect("the demo opens");
    window.settle();
    window.frame();
    window.settle();
    assert!(window.shows("Search settings"), "{:?}", window.texts());
    window.key(NamedKey::BrowserBack);
    window.frame();
    window.settle();
    assert!(!window.shows("Search settings"), "{:?}", window.texts());
    assert!(window.shows("CHOOSE FILES"), "{:?}", window.texts());
    // Nothing open: Back does nothing more here (a desktop or the demo has no platform Back to hand it to).
    window.key(NamedKey::BrowserBack);
    window.settle();
    assert!(window.shows("CHOOSE FILES"), "{:?}", window.texts());
}

#[test]
fn back_closes_the_newest_list_and_leaves_the_page_under_it() {
    // The language list over the Settings page: Back closes the list and nothing else.
    let mut window =
        Window::open(&["--demo-page=settings".to_owned()], (1100., 3200.)).expect("the demo opens");
    let english = |window: &Window| {
        window
            .texts()
            .iter()
            .filter(|text| *text == "English")
            .count()
    };
    let closed = english(&window);
    assert!(window.click_text("English"), "{:?}", window.texts());
    window.frame();
    window.settle();
    assert!(
        english(&window) > closed,
        "the list did not open: {:?}",
        window.texts()
    );
    window.key(NamedKey::BrowserBack);
    window.frame();
    window.settle();
    assert_eq!(english(&window), closed, "{:?}", window.texts());
    assert!(
        window.shows("Search settings"),
        "Back went further than the list: {:?}",
        window.texts()
    );
}

#[test]
fn a_colour_typed_in_the_theme_editor_is_applied_at_once_and_an_unreadable_one_is_named() {
    const WARNING: &str = "Low contrast: “Primary text” on “Page background” may be hard to read.";
    const HINT: &str = "Enter six hexadecimal RGB digits, such as #708090.";
    // Tall enough that the whole editor is on the screen, with every row above it that a desktop session can add (the place of
    // the panel of an offer is offered only where there is a display for one): a text that is scrolled away cannot be pressed.
    let mut window =
        Window::open(&["--demo-page=settings".to_owned()], (1100., 3200.)).expect("the demo opens");
    assert!(window.click_text("Appearance"), "{:?}", window.texts());
    // The page background is the element chosen when the editor opens, and the draft starts from the dark base.
    assert!(
        window.shows("Editing: Page background"),
        "{:?}",
        window.texts()
    );
    assert!(!window.shows(WARNING) && !window.shows(HINT));
    assert!(window.click_text("#000000"), "{:?}", window.texts());
    window.key(freya::prelude::NamedKey::End);
    for _ in 0..7 {
        window.key(freya::prelude::NamedKey::Backspace);
    }
    assert!(
        window.shows(HINT),
        "an empty colour is not said to be one: {:?}",
        window.texts()
    );
    // The colour of the dark base's own text: the page can no longer be read, and no button was pressed to apply it.
    window.write("#EEF2EE");
    assert!(
        window.shows(WARNING) && !window.shows(HINT),
        "{:?}",
        window.texts()
    );
    assert!(
        window.click_text("Reset selected element"),
        "{:?}",
        window.texts()
    );
    assert!(
        window.shows("#000000") && !window.shows(WARNING),
        "resetting the element did not give the base's colour back: {:?}",
        window.texts()
    );
    // What an action came to is said under the title, and discarding says so.
    assert!(window.click_text("Discard changes"));
    assert!(
        window.shows("Changes discarded; saved preferences are unchanged."),
        "{:?}",
        window.texts()
    );
}

/// The sizes every screen is laid out at, in logical pixels. A larger interface (the size a person chooses in Settings, or a
/// display with larger text) is the same window with fewer logical pixels across, so the narrow ones stand for a small phone
/// with the interface made larger, and the last for the desktop window. No size is offered that makes a window narrower than the
/// first (`interface_size::Percent::largest`).
const SIZES: [(f32, f32); 4] = [
    (crate::interface_size::Percent::NARROWEST, 6000.),
    (360., 6000.),
    (412., 6000.),
    (1100., 720.),
];

#[test]
fn no_text_is_broken_inside_a_word_at_any_size_in_any_language() {
    use crate::{i18n, view::SettingsTab};
    let mut found = Vec::new();
    // English, the test languages, and every language file of the local copy of the languages branch when there is one.
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/i18n/fixtures");
    let branch = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../languages");
    let mut files: Vec<Option<std::path::PathBuf>> = vec![None];
    for directory in std::iter::once(fixtures).chain(
        std::fs::read_dir(&branch)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path()),
    ) {
        let mut here: Vec<_> = std::fs::read_dir(directory)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "po"))
            .collect();
        here.sort();
        files.extend(here.into_iter().map(Some));
    }
    for file in files {
        let language = file.as_deref().map_or(i18n::Language::ENGLISH, |path| {
            let text = std::fs::read_to_string(path).expect("a language file reads");
            i18n::install(&text).expect("a language file installs")
        });
        let tag = file
            .as_deref()
            .map_or_else(|| "en".to_owned(), |path| path.display().to_string());
        for size in SIZES {
            let open = |page: &str| {
                let mut arguments = vec![format!("--demo-page={page}")];
                if let Some(path) = &file {
                    arguments.push(format!("--demo-language={}", path.display()));
                }
                Window::open(&arguments, size).expect("the demo opens")
            };
            let place = |page: &str| format!("{tag} {}x{} {page}", size.0, size.1);
            let mut look = |window: &Window, place: String| {
                found.extend(
                    window
                        .broken_words()
                        .into_iter()
                        .map(|broken| format!("{place}: {broken}")),
                );
            };
            for page in ["send", "receive", "peers", "transfers"] {
                look(&open(page), place(page));
            }
            // Upright, Settings is one page of every section; lying down, each section is opened by its name.
            let mut settings = open("settings");
            look(&settings, place("settings"));
            if size.0 > size.1 {
                for tab in SettingsTab::ALL {
                    let title = i18n::translate(language, tab.title());
                    if settings.click_text(title) {
                        look(&settings, place(title));
                    }
                }
            }
        }
    }
    found.sort();
    found.dedup();
    assert!(
        found.is_empty(),
        "{} texts are broken inside a word; give each what `crate::text` says its kind of text gets:\n{}",
        found.len(),
        found.join("\n")
    );
}

#[test]
fn saved_devices_that_do_not_fit_in_a_row_go_on_under_it_instead_of_off_the_window() {
    // The demo's own three cards fit one row of a desktop window, which is how a row that did not wrap went unseen: a person with a
    // fourth saved device had the fourth card cut by the window's edge. Eight of them cannot fit any of these widths.
    for size in [(1100., 720.), (900., 720.), (700., 720.)] {
        let window = Window::open(
            &["--demo-page=send".to_owned(), "--demo-peers=8".to_owned()],
            size,
        )
        .expect("the demo opens");
        let cut = window.cut_off_at_the_side(size.0);
        assert!(
            cut.is_empty(),
            "at {}x{} these are cut off at the window's side: {cut:?}",
            size.0,
            size.1
        );
    }
}

#[test]
fn nothing_on_the_settings_sheet_of_a_phone_is_drawn_under_the_group_icons() {
    use freya::prelude::{Label, Paragraph};
    // The icons of the groups stand in a column at the right edge of the sheet's scrolling area. A card's button and a long
    // group title ran under them on NX (E-146), 335 logical pixels across: the page now keeps that column free. Taller than NX,
    // so that a card with a button is on the screen at once.
    let size = (335., 1100.);
    let mut window = open("settings", size);
    window.frame();
    window.settle();
    // The demo's notice of an extension's update is drawn over everything, the icons too: closed first.
    if window.shows("✕") {
        assert!(window.click_text("✕"));
    }
    let top_of = |window: &Window, words: &str, bottom: bool| -> f32 {
        let found: Option<f32> = window.runner.find(|node, element| {
            (text_of!(element).as_deref() == Some(words) && node.is_visible()).then(|| {
                let area = node.layout().area;
                if bottom { area.max_y() } else { area.min_y() }
            })
        });
        found.unwrap_or_else(|| panic!("no {words:?} on the sheet: {:?}", window.texts()))
    };
    // What scrolls is between the sheet's header (title, search) and the tabs at the bottom.
    let (from, to) = (
        top_of(&window, "Search settings", true),
        top_of(&window, "SEND", false),
    );
    let edge = size.0 - crate::GroupMarks::COLUMN;
    let under = window.runner.find_many(|node, element| {
        let text = text_of!(element)?;
        let area = node.layout().area;
        (node.is_visible() && area.min_y() > from && area.max_y() < to && area.max_x() > edge + 0.5)
            .then(|| format!("{text:?} to x {}", area.max_x()))
    });
    assert!(
        window.shows("Paired devices") && window.shows("Remove"),
        "{:?}",
        window.texts()
    );
    assert!(
        under.is_empty(),
        "these are drawn under the column of group icons (right of x {edge}): {under:?}"
    );
}

#[test]
fn the_phone_settings_body_scrolls_without_moving_its_header() {
    use freya::prelude::{Label, Paragraph};
    let mut window = open("settings", (360., 840.));
    window.settle();
    if window.shows("✕") {
        assert!(window.click_text("✕"));
    }
    let top = |window: &Window, words: &str| -> f32 {
        window
            .runner
            .find(|node, element| {
                (text_of!(element).as_deref() == Some(words) && node.is_visible())
                    .then(|| node.layout().area.min_y())
            })
            .unwrap_or_else(|| panic!("no visible {words:?}"))
    };
    let header = top(&window, "Search settings");
    let status = top(&window, "Paired devices");
    window.scroll((150., 400.), (0., -140.));
    assert_eq!(top(&window, "Search settings"), header);
    assert!(top(&window, "Paired devices") < status);
}

#[test]
fn the_same_arguments_draw_the_same_picture_twice() {
    // What makes a picture comparable between two revisions: the fixture, the font and the
    // drawing are deterministic. Settings has no clock or animation on it.
    let first = open("settings", DESKTOP).png();
    let second = open("settings", DESKTOP).png();
    assert!(first == second, "two runs of one screen drew two pictures");
}

#[test]
fn the_binds_tab_lists_what_a_key_can_run_and_record_waits_for_the_keys() {
    let mut window = open("settings", DESKTOP);
    assert!(window.click_text("Binds"), "{:?}", window.texts());
    for words in [
        "KEY BINDINGS",
        "Accept incoming files",
        "Accepts the newest offer of files waiting for you, saved where your rule for received files says.",
        "Decline incoming files",
    ] {
        assert!(window.shows(words), "{words}: {:?}", window.texts());
    }
    assert!(window.click_text("+"));
    assert!(window.shows("press the keys…"), "{:?}", window.texts());
    // A modifier held on its own is shown as the start of a chord while the recording waits for a key.
    window.key_down(Key::Named(NamedKey::Alt), Code::AltLeft, Modifiers::empty());
    assert!(window.shows("alt+…"), "{:?}", window.texts());
    // The keys pressed together end the recording; a read-only demo changes nothing, so the row says none again.
    window.chord(
        Key::Character("g".into()),
        Code::KeyG,
        Modifiers::CONTROL | Modifiers::ALT,
    );
    assert!(!window.shows("press the keys…"), "{:?}", window.texts());
}

#[test]
fn arguments_the_demo_refuses_open_no_window() {
    let refused = Window::open(&["--demo-page=nowhere".to_owned()], DESKTOP);
    assert!(refused.is_err());
}

/// Let the window turn until `so`, for ten seconds at most: what an owner did reaches the window a moment later.
fn until(window: &mut Window, what: &str, mut so: impl FnMut(&Window) -> bool) {
    for _ in 0..200 {
        if so(window) {
            return;
        }
        window.settle();
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!(
        "{what}: it did not come. The window shows {:?}",
        window.texts()
    );
}

/// What the demo's pictures cannot show, since the demo hands nothing over: the Send page itself carrying out a send
/// to a device held near (experimental; `docs/nearby-send.md`). The real window, on a real owner that runs in this
/// process and is paired with a second one; only what the radio would say is said by the test.
///
/// It holds two things nothing else does. The page sends ONCE, with nothing pressed, when the paired device that is
/// near has Receive open, and not before. And the window goes on answering afterwards: the hand-over once read, inside
/// the effect that calls it, the very list it then empties, the effect called itself for ever, and the window drew
/// nothing more (engineering log, E-035). A window that stops answering stops its thread, so it lives on a thread of
/// its own and is given a time.
#[test]
fn files_listed_on_send_go_once_to_the_paired_device_held_near_with_receive_open_and_the_window_still_answers()
 {
    let (done, waited) = std::sync::mpsc::channel();
    let window = std::thread::spawn(move || {
        hand_over_to_the_device_held_near();
        let _ = done.send(());
    });
    match waited.recv_timeout(std::time::Duration::from_secs(120)) {
        Ok(()) => window.join().unwrap(),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("the window stopped answering: two minutes passed and it had not finished")
        }
        // It ended without a word: what it panicked with is the failure.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            if let Err(why) = window.join() {
                std::panic::resume_unwind(why);
            }
        }
    }
}

fn hand_over_to_the_device_held_near() {
    use crate::link::Session;
    use peersus_core::{config::SettingsPatch, model::Command, nearby::Found};
    const HINT: &str = "other-device is near. Open Receive on it and these files go to it.";
    const SENT: &str =
        "Sent to the device held near with Receive open. Each file is that person's to accept.";
    let root = tempfile::tempdir().unwrap();
    let (session, own, other) = Session::paired_for_test(root.path());
    // A command is a message and an answer: any executor carries it.
    let ask = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let tell = |command: Command| {
        ask.block_on(own.command(command)).unwrap();
    };
    tell(Command::PatchSettings {
        patch: Box::new(SettingsPatch {
            nearby_send: Some(true),
            ..Default::default()
        }),
    });
    let friend = own.snapshot().peers[0].id.clone();
    // What this device's radio would tell its owner: the paired device is near, and whether it says it takes files.
    let heard = |receiving: bool| Command::SetNearby {
        found: Found {
            near: vec![friend.clone()],
            receiving: if receiving {
                vec![friend.clone()]
            } else {
                Vec::new()
            },
            ..Found::default()
        },
        offers: Vec::new(),
    };
    let files: Vec<std::path::PathBuf> = ["first.bin", "second.bin"]
        .iter()
        .map(|name| {
            let path = root.path().join(name);
            std::fs::write(&path, [7_u8; 300]).unwrap();
            path
        })
        .collect();
    let sent = || -> Vec<String> {
        own.snapshot()
            .transfers
            .iter()
            .filter(|transfer| !transfer.incoming)
            .map(|transfer| transfer.name.clone())
            .collect()
    };
    // Tall enough that what the page says under the listed files is on the screen.
    let mut window = Window::on(session, (1100., 1600.));
    assert!(window.shows("CHOOSE FILES"), "{:?}", window.texts());

    // Near, with some other page open: a file listed goes nowhere, and the page says what is missing.
    tell(heard(false));
    window.drop_files((300., 300.), vec![files[0].clone()]);
    until(&mut window, "the file listed", |window| {
        window.shows("first.bin")
    });
    until(&mut window, "what is missing, said", |window| {
        window.shows(HINT)
    });
    assert!(sent().is_empty(), "sent to a device without Receive open");

    // Its person opens Receive: the file goes, once, with nothing pressed here.
    tell(heard(true));
    until(&mut window, "the file sent", |_| !sent().is_empty());
    until(&mut window, "the sending, said", |window| {
        window.shows(SENT)
    });
    assert!(!window.shows(HINT), "{:?}", window.texts());
    // The window turns on: nothing is sent a second time, and the other device was offered that one file.
    for _ in 0..20 {
        window.settle();
    }
    assert_eq!(sent(), ["first.bin"]);
    until(&mut window, "the offer at the other device", |_| {
        other.snapshot().transfers.len() == 1
    });
    assert!(other.snapshot().transfers[0].incoming);

    // It still answers: another page opens, and back on Send a file listed while it stays so goes too.
    assert!(window.click_text("RECEIVE"));
    until(&mut window, "Receive opened", |window| {
        window.shows("READY TO RECEIVE")
    });
    assert!(window.click_text("SEND"));
    until(&mut window, "Send opened again", |window| {
        !window.shows("READY TO RECEIVE")
    });
    window.drop_files((300., 300.), vec![files[1].clone()]);
    until(&mut window, "the second file sent", |_| sent().len() == 2);
    assert_eq!(sent(), ["first.bin", "second.bin"]);

    // The other device leaves Receive: a file listed now stays listed.
    tell(heard(false));
    until(
        &mut window,
        "the sending no longer said for a new list",
        |_| own.snapshot().nearby.receiving.is_empty(),
    );
    window.drop_files((300., 300.), vec![files[0].clone()]);
    until(&mut window, "what is missing, said again", |window| {
        window.shows(HINT)
    });
    for _ in 0..20 {
        window.settle();
    }
    assert_eq!(sent().len(), 2, "sent after Receive was left");

    for client in [&own, &other] {
        let _ = ask.block_on(client.command(Command::Shutdown {}));
    }
}

/// An extension that pans or zooms is painted from its live geometry, not new raster assets.
#[test]
fn changing_vector_drawings_do_not_create_raster_assets() {
    use freya::prelude::*;
    use freya_testing::TestingRunner;
    use peersus_api::elements::{Colour, Shape};
    fn graph() -> impl IntoElement {
        let step = use_consume::<State<u32>>();
        let peak = 5 + step() % 30;
        crate::contributed_page::drawing_view(
            100,
            40,
            &[Shape {
                path: format!("M 0 35 L 50 {peak} L 100 35"),
                stroke: Colour::Accent,
                line: 20,
                fill: Colour::default(),
                opacity: None,
                round: true,
            }],
            crate::theme::DARK,
        )
    }
    let (mut runner, (mut step, cache)) = TestingRunner::new(
        graph,
        (200., 100.).into(),
        |runner| {
            (
                runner.provide_root_context(|| State::create(0_u32)),
                runner.run_in(AssetCacher::get),
            )
        },
        1.,
    );
    let initial = runner.render().as_bytes().to_vec();
    for _ in 0..61 {
        *step.write() += 1;
        runner.sync_and_update();
        runner.render_to_surface();
        runner.poll_n(std::time::Duration::from_millis(1), 4);
    }
    let current = runner.render();
    assert_ne!(
        current.as_bytes(),
        initial,
        "updated vector path was not drawn"
    );
    assert_eq!(cache.size(), 0, "vector drawings allocated an image asset");
}

#[test]
fn a_vector_globe_is_round_and_scales_without_stretching_on_a_wide_surface() {
    use freya::prelude::*;
    use freya_testing::TestingRunner;
    use peersus_api::elements::{Colour, Shape};

    let (mut runner, mut zoom) = TestingRunner::new(
        || {
            let zoom = use_consume::<State<bool>>();
            let radius = if zoom() { 40 } else { 28 };
            let (left, right) = (120 - radius, 120 + radius);
            rect().expanded().background((0, 0, 0)).child(crate::contributed_page::drawing_view(
                240, 120,
                &[Shape {
                    path: format!("M{left} 60 A{radius} {radius} 0 1 0 {right} 60 A{radius} {radius} 0 1 0 {left} 60 Z"),
                    stroke: Colour::Transparent, line: 0, fill: Colour::Text, opacity: None, round: false,
                }],
                crate::theme::DARK,
            ))
        },
        (240., 120.).into(),
        |runner| runner.provide_root_context(|| State::create(false)),
        1.,
    );
    runner.sync_and_update();
    let first = image::load_from_memory(runner.render().as_bytes())
        .unwrap()
        .to_rgba8();
    assert_eq!(first.get_pixel(86, 60), first.get_pixel(120, 26));
    assert_eq!(first.get_pixel(86, 60), first.get_pixel(10, 10));
    zoom.set(true);
    runner.sync_and_update();
    let png = runner.render();
    let pixels = image::load_from_memory(png.as_bytes()).unwrap().to_rgba8();
    let inside = *pixels.get_pixel(120, 60);
    let outside = *pixels.get_pixel(10, 10);
    assert_ne!(inside, outside);
    for (x, y) in [(86, 60), (154, 60), (120, 26), (120, 94)] {
        assert_eq!(*pixels.get_pixel(x, y), inside);
    }
    for (x, y) in [(78, 60), (162, 60), (120, 18), (120, 102), (86, 26)] {
        assert_eq!(*pixels.get_pixel(x, y), outside);
    }
}

/// What a person saw on a phone in an extension's page: chips side by side in a box that wraps, the last one squeezed into what the
/// others left of the line and its word broken letter by letter ("b or de r"). A child of a box that wraps is measured against a
/// whole line (vendor/torin/PATCHES.md), so a text that does not fit the rest of the line goes under the others whole.
#[test]
fn a_text_in_a_box_that_wraps_goes_under_the_others_whole() {
    use freya::{engine::prelude::SkParagraph, prelude::*};
    use freya_testing::TestingRunner;
    const WORDS: [&str; 3] = ["surface", "raised", "border"];
    fn chips() -> impl IntoElement {
        // As the elements of an extension's page draw a box that wraps and a text of no fixed number of lines.
        WORDS.iter().fold(
            rect()
                .width(Size::px(150.))
                .horizontal()
                .content(Content::wrap_spacing(8.))
                .spacing(8.),
            |row, word| row.child(rect().padding(8.).child(label().text(*word).font_size(13.))),
        )
    }
    let (mut runner, ()) = TestingRunner::new(chips, (300., 200.).into(), |_| (), 1.);
    runner.sync_and_update();
    // Each word: how many lines it was laid out on, and the top of its box.
    let laid: Vec<(String, usize, f32)> = runner.find_many(|node, element| {
        let text = Label::try_downcast(element)?.text.into_owned();
        let layout = node.layout();
        let lines = layout
            .data
            .as_ref()?
            .downcast_ref::<SkParagraph>()?
            .get_line_metrics()
            .len();
        Some((text, lines, layout.area.min_y()))
    });
    assert_eq!(laid.len(), WORDS.len(), "{laid:?}");
    for (word, lines, _) in &laid {
        assert_eq!(
            *lines, 1,
            "{word:?} was broken over {lines} lines: {laid:?}"
        );
    }
    let top = |word: &str| {
        laid.iter()
            .find(|(text, ..)| text == word)
            .map(|(.., y)| *y)
    };
    assert_eq!(
        top("surface"),
        top("raised"),
        "the two that fit share the first line: {laid:?}"
    );
    assert!(
        top("border") > top("surface"),
        "the one that does not fit went under the others: {laid:?}"
    );
}

/// The pictures of the application the website shows (`site/shots/`), drawn by the application itself on the demo's made-up
/// data, at twice the density of a phone's logical pixels. Not a check: run on demand with `PEERSUS_SITE_SHOTS=<folder>`, as
/// docs/website.md says, and nothing is written without it.
#[test]
#[ignore = "writes the website's pictures; run on demand"]
fn site_shots() {
    let Some(folder) = std::env::var_os("PEERSUS_SITE_SHOTS").map(std::path::PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&folder).expect("the pictures' folder");
    // The runner's size is in device pixels: twice the logical size is the same layout, drawn twice as finely.
    const SCALE: f32 = 2.;
    let shoot = |name: &str, args: &[&str], size: (f32, f32)| {
        let args: Vec<String> = ["--demo-theme=dark"]
            .iter()
            .chain(args)
            .map(|arg| (*arg).to_owned())
            .collect();
        let mut window =
            Window::open_scaled(&args, (size.0 * SCALE, size.1 * SCALE), f64::from(SCALE))
                .expect("the demo opens");
        window.settle();
        window.frame();
        window.settle();
        // The fixture's notice of an extension's update is not what a picture of the screen is about: it goes by itself after
        // a few seconds of the wall clock (`notices::NOTICE_MS`), and the picture is taken once it has gone.
        let notice = "Update available: 1.2.0";
        let until = std::time::Instant::now()
            + std::time::Duration::from_millis(crate::notices::NOTICE_MS + 3_000);
        while window.texts().iter().any(|text| text == notice) && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(250));
            window.settle();
        }
        window.frame();
        window.settle();
        window
            .save(&folder.join(format!("{name}.png")))
            .expect("the picture is written");
    };
    for page in ["send", "receive", "peers", "transfers"] {
        shoot(
            &format!("phone-{page}"),
            &[
                &format!("--demo-page={page}"),
                "--demo-peers=3",
                "--demo-transfers=6",
            ],
            NARROW,
        );
    }
    shoot(
        "phone-store",
        &[
            "--demo-page=send",
            "--demo-peers=3",
            "--demo-link=peersus://store/synthetic-maps",
        ],
        NARROW,
    );
    shoot(
        "desktop-send",
        &["--demo-page=send", "--demo-peers=3", "--demo-transfers=6"],
        DESKTOP,
    );
}

/// The list of the actions an extension offers for the file listed on Send: pressing the extension's button opens it right under
/// that button, each action by its own name whole (`short_label`, else `label`), over the page and inside the window; a press
/// on a row, anywhere else, or Back closes it. Nothing in it is of any one extension: the fixture is two `selected_file`
/// actions with long names, one with a short name too. Pictures of it open at both widths are written to `target/bench/`.
#[test]
fn the_actions_of_an_extension_for_the_listed_file_open_in_a_list_under_its_button_at_both_widths()
{
    use crate::link::Session;
    use peersus_api::contributions::{Action, ActionInput, Source};
    use peersus_core::model::{PluginInfo, PluginStatus, Standing};
    const FIRST: &str = "Upload selected file (poo.wang)";
    const SECOND_LONG: &str = "Upload selected file (Catbox, anonymous)";
    const SECOND: &str = "Catbox (anonymous)";
    // The longest a label may be (80 characters), with no short one: on a phone it cannot be on one line.
    const LONG: &str =
        "Upload the selected file to the destination the person chose in settings page";
    const SCALE: f32 = 2.;
    let action = |id: &str, label: &str, short: Option<&str>| Action {
        id: id.into(),
        label: label.into(),
        short_label: short.map(str::to_owned),
        input: ActionInput::SelectedFile,
        target: Source {
            service: "svc".into(),
            method: id.into(),
        },
        capability: None,
        needs_device: false,
        destructive: false,
        timeout_ms: peersus_api::contributions::default_action_timeout(),
        chord: None,
        help: None,
    };
    let mut manifest =
        peersus_api::manifest::Manifest::new(peersus_api::manifest::Execution::Process);
    manifest.contributions.actions = vec![
        action("first", FIRST, None),
        action("second", SECOND_LONG, Some(SECOND)),
        action("third", LONG, None),
    ];
    let plugin = PluginInfo {
        id: peersus_core::plugins::PluginId::new("synthetic-files").unwrap(),
        manifest,
        standing: Standing::Ready,
        status: PluginStatus::Running,
        grantable: Default::default(),
        program: None,
        package: Some(peersus_api::package::Package {
            name: "Upload".into(),
            version: "0.1.0".into(),
            description: String::new(),
            changelog: String::new(),
            author: String::new(),
            link: String::new(),
        }),
        agreed: false,
        from_store: false,
    };
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("holiday-photo.png");
    std::fs::write(&file, [7_u8; 64]).unwrap();
    let pictures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench");
    std::fs::create_dir_all(&pictures).unwrap();
    for (size, picture) in [
        (DESKTOP, "upload-chooser-desktop"),
        (NARROW, "upload-chooser-phone"),
    ] {
        let mut snapshot = crate::demo::snapshot_sized(3, 0);
        snapshot.plugins.push(plugin.clone());
        let mut window = Window::on_scaled(
            Session::serving(snapshot),
            (size.0 * SCALE, size.1 * SCALE),
            f64::from(SCALE),
        );
        assert!(window.shows("CHOOSE FILES"), "{:?}", window.texts());
        window.drop_files((f64::from(size.0) / 2., 300.), vec![file.clone()]);
        until(&mut window, "the file listed", |window| {
            window.shows("holiday-photo.png")
        });
        // Closed: the extension's one button, and none of its actions.
        until(&mut window, "the extension's button", |window| {
            window.shows("Upload")
        });
        assert!(!window.shows(FIRST) && !window.shows(SECOND));

        let open = |window: &mut Window| {
            assert!(window.click_text("Upload"), "{:?}", window.texts());
            until(window, "the list", |window| window.shows(FIRST));
        };
        open(&mut window);
        // Each action by its own name: the short one in place of its long one, and every name whole, in the window.
        assert!(
            window.shows(SECOND) && !window.shows(SECOND_LONG),
            "{:?}",
            window.texts()
        );
        assert!(
            window.broken_words().is_empty(),
            "{:?}",
            window.broken_words()
        );
        let cut: Vec<String> = window
            .narrower_than_itself()
            .into_iter()
            .filter(|text| [FIRST, SECOND].contains(&text.as_str()))
            .collect();
        assert!(cut.is_empty(), "a name is cut short or wrapped: {cut:?}");
        // The picture is written before the geometry is judged, so that a failure of it can be looked at.
        window.frame();
        window.settle();
        window
            .save(&pictures.join(format!("{picture}.png")))
            .expect("the picture is written");
        let button = window.text_area("Upload").expect("the button");
        let (mut above, mut lowest) = (button.3, button.3);
        let mut edge = None;
        for name in [FIRST, SECOND, LONG] {
            let (left, top, right, bottom) = window.text_area(name).expect(name);
            assert!(
                left >= 0. && top >= 0. && right <= size.0 && bottom <= size.1,
                "{name} is not inside the window: {left} {top} {right} {bottom}"
            );
            // Right under the button, one row under the other, every name on the list's one left edge.
            assert!(
                top > above && edge.is_none_or(|edge: f32| (left - edge).abs() < 2.),
                "{name} is not right under the button: {left} {top} {right} {bottom} against {above} {button:?}"
            );
            edge = Some(left);
            above = bottom;
            lowest = lowest.max(bottom);
        }
        // On the button's left edge, unless the list had to be pushed in so its widest name stays in the window.
        let edge = edge.unwrap();
        assert!(
            (edge - button.0).abs() < 24. || edge < button.0,
            "the list is not under the button: {edge} against {button:?}"
        );
        assert!(
            lowest < button.3 + 320.,
            "the list is as tall as its three rows only"
        );
        // There is no sheet: no Cancel and no title over the page.
        assert!(!window.shows("Cancel"), "{:?}", window.texts());

        // Every way out: a press on a row, anywhere else, and Back.
        assert!(window.click_text(SECOND));
        assert!(!window.shows(FIRST), "a row pressed left the list open");
        open(&mut window);
        window.click(5., f64::from(size.1) - 5.);
        assert!(!window.shows(FIRST), "a press elsewhere left the list open");
        open(&mut window);
        window.key(NamedKey::BrowserBack);
        assert!(!window.shows(FIRST), "Back left the list open");
        // And it opens again: the button is not left held by the layer that closed the list.
        open(&mut window);
    }
}
