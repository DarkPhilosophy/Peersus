//! Where this device is now, for an extension the person allowed to know it (`location.read`).
//!
//! The position is the system's own: its location service decides how it finds it (satellites, the networks it sees, which a
//! system service may look up online on its own account). The application asks once per call, keeps nothing and answers only
//! latitude, longitude, how sure the system is and how old the fix is. What the extension does with it (sending it to a paired
//! device, drawing it) is the extension's, under its own permissions.
use peersus_api::message::Outcome;
use serde::Serialize;

pub const SERVICE: &str = "peersus.location";
pub const CURRENT: &str = "current";
pub const CAPABILITY: &str = "location.read";
/// The longest a call may take: a system that has no fix yet may need a few seconds to get one, never a person's answer.
pub const CEILING: std::time::Duration = std::time::Duration::from_secs(20);

/// Answered when the system says nothing: no location service, or no fix in time.
pub const UNAVAILABLE: &str = "This device does not say where it is now";
/// Answered when the person's own setting refuses: location turned off on this device, or not allowed to the application.
pub const REFUSED: &str =
    "Location is turned off on this device, or not allowed to the application";

/// One position, checked: the system's numbers are passed on only when they are a place on Earth.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Position {
    /// Degrees north (negative south), -90 to 90.
    pub latitude: f64,
    /// Degrees east (negative west), -180 to 180.
    pub longitude: f64,
    /// Metres around the position the device is within, as sure as the system says it is; `None` when it does not say.
    pub accuracy: Option<f64>,
    /// Seconds since the system found it.
    pub age: u64,
}

impl Position {
    /// `found_at` is when the system found it, in seconds since 1970; a fix from the future (a clock set back) is of age 0.
    pub fn new(
        latitude: f64,
        longitude: f64,
        accuracy: Option<f64>,
        found_at: u64,
    ) -> Option<Self> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or(0);
        ((-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude)).then_some(
            Self {
                latitude,
                longitude,
                // A negative, infinite or not-a-number accuracy is the system not saying.
                accuracy: accuracy.filter(|metres| metres.is_finite() && *metres >= 0.0),
                age: now.saturating_sub(found_at),
            },
        )
    }
}

/// The answer of one call to `method`, finding the position with this platform's `find` only for a method the service has.
pub async fn answer<F>(method: &str, find: impl FnOnce() -> F) -> Outcome
where
    F: std::future::Future<Output = Result<Position, &'static str>>,
{
    let found = if method == CURRENT {
        find().await
    } else {
        Err("Unsupported platform operation")
    };
    match found.and_then(|position| serde_json::to_value(position).map_err(|_| UNAVAILABLE)) {
        Ok(value) => Outcome::Ok { value },
        Err(message) => Outcome::Error {
            code: "unavailable".into(),
            message: message.into(),
        },
    }
}

/// Where this computer is, as the desktop's location portal finds it: the portal applies the person's own setting for location
/// and for this application, so a desktop with location turned off answers [`REFUSED`].
#[cfg(target_os = "linux")]
pub async fn find() -> Result<Position, &'static str> {
    use ashpd::desktop::location::{Accuracy, CreateSessionOptions, LocationProxy, StartOptions};
    use futures_lite::StreamExt;
    let refused = |error: ashpd::Error| match error {
        ashpd::Error::Portal(ashpd::PortalError::NotAllowed(_)) => REFUSED,
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
            "The desktop location request was cancelled"
        }
        ashpd::Error::Response(_) => "The desktop location service could not start a session",
        _ => UNAVAILABLE,
    };
    let proxy = LocationProxy::new().await.map_err(|_| UNAVAILABLE)?;
    // Subscribe before starting: a position already known is said at once.
    let mut updated = proxy
        .receive_location_updated()
        .await
        .map_err(|_| UNAVAILABLE)?;
    let session = proxy
        .create_session(CreateSessionOptions::default().set_accuracy(Accuracy::Exact))
        .await
        .map_err(refused)?;
    let found = async {
        proxy
            .start(&session, None, StartOptions::default())
            .await
            .and_then(|request| request.response())
            .map_err(refused)?;
        // A little under the service's ceiling, so the session is closed here rather than dropped mid-call.
        let location =
            tokio::time::timeout(CEILING - std::time::Duration::from_secs(2), updated.next())
                .await
                .map_err(|_| UNAVAILABLE)?
                .ok_or(UNAVAILABLE)?;
        Position::new(
            location.latitude(),
            location.longitude(),
            Some(location.accuracy()),
            location.timestamp().as_secs(),
        )
        .ok_or(UNAVAILABLE)
    }
    .await;
    // The session is this call's alone: closed whatever the answer, so the system does not keep looking for nobody.
    let _ = session.close().await;
    found
}

/// A desktop whose location service the application does not speak to says nothing.
#[cfg(all(not(target_os = "linux"), not(target_os = "android")))]
pub async fn find() -> Result<Position, &'static str> {
    Err(UNAVAILABLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn only_a_place_on_earth_is_a_position_and_an_accuracy_the_system_cannot_mean_is_not_said() {
        let at = now() - 30;
        let found = Position::new(44.43, 26.1, Some(12.5), at).unwrap();
        assert_eq!(
            (found.latitude, found.longitude, found.accuracy),
            (44.43, 26.1, Some(12.5))
        );
        assert!((30..=31).contains(&found.age));
        for (latitude, longitude) in [
            (90.1, 0.0),
            (-90.1, 0.0),
            (0.0, 180.5),
            (0.0, -181.0),
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
        ] {
            assert_eq!(
                Position::new(latitude, longitude, None, at),
                None,
                "{latitude} {longitude}"
            );
        }
        for unsaid in [-1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                Position::new(0.0, 0.0, Some(unsaid), at).unwrap().accuracy,
                None
            );
        }
        // A fix stamped after now (a clock set back) is new, never of a huge age.
        assert_eq!(Position::new(0.0, 0.0, None, now() + 3600).unwrap().age, 0);
    }

    #[tokio::test]
    async fn the_answer_is_the_position_or_a_refusal_that_says_nothing_more() {
        let at = now();
        let found = || async move { Ok(Position::new(1.0, 2.0, None, at).unwrap()) };
        let Outcome::Ok { value } = answer(CURRENT, found).await else {
            panic!("a position was refused");
        };
        assert_eq!(value["latitude"], 1.0);
        assert_eq!(value["longitude"], 2.0);
        assert!(value["accuracy"].is_null());
        let Outcome::Error { code, message } = answer(CURRENT, || async { Err(UNAVAILABLE) }).await
        else {
            panic!("nothing found was answered");
        };
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("unavailable", UNAVAILABLE)
        );
        // A method the service does not have never asks the system.
        let asked = std::cell::Cell::new(false);
        let refused = answer("history", || {
            asked.set(true);
            found()
        })
        .await;
        assert!(matches!(refused, Outcome::Error { .. }) && !asked.get());
    }
}
