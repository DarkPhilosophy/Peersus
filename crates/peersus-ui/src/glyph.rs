//! Which picture stands for which place, and how large pictures are drawn. One definition for the
//! whole application, so the sidebar, the tabs and the header never choose different pictures for
//! the same thing, and a size is never a number typed into a screen.
//!
//! The pictures are small SVG files in `assets/icons`, with no colour of their own (`currentColor`), so that the theme
//! colours them. Most are drawn from IconPark (Apache-2.0, Copyright 2019-present Bytedance Inc.), changed by
//! `assets/import_icons.py`: split by layer, colours replaced. The licence is in `assets/IconPark-LICENSE` and is shown in About.
//!
//! A picture may have more layers than its line, each in a file of its own so that they can be coloured apart without any
//! colour ever being written in a file: `<name>-tint.svg` is the main shape and takes the person's accent, `<name>-soft.svg`
//! a second shape, and `<name>-cut.svg` the strokes drawn inside the shapes (white in the source), in the colour of the line.

use freya::prelude::*;

use crate::view::SettingsTab;

/// Every picture the application has, by name, as the bytes of its SVG file.
pub const ALL: &[(&str, &[u8])] = &[
    ("send", include_bytes!("../assets/icons/send.svg")),
    ("receive", include_bytes!("../assets/icons/receive.svg")),
    ("peers", include_bytes!("../assets/icons/peers.svg")),
    ("plugins", include_bytes!("../assets/icons/plugins.svg")),
    ("settings", include_bytes!("../assets/icons/settings.svg")),
    ("about", include_bytes!("../assets/icons/about.svg")),
    ("collapse", include_bytes!("../assets/icons/collapse.svg")),
    ("expand", include_bytes!("../assets/icons/expand.svg")),
    ("overview", include_bytes!("../assets/icons/overview.svg")),
    ("device", include_bytes!("../assets/icons/device.svg")),
    (
        "appearance",
        include_bytes!("../assets/icons/appearance.svg"),
    ),
    ("network", include_bytes!("../assets/icons/network.svg")),
    (
        "permissions",
        include_bytes!("../assets/icons/permissions.svg"),
    ),
    ("minimise", include_bytes!("../assets/icons/minimise.svg")),
    ("maximise", include_bytes!("../assets/icons/maximise.svg")),
    ("close", include_bytes!("../assets/icons/close.svg")),
    ("history", include_bytes!("../assets/icons/history.svg")),
    ("transfers", include_bytes!("../assets/icons/transfers.svg")),
    ("pointer", include_bytes!("../assets/icons/pointer.svg")),
    ("backspace", include_bytes!("../assets/icons/backspace.svg")),
];

/// The layers of a picture other than its line, by picture name.
pub struct Layers {
    pub tint: Option<&'static [u8]>,
    pub soft: Option<&'static [u8]>,
    pub cut: Option<&'static [u8]>,
}

type LayerBytes = Option<&'static [u8]>;

/// The layers a picture has, or none for a picture that is its line alone.
pub fn layers_of(name: &str) -> Layers {
    let (tint, soft, cut): (LayerBytes, LayerBytes, LayerBytes) = match name {
        "send" => (
            Some(include_bytes!("../assets/icons/send-tint.svg")),
            None,
            Some(include_bytes!("../assets/icons/send-cut.svg")),
        ),
        "receive" => (
            Some(include_bytes!("../assets/icons/receive-tint.svg")),
            None,
            Some(include_bytes!("../assets/icons/receive-cut.svg")),
        ),
        "peers" => (
            Some(include_bytes!("../assets/icons/peers-tint.svg")),
            None,
            None,
        ),
        "overview" => (
            Some(include_bytes!("../assets/icons/overview-tint.svg")),
            None,
            None,
        ),
        "device" => (
            Some(include_bytes!("../assets/icons/device-tint.svg")),
            None,
            None,
        ),
        "appearance" => (
            Some(include_bytes!("../assets/icons/appearance-tint.svg")),
            Some(include_bytes!("../assets/icons/appearance-soft.svg")),
            Some(include_bytes!("../assets/icons/appearance-cut.svg")),
        ),
        "network" => (
            Some(include_bytes!("../assets/icons/network-tint.svg")),
            None,
            None,
        ),
        "plugins" => (
            Some(include_bytes!("../assets/icons/plugins-tint.svg")),
            None,
            None,
        ),
        "permissions" => (
            Some(include_bytes!("../assets/icons/permissions-tint.svg")),
            None,
            Some(include_bytes!("../assets/icons/permissions-cut.svg")),
        ),
        "history" => (
            Some(include_bytes!("../assets/icons/history-tint.svg")),
            None,
            None,
        ),
        "transfers" => (
            Some(include_bytes!("../assets/icons/transfers-tint.svg")),
            None,
            None,
        ),
        "pointer" => (
            Some(include_bytes!("../assets/icons/pointer-tint.svg")),
            None,
            None,
        ),
        "about" => (
            Some(include_bytes!("../assets/icons/about-tint.svg")),
            None,
            Some(include_bytes!("../assets/icons/about-cut.svg")),
        ),
        "settings" => (
            Some(include_bytes!("../assets/icons/settings-tint.svg")),
            Some(include_bytes!("../assets/icons/settings-soft.svg")),
            Some(include_bytes!("../assets/icons/settings-cut.svg")),
        ),
        _ => (None, None, None),
    };
    Layers { tint, soft, cut }
}

/// The names of the pictures that have layers.
pub const LAYERED: [&str; 14] = [
    "send",
    "receive",
    "peers",
    "transfers",
    "overview",
    "device",
    "appearance",
    "network",
    "plugins",
    "permissions",
    "history",
    "about",
    "settings",
    "pointer",
];

fn named(name: &str) -> &'static [u8] {
    ALL.iter()
        .find(|(n, _)| *n == name)
        .map(|(_, bytes)| *bytes)
        .expect("a picture of that name")
}

/// The name of the picture of a tab of Settings.
pub fn name_of_tab(tab: SettingsTab) -> &'static str {
    match tab {
        SettingsTab::Overview => "overview",
        SettingsTab::Device => "device",
        SettingsTab::Appearance => "appearance",
        SettingsTab::Network => "network",
        SettingsTab::Extensions => "plugins",
        SettingsTab::Permissions => "permissions",
        SettingsTab::About => "about",
    }
}

/// The picture of a tab of Settings.
pub fn of_tab(tab: SettingsTab) -> &'static [u8] {
    named(name_of_tab(tab))
}

/// The picture with the given name, for the controls that are not a place.
pub fn by_name(name: &str) -> &'static [u8] {
    named(name)
}

/// The picture an extension named, when the application has one by that name, or the generic picture of an extension: a name it
/// does not know is never a failure.
pub fn known_or_generic(name: Option<&str>) -> &'static str {
    name.and_then(|name| ALL.iter().find(|(known, _)| *known == name))
        .map_or("plugins", |(known, _)| known)
}

/// A picture drawn `size` pixels square in `colour`.
pub fn draw(picture: &'static [u8], size: f32, colour: (u8, u8, u8)) -> Element {
    SvgViewer::new(picture)
        .color(colour)
        .width(Size::px(size))
        .height(Size::px(size))
        .into()
}

/// A picture drawn `size` pixels square in layers: the main shape in `accent`, a second shape in `soft`, over them the strokes inside the
/// shapes in `detail`, and over all of it the outline in `line`. A picture with no layers is drawn as its outline alone.
pub fn draw_duo(
    name: &str,
    size: f32,
    line: (u8, u8, u8),
    detail: (u8, u8, u8),
    accent: (u8, u8, u8),
    soft: (u8, u8, u8),
) -> Element {
    let layers = layers_of(name);
    let at = || Position::new_absolute().top(0.).left(0.);
    let mut stack = rect().width(Size::px(size)).height(Size::px(size));
    // The shapes first, then the strokes inside them and the outline over all of it.
    for (bytes, colour) in [
        (layers.tint, accent),
        (layers.soft, soft),
        (layers.cut, detail),
    ] {
        if let Some(bytes) = bytes {
            stack = stack.child(rect().position(at()).child(draw(bytes, size, colour)));
        }
    }
    stack
        .child(rect().position(at()).child(draw(named(name), size, line)))
        .into()
}

/// The second shape's colour: the accent lightened toward white, so the second shape is lighter than the main one on a dark page and
/// on a light one alike. A mix toward the text colour would darken it on a light page, where the text is nearly black.
pub fn soft_of(accent: (u8, u8, u8)) -> (u8, u8, u8) {
    let mix = |a: u8| (f32::from(a) + (255. - f32::from(a)) * 0.5).round() as u8;
    (mix(accent.0), mix(accent.1), mix(accent.2))
}

/// A layered picture in the colours of `palette`: the line in `ink`, and the main shape in the accent the person chose, as strong as
/// `strength` (0 to 1) of the way from the page to the accent. This is the one place that decides which colour a layer takes, so that
/// the sidebar, the menu and the bars never differ. The line is always one that can be told from the shape under it (see `picture_on`).
pub fn picture_at(
    name: &str,
    size: f32,
    palette: &crate::theme::Palette,
    ink: (u8, u8, u8),
    strength: f32,
) -> Element {
    picture_on(
        name,
        size,
        dim(palette.send, palette.background, strength),
        ink,
        palette.background,
    )
}

/// A layered picture at full strength: the accent as chosen.
pub fn picture(
    name: &str,
    size: f32,
    palette: &crate::theme::Palette,
    ink: (u8, u8, u8),
) -> Element {
    picture_at(name, size, palette, ink, 1.)
}

/// `colour` moved from `page` toward itself, `strength` of the way (0 is the page, 1 is the colour).
pub fn dim(colour: (u8, u8, u8), page: (u8, u8, u8), strength: f32) -> (u8, u8, u8) {
    let s = strength.clamp(0., 1.);
    let mix = |c: u8, p: u8| (f32::from(p) + (f32::from(c) - f32::from(p)) * s).round() as u8;
    (
        mix(colour.0, page.0),
        mix(colour.1, page.1),
        mix(colour.2, page.2),
    )
}

/// The colour of the line of a picture that is not the one in use: the secondary text colour moved a third of the way toward the
/// primary one, which is what it takes for the line to be read on a light page (the secondary colour alone is 3:1 there) and costs
/// nothing on a dark one.
pub fn resting_ink(palette: &crate::theme::Palette) -> (u8, u8, u8) {
    dim(palette.text, palette.muted, 0.35)
}

/// The colour a mark is drawn in on a disc of `colour`: black or white, whichever is read better on it. One rule for every round
/// control of the header, so that none is drawn in a colour that cannot be seen on its own disc.
pub fn ink_on(colour: (u8, u8, u8)) -> (u8, u8, u8) {
    if crate::theme::contrast_ratio(colour, (0, 0, 0))
        >= crate::theme::contrast_ratio(colour, (255, 255, 255))
    {
        (0, 0, 0)
    } else {
        (255, 255, 255)
    }
}

/// The colour of the OUTLINE of a picture: `ink` when it can be told from the `page` it lies on, and black or white when it cannot. The
/// outline touches the page and nothing else.
pub fn line_on_page(ink: (u8, u8, u8), page: (u8, u8, u8)) -> (u8, u8, u8) {
    if crate::theme::contrast_ratio(ink, page) >= 3.0 {
        ink
    } else {
        ink_on(page)
    }
}

/// The colour of the DETAIL drawn inside a picture's shapes (an arrow in a tray, the dot of a question mark): the line's own colour when it
/// can be told from the `fill` it is drawn over, and black or white when it cannot. A detail in the colour of the shape under it vanishes,
/// which turned an arrow into a plain tray and a question mark into a plain disc.
pub fn detail_on(ink: (u8, u8, u8), fill: (u8, u8, u8)) -> (u8, u8, u8) {
    if crate::theme::contrast_ratio(ink, fill) >= 3.0 {
        ink
    } else {
        ink_on(fill)
    }
}

/// A layered picture with its main shape in `fill`, on a `page`: the outline in `ink` (kept to something the page shows), and the detail
/// inside the shapes in a colour the fill shows. The two touch different things, so they are two colours; one colour for both is what made
/// an outline vanish on the page when it was chosen to read on the shape, and a detail vanish on the shape when it was chosen for the page.
pub fn picture_on(
    name: &str,
    size: f32,
    fill: (u8, u8, u8),
    ink: (u8, u8, u8),
    page: (u8, u8, u8),
) -> Element {
    let line = line_on_page(ink, page);
    draw_duo(name, size, line, detail_on(line, fill), fill, soft_of(fill))
}

/// The size of a picture beside a name in a list.
pub const LIST: f32 = 22.;
/// The size of a picture alone in a narrow bar.
pub const BAR: f32 = 24.;
/// The size of a picture on a round control in the header.
pub const HEADER: f32 = 18.;
/// The size of the mark on a window button: small enough to sit inside the disc with room round it.
pub const WINDOW: f32 = 14.;
/// The size of the picture on the Settings button: it is a picture of several parts and not one stroke, so it needs more of the disc
/// than the single mark of a window button to be read, about two thirds of it.
pub const HEADER_MARK: f32 = 22.;

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: &[u8]) -> &str {
        std::str::from_utf8(bytes).expect("an SVG file is text")
    }

    /// The grid a picture is drawn on, read from its `viewBox`.
    fn grid(svg: &str) -> &str {
        let start = svg.find("viewBox=\"").expect("an SVG with a viewBox") + "viewBox=\"".len();
        &svg[start..start + svg[start..].find('"').expect("a closed viewBox")]
    }

    /// Every file the application draws, with its name, the line first and then each layer.
    fn files() -> Vec<(String, &'static [u8])> {
        let mut all: Vec<(String, &'static [u8])> = ALL
            .iter()
            .map(|(name, bytes)| (name.to_string(), *bytes))
            .collect();
        for name in LAYERED {
            let layers = layers_of(name);
            for (suffix, bytes) in [
                ("tint", layers.tint),
                ("soft", layers.soft),
                ("cut", layers.cut),
            ] {
                if let Some(bytes) = bytes {
                    all.push((format!("{name}-{suffix}"), bytes));
                }
            }
        }
        all
    }

    #[test]
    fn every_file_takes_its_colour_from_the_theme_and_has_none_of_its_own() {
        for (name, bytes) in files() {
            let svg = text(bytes);
            assert!(
                svg.contains("currentColor"),
                "{name}: has no way to take the theme's colour"
            );
            // A colour written into a file would ignore the theme, in every one of them.
            assert!(
                !svg.contains("fill=\"#") && !svg.contains("stroke=\"#"),
                "{name}: has a colour of its own"
            );
            assert!(
                !svg.to_lowercase().contains("rgb("),
                "{name}: has a colour of its own"
            );
            for word in ["black", "white"] {
                assert!(
                    !svg.contains(&format!("=\"{word}\"")),
                    "{name}: has the colour {word}"
                );
            }
        }
    }

    #[test]
    fn the_layers_of_a_picture_lie_on_the_same_grid_as_its_line() {
        for name in LAYERED {
            let line = grid(text(named(name)));
            let layers = layers_of(name);
            for (which, bytes) in [
                ("tint", layers.tint),
                ("soft", layers.soft),
                ("cut", layers.cut),
            ] {
                if let Some(bytes) = bytes {
                    assert_eq!(
                        grid(text(bytes)),
                        line,
                        "{name}-{which}: not on the grid of its line"
                    );
                }
            }
        }
    }

    #[test]
    fn no_picture_is_empty_and_no_two_are_the_same_drawing() {
        let all = files();
        for (name, bytes) in &all {
            assert!(bytes.len() > 120, "{name}: draws almost nothing");
        }
        // Two pictures may share an outline (a tray for Send and for Receive) but not what is drawn in it: compare the whole
        // of each picture, its line and every layer, and not one file at a time.
        let whole = |name: &str| -> Vec<&'static [u8]> {
            let layers = layers_of(name);
            let mut parts = vec![named(name)];
            parts.extend([layers.tint, layers.soft, layers.cut].into_iter().flatten());
            parts
        };
        for (i, a) in ALL.iter().enumerate() {
            for b in ALL.iter().skip(i + 1) {
                assert_ne!(
                    whole(a.0),
                    whole(b.0),
                    "{} and {} are the same drawing",
                    a.0,
                    b.0
                );
            }
        }
    }

    #[test]
    fn every_picture_of_a_place_has_a_main_shape_to_take_the_accent() {
        for name in LAYERED {
            assert!(
                layers_of(name).tint.is_some(),
                "{name}: nothing to colour with the accent"
            );
        }
    }

    #[test]
    fn the_second_shape_is_lighter_than_the_accent_on_a_dark_page_and_on_a_light_one() {
        let light = |c: (u8, u8, u8)| u32::from(c.0) + u32::from(c.1) + u32::from(c.2);
        for base in [crate::theme::DARK, crate::theme::LIGHT] {
            // The accents a person can choose, and the brand colour, each as the palette would hold it.
            for chosen in [
                [0xf4, 0x6b, 0x52],
                [0x5b, 0x9c, 0xe0],
                [0xe8, 0xba, 0x4c],
                [0xba, 0x84, 0xe8],
            ] {
                let palette = base.with_accent((chosen[0], chosen[1], chosen[2]));
                let soft = soft_of(palette.send);
                assert!(
                    light(soft) > light(palette.send),
                    "{soft:?} is not lighter than {:?}",
                    palette.send
                );
            }
        }
        // Even black has a lighter second shape, so no accent can make the two layers the same.
        assert!(light(soft_of((0, 0, 0))) > 0);
    }

    #[test]
    fn the_line_of_a_picture_not_in_use_can_be_read_on_a_dark_page_and_on_a_light_one() {
        for palette in [crate::theme::DARK, crate::theme::LIGHT] {
            for page in [palette.background, palette.sidebar, palette.surface] {
                let ratio = crate::theme::contrast_ratio(resting_ink(&palette), page);
                assert!(
                    ratio >= 4.5,
                    "the resting line gives only {ratio} on {page:?}"
                );
            }
        }
    }

    #[test]
    fn dim_goes_from_the_page_to_the_colour_and_stays_between() {
        let (colour, page) = ((200, 100, 50), (0, 0, 0));
        assert_eq!(dim(colour, page, 0.), page);
        assert_eq!(dim(colour, page, 1.), colour);
        assert_eq!(dim(colour, page, 0.5), (100, 50, 25));
        // Out of range is held at the ends, never past them.
        assert_eq!(dim(colour, page, -3.), page);
        assert_eq!(dim(colour, page, 9.), colour);
    }

    #[test]
    fn every_tab_has_its_own_picture() {
        for (i, a) in SettingsTab::ALL.iter().enumerate() {
            for b in SettingsTab::ALL.iter().skip(i + 1) {
                assert_ne!(
                    of_tab(*a),
                    of_tab(*b),
                    "{a:?} and {b:?} look alike among the tabs"
                );
            }
        }
    }

    #[test]
    fn pictures_are_smaller_in_the_header_than_beside_a_name_and_a_bar_holds_one() {
        const { assert!(HEADER < LIST) };
        const { assert!(LIST <= BAR) };
        // A picture alone in the narrow bar fits it with room around.
        const { assert!(BAR + 2. * 12. <= crate::view::Sidebar::NARROW) };
    }

    #[test]
    fn the_mark_on_a_round_control_can_be_read_whatever_colour_the_disc_is() {
        // The accents a person can choose, and the colours of the three window buttons, on both themes.
        for palette in [crate::theme::DARK, crate::theme::LIGHT] {
            for disc in [
                palette.send,
                palette.minimise,
                palette.maximise,
                palette.close,
            ] {
                let ratio = crate::theme::contrast_ratio(ink_on(disc), disc);
                assert!(ratio >= 4.5, "the mark gives only {ratio} on {disc:?}");
            }
        }
    }

    #[test]
    fn the_settings_button_is_no_smaller_than_the_window_buttons_beside_it() {
        // Same disc for all four, and the picture of Settings fills at least as much of it as a window mark does.
        const { assert!(HEADER_MARK > WINDOW) };
        const { assert!(HEADER_MARK <= 0.75 * crate::HEADER_ROUND) };
        // The mark of Settings is the largest picture on a round control: it is the one made of several parts.
        const { assert!(HEADER_MARK >= HEADER) };
    }

    #[test]
    fn the_outline_is_read_on_the_page_and_the_detail_inside_is_read_on_the_shape_it_is_drawn_over()
    {
        // What went wrong, twice: one colour for both. Chosen to read on the shape, the outline became black on a black page and the sidebar
        // emptied; chosen for the page, the detail became the colour of the shape and an arrow turned into a plain tray.
        for base in [crate::theme::DARK, crate::theme::LIGHT] {
            for chosen in [
                [0xf4, 0x6b, 0x52],
                [0x5b, 0x9c, 0xe0],
                [0x4f, 0xc4, 0x8c],
                [0xe8, 0xba, 0x4c],
                [0xba, 0x84, 0xe8],
                [0xf0, 0x78, 0xa0],
            ] {
                let palette = base.with_accent((chosen[0], chosen[1], chosen[2]));
                for fill in [palette.send, palette.receive] {
                    for ink in [
                        palette.text,
                        resting_ink(&palette),
                        palette.active_icons,
                        fill,
                    ] {
                        let line = line_on_page(ink, palette.background);
                        let detail = detail_on(line, fill);
                        let outline = crate::theme::contrast_ratio(line, palette.background);
                        let inside = crate::theme::contrast_ratio(detail, fill);
                        assert!(
                            outline >= 3.0,
                            "{ink:?} outline on the page gives only {outline}"
                        );
                        assert!(
                            inside >= 2.9,
                            "{ink:?} detail over {fill:?} gives only {inside}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn an_outline_that_shows_on_its_page_is_left_as_it_is_and_one_that_does_not_is_moved() {
        assert_eq!(line_on_page((255, 255, 255), (0, 0, 0)), (255, 255, 255));
        assert_eq!(line_on_page((0, 0, 0), (255, 255, 255)), (0, 0, 0));
        for page in [(0, 0, 0), (255, 255, 255), (244, 107, 82)] {
            assert_ne!(
                line_on_page(page, page),
                page,
                "the colour of the page was kept as the outline on it"
            );
        }
    }

    #[test]
    fn the_sidebar_outline_keeps_its_colour_when_only_the_detail_inside_has_to_change() {
        // The real case that emptied the sidebar: a light grey outline on a black page, over a coral shape. The outline reads on the page, so it
        // stays; the detail inside does not read on the coral, so it changes. One rule for both would have moved the outline to black.
        let (fill, page) = ((0xf4, 0x6b, 0x52), (0, 0, 0));
        let ink = resting_ink(&crate::theme::DARK);
        assert_eq!(line_on_page(ink, page), ink);
        assert_ne!(detail_on(ink, fill), ink);
    }
}
