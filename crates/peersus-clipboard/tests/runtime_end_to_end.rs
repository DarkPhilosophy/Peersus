//! The clipboard plugin as a user gets it: installed in a plugins directory, found by the
//! application's own discovery, started by the real runtime under the real supervisor, and
//! used between two devices that really paired and really talk over the network.
//!
//! What this shows that the other tests do not: that the manifest on disk, the binary, the
//! permissions the user grants, the device the user chooses, the network stream that carries
//! the text and the clipboard of the machine all agree with each other. Each piece is tested
//! alone elsewhere; a wrong name or an unreached permission only shows when they are joined.
//!
//! IT MUST NEVER TOUCH THE USER'S OWN CLIPBOARD, so it only runs in a virtual X server it is
//! started in on purpose (see `isolated_desktop.rs`, whose guards this repeats):
//!
//! ```text
//! PEERSUS_ISOLATED_CLIPBOARD=1 xvfb-run -a env -u WAYLAND_DISPLAY \
//!     cargo test -p peersus-clipboard --test runtime_end_to_end
//! ```
// LINUX ONLY, on purpose. A virtual X server isolates the clipboard only where the library
// talks X11. On macOS it talks to the system pasteboard whatever `DISPLAY` says, so a test
// that wrote to "the clipboard" there would write to the user's real one.
#![cfg(target_os = "linux")]

use iroh::SecretKey;
use peersus_app::{
    discovery::discover,
    runtime::{Client, Reply, Runtime},
};
use peersus_clipboard::wire::{self, Item};
use peersus_core::{
    Settings,
    config::NetworkSettings,
    model::{ActionState, Command, PeerId, Snapshot},
    plugins::{Grants, PluginId},
};
use peersus_net::{
    history::History, network_host::NetworkHost, peers::Peers, settings::SettingsStore, transport,
};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command as Process, Stdio},
    sync::Arc,
    time::Duration,
};

/// `Some` only when it is certain that the clipboard in reach is a throwaway one.
fn isolated() -> Option<()> {
    if std::env::var("PEERSUS_ISOLATED_CLIPBOARD").as_deref() != Ok("1") {
        eprintln!("not run: PEERSUS_ISOLATED_CLIPBOARD=1 is not set");
        return None;
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        eprintln!("not run: WAYLAND_DISPLAY is set, so the real clipboard is in reach");
        return None;
    }
    let number: Option<u32> = std::env::var("DISPLAY")
        .unwrap_or_default()
        .strip_prefix(':')
        .and_then(|rest| rest.split('.').next())
        .and_then(|n| n.parse().ok());
    if !number.is_some_and(|n| n >= 99) {
        eprintln!("not run: DISPLAY is not a virtual server");
        return None;
    }
    Some(())
}

/// The scenarios share the one clipboard of the virtual server, so they cannot overlap.
static CLIPBOARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn id(name: &str) -> PluginId {
    PluginId::new(name).unwrap()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Install the plugin the way a user would: `<root>/clipboard/plugin.json` and the program the
/// manifest names, in directories only the user can change.
fn install(root: &Path) {
    let directory = root.join("clipboard");
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        directory.join("plugin.json"),
        include_str!("../plugin.json"),
    )
    .unwrap();
    // The file the shipped manifest names, so this follows what ships and not a name written here.
    let manifest: peersus_plugin_api::manifest::Manifest =
        serde_json::from_str(include_str!("../plugin.json")).unwrap();
    let program = directory.join(manifest.program_file());
    std::fs::copy(env!("CARGO_BIN_EXE_peersus-clipboard"), &program).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A short private directory: the sockets of plugin processes live in it.
fn sockets() -> tempfile::TempDir {
    let directory = tempfile::Builder::new()
        .prefix("pe")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

async fn start(root: &Path) -> (Client, tokio::task::JoinHandle<()>) {
    let plugins_dir = root.join("plugins");
    std::fs::create_dir(&plugins_dir).unwrap();
    install(&plugins_dir);
    let found = discover(&plugins_dir);
    assert!(found.problems.is_empty(), "{:?}", found.problems);
    assert_eq!(
        found.registry.len(),
        1,
        "the installed plugin was not found"
    );

    let path = root.join("a");
    std::fs::create_dir_all(&path).unwrap();
    let mut store = SettingsStore::open(path.join("preferences.json")).unwrap();
    let settings = Settings {
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
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
        Peers::open(path.join("peers.json")).unwrap(),
        History::open(path.join("history.json")).unwrap(),
    );
    let directory = sockets();
    let standalone = peersus_net::grants::StandaloneGrants::open(path.join("grants.json")).unwrap();
    let (runtime, client) = Runtime::new(
        network,
        standalone,
        store,
        found.registry,
        directory.path().to_path_buf(),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        runtime.run().await;
        drop(directory);
    });
    (client, task)
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

/// One stream the runtime opened to the second device: what it was for, and every byte so far.
#[derive(Clone, Debug)]
struct Received {
    service: String,
    version: u32,
    bytes: Vec<u8>,
}

/// A second device, paired with the runtime through the real ceremony, that records what the
/// runtime sends it and can send the runtime a plugin stream of its own.
struct Device {
    host: Arc<tokio::sync::Mutex<NetworkHost>>,
    peer_of_runtime: PeerId,
    received: Arc<std::sync::Mutex<Vec<Received>>>,
    _driver: tokio::task::JoinHandle<()>,
}

async fn paired_device(root: &Path, runtime: &Client) -> (Device, PeerId) {
    let policy = NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
    };
    let endpoint = transport::bind(SecretKey::generate(), &policy)
        .await
        .unwrap();
    let device_id = PeerId(endpoint.id().to_string());
    let host = NetworkHost::new(
        endpoint,
        "Second".into(),
        Peers::open(root.join("second-peers.json")).unwrap(),
        History::open(root.join("second-history.json")).unwrap(),
    );
    let host = Arc::new(tokio::sync::Mutex::new(host));
    let Reply::Invitation(invitation) =
        runtime.command(Command::CreateInvitation {}).await.unwrap()
    else {
        panic!("expected an invitation")
    };
    let invitation = peersus_net::invitation::Invitation::decode(&invitation, now()).unwrap();
    host.lock().await.join(invitation).unwrap();
    let received: Arc<std::sync::Mutex<Vec<Received>>> = Default::default();
    let (driven, collected) = (host.clone(), received.clone());
    let driver = tokio::spawn(async move {
        loop {
            let event = {
                let mut host = driven.lock().await;
                tokio::time::timeout(Duration::from_millis(20), host.next(now())).await
            };
            if let Ok(peersus_net::network_host::Event::Plugin {
                service,
                version,
                send,
                mut receive,
                ..
            }) = event
            {
                let collected = collected.clone();
                tokio::spawn(async move {
                    let _keep_open = send;
                    let mut bytes = Vec::new();
                    let mut chunk = vec![0u8; 8192];
                    loop {
                        match receive.read(&mut chunk).await {
                            Ok(Some(read)) if read > 0 => {
                                bytes.extend_from_slice(&chunk[..read]);
                                let mut all = collected.lock().unwrap();
                                all.retain(|r| r.service != service);
                                all.push(Received {
                                    service: service.clone(),
                                    version,
                                    bytes: bytes.clone(),
                                });
                            }
                            _ => break,
                        }
                    }
                });
            }
            tokio::task::yield_now().await;
        }
    });
    let prompt = until(runtime, |s| !s.pairing.is_empty()).await;
    let number = prompt.pairing[0].options[0];
    runtime
        .command(Command::ChoosePairing {
            request: prompt.pairing[0].id.clone(),
            number,
        })
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let waiting = host.lock().await.pairing();
        if let Some(prompt) = waiting.first() {
            host.lock()
                .await
                .choose(&prompt.id, number)
                .expect("the second device could not confirm");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no pairing prompt on the second device"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    until(runtime, |s| s.peers.iter().any(|p| p.online)).await;
    let peer = {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let known = host.lock().await.peers();
            if let Some(found) = known.iter().find(|p| p.online) {
                break found.id.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the second device never saw the runtime online"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    (
        Device {
            host,
            peer_of_runtime: peer,
            received,
            _driver: driver,
        },
        device_id,
    )
}

impl Device {
    /// Send the runtime a stream for `service` carrying `bytes`, as a device sharing would.
    async fn send_plugin_stream(&self, service: &str, version: u32, bytes: &[u8]) {
        let connection = self
            .host
            .lock()
            .await
            .connection(&self.peer_of_runtime)
            .expect("the second device has no connection to the runtime");
        let (mut send, _receive) = connection.open_bi().await.expect("open stream");
        peersus_net::streams::send(
            &mut send,
            &peersus_net::streams::Request::Plugin {
                service: service.into(),
                version,
            },
        )
        .await
        .expect("send request");
        send.write_all(bytes).await.expect("send bytes");
        send.finish().expect("finish");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    /// What arrived for `service`, once at least `at_least` bytes have.
    async fn received_for(&self, service: &str, at_least: usize) -> Option<Vec<u8>> {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let found = self
                .received
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.service == service)
                .map(|r| (r.version, r.bytes.clone()));
            if let Some((version, bytes)) = found
                && bytes.len() >= at_least
            {
                assert_eq!(version, 1);
                return Some(bytes);
            }
            if std::time::Instant::now() > deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// Grant the clipboard plugin permissions for one device, each written with its owner: its own
/// guard `clipboard.use` is the plugin's, and `peers.send` is the application's.
fn grant(
    client: &Client,
    peer: &PeerId,
    permissions: &[(&str, &str)],
) -> impl std::future::Future<Output = Result<Reply, &'static str>> {
    client.command(Command::SetGrants {
        plugin: id("clipboard"),
        peer: Some(peer.clone()),
        grants: Grants::new(permissions.iter().map(|(owner, name)| {
            peersus_plugin_api::permission::Permission::new(*owner, *name).unwrap()
        }))
        .unwrap(),
    })
}

/// Put text on the clipboard the way another application would.
fn put(text: &str) {
    use std::io::Write;
    let mut child = Process::new("xclip")
        .args(["-selection", "clipboard", "-i"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("xclip");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    std::mem::forget(child);
    std::thread::sleep(Duration::from_millis(300));
}

/// What another application would paste.
fn get() -> String {
    let out = Process::new("xclip")
        .args(["-selection", "clipboard", "-o"])
        .output()
        .expect("xclip");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn state_of(snapshot: &Snapshot, action: &str) -> Option<ActionState> {
    snapshot
        .actions
        .iter()
        .find(|a| a.plugin == id("clipboard") && a.action == action)
        .map(|a| a.state)
}

async fn running(client: &Client) {
    use peersus_core::model::PluginStatus;
    until(client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == id("clipboard") && p.status == PluginStatus::Running)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pressing_share_sends_the_clipboard_text_to_the_chosen_device_over_the_network() {
    if isolated().is_none() {
        return;
    }
    let _exclusive = CLIPBOARD.lock().await;
    let root = tempfile::tempdir().unwrap();
    let (runtime, task) = start(root.path()).await;
    running(&runtime).await;
    let (device, device_id) = paired_device(root.path(), &runtime).await;
    // The user decides, for this device, that the plugin may be used and may send.
    grant(
        &runtime,
        &device_id,
        &[("clipboard", "clipboard.use"), ("host", "peers.send")],
    )
    .await
    .unwrap();

    put("shared by pressing the button ✓");
    runtime
        .command(Command::InvokeAction {
            plugin: id("clipboard"),
            action: "share".into(),
            peer: Some(device_id.clone()),
        })
        .await
        .unwrap();

    let expected = wire::encode(&Item::Text("shared by pressing the button ✓".into())).unwrap();
    let arrived = device.received_for("clipboard.text", expected.len()).await;
    assert_eq!(
        arrived.as_deref(),
        Some(expected.as_slice()),
        "the device did not get the clipboard's text"
    );
    // And the user is told, in the application's own words, that it is done.
    until(&runtime, |s| {
        state_of(s, "share") == Some(ActionState::Done)
    })
    .await;
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn text_a_granted_device_sends_ends_up_on_the_clipboard() {
    if isolated().is_none() {
        return;
    }
    let _exclusive = CLIPBOARD.lock().await;
    let root = tempfile::tempdir().unwrap();
    let (runtime, task) = start(root.path()).await;
    running(&runtime).await;
    let (device, device_id) = paired_device(root.path(), &runtime).await;
    grant(&runtime, &device_id, &[("clipboard", "clipboard.use")])
        .await
        .unwrap();

    put("what was here before");
    let item = wire::encode(&Item::Text("sent by the other device ✓".into())).unwrap();
    device.send_plugin_stream("clipboard.text", 1, &item).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while get() != "sent by the other device ✓" {
        assert!(
            std::time::Instant::now() < deadline,
            "the text never reached the clipboard; it holds {:?}",
            get()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_was_not_granted_cannot_put_anything_on_the_clipboard() {
    if isolated().is_none() {
        return;
    }
    let _exclusive = CLIPBOARD.lock().await;
    let root = tempfile::tempdir().unwrap();
    let (runtime, task) = start(root.path()).await;
    running(&runtime).await;
    // Paired, online, and never granted anything.
    let (device, _device_id) = paired_device(root.path(), &runtime).await;

    put("must survive");
    let item = wire::encode(&Item::Text("an unwelcome text".into())).unwrap();
    device.send_plugin_stream("clipboard.text", 1, &item).await;
    // Time for it to arrive, were it going to.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(get(), "must survive");
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn automatic_copies_survive_restart_without_replaying_revoked_or_received_text() {
    if isolated().is_none() {
        return;
    }
    let _exclusive = CLIPBOARD.lock().await;
    let root = tempfile::tempdir().unwrap();
    let (runtime, task) = start(root.path()).await;
    running(&runtime).await;
    let (device, device_id) = paired_device(root.path(), &runtime).await;
    let permissions = [("clipboard", "clipboard.use"), ("host", "peers.send")];
    grant(&runtime, &device_id, &permissions).await.unwrap();
    for (setting, peer) in [("automatic", None), ("sync", Some(device_id.clone()))] {
        runtime
            .command(Command::SetPluginSetting {
                plugin: id("clipboard"),
                peer,
                setting: setting.into(),
                value: peersus_core::plugins::Value::Toggle(true),
            })
            .await
            .unwrap();
    }

    // Two polling turns acquire eligibility and the baseline. Nothing already on
    // the isolated clipboard is a new copy, including after a permission change.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(device.received.lock().unwrap().is_empty());
    for text in ["first automatic copy", "second automatic copy"] {
        device.received.lock().unwrap().clear();
        put(text);
        let expected = wire::encode(&Item::Text(text.into())).unwrap();
        assert_eq!(
            device.received_for("clipboard.text", expected.len()).await,
            Some(expected)
        );
    }

    grant(&runtime, &device_id, &[]).await.unwrap();
    device.received.lock().unwrap().clear();
    put("copied while the device was not authorized");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        device.received.lock().unwrap().is_empty(),
        "revoked device received clipboard text"
    );
    grant(&runtime, &device_id, &permissions).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        device.received.lock().unwrap().is_empty(),
        "grant restoration replayed old text"
    );

    runtime
        .command(Command::DisablePlugin {
            plugin: id("clipboard"),
        })
        .await
        .unwrap();
    until(&runtime, |snapshot| {
        snapshot.plugins.iter().any(|plugin| {
            plugin.id == id("clipboard")
                && plugin.status != peersus_core::model::PluginStatus::Running
        })
    })
    .await;
    put("copied while the plugin was stopped");
    runtime
        .command(Command::EnablePlugin {
            plugin: id("clipboard"),
        })
        .await
        .unwrap();
    running(&runtime).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        device.received.lock().unwrap().is_empty(),
        "plugin restart replayed old text"
    );

    // Remote writes must not loop back, but a later local copy still uses the
    // persisted automatic and per-device opt-ins without another UI action.
    let incoming = "fresh text from the paired device";
    device
        .send_plugin_stream(
            "clipboard.text",
            1,
            &wire::encode(&Item::Text(incoming.into())).unwrap(),
        )
        .await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while get() != incoming {
        assert!(
            std::time::Instant::now() < deadline,
            "remote text was not applied"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        device.received.lock().unwrap().is_empty(),
        "received clipboard text was echoed"
    );
    put("new local copy after restart");
    let expected = wire::encode(&Item::Text("new local copy after restart".into())).unwrap();
    assert_eq!(
        device.received_for("clipboard.text", expected.len()).await,
        Some(expected)
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}
