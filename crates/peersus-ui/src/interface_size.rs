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
    /// The narrowest window, in logical pixels, that every screen is made and laid out for (`headless::tests::SIZES`). A larger
    /// interface is a window with fewer logical pixels across, so on a phone a large size can make it narrower than this: the
    /// header's pieces then lie on each other and words are cut. Such a size is not offered there (see [`Self::largest`]).
    pub const NARROWEST: f32 = 300.;

    /// The place on the slider (0 to 100) of `scale`, on a slider whose left end is the smallest size and whose right end is `top`
    /// (the largest this window holds, [`Self::largest`]); a size outside the two is held at the nearer end, so the slider can always
    /// show what it is given.
    pub fn place(scale: f32, top: f32) -> f64 {
        let scale = if scale.is_finite() {
            scale
        } else {
            Self::NORMAL
        };
        f64::from(((scale - Self::MIN) / (top - Self::MIN)).clamp(0., 1.) * 100.)
    }

    /// The size at `place` (0 to 100) on a slider that ends at `top`, snapped to a step so that "100 %" is exactly 1.0 and not
    /// 0.9999, and kept inside the slider.
    pub fn scale(place: f64, top: f32) -> f32 {
        let raw = Self::MIN + (top - Self::MIN) * (place.clamp(0., 100.) as f32 / 100.);
        let snapped = (raw / Self::STEP).round() * Self::STEP;
        // Rounded to hundredths: a multiple of a step in floating point is not exactly the number it names.
        ((snapped * 100.).round() / 100.).clamp(Self::MIN, top)
    }

    /// The size as a whole percentage, for the words next to the slider.
    pub fn percent(scale: f32) -> u32 {
        (scale * 100.).round().max(0.) as u32
    }

    /// The largest size a window `width_at_normal` logical pixels across at 100 % can be drawn at and stay at least
    /// [`Self::NARROWEST`] wide, down to a step, and never less than the normal size nor more than [`Self::MAX`]. A width not known
    /// yet (no layout so far, or not a number) holds back nothing, so a window does not open smaller than the size that was saved.
    pub fn largest(width_at_normal: f32) -> f32 {
        if !(width_at_normal.is_finite() && width_at_normal > 0.) {
            return Self::MAX;
        }
        // A hair over the quotient, so that a width of exactly 1.1 × NARROWEST is 1.1 and not 1.05 by a rounding.
        let steps = (width_at_normal / Self::NARROWEST / Self::STEP + 1e-4).floor();
        ((steps * Self::STEP * 100.).round() / 100.).clamp(Self::NORMAL, Self::MAX)
    }
}

/// [`Percent::largest`] for the window this is drawn in, kept as its width changes. Its width at 100 % is its laid-out width times
/// the size it was laid out at, read when the laid-out width changes: a new size changes the scale at once and the width one layout
/// later, so the two read together in between would be a width the window never had (and a largest size that swings).
pub fn use_largest() -> f32 {
    let mut largest = use_state(|| Percent::MAX);
    use_side_effect(move || {
        let window = Platform::get().current_window();
        let width = window.root_size.read().width;
        let drawn_at = *window.custom_scale_factor.peek() as f32;
        largest.set_if_modified(Percent::largest(width * drawn_at));
    });
    largest()
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
        // What this window can hold is the slider's right end, and a saved size above it is shown there (see `Percent::largest`).
        let largest = use_largest();
        let shown = asked().unwrap_or(saved).min(largest);
        let link = self.link.clone();
        let save = move |scale: f32| {
            link.send(Command::PatchSettings {
                patch: Box::new(SettingsPatch {
                    scale: Some(scale),
                    ..Default::default()
                }),
            });
        };
        let on_moved = {
            let save = save.clone();
            move |place: f64| {
                let scale = Percent::scale(place, largest);
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
                                .value(Percent::place(value_shown, largest))
                                .step(f64::from(Percent::STEP / (largest - Percent::MIN) * 100.))
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
        assert_eq!(Percent::scale(0., Percent::MAX), Percent::MIN);
        assert_eq!(Percent::scale(100., Percent::MAX), Percent::MAX);
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
            let scale = Percent::scale(f64::from(place) / 10., Percent::MAX);
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
        assert_eq!(
            Percent::scale(Percent::place(Percent::NORMAL, Percent::MAX), Percent::MAX),
            1.0
        );
        assert_eq!(Percent::percent(1.0), 100);
    }

    #[test]
    fn a_size_goes_to_its_place_and_back_to_the_same_size() {
        // Every size the slider can make survives the trip to a place and back: what is shown is what is saved.
        let mut scale = Percent::MIN;
        while scale <= Percent::MAX + 0.001 {
            let back = Percent::scale(Percent::place(scale, Percent::MAX), Percent::MAX);
            assert!((back - scale).abs() < 0.001, "{scale} came back as {back}");
            scale += Percent::STEP;
        }
    }

    #[test]
    fn the_slider_only_goes_up_as_the_size_does() {
        let mut last = 0.;
        for place in 0..=100 {
            let scale = Percent::scale(f64::from(place), Percent::MAX);
            assert!(scale >= last, "the size went down at {place}");
            last = scale;
        }
    }

    #[test]
    fn a_size_outside_what_is_accepted_or_not_a_number_is_still_shown_somewhere() {
        assert_eq!(Percent::place(0.1, Percent::MAX), 0.);
        assert_eq!(Percent::place(9., Percent::MAX), 100.);
        assert_eq!(
            Percent::place(f32::NAN, Percent::MAX),
            Percent::place(Percent::NORMAL, Percent::MAX)
        );
        assert_eq!(
            Percent::place(f32::INFINITY, Percent::MAX),
            Percent::place(Percent::NORMAL, Percent::MAX)
        );
        // And a place outside the slider is held at its ends.
        assert_eq!(Percent::scale(-30., Percent::MAX), Percent::MIN);
        assert_eq!(Percent::scale(400., Percent::MAX), Percent::MAX);
    }

    #[test]
    fn a_window_is_never_made_narrower_than_the_narrowest_one_the_screens_are_laid_out_for() {
        // A phone of 335 logical pixels at 100 % (the NX with its display made larger) holds 110 %, a desktop window all of them.
        assert_eq!(Percent::largest(335.), 1.1);
        assert_eq!(Percent::largest(330.), 1.1);
        assert_eq!(Percent::largest(1280.), Percent::MAX);
        for width in 300..=1200 {
            let width = width as f32;
            let largest = Percent::largest(width);
            assert!(
                width / largest >= Percent::NARROWEST - 0.01,
                "{width} at {largest}"
            );
            assert!(
                (Percent::NORMAL..=Percent::MAX).contains(&largest),
                "{width} at {largest}"
            );
            assert_eq!(Percent::percent(largest) % 5, 0, "{largest} is not a step");
        }
    }

    #[test]
    fn on_a_narrow_window_the_whole_slider_is_the_sizes_it_holds_and_its_right_end_is_the_largest()
    {
        let top = Percent::largest(335.);
        assert_eq!(Percent::place(top, top), 100.);
        assert_eq!(Percent::scale(100., top), top);
        assert_eq!(Percent::scale(0., top), Percent::MIN);
        // 100 % is still exactly a place on it, and a saved size above what the window holds is shown at the right end.
        assert_eq!(
            Percent::scale(Percent::place(Percent::NORMAL, top), top),
            1.0
        );
        assert_eq!(Percent::place(1.85, top), 100.);
    }

    #[test]
    fn a_window_narrower_than_that_at_100_percent_keeps_100_percent_and_an_unknown_width_holds_back_nothing()
     {
        assert_eq!(Percent::largest(250.), Percent::NORMAL);
        for unknown in [0., -1., f32::NAN, f32::INFINITY] {
            assert_eq!(Percent::largest(unknown), Percent::MAX, "{unknown}");
        }
    }

    #[test]
    fn the_percent_is_the_whole_number_of_the_size() {
        assert_eq!(Percent::percent(0.75), 75);
        assert_eq!(Percent::percent(1.25), 125);
        assert_eq!(Percent::percent(2.0), 200);
    }
}
