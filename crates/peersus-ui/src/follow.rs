//! Bounded gesture steps for previewing the last finished canvas bitmap.
//!
//! A `PSF1` frame's `settled` timestamp replaces the steps it already shows. The remaining
//! steps translate or scale the bitmap while the viewport stays fixed. At most [`MOST_STEPS`]
//! steps are kept; the two oldest become one, which settles when the later of them does.
use peersus_api::elements::Follow;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// How many steps are kept at most, however long the extension takes to answer.
pub(crate) const MOST_STEPS: usize = 256;
/// The most one gesture changes the scale by, up or down.
pub(crate) const MOST_FACTOR: f32 = 16.;
/// The scale all the steps together come to is kept between these.
pub(crate) const LEAST_SCALE: f32 = 1. / 4096.;
pub(crate) const MOST_SCALE: f32 = 4096.;
const SCALE_BOUNDS: (f64, f64) = (LEAST_SCALE as f64, MOST_SCALE as f64);

/// Where the content is, laid over what the extension drew: a point `p` of it is at `scale * p + (x, y)`, in logical pixels from the
/// content's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pose {
    pub scale: f32,
    pub x: f32,
    pub y: f32,
}

impl Pose {
    pub(crate) const IDENTITY: Pose = Pose {
        scale: 1.,
        x: 0.,
        y: 0.,
    };

    /// `self`, and after it `next`.
    fn then(self, next: Pose) -> Pose {
        Pose {
            scale: next.scale * self.scale,
            x: next.scale * self.x + next.x,
            y: next.scale * self.y + next.y,
        }
    }
}

#[derive(Clone, Copy)]
enum Step {
    Affine(Pose),
    Zoom { factor: f64, about: (f32, f32) },
}

impl Step {
    fn apply(self, pose: Pose, (min, max): (f64, f64)) -> Pose {
        match self {
            Self::Affine(next) => Pose {
                scale: (f64::from(pose.scale) * f64::from(next.scale)).clamp(min, max) as f32,
                ..pose.then(next)
            },
            Self::Zoom { factor, about } => {
                let scale = (f64::from(pose.scale) * factor).clamp(min, max) as f32;
                let factor = scale / pose.scale;
                pose.then(Pose {
                    scale: factor,
                    x: (1. - factor) * about.0,
                    y: (1. - factor) * about.1,
                })
            }
        }
    }
}

/// What the person has done on the canvas that its latest bitmap does not show yet.
#[derive(Default)]
pub(crate) struct Gestures {
    /// Each step with the `at` of the event it was made with, in the order they were made.
    steps: Vec<(u64, Step)>,
    /// The last `at` given: each event of the box is given a later one than the one before it, so that "shown by an answer" is
    /// decided by the time and never by two events that happened in one millisecond.
    last_at: u64,
    /// Where the pointer held on the box was last, in thousandths of a pixel.
    held: Option<(i32, i32)>,
    /// The scale of the last pinch sample, 10000 being as it was when the second finger came.
    pinched: u32,
}

impl Gestures {
    /// The `at` for the next gesture event of the box: `now`, or one more than the last given when that is not later.
    pub(crate) fn stamp(&mut self, now: u64) -> u64 {
        let at = now.max(self.last_at.saturating_add(1));
        self.last_at = at;
        at
    }

    fn push(&mut self, at: u64, step: Step) {
        if self.steps.len() == MOST_STEPS {
            let (later, after) = self.steps.remove(1);
            let first = self.steps[0].1;
            let bounds = (1e-6, 1e6);
            self.steps[0] = (
                later,
                Step::Affine(after.apply(first.apply(Pose::IDENTITY, bounds), bounds)),
            );
        }
        self.steps.push((at, step));
    }

    fn zoom(&mut self, at: u64, factor: f64, about: (f32, f32)) {
        if about.0.is_finite() && about.1.is_finite() && factor != 1. {
            self.push(at, Step::Zoom { factor, about });
        }
    }

    /// Apply wheel distance using the same logical pixels (and quantization) the extension receives.
    pub(crate) fn wheel(&mut self, at: u64, follow: Follow, dy: f64, about: (f32, f32)) {
        if follow.wheel_doubling != 0 && dy.is_finite() {
            let doublings = (-dy / f64::from(follow.wheel_doubling)).clamp(-4., 4.);
            self.zoom(at, doublings.exp2(), about);
        }
    }

    /// The pointer was pressed on the box where it is now (thousandths of a pixel): a drag may follow.
    pub(crate) fn press(&mut self, x: i32, y: i32) {
        self.held = Some((x, y));
    }

    /// The pointer held on the box moved to (x, y): the content moves with it. Nothing when it was not held.
    pub(crate) fn drag(&mut self, at: u64, x: i32, y: i32) {
        if let Some((from_x, from_y)) = self.held.replace((x, y)) {
            self.push(
                at,
                Step::Affine(Pose {
                    scale: 1.,
                    x: ((f64::from(x) - f64::from(from_x)) / 1000.) as f32,
                    y: ((f64::from(y) - f64::from(from_y)) / 1000.) as f32,
                }),
            );
        }
    }

    /// The pointer held on the box was let go of, or taken away.
    pub(crate) fn release(&mut self) {
        self.held = None;
    }

    /// A pinch sample: `scale` is absolute (10000 is as it was when the second finger came) and `begin` says this is that moment.
    /// The content grows by the change since the sample before, about the pinch's midpoint `about`.
    pub(crate) fn pinch(&mut self, at: u64, scale: u32, begin: bool, about: (f32, f32)) {
        if begin || self.pinched == 0 {
            self.pinched = scale.max(1);
            return;
        }
        let factor = f64::from(scale.max(1)) / f64::from(self.pinched);
        self.pinched = scale.max(1);
        self.zoom(
            at,
            factor.clamp(1. / f64::from(MOST_FACTOR), f64::from(MOST_FACTOR)),
            about,
        );
    }

    /// Where the content is, laid over what the extension drew: all the steps made after `settled`.
    fn pose(&self, settled: u64) -> Pose {
        let pose = self
            .steps
            .iter()
            .filter(|(at, _)| *at > settled)
            .fold(Pose::IDENTITY, |pose, (_, step)| {
                step.apply(pose, SCALE_BOUNDS)
            });
        if pose.x.is_finite() && pose.y.is_finite() {
            pose
        } else {
            Pose::IDENTITY
        }
    }

    /// Let go of the steps a frame with this `settled` already shows.
    fn settle(&mut self, settled: u64) {
        self.steps.retain(|(at, _)| *at > settled);
    }

    #[cfg(test)]
    pub(crate) fn steps(&self) -> usize {
        self.steps.len()
    }
}

/// State shared by a canvas's input handlers and bitmap preview.
#[derive(Clone, Default)]
pub(crate) struct Motion {
    gestures: Rc<RefCell<Gestures>>,
    /// Fixed viewport origin in the window's logical pixels.
    origin: Rc<Cell<(f32, f32)>>,
}

impl PartialEq for Motion {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.gestures, &other.gestures)
    }
}

impl Motion {
    pub(crate) fn gestures(&self) -> std::cell::RefMut<'_, Gestures> {
        self.gestures.borrow_mut()
    }

    pub(crate) fn set_origin(&self, origin: (f32, f32)) {
        self.origin.set(origin);
    }

    /// A point in the window as a point of the untransformed canvas viewport.
    pub(crate) fn about(&self, x: f32, y: f32) -> (f32, f32) {
        let (left, top) = self.origin.get();
        (x - left, y - top)
    }

    pub(crate) fn for_frame(&self, settled: u64) -> Pose {
        let mut gestures = self.gestures();
        gestures.settle(settled);
        gestures.pose(settled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wheel(doubling: u16) -> Follow {
        Follow {
            wheel_doubling: doubling,
            ..Follow::default()
        }
    }

    /// Where the point `p` of the content is on screen under `pose`.
    fn on_screen(pose: Pose, p: (f32, f32)) -> (f32, f32) {
        (pose.scale * p.0 + pose.x, pose.scale * p.1 + pose.y)
    }

    fn near(a: (f32, f32), b: (f32, f32)) {
        assert!(
            (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3,
            "{a:?} is not {b:?}"
        );
    }

    #[test]
    fn the_wheel_scales_about_the_pointer_by_two_to_the_travel_over_the_doubling() {
        let mut gestures = Gestures::default();
        // Up by one doubling of travel (negative dy): twice as large, and the point under the pointer stays under it.
        gestures.wheel(10, wheel(120), -120., (50., 30.));
        let pose = gestures.pose(0);
        assert!((pose.scale - 2.).abs() < 1e-5, "{pose:?}");
        near(on_screen(pose, (50., 30.)), (50., 30.));
        near(on_screen(pose, (60., 30.)), (70., 30.));
        // Down by one doubling: half as large, about the same point.
        let mut gestures = Gestures::default();
        gestures.wheel(10, wheel(120), 120., (50., 30.));
        let pose = gestures.pose(0);
        assert!((pose.scale - 0.5).abs() < 1e-5, "{pose:?}");
        near(on_screen(pose, (50., 30.)), (50., 30.));
        // A wheel that is not followed (doubling zero), and one that is not a number, change nothing.
        let mut gestures = Gestures::default();
        gestures.wheel(10, wheel(0), -120., (50., 30.));
        gestures.wheel(11, wheel(120), f64::NAN, (50., 30.));
        gestures.wheel(12, wheel(120), f64::INFINITY, (50., 30.));
        assert_eq!(gestures.pose(0), Pose::IDENTITY);
        assert_eq!(gestures.steps(), 0);
    }

    #[test]
    fn steps_compose_in_the_order_they_were_made() {
        let mut gestures = Gestures::default();
        gestures.press(10_000, 10_000);
        gestures.drag(1, 30_000, 25_000);
        gestures.wheel(2, wheel(100), -100., (40., 40.));
        gestures.drag(3, 35_000, 25_000);
        let pose = gestures.pose(0);
        // The content point that started at (0, 0): moved by (20, 15), doubled about (40, 40), moved by (5, 0).
        let first = (20., 15.);
        let second = (2. * (first.0 - 40.) + 40., 2. * (first.1 - 40.) + 40.);
        near(on_screen(pose, (0., 0.)), (second.0 + 5., second.1));
        assert!((pose.scale - 2.).abs() < 1e-5);
    }

    #[test]
    fn an_answer_takes_the_place_of_the_steps_it_shows_and_the_rest_stay() {
        let mut gestures = Gestures::default();
        for (at, dy) in [(10, -50.), (20, -50.), (30, -50.)] {
            gestures.wheel(at, wheel(100), dy, (0., 0.));
        }
        let all = gestures.pose(0);
        assert!((all.scale - 2f32.powf(1.5)).abs() < 1e-4);
        // Shown up to the second: the third alone is laid over the new content.
        let rest = gestures.pose(20);
        assert!((rest.scale - 2f32.powf(0.5)).abs() < 1e-4, "{rest:?}");
        // Without letting go of them yet, and after: the same.
        gestures.settle(20);
        assert_eq!(gestures.steps(), 1);
        assert_eq!(gestures.pose(20), rest);
        // An answer that shows nothing new (the same `settled`) leaves them as they are; one that shows the third leaves none.
        gestures.settle(20);
        assert_eq!(gestures.pose(20), rest);
        assert_eq!(gestures.pose(30), Pose::IDENTITY);
        gestures.settle(30);
        assert_eq!(gestures.steps(), 0);
        // An older `settled` than one already seen does not bring a step back.
        assert_eq!(gestures.pose(5), Pose::IDENTITY);
    }

    #[test]
    fn what_the_content_does_between_two_answers_is_the_same_whether_or_not_the_first_is_laid_over_it()
     {
        // The content the extension draws for the first two steps is the first two steps applied to the old content; the third laid over
        // it must put every point where all three put it over the old content: no jump when the answer comes.
        let mut gestures = Gestures::default();
        gestures.wheel(10, wheel(100), -100., (30., 20.));
        gestures.press(0, 0);
        gestures.drag(20, 12_000, -4_000);
        gestures.wheel(30, wheel(100), 50., (70., 60.));
        let everything = gestures.pose(0);
        let first_two = gestures.pose(0).then_only_steps(&gestures, 20);
        let rest = gestures.pose(20);
        for point in [(0., 0.), (10., 90.), (-30., 4.)] {
            near(
                on_screen(rest, on_screen(first_two, point)),
                on_screen(everything, point),
            );
        }
    }

    impl Pose {
        /// What the steps up to and including `until` come to: what an answer that shows them has drawn.
        fn then_only_steps(self, gestures: &Gestures, until: u64) -> Pose {
            gestures
                .steps
                .iter()
                .filter(|(at, _)| *at <= until)
                .fold(Pose::IDENTITY, |pose, (_, step)| {
                    step.apply(pose, SCALE_BOUNDS)
                })
        }
    }

    #[test]
    fn a_drag_follows_the_movement_of_the_held_pointer_only() {
        let mut gestures = Gestures::default();
        gestures.drag(1, 5_000, 5_000);
        assert_eq!(
            gestures.pose(0),
            Pose::IDENTITY,
            "not held, nothing follows"
        );
        gestures.press(10_000, 20_000);
        gestures.drag(2, 13_000, 18_000);
        gestures.drag(3, 14_500, 18_000);
        let pose = gestures.pose(0);
        assert_eq!((pose.scale, pose.x, pose.y), (1., 4.5, -2.));
        gestures.release();
        gestures.drag(4, 99_000, 99_000);
        assert_eq!(gestures.pose(0), pose, "let go of");
        // The movement of a second press starts where that press was.
        gestures.press(0, 0);
        gestures.drag(5, 1_000, 0);
        assert_eq!(gestures.pose(0).x, 5.5);
    }

    #[test]
    fn a_pinch_scales_by_its_change_about_its_midpoint() {
        let mut gestures = Gestures::default();
        gestures.pinch(1, 10_000, true, (10., 10.));
        assert_eq!(
            gestures.pose(0),
            Pose::IDENTITY,
            "the second finger alone changes nothing"
        );
        gestures.pinch(2, 15_000, false, (20., 20.));
        gestures.pinch(3, 30_000, false, (20., 20.));
        let pose = gestures.pose(0);
        assert!((pose.scale - 3.).abs() < 1e-4, "{pose:?}");
        near(on_screen(pose, (20., 20.)), (20., 20.));
        // A new pinch starts from one again.
        gestures.pinch(4, 10_000, true, (0., 0.));
        gestures.pinch(5, 5_000, false, (0., 0.));
        assert!((gestures.pose(0).scale - 1.5).abs() < 1e-4);
        // Settling the first pinch leaves the second.
        assert!((gestures.pose(3).scale - 0.5).abs() < 1e-4);
    }

    #[test]
    fn every_event_of_a_previewing_canvas_is_stamped_later_than_the_one_before() {
        let mut gestures = Gestures::default();
        let stamped: Vec<u64> = [100, 100, 100, 99, 250, 250]
            .iter()
            .map(|now| gestures.stamp(*now))
            .collect();
        assert_eq!(stamped, [100, 101, 102, 103, 250, 251]);
    }

    #[test]
    fn what_is_kept_and_what_is_drawn_stay_within_bounds_however_long_the_extension_is_silent() {
        let mut gestures = Gestures::default();
        for at in 1..=(10 * MOST_STEPS as u64) {
            gestures.wheel(at, wheel(1), -1e6, (3., 4.));
        }
        assert!(gestures.steps() <= MOST_STEPS, "{}", gestures.steps());
        let pose = gestures.pose(0);
        assert!(
            pose.scale <= MOST_SCALE && pose.scale.is_finite(),
            "{pose:?}"
        );
        assert!(pose.x.is_finite() && pose.y.is_finite());
        let mut gestures = Gestures::default();
        for at in 1..=(10 * MOST_STEPS as u64) {
            gestures.wheel(at, wheel(1), 1e6, (3., 4.));
        }
        assert!(gestures.pose(0).scale >= LEAST_SCALE);
        // One gesture changes the scale by a bounded factor.
        let mut gestures = Gestures::default();
        gestures.wheel(1, wheel(1), -1e6, (0., 0.));
        assert_eq!(gestures.pose(0).scale, MOST_FACTOR);
        // The oldest steps become one that settles with the later of them.
        let mut gestures = Gestures::default();
        for at in 1..=(MOST_STEPS as u64 + 1) {
            gestures.press(0, 0);
            gestures.drag(at, 1_000, 0);
            gestures.release();
        }
        assert_eq!(gestures.steps(), MOST_STEPS);
        assert_eq!(gestures.pose(0).x, MOST_STEPS as f32 + 1.);
        assert_eq!(gestures.pose(2).x, MOST_STEPS as f32 - 1.);
    }
}
