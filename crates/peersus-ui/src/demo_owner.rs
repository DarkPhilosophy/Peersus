//! Owners that live only in this process, in a directory that is thrown away: what a demo needs to show an extension as it
//! really runs (`--demo-extension`), and what the window's own tests drive.
//!
//! Each has an identity made in memory (never the keyring), listens on the loopback address only, with no relay and no
//! discovery, and keeps its preferences, devices, grants and packages under the directory it is given. Nothing of the
//! person's is read or written, and no owner the person runs is reached.
use peersus_app::runtime::{Client, Reply};
use peersus_core::model::{Command, Snapshot};
use std::{path::Path, time::Duration};

/// How long a step waits for the owner to say it is done.
const WAIT: Duration = Duration::from_secs(20);

/// Start an owner named `name` in `root/name`, with the program's own helpers to run extensions when they are beside it.
pub(crate) async fn start(
    root: &Path,
    name: &str,
    held: peersus_net::held::Held,
) -> Result<(Client, tokio::task::JoinHandle<()>), &'static str> {
    use peersus_core::{Settings, config::NetworkSettings};
    use peersus_net::{
        grants::StandaloneGrants, history::History, network_host::NetworkHost, peers::Peers,
        settings::SettingsStore, transport,
    };
    const NOT_STARTED: &str = "The demo's own owner could not be started";
    let root = root.join(name);
    std::fs::create_dir_all(&root).map_err(|_| NOT_STARTED)?;
    let settings = Settings {
        network: NetworkSettings {
            bind: Some(std::net::Ipv4Addr::LOCALHOST.into()),
            allow_relay: false,
            ..NetworkSettings::default()
        },
        device_name: name.to_owned(),
        ..Settings::default()
    };
    let mut store = SettingsStore::open(root.join("preferences.json")).map_err(|_| NOT_STARTED)?;
    store.replace(settings.clone()).map_err(|_| NOT_STARTED)?;
    let endpoint = transport::bind(iroh::SecretKey::generate(), &settings.network)
        .await
        .map_err(|_| NOT_STARTED)?;
    let network = NetworkHost::new(
        endpoint,
        settings.device_name,
        Peers::open(root.join("peers.json")).map_err(|_| NOT_STARTED)?,
        History::open(root.join("history.json")).map_err(|_| NOT_STARTED)?,
    );
    let sockets = tempfile::tempdir().map_err(|_| NOT_STARTED)?;
    let (owner, client) = peersus_app::runtime::Runtime::new(
        network,
        StandaloneGrants::open(root.join("grants.json")).map_err(|_| NOT_STARTED)?,
        store,
        Default::default(),
        sockets.path().to_path_buf(),
    )
    .map_err(|_| NOT_STARTED)?;
    // The manager is given the data directory, as the application's own is: it keeps the store in `plugins/` and what the
    // person accepted beside it. Given `plugins/` itself, the store was `plugins/plugins` and the acceptance had no folder.
    let mut owner = owner.with_packages(peersus_app::plugin_manager::Manager::new(&root));
    // The helpers that run an extension's program are the ones beside this program, as for the application itself.
    if let Some(modules) = std::env::current_exe()
        .ok()
        .and_then(|program| peersus_plugin::module::DesktopModules::beside(&program).ok())
    {
        owner = owner.with_modules(std::sync::Arc::new(modules));
    }
    let task = tokio::spawn(async move {
        owner.with_held(held).run().await;
        drop(sockets);
    });
    Ok((client, task))
}

/// Wait until `client`'s snapshot passes `done`, and give what it found.
async fn until<T>(
    client: &Client,
    done: impl Fn(&Snapshot) -> Option<T>,
) -> Result<T, &'static str> {
    let mut changes = client.subscribe();
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(found) = done(&client.snapshot()) {
                return Ok(found);
            }
            changes
                .changed()
                .await
                .map_err(|_| "The demo's own owner stopped")?;
        }
    })
    .await
    .map_err(|_| "The demo's own owner did not answer in time")?
}

/// Pair two owners the way two people do: an invitation made by one, joined by the other, and the same number chosen on both.
pub(crate) async fn pair(a: &Client, b: &Client) -> Result<(), &'static str> {
    let Ok(Reply::Invitation(invitation)) = a.command(Command::CreateInvitation {}).await else {
        return Err("The demo's devices could not be paired");
    };
    b.command(Command::JoinInvitation { invitation }).await?;
    let prompt = |snapshot: &Snapshot| snapshot.pairing.first().cloned();
    let (on_a, on_b) = (until(a, prompt).await?, until(b, prompt).await?);
    let number = on_a.options[0];
    a.command(Command::ChoosePairing {
        request: on_a.id,
        number,
    })
    .await?;
    b.command(Command::ChoosePairing {
        request: on_b.id,
        number,
    })
    .await?;
    for client in [a, b] {
        until(client, |snapshot| {
            snapshot.peers.iter().any(|peer| peer.online).then_some(())
        })
        .await?;
    }
    Ok(())
}

/// Show the unpacked package in `source` to `client` as a package the person chose: the owner reads it and the window shows its
/// licences, notice and the warning of a package from outside the store, as for any install. Nothing is accepted or allowed here:
/// the person accepts on the window and allows what they want on its Permissions, as they would, so that the preview shows the
/// extension exactly as a new install with what was given and nothing else.
#[cfg(feature = "demo")]
pub(crate) async fn stage(client: &Client, source: &Path) -> Result<(), &'static str> {
    const NOT_A_PACKAGE: &str =
        "--demo-extension takes the folder of an unpacked extension package";
    let source = std::fs::canonicalize(source).map_err(|_| NOT_A_PACKAGE)?;
    let source = peersus_core::model::SourceDir::new(source).ok_or(NOT_A_PACKAGE)?;
    client.command(Command::StageInstall { source }).await?;
    until(client, |snapshot| {
        (!snapshot.staged.is_empty()).then_some(())
    })
    .await
}
