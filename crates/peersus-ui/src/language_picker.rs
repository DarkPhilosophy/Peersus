//! The language control of Settings: a button that shows the language in use, and beside it a quiet strip of
//! the other languages sliding past, so a person can see that more are offered without a list being thrown on
//! the page. Pressing the button opens the whole list over the page; pressing a language in it, or the strip
//! itself, chooses it.
//!
//! What moves where is [`Ribbon`], which knows no toolkit and is tested on its own. The components below only
//! draw it.
use freya::{animation::*, prelude::*};
use peersus_core::{Settings, config::SettingsPatch, model::Command};

use crate::{i18n, link::Link, motion, theme::Palette};

/// How the strip of languages moves: one slow, endless slide to the left, with the two ends fading out so that a
/// language comes and goes without a hard edge.
pub struct Ribbon;

impl Ribbon {
    /// How long one language takes to pass, in milliseconds: slow enough to read, quick enough to show them all.
    pub const MS_PER_ITEM: u64 = 1600;
    /// The width a fade takes at each end of the strip, in pixels.
    pub const FADE: f32 = 36.;
    /// The width every language takes in the strip, in pixels: the same for all, so the length of a row is known exactly and
    /// a lap can end where the next one begins. Wide enough for the longest name at the button's text size.
    pub const ITEM: f32 = 136.;

    /// The offset of the strip at `progress` (0 to 1 of a whole lap), in pixels: it slides left by the width of
    /// the whole row, so the end of the lap looks exactly like the start and the loop has no seam.
    pub fn offset(progress: f32, lap: f32) -> f32 {
        -(progress.clamp(0., 1.) * lap.max(0.))
    }

    /// The width of a row of `items` languages, in pixels: exactly one lap.
    pub fn row_width(items: usize) -> f32 {
        items as f32 * Self::ITEM
    }

    /// How much of its colour a thing keeps at `x` pixels from the left of a strip `width` wide: nothing at the
    /// very edge, all of it past the fade, and a smooth ramp between, the same at both ends.
    pub fn fade(x: f32, width: f32) -> f32 {
        if width <= 0. || Self::FADE <= 0. {
            return 1.;
        }
        let edge = x.min(width - x).clamp(0., Self::FADE) / Self::FADE;
        // Smoothstep, so there is no visible corner where the ramp meets the flat part.
        edge * edge * (3. - 2. * edge)
    }

    /// The time of a whole lap over `items`, in milliseconds. Never zero, so the animation always has a length.
    pub fn lap_ms(items: usize) -> u64 {
        Self::MS_PER_ITEM * items.max(1) as u64
    }

    /// The languages the strip shows: all but the one in use, in their usual order.
    pub fn others(current: peersus_core::config::Language) -> Vec<peersus_core::config::Language> {
        peersus_core::config::Language::ALL.into_iter().filter(|language| *language != current).collect()
    }
}

/// The patch that chooses `language`.
fn choose(link: &Link, language: peersus_core::config::Language) {
    link.send(Command::PatchSettings { patch: SettingsPatch { language: Some(language), ..Default::default() } });
}

/// One language in the strip or in the list: its own name, in its own language, and nothing else.
#[derive(PartialEq)]
struct Name {
    language: peersus_core::config::Language,
    palette: Palette,
    selected: bool,
    on_choose: EventHandler<peersus_core::config::Language>,
}
impl Component for Name {
    fn render(&self) -> impl IntoElement {
        let (language, palette) = (self.language, self.palette);
        let on_choose = self.on_choose.clone();
        let mut hovered = use_state(|| false);
        rect()
            .width(Size::fill())
            .padding((10., 16.))
            .corner_radius(8.)
            .background(if self.selected {
                palette.raised
            } else if hovered() {
                palette.surface
            } else {
                palette.header
            })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            // The press must not reach the layer under the list, which closes it before the choice is made.
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                on_choose.call(language);
            })
            .child(label().text(language.name()).color(if self.selected { palette.text } else { palette.muted }).font_size(15.))
    }
}

/// The strip of the other languages, sliding past forever, with a small arrow at its start that points at the
/// button it belongs to. Pressing it opens the list, like the button does.
#[derive(PartialEq)]
struct Strip {
    current: peersus_core::config::Language,
    palette: Palette,
    /// The colour of what the strip lies on, which its two fades start from.
    ground: (u8, u8, u8),
    reduced_motion: bool,
    on_open: EventHandler<()>,
}
impl Component for Strip {
    fn render(&self) -> impl IntoElement {
        let (palette, current) = (self.palette, self.current);
        let on_open = self.on_open.clone();
        let items = Ribbon::others(current);
        let lap = Ribbon::lap_ms(items.len());
        // One lap, again and again: the toolkit restarts it when it ends. With reduced motion the strip stands still.
        let animation = use_animation_with_dependencies(&(current, self.reduced_motion), move |conf, (_, reduced)| {
            conf.on_creation(OnCreation::Run);
            conf.on_change(OnChange::Rerun);
            conf.on_finish(OnFinish::restart());
            AnimNum::new(0., 1.).time(if *reduced { 0 } else { lap }).function(Function::Linear)
        });
        let progress: f32 = animation.get().value();
        // Every language takes the same width, and the lap is exactly the width of one whole row: the row is drawn twice, one
        // after the other, so when the first copy has gone past the second sits where the first began, and the end of a lap is
        // the start of the next with nothing to see in between.
        let offset = Ribbon::offset(progress, Ribbon::row_width(items.len()));
        let row = || -> Element {
            rect()
                .horizontal()
                .children(items.iter().map(|language| -> Element {
                    rect()
                        .width(Size::px(Ribbon::ITEM))
                        .child(label().text(language.name()).color(palette.muted).font_size(Picker::FONT_SIZE).max_lines(1))
                        .into()
                }))
                .into()
        };
        let track = rect().horizontal().offset_x(offset).child(row()).child(row());
        let fade = |side_left: bool| -> Element {
            // From the colour of the PAGE the strip lies on (not the header's, which is another colour) to nothing: what is under it shows
            // through more and more toward the middle, so a name comes and goes without a hard edge at either side.
            let solid = Color::from_rgb(self.ground.0, self.ground.1, self.ground.2);
            let clear = solid.with_a(0);
            // The renderer's 270 degrees runs left to right; each solid stop belongs at the outside edge.
            let (angle, from, to) = if side_left { (270., solid, clear) } else { (90., solid, clear) };
            let at = Position::new_absolute().top(0.);
            rect()
                .position(if side_left { at.left(0.) } else { at.right(0.) })
                // Names are nested four levels below this viewport; paint above them, not beneath their text.
                .layer(Layer::Relative(4))
                .width(Size::px(Ribbon::FADE))
                .height(Size::fill())
                .background(LinearGradient::new().angle(angle).stop((from, 0.)).stop((to, 100.)))
                .into()
        };
        rect()
            .width(Size::flex(1.))
            .height(Size::px(Picker::BUTTON_HEIGHT))
            .horizontal()
            .cross_align(Alignment::center())
            // The whole strip opens the list, as the button does.
            .on_press(move |_| on_open.call(()))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .height(Size::fill())
                    .overflow(Overflow::Clip)
                    .main_align(Alignment::Center)
                    .child(track)
                    .child(fade(true))
                    .child(fade(false)),
            )
    }
}

/// The control: the button with the language in use, the strip beside it, and the list over the page.
#[derive(PartialEq)]
pub struct Picker {
    pub link: Link,
    pub settings: Settings,
    pub palette: Palette,
    /// The colour of what the control lies on: the strip fades into it at both ends.
    pub ground: (u8, u8, u8),
}
impl Component for Picker {
    fn render(&self) -> impl IntoElement {
        let (palette, current, reduced) = (self.palette, self.settings.language, self.settings.reduced_motion);
        let mut open = use_state(|| false);
        let on_open = EventHandler::new(move |()| open.set(true));
        let button = rect()
            .horizontal()
            .height(Size::px(Self::BUTTON_HEIGHT))
            .cross_align(Alignment::center())
            .spacing(10.)
            .padding((0., 16.))
            .corner_radius(10.)
            .background(palette.raised)
            .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
            .on_press({
                let on_open = on_open.clone();
                move |_| on_open.call(())
            })
            .child(label().text(current.name()).color(palette.text).font_size(Self::FONT_SIZE))
            // A plain mark that says "opens a list", and nothing that belongs to another part of the program.
            .child(label().text("\u{25BE}").color(palette.muted).font_size(Self::FONT_SIZE));
        let link = self.link.clone();
        // The list belongs to the button: it is placed just under it, inside the same row, and moves with it. `Reveal` draws in
        // the window's own coordinates and would put it in a corner, so the move is followed here with the same progress.
        let progress = motion::use_progress(open(), reduced);
        let shown = if reduced { crate::view::Popup::still() } else { crate::view::Popup::at(progress) };
        let mounted = crate::view::Motion::mounted(open(), progress);
        let list = {
            let mut panel = rect()
                .position(Position::new_absolute().top(Self::BUTTON_HEIGHT + 6.).left(0.))
                .layer(Layer::Overlay)
                .width(Size::px(260.))
                .padding(6.)
                .spacing(2.)
                .corner_radius(12.)
                .background(palette.header)
                .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
                .opacity(shown.opacity)
                .offset_y(shown.lift)
                .scale(shown.scale)
                .transform_origin(TransformOrigin::top_left())
                // The layer under it closes the list on a pointer-down: this one must not reach it.
                .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation());
            for language in peersus_core::config::Language::ALL {
                let link = link.clone();
                panel = panel.child(Name {
                    language,
                    palette,
                    selected: language == current,
                    on_choose: EventHandler::new(move |chosen| {
                        open.set(false);
                        choose(&link, chosen);
                    }),
                });
            }
            panel
        };
        // A layer as large as the WINDOW under the list, that takes a click anywhere else to close it. `Size::fill()` here would only fill the
        // row this component is in, so a click outside the row would never reach it and the list would stay open: the window's own size is
        // given in pixels, as the other full-window layers are.
        let window = Platform::get().current_window();
        let (width, height) = {
            let size = *window.root_size.read();
            (size.width.max(0.), size.height.max(0.))
        };
        let away = rect()
            .layer(Layer::Overlay)
            .position(Position::new_global().top(0.).left(0.))
            .width(Size::px(width))
            .height(Size::px(height))
            .on_pointer_down(move |_| open.set(false));
        rect()
            .width(Size::fill())
            .spacing(8.)
            .child(label().text(i18n::tr("Language")).color(palette.muted).font_size(13.))
            .child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::center())
                    .spacing(16.)
                    .child(button)
                    .child(Strip { current, palette, ground: self.ground, reduced_motion: reduced, on_open: on_open.clone() }),
            )
            .maybe_child(mounted.then(|| Element::from(away)))
            .maybe_child(mounted.then(|| Element::from(list)))
    }
}

impl Picker {
    /// The height of the button, in pixels: where the list starts under it, and the height of the strip beside it, so that the two are one size.
    const BUTTON_HEIGHT: f32 = 44.;
    const FONT_SIZE: f32 = 16.;
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_core::config::Language;

    #[test]
    fn the_strip_shows_every_language_but_the_one_in_use() {
        for current in Language::ALL {
            let others = Ribbon::others(current);
            assert_eq!(others.len(), Language::ALL.len() - 1);
            assert!(!others.contains(&current));
        }
    }

    #[test]
    fn a_lap_ends_exactly_one_row_to_the_left_of_where_it_began() {
        assert_eq!(Ribbon::offset(0., 500.), 0.);
        assert_eq!(Ribbon::offset(1., 500.), -500.);
        // Out of range is held at the ends and never past them, so the strip cannot drift off.
        assert_eq!(Ribbon::offset(-3., 500.), 0.);
        assert_eq!(Ribbon::offset(9., 500.), -500.);
    }

    #[test]
    fn a_language_fades_to_nothing_at_both_ends_and_is_whole_in_the_middle() {
        let width = 300.;
        assert_eq!(Ribbon::fade(0., width), 0.);
        assert_eq!(Ribbon::fade(width, width), 0.);
        assert_eq!(Ribbon::fade(width / 2., width), 1.);
        // Both ends are the same, so neither looks cut and the other not.
        for step in 0..=Ribbon::FADE as i32 {
            let x = step as f32;
            assert!((Ribbon::fade(x, width) - Ribbon::fade(width - x, width)).abs() < 1e-6);
        }
        // And it rises without ever falling on the way in.
        let mut last = 0.;
        for step in 0..=Ribbon::FADE as i32 {
            let now = Ribbon::fade(step as f32, width);
            assert!(now >= last, "the fade dips at {step}");
            last = now;
        }
    }

    #[test]
    fn a_strip_too_narrow_to_fade_still_has_a_defined_colour() {
        for width in [0., -5.] {
            assert_eq!(Ribbon::fade(10., width), 1.);
        }
        assert!(Ribbon::fade(1., 4.) <= 1.);
    }

    #[test]
    fn a_lap_always_takes_some_time_and_grows_with_the_number_of_languages() {
        assert!(Ribbon::lap_ms(0) > 0);
        assert!(Ribbon::lap_ms(11) > Ribbon::lap_ms(3));
    }

    #[test]
    fn a_row_is_exactly_as_wide_as_the_slots_that_make_it_so_the_loop_has_no_seam() {
        // The lap and the row are the same number, so the second copy is where the first began when a lap ends.
        for items in [1usize, 11] {
            assert_eq!(Ribbon::row_width(items), items as f32 * Ribbon::ITEM);
        }
        assert_eq!(Ribbon::offset(1., Ribbon::row_width(11)), -Ribbon::row_width(11));
    }

    #[test]
    fn the_longest_name_fits_the_slot_it_is_drawn_in() {
        // A name wider than its slot would run under the next one; the same estimate the headlines are fitted with.
        for language in Language::ALL {
            let width = crate::view::Headline::width_of(language.name().chars().count(), Picker::FONT_SIZE);
            assert!(width <= Ribbon::ITEM, "{} needs {width}px of {}", language.name(), Ribbon::ITEM);
        }
    }
}
