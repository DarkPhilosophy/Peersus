//! Indexed Natural Earth layers. No network, file lookup or device coordinates are needed.
use crate::geometry::Paths;
use crate::tile::layers::Rect;
use crate::world::{Camera, Frame, View};
const LINES: &[u8] = include_bytes!("world/lines.bin");
const HEAD: usize = 4 * (72 * 36 + 1);
// Source order is by descending population; local labels must leave other labels readable.
const CITIES: &[u8] = include_bytes!("world/places.bin");

pub const COUNTRY_SPAN: f64 = 30.;

pub fn city_limit(view: View, viewport: Frame) -> usize {
    let pixels = usize::from(viewport.0) * usize::from(viewport.1);
    if view.span <= COUNTRY_SPAN {
        (pixels / 30_000).clamp(7, 24)
    } else {
        (pixels / 80_000).clamp(3, 12)
    }
}
/// Count candidates as well as projections, so clipping a dense extract is bounded too.
pub struct Work {
    scan: usize,
    project: usize,
}

impl Default for Work {
    fn default() -> Self {
        Self {
            scan: 2400,
            project: 480,
        }
    }
}

impl Work {
    /// Share a turn with the initial land and border projection.
    pub fn initial() -> Self {
        Self {
            scan: 900,
            project: 160,
        }
    }

    pub fn scan(&mut self) -> bool {
        if let Some(left) = self.scan.checked_sub(1) {
            self.scan = left;
            true
        } else {
            false
        }
    }

    pub fn project(&mut self) -> bool {
        if let Some(left) = self.project.checked_sub(1) {
            self.project = left;
            true
        } else {
            false
        }
    }
}

pub enum Progress {
    Street {
        local: Option<crate::streets::Progress>,
        regional: Regional,
    },
    Regional(Regional),
}

impl Progress {
    pub fn draw(
        &mut self,
        view: View,
        frame: Frame,
        viewport: Frame,
        camera: &Camera,
        detail: &mut Detail,
        work: &mut Work,
    ) -> bool {
        let done = match self {
            Self::Street { local, regional } => {
                if let Some(progress) = local {
                    if !crate::streets::draw(camera, view.span, frame, detail, progress, work) {
                        return false;
                    }
                    *local = None;
                }
                // Local extracts take precedence, but a city zoom without an extract
                // still projects the roads in the continental atlas at this camera.
                !detail.roads.is_empty() || regional.draw(view, frame, camera, detail, work)
            }
            Self::Regional(regional) => regional.draw(view, frame, camera, detail, work),
        };
        if done {
            city_labels(view, viewport, frame, camera, detail);
        }
        done
    }
}

#[derive(Default)]
pub struct Regional {
    cell: usize,
    segment: usize,
    limits: [usize; 2],
}
#[derive(Default)]
pub struct Detail {
    pub borders: Paths,
    pub roads: Paths,
    pub secondary_roads: Paths,
    pub names: Vec<(i32, i32, &'static str)>,
    pub countries: Vec<Country>,
    pub first_city: usize,
}

pub struct Country {
    pub name: &'static str,
    x: f64,
    y: f64,
    extent: f64,
}

impl Country {
    pub fn place(
        &self,
        bounds: Rect,
        sized: bool,
        clear: impl Fn([f64; 4]) -> bool,
    ) -> Option<([i32; 4], Option<u8>)> {
        let [x0, y0, x1, y1] = bounds.0.map(f64::from);
        let size = sized.then(|| (self.extent / 7.).round().clamp(8., 42.) as u8);
        let px = f64::from(size.unwrap_or(22));
        // Keep a whole label or omit it; a narrower box would cut its glyph run.
        let width = crate::tile::text_width(self.name, px) + 6.;
        let height = (px * 1.5).ceil() + 2.;
        if width + 4. > x1 - x0 || height + 4. > y1 - y0 {
            return None;
        }
        let left = (self.x - width / 2.)
            .round()
            .clamp(x0 + 2., x1 - width - 2.);
        let top = (self.y - height / 2.)
            .round()
            .clamp(y0 + 2., y1 - height - 2.);
        let nudge = (self.extent / 4.).round().clamp(8., height + 4.);
        for (dx, dy) in [
            (0., 0.),
            (0., -nudge),
            (0., nudge),
            (-nudge, 0.),
            (nudge, 0.),
        ] {
            let x = (left + dx).clamp(x0 + 2., x1 - width - 2.);
            let y = (top + dy).clamp(y0 + 2., y1 - height - 2.);
            if clear([x, y, width, height]) {
                return Some(([x as i32, y as i32, width as i32, height as i32], size));
            }
        }
        None
    }
}

fn countries(v: View, f: Frame, camera: &Camera) -> Vec<Country> {
    let mut records = include_bytes!("world/countries.bin").as_slice();
    let mut labels = Vec::new();
    while let Some(header) = records.get(..7) {
        let lon = f64::from(i16::from_le_bytes([header[0], header[1]])) / 100.;
        let lat = f64::from(i16::from_le_bytes([header[2], header[3]])) / 100.;
        let angular = f64::from(u16::from_le_bytes([header[4], header[5]])) / 100.;
        let (name, rest) = records[7..].split_at(usize::from(header[6]));
        records = rest;
        let Some(((x, y), scale)) = camera.label_anchor(lon, lat) else {
            continue;
        };
        // Square root of the country area, in projected pixels. This discounts
        // foreshortening near the globe's limb and long, narrow landmasses.
        let extent = angular.to_radians() * scale;
        // At continent scale a large country can occupy part of the view even
        // when its label anchor is just outside. Shift it in instead of clipping it.
        let [rx, ry] = if v.span > COUNTRY_SPAN {
            [f64::from(f.0) / 4., f64::from(f.1) / 4.]
        } else {
            [0., 0.]
        };
        if extent < 6.
            || !(-rx..=f64::from(f.0) + rx).contains(&x)
            || !(-ry..=f64::from(f.1) + ry).contains(&y)
        {
            continue;
        }
        let name = std::str::from_utf8(name).expect("embedded country name");
        labels.push(Country { name, x, y, extent });
    }
    labels.sort_unstable_by(|a, b| {
        b.extent
            .total_cmp(&a.extent)
            .then_with(|| a.name.cmp(b.name))
    });
    labels
}
fn offset(i: usize) -> usize {
    u32::from_le_bytes(
        LINES[4 * i..4 * i + 4]
            .try_into()
            .expect("embedded atlas index"),
    ) as usize
}
fn axis(a: f64, b: f64, base: f64, max: i32) -> std::ops::RangeInclusive<i32> {
    (((a + base) / 5.).floor() as i32).clamp(0, max)
        ..=(((b + base) / 5.).floor() as i32).clamp(0, max)
}
#[cfg(test)]
pub fn project(v: View, f: Frame) -> Detail {
    let camera = Camera::new(v, f);
    let (mut detail, progress) = base(v, f, f, &camera);
    if let Some(mut progress) = progress {
        while !progress.draw(v, f, f, &camera, &mut detail, &mut Work::default()) {}
    }
    detail
}

pub fn base(v: View, viewport: Frame, f: Frame, cam: &Camera) -> (Detail, Option<Progress>) {
    let mut d = Detail {
        countries: countries(v, f, cam),
        borders: crate::borders::path(cam, f, v.span),
        ..Detail::default()
    };
    let work = if v.span <= 60. {
        if v.span < 0.4 {
            Some(Progress::Street {
                local: Some(crate::streets::Progress::default()),
                regional: Regional::default(),
            })
        } else if v.span <= crate::world::REGIONAL_SPAN {
            Some(Progress::Regional(Regional::default()))
        } else {
            city_labels(v, viewport, f, cam, &mut d);
            None
        }
    } else {
        None
    };
    (d, work)
}

fn city_labels(view: View, viewport: Frame, canvas: Frame, cam: &Camera, d: &mut Detail) {
    let area = Camera::new(view, viewport).visible_area();
    let bounds = Rect::viewport(viewport, canvas);
    d.first_city = d.names.len();
    // Keep more ranked candidates than the page can show: marker/country collisions
    // and the control rail can reject a candidate, but should not consume its slot.
    let limit = 3 * city_limit(view, viewport);
    let mut records = CITIES;
    while let Some(header) = records.get(..5) {
        let lon = f64::from(i16::from_le_bytes([header[0], header[1]])) / 100.;
        let lat = f64::from(i16::from_le_bytes([header[2], header[3]])) / 100.;
        let Some((raw, rest)) = records[5..].split_at_checked(usize::from(header[4])) else {
            break;
        };
        records = rest;
        if !area.contains(lon, lat) {
            continue;
        }
        let Some((x, y)) = cam.point(lon, lat).filter(|&(x, y)| bounds.contains(x, y)) else {
            continue;
        };
        if let Ok(name) = std::str::from_utf8(raw) {
            d.names.push((x.round() as i32, y.round() as i32, name));
        }
        if d.names.len() - d.first_city >= limit {
            break;
        }
    }
}

impl Regional {
    fn draw(&mut self, v: View, f: Frame, cam: &Camera, d: &mut Detail, work: &mut Work) -> bool {
        let area = cam.visible_area();
        let (bottom, top) = (area.south, area.north);
        let mut next_cell = 0;
        let regional = !(0.4..1.).contains(&v.span);
        let xy = |lo, la| cam.point(lo, la);
        for (west, east) in area.longitude_ranges() {
            let count = axis(bottom, top, 90., 35).count() * axis(west, east, 180., 71).count();
            for y in axis(bottom, top, 90., 35) {
                for x in axis(west, east, 180., 71) {
                    let current = next_cell;
                    next_cell += 1;
                    if current < self.cell {
                        continue;
                    }
                    let cell = (y * 72 + x) as usize;
                    if self.segment == 0 {
                        self.limits = [
                            (d.roads.len() + 850 / count.max(1)).min(850),
                            (d.secondary_roads.len() + 650 / count.max(1)).min(650),
                        ];
                    }
                    for seg in LINES[HEAD + offset(cell)..HEAD + offset(cell + 1)]
                        .chunks_exact(9)
                        .skip(self.segment)
                    {
                        if !work.scan() {
                            return false;
                        }
                        self.segment += 1;
                        let rank = seg[0];
                        if rank == 0 {
                            continue;
                        }
                        if !regional && rank > 7 {
                            break;
                        }
                        let secondary = regional && rank > 4;
                        let (path, until) = if secondary {
                            (&mut d.secondary_roads, self.limits[1])
                        } else {
                            (&mut d.roads, self.limits[0])
                        };
                        if path.len() + 2 > until {
                            if secondary || !regional {
                                break;
                            }
                            continue;
                        }
                        let coord = |i| i16::from_le_bytes([seg[i], seg[i + 1]]) as f64 / 100.;
                        let (ax, ay, bx, by) = (coord(1), coord(3), coord(5), coord(7));
                        if ax.max(bx) < west
                            || ax.min(bx) > east
                            || ay.max(by) < bottom
                            || ay.min(by) > top
                        {
                            continue;
                        }
                        // The record is repeated in adjacent cells at a boundary; only the first visible one owns it.
                        if x != *axis(ax.min(bx).max(west), east, 180., 71).start()
                            || y != *axis(ay.min(by).max(bottom), top, 90., 35).start()
                        {
                            continue;
                        }
                        if !work.project() {
                            self.segment -= 1;
                            return false;
                        }
                        let Some(a) = xy(ax, ay) else { continue };
                        let Some(b) = xy(bx, by) else { continue };
                        let Some((start, end)) = crate::borders::clipped(a, b, f) else {
                            continue;
                        };
                        if start == end {
                            continue;
                        }
                        path.line(start, end);
                    }
                    self.cell += 1;
                    self.segment = 0;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resuming_a_projection_keeps_the_same_streets_as_one_large_turn() {
        for (lon, lat, span) in [(26.1025, 44.4268, 0.038), (21.23, 45.75, 5.)] {
            let view = View {
                lon,
                lat,
                span,
                roll: 0.,
            };
            let frame = Frame(1920, 1000);
            let camera = Camera::new(view, frame);
            let (mut expected, one) = base(view, frame, frame, &camera);
            let ample = &mut Work {
                scan: usize::MAX,
                project: usize::MAX,
            };
            assert!(
                one.unwrap()
                    .draw(view, frame, frame, &camera, &mut expected, ample)
            );
            let (mut actual, split) = base(view, frame, frame, &camera);
            let mut split = split.unwrap();
            let mut finished = false;
            for turn in 0..512 {
                let small = &mut Work {
                    scan: 160,
                    project: 32,
                };
                if split.draw(view, frame, frame, &camera, &mut actual, small) {
                    assert!(turn > 0, "fixture did not cross a turn");
                    finished = true;
                    break;
                }
            }
            assert!(finished);
            assert_eq!(actual.roads, expected.roads);
            assert_eq!(actual.secondary_roads, expected.secondary_roads);
            assert_eq!(actual.names, expected.names);
        }
    }
}
