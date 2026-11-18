//! Positions on the Earth: how far apart two are and in which direction, the words that say so, and where each is drawn on
//! a map with one of them in the middle. Pure arithmetic: no device, no host.

/// The Earth's mean radius, in metres.
const EARTH: f64 = 6_371_008.8;
/// The widest an accuracy is taken to be, in metres: a system that says it is less sure than this says nothing useful.
pub const ACCURACY_MAX: f64 = 1_000_000.0;
/// The oldest a position is kept as, in seconds: thirty days.
pub const AGE_MAX: u64 = 30 * 24 * 60 * 60;

/// One position, as a system found it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fix {
    /// Degrees north (negative south).
    pub latitude: f64,
    /// Degrees east (negative west).
    pub longitude: f64,
    /// Metres around the position the device is within; `None` when the system did not say.
    pub accuracy: Option<f64>,
}

impl Fix {
    /// A position only when it is a place on Earth; an accuracy that cannot be meant is not said.
    pub fn new(latitude: f64, longitude: f64, accuracy: Option<f64>) -> Option<Self> {
        ((-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude)).then_some(
            Self {
                latitude,
                longitude,
                accuracy: accuracy.filter(|metres| (0.0..=ACCURACY_MAX).contains(metres)),
            },
        )
    }
}

/// Metres along the Earth's surface from `a` to `b`, and the direction to set out in, in degrees clockwise from north.
pub fn between(a: &Fix, b: &Fix) -> (f64, f64) {
    let (from, to) = (a.latitude.to_radians(), b.latitude.to_radians());
    let north = to - from;
    let east = (b.longitude - a.longitude).to_radians();
    let half = (north / 2.0).sin().powi(2) + from.cos() * to.cos() * (east / 2.0).sin().powi(2);
    let distance = 2.0 * EARTH * half.sqrt().min(1.0).asin();
    let y = east.sin() * to.cos();
    let x = from.cos() * to.sin() - from.sin() * to.cos() * east.cos();
    (distance, (y.atan2(x).to_degrees() + 360.0) % 360.0)
}

/// The nearest of the eight points of the compass.
pub fn compass(bearing: f64) -> &'static str {
    const POINTS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    POINTS[((bearing.rem_euclid(360.0) + 22.5) / 45.0) as usize % POINTS.len()]
}

/// A distance as a person reads it: metres under a kilometre, one decimal under ten kilometres, whole kilometres beyond.
pub fn distance(metres: f64) -> String {
    if metres < 1000.0 {
        format!("{} m", metres.round() as u64)
    } else if metres < 10_000.0 {
        format!("{:.1} km", metres / 1000.0)
    } else {
        format!("{} km", (metres / 1000.0).round() as u64)
    }
}

/// Latitude and longitude, four decimals (about ten metres), with their hemispheres.
pub fn coordinates(fix: &Fix) -> String {
    format!(
        "{:.4}° {}, {:.4}° {}",
        fix.latitude.abs(),
        if fix.latitude < 0.0 { "S" } else { "N" },
        fix.longitude.abs(),
        if fix.longitude < 0.0 { "W" } else { "E" },
    )
}

/// How long ago, in the largest whole unit.
pub fn age(seconds: u64) -> String {
    match seconds {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} d ago", seconds / 86_400),
    }
}

/// The smallest of 1, 2 or 5 times a power of ten that is at least `metres` (at least one metre).
pub fn step(metres: f64) -> f64 {
    let metres = metres.max(1.0);
    let power = 10f64.powf(metres.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|factor| factor * power)
        .find(|candidate| *candidate >= metres)
        .unwrap_or(10.0 * power)
}

/// A square map `size` pixels wide with a centre position in the middle. Each position is drawn at its true distance from
/// the centre and in its true direction (an azimuthal equidistant map), so what the rings say holds at every scale, from a
/// house to a continent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Map {
    pub size: f64,
    /// Metres between two rings.
    pub ring: f64,
    /// How many rings, the outermost reaching the farthest position.
    pub rings: u32,
}

/// Room left between the outermost ring and the edge, in pixels: a dot and its label stay inside.
const MARGIN: f64 = 18.0;
/// The least a map shows around its centre, in metres, when every position is (nearly) at the centre.
const LEAST: f64 = 100.0;

impl Map {
    /// The map whose outermost ring reaches `farthest` metres, in at most three rings.
    pub fn fit(size: f64, farthest: f64) -> Self {
        let farthest = farthest.max(LEAST);
        let ring = step(farthest / 3.0);
        Self {
            size,
            ring,
            rings: ((farthest / ring).ceil() as u32).clamp(1, 3),
        }
    }

    fn radius(&self) -> f64 {
        self.size / 2.0 - MARGIN
    }

    /// Pixels from the centre per metre.
    fn scale(&self) -> f64 {
        self.radius() / (self.ring * f64::from(self.rings))
    }

    /// Where a position `metres` away in direction `bearing` is drawn, from the map's top-left corner; never outside it.
    pub fn place(&self, metres: f64, bearing: f64) -> (f64, f64) {
        let centre = self.size / 2.0;
        let reach = (metres * self.scale()).min(self.radius());
        let angle = bearing.to_radians();
        (centre + reach * angle.sin(), centre - reach * angle.cos())
    }

    /// The radius, in pixels, of a ring of `metres`, or of an accuracy around a position: never beyond the map.
    pub fn pixels(&self, metres: f64) -> f64 {
        (metres * self.scale()).min(self.size / 2.0)
    }
}

/// The outline of a circle in SVG path notation: what a drawing shape takes.
pub fn circle(x: f64, y: f64, radius: f64) -> String {
    format!(
        "M {:.1} {y:.1} A {radius:.1} {radius:.1} 0 1 0 {:.1} {y:.1} A {radius:.1} {radius:.1} 0 1 0 {:.1} {y:.1} Z",
        x - radius,
        x + radius,
        x - radius,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(latitude: f64, longitude: f64) -> Fix {
        Fix::new(latitude, longitude, None).unwrap()
    }

    #[test]
    fn distance_and_direction_are_those_of_the_earth() {
        // Bucharest to Cluj-Napoca: about 324 km to the north-west.
        let (metres, bearing) = between(&at(44.4268, 26.1025), &at(46.7712, 23.6236));
        assert!((320_000.0..330_000.0).contains(&metres), "{metres}");
        assert_eq!(compass(bearing), "NW");
        // A quarter of the way round the equator, due east, and back due west.
        let (metres, bearing) = between(&at(0.0, 0.0), &at(0.0, 90.0));
        assert!((metres - EARTH * std::f64::consts::FRAC_PI_2).abs() < 1.0);
        assert!((bearing - 90.0).abs() < 1e-9);
        assert_eq!(compass(between(&at(0.0, 90.0), &at(0.0, 0.0)).1), "W");
        // Across the date line the short way, not round the world.
        let (metres, bearing) = between(&at(0.0, 179.5), &at(0.0, -179.5));
        assert!(metres < 120_000.0, "{metres}");
        assert_eq!(compass(bearing), "E");
        // The same place: nothing between them.
        assert_eq!(between(&at(10.0, 10.0), &at(10.0, 10.0)).0, 0.0);
    }

    #[test]
    fn only_a_place_on_earth_is_a_position() {
        assert!(Fix::new(91.0, 0.0, None).is_none());
        assert!(Fix::new(0.0, -180.1, None).is_none());
        assert!(Fix::new(f64::NAN, 0.0, None).is_none());
        assert_eq!(Fix::new(1.0, 2.0, Some(-3.0)).unwrap().accuracy, None);
        assert_eq!(
            Fix::new(1.0, 2.0, Some(f64::INFINITY)).unwrap().accuracy,
            None
        );
        assert_eq!(Fix::new(1.0, 2.0, Some(25.0)).unwrap().accuracy, Some(25.0));
    }

    #[test]
    fn words_say_the_distance_the_age_and_the_place() {
        assert_eq!(distance(12.4), "12 m");
        assert_eq!(distance(1_234.0), "1.2 km");
        assert_eq!(distance(324_499.0), "324 km");
        assert_eq!(age(5), "just now");
        assert_eq!(age(125), "2 min ago");
        assert_eq!(age(7_300), "2 h ago");
        assert_eq!(age(200_000), "2 d ago");
        assert_eq!(
            coordinates(&at(-33.8688, 151.2093)),
            "33.8688° S, 151.2093° E"
        );
        assert_eq!(compass(359.0), "N");
        assert_eq!(compass(-90.0), "W");
    }

    #[test]
    fn rings_are_round_numbers_and_the_farthest_position_is_on_the_outermost() {
        assert_eq!(step(0.2), 1.0);
        assert_eq!(step(3.0), 5.0);
        assert_eq!(step(120.0), 200.0);
        assert_eq!(step(1000.0), 1000.0);
        let map = Map::fit(300.0, 2_500.0);
        assert_eq!((map.ring, map.rings), (1000.0, 3));
        // Due north is straight up, due east straight right, and nothing is drawn outside the map.
        let (x, y) = map.place(3_000.0, 0.0);
        assert!((x - 150.0).abs() < 1e-9 && (y - MARGIN).abs() < 1e-9);
        let (x, y) = map.place(1_500.0, 90.0);
        assert!((x - (150.0 + 66.0)).abs() < 1e-9 && (y - 150.0).abs() < 1e-9);
        let (x, _) = map.place(1e9, 90.0);
        assert!(x <= 300.0 - MARGIN + 1e-9);
        // Everything at the centre still has a map of its own scale.
        assert_eq!(Map::fit(300.0, 0.0), Map::fit(300.0, LEAST));
    }

    #[test]
    fn a_circle_is_an_outline_a_drawing_takes() {
        let path = circle(10.0, 20.0, 5.0);
        assert!(path.starts_with("M 5.0 20.0 A 5.0 5.0 0 1 0 15.0 20.0"));
        assert!(
            path.bytes()
                .all(|b| b.is_ascii_digit() || b" .,-MmLlHhVvCcSsQqTtAaZz".contains(&b))
        );
    }
}
