//! The palettes of the approved prototype (warm light, deep green dark), defined once, and the
//! toolkit theme built from the one in use.
//!
//! Components of the toolkit (buttons, inputs, scroll views, progress bars) look their colours
//! up by NAME in the theme's sheet each time they are drawn, so replacing the sheet restyles all
//! of them with no per-component code. Our own elements read the same [`Palette`].
use freya::prelude::*;
use peersus_core::config::{CustomTheme, Theme as Choice, ThemeBase, ThemeColor};

type Rgb = (u8, u8, u8);

/// Every colour the screens use. Two are accents with a meaning: `send` is for what goes out,
/// `receive` for what comes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: Rgb,
    /// Panels and rows.
    pub surface: Rgb,
    /// Hovered rows and raised controls.
    pub raised: Rgb,
    pub header: Rgb,
    pub sidebar: Rgb,
    pub minimise: Rgb,
    pub maximise: Rgb,
    pub close: Rgb,
    pub icons: Rgb,
    pub active_icons: Rgb,
    pub scanner_outline: Rgb,
    pub border: Rgb,
    pub text: Rgb,
    pub muted: Rgb,
    /// The theme's disc surface at the centre of each screen, paired with `portal_text`.
    pub portal: Rgb,
    pub portal_text: Rgb,
    pub send: Rgb,
    pub receive: Rgb,
    /// Online, accepted, done.
    pub good: Rgb,
    /// Refused, failed, offline.
    pub bad: Rgb,
    /// Risk notices, distinct from errors and never recoloured by the personal accent.
    pub warning: Rgb,
    pub warning_background: Rgb,
    /// The solid button.
    pub button: Rgb,
    pub button_text: Rgb,
}

pub const LIGHT: Palette = Palette {
    background: (0xf9, 0xf7, 0xf3),
    surface: (0xf1, 0xee, 0xe8),
    raised: (0xfd, 0xfc, 0xfa),
    header: (0xfd, 0xfc, 0xfa),
    sidebar: (0xf9, 0xf7, 0xf3),
    minimise: (0xc9, 0x8a, 0x1d),
    maximise: (0x3f, 0x93, 0x69),
    close: (0xd0, 0x4a, 0x47),
    icons: (0x8a, 0x8f, 0x8c),
    active_icons: (0x1b, 0x23, 0x25),
    scanner_outline: (255, 255, 255),
    border: (0xe0, 0xdc, 0xd3),
    text: (0x1b, 0x23, 0x25),
    muted: (0x8a, 0x8f, 0x8c),
    portal: (0xfd, 0xfc, 0xfa),
    portal_text: (0x1b, 0x23, 0x25),
    send: (0xf4, 0x6b, 0x52),
    receive: (0x5a, 0x9c, 0xe0),
    good: (0x4f, 0xa3, 0x7c),
    bad: (0xd9, 0x4f, 0x4f),
    warning: (0x60, 0x42, 0x08),
    warning_background: (0xff, 0xf5, 0xd5),
    button: (0x16, 0x1b, 0x1d),
    button_text: (0xf9, 0xf7, 0xf3),
};

/// The dark theme is made for OLED screens: the page is pure black, so that a pixel with nothing
/// to show is switched off and not merely dim, and the things lifted above it are only a few
/// steps above black and carry no tint, because a tint would light every pixel of every card.
pub const DARK: Palette = Palette {
    background: (0x00, 0x00, 0x00),
    surface: (0x0c, 0x0c, 0x0c),
    raised: (0x14, 0x14, 0x14),
    header: (0x00, 0x00, 0x00),
    sidebar: (0x00, 0x00, 0x00),
    minimise: (0xe3, 0xa8, 0x3c),
    maximise: (0x4f, 0xa3, 0x7c),
    close: (0xe0, 0x6a, 0x6a),
    icons: (0x8b, 0x99, 0x97),
    active_icons: (0xee, 0xf2, 0xee),
    scanner_outline: (255, 255, 255),
    border: (0x26, 0x26, 0x26),
    text: (0xee, 0xf2, 0xee),
    muted: (0x8b, 0x99, 0x97),
    portal: (0x04, 0x04, 0x04),
    portal_text: (0xf2, 0xf5, 0xf2),
    send: (0xf4, 0x6b, 0x52),
    receive: (0x5a, 0x9c, 0xe0),
    good: (0x4f, 0xa3, 0x7c),
    bad: (0xe0, 0x6a, 0x6a),
    warning: (0xf9, 0xda, 0x89),
    warning_background: (0x33, 0x28, 0x0e),
    button: (0xf2, 0xf5, 0xf0),
    button_text: (0x16, 0x1b, 0x1d),
};

impl Palette {
    /// The palette for a choice. `System` follows the platform, which is dark until the
    /// platform's preference is read (the toolkit exposes it per window, not here).
    pub fn of(choice: Choice, platform_dark: bool) -> Palette {
        match choice {
            Choice::Dark => DARK,
            Choice::Custom => DARK,
            Choice::Light => LIGHT,
            Choice::System if platform_dark => DARK,
            Choice::System => LIGHT,
        }
    }

    /// Navigation icons have one neutral colour, not an arbitrary hue per destination.
    pub fn of_tab(self, _tab: crate::view::SettingsTab) -> Rgb {
        self.icons
    }

    pub fn custom(custom: &CustomTheme) -> Self {
        let mut palette = match custom.base {
            ThemeBase::Dark => DARK,
            ThemeBase::Light => LIGHT,
        };
        for (&role, &rgb) in &custom.colors {
            *palette.role_mut(role) = (rgb[0], rgb[1], rgb[2]);
        }
        palette
    }

    pub fn color(mut self, role: ThemeColor) -> [u8; 3] {
        let &(r, g, b) = &*self.role_mut(role);
        [r, g, b]
    }

    fn role_mut(&mut self, role: ThemeColor) -> &mut Rgb {
        match role {
            ThemeColor::Background => &mut self.background,
            ThemeColor::Surface => &mut self.surface,
            ThemeColor::Raised => &mut self.raised,
            ThemeColor::Header => &mut self.header,
            ThemeColor::Sidebar => &mut self.sidebar,
            ThemeColor::Border => &mut self.border,
            ThemeColor::Text => &mut self.text,
            ThemeColor::Muted => &mut self.muted,
            ThemeColor::Portal => &mut self.portal,
            ThemeColor::PortalText => &mut self.portal_text,
            ThemeColor::Send => &mut self.send,
            ThemeColor::Receive => &mut self.receive,
            ThemeColor::Good => &mut self.good,
            ThemeColor::Bad => &mut self.bad,
            ThemeColor::Warning => &mut self.warning,
            ThemeColor::WarningBackground => &mut self.warning_background,
            ThemeColor::Button => &mut self.button,
            ThemeColor::ButtonText => &mut self.button_text,
            ThemeColor::Minimise => &mut self.minimise,
            ThemeColor::Maximise => &mut self.maximise,
            ThemeColor::Close => &mut self.close,
            ThemeColor::Icons => &mut self.icons,
            ThemeColor::ActiveIcons => &mut self.active_icons,
            ThemeColor::ScannerOutline => &mut self.scanner_outline,
        }
    }

    /// The palette for a choice with the accent the owner holds. The value it starts with is the
    /// owner's own default, not the brand colour, so it is read as "not chosen" and leaves the
    /// palette alone; any other value is the user's and recolours what goes out. The owner has
    /// no "not chosen" of its own, so a user who picks exactly the starting colour is not
    /// recoloured: that needs the stored accent to be optional, which is the owner's to change.
    pub fn of_choice(choice: Choice, platform_dark: bool, accent: [u8; 3]) -> Palette {
        let base = Self::of(choice, platform_dark);
        if accent == peersus_core::config::Settings::default().accent {
            base
        } else {
            base.with_accent((accent[0], accent[1], accent[2]))
        }
    }

    /// This palette with `accent` for what goes out. A colour that would be lost against the
    /// background is moved toward the text colour until it can be seen: the accent underlines
    /// the active tab and rings the portal, and must never vanish.
    pub fn with_accent(self, accent: Rgb) -> Palette {
        let mut send = accent;
        let mut step = 0;
        while contrast_ratio(send, self.background) < 3.0 && step < 16 {
            send = toward(send, self.text, 0.15);
            step += 1;
        }
        Palette { send, ..self }
    }
}

/// Contrast ratio of two colours (1 to 21), as the accessibility guidelines define it.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let channel = |v: u8| {
        let v = f32::from(v) / 255.;
        if v <= 0.039_28 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let luminance = |(r, g, b): Rgb| 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// `from` moved `part` of the way (0 to 1) toward `to`.
fn toward(from: Rgb, to: Rgb, part: f32) -> Rgb {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * part).round() as u8;
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}


fn colour((r, g, b): Rgb) -> Color {
    Color::from_rgb(r, g, b)
}

fn switch_preferences(palette: Palette) -> SwitchColorsThemePreference {
    let on_track = palette.good;
    let light_thumb = (0xff, 0xff, 0xff);
    let dark_thumb = (0x16, 0x1b, 0x1d);
    let on_thumb = if contrast_ratio(on_track, light_thumb) >= 3.0 { light_thumb } else { dark_thumb };
    SwitchColorsThemePreference {
        background: Preference::Specific(colour(palette.border)),
        thumb_background: Preference::Specific(colour(palette.muted)),
        toggled_background: Preference::Specific(colour(on_track)),
        toggled_thumb_background: Preference::Specific(colour(on_thumb)),
        focus_border_fill: Preference::Specific(colour(palette.send)),
    }
}

/// The theme every window of this program is drawn with.
///
/// It starts from the toolkit's own theme for the same brightness and replaces only the colour
/// sheet AFTERWARDS. A theme built empty has no entry for any component, and the first button
/// drawn would panic; and the toolkit's builder lets the operating system's accent colour
/// overwrite the brand colours, which the assignment below puts back.
pub fn theme(palette: Palette, dark: bool) -> Theme {
    let mut theme = if dark { dark_theme() } else { light_theme() };
    let base = theme.colors.clone();
    theme.colors = ColorsSheet {
        primary: colour(palette.button),
        secondary: colour(palette.button),
        tertiary: colour(palette.button),
        background: colour(palette.background),
        surface_primary: colour(palette.surface),
        surface_secondary: colour(palette.raised),
        surface_tertiary: colour(palette.raised),
        border: colour(palette.border),
        text_primary: colour(palette.text),
        text_secondary: colour(palette.muted),
        text_inverse: colour(palette.button_text),
        ..base
    };
    // The default switch uses primary and secondary for thumb/track. Our monochrome
    // button palette makes those identical, hiding the thumb entirely when enabled.
    theme.set("switch", switch_preferences(palette));
    theme
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_switch_keeps_its_thumb_distinct_from_the_track() {
        for palette in [LIGHT, DARK] {
            let colors = switch_preferences(palette);
            let Preference::Specific(track) = colors.toggled_background else { panic!("the enabled track must not inherit the monochrome button color") };
            let Preference::Specific(thumb) = colors.toggled_thumb_background else { panic!("the enabled thumb must not inherit the monochrome button color") };
            assert_ne!(colors.background, colors.toggled_background);
            assert_ne!(colors.thumb_background, colors.background);
            assert_ne!(track, thumb);
            assert!(contrast_ratio((track.r(), track.g(), track.b()), (thumb.r(), thumb.g(), thumb.b())) >= 3.0);
        }
    }

    #[test]
    fn an_accent_recolours_only_what_goes_out() {
        for base in [LIGHT, DARK] {
            // A colour that is already visible on both backgrounds, so nothing moves it.
            let tinted = base.with_accent((0xe0, 0x30, 0x90));
            assert_eq!(tinted.send, (0xe0, 0x30, 0x90));
            // Everything else is the theme's own: what comes in keeps its colour, and so do the
            // surfaces and the text.
            assert_eq!(Palette { send: base.send, ..tinted }, base);
        }
    }

    #[test]
    fn the_value_the_owner_starts_with_leaves_the_brand_colours_alone() {
        // The owner stores an accent from the start, and it is not the brand colour. Applying it
        // would repaint the application of everyone who never chose one, so the starting value is
        // read as "not chosen". The limit: someone who picks exactly that colour is not recoloured.
        let starting = peersus_core::config::Settings::default().accent;
        assert_eq!(Palette::of_choice(Choice::Dark, true, starting), DARK);
        assert_eq!(Palette::of_choice(Choice::Light, true, starting), LIGHT);
        let chosen = Palette::of_choice(Choice::Dark, true, [0xe0, 0x30, 0x90]);
        assert_eq!(chosen.send, (0xe0, 0x30, 0x90));
    }

    #[test]
    fn an_accent_too_close_to_the_background_is_pulled_away_from_it() {
        // A user can pick a colour that vanishes on the page: the accent marks the active tab and
        // the portal ring, so it has to stay visible whatever they choose.
        for base in [LIGHT, DARK] {
            for chosen in [base.background, base.surface, (0, 0, 0), (255, 255, 255)] {
                let tinted = base.with_accent(chosen);
                let ratio = contrast_ratio(tinted.send, base.background);
                assert!(ratio >= 3.0, "{chosen:?} on {:?} gives contrast {ratio}", base.background);
            }
        }
    }

    #[test]
    fn the_dark_theme_is_pure_black_where_the_page_is_so_that_an_oled_pixel_is_off() {
        assert_eq!(DARK.background, (0, 0, 0));
        // Raised things are only a little above black, never a coloured grey that lights the panel.
        for surface in [DARK.surface, DARK.raised, DARK.portal] {
            assert!(surface.0 < 0x20 && surface.1 < 0x20 && surface.2 < 0x20, "{surface:?} is too bright for black");
            // And they are neutral: a tint here would be lit on every pixel of every card.
            assert!(surface.0 == surface.1 && surface.1 == surface.2, "{surface:?} is tinted");
        }
    }

    #[test]
    fn portal_surface_follows_the_theme_without_losing_text_contrast() {
        for (choice, platform_dark) in [(Choice::Light, true), (Choice::System, false)] {
            let palette = Palette::of(choice, platform_dark);
            assert!(contrast_ratio(palette.portal, palette.text) >= 4.5);
            assert!(contrast_ratio(palette.portal_text, palette.portal) >= 4.5);
        }
        for palette in [LIGHT, DARK] {
            assert!(contrast_ratio(palette.portal_text, palette.portal) >= 4.5);
        }
    }

    #[test]
    fn what_is_read_on_the_black_page_is_still_easy_to_read() {
        assert!(contrast_ratio(DARK.text, DARK.background) >= 12.0);
        assert!(contrast_ratio(DARK.muted, DARK.background) >= 4.5);
        assert!(contrast_ratio(DARK.text, DARK.surface) >= 12.0);
        // The edges that separate cards from the page can be seen against it.
        assert!(contrast_ratio(DARK.border, DARK.background) >= 1.3);
    }

    #[test]
    fn navigation_uses_neutral_icons_with_a_distinct_selected_state() {
        use crate::view::SettingsTab;
        for palette in [DARK, LIGHT] {
            for tab in SettingsTab::ALL {
                assert_eq!(palette.of_tab(tab), palette.icons);
            }
            assert_ne!(palette.icons, palette.active_icons);
            assert!(contrast_ratio(palette.active_icons, palette.sidebar) >= 4.5);
            assert!(contrast_ratio(palette.icons, palette.background) >= 3.0);
        }
    }

    #[test]
    fn a_custom_header_does_not_recolour_hovered_rows_or_the_sidebar() {
        let mut custom = CustomTheme::default();
        custom.colors.insert(ThemeColor::Header, [1, 2, 3]);
        let palette = Palette::custom(&custom);
        assert_eq!(palette.header, (1, 2, 3));
        assert_eq!(palette.raised, DARK.raised);
        assert_eq!(palette.sidebar, DARK.sidebar);
        custom.colors.insert(ThemeColor::ActiveIcons, [4, 5, 6]);
        assert_eq!(Palette::custom(&custom).active_icons, (4, 5, 6));
    }

    #[test]
    fn camera_detection_outline_keeps_its_contrast_when_panel_borders_change() {
        for base in [ThemeBase::Dark, ThemeBase::Light] {
            let mut custom = CustomTheme { base, ..Default::default() };
            custom.colors.insert(ThemeColor::Border, [0, 0, 0]);
            assert_eq!(Palette::custom(&custom).scanner_outline, (255, 255, 255));
            custom.colors.insert(ThemeColor::ScannerOutline, [120, 240, 180]);
            let palette = Palette::custom(&custom);
            assert_eq!(palette.scanner_outline, (120, 240, 180));
            assert_eq!(palette.border, (0, 0, 0));
        }
    }

    #[test]
    fn system_follows_what_the_platform_prefers_and_an_explicit_choice_does_not() {
        // The two ends of `System`: what the platform says is what is drawn, so a phone that goes to night is dark and one that goes
        // to day is light, and a window is never dark whatever the system says.
        assert_eq!(Palette::of(Choice::System, true), DARK);
        assert_eq!(Palette::of(Choice::System, false), LIGHT);
        // A choice the person made is theirs, whatever the platform prefers.
        for platform_dark in [true, false] {
            assert_eq!(Palette::of(Choice::Dark, platform_dark), DARK);
            assert_eq!(Palette::of(Choice::Light, platform_dark), LIGHT);
        }
    }
}
