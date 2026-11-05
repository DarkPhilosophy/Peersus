//! Reading the picture file a person chose into a profile picture.
//!
//! WHERE THIS RUNS. Only here, in the window, on a file the person themselves picked on their own
//! disk. Nothing received from another device ever reaches this reader: a picture that travels is
//! plain pixels, which need no decoder. So the decoder below is trusted with the person's own
//! files, and is still held to limits, because a small file can announce an enormous picture.
//!
//! THE ORDER IS THE POINT. A file states its own width and height near its start. They are read
//! FIRST, from the header alone, and a picture whose sides or whose decoded size would be more than
//! this reader allows is refused before a single pixel is decoded or a buffer for it is allocated.
//! The decoder's own limits stay on as a second barrier. The file's size on disk is bounded before
//! it is read at all, and a link is not followed: what is read is the file that was chosen.

use crate::avatar::{self, Decoded};
use image::{ImageDecoder, ImageFormat, ImageReader, Limits};
use peersus_core::profile::Picture;
use std::io::{Cursor, Read};
use std::path::Path;

/// Largest file read, in bytes. A phone photograph is a few megabytes; this leaves room above that
/// and stops a file of a different kind altogether from being pulled into memory.
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// Longest side a picture may DECLARE before it is decoded. Far above what the profile picture
/// keeps (it is reduced to `profile::MAX_EDGE`), so a large photograph is accepted, and far below
/// what could not be held.
pub const MAX_DECLARED_EDGE: u32 = 12_000;
/// Most bytes the decoded pixels may take. `12_000 x 12_000` would be over half a gigabyte; this is
/// a picture of about 25 megapixels, which is more than a camera phone makes.
pub const MAX_DECODED_BYTES: u64 = 100 * 1024 * 1024;

/// Why a file was not made a profile picture, in words for the person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The file could not be opened or read, or is not a plain file.
    Unreadable,
    /// The file is larger than [`MAX_FILE_BYTES`].
    FileTooLarge,
    /// It is not a PNG or a JPEG.
    NotAPicture,
    /// It announces a picture larger than this reader allows.
    PictureTooLarge,
    /// It is the kind of file it says, but cannot be decoded.
    Damaged,
}

impl Refused {
    pub fn words(self) -> &'static str {
        match self {
            Self::Unreadable => "That file could not be read.",
            Self::FileTooLarge => "That file is too large to be a profile picture.",
            Self::NotAPicture => "Choose a PNG or a JPEG picture.",
            Self::PictureTooLarge => "That picture is too large to use.",
            Self::Damaged => "That picture could not be read.",
        }
    }
}

/// The profile picture in the file at `path`.
pub fn read(path: &Path) -> Result<Picture, Refused> {
    let bytes = read_bounded(path)?;
    decode(&bytes)
}

/// The profile picture in a file that is already open (one the system's own picker handed over, which has no path to
/// follow). It is held to the same bounds as a path: the size is looked at before reading, and the read itself stops
/// one byte past the limit, so a file that lies about its size, or one that has none, is still seen to be too large.
pub fn read_file(file: std::fs::File) -> Result<Picture, Refused> {
    let bytes = read_open(file)?;
    decode(&bytes)
}

/// The bytes of a plain file, bounded, without following a link and without reading past the limit.
fn read_bounded(path: &Path) -> Result<Vec<u8>, Refused> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // NOFOLLOW: a link is refused, so what is read is the file that was chosen.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    // Windows: the link itself is opened, never what it leads to, and it is not a plain file below.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|_| Refused::Unreadable)?;
    let meta = file.metadata().map_err(|_| Refused::Unreadable)?;
    if !meta.is_file() {
        return Err(Refused::Unreadable);
    }
    read_open(file)
}

/// The bytes of an open file, bounded: its size is checked first when it states one, and the read stops past the limit.
fn read_open(file: std::fs::File) -> Result<Vec<u8>, Refused> {
    let meta = file.metadata().map_err(|_| Refused::Unreadable)?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(Refused::FileTooLarge);
    }
    let mut bytes = Vec::new();
    // One byte past the limit, so a file that grew since it was measured is still seen to be over.
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Refused::Unreadable)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Refused::FileTooLarge);
    }
    Ok(bytes)
}

/// The kind of picture `bytes` start as, by the bytes themselves: the name of a file says nothing
/// about what is in it.
fn format_of(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => Some(ImageFormat::Png),
        [0xff, 0xd8, 0xff, ..] => Some(ImageFormat::Jpeg),
        _ => None,
    }
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECLARED_EDGE);
    limits.max_image_height = Some(MAX_DECLARED_EDGE);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    limits
}

/// Whether a JPEG holds the end-of-image marker `FF D9` of its OWN picture.
///
/// The header is a run of segments, each with a declared length, and a segment can hold anything,
/// including a whole small JPEG (a camera's thumbnail), whose `FF D9` would otherwise be taken for the
/// end of the picture. So the segments are stepped over by their lengths until the start of the
/// compressed data (`FF DA`), and the end marker is looked for only after it. Inside compressed data
/// a `FF` is always followed by `00` or a restart marker, never by `D9`, so the first `FF D9` there is
/// the end. A file whose segments do not add up is refused: it cannot be told where its data starts.
fn holds_jpeg_end(bytes: &[u8]) -> bool {
    let mut at = 2; // after FF D8
    loop {
        // A marker is FF followed by its code; fill bytes (more FF) may come before the code.
        if bytes.get(at) != Some(&0xff) {
            return false;
        }
        while bytes.get(at + 1) == Some(&0xff) {
            at += 1;
        }
        let Some(&code) = bytes.get(at + 1) else {
            return false;
        };
        match code {
            // The start of the compressed data: from here on, the first FF D9 is the end.
            0xda => {
                let Some(length) = bytes.get(at + 2..at + 4) else {
                    return false;
                };
                let header = usize::from(u16::from_be_bytes([length[0], length[1]]));
                let data = at + 2 + header;
                return bytes
                    .get(data..)
                    .is_some_and(|rest| rest.windows(2).any(|pair| pair == [0xff, 0xd9]));
            }
            // Markers with no length: a restart, a padding zero, or a start marker repeated.
            0x00 | 0x01 | 0xd0..=0xd8 => at += 2,
            // The end before any data: there is no picture.
            0xd9 => return false,
            // Every other segment declares its length, which includes the two bytes that say it.
            _ => {
                let Some(length) = bytes.get(at + 2..at + 4) else {
                    return false;
                };
                let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
                // A length below two cannot be a real one, and needs no check of its own: stepping by
                // it lands on bytes that are not a marker, which the top of this loop refuses.
                at += 2 + length;
            }
        }
    }
}

/// Decode `bytes`, having read what they announce first and refused what is too large.
pub fn decode(bytes: &[u8]) -> Result<Picture, Refused> {
    let format = format_of(bytes).ok_or(Refused::NotAPicture)?;
    // A PNG that is cut short is refused by its decoder, which is measured, not assumed. A JPEG is
    // not: its decoder hands back what it has with the rest left grey, so a picture cut at a tenth of
    // its length would be "accepted" with most of its pixels wrong. A complete JPEG holds its end
    // marker, which a cut one does not, so the file is refused when it is missing. Whatever follows
    // the marker (a thumbnail, a signature) does not matter: the picture itself is whole.
    if format == ImageFormat::Jpeg && !holds_jpeg_end(bytes) {
        return Err(Refused::Damaged);
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits());
    // The header only: sides and nothing else. No pixel and no buffer for one yet.
    let (width, height) = {
        let decoder = ImageReader::with_format(Cursor::new(bytes), format)
            .into_decoder()
            .map_err(|_| Refused::Damaged)?;
        decoder.dimensions()
    };
    let announced = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|p| p.checked_mul(4));
    if width == 0
        || height == 0
        || width > MAX_DECLARED_EDGE
        || height > MAX_DECLARED_EDGE
        || announced.is_none_or(|bytes| bytes > MAX_DECODED_BYTES)
    {
        return Err(Refused::PictureTooLarge);
    }
    let image = reader.decode().map_err(|error| match error {
        image::ImageError::Limits(_) => Refused::PictureTooLarge,
        _ => Refused::Damaged,
    })?;
    let rgba = image.into_rgba8();
    avatar::square(&Decoded {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
    .ok_or(Refused::Damaged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    /// A PNG of `width` by `height`, each pixel coloured by `paint`.
    fn png(width: u32, height: u32, paint: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_fn(width, height, |x, y| Rgba(paint(x, y)));
        let mut out = Vec::new();
        buffer
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    /// A JPEG of `width` by `height`, one flat colour.
    fn jpeg(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let buffer: ImageBuffer<image::Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(width, height, |_, _| image::Rgb(rgb));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode_image(&buffer)
            .unwrap();
        out
    }

    #[test]
    fn a_png_becomes_a_profile_picture_of_its_centre_no_larger_than_the_limit() {
        let wide = png(300, 100, |x, _| {
            if x < 100 {
                [255, 0, 0, 255]
            } else if x < 200 {
                [0, 255, 0, 255]
            } else {
                [0, 0, 255, 255]
            }
        });
        let picture = decode(&wide).unwrap();
        assert_eq!((picture.width(), picture.height()), (100, 100));
        assert!(
            picture.rgba().chunks(4).all(|p| p == [0, 255, 0, 255]),
            "not the centre of the picture"
        );

        let large = png(2000, 1500, |_, _| [10, 20, 30, 255]);
        let picture = decode(&large).unwrap();
        assert_eq!((picture.width(), picture.height()), (128, 128));
    }

    #[test]
    fn a_jpeg_becomes_a_profile_picture_with_its_colour_nearly_kept() {
        let picture = decode(&jpeg(400, 300, [200, 40, 40])).unwrap();
        assert_eq!((picture.width(), picture.height()), (128, 128));
        let first = &picture.rgba()[..4];
        // JPEG is lossy: the colour is close, and the picture is opaque.
        assert!(
            first[0] > 180 && first[1] < 70 && first[2] < 70,
            "colour was {first:?}"
        );
        assert_eq!(first[3], 255);
    }

    #[test]
    fn the_kind_of_file_is_told_by_its_bytes_and_not_by_what_it_is_called() {
        // A PNG, whatever the file is named, is read as one; a text file named .png is not a picture.
        assert!(decode(&png(8, 8, |_, _| [1, 2, 3, 255])).is_ok());
        assert_eq!(
            decode(b"this is a text file, not a picture").unwrap_err(),
            Refused::NotAPicture
        );
        assert_eq!(decode(b"").unwrap_err(), Refused::NotAPicture);
        assert_eq!(decode(&[0xff, 0xd8]).unwrap_err(), Refused::NotAPicture);
        // Another picture format that a decoder might read is not offered here.
        assert_eq!(
            decode(b"GIF89a\x01\x00\x01\x00").unwrap_err(),
            Refused::NotAPicture
        );
        assert_eq!(
            decode(b"BM\x00\x00\x00\x00").unwrap_err(),
            Refused::NotAPicture
        );
    }

    #[test]
    fn a_file_that_starts_as_a_picture_but_is_cut_short_or_damaged_is_refused_not_trusted() {
        let whole = png(64, 64, |x, y| [x as u8, y as u8, 7, 255]);
        // Cut anywhere before the pixels are complete: refused.
        for keep in [8usize, 20, 33, whole.len() / 2, whole.len() - 30] {
            assert!(
                decode(&whole[..keep]).is_err(),
                "a PNG cut to {keep} bytes was accepted"
            );
        }
        // Cut after every pixel, losing only the closing marker: the picture that results is the whole
        // picture, so it is not damaged, and what comes out is exactly what the full file gives.
        let complete = decode(&whole).unwrap();
        if let Ok(partial) = decode(&whole[..whole.len() - 4]) {
            assert_eq!(
                partial, complete,
                "a file that lost only its end gave a different picture"
            );
        }
        let whole = jpeg(64, 64, [9, 9, 9]);
        assert!(decode(&whole[..whole.len() / 2]).is_err());
        // Right start, garbage after it.
        let mut garbage = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        garbage.extend(std::iter::repeat_n(0xAB, 200));
        assert_eq!(decode(&garbage).unwrap_err(), Refused::Damaged);
    }

    /// A PNG that announces `width` by `height` and holds almost nothing: the header (signature and
    /// the IHDR chunk) is real and correctly checksummed, so a decoder believes it, and the pixel data
    /// is a single empty block, so the file is a few dozen bytes.
    fn png_announcing(width: u32, height: u32) -> Vec<u8> {
        fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
            let mut crc = crc32(kind, 0xFFFF_FFFF);
            crc = crc32(data, crc);
            let mut out = (data.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            out.extend_from_slice(&(crc ^ 0xFFFF_FFFF).to_be_bytes());
            out
        }
        fn crc32(bytes: &[u8], mut crc: u32) -> u32 {
            for byte in bytes {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xEDB8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            crc
        }
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 bits, RGBA, no interlace
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        out.extend(chunk(b"IHDR", &ihdr));
        out.extend(chunk(
            b"IDAT",
            &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        ));
        out.extend(chunk(b"IEND", &[]));
        out
    }

    #[test]
    fn a_small_file_that_announces_an_enormous_picture_is_refused_before_anything_is_decoded() {
        for (w, h) in [
            (60_000, 60_000),
            (MAX_DECLARED_EDGE + 1, 10),
            (10, MAX_DECLARED_EDGE + 1),
        ] {
            let bomb = png_announcing(w, h);
            assert!(
                bomb.len() < 200,
                "the file is not small ({} bytes), so this proves nothing",
                bomb.len()
            );
            assert_eq!(
                decode(&bomb).unwrap_err(),
                Refused::PictureTooLarge,
                "{w}x{h}"
            );
        }
        // Sides so large that the decoder itself will not read the header: still refused, and still
        // before any pixel, only under another word.
        let absurd = png_announcing(u32::MAX, u32::MAX);
        assert!(absurd.len() < 200);
        assert!(matches!(
            decode(&absurd),
            Err(Refused::PictureTooLarge | Refused::Damaged)
        ));
        // Inside the sides but past the memory a decoded picture may take: 12000 x 12000 x 4 bytes.
        let big = png_announcing(MAX_DECLARED_EDGE, MAX_DECLARED_EDGE);
        assert_eq!(decode(&big).unwrap_err(), Refused::PictureTooLarge);
        // A picture of nothing.
        assert!(decode(&png_announcing(0, 10)).is_err());
    }

    /// A JPEG with real detail, so that a missing part would show as a different picture.
    fn detailed_jpeg() -> Vec<u8> {
        let buffer: ImageBuffer<image::Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(200, 200, |x, y| {
                image::Rgb([
                    (x * 5 % 256) as u8,
                    (y * 5 % 256) as u8,
                    ((x + y) * 3 % 256) as u8,
                ])
            });
        let mut whole = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut whole, 90)
            .encode_image(&buffer)
            .unwrap();
        whole
    }

    #[test]
    fn a_jpeg_cut_short_is_refused_because_the_decoder_would_hand_back_a_picture_with_a_part_missing()
     {
        // Measured, not assumed: this decoder does not fail on a cut JPEG, it returns what it has with
        // the rest left grey, so a picture cut at a tenth of its length is "accepted" with most of its
        // pixels wrong. A picture someone is going to be shown as theirs must be the whole picture.
        let whole = detailed_jpeg();
        assert!(decode(&whole).is_ok());
        for tenth in 1..=9usize {
            let keep = whole.len() * tenth / 10;
            assert_eq!(
                decode(&whole[..keep]).unwrap_err(),
                Refused::Damaged,
                "a JPEG cut at {tenth}/10 of its length was accepted"
            );
        }
        // Even cut one byte short of the end.
        assert_eq!(
            decode(&whole[..whole.len() - 1]).unwrap_err(),
            Refused::Damaged
        );
    }

    #[test]
    fn a_jpeg_with_something_after_its_end_is_still_the_picture_it_was() {
        // Cameras and editors append a thumbnail or a signature after the end marker. What counts is
        // that the picture itself is complete, not that nothing follows it.
        let mut whole = detailed_jpeg();
        let alone = decode(&whole).unwrap();
        whole.extend_from_slice(b"trailing data some tool added");
        assert_eq!(decode(&whole).unwrap(), alone);
    }

    /// A JPEG whose header carries a complete little JPEG of its own, as camera thumbnails are: the
    /// bytes `FF D9` then appear early, inside the header, though the picture itself is far from over.
    fn jpeg_with_a_thumbnail() -> Vec<u8> {
        let thumbnail = {
            let small: ImageBuffer<image::Rgb<u8>, Vec<u8>> =
                ImageBuffer::from_fn(16, 16, |x, y| {
                    image::Rgb([(x * 15) as u8, (y * 15) as u8, 99])
                });
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
                .encode_image(&small)
                .unwrap();
            out
        };
        let main = detailed_jpeg();
        // The thumbnail goes in an APP1 segment right after the start marker: FF D8, FF E1, length, data.
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&thumbnail);
        let length = (payload.len() + 2) as u16;
        let mut out = vec![0xff, 0xd8, 0xff, 0xe1];
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&main[2..]);
        out
    }

    #[test]
    fn a_thumbnail_inside_the_header_does_not_make_a_cut_picture_look_complete() {
        let whole = jpeg_with_a_thumbnail();
        assert!(
            decode(&whole).is_ok(),
            "the picture with its thumbnail should be readable"
        );
        // The thumbnail's own end marker is among the first bytes; a cut after it, but well before the
        // picture ends, is still a cut picture.
        let thumbnail_end = whole.windows(2).position(|p| p == [0xff, 0xd9]).unwrap();
        assert!(
            thumbnail_end < whole.len() / 4,
            "the thumbnail's marker is not early, so this proves nothing"
        );
        for tenth in 3..=9usize {
            let keep = whole.len() * tenth / 10;
            assert!(keep > thumbnail_end);
            assert_eq!(
                decode(&whole[..keep]).unwrap_err(),
                Refused::Damaged,
                "a JPEG with a thumbnail, cut at {tenth}/10, was accepted"
            );
        }
    }

    #[test]
    fn hostile_jpeg_headers_are_refused_and_can_neither_loop_nor_panic() {
        // Every one of these starts as a JPEG and is wrong in the way the stepping over segments could
        // be fooled by: a length that leaps past the end, a length too short to move on, markers that
        // never lead to data, and runs of padding. Each must come back, and come back refused.
        let hostile: Vec<Vec<u8>> = vec![
            vec![0xff, 0xd8, 0xff],                                 // cut in a marker
            vec![0xff, 0xd8, 0xff, 0xe0],                           // a marker with no length
            vec![0xff, 0xd8, 0xff, 0xe0, 0xff],                     // half a length
            vec![0xff, 0xd8, 0xff, 0xe0, 0xff, 0xff, 0x00], // a length that leaps past the end
            vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x00, 0x00], // a length of zero: cannot move on
            vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x01, 0x00], // a length of one: cannot move on
            vec![0xff, 0xd8, 0xff, 0xd9],                   // the end before any data
            vec![0xff, 0xd8, 0xff, 0xda],                   // the data starts and nothing follows
            vec![0xff, 0xd8, 0xff, 0xda, 0x00, 0x02],       // data of nothing, no end
            vec![0xff, 0xd8, 0xff, 0xda, 0xff, 0xff],       // a length that leaps past the end
            vec![0xff, 0xd8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], // padding that never ends in a code
            [vec![0xff, 0xd8], vec![0xff; 5000]].concat(),        // a very long run of padding
            [vec![0xff, 0xd8], [0xff, 0x01].repeat(5000)].concat(), // many markers without a length
        ];
        for bytes in hostile {
            assert!(
                !holds_jpeg_end(&bytes),
                "{} bytes of hostile header were taken for a whole picture",
                bytes.len()
            );
            assert!(decode(&bytes).is_err());
        }
    }

    #[test]
    fn a_segment_that_declares_an_impossible_length_is_refused_even_when_a_whole_picture_follows_it()
     {
        // A segment's length counts the two bytes that say it, so zero and one are impossible. If they
        // were stepped over, the real picture behind them would be found and taken: the file is wrong,
        // and what makes it wrong is the length, not what follows.
        let whole = detailed_jpeg();
        for impossible in [[0x00u8, 0x00], [0x00, 0x01]] {
            let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0];
            bytes.extend_from_slice(&impossible);
            // Enough padding that stepping by two or three bytes lands back on the real picture.
            bytes.extend_from_slice(&whole[2..]);
            assert!(
                !holds_jpeg_end(&bytes),
                "a segment of length {impossible:?} was stepped over"
            );
        }
    }

    fn file_with(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("me.png");
        std::fs::write(&path, bytes).unwrap();
        (folder, path)
    }

    #[test]
    fn a_chosen_file_is_read_and_what_is_not_a_plain_file_is_refused() {
        let (folder, path) = file_with(&png(40, 40, |_, _| [5, 6, 7, 255]));
        assert!(read(&path).is_ok());
        // Gone.
        assert_eq!(
            read(&folder.path().join("missing.png")).unwrap_err(),
            Refused::Unreadable
        );
        // A folder.
        assert_eq!(read(folder.path()).unwrap_err(), Refused::Unreadable);
        // A link to a good picture: not followed, so what was chosen is what is read.
        let link = folder.path().join("link.png");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(read(&link).unwrap_err(), Refused::Unreadable);
    }

    #[test]
    fn a_file_larger_than_the_limit_is_refused_without_being_read_into_memory() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("huge.png");
        let file = std::fs::File::create(&path).unwrap();
        // A sparse file: it is as long as the limit says is too long, and takes no disk.
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert_eq!(read(&path).unwrap_err(), Refused::FileTooLarge);
    }

    /// A file with no name on the disk, as a picker hands one over: open, with nothing to follow.
    fn pathless(bytes: &[u8]) -> std::fs::File {
        use std::io::{Seek, Write};
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.rewind().unwrap();
        file
    }

    #[test]
    fn a_picture_in_an_open_file_with_no_path_is_read_as_one_by_path_is() {
        let bytes = png(40, 40, |_, _| [5, 6, 7, 255]);
        let (_folder, path) = file_with(&bytes);
        let by_path = read(&path).expect("the path version refused a good picture");
        let by_file =
            read_file(pathless(&bytes)).expect("an open file with a good picture was refused");
        assert_eq!(
            by_file, by_path,
            "the two ways of reading the same picture made different pictures"
        );
    }

    #[test]
    fn an_open_file_over_the_limit_or_that_is_not_a_picture_is_refused_like_a_path_is() {
        // Over the limit: a sparse file as long as the limit says is too long, which takes no disk.
        let big = tempfile::tempfile().unwrap();
        big.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert_eq!(read_file(big).unwrap_err(), Refused::FileTooLarge);
        // Exactly at the limit is not over it: it is read, and is then refused for what it holds (zeros), not for size.
        let edge = tempfile::tempfile().unwrap();
        edge.set_len(MAX_FILE_BYTES).unwrap();
        assert_eq!(read_file(edge).unwrap_err(), Refused::NotAPicture);
        // Text, and nothing at all.
        assert_eq!(
            read_file(pathless(b"not a picture at all")).unwrap_err(),
            Refused::NotAPicture
        );
        assert_eq!(read_file(pathless(b"")).unwrap_err(), Refused::NotAPicture);
    }

    #[test]
    fn an_open_file_that_states_no_size_but_holds_more_than_the_limit_is_refused_by_the_read_itself()
     {
        use std::io::Write;
        // A pipe says its length is nothing, as a provider's stream may, so the size check cannot be what refuses it:
        // only the read, which stops one byte past the limit, can. A writer on another thread feeds it past the limit.
        let (reader, writer) = rustix::pipe::pipe().unwrap();
        let feeder = std::thread::spawn(move || {
            let mut writer = std::fs::File::from(writer);
            let chunk = vec![0_u8; 1024 * 1024];
            // Ignore the end of the pipe closing: the reader stops reading once it has seen enough.
            for _ in 0..(MAX_FILE_BYTES / chunk.len() as u64 + 8) {
                if writer.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
        let file = std::fs::File::from(reader);
        assert_eq!(
            file.metadata().unwrap().len(),
            0,
            "the pipe was expected to state no size"
        );
        assert_eq!(read_file(file).unwrap_err(), Refused::FileTooLarge);
        feeder.join().unwrap();
    }

    #[test]
    fn every_refusal_can_be_told_to_a_person_in_words() {
        for refused in [
            Refused::Unreadable,
            Refused::FileTooLarge,
            Refused::NotAPicture,
            Refused::PictureTooLarge,
            Refused::Damaged,
        ] {
            assert!(!refused.words().trim().is_empty());
        }
    }
}
