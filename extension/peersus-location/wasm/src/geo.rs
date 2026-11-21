//! Positions on the Earth: how far apart two are and in which direction, the words that say so, and where each is drawn on
//! a map with one of them in the middle. Pure arithmetic: no device, no host.

pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(latitude: f64, longitude: f64) -> Fix {
        Fix::new(latitude, longitude, None).unwrap()
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
    }
}
