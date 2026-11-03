//! The application's icon, drawn from the same picture the packages use.
//! The editable source is `assets/peersus-icon.svg`; regenerate the packaged PNGs and Android
//! adaptive launcher resources with `python3 assets/generate_icons.py` from this crate.
use freya::{elements::image::ImageHandle, engine::prelude::AlphaType, prelude::*};

/// The mark as shipped: a PNG with no plate behind it (only the launcher has one), decoded once by the picture codec the workspace already has.
/// The second is the same mark for a light ground, with the cream turned to black so that it does not vanish.
const PNG: &[u8] = include_bytes!("../assets/peersus-icon.png");
const PNG_LIGHT: &[u8] = include_bytes!("../assets/peersus-icon-light.png");

/// Decode the icon into straight RGBA, with its side in pixels.
#[cfg(test)]
fn decode() -> Option<(u32, Vec<u8>)> {
    let picture = image::load_from_memory_with_format(PNG, image::ImageFormat::Png).ok()?;
    let rgba = picture.to_rgba8();
    Some((rgba.width(), rgba.into_raw()))
}

/// The icon as straight RGBA, `side` pixels square, reduced with a filter that keeps its edges
/// (a picture of 512 pixels brought to a few dozen would otherwise be a smear). `None` for a
/// size of nothing or if the icon cannot be decoded.
pub fn scaled(side: u32) -> Option<Vec<u8>> {
    if side == 0 {
        return None;
    }
    let picture = image::load_from_memory_with_format(PNG, image::ImageFormat::Png).ok()?;
    let reduced = image::imageops::resize(&picture.to_rgba8(), side, side, image::imageops::FilterType::Lanczos3);
    Some(reduced.into_raw())
}

/// The icon as an element `size` pixels square for a header of the colour `ground`, or `None` if the file cannot be decoded. The light
/// variant is used when the ground is light, so the mark is dark on it, as it is light on a dark ground.
pub fn element(size: f32, ground: (u8, u8, u8)) -> Option<Element> {
    let luminance = 0.299 * f32::from(ground.0) + 0.587 * f32::from(ground.1) + 0.114 * f32::from(ground.2);
    let bytes = if luminance > 140. { PNG_LIGHT } else { PNG };
    let picture = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?.to_rgba8();
    let side = picture.width();
    // The picture has transparent corners, so its colours are not premultiplied by the decoder.
    let handle = ImageHandle::from_rgba(side, side, Bytes::from(picture.into_raw()), AlphaType::Unpremul)?;
    Some(
        image(handle)
            .sampling_mode(SamplingMode::Trilinear)
            .width(Size::px(size))
            .height(Size::px(size))
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_icon_decodes_to_a_square_with_four_bytes_per_pixel() {
        let (side, rgba) = decode().expect("the icon must decode");
        assert!(side > 0);
        assert_eq!(rgba.len() as u32, side * side * 4);
    }

    #[test]
    fn the_icon_has_transparent_corners() {
        // A rounded square on a transparent field: the very first pixel is see-through.
        let (_, rgba) = decode().unwrap();
        assert_eq!(rgba[3], 0);
    }

    #[test]
    fn the_icon_scaled_down_has_exactly_the_size_asked_for() {
        for side in [21u32, 24, 48] {
            let rgba = scaled(side).expect("the icon must scale");
            assert_eq!(rgba.len() as u32, side * side * 4);
        }
    }

    #[test]
    fn the_icon_scaled_down_keeps_its_transparent_corners_and_its_colour() {
        let side = 21u32;
        let rgba = scaled(side).unwrap();
        let alpha = |x: u32, y: u32| rgba[((y * side + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0, "the corner must stay see-through");
        assert!(alpha(side / 2, side / 2) > 200, "the middle must be opaque");
        // And it is not blank: somewhere there is a pixel that is neither black nor white.
        assert!(rgba.chunks(4).any(|p| p[3] > 200 && p[0] != p[1]), "no colour left");
    }

    #[test]
    fn asking_for_no_size_gives_nothing() {
        assert_eq!(scaled(0), None);
    }
}
