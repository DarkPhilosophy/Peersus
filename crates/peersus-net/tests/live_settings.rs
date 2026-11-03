use iroh::SecretKey;
use peersus_core::{
    Settings,
    config::{BindChange, NetworkSettings, SettingsPatch},
};
use peersus_net::{
    handshake, history::History, network_host::NetworkHost, peers::Peers, settings::SettingsStore,
    transport,
};
use std::time::Duration;

#[tokio::test]
async fn saved_settings_rebind_real_transport_without_changing_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("preferences.json");
    let mut store = SettingsStore::open(&path).unwrap();
    let initial = Settings {
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
        },
        ..Settings::default()
    };
    store.replace(initial.clone()).unwrap();
    let endpoint = transport::bind(SecretKey::generate(), &initial.network)
        .await
        .unwrap();
    let identity = endpoint.id();
    let mut host = NetworkHost::new(
        endpoint,
        initial.device_name.clone(),
        Peers::open(directory.path().join("peers.json")).unwrap(),
        History::open(directory.path().join("history.json")).unwrap(),
    );
    let candidate = SettingsPatch {
        bind: Some(BindChange::Address("127.0.0.2".parse().unwrap())),
        background: Some(true),
        device_name: Some("Changed device".into()),
        ..Default::default()
    }
    .apply(store.current())
    .unwrap();
    host.update_settings(&mut store, candidate.clone())
        .await
        .unwrap();
    assert_eq!(SettingsStore::open(&path).unwrap().current(), &candidate);
    let invitation = host.invite(100).unwrap();
    assert_eq!(invitation.address.id, identity);
    let addresses: Vec<_> = invitation.address.ip_addrs().collect();
    assert!(!addresses.is_empty());
    assert!(
        addresses
            .iter()
            .all(|address| address.ip() == "127.0.0.2".parse::<std::net::IpAddr>().unwrap())
    );
    let client = transport::bind(SecretKey::generate(), &initial.network)
        .await
        .unwrap();
    let join = handshake::join(&client, invitation, "Client".into());
    tokio::pin!(join);
    let admitted = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut join => break result.unwrap(),
                _ = host.next(101) => {},
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(admitted.name, "Changed device");
    admitted.connection.close(0_u32.into(), b"test finished");
    client.close().await;
    host.close().await;
}

#[tokio::test]
async fn invalid_or_unsavable_preferences_leave_current_listener_usable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("preferences.json");
    let mut store = SettingsStore::open(&path).unwrap();
    let initial = Settings {
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
        },
        ..Settings::default()
    };
    store.replace(initial.clone()).unwrap();
    let endpoint = transport::bind(SecretKey::generate(), &initial.network)
        .await
        .unwrap();
    let before = endpoint.addr();
    let mut host = NetworkHost::new(
        endpoint,
        initial.device_name.clone(),
        Peers::open(directory.path().join("peers.json")).unwrap(),
        History::open(directory.path().join("history.json")).unwrap(),
    );
    let mut invalid = initial.clone();
    invalid.scale = 99.0;
    assert!(host.update_settings(&mut store, invalid).await.is_err());
    assert_eq!(store.current(), &initial);
    // Replace this test's own file with a directory to force publication failure.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let mut candidate = initial.clone();
    candidate.network.bind = Some("127.0.0.2".parse().unwrap());
    assert!(host.update_settings(&mut store, candidate).await.is_err());
    assert_eq!(store.current(), &initial);
    assert_eq!(host.invite(100).unwrap().address, before);
    let client = transport::bind(SecretKey::generate(), &initial.network)
        .await
        .unwrap();
    let join = handshake::join(&client, host.invite(100).unwrap(), "Client".into());
    tokio::pin!(join);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut join => { result.unwrap().connection.close(0_u32.into(), b"test finished"); break },
                _ = host.next(101) => {},
            }
        }
    }).await.unwrap();
    client.close().await;
    host.close().await;
}
