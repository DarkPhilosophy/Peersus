//! Isolated store UX demo. Only compiled with the existing test-plugin feature.
//! Uses public, signed test fixtures; never changes the product's trust root.
use ed25519_dalek::SigningKey;
use peersus_app::{
    instance::Instance, lifetime::Presence, plugin_host::Registry, plugin_manager::Manager,
    runtime::Runtime, store_runtime::fake::MapFetcher, store_service::StoreService,
};
use peersus_core::{Settings, config::NetworkSettings};
use peersus_net::{
    grants::StandaloneGrants, history::History, network_host::NetworkHost, peers::Peers,
    settings::SettingsStore, transport,
};
use std::{path::PathBuf, sync::Arc};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let root = arguments.next().map(PathBuf::from).ok_or(
        "Usage: store_demo NEW_DATA_ROOT (launch the UI with XDG_DATA_HOME set to this root)",
    )?;
    if arguments.next().is_some() || root.exists() {
        return Err(
            "Choose a new, empty demo data root; existing application data is never used".into(),
        );
    }
    std::fs::create_dir_all(&root)?;
    let directory = root.join("peersus");
    let mut instance = Instance::acquire(&directory)?.ok_or("Demo owner already running")?;
    let mut settings = SettingsStore::open(directory.join("preferences.json"))?;
    let preferences = Settings {
        device_name: "Store demo — public test catalog".into(),
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse()?),
            allow_relay: false,
            ..NetworkSettings::default()
        },
        ..Settings::default()
    };
    settings.replace(preferences.clone())?;
    // Disposable network identity, held only in memory: no system keyring access.
    let endpoint = transport::bind(iroh::SecretKey::generate(), &preferences.network).await?;
    let network = NetworkHost::new(
        endpoint,
        preferences.device_name,
        Peers::open(directory.join("peers.json"))?,
        History::open(directory.join("history.json"))?,
    );
    let sockets = tempfile::Builder::new().prefix("store-demo-").tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(sockets.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    let (runtime, client) = Runtime::new(
        network,
        StandaloneGrants::open(directory.join("grants.json"))?,
        settings,
        Registry::default(),
        sockets.path().to_path_buf(),
    )?;
    let fetcher = MapFetcher::default();
    fetcher.put(
        "catalog.json",
        include_bytes!("../tests/fixtures/catalog/catalog.json").to_vec(),
    );
    fetcher.put(
        "catalog.sig",
        include_bytes!("../tests/fixtures/catalog/catalog.sig").to_vec(),
    );
    fetcher.put(
        "echo.tar.gz",
        include_bytes!("../tests/fixtures/catalog/echo.tar.gz").to_vec(),
    );
    // This is the public, committed TEST seed used by the existing fixture tests,
    // not a project signing credential. No new key or catalog is generated.
    let public_key = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes();
    // Cargo puts examples one directory below the normal application helpers.
    let executable = std::env::current_exe()?;
    let application_directory = executable
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or("Cannot locate the built application helpers")?;
    if !application_directory
        .join(format!(
            "peersus-native-host{}",
            std::env::consts::EXE_SUFFIX
        ))
        .is_file()
    {
        return Err(
            "Build the release workspace, including the native host, before running this demo"
                .into(),
        );
    }
    let modules = peersus_plugin::module::DesktopModules::beside(
        &application_directory.join(format!("peersus{}", std::env::consts::EXE_SUFFIX)),
    )?;
    let runtime = runtime
        .with_modules(Arc::new(modules))
        .with_packages(Manager::new(&directory))
        .with_store(
            StoreService::new(Some(public_key), &directory),
            Arc::new(fetcher),
        );
    let listener = instance.listen()?;
    let mut owner = tokio::spawn(runtime.run());
    let control = tokio::spawn(peersus_app::ipc::serve(
        listener,
        client.clone(),
        Presence::default(),
    ));
    println!(
        "Store demo ready. Open Settings > Extensions > Store and refresh. No production catalog or personal data is used."
    );
    tokio::select! {
        result = &mut owner => { result?; }
        signal = tokio::signal::ctrl_c() => {
            signal?;
            client
                .command(peersus_core::model::Command::Shutdown {})
                .await?;
            owner.await?;
        }
    }
    control.abort();
    let _ = control.await;
    Ok(())
}
