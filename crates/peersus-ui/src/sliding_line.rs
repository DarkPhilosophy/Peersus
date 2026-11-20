//! One line of text that may be wider than the room it is given. It is never cut, broken or made smaller: the row
//! scrolls. While nobody touches it, it glides by itself to its end and back, with a pause at each end; once the
//! person scrolls it (or touches it), it stays where they leave it. A line that fits does not move, and nothing moves
//! with reduced motion or while the line is off screen.
//!
//! Where the line stands at each moment is [`Glide`], which knows no toolkit and is tested on its own.
use freya::{animation::*, prelude::*};

/// How an overflowing line moves when nobody touches it: a pause at the start, a glide to the end, a pause there and
/// a glide back, again and again.
pub struct Glide;

impl Glide {
    /// How long the line rests at each end, in milliseconds: long enough to read the start, and the end.
    pub const PAUSE_MS: u64 = 1600;
    /// How fast it glides, in pixels a second: slow enough to read while it moves.
    pub const PX_PER_S: f32 = 32.;
    /// Less than this much overflow, in pixels, is a line that fits.
    pub const FITS: f32 = 0.5;

    /// The time of one whole cycle (rest, out, rest, back) for `overflow` pixels, in milliseconds. Never zero.
    pub fn period_ms(overflow: f32) -> u64 {
        2 * (Self::PAUSE_MS + Self::travel_ms(overflow))
    }

    fn travel_ms(overflow: f32) -> u64 {
        ((overflow.max(0.) / Self::PX_PER_S) * 1000.)
            .round()
            .max(1.) as u64
    }

    /// How much wider the line is than its row, from a scroll view's (content, viewport) bounds; nothing before it is laid out.
    pub fn overflow(bounds: Option<(Size2D, Size2D)>) -> f32 {
        bounds
            .map(|(content, viewport)| (content.width - viewport.width).max(0.))
            .unwrap_or(0.)
    }

    /// How far the line is scrolled, in pixels from its start, at `progress` (0 to 1 of a cycle).
    pub fn offset(progress: f32, overflow: f32) -> f32 {
        if overflow < Self::FITS {
            return 0.;
        }
        let travel = Self::travel_ms(overflow) as f32;
        let pause = Self::PAUSE_MS as f32;
        let at = progress.clamp(0., 1.) * Self::period_ms(overflow) as f32;
        let eased = |t: f32| {
            // Smoothstep: no jolt where a glide meets a rest.
            let t = t.clamp(0., 1.);
            t * t * (3. - 2. * t)
        };
        if at < pause {
            0.
        } else if at < pause + travel {
            overflow * eased((at - pause) / travel)
        } else if at < 2. * pause + travel {
            overflow
        } else {
            overflow * (1. - eased((at - 2. * pause - travel) / travel))
        }
    }
}

/// One line of text in a row as wide as its parent, scrolled inside the row when it does not fit.
#[derive(PartialEq)]
pub struct SlidingLine {
    pub text: String,
    pub colour: (u8, u8, u8),
    pub size: f32,
    pub bold: bool,
    pub reduced_motion: bool,
}

impl Component for SlidingLine {
    fn render(&self) -> impl IntoElement {
        let mut controller = use_scroll_controller(ScrollConfig::default);
        // Once the person has touched or scrolled the line it is theirs: it is not moved again while it is shown.
        let mut held = use_state(|| false);
        let mut visible = use_state(|| false);
        let overflow = Glide::overflow(*controller.bounds.read());
        let moving = !self.reduced_motion && visible() && !held() && overflow >= Glide::FITS;
        let period = Glide::period_ms(overflow);
        let animation =
            use_animation_with_dependencies(&(moving, period), |conf, (moving, period)| {
                // A still line must not restart: only a moving one loops.
                if *moving {
                    conf.on_creation(OnCreation::Run);
                    conf.on_change(OnChange::Rerun);
                    conf.on_finish(OnFinish::restart());
                }
                AnimNum::new(0., 1.)
                    .time(*period)
                    .function(Function::Linear)
            });
        // The glide is applied as the animation runs, never while the person holds the line. The scroll view draws what
        // its controller says, so the person's own scrolling and this one move the same thing.
        use_side_effect(move || {
            let progress = animation.get().value();
            if *held.peek() {
                return;
            }
            let overflow = Glide::overflow(*controller.bounds.peek());
            controller.scroll_to_x(-(Glide::offset(progress, overflow).round() as i32));
        });
        let mut text = label()
            .text(self.text.clone())
            .color(self.colour)
            .font_size(self.size)
            .max_lines(1)
            .max_width(Size::Inner);
        if self.bold {
            text = text.font_weight(FontWeight::BOLD);
        }
        rect()
            .width(Size::fill())
            .on_visible(move |_| visible.set(true))
            .on_hidden(move |_| visible.set(false))
            // Any touch, press or wheel on the line hands it to the person.
            .on_pointer_down(move |_| held.set_if_modified(true))
            .on_wheel(move |_| held.set_if_modified(true))
            .child(
                ScrollView::new()
                    .scroll_controller(controller)
                    .direction(Direction::Horizontal)
                    .width(Size::fill())
                    .height(Size::auto())
                    .show_scrollbar(false)
                    .child(text),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_that_fits_never_moves() {
        for progress in [0., 0.3, 0.6, 1.] {
            assert_eq!(Glide::offset(progress, 0.), 0.);
            assert_eq!(Glide::offset(progress, 0.4), 0.);
        }
    }

    #[test]
    fn an_overflowing_line_rests_glides_out_rests_and_glides_back() {
        let overflow = 96.;
        let period = Glide::period_ms(overflow) as f32;
        let pause = Glide::PAUSE_MS as f32;
        let travel = period / 2. - pause;
        let at = |ms: f32| Glide::offset(ms / period, overflow);
        // Three seconds of travel for 96 px at 32 px a second.
        assert_eq!(travel, 3000.);
        assert_eq!(at(0.), 0.);
        assert_eq!(at(pause - 1.), 0.);
        let halfway = at(pause + travel / 2.);
        assert!((halfway - overflow / 2.).abs() < 0.01, "{halfway}");
        assert_eq!(at(pause + travel), overflow);
        assert_eq!(at(2. * pause + travel - 1.), overflow);
        assert!(at(2. * pause + travel + travel / 2.) < overflow);
        assert!(at(period - 1.) < 0.01);
        // Never past either end.
        for step in 0..=100 {
            let offset = Glide::offset(step as f32 / 100., overflow);
            assert!((0. ..=overflow).contains(&offset), "{offset}");
        }
    }

    #[test]
    fn the_cycle_is_never_empty() {
        assert!(Glide::period_ms(0.) > 0);
        assert!(Glide::period_ms(-5.) >= 2 * Glide::PAUSE_MS);
    }
}
