//! The extension store through the REAL owner: commands in, snapshots out, a real package store on disk. Run with:
//!   cargo test -p peersus-app --features test-plugin --test store
//! Without the feature the whole file is skipped (the in-memory fetcher is only built for tests), so check that the
//! count printed is not 0.
//!
//! The only stand-in is where bytes come from (an in-memory fetcher), because the download is the one
//! step that needs a network. Everything that TRUSTS or INSTALLS is real: the signed catalog (made by the
//! real `cargo xtask catalog-sign` from a package the real CI validator accepted), the rollback floor on
//! disk, the unpacker, the manifest and embedded-binary metadata checks, and `Manager::install`.
#![cfg(all(unix, feature = "test-plugin"))]

use ed25519_dalek::SigningKey;
use iroh::SecretKey;
use peersus_app::{
    plugin_host::Registry,
    runtime::{Client, Runtime},
    store_runtime::fake::MapFetcher,
    store_service::StoreService,
};
use peersus_core::{
    Settings,
    config::NetworkSettings,
    model::{Command, Snapshot, StoreStatus},
    plugins::PluginId,
};
use peersus_net::{
    history::History, network_host::NetworkHost, peers::Peers, settings::SettingsStore, transport,
};
use std::{path::Path, sync::Arc, time::Duration};

const CATALOG: &[u8] = include_bytes!("fixtures/catalog/catalog.json");
const SIGNATURE: &[u8] = include_bytes!("fixtures/catalog/catalog.sig");
const ARCHIVE: &[u8] = include_bytes!("fixtures/catalog/echo.tar.gz");

fn echo() -> PluginId {
    PluginId::new("echo").unwrap()
}

/// The public half of the throwaway seed the fixtures were signed with.
fn test_key() -> [u8; 32] {
    SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes()
}

fn served() -> MapFetcher {
    let fetcher = MapFetcher::default();
    fetcher.put("catalog.json", CATALOG.to_vec());
    fetcher.put("catalog.sig", SIGNATURE.to_vec());
    fetcher.put("echo.tar.gz", ARCHIVE.to_vec());
    fetcher
}

struct Owner {
    client: Client,
    /// The application's data directory: the package store and the rollback floor live here.
    data: tempfile::TempDir,
    _task: tokio::task::JoinHandle<()>,
    _private: tempfile::TempDir,
}

/// An owner started as the program starts one: a package store, and (when given) the extension store.
async fn start(
    key: Option<[u8; 32]>,
    fetcher: MapFetcher,
    data: Option<tempfile::TempDir>,
) -> Owner {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let data = data.unwrap_or_else(|| tempfile::tempdir().unwrap());
    let mut store = SettingsStore::open(root.path().join("preferences.json")).unwrap();
    let settings = Settings {
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        },
        ..Settings::default()
    };
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
        .prefix("pt")
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
    let runtime = runtime
        .with_packages(peersus_app::plugin_manager::Manager::new(data.path()))
        .with_store(StoreService::new(key, data.path()), Arc::new(fetcher));
    let task = tokio::spawn(async move {
        runtime.run().await;
        drop(root);
    });
    Owner {
        client,
        data,
        _task: task,
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

/// Refresh and wait until the store settles, either way.
async fn refresh(owner: &Owner) -> Arc<Snapshot> {
    owner
        .client
        .command(Command::StoreRefresh {})
        .await
        .expect("a refresh starts");
    until(&owner.client, |s| {
        matches!(s.store.status, StoreStatus::Ready | StoreStatus::Failed)
    })
    .await
}

fn installed(data: &Path, name: &str) -> bool {
    data.join("plugins")
        .join(name)
        .join("plugin.json")
        .is_file()
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_maintainer_key_the_store_is_not_set_up_and_refuses_everything() {
    let owner = start(None, served(), None).await;
    assert_eq!(
        owner.client.snapshot().store.status,
        StoreStatus::NotConfigured
    );
    assert!(
        owner
            .client
            .command(Command::StoreRefresh {})
            .await
            .is_err()
    );
    assert!(
        owner
            .client
            .command(Command::StoreInstall { plugin: echo() })
            .await
            .is_err()
    );
    assert!(!installed(owner.data.path(), "echo"));
    assert_eq!(
        owner.client.snapshot().store.status,
        StoreStatus::NotConfigured,
        "a refusal must not invite a retry"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_catalog_lists_what_the_maintainer_approved_with_author_and_link() {
    let owner = start(Some(test_key()), served(), None).await;
    let snapshot = refresh(&owner).await;
    assert_eq!(
        snapshot.store.status,
        StoreStatus::Ready,
        "{:?}",
        snapshot.store.error
    );
    let [listing] = snapshot.store.listings.as_slice() else {
        panic!("{:?}", snapshot.store.listings)
    };
    assert_eq!(listing.plugin, echo());
    assert_eq!(
        (listing.version.as_str(), listing.author.as_str()),
        ("1.0.0", "Someone")
    );
    assert_eq!(listing.link, "https://example.org/echo");
    assert_eq!(listing.installed_version, None, "nothing is installed yet");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_install_verifies_installs_and_grants_nothing() {
    let owner = start(Some(test_key()), served(), None).await;
    refresh(&owner).await;
    owner
        .client
        .command(Command::StoreInstall { plugin: echo() })
        .await
        .expect("the install starts");
    let done = until(&owner.client, |s| {
        s.store.status == StoreStatus::Ready
            && s.store
                .listings
                .first()
                .is_some_and(|l| l.installed_version.is_some())
    })
    .await;
    assert_eq!(
        done.store.listings[0].installed_version.as_deref(),
        Some("1.0.0")
    );
    // It is a real installed package, listed with who made it, and the store's own card is not a staged card.
    assert!(installed(owner.data.path(), "echo"));
    let info = done
        .plugins
        .iter()
        .find(|p| p.id == echo())
        .expect("listed among the plugins");
    let package = info
        .package
        .as_ref()
        .expect("its author and link reach the window");
    assert_eq!(
        (package.author.as_str(), package.link.as_str()),
        ("Someone", "https://example.org/echo")
    );
    assert!(
        done.staged.is_empty(),
        "a store install must not leave a card waiting for a click"
    );
    // Nothing was granted: an installed extension starts with every permission refused.
    assert!(
        done.grants.get(&echo()).is_none_or(|g| g.is_empty()),
        "{:?}",
        done.grants.get(&echo())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_that_is_not_the_approved_one_is_refused_and_the_store_is_usable_again() {
    let fetcher = served();
    let owner = start(Some(test_key()), fetcher.clone(), None).await;
    refresh(&owner).await;
    // The approved archive with only its gzip timestamp changed: the SAME length, and it unpacks to the very same
    // package, so the fetch size cap, the unpacker and the package checks all accept it. Only the pinned SHA-256
    // can refuse it, so reaching a refusal here proves the digest is checked in the real owner's install path.
    // (A corrupted or longer archive would be stopped by an earlier layer and prove nothing about the digest.)
    let mut altered = ARCHIVE.to_vec();
    altered[4] ^= 1;
    assert_ne!(altered, ARCHIVE);
    fetcher.put("echo.tar.gz", altered);
    owner
        .client
        .command(Command::StoreInstall { plugin: echo() })
        .await
        .unwrap();
    let failed = until(&owner.client, |s| s.store.status == StoreStatus::Failed).await;
    assert!(failed.store.error.is_some());
    assert!(
        !installed(owner.data.path(), "echo"),
        "nothing of an unapproved package may be installed"
    );
    assert_eq!(failed.plugins.iter().filter(|p| p.id == echo()).count(), 0);
    // Not stuck busy: serving the real archive again, the same click now works.
    fetcher.put("echo.tar.gz", ARCHIVE.to_vec());
    owner
        .client
        .command(Command::StoreInstall { plugin: echo() })
        .await
        .expect("released after the refusal");
    until(&owner.client, |s| {
        s.store
            .listings
            .first()
            .is_some_and(|l| l.installed_version.is_some())
    })
    .await;
    assert!(installed(owner.data.path(), "echo"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_the_maintainer_never_approved_is_refused_before_anything_is_fetched() {
    let owner = start(Some(test_key()), served(), None).await;
    refresh(&owner).await;
    let stranger = PluginId::new("unlisted").unwrap();
    assert!(
        owner
            .client
            .command(Command::StoreInstall { plugin: stranger })
            .await
            .is_err()
    );
    assert!(!installed(owner.data.path(), "unlisted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_signed_catalog_is_refused_even_after_the_owner_restarts() {
    let data = tempfile::tempdir().unwrap();
    let first = start(Some(test_key()), served(), Some(data)).await;
    refresh(&first).await;
    // The floor is on disk, in the application's data directory. Raise it past the catalog the store serves.
    std::fs::write(first.data.path().join("store-sequence"), "5\n").unwrap();
    let data = first.data;
    drop(first.client);
    // A restarted owner over the same data directory and the same (now older) catalog.
    let second = start(Some(test_key()), served(), Some(data)).await;
    let failed = refresh(&second).await;
    assert_eq!(
        failed.store.status,
        StoreStatus::Failed,
        "sequence 1 is below the saved 5"
    );
    assert!(
        failed.store.listings.is_empty(),
        "an older catalog must never become the listing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_waiting_for_the_persons_confirmation_is_not_replaced_by_a_store_install() {
    let owner = start(Some(test_key()), served(), None).await;
    refresh(&owner).await;
    // The person stages the very same archive by hand: it waits for THEIR confirmation.
    let scratch = tempfile::tempdir().unwrap();
    let archive = scratch.path().join("echo.tar.gz");
    std::fs::write(&archive, ARCHIVE).unwrap();
    let source = peersus_core::model::SourceDir::new(&archive).unwrap();
    owner
        .client
        .command(Command::StageInstall { source })
        .await
        .expect("staged by hand");
    until(&owner.client, |s| s.staged.iter().any(|p| p.id == echo())).await;
    assert!(
        owner
            .client
            .command(Command::StoreInstall { plugin: echo() })
            .await
            .is_err()
    );
    assert!(
        owner
            .client
            .snapshot()
            .staged
            .iter()
            .any(|p| p.id == echo()),
        "the person's card is still there"
    );
}

const CATALOG_2: &[u8] = include_bytes!("fixtures/catalog-v2/catalog.json");
const SIGNATURE_2: &[u8] = include_bytes!("fixtures/catalog-v2/catalog.sig");
const ARCHIVE_2: &[u8] = include_bytes!("fixtures/catalog-v2/echo.tar.gz");

/// Serve the catalog the maintainer published AFTER the first one (echo 1.0.1, sequence 2).
fn publish_the_update(fetcher: &MapFetcher) {
    fetcher.put("catalog.json", CATALOG_2.to_vec());
    fetcher.put("catalog.sig", SIGNATURE_2.to_vec());
    fetcher.put("echo.tar.gz", ARCHIVE_2.to_vec());
}

async fn installed_version(owner: &Owner) -> Option<String> {
    owner
        .client
        .snapshot()
        .store
        .listings
        .iter()
        .find(|l| l.plugin == echo())
        .and_then(|l| l.installed_version.clone())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_replaces_the_installed_version_and_the_listing_follows() {
    let fetcher = served();
    let owner = start(Some(test_key()), fetcher.clone(), None).await;
    refresh(&owner).await;
    owner
        .client
        .command(Command::StoreInstall { plugin: echo() })
        .await
        .unwrap();
    until(&owner.client, |s| {
        s.store
            .listings
            .first()
            .is_some_and(|l| l.installed_version.as_deref() == Some("1.0.0"))
    })
    .await;
    let program = owner.data.path().join("plugins/echo/echo.so");
    let before = std::fs::read(&program).unwrap();

    // The maintainer publishes 1.0.1: after a refresh the row offers 1.0.1 while 1.0.0 is installed (the Update case).
    publish_the_update(&fetcher);
    let after_refresh = refresh(&owner).await;
    let row = &after_refresh.store.listings[0];
    assert_eq!(
        (row.version.as_str(), row.installed_version.as_deref()),
        ("1.0.1", Some("1.0.0"))
    );

    owner
        .client
        .command(Command::StoreInstall { plugin: echo() })
        .await
        .expect("an update is the same click");
    let done = until(&owner.client, |s| {
        s.store
            .listings
            .first()
            .is_some_and(|l| l.installed_version.as_deref() == Some("1.0.1"))
    })
    .await;
    assert_ne!(
        std::fs::read(&program).unwrap(),
        before,
        "the program on disk is the new one"
    );
    let package = done
        .plugins
        .iter()
        .find(|p| p.id == echo())
        .and_then(|p| p.package.clone())
        .expect("package metadata shown");
    assert_eq!(
        (package.version.as_str(), package.author.as_str()),
        ("1.0.1", "Someone")
    );
    assert!(done.staged.is_empty());
    assert!(
        done.grants.get(&echo()).is_none_or(|g| g.is_empty()),
        "a replacement starts with nothing allowed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_an_installed_extension_takes_it_out_and_the_store_can_install_it_again() {
    let owner = start(Some(test_key()), served(), None).await;
    refresh(&owner).await;
    owner
        .client
        .command(Command::StoreInstall { plugin: echo() })
        .await
        .unwrap();
    until(&owner.client, |s| {
        s.store
            .listings
            .first()
            .is_some_and(|l| l.installed_version.is_some())
    })
    .await;
    assert!(installed(owner.data.path(), "echo"));

    owner
        .client
        .command(Command::RemovePackage { plugin: echo() })
        .await
        .expect("removed");
    let after = until(&owner.client, |s| {
        s.store
            .listings
            .first()
            .is_some_and(|l| l.installed_version.is_none())
    })
    .await;
    assert!(!installed(owner.data.path(), "echo"), "the folder is gone");
    assert!(
        after.plugins.iter().all(|p| p.id != echo()),
        "and it is not listed as installed"
    );
    assert_eq!(installed_version(&owner).await, None);
    // Gone for good, not blocked: the same one-click install works again.
    owner
        .client
        .command(Command::StoreInstall { plugin: echo() })
        .await
        .expect("installable again");
    until(&owner.client, |s| {
        s.store
            .listings
            .first()
            .is_some_and(|l| l.installed_version.as_deref() == Some("1.0.0"))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_older_catalog_cannot_bring_an_old_version_back_after_an_update() {
    let fetcher = served();
    let owner = start(Some(test_key()), fetcher.clone(), None).await;
    refresh(&owner).await;
    publish_the_update(&fetcher);
    refresh(&owner).await;
    owner
        .client
        .command(Command::StoreInstall { plugin: echo() })
        .await
        .unwrap();
    until(&owner.client, |s| {
        s.store
            .listings
            .first()
            .is_some_and(|l| l.installed_version.as_deref() == Some("1.0.1"))
    })
    .await;
    // Someone serves the OLD signed catalog again (a replay): sequence 1 is below the saved 2.
    fetcher.put("catalog.json", CATALOG.to_vec());
    fetcher.put("catalog.sig", SIGNATURE.to_vec());
    fetcher.put("echo.tar.gz", ARCHIVE.to_vec());
    let replayed = refresh(&owner).await;
    assert_eq!(replayed.store.status, StoreStatus::Failed);
    assert_eq!(
        installed_version(&owner).await.as_deref(),
        Some("1.0.1"),
        "what is installed is untouched"
    );
    assert_eq!(
        std::fs::read_to_string(owner.data.path().join("store-sequence"))
            .unwrap()
            .trim(),
        "2"
    );
}
