//! What the camera sees, shown in the circle, so that the person can point it at a code.
//!
//! The camera gives frames to be read for a code and gives nothing to the screen, so without this the circle could
//! only say "looking" while the person aimed blind. The frame is already here, as a plane of brightness, one byte to
//! a pixel: it is cut to the square at its centre (what a circle shows), made as small as the disc is, and given round
//! edges, and is then an ordinary picture like the one of a code. Grey, since a brightness plane has no colour, which is
//! also all that is needed to line a code up.
//!
//! Only the centre is shown, and a code read lies anywhere in the whole frame; the centre is what the person is aiming
//! with, and a code that is wholly inside the circle is read. This costs one pass over a few tens of thousands of
//! pixels, done on the watcher's own thread and never on the interface's.

use crate::{qr::Raster, scanner::Colour};

/// The smallest picture that is made: below this nothing can be told apart, and a disc this small does not exist.
const LEAST_SIDE: u32 = 16;
/// The largest: a camera preview needs no more than this to be lined up, and a larger one only costs more to hand over.
const MOST_SIDE: u32 = 512;

/// The round picture of the centre of a frame, `side` pixels across, turned `turns` quarter turns clockwise to stand
/// upright, or `None` when the frame cannot be shown (it has no pixels, or fewer bytes than it says it has).
///
/// `luma` is the frame's brightness plane, `width` bytes to a row and `height` rows. `side` is asked for and held between
/// [`LEAST_SIDE`] and [`MOST_SIDE`]. A pixel outside the circle is see-through, as in the picture of a code, so that the
/// disc's own edge shows and not the corners of a square.
///
/// A camera gives its frames as its sensor sees them, which on a phone held upright is on its side: `turns` is how many
/// quarter turns clockwise the frame must be turned for what is on it to be the right way up for the person holding
/// the phone (the sensor's own orientation, taken with the screen's; see [`turns_to_upright`]). Any number is taken
/// modulo four. A code is read from the frame as it is, which does not depend on it; only the person's eyes do.
pub fn disc_raster(
    luma: &[u8],
    width: usize,
    height: usize,
    side: u32,
    turns: u32,
) -> Option<Raster> {
    disc_raster_coloured(luma, width, height, side, turns, None)
}

/// [`disc_raster`], in colour when `colour` is the two colour planes of the frame's own centre square (half its side,
/// see [`Colour`]) and in grey when it is not, so that a camera that gives no colour, or a plane that does not fit,
/// shows the picture as it always did.
///
/// A pixel of the picture is the mean of the pixels of the frame it covers (at most four across and four down of
/// them, spread evenly over what it covers), and not the one pixel under its middle: a frame several times larger than
/// the picture, taken at every few pixels, shows a fine pattern as noise and a thin line as nothing, which is no
/// help to anyone lining up a code.
pub fn disc_raster_coloured(
    luma: &[u8],
    width: usize,
    height: usize,
    side: u32,
    turns: u32,
    colour: Option<&Colour>,
) -> Option<Raster> {
    let needed = width.checked_mul(height)?;
    if width == 0 || height == 0 || luma.len() < needed {
        return None;
    }
    let side = side.clamp(LEAST_SIDE, MOST_SIDE) as usize;
    // The square at the centre of the frame: as wide as the frame is high, or as high as it is wide.
    let edge = width.min(height);
    let (left, top) = ((width - edge) / 2, (height - edge) / 2);
    let colour = colour.filter(|colour| colour.fits(edge));
    let radius = side as f32 / 2.;
    let mut rgba = vec![0_u8; side * side * 4];
    for y in 0..side {
        for x in 0..side {
            // The middle of this pixel, to its distance from the middle of the picture.
            let (dx, dy) = (x as f32 + 0.5 - radius, y as f32 + 0.5 - radius);
            if dx * dx + dy * dy > radius * radius {
                continue;
            }
            // The block of the square under this pixel, before it was turned: at least one pixel across.
            let (u0, v0) = ((x * edge) / side, (y * edge) / side);
            let u1 = (((x + 1) * edge) / side).max(u0 + 1).min(edge);
            let v1 = (((y + 1) * edge) / side).max(v0 + 1).min(edge);
            let (columns, rows) = match turns % 4 {
                0 => ((u0, u1), (v0, v1)),
                1 => ((v0, v1), (edge - u1, edge - u0)),
                2 => ((edge - u1, edge - u0), (edge - v1, edge - v0)),
                _ => ((edge - v1, edge - v0), (u0, u1)),
            };
            let (step_c, step_r) = (
                (columns.1 - columns.0).div_ceil(4).max(1),
                (rows.1 - rows.0).div_ceil(4).max(1),
            );
            let (mut sum, mut count) = (0_u32, 0_u32);
            for row in (rows.0..rows.1).step_by(step_r) {
                let line = (top + row) * width + left;
                for column in (columns.0..columns.1).step_by(step_c) {
                    sum += u32::from(luma[line + column]);
                    count += 1;
                }
            }
            let value = (sum / count.max(1)) as u8;
            let at = (y * side + x) * 4;
            let pixel = match colour {
                // The colour of the middle of the block: the planes have a quarter of the pixels.
                Some(colour) => {
                    colour.pixel(value, (columns.0 + columns.1) / 2, (rows.0 + rows.1) / 2)
                }
                None => [value, value, value],
            };
            rgba[at..at + 4].copy_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]);
        }
    }
    Some(Raster {
        side: side as u32,
        rgba,
    })
}

/// How many quarter turns clockwise a frame must be turned to stand upright, from what the platform knows: the angle
/// the camera's sensor is mounted at (`sensor`, in degrees: 0, 90, 180 or 270, 90 on nearly every phone's back camera)
/// and the angle the screen is turned from its natural position (`display`, in degrees, the same four values).
///
/// For a back camera the frame must be turned by the sensor's angle minus the screen's, which is what Android's own
/// documentation gives. An angle that is not a multiple of ninety is taken to the nearest, so that a platform that
/// reports something odd gives a picture the right way up or a quarter out, never a crash or nothing.
pub fn turns_to_upright(sensor: i32, display: i32) -> u32 {
    let quarter = |degrees: i32| (f64::from(degrees) / 90.).round() as i32;
    (quarter(sensor) - quarter(display)).rem_euclid(4) as u32
}

/// Where a point of the frame falls in the picture [`disc_raster`] makes of it, as a position in pixels of that picture,
/// or `None` when it is outside the square at the frame's centre (which the picture does not show).
///
/// This is the inverse of how the picture is made: the same square, the same turns, the same size, so that a mark drawn
/// at the position it returns lies on the very thing it marks. A code is looked for in the whole frame and so may be found
/// outside the circle's square; such a point is not drawn at the edge, where it would lie, because it would tell the
/// person it is there when it is not in view.
pub fn locate(
    point: (usize, usize),
    width: usize,
    height: usize,
    side: u32,
    turns: u32,
) -> Option<(f32, f32)> {
    if width == 0 || height == 0 {
        return None;
    }
    let side = side.clamp(LEAST_SIDE, MOST_SIDE) as usize;
    let edge = width.min(height);
    let (left, top) = ((width - edge) / 2, (height - edge) / 2);
    let (column, row) = (point.0.checked_sub(left)?, point.1.checked_sub(top)?);
    if column >= edge || row >= edge {
        return None;
    }
    // The picture turned the square by `turns`; here the point is turned the same way, from where it is in the square to
    // where it is in the picture before it was scaled. Each case undoes the matching one in `disc_raster`.
    let (u, v) = match turns % 4 {
        0 => (column, row),
        1 => (edge - 1 - row, column),
        2 => (edge - 1 - column, edge - 1 - row),
        _ => (row, edge - 1 - column),
    };
    // To the middle of the picture's pixel that shows it.
    let scale = side as f32 / edge as f32;
    Some(((u as f32 + 0.5) * scale, (v as f32 + 0.5) * scale))
}

/// What the circle shows of the camera: the round picture, and where each code that was seen lies on it.
#[derive(Debug, PartialEq)]
pub struct Shown {
    pub picture: Raster,
    /// Each code seen, as the four corners in pixels of `picture`; a code with any corner outside the square the picture
    /// shows is left out, since a mark drawn with a corner missing would be a lie about where the code is.
    pub codes: Vec<[(f32, f32); 4]>,
    /// The serial of the frame this was made from, to be asked for the next one after.
    pub serial: u64,
}

/// Make [`Shown`] from what the scanner has to show: a picture `side` pixels across, turned `turns` quarter turns, with the
/// corners of the codes seen placed on it. `None` when the frame cannot be shown.
pub fn show(view: &crate::scanner::View, side: u32, turns: u32) -> Option<Shown> {
    let frame = &view.frame;
    let picture = disc_raster_coloured(
        &frame.luma,
        frame.width,
        frame.height,
        side,
        turns,
        frame.colour.as_ref(),
    )?;
    let codes = view
        .corners
        .iter()
        .filter_map(|code| {
            let mut placed = [(0., 0.); 4];
            for (slot, &point) in placed.iter_mut().zip(code.iter()) {
                *slot = locate(point, frame.width, frame.height, picture.side, turns)?;
            }
            Some(placed)
        })
        .collect();
    Some(Shown {
        picture,
        codes,
        serial: view.serial,
    })
}

/// The top left of a dot of `dot` logical units, centred on a corner at (`x`, `y`) pixels of a picture that is shown
/// `shown` logical units across, which is `side` pixels across: a corner is a point and the dot is a box, so the box is
/// moved back by half of itself to be centred on the point. A picture with no side has nothing to place, and gives the
/// dot at the origin, never a division by nothing.
pub fn dot_origin(point: (f32, f32), side: u32, shown: f32, dot: f32) -> (f32, f32) {
    if side == 0 || !shown.is_finite() {
        return (0., 0.);
    }
    let unit = shown / side as f32;
    (point.0 * unit - dot / 2., point.1 * unit - dot / 2.)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(raster: &Raster, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * raster.side + x) * 4) as usize;
        raster.rgba[index..index + 4].try_into().unwrap()
    }

    /// A frame 40 wide and 20 high whose brightness is its column, so that what was taken from where can be told.
    fn ramp() -> Vec<u8> {
        (0..20)
            .flat_map(|_| (0..40).map(|column| column as u8 * 6))
            .collect()
    }

    #[test]
    fn the_picture_is_square_of_the_side_asked_for_and_four_bytes_to_a_pixel() {
        let raster = disc_raster(&ramp(), 40, 20, 32, 0).unwrap();
        assert_eq!(raster.side, 32);
        assert_eq!(raster.rgba.len(), 32 * 32 * 4);
    }

    #[test]
    fn it_shows_the_centre_of_the_frame_and_not_its_sides() {
        // 40 wide, 20 high: the centre square is columns 10..30. Its left edge is brightness 60, its right near 174.
        let raster = disc_raster(&ramp(), 40, 20, 20, 0).unwrap();
        let middle_row = raster.side / 2;
        let left = at(&raster, 2, middle_row)[0];
        let right = at(&raster, raster.side - 3, middle_row)[0];
        assert!(
            (60..=90).contains(&left),
            "the left of the circle shows column brightness {left}, not the centre's"
        );
        assert!(
            (150..=180).contains(&right),
            "the right of the circle shows {right}, not the centre's"
        );
        // A column outside the centre (brightness below 60 or above 174) is never shown.
        for y in 0..raster.side {
            for x in 0..raster.side {
                let pixel = at(&raster, x, y);
                if pixel[3] != 0 {
                    assert!(
                        (60..=180).contains(&pixel[0]),
                        "a pixel outside the centre square was shown: {}",
                        pixel[0]
                    );
                }
            }
        }
    }

    #[test]
    fn the_corners_are_see_through_and_the_middle_is_not() {
        let raster = disc_raster(&vec![200; 64 * 64], 64, 64, 32, 0).unwrap();
        for (x, y) in [(0, 0), (31, 0), (0, 31), (31, 31)] {
            assert_eq!(
                at(&raster, x, y),
                [0, 0, 0, 0],
                "the corner ({x}, {y}) is not see-through"
            );
        }
        assert_eq!(at(&raster, 16, 16), [200, 200, 200, 255]);
        // Grey: the three colours are the one brightness, and nothing in the circle is half see-through.
        for y in 0..32 {
            for x in 0..32 {
                let pixel = at(&raster, x, y);
                assert!(
                    pixel[3] == 0
                        || (pixel[3] == 255 && pixel[0] == pixel[1] && pixel[1] == pixel[2])
                );
            }
        }
    }

    #[test]
    fn a_frame_taller_than_it_is_wide_is_cut_to_its_centre_too() {
        // 20 wide, 40 high, brightness is the row: the centre square is rows 10..30.
        let tall: Vec<u8> = (0..40)
            .flat_map(|row| (0..20).map(move |_| row as u8 * 6))
            .collect();
        let raster = disc_raster(&tall, 20, 40, 20, 0).unwrap();
        let top = at(&raster, raster.side / 2, 2)[0];
        let bottom = at(&raster, raster.side / 2, raster.side - 3)[0];
        assert!(
            top >= 60 && bottom <= 180 && top < bottom,
            "rows {top}..{bottom} are not the centre's"
        );
    }

    #[test]
    fn a_frame_that_cannot_be_shown_is_none_and_the_side_is_held_between_its_limits() {
        assert!(disc_raster(&[], 0, 0, 32, 0).is_none());
        assert!(
            disc_raster(&[1, 2, 3], 4, 4, 32, 0).is_none(),
            "fewer bytes than the frame says it has"
        );
        assert!(disc_raster(&[0; 16], 0, 16, 32, 0).is_none());
        assert!(
            disc_raster(&[0; 16], usize::MAX, 2, 32, 0).is_none(),
            "a size that overflows is refused, not wrapped"
        );
        let big = vec![9_u8; 40 * 40];
        assert_eq!(disc_raster(&big, 40, 40, 1, 0).unwrap().side, LEAST_SIDE);
        assert_eq!(
            disc_raster(&big, 40, 40, 100_000, 0).unwrap().side,
            MOST_SIDE
        );
    }

    /// A square frame of 40 whose top left quarter is bright (250) and everything else dark (10): where the bright
    /// quarter ends up says how the picture was turned.
    fn corner_frame() -> Vec<u8> {
        (0..40)
            .flat_map(|y| (0..40).map(move |x| if x < 20 && y < 20 { 250_u8 } else { 10 }))
            .collect()
    }

    /// Which quarter of the picture is bright: (right?, below?) of the picture's own middle, by the pixel a little in
    /// from each corner of the circle (a point inside the circle on its diagonal).
    fn bright_quarter(raster: &Raster) -> (bool, bool) {
        let near = raster.side / 4;
        let far = raster.side - 1 - near;
        let value = |x: u32, y: u32| at(raster, x, y)[0];
        let corners = [(near, near), (far, near), (near, far), (far, far)];
        let bright: Vec<usize> = (0..4)
            .filter(|&index| value(corners[index].0, corners[index].1) > 128)
            .collect();
        assert_eq!(
            bright.len(),
            1,
            "exactly one quarter should be bright, got {bright:?}"
        );
        (bright[0] % 2 == 1, bright[0] >= 2)
    }

    #[test]
    fn each_quarter_turn_moves_the_bright_corner_a_quarter_of_the_way_round_clockwise() {
        let frame = corner_frame();
        let quarter = |turns| bright_quarter(&disc_raster(&frame, 40, 40, 32, turns).unwrap());
        // Top left, then clockwise: top right, bottom right, bottom left.
        assert_eq!(quarter(0), (false, false));
        assert_eq!(quarter(1), (true, false));
        assert_eq!(quarter(2), (true, true));
        assert_eq!(quarter(3), (false, true));
        // A whole number of turns is a full circle, taken modulo four.
        assert_eq!(quarter(4), quarter(0));
        assert_eq!(quarter(7), quarter(3));
    }

    #[test]
    fn the_turns_that_stand_a_frame_upright_follow_the_sensor_and_the_screen() {
        // The usual phone: a back camera mounted at 90, the screen upright: turned once.
        assert_eq!(turns_to_upright(90, 0), 1);
        // The phone on its side the one way, and the other, and upside down.
        assert_eq!(turns_to_upright(90, 90), 0);
        assert_eq!(turns_to_upright(90, 270), 2);
        assert_eq!(turns_to_upright(90, 180), 3);
        // A sensor at 270 (front cameras) and one at 0.
        assert_eq!(turns_to_upright(270, 0), 3);
        assert_eq!(turns_to_upright(0, 0), 0);
        // Never negative or past three, whatever the platform says, and an odd angle is taken to the nearest quarter.
        assert_eq!(turns_to_upright(0, 270), 1);
        assert_eq!(turns_to_upright(100, 0), 1);
        assert_eq!(turns_to_upright(-90, 0), 3);
        assert_eq!(turns_to_upright(720, 90), 3);
    }

    fn view_of(
        frame_side: usize,
        corners: Vec<crate::qr::Corners>,
        serial: u64,
    ) -> crate::scanner::View {
        crate::scanner::View {
            frame: std::sync::Arc::new(crate::scanner::Frame {
                luma: vec![100; frame_side * frame_side],
                width: frame_side,
                height: frame_side,
                colour: None,
            }),
            corners,
            serial,
        }
    }

    #[test]
    fn what_is_shown_has_the_picture_the_serial_and_only_the_codes_wholly_in_the_circles_square() {
        // A frame 40 wide and 20 high: the square shown is columns 10..30. One code inside it, one wholly outside, and one
        // with a corner outside: only the first is placed.
        let inside = [(12, 3), (27, 3), (27, 17), (12, 17)];
        let outside = [(0, 3), (5, 3), (5, 17), (0, 17)];
        let partly = [(12, 3), (35, 3), (35, 17), (12, 17)];
        let view = crate::scanner::View {
            frame: std::sync::Arc::new(crate::scanner::Frame {
                luma: vec![100; 40 * 20],
                width: 40,
                height: 20,
                colour: None,
            }),
            corners: vec![inside, outside, partly],
            serial: 7,
        };
        let shown = show(&view, 32, 0).expect("a good frame was not shown");
        assert_eq!(shown.serial, 7);
        assert_eq!(shown.picture.side, 32);
        assert_eq!(
            shown.codes.len(),
            1,
            "{} codes were placed, not the one wholly inside",
            shown.codes.len()
        );
        // The placed code is inside the picture, and its corners are four distinct places.
        for &(x, y) in &shown.codes[0] {
            assert!(
                (0. ..32.).contains(&x) && (0. ..32.).contains(&y),
                "a corner ({x}, {y}) is off the picture"
            );
        }
    }

    #[test]
    fn a_view_with_no_codes_has_no_marks_and_one_that_cannot_be_drawn_is_none() {
        let shown = show(&view_of(32, Vec::new(), 1), 32, 1).unwrap();
        assert!(shown.codes.is_empty());
        let broken = crate::scanner::View {
            frame: std::sync::Arc::new(crate::scanner::Frame {
                luma: vec![1, 2, 3],
                width: 4,
                height: 4,
                colour: None,
            }),
            corners: Vec::new(),
            serial: 2,
        };
        assert!(
            show(&broken, 32, 0).is_none(),
            "a frame with fewer bytes than it says was shown"
        );
    }

    #[test]
    fn the_marks_of_a_code_fall_on_what_they_mark_in_the_picture_for_every_turn() {
        // The frame is dark except a bright block where the code is. Each corner of the code is marked in the frame, and
        // after the picture is turned the mark of each corner must lie on the block, not beside it, whatever the turn:
        // this is what an aim by the marks relies on, and it fails if the marks are placed without the picture's turn.
        let frame_side = 40;
        let mut luma = vec![10_u8; frame_side * frame_side];
        for y in 6..14 {
            for x in 26..34 {
                luma[y * frame_side + x] = 250;
            }
        }
        let code = [(26, 6), (33, 6), (33, 13), (26, 13)];
        for turns in 0..4 {
            let view = crate::scanner::View {
                frame: std::sync::Arc::new(crate::scanner::Frame {
                    luma: luma.clone(),
                    width: frame_side,
                    height: frame_side,
                    colour: None,
                }),
                corners: vec![code],
                serial: 1,
            };
            let shown = show(&view, 40, turns).unwrap();
            assert_eq!(
                shown.codes.len(),
                1,
                "turn {turns}: the code was not placed"
            );
            for &(x, y) in &shown.codes[0] {
                let pixel = at(&shown.picture, x.floor() as u32, y.floor() as u32);
                assert!(
                    pixel[3] == 0 || pixel[0] > 128,
                    "turn {turns}: a corner marked at ({x}, {y}) is on {pixel:?}, not on the bright block"
                );
            }
        }
    }

    #[test]
    fn a_point_of_the_frame_lands_in_the_picture_on_the_very_pixel_that_shows_it() {
        // The strongest test there is: for every turn, mark one pixel of the frame, make the picture, find the one
        // bright pixel in it, and require that `locate` says it is there. The frame is square so that nothing is cut.
        let (frame_side, side) = (40_usize, 40_u32);
        for turns in 0..4 {
            for &(px, py) in &[(5_usize, 7_usize), (30, 12), (21, 33), (8, 36)] {
                let mut frame = vec![10_u8; frame_side * frame_side];
                frame[py * frame_side + px] = 250;
                let raster = disc_raster(&frame, frame_side, frame_side, side, turns).unwrap();
                let (x, y) = locate((px, py), frame_side, frame_side, side, turns)
                    .expect("a point inside the frame was said to be outside it");
                let bright = at(&raster, x.floor() as u32, y.floor() as u32);
                // Seen at the pixel locate points to, or, where the circle does not reach it, not seen at all.
                assert!(
                    bright[3] == 0 || bright[0] > 128,
                    "turn {turns}, frame point ({px}, {py}): locate says ({x}, {y}) but that pixel is {bright:?}"
                );
            }
        }
    }

    #[test]
    fn a_dot_is_centred_on_the_corner_it_marks_at_whatever_size_the_picture_is_shown() {
        // A picture of 40 pixels shown 20 logical units across: one logical unit is two pixels, so a corner at (10, 30) is
        // at (5, 15) and a dot of 6 is moved back by 3 to be centred on it.
        assert_eq!(dot_origin((10., 30.), 40, 20., 6.), (2., 12.));
        // Shown at its own size: pixels are units.
        assert_eq!(dot_origin((10., 30.), 40, 40., 6.), (7., 27.));
        // Shown larger than it is: a corner moves out with the picture.
        assert_eq!(dot_origin((10., 30.), 40, 80., 6.), (17., 57.));
        // The middle of the picture is the middle of what is shown, however it is scaled.
        let (x, y) = dot_origin((20., 20.), 40, 100., 0.);
        assert_eq!((x, y), (50., 50.));
        // Nothing to place with no side, and an unusable size gives the origin and not a NaN.
        assert_eq!(dot_origin((10., 30.), 0, 20., 6.), (0., 0.));
        assert_eq!(dot_origin((10., 30.), 40, f32::NAN, 6.), (0., 0.));
        assert_eq!(dot_origin((10., 30.), 40, f32::INFINITY, 6.), (0., 0.));
    }

    #[test]
    fn a_point_outside_the_square_at_the_centre_is_not_placed() {
        // 40 wide, 20 high: the square is columns 10..30.
        assert!(
            locate((5, 10), 40, 20, 32, 0).is_none(),
            "left of the square"
        );
        assert!(
            locate((35, 10), 40, 20, 32, 0).is_none(),
            "right of the square"
        );
        assert!(locate((20, 10), 40, 20, 32, 0).is_some());
        assert!(
            locate((10, 0), 40, 20, 32, 0).is_some(),
            "the edge of the square is inside it"
        );
        assert!(locate((29, 19), 40, 20, 32, 0).is_some());
        assert!(
            locate((30, 0), 40, 20, 32, 0).is_none(),
            "one past the square"
        );
        assert!(locate((0, 0), 0, 0, 32, 0).is_none());
    }

    fn pixel_of(raster: &Raster, x: u32, y: u32) -> [u8; 4] {
        at(raster, x, y)
    }

    #[test]
    fn a_fine_pattern_is_averaged_and_not_picked_from() {
        // Alternate columns of 0 and 254, four times finer than a pixel of the picture: taking the pixel under the middle of
        // each would show black or white by chance; the mean of what it covers is the grey in between.
        let frame: Vec<u8> = (0..64 * 64)
            .map(|index| if index % 2 == 0 { 0 } else { 254 })
            .collect();
        let raster = disc_raster(&frame, 64, 64, 16, 0).unwrap();
        for (x, y) in [(8, 8), (6, 9), (9, 6), (7, 7)] {
            let value = pixel_of(&raster, x, y)[0];
            assert!(
                (120..=134).contains(&value),
                "pixel {x},{y} was {value}, not the grey between the stripes"
            );
        }
    }

    #[test]
    fn colour_planes_that_fit_tint_the_picture_and_ones_that_do_not_are_grey() {
        let frame = vec![128_u8; 64 * 64];
        // Red difference high, blue difference neutral: red.
        let red = Colour {
            side: 32,
            u: vec![128; 32 * 32],
            v: vec![230; 32 * 32],
        };
        let raster = disc_raster_coloured(&frame, 64, 64, 32, 0, Some(&red)).unwrap();
        let [r, g, b, a] = pixel_of(&raster, 16, 16);
        assert!(
            r > 200 && g < 128 && b > 100 && b < 140 && a == 0xff,
            "{r},{g},{b}"
        );
        // Planes of another square's size are not used: grey as before.
        let wrong = Colour {
            side: 16,
            u: vec![128; 16 * 16],
            v: vec![230; 16 * 16],
        };
        let raster = disc_raster_coloured(&frame, 64, 64, 32, 0, Some(&wrong)).unwrap();
        let [r, g, b, _] = pixel_of(&raster, 16, 16);
        assert!(r == g && g == b, "{r},{g},{b}");
        // Planes shorter than they say are not read past their end.
        let short = Colour {
            side: 32,
            u: vec![128; 10],
            v: vec![128; 10],
        };
        let raster = disc_raster_coloured(&frame, 64, 64, 32, 0, Some(&short)).unwrap();
        let [r, g, b, _] = pixel_of(&raster, 16, 16);
        assert!(r == g && g == b);
    }

    #[test]
    fn the_colour_follows_the_quarter_turns_with_the_brightness() {
        // Left half of the square red, right half blue; a frame as bright everywhere.
        let frame = vec![120_u8; 64 * 64];
        let (mut u, mut v) = (vec![0_u8; 32 * 32], vec![0_u8; 32 * 32]);
        for row in 0..32 {
            for column in 0..32 {
                let (cb, cr) = if column < 16 { (110, 230) } else { (230, 110) };
                u[row * 32 + column] = cb;
                v[row * 32 + column] = cr;
            }
        }
        let colour = Colour { side: 32, u, v };
        // Upright: red on the left. Turned once clockwise: the left goes to the top.
        let upright = disc_raster_coloured(&frame, 64, 64, 32, 0, Some(&colour)).unwrap();
        let [r, _, b, _] = pixel_of(&upright, 8, 16);
        assert!(r > b, "left is red upright");
        let turned = disc_raster_coloured(&frame, 64, 64, 32, 1, Some(&colour)).unwrap();
        let [r, _, b, _] = pixel_of(&turned, 16, 8);
        assert!(r > b, "left is the top once turned clockwise");
        let [r, _, b, _] = pixel_of(&turned, 16, 24);
        assert!(b > r, "right is the bottom once turned clockwise");
    }
}
