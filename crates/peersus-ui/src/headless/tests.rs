//! The demo's screens, opened and driven with no display.
//!
//! These run the real root component, layout and drawing code on the demo's synthetic
//! fixture. They are evidence that a screen builds, lays out, draws and answers a press; they
//! are not evidence of the desktop window on a compositor or of any device.
//!
//! Unit tests of the library and not an integration test on purpose: an integration test
//! makes Cargo build the package's program with this feature on, and that would leave a
//! demo-capable `peersus-ui` where the ordinary development build is expected.

use super::{DESKTOP, NARROW, PHONE, Window};
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
fn no_installed_extensions_leave_only_the_application_navigation() {
    use crate::{contributed_page, link::Session, navigation, view::View};
    use peersus_api::contributions::Placement;
    let mut snapshot = crate::demo::snapshot_sized(1, 0);
    snapshot.plugins.clear();
    snapshot.settings.extension_pages_together = true;
    let model = View::of(&snapshot);
    assert!(navigation::contributed_entries(&model).is_empty());
    for placement in [Placement::Primary, Placement::Settings, Placement::Status] {
        assert!(contributed_page::pages_here(&model, placement).is_empty());
    }
    let mut window = Window::on(Session::serving(snapshot), DESKTOP);
    assert!(window.shows("CHOOSE FILES"));
    assert!(
        !window.shows("EXTENSIONS"),
        "the main navigation must not offer an empty extension page"
    );
    assert!(window.png().starts_with(PNG));
}

#[test]
fn extension_navigation_keeps_its_labels_out_of_the_application_catalog() {
    use crate::{
        Page, i18n,
        navigation::{BottomTab, NavEntry, Tab},
        theme::DARK,
    };
    use freya::prelude::*;
    use freya_testing::TestingRunner;

    let language = i18n::fixture::language("ro");
    for mobile in [false, true] {
        let (mut runner, ()) = TestingRunner::new(
            move || {
                let entry = NavEntry {
                    page: Page::Contributed(1),
                    caption: "Battery".into(),
                    icon: "settings",
                };
                let on_select = EventHandler::new(|_| {});
                if mobile {
                    BottomTab {
                        entry,
                        active: true,
                        side: false,
                        palette: DARK,
                        on_select,
                    }
                    .into_element()
                } else {
                    Tab {
                        entry,
                        active: true,
                        palette: DARK,
                        dense: false,
                        bottom: false,
                        on_select,
                    }
                    .into_element()
                }
            },
            (300., 80.).into(),
            |runner| {
                runner.provide_root_context(|| {
                    i18n::Current(State::create(i18n::Shown {
                        language,
                        revision: 0,
                    }))
                });
            },
            1.,
        );
        runner.animation_clock().disable();
        runner.poll_n(std::time::Duration::from_millis(2), 12);
        runner.sync_and_update();
        runner.render_to_surface();
        let captions: Vec<_> = runner.find_many(|_, element| {
            Label::try_downcast(element).map(|label| label.text.into_owned())
        });
        assert_eq!(captions, ["Battery"], "mobile: {mobile}");
    }
}

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
fn desktop_window_bar_wheel_reaches_the_page_through_gaps_and_curved_edges() {
    use crate::link::Session;
    use peersus_core::{
        config::DesktopBarPosition::{Bottom, Top},
        model::TransferState,
    };

    // The default opening size and the smallest allowed by the desktop window.
    for size in [DESKTOP, (760., 560.)] {
        for edge in [Top, Bottom] {
            let mut snapshot = crate::demo::snapshot_sized(1, 28);
            snapshot.settings.desktop_bar_position = edge;
            snapshot.settings.reduced_motion = true;
            snapshot.plugins.clear();
            snapshot.store = Default::default();
            for transfer in &mut snapshot.transfers {
                transfer.state = TransferState::Complete;
            }
            let mut window = Window::on(Session::serving(snapshot), size);
            assert!(window.click_text("TRANSFERS"));
            let [left, right] = wide_bar_gaps(&window);
            // The last point is inside the centre piece's bounding box, but not its round.
            let cutout = (right.0 - 8., right.1 + 26.);
            let marker = "fixture-file-04.dat";
            for point in [left, right, cutout] {
                let before = window.text_area(marker).expect("transfer before scrolling");
                window.scroll(point, (0., -32.));
                let after = window.text_area(marker).expect("transfer after scrolling");
                assert!(
                    after.1 < before.1,
                    "{edge:?}, {size:?}, {point:?}: transfer did not move: {before:?} -> {after:?}"
                );
            }
        }
    }
}

fn wide_bar_gaps(window: &Window) -> [(f64, f64); 2] {
    use freya::prelude::{Color, Fill};
    let mut pieces = window.runner.find_many(|node, element| {
        let area = node.layout().area;
        let fill = element.style().background.clone();
        (area.height() == crate::view::HEADER_HEIGHT
            && [crate::theme::DARK.header, crate::theme::LIGHT.header]
                .into_iter()
                .any(|rgb| fill == Fill::Color(Color::from(rgb))))
        .then_some(area)
    });
    pieces.sort_by(|left, right| left.min_x().total_cmp(&right.min_x()));
    assert_eq!(pieces.len(), 3, "wide bar pieces: {pieces:?}");
    std::array::from_fn(|i| {
        assert!((pieces[i + 1].min_x() - pieces[i].max_x() - 12.).abs() < 0.5);
        (
            f64::from((pieces[i + 1].min_x() + pieces[i].max_x()) / 2.),
            f64::from(pieces[i].center().y),
        )
    })
}

fn desktop_gear(window: &Window) -> (f64, f64) {
    let [_, (right_gap, y)] = wide_bar_gaps(window);
    let diameter = crate::HEADER_ROUND;
    let discs: Vec<_> = window.runner.find_many(|node, _| {
        let area = node.layout().area;
        let centre = area.center();
        (f64::from(centre.x) > right_gap
            && (f64::from(centre.y) - y).abs() < 1.
            && (area.width() - diameter).abs() < 0.5
            && (area.height() - diameter).abs() < 0.5)
            .then_some(f64::from(centre.x))
    });
    (
        discs.into_iter().reduce(f64::min).expect("Settings gear"),
        y,
    )
}

#[test]
fn desktop_window_bar_choice_saves_from_settings_and_is_used_when_reopened() {
    use crate::link::Session;
    use peersus_core::config::DesktopBarPosition::{Bottom, Top};

    let root = tempfile::tempdir().unwrap();
    let (session, owner, _other) = Session::paired_for_test(root.path());
    let mut window = Window::on(session, DESKTOP);
    let (x, y) = desktop_gear(&window);
    window.click(x, y);
    assert!(window.click_text("Appearance"));
    assert!(window.shows("Window bar position"), "{:?}", window.texts());
    assert_eq!(owner.snapshot().settings.desktop_bar_position, Top);
    assert!(window.click_text("Bottom"));
    until(
        &mut window,
        "the bar moving down without leaving Settings",
        |window| {
            owner.snapshot().settings.desktop_bar_position == Bottom
                && wide_bar_gaps(window)[0].1 > f64::from(DESKTOP.1 / 2.)
                && window.shows("Window bar position")
        },
    );
    let saved = peersus_net::settings::SettingsStore::open(
        root.path().join("this-device").join("preferences.json"),
    )
    .unwrap();
    assert_eq!(saved.current().desktop_bar_position, Bottom);
    let mut restart = (*owner.snapshot()).clone();
    restart.settings = saved.current().clone();
    drop(window);
    let restarted = Window::on(Session::serving(restart), DESKTOP);
    assert!(wide_bar_gaps(&restarted)[0].1 > f64::from(DESKTOP.1 / 2.));
}

#[test]
fn desktop_window_bar_settings_menu_opens_toward_the_page_and_accepts_a_press() {
    use crate::{link::Session, view::HEADER_HEIGHT};
    use freya::prelude::MouseButton;
    use freya_testing::prelude::{MouseEventName, PlatformEvent};
    use peersus_core::config::DesktopBarPosition::{Bottom, Top};
    for edge in [Top, Bottom] {
        let mut snapshot = crate::demo::snapshot_sized(1, 4);
        snapshot.settings.desktop_bar_position = edge;
        snapshot.settings.reduced_motion = true;
        let mut window = Window::on(Session::serving(snapshot), DESKTOP);
        assert!(window.click_text("TRANSFERS"));
        let open = |window: &mut Window| {
            let point = desktop_gear(window);
            for name in [MouseEventName::MouseDown, MouseEventName::MouseUp] {
                window.runner.send_event(PlatformEvent::Mouse {
                    name,
                    cursor: point.into(),
                    button: Some(MouseButton::Right),
                });
                window.settle();
            }
        };
        open(&mut window);
        let (_, top, _, bottom) = window.text_area("Appearance").expect("settings menu item");
        match edge {
            Top => assert!(top > HEADER_HEIGHT),
            Bottom => assert!(bottom < DESKTOP.1 - HEADER_HEIGHT),
        }
        window.click(445., 356.);
        assert!(!window.shows("Appearance"));
        open(&mut window);
        assert!(window.click_text("Device"));
        assert!(window.shows("THIS DEVICE"), "{:?}", window.texts());
    }
}

#[test]
fn desktop_settings_search_comes_only_when_typed_and_at_the_edge_of_the_window_bar() {
    use crate::{link::Session, view::HEADER_HEIGHT};
    use freya::prelude::MouseButton;
    use freya_testing::prelude::{MouseEventName, PlatformEvent};
    use peersus_core::config::DesktopBarPosition::{Bottom, Top};
    for edge in [Top, Bottom] {
        let mut snapshot = crate::demo::snapshot_sized(1, 4);
        snapshot.settings.desktop_bar_position = edge;
        snapshot.settings.reduced_motion = true;
        snapshot.settings.device_name = "Desk".into();
        let mut window = Window::on(Session::serving(snapshot), DESKTOP);
        assert!(window.click_text("TRANSFERS"));
        let point = desktop_gear(&window);
        for name in [MouseEventName::MouseDown, MouseEventName::MouseUp] {
            window.runner.send_event(PlatformEvent::Mouse {
                name,
                cursor: point.into(),
                button: Some(MouseButton::Right),
            });
            window.settle();
        }
        assert!(window.click_text("Device"));
        assert!(window.shows("THIS DEVICE"), "{:?}", window.texts());
        assert!(
            !window.shows("Search settings"),
            "{edge:?}: no search is drawn before something is typed"
        );
        // Typed with no field holding the keyboard: the search comes with every letter, in the order typed.
        for letter in ["l", "a", "n", "g"] {
            window.write(letter);
        }
        assert!(window.shows("lang"), "{edge:?}: {:?}", window.texts());
        assert!(
            window.shows("Language · Device"),
            "{edge:?}: {:?}",
            window.texts()
        );
        let (_, top, _, bottom) = window.text_area("lang").expect("the search");
        match edge {
            Top => assert!(top > HEADER_HEIGHT && bottom < DESKTOP.1 / 2.),
            Bottom => assert!(top > DESKTOP.1 / 2. && bottom < DESKTOP.1 - HEADER_HEIGHT),
        }
        // Emptied, it goes, and the page is back.
        for _ in 0..4 {
            window.key(NamedKey::Backspace);
        }
        assert!(!window.shows("lang"), "{edge:?}: {:?}", window.texts());
        assert!(
            window.shows("THIS DEVICE"),
            "{edge:?}: {:?}",
            window.texts()
        );
        // What is typed into a field stays there: no search comes. The name is also in the window bar; the field is the last one drawn.
        let field = {
            use freya::prelude::{Label, Paragraph};
            window
                .runner
                .find_many(|node, element| {
                    (text_of!(element).as_deref() == Some("Desk") && node.is_visible())
                        .then(|| node.layout().area.center())
                })
                .pop()
                .expect("the device name field")
        };
        window.click(
            f64::from(field.x / window.scale),
            f64::from(field.y / window.scale),
        );
        window.write("x");
        assert!(
            window.shows("THIS DEVICE") && !window.texts().iter().any(|text| text == "x"),
            "{edge:?}: {:?}",
            window.texts()
        );
    }
}

#[test]
fn desktop_window_bar_leaves_the_offer_and_notice_clear_of_either_edge() {
    use crate::{link::Session, view::HEADER_HEIGHT};
    use peersus_api::notification::Notification;
    use peersus_core::{
        config::DesktopBarPosition::{Bottom, Top},
        model::{NotificationSource, PostedNotification, TransferState},
    };

    for edge in [Top, Bottom] {
        let mut snapshot = crate::demo::snapshot_sized(1, 1);
        snapshot.settings.desktop_bar_position = edge;
        snapshot.settings.reduced_motion = true;
        let offer = &mut snapshot.transfers[0];
        offer.incoming = true;
        offer.state = TransferState::Offered;
        offer.name = "Offered document.txt".into();
        snapshot.notifications.push(PostedNotification {
            id: 1,
            source: NotificationSource::App,
            notification: Notification::saying("Document is ready"),
        });
        let window = Window::on(Session::serving(snapshot), DESKTOP);
        let (_, offer_y, _, _) = window.text_area("Offered document.txt").expect("offer");
        let (_, _, _, cancel_bottom) = window.text_area("Cancel").expect("offer control");
        let (_, notice_y, _, notice_bottom) =
            window.text_area("Document is ready").expect("notice");
        assert!(notice_y > DESKTOP.1 / 2.);
        match edge {
            Top => assert!(notice_bottom < offer_y && cancel_bottom <= DESKTOP.1),
            Bottom => {
                assert!(cancel_bottom < DESKTOP.1 / 2.);
                assert!(notice_bottom <= DESKTOP.1 - HEADER_HEIGHT);
            }
        }
    }
}

const CANVAS_INK: [u8; 4] = [38, 170, 133, 255];

fn document_on(
    snapshot: &mut peersus_core::model::Snapshot,
    canvas: bool,
) -> Option<crate::canvas_view::TestFrame> {
    use peersus_api::{
        contributions::{Page, Placement, Surface},
        elements::{Colour, Length, Node, Style, Tone},
        manifest::{Execution, Manifest},
    };
    use peersus_core::{
        model::{PageView, PluginInfo, PluginStatus, Standing},
        plugins::PluginId,
    };
    let plugin = PluginId::new("layout-fixture").unwrap();
    let mut manifest = Manifest::new(Execution::Process);
    manifest.contributions.pages = vec![Page {
        id: "document".into(),
        label: "Document".into(),
        placement: Placement::Primary,
        icon: None,
        source: None,
        surface: Some(Surface::Elements),
        needs_device: false,
        capability: None,
        platforms: vec![],
    }];
    snapshot.plugins.push(PluginInfo {
        id: plugin.clone(),
        manifest,
        standing: Standing::Ready,
        status: PluginStatus::Running,
        grantable: Default::default(),
        program: None,
        package: None,
        agreed: true,
        from_store: false,
    });

    let style = Style {
        width: Length::Percent(10000),
        height: Length::Px(1100),
        background: if canvas {
            Colour::Transparent
        } else {
            Colour::Accent
        },
        ..Default::default()
    };
    let plane = if canvas {
        Node::Canvas {
            id: "picture".into(),
            style,
            observe: vec![],
            a11y: vec![],
        }
    } else {
        Node::Box {
            id: None,
            style,
            children: vec![],
            observe: vec![],
        }
    };
    let row = |text: &str| Node::Text {
        text: text.into(),
        tone: Tone::Body,
        lines: None,
        size: Some(20),
    };
    let tree = Node::Box {
        id: None,
        style: Style {
            width: Length::Percent(10000),
            gap: 12,
            ..Default::default()
        },
        children: vec![row("First item"), plane, row("Last item")],
        observe: vec![],
    };
    tree.validate().unwrap();
    snapshot.page_views.push(PageView {
        plugin: plugin.clone(),
        page: "document".into(),
        view: tree,
    });
    canvas.then(|| {
        crate::canvas_view::TestFrame::new(
            peersus_app::ipc::FrameKey {
                plugin,
                page: "document".into(),
                canvas: "picture".into(),
            },
            2,
            2,
            CANVAS_INK.repeat(4),
        )
        .expect("checked Canvas frame")
    })
}

#[test]
fn desktop_window_bar_renders_every_page_kind_through_the_gaps_at_both_edges() {
    use crate::{link::Session, view::HEADER_HEIGHT};
    use peersus_core::{
        config::{DesktopBarPosition, Theme},
        model::TransferState,
    };
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/px-topbar");
    std::fs::create_dir_all(&output).unwrap();
    let background = [0, 0, 0, 255];
    let (r, g, b) = crate::theme::DARK.receive;
    let elements_ink = [r, g, b, 255];

    for (edge, placement) in [
        (DesktopBarPosition::Top, "top"),
        (DesktopBarPosition::Bottom, "bottom"),
    ] {
        let (clear_top, clear_bottom) = crate::view::Form::Wide.window_bar(edge).clearance();
        for (kind, first, last) in [
            ("built-in", "Search transfers", "fixture-file-28.dat"),
            ("settings", "THIS DEVICE", "Choose folder"),
            ("elements", "First item", "Last item"),
            ("canvas", "First item", "Last item"),
        ] {
            let mut snapshot = crate::demo::snapshot_sized(3, 28);
            snapshot.settings.desktop_bar_position = edge;
            snapshot.settings.theme = Theme::Dark;
            snapshot.settings.reduced_motion = true;
            snapshot.plugins.clear();
            snapshot.store = Default::default();
            for transfer in &mut snapshot.transfers {
                transfer.state = TransferState::Complete;
            }
            let frame = if matches!(kind, "elements" | "canvas") {
                document_on(&mut snapshot, kind == "canvas")
            } else {
                None
            };
            let mut window = Window::on(Session::serving(snapshot), DESKTOP);
            if let Some(frame) = frame {
                window
                    .runner
                    .run_in(|| freya::prelude::provide_root_context(frame));
            }
            match kind {
                "built-in" => assert!(window.click_text("TRANSFERS")),
                "settings" => {
                    let (x, y) = desktop_gear(&window);
                    window.click(x, y);
                    assert!(window.click_text("Device"));
                }
                _ => assert!(window.click_text("Document")),
            }
            window.settle();
            let gaps = wide_bar_gaps(&window);
            let [_, (x, y)] = gaps;
            let edge_y = if edge == DesktopBarPosition::Top {
                HEADER_HEIGHT / 2.
            } else {
                DESKTOP.1 - HEADER_HEIGHT / 2.
            };
            assert!(
                (y - f64::from(edge_y)).abs() < 1.,
                "{kind} {placement}: {y}"
            );
            let area = |window: &Window, label: &str| {
                window.text_area(label).unwrap_or_else(|| {
                    panic!("{kind} {placement}: no {label:?}: {:?}", window.texts())
                })
            };
            let first_area = area(&window, first);
            assert!(first_area.1 >= clear_top && first_area.3 <= DESKTOP.1 - clear_bottom);

            let rendered = (0..16).find_map(|_| {
                let png = window.png();
                let pixels = image::load_from_memory(&png).unwrap().to_rgba8();
                let colour = match kind {
                    "elements" => Some(elements_ink),
                    "canvas" => Some(CANVAS_INK),
                    _ => None,
                };
                // In Settings the left gap is over the fixed sidebar, not over the scroll view.
                let start = usize::from(kind == "settings");
                let shows_page = gaps[start..].iter().all(|&(x, y)| {
                    let x = x.round() as u32;
                    let y = y.round() as u32;
                    (y - 18..=y + 18).any(|y| {
                        let pixel = pixels.get_pixel(x, y).0;
                        colour.map_or(pixel != background, |ink| pixel == ink)
                    })
                });
                if shows_page {
                    if let Some(ink) = colour {
                        assert_ne!(
                            pixels
                                .get_pixel((x - 45.).round() as u32, y.round() as u32)
                                .0,
                            ink,
                            "{kind} {placement}: the centre piece must be above the document"
                        );
                    }
                    Some(png)
                } else {
                    window.scroll((x, y), (0., -56.));
                    None
                }
            });
            std::fs::write(
                output.join(format!("{kind}-{placement}.png")),
                rendered.unwrap_or_else(|| panic!("{kind} {placement}: no page paint in the gaps")),
            )
            .unwrap();

            window.scroll((x, 400.), (0., -12000.));
            let last_area = area(&window, last);
            assert!(
                last_area.1 >= clear_top && last_area.3 <= DESKTOP.1 - clear_bottom,
                "{kind} {placement}: last row is laid under the bar: {last_area:?}"
            );
        }
    }
}

/// Draws every `*.json` extension page in `$PAGE_JSON_DIR` in the real window, as the application draws an extension's page,
/// and writes `<name>.png` beside it: the narrowest phone for a name with `phone` in it, a larger phone for `narrow`, else the
/// desktop window. How an
/// extension's own rendered page is seen without a device (Chat's `dump_pages` writes such files). Run by hand:
/// `PAGE_JSON_DIR=dir cargo test -p peersus-ui --features headless draws_extension_pages_from_files -- --ignored`.
#[test]
#[ignore]
fn draws_extension_pages_from_files() {
    let Some(dir) = std::env::var_os("PAGE_JSON_DIR") else {
        return;
    };
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension() != Some("json".as_ref()) {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let tree = peersus_api::elements::Node::parse(&value).unwrap();
        let name = path.to_string_lossy();
        let size = if name.contains("phone") {
            PHONE
        } else if name.contains("narrow") {
            NARROW
        } else {
            DESKTOP
        };
        let mut snapshot = crate::demo::snapshot_sized(1, 0);
        snapshot.settings.reduced_motion = true;
        snapshot.plugins.clear();
        snapshot.store = Default::default();
        document_on(&mut snapshot, false);
        snapshot.page_views.last_mut().unwrap().view = tree;
        let mut window = Window::on(crate::link::Session::serving(snapshot), size);
        assert!(
            window.click_text("Document"),
            "{}: no page to open",
            path.display()
        );
        std::fs::write(path.with_extension("png"), window.png()).unwrap();
    }
}

fn dock_document(
    edge: peersus_core::config::DesktopBarPosition,
    tree: peersus_api::elements::Node,
) -> Window {
    let mut snapshot = crate::demo::snapshot_sized(1, 0);
    snapshot.settings.desktop_bar_position = edge;
    snapshot.settings.reduced_motion = true;
    snapshot.plugins.clear();
    snapshot.store = Default::default();
    document_on(&mut snapshot, false);
    tree.validate().unwrap();
    snapshot.page_views.last_mut().unwrap().view = tree;
    let mut window = Window::on(crate::link::Session::serving(snapshot), DESKTOP);
    assert!(window.click_text("Document"));
    window
}

fn dock_box(
    style: peersus_api::elements::Style,
    children: Vec<peersus_api::elements::Node>,
) -> peersus_api::elements::Node {
    peersus_api::elements::Node::Box {
        id: None,
        style,
        children,
        observe: vec![],
    }
}

#[test]
fn desktop_dock_window_controls_take_the_press_over_nested_scrollbars() {
    use freya::prelude::Layer;
    use peersus_api::elements::{Length, MAX_DEPTH, Style};
    use peersus_core::config::DesktopBarPosition::Top;

    let content = dock_box(
        Style {
            height: Length::Px(840),
            ..Default::default()
        },
        vec![],
    );
    let scroller = dock_box(
        Style {
            width: Length::Px(890),
            height: Length::Px(160),
            scroll: true,
            ..Default::default()
        },
        vec![content],
    );
    let raised = (0..MAX_DEPTH - 3).fold(scroller, |child, _| {
        dock_box(
            Style {
                width: Length::Percent(10000),
                height: Length::Px(160),
                overlay: true,
                ..Default::default()
            },
            vec![child],
        )
    });
    let mut window = dock_document(
        Top,
        dock_box(
            Style {
                width: Length::Percent(10000),
                height: Length::Px(1200),
                ..Default::default()
            },
            vec![raised],
        ),
    );
    let gear = desktop_gear(&window);
    window.scroll((600., 350.), (0., -100.));
    assert!(
        window
            .runner
            .find(|node, element| {
                let area = node.layout().area;
                (element.layer() == Layer::Relative(999)
                    && area.contains((gear.0 as f32, gear.1 as f32).into()))
                .then_some(())
            })
            .is_some(),
        "a guest scrollbar must actually cross the gear at {gear:?}",
    );
    window.click(gear.0, gear.1);
    assert!(
        window.shows("HOW THINGS STAND"),
        "the gear did not open Settings through the scrollbar: {:?}",
        window.texts()
    );
}

fn dock_device_chooser(edge: peersus_core::config::DesktopBarPosition) -> Window {
    use peersus_api::elements::{Length, Node, Style};
    use peersus_core::config::DesktopBarPosition::Bottom;
    let chooser = dock_box(
        Style {
            at: Some([340_000, if edge == Bottom { 641_000 } else { 0 }]),
            ..Default::default()
        },
        vec![Node::Devices {
            id: "destination".into(),
            chosen: None,
            pick: Default::default(),
            show: Default::default(),
            above: false,
            this_device: false,
            entries: vec![],
        }],
    );
    dock_document(
        edge,
        dock_box(
            Style {
                width: Length::Percent(10000),
                height: Length::Px(1150),
                ..Default::default()
            },
            vec![chooser],
        ),
    )
}

#[test]
fn desktop_dock_focus_reveals_a_page_control_before_keyboard_activation_at_either_edge() {
    use crate::view::HEADER_HEIGHT;
    use peersus_core::config::DesktopBarPosition::{Bottom, Top};

    for edge in [Top, Bottom] {
        let mut window = dock_device_chooser(edge);
        let label = "Choose a device";
        if edge == Top {
            window.scroll((700., 350.), (0., -90.));
        }
        let (_, top, _, bottom) = window.text_area(label).expect("chooser face");
        let [left, right] = wide_bar_gaps(&window);
        let (x0, _, x1, _) = window.text_area(label).unwrap();
        assert!(f64::from(x0) > left.0 + 12. && f64::from(x1) < right.0 - 12.);
        assert!(
            if edge == Top {
                top >= 0. && bottom < HEADER_HEIGHT
            } else {
                top > DESKTOP.1 - HEADER_HEIGHT && bottom <= DESKTOP.1
            },
            "{edge:?}: chooser is not under the solid centre piece: {top}..{bottom}",
        );
        window.key(NamedKey::Tab);
        let (_, top, _, bottom) = window.text_area(label).expect("focused chooser");
        assert!(
            if edge == Top {
                top >= HEADER_HEIGHT
            } else {
                bottom <= DESKTOP.1 - HEADER_HEIGHT
            },
            "{edge:?}: Tab left the control behind the dock: {top}..{bottom}",
        );
        window.key(NamedKey::Enter);
        assert!(
            window.shows("No device is paired"),
            "the revealed chooser did not take Enter"
        );
    }
}

#[test]
fn desktop_dock_does_not_let_a_page_menu_follow_its_anchor_into_the_bar() {
    use crate::view::HEADER_HEIGHT;
    use peersus_core::config::DesktopBarPosition::Bottom;

    let mut window = dock_device_chooser(Bottom);
    let point = (700., 350.);
    window.scroll(point, (0., -120.));
    assert!(window.click_text("Choose a device"));
    assert!(window.shows("No device is paired"));
    window.scroll(point, (0., 120.));
    assert!(
        window.text_area("Choose a device").unwrap().1 > DESKTOP.1 - HEADER_HEIGHT,
        "the open menu's anchor must be behind the dock",
    );
    let menu = window.text_area("No device is paired");
    assert!(
        menu.is_none_or(|area| area.3 <= DESKTOP.1 - HEADER_HEIGHT),
        "the page menu moved across the dock: {menu:?}",
    );
}

#[test]
fn desktop_dock_resize_grip_takes_the_press_over_the_occupied_edge_with_the_menu_closed() {
    use crate::link::Session;
    use freya::prelude::{AppCommand, CursorIcon, GlobalContexts, Platform};
    use peersus_core::config::DesktopBarPosition::{Bottom, Top};
    use std::{cell::Cell, rc::Rc};

    let actual = [Top, Bottom].map(|edge| {
        let mut snapshot = crate::demo::snapshot_sized(1, 4);
        snapshot.settings.desktop_bar_position = edge;
        snapshot.settings.reduced_motion = true;
        let mut window = Window::on(Session::serving(snapshot), DESKTOP);
        let peers = window.text_area("PEERS").expect("the middle dock piece");
        let x = f64::from((peers.0 + peers.2) / 2.);
        let y = if edge == Top { 3. } else { DESKTOP.1 - 3. };
        let grip = (x, f64::from(y));
        window.runner.move_cursor(grip);
        let cursor = window.runner.cursor_icon();

        // There is no native window in this runner. Count the platform callback queued
        // by the first pointer-down, before any release, movement, or double click.
        let requests = Rc::new(Cell::new(0));
        window.runner.run_in(|| {
            let original = Platform::get();
            let native_window = original.current_window();
            let requests = requests.clone();
            let observer = Platform::new(move |event| match event {
                AppCommand::Erased(_) => requests.set(requests.get() + 1),
                event => original.send(event),
            });
            observer.register_window(native_window);
            GlobalContexts::get().insert_context(observer);
        });
        window.runner.press_cursor(grip);
        (requests.get(), cursor)
    });
    assert_eq!(actual, [(1, CursorIcon::NResize), (1, CursorIcon::SResize)]);
}

#[test]
fn desktop_dock_resize_grip_still_handles_pointer_down_with_the_quick_menu_open() {
    use freya::prelude::{CursorIcon, MouseButton};
    use freya_testing::prelude::{MouseEventName, PlatformEvent};

    let mut window = open("transfers", DESKTOP);
    let grip = (3., 230.);
    window.runner.move_cursor(grip);
    assert_eq!(window.runner.cursor_icon(), CursorIcon::WResize);
    let gear = desktop_gear(&window);
    for name in [MouseEventName::MouseDown, MouseEventName::MouseUp] {
        window.runner.send_event(PlatformEvent::Mouse {
            name,
            cursor: gear.into(),
            button: Some(MouseButton::Right),
        });
        window.settle();
    }
    assert!(window.shows("Appearance"));
    window.runner.press_cursor(grip);
    window.settle();
    assert!(
        !window.shows("Appearance"),
        "the grip did not handle the resize press"
    );
}

#[test]
fn desktop_dock_room_filling_canvas_fits_the_viewport_without_scrolling_at_either_edge() {
    use peersus_api::elements::{Length, Node};
    use peersus_core::config::{DesktopBarPosition, Theme};

    for edge in [DesktopBarPosition::Top, DesktopBarPosition::Bottom] {
        let mut snapshot = crate::demo::snapshot_sized(1, 0);
        snapshot.settings.desktop_bar_position = edge;
        snapshot.settings.theme = Theme::Dark;
        snapshot.settings.reduced_motion = true;
        snapshot.plugins.clear();
        snapshot.store = Default::default();
        let frame = document_on(&mut snapshot, true).unwrap();
        let page = snapshot.page_views.last_mut().unwrap();
        let Node::Box { children, .. } = &mut page.view else {
            panic!("the fixture must contain a canvas");
        };
        let mut root = children.remove(1);
        let Node::Canvas { style, .. } = &mut root else {
            panic!("a viewport-sized Canvas root");
        };
        style.height = Length::Percent(10000);
        root.validate().unwrap();
        page.view = root;
        let mut window = Window::on(crate::link::Session::serving(snapshot), DESKTOP);
        window
            .runner
            .run_in(|| freya::prelude::provide_root_context(frame));
        assert!(window.click_text("Document"));
        let gaps = wide_bar_gaps(&window);
        let shows_canvas = |window: &mut Window| {
            let image = image::load_from_memory(&window.png()).unwrap().to_rgba8();
            gaps.into_iter()
                .all(|(x, y)| image.get_pixel(x.round() as u32, y.round() as u32).0 == CANVAS_INK)
        };
        // The page is the room between the document's padding and the bar's clearance: nothing of it lies behind the
        // bar, and the document has nothing to scroll, whichever edge the bar is on.
        let before = window.png();
        assert!(
            !shows_canvas(&mut window),
            "{edge:?}: canvas must stay clear of the bar"
        );
        window.scroll((600., 350.), (0., -300.));
        assert!(
            !shows_canvas(&mut window),
            "{edge:?}: canvas must stay clear of the bar"
        );
        assert!(
            before == window.png(),
            "{edge:?}: a page that fills the room must not scroll"
        );
    }
}

#[test]
fn desktop_dock_keeps_tall_notices_out_of_the_incoming_offer_at_high_scale() {
    use crate::link::Session;
    use peersus_api::notification::{Button, Notification};
    use peersus_core::{
        config::DesktopBarPosition::Bottom,
        model::{NotificationSource, PostedNotification, TransferState},
    };
    let mut snapshot = crate::demo::snapshot_sized(1, 2);
    snapshot.settings.desktop_bar_position = Bottom;
    snapshot.settings.reduced_motion = true;
    for (i, transfer) in snapshot.transfers.iter_mut().enumerate() {
        transfer.state = TransferState::Offered;
        transfer.incoming = true;
        transfer.name = format!("Offered file {}.txt", i + 1);
    }
    let response = |id| format!("Answer notice {id}");
    for id in 1..=3 {
        let notification = Notification {
            buttons: vec![Button {
                id: "answer".into(),
                label: response(id),
            }],
            ..Notification::saying(
                "This notification contains sufficient detail to wrap across several lines. "
                    .repeat(4),
            )
        };
        notification.validate().unwrap();
        snapshot.notifications.push(PostedNotification {
            id,
            source: NotificationSource::App,
            notification,
        });
    }
    let mut window = Window::on_scaled(Session::serving(snapshot), (762., 510.), 1.5);
    let offer = window.text_area("View files").expect("incoming offer");
    assert!(!window.shows("Offered file 2.txt"));
    assert!(!window.shows(&response(3)), "not all three notices fit");
    for id in 1..=3 {
        if let Some(notice) = window.text_area(&response(id)) {
            assert!(
                notice.1 >= offer.3,
                "notice {id} overlapped the offer: {notice:?} vs {offer:?}"
            );
        }
    }
    assert!(window.click_text("View files"));
    assert!(
        window.shows("Offered file 2.txt"),
        "a notice intercepted the offer's press"
    );
    assert!(window.click_text("View files"));
    // This row is below the notice viewport at first, not discarded or permanently clipped.
    window.scroll((660., 330.), (0., -1200.));
    assert!(window.shows(&response(3)), "{:?}", window.texts());
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
        // and its Install. The sheet always has its search; the desktop page draws one only when something is typed.
        assert!(
            window.shows("Search settings") == (size == NARROW),
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
fn escape_on_a_desktop_closes_the_newest_list_then_goes_to_the_first_screen_then_does_nothing() {
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
    assert!(english(&window) > closed, "the list did not open");
    window.key(NamedKey::Escape);
    window.frame();
    window.settle();
    assert_eq!(english(&window), closed, "{:?}", window.texts());
    assert!(
        window.shows("Search settings"),
        "Escape went further than the list"
    );
    window.key(NamedKey::Escape);
    window.frame();
    window.settle();
    assert!(!window.shows("Search settings"), "{:?}", window.texts());
    assert!(window.shows("CHOOSE FILES"), "{:?}", window.texts());
    // On the first screen a desktop's Escape has nowhere to go and leaves nothing.
    window.key(NamedKey::Escape);
    window.settle();
    assert!(window.shows("CHOOSE FILES"), "{:?}", window.texts());
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
fn the_phone_settings_heading_goes_as_the_sheet_is_read_down_and_comes_back_up() {
    let mut window = open("settings", (360., 840.));
    window.settle();
    if window.shows("✕") {
        assert!(window.click_text("✕"));
    }
    let top = |window: &Window, words: &str| window.text_area(words).map(|area| area.1);
    let status = top(&window, "Paired devices").expect("the first group");
    assert!(window.shows("Search settings"), "{:?}", window.texts());
    window.scroll((150., 400.), (0., -140.));
    assert!(top(&window, "Paired devices").expect("still there") < status);
    assert!(!window.shows("Search settings"), "{:?}", window.texts());
    window.scroll((150., 400.), (0., 60.));
    assert!(window.shows("Search settings"), "{:?}", window.texts());
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

#[test]
fn send_live_destinations_replace_stale_results_and_keep_parallel_actions_reachable() {
    for size in [DESKTOP, NARROW] {
        check_send_live_destinations(size);
    }
}

fn check_send_live_destinations(size: (f32, f32)) {
    use crate::{link::Session, plugin_controls::SelectedControls, view::View};
    use freya::prelude::*;
    use freya_testing::TestingRunner;
    use peersus_api::contributions::{Action, ActionInput, Source};
    use peersus_api::manifest::{Execution, Manifest};
    use peersus_core::model::{
        ActionState, ActionStatus, PluginInfo, PluginStatus, Snapshot, Standing,
    };
    use peersus_core::plugins::PluginId;

    let id = PluginId::new("fixture-destinations").unwrap();
    let mut manifest = Manifest::new(Execution::Process);
    for (id, label) in [
        ("one", "First destination"),
        ("two", "Second destination"),
        ("three", "Third destination"),
    ] {
        manifest.contributions.actions.push(Action {
            id: id.into(),
            label: label.into(),
            short_label: None,
            input: ActionInput::SelectedFile,
            target: Source {
                service: "fixture".into(),
                method: id.into(),
            },
            capability: None,
            needs_device: false,
            destructive: false,
            timeout_ms: peersus_api::contributions::default_action_timeout(),
            chord: None,
            help: None,
        });
    }
    let mut snapshot = crate::demo::snapshot_sized(3, 0);
    snapshot.plugins.push(PluginInfo {
        id: id.clone(),
        manifest,
        standing: Standing::Ready,
        status: PluginStatus::Running,
        grantable: Default::default(),
        program: None,
        package: None,
        agreed: false,
        from_store: false,
    });
    let action = |which: &str, state, progress| ActionStatus {
        plugin: id.clone(),
        action: which.into(),
        peer: None,
        state,
        progress,
    };
    {
        // Actual Send, no file listed locally and no menu press: two other-client actions must
        // stay distinguishable and reachable for cancellation.
        let mut parallel = snapshot.clone();
        parallel.actions = vec![
            action("one", ActionState::Running, Some((28, 100))),
            action("two", ActionState::Running, Some((74, 100))),
        ];
        let mut send = Window::on(Session::serving(parallel), size);
        until(
            &mut send,
            "both CLI actions without a file selection",
            |window| {
                window.shows("First destination · 28% · Cancel")
                    && window.shows("Second destination · 74% · Cancel")
            },
        );
        assert!(send.shows("CHOOSE FILES"));
        assert!(!send.shows("Third destination"));
        assert!(send.click_text(id.as_str()));
        until(&mut send, "the full menu", |window| {
            window.shows("Third destination")
        });
        assert!(send.click_text("Third destination"));
        assert!(
            send.shows("Third destination"),
            "a row started work without a local file"
        );
        assert!(send.click_text("First destination · 28% · Cancel"));
        assert!(send.shows("Second destination · 74% · Cancel"));
        assert!(
            !send.shows("Third destination"),
            "pressing a running row did not close the full menu"
        );
    }

    let cancelled = "First destination · Cancelled. Check any external result before retrying.";
    let other = action("two", ActionState::Done, None);
    snapshot.actions = vec![action("one", ActionState::Cancelled, None), other.clone()];
    let session = Session::serving(snapshot.clone());
    let link = session.link.as_ref().unwrap().clone();
    let (runner, mut latest) = TestingRunner::new(
        move || {
            // Like Send's viewport, a clipped root gives normal children inherited
            // visibility state before the dropdown moves onto its own overlay.
            rect()
                .expanded()
                .overflow(Overflow::Clip)
                .child(SelectedControls {
                    link: link.clone(),
                    view: View::of(&use_consume::<State<Snapshot>>().read()).into(),
                    path: Some("staged.txt".into()),
                    held: None,
                    palette: crate::theme::DARK,
                })
        },
        size.into(),
        |runner| runner.provide_root_context(|| State::create(snapshot)),
        1.,
    );
    let mut menu = Window {
        runner,
        _session: session,
        scale: 1.,
    };
    menu.settle();
    assert!(menu.click_text(id.as_str()));
    until(&mut menu, "the earlier outcome", |window| {
        window.shows(cancelled)
    });
    for (state, label) in [
        (
            Some(action("one", ActionState::Running, None)),
            "First destination · Working… · Cancel",
        ),
        (
            Some(action("one", ActionState::Running, Some((67, 134)))),
            "First destination · 50% · Cancel",
        ),
        (
            Some(action("one", ActionState::Running, Some((134, 134)))),
            "First destination · Finishing… · Cancel",
        ),
        (
            Some(action("one", ActionState::Done, None)),
            "First destination · Completed",
        ),
        (None, "First destination"),
    ] {
        latest.write().actions = state.into_iter().chain([other.clone()]).collect();
        until(&mut menu, label, |window| window.shows(label));
        assert!(!menu.shows(cancelled));
        assert!(
            menu.shows("Second destination · Completed"),
            "another action's result was changed"
        );
    }
}
