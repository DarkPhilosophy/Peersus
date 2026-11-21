//! One Location viewport paints one whole frame. Only common page chrome stays declarative.
use super::{
    Elevation, ScaleBar, State, ZoomRail, atlas, geo, marker_badge_bounds, overlaps, tile, world,
};
use crate::{
    canvas_sdk::{Align, Color, List, Paint, Palette, Path, Raster},
    geometry,
    host::{Data, Handle, Wire},
};
use serde_json::Value;

pub const ID: &str = "map";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub bounds: (f64, f64, f64, f64),
    pub raster: Raster,
    pub palette: Palette,
}

impl Metrics {
    pub fn from_event(event: &Value) -> Option<Self> {
        let integer = |key: &str| u32::try_from(event[key].as_u64()?).ok();
        let logical = [integer("width")?, integer("height")?];
        let reported = integer("scale")?.min(16_000);
        // Usual viewports render pixel-exactly. An exceptionally large/high-DPI
        // viewport keeps its logical geometry but fits the helper's raster cap.
        let mut lo = 1;
        let mut hi = reported.checked_add(1)?;
        let mut raster = Raster::new(logical, lo)?;
        if let Some(exact) = Raster::new(logical, reported) {
            raster = exact;
        } else {
            while lo + 1 < hi {
                let mid = lo.midpoint(hi);
                if let Some(fit) = Raster::new(logical, mid) {
                    lo = mid;
                    raster = fit;
                } else {
                    hi = mid;
                }
            }
        }
        if reported == 0 {
            return None;
        }
        let [w, h] = logical.map(|n| f64::from(n) / 1000.);
        let coordinate = |key: &str| i32::try_from(event[key].as_i64()?).ok().map(f64::from);
        let mut palette = Palette::default();
        for (slot, name) in palette.iter_mut().zip([
            "surface", "raised", "text", "muted", "border", "scrim", "accent", "success", "danger",
        ]) {
            let rgba = event["palette"][name].as_array()?;
            if rgba.len() != 4 {
                return None;
            }
            for (channel, value) in slot.iter_mut().zip(rgba) {
                *channel = u8::try_from(value.as_u64()?).ok()?;
            }
        }
        Some(Self {
            bounds: (coordinate("x")?, coordinate("y")?, w, h),
            raster,
            palette,
        })
    }

    pub fn at_scale(mut self, scale: u32) -> Self {
        let (_, _, w, h) = self.bounds;
        let logical = [w, h].map(|n| (n * 1000.).round() as u32);
        if let Some(raster) = Raster::new(logical, scale) {
            self.raster = raster;
        }
        self
    }
}

/// The Wasm-only buffer import is separate from the ordinary data-stream SDK.
pub trait CanvasIo: Wire + Data {
    fn open_canvas(&mut self, claim: u64) -> Option<Handle>;
    fn present_canvas(&mut self, bytes: &[u8]) -> i32;
}

#[cfg(target_family = "wasm")]
impl CanvasIo for crate::host::Host {
    fn open_canvas(&mut self, claim: u64) -> Option<Handle> {
        crate::canvas_sdk::open(claim)
    }
    fn present_canvas(&mut self, bytes: &[u8]) -> i32 {
        crate::canvas_sdk::present(bytes)
    }
}

#[derive(Default)]
pub struct Surface {
    pub metrics: Option<Metrics>,
    pub opening: Option<u64>,
    pub stream: Option<Handle>,
    pub seq: u64,
    pub retry_at: u64,
    pub painted: Option<Stamp>,
    pub painted_at: u64,
    pub raster_pressure: Option<RasterPressure>,
    pub picture: Picture,
}

pub struct RasterPressure {
    pub scale: u32,
    pub full_refused: Option<Stamp>,
}

#[derive(Clone, Copy, PartialEq)]
pub struct Stamp {
    pub metrics: Metrics,
    pub view: world::View,
    pub settled: u64,
    pub markers: u64,
    pub online: Option<u64>,
    pub detail: tile::Detail,
}

#[derive(Default)]
pub struct Picture {
    pub markers: Vec<Marker>,
    pub online_drawn: bool,
    pub previewing_tiles: bool,
}

pub struct Marker {
    pub id: String,
    pub name: String,
    /// What its badge says of its position's age, under the name.
    pub last: Option<String>,
    pub fix: geo::Fix,
    pub point: (f64, f64),
    pub badge: Option<[f64; 4]>,
}

/// The marker of this device, among the names the application gives the paired ones.
pub const OWN: &str = "this-device";

impl Marker {
    pub fn dot(&self) -> [f64; 4] {
        let (x, y) = self.point;
        [x - 16., y - 16., 32., 32.]
    }
}

/// Markers closer than this, centre to centre, cover one another: a dot is 32 across.
const APART: f64 = 36.;

/// Where each marker is drawn, given where each one's position falls on the screen. Markers that would cover one another
/// are moved apart around the place they share, so two devices on one desk are two markers side by side and not one; a
/// marker with room stays where it is. Parting them never takes out of the map (`bounds`) markers that were all in it.
/// Only the drawing moves: what a marker is of, and the ring of how sure it is, do not.
fn spread(points: &[(f64, f64)], bounds: tile::layers::Rect) -> Vec<(f64, f64)> {
    let dot = |(x, y): (f64, f64)| [x - 16., y - 16., 32., 32.];
    let mut placed = points.to_vec();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, &(x, y)) in points.iter().enumerate() {
        let near = |group: &&mut Vec<usize>| {
            group
                .iter()
                .any(|&other| (x - points[other].0).hypot(y - points[other].1) < APART)
        };
        match groups.iter_mut().find(near) {
            Some(group) => group.push(index),
            None => groups.push(vec![index]),
        }
    }
    for group in groups.iter().filter(|group| group.len() > 1) {
        let count = group.len() as f64;
        let (cx, cy) = group.iter().fold((0., 0.), |(x, y), &index| {
            (x + points[index].0 / count, y + points[index].1 / count)
        });
        if let [first, second] = group[..] {
            // Two keep the direction they lie in, so they part smoothly as the map is brought closer; on the very same
            // spot they stand side by side.
            let (dx, dy) = (
                points[second].0 - points[first].0,
                points[second].1 - points[first].1,
            );
            let length = dx.hypot(dy);
            let (ux, uy) = if length > 0.5 {
                (dx / length, dy / length)
            } else {
                (1., 0.)
            };
            placed[first] = (cx - ux * APART / 2., cy - uy * APART / 2.);
            placed[second] = (cx + ux * APART / 2., cy + uy * APART / 2.);
        } else {
            // More stand evenly on a ring wide enough for each, the first at the top.
            let radius = APART / 2. / (std::f64::consts::PI / count).sin();
            for (turn, &index) in group.iter().enumerate() {
                let angle =
                    std::f64::consts::TAU * turn as f64 / count - std::f64::consts::FRAC_PI_2;
                placed[index] = (cx + radius * angle.cos(), cy + radius * angle.sin());
            }
        }
        if !group.iter().all(|&index| bounds.fits(dot(points[index]))) {
            continue;
        }
        // All were in the map: the group as a whole is brought back in, by as little as that takes.
        let [left, top, right, bottom] = bounds.0.map(f64::from);
        let (mut low, mut high) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
        for &index in group {
            let [x, y, width, height] = dot(placed[index]);
            low = (low.0.min(x), low.1.min(y));
            high = (high.0.max(x + width), high.1.max(y + height));
        }
        let back = |low: f64, high: f64, start: f64, end: f64| {
            (start - low).max(0.) - (high - end).max(0.)
        };
        let (dx, dy) = (
            back(low.0, high.0, left, right),
            back(low.1, high.1, top, bottom),
        );
        for &index in group {
            placed[index] = (placed[index].0 + dx, placed[index].1 + dy);
        }
    }
    placed
}

fn markers(
    state: &State,
    camera: &world::Camera,
    bounds: tile::layers::Rect,
    clear: impl Fn([f64; 4]) -> bool,
) -> Vec<Marker> {
    let own = state.own_now().map(|s| {
        let last = match &state.own {
            super::Own::Found(told) => state.last(told),
            _ => None,
        };
        (OWN, "This device".to_owned(), s.fix, last)
    });
    let (found, points): (Vec<_>, Vec<_>) = own
        .into_iter()
        .chain(state.seen.iter().map(|(id, told)| {
            (
                id.as_str(),
                state.name(id),
                told.shared.fix,
                state.last(told),
            )
        }))
        .filter_map(|found| {
            let point = camera.point(found.2.longitude, found.2.latitude)?;
            Some((found, point))
        })
        .unzip();
    let dot = |(x, y): (f64, f64)| [x - 16., y - 16., 32., 32.];
    let shown: Vec<_> = found
        .into_iter()
        .zip(spread(&points, bounds))
        .filter(|&(_, point)| bounds.fits(dot(point)) && clear(dot(point)))
        .collect();
    let dots: Vec<[f64; 4]> = shown.iter().map(|&(_, point)| dot(point)).collect();
    let mut markers: Vec<Marker> = Vec::with_capacity(shown.len());
    for (index, ((id, name, fix, last), (x, y))) in shown.into_iter().enumerate() {
        // A name stands clear of the other markers and of the names already placed, so neighbours are both read.
        let free = |area| {
            clear(area)
                && dots
                    .iter()
                    .enumerate()
                    .all(|(other, dot)| other == index || !overlaps(area, *dot))
                && markers
                    .iter()
                    .all(|marker| marker.badge.is_none_or(|placed| !overlaps(area, placed)))
        };
        // What is said of the age goes under the name where both have room; the name alone where only it has.
        let whole = last
            .as_deref()
            .and_then(|last| marker_badge_bounds(&name, Some(last), x, y, bounds, &free));
        let badge = whole.or_else(|| marker_badge_bounds(&name, None, x, y, bounds, &free));
        markers.push(Marker {
            id: id.to_owned(),
            name,
            last: last.filter(|_| whole.is_some()),
            fix,
            point: (x, y),
            badge,
        });
    }
    markers
}

/// Raw canvas hit testing uses the same geometry painted into the last frame.
pub enum Hit {
    Rail,
    Closer,
    Wider,
    Centre,
    Marker(String),
    Map,
}

pub fn hit(markers: &[Marker], frame: world::Frame, (x, y): (f64, f64)) -> Hit {
    if let Some(rail) = ZoomRail::new(frame) {
        if (f64::from(rail.left())..f64::from(frame.0)).contains(&x)
            && (0. ..f64::from(frame.1)).contains(&y)
        {
            let h = f64::from(ZoomRail::TARGET);
            return if y < h {
                Hit::Closer
            } else if y >= f64::from(frame.1) - h {
                Hit::Wider
            } else if x < rail.track_x() - 3.
                && (f64::from(rail.middle())..f64::from(rail.middle()) + h).contains(&y)
            {
                Hit::Centre
            } else {
                Hit::Rail
            };
        }
    }
    let contains = |[a, b, w, h]: [f64; 4]| (a..a + w).contains(&x) && (b..b + h).contains(&y);
    markers
        .iter()
        .rev()
        .find(|marker| contains(marker.dot()) || marker.badge.is_some_and(contains))
        .map_or(Hit::Map, |marker| Hit::Marker(marker.id.clone()))
}

pub fn render(state: &State, stamp: Stamp, advance: bool, list: &mut List<'_>) -> Picture {
    let mut cache = state.cache.borrow_mut();
    let view = stamp.view;
    let frame = state.frame();
    let layer = cache.prepare(view, frame, advance);
    let camera = world::Camera::new(view, frame);
    let rail = ZoomRail::new(frame);
    let scale = ScaleBar::new(frame, &camera);
    let bounds = super::map_label_bounds(frame);
    let readout = rail.map(|rail| rail.elevation(view, &camera));
    let scale_bounds = scale.as_ref().map(ScaleBar::bounds);
    let clear_chrome = |area| scale_bounds.is_none_or(|bounds| !overlaps(area, bounds));
    let markers = markers(state, &camera, bounds, clear_chrome);
    let clear = |area| {
        clear_chrome(area)
            && markers.iter().all(|m| {
                !overlaps(area, m.dot()) && m.badge.is_none_or(|bounds| !overlaps(area, bounds))
            })
    };
    let mut p = Painter {
        list,
        path: Path::default(),
        raised: stamp.metrics.palette[1],
    };
    p.list.clear(Color::Surface);
    if let Some((x, y, r)) = camera.disc() {
        p.circle(x, y, r);
        p.list.path(&p.path, Paint::Fill(Color::Raised, 255));
        p.list.path(&p.path, Paint::Stroke(Color::Border, 1., 255));
    }
    if let Some(path) = &layer.land {
        path.paint(p.list, Paint::Fill(Color::Border, 184));
        // The fill already defines coastlines; add their contrast stroke at rest.
        if view.span > 60. && stamp.detail != tile::Detail::Gesture {
            path.paint(p.list, Paint::Stroke(Color::Muted, 1.2, 184));
        }
    }
    layer
        .detail
        .borders
        .paint(p.list, Paint::Stroke(Color::Text, 2., 224));

    // Only a full replacement can take over. Moving the camera can reproject
    // the last accepted tiles over the complete offline base.
    let (online_drawn, online_labels, previewing_tiles) =
        state.online.with_drawing(view, frame, |detail| {
            let detail = detail
                .filter(|(_, complete)| state.online_enabled && *complete)
                .map(|(d, _)| d);
            let complete = detail.is_some_and(tile::Drawing::has_roads);
            let preview = if detail.is_none()
                && state.online_enabled
                && !state.clearing_kept
                && state.map_moving()
            {
                state.online.preview_drawing(view, frame, &camera)
            } else {
                None
            };
            if !complete || view.span > 1.25 {
                layer
                    .detail
                    .secondary_roads
                    .paint(p.list, Paint::Stroke(Color::Muted, 0.8, 255));
            }
            if !complete {
                layer
                    .detail
                    .roads
                    .paint(p.list, Paint::Stroke(Color::Accent, 1., 255));
            }
            let Some(detail) = detail.or(preview.as_ref()) else {
                return (false, Vec::new(), false);
            };
            (
                detail.paint(p.list, stamp.detail, stamp.metrics.raster.scale),
                detail
                    .labels
                    .iter()
                    .filter(|l| {
                        !(stamp.detail.preview() && l.street.is_some()) && clear(l.bounds())
                    })
                    .map(|l| (l.bounds(), l.name.clone(), l.street))
                    .collect::<Vec<_>>(),
                preview.is_some(),
            )
        });

    let mut countries = Vec::new();
    for country in &layer.detail.countries {
        let Some((area, size)) = country.place(bounds, true, |area| {
            clear(area) && countries.iter().all(|&other| !overlaps(area, other))
        }) else {
            continue;
        };
        let area = area.map(f64::from);
        countries.push(area);
        p.label(country.name, area, f32::from(size.unwrap_or(22)), None);
    }
    let label_clear = |area| clear(area) && countries.iter().all(|&other| !overlaps(area, other));
    let mut offline = Vec::new();
    let mut cities = 0;
    if online_labels.is_empty() {
        let city_limit = atlas::city_limit(view, frame);
        for (index, &(x, y, name)) in layer.detail.names.iter().enumerate() {
            let city = index >= layer.detail.first_city;
            if city && cities == city_limit {
                break;
            }
            let Some(label) =
                tile::Label::place(name, (x.into(), y.into()), bounds, &offline, label_clear)
            else {
                continue;
            };
            if city {
                p.circle(x.into(), y.into(), 2.5);
                p.list.path(&p.path, Paint::Fill(Color::Text, 255));
                cities += 1;
            }
            p.label(
                &label.name,
                label.bounds(),
                tile::Label::FONT,
                Some(p.label_backing()),
            );
            offline.push(label);
        }
    } else {
        for (area, name, street) in &online_labels {
            if label_clear(*area) {
                if let Some(street) = street {
                    p.street_label(name, *area, *street);
                } else {
                    p.label(name, *area, tile::Label::FONT, Some(p.label_backing()));
                }
            }
        }
    }

    for m in &markers {
        if let Some(ring) = m
            .fix
            .accuracy
            .and_then(|accuracy| camera.uncertainty(m.fix.longitude, m.fix.latitude, accuracy))
        {
            p.path.clear();
            // At maximum zoom geodesic accuracy can extend well outside the display-list coordinate range.
            if let Some(clipped) = world::clip_frame(ring.to_vec(), frame) {
                geometry::ring(&mut p.path, clipped);
            }
            p.list.path(&p.path, Paint::Fill(Color::Accent, 46));
            p.list.path(&p.path, Paint::Stroke(Color::Accent, 0.8, 46));
        }
    }
    for m in &markers {
        if let Some(badge) = m.badge {
            p.badge(&m.name, m.last.as_deref(), badge);
        }
        let (x, y) = m.point;
        p.circle(x, y, 15.);
        if m.id == OWN {
            // Where this device is: a filled dot with a centre, the sign maps use for "you are here". Initials of its
            // fixed words ("TD") would read as a code, and the badge beside it already says what it is.
            p.list.path(&p.path, Paint::Fill(Color::Accent, 255));
            p.list.path(&p.path, Paint::Stroke(Color::Raised, 2., 255));
            p.circle(x, y, 5.);
            p.list.path(&p.path, Paint::Fill(Color::Raised, 255));
            continue;
        }
        p.list.path(&p.path, Paint::Fill(Color::Raised, 255));
        p.list.path(&p.path, Paint::Stroke(Color::Accent, 2., 255));
        let initials: String = m
            .name
            .split_whitespace()
            .filter_map(|word| word.chars().next())
            .flat_map(char::to_uppercase)
            .take(2)
            .collect();
        p.list.text(
            &initials,
            14.,
            Color::Text,
            [x as f32, y as f32 + 5.],
            Align::Center,
        );
    }
    if let Some((rail, readout)) = rail.zip(readout.as_ref()) {
        p.rail(rail, view, readout);
    }
    if let Some(bar) = scale {
        p.scale_bar(&bar);
    }
    let online_drawn = online_drawn
        || online_labels
            .iter()
            .any(|(bounds, _, _)| label_clear(*bounds));
    Picture {
        markers,
        online_drawn,
        previewing_tiles,
    }
}

struct Painter<'a, 'b> {
    list: &'a mut List<'b>,
    path: Path,
    raised: [u8; 4],
}

impl Painter<'_, '_> {
    fn rect(&mut self, [x, y, w, h]: [f64; 4], color: Color) {
        let [x, y, w, h] = [x, y, w, h].map(|v| v as f32);
        self.polyline(&[[x, y], [x + w, y], [x + w, y + h], [x, y + h]], true);
        self.list.path(&self.path, Paint::Fill(color, 255));
    }

    fn polyline(&mut self, points: &[[f32; 2]], closed: bool) {
        self.path.clear();
        if let Some((first, rest)) = points.split_first() {
            self.path.move_to(*first);
            for &next in rest {
                self.path.line_to(next);
            }
            if closed {
                self.path.close();
            }
        }
    }

    fn circle(&mut self, x: f64, y: f64, r: f64) {
        let [x, y, r] = [x, y, r].map(|n| n as f32);
        let b = r * 0.552_284_8;
        self.path.clear();
        self.path.move_to([x + r, y]);
        self.path.cubic_to([x + r, y + b, x + b, y + r, x, y + r]);
        self.path.cubic_to([x - b, y + r, x - r, y + b, x - r, y]);
        self.path.cubic_to([x - r, y - b, x - b, y - r, x, y - r]);
        self.path.cubic_to([x + b, y - r, x + r, y - b, x + r, y]);
        self.path.close();
    }

    fn label(&mut self, text: &str, area: [f64; 4], size: f32, background: Option<Color>) {
        if let Some(color) = background {
            self.rect(area, color);
        }
        let [x, y, w, h] = area.map(|v| v as f32);
        self.list.clipped([x, y, w, h], |list| {
            list.text(
                text,
                size,
                Color::Text,
                [x + w / 2., y + h / 2. + size * 0.35],
                Align::Center,
            );
        });
    }

    /// A marker's name and, under it, what is said of its position's age.
    fn badge(&mut self, name: &str, last: Option<&str>, area: [f64; 4]) {
        let backing = self.label_backing();
        let Some(last) = last else {
            return self.label(name, area, 15., Some(backing));
        };
        self.rect(area, backing);
        let [x, y, w, h] = area.map(|v| v as f32);
        let status = super::STATUS_FONT as f32;
        self.list.clipped([x, y, w, h], |list| {
            let middle = x + w / 2.;
            list.text(name, 15., Color::Text, [middle, y + 18.25], Align::Center);
            list.text(
                last,
                status,
                Color::Text,
                [middle, y + 33. + status * 0.35],
                Align::Center,
            );
        });
    }

    fn label_backing(&self) -> Color {
        let mut rgba = self.raised;
        rgba[3] = 166;
        Color::Rgba(rgba)
    }

    fn street_label(&mut self, text: &str, area: [f64; 4], street: tile::StreetLabel) {
        let [x, y] = street.centre;
        let [cos, sin] = street.direction;
        let size = tile::Label::STREET_FONT;
        self.list.clipped(area.map(|n| n as f32), |list| {
            list.transformed([cos, sin, -sin, cos, x, y], |list| {
                let baseline = size * 0.35;
                for (dx, dy) in [(-1.2, 0.), (1.2, 0.), (0., -1.2), (0., 1.2)] {
                    list.text(
                        text,
                        size,
                        Color::Raised,
                        [dx, baseline + dy],
                        Align::Center,
                    );
                }
                list.text(text, size, Color::Text, [0., baseline], Align::Center);
            });
        });
    }

    fn rail(&mut self, rail: ZoomRail, view: world::View, readout: &Elevation) {
        let [x, y, width, height] = rail.bounds().map(|n| n as f32);
        let (right, bottom) = (x + width, y + height);
        let r = width / 2.;
        self.path.clear();
        self.path.move_to([x + r, y]);
        self.path.quad_to([right, y, right, y + r]);
        self.path.line_to([right, bottom - r]);
        self.path.quad_to([right, bottom, x + r, bottom]);
        self.path.quad_to([x, bottom, x, bottom - r]);
        self.path.line_to([x, y + r]);
        self.path.quad_to([x, y, x + r, y]);
        self.path.close();
        let mut rgba = self.raised;
        rgba[3] = 220;
        self.list
            .path(&self.path, Paint::Fill(Color::Rgba(rgba), 255));

        let button = f64::from(ZoomRail::TARGET);
        let mid = f64::from(rail.middle()) + button / 2.;
        let axis = rail.track_x() as f32;
        let [top, bottom] = rail.track().map(|n| n as f32);
        self.polyline(&[[axis, top], [axis, bottom]], false);
        let ticks = ((bottom - top) / 20.).clamp(1., 12.) as u8;
        for i in 0..=ticks {
            let y = top + (bottom - top) * f32::from(i) / f32::from(ticks);
            if (f64::from(y) - mid).abs() < 7. {
                continue;
            }
            self.path
                .move_to([axis - if i % 2 == 0 { 4. } else { 2. }, y]);
            self.path.line_to([axis, y]);
        }
        self.list
            .path(&self.path, Paint::Stroke(Color::Muted, 1., 168));
        let marker = rail.marker(view) as f32;
        self.polyline(&[[axis - 2.25, marker], [axis + 2.25, marker]], false);
        self.list
            .path(&self.path, Paint::Stroke(Color::Accent, 2.5, 255));
        let [x, y, w, h] = rail.bounds();
        let inset = f64::from(ZoomRail::EDGE);
        for (label, top) in [("+", y), ("−", y + h - button + inset)] {
            self.label(label, [x, top, w, button - inset], 21., None);
        }
        let centre_x = rail.centre_x();
        self.circle(centre_x, mid, 5.5);
        self.list
            .path(&self.path, Paint::Stroke(Color::Text, 1.5, 255));
        let (s, c) = view.bearing().to_radians().sin_cos();
        let arrow = |along, side| {
            [
                (centre_x + along * s + side * c) as f32,
                (mid - along * c + side * s) as f32,
            ]
        };
        self.polyline(&[arrow(4., 0.), arrow(-2.5, 2.5), arrow(-2.5, -2.5)], true);
        self.list.path(&self.path, Paint::Fill(Color::Accent, 255));

        let [x, y, w, h] = readout.bounds.map(|n| n as f32);
        self.list.clipped([x, y, w, h], |list| {
            list.transformed([0., -1., 1., 0., x + w / 2., y + h / 2.], |list| {
                list.text(
                    &readout.text,
                    readout.font,
                    Color::Text,
                    [0., readout.font * 0.35],
                    Align::Center,
                );
            });
        });
    }

    fn scale_bar(&mut self, bar: &ScaleBar) {
        let area = bar.bounds();
        let [x, y, w, _] = area.map(|n| n as f32);
        self.rect(area, Color::Raised);
        let mid = x + w / 2.;
        self.list.text(
            &bar.caption(),
            15.,
            Color::Text,
            [mid, y + 17.],
            Align::Center,
        );
        let left = mid - bar.length as f32 / 2.;
        let right = mid + bar.length as f32 / 2.;
        self.polyline(
            &[
                [left, y + 25.],
                [left, y + 31.],
                [right, y + 31.],
                [right, y + 25.],
            ],
            false,
        );
        self.path.move_to([mid, y + 28.]);
        self.path.line_to([mid, y + 31.]);
        self.list
            .path(&self.path, Paint::Stroke(Color::Text, 1.4, 255));
    }
}
