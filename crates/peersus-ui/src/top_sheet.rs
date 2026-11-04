//! Settings on a phone: an overlay that comes down from the top of the screen over the page, with a dark layer under it that closes it when
//! pressed. The page under it (Send, Receive or Peers) is not left: closing the overlay shows it exactly as it was, and the bar of places
//! at the bottom stays clear of it.
//!
//! The shared Settings page owns its fixed title/search/close header and its scrolling body. This sheet only supplies the overlay surface.
//!
//! Where the panel is at each moment, and how dark the layer is, are `view::TopSheet`, which knows no toolkit and is tested on its own.
//! This file only draws them, with the same progress that every other thing in the application appears with.
use freya::prelude::*;

use crate::{motion, theme::Palette, view::TopSheet};

/// The overlay and the layer under it. Always given to the root, which shows it while it is wanted and for as long as it takes to go away.
#[derive(PartialEq)]
pub struct Sheet {
    pub wanted: bool,
    pub reduced_motion: bool,
    /// The margin the system keeps at the top of the screen (the status bar), in pixels.
    pub inset_top: f32,
    /// What is under the panel and must stay clear of it: the bar of places at the bottom, and the margin the system keeps there, in pixels.
    pub inset_bottom: f32,
    /// The height of the window, in pixels.
    pub room: f32,
    pub palette: Palette,
    /// The shared Settings shell, including its fixed header and scrollable groups.
    pub body: Element,
    pub on_close: EventHandler<()>,
}

// One concrete `Element` is returned because it leaves early with an empty one.
#[allow(refining_impl_trait)]
impl Component for Sheet {
    fn render(&self) -> Element {
        let progress = motion::use_progress(self.wanted, self.reduced_motion);
        if !crate::view::Motion::mounted(self.wanted, progress) {
            return rect().into();
        }
        // With reduced motion it is simply there or not.
        let progress = if self.reduced_motion { 1. } else { progress };
        let palette = self.palette;
        // The panel stops above the bar of places and floats a little off it, so that it reads as a layer over the page and its rounded
        // lower corners are seen; the bar is not under the dark layer and can still be pressed.
        const FLOAT: f32 = 12.;
        let usable = (self.room - self.inset_top - self.inset_bottom - FLOAT).max(0.);
        let height = TopSheet::height(usable);
        let on_close = self.on_close.clone();

        let sheet = rect()
            .layer(Layer::Overlay)
            .position(
                Position::new_global()
                    .top(TopSheet::top(progress, height, self.inset_top))
                    .left(0.),
            )
            .width(Size::fill())
            .height(Size::px(height))
            .content(Content::flex())
            .background(palette.raised)
            // A full outline, not only at the bottom, so that the panel is seen as a layer of its own over the page: on a dark page its
            // lifted background and this line are what set its edge, rounded at the lower corners, apart from what is under it.
            .border(
                Border::new()
                    .fill(palette.border)
                    .width(1.)
                    .alignment(BorderAlignment::Inner),
            )
            .corner_radius((0., 0., 20., 20.))
            .overflow(Overflow::Clip)
            // The layer under it closes the panel on a pointer-down: this one must not reach it. Nor must a wheel, a touch or a move: what
            // is under the panel is not reached through it, and the page under does not scroll while the panel is open.
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_pointer_move(|e: Event<PointerEventData>| e.stop_propagation())
            .on_wheel(|e: Event<WheelEventData>| e.stop_propagation())
            .on_touch_start(|e: Event<TouchEventData>| e.stop_propagation())
            .on_touch_move(|e: Event<TouchEventData>| e.stop_propagation())
            .on_touch_end(|e: Event<TouchEventData>| e.stop_propagation())
            .child(
                rect()
                    .width(Size::fill())
                    .height(Size::flex(1.))
                    .child(self.body.clone()),
            )
            // The outline is drawn last, over the body, as a layer of its own that takes no touch: the body is a scrolling page that fills the
            // panel and would otherwise be drawn over the rounded lower corners and over the line at the bottom.
            .child(
                rect()
                    .position(Position::new_absolute().top(0.).left(0.))
                    .width(Size::fill())
                    .height(Size::px(height))
                    .interactive(false)
                    .corner_radius((0., 0., 20., 20.))
                    .border(
                        Border::new()
                            .fill(palette.border)
                            .width(1.)
                            .alignment(BorderAlignment::Inner),
                    ),
            );

        // The dark layer is a plain element over the whole area, on the overlay layer, with the panel as its child. A press that begins on it
        // closes the panel. What is under it is not reached through it: the page is turned off for pointers while the panel is open (see
        // `Connected`), which is what stops a touch or a scroll from passing through, and not a button put there to swallow them.
        rect()
            .layer(Layer::Overlay)
            .position(Position::new_global().top(0.).left(0.))
            .width(Size::fill())
            .height(Size::fill())
            .background((0, 0, 0, TopSheet::scrim(progress)))
            .on_pointer_down(move |_| on_close.call(()))
            .on_pointer_move(|e: Event<PointerEventData>| e.stop_propagation())
            .on_wheel(|e: Event<WheelEventData>| e.stop_propagation())
            .on_touch_start(|e: Event<TouchEventData>| e.stop_propagation())
            .on_touch_move(|e: Event<TouchEventData>| e.stop_propagation())
            .on_touch_end(|e: Event<TouchEventData>| e.stop_propagation())
            .child(sheet)
            .into()
    }
}
