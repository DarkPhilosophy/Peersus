//! A picture an extension keeps for its pages (`peersus.pictures`), placed by a page: the finished pixels the extension made in
//! its own process, drawn as they are. The window decodes nothing: it only turns kept RGBA into the toolkit's image.
//!
//! WHAT IS MADE WHEN. The component renders when its props differ, compared by the picture's digest and the box, never by its
//! pixels; only then is the toolkit's image made again.
use freya::{elements::image::ImageHandle, engine::prelude::AlphaType, prelude::*};
use peersus_core::profile::Kept;
use std::sync::Arc;

#[derive(Clone)]
pub struct PictureView {
    /// The pixels, shared with the shelf that holds them; none while the window does not hold them (or nothing is kept).
    pub picture: Option<Arc<Kept>>,
    pub width: f32,
    pub height: f32,
    pub radius: f32,
}

impl PartialEq for PictureView {
    fn eq(&self, other: &Self) -> bool {
        let same = match (&self.picture, &other.picture) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a.digest() == b.digest(),
            (None, None) => true,
            _ => false,
        };
        same && self.width == other.width
            && self.height == other.height
            && self.radius == other.radius
    }
}

impl Component for PictureView {
    fn render(&self) -> impl IntoElement {
        let frame = rect()
            .width(Size::px(self.width))
            .height(Size::px(self.height))
            .corner_radius(self.radius)
            .overflow(Overflow::Clip);
        // Nothing kept, or not yet here: the box keeps its size, so the page does not move when the picture arrives.
        let Some(handle) = self.picture.as_ref().and_then(|picture| {
            ImageHandle::from_rgba(
                picture.width(),
                picture.height(),
                Bytes::copy_from_slice(picture.rgba()),
                AlphaType::Unpremul,
            )
        }) else {
            return frame;
        };
        frame.child(
            image(handle)
                .sampling_mode(SamplingMode::Trilinear)
                .aspect_ratio(AspectRatio::Max)
                .image_cover(ImageCover::Center)
                .width(Size::px(self.width))
                .height(Size::px(self.height)),
        )
    }
}
