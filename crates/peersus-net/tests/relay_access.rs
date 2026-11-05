//! The access token of a personal relay lives in the host, apart from the preferences. These tests hold what the
//! person depends on: it is applied by binding a replacement transport (and only when a personal relay uses it), a token
//! that changes nothing does not disturb the connections, and a refusal leaves everything as it was.
use iroh::SecretKey;
use peersus_core::{
    Settings,
    config::{NetworkSettings, RelayChange, SettingsPatch},
};
use peersus_net::{
    history::History, network_host::NetworkHost, peers::Peers, settings::SettingsStore, transport,
    transport::RelayAccess,
};
use std::net::SocketAddr;

/// A host on loopback with relays off, which is how every test of the network runs without reaching a real relay.
async fn host_in(directory: &std::path::Path) -> (NetworkHost, SettingsStore) {
    let mut store = SettingsStore::open(directory.join("preferences.json")).unwrap();
    let initial = Settings {
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        },
        ..Settings::default()
    };
    store.replace(initial.clone()).unwrap();
    let endpoint = transport::bind(SecretKey::generate(), &initial.network)
        .await
        .unwrap();
    let host = NetworkHost::new(
        endpoint,
        initial.device_name,
        Peers::open(directory.join("peers.json")).unwrap(),
        History::open(directory.join("history.json")).unwrap(),
    );
    (host, store)
}

fn sockets(host: &NetworkHost) -> Vec<SocketAddr> {
    let found: Vec<SocketAddr> = host
        .invite(100)
        .unwrap()
        .address
        .ip_addrs()
        .copied()
        .collect();
    assert!(
        !found.is_empty(),
        "a host that is listening has addresses: an empty list proves nothing"
    );
    found
}

#[tokio::test]
async fn a_token_is_only_remembered_while_no_personal_relay_uses_it() {
    let directory = tempfile::tempdir().unwrap();
    let (mut host, mut store) = host_in(directory.path()).await;
    let before = sockets(&host);
    host.set_relay_access(&mut store, RelayAccess::new("s3cret-token"))
        .await
        .unwrap();
    assert_eq!(
        sockets(&host),
        before,
        "no personal relay is chosen, so there is nothing to rebind for"
    );
    host.close().await;
}

#[tokio::test]
async fn choosing_a_personal_relay_rebinds_with_it_and_saves_only_its_address() {
    let directory = tempfile::tempdir().unwrap();
    let (mut host, mut store) = host_in(directory.path()).await;
    let before = sockets(&host);
    host.set_relay_access(&mut store, RelayAccess::new("s3cret-token"))
        .await
        .unwrap();

    // A personal relay needs relays on, and an address bound to one interface cannot carry relay traffic.
    let candidate = SettingsPatch {
        bind: Some(peersus_core::config::BindChange::Automatic),
        relay: Some(RelayChange::Own("https://relay.example.org".into())),
        ..SettingsPatch::default()
    }
    .apply(store.current())
    .unwrap();
    host.update_settings(&mut store, candidate).await.unwrap();
    assert_ne!(
        sockets(&host),
        before,
        "the choice was applied by binding a replacement transport"
    );

    // Only the address is saved. The token is a secret and is not in any file of the preferences.
    let saved = std::fs::read_to_string(directory.path().join("preferences.json")).unwrap();
    assert!(saved.contains("relay.example.org"), "{saved}");
    assert!(
        !saved.contains("s3cret"),
        "the token must never reach the preferences file"
    );
    host.close().await;
}

#[tokio::test]
async fn the_same_token_again_disturbs_nothing_and_a_new_one_is_applied() {
    let directory = tempfile::tempdir().unwrap();
    let (mut host, mut store) = host_in(directory.path()).await;
    let candidate = SettingsPatch {
        bind: Some(peersus_core::config::BindChange::Automatic),
        relay: Some(RelayChange::Own("https://relay.example.org".into())),
        ..SettingsPatch::default()
    }
    .apply(store.current())
    .unwrap();
    host.update_settings(&mut store, candidate).await.unwrap();
    host.set_relay_access(&mut store, RelayAccess::new("first"))
        .await
        .unwrap();

    let settled = sockets(&host);
    host.set_relay_access(&mut store, RelayAccess::new("first"))
        .await
        .unwrap();
    assert_eq!(
        sockets(&host),
        settled,
        "the same token is not a reason to drop connections"
    );

    host.set_relay_access(&mut store, RelayAccess::new("second"))
        .await
        .unwrap();
    assert_ne!(
        sockets(&host),
        settled,
        "a different token is applied by a replacement transport"
    );
    host.close().await;
}
