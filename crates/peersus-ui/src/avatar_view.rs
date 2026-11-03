//! The round picture that stands for a device: a profile picture when there is one, the first letter of
//! its name when there is not.
//!
//! WHAT IS DECIDED ELSEWHERE. Whether a picture or a letter is shown, and the pixels of the circle, are
//! [`crate::avatar::Look`], which is tried without a window. This only draws what that says, so there is
//! little here that a test could see, and little that can be wrong.
//!
//! WHAT IS MADE WHEN. A component renders when its props differ, and the circle is made again then. The
//! props are compared by the picture's digest, the name, the size and the colours: comparing sixty
//! kilobytes of pixels on every pass would cost more than making the circle.

use crate::{avatar::Look, theme::Palette};
use freya::{elements::image::ImageHandle, engine::prelude::AlphaType, prelude::*};
use peersus_core::profile::Picture;
use std::sync::Arc;

/// A picture shared with the shelf that holds it, compared by its digest: equal digests are equal pictures,
/// so a component holding one is compared by thirty-two bytes and not by sixty thousand.
#[derive(Clone)]
pub struct Shared(pub Arc<Picture>);

impl PartialEq for Shared {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.digest() == other.0.digest()
    }
}

#[derive(Clone)]
pub struct Avatar {
    /// The picture, shared with the shelf that holds it, so passing it on copies nothing.
    pub picture: Option<Arc<Picture>>,
    /// The device's name, of which the first letter shows when there is no picture.
    pub name: String,
    /// The circle's width and height in pixels.
    pub size: f32,
    pub palette: Palette,
}

impl PartialEq for Avatar {
    /// By the picture's digest and not by its pixels: the digest is made from them, so equal digests are
    /// equal pictures, and comparing it is thirty-two bytes and not sixty thousand.
    fn eq(&self, other: &Self) -> bool {
        self.picture.as_ref().map(|p| p.digest()) == other.picture.as_ref().map(|p| p.digest())
            && self.name == other.name
            && self.size == other.size
            && self.palette == other.palette
    }
}

impl Component for Avatar {
    fn render(&self) -> impl IntoElement {
        // Made from the current props on every render. A render happens when the props differ (`eq`
        // above compares the picture by digest, the name, the size and the colours), which is exactly when
        // the look must be made again; a memo here would keep the props it first captured and never see a
        // picture that arrives after the first draw.
        let look = Look::of(self.picture.as_deref(), &self.name, self.size.round() as u32).into_handle();
        let circle = rect().width(Size::px(self.size)).height(Size::px(self.size)).corner_radius(self.size / 2.);
        let shown: Element = match &look {
            Shown::Picture(handle) => circle
                .child(
                    image(handle.clone())
                        .sampling_mode(SamplingMode::Trilinear)
                        .width(Size::px(self.size))
                        .height(Size::px(self.size)),
                )
                .into(),
            Shown::Initial(letter) => circle
                .center()
                .background(self.palette.send)
                .child(
                    label()
                        .text(letter.to_string())
                        .color(self.palette.background)
                        .font_size(self.size * 0.44)
                        .font_weight(FontWeight::BOLD),
                )
                .into(),
        };
        shown
    }
}

/// What is drawn: the toolkit's image for a picture, or a letter.
#[derive(PartialEq)]
enum Shown {
    Picture(ImageHandle),
    Initial(char),
}

impl Look {
    /// The same decision, with a picture's pixels turned into the toolkit's image. A picture the toolkit
    /// will not take falls back to the letter, so a fault in drawing is never an empty circle.
    fn into_handle(self) -> Shown {
        match self {
            Look::Picture(rgba) => {
                let side = (rgba.len() as f64 / 4.0).sqrt() as u32;
                // Unpremultiplied: the corners outside the circle are see-through and must stay so.
                ImageHandle::from_rgba(side, side, Bytes::from(rgba), AlphaType::Unpremul)
                    .map_or(Shown::Initial('?'), Shown::Picture)
            }
            Look::Initial(letter) => Shown::Initial(letter),
        }
    }
}
