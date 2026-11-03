//! Making a profile picture out of whatever picture a person chose.
//!
//! WHAT THIS DECIDES. Only the shape: the largest centred square of the picture, reduced to at most
//! [`peersus_core::profile::MAX_EDGE`] on a side, so that a wide photograph and a tall one both end
//! as a face-sized square and nothing is stretched. It works on plain pixels and knows no file
//! format and no window, so it is tried without either.
//!
//! WHAT IT DOES NOT DECIDE. Whether a file is a picture at all, or how large it may be before it is
//! decoded: that is the reader's, in [`crate::avatar_file`], which looks at a file's own statement of
//! its size before one pixel of it is read.

use peersus_core::profile::{MAX_EDGE, Picture};

/// A decoded picture as the pixels the reader produced: width, height and RGBA bytes.
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The profile picture for `decoded`: the largest centred square, reduced to at most `MAX_EDGE`.
/// `None` when there is nothing to make a picture of: a side of nothing, or pixels that do not
/// add up to the size claimed.
pub fn square(decoded: &Decoded) -> Option<Picture> {
    let (width, height) = (decoded.width, decoded.height);
    let claimed = u64::from(width).checked_mul(u64::from(height)).and_then(|pixels| pixels.checked_mul(4));
    if width == 0 || height == 0 || claimed != Some(decoded.rgba.len() as u64) {
        return None;
    }
    let side = width.min(height);
    let (left, top) = ((width - side) / 2, (height - side) / 2);
    let out = side.min(MAX_EDGE);

    // Each output pixel is the average of the source block it covers: a plain box filter, which for
    // a reduction does not ring or invent colours, and for no reduction (`out == side`) is a copy.
    let mut rgba = Vec::with_capacity((out * out * 4) as usize);
    for y in 0..out {
        let (y0, y1) = block(y, out, side);
        for x in 0..out {
            let (x0, x1) = block(x, out, side);
            let mut sum = [0u64; 4];
            let mut count = 0u64;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let at = (((top + sy) * width + left + sx) * 4) as usize;
                    // Colour is averaged weighted by opacity, so a see-through pixel does not drag
                    // the colour of what is next to it towards black.
                    let alpha = u64::from(decoded.rgba[at + 3]);
                    sum[0] += u64::from(decoded.rgba[at]) * alpha;
                    sum[1] += u64::from(decoded.rgba[at + 1]) * alpha;
                    sum[2] += u64::from(decoded.rgba[at + 2]) * alpha;
                    sum[3] += alpha;
                    count += 1;
                }
            }
            if let Some(weight) = std::num::NonZeroU64::new(sum[3]) {
                let weight = weight.get();
                rgba.push((sum[0] / weight) as u8);
                rgba.push((sum[1] / weight) as u8);
                rgba.push((sum[2] / weight) as u8);
                rgba.push((sum[3] / count) as u8);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Picture::new(out, out, rgba).ok()
}

/// The source rows (or columns) that output index `index` of `out` covers in a side of `side`: at
/// least one, and together they cover the whole side without overlap.
fn block(index: u32, out: u32, side: u32) -> (u32, u32) {
    let start = (u64::from(index) * u64::from(side) / u64::from(out)) as u32;
    let end = (u64::from(index + 1) * u64::from(side) / u64::from(out)) as u32;
    (start, end.max(start + 1).min(side))
}

/// A picture as it is drawn in a circle `size` pixels across: the pixels outside the circle made
/// see-through, softened at the edge by how much of each pixel is inside. The toolkit does not clip
/// a child to a rounded shape, so the shape is made in the pixels. Straight (not premultiplied)
/// RGBA, as the picture already is.
pub fn circular(picture: &Picture, size: u32) -> Vec<u8> {
    let size = size.max(1);
    let (width, height) = (picture.width(), picture.height());
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let radius = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            // Nearest source pixel: the picture is already at most MAX_EDGE, and a list of avatars
            // is drawn small, so this is the cheapest filter that does not blur.
            let sx = ((x * width) / size).min(width - 1);
            let sy = ((y * height) / size).min(height - 1);
            let at = ((sy * width + sx) * 4) as usize;
            let (dx, dy) = (x as f32 + 0.5 - radius, y as f32 + 0.5 - radius);
            let coverage = (radius - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            let source = &picture.rgba()[at..at + 4];
            out.extend_from_slice(&[source[0], source[1], source[2], (f32::from(source[3]) * coverage).round() as u8]);
        }
    }
    out
}

/// The largest circle a picture is made for. A profile picture is at most 128 pixels, so a circle past
/// this is not one anything draws, and an absurd size must not become an absurd allocation.
pub const LARGEST_CIRCLE: u32 = 512;

/// What a round avatar shows: the picture, made for the size, or the first letter of the name.
#[derive(Debug, PartialEq)]
pub enum Look {
    /// Straight RGBA, `size * size * 4` bytes, the picture cut to a circle.
    Picture(Vec<u8>),
    /// The initial, by the same rule as everywhere else a device name is shortened to one letter.
    Initial(char),
}

impl Look {
    /// The look for `picture` (if any) and `name`, in a circle `size` pixels across.
    pub fn of(picture: Option<&Picture>, name: &str, size: u32) -> Self {
        match picture {
            Some(picture) if (1..=LARGEST_CIRCLE).contains(&size) => Self::Picture(circular(picture, size)),
            _ => Self::Initial(crate::view::Sidebar::initial(name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture of one flat colour.
    fn flat(width: u32, height: u32, colour: [u8; 4]) -> Decoded {
        Decoded { width, height, rgba: colour.iter().copied().cycle().take((width * height * 4) as usize).collect() }
    }

    /// A picture whose pixel at (x, y) is coloured by `paint`.
    fn painted(width: u32, height: u32, paint: impl Fn(u32, u32) -> [u8; 4]) -> Decoded {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&paint(x, y));
            }
        }
        Decoded { width, height, rgba }
    }

    fn pixel(picture: &Picture, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * picture.width() + x) * 4) as usize;
        picture.rgba()[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn a_small_square_picture_is_kept_as_it_is() {
        let source = painted(4, 4, |x, y| [x as u8 * 10, y as u8 * 10, 7, 255]);
        let picture = square(&source).unwrap();
        assert_eq!((picture.width(), picture.height()), (4, 4));
        assert_eq!(picture.rgba(), source.rgba.as_slice(), "a picture that needs no reduction was changed");
    }

    #[test]
    fn a_wide_picture_gives_its_centre_square_and_a_tall_one_the_same() {
        // Three bands side by side: left red, middle green, right blue. Only the middle remains.
        let wide = painted(30, 10, |x, _| match x / 10 {
            0 => [255, 0, 0, 255],
            1 => [0, 255, 0, 255],
            _ => [0, 0, 255, 255],
        });
        let picture = square(&wide).unwrap();
        assert_eq!((picture.width(), picture.height()), (10, 10));
        assert!(picture.rgba().chunks(4).all(|p| p == [0, 255, 0, 255]), "the sides were not cut away");

        let tall = painted(10, 30, |_, y| match y / 10 {
            0 => [255, 0, 0, 255],
            1 => [0, 255, 0, 255],
            _ => [0, 0, 255, 255],
        });
        let picture = square(&tall).unwrap();
        assert_eq!((picture.width(), picture.height()), (10, 10));
        assert!(picture.rgba().chunks(4).all(|p| p == [0, 255, 0, 255]));
    }

    #[test]
    fn a_large_picture_is_reduced_to_the_largest_a_profile_picture_may_be_and_stays_square() {
        for (w, h) in [(1000, 1000), (4000, 3000), (3000, 4000), (129, 129), (128, 128), (127, 400)] {
            let picture = square(&flat(w, h, [10, 20, 30, 255])).unwrap();
            assert_eq!(picture.width(), picture.height(), "{w}x{h} is not square");
            assert_eq!(picture.width(), w.min(h).min(MAX_EDGE), "{w}x{h} came out at the wrong size");
            assert_eq!(picture.rgba().len(), (picture.width() * picture.height() * 4) as usize);
        }
    }

    #[test]
    fn a_reduction_keeps_the_colour_and_does_not_invent_one() {
        let picture = square(&flat(512, 512, [200, 100, 50, 255])).unwrap();
        assert!(picture.rgba().chunks(4).all(|p| p == [200, 100, 50, 255]));
        // A black and white checkerboard reduced by averaging is grey, not black or white.
        let board = painted(256, 256, |x, y| if (x + y) % 2 == 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        let picture = square(&board).unwrap();
        let middle = pixel(&picture, 64, 64);
        assert!(middle[0] > 100 && middle[0] < 160, "averaged to {middle:?}");
    }

    #[test]
    fn see_through_pixels_do_not_darken_the_colour_beside_them() {
        // The boundary between see-through black and opaque white falls INSIDE a block that is
        // averaged: a side of 255 reduced to 128 makes block 63 the columns 125 and 126, and the
        // edge at 126 is in the middle of it (an edge at 127 would fall between blocks and prove
        // nothing), so the average has to decide what that block is.
        let source = painted(255, 255, |x, _| if x < 126 { [0, 0, 0, 0] } else { [255, 255, 255, 255] });
        let picture = square(&source).unwrap();
        // Looked at across the one row, the boundary block is the only one that holds both kinds.
        let row: Vec<[u8; 4]> = (0..picture.width()).map(|x| pixel(&picture, x, 64)).collect();
        let mixed: Vec<&[u8; 4]> = row.iter().filter(|p| p[3] > 0 && p[3] < 255).collect();
        assert!(!mixed.is_empty(), "no block held both kinds, so this proves nothing: {row:?}");
        for block in mixed {
            // Half of it is white, half see-through: it is white at partial opacity, never grey.
            assert_eq!(&block[..3], &[255, 255, 255], "a see-through pixel darkened the colour: {block:?}");
        }
        assert_eq!(row[0][3], 0, "the see-through part became visible");
        assert_eq!(row[row.len() - 1], [255, 255, 255, 255], "the opaque part was darkened");
    }

    #[test]
    fn what_is_not_a_picture_gives_nothing() {
        assert!(square(&Decoded { width: 0, height: 5, rgba: Vec::new() }).is_none());
        assert!(square(&Decoded { width: 5, height: 0, rgba: Vec::new() }).is_none());
        // The pixels do not add up to the size claimed.
        assert!(square(&Decoded { width: 2, height: 2, rgba: vec![0; 15] }).is_none());
        assert!(square(&Decoded { width: 2, height: 2, rgba: vec![0; 17] }).is_none());
        // Sides whose product would overflow are not trusted either.
        assert!(square(&Decoded { width: u32::MAX, height: u32::MAX, rgba: Vec::new() }).is_none());
    }

    #[test]
    fn a_circle_is_see_through_at_the_corners_and_solid_in_the_middle_at_every_size() {
        let picture = square(&flat(64, 64, [90, 140, 30, 255])).unwrap();
        for size in [1u32, 2, 24, 44, 96, 200] {
            let rgba = circular(&picture, size);
            assert_eq!(rgba.len() as u32, size * size * 4, "size {size}");
            if size >= 8 {
                assert_eq!(rgba[3], 0, "the corner at size {size} was not cut");
                let centre = ((size / 2 * size + size / 2) * 4) as usize;
                assert_eq!(&rgba[centre..centre + 4], &[90, 140, 30, 255], "the middle at size {size}");
            }
        }
    }

    #[test]
    fn a_circle_keeps_what_the_picture_says_and_does_not_change_its_colour_at_the_edge() {
        let picture = square(&painted(16, 16, |x, _| if x < 8 { [255, 0, 0, 255] } else { [0, 0, 255, 255] })).unwrap();
        let rgba = circular(&picture, 64);
        let at = |x: u32, y: u32| &rgba[((y * 64 + x) * 4) as usize..((y * 64 + x) * 4 + 4) as usize];
        assert_eq!(at(20, 32)[..3], [255, 0, 0], "left half is not red");
        assert_eq!(at(44, 32)[..3], [0, 0, 255], "right half is not blue");
        // On the rim a pixel is softer, never a different colour: the diagonal at the radius.
        let edge = (32.0 - 32.0 / std::f32::consts::SQRT_2) as u32;
        let rim = at(edge, edge);
        assert!(rim[3] > 0 && rim[3] < 255, "the rim is not softened: {rim:?}");
    }

    #[test]
    fn a_round_avatar_shows_the_picture_when_there_is_one_and_the_initial_when_there_is_not() {
        let picture = square(&flat(64, 64, [90, 140, 30, 255])).unwrap();
        match Look::of(Some(&picture), "Alex", 44) {
            Look::Picture(rgba) => assert_eq!(rgba.len(), 44 * 44 * 4),
            other => panic!("a picture was not shown: {other:?}"),
        }
        assert_eq!(Look::of(None, "Alex", 44), Look::Initial('A'));
        // A name that starts with a lower-case or a non-Latin letter still gets its initial, upper-cased.
        assert_eq!(Look::of(None, "alex", 44), Look::Initial('A'));
        assert_eq!(Look::of(None, "élan", 44), Look::Initial('É'));
        // No name at all: a mark that is not a letter, never an empty circle.
        assert_eq!(Look::of(None, "", 44), Look::Initial('?'));
        assert_eq!(Look::of(None, "   ", 44), Look::Initial('?'));
    }

    #[test]
    fn the_pixels_are_made_for_the_size_asked_and_a_size_of_nothing_shows_the_initial() {
        let picture = square(&flat(64, 64, [1, 2, 3, 255])).unwrap();
        for size in [1u32, 8, 36, 44, 96, 200] {
            match Look::of(Some(&picture), "A", size) {
                Look::Picture(rgba) => assert_eq!(rgba.len() as u32, size * size * 4, "size {size}"),
                other => panic!("size {size}: {other:?}"),
            }
        }
        // A circle of no size has nothing to draw a picture in.
        assert_eq!(Look::of(Some(&picture), "Alex", 0), Look::Initial('A'));
        // And an absurd size is not honoured with an absurd allocation.
        assert_eq!(Look::of(Some(&picture), "Alex", 100_000), Look::Initial('A'));
    }
}