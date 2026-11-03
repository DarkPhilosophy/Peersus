//! Platform preference missing from winit on Linux. No polling processes or UI-thread I/O.
use tokio::sync::watch;

pub fn watch(executor: &tokio::runtime::Handle) -> watch::Receiver<Option<bool>> {
    let (sender, receiver) = watch::channel(None);
    #[cfg(target_os = "linux")]
    executor.spawn(async move {
        let mut delay = 1;
        loop {
            tokio::select! {
                _ = sender.closed() => break,
                _ = follow(&sender) => {}
            }
            sender.send_replace(None);
            tokio::select! {
                _ = sender.closed() => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(delay)) => {}
            }
            delay = (delay * 2).min(30);
        }
    });
    #[cfg(not(target_os = "linux"))]
    let _ = (executor, sender);
    receiver
}

#[cfg(target_os = "linux")]
async fn follow(sender: &watch::Sender<Option<bool>>) -> zbus::Result<()> {
    let connection = zbus::Connection::session().await?;
    follow_on(&connection, "org.freedesktop.portal.Desktop", sender).await
}

#[cfg(target_os = "linux")]
async fn follow_on(connection: &zbus::Connection, destination: &str, sender: &watch::Sender<Option<bool>>) -> zbus::Result<()> {
    use futures_lite::StreamExt;
    use zbus::zvariant::OwnedValue;
    let proxy = zbus::Proxy::new(
        connection,
        destination,
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
    ).await?;
    // Subscribe before reading: a preference changed during startup must not be lost.
    let mut signals = proxy.receive_signal_with_args("SettingChanged", &[(0, "org.freedesktop.appearance"), (1, "color-scheme")]).await?;
    let mut owners = proxy.receive_owner_changed().await?;
    let value: OwnedValue = proxy.call("ReadOne", &("org.freedesktop.appearance", "color-scheme")).await?;
    sender.send_replace(preference(u32::try_from(value).ok()));
    loop {
        tokio::select! {
            signal = signals.next() => {
                let Some(signal) = signal else { return Ok(()); };
                let (namespace, key, value): (String, String, OwnedValue) = signal.body().deserialize()?;
                if namespace == "org.freedesktop.appearance" && key == "color-scheme" {
                    sender.send_replace(preference(u32::try_from(value).ok()));
                }
            }
            _ = owners.next() => return Ok(()),
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn preference(value: Option<u32>) -> Option<bool> {
    match value {
        Some(1) => Some(true),
        Some(2) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_preference_and_future_values_use_toolkit_fallback_not_light() {
        assert_eq!(preference(Some(1)), Some(true));
        assert_eq!(preference(Some(2)), Some(false));
        assert_eq!(preference(Some(0)), None);
        assert_eq!(preference(Some(99)), None);
        assert_eq!(preference(None), None);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires a local desktop D-Bus session; does not change the system preference"]
    async fn follows_initial_preference_and_live_portal_changes() {
        struct SettingsFixture;
        #[zbus::interface(name = "org.freedesktop.portal.Settings")]
        impl SettingsFixture {
            fn read_one(&self, namespace: &str, key: &str) -> zbus::fdo::Result<zbus::zvariant::OwnedValue> {
                if namespace != "org.freedesktop.appearance" || key != "color-scheme" {
                    return Err(zbus::fdo::Error::Failed("Unknown test preference".into()));
                }
                Ok(1u32.into())
            }
        }
        let name = format!("ro.peersus.ThemeTest.p{}", std::process::id());
        let service = zbus::connection::Builder::session().unwrap()
            .name(name.as_str()).unwrap()
            .serve_at("/org/freedesktop/portal/desktop", SettingsFixture).unwrap()
            .build().await.unwrap();
        let (sender, mut receiver) = watch::channel(None);
        let client = zbus::Connection::session().await.unwrap();
        let worker = tokio::spawn(async move { follow_on(&client, &name, &sender).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), receiver.changed()).await.unwrap().unwrap();
        assert_eq!(*receiver.borrow_and_update(), Some(true));
        for (value, expected) in [(2u32, Some(false)), (0u32, None), (1u32, Some(true))] {
            service.emit_signal(
                None::<&str>, "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.Settings", "SettingChanged",
                &("org.freedesktop.appearance", "color-scheme", zbus::zvariant::OwnedValue::from(value)),
            ).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), receiver.changed()).await.unwrap().unwrap();
            assert_eq!(*receiver.borrow_and_update(), expected);
        }
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
    }
}
