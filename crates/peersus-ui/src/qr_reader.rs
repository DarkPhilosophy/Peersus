//! A second way to read a code: by the MIDDLE of its three finder patterns, as a phone's own camera does.
//!
//! The reader of the `rqrr` crate finds a finder pattern by the four extreme points of its outer ring, so a ring drawn round has none to
//! find (measured: a full circle was found on no disc at all). This one finds a finder by the proportions 1:1:3:1:1 of a line through its
//! middle, which a round ring has as much as a square one, and then samples the code's modules from where the three middles are. It is the
//! fallback of the first reader and never replaces it: a code the first reads is read by the first.
//!
//! Nothing here knows a scale or an angle. The size of a module comes from the width of a finder (7 modules), and the way the code lies
//! from the line between the finder middles, so a code that is turned or smaller or bigger than drawn reads as well as one that is not.
//! What knows no toolkit is tested on its own, with synthetic pictures turned, scaled and blurred (see the tests).

/// A point in the pixels of the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    fn minus(self, other: Point) -> Point {
        Point { x: self.x - other.x, y: self.y - other.y }
    }
    fn length(self) -> f32 {
        self.x.hypot(self.y)
    }
}

/// A frame of brightness, one byte a pixel, `width` pixels to a row.
struct Frame<'a> {
    luma: &'a [u8],
    width: usize,
    height: usize,
}

impl Frame<'_> {
    fn at(&self, x: isize, y: isize) -> u8 {
        let x = x.clamp(0, self.width as isize - 1) as usize;
        let y = y.clamp(0, self.height as isize - 1) as usize;
        self.luma[y * self.width + x]
    }

    /// Whether the pixel is dark, against the middle of the frame's own range, so a dim or a glaring picture reads alike.
    fn dark(&self, x: isize, y: isize, threshold: u8) -> bool {
        self.at(x, y) < threshold
    }
}

/// The grey that divides dark from light in a frame: the middle of the range between its darkest and its lightest pixel (sampled), so a code
/// seen dim or washed out is still told apart. A frame of one grey has none that means anything, and then nothing is read from it.
fn threshold_of(luma: &[u8]) -> Option<u8> {
    let step = (luma.len() / 4096).max(1);
    let (mut low, mut high) = (255u8, 0u8);
    for &value in luma.iter().step_by(step) {
        low = low.min(value);
        high = high.max(value);
    }
    (high.saturating_sub(low) >= 32).then(|| ((u16::from(low) + u16::from(high)) / 2) as u8)
}

/// Run lengths of a line of dark and light, as `(is_dark, length)`.
fn runs_of(line: impl Iterator<Item = bool>) -> Vec<(bool, usize)> {
    let mut runs: Vec<(bool, usize)> = Vec::new();
    for dark in line {
        match runs.last_mut() {
            Some((was, length)) if *was == dark => *length += 1,
            _ => runs.push((dark, 1)),
        }
    }
    runs
}

/// Whether five runs, dark first, have the proportions 1:1:3:1:1 of a finder pattern, with room for blur.
fn is_finder(five: &[(bool, usize)]) -> bool {
    let total: usize = five.iter().map(|run| run.1).sum();
    if five.len() != 5 || !five[0].0 || total < 7 {
        return false;
    }
    let unit = total as f32 / 7.;
    [1., 1., 3., 1., 1.].iter().zip(five).all(|(want, run)| (run.1 as f32 - want * unit).abs() <= unit * 0.5 + 0.5)
}

/// A finder pattern found by a line through its middle: where the middle is and how wide the pattern is, in pixels.
#[derive(Clone, Copy, Debug)]
struct Seen {
    middle: Point,
    width: f32,
}

/// The finder patterns a line of the frame shows, found along `direction` (a step of whole pixels) starting at `from`.
fn along(frame: &Frame, threshold: u8, from: (isize, isize), direction: (isize, isize), length: usize) -> Vec<Seen> {
    let points = (0..length as isize).map(|t| (from.0 + direction.0 * t, from.1 + direction.1 * t));
    let runs = runs_of(points.map(|(x, y)| frame.dark(x, y, threshold)));
    // A hit's width is its length in steps. Along a diagonal a step is longer than a pixel, but the same finder crossed along its
    // diagonal is longer by the same factor: counted in steps, a finder is as wide whichever way it is crossed, so hits found along rows,
    // columns and diagonals are the same size and merge as one finder.
    let mut found = Vec::new();
    let mut at = 0usize;
    for (index, run) in runs.iter().enumerate() {
        if index + 5 <= runs.len() && is_finder(&runs[index..index + 5]) {
            let steps: usize = runs[index..index + 5].iter().map(|run| run.1).sum();
            let middle = at as f32 + steps as f32 / 2.;
            found.push(Seen {
                middle: Point { x: from.0 as f32 + direction.0 as f32 * middle, y: from.1 as f32 + direction.1 as f32 * middle },
                width: steps as f32,
            });
        }
        at += run.1;
    }
    found
}

/// Whether the four lines through `seen` (across, down and both diagonals) all show a finder centred on it: a round ring and a square one do,
/// a chance run of the right proportions in the data does not.
fn confirmed(frame: &Frame, threshold: u8, seen: Seen) -> bool {
    let reach = (seen.width * 1.2).ceil() as isize + 2;
    let centre = (seen.middle.x.round() as isize, seen.middle.y.round() as isize);
    [(1, 0), (0, 1), (1, 1), (-1, 1)].into_iter().all(|direction| {
        let from = (centre.0 - direction.0 * reach, centre.1 - direction.1 * reach);
        let hits = along(frame, threshold, from, direction, 2 * reach as usize + 1);
        hits.iter().any(|hit| hit.middle.minus(seen.middle).length() <= seen.width / 5. + 1.)
    })
}

/// Where a scan along `direction` starts each of its lines, for a frame of `width` by `height`: every pixel of the edges it enters by.
fn lines_of(direction: (isize, isize), width: usize, height: usize) -> Vec<((isize, isize), usize)> {
    let (w, h) = (width as isize, height as isize);
    match direction {
        (1, 0) => (0..h).map(|y| ((0, y), width)).collect(),
        (0, 1) => (0..w).map(|x| ((x, 0), height)).collect(),
        // Down to the right: enter by the top edge and by the left edge.
        (1, 1) => (0..w).map(|x| ((x, 0), (w - x).min(h) as usize)).chain((1..h).map(|y| ((0, y), (h - y).min(w) as usize))).collect(),
        // Down to the left: enter by the top edge and by the right edge.
        _ => (0..w).map(|x| ((x, 0), (x + 1).min(h) as usize)).chain((1..h).map(|y| ((w - 1, y), (h - y).min(w) as usize))).collect(),
    }
}

/// What scans of the frame show of finder patterns, merged where neighbouring lines agree: `(middle, width, lines that saw it)`.
///
/// A finder is seen along a line only if the line crosses it nearly square: at a turn of 38 degrees a row crosses a finder at the corner of
/// its rings and sees runs that are nothing like 1:1:3:1:1, and a column or a diagonal sees it plainly. So the frame is scanned along the
/// rows, the columns and both diagonals, and every hit goes to the same candidates; [`confirmed`] is what keeps the chance runs of data.
fn row_candidates(frame: &Frame, threshold: u8) -> Vec<(Point, f32, usize)> {
    // Every line is read. Reading one in three was tried and lost a code turned by 25 degrees that is perfectly readable: at a turn only a
    // few lines cross a finder squarely enough to show its proportions, and thinning them leaves too few to agree.
    let mut candidates: Vec<(Point, f32, usize)> = Vec::new();
    for direction in [(1isize, 0isize), (0, 1), (1, 1), (-1, 1)] {
        for (from, length) in lines_of(direction, frame.width, frame.height) {
            for seen in along(frame, threshold, from, direction, length) {
                // A hit belongs to the nearest candidate of a width that is much the same and a middle that has not moved: a hit of another
                // size beside it (a run of the data that happens to have the proportions) is a different thing, and not an average to be
                // taken, or the finder's middle is dragged off it.
                let nearest = candidates
                    .iter_mut()
                    .filter(|(point, wide, _)| {
                        (*wide - seen.width).abs() <= *wide * 0.25 && point.minus(seen.middle).length() <= *wide / 3.
                    })
                    .min_by(|a, b| a.0.minus(seen.middle).length().total_cmp(&b.0.minus(seen.middle).length()));
                match nearest {
                    Some((point, wide, count)) => {
                        // The mean of what the lines saw: a ring read on several lines has its middle where they agree.
                        let n = *count as f32;
                        point.x = (point.x * n + seen.middle.x) / (n + 1.);
                        point.y = (point.y * n + seen.middle.y) / (n + 1.);
                        *wide = (*wide * n + seen.width) / (n + 1.);
                        *count += 1;
                    }
                    None => candidates.push((seen.middle, seen.width, 1)),
                }
            }
        }
    }
    candidates
}

/// The middles of the finder patterns in the frame: found on rows, merged where neighbours agree, kept only where every direction through
/// them agrees. Never more than the strongest few, so a busy picture cannot make this slow.
pub fn finder_middles(luma: &[u8], width: usize, height: usize) -> Vec<(Point, f32)> {
    if width < 21 || height < 21 || luma.len() != width * height {
        return Vec::new();
    }
    let Some(threshold) = threshold_of(luma) else { return Vec::new() };
    let frame = Frame { luma, width, height };
    let mut strong: Vec<(Point, f32, usize)> =
        row_candidates(&frame, threshold).into_iter().filter(|(_, wide, count)| *count as f32 >= (*wide / 7.).max(2.)).collect();
    // A round ring crossed along its diagonal shows 1:1:3:1:1 a second time, smaller, at the same middle. That is the same finder and not
    // another: at one place the widest is kept, which is the ring's own width.
    strong.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut kept: Vec<(Point, f32, usize)> = Vec::new();
    for candidate in strong {
        if !kept.iter().any(|(point, wide, _)| point.minus(candidate.0).length() <= *wide / 4.) {
            kept.push(candidate);
        }
    }
    kept.sort_by_key(|candidate| std::cmp::Reverse(candidate.2));
    kept.truncate(12);
    kept.into_iter()
        .filter(|(point, wide, _)| confirmed(&frame, threshold, Seen { middle: *point, width: *wide }))
        .map(|(point, wide, _)| (point, wide))
        .collect()
}

/// The three finders of one code, named by where they are: the corner that is the right angle, and the two that lie along its sides.
/// `None` unless exactly one set of three can be the corners of one code: the same size and a right angle between them.
fn corners(found: &[(Point, f32)]) -> Option<(Point, Point, Point, f32)> {
    let mut best: Option<(f32, (Point, Point, Point, f32))> = None;
    for (i, &(a, wa)) in found.iter().enumerate() {
        for (j, &(b, wb)) in found.iter().enumerate().skip(i + 1) {
            for &(c, wc) in found.iter().skip(j + 1) {
                // The finders of one code are one size.
                let wide = (wa + wb + wc) / 3.;
                if [wa, wb, wc].iter().any(|w| (w - wide).abs() > wide * 0.3) {
                    continue;
                }
                // The right angle is at the point whose two sides are the shortest pair and equal.
                for (corner, p, q) in [(a, b, c), (b, a, c), (c, a, b)] {
                    let (u, v) = (p.minus(corner), q.minus(corner));
                    let (lu, lv) = (u.length(), v.length());
                    if lu < wide * 2. || lv < wide * 2. || (lu - lv).abs() > lu.max(lv) * 0.2 {
                        continue;
                    }
                    let cosine = (u.x * v.x + u.y * v.y) / (lu * lv);
                    // A right angle, within a few degrees of a camera looking a little askew.
                    if cosine.abs() > 0.25 {
                        continue;
                    }
                    let flaw = cosine.abs() + (lu - lv).abs() / lu.max(lv);
                    if best.as_ref().is_none_or(|(worst, _)| flaw < *worst) {
                        // Which of the two is "right" and which "down": the cross product's sign says, so a code seen in a
                        // mirror (never the case for a screen, but cheap to refuse) is not mistaken for one that is not.
                        let cross = u.x * v.y - u.y * v.x;
                        let (right, down) = if cross > 0. { (p, q) } else { (q, p) };
                        best = Some((flaw, (corner, right, down, wide)));
                    }
                }
            }
        }
    }
    best.map(|(_, set)| set)
}

/// A code found by the middles of its finders: where its four corners lie in the frame (in the order top left, top right, bottom right,
/// bottom left of the code, as it is drawn) and what it says, if it could be read.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub corners: [Point; 4],
    pub text: Option<String>,
}

/// The code in the frame, found and read from the middles of its three finders, or `None` if there is no code in it to be found.
///
/// The code's size in modules is not told, and is not taken from one measurement either: the width of a finder is read along a row, which
/// cuts a turned square short, so the size it gives is a little low or high. Between two finder middles lie `size - 7` modules, so the
/// measured size says which sizes a code can have (21 plus a multiple of 4) are near, and each of those is tried, nearest first, until one
/// decodes. A code that decodes at a size is that size: the error correction does not decode a wrong grid by chance.
pub fn find(luma: &[u8], width: usize, height: usize) -> Option<Found> {
    let found = finder_middles(luma, width, height);
    let (corner, right, down, wide) = corners(&found)?;
    // A finder's width is counted in steps along the line that found it, and the same finder is as many steps wide along a row, a column or
    // a diagonal, so the module is the finder's width over seven whichever line it came from.
    let module = wide / 7.;
    let across = (right.minus(corner).length() + down.minus(corner).length()) / 2. / module;
    let measured = across + 7.;
    let threshold = threshold_of(luma)?;
    let frame = Frame { luma, width, height };
    let mut sizes: Vec<usize> = (0..=35).map(|version| 21 + 4 * version).filter(|size| (*size as f32 - measured).abs() <= 6.).collect();
    sizes.sort_by(|a, b| (*a as f32 - measured).abs().total_cmp(&(*b as f32 - measured).abs()));
    let mut shape: Option<[Point; 4]> = None;
    for size in sizes {
        let span = size as f32 - 7.;
        let step = |from: Point, to: Point| Point { x: (to.x - from.x) / span, y: (to.y - from.y) / span };
        let (u, v) = (step(corner, right), step(corner, down));
        // `at(x, y)` is the point `x` modules right and `y` modules down from the code's top left corner, which is where the module grid is
        // counted from. A finder is seven modules wide, so its middle is 3.5 modules from the corner: that is where `corner` is.
        let at = |x: f32, y: f32| Point { x: corner.x + (x - 3.5) * u.x + (y - 3.5) * v.x, y: corner.y + (x - 3.5) * u.y + (y - 3.5) * v.y };
        let edge = size as f32;
        let corners = [at(0., 0.), at(edge, 0.), at(edge, edge), at(0., edge)];
        shape.get_or_insert(corners);
        let grid = rqrr::SimpleGrid::from_func(size, |mx, my| {
            // A finder pattern is fixed by the standard and carries no data, so the grid takes the standard's own modules there: a ring
            // drawn round has light corners where the standard has dark ones, and a decoder that checks the pattern would be fooled by
            // what is only a way of drawing it.
            let far = size as i32 - 7;
            for (ox, oy) in [(0i32, 0i32), (far, 0), (0, far)] {
                let (fx, fy) = (mx as i32 - ox, my as i32 - oy);
                if (0..7).contains(&fx) && (0..7).contains(&fy) {
                    let ring = fx == 0 || fx == 6 || fy == 0 || fy == 6;
                    let core = (2..=4).contains(&fx) && (2..=4).contains(&fy);
                    return ring || core;
                }
            }
            // The middle of the module and four points a quarter of a module from it along its own axes: a code that is turned puts the
            // middle of a module near an edge where one pixel can be either side of it, and the majority of five is not fooled.
            let (cx, cy) = (mx as f32 + 0.5, my as f32 + 0.5);
            let dark = [(0., 0.), (0.25, 0.), (-0.25, 0.), (0., 0.25), (0., -0.25)]
                .into_iter()
                .filter(|(dx, dy)| {
                    let point = at(cx + dx, cy + dy);
                    frame.dark(point.x.round() as isize, point.y.round() as isize, threshold)
                })
                .count();
            dark >= 3
        });
        if let Ok((_, text)) = rqrr::Grid::new(grid).decode() {
            return Some(Found { corners, text: Some(text) });
        }
    }
    // Found and not read: the corners of the nearest size say where it is.
    shape.map(|corners| Found { corners, text: None })
}

/// What a code found by its finders says, or `None` if there is none or it cannot be read. See [`find`].
pub fn read(luma: &[u8], width: usize, height: usize) -> Option<String> {
    find(luma, width, height)?.text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_runs_of_a_finders_proportions_are_one_and_others_are_not() {
        let finder = [(true, 4), (false, 4), (true, 12), (false, 4), (true, 4)];
        assert!(is_finder(&finder));
        // A little blur: still a finder.
        assert!(is_finder(&[(true, 4), (false, 5), (true, 11), (false, 4), (true, 5)]));
        // Not: a bar, a wrong middle, or starting on light.
        assert!(!is_finder(&[(true, 4), (false, 4), (true, 4), (false, 4), (true, 4)]));
        assert!(!is_finder(&[(false, 4), (true, 4), (false, 12), (true, 4), (false, 4)]));
        assert!(!is_finder(&finder[..4]));
    }

    #[test]
    fn a_frame_of_one_grey_or_of_the_wrong_length_reads_nothing() {
        assert!(read(&vec![128; 100 * 100], 100, 100).is_none());
        assert!(read(&[255; 99], 100, 100).is_none());
        assert!(read(&[], 0, 0).is_none());
        assert!(finder_middles(&[0; 4], 2, 2).is_empty());
    }
}
