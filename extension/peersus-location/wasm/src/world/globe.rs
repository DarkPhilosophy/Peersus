//! Orthographic globe at every zoom level.
//! No GPU context or imagery service: coastlines and device pins use this same camera.
use super::{Frame, View, wrap};

type Vec3 = (f64, f64, f64);

fn dot(a: Vec3, b: Vec3) -> f64 {
    a.0 * b.0 + a.1 * b.1 + a.2 * b.2
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    (
        a.1 * b.2 - a.2 * b.1,
        a.2 * b.0 - a.0 * b.2,
        a.0 * b.1 - a.1 * b.0,
    )
}

/// The local projection uses small angles on every street. Within this interval the
/// polynomial differs from sin/cos by less than 1e-13 and avoids repeated libm range reduction.
fn small_sin_cos(angle: f64) -> (f64, f64) {
    let x = angle * angle;
    (
        angle * (1. - x * (1. / 6. - x * (1. / 120. - x / 5040.))),
        1. - x * (0.5 - x * (1. / 24. - x * (1. / 720. - x / 40_320.))),
    )
}

/// A conservative geographic envelope for the pixels inside the frame.
pub(crate) struct VisibleArea {
    pub(crate) south: f64,
    pub(crate) north: f64,
    pub(crate) longitude: f64,
    pub(crate) half_longitude: f64,
}

impl VisibleArea {
    pub(crate) fn contains(&self, lon: f64, lat: f64) -> bool {
        (self.south..=self.north).contains(&lat)
            && wrap(lon - self.longitude).abs() <= self.half_longitude
    }
    /// The envelope split at the antimeridian for indexes in [-180°, 180°].
    pub(crate) fn longitude_ranges(&self) -> impl Iterator<Item = (f64, f64)> {
        let (west, east) = (
            self.longitude - self.half_longitude,
            self.longitude + self.half_longitude,
        );
        let ranges = if self.half_longitude == 180. {
            [Some((-180., 180.)), None]
        } else if west < -180. {
            [Some((-180., east)), Some((west + 360., 180.))]
        } else if east > 180. {
            [Some((west, 180.)), Some((-180., east - 360.))]
        } else {
            [Some((west, east)), None]
        };
        ranges.into_iter().flatten()
    }
}

pub(crate) struct Camera {
    view: View,
    frame: Frame,
    sin_lat: f64,
    cos_lat: f64,
    radius: f64,
    sin_lon: f64,
    cos_lon: f64,
    roll: Option<(f64, f64)>,
}
impl Camera {
    pub(crate) fn new(view: View, frame: Frame) -> Self {
        Self::on_canvas(view, frame, frame)
    }

    /// Centre the same camera on a larger canvas. Only the crop changes, not its zoom.
    pub(crate) fn on_canvas(view: View, viewport: Frame, frame: Frame) -> Self {
        let (sin_lat, cos_lat) = view.lat.to_radians().sin_cos();
        let (sin_lon, cos_lon) = view.lon.to_radians().sin_cos();
        Self {
            view,
            frame,
            sin_lat,
            cos_lat,
            sin_lon,
            cos_lon,
            radius: 0.44 * f64::from(viewport.0.min(viewport.1)) * 360. / view.span,
            roll: (view.roll != 0.).then(|| view.roll.to_radians().sin_cos()),
        }
    }

    /// Equivalent perspective elevation for a nadir camera with a 60° field of view
    /// on the shorter axis, matching this orthographic camera's scale at the centre.
    pub(crate) fn elevation(&self) -> f64 {
        const TAN_HALF_FOV: f64 = 0.577_350_269_189_625_8;
        let half_shorter_axis = 0.5 * f64::from(self.frame.0.min(self.frame.1));
        crate::geo::EARTH_RADIUS_M * half_shorter_axis / (self.radius * TAN_HALF_FOV)
    }

    /// A horizontal ruler represents the great-circle arc through the centre of the view.
    pub(crate) fn ground_distance(&self, pixels: f64) -> f64 {
        2. * crate::geo::EARTH_RADIUS_M * (pixels / (2. * self.radius)).min(1.).asin()
    }

    pub(crate) fn ground_pixels(&self, metres: f64) -> f64 {
        2. * self.radius * (metres / (2. * crate::geo::EARTH_RADIUS_M)).sin()
    }

    pub(crate) fn vector(&self, lon: f64, lat: f64) -> Vec3 {
        let latitude = (lat - self.view.lat).to_radians();
        let longitude = (lon - self.view.lon).to_radians();
        if latitude.abs() <= 0.08 && longitude.abs() <= 0.08 {
            let (s, c) = small_sin_cos(latitude);
            let (e, d) = small_sin_cos(longitude);
            let cos_lat = self.cos_lat * c - self.sin_lat * s;
            let away = cos_lat * (1. - d);
            return (
                cos_lat * e,
                s + self.sin_lat * away,
                c - self.cos_lat * away,
            );
        }
        let (s, c) = lat.to_radians().sin_cos();
        self.vector_with_lat(lon, s, c)
    }
    fn vector_with_lat(&self, lon: f64, s: f64, c: f64) -> Vec3 {
        let (e, d) = (lon - self.view.lon).to_radians().sin_cos();
        (
            c * e,
            self.cos_lat * s - self.sin_lat * c * d,
            self.sin_lat * s + self.cos_lat * c * d,
        )
    }
    fn geographic(&self, (x, y, z): Vec3) -> (f64, f64) {
        (
            wrap(self.view.lon + x.atan2(self.cos_lat * z - self.sin_lat * y).to_degrees()),
            (self.cos_lat * y + self.sin_lat * z)
                .clamp(-1., 1.)
                .asin()
                .to_degrees(),
        )
    }
    fn screen(&self, (x, y, _): Vec3) -> (f64, f64) {
        let (x, y) = self
            .roll
            .map_or((x, y), |(s, c)| (c * x + s * y, c * y - s * x));
        (
            f64::from(self.frame.0) / 2. + self.radius * x,
            f64::from(self.frame.1) / 2. - self.radius * y,
        )
    }
    pub(crate) fn point(&self, lon: f64, lat: f64) -> Option<(f64, f64)> {
        let v = self.vector(lon, lat);
        (v.2 >= 0.).then(|| self.screen(v))
    }
    /// The anchor and local area scale of the visible orthographic projection.
    pub(crate) fn label_anchor(&self, lon: f64, lat: f64) -> Option<((f64, f64), f64)> {
        let v = self.vector(lon, lat);
        (v.2 >= 0.).then(|| (self.screen(v), self.radius * v.2.sqrt()))
    }

    /// An open line stops at the limb. Unlike a filled ring, it never traces
    /// the arc along the edge of the globe.
    pub(crate) fn visible_edge(&self, a: Vec3, b: Vec3) -> Option<((f64, f64), (f64, f64))> {
        Some(match (a.2 >= 0., b.2 >= 0.) {
            (true, true) => (self.screen(a), self.screen(b)),
            (false, false) => return None,
            (a_visible, _) => {
                let t = a.2 / (a.2 - b.2);
                let (x, y) = (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
                let length = x.hypot(y);
                if length == 0. {
                    return None;
                }
                let rim = self.screen((x / length, y / length, 0.));
                if a_visible {
                    (self.screen(a), rim)
                } else {
                    (rim, self.screen(b))
                }
            }
        })
    }
    /// Web Mercator supplies `asinh(tan(latitude))`. Evaluate the same
    /// orthographic projection without inverse trig for each tile vertex.
    pub(crate) fn mercator_point(&self, lon: f64, mercator: f64) -> Option<(f64, f64)> {
        let e = mercator.exp();
        let sq = e * e;
        let denom = sq + 1.;
        let v = self.vector_with_lat(lon, (sq - 1.) / denom, 2. * e / denom);
        (v.2 >= 0.).then(|| self.screen(v))
    }
    /// A geodesic uncertainty circle, not a fixed halo under the avatar.
    pub(crate) fn uncertainty(&self, lon: f64, lat: f64, metres: f64) -> Option<[(f64, f64); 24]> {
        let (sd, cd) = (metres / crate::geo::EARTH_RADIUS_M).sin_cos();
        let (sl, cl) = lat.to_radians().sin_cos();
        let (cx, cy) = self.point(lon, lat)?;
        let mut path = [(0., 0.); 24];
        let mut reach = 0.0_f64;
        for (n, vertex) in path.iter_mut().enumerate() {
            let (s, c) = ((n as f64) * std::f64::consts::TAU / 24.).sin_cos();
            let sin_lat = (sl * cd + cl * sd * c).clamp(-1., 1.);
            let east = (s * sd * cl).atan2(cd - sl * sin_lat).to_degrees();
            let (x, y) = self.point(lon + east, sin_lat.asin().to_degrees())?;
            reach = reach.max((cx - x).hypot(cy - y));
            if n == 0 && reach < 0.4 {
                return None;
            }
            *vertex = (x, y);
        }
        (reach >= 1.).then_some(path)
    }
    /// Inverse of the visible hemisphere's orthographic projection.
    pub(crate) fn locate(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (east, north) = self.screen_offset(x, y);
        let z = 1. - east * east - north * north;
        (z >= 0.).then(|| self.geographic(self.unroll((east, north, z.sqrt()))))
    }

    fn screen_offset(&self, x: f64, y: f64) -> (f64, f64) {
        (
            (x - f64::from(self.frame.0) / 2.) / self.radius,
            (f64::from(self.frame.1) / 2. - y) / self.radius,
        )
    }

    fn unroll(&self, (x, y, z): Vec3) -> Vec3 {
        self.roll
            .map_or((x, y, z), |(s, c)| (c * x - s * y, c * y + s * x, z))
    }

    fn in_world(&self, v: Vec3) -> Vec3 {
        let (x, y, z) = self.unroll(v);
        let meridian = self.cos_lat * z - self.sin_lat * y;
        (
            self.cos_lon * meridian - self.sin_lon * x,
            self.sin_lon * meridian + self.cos_lon * x,
            self.sin_lat * z + self.cos_lat * y,
        )
    }

    /// The shortest 3D rotation from the picked point on the sphere to the new one.
    /// A point outside the sphere projects to its nearest point on the limb.
    pub(crate) fn trackball(&self, from: (f64, f64), to: (f64, f64)) -> View {
        let ball = |(x, y)| {
            let (x, y) = self.screen_offset(x, y);
            let r = x.hypot(y);
            if r > 1. {
                (x / r, y / r, 0.)
            } else {
                (x, y, (1. - r * r).sqrt())
            }
        };
        let a = ball(from);
        let b = ball(to);
        let cosine = dot(a, b).clamp(-1., 1.);
        let axis = cross(a, b);
        let sine = dot(axis, axis).sqrt();
        let axis = if sine > 1e-12 {
            (axis.0 / sine, axis.1 / sine, axis.2 / sine)
        } else if cosine < 0. {
            // Opposite points on the limb have no unique axis. Rotate about the tangent.
            cross(a, (0., 0., 1.))
        } else {
            return self.view;
        };
        let inverse = |v: Vec3| {
            let (x, y, z) = cross(axis, v);
            let k = dot(axis, v) * (1. - cosine);
            self.in_world((
                v.0 * cosine - x * sine + axis.0 * k,
                v.1 * cosine - y * sine + axis.1 * k,
                v.2 * cosine - z * sine + axis.2 * k,
            ))
        };
        let (x, y, z) = inverse((0., 0., 1.));
        let lon = y.atan2(x);
        let lat = z.atan2(x.hypot(y));
        let (sl, cl) = lon.sin_cos();
        let (sp, cp) = lat.sin_cos();
        let right = inverse((1., 0., 0.));
        let roll = dot(right, (-sp * cl, -sp * sl, cp)).atan2(dot(right, (-sl, cl, 0.)));
        View {
            lon: lon.to_degrees(),
            lat: lat.to_degrees(),
            roll: roll.to_degrees(),
            ..self.view
        }
        .kept()
    }

    pub(crate) fn visible_area(&self) -> VisibleArea {
        let radius_in_frame = f64::from(self.frame.0).hypot(f64::from(self.frame.1)) / 2.;
        let angle = (radius_in_frame / self.radius).min(1.).asin();
        let (longitude, latitude) = self.geographic((0., 0., 1.));
        let latitude_radius = angle.to_degrees();
        let south = (latitude - latitude_radius).max(-90.);
        let north = (latitude + latitude_radius).min(90.);
        let half_longitude = if south == -90. || north == 90. {
            180.
        } else {
            (angle.sin() / latitude.to_radians().cos())
                .min(1.)
                .asin()
                .to_degrees()
        };
        VisibleArea {
            south,
            north,
            longitude,
            half_longitude,
        }
    }

    /// Rotate about the camera's horizontal axis rather than stopping at a geographical
    /// pole. Horizontal drag spins the meridian; screen east reverses after a half turn.
    pub(crate) fn orbit(&self, dx: f64, dy: f64) -> View {
        let degrees = 1_f64.to_degrees() / self.radius;
        let (dx, dy, _) = self.unroll((dx, -dy, 0.));
        View {
            lon: self.view.lon - dx * degrees * self.cos_lat.signum(),
            lat: self.view.lat - dy * degrees,
            ..self.view
        }
        .kept()
    }
    /// The view that puts `place` at `pixel`, or the view itself when there is none (zoom keeps its scale change).
    pub(crate) fn anchor(view: View, frame: Frame, place: (f64, f64), pixel: (f64, f64)) -> View {
        Self::grab(view, frame, place, pixel).unwrap_or(view)
    }

    /// The view that puts `place` at `pixel`; `None` when no nearby view does (a place over the limb, or one a pole keeps
    /// from turning there), so that a drag can rotate the globe instead of standing still.
    pub(crate) fn grab(
        view: View,
        frame: Frame,
        place: (f64, f64),
        pixel: (f64, f64),
    ) -> Option<View> {
        Self::solve((view.lon, view.lat), pixel, |lon, lat| {
            Self::new(View { lon, lat, ..view }, frame).point(place.0, place.1)
        })
        .map(|(lon, lat)| View { lon, lat, ..view }.kept())
    }

    fn solve(
        mut pos: (f64, f64),
        target: (f64, f64),
        sample: impl Fn(f64, f64) -> Option<(f64, f64)>,
    ) -> Option<(f64, f64)> {
        const H: f64 = 0.0001;
        for _ in 0..8 {
            let (x, y) = sample(pos.0, pos.1)?;
            let (dx, dy) = (target.0 - x, target.1 - y);
            if dx.hypot(dy) < 0.02 {
                return Some(pos);
            }
            let a = sample(pos.0 + H, pos.1)?;
            let b = sample(pos.0, pos.1 + H)?;
            let (ax, ay, bx, by) = (a.0 - x, a.1 - y, b.0 - x, b.1 - y);
            let det = ax * by - ay * bx;
            if det.abs() < 1e-15 {
                return None;
            }
            pos.0 = wrap(pos.0 + (H * (dx * by - dy * bx) / det).clamp(-40., 40.));
            pos.1 = wrap(pos.1 + (H * (ax * dy - ay * dx) / det).clamp(-40., 40.));
        }
        None
    }
    /// The visible part of a closed ring. A run on the far side is replaced by an arc of the limb,
    /// not by a chord across the visible land. Ring segments are short in the cartographic source.
    pub(super) fn ring(&self, ring: &[(f32, f32, f32)]) -> Vec<(f64, f64)> {
        let vector = |(x, y, z): (f32, f32, f32)| {
            let (x, y, z) = (f64::from(x), f64::from(y), f64::from(z));
            let front = x * self.cos_lon + y * self.sin_lon;
            (
                y * self.cos_lon - x * self.sin_lon,
                self.cos_lat * z - self.sin_lat * front,
                self.sin_lat * z + self.cos_lat * front,
            )
        };
        let Some(first) = ring.iter().position(|&point| vector(point).2 >= 0.) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut exit: Option<f64> = None;
        let mut a = vector(ring[first]);
        for k in 1..=ring.len() {
            let b = vector(ring[(first + k) % ring.len()]);
            if (a.2 >= 0.) != (b.2 >= 0.) {
                let t = a.2 / (a.2 - b.2);
                let angle = (a.1 + t * (b.1 - a.1)).atan2(a.0 + t * (b.0 - a.0));
                let rim = |theta: f64| {
                    let (y, x) = theta.sin_cos();
                    let v = (x, y, 0.);
                    self.screen(v)
                };
                if let Some(start) = exit.take() {
                    let delta = wrap((angle - start).to_degrees()).to_radians();
                    let steps = (delta.abs() / 0.07).ceil().max(1.) as usize;
                    for i in 1..steps {
                        out.push(rim(start + delta * i as f64 / steps as f64));
                    }
                } else {
                    exit = Some(angle);
                }
                out.push(rim(angle));
            }
            if b.2 >= 0. {
                out.push(self.screen(b));
            }
            a = b;
        }
        out
    }

    pub(crate) fn globe_fits(&self) -> bool {
        self.radius * 2. <= f64::from(self.frame.0.min(self.frame.1))
    }

    /// At regional and closer scales the rectangular crop behaves as a local map.
    /// Wider views and any exposed limb require spherical gesture rendering.
    pub(crate) fn local_map(&self) -> bool {
        self.view.span <= super::REGIONAL_SPAN && self.disc().is_none()
    }
    pub(crate) fn disc(&self) -> Option<(f64, f64, f64)> {
        // Once the limb is outside the viewport the rectangular background is the ocean.
        (self.radius <= f64::from(self.frame.0).hypot(f64::from(self.frame.1)) / 2.).then_some((
            f64::from(self.frame.0) / 2.,
            f64::from(self.frame.1) / 2.,
            self.radius,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zoom_scales_the_same_globe_without_revealing_the_far_side() {
        let frame = Frame(400, 600);
        let camera = |span| {
            Camera::new(
                View {
                    lon: 25.,
                    lat: 45.,
                    span,
                    roll: 0.,
                },
                frame,
            )
        };
        let base = camera(360.).point(25.1, 45.1).unwrap();
        for span in [225., 150., 61., 60., 35., 5., 1.] {
            let cam = camera(span);
            let p = cam.point(25.1, 45.1).unwrap();
            let scale = 360. / span;
            assert!((p.0 - 200. - (base.0 - 200.) * scale).abs() < 1e-8);
            assert!((p.1 - 300. - (base.1 - 300.) * scale).abs() < 1e-8);
            assert!(cam.point(-155., -45.).is_none());
            let (lon, lat) = cam.locate(p.0, p.1).unwrap();
            assert!((lon - 25.1).abs() < 1e-8 && (lat - 45.1).abs() < 1e-8);
        }
    }

    #[test]
    fn zoom_keeps_an_off_centre_anchor_after_passing_a_pole() {
        let frame = Frame(400, 600);
        let view = View {
            lon: 0.,
            lat: 120.,
            span: 100.,
            roll: 0.,
        };
        let pixel = (270., 345.);
        let place = Camera::new(view, frame).locate(pixel.0, pixel.1).unwrap();
        let zoomed = Camera::anchor(view.zoomed(1.5, None), frame, place, pixel);
        assert!(zoomed.lat > 90.);
        let projected = Camera::new(zoomed, frame).point(place.0, place.1).unwrap();
        assert!((projected.0 - pixel.0).hypot(projected.1 - pixel.1) < 0.02);
    }
    #[test]
    fn zoom_after_passing_a_pole_keeps_the_geographic_centre() {
        let frame = Frame(920, 600);
        for pitch in [-120_f64, 120.] {
            for span in [360., 225., 150., 90., 61., 60., 35.] {
                let camera = Camera::new(
                    View {
                        lon: 0.,
                        lat: pitch,
                        span,
                        roll: 0.,
                    }
                    .kept(),
                    frame,
                );
                let pixel = camera.point(180., pitch.signum() * 60.).unwrap();
                assert!(
                    (pixel.0 - 460.).hypot(pixel.1 - 300.) < 1e-6,
                    "{pitch} {span}: {pixel:?}"
                );
            }
        }
    }
}
