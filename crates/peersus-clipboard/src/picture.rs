//! A picture from another device, turned into pixels without trusting what it says about itself.
//!
//! A PNG states its own width and height in a few bytes at its start, and a decoder that believed
//! them would allocate whatever they implied. So the size it states is checked first, from its
//! header alone, against the RGBA this hands on (which is four bytes a pixel whatever the file
//! stored), and the decoder is given the same limits as a second barrier: a small file that
//! claims to be enormous is refused having cost almost nothing.
use crate::wire::{Error, MAX_DIMENSION, MAX_PIXEL_BYTES};
use image::{ImageDecoder, ImageFormat, ImageReader, Limits, codecs::png::PngDecoder};
use std::io::Cursor;

/// A picture as 8-bit RGBA, row by row, the way a clipboard takes one.
#[derive(PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Its size, never its contents: what a person copied is theirs, and a log or a failing test
/// has no business carrying it.
impl std::fmt::Debug for Rgba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Rgba {{ {} by {}, {} bytes }}",
            self.width,
            self.height,
            self.pixels.len()
        )
    }
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_PIXEL_BYTES);
    limits
}

/// Whether a picture of this size may be held as the RGBA this hands on.
///
/// The limit is on that RGBA, four bytes a pixel whatever the file stored: a grayscale file
/// is a quarter of the size when it is decoded, and the decoder's own memory budget is
/// reserved for that native size, so it would pass a picture that then quadruples. The
/// product is worked out in 64 bits as margin: with the dimensions already bounded it cannot
/// wrap in 32 bits today, but that should not depend on the order of these checks.
pub fn fits(width: u32, height: u32) -> bool {
    width != 0
        && height != 0
        && width <= MAX_DIMENSION
        && height <= MAX_DIMENSION
        && u64::from(width) * u64::from(height) * 4 <= MAX_PIXEL_BYTES
}

/// The pixels of a PNG, if it is a PNG within the limits.
///
/// The size it states is checked first, from its header alone, against the RGBA it would
/// become; only then is a pixel decoded. The decoder's own limits stay as a second barrier.
pub fn decode_png(bytes: &[u8]) -> Result<Rgba, Error> {
    let (width, height) = stated_size(bytes)?;
    if !fits(width, height) {
        return Err(Error::BadPicture);
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    reader.limits(limits());
    let image = reader.decode().map_err(|_| Error::BadPicture)?.into_rgba8();
    let (width, height) = image.dimensions();
    Ok(Rgba {
        width,
        height,
        pixels: image.into_raw(),
    })
}

/// A picture's size as its header states it, read without decoding any pixel, so a caller can
/// refuse a picture before spending anything on it.
///
/// Reading the header is not free: the decoder also reads the chunks before the pixels, and a
/// colour profile there is compressed, so a file of a few kilobytes can ask for hundreds of
/// megabytes before a single pixel is reached. So this reads under the same limits as the
/// pixels do, never the decoder's default of none.
pub fn stated_size(bytes: &[u8]) -> Result<(u32, u32), Error> {
    Ok(bounded_decoder(bytes)?.dimensions())
}

/// The one way a picture's header is read here: under the limits, never without them.
fn bounded_decoder(bytes: &[u8]) -> Result<PngDecoder<Cursor<&[u8]>>, Error> {
    PngDecoder::with_limits(Cursor::new(bytes), limits()).map_err(|_| Error::BadPicture)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, codecs::png::PngEncoder};

    /// A real PNG of `width` by `height`, each pixel a function of where it is.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let pixels: Vec<u8> = (0..width * height)
            .flat_map(|n| {
                [
                    (n % 251) as u8,
                    (n / 7 % 251) as u8,
                    (n / 13 % 251) as u8,
                    255,
                ]
            })
            .collect();
        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(&pixels, width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    /// The checksum PNG puts after each chunk: CRC-32 over its type and data.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// One PNG chunk: length, type, data and the checksum over type and data.
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        let mut checked = kind.to_vec();
        checked.extend_from_slice(data);
        out.extend_from_slice(&checked);
        out.extend_from_slice(&crc32(&checked).to_be_bytes());
        out
    }

    /// A complete, well-formed PNG that STATES `width` by `height` but carries no pixels worth
    /// the name: what a hostile device sends to see how much is allocated for a promise.
    ///
    /// It is complete on purpose (header, one empty compressed data block, end marker, every
    /// checksum right). A decoder that refused a truncated file would refuse this for the
    /// wrong reason; with nothing wrong with the file, the only thing left to object to is the
    /// size it states.
    fn lying_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut header = Vec::new();
        header.extend_from_slice(&width.to_be_bytes());
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
        bytes.extend(chunk(b"IHDR", &header));
        // zlib: header, one final stored block of no bytes, then the Adler-32 of nothing.
        bytes.extend(chunk(
            b"IDAT",
            &[
                0x78, 0x01, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01,
            ],
        ));
        bytes.extend(chunk(b"IEND", &[]));
        bytes
    }

    #[test]
    fn a_picture_comes_back_with_its_size_and_every_pixel() {
        let bytes = png(5, 3);
        let picture = decode_png(&bytes).unwrap();
        assert_eq!((picture.width, picture.height), (5, 3));
        assert_eq!(picture.pixels.len(), 5 * 3 * 4);
        // The pixel at (4, 2) is number 14, which is where the generator put it.
        let at = (2 * 5 + 4) * 4;
        assert_eq!(picture.pixels[at..at + 4], [14, 2, 1, 255]);
    }

    #[test]
    fn a_picture_that_claims_to_be_enormous_is_refused_without_being_allocated() {
        // Far past the limit on both sides, in a complete file of well under two hundred
        // bytes: what it states would take tens of gigabytes if it were believed.
        let bytes = lying_png(100_000, 100_000);
        assert!(bytes.len() < 128, "{} bytes", bytes.len());
        assert_eq!(decode_png(&bytes), Err(Error::BadPicture));
    }

    #[test]
    fn the_dimension_limit_is_exact_on_each_side() {
        // Real pictures, small in memory, that WOULD decode if only the size were allowed: so
        // the one thing that can refuse them is the limit on that dimension.
        for (width, height) in [(MAX_DIMENSION + 1, 1), (1, MAX_DIMENSION + 1)] {
            assert_eq!(
                decode_png(&png(width, height)),
                Err(Error::BadPicture),
                "{width} by {height} is one past the limit"
            );
        }
        // Exactly at the limit, in each direction, a real picture is accepted whole.
        for (width, height) in [(MAX_DIMENSION, 1), (1, MAX_DIMENSION)] {
            let picture = decode_png(&png(width, height)).unwrap();
            assert_eq!((picture.width, picture.height), (width, height));
        }
    }

    #[test]
    fn the_memory_limit_is_exact_not_only_the_dimensions() {
        // 4096 wide and just over 64 MiB of pixels tall is inside the dimension limit on both
        // sides, so the only thing that can refuse this real picture is the memory limit.
        let height = (MAX_PIXEL_BYTES / (4096 * 4)) as u32 + 1;
        assert!(height <= MAX_DIMENSION);
        assert_eq!(
            decode_png(&png(4096, height)),
            Err(Error::BadPicture),
            "a picture that would take more memory than allowed was not refused"
        );
        // And one exactly at the memory limit is accepted whole.
        let height = (MAX_PIXEL_BYTES / (4096 * 4)) as u32;
        let picture = decode_png(&png(4096, height)).unwrap();
        assert_eq!((picture.width, picture.height), (4096, height));
    }

    #[test]
    fn what_may_be_handed_on_is_decided_by_the_rgba_it_would_take_exactly() {
        // At four bytes a pixel, exactly the limit fits and one more pixel row does not.
        let row = 4096u32;
        let rows = (MAX_PIXEL_BYTES / (u64::from(row) * 4)) as u32;
        assert!(fits(row, rows));
        assert!(!fits(row, rows + 1));
        // Each dimension has its own limit, whatever the product.
        assert!(fits(MAX_DIMENSION, 1));
        assert!(!fits(MAX_DIMENSION + 1, 1));
        assert!(!fits(1, MAX_DIMENSION + 1));
        // Nothing at all is not a picture.
        assert!(!fits(0, 5));
        assert!(!fits(5, 0));
        // Sizes far past the limits are refused. (They are refused by the per-dimension check
        // before the product is ever worked out, so this cannot show whether the product is
        // wide enough: with the dimensions bounded it could not wrap in 32 bits either. The
        // 64-bit product is margin against the checks being reordered or the limit raised.)
        assert!(!fits(u32::MAX, u32::MAX));
        assert!(!fits(1 << 16, 1 << 16));
    }

    /// A real grayscale PNG: one byte a pixel as decoded, though four as the RGBA this hands on.
    fn gray_png(width: u32, height: u32) -> Vec<u8> {
        let pixels = vec![0x7Fu8; (width as usize) * (height as usize)];
        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(&pixels, width, height, image::ExtendedColorType::L8)
            .unwrap();
        out
    }

    #[test]
    fn the_memory_limit_counts_the_rgba_that_is_handed_on_not_only_what_the_file_decodes_to() {
        // As a grayscale file, 4096 by 4097 decodes to about 16 MiB, well inside the limit. As
        // the RGBA this hands on it is over 64 MiB. The limit is on what is held, so it is the
        // RGBA that has to fit.
        let height = (MAX_PIXEL_BYTES / (4096 * 4)) as u32 + 1;
        assert!(height <= MAX_DIMENSION);
        assert!(
            u64::from(height) * 4096 < MAX_PIXEL_BYTES,
            "the test only means something if the file alone is within the limit"
        );
        assert_eq!(
            decode_png(&gray_png(4096, height)),
            Err(Error::BadPicture),
            "a grayscale picture that becomes more RGBA than allowed was not refused"
        );
        // And a grayscale picture whose RGBA exactly fits is accepted whole.
        let height = (MAX_PIXEL_BYTES / (4096 * 4)) as u32;
        let picture = decode_png(&gray_png(4096, height)).unwrap();
        assert_eq!((picture.width, picture.height), (4096, height));
        assert_eq!(picture.pixels.len() as u64, MAX_PIXEL_BYTES);
    }

    /// A zlib stream of `bytes`.
    fn zlib(bytes: &[u8]) -> Vec<u8> {
        use flate2::{Compression, write::ZlibEncoder};
        use std::io::Write;
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// A valid 1 by 1 picture with a colour profile that EXPANDS to `profile_len` bytes: a
    /// small file whose header chunks, before a single pixel, ask a decoder for far more memory
    /// than the file weighs.
    fn png_with_profile(profile_len: usize) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut header = Vec::new();
        header.extend_from_slice(&1u32.to_be_bytes());
        header.extend_from_slice(&1u32.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
        bytes.extend(chunk(b"IHDR", &header));
        // iCCP: a name, a method byte (0 = zlib), then the compressed profile.
        let mut profile = b"p\0\0".to_vec();
        profile.extend(zlib(&vec![0u8; profile_len]));
        bytes.extend(chunk(b"iCCP", &profile));
        // One pixel: a filter byte and four channel bytes.
        bytes.extend(chunk(b"IDAT", &zlib(&[0, 10, 20, 30, 255])));
        bytes.extend(chunk(b"IEND", &[]));
        bytes
    }

    #[test]
    fn a_colour_profile_that_expands_past_the_budget_is_not_kept_by_the_decoder_used_here() {
        // The profile is 70 MiB once expanded, in a file of well under a megabyte. The png
        // library drops a profile that overruns its budget and carries on, so the picture itself
        // is still accepted; what must not happen is the profile being expanded and KEPT.
        let hostile = png_with_profile(70 * 1024 * 1024);
        assert!(hostile.len() < 1024 * 1024, "{} bytes", hostile.len());

        // The control: the decoder's default, with no budget, does keep all of it. Without
        // this the check below could be passing because nothing ever reads a profile.
        let mut unbounded = PngDecoder::new(Cursor::new(&hostile[..])).unwrap();
        let kept = unbounded.icc_profile().unwrap();
        assert_eq!(kept.map(|profile| profile.len()), Some(70 * 1024 * 1024));

        // The decoder as this module builds it for every header keeps nothing of it.
        let mut bounded = bounded_decoder(&hostile).unwrap();
        assert_eq!(bounded.icc_profile().unwrap(), None);

        // And the picture is read from it all the same.
        let picture = decode_png(&hostile).unwrap();
        assert_eq!((picture.width, picture.height), (1, 1));
        assert_eq!(picture.pixels, [10, 20, 30, 255]);
        // A profile that is no trouble is just a good picture.
        let fine = png_with_profile(1024);
        assert_eq!(decode_png(&fine).unwrap().pixels, [10, 20, 30, 255]);
        assert_eq!(stated_size(&fine), Ok((1, 1)));
    }

    #[test]
    fn a_picture_never_prints_its_pixels() {
        // What was copied is the user's, and a failing test or a log line must not carry it.
        let secret = Rgba {
            width: 2,
            height: 1,
            pixels: vec![111, 222, 133, 255, 1, 2, 3, 4],
        };
        let shown = format!("{secret:?}");
        assert!(shown.contains('2') && shown.contains('1'), "{shown}");
        assert!(!shown.contains("222") && !shown.contains("133"), "{shown}");
        assert!(shown.len() < 100, "{shown}");
    }

    #[test]
    fn something_that_is_not_a_png_is_refused() {
        for bytes in [
            Vec::new(),
            b"GIF89a".to_vec(),
            b"\x89PNG\r\n\x1a\n".to_vec(),
            vec![0u8; 100],
            {
                let mut cut = png(8, 8);
                cut.truncate(cut.len() / 2);
                cut
            },
        ] {
            assert_eq!(decode_png(&bytes), Err(Error::BadPicture), "{bytes:?}");
        }
    }

    #[test]
    fn the_stated_size_is_read_without_decoding_any_pixel() {
        assert_eq!(stated_size(&png(7, 4)).unwrap(), (7, 4));
        // A header with no pixels behind it still states its size, so a caller can refuse a
        // picture before spending anything on it: a size within the limits is reported...
        assert_eq!(stated_size(&lying_png(100, 3)).unwrap(), (100, 3));
        // ...and one past them is refused at the header, under the same limits as the pixels.
        assert_eq!(stated_size(&lying_png(100_000, 3)), Err(Error::BadPicture));
        assert_eq!(stated_size(&lying_png(3, 100_000)), Err(Error::BadPicture));
        assert_eq!(stated_size(b"nope"), Err(Error::BadPicture));
    }
}
