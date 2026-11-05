//! Where the circles of a pairing request are drawn.
//!
//! They are scattered, not set in a row: the person picks by what a circle SAYS, never by where
//! it sits, so its place tells a watcher or a program nothing. The places come from a seed that
//! belongs to the request, so they stay put while the request is on screen (a circle that moved
//! under the finger on every redraw could not be pressed) and differ from one request to the next.
use rand::{RngExt, SeedableRng, rngs::StdRng};

/// A circle's centre in the area, in pixels from its top left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub x: f32,
    pub y: f32,
}

/// Places for `count` circles of `diameter` pixels in an area of `width` by `height`.
///
/// Every circle lies wholly inside the area and no two overlap, with a gap of at least `gap`.
/// The same `seed` always gives the same places. Returns fewer than `count` only when the area
/// cannot hold that many, which the caller sizes it to avoid.
pub fn spots(
    seed: u64,
    count: usize,
    diameter: f32,
    gap: f32,
    width: f32,
    height: f32,
) -> Vec<Spot> {
    let mut random = StdRng::seed_from_u64(seed);
    let radius = diameter / 2.;
    let (min_x, max_x) = (radius, width - radius);
    let (min_y, max_y) = (radius, height - radius);
    let mut placed: Vec<Spot> = Vec::with_capacity(count);
    if max_x < min_x || max_y < min_y {
        return placed;
    }
    // Rejection sampling: draw a place, keep it if it clears every circle already set down.
    // Bounded, so a crowded area ends with fewer circles instead of looping for ever.
    let apart = diameter + gap;
    for _ in 0..count.saturating_mul(400) {
        if placed.len() == count {
            break;
        }
        let spot = Spot {
            x: random.random_range(min_x..=max_x),
            y: random.random_range(min_y..=max_y),
        };
        let clear = placed
            .iter()
            .all(|other| (other.x - spot.x).hypot(other.y - spot.y) >= apart);
        if clear {
            placed.push(spot);
        }
    }
    placed
}

/// The seed of a request: a number that is the same every time the same request is drawn and
/// differs between requests. Taken from the request's own identifier.
pub fn seed_of(request: &str) -> u64 {
    // FNV-1a: a few lines, no dependency, and what matters here is only that it is stable and
    // spreads similar identifiers apart. It protects nothing: the place of a circle is not secret.
    request.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: f32 = 88.;
    const GAP: f32 = 16.;
    const W: f32 = 560.;
    const H: f32 = 300.;

    fn place(seed: u64, count: usize) -> Vec<Spot> {
        spots(seed, count, D, GAP, W, H)
    }

    #[test]
    fn there_is_one_place_for_every_circle() {
        for count in [1, 3, 5, 8] {
            assert_eq!(place(7, count).len(), count);
        }
    }

    #[test]
    fn every_circle_lies_wholly_inside_the_area() {
        for seed in 0..200 {
            for spot in place(seed, 5) {
                assert!(
                    spot.x - D / 2. >= 0. && spot.x + D / 2. <= W,
                    "x out: {spot:?}"
                );
                assert!(
                    spot.y - D / 2. >= 0. && spot.y + D / 2. <= H,
                    "y out: {spot:?}"
                );
            }
        }
    }

    #[test]
    fn no_two_circles_touch_or_overlap() {
        for seed in 0..200 {
            let spots = place(seed, 5);
            for (i, a) in spots.iter().enumerate() {
                for b in &spots[i + 1..] {
                    let distance = (a.x - b.x).hypot(a.y - b.y);
                    assert!(distance >= D + GAP, "seed {seed}: {distance} apart");
                }
            }
        }
    }

    #[test]
    fn one_request_always_gets_the_same_places_so_nothing_moves_under_the_finger() {
        assert_eq!(place(42, 5), place(42, 5));
    }

    #[test]
    fn different_requests_get_different_places() {
        let layouts: std::collections::HashSet<Vec<(i32, i32)>> = (0..50)
            .map(|seed| {
                place(seed, 5)
                    .iter()
                    .map(|s| (s.x as i32, s.y as i32))
                    .collect()
            })
            .collect();
        assert!(
            layouts.len() > 45,
            "only {} distinct layouts in 50",
            layouts.len()
        );
    }

    #[test]
    fn the_circles_are_not_set_in_a_row() {
        // In a row would mean the same height, or the same distance from the left, for all.
        // Over many requests, the spread in both directions must be wide, not a line.
        let mut rows = 0;
        for seed in 0..100 {
            let spots = place(seed, 5);
            let ys: Vec<f32> = spots.iter().map(|s| s.y).collect();
            let spread = ys.iter().cloned().fold(f32::MIN, f32::max)
                - ys.iter().cloned().fold(f32::MAX, f32::min);
            if spread < D {
                rows += 1;
            }
        }
        assert!(rows < 5, "{rows} of 100 layouts were a single row");
    }

    #[test]
    fn an_area_too_small_for_the_circles_gives_fewer_not_an_endless_loop() {
        let crowded = spots(1, 5, D, GAP, 120., 120.);
        assert!(crowded.len() < 5);
        assert!(!crowded.is_empty());
    }

    #[test]
    fn an_area_smaller_than_one_circle_gives_none() {
        assert!(spots(1, 3, D, GAP, 50., 50.).is_empty());
    }

    #[test]
    fn the_seed_of_a_request_is_stable_and_tells_requests_apart() {
        assert_eq!(seed_of("abc"), seed_of("abc"));
        assert_ne!(seed_of("abc"), seed_of("abd"));
        assert_ne!(seed_of(""), seed_of("a"));
    }
}
