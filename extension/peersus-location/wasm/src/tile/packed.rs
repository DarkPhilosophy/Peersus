//! Prepare provider geometry once. The helper places and clips complete tile-local paths;
//! panning does not re-project every road vertex or rebuild any polygon in the guest.
use super::{Detail, Feature, Key, Layer, Pen, Point};
use crate::{
    canvas_sdk::{Color, List, MAX_PATH_VERBS, Paint, Path},
    world::{Camera, Frame},
};
use std::{collections::BTreeMap, rc::Rc};

const EXTENT: f32 = 4096.;
const CELLS: u16 = 8;

const ARTERIAL_WIDTH: f32 = 1.6;
// Optional layers above this much paint are deferred on compact high-DPI
// gestures. These whole-layer limits keep the major network intact.
const GESTURE_BUILT_UP_VERBS: usize = 2_048;
const GESTURE_BUILDING_VERBS: usize = 8_192;
const GESTURE_LOCAL_ROAD_VERBS: usize = 4_096;
const STYLES: [(Layer, Color, f32, u8); 7] = [
    (Layer::BuiltUp, Color::Muted, 0., 26),
    (Layer::Water, Color::Surface, 0., 255),
    (Layer::Park, Color::Accent, 0., 38),
    (Layer::Building, Color::Raised, 0., 255),
    (Layer::Path, Color::Muted, 0.65, 180),
    (Layer::LocalRoad, Color::Muted, 1., 215),
    (Layer::Road, Color::Accent, ARTERIAL_WIDTH, 255),
];

#[derive(Clone, Copy, Default)]
struct Budget {
    paths: usize,
    verbs: usize,
}

impl Budget {
    fn within(self, limit: Self) -> Self {
        Self {
            paths: self.paths.min(limit.paths),
            verbs: self.verbs.min(limit.verbs),
        }
    }

    fn take(&mut self, wanted: Self) -> Self {
        let n = self.within(wanted);
        self.paths -= n.paths;
        self.verbs -= n.verbs;
        n
    }
}

fn demand(tiles: &[Placement], span: f64) -> [Budget; STYLES.len()] {
    let mut demand = [Budget::default(); STYLES.len()];
    for (i, (kind, _, _, _)) in STYLES.into_iter().enumerate() {
        if kind.shown(span) {
            for path in tiles.iter().flat_map(|tile| tile.paths(kind)) {
                demand[i].paths += 1;
                demand[i].verbs += path.len();
            }
        }
    }
    demand
}

#[cfg(test)]
pub(super) fn counts(tiles: &[Placement], span: f64) -> [(usize, usize); STYLES.len()] {
    demand(tiles, span).map(|budget| (budget.paths, budget.verbs))
}

pub(super) fn has_detail(tiles: &[Placement], span: f64) -> bool {
    STYLES.into_iter().any(|(kind, ..)| {
        kind.shown(span) && tiles.iter().any(|tile| tile.paths(kind).next().is_some())
    })
}

pub(super) fn paint(
    tiles: &[Placement],
    list: &mut List<'_>,
    span: f64,
    detail: Detail,
    scale: u32,
) -> bool {
    let mut demand = demand(tiles, span);
    if detail.preview() {
        let has = |layer| {
            STYLES
                .into_iter()
                .zip(demand)
                .any(|((kind, ..), budget)| kind == layer && budget.paths > 0)
        };
        let arterials = has(Layer::Road);
        let streets = arterials || has(Layer::LocalRoad);
        let has_context = streets || has(Layer::Water) || has(Layer::Park);
        for ((kind, ..), budget) in STYLES.into_iter().zip(&mut demand) {
            let defer = match kind {
                // Footways are still useful when they are the only roads.
                Layer::Path => streets,
                Layer::BuiltUp => {
                    detail == Detail::Gesture
                        && has_context
                        && budget.verbs > GESTURE_BUILT_UP_VERBS
                }
                Layer::Building => {
                    detail == Detail::Gesture
                        && has_context
                        && budget.verbs > GESTURE_BUILDING_VERBS
                }
                Layer::LocalRoad => {
                    detail == Detail::Gesture
                        && arterials
                        && budget.verbs > GESTURE_LOCAL_ROAD_VERBS
                }
                _ => false,
            };
            if defer {
                *budget = Budget::default();
            }
        }
    }
    // A native hairline covers one device pixel without building a stroked
    // outline. Use it only when the normal arterial stroke would be narrower.
    let arterial_hairline = detail.preview() && ARTERIAL_WIDTH * (scale as f32) < 1000.;
    let active = demand.iter().filter(|d| d.paths > 0).count();
    if active == 0 {
        return false;
    }
    // Apart from four scope commands per tile, keep room for controls and labels.
    let mut rest = Budget {
        paths: list
            .remaining_commands()
            .saturating_sub(256 + 4 * tiles.len()),
        verbs: list.remaining_verbs().saturating_sub(512),
    };
    let share = Budget {
        paths: rest.paths / active,
        verbs: rest.verbs / active,
    };
    let mut room = demand.map(|d| rest.take(d.within(share)));
    // Full demand fits without dropping anything in ordinary views. Under pressure
    // redistribute unused shares to the street network and buildings before broad fills.
    for i in [6, 5, 3, 1, 4, 2, 0] {
        let extra = rest.take(Budget {
            paths: demand[i].paths - room[i].paths,
            verbs: demand[i].verbs - room[i].verbs,
        });
        room[i].paths += extra.paths;
        room[i].verbs += extra.verbs;
    }
    tiles.iter().fold(false, |drawn, tile| {
        tile.paint(list, &mut room, arterial_hairline) || drawn
    })
}

#[derive(Clone, Copy)]
struct Bounds([f32; 4]);

impl Bounds {
    fn of(points: impl IntoIterator<Item = [f32; 2]>) -> Self {
        Self(points.into_iter().fold(
            [
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            ],
            |[l, t, r, b], [x, y]| [l.min(x), t.min(y), r.max(x), b.max(y)],
        ))
    }

    fn extend(&mut self, Self([l, t, r, b]): Self) {
        let [a, c, d, e] = self.0;
        self.0 = [a.min(l), c.min(t), d.max(r), e.max(b)];
    }

    fn intersects(self, Self([l, t, r, b]): Self) -> bool {
        let [x, y, e, f] = self.0;
        x <= r && y <= b && e >= l && f >= t
    }

    fn in_tile(self) -> bool {
        let [left, top, right, bottom] = self.0;
        left >= 0. && top >= 0. && right <= EXTENT && bottom <= EXTENT
    }

    fn cell(self) -> u16 {
        let [l, t, r, b] = self.0;
        let index = |a: f32, b: f32| {
            (a.midpoint(b) * f32::from(CELLS) / EXTENT)
                .floor()
                .clamp(0., f32::from(CELLS - 1)) as u16
        };
        index(l, r) + CELLS * index(t, b)
    }
}

struct Batch {
    bounds: Bounds,
    path: Path,
}

#[derive(Default)]
pub(super) struct Packed(BTreeMap<(Layer, u16), Vec<Batch>>);

impl Packed {
    pub(super) fn source_bytes(&self) -> usize {
        self.0
            .values()
            .map(|batches| {
                batches.capacity() * size_of::<Batch>()
                    + batches
                        .iter()
                        .map(|b| 2 * b.path.bytes().len())
                        .sum::<usize>()
            })
            .sum()
    }

    /// Prepare `feature` and say what it cost, in points handled: what [`super::Decode`] bounds a turn by. A line costs its
    /// points once; a polygon too large for one path costs them again at every split. One feature is bounded by the points
    /// kept of it (`super::FEATURE_KEPT`) and by the six splits below.
    pub(super) fn add(&mut self, feature: &Feature) -> usize {
        let mut work = feature.geometry.len();
        let factor = EXTENT / feature.extent as f32;
        let point = |(x, y): (i32, i32)| [x as f32 * factor, y as f32 * factor];
        let layer = feature.kind;
        if feature.kind.line() {
            let mut path = Path::default();
            let mut previous = None;
            let mut bounds = Bounds([0.; 4]);
            for pen in straight_runs(&feature.geometry) {
                match pen {
                    Pen::Move(p) => {
                        self.batch(layer, bounds, &path);
                        path.clear();
                        previous = Some(point(p));
                    }
                    Pen::Line(p) => {
                        let p = point(p);
                        if let Some(last) = previous {
                            if last != p {
                                if path.len() == MAX_PATH_VERBS {
                                    self.batch(layer, bounds, &path);
                                    path.clear();
                                }
                                if path.is_empty() {
                                    path.move_to(last);
                                    bounds = Bounds::of([last]);
                                }
                                path.line_to(p);
                                bounds.extend(Bounds::of([p]));
                            }
                        }
                        previous = Some(p);
                    }
                    Pen::Close => return work,
                }
            }
            self.batch(layer, bounds, &path);
        } else if let Some(rings) = polygon(feature, point) {
            // Providers may combine hundreds of separate footprints into one
            // MVT feature. Batch each shell with only its own holes.
            let mut start = 0;
            for (index, ring) in rings.iter().enumerate().skip(1) {
                if signed_area(ring) > 0. {
                    self.polygons(
                        feature.kind,
                        &rings[start..index],
                        [0., 0., EXTENT, EXTENT],
                        0,
                        &mut work,
                    );
                    start = index;
                }
            }
            self.polygons(
                feature.kind,
                &rings[start..],
                [0., 0., EXTENT, EXTENT],
                0,
                &mut work,
            );
        }
        work
    }

    fn polygons(
        &mut self,
        kind: Layer,
        rings: &[Vec<[f32; 2]>],
        clip: [f32; 4],
        depth: u8,
        work: &mut usize,
    ) {
        let count: usize = rings.iter().map(|ring| ring.len() + 1).sum();
        if count <= MAX_PATH_VERBS {
            let bounds = Bounds::of(rings.iter().flatten().copied());
            self.batch(kind, bounds, &closed_path(rings));
            return;
        }
        // Unusually long rings are split spatially rather than closed across an
        // open line, which would introduce a diagonal wedge. Clip shells and holes together.
        if depth >= 6 {
            return;
        }
        // Both halves read every point of these rings.
        *work += 2 * count;
        let [l, t, r, b] = clip;
        let parts = if r - l >= b - t {
            let mid = l.midpoint(r);
            [[l, t, mid, b], [mid, t, r, b]]
        } else {
            let mid = t.midpoint(b);
            [[l, t, r, mid], [l, mid, r, b]]
        };
        for area in parts {
            let clipped: Vec<_> = rings
                .iter()
                .map(|ring| clip_ring(ring, area))
                .filter(|ring| ring.len() >= 3)
                .collect();
            self.polygons(kind, &clipped, area, depth + 1, work);
        }
    }

    fn batch(&mut self, kind: Layer, bounds: Bounds, path: &Path) {
        if path.is_empty() {
            return;
        }
        let batches = self.0.entry((kind, bounds.cell())).or_default();
        if batches
            .last()
            .is_none_or(|b| b.path.len() + path.len() > MAX_PATH_VERBS)
        {
            batches.push(Batch {
                bounds,
                path: Path::default(),
            });
        }
        let last = batches.last_mut().expect("valid bounded batch");
        last.bounds.extend(bounds);
        last.path.append(path);
    }

    fn batches(&self, kind: Layer, visible: Bounds) -> impl Iterator<Item = &Batch> {
        self.0
            .range((kind, 0)..=(kind, u16::MAX))
            .flat_map(|(_, parts)| parts)
            .filter(move |part| part.bounds.intersects(visible))
    }

    #[cfg(test)]
    fn paths(&self, kind: Layer, visible: Bounds) -> impl Iterator<Item = &Path> {
        self.batches(kind, visible).map(|part| &part.path)
    }
}

/// Skip only vertices that lie exactly on a forward straight run in the
/// provider's integer grid. Skia otherwise strokes each redundant join,
/// which is particularly expensive when the tile matrix is turned.
fn straight_runs(pens: &[Pen]) -> impl Iterator<Item = Pen> + '_ {
    let mut pens = pens.iter().copied().peekable();
    let mut previous = None;
    std::iter::from_fn(move || {
        loop {
            return match pens.next()? {
                Pen::Move(p) => {
                    previous = Some(p);
                    Some(Pen::Move(p))
                }
                Pen::Line(mut p) => {
                    let Some(from) = previous.filter(|&from| from != p) else {
                        continue;
                    };
                    while let Some(&Pen::Line(next)) = pens.peek() {
                        if !continues_straight(from, p, next) {
                            break;
                        }
                        pens.next();
                        p = next;
                    }
                    previous = Some(p);
                    Some(Pen::Line(p))
                }
                Pen::Close => Some(Pen::Close),
            };
        }
    })
}

fn continues_straight(from: Point, at: Point, to: Point) -> bool {
    let delta = |a: Point, b: Point| {
        (
            i64::from(b.0) - i64::from(a.0),
            i64::from(b.1) - i64::from(a.1),
        )
    };
    let ((dx, dy), (x, y)) = (delta(from, at), delta(at, to));
    dx * y == dy * x && dx * x + dy * y >= 0
}

fn signed_area(ring: &[[f32; 2]]) -> f64 {
    ring.iter()
        .zip(ring.iter().cycle().skip(1))
        .map(|([x, y], [r, b])| f64::from(*x) * f64::from(*b) - f64::from(*y) * f64::from(*r))
        .sum()
}

fn polygon(
    feature: &Feature,
    point: impl Fn((i32, i32)) -> [f32; 2],
) -> Option<Vec<Vec<[f32; 2]>>> {
    let mut rings = Vec::new();
    let mut ring = Vec::new();
    for &pen in &feature.geometry {
        match pen {
            Pen::Move(p) if ring.is_empty() => ring.push(point(p)),
            Pen::Line(p) if !ring.is_empty() => {
                let p = point(p);
                if ring.last() != Some(&p) {
                    ring.push(p);
                }
            }
            Pen::Close if ring.len() >= 3 => {
                if ring.first() == ring.last() {
                    ring.pop();
                }
                if ring.len() >= 3 {
                    rings.push(std::mem::take(&mut ring));
                } else {
                    ring.clear();
                }
            }
            _ => return None,
        }
    }
    ring.is_empty().then_some(rings)
}

fn closed_path(rings: &[Vec<[f32; 2]>]) -> Path {
    let mut path = Path::default();
    for ring in rings {
        if let Some((&first, rest)) = ring.split_first() {
            path.move_to(first);
            for &point in rest {
                path.line_to(point);
            }
            path.close();
        }
    }
    path
}

fn clip_ring(ring: &[[f32; 2]], [l, t, r, b]: [f32; 4]) -> Vec<[f32; 2]> {
    let mut ring = ring.to_vec();
    let mut out = Vec::new();
    for (axis, bound, lower) in [(0, l, true), (0, r, false), (1, t, true), (1, b, false)] {
        let Some(mut prev) = ring.last().copied() else {
            break;
        };
        let inside = |p: [f32; 2]| {
            if lower {
                p[axis] >= bound
            } else {
                p[axis] <= bound
            }
        };
        for &next in &ring {
            if inside(prev) != inside(next) {
                let q = (bound - prev[axis]) / (next[axis] - prev[axis]);
                let mut cross = [
                    prev[0] + q * (next[0] - prev[0]),
                    prev[1] + q * (next[1] - prev[1]),
                ];
                cross[axis] = bound;
                out.push(cross);
            }
            if inside(next) {
                out.push(next);
            }
            prev = next;
        }
        std::mem::swap(&mut ring, &mut out);
        out.clear();
    }
    ring
}

/// All tiles at one level use one affine grid so shared edges stay shared, even when turned.
#[derive(Clone, Copy)]
pub(super) struct Grid {
    origin: [f64; 2],
    centre: (f64, f64),
    east: [f64; 2],
    south: [f64; 2],
    period: f64,
    frame: Frame,
}

impl Grid {
    pub(super) fn new(
        camera: &Camera,
        view: crate::world::View,
        frame: Frame,
        z: u8,
    ) -> Option<Self> {
        let (lon, lat) = view.centre();
        let centre = super::xy(z, lon, lat);
        let period = f64::from(1_u32 << z);
        let project = |x: f64, y: f64| {
            camera.mercator_point(
                360. * x / period - 180.,
                std::f64::consts::PI * (1. - 2. * y / period),
            )
        };
        let (x, y) = centre;
        let (cx, cy) = project(x, y)?;
        let (left, right) = (project(x - 0.01, y)?, project(x + 0.01, y)?);
        let (top, bottom) = (project(x, y - 0.01)?, project(x, y + 0.01)?);
        let basis = |(a, b): (f64, f64), (c, d): (f64, f64)| [(c - a) / 0.02, (d - b) / 0.02];
        Some(Self {
            origin: [cx, cy],
            centre,
            east: basis(left, right),
            south: basis(top, bottom),
            period,
            frame,
        })
    }

    pub(super) fn place(self, key: Key, art: Rc<Packed>) -> Option<Placement> {
        let mut x = f64::from(key.x) - self.centre.0;
        x -= ((x + 0.5) / self.period).round() * self.period;
        let y = f64::from(key.y) - self.centre.1;
        let [a, b] = self.east.map(|n| n / f64::from(EXTENT));
        let [c, d] = self.south.map(|n| n / f64::from(EXTENT));
        let e = self.origin[0] + x * self.east[0] + y * self.south[0];
        let f = self.origin[1] + x * self.east[1] + y * self.south[1];
        let det = a * d - b * c;
        if det.abs() < f64::EPSILON {
            return None;
        }
        let cell = |x: f64, y: f64| {
            let (x, y) = (x - e, y - f);
            [
                ((d * x - c * y) / det) as f32,
                ((a * y - b * x) / det) as f32,
            ]
        };
        let w = f64::from(self.frame.0);
        let h = f64::from(self.frame.1);
        let visible = Bounds::of([cell(0., 0.), cell(w, 0.), cell(0., h), cell(w, h)]);
        if !visible.intersects(Bounds([0., 0., EXTENT, EXTENT])) {
            return None;
        }
        Some(Placement {
            art,
            visible,
            affine: [a, b, c, d, e, f].map(|n| n as f32),
            unit: a.hypot(b).max(c.hypot(d)) as f32,
        })
    }
}

pub(super) struct Placement {
    art: Rc<Packed>,
    visible: Bounds,
    affine: [f32; 6],
    unit: f32,
}

impl Placement {
    pub(super) fn point(&self, extent: u32, (x, y): (i32, i32)) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.affine.map(f64::from);
        let scale = f64::from(EXTENT) / f64::from(extent);
        let (x, y) = (f64::from(x) * scale, f64::from(y) * scale);
        (a * x + c * y + e, b * x + d * y + f)
    }

    fn batches(&self, kind: Layer) -> impl Iterator<Item = &Batch> {
        let mut visible = self.visible;
        if kind.line() {
            let margin = 1. / self.unit;
            let [l, t, r, b] = visible.0;
            visible = Bounds([l - margin, t - margin, r + margin, b + margin]);
        }
        self.art.batches(kind, visible)
    }

    fn paths(&self, kind: Layer) -> impl Iterator<Item = &Path> {
        self.batches(kind).map(|batch| &batch.path)
    }

    pub(super) fn count(&self, kind: Layer) -> usize {
        self.paths(kind).count()
    }

    fn paint(
        &self,
        list: &mut List<'_>,
        room: &mut [Budget; STYLES.len()],
        arterial_hairline: bool,
    ) -> bool {
        let mut painted = false;
        if room.iter().all(|b| b.paths == 0 || b.verbs == 0) {
            return false;
        }
        // An AA clip on a rotated tile is expensive for every path it covers.
        // In-tile hairlines need no mask: only their half-pixel caps may
        // cross the edge, where they can join the next tile. Keep the exact
        // tile clip for fills, normal strokes, and any buffered centreline.
        let needs_clip = !arterial_hairline
            || STYLES
                .into_iter()
                .zip(room.iter())
                .any(|((kind, ..), quota)| {
                    quota.paths > 0
                        && quota.verbs > 0
                        && self
                            .batches(kind)
                            .any(|b| kind != Layer::Road || !b.bounds.in_tile())
                });
        let draw = |list: &mut List<'_>| {
            for ((kind, fill, stroke, opacity), quota) in STYLES.into_iter().zip(room) {
                if quota.paths == 0 || quota.verbs == 0 {
                    continue;
                }
                let paint = if stroke == 0. {
                    Paint::Fill(fill, opacity)
                } else {
                    let width = if kind == Layer::Road && arterial_hairline {
                        0.
                    } else {
                        (stroke / self.unit).min(1024.)
                    };
                    Paint::Stroke(fill, width, opacity)
                };
                for path in self.paths(kind) {
                    if quota.paths == 0 {
                        break;
                    }
                    if quota.verbs < path.len() {
                        continue;
                    }
                    quota.paths -= 1;
                    quota.verbs -= path.len();
                    list.path(path, paint);
                    painted = true;
                }
            }
        };
        list.transformed(self.affine, |list| {
            if needs_clip {
                list.clipped([0., 0., EXTENT, EXTENT], draw);
            } else {
                draw(list);
            }
        });
        painted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas_sdk::{Frame as CanvasFrame, Raster};

    #[test]
    fn one_turned_tile_grid_keeps_streets_aligned() {
        let frame = Frame(3392, 1600);
        let view = crate::world::View {
            lon: 21.2287,
            lat: 45.754,
            span: 0.029,
            roll: 35.,
        };
        let camera = Camera::new(view, frame);
        let key = super::super::visible(view, frame, 14)[0];
        let placement = Grid::new(&camera, view, frame, key.z)
            .unwrap()
            .place(key, Rc::default())
            .unwrap();
        for point in [(0, 0), (2048, 2048), (4096, 4096)] {
            let expected = key.project(4096., point, &camera).unwrap();
            let actual = placement.point(4096, point);
            assert!(
                (expected.0 - actual.0).hypot(expected.1 - actual.1) < 1.,
                "{expected:?} vs {actual:?}"
            );
        }
    }

    #[test]
    fn road_packing_keeps_turns_reversals_and_buffered_preview_clips() {
        let mut art = Packed::default();
        art.add(&Feature {
            kind: Layer::Road,
            extent: 4096,
            geometry: vec![
                Pen::Move((10, 10)),
                Pen::Line((20, 12)),
                Pen::Line((30, 14)),
                Pen::Line((30, 14)),
                Pen::Line((40, 14)),
                Pen::Line((35, 14)),
                Pen::Line((35, 21)),
            ],
            name: None,
        });
        let mut paths = art.paths(Layer::Road, Bounds([0., 0., EXTENT, EXTENT]));
        let path = paths.next().unwrap();
        assert!(paths.next().is_none());
        let points: Vec<_> = path
            .bytes()
            .chunks_exact(9)
            .map(|p| {
                (
                    p[0],
                    f32::from_le_bytes(p[1..5].try_into().unwrap()),
                    f32::from_le_bytes(p[5..9].try_into().unwrap()),
                )
            })
            .collect();
        assert_eq!(
            points,
            [
                (0, 10., 10.),
                (1, 30., 14.),
                (1, 40., 14.),
                (1, 35., 14.),
                (1, 35., 21.)
            ]
        );

        for (start, expected_clips) in [(0, 0), (-32, 1)] {
            let mut art = Packed::default();
            art.add(&Feature {
                kind: Layer::Road,
                extent: 4096,
                geometry: vec![Pen::Move((start, 1000)), Pen::Line((2000, 1200))],
                name: None,
            });
            let tile = Placement {
                art: Rc::new(art),
                visible: Bounds([0., 0., EXTENT, EXTENT]),
                affine: [0.01, 0.005, -0.005, 0.01, 32., 10.],
                unit: 0.01_f32.hypot(0.005),
            };
            let mut bytes = Vec::new();
            let mut list = List::begin(
                &mut bytes,
                CanvasFrame {
                    id: "map",
                    stream: 7,
                    seq: 1,
                    settled: 0,
                    raster: Raster::new([96_000, 96_000], 240).unwrap(),
                    palette: &[[0, 0, 0, 255]; 9],
                },
            );
            assert!(
                paint(&[tile], &mut list, 0.029, Detail::Pressure, 240),
                "keep the road"
            );
            let bytes = list.finish().unwrap();
            assert_eq!(crate::tests::drawn_tile_paths(bytes, 1, 8), expected_clips);
        }
    }

    #[test]
    fn dense_land_cannot_spend_the_building_and_street_budget() {
        let mut art = Packed::default();
        for i in 0..14_000 {
            let (x, y) = (80 + (i % 120) * 30, 80 + (i / 120) * 30);
            art.add(&Feature {
                kind: Layer::BuiltUp,
                extent: 4096,
                geometry: vec![
                    Pen::Move((x, y)),
                    Pen::Line((x + 10, y)),
                    Pen::Line((x + 10, y + 10)),
                    Pen::Line((x, y + 10)),
                    Pen::Close,
                ],
                name: None,
            });
        }
        let square = art
            .paths(Layer::BuiltUp, Bounds([0., 0., EXTENT, EXTENT]))
            .next()
            .unwrap();
        assert_eq!(square.len() % 5, 0, "complete rings only");
        art.add(&Feature {
            kind: Layer::Building,
            extent: 4096,
            geometry: vec![
                Pen::Move((1700, 1700)),
                Pen::Line((2200, 1700)),
                Pen::Line((2200, 2300)),
                Pen::Line((1700, 2300)),
                Pen::Close,
            ],
            name: None,
        });
        for kind in [Layer::LocalRoad, Layer::Road] {
            art.add(&Feature {
                kind,
                extent: 4096,
                geometry: vec![Pen::Move((80, 800)), Pen::Line((3700, 2400))],
                name: None,
            });
        }
        let tile = Placement {
            art: Rc::new(art),
            visible: Bounds([0., 0., EXTENT, EXTENT]),
            affine: [1., 0., 0., 1., 0., 0.],
            unit: 1.,
        };
        for detail in [Detail::Full, Detail::Gesture] {
            let mut bytes = Vec::new();
            let mut list = List::begin(
                &mut bytes,
                CanvasFrame {
                    id: "map",
                    stream: 7,
                    seq: 1,
                    settled: 0,
                    raster: Raster::new([3_392_000, 1_600_000], 1000).unwrap(),
                    palette: &[[0, 0, 0, 255]; 9],
                },
            );
            assert!(paint(
                std::slice::from_ref(&tile),
                &mut list,
                0.029,
                detail,
                1000
            ));
            let bytes = list.finish().expect("bounded mix of map layers");
            for (mode, color) in [(0, 3), (1, 5), (1, 8)] {
                assert!(
                    crate::tests::drawn_tile_paths(bytes, mode, color) > 0,
                    "lost fill/stroke {mode}/{color}"
                );
            }
            assert_eq!(
                crate::tests::drawn_tile_paths(bytes, 0, 5) > 0,
                detail == Detail::Full,
                "only the settled view needs the dense built-up tint"
            );
        }
    }

    #[test]
    fn closed_holes_share_one_path_and_the_native_crop_stops_at_the_tile() {
        let mut art = Packed::default();
        let make = |points| Feature {
            kind: Layer::Building,
            extent: 4096,
            geometry: points,
            name: None,
        };
        let shape = make(vec![
            Pen::Move((-512, -512)),
            Pen::Line((4608, -512)),
            Pen::Line((4608, 4608)),
            Pen::Line((-512, 4608)),
            Pen::Close,
            Pen::Move((1536, 1536)),
            Pen::Line((1536, 2560)),
            Pen::Line((2560, 2560)),
            Pen::Line((2560, 1536)),
            Pen::Close,
        ]);
        art.add(&shape);
        art.add(&make(vec![
            Pen::Move((1024, 1024)),
            Pen::Line((1024, 3072)),
            Pen::Move((4096, 4096)),
        ]));
        let visible = Bounds([0., 0., 4096., 4096.]);
        let paths: Vec<_> = art.paths(Layer::Building, visible).collect();
        assert_eq!(
            paths.len(),
            1,
            "an open ring must not be drawn or close a wedge"
        );
        assert_eq!(
            paths[0].len(),
            10,
            "outer contour and opposite winding hole are in one path"
        );
        let view = Placement {
            art: Rc::new(art),
            visible,
            affine: [1. / 64., 0., 0., 1. / 64., 16., 16.],
            unit: 1. / 64.,
        };
        let mut palette = [[0, 0, 0, 255]; 9];
        palette[0] = [13, 15, 17, 255];
        palette[1] = [101, 107, 113, 255];
        let mut bytes = Vec::new();
        let mut list = List::begin(
            &mut bytes,
            CanvasFrame {
                id: "map",
                stream: 7,
                seq: 1,
                settled: 0,
                raster: Raster::new([96000, 96000], 1000).unwrap(),
                palette: &palette,
            },
        );
        list.clear(Color::Surface);
        assert!(paint(&[view], &mut list, 0.029, Detail::Full, 1000));
        list.finish().unwrap();
        if let Some(out) = std::env::var_os("LOCATION_PARITY_OUT") {
            std::fs::write(
                std::path::PathBuf::from(out).join("display-hole.bin"),
                bytes,
            )
            .unwrap();
        }
    }
}
