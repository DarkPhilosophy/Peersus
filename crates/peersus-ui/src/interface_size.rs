//! How large the interface is drawn, as a percentage on a slider: the person picks any size between the smallest and the largest the owner
//! accepts, and not one of a few that were chosen for them. The size is saved as the owner's `scale`, and the toolkit multiplies every size by
//! it, so text, icons and spacing grow together and nothing is multiplied by hand.
//!
//! How a place on the slider is a size, and a size a percentage, is [`Percent`], which knows no toolkit and is tested on its own.
use freya::prelude::*;
use peersus_core::{Settings, config::SettingsPatch, model::Command};

use crate::{i18n, link::Link, theme::Palette};

/// The sizes the owner accepts (see `Settings::validate`), as the end points of the slider, and the maths between a place on it and a size.
pub struct Percent;

impl Percent {
    /// The smallest size, as a scale (0.75 is 75 %).
    pub const MIN: f32 = 0.75;
    /// The largest size, as a scale (2.0 is 200 %).
    pub const MAX: f32 = 2.0;
    /// The size that is no change, and what Reset goes back to.
    pub const NORMAL: f32 = 1.0;
    /// The smallest move of the slider, in percent of a size (a step of 5 %): fine enough to be any size, coarse enough to be easy to land on.
    pub const STEP: f32 = 0.05;

    /// The place on the slider (0 to 100) of `scale`: the smallest size is the left end and the largest the right, and a size outside the two is
    /// held at the nearer end, so the slider can always show what it is given.
    pub fn place(scale: f32) -> f64 {
        let scale = if scale.is_finite() {
            scale
        } else {
            Self::NORMAL
        };
        f64::from(((scale - Self::MIN) / (Self::MAX - Self::MIN)).clamp(0., 1.) * 100.)
    }

    /// The size at `place` (0 to 100) on the slider, snapped to a step so that "100 %" is exactly 1.0 and not 0.9999, and kept inside what the
    /// owner accepts.
    pub fn scale(place: f64) -> f32 {
        let raw = Self::MIN + (Self::MAX - Self::MIN) * (place.clamp(0., 100.) as f32 / 100.);
        let snapped = (raw / Self::STEP).round() * Self::STEP;
        // Rounded to hundredths: a multiple of a step in floating point is not exactly the number it names.
        ((snapped * 100.).round() / 100.).clamp(Self::MIN, Self::MAX)
    }

    /// The size as a whole percentage, for the words next to the slider.
    pub fn percent(scale: f32) -> u32 {
        (scale * 100.).round().max(0.) as u32
    }
}

/// The slider, its value in words, and a button to go back to the normal size.
#[derive(PartialEq)]
pub struct Picker {
    pub link: Link,
    pub settings: Settings,
    pub palette: Palette,
}
impl Component for Picker {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let saved = self.settings.scale;
        // What the person has asked for and the owner has not yet given back. The slider follows this and not the saved size while it is
        // there: the window is laid out again at each size the owner confirms, and a slider that jumped back to a size from a moment ago would
        // fight the finger. It is let go of as soon as the saved size is the one asked for.
        let mut asked = use_state(|| None::<f32>);
        if asked
            .peek()
            .is_some_and(|wanted| (wanted - saved).abs() < 0.001)
        {
            asked.set(None);
        }
        let shown = asked().unwrap_or(saved);
        let link = self.link.clone();
        let save = move |scale: f32| {
            link.send(Command::PatchSettings {
                patch: SettingsPatch {
                    scale: Some(scale),
                    ..Default::default()
                },
            });
        };
        let on_moved = {
            let save = save.clone();
            move |place: f64| {
                let scale = Percent::scale(place);
                // The slider moves a pixel at a time but a size changes in steps: the owner is told only when the step is a new one, so a drag
                // across the whole slider is at most a few dozen small patches and never one per pixel.
                if asked
                    .peek()
                    .is_none_or(|wanted| (wanted - scale).abs() >= 0.001)
                {
                    asked.set(Some(scale));
                    save(scale);
                }
            }
        };
        let reset = {
            let save = save.clone();
            move |_| {
                asked.set(Some(Percent::NORMAL));
                save(Percent::NORMAL);
            }
        };
        let value_shown = shown;
        rect()
            .width(Size::fill())
            .spacing(10.)
            .child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .cross_align(Alignment::Center)
                    .child(label().text(i18n::tr("Interface size")).color(palette.muted).font_size(13.))
                    .child(rect().width(Size::flex(1.)))
                    .child(
                        label()
                            .text(format!("{} %", Percent::percent(value_shown)))
                            .color(palette.text)
                            .font_size(15.)
                            .font_weight(FontWeight::BOLD),
                    ),
            )
            .child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .cross_align(Alignment::Center)
                    .spacing(16.)
                    .child(
                        rect().width(Size::flex(1.)).child(
                            Slider::new(on_moved)
                                .value(Percent::place(value_shown))
                                .step(f64::from(Percent::STEP / (Percent::MAX - Percent::MIN) * 100.))
                                // The page scrolls with the wheel; a slider that took the wheel too would change the size by passing over it.
                                .scroll_enabled(false),
                        ),
                    )
                    .child(Button::new().on_press(reset).enabled(Percent::percent(value_shown) != 100).child(i18n::tr("Reset"))),
            )
            .child(
                label()
                    .text(i18n::tr("Text, icons and spacing grow together. Ctrl and + or − changes it for this window only."))
                    .color(palette.muted)
                    .font_size(13.),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ends_of_the_slider_are_the_smallest_and_the_largest_size_the_owner_accepts() {
        assert_eq!(Percent::scale(0.), Percent::MIN);
        assert_eq!(Percent::scale(100.), Percent::MAX);
        for end in [Percent::MIN, Percent::MAX] {
            assert_eq!(
                Settings {
                    scale: end,
                    ..Settings::default()
                }
                .validate(),
                Ok(()),
                "{end} is refused"
            );
        }
    }

    #[test]
    fn every_place_on_the_slider_is_a_size_the_owner_accepts() {
        for place in 0..=1000 {
            let scale = Percent::scale(f64::from(place) / 10.);
            assert_eq!(
                Settings {
                    scale,
                    ..Settings::default()
                }
                .validate(),
                Ok(()),
                "{scale} at {place} is refused"
            );
        }
    }

    #[test]
    fn the_size_that_is_no_change_is_exactly_one_and_is_a_place_on_the_slider() {
        // 100 % must be 1.0 itself, or Reset and the normal size would differ by a rounding.
        assert_eq!(Percent::scale(Percent::place(Percent::NORMAL)), 1.0);
        assert_eq!(Percent::percent(1.0), 100);
    }

    #[test]
    fn a_size_goes_to_its_place_and_back_to_the_same_size() {
        // Every size the slider can make survives the trip to a place and back: what is shown is what is saved.
        let mut scale = Percent::MIN;
        while scale <= Percent::MAX + 0.001 {
            let back = Percent::scale(Percent::place(scale));
            assert!((back - scale).abs() < 0.001, "{scale} came back as {back}");
            scale += Percent::STEP;
        }
    }

    #[test]
    fn the_slider_only_goes_up_as_the_size_does() {
        let mut last = 0.;
        for place in 0..=100 {
            let scale = Percent::scale(f64::from(place));
            assert!(scale >= last, "the size went down at {place}");
            last = scale;
        }
    }

    #[test]
    fn a_size_outside_what_is_accepted_or_not_a_number_is_still_shown_somewhere() {
        assert_eq!(Percent::place(0.1), 0.);
        assert_eq!(Percent::place(9.), 100.);
        assert_eq!(Percent::place(f32::NAN), Percent::place(Percent::NORMAL));
        assert_eq!(
            Percent::place(f32::INFINITY),
            Percent::place(Percent::NORMAL)
        );
        // And a place outside the slider is held at its ends.
        assert_eq!(Percent::scale(-30.), Percent::MIN);
        assert_eq!(Percent::scale(400.), Percent::MAX);
    }

    #[test]
    fn the_percent_is_the_whole_number_of_the_size() {
        assert_eq!(Percent::percent(0.75), 75);
        assert_eq!(Percent::percent(1.25), 125);
        assert_eq!(Percent::percent(2.0), 200);
    }
}
