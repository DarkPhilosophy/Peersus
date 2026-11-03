//! The access token of a personal relay, through a real runtime. What the person depends on: it is kept only by the store the
//! platform supplies, it is in no preference, no snapshot and no `Debug` text, and a token that could not be kept is not left in use.
use iroh::SecretKey;
use peersus_app::{
    plugin_host::Registry,
    runtime::{Client, Runtime, TokenStore},
};
use peersus_core::{
    Settings,
    config::{BindChange, NetworkSettings, RelayChange, SettingsPatch},
    model::{Command, SecretText},
};
use peersus_net::{
    history::History, network_host::NetworkHost, peers::Peers, settings::SettingsStore, transport,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

const TOKEN: &str = "s3cret-token-value";

/// A store in memory that can be told to refuse, and that remembers what it was asked to keep.
#[derive(Default)]
struct Memory {
    kept: Mutex<Vec<String>>,
    refuse: Mutex<bool>,
}

impl TokenStore for Memory {
    fn save(&self, token: &str) -> Result<(), &'static str> {
        if *self.refuse.lock().unwrap() {
            return Err("Cannot save the relay token in the system credential store");
        }
        self.kept.lock().unwrap().push(token.to_owned());
        Ok(())
    }
}

async fn start(path: &Path, store: Option<Arc<Memory>>) -> (Client, tokio::task::JoinHandle<()>) {
    std::fs::create_dir_all(path).unwrap();
    let mut settings_store = SettingsStore::open(path.join("preferences.json")).unwrap();
    let settings = Settings {
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        },
        ..Settings::default()
    };
    settings_store.replace(settings.clone()).unwrap();
    let endpoint = transport::bind(SecretKey::generate(), &settings.network)
        .await
        .unwrap();
    let network = NetworkHost::new(
        endpoint,
        settings.device_name,
        Peers::open(path.join("peers.json")).unwrap(),
        History::open(path.join("history.json")).unwrap(),
    );
    let sockets = tempfile::Builder::new()
        .prefix("rt-")
        .tempdir_in("/tmp")
        .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(sockets.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let standalone = peersus_net::grants::StandaloneGrants::open(path.join("grants.json")).unwrap();
    let (runtime, client) = Runtime::new(
        network,
        standalone,
        settings_store,
        Registry::default(),
        sockets.path().to_path_buf(),
    )
    .unwrap();
    let runtime = match store {
        Some(store) => runtime.with_token_store(store),
        None => runtime,
    };
    let task = tokio::spawn(async move {
        runtime.run().await;
        drop(sockets);
    });
    (client, task)
}

fn token() -> SecretText {
    SecretText::new(TOKEN).unwrap()
}

/// Choose a personal relay the way a window does, so that a token has something to be used by.
async fn choose_a_personal_relay(client: &Client) {
    client
        .command(Command::PatchSettings {
            patch: SettingsPatch {
                bind: Some(BindChange::Automatic),
                relay: Some(RelayChange::Own("https://relay.example.org".into())),
                ..SettingsPatch::default()
            },
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_with_no_place_to_keep_a_token_refuses_it_instead_of_holding_it_in_memory() {
    let directory = tempfile::tempdir().unwrap();
    let (client, task) = start(directory.path(), None).await;
    let refused = client
        .command(Command::SetRelayToken { token: token() })
        .await
        .err()
        .expect("it must be refused");
    assert_eq!(refused.to_string(), "This device cannot keep a relay token");
    client.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_token_is_kept_by_the_store_and_is_in_no_file_snapshot_or_debug_text() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(Memory::default());
    let (client, task) = start(directory.path(), Some(store.clone())).await;
    choose_a_personal_relay(&client).await;
    client
        .command(Command::SetRelayToken { token: token() })
        .await
        .unwrap();
    assert_eq!(
        *store.kept.lock().unwrap(),
        vec![TOKEN.to_owned()],
        "the store was asked to keep exactly the token"
    );

    // The snapshot is what every window, the command line and every script read: it carries the address and never the token.
    let snapshot = serde_json::to_string(&*client.snapshot()).unwrap();
    assert!(
        snapshot.contains("relay.example.org"),
        "the address is a preference and is shown"
    );
    assert!(!snapshot.contains(TOKEN), "the token reached the snapshot");
    // Nor `Debug`, which is what logs and panic messages are made of.
    let command = Command::SetRelayToken { token: token() };
    assert!(
        !format!("{command:?}").contains(TOKEN),
        "the token reached Debug"
    );
    assert!(
        !format!("{:?}", client.snapshot()).contains(TOKEN),
        "the token reached the snapshot's Debug"
    );

    client.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();

    // And not the preferences file, which is plain text on disk, or any other file of the application's folder.
    for entry in std::fs::read_dir(directory.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(TOKEN),
                "the token reached {}",
                path.display()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_that_could_not_be_kept_is_refused_and_not_left_in_use() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(Memory::default());
    let (client, task) = start(directory.path(), Some(store.clone())).await;
    choose_a_personal_relay(&client).await;
    *store.refuse.lock().unwrap() = true;
    let refused = client
        .command(Command::SetRelayToken { token: token() })
        .await
        .err()
        .expect("it must be refused");
    assert_eq!(
        refused.to_string(),
        "Cannot save the relay token in the system credential store"
    );
    assert!(store.kept.lock().unwrap().is_empty(), "nothing was kept");

    // The device is not stuck: the same token is accepted as soon as the store works again.
    *store.refuse.lock().unwrap() = false;
    client
        .command(Command::SetRelayToken { token: token() })
        .await
        .unwrap();
    assert_eq!(store.kept.lock().unwrap().len(), 1);

    client.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_token_forgets_and_an_unusable_one_never_reaches_the_owner() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(Memory::default());
    let (client, task) = start(directory.path(), Some(store.clone())).await;
    client
        .command(Command::SetRelayToken {
            token: SecretText::new("").unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(
        *store.kept.lock().unwrap(),
        vec![String::new()],
        "an empty token is the store's word for forgetting"
    );

    // A command that carries more than a line of a token cannot even be read, so no client can make the owner hold it.
    for bad in [
        format!("\"{}\"", "x".repeat(513)),
        "\"two\\nlines\"".to_owned(),
    ] {
        let line = format!("{{\"command\":\"set-relay-token\",\"token\":{bad}}}");
        assert!(serde_json::from_str::<Command>(&line).is_err(), "{line}");
    }
    let good =
        serde_json::from_str::<Command>("{\"command\":\"set-relay-token\",\"token\":\"abc\"}")
            .unwrap();
    assert!(matches!(good, Command::SetRelayToken { .. }));

    client.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}
