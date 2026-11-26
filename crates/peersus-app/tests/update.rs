//! Updates of the application through the REAL owner: commands and preferences in, snapshots and the
//! owner's way of ending out. Run with:
//!   cargo test -p peersus-app --features test-plugin --test update
//! Without the feature the whole file is skipped (the in-memory fetcher is only built for tests), so
//! check that the count printed is not 0.
//!
//! Two stand-ins: where bytes come from (an in-memory fetcher), and what puts an update in place (an
//! installer that records what it was given). Everything that TRUSTS is real: the signed release,
//! the rollback floor on disk, and the byte-for-byte check before the installer is handed anything.
#![cfg(all(unix, feature = "test-plugin"))]

use ed25519_dalek::{Signer, SigningKey};
use iroh::SecretKey;
use peersus_app::{
    app_update::{
        Build, Installer, Kind, SIGNATURE_CONTEXT, SIGNATURE_FILE, SUMS_FILE, TAGS_NAME, Target,
        UpdateService, Verified, listing_name,
    },
    plugin_host::Registry,
    runtime::{Client, Exit, Runtime},
    store_runtime::fake::MapFetcher,
    store_service::StoreService,
};
use peersus_core::{
    Settings,
    config::NetworkSettings,
    model::{Command, Snapshot, UpdateStatus},
};
use peersus_net::{
    history::History, network_host::NetworkHost, peers::Peers, settings::SettingsStore, transport,
};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

const SEED: [u8; 32] = [11; 32];
const ARTIFACT: &[u8] = b"the programs of the next build";

fn key() -> [u8; 32] {
    SigningKey::from_bytes(&SEED).verifying_key().to_bytes()
}

/// Serve one release of the `dev` channel: its tag, its listing, its signed list and its one
/// artifact. `pinned` is what the list says the artifact is; `served` is what is actually handed out.
fn publish(fetcher: &MapFetcher, version: &str, pinned: &[u8], served: &[u8]) {
    let tag = format!("v{version}-dev.1");
    let file = format!("peersus-{version}-dev.1-test-any.tar.gz");
    let sums = format!("{}  {file}\n", hex::encode(Sha256::digest(pinned))).into_bytes();
    let mut signed = SIGNATURE_CONTEXT.to_vec();
    signed.extend_from_slice(&sums);
    let signature = SigningKey::from_bytes(&SEED).sign(&signed).to_bytes();
    let asset = |id: u64, name: &str, size: usize| serde_json::json!({"id": id, "name": name, "size": size, "state": "uploaded"});
    let listing = serde_json::json!({
        "draft": false, "tag_name": tag,
        "assets": [
            asset(1, SUMS_FILE, sums.len()),
            asset(2, SIGNATURE_FILE, 64),
            asset(3, &file, served.len()),
        ],
    });
    fetcher.put(
        TAGS_NAME,
        serde_json::to_vec(&serde_json::json!([{ "ref": format!("refs/tags/{tag}") }])).unwrap(),
    );
    fetcher.put(&listing_name(&tag), serde_json::to_vec(&listing).unwrap());
    fetcher.put("store-asset-1", sums);
    fetcher.put("store-asset-2", signature.to_vec());
    fetcher.put("store-asset-3", served.to_vec());
}

/// What the installer was asked to put in place, and what it answers.
#[derive(Default)]
struct Recorder {
    given: Mutex<Vec<(Vec<u8>, bool)>>,
    refuses: Option<&'static str>,
}

impl Installer for Recorder {
    fn target(&self) -> Result<Target, &'static str> {
        Ok(Target {
            platform: "test",
            architecture: "sample",
            kind: Kind::Archive,
        })
    }

    fn install(&self, update: Verified, reopen: bool) -> Result<(), &'static str> {
        self.given.lock().unwrap().push((update.bytes, reopen));
        self.refuses.map_or(Ok(()), Err)
    }
}

struct Owner {
    client: Client,
    task: tokio::task::JoinHandle<Exit>,
    installer: Arc<Recorder>,
    _data: tempfile::TempDir,
    _private: tempfile::TempDir,
}

async fn start(
    key: Option<[u8; 32]>,
    fetcher: MapFetcher,
    installer: Recorder,
    prefer: impl FnOnce(&mut Settings),
) -> Owner {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut store = SettingsStore::open(root.path().join("preferences.json")).unwrap();
    let mut settings = Settings {
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        },
        ..Settings::default()
    };
    prefer(&mut settings);
    store.replace(settings.clone()).unwrap();
    let endpoint = transport::bind(SecretKey::generate(), &settings.network)
        .await
        .unwrap();
    let network = NetworkHost::new(
        endpoint,
        settings.device_name,
        Peers::open(root.path().join("peers.json")).unwrap(),
        History::open(root.path().join("history.json")).unwrap(),
    );
    let private = tempfile::Builder::new()
        .prefix("pu")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(private.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let standalone =
        peersus_net::grants::StandaloneGrants::open(root.path().join("grants.json")).unwrap();
    let (runtime, client) = Runtime::new(
        network,
        standalone,
        store,
        Registry::default(),
        private.path().to_path_buf(),
    )
    .unwrap();
    let installer = Arc::new(installer);
    let runtime = runtime
        // The extension store itself is not set up here: updates only borrow its way to fetch.
        .with_store(StoreService::new(None, data.path()), Arc::new(fetcher))
        .with_updates(
            UpdateService::new(
                key,
                "dev",
                Build::running(),
                installer.target(),
                data.path(),
            ),
            installer.clone(),
        );
    let task = tokio::spawn(async move {
        let exit = runtime.run().await;
        drop(root);
        exit
    });
    Owner {
        client,
        task,
        installer,
        _data: data,
        _private: private,
    }
}

async fn until(client: &Client, predicate: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    let mut updates = client.subscribe();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let snapshot = Arc::clone(&updates.borrow_and_update());
            if predicate(&snapshot) {
                return snapshot;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .expect("the expected state was never reached")
}

async fn status(owner: &Owner, wanted: UpdateStatus) -> Arc<Snapshot> {
    until(&owner.client, |snapshot| snapshot.update.status == wanted).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_is_found_unasked_and_each_further_step_waits_for_the_person() {
    let fetcher = MapFetcher::default();
    publish(&fetcher, "9.0.0", ARTIFACT, ARTIFACT);
    let owner = start(Some(key()), fetcher, Recorder::default(), |_| {}).await;
    // Looked for without being asked, verified, offered; and with both switches off, left there.
    let offered = status(&owner, UpdateStatus::Available).await;
    assert_eq!(offered.update.offered.as_deref(), Some("9.0.0 (build 1)"));
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        owner.client.snapshot().update.status,
        UpdateStatus::Available
    );
    assert!(owner.installer.given.lock().unwrap().is_empty());
    // Nothing is in place yet, so there is nothing to restart for.
    assert!(
        owner
            .client
            .command(Command::UpdateRestart {})
            .await
            .is_err()
    );

    owner
        .client
        .command(Command::UpdateInstall {})
        .await
        .unwrap();
    status(&owner, UpdateStatus::Installed).await;
    assert_eq!(
        *owner.installer.given.lock().unwrap(),
        [(ARTIFACT.to_vec(), false)]
    );
    // In place, and the owner goes on until it is told: a second install is refused meanwhile.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(!owner.task.is_finished());
    assert!(
        owner
            .client
            .command(Command::UpdateInstall {})
            .await
            .is_err()
    );
    assert!(owner.client.command(Command::UpdateCheck {}).await.is_err());
    assert_eq!(
        owner.client.snapshot().update.status,
        UpdateStatus::Installed
    );

    owner
        .client
        .command(Command::UpdateRestart {})
        .await
        .unwrap();
    // The windows are told why the owner goes before it goes.
    status(&owner, UpdateStatus::Restarting).await;
    let exit = tokio::time::timeout(Duration::from_secs(10), owner.task)
        .await
        .expect("the owner ends")
        .unwrap();
    assert_eq!(exit, Exit::Restart);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_both_switches_on_the_update_is_put_in_place_and_the_owner_ends_to_run_it() {
    let fetcher = MapFetcher::default();
    publish(&fetcher, "9.0.0", ARTIFACT, ARTIFACT);
    let owner = start(Some(key()), fetcher, Recorder::default(), |settings| {
        settings.update_automatically = true;
        settings.restart_after_update = true;
    })
    .await;
    let exit = tokio::time::timeout(Duration::from_secs(20), owner.task)
        .await
        .expect("the owner ends by itself")
        .unwrap();
    assert_eq!(exit, Exit::Restart);
    // Once, with exactly the signed bytes, and told that the person wants it started again.
    assert_eq!(
        *owner.installer.given.lock().unwrap(),
        [(ARTIFACT.to_vec(), true)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn with_only_the_first_switch_on_the_update_waits_in_place_for_a_restart() {
    let fetcher = MapFetcher::default();
    publish(&fetcher, "9.0.0", ARTIFACT, ARTIFACT);
    let owner = start(Some(key()), fetcher, Recorder::default(), |settings| {
        settings.update_automatically = true;
    })
    .await;
    status(&owner, UpdateStatus::Installed).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(!owner.task.is_finished());
    assert_eq!(owner.installer.given.lock().unwrap().len(), 1);
    // The second switch, turned on afterwards, is honoured without anything being pressed.
    owner
        .client
        .command(Command::PatchSettings {
            patch: Box::new(peersus_core::config::SettingsPatch {
                restart_after_update: Some(true),
                ..Default::default()
            }),
        })
        .await
        .unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), owner.task)
        .await
        .expect("the owner ends")
        .unwrap();
    assert_eq!(exit, Exit::Restart);
}

#[tokio::test(flavor = "multi_thread")]
async fn bytes_that_are_not_what_was_signed_never_reach_the_installer() {
    let fetcher = MapFetcher::default();
    // Same length, so only the digest can tell.
    let other: Vec<u8> = ARTIFACT.iter().map(|byte| byte ^ 1).collect();
    publish(&fetcher, "9.0.0", ARTIFACT, &other);
    let owner = start(Some(key()), fetcher, Recorder::default(), |settings| {
        settings.update_automatically = true;
        settings.restart_after_update = true;
    })
    .await;
    let failed = status(&owner, UpdateStatus::Failed).await;
    assert!(failed.update.error.is_some());
    // Not tried again in a loop, and nothing was put in place.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert!(owner.installer.given.lock().unwrap().is_empty());
    assert!(!owner.task.is_finished());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_installer_that_fails_is_said_once_and_the_application_goes_on() {
    let fetcher = MapFetcher::default();
    publish(&fetcher, "9.0.0", ARTIFACT, ARTIFACT);
    let installer = Recorder {
        refuses: Some("The update could not be written beside the application"),
        ..Recorder::default()
    };
    let owner = start(Some(key()), fetcher, installer, |settings| {
        settings.update_automatically = true;
        settings.restart_after_update = true;
    })
    .await;
    let failed = status(&owner, UpdateStatus::Failed).await;
    assert_eq!(
        failed.update.error.as_deref(),
        Some("The update could not be written beside the application")
    );
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert_eq!(owner.installer.given.lock().unwrap().len(), 1);
    assert!(!owner.task.is_finished());
    // The person can still ask: the offer is checked again and stands.
    owner.client.command(Command::UpdateCheck {}).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_key_or_with_another_key_nothing_is_offered_or_applied() {
    let fetcher = MapFetcher::default();
    publish(&fetcher, "9.0.0", ARTIFACT, ARTIFACT);
    let owner = start(None, fetcher.clone(), Recorder::default(), |settings| {
        settings.update_automatically = true;
    })
    .await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        owner.client.snapshot().update.status,
        UpdateStatus::NotConfigured
    );
    for command in [
        Command::UpdateCheck {},
        Command::UpdateInstall {},
        Command::UpdateRestart {},
    ] {
        assert!(owner.client.command(command).await.is_err());
    }
    assert!(owner.installer.given.lock().unwrap().is_empty());

    // A build that trusts another publisher: the release is authentic to someone else, and refused.
    let stranger = SigningKey::from_bytes(&[12; 32]).verifying_key().to_bytes();
    let owner = start(Some(stranger), fetcher, Recorder::default(), |settings| {
        settings.update_automatically = true;
    })
    .await;
    status(&owner, UpdateStatus::Failed).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(owner.installer.given.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_that_is_not_newer_is_not_an_update() {
    let fetcher = MapFetcher::default();
    publish(&fetcher, "0.0.1", ARTIFACT, ARTIFACT);
    let owner = start(Some(key()), fetcher, Recorder::default(), |settings| {
        settings.update_automatically = true;
        settings.restart_after_update = true;
    })
    .await;
    status(&owner, UpdateStatus::Current).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(owner.installer.given.lock().unwrap().is_empty());
    assert!(!owner.task.is_finished());
}
