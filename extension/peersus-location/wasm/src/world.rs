//! The world a map is drawn on: its land outlines, and where a view of them puts a place.
//!
//! The outlines travel with the extension. They are Natural Earth's 1:110m land data, which is in the public domain, and
//! nothing here reads a file, asks a service or keeps anything between runs: a view is a middle and a width in degrees, and
//! the arithmetic that puts a coastline pixel puts a position's pixel, so a dot lands on the coast it is on. At this scale a
//! coast is a line a few kilometres out: the drawing is a continental one, and a street is not in the data at all.

use crate::{
    canvas_sdk::{MAX_PATH_VERBS, Path},
    geometry::{self, Paths},
};
#[cfg(test)]
use std::sync::LazyLock;
mod globe;
pub(super) use globe::Camera;

/// The drawing's width in pixels.
pub const WIDTH: u16 = 300;
/// The drawing's height in pixels: half the width, as a degree of longitude is drawn as tall as it is wide.
pub const HEIGHT: u16 = 150;

/// Logical canvas to actual page pixels. The same transform drives land, avatars and gestures.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Frame(pub u16, pub u16);

/// The most degrees of longitude a view shows: the whole world.
const MOST_SPAN: f64 = 360.0;
/// Most regional atlases do not have street detail below this span.
pub const REGIONAL_SPAN: f64 = 5.0;
/// Street-level limit of the projection itself, independent of available data.
const LEAST_SPAN: f64 = 0.002;
/// Bound the number of outline vertices put into a single basemap frame.
const ROOM: usize = 1800;
/// A ring smaller than this, in square pixels, is a speck nobody sees.
const SPECK: f64 = 0.75;

/// A view of the world: where its middle is and how many degrees of longitude fit across the drawing. Latitudes are not
/// stretched: a degree is as tall as it is wide, so the view is twice as wide as it is tall, in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub lon: f64,
    pub lat: f64,
    pub span: f64,
    /// Screen-clockwise turn about the view axis, in degrees.
    pub roll: f64,
}

impl Default for View {
    fn default() -> Self {
        Self::whole()
    }
}

impl View {
    /// The whole world.
    pub fn whole() -> Self {
        Self {
            lon: 0.0,
            lat: 0.0,
            span: MOST_SPAN,
            roll: 0.,
        }
    }

    /// The geographic centre, independent of whether the globe is upside down.
    pub fn centre(self) -> (f64, f64) {
        let pitch = wrap(self.lat);
        if pitch.abs() > 90. {
            (wrap(self.lon + 180.), pitch.signum() * (180. - pitch.abs()))
        } else {
            (wrap(self.lon), pitch)
        }
    }

    pub fn bearing(self) -> f64 {
        let flip = if self.lat.abs() > 90. { 180. } else { 0. };
        wrap(self.roll + flip)
    }

    pub fn north_up(self) -> Self {
        let (lon, lat) = self.centre();
        Self {
            lon,
            lat,
            roll: 0.,
            ..self
        }
    }

    /// This view as a map can be drawn in: a width between the two limits, a middle that keeps the whole drawing on the
    /// Earth, and a longitude in the usual half turn.
    pub fn kept(self) -> Self {
        let span = self.span.clamp(LEAST_SPAN, MOST_SPAN);
        Self {
            lon: wrap(self.lon),
            lat: wrap(self.lat),
            span,
            roll: wrap(self.roll),
        }
    }

    /// This view `factor` times closer (`factor` above one) or wider, keeping the place under `anchor` where it is drawn.
    pub fn zoomed(self, factor: f64, anchor: Option<(f64, f64)>) -> Self {
        let span = (self.span / factor).clamp(LEAST_SPAN, MOST_SPAN);
        let keeps = span / self.span;
        match anchor {
            Some((lon, lat)) => Self {
                lon: lon - wrap(lon - self.lon) * keeps,
                lat: lat - (lat - self.lat) * keeps,
                span,
                roll: self.roll,
            },
            None => Self {
                lon: self.lon,
                lat: self.lat,
                span,
                roll: self.roll,
            },
        }
        .kept()
    }

    /// Fraction down the zoom scale. Each equal step changes magnification by the same ratio.
    pub(crate) fn zoom_position(self) -> f64 {
        (self.span / LEAST_SPAN).ln() / (MOST_SPAN / LEAST_SPAN).ln()
    }

    pub(crate) fn zoom_at(self, down: f64) -> Self {
        let span = if down >= 1. {
            MOST_SPAN
        } else if down <= 0. {
            LEAST_SPAN
        } else {
            LEAST_SPAN * (MOST_SPAN / LEAST_SPAN).powf(down)
        };
        Self { span, ..self }.kept()
    }

    /// The shortest turn to a bookmarked view; magnification changes evenly in octaves.
    pub(crate) fn towards(self, target: Self, progress: f64) -> Self {
        if progress >= 1. {
            return target;
        }
        Self {
            lon: self.lon + wrap(target.lon - self.lon) * progress,
            lat: self.lat + wrap(target.lat - self.lat) * progress,
            span: self.span * (target.span / self.span).powf(progress),
            roll: self.roll + wrap(target.roll - self.roll) * progress,
        }
        .kept()
    }
}

/// A longitude in the half turn from -180 to 180, the date line itself at 180.
pub(super) fn wrap(lon: f64) -> f64 {
    let turn = (lon + 180.0).rem_euclid(360.0) - 180.0;
    if turn == -180.0 { 180.0 } else { turn }
}

/// The land the extension carries as `world/land.txt`: for each outline, its points as degrees east and north. Read once,
/// the first time a map is drawn.
#[cfg(test)]
static LAND: LazyLock<Vec<Vec<(f64, f64)>>> = LazyLock::new(|| {
    include_str!("world/land.txt")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let mut outline = Vec::new();
            // Hundredths of a degree, whole: what the file writes, and what a guest needs no floating-point reading for.
            let (mut east, mut north) = (0i32, 0i32);
            for (index, step) in line.split_whitespace().enumerate() {
                let (east_step, north_step) = step.split_once(',')?;
                let (east_step, north_step) = (
                    east_step.parse::<i32>().ok()?,
                    north_step.parse::<i32>().ok()?,
                );
                if index == 0 {
                    (east, north) = (east_step, north_step);
                } else {
                    east += east_step;
                    north += north_step;
                }
                outline.push((f64::from(east) / 100.0, f64::from(north) / 100.0));
            }
            (outline.len() >= 4).then_some(outline)
        })
        .collect()
});

/// The land outlines as carried: degrees east and north, a whole outline to a ring.
#[cfg(test)]
fn land() -> &'static [Vec<(f64, f64)>] {
    &LAND
}

static LAND_VECTORS: &[&[(f32, f32, f32)]] = include!(concat!(env!("OUT_DIR"), "/land_vectors.rs"));

/// The land this view shows, as one outline path in the drawing's pixels, or nothing where no land is in it. What is
/// written fits a drawing whatever the view: a coarser outline is drawn instead, and at the last only as many outlines as
/// fit, the largest first.
#[cfg(test)]
pub fn land_path(view: &View) -> Option<Paths> {
    let frame = Frame(WIDTH, HEIGHT);
    projected_land(&Camera::new(*view, frame), frame)
}

pub fn projected_land(camera: &Camera, frame: Frame) -> Option<Paths> {
    let fits = camera.globe_fits();
    let mut rings: Vec<_> = LAND_VECTORS
        .iter()
        .filter_map(|ring| {
            let points = camera.ring(ring);
            if fits {
                (points.len() >= 3).then_some(points)
            } else {
                clip_frame(points, frame)
            }
        })
        .filter_map(|ring| {
            let enclosed = area(&ring);
            (enclosed >= SPECK).then_some((enclosed, ring))
        })
        .collect();
    rings.sort_by(|(a, _), (b, _)| b.total_cmp(a));
    let mut tolerance = 0.6;
    loop {
        let mut path = Paths::default();
        let mut overflow = false;
        for (_, ring) in &rings {
            let points = simplify(ring, tolerance);
            if points.len() < 3 {
                continue;
            }
            let mut part = Path::default();
            geometry::ring(&mut part, points);
            if part.len() > MAX_PATH_VERBS || path.len() + part.len() > ROOM {
                overflow = true;
                break;
            }
            path.append(&part);
        }
        if !overflow || tolerance >= 60. {
            return (!path.is_empty()).then_some(path);
        }
        tolerance *= 1.7;
    }
}

/// The part of a ring inside the drawing, by cutting it off at each of the drawing's four edges, or nothing when it is all
/// outside. The last point need not repeat the first: the outline is closed by the drawing itself.
#[cfg(test)]
fn clip(ring: &[(f64, f64)]) -> Option<Vec<(f64, f64)>> {
    clip_frame(ring.to_vec(), Frame(WIDTH, HEIGHT))
}

pub(crate) fn clip_frame(mut points: Vec<(f64, f64)>, frame: Frame) -> Option<Vec<(f64, f64)>> {
    if points.len() < 3 {
        return None;
    }
    let (width, height) = (f64::from(frame.0), f64::from(frame.1));
    for edge in 0..4 {
        // Which side of this edge a point must be on to be kept, the edge's own coordinate, and which of a point's two
        // numbers it is compared against.
        let limit = match edge {
            0 => 0.0,
            1 => width,
            2 => 0.0,
            _ => height,
        };
        let above = edge < 2;
        let inside = |point: &(f64, f64)| {
            let value = if above { point.0 } else { point.1 };
            if edge % 2 == 0 {
                value >= limit
            } else {
                value <= limit
            }
        };
        if points.iter().all(inside) {
            continue;
        }
        let crossing = |a: &(f64, f64), b: &(f64, f64)| -> (f64, f64) {
            let (from, to) = if above { (a.0, b.0) } else { (a.1, b.1) };
            let t = if (to - from).abs() <= f64::EPSILON {
                0.0
            } else {
                (limit - from) / (to - from)
            };
            (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
        };
        let mut kept = Vec::with_capacity(points.len() + 4);
        let mut previous = points.last().expect("ring is not empty");
        let mut previous_inside = inside(previous);
        for current in &points {
            let current_inside = inside(current);
            match (previous_inside, current_inside) {
                (true, true) => kept.push(*current),
                (true, false) => kept.push(crossing(previous, current)),
                (false, true) => {
                    kept.push(crossing(previous, current));
                    kept.push(*current);
                }
                (false, false) => {}
            }
            previous = current;
            previous_inside = current_inside;
        }
        if kept.len() < 3 {
            return None;
        }
        points = kept;
    }
    (points.len() >= 3).then_some(points)
}

/// `ring` with the points that say nothing about its shape left out: no point of it is further than `tolerance` pixels
/// from the line the outline takes without it.
pub(crate) fn simplify(ring: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    if ring.len() <= 2 {
        return ring.to_vec();
    }
    let mut kept = vec![false; ring.len()];
    kept[0] = true;
    kept[ring.len() - 1] = true;
    let mut spans = vec![(0usize, ring.len() - 1)];
    let limit = tolerance * tolerance;
    while let Some((first, last)) = spans.pop() {
        let (a, b) = (ring[first], ring[last]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = dx * dx + dy * dy;
        // Each candidate has the same denominator. Compare squared cross products
        // and divide neither for the threshold nor for the farthest vertex.
        let mut worst = limit * length.max(f64::EPSILON);
        let mut at = None;
        for (i, &(x, y)) in ring[first + 1..last].iter().enumerate() {
            let (x, y) = (x - a.0, y - a.1);
            let off = if length <= f64::EPSILON {
                (x * x + y * y) * f64::EPSILON
            } else {
                let cross = dx * y - dy * x;
                cross * cross
            };
            if off > worst {
                worst = off;
                at = Some(first + 1 + i);
            }
        }
        if let Some(index) = at {
            kept[index] = true;
            spans.push((first, index));
            spans.push((index, last));
        }
    }
    ring.iter()
        .zip(kept)
        .filter_map(|(point, kept)| kept.then_some(*point))
        .collect()
}

/// The area a ring encloses, in square pixels.
fn area(ring: &[(f64, f64)]) -> f64 {
    let mut sum = 0.0;
    for (index, point) in ring.iter().enumerate() {
        let next = ring[(index + 1) % ring.len()];
        sum += point.0 * next.1 - next.0 * point.1;
    }
    (sum / 2.0).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_carried_world_is_the_carried_data() {
        let rings = land();
        assert_eq!(rings.len(), 128, "every outline of the data is read");
        let points: usize = rings.iter().map(Vec::len).sum();
        assert_eq!(points, 4998);
        assert!(
            rings
                .iter()
                .flatten()
                .all(|(lon, lat)| (-540.0..=540.0).contains(lon) && (-90.0..=90.0).contains(lat)),
            "every point is a place on Earth, and an outline across the date line is unrolled"
        );
        assert!(
            rings.iter().map(Vec::len).max().unwrap() > 1_000,
            "the old world is one outline of more than a thousand points"
        );
    }

    #[test]
    fn the_carried_world_draws_land_where_land_is() {
        // Places whose answer nobody argues with, and the open sea between them. The test is of the carried outlines, not of
        // a projection: it is what says the drawing is a map and not a pattern.
        for (lon, lat) in [
            (10.0, 50.0),
            (100.0, 60.0),
            (-100.0, 40.0),
            (135.0, -25.0),
            (20.0, 0.0),
            (-60.0, -10.0),
            (172.0, -42.0),
            (178.0, 67.0),
        ] {
            assert!(covered(lon, lat), "{lon} {lat} is land");
        }
        for (lon, lat) in [
            (0.0, 0.0),
            (-30.0, 30.0),
            (-150.0, 0.0),
            (0.0, -60.0),
            (80.0, -40.0),
            (-179.0, -30.0),
            (179.0, 0.0),
        ] {
            assert!(!covered(lon, lat), "{lon} {lat} is sea");
        }
    }

    /// Whether the carried outlines cover a place: a ray across it crosses an odd number of them, the sea inside an outline
    /// counting as one crossing of its own.
    fn covered(lon: f64, lat: f64) -> bool {
        let mut crossings = 0;
        for ring in land() {
            for turn in [-360.0, 0.0, 360.0] {
                let points: Vec<(f64, f64)> = ring.iter().map(|(x, y)| (*x + turn, *y)).collect();
                for (index, a) in points.iter().enumerate() {
                    let b = points[(index + 1) % points.len()];
                    if (a.1 > lat) == (b.1 > lat) {
                        continue;
                    }
                    let at = a.0 + (lat - a.1) / (b.1 - a.1) * (b.0 - a.0);
                    if at > lon {
                        crossings += 1;
                    }
                }
            }
        }
        crossings % 2 == 1
    }

    #[test]
    fn zoom_changes_the_span_within_its_limits() {
        let view = View {
            lon: 0.0,
            lat: 0.0,
            span: 90.0,
            roll: 0.,
        };
        let closer = view.zoomed(4.0, None);
        assert!((closer.span - 22.5).abs() < 1e-9);
        // Neither limit is passed.
        assert_eq!(view.zoomed(1e9, None).span, LEAST_SPAN);
        assert_eq!(view.zoomed(1e-9, None).span, MOST_SPAN);
        // Geometry has its own limit; the page separately limits offline views to the available data.
        assert_eq!(
            View {
                lon: 0.0,
                lat: 0.0,
                span: 0.001,
                roll: 0.,
            }
            .kept()
            .span,
            LEAST_SPAN
        );
    }

    #[test]
    fn the_drawing_shows_land_where_the_data_has_land() {
        // Every pixel the drawing fills is land the data has, and no other: a longitude folded back across the drawing, or
        // a misplaced one, would fill a band of open sea at once. Places well inside a continent and well out at sea are
        // asked, at a world view and at a view across the date line.
        let places = [
            (10.0, 50.0),
            (100.0, 60.0),
            (-100.0, 40.0),
            (135.0, -25.0),
            (20.0, 0.0),
            (-60.0, -10.0),
            (172.0, -42.0),
            (0.0, 0.0),
            (-30.0, 30.0),
            (-150.0, 0.0),
            (0.0, -60.0),
            (80.0, -40.0),
            (-179.0, -30.0),
            (179.0, 0.0),
            (179.0, -17.0),
        ];
        for view in [
            View::whole(),
            View {
                lon: 179.0,
                lat: -17.0,
                span: 20.0,
                roll: 0.,
            },
            View {
                lon: -179.5,
                lat: -20.0,
                span: 8.0,
                roll: 0.,
            },
            View {
                lon: 20.0,
                lat: 45.0,
                span: 5.0,
                roll: 0.,
            },
        ] {
            let view = view.kept();
            let drawn = land_path(&view).unwrap_or_default().contours();
            for (lon, lat) in places {
                let Some((x, y)) = Camera::new(view, Frame(WIDTH, HEIGHT)).point(lon, lat) else {
                    continue;
                };
                if !(4.0..=f64::from(WIDTH) - 4.0).contains(&x)
                    || !(4.0..=f64::from(HEIGHT) - 4.0).contains(&y)
                {
                    continue;
                }
                assert_eq!(
                    within(&drawn, x, y),
                    covered(lon, lat),
                    "{lon} {lat} at {view:?}"
                );
            }
        }
    }

    /// Whether a point of the drawing is inside the outlines a path draws: a ray to the right crosses an odd number of
    /// them, each outline closed from its last point to its first as the drawing closes it.
    fn within(outlines: &[Vec<(f64, f64)>], x: f64, y: f64) -> bool {
        let mut crossings = 0;
        for outline in outlines {
            for (index, a) in outline.iter().enumerate() {
                let b = outline[(index + 1) % outline.len()];
                if (a.1 > y) == (b.1 > y) {
                    continue;
                }
                let at = a.0 + (y - a.1) / (b.1 - a.1) * (b.0 - a.0);
                if at > x {
                    crossings += 1;
                }
            }
        }
        crossings % 2 == 1
    }

    #[test]
    fn every_outline_stays_on_the_drawing_and_the_date_line_has_no_seam() {
        for view in [
            View::whole(),
            View {
                lon: 179.0,
                lat: -17.0,
                span: 20.0,
                roll: 0.,
            },
            View {
                lon: -180.0,
                lat: -70.0,
                span: 60.0,
                roll: 0.,
            },
            View {
                lon: 0.0,
                lat: 90.0,
                span: 360.0,
                roll: 0.,
            },
            View {
                lon: 0.0,
                lat: -90.0,
                span: 360.0,
                roll: 0.,
            },
        ] {
            let view = view.kept();
            let path = land_path(&view).expect("the world has land");
            assert!(path.contours().iter().all(|ring| ring.len() >= 3));
        }
        let view = View {
            lon: 180.,
            lat: 0.,
            span: 360.,
            roll: 0.,
        };
        let camera = Camera::new(view, Frame(WIDTH, HEIGHT));
        let (a, b) = (
            camera.point(179., 0.).unwrap(),
            camera.point(-179., 0.).unwrap(),
        );
        assert!((a.0 - b.0).abs() < 3.);
        assert!(camera.point(0., 0.).is_none());
    }

    #[test]
    fn no_view_can_ask_for_more_than_a_drawing_may_carry() {
        // A grid of views, from the whole world to the closest the data is drawn, every corner of the Earth and both sides
        // of the date line.
        for (lon, lat) in [
            (0.0, 0.0),
            (179.0, 0.0),
            (-179.0, 0.0),
            (0.0, 78.0),
            (0.0, -78.0),
            (26.0, 45.0),
            (151.0, -33.0),
            (-70.0, -40.0),
            (10.0, 50.0),
            (-100.0, 40.0),
        ] {
            for span in [360.0, 180.0, 90.0, 45.0, 20.0, 8.0, 5.0, 1.0, 0.02] {
                let view = View {
                    lon,
                    lat,
                    span,
                    roll: 0.,
                }
                .kept();
                let path = land_path(&view).unwrap_or_default();
                assert!(
                    path.len() <= ROOM,
                    "{} at {lon},{lat} span {span}",
                    path.len()
                );
                for (x, y) in path.contours().into_iter().flatten() {
                    assert!(
                        (0.0..=f64::from(WIDTH)).contains(&x)
                            && (0.0..=f64::from(HEIGHT)).contains(&y),
                        "{x} {y} at {lon},{lat} span {span}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_position_is_drawn_on_the_coast_it_is_on() {
        // A place of a carried outline is drawn where that outline is drawn, to within a pixel of what the path writes.
        let ring = land().iter().max_by_key(|ring| ring.len()).unwrap();
        let (lon, lat) = ring[ring.len() / 3];
        let view = View {
            lon,
            lat,
            span: 12.0,
            roll: 0.,
        }
        .kept();
        let path = land_path(&view).expect("there is land around it");
        let (x, y) = Camera::new(view, Frame(WIDTH, HEIGHT))
            .point(lon, lat)
            .expect("the place is on the near side");
        let nearest = path
            .contours()
            .into_iter()
            .flatten()
            .map(|(px, py)| ((px - x).powi(2) + (py - y).powi(2)).sqrt())
            .fold(f64::INFINITY, f64::min);
        assert!(
            nearest <= 1.5,
            "the coast is drawn {nearest} pixels from the place"
        );
    }

    #[test]
    fn a_ring_is_cut_at_the_edge_of_the_drawing_and_nothing_else() {
        let square = [(10.0, 10.0), (290.0, 10.0), (290.0, 140.0), (10.0, 140.0)];
        assert_eq!(clip(&square).unwrap().len(), 4);
        let off = [(310.0, 10.0), (330.0, 40.0), (310.0, 70.0)];
        assert!(clip(&off).is_none());
        let over = [(-40.0, 75.0), (340.0, 75.0), (340.0, 90.0), (-40.0, 90.0)];
        let clipped = clip(&over).unwrap();
        assert!(
            clipped
                .iter()
                .all(|(x, y)| (0.0..=300.0).contains(x) && (0.0..=150.0).contains(y))
        );
        assert!(area(&clipped) > 200.0 * 10.0);
    }

    #[test]
    fn a_coarse_outline_says_what_a_fine_one_says_more_cheaply() {
        let line: Vec<(f64, f64)> = (0..=20).map(|i| (i as f64 * 5.0, 0.0)).collect();
        assert_eq!(
            simplify(&line, 0.4).len(),
            2,
            "a straight line needs its ends"
        );
        let bend: Vec<(f64, f64)> = vec![(0.0, 0.0), (10.0, 10.0), (20.0, 0.0)];
        assert_eq!(simplify(&bend, 0.4).len(), 3);
        let mut part = Path::default();
        geometry::ring(&mut part, [(0., 0.), (10., 0.), (10., 5.), (10., 5.)]);
        let mut paths = Paths::default();
        paths.append(&part);
        assert_eq!(paths.contours(), [vec![(0., 0.), (10., 0.), (10., 5.)]]);
    }
}
