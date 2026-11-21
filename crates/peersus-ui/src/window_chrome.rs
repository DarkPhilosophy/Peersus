//! The window's own chrome on a desktop: the dots that close, shrink and enlarge it, the zones along its edges that resize it,
//! and the sparks and the fuse drawn around a running code.
use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Dot {
    Minimise,
    Maximise,
    Close,
}

/// The diameter of every round control at the right of the header: the menu button and the three
/// window buttons are the same size, so they read as one row, and large enough to hit easily.
pub(crate) const HEADER_ROUND: f32 = 32.;

/// How many frames one run of the sparks of the fuse has: a second at sixty frames in the second. The sparks at each frame are a function of its
/// number (see `view::fuse_particles`), and the run goes round again when it ends.
pub(crate) const FUSE_FRAMES: u32 = 60;

/// The sparks of the tip of a fuse: a run of frames that goes round again for as long as `running`, and the frame it is at. Past the longest life
/// of a particle, so that every frame of the run has its sparks in flight and the picture of the run never empties. The picture of each frame is a
/// function of its number (see `view::fuse_particles`), so the flight of the sparks is as long as the run. With reduced motion, or while nothing
/// burns (`running` false), it does not move: no run, no redraw, and the frame stands still.
///
/// One function for every circle that has a fuse, so that the sparks are the same wherever a code burns. It makes its hook on EVERY draw, in the
/// same order, whether or not it runs: a hook made only sometimes would change the order between two draws of the same component.
pub(crate) fn use_sparks(reduced: bool, running: bool) -> u32 {
    let run =
        use_animation_with_dependencies(&(reduced, running), move |conf, (reduced, running)| {
            // Only a burning fuse runs and goes round again: a still one must not restart, or a zero-length run would finish and
            // begin again on every frame and keep the whole window redrawing.
            if !*reduced && *running {
                conf.on_creation(OnCreation::Run);
                conf.on_change(OnChange::Rerun);
                conf.on_finish(OnFinish::restart());
            }
            AnimNum::new(0., 1.).time(900).function(Function::Linear)
        });
    let share: f32 = run.get().value();
    view::FUSE_PARTICLE_FRAMES
        + if reduced || !running {
            0
        } else {
            (share * FUSE_FRAMES as f32) as u32
        }
}

/// The share of the life of a code that is left, from 1 to 0, as a smooth line: from where it is when `code` is first seen (`from`, the share left at
/// that moment) to nothing over the `seconds` the code still has, begun again for each code (`code` is what a new one changes). A fuse that is read
/// from the clock once a second moves its tip a step each second, and the sparks that fly from the tip then shake with it: this one moves with every
/// frame, as the fuse of the Pair page always did, so that the two are the same wherever a code burns. With reduced motion it does not move: it is
/// shown whole, and `running` false (no code on show) holds it still.
///
/// Made on EVERY draw, in the same order, whether or not it runs.
pub(crate) fn use_fuse(code: Option<u64>, from: f32, seconds: u64, reduced: bool) -> f32 {
    let run = use_animation_with_dependencies(&(code, reduced), move |conf, (code, reduced)| {
        conf.on_creation(OnCreation::Run);
        conf.on_change(OnChange::Rerun);
        AnimNum::new(from, 0.)
            .time(if *reduced || code.is_none() {
                0
            } else {
                seconds.saturating_mul(1_000)
            })
            .function(Function::Linear)
    });
    if reduced { 1. } else { run.get().value() }
}

/// One of the application's own window buttons: a disc in the colour of what it does, with the mark of that action on it.
/// The mark is the one colour that can be read on the disc, so it stays readable whatever colour the theme gives the disc.
#[derive(PartialEq)]
pub(crate) struct WindowDot(pub(crate) Dot, pub(crate) Palette);
impl Component for WindowDot {
    fn render(&self) -> impl IntoElement {
        let action = self.0;
        let (colour, mark) = match action {
            Dot::Minimise => (self.1.minimise, "minimise"),
            Dot::Maximise => (self.1.maximise, "maximise"),
            Dot::Close => (self.1.close, "close"),
        };
        let ink = glyph::ink_on(colour);
        let mut hovered = use_state(|| false);
        rect()
            .width(Size::px(HEADER_ROUND))
            .height(Size::px(HEADER_ROUND))
            .corner_radius(HEADER_ROUND / 2.)
            .center()
            // A hovered dot is dimmed slightly so it visibly reacts.
            .background(if hovered() {
                (colour.0 / 5 * 4, colour.1 / 5 * 4, colour.2 / 5 * 4)
            } else {
                colour
            })
            .child(glyph::draw(glyph::by_name(mark), glyph::WINDOW, ink))
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            // The press must not start a window drag from under the header.
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                let platform = Platform::get();
                platform
                    .clone()
                    .with_window(Platform::window_id(), move |window| match action {
                        Dot::Minimise => window.set_minimized(true),
                        Dot::Maximise => window.set_maximized(!window.is_maximized()),
                        Dot::Close => {
                            platform.close_window(window.id());
                        }
                    });
            })
    }
}

/// Edge and corner zones that start a native resize, since there is no OS frame to grab.
#[cfg(not(target_os = "android"))]
pub(crate) fn resize_zones(width: f32, height: f32) -> impl IntoElement {
    const EDGE: f32 = 6.;
    // A generous diagonal grip, separate from the thinner straight edge handles.
    const CORNER: f32 = 16.;
    let zone = |direction: ResizeDirection,
                cursor: CursorIcon,
                position: Position,
                w: f32,
                h: f32|
     -> Element {
        rect()
            .position(position)
            .width(Size::px(w.max(0.)))
            .height(Size::px(h.max(0.)))
            .cursor(cursor)
            // Freya lets events pass through exactly transparent backgrounds to non-ancestor
            // listeners too. A minimal solid hit surface prevents the header starting a window
            // move on the same press that starts a resize.
            .background(Color::from_rgb(0, 0, 0).with_a(1))
            .on_pointer_down(move |e: Event<PointerEventData>| {
                if !matches!(&*e, PointerEventData::Mouse(mouse) if mouse.button == Some(MouseButton::Left)) {
                    return;
                }
                e.stop_propagation();
                Platform::get().with_window(Platform::window_id(), move |window| {
                    if window.is_resizable() && !window.is_maximized() && window.fullscreen().is_none() {
                        let _ = window.drag_resize_window(direction);
                    }
                });
            })
            .into()
    };
    let at = || Position::new_absolute();
    // Absolute positioning changes the origin, not Fill's available-space calculation: after the
    // stacked body fills the root there is no height left. Use the measured window size for both
    // the overlay and its strips, and reserve the corners rather than overlapping their hit areas.
    rect()
        .position(at().top(0.).left(0.))
        .width(Size::px(width))
        .height(Size::px(height))
        // Window grips must also receive presses outside the rounded content's clipping path.
        .layer(Layer::Overlay)
        .child(zone(
            ResizeDirection::North,
            CursorIcon::NResize,
            at().top(0.).left(CORNER),
            width - CORNER * 2.,
            EDGE,
        ))
        .child(zone(
            ResizeDirection::South,
            CursorIcon::SResize,
            at().bottom(0.).left(CORNER),
            width - CORNER * 2.,
            EDGE,
        ))
        .child(zone(
            ResizeDirection::West,
            CursorIcon::WResize,
            at().top(CORNER).left(0.),
            EDGE,
            height - CORNER * 2.,
        ))
        .child(zone(
            ResizeDirection::East,
            CursorIcon::EResize,
            at().top(CORNER).right(0.),
            EDGE,
            height - CORNER * 2.,
        ))
        .child(zone(
            ResizeDirection::NorthWest,
            CursorIcon::NwResize,
            at().top(0.).left(0.),
            CORNER,
            CORNER,
        ))
        .child(zone(
            ResizeDirection::NorthEast,
            CursorIcon::NeResize,
            at().top(0.).right(0.),
            CORNER,
            CORNER,
        ))
        .child(zone(
            ResizeDirection::SouthWest,
            CursorIcon::SwResize,
            at().bottom(0.).left(0.),
            CORNER,
            CORNER,
        ))
        .child(zone(
            ResizeDirection::SouthEast,
            CursorIcon::SeResize,
            at().bottom(0.).right(0.),
            CORNER,
            CORNER,
        ))
}
