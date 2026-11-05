//! The displays of this computer's desktop, as the system arranges them, told to the owner whenever
//! they change. They are what a layout of devices places this computer as.
//!
//! GNOME (Mutter) says them on the session bus, in logical pixels, which are what the pointer moves
//! in. A desktop that does not say them leaves the list empty: the layout then has nothing of this
//! computer to place, and says so, rather than guessing a size.
#![cfg(target_os = "linux")]

use peersus_core::layout::Display;
use std::collections::HashMap;
use zbus::OwnedVariant as OwnedValue;

/// `(connector, vendor, product, serial)`.
type MonitorSpec = (String, String, String, String);
/// `(id, width, height, refresh, preferred scale, supported scales, properties)`.
type Mode = (
    String,
    i32,
    i32,
    f64,
    f64,
    Vec<f64>,
    HashMap<String, OwnedValue>,
);
type Monitor = (MonitorSpec, Vec<Mode>, HashMap<String, OwnedValue>);
/// `(x, y, scale, transform, primary, monitors, properties)`.
type Logical = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<MonitorSpec>,
    HashMap<String, OwnedValue>,
);
type State = (u32, Vec<Monitor>, Vec<Logical>, HashMap<String, OwnedValue>);

/// Mutter's layout mode in which logical monitors are sized in logical pixels (the scale already applied).
const LAYOUT_LOGICAL: u32 = 1;

/// The displays Mutter says the desktop has now, or none when it does not answer.
pub async fn current() -> Vec<Display> {
    let Ok(connection) = zbus::Connection::session().await else {
        return Vec::new();
    };
    let reply = connection
        .call_method(
            Some("org.gnome.Mutter.DisplayConfig"),
            "/org/gnome/Mutter/DisplayConfig",
            Some("org.gnome.Mutter.DisplayConfig"),
            "GetCurrentState",
            &(),
        )
        .await;
    let Ok(reply) = reply else {
        return Vec::new();
    };
    let Ok(state) = reply.body().deserialize::<State>() else {
        return Vec::new();
    };
    from_state(&state)
}

fn from_state((_, monitors, logical, properties): &State) -> Vec<Display> {
    let layout_mode = properties
        .get("layout-mode")
        .and_then(|value| u32::try_from(value).ok());
    let current_mode = |spec: &MonitorSpec| -> Option<(i32, i32)> {
        let (_, modes, _) = monitors.iter().find(|(s, _, _)| s == spec)?;
        modes
            .iter()
            .find(|(_, _, _, _, _, _, props)| {
                props
                    .get("is-current")
                    .and_then(|value| bool::try_from(value).ok())
                    == Some(true)
            })
            .map(|(_, width, height, ..)| (*width, *height))
    };
    let mut displays: Vec<Display> = logical
        .iter()
        .filter_map(|(x, y, scale, transform, _, specs, _)| {
            let spec = specs.first()?;
            let (mut width, mut height) = current_mode(spec)?;
            // A display turned on its side is as tall as it was wide.
            if transform % 2 == 1 {
                std::mem::swap(&mut width, &mut height);
            }
            if layout_mode == Some(LAYOUT_LOGICAL) && *scale > 0. {
                width = (f64::from(width) / scale).round() as i32;
                height = (f64::from(height) / scale).round() as i32;
            }
            Some(Display {
                id: spec.0.clone(),
                x: *x,
                y: *y,
                width: u32::try_from(width).ok()?,
                height: u32::try_from(height).ok()?,
            })
        })
        .filter(Display::valid)
        .collect();
    displays.truncate(peersus_core::layout::MAX_DISPLAYS);
    displays
}

/// Tell the owner the displays now and whenever they change, for as long as it runs. Polled: the
/// arrangement changes rarely and a poll every few seconds costs one small call.
pub async fn follow(client: crate::runtime::Client) {
    let mut told: Option<Vec<Display>> = None;
    loop {
        let now = current().await;
        if told.as_ref() != Some(&now) {
            if client
                .command(peersus_core::model::Command::SetDisplays {
                    displays: now.clone(),
                })
                .await
                .is_err()
            {
                return;
            }
            told = Some(now);
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> MonitorSpec {
        (name.into(), String::new(), String::new(), String::new())
    }
    fn mode(width: i32, height: i32, current: bool) -> Mode {
        let mut props = HashMap::new();
        if current {
            props.insert("is-current".into(), OwnedValue::from(true));
        }
        ("m".into(), width, height, 60., 1., vec![1.], props)
    }

    #[test]
    fn logical_monitors_become_displays_in_logical_pixels_on_their_side_when_turned() {
        let monitors = vec![
            (
                spec("HDMI-1"),
                vec![mode(4096, 2160, true), mode(1920, 1080, false)],
                HashMap::new(),
            ),
            (spec("DP-1"), vec![mode(1920, 1080, true)], HashMap::new()),
        ];
        let logical = vec![
            (0, 0, 2., 0, true, vec![spec("HDMI-1")], HashMap::new()),
            (2048, 0, 1., 1, false, vec![spec("DP-1")], HashMap::new()),
        ];
        let mut props = HashMap::new();
        props.insert("layout-mode".into(), OwnedValue::from(LAYOUT_LOGICAL));
        let displays = from_state(&(1, monitors, logical, props));
        assert_eq!(
            displays,
            vec![
                Display {
                    id: "HDMI-1".into(),
                    x: 0,
                    y: 0,
                    width: 2048,
                    height: 1080
                },
                Display {
                    id: "DP-1".into(),
                    x: 2048,
                    y: 0,
                    width: 1080,
                    height: 1920
                },
            ]
        );
    }

    #[test]
    fn a_monitor_without_a_current_mode_is_left_out() {
        let monitors = vec![(spec("X"), vec![mode(800, 600, false)], HashMap::new())];
        let logical = vec![(0, 0, 1., 0, true, vec![spec("X")], HashMap::new())];
        assert!(from_state(&(1, monitors, logical, HashMap::new())).is_empty());
    }
}
