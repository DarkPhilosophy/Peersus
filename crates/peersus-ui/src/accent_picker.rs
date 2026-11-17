//! The accent: the pair of colours the person chooses. What goes out is drawn in the first, what comes in and what an extension
//! draws in its accent in the second, and so are the main shapes of the pictures, so a choice here changes the look of the
//! whole application at once. A few named pairs, each drawn in its own two colours, with the one in use marked, and not a field
//! for a number.
//!
//! Which pairs there are is [`ACCENTS`]; which one a saved value is, is [`Choices`], which knows no toolkit and is tested on
//! its own.
use freya::prelude::*;
use peersus_core::{Settings, config::SettingsPatch, model::Command};

use crate::{
    i18n,
    link::Link,
    theme::{ACCENTS, Palette},
};

/// The accents offered.
pub struct Choices;

impl Choices {
    /// The two colours a choice is shown in. The first is shown in the brand colours of the palette and not in the value that
    /// stands for it, which is a different colour (the owner's starting value is not what is drawn).
    pub fn shown(index: usize, brand: Palette) -> ((u8, u8, u8), (u8, u8, u8)) {
        if index == 0 {
            (brand.send, brand.receive)
        } else {
            let ([r, g, b], [s, t, u]) = (ACCENTS[index].send, ACCENTS[index].receive);
            ((r, g, b), (s, t, u))
        }
    }

    /// The choice a saved accent is, or none for a colour that is not one of these (set by the command line, say).
    pub fn of(saved: [u8; 3]) -> Option<usize> {
        ACCENTS.iter().position(|accent| accent.send == saved)
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
        let (value, words) = (ACCENTS[index].send, ACCENTS[index].name);
        let (first, second) = Choices::shown(index, crate::theme::DARK);
        let link = self.link.clone();
        let mut hovered = use_state(|| false);
        rect()
            .spacing(6.)
            .cross_align(Alignment::center())
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |_| {
                link.send(Command::PatchSettings {
                    patch: Box::new(SettingsPatch {
                        accent: Some(value),
                        ..Default::default()
                    }),
                })
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
                            .fill(if self.selected {
                                palette.text
                            } else if hovered() {
                                palette.muted
                            } else {
                                palette.border
                            })
                            .width(if self.selected { 3. } else { 1.5 })
                            .alignment(BorderAlignment::Outer),
                    )
                    // The disc halved: what goes out on the left, what comes in on the right.
                    .child(
                        rect()
                            .width(Size::px(30.))
                            .height(Size::px(30.))
                            .corner_radius(15.)
                            .overflow(Overflow::Clip)
                            .horizontal()
                            .child(
                                rect()
                                    .width(Size::px(15.))
                                    .height(Size::fill())
                                    .background(first),
                            )
                            .child(
                                rect()
                                    .width(Size::px(15.))
                                    .height(Size::fill())
                                    .background(second),
                            ),
                    ),
            )
            .child(
                label()
                    .text(i18n::tr(words))
                    .color(if self.selected {
                        palette.text
                    } else {
                        palette.muted
                    })
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
        // Wrapped, so that the last colours are not cut off where six do not fit a phone's width.
        let mut row = rect()
            .width(Size::fill())
            .horizontal()
            .content(Content::wrap_spacing(10.))
            .spacing(14.);
        for index in 0..ACCENTS.len() {
            row = row.child(Swatch {
                index,
                selected: Some(index) == chosen,
                palette: self.palette,
                link: self.link.clone(),
            });
        }
        rect()
            .width(Size::fill())
            .spacing(10.)
            .child(
                label()
                    .text(i18n::tr("Accent colour"))
                    .color(self.palette.muted)
                    .font_size(13.),
            )
            .child(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_choice_is_the_value_the_owner_starts_with_so_the_brand_colours_stay() {
        let brand = crate::theme::DARK.receive;
        assert_eq!(ACCENTS[0].send, Settings::default().accent);
        assert_eq!(ACCENTS[0].receive, [brand.0, brand.1, brand.2]);
        // Choosing it must read as "none chosen" to the palette, which leaves the brand colours alone.
        assert_eq!(
            Palette::of_choice(peersus_core::config::Theme::Dark, true, ACCENTS[0].send),
            crate::theme::DARK
        );
    }

    #[test]
    fn every_other_choice_recolours_both_tones_and_each_can_be_seen_on_both_pages() {
        use peersus_core::config::Theme;
        let rgb = |[r, g, b]: [u8; 3]| (r, g, b);
        for accent in ACCENTS.iter().skip(1) {
            for (theme, base) in [
                (Theme::Dark, crate::theme::DARK),
                (Theme::Light, crate::theme::LIGHT),
            ] {
                let palette = Palette::of_choice(theme, true, accent.send);
                // Moved only as far as the page needs: on the black page the offered colours are seen as they are.
                if theme == Theme::Dark {
                    assert_eq!(palette.send, rgb(accent.send), "{}", accent.name);
                    assert_eq!(palette.receive, rgb(accent.receive), "{}", accent.name);
                }
                for (tone, colour) in [("send", palette.send), ("receive", palette.receive)] {
                    let ratio = crate::theme::contrast_ratio(colour, base.background);
                    assert!(
                        ratio >= 3.0,
                        "{} {tone} gives only {ratio} on {theme:?}",
                        accent.name
                    );
                }
                // Nothing else of the theme changes.
                assert_eq!(
                    Palette {
                        send: base.send,
                        receive: base.receive,
                        ..palette
                    },
                    base
                );
            }
        }
    }

    #[test]
    fn the_two_tones_of_a_choice_stand_apart_and_no_two_choices_look_or_read_the_same() {
        let apart = |a: (u8, u8, u8), b: (u8, u8, u8)| -> i32 {
            [(a.0, b.0), (a.1, b.1), (a.2, b.2)]
                .iter()
                .map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs())
                .sum()
        };
        let brand = crate::theme::DARK;
        for (i, accent) in ACCENTS.iter().enumerate() {
            // What is drawn: the brand's own pair for the first.
            let (send, receive) = Choices::shown(i, brand);
            assert!(
                apart(send, receive) >= 120,
                "{}: its two tones are too alike to tell going from coming",
                accent.name
            );
            for other in ACCENTS.iter().skip(i + 1) {
                assert_ne!(
                    (accent.send, accent.receive),
                    (other.send, other.receive),
                    "{} and {} are the same pair",
                    accent.name,
                    other.name
                );
                assert_ne!(
                    accent.name, other.name,
                    "two choices are called {}",
                    accent.name
                );
            }
        }
        // The point of a pair: no other choice is one colour beside the brand's sky blue.
        for (i, accent) in ACCENTS.iter().enumerate().skip(1) {
            let (_, receive) = Choices::shown(i, brand);
            assert!(
                apart(receive, brand.receive) >= 60,
                "{} still pairs with the brand's sky blue",
                accent.name
            );
        }
    }

    #[test]
    fn a_saved_colour_is_found_among_the_choices_and_a_stranger_is_none() {
        for (index, accent) in ACCENTS.iter().enumerate() {
            assert_eq!(Choices::of(accent.send), Some(index));
        }
        assert_eq!(Choices::of([1, 2, 3]), None);
    }

    #[test]
    fn a_colour_that_is_not_a_choice_recolours_only_what_goes_out() {
        let palette =
            Palette::of_choice(peersus_core::config::Theme::Dark, true, [0xe0, 0x30, 0x90]);
        assert_eq!(palette.send, (0xe0, 0x30, 0x90));
        assert_eq!(palette.receive, crate::theme::DARK.receive);
    }

    #[test]
    fn every_choice_is_a_colour_the_owner_accepts() {
        for accent in ACCENTS {
            let settings = Settings {
                accent: accent.send,
                ..Settings::default()
            };
            assert_eq!(settings.validate(), Ok(()), "{} is refused", accent.name);
        }
    }
}
