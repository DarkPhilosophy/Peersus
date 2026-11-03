//! The accent: the one colour the person chooses. What goes out is drawn in it, and so are the main shapes of the pictures, so a
//! choice here changes the look of the whole application at once. A few named colours, each drawn in its own colour, with the
//! one in use marked, and not a field for a number.
//!
//! Which colours there are, and which one a saved value is, is [`Choices`], which knows no toolkit and is tested on its own.
use freya::prelude::*;
use peersus_core::{Settings, config::SettingsPatch, model::Command};

use crate::{i18n, link::Link, theme::Palette};

/// The accents offered.
pub struct Choices;

impl Choices {
    /// The colours, with the words that name them. The first is the application's own colour: it is what the owner starts with,
    /// which the palette reads as "none chosen" and so leaves the brand colour alone. The others are real choices.
    pub const ALL: [([u8; 3], &'static str); 6] = [
        ([96, 212, 180], "Coral"),
        ([91, 156, 224], "Sky"),
        ([79, 196, 140], "Moss"),
        ([232, 186, 76], "Amber"),
        ([186, 132, 232], "Violet"),
        ([240, 120, 160], "Rose"),
    ];

    /// The colour a choice is shown in. The first is shown in the brand colour of the palette and not in the value that stands
    /// for it, which is a different colour (the owner's starting value is not what is drawn).
    pub fn shown(index: usize, brand: (u8, u8, u8)) -> (u8, u8, u8) {
        if index == 0 {
            brand
        } else {
            let [r, g, b] = Self::ALL[index].0;
            (r, g, b)
        }
    }

    /// The choice a saved accent is, or none for a colour that is not one of these (set by the command line, say).
    pub fn of(saved: [u8; 3]) -> Option<usize> {
        Self::ALL.iter().position(|(colour, _)| *colour == saved)
    }
}

/// One colour: a disc in the colour, ringed when it is the one in use, with its name under it.
#[derive(PartialEq)]
struct Swatch {
    index: usize,
    selected: bool,
    palette: Palette,
    link: Link,
}
impl Component for Swatch {
    fn render(&self) -> impl IntoElement {
        let (index, palette) = (self.index, self.palette);
        let (value, words) = Choices::ALL[index];
        let colour = Choices::shown(index, crate::theme::DARK.send);
        let link = self.link.clone();
        let mut hovered = use_state(|| false);
        rect()
            .spacing(6.)
            .cross_align(Alignment::center())
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |_| {
                link.send(Command::PatchSettings { patch: SettingsPatch { accent: Some(value), ..Default::default() } })
            })
            .child(
                rect()
                    .width(Size::px(40.))
                    .height(Size::px(40.))
                    .corner_radius(20.)
                    .center()
                    // The ring is the page's own text colour, so it shows on any page and is never the colour it surrounds.
                    .border(
                        Border::new()
                            .fill(if self.selected { palette.text } else if hovered() { palette.muted } else { palette.border })
                            .width(if self.selected { 3. } else { 1.5 })
                            .alignment(BorderAlignment::Outer),
                    )
                    .child(rect().width(Size::px(30.)).height(Size::px(30.)).corner_radius(15.).background(colour)),
            )
            .child(
                label()
                    .text(i18n::tr(words))
                    .color(if self.selected { palette.text } else { palette.muted })
                    .font_size(12.),
            )
    }
}

/// The row of colours, under its caption.
#[derive(PartialEq)]
pub struct Picker {
    pub link: Link,
    pub settings: Settings,
    pub palette: Palette,
}
impl Component for Picker {
    fn render(&self) -> impl IntoElement {
        let chosen = Choices::of(self.settings.accent);
        let mut row = rect().width(Size::fill()).horizontal().spacing(14.);
        for index in 0..Choices::ALL.len() {
            row = row.child(Swatch { index, selected: Some(index) == chosen, palette: self.palette, link: self.link.clone() });
        }
        rect()
            .width(Size::fill())
            .spacing(10.)
            .child(label().text(i18n::tr("Accent colour")).color(self.palette.muted).font_size(13.))
            .child(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_choice_is_the_value_the_owner_starts_with_so_the_brand_colour_stays() {
        assert_eq!(Choices::ALL[0].0, Settings::default().accent);
        // Choosing it must read as "none chosen" to the palette, which leaves the brand colour alone.
        assert_eq!(
            Palette::of_choice(peersus_core::config::Theme::Dark, true, Choices::ALL[0].0),
            crate::theme::DARK
        );
    }

    #[test]
    fn every_other_choice_recolours_what_goes_out_and_can_be_seen_on_both_pages() {
        for (index, (colour, name)) in Choices::ALL.iter().enumerate().skip(1) {
            for base in [crate::theme::DARK, crate::theme::LIGHT] {
                let palette = Palette::of_choice(peersus_core::config::Theme::Dark, true, *colour);
                let _ = base;
                assert_eq!(palette.send, (colour[0], colour[1], colour[2]), "{name} ({index}) was moved");
                let ratio = crate::theme::contrast_ratio(palette.send, crate::theme::DARK.background);
                assert!(ratio >= 3.0, "{name} gives only {ratio} on the dark page");
                let tinted = base.with_accent((colour[0], colour[1], colour[2]));
                let light = crate::theme::contrast_ratio(tinted.send, base.background);
                assert!(light >= 3.0, "{name} gives only {light} on its page");
            }
        }
    }

    #[test]
    fn no_two_choices_are_the_same_colour_or_the_same_word() {
        for (i, (a, words)) in Choices::ALL.iter().enumerate() {
            for (b, other) in Choices::ALL.iter().skip(i + 1) {
                assert_ne!(a, b, "{words} and {other} are the same colour");
                assert_ne!(words, other, "two choices are called {words}");
            }
        }
    }

    #[test]
    fn a_saved_colour_is_found_among_the_choices_and_a_stranger_is_none() {
        for (index, (colour, _)) in Choices::ALL.iter().enumerate() {
            assert_eq!(Choices::of(*colour), Some(index));
        }
        assert_eq!(Choices::of([1, 2, 3]), None);
    }

    #[test]
    fn every_choice_is_a_colour_the_owner_accepts() {
        for (colour, name) in Choices::ALL {
            let settings = Settings { accent: colour, ..Settings::default() };
            assert_eq!(settings.validate(), Ok(()), "{name} is refused");
        }
    }
}
