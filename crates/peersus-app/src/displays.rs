//! Native desktop monitors published to paired devices for their pass-through layout.
//! Linux uses Mutter's logical coordinates; Windows uses the same physical desktop
//! coordinates as its input adapter. Enumeration failure reports no displays, never
//! invented geometry. Only changes are published.
use peersus_core::layout::Display;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::current;

#[cfg(target_os = "windows")]
pub async fn current() -> Vec<Display> {
    tokio::task::spawn_blocking(windows_current)
        .await
        .unwrap_or_default()
}

#[cfg(target_os = "windows")]
fn windows_current() -> Vec<Display> {
    let Ok(monitors) = display_info::DisplayInfo::all() else {
        peersus_diagnostics::dev_trace!(
            stage = "displays",
            outcome = "unavailable",
            "Native display enumeration failed"
        );
        return Vec::new();
    };
    monitors
        .into_iter()
        .map(|monitor| Display {
            id: monitor.name,
            x: monitor.x,
            y: monitor.y,
            width: monitor.width,
            height: monitor.height,
        })
        .filter(Display::valid)
        .take(peersus_core::layout::MAX_DISPLAYS)
        .collect()
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

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires an interactive Windows desktop"]
    async fn interactive_windows_monitors_are_published_in_receiver_coordinates() {
        let actual = current().await;
        assert!(
            !actual.is_empty(),
            "an interactive Windows receiver must publish its actual monitors"
        );
        assert!(peersus_core::layout::valid_displays(&actual));
        assert!(
            actual
                .iter()
                .any(|display| display.x == 0 && display.y == 0),
            "the primary monitor retains the OS desktop origin"
        );
    }
}
