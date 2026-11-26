//! Decoding a picture inside an extension's helper process ([`peersus_api::pictures::DECODE_FUNCTION`]). The application never
//! runs this for an extension: a damaged or enormous picture costs only the process of the extension that asked.
use peersus_api::pictures::{MAX_DECODE_EDGE, MAX_DECODE_INPUT, MAX_DECODE_PIXELS};
use skia_safe::{
    AlphaType, ColorSpace, ColorType, Data, FilterMode, ISize, ImageInfo, MipmapMode, Pixmap,
    SamplingOptions,
    codec::{self, Codec},
    image::CachingHint,
};

/// Why a picture was not decoded.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("not a picture this decoder reads, or a damaged one")]
    Invalid,
    #[error("the picture or the request is past its bound")]
    TooLarge,
}

/// A decoded picture: `width * height * 4` bytes of unpremultiplied sRGB RGBA.
#[derive(Debug, PartialEq, Eq)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decode `encoded` (PNG, JPEG, WebP, GIF's first frame or BMP), turn it upright as its EXIF orientation says, and make it no
/// larger than `max_edge` on its longest side, keeping its proportions; never enlarge it.
pub fn decode(encoded: &[u8], max_edge: u32) -> Result<Decoded, DecodeError> {
    if encoded.is_empty() {
        return Err(DecodeError::Invalid);
    }
    if encoded.len() > MAX_DECODE_INPUT || !(1..=MAX_DECODE_EDGE).contains(&max_edge) {
        return Err(DecodeError::TooLarge);
    }
    let decoders = [
        codec::png_decoder::decoder(),
        codec::jpeg_decoder::decoder(),
        codec::webp_decoder::decoder(),
        codec::gif_decoder::decoder(),
        codec::bmp_decoder::decoder(),
    ];
    let mut codec = Codec::from_data_with_decoders(Data::new_copy(encoded), &decoders)
        .ok_or(DecodeError::Invalid)?;
    let full = codec.dimensions();
    if full.width <= 0 || full.height <= 0 {
        return Err(DecodeError::Invalid);
    }
    // A format that can decode smaller (JPEG, by eighths) does, so a large photo never exists whole here; another decodes whole.
    let longest = full.width.max(full.height) as f32;
    let scaled = codec.get_scaled_dimensions((max_edge as f32 / longest).min(1.));
    if scaled.width <= 0
        || scaled.height <= 0
        || scaled.width as u64 * scaled.height as u64 > MAX_DECODE_PIXELS
    {
        return Err(DecodeError::TooLarge);
    }
    let info = ImageInfo::new(
        scaled,
        ColorType::RGBA8888,
        AlphaType::Premul,
        ColorSpace::new_srgb(),
    );
    // `get_image` applies the EXIF orientation, so its sides may be swapped.
    let upright = codec
        .get_image(info, None)
        .map_err(|_| DecodeError::Invalid)?;
    let (width, height) = fit(upright.width(), upright.height(), max_edge);
    let out = ImageInfo::new(
        ISize::new(width as i32, height as i32),
        ColorType::RGBA8888,
        AlphaType::Unpremul,
        ColorSpace::new_srgb(),
    );
    let row = width as usize * 4;
    let mut rgba = vec![0; row * height as usize];
    let pixmap = Pixmap::new(&out, &mut rgba, row).ok_or(DecodeError::Invalid)?;
    // Mipmaps make a large step down an average of what it covers rather than a sample of it.
    let source = upright.with_default_mipmaps().unwrap_or(upright);
    if !source.scale_pixels(
        &pixmap,
        SamplingOptions::new(FilterMode::Linear, MipmapMode::Linear),
        CachingHint::Disallow,
    ) {
        return Err(DecodeError::Invalid);
    }
    drop(pixmap);
    Ok(Decoded {
        width,
        height,
        rgba,
    })
}

/// `width` by `height` made no larger than `max_edge` on its longest side, proportions kept, never enlarged, never zero.
fn fit(width: i32, height: i32, max_edge: u32) -> (u32, u32) {
    let (width, height) = (width.max(1) as u64, height.max(1) as u64);
    let longest = width.max(height);
    let edge = u64::from(max_edge).min(longest);
    let side = |n: u64| ((n * edge + longest / 2) / longest).max(1) as u32;
    (side(width), side(height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use skia_safe::{EncodedImageFormat, surfaces};

    /// A `width` by `height` picture, red on the left half and blue on the right, encoded as `format`.
    fn encoded(width: i32, height: i32, format: EncodedImageFormat) -> Vec<u8> {
        let mut surface = surfaces::raster_n32_premul((width, height)).unwrap();
        let canvas = surface.canvas();
        let mut paint = skia_safe::Paint::default();
        paint.set_color(skia_safe::Color::RED);
        canvas.draw_rect(
            skia_safe::Rect::from_wh(width as f32 / 2., height as f32),
            &paint,
        );
        paint.set_color(skia_safe::Color::BLUE);
        canvas.draw_rect(
            skia_safe::Rect::from_xywh(width as f32 / 2., 0., width as f32 / 2., height as f32),
            &paint,
        );
        let image = surface.image_snapshot();
        image
            .encode(None, format, 95)
            .expect("the test picture encodes")
            .as_bytes()
            .to_vec()
    }

    #[test]
    fn a_photo_is_made_no_larger_than_the_edge_keeping_its_proportions_and_its_colours() {
        for format in [EncodedImageFormat::PNG, EncodedImageFormat::JPEG] {
            let picture = decode(&encoded(1200, 600, format), 256).unwrap();
            assert_eq!((picture.width, picture.height), (256, 128), "{format:?}");
            assert_eq!(picture.rgba.len(), 256 * 128 * 4);
            let at = |x: usize, y: usize| &picture.rgba[(y * 256 + x) * 4..][..4];
            let (left, right) = (at(20, 64), at(236, 64));
            assert!(
                left[0] > 200 && left[2] < 60 && left[3] == 255,
                "{format:?} {left:?}"
            );
            assert!(right[2] > 200 && right[0] < 60, "{format:?} {right:?}");
        }
    }

    #[test]
    fn a_small_picture_is_never_enlarged() {
        let picture = decode(&encoded(40, 30, EncodedImageFormat::PNG), 256).unwrap();
        assert_eq!((picture.width, picture.height), (40, 30));
    }

    #[test]
    fn what_is_not_a_picture_or_is_past_a_bound_is_refused_without_a_fault() {
        assert_eq!(decode(b"", 256), Err(DecodeError::Invalid));
        assert_eq!(
            decode(b"not a picture at all", 256),
            Err(DecodeError::Invalid)
        );
        let png = encoded(64, 64, EncodedImageFormat::PNG);
        // Only the signature: refused. Half a picture is shown as far as it goes (Skia fills the rest), never a fault.
        assert_eq!(decode(&png[..12], 256), Err(DecodeError::Invalid));
        let _ = decode(&png[..png.len() / 2], 256);
        assert_eq!(decode(&png, 0), Err(DecodeError::TooLarge));
        assert_eq!(
            decode(&png, MAX_DECODE_EDGE + 1),
            Err(DecodeError::TooLarge)
        );
        assert_eq!(
            decode(&vec![0; MAX_DECODE_INPUT + 1], 256),
            Err(DecodeError::TooLarge)
        );
    }

    #[test]
    fn fitting_keeps_proportions_and_never_reaches_zero() {
        assert_eq!(fit(4000, 3000, 256), (256, 192));
        assert_eq!(fit(3000, 4000, 256), (192, 256));
        assert_eq!(fit(10_000, 1, 256), (256, 1));
        assert_eq!(fit(100, 50, 256), (100, 50));
    }
}
