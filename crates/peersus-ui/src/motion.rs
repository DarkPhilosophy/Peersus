//! The one way anything appears or goes away in the application: a thing that is wanted fades in
//! and moves into its place, and the same thing backwards when it is no longer wanted. Nothing is
//! shown or removed in a single frame.
//!
//! The rules (how far, how long, when a thing may be taken off the screen) are `view::Popup`,
//! `view::Motion` and `view::Sidebar`, which know no toolkit and are tested on their own. This
//! file only turns them into components, and the one thing it must get right is that a move that
//! is turned round in the middle starts from where the thing is drawn at that moment and never
//! from either end.
//!
//! Two components share one hook for the progress:
//! - [`Reveal`] is for a thing laid over the page: it fills the window it is placed in, grows out
//!   of a corner, and may carry a layer under it that takes a click elsewhere.
//! - [`Slide`] is for a thing in the layout, such as the list of tabs beside a page: it keeps its
//!   own size, so the things next to it keep theirs, and it comes in from the left edge.

use freya::animation::*;
use freya::prelude::*;

use crate::view::{Motion, Popup};

/// Where a thing is on its way in or out, from 0 (not there) to 1 (whole), following `wanted`.
///
/// A move starts from where the thing was last drawn, never from either end, so turning round in
/// the middle does not jump. With `reduced_motion` the thing is simply there or not.
pub fn use_progress(wanted: bool, reduced: bool) -> f32 {
    // Where the thing was last drawn. A move starts from it.
    let mut drawn = use_state(|| if wanted && reduced { 1.0_f32 } else { 0.0_f32 });
    let from = Motion::start_from(*drawn.peek());
    let animation =
        use_animation_with_dependencies(&(wanted, reduced), move |conf, (wanted, reduced)| {
            conf.on_creation(OnCreation::Run);
            conf.on_change(OnChange::Rerun);
            let to = if *wanted { 1.0_f32 } else { 0.0_f32 };
            // Linear: `Popup::at` already shapes the curve, and easing twice would stall at the ends.
            AnimNum::new(from, to).time(Motion::duration_ms(from, to, *reduced))
        });
    let progress: f32 = animation.get().value();
    // Remember what is on the screen, so that the next move starts from here.
    drawn.set_if_modified(progress);
    progress
}

/// A width followed toward `target` pixels, from wherever it was last drawn, and starting from
/// nothing: a thing that is first drawn already wanted at some width therefore opens to it from
/// the left edge, and the things beside it are pushed along as it grows. `scale` is the width at
/// which a move takes the whole time, so that a short move is short and the speed is always the
/// same; with `reduced` it is at the target at once.
pub fn use_toward(target: f32, scale: f32, reduced: bool) -> f32 {
    let mut drawn = use_state(|| if reduced { target } else { 0.0_f32 });
    let from = *drawn.peek();
    let animation = use_animation_with_dependencies(
        &((target * 1000.) as i64, reduced),
        move |conf, (target, reduced)| {
            conf.on_creation(OnCreation::Run);
            conf.on_change(OnChange::Rerun);
            let to = *target as f32 / 1000.;
            // Linear in pixels: the curve is applied to the progress, not stacked on a width.
            AnimNum::new(from, to).time(Motion::duration_ms(from / scale, to / scale, *reduced))
        },
    );
    let width: f32 = animation.get().value();
    drawn.set_if_modified(width);
    width
}

/// `child` laid over the page with the motion of a popup while `wanted`, and still shown, going
/// away, until the move out has finished.
#[derive(Clone)]
pub struct Reveal {
    /// Whether the thing is wanted now.
    pub wanted: bool,
    /// Whether the person asked for no decorative motion: then it is there or not, at once.
    pub reduced_motion: bool,
    /// The point it grows out of: the corner of the button that opened it, or its middle.
    pub origin: TransformOrigin,
    /// The measured height affects the menu's maximum height, even during resize.
    pub room_height: f32,
    /// What is shown.
    pub child: Element,
    /// A layer under it that does not move, and that is there for as long as it is: the thing
    /// that takes a click anywhere else, to close it. `None` for a thing that needs none.
    pub backdrop: Option<Element>,
}

impl PartialEq for Reveal {
    fn eq(&self, other: &Self) -> bool {
        // The child is rebuilt on every draw and cannot be compared; what decides whether the
        // motion/placement can change is whether it is wanted, reduced, its origin or room.
        self.wanted == other.wanted
            && self.reduced_motion == other.reduced_motion
            && self.origin == other.origin
            && self.room_height == other.room_height
    }
}

// One concrete `Element` is returned because it leaves early with an empty one.
#[allow(refining_impl_trait)]
impl Component for Reveal {
    fn render(&self) -> Element {
        let progress = use_progress(self.wanted, self.reduced_motion);
        if !Motion::mounted(self.wanted, progress) {
            return rect().into();
        }
        let shown = if self.reduced_motion {
            Popup::still()
        } else {
            Popup::at(progress)
        };
        let moving = rect()
            .width(Size::fill())
            .height(Size::fill())
            .opacity(shown.opacity)
            .offset_y(shown.lift)
            .scale(shown.scale)
            .transform_origin(self.origin)
            .child(self.child.clone());
        match &self.backdrop {
            // A layer as large as the window that does not move, under the thing that does: the
            // moving thing is scaled around its own corner, and the layer is not scaled with it.
            Some(backdrop) => rect()
                .layer(crate::window_chrome::DESKTOP_MENU_LAYER)
                .position(Position::new_global().top(0.).left(0.))
                .width(Size::window_percent(100.))
                .height(Size::window_percent(100.))
                .child(backdrop.clone())
                .child(
                    moving
                        .position(Position::new_absolute().top(0.).left(0.))
                        .width(Size::percent(100.))
                        .height(Size::percent(100.)),
                )
                .into(),
            None => moving.into(),
        }
    }
}
