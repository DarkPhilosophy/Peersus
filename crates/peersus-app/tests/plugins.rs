//! The plugin side of the application, tested with a real plugin process.
//!
//! Each test starts a real runtime, which starts the test plugin as its own process under
//! the real supervisor and the real hub, and drives it through the same commands a user
//! interface or the command line would send. Run with:
//!   cargo test -p peersus-app --features test-plugin --test plugins
//! Without the feature the whole file is skipped, so check that the count printed is not 0.
#![cfg(all(unix, feature = "test-plugin"))]

use iroh::SecretKey;
use peersus_app::{
    plugin_host::{PluginSpec, Registry},
    runtime::{Client, Reply, Runtime},
};
use peersus_core::{
    Settings,
    config::NetworkSettings,
    model::{Command, PeerId, Snapshot},
    plugins::{Grants, PluginId},
};
use peersus_net::{
    history::History, network_host::NetworkHost, peers::Peers, settings::SettingsStore, transport,
};
use peersus_plugin_api::{
    contributions::{Action, Contributions, Kind, Scope, Setting, Source},
    hub::HOST,
    manifest::{Export, Manifest, Member},
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

fn plugin_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_peersus-test-plugin"))
}

fn id(name: &str) -> PluginId {
    PluginId::new(name).unwrap()
}

fn permission(owner: &str, name: &str) -> peersus_plugin_api::permission::Permission {
    peersus_plugin_api::permission::Permission::new(owner, name).unwrap()
}

/// Grant `plugin` the APPLICATION's permission `name` for `peer`: what a plugin asking the
/// application to do something holds, as opposed to one of the plugin's own guards.
fn grant_host(
    client: &Client,
    plugin: &str,
    peer: &PeerId,
    names: &[&str],
) -> impl std::future::Future<Output = Result<Reply, &'static str>> {
    client.command(Command::SetGrants {
        plugin: id(plugin),
        peer: Some(peer.clone()),
        grants: Grants::new(names.iter().map(|name| permission(HOST, name))).unwrap(),
    })
}

/// Capabilities, each described: a name alone is refused.
fn capabilities(values: &[&str]) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|c| ((*c).to_owned(), format!("Lets the plugin use {c}.")))
        .collect()
}

/// A member that needs `capability` (or nothing) and says what it does.
fn member(capability: Option<&str>) -> Member {
    Member {
        capability: capability.map(str::to_owned),
        description: "Does what its name says.".to_owned(),
    }
}

/// A plugin that offers a guarded device action (`share`, needs capability `<name>.use`) and
/// an unguarded global one (`status`), both open to the application.
fn spec(name: &str) -> PluginSpec {
    let service = format!("{name}.service");
    let capability = format!("{name}.use");
    PluginSpec {
        id: id(name),
        manifest: Manifest {
            capabilities: capabilities(&[&capability]),
            exports: vec![Export {
                service: service.clone(),
                version: 1,
                description: format!("What {name} offers."),
                consumers: [HOST.to_owned()].into(),
                methods: BTreeMap::from([
                    ("share".to_owned(), member(Some(&capability))),
                    ("status".to_owned(), member(None)),
                ]),
                events: BTreeMap::new(),
            }],
            contributions: Contributions {
                actions: vec![
                    Action {
                        id: "share".into(),
                        label: "Share".into(),
                        short_label: None,
                        target: Source {
                            service: service.clone(),
                            method: "share".into(),
                        },
                        capability: Some(capability),
                        needs_device: true,
                        destructive: false,
                        timeout_ms: peersus_plugin_api::contributions::default_action_timeout(),
                    },
                    Action {
                        id: "status".into(),
                        label: "Status".into(),
                        short_label: None,
                        target: Source {
                            service,
                            method: "status".into(),
                        },
                        capability: None,
                        needs_device: false,
                        destructive: false,
                        timeout_ms: peersus_plugin_api::contributions::default_action_timeout(),
                    },
                ],
                ..Contributions::default()
            },
            ..Manifest::default()
        },
        program: Some(plugin_binary()),
        arguments: Vec::new(),
        restart_policy: None,
    }
}

fn registry(names: &[&str]) -> Registry {
    let mut registry = Registry::default();
    for name in names {
        registry.register(spec(name)).unwrap();
    }
    registry
}

/// Like [`registry`] for one plugin, but the process also records what it observes in
/// `log` and leaves calls to `share` open, so a test can see them arrive and be withdrawn.
fn logging_registry(name: &str, log: &std::path::Path) -> Registry {
    let mut registry = Registry::default();
    let mut logging = spec(name);
    logging.arguments = vec![log.display().to_string()];
    registry.register(logging).unwrap();
    registry
}

/// Two logging plugins, each declaring a global setting `volume` (a number from 0 to 10).
fn two_logging_registry(alpha_log: &std::path::Path, beta_log: &std::path::Path) -> Registry {
    let mut registry = Registry::default();
    for (name, log) in [("alpha", alpha_log), ("beta", beta_log)] {
        let mut plugin = spec(name);
        plugin.arguments = vec![log.display().to_string()];
        plugin.manifest.contributions.settings.push(Setting {
            id: "volume".into(),
            label: "Volume".into(),
            help: None,
            scope: Scope::Global,
            kind: Kind::Number {
                min: 0,
                max: 10,
                default: 5,
            },
        });
        registry.register(plugin).unwrap();
    }
    registry
}

/// Lines the plugin process has written so far, or none if it has not written yet.
fn observed(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Wait until the plugin's log satisfies `condition`, or fail with what it did contain.
async fn log_reaches(log: &std::path::Path, condition: impl Fn(&[String]) -> bool) -> Vec<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let lines = observed(log);
        if condition(&lines) {
            return lines;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the plugin never logged what was expected; it logged {lines:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A short private directory: the sockets of plugin processes live in it.
fn sockets() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::Builder::new()
        .prefix("pt")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

async fn start(
    root: &std::path::Path,
    name: &str,
    plugins: Registry,
) -> (Client, tokio::task::JoinHandle<()>) {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    let mut store = SettingsStore::open(path.join("preferences.json")).unwrap();
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
        Peers::open(path.join("peers.json")).unwrap(),
        History::open(path.join("history.json")).unwrap(),
    );
    let directory = sockets();
    let standalone = peersus_net::grants::StandaloneGrants::open(path.join("grants.json")).unwrap();
    let (runtime, client) = Runtime::new(
        network,
        standalone,
        store,
        plugins,
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
    tokio::time::timeout(Duration::from_secs(15), async {
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

/// Pair two runtimes through the real three-circle ceremony and return the peer id, as `a`
/// knows it, of the device that was just paired.
///
/// Devices `a` already knew are remembered first, because "some device is online" is
/// already true when a second one is added, and the first one is not the one just paired.
async fn pair(a: &Client, b: &Client) -> PeerId {
    let known: Vec<PeerId> = a.snapshot().peers.iter().map(|p| p.id.clone()).collect();
    let Reply::Invitation(invitation) = b.command(Command::CreateInvitation {}).await.unwrap()
    else {
        panic!("Expected invitation")
    };
    a.command(Command::JoinInvitation { invitation })
        .await
        .unwrap();
    let pa = until(a, |s| !s.pairing.is_empty()).await;
    let pb = until(b, |s| !s.pairing.is_empty()).await;
    let number = pa.pairing[0].options[0];
    for (client, prompt) in [(a, &pa), (b, &pb)] {
        client
            .command(Command::ChoosePairing {
                request: prompt.pairing[0].id.clone(),
                number,
            })
            .await
            .unwrap();
    }
    let state = until(a, |s| {
        s.peers.iter().any(|p| p.online && !known.contains(&p.id))
    })
    .await;
    until(b, |s| s.peers.iter().any(|p| p.online)).await;
    state
        .peers
        .iter()
        .find(|p| p.online && !known.contains(&p.id))
        .map(|p| p.id.clone())
        .expect("the device that was just paired")
}

async fn shutdown<const N: usize>(clients: [(Client, tokio::task::JoinHandle<()>); N]) {
    for (client, task) in clients {
        client.command(Command::Shutdown {}).await.unwrap();
        task.await.unwrap();
    }
}

fn invoke(
    client: &Client,
    plugin: &str,
    action: &str,
    peer: Option<PeerId>,
) -> impl std::future::Future<Output = Result<Reply, &'static str>> {
    client.command(Command::InvokeAction {
        plugin: id(plugin),
        action: action.into(),
        peer,
    })
}

/// Grant `plugin` the names of ITS OWN guards for `peer`: each is a permission of that plugin,
/// which is what an action or `receive` of the plugin is guarded by.
fn grant(
    client: &Client,
    plugin: &str,
    peer: &PeerId,
    capabilities: &[&str],
) -> impl std::future::Future<Output = Result<Reply, &'static str>> {
    client.command(Command::SetGrants {
        plugin: id(plugin),
        peer: Some(peer.clone()),
        grants: Grants::new(
            capabilities.iter().map(|name| {
                peersus_plugin_api::permission::Permission::new(plugin, *name).unwrap()
            }),
        )
        .unwrap(),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_process_shows_as_running_once_it_is_connected() {
    use peersus_core::model::PluginStatus;
    let root = tempfile::tempdir().unwrap();
    let (a, task_a) = start(root.path(), "a", registry(&["alpha"])).await;
    // The snapshot is taken as the runtime starts, before the process has connected, so it
    // must not already claim the plugin works.
    let first = a.snapshot();
    assert_eq!(first.plugins[0].status, PluginStatus::Stopped);
    let running = until(&a, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == id("alpha") && p.status == PluginStatus::Running)
    })
    .await;
    assert_eq!(running.plugins.len(), 1);
    a.command(Command::Shutdown {}).await.unwrap();
    task_a.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_cannot_be_launched_ends_up_failed_and_the_user_is_told() {
    use peersus_core::model::PluginStatus;
    let root = tempfile::tempdir().unwrap();
    let mut broken = Registry::default();
    let mut missing = spec("ghost");
    missing.program = Some(PathBuf::from("/nonexistent/peersus-plugin"));
    // The default policy waits seconds between attempts, which is right for a real plugin
    // and far too slow to watch in a test. A plugin may carry its own policy; this one
    // gives up quickly.
    missing.restart_policy = Some(peersus_plugin_host::supervisor::Policy {
        first_delay: Duration::from_millis(20),
        max_delay: Duration::from_millis(50),
        max_failures: 2,
        window: Duration::from_secs(60),
        healthy_after: Duration::from_secs(10),
    });
    broken.register(missing).unwrap();
    let (a, task_a) = start(root.path(), "a", broken).await;
    // The state is watched, not every step on the way to it, so this shows where it ends
    // up: failed, with the user told in fixed words. A program that does not exist can
    // never connect, which is why it cannot have been running.
    let failed = until(&a, |s| {
        s.plugins.iter().any(|p| p.status == PluginStatus::Failed)
    })
    .await;
    assert_eq!(failed.plugins[0].status, PluginStatus::Failed);
    assert!(
        failed
            .errors
            .iter()
            .any(|e| e == "A plugin stopped working and was turned off"),
        "the user is told, in fixed words, that it was turned off"
    );
    a.command(Command::Shutdown {}).await.unwrap();
    task_a.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guarded_action_is_refused_until_the_user_grants_it_and_works_after() {
    use peersus_core::model::PluginStatus;
    let root = tempfile::tempdir().unwrap();
    let (a, task_a) = start(root.path(), "a", registry(&["alpha"])).await;
    let (b, task_b) = start(root.path(), "b", Registry::default()).await;
    let peer = pair(&a, &b).await;

    // Paired, nothing granted: the device layer refuses before anything reaches the plugin.
    assert_eq!(
        invoke(&a, "alpha", "share", Some(peer.clone())).await.err(),
        Some("Action is not permitted for this device")
    );
    // Granting a capability the plugin never declared is refused, and so is an unknown plugin.
    assert!(grant(&a, "alpha", &peer, &["alpha.admin"]).await.is_err());
    assert!(grant(&a, "missing", &peer, &["x.use"]).await.is_err());

    // The user's decision. Only now can the hub let the application call the plugin.
    grant(&a, "alpha", &peer, &["alpha.use"]).await.unwrap();
    // A screen would not offer the button until the plugin shows as running, so neither
    // does this: calling a plugin that has not connected yet is refused with its own words.
    until(&a, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == id("alpha") && p.status == PluginStatus::Running)
    })
    .await;
    invoke(&a, "alpha", "share", Some(peer.clone()))
        .await
        .expect("a granted action must be admitted");

    shutdown([(a, task_a), (b, task_b)]).await;
}

/// Wait until the plugin shows as running, as a screen would before offering its buttons.
async fn running(client: &Client, plugin: &str) {
    use peersus_core::model::PluginStatus;
    until(client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == id(plugin) && p.status == PluginStatus::Running)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pressing_the_same_button_twice_does_not_run_the_action_twice() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("alpha.log");
    let (a, task_a) = start(root.path(), "a", logging_registry("alpha", &log)).await;
    let (b, task_b) = start(root.path(), "b", Registry::default()).await;
    let peer = pair(&a, &b).await;
    grant(&a, "alpha", &peer, &["alpha.use"]).await.unwrap();
    running(&a, "alpha").await;

    invoke(&a, "alpha", "share", Some(peer.clone()))
        .await
        .expect("the first press is admitted");
    log_reaches(&log, |lines| lines.iter().any(|l| l == "call share")).await;
    // The first run is still open (the plugin never answers `share` here), so a second press
    // for the same device is the same action and must not start another run.
    assert_eq!(
        invoke(&a, "alpha", "share", Some(peer.clone())).await.err(),
        Some("That action is already running")
    );
    // Give a wrongly admitted second call time to reach the plugin, then check it did not.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        observed(&log)
            .iter()
            .filter(|l| l.as_str() == "call share")
            .count(),
        1,
        "the plugin was asked to run the same action twice"
    );

    shutdown([(a, task_a), (b, task_b)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn withdrawing_a_grant_stops_that_devices_work_without_restarting_the_plugin() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("alpha.log");
    let (a, task_a) = start(root.path(), "a", logging_registry("alpha", &log)).await;
    let (b, task_b) = start(root.path(), "b", Registry::default()).await;
    let peer = pair(&a, &b).await;
    grant(&a, "alpha", &peer, &["alpha.use"]).await.unwrap();
    running(&a, "alpha").await;
    invoke(&a, "alpha", "share", Some(peer.clone()))
        .await
        .unwrap();
    log_reaches(&log, |lines| lines.iter().any(|l| l == "call share")).await;

    // The user takes the permission back while the action is still running.
    grant(&a, "alpha", &peer, &[]).await.unwrap();

    // The plugin is told the work was withdrawn...
    let lines = log_reaches(&log, |lines| lines.iter().any(|l| l == "cancelled")).await;
    // ...and it is the very same process: it started once and was never restarted.
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("start ")).count(),
        1,
        "withdrawing a permission must not restart the plugin: {lines:?}"
    );
    // It stays usable for everyone else, so it is still reported as running.
    running(&a, "alpha").await;

    shutdown([(a, task_a), (b, task_b)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_a_global_setting_restarts_only_the_plugin_that_owns_it() {
    let root = tempfile::tempdir().unwrap();
    let alpha_log = root.path().join("alpha.log");
    let beta_log = root.path().join("beta.log");
    let (a, task_a) = start(
        root.path(),
        "a",
        two_logging_registry(&alpha_log, &beta_log),
    )
    .await;
    running(&a, "alpha").await;
    running(&a, "beta").await;
    let starts = |log: &std::path::Path| {
        observed(log)
            .iter()
            .filter(|l| l.starts_with("start "))
            .count()
    };
    let set = |plugin: &str, value: i64| {
        a.command(Command::SetPluginSetting {
            plugin: id(plugin),
            peer: None,
            setting: "volume".into(),
            value: peersus_core::plugins::Value::Number(value),
        })
    };
    log_reaches(&alpha_log, |l| l.iter().any(|x| x.starts_with("start "))).await;
    log_reaches(&beta_log, |l| l.iter().any(|x| x.starts_with("start "))).await;

    // Give beta a saved value of its own first: from here on beta is a plugin the change
    // detection knows about, so only a correct comparison keeps it out of the next change.
    set("beta", 3).await.unwrap();
    log_reaches(&beta_log, |l| {
        l.iter().filter(|x| x.starts_with("start ")).count() >= 2
    })
    .await;
    assert_eq!(starts(&alpha_log), 1, "alpha restarted for beta's setting");

    // Now change alpha's.
    set("alpha", 7).await.unwrap();
    let lines = log_reaches(&alpha_log, |l| {
        l.iter().filter(|x| x.starts_with("start ")).count() >= 2
    })
    .await;
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("start ")).count(),
        2,
        "{lines:?}"
    );
    // beta, which has a saved value but did not change, was left alone.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        starts(&beta_log),
        2,
        "a plugin whose own value did not change was restarted: {:?}",
        observed(&beta_log)
    );
    running(&a, "alpha").await;
    running(&a, "beta").await;

    shutdown([(a, task_a)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_one_devices_permission_leaves_another_devices_work_alone() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("alpha.log");
    let (a, task_a) = start(root.path(), "a", logging_registry("alpha", &log)).await;
    let (b, task_b) = start(root.path(), "b", Registry::default()).await;
    let (c, task_c) = start(root.path(), "c", Registry::default()).await;
    let to_b = pair(&a, &b).await;
    let to_c = pair(&a, &c).await;
    grant(&a, "alpha", &to_b, &["alpha.use"]).await.unwrap();
    grant(&a, "alpha", &to_c, &["alpha.use"]).await.unwrap();
    running(&a, "alpha").await;
    invoke(&a, "alpha", "share", Some(to_b.clone()))
        .await
        .unwrap();
    invoke(&a, "alpha", "share", Some(to_c.clone()))
        .await
        .unwrap();
    log_reaches(&log, |lines| {
        lines.iter().filter(|l| l.as_str() == "call share").count() == 2
    })
    .await;

    // Withdraw only the first device's permission.
    grant(&a, "alpha", &to_b, &[]).await.unwrap();

    // Exactly one of the two open calls is withdrawn: the other device's work goes on.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let lines = observed(&log);
    assert_eq!(
        lines.iter().filter(|l| l.as_str() == "cancelled").count(),
        1,
        "only the withdrawn device's call may be cancelled: {lines:?}"
    );
    assert_eq!(lines.iter().filter(|l| l.starts_with("start ")).count(), 1);
    // The surviving device can still not start a second run of the same action, which
    // proves its first run is genuinely still open.
    assert_eq!(
        invoke(&a, "alpha", "share", Some(to_c)).await.err(),
        Some("That action is already running")
    );

    for (client, task) in [(a, task_a), (b, task_b), (c, task_c)] {
        client.command(Command::Shutdown {}).await.unwrap();
        task.await.unwrap();
    }
}

/// A plugin that takes in what peers send: it exports `receive`, guarded by `sink.write`,
/// open to the application, and records the bytes it was given in `bytes` and what it
/// observed in `log`.
fn receiving_registry(log: &std::path::Path, bytes: &std::path::Path) -> Registry {
    let mut registry = Registry::default();
    registry
        .register(PluginSpec {
            id: id("sink"),
            manifest: Manifest {
                capabilities: capabilities(&["sink.write"]),
                exports: vec![Export {
                    service: "sink.service".into(),
                    version: 1,
                    description: "Takes in what a device sends.".into(),
                    consumers: [HOST.to_owned()].into(),
                    methods: BTreeMap::from([(
                        peersus_app::plugin_host::RECEIVE_METHOD.to_owned(),
                        member(Some("sink.write")),
                    )]),
                    events: BTreeMap::new(),
                }],
                ..Manifest::default()
            },
            program: Some(plugin_binary()),
            // First argument: the plugin's log; second: where it keeps the bytes it received.
            arguments: vec![log.display().to_string(), bytes.display().to_string()],
            restart_policy: None,
        })
        .unwrap();
    registry
}

/// A bare network host standing in for a device that sends a plugin stream. It is driven on
/// a task of its own so its connection stays alive, and keeps the connection to the runtime
/// reachable.
struct Sender {
    host: std::sync::Arc<tokio::sync::Mutex<NetworkHost>>,
    peer_of_runtime: PeerId,
    /// Everything the runtime has sent this device on plugin streams it opened.
    received: std::sync::Arc<std::sync::Mutex<Vec<Received>>>,
    _driver: tokio::task::JoinHandle<()>,
}

/// One plugin stream the runtime opened to a device, with what has arrived on it so far.
#[derive(Clone, Debug)]
struct Received {
    service: String,
    version: u32,
    bytes: Vec<u8>,
}

/// Pair a bare host with `runtime` through the real ceremony and return it ready to send.
/// `label` names its files, so that several senders can exist side by side.
async fn paired_sender(root: &std::path::Path, label: &str, runtime: &Client) -> (Sender, PeerId) {
    let policy = NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..NetworkSettings::default()
    };
    let endpoint = transport::bind(SecretKey::generate(), &policy)
        .await
        .unwrap();
    let sender_id = PeerId(endpoint.id().to_string());
    let host = NetworkHost::new(
        endpoint,
        format!("Sender-{label}"),
        Peers::open(root.join(format!("{label}-peers.json"))).unwrap(),
        History::open(root.join(format!("{label}-history.json"))).unwrap(),
    );
    let host = std::sync::Arc::new(tokio::sync::Mutex::new(host));
    // The invitation comes from the runtime; the sender joins it.
    let Reply::Invitation(invitation) =
        runtime.command(Command::CreateInvitation {}).await.unwrap()
    else {
        panic!("Expected invitation")
    };
    let invitation = peersus_net::invitation::Invitation::decode(&invitation, now()).unwrap();
    host.lock().await.join(invitation).unwrap();
    // What the runtime sent to this device on streams it opened, by stream: a task per stream
    // reads it to its end and records the service it was for and every byte.
    let received: std::sync::Arc<std::sync::Mutex<Vec<Received>>> = Default::default();
    let (driven, collected) = (host.clone(), received.clone());
    let driver = tokio::spawn(async move {
        loop {
            // A short hold on the lock per step keeps the test free to look at the host.
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
                    // Record as it arrives, so a stream still open is visible too.
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
    // The sender confirms the same number on its side.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let waiting = host.lock().await.pairing();
        if let Some(prompt) = waiting.first() {
            host.lock()
                .await
                .choose(&prompt.id, number)
                .expect("the sender could not confirm");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no pairing prompt on the sender"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Both sides must be online. The id the sender's connection is keyed by is the
    // RUNTIME's, as the sender knows it, not the sender's own id that the runtime records.
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
                "the sender never saw the runtime online"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    (
        Sender {
            host,
            peer_of_runtime: peer.clone(),
            received,
            _driver: driver,
        },
        sender_id,
    )
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

impl Sender {
    /// Open a plugin stream to the runtime for `service` and send `bytes` over it.
    async fn send_plugin_stream(&self, service: &str, version: u32, bytes: &[u8]) {
        let connection = self
            .host
            .lock()
            .await
            .connection(&self.peer_of_runtime)
            .expect("the sender has no connection to the runtime");
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
        // Keep the connection alive until the runtime has had time to take the stream.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    /// Open a plugin stream to the runtime and return it still open. The connection is returned
    /// with it because the stream ends if the connection is dropped.
    async fn open_plugin_stream(
        &self,
        service: &str,
        version: u32,
    ) -> (iroh::endpoint::SendStream, iroh::endpoint::Connection) {
        let connection = self
            .host
            .lock()
            .await
            .connection(&self.peer_of_runtime)
            .expect("the sender has no connection to the runtime");
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
        (send, connection)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_device_without_the_grant_sends_a_plugin_stream_and_nothing_arrives() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("sink.log");
    let bytes = root.path().join("sink.bytes");
    let (runtime, task) = start(root.path(), "a", receiving_registry(&log, &bytes)).await;
    let (trusted, trusted_id) = paired_sender(root.path(), "trusted", &runtime).await;
    let (stranger, _) = paired_sender(root.path(), "stranger", &runtime).await;
    running(&runtime, "sink").await;

    // The user grants `sink.write` to ONE device. That also opens the hub to the
    // application for `receive`, which is the point: from here on the only thing standing
    // between the other, merely paired device and the plugin is the device-level admission.
    // (With nothing granted at all the hub would refuse too, and a failure of the admission
    // would go unnoticed behind it.)
    grant(&runtime, "sink", &trusted_id, &["sink.write"])
        .await
        .unwrap();

    stranger
        .send_plugin_stream("sink.service", 1, b"must never reach the plugin")
        .await;

    // The plugin was never even told: no call arrived, so nothing was fetched or written.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !observed(&log).iter().any(|l| l.starts_with("call receive")),
        "an ungranted device caused a call to the plugin: {:?}",
        observed(&log)
    );
    assert!(
        !bytes.exists(),
        "an ungranted device's bytes reached the plugin"
    );
    // And the refusal was reported to the user, in fixed words.
    let state = runtime.snapshot();
    assert!(
        state
            .errors
            .iter()
            .any(|e| e == "Plugin is not permitted for this device"),
        "the user was not told: {:?}",
        state.errors
    );
    drop(trusted);
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_granted_devices_bytes_reach_the_plugin_process_exactly() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("sink.log");
    let bytes = root.path().join("sink.bytes");
    let (runtime, task) = start(root.path(), "a", receiving_registry(&log, &bytes)).await;
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    running(&runtime, "sink").await;
    grant(&runtime, "sink", &sender_id, &["sink.write"])
        .await
        .unwrap();

    // Larger than one read of the pump (64 KiB) and not a repeating pattern, so that lost,
    // duplicated, truncated or reordered chunks cannot hide behind equal-looking bytes.
    let payload: Vec<u8> = (0..200_003u32)
        .map(|n| (n.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    sender.send_plugin_stream("sink.service", 1, &payload).await;

    // The plugin writes what it received once the peer's stream ends.
    log_reaches(&log, |lines| {
        lines.iter().any(|l| l.starts_with("received"))
    })
    .await;
    let received = std::fs::read(&bytes).expect("the plugin kept nothing");
    assert_eq!(received.len(), payload.len(), "the length differs");
    assert!(received == payload, "the bytes differ from what was sent");
    // It was the application that asked the plugin to fetch them, under its own identity.
    assert!(
        observed(&log).iter().any(|l| l == "call receive"),
        "{:?}",
        observed(&log)
    );
    // What the plugin was told about who sent it is a name for the device, not its key.
    let lines = observed(&log);
    let told = lines
        .iter()
        .find(|l| l.starts_with("arg "))
        .expect("the plugin recorded what it was told");
    assert!(
        !told.contains(&sender_id.0),
        "the plugin was told the sender's network key: {told}"
    );
    let alias = told
        .strip_prefix(r#"arg {"peer":""#)
        .and_then(|rest| rest.strip_suffix(r#""}"#))
        .unwrap_or_else(|| panic!("the argument is not exactly a peer alias: {told}"));
    assert!(
        alias.len() == 32 && alias.bytes().all(|b| b.is_ascii_hexdigit()),
        "{alias:?} does not look like an alias"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// A plugin that receives for the application and also attacks. Its `receive` is guarded by
/// `sink.write` and open to the application only, like any receiving plugin's. It also
/// exports a second, unrelated service, `attack.service`/`echo`, with no capability and only
/// to itself, and imports it from itself, so that the hub lets it call itself freely. Those
/// self-calls are only a way to hold genuine data rights under chosen call numbers: what
/// the application sees of a right is its caller and its number, not the method.
fn attacking_registry(
    log: &std::path::Path,
    bytes: &std::path::Path,
    stolen: &std::path::Path,
) -> Registry {
    use peersus_plugin_api::manifest::Import;
    let mut registry = Registry::default();
    registry
        .register(PluginSpec {
            id: id("sink"),
            manifest: Manifest {
                capabilities: capabilities(&["sink.write"]),
                imports: vec![Import {
                    provider: "sink".into(),
                    service: "attack.service".into(),
                    version: 1,
                    methods: ["echo".to_owned()].into(),
                    events: Default::default(),
                }],
                exports: vec![
                    Export {
                        service: "sink.service".into(),
                        version: 1,
                        description: "Takes in what a device sends.".into(),
                        consumers: [HOST.to_owned()].into(),
                        methods: BTreeMap::from([(
                            peersus_app::plugin_host::RECEIVE_METHOD.to_owned(),
                            member(Some("sink.write")),
                        )]),
                        events: BTreeMap::new(),
                    },
                    Export {
                        service: "attack.service".into(),
                        version: 1,
                        description: "A service another plugin tries to reach.".into(),
                        consumers: ["sink".to_owned()].into(),
                        methods: BTreeMap::from([("echo".to_owned(), member(None))]),
                        events: BTreeMap::new(),
                    },
                ],
                ..Manifest::default()
            },
            program: Some(plugin_binary()),
            arguments: vec![
                log.display().to_string(),
                bytes.display().to_string(),
                "steal".into(),
                stolen.display().to_string(),
            ],
            restart_policy: None,
        })
        .unwrap();
    registry
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_calls_itself_with_the_applications_call_number_cannot_collect_a_peers_stream()
 {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("sink.log");
    let bytes = root.path().join("sink.bytes");
    let stolen = root.path().join("sink.stolen");
    let (runtime, task) = start(root.path(), "a", attacking_registry(&log, &bytes, &stolen)).await;
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    running(&runtime, "sink").await;
    // Admitted, so that its stream really is parked and there is something to steal.
    grant(&runtime, "sink", &sender_id, &["sink.write"])
        .await
        .unwrap();

    let payload: Vec<u8> = (0..50_001u32)
        .map(|n| (n.wrapping_mul(2_654_435_761) >> 11) as u8)
        .collect();
    sender.send_plugin_stream("sink.service", 1, &payload).await;

    // The attack really ran, or the rest would prove nothing. The plugin held 32 genuine
    // rights, one under each number from 1 to 32, so one of them carried the number the
    // application parked the stream under, and it presented every one of them.
    let lines = log_reaches(&log, |lines| lines.iter().any(|l| l.starts_with("stole "))).await;
    assert!(
        lines.iter().any(|l| l == "sprayed 32"),
        "the attack did not start: {lines:?}"
    );
    // It collected nothing with any of its 32 genuine rights.
    assert!(
        lines.iter().any(|l| l == "stole 0"),
        "a plugin collected a stream that was not its own: {lines:?}"
    );
    let taken = std::fs::read(&stolen).unwrap_or_default();
    assert!(taken.is_empty(), "{} stolen bytes", taken.len());
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// The receiving plugin run in a given mode (third argument): `trickle` records each chunk the
/// moment it arrives instead of writing everything once at the end; `late` holds its right for
/// a while before collecting the stream.
fn moded_registry(log: &std::path::Path, bytes: &std::path::Path, mode: &str) -> Registry {
    let mut registry = Registry::default();
    registry
        .register(PluginSpec {
            id: id("sink"),
            manifest: Manifest {
                capabilities: capabilities(&["sink.write"]),
                exports: vec![Export {
                    service: "sink.service".into(),
                    version: 1,
                    description: "Takes in what a device sends.".into(),
                    consumers: [HOST.to_owned()].into(),
                    methods: BTreeMap::from([(
                        peersus_app::plugin_host::RECEIVE_METHOD.to_owned(),
                        member(Some("sink.write")),
                    )]),
                    events: BTreeMap::new(),
                }],
                ..Manifest::default()
            },
            program: Some(plugin_binary()),
            arguments: vec![
                log.display().to_string(),
                bytes.display().to_string(),
                mode.into(),
            ],
            restart_policy: None,
        })
        .unwrap();
    registry
}

#[tokio::test(flavor = "multi_thread")]
async fn withdrawing_a_grant_stops_a_transfer_that_is_already_carrying_bytes() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("sink.log");
    let bytes = root.path().join("sink.bytes");
    let (runtime, task) = start(root.path(), "a", moded_registry(&log, &bytes, "trickle")).await;
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    running(&runtime, "sink").await;
    grant(&runtime, "sink", &sender_id, &["sink.write"])
        .await
        .unwrap();

    // A stream that stays open, so there is a moment when bytes have been carried and more
    // are still to come.
    let (mut send, _connection) = sender.open_plugin_stream("sink.service", 1).await;
    let prefix = vec![0xAA_u8; 20_000];
    send.write_all(&prefix).await.unwrap();

    // Wait until the plugin has really received the prefix: only then is the transfer
    // running rather than merely announced.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::fs::metadata(&bytes).map(|m| m.len()).unwrap_or(0) < prefix.len() as u64 {
        assert!(
            std::time::Instant::now() < deadline,
            "the prefix never reached the plugin: {:?}",
            observed(&log)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The user takes the permission back while the transfer is under way.
    grant(&runtime, "sink", &sender_id, &[]).await.unwrap();

    // The same live stream now carries something else. It must not arrive.
    let suffix = vec![0xBB_u8; 20_000];
    let _ = send.write_all(&suffix).await;
    let _ = send.finish();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let received = std::fs::read(&bytes).unwrap();
    assert!(
        !received.contains(&0xBB),
        "{} bytes arrived after the permission was withdrawn",
        received.iter().filter(|b| **b == 0xBB).count()
    );
    assert_eq!(
        received, prefix,
        "what arrived before the withdrawal is intact"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// What the user sees when a device is forgotten mid-transfer: nothing more arrives.
///
/// This does NOT prove the runtime's own stop of running transfers. Forgetting a device also
/// closes its connection, which ends the transfer by itself, and this test passes with the
/// runtime's stop removed (checked by mutation). That stop is proven by
/// `withdrawing_a_grant_stops_a_transfer_that_is_already_carrying_bytes`, where the
/// connection stays open. Here it is only the second line of defence.
#[tokio::test(flavor = "multi_thread")]
async fn after_a_device_is_forgotten_nothing_more_of_its_transfer_reaches_the_plugin() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("sink.log");
    let bytes = root.path().join("sink.bytes");
    let (runtime, task) = start(root.path(), "a", moded_registry(&log, &bytes, "trickle")).await;
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    running(&runtime, "sink").await;
    grant(&runtime, "sink", &sender_id, &["sink.write"])
        .await
        .unwrap();

    let (mut send, _connection) = sender.open_plugin_stream("sink.service", 1).await;
    let prefix = vec![0xCC_u8; 20_000];
    send.write_all(&prefix).await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::fs::metadata(&bytes).map(|m| m.len()).unwrap_or(0) < prefix.len() as u64 {
        assert!(
            std::time::Instant::now() < deadline,
            "the prefix never reached the plugin: {:?}",
            observed(&log)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The user forgets the device altogether while its transfer is under way.
    runtime
        .command(Command::ForgetPeer {
            peer: sender_id.clone(),
        })
        .await
        .unwrap();

    let suffix = vec![0xDD_u8; 20_000];
    let _ = send.write_all(&suffix).await;
    let _ = send.finish();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let received = std::fs::read(&bytes).unwrap();
    assert!(
        !received.contains(&0xDD),
        "{} bytes arrived after the device was forgotten",
        received.iter().filter(|b| **b == 0xDD).count()
    );
    assert_eq!(
        received, prefix,
        "what arrived before it was forgotten is intact"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_forgotten_before_the_plugin_collects_its_stream_delivers_nothing() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("sink.log");
    let bytes = root.path().join("sink.bytes");
    let (runtime, task) = start(root.path(), "a", moded_registry(&log, &bytes, "late")).await;
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    running(&runtime, "sink").await;
    grant(&runtime, "sink", &sender_id, &["sink.write"])
        .await
        .unwrap();

    // The device sends its bytes and then keeps the stream open. They sit parked in the
    // application until the plugin, which is holding its right, comes for them.
    let (mut send, _connection) = sender.open_plugin_stream("sink.service", 1).await;
    send.write_all(&vec![0xEE_u8; 30_000]).await.unwrap();

    // Wait until the plugin has been told and is holding its right.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !observed(&log).iter().any(|l| l == "late waiting") {
        assert!(
            std::time::Instant::now() < deadline,
            "the plugin was never told about the stream: {:?}",
            observed(&log)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The user forgets the device while its stream is parked and uncollected.
    runtime
        .command(Command::ForgetPeer {
            peer: sender_id.clone(),
        })
        .await
        .unwrap();

    // Let the plugin come for its stream, and give anything that would arrive time to.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !observed(&log).iter().any(|l| l.starts_with("received")) {
        assert!(
            std::time::Instant::now() < deadline,
            "the plugin never finished collecting: {:?}",
            observed(&log)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = send.finish();
    let taken = std::fs::read(&bytes).unwrap_or_default();
    assert!(
        taken.is_empty(),
        "{} bytes of a forgotten device reached the plugin: {:?}",
        taken.len(),
        observed(&log)
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// A transfer is announced to the plugin under a call with a 15 second deadline. The copy
/// itself only gives up after 30 seconds of silence, renewed by every byte, so a peer that
/// keeps trickling would keep the plugin inside that one call indefinitely, and with it
/// everything queued behind it. Once the call's deadline has passed, the transfer is cut.
///
/// The peer here never goes quiet for long, which is what makes this a test of the deadline
/// and not of the idle limit: it would pass for the wrong reason if the stream simply stalled.
#[tokio::test(flavor = "multi_thread")]
async fn a_transfer_that_keeps_trickling_is_cut_when_the_call_that_announced_it_expires() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("sink.log");
    let bytes = root.path().join("sink.bytes");
    let (runtime, task) = start(root.path(), "a", moded_registry(&log, &bytes, "trickle")).await;
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    running(&runtime, "sink").await;
    grant(&runtime, "sink", &sender_id, &["sink.write"])
        .await
        .unwrap();

    let (mut send, _connection) = sender.open_plugin_stream("sink.service", 1).await;
    let began = std::time::Instant::now();
    send.write_all(&[0xAA_u8; 1_000]).await.unwrap();
    // Wait for the plugin to really be receiving: only then is the transfer running.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::fs::metadata(&bytes).map(|m| m.len()).unwrap_or(0) < 1_000 {
        assert!(
            std::time::Instant::now() < deadline,
            "the first bytes never reached the plugin: {:?}",
            observed(&log)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Keep sending a little, often, for well beyond the 15 seconds the call was given. Every
    // gap is far shorter than the idle limit, so only the deadline can end this. Bytes sent
    // before the deadline are 0xAA, and from a second past it 0xCC, so what got through is
    // told apart. Once the transfer is cut the writes start to fail, and that is the result
    // wanted here, not an error of the test, so the loop just ends.
    let mut trickled_for = Duration::ZERO;
    while began.elapsed() < Duration::from_secs(21) {
        let late = began.elapsed() > Duration::from_secs(16);
        let byte = if late { 0xCC } else { 0xAA };
        if send.write_all(&[byte; 100]).await.is_err() {
            break;
        }
        trickled_for = began.elapsed();
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    // The peer really did keep sending, with short gaps, for most of the call's life: this
    // is what makes a cut at the deadline a result of the deadline and of nothing else. A
    // stream that merely stalled or broke early would end sooner than this.
    assert!(
        trickled_for > Duration::from_secs(10),
        "the trickle ended after {trickled_for:?}: it measured the wrong thing"
    );
    let _ = send.finish();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let received = std::fs::read(&bytes).unwrap();
    assert!(
        !received.contains(&0xCC),
        "{} bytes arrived after the call's deadline had passed",
        received.iter().filter(|b| **b == 0xCC).count()
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_withdrawn_before_the_plugin_collects_stops_delivery_though_the_device_stays_connected()
 {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("sink.log");
    let bytes = root.path().join("sink.bytes");
    let (runtime, task) = start(root.path(), "a", moded_registry(&log, &bytes, "late")).await;
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    running(&runtime, "sink").await;
    grant(&runtime, "sink", &sender_id, &["sink.write"])
        .await
        .unwrap();

    let (mut send, _connection) = sender.open_plugin_stream("sink.service", 1).await;
    send.write_all(&vec![0xF1_u8; 30_000]).await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !observed(&log).iter().any(|l| l == "late waiting") {
        assert!(
            std::time::Instant::now() < deadline,
            "the plugin was never told about the stream: {:?}",
            observed(&log)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The user takes the permission back. The device is still paired and still connected, so
    // nothing but the application's own decision stands between its bytes and the plugin.
    grant(&runtime, "sink", &sender_id, &[]).await.unwrap();

    // The plugin wakes from holding its right (after 1.5 s) and collects whatever it can.
    // It writes each chunk as it arrives, so what reached it is visible with the stream
    // still open; waiting for the end of the stream would hide exactly the case under test.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let taken = std::fs::read(&bytes).unwrap_or_default();
    let _ = send.finish();
    assert!(
        taken.is_empty(),
        "{} bytes arrived after the permission was withdrawn: {:?}",
        taken.len(),
        observed(&log)
    );
    // The device really was still connected: it is still online.
    assert!(
        sender
            .host
            .lock()
            .await
            .connection(&sender.peer_of_runtime)
            .is_some(),
        "the test did not keep the device connected, so it proves nothing about the grant"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// A plugin, `clip`, that on start asks the application for `peersus.peers` `list` and
/// `send`, and records how each call ended in `log`.
fn asking_registry(log: &std::path::Path) -> Registry {
    use peersus_plugin_api::manifest::Import;
    let mut registry = Registry::default();
    registry
        .register(PluginSpec {
            id: id("clip"),
            manifest: Manifest {
                capabilities: capabilities(&[]),
                imports: vec![Import {
                    provider: HOST.to_owned(),
                    service: peersus_app::runtime::PEERS_SERVICE.to_owned(),
                    version: 1,
                    methods: ["list".to_owned(), "send".to_owned()].into(),
                    events: Default::default(),
                }],
                ..Manifest::default()
            },
            program: Some(plugin_binary()),
            arguments: vec![log.display().to_string(), String::new(), "ask-host".into()],
            restart_policy: None,
        })
        .unwrap();
    registry
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_asked_for_peers_is_answered_by_the_application_and_send_needs_a_grant() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("clip.log");
    let (runtime, task) = start(root.path(), "a", asking_registry(&log)).await;
    running(&runtime, "clip").await;
    let lines = log_reaches(&log, |l| {
        l.iter().any(|x| x.starts_with("result 1 ")) && l.iter().any(|x| x.starts_with("result 2 "))
    })
    .await;
    // `list` is open to a plugin that asked: the application answers it, with data.
    assert!(
        lines.iter().any(|l| l == "result 1 ok"),
        "list was not answered: {lines:?}"
    );
    // `send` is guarded and nothing is granted: refused by the plugin layer.
    assert!(
        lines.iter().any(|l| l == "result 2 denied"),
        "an ungranted send was not refused as denied: {lines:?}"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// A plugin that never imported the application's service is refused, while another that did
/// is served, in the same run.
///
/// This does NOT prove the application's `consumers` list: the hub also refuses a caller
/// that lacks the import, which happens first, so this test passes with `consumers` widened
/// to every plugin (checked by mutation). A plugin that imports correctly is by construction
/// a consumer, so no registry can separate the two layers from outside. `consumers` is
/// proven by the unit tests of `host_exports`.
#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_never_asked_for_peers_is_refused_even_though_another_one_is_served() {
    let root = tempfile::tempdir().unwrap();
    let clip_log = root.path().join("clip.log");
    let intruder_log = root.path().join("intruder.log");
    let mut registry = asking_registry(&clip_log);
    // Same program, same behaviour, but its manifest never imports the application's service.
    registry
        .register(PluginSpec {
            id: id("intruder"),
            manifest: Manifest::default(),
            program: Some(plugin_binary()),
            arguments: vec![
                intruder_log.display().to_string(),
                String::new(),
                "ask-host".into(),
            ],
            restart_policy: None,
        })
        .unwrap();
    let (runtime, task) = start(root.path(), "a", registry).await;
    running(&runtime, "clip").await;
    running(&runtime, "intruder").await;
    let served = log_reaches(&clip_log, |l| l.iter().any(|x| x == "result 1 ok")).await;
    let refused = log_reaches(&intruder_log, |l| {
        l.iter().any(|x| x.starts_with("result 1 ")) && l.iter().any(|x| x.starts_with("result 2 "))
    })
    .await;
    assert!(served.iter().any(|l| l == "result 1 ok"), "{served:?}");
    // Both of the intruder's calls are refused before they reach the application.
    assert!(
        refused.iter().any(|l| l == "result 1 denied"),
        "an unasked-for list was not refused: {refused:?}"
    );
    assert!(
        refused.iter().any(|l| l == "result 2 denied"),
        "an unasked-for send was not refused: {refused:?}"
    );
    assert!(
        !refused.iter().any(|l| l == "result 1 ok"),
        "an intruder was served: {refused:?}"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// How the plugin writes what it sends.
#[derive(Clone, Copy)]
enum Writer {
    /// All at once.
    Fast,
    /// In slices with a pause between them, so there is time to interrupt it.
    Slow,
    /// Slowly, and only the first time: a restarted plugin does not start over.
    SlowOnce,
    /// All at once, but only after publishing its claim number to a file and waiting, so
    /// another plugin has time to present it first.
    SharedClaim,
}

/// A plugin, `clip`, that keeps asking for the devices until the one it wants is listed (the
/// one named `target`, or the first if none), sends `payload` (a file) to it for
/// `sink.service`, and records each step in `log`.
fn sending_registry(
    log: &std::path::Path,
    payload: &std::path::Path,
    target: Option<&str>,
    writer: Writer,
) -> Registry {
    use peersus_plugin_api::manifest::Import;
    let mut registry = Registry::default();
    registry
        .register(PluginSpec {
            id: id("clip"),
            manifest: Manifest {
                capabilities: capabilities(&[]),
                imports: vec![Import {
                    provider: HOST.to_owned(),
                    service: peersus_app::runtime::PEERS_SERVICE.to_owned(),
                    version: 1,
                    methods: ["list".to_owned(), "send".to_owned()].into(),
                    events: Default::default(),
                }],
                contributions: Contributions {
                    // Never read by the plugin: it exists so a test can restart the process
                    // the way a user does, by changing one of its own settings.
                    settings: vec![Setting {
                        id: "volume".into(),
                        label: "Volume".into(),
                        help: None,
                        scope: Scope::Global,
                        kind: Kind::Number {
                            min: 0,
                            max: 10,
                            default: 5,
                        },
                    }],
                    ..Contributions::default()
                },
                ..Manifest::default()
            },
            program: Some(plugin_binary()),
            arguments: {
                let mut arguments = vec![
                    log.display().to_string(),
                    payload.display().to_string(),
                    "send-out".into(),
                    // Which device to send to, by the name it gave itself; `*` for the first.
                    target.unwrap_or("*").to_owned(),
                ];
                match writer {
                    Writer::Fast => {}
                    Writer::Slow => arguments.push("slow".into()),
                    // The fifth argument is `slow`, the sixth `once`: sending once is only
                    // interesting while it is slow enough to be interrupted.
                    Writer::SlowOnce => arguments.extend(["slow".into(), "once".into()]),
                    // The seventh argument makes it publish its claim and wait before using
                    // it; the fifth and sixth are placeholders the plugin does not read.
                    Writer::SharedClaim => {
                        arguments.extend([
                            "-".into(),
                            "-".into(),
                            "share-claim".into(),
                            format!("{}.send-ready", log.display()),
                        ]);
                    }
                }
                arguments
            },
            restart_policy: None,
        })
        .unwrap();
    registry
}

/// Add a second plugin, `thief`, that presents the number found in `claim_file` on its own data
/// connection: a number the application issued to `clip`, not to it. It imports nothing and
/// is granted nothing, so the only thing between it and `clip`'s send is the application
/// checking WHO presents a number.
fn with_thief(
    mut registry: Registry,
    claim_file: &std::path::Path,
    log: &std::path::Path,
) -> Registry {
    registry
        .register(PluginSpec {
            id: id("thief"),
            manifest: Manifest::default(),
            program: Some(plugin_binary()),
            arguments: vec![
                log.display().to_string(),
                claim_file.display().to_string(),
                "thief".into(),
            ],
            restart_policy: None,
        })
        .unwrap();
    registry
}

/// Wait until `sender` has received at least `at_least` bytes for `service`, or `wait` passes,
/// and return what it has.
///
/// The wait is for the amount expected, not for the first byte: the stream is recorded as it
/// flows, so the first byte arrives long before the last and a snapshot taken then is only
/// part of a transfer that is still going. Waiting on the first byte made this a race in the
/// test. A real loss of bytes still shows, as a length that never reaches `at_least`.
async fn received_for(
    sender: &Sender,
    service: &str,
    at_least: usize,
    wait: Duration,
) -> Option<Vec<u8>> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        let found = sender
            .received
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.service == service)
            .map(|r| r.bytes.clone());
        let enough = found.as_ref().is_some_and(|bytes| bytes.len() >= at_least);
        if enough || std::time::Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_granted_for_a_device_sends_it_bytes_that_arrive_exactly() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("clip.log");
    let payload = root.path().join("payload.bin");
    // Not a round number and not repeating, so truncation or reordering would show.
    let bytes: Vec<u8> = (0..150_001u32)
        .map(|n| (n.wrapping_mul(31) % 251) as u8)
        .collect();
    std::fs::write(&payload, &bytes).unwrap();
    let (runtime, task) = start(
        root.path(),
        "a",
        sending_registry(&log, &payload, None, Writer::Fast),
    )
    .await;
    running(&runtime, "clip").await;
    // The plugin is already asking. A device pairs, and the user lets the plugin send to it.
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    grant_host(
        &runtime,
        "clip",
        &sender_id,
        &[peersus_app::runtime::PEERS_SEND_CAPABILITY],
    )
    .await
    .unwrap();

    let arrived = received_for(
        &sender,
        "sink.service",
        bytes.len(),
        Duration::from_secs(15),
    )
    .await;
    assert_eq!(
        arrived.as_deref().map(<[u8]>::len),
        Some(bytes.len()),
        "what reached the device has the wrong length: {:?}",
        observed(&log)
    );
    assert_eq!(arrived.as_deref(), Some(bytes.as_slice()), "bytes differ");
    // The device was told which version of the service the stream is for: the one asked.
    let version = sender
        .received
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.service == "sink.service")
        .map(|r| r.version);
    assert_eq!(
        version,
        Some(1),
        "the stream carried the wrong service version"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// Nobody has granted the plugin anything, so it holds nothing at the hub and is refused
/// there, before the application is even asked. This is the first of the two layers.
#[tokio::test(flavor = "multi_thread")]
async fn when_no_device_granted_the_plugin_the_hub_refuses_it_and_nothing_is_sent() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("clip.log");
    let payload = root.path().join("payload.bin");
    std::fs::write(&payload, vec![0xA5_u8; 20_000]).unwrap();
    let (runtime, task) = start(
        root.path(),
        "a",
        sending_registry(&log, &payload, None, Writer::Fast),
    )
    .await;
    running(&runtime, "clip").await;
    let (sender, _sender_id) = paired_sender(root.path(), "sender", &runtime).await;

    let lines = log_reaches(&log, |l| l.iter().any(|x| x.starts_with("result 2 "))).await;
    assert!(
        lines.iter().any(|l| l == "result 2 denied"),
        "an ungranted plugin was not refused at the hub: {lines:?}"
    );
    assert!(!lines.iter().any(|l| l.starts_with("pushed")), "{lines:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        sender.received.lock().unwrap().is_empty(),
        "bytes reached a device that granted nothing: {:?}",
        sender.received.lock().unwrap()
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// One device granted the plugin, so the hub lets it through, but the plugin asks to send to a
/// DIFFERENT device that did not. Only the application's per-device decision, the second
/// layer, stands between the plugin and that device.
#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_granted_by_one_device_cannot_send_to_another_that_did_not_grant_it() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("clip.log");
    let payload = root.path().join("payload.bin");
    std::fs::write(&payload, vec![0xA5_u8; 20_000]).unwrap();
    // The plugin is told to send to the device named for the second sender.
    let (runtime, task) = start(
        root.path(),
        "a",
        sending_registry(&log, &payload, Some("Sender-ungranted"), Writer::Fast),
    )
    .await;
    running(&runtime, "clip").await;
    let (granted, granted_id) = paired_sender(root.path(), "granted", &runtime).await;
    grant_host(
        &runtime,
        "clip",
        &granted_id,
        &[peersus_app::runtime::PEERS_SEND_CAPABILITY],
    )
    .await
    .unwrap();
    // Only now does the device the plugin is looking for come into existence. The plugin
    // polls the list and sends the moment it sees its target, so if the target appeared
    // before the grant, the plugin would try to send holding no permission at all, and the
    // refusal would come from the plugin layer instead of the device layer this test is
    // about.
    let (ungranted, _ungranted_id) = paired_sender(root.path(), "ungranted", &runtime).await;

    let lines = log_reaches(&log, |l| l.iter().any(|x| x.starts_with("result 2 "))).await;
    assert!(
        lines.iter().any(|l| l == "result 2 refused"),
        "a send to a device that did not grant it was not refused by the device layer: {lines:?}"
    );
    assert!(!lines.iter().any(|l| l.starts_with("pushed")), "{lines:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        ungranted.received.lock().unwrap().is_empty(),
        "bytes reached a device that never granted the plugin: {:?}",
        ungranted.received.lock().unwrap()
    );
    assert!(
        granted.received.lock().unwrap().is_empty(),
        "the device that did grant was sent what was meant for another"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// The user takes the permission back while the plugin is in the middle of sending. What has
/// already gone cannot be recalled, but nothing more may reach the device, and the plugin
/// must learn its transfer was cut rather than carry on into the void.
#[tokio::test(flavor = "multi_thread")]
async fn revoking_the_grant_mid_send_stops_the_bytes_and_tells_the_plugin() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("clip.log");
    let payload = root.path().join("payload.bin");
    let bytes = vec![0x5C_u8; 200_000];
    std::fs::write(&payload, &bytes).unwrap();
    let (runtime, task) = start(
        root.path(),
        "a",
        sending_registry(&log, &payload, None, Writer::Slow),
    )
    .await;
    running(&runtime, "clip").await;
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    grant_host(
        &runtime,
        "clip",
        &sender_id,
        &[peersus_app::runtime::PEERS_SEND_CAPABILITY],
    )
    .await
    .unwrap();

    // Wait until the transfer is genuinely under way: some bytes are at the device.
    let started = received_for(&sender, "sink.service", 10_000, Duration::from_secs(15)).await;
    assert!(
        started.as_ref().is_some_and(|b| b.len() >= 10_000),
        "the send never got under way: {:?}",
        observed(&log)
    );

    // The user withdraws the permission.
    grant(&runtime, "clip", &sender_id, &[]).await.unwrap();

    // Give anything still in flight time to land, then take the length, then wait and look
    // again: after the withdrawal nothing may keep arriving.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let length = |s: &Sender| {
        s.received
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.service == "sink.service")
            .map_or(0, |r| r.bytes.len())
    };
    let after_revoke = length(&sender);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let later = length(&sender);
    assert_eq!(
        later,
        after_revoke,
        "{} more bytes reached the device after the permission was withdrawn",
        later - after_revoke
    );
    assert!(
        later < bytes.len(),
        "the whole payload arrived despite the withdrawal"
    );
    // The plugin is not left carrying on into the void: its write fails.
    let lines = log_reaches(&log, |l| l.iter().any(|x| x.starts_with("write failed"))).await;
    assert!(
        lines.iter().any(|l| l.starts_with("write failed")),
        "the plugin was never told its transfer was cut: {lines:?}"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// The plugin is restarted while it is sending, as when the user changes one of its own
/// settings. The new process cannot continue what the old one was sending, so nothing more
/// may reach the device. The plugin sends only once, so a new process starting over is not
/// mistaken for the old transfer carrying on.
///
/// This does NOT prove the application's own stop of sends on restart
/// (`outbound.drop_for_plugin` and `carrying_out.stop_for_plugin`): restarting ends the old
/// process, which closes its data connection and ends the transfer by itself, so this test
/// passes with that stop removed (checked by mutation). It is the second line of defence
/// here; the withdrawal of a grant, where the connection stays open, is the test that proves
/// the stop (`revoking_the_grant_mid_send_stops_the_bytes_and_tells_the_plugin`).
#[tokio::test(flavor = "multi_thread")]
async fn restarting_the_plugin_mid_send_stops_the_bytes_reaching_the_device() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("clip.log");
    let payload = root.path().join("payload.bin");
    let bytes = vec![0x3D_u8; 200_000];
    std::fs::write(&payload, &bytes).unwrap();
    let (runtime, task) = start(
        root.path(),
        "a",
        sending_registry(&log, &payload, None, Writer::SlowOnce),
    )
    .await;
    running(&runtime, "clip").await;
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    grant_host(
        &runtime,
        "clip",
        &sender_id,
        &[peersus_app::runtime::PEERS_SEND_CAPABILITY],
    )
    .await
    .unwrap();
    let started = received_for(&sender, "sink.service", 10_000, Duration::from_secs(15)).await;
    assert!(
        started.as_ref().is_some_and(|b| b.len() >= 10_000),
        "the send never got under way: {:?}",
        observed(&log)
    );

    // The user changes one of the plugin's own settings: its process is restarted.
    runtime
        .command(Command::SetPluginSetting {
            plugin: id("clip"),
            peer: None,
            setting: "volume".into(),
            value: peersus_core::plugins::Value::Number(7),
        })
        .await
        .unwrap();
    // It really was a new process.
    log_reaches(&log, |l| {
        l.iter().filter(|x| x.starts_with("start ")).count() >= 2
    })
    .await;

    tokio::time::sleep(Duration::from_millis(1500)).await;
    let length = |s: &Sender| {
        s.received
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.service == "sink.service")
            .map_or(0, |r| r.bytes.len())
    };
    let after_restart = length(&sender);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let later = length(&sender);
    assert_eq!(
        later,
        after_restart,
        "{} more bytes reached the device after the plugin was restarted",
        later - after_restart
    );
    assert!(
        later < bytes.len(),
        "the whole payload arrived despite the restart"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// A second plugin gets hold of the number the application issued to `clip` for its send, and
/// presents it on its own data connection before `clip` does. It must get nothing, AND it must
/// not use the number up: `clip` then sends exactly what it meant to, and the device receives
/// only that.
#[tokio::test(flavor = "multi_thread")]
async fn another_plugin_presenting_a_claim_gets_nothing_and_cannot_burn_it() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("clip.log");
    let thief_log = root.path().join("thief.log");
    let payload = root.path().join("payload.bin");
    // Not a round number and not repeating, and with no 0xEE in it, so the thief's own
    // marker cannot be mistaken for it.
    let bytes: Vec<u8> = (0..60_001u32)
        .map(|n| (n.wrapping_mul(31) % 200) as u8)
        .collect();
    std::fs::write(&payload, &bytes).unwrap();
    let claim_file = root.path().join("clip.log.claim");
    let registry = with_thief(
        sending_registry(&log, &payload, None, Writer::SharedClaim),
        &claim_file,
        &thief_log,
    );
    let (runtime, task) = start(root.path(), "a", registry).await;
    running(&runtime, "clip").await;
    running(&runtime, "thief").await;
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    grant_host(
        &runtime,
        "clip",
        &sender_id,
        &[peersus_app::runtime::PEERS_SEND_CAPABILITY],
    )
    .await
    .unwrap();
    std::fs::write(root.path().join("clip.log.send-ready"), b"").unwrap();

    // The thief really did get the number and really did present it.
    let thief_lines = log_reaches(&thief_log, |l| {
        l.iter().any(|x| x.starts_with("thief presented"))
    })
    .await;
    assert!(
        thief_lines.iter().any(|l| l.starts_with("thief has ")),
        "{thief_lines:?}"
    );

    // clip is not robbed: what it sent arrives exactly.
    let arrived = received_for(
        &sender,
        "sink.service",
        bytes.len(),
        Duration::from_secs(15),
    )
    .await;
    assert_eq!(
        arrived.as_deref().map(<[u8]>::len),
        Some(bytes.len()),
        "clip's send was disturbed by another plugin: {:?} / {:?}",
        observed(&log),
        thief_lines
    );
    assert_eq!(arrived.as_deref(), Some(bytes.as_slice()), "bytes differ");
    // And nothing of the thief's reached the device.
    assert!(
        !arrived.unwrap_or_default().contains(&0xEE),
        "bytes written by another plugin reached the device"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

/// The grant is withdrawn after the application issued the plugin its number but before the
/// plugin has used it. The device is still paired and connected, so nothing but the
/// application's own decision stands between the plugin and the device: the stream must
/// never be opened and nothing may arrive.
///
/// Two things in the application each stop this on their own: dropping the waiting entry
/// (`outbound.drop_for`) and stopping the task that was issued the number
/// (`carrying_out.stop_for`). Removing either ONE leaves this test green (checked by
/// mutation, 0/10 red each way), because the other still covers it; removing BOTH makes it
/// fail every time (10/10 red on the assertion below). So it proves the pair works and that
/// the device is never contacted, but not that each half is individually needed. Each half is
/// separately proven where it can be: the stop by the mid-send withdrawal test, the entry
/// removal only by the unit tests of `Parked`.
#[tokio::test(flavor = "multi_thread")]
async fn a_grant_withdrawn_before_the_plugin_uses_its_claim_means_the_device_is_never_contacted() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("clip.log");
    let payload = root.path().join("payload.bin");
    std::fs::write(&payload, vec![0x42_u8; 30_000]).unwrap();
    let (runtime, task) = start(
        root.path(),
        "a",
        sending_registry(&log, &payload, None, Writer::SharedClaim),
    )
    .await;
    running(&runtime, "clip").await;
    let (sender, sender_id) = paired_sender(root.path(), "sender", &runtime).await;
    grant_host(
        &runtime,
        "clip",
        &sender_id,
        &[peersus_app::runtime::PEERS_SEND_CAPABILITY],
    )
    .await
    .unwrap();
    std::fs::write(root.path().join("clip.log.send-ready"), b"").unwrap();

    // The plugin has its number and is holding it (it publishes it and waits).
    let claim_file = root.path().join("clip.log.claim");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !claim_file.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the plugin was never given a claim: {:?}",
            observed(&log)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The user takes the permission back while it is still holding it.
    grant(&runtime, "clip", &sender_id, &[]).await.unwrap();

    // It now uses the number, and writes everything it meant to.
    log_reaches(&log, |l| l.iter().any(|x| x.starts_with("pushed"))).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        sender.received.lock().unwrap().is_empty(),
        "the device was contacted after the permission was withdrawn: {:?}",
        sender.received.lock().unwrap()
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

fn action_state_of(
    snapshot: &Snapshot,
    plugin: &str,
    action: &str,
) -> Option<peersus_core::model::ActionState> {
    snapshot
        .actions
        .iter()
        .find(|a| a.plugin == id(plugin) && a.action == action)
        .map(|a| a.state)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_action_the_plugin_answered_shows_as_done_and_nothing_else_about_it() {
    use peersus_core::model::ActionState;
    let root = tempfile::tempdir().unwrap();
    let (a, task_a) = start(root.path(), "a", registry(&["alpha"])).await;
    running(&a, "alpha").await;
    // Nothing has been started, so nothing is on the board.
    assert!(until(&a, |_| true).await.actions.is_empty());

    invoke(&a, "alpha", "status", None).await.unwrap();
    let shown = until(&a, |s| {
        action_state_of(s, "alpha", "status") == Some(ActionState::Done)
    })
    .await;
    // Exactly one entry, named by ids, with no free text anywhere in it.
    assert_eq!(shown.actions.len(), 1);
    let json = serde_json::to_string(&shown.actions).unwrap();
    assert_eq!(
        json,
        r#"[{"plugin":"alpha","action":"status","peer":null,"state":{"state":"done"}}]"#
    );
    for client in [a] {
        client.command(Command::Shutdown {}).await.unwrap();
    }
    task_a.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_action_still_in_progress_shows_as_running_and_is_not_left_so_when_withdrawn() {
    use peersus_core::model::ActionState;
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("alpha.log");
    let (a, task_a) = start(root.path(), "a", logging_registry("alpha", &log)).await;
    let (b, task_b) = start(root.path(), "b", Registry::default()).await;
    let peer = pair(&a, &b).await;
    grant(&a, "alpha", &peer, &["alpha.use"]).await.unwrap();
    running(&a, "alpha").await;

    // This plugin leaves `share` open on purpose, so it is genuinely in progress.
    invoke(&a, "alpha", "share", Some(peer.clone()))
        .await
        .unwrap();
    log_reaches(&log, |l| l.iter().any(|x| x == "call share")).await;
    let shown = until(&a, |s| {
        action_state_of(s, "alpha", "share") == Some(ActionState::Running)
    })
    .await;
    assert_eq!(shown.actions.len(), 1);

    // The user withdraws the permission: that work is cancelled and will never be answered,
    // so the screen must stop saying it is running.
    grant(&a, "alpha", &peer, &[]).await.unwrap();
    let after = until(&a, |s| action_state_of(s, "alpha", "share").is_none()).await;
    assert!(after.actions.is_empty(), "{:?}", after.actions);

    shutdown([(a, task_a), (b, task_b)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_action_refused_before_it_started_leaves_nothing_on_the_board() {
    let root = tempfile::tempdir().unwrap();
    let (a, task_a) = start(root.path(), "a", registry(&["alpha"])).await;
    let (b, task_b) = start(root.path(), "b", Registry::default()).await;
    let peer = pair(&a, &b).await;
    running(&a, "alpha").await;
    // Not granted: refused by the device layer before anything is sent.
    assert!(
        invoke(&a, "alpha", "share", Some(peer.clone()))
            .await
            .is_err()
    );
    assert!(until(&a, |_| true).await.actions.is_empty());

    shutdown([(a, task_a), (b, task_b)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restarting_a_plugin_stops_its_actions_showing_as_running_and_leaves_another_plugins() {
    use peersus_core::model::ActionState;
    let root = tempfile::tempdir().unwrap();
    let alpha_log = root.path().join("alpha.log");
    let beta_log = root.path().join("beta.log");
    let (a, task_a) = start(
        root.path(),
        "a",
        two_logging_registry(&alpha_log, &beta_log),
    )
    .await;
    let (b, task_b) = start(root.path(), "b", Registry::default()).await;
    let peer = pair(&a, &b).await;
    grant(&a, "alpha", &peer, &["alpha.use"]).await.unwrap();
    grant(&a, "beta", &peer, &["beta.use"]).await.unwrap();
    running(&a, "alpha").await;
    running(&a, "beta").await;
    // Both leave `share` open, so both genuinely are in progress.
    invoke(&a, "alpha", "share", Some(peer.clone()))
        .await
        .unwrap();
    invoke(&a, "beta", "share", Some(peer.clone()))
        .await
        .unwrap();
    until(&a, |s| {
        action_state_of(s, "alpha", "share") == Some(ActionState::Running)
            && action_state_of(s, "beta", "share") == Some(ActionState::Running)
    })
    .await;

    // The user changes one of alpha's own settings: only alpha's process is restarted.
    a.command(Command::SetPluginSetting {
        plugin: id("alpha"),
        peer: None,
        setting: "volume".into(),
        value: peersus_core::plugins::Value::Number(7),
    })
    .await
    .unwrap();
    let after = until(&a, |s| action_state_of(s, "alpha", "share").is_none()).await;
    // alpha's old work is gone from the screen; beta's is still shown, and still running.
    assert_eq!(
        action_state_of(&after, "beta", "share"),
        Some(ActionState::Running)
    );

    shutdown([(a, task_a), (b, task_b)]).await;
}

/// How many times the plugin process has started, and with which process ids, from its log.
fn starts_of(log: &std::path::Path) -> Vec<String> {
    observed(log)
        .into_iter()
        .filter(|line| line.starts_with("start "))
        .collect()
}

/// Wait until the plugin's log shows at least `count` starts, and return them.
async fn starts_reach(log: &std::path::Path, count: usize) -> Vec<String> {
    log_reaches(log, |lines| {
        lines.iter().filter(|l| l.starts_with("start ")).count() >= count
    })
    .await;
    starts_of(log)
}

/// A quiet `target` that only logs its starts, and a `watchdog` that on start asks the
/// application for `peersus.plugins` `list` and then to restart `aims_at`, importing the
/// service's `methods`. Nothing here is paired with anything.
fn managing_registry(
    watchdog_log: &std::path::Path,
    target_log: &std::path::Path,
    aims_at: &str,
    methods: &[&str],
) -> Registry {
    use peersus_plugin_api::manifest::Import;
    let mut registry = Registry::default();
    let mut target = spec("target");
    target.arguments = vec![target_log.display().to_string()];
    registry.register(target).unwrap();
    registry
        .register(PluginSpec {
            id: id("watchdog"),
            manifest: Manifest {
                capabilities: capabilities(&[]),
                imports: vec![Import {
                    provider: HOST.to_owned(),
                    service: peersus_app::runtime::PLUGINS_SERVICE.to_owned(),
                    version: 1,
                    methods: methods.iter().map(|m| (*m).to_owned()).collect(),
                    events: Default::default(),
                }],
                ..Manifest::default()
            },
            program: Some(plugin_binary()),
            arguments: vec![
                watchdog_log.display().to_string(),
                String::new(),
                "manage".into(),
                aims_at.into(),
            ],
            restart_policy: None,
        })
        .unwrap();
    registry
}

/// Grant a plugin the application's permission to restart plugins, outright: for no device.
fn grant_restart(
    client: &Client,
    plugin: &str,
) -> impl std::future::Future<Output = Result<Reply, &'static str>> {
    client.command(Command::SetGrants {
        plugin: id(plugin),
        peer: None,
        grants: Grants::new([permission(
            HOST,
            peersus_app::runtime::PLUGINS_RESTART_CAPABILITY,
        )])
        .unwrap(),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ungranted_plugin_cannot_restart_another_and_the_other_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let (watchdog_log, target_log) = (root.path().join("w.log"), root.path().join("t.log"));
    let registry = managing_registry(&watchdog_log, &target_log, "target", &["list", "restart"]);
    let (a, task) = start(root.path(), "a", registry).await;
    assert!(
        a.snapshot().peers.is_empty(),
        "the test needs no paired device"
    );
    running(&a, "target").await;
    let lines = log_reaches(&watchdog_log, |l| {
        l.iter().any(|x| x.starts_with("result 1 ")) && l.iter().any(|x| x.starts_with("result 2 "))
    })
    .await;
    // `list` is open to a plugin that asked for the service.
    assert!(
        lines.iter().any(|l| l.starts_with("result 1 ok")),
        "list was not answered: {lines:?}"
    );
    // `restart` is guarded and was never granted: refused by the hub, as `denied`.
    assert!(
        lines.iter().any(|l| l == "result 2 denied"),
        "an ungranted restart was not refused: {lines:?}"
    );
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        starts_of(&target_log).len(),
        1,
        "the target was restarted without a grant"
    );
    shutdown([(a, task)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_granted_outright_restarts_another_with_no_device_paired() {
    let root = tempfile::tempdir().unwrap();
    let (watchdog_log, target_log) = (root.path().join("w.log"), root.path().join("t.log"));
    let registry = managing_registry(&watchdog_log, &target_log, "target", &["list", "restart"]);
    let (a, task) = start(root.path(), "a", registry).await;
    assert!(
        a.snapshot().peers.is_empty(),
        "the test needs no paired device"
    );
    running(&a, "target").await;
    let first = starts_reach(&target_log, 1).await;
    // The first run asks before anything is granted and is refused. Waiting for that makes
    // what follows ordered: the grant cannot race the question it is meant to answer.
    log_reaches(&watchdog_log, |l| l.iter().any(|x| x == "result 2 denied")).await;
    // Granted on a machine that has paired nothing: the grant is for the plugin, not for any
    // device. The process that is running is replaced by one that asks again.
    grant_restart(&a, "watchdog").await.unwrap();
    let _ = std::fs::remove_file(format!("{}.managed", watchdog_log.display()));
    a.command(Command::RestartPlugin {
        plugin: id("watchdog"),
    })
    .await
    .unwrap();
    let starts = starts_reach(&target_log, 2).await;
    assert_eq!(starts.len(), 2, "the target was not restarted: {starts:?}");
    assert_ne!(starts[0], starts[1], "the restart reused the same process");
    assert_eq!(starts[0], first[0]);
    let lines = log_reaches(&watchdog_log, |l| l.iter().any(|x| x == "result 2 ok")).await;
    assert!(lines.iter().any(|l| l == "result 2 ok"), "{lines:?}");
    // Granted outright it is a decision about no device, and it stays out of every device.
    assert!(a.snapshot().peers.is_empty());
    assert!(
        a.snapshot()
            .grants
            .get(&id("watchdog"))
            .is_some_and(|g| g.allows(&permission(
                HOST,
                peersus_app::runtime::PLUGINS_RESTART_CAPABILITY
            ))),
        "the snapshot does not show what was granted outright"
    );
    shutdown([(a, task)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_listing_a_plugin_gets_says_what_state_each_plugin_is_really_in() {
    let root = tempfile::tempdir().unwrap();
    let (watchdog_log, target_log) = (root.path().join("w.log"), root.path().join("t.log"));
    let registry = managing_registry(&watchdog_log, &target_log, "target", &["list", "restart"]);
    let (a, task) = start(root.path(), "a", registry).await;
    running(&a, "target").await;
    running(&a, "watchdog").await;
    // Only the plugin that stays up long enough to be listed as running is a fair witness, so
    // ask again with the watchdog's marker cleared: the first listing may precede `target`.
    let _ = std::fs::remove_file(format!("{}.managed", watchdog_log.display()));
    a.command(Command::RestartPlugin {
        plugin: id("watchdog"),
    })
    .await
    .unwrap();
    let lines = log_reaches(&watchdog_log, |l| {
        l.iter().filter(|x| x.starts_with("result 1 ok ")).count() >= 2
    })
    .await;
    let listing = lines
        .iter()
        .rfind(|l| l.starts_with("result 1 ok "))
        .unwrap()
        .trim_start_matches("result 1 ok ");
    let parsed: serde_json::Value = serde_json::from_str(listing).unwrap();
    let state_of = |name: &str| {
        parsed
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == name)
            .map(|e| e["status"].clone())
    };
    // The real state, not the registry's blank default of `stopped`.
    assert_eq!(
        state_of("target"),
        Some(serde_json::json!("running")),
        "{listing}"
    );
    // Id and state, and nothing else: no manifest, grants or paths.
    for entry in parsed.as_array().unwrap() {
        let mut keys: Vec<&String> = entry.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["id", "status"], "{listing}");
    }
    shutdown([(a, task)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_restarts_itself_is_restarted_exactly_once_and_settles() {
    let root = tempfile::tempdir().unwrap();
    let (watchdog_log, target_log) = (root.path().join("w.log"), root.path().join("t.log"));
    // The watchdog names itself.
    let registry = managing_registry(&watchdog_log, &target_log, "watchdog", &["list", "restart"]);
    let (a, task) = start(root.path(), "a", registry).await;
    running(&a, "watchdog").await;
    // The first process asks with nothing granted and is refused. Waiting for that finishes
    // the question before anything changes, so the grant and the marker cannot race it.
    log_reaches(&watchdog_log, |l| l.iter().any(|x| x == "result 2 denied")).await;
    grant_restart(&a, "watchdog").await.unwrap();
    // Drop the first run's marker so the next process asks again, with the grant held.
    let _ = std::fs::remove_file(format!("{}.managed", watchdog_log.display()));
    a.command(Command::RestartPlugin {
        plugin: id("watchdog"),
    })
    .await
    .unwrap();
    // What is promised is the restart: the first process, the one the command made, and the
    // one its own request made. Whether it READS the answer before it is replaced is best
    // effort, so nothing here depends on it.
    let starts = starts_reach(&watchdog_log, 3).await;
    assert!(starts.len() >= 3, "{starts:?}");
    // Exactly once: the marker keeps the process that follows from asking again, so this is a
    // request and not a loop, and the plugin settles.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        starts_of(&watchdog_log).len(),
        starts.len(),
        "it kept restarting itself"
    );
    shutdown([(a, task)]).await;
}

/// A plugin in `watch` mode that imports `peersus.plugins`' `list` and, if `events`, the
/// `state-changed` event, logging every event it is delivered.
fn watcher(name: &str, log: &std::path::Path, events: bool) -> PluginSpec {
    use peersus_plugin_api::manifest::Import;
    PluginSpec {
        id: id(name),
        manifest: Manifest {
            capabilities: capabilities(&[]),
            imports: vec![Import {
                provider: HOST.to_owned(),
                service: peersus_app::runtime::PLUGINS_SERVICE.to_owned(),
                version: 1,
                methods: ["list".to_owned()].into(),
                events: if events {
                    [peersus_app::runtime::PLUGINS_STATE_CHANGED.to_owned()].into()
                } else {
                    Default::default()
                },
            }],
            ..Manifest::default()
        },
        program: Some(plugin_binary()),
        arguments: vec![log.display().to_string(), String::new(), "watch".into()],
        restart_policy: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_asked_is_told_when_another_changes_state_and_only_its_id_and_state() {
    let root = tempfile::tempdir().unwrap();
    let (watcher_log, bystander_log, target_log) = (
        root.path().join("watcher.log"),
        root.path().join("bystander.log"),
        root.path().join("target.log"),
    );
    let mut registry = Registry::default();
    let mut target = spec("target");
    target.arguments = vec![target_log.display().to_string()];
    registry.register(target).unwrap();
    registry
        .register(watcher("watcher", &watcher_log, true))
        .unwrap();
    // Same program, same mode, but it never imported the event.
    registry
        .register(watcher("bystander", &bystander_log, false))
        .unwrap();
    let (a, task) = start(root.path(), "a", registry).await;
    running(&a, "target").await;
    running(&a, "watcher").await;
    running(&a, "bystander").await;
    // Everything above settled, so what follows is caused by what the test does next.
    log_reaches(&watcher_log, |l| l.iter().any(|x| x == "subscribed")).await;
    log_reaches(&bystander_log, |l| l.iter().any(|x| x == "subscribed")).await;
    // The target's own start-up was announced too. Wait for it, and mark where the log stands:
    // only what arrives AFTER the mark can be about the restart below.
    log_reaches(&watcher_log, |l| {
        l.iter()
            .any(|x| x.starts_with("event ") && x.contains("\"target\"") && x.contains("running"))
    })
    .await;
    let boundary = observed(&watcher_log).len();
    a.command(Command::RestartPlugin {
        plugin: id("target"),
    })
    .await
    .unwrap();
    let lines = log_reaches(&watcher_log, |l| {
        l.iter()
            .skip(boundary)
            .any(|x| x.starts_with("event ") && x.contains("\"target\""))
    })
    .await;
    for line in lines.iter().filter(|l| l.starts_with("event ")) {
        let data = line.trim_start_matches("event peersus.plugins state-changed ");
        let parsed: serde_json::Value = serde_json::from_str(data).unwrap();
        let mut keys: Vec<&String> = parsed.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["id", "status"],
            "an event said more than id and state: {line}"
        );
    }
    // The one that never asked was told nothing, though it was running throughout.
    assert!(
        !observed(&bystander_log)
            .iter()
            .any(|l| l.starts_with("event ")),
        "a plugin that did not import the event was told: {:?}",
        observed(&bystander_log)
    );
    shutdown([(a, task)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_withdrawal_made_while_a_crashed_plugin_waits_to_restart_binds_the_next_process() {
    let root = tempfile::tempdir().unwrap();
    let (watchdog_log, target_log) = (root.path().join("w.log"), root.path().join("t.log"));
    let built = managing_registry(&watchdog_log, &target_log, "target", &["list", "restart"]);
    // The watchdog asks, sees the answer, and crashes; the pause before its next process is
    // long enough for the test to act inside it, and a plugin that dies every time must not
    // be given up on before the test is done.
    let mut watchdog = built
        .get(built.find(&id("watchdog")).unwrap())
        .unwrap()
        .clone();
    watchdog.arguments.push("die".into());
    watchdog.restart_policy = Some(peersus_plugin_host::supervisor::Policy {
        first_delay: Duration::from_secs(3),
        max_delay: Duration::from_secs(3),
        max_failures: 50,
        window: Duration::from_secs(60),
        healthy_after: Duration::from_secs(60),
    });
    let mut registry = Registry::default();
    registry
        .register(
            built
                .get(built.find(&id("target")).unwrap())
                .unwrap()
                .clone(),
        )
        .unwrap();
    registry.register(watchdog).unwrap();
    // The grant is saved BEFORE the application starts, so the very first process is launched
    // with it and is answered `ok`. Granting afterwards would land between two processes
    // itself, and the test could not tell the grant from the withdrawal it is about.
    let state = root.path().join("a");
    std::fs::create_dir_all(&state).unwrap();
    peersus_net::grants::StandaloneGrants::open(state.join("grants.json"))
        .unwrap()
        .set(
            id("watchdog"),
            Grants::new([permission(
                HOST,
                peersus_app::runtime::PLUGINS_RESTART_CAPABILITY,
            )])
            .unwrap(),
        )
        .unwrap();
    let (a, task) = start(root.path(), "a", registry).await;
    log_reaches(&watchdog_log, |l| l.iter().any(|x| x == "result 2 ok")).await;
    // It has now crashed and is waiting out its pause. Withdraw the grant NOW: the decision
    // must be accepted even though no process of the plugin is registered at the hub.
    let asked_before = observed(&watchdog_log)
        .iter()
        .filter(|l| l.starts_with("result 2 "))
        .count();
    a.command(Command::SetGrants {
        plugin: id("watchdog"),
        peer: None,
        grants: Grants::default(),
    })
    .await
    .unwrap();
    // The next process starts from what the supervisor remembers, which is the OLD grant. It
    // asks again and must be refused: the withdrawal outlived the crash.
    let lines = log_reaches(&watchdog_log, |l| {
        l.iter().filter(|x| x.starts_with("result 2 ")).count() > asked_before
    })
    .await;
    let latest = lines
        .iter()
        .rfind(|l| l.starts_with("result 2 "))
        .unwrap()
        .clone();
    assert_eq!(
        latest, "result 2 denied",
        "a withdrawal made between two processes was lost: {lines:?}"
    );
    shutdown([(a, task)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_imported_only_list_cannot_restart_even_when_granted_outright() {
    let root = tempfile::tempdir().unwrap();
    let (watchdog_log, target_log) = (root.path().join("w.log"), root.path().join("t.log"));
    let registry = managing_registry(&watchdog_log, &target_log, "target", &["list"]);
    let (a, task) = start(root.path(), "a", registry).await;
    running(&a, "target").await;
    // It never imported `restart`, so the permission is not one that can be granted to it.
    assert!(
        grant_restart(&a, "watchdog").await.is_err(),
        "a permission the plugin never asked for was grantable"
    );
    shutdown([(a, task)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_two_kinds_of_permission_are_each_refused_in_the_others_place() {
    let root = tempfile::tempdir().unwrap();
    let (watchdog_log, target_log) = (root.path().join("w.log"), root.path().join("t.log"));
    let registry = managing_registry(&watchdog_log, &target_log, "target", &["list", "restart"]);
    let (a, task_a) = start(root.path(), "a", registry).await;
    let (b, task_b) = start(root.path(), "b", Registry::default()).await;
    let device = pair(&a, &b).await;
    // `restart` is about no device, so a device is no place to record it.
    let for_a_device = a
        .command(Command::SetGrants {
            plugin: id("watchdog"),
            peer: Some(device.clone()),
            grants: Grants::new([permission(
                HOST,
                peersus_app::runtime::PLUGINS_RESTART_CAPABILITY,
            )])
            .unwrap(),
        })
        .await;
    assert!(for_a_device.is_err(), "restart was stored on a device");
    let on_device = a.snapshot().peers[0].grants.clone();
    assert!(
        on_device.is_empty(),
        "something was stored on the device: {on_device:?}"
    );
    shutdown([(a, task_a), (b, task_b)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn what_was_granted_outright_survives_a_restart_of_the_application() {
    let root = tempfile::tempdir().unwrap();
    let (watchdog_log, target_log) = (root.path().join("w.log"), root.path().join("t.log"));
    let registry = managing_registry(&watchdog_log, &target_log, "target", &["list", "restart"]);
    let (a, task) = start(root.path(), "a", registry).await;
    grant_restart(&a, "watchdog").await.unwrap();
    shutdown([(a, task)]).await;
    // The same state directory, a new application.
    let registry = managing_registry(&watchdog_log, &target_log, "target", &["list", "restart"]);
    let (a, task) = start(root.path(), "a", registry).await;
    assert!(
        a.snapshot()
            .grants
            .get(&id("watchdog"))
            .is_some_and(|granted| granted.allows(&permission(
                HOST,
                peersus_app::runtime::PLUGINS_RESTART_CAPABILITY
            ))),
        "a decision made outright was forgotten"
    );
    shutdown([(a, task)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restarting_a_healthy_plugin_gives_it_a_new_process_and_leaves_the_others() {
    let root = tempfile::tempdir().unwrap();
    let (alpha_log, beta_log) = (root.path().join("alpha.log"), root.path().join("beta.log"));
    let (a, task_a) = start(
        root.path(),
        "a",
        two_logging_registry(&alpha_log, &beta_log),
    )
    .await;
    running(&a, "alpha").await;
    running(&a, "beta").await;
    let first_alpha = starts_reach(&alpha_log, 1).await;
    let first_beta = starts_reach(&beta_log, 1).await;
    assert_eq!((first_alpha.len(), first_beta.len()), (1, 1));

    // Nothing is wrong with alpha. It is restarted anyway, because asking is enough: the
    // way a plugin is updated, or a watchdog or a person tidies one up.
    a.command(Command::RestartPlugin {
        plugin: id("alpha"),
    })
    .await
    .unwrap();

    let alpha_starts = starts_reach(&alpha_log, 2).await;
    assert_eq!(
        alpha_starts.len(),
        2,
        "alpha was not restarted: {alpha_starts:?}"
    );
    assert_ne!(
        alpha_starts[0], alpha_starts[1],
        "the restart reused the same process"
    );
    // Give a wrongly restarted beta time to show itself, then check it did not.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        starts_of(&beta_log),
        first_beta,
        "another plugin was restarted too"
    );
    shutdown([(a, task_a)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restarting_a_plugin_that_does_not_exist_is_refused_and_touches_nothing() {
    let root = tempfile::tempdir().unwrap();
    let (alpha_log, beta_log) = (root.path().join("alpha.log"), root.path().join("beta.log"));
    let (a, task_a) = start(
        root.path(),
        "a",
        two_logging_registry(&alpha_log, &beta_log),
    )
    .await;
    running(&a, "alpha").await;
    running(&a, "beta").await;
    let before = (
        starts_reach(&alpha_log, 1).await,
        starts_reach(&beta_log, 1).await,
    );
    assert!(
        a.command(Command::RestartPlugin {
            plugin: id("missing")
        })
        .await
        .is_err()
    );
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!((starts_of(&alpha_log), starts_of(&beta_log)), before);
    shutdown([(a, task_a)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restarting_a_plugin_withdraws_what_it_was_doing_like_a_changed_setting_does() {
    use peersus_core::model::ActionState;
    let root = tempfile::tempdir().unwrap();
    let (alpha_log, beta_log) = (root.path().join("alpha.log"), root.path().join("beta.log"));
    let (a, task_a) = start(
        root.path(),
        "a",
        two_logging_registry(&alpha_log, &beta_log),
    )
    .await;
    let (b, task_b) = start(root.path(), "b", Registry::default()).await;
    let peer = pair(&a, &b).await;
    grant(&a, "alpha", &peer, &["alpha.use"]).await.unwrap();
    grant(&a, "beta", &peer, &["beta.use"]).await.unwrap();
    running(&a, "alpha").await;
    running(&a, "beta").await;
    invoke(&a, "alpha", "share", Some(peer.clone()))
        .await
        .unwrap();
    invoke(&a, "beta", "share", Some(peer.clone()))
        .await
        .unwrap();
    until(&a, |s| {
        action_state_of(s, "alpha", "share") == Some(ActionState::Running)
            && action_state_of(s, "beta", "share") == Some(ActionState::Running)
    })
    .await;

    a.command(Command::RestartPlugin {
        plugin: id("alpha"),
    })
    .await
    .unwrap();

    // What the old process was doing will never be answered, so it stops showing as running:
    // exactly what a changed setting does, because it is the same restart.
    let after = until(&a, |s| action_state_of(s, "alpha", "share").is_none()).await;
    assert_eq!(
        action_state_of(&after, "beta", "share"),
        Some(ActionState::Running)
    );
    shutdown([(a, task_a), (b, task_b)]).await;
}

/// A plugin, `clip`, with one action, `share`, that needs a device. When it is pressed the
/// plugin sends `payload` to the device it was told about, through `peersus.peers`, and
/// records the argument it was handed exactly as it arrived. One grant, `peers.send`, covers
/// both the action and the send.
fn sharing_registry(log: &std::path::Path, payload: &std::path::Path) -> Registry {
    use peersus_plugin_api::manifest::Import;
    let mut registry = Registry::default();
    registry
        .register(PluginSpec {
            id: id("clip"),
            manifest: Manifest {
                capabilities: capabilities(&["share.use"]),
                imports: vec![Import {
                    provider: HOST.to_owned(),
                    service: peersus_app::runtime::PEERS_SERVICE.to_owned(),
                    version: 1,
                    methods: ["send".to_owned()].into(),
                    events: Default::default(),
                }],
                exports: vec![Export {
                    service: "clip.service".into(),
                    version: 1,
                    description: "Shares what the plugin holds with a device.".into(),
                    consumers: [HOST.to_owned()].into(),
                    methods: BTreeMap::from([("share".to_owned(), member(Some("share.use")))]),
                    events: BTreeMap::new(),
                }],
                contributions: Contributions {
                    actions: vec![Action {
                        id: "share".into(),
                        label: "Share".into(),
                        short_label: None,
                        target: Source {
                            service: "clip.service".into(),
                            method: "share".into(),
                        },
                        capability: Some("share.use".to_owned()),
                        needs_device: true,
                        destructive: false,
                        timeout_ms: peersus_plugin_api::contributions::default_action_timeout(),
                    }],
                    ..Contributions::default()
                },
                ..Manifest::default()
            },
            program: Some(plugin_binary()),
            arguments: vec![
                log.display().to_string(),
                payload.display().to_string(),
                "share-to-peer".into(),
            ],
            restart_policy: None,
        })
        .unwrap();
    registry
}

/// The way a user really sends something: they press a button for a chosen device. The
/// plugin is told which device by the argument the application hands it, and sends to that
/// device through `peersus.peers`. The device must receive the bytes, and the plugin must
/// have been given a name for the device, never its network key.
#[tokio::test(flavor = "multi_thread")]
async fn pressing_share_for_a_device_sends_to_that_device_and_the_plugin_never_sees_its_key() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("clip.log");
    let payload = root.path().join("payload.bin");
    let bytes: Vec<u8> = (0..40_001u32)
        .map(|n| (n.wrapping_mul(17) % 211) as u8)
        .collect();
    std::fs::write(&payload, &bytes).unwrap();
    let (runtime, task) = start(root.path(), "a", sharing_registry(&log, &payload)).await;
    running(&runtime, "clip").await;
    let (chosen, chosen_id) = paired_sender(root.path(), "chosen", &runtime).await;
    let (other, other_id) = paired_sender(root.path(), "other", &runtime).await;
    for device in [&chosen_id, &other_id] {
        // The plugin's own guard on its `share` action, and the application's own permission
        // to send. They are two different permissions that merely share a purpose: one is
        // the plugin's, the other the application's. A grant replaces the whole set, so both
        // are given together.
        runtime
            .command(Command::SetGrants {
                plugin: id("clip"),
                peer: Some(device.clone()),
                grants: Grants::new([
                    permission("clip", "share.use"),
                    permission(HOST, peersus_app::runtime::PEERS_SEND_CAPABILITY),
                ])
                .unwrap(),
            })
            .await
            .unwrap();
    }

    // The user presses the button for ONE of the two devices.
    invoke(&runtime, "clip", "share", Some(chosen_id.clone()))
        .await
        .unwrap();

    let arrived = received_for(
        &chosen,
        "sink.service",
        bytes.len(),
        Duration::from_secs(15),
    )
    .await;
    assert_eq!(
        arrived.as_deref(),
        Some(bytes.as_slice()),
        "the chosen device did not get what was sent: {:?}",
        observed(&log)
    );
    // The other device, though it granted the same thing, is not sent anything.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        other.received.lock().unwrap().is_empty(),
        "a device that was not chosen was sent bytes"
    );

    // What the plugin was handed is a name for the device, and the key is nowhere in it.
    let lines = observed(&log);
    let handed = lines
        .iter()
        .find(|l| l.starts_with("arg "))
        .expect("the plugin recorded the argument it was given");
    assert!(
        !handed.contains(&chosen_id.0),
        "the plugin was told the device's network key: {handed}"
    );
    assert!(
        !handed.contains(&other_id.0),
        "the plugin was told another device's network key: {handed}"
    );
    runtime.command(Command::Shutdown {}).await.unwrap();
    task.await.unwrap();
}

// ---- Installing, enabling, disabling and removing from the window ----------------------------

/// Like `start`, but the owner is given a store of packages in `data`, as the real one is.
async fn start_with_packages(
    root: &std::path::Path,
    name: &str,
    data: &std::path::Path,
) -> (Client, tokio::task::JoinHandle<()>) {
    let (client, task, _private) = start_with_packages_in(root, name, data).await;
    (client, task)
}

/// [`start_with_packages`], also giving the owner's private directory, where it unpacks archives.
async fn start_with_packages_in(
    root: &std::path::Path,
    name: &str,
    data: &std::path::Path,
) -> (Client, tokio::task::JoinHandle<()>, PathBuf) {
    start_with_packages_and_registry(root, name, data, Registry::default(), None).await
}

async fn start_with_packages_and_registry(
    root: &std::path::Path,
    name: &str,
    data: &std::path::Path,
    registry: Registry,
    bundled: Option<PluginId>,
) -> (Client, tokio::task::JoinHandle<()>, PathBuf) {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    let mut store = SettingsStore::open(path.join("preferences.json")).unwrap();
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
        Peers::open(path.join("peers.json")).unwrap(),
        History::open(path.join("history.json")).unwrap(),
    );
    let directory = sockets();
    let private = directory.path().to_path_buf();
    let standalone = peersus_net::grants::StandaloneGrants::open(path.join("grants.json")).unwrap();
    let (runtime, client) = Runtime::new(
        network,
        standalone,
        store,
        registry,
        directory.path().to_path_buf(),
    )
    .unwrap();
    let mut runtime = runtime.with_packages(peersus_app::plugin_manager::Manager::new(data));
    if let Some(plugin) = bundled {
        runtime = runtime.with_launcher(plugin, Arc::new(BundledTestLauncher));
    }
    let task = tokio::spawn(async move {
        runtime.run().await;
        drop(directory);
    });
    (client, task, private)
}

struct BundledTestLauncher;
impl peersus_plugin_host::supervisor::Launcher for BundledTestLauncher {
    fn launch(
        &self,
        bootstrap: peersus_plugin_host::supervisor::Bootstrap,
    ) -> std::io::Result<Box<dyn peersus_plugin_host::supervisor::Lifecycle>> {
        peersus_plugin_host::supervisor::Launch {
            program: plugin_binary(),
            arguments: vec![],
            socket: bootstrap.socket,
            data_socket: bootstrap.data_socket,
            token: bootstrap.token,
        }
        .spawn()
        .map(|running| Box::new(running) as Box<dyn peersus_plugin_host::supervisor::Lifecycle>)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_external_store_does_not_break_or_replace_a_bundled_service() {
    use peersus_core::model::PluginStatus;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let mut registry = Registry::default();
    let mut bundled = spec("included");
    bundled.program = None;
    registry.register(bundled).unwrap();
    let (client, task, _) = start_with_packages_and_registry(
        root.path(),
        "owner",
        data.path(),
        registry,
        Some(id("included")),
    )
    .await;
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == id("included") && p.status == PluginStatus::Running)
    })
    .await;
    client
        .command(Command::DisablePlugin {
            plugin: id("included"),
        })
        .await
        .expect("a package store must not require a bundled plugin to have an external folder");
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == id("included") && p.status == PluginStatus::Stopped)
    })
    .await;
    client
        .command(Command::EnablePlugin {
            plugin: id("included"),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == id("included") && p.status == PluginStatus::Running)
    })
    .await;
    let replacement = package_on_disk(source.path(), "included");
    assert!(
        client
            .command(Command::StageInstall {
                source: source_dir(&replacement)
            })
            .await
            .is_err(),
        "an external package must not replace a bundled lifecycle"
    );
    assert!(
        client
            .command(Command::RemovePackage {
                plugin: id("included")
            })
            .await
            .is_err()
    );
    assert!(
        client
            .snapshot()
            .plugins
            .iter()
            .any(|p| p.id == id("included") && p.status == PluginStatus::Running)
    );
    assert!(!data.path().join("plugins/included").exists());
    client
        .command(Command::InvokeAction {
            plugin: id("included"),
            action: "status".into(),
            peer: None,
        })
        .await
        .unwrap();
    until(&client, |s| {
        action_state_of(s, "included", "status") == Some(peersus_core::model::ActionState::Done)
    })
    .await;
    client.command(Command::Shutdown {}).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
}

/// A package on disk the way a person would have it: a folder named for the plugin, with its
/// manifest and a program that really is a plugin (the test plugin, copied).
fn package_on_disk(parent: &std::path::Path, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.json"),
        serde_json::to_vec(&spec(name).manifest).unwrap(),
    )
    .unwrap();
    std::fs::copy(plugin_binary(), dir.join("plugin")).unwrap();
    std::fs::set_permissions(dir.join("plugin"), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn source_dir(path: &std::path::Path) -> peersus_core::model::SourceDir {
    peersus_core::model::SourceDir::new(path).unwrap()
}

/// Start an owner exactly as the program does: the store is LOADED first, and what loading reports (what runs, what is
/// switched off, and what was found and is not run) is what the owner is given. `start_with_packages` skips that and gives
/// an empty registry, so it cannot show what a person sees when a plugin was already broken before the program started.
async fn start_from_store(
    root: &std::path::Path,
    name: &str,
    data: &std::path::Path,
) -> (Client, tokio::task::JoinHandle<()>) {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    let mut store = SettingsStore::open(path.join("preferences.json")).unwrap();
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
        Peers::open(path.join("peers.json")).unwrap(),
        History::open(path.join("history.json")).unwrap(),
    );
    let directory = sockets();
    let standalone = peersus_net::grants::StandaloneGrants::open(path.join("grants.json")).unwrap();
    let found = peersus_app::plugin_manager::Manager::new(data).load();
    let (runtime, client) = Runtime::new(
        network,
        standalone,
        store,
        found.registry,
        directory.path().to_path_buf(),
    )
    .unwrap();
    let runtime = runtime
        .with_packages(peersus_app::plugin_manager::Manager::new(data))
        .with_disabled(found.disabled)
        .with_held_back(found.held_back);
    let task = tokio::spawn(async move {
        runtime.run().await;
        drop(directory);
    });
    (client, task)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_was_broken_before_the_program_started_is_in_the_snapshot_and_can_be_removed_and_installed_again()
 {
    use peersus_core::model::Standing;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        data.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let parent = tempfile::tempdir().unwrap();
    let broken = id("wrecked");

    // Installed with the program's own installer, then damaged WHILE NO OWNER RUNS: its manifest is destroyed and the record
    // of installations is destroyed too. At the next start it is not in the registry, which is the state the person was in.
    peersus_app::plugin_manager::Manager::new(data.path())
        .install(&package_on_disk(parent.path(), "wrecked"))
        .unwrap();
    std::fs::write(
        data.path()
            .join("plugins")
            .join("wrecked")
            .join("plugin.json"),
        b"destroyed {{{",
    )
    .unwrap();
    std::fs::write(data.path().join("installed.json"), b"destroyed {{{").unwrap();

    let (client, _task) = start_from_store(root.path(), "owner", data.path()).await;
    // It is IN THE SNAPSHOT, broken, though it never entered the registry: it can be seen, which it could not be.
    let seen = until(&client, |s| s.plugins.iter().any(|p| p.id == broken)).await;
    let info = seen.plugins.iter().find(|p| p.id == broken).unwrap();
    assert_eq!(info.standing, Standing::Broken);
    assert_eq!(info.status, peersus_core::model::PluginStatus::Stopped);

    // And it can be removed from where the person is: the owner no longer answers "unknown plugin" for what it did not load.
    client
        .command(Command::RemovePackage {
            plugin: broken.clone(),
        })
        .await
        .expect("a plugin that never loaded could not be removed");
    let after = until(&client, |s| s.plugins.iter().all(|p| p.id != broken)).await;
    assert!(after.plugins.iter().all(|p| p.id != broken));
    assert!(
        !data.path().join("plugins").join("wrecked").exists(),
        "its folder was left behind"
    );

    // The name is free: the same plugin installs again and runs.
    let source = package_on_disk(parent.path(), "wrecked");
    client
        .command(Command::StageInstall {
            source: source_dir(&source),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == broken)).await;
    client
        .command(Command::ConfirmInstall {
            plugin: broken.clone(),
        })
        .await
        .unwrap();
    let again = until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == broken && p.status == peersus_core::model::PluginStatus::Running)
    })
    .await;
    assert_eq!(
        again
            .plugins
            .iter()
            .find(|p| p.id == broken)
            .unwrap()
            .standing,
        Standing::Ready
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_nobody_approved_is_listed_and_not_run() {
    use peersus_core::model::Standing;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        data.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let parent = tempfile::tempdir().unwrap();
    let manager = peersus_app::plugin_manager::Manager::new(data.path());
    manager
        .install(&package_on_disk(parent.path(), "approved"))
        .unwrap();
    // A package put in the store by hand: good in every way, and nobody approved it.
    let by_hand = package_on_disk(&data.path().join("plugins"), "unapproved");
    std::fs::set_permissions(
        &by_hand,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();

    let (client, _task) = start_from_store(root.path(), "owner", data.path()).await;
    let seen = until(&client, |s| s.plugins.len() == 2).await;
    let standing = |name: &str| {
        seen.plugins
            .iter()
            .find(|p| p.id.as_str() == name)
            .map(|p| (p.standing, p.status))
    };
    assert_eq!(standing("approved").map(|s| s.0), Some(Standing::Ready));
    assert_eq!(
        standing("unapproved"),
        Some((
            Standing::NeedsApproval,
            peersus_core::model::PluginStatus::Stopped
        ))
    );
    // Nothing of it ever runs: the registry never held it, so there is no process to start.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let later = client.snapshot();
    assert_eq!(
        later
            .plugins
            .iter()
            .find(|p| p.id.as_str() == "unapproved")
            .map(|p| p.status),
        Some(peersus_core::model::PluginStatus::Stopped)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_cannot_be_run_is_not_approved_even_when_its_program_can_be_measured() {
    use peersus_core::model::Standing;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        data.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    std::fs::create_dir_all(data.path().join("plugins")).unwrap();
    std::fs::set_permissions(
        data.path().join("plugins"),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    // Complete in every way except that its program is not allowed to run: Broken, yet its bytes can be measured.
    let folder = package_on_disk(&data.path().join("plugins"), "inert");
    std::fs::set_permissions(&folder, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    std::fs::set_permissions(
        folder.join("plugin"),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let measured =
        peersus_app::plugin_approval::program_digest(&folder.join("plugin"), 64 * 1024 * 1024)
            .expect("the program could not be measured");

    let (client, _task) = start_from_store(root.path(), "owner", data.path()).await;
    let seen = until(&client, |s| {
        s.plugins.iter().any(|p| p.id.as_str() == "inert")
    })
    .await;
    assert_eq!(seen.plugins[0].standing, Standing::Broken);
    // With the right digest for the bytes that are there, it is still refused: broken is not something to approve.
    assert!(
        client
            .command(Command::ApprovePlugin {
                plugin: id("inert"),
                program: measured
            })
            .await
            .is_err()
    );
    assert!(
        !folder.join("approval.json").exists(),
        "a plugin that cannot run was approved"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn approving_a_plugin_as_it_was_shown_runs_it_at_once_and_what_was_not_shown_never_does() {
    use peersus_core::model::{PluginStatus, Standing};
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        data.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    std::fs::create_dir_all(data.path().join("plugins")).unwrap();
    std::fs::set_permissions(
        data.path().join("plugins"),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    // Put in the store by hand: complete, and nobody approved it.
    let waiting = package_on_disk(&data.path().join("plugins"), "waiting");
    std::fs::set_permissions(
        &waiting,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    // And one that is broken: no usable manifest.
    let broken = package_on_disk(&data.path().join("plugins"), "wrecked");
    std::fs::set_permissions(&broken, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    std::fs::write(broken.join("plugin.json"), b"destroyed {{{").unwrap();

    let (client, _task) = start_from_store(root.path(), "owner", data.path()).await;
    let seen = until(&client, |s| s.plugins.len() == 2).await;
    let shown = |snapshot: &Snapshot, name: &str| {
        snapshot
            .plugins
            .iter()
            .find(|p| p.id.as_str() == name)
            .map(|p| (p.standing, p.program.clone()))
    };
    let (standing, digest) = shown(&seen, "waiting").unwrap();
    assert_eq!(standing, Standing::NeedsApproval);
    let digest = digest.expect("no digest was offered for a plugin that waits");
    // A broken plugin is offered nothing to approve.
    assert_eq!(shown(&seen, "wrecked"), Some((Standing::Broken, None)));

    // Refused, and nothing runs: a digest that is not the one shown, an empty one, a broken plugin, an unknown one.
    for (name, program) in [
        ("waiting", "0".repeat(64)),
        ("waiting", String::new()),
        ("wrecked", digest.clone()),
        ("nothing", digest.clone()),
    ] {
        let refused = client
            .command(Command::ApprovePlugin {
                plugin: id(name),
                program: program.clone(),
            })
            .await;
        assert!(refused.is_err(), "{name} with {program:?} was approved");
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        client
            .snapshot()
            .plugins
            .iter()
            .find(|p| p.id.as_str() == "waiting")
            .map(|p| p.status),
        Some(PluginStatus::Stopped)
    );

    // Swapped after it was shown: the digest that was shown is not what is there, so it is not approved and never runs.
    let program = waiting.join("plugin");
    let kept = std::fs::read(&program).unwrap();
    std::fs::write(&program, b"another program").unwrap();
    assert!(
        client
            .command(Command::ApprovePlugin {
                plugin: id("waiting"),
                program: digest.clone()
            })
            .await
            .is_err()
    );
    assert!(
        !waiting.join("approval.json").exists(),
        "an approval was written for a program that was not shown"
    );
    // The owner holds the digest it REPORTED, and does not take the folder's word for what was shown: the digest of the
    // swapped program is what is on disk now, which the manager would accept, and it is still not what the window was shown.
    let swapped = {
        use sha2::Digest;
        let bytes = std::fs::read(&program).unwrap();
        let mut hash = sha2::Sha256::new();
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
        hash.update([1u8]);
        hex::encode(hash.finalize())
    };
    assert!(
        client
            .command(Command::ApprovePlugin {
                plugin: id("waiting"),
                program: swapped
            })
            .await
            .is_err()
    );
    assert!(
        !waiting.join("approval.json").exists(),
        "the owner approved a program it never showed"
    );
    std::fs::write(&program, kept).unwrap();

    // Approved as shown: it runs at once, with no restart, and it is ready.
    client
        .command(Command::ApprovePlugin {
            plugin: id("waiting"),
            program: digest,
        })
        .await
        .expect("what was shown could not be approved");
    let running = until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id.as_str() == "waiting" && p.status == PluginStatus::Running)
    })
    .await;
    assert_eq!(shown(&running, "waiting"), Some((Standing::Ready, None)));
    assert_eq!(
        running
            .plugins
            .iter()
            .filter(|p| p.id.as_str() == "waiting")
            .count(),
        1,
        "it is listed twice after approval"
    );
    assert!(waiting.join("approval.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_installed_from_the_window_runs_at_once_and_a_removed_one_leaves_nothing_behind() {
    use peersus_core::model::PluginStatus::Running;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let source = package_on_disk(parent.path(), "extra");
    let (client, _task) = start_with_packages(root.path(), "owner", data.path()).await;
    let extra = id("extra");

    // 1. The person chooses the folder: it is READ and shown, and nothing is installed.
    client
        .command(Command::StageInstall {
            source: source_dir(&source),
        })
        .await
        .unwrap();
    let shown = until(&client, |s| s.staged.iter().any(|p| p.id == extra)).await;
    assert!(
        shown.plugins.iter().all(|p| p.id != extra),
        "it was installed by only being shown"
    );
    assert!(!data.path().join("plugins").join("extra").exists());

    // 2. Confirming installs it and it RUNS, with no restart of the owner.
    client
        .command(Command::ConfirmInstall {
            plugin: extra.clone(),
        })
        .await
        .unwrap();
    let running = until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == extra && p.status == Running)
    })
    .await;
    assert!(
        running.staged.is_empty(),
        "it is still waiting after it was confirmed"
    );

    // 3. It starts with NOTHING granted: a new plugin is a new thing.
    assert!(
        running.grants.get(&extra).is_none_or(|g| g.is_empty()),
        "it started with rights"
    );

    // Running is not enough: the owner's imports must include the freshly installed
    // plugin, so its real unguarded action works without restarting the application.
    invoke(&client, "extra", "status", None).await.unwrap();
    until(&client, |s| {
        action_state_of(s, "extra", "status") == Some(peersus_core::model::ActionState::Done)
    })
    .await;

    // 4. Disabling stops it and it stays installed; enabling runs it again.
    client
        .command(Command::DisablePlugin {
            plugin: extra.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == extra && p.status != Running)
    })
    .await;
    assert!(
        data.path().join("plugins").join("extra").exists(),
        "disabling removed it"
    );
    client
        .command(Command::EnablePlugin {
            plugin: extra.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == extra && p.status == Running)
    })
    .await;

    // 5. Grant it something outright, then remove it: files, rights and listing all go.
    client
        .command(Command::SetGrants {
            plugin: extra.clone(),
            peer: None,
            grants: Grants::new([permission("extra", "extra.use")]).unwrap(),
        })
        .await
        .ok();
    client
        .command(Command::RemovePackage {
            plugin: extra.clone(),
        })
        .await
        .unwrap();
    let gone = until(&client, |s| s.plugins.iter().all(|p| p.id != extra)).await;
    assert!(
        !data.path().join("plugins").join("extra").exists(),
        "its files stayed"
    );
    assert!(
        !gone.grants.contains_key(&extra),
        "its rights stayed in the record"
    );

    // 6. A package installed afterwards under the SAME name starts with nothing again.
    client
        .command(Command::StageInstall {
            source: source_dir(&source),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == extra)).await;
    client
        .command(Command::ConfirmInstall {
            plugin: extra.clone(),
        })
        .await
        .unwrap();
    let again = until(&client, |s| s.plugins.iter().any(|p| p.id == extra)).await;
    assert!(
        again.grants.get(&extra).is_none_or(|g| g.is_empty()),
        "the new package inherited rights"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_that_changed_after_it_was_shown_is_not_installed() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let source = package_on_disk(parent.path(), "swap");
    let (client, _task) = start_with_packages(root.path(), "owner", data.path()).await;
    let swap = id("swap");

    client
        .command(Command::StageInstall {
            source: source_dir(&source),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == swap)).await;
    // After the look and before the confirmation, the program in the folder is replaced.
    std::fs::write(source.join("plugin"), b"#!/bin/sh\necho changed\n").unwrap();
    let answer = client
        .command(Command::ConfirmInstall {
            plugin: swap.clone(),
        })
        .await;
    assert!(answer.is_err(), "a package that changed was installed");
    assert!(
        !data.path().join("plugins").join("swap").exists(),
        "its files were installed"
    );
    let after = until(&client, |s| s.staged.iter().all(|p| p.id != swap)).await;
    assert!(after.plugins.iter().all(|p| p.id != swap));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_store_of_packages_the_install_commands_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let (client, _task) = start(root.path(), "owner", Registry::default()).await;
    let parent = tempfile::tempdir().unwrap();
    let source = package_on_disk(parent.path(), "nope");
    let staged = client
        .command(Command::StageInstall {
            source: source_dir(&source),
        })
        .await;
    assert!(staged.is_err(), "an owner with no store accepted a package");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_changed_behind_the_applications_back_can_still_be_removed_and_leaves_nothing_running()
 {
    use peersus_core::model::PluginStatus::Running;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let source = package_on_disk(parent.path(), "stays");
    let (client, _task) = start_with_packages(root.path(), "owner", data.path()).await;
    let stays = id("stays");
    client
        .command(Command::StageInstall {
            source: source_dir(&source),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == stays)).await;
    client
        .command(Command::ConfirmInstall {
            plugin: stays.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == stays && p.status == Running)
    })
    .await;

    // Someone changes the installed files behind the application's back. This used to leave the plugin installed and
    // impossible to remove (the store refused to delete what it had not installed); a plugin must never be in a state
    // that can be neither used nor left, so the removal now goes through whatever the files are.
    let installed = data
        .path()
        .join("plugins")
        .join("stays")
        .join("plugin.json");
    std::fs::write(&installed, b"{}").unwrap();

    client
        .command(Command::RemovePackage {
            plugin: stays.clone(),
        })
        .await
        .expect("a plugin that was changed could not be removed");
    let after = until(&client, |s| s.plugins.iter().all(|p| p.id != stays)).await;
    assert!(
        after.plugins.iter().all(|p| p.id != stays),
        "it is still listed after it was removed"
    );
    assert!(
        !data.path().join("plugins").join("stays").exists(),
        "its folder was left behind"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn what_a_window_turns_on_for_a_device_is_what_the_owner_offered_and_is_kept_across_a_restart()
 {
    use peersus_core::model::PluginStatus::Running;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let source = package_on_disk(parent.path(), "allowed");
    let allowed = id("allowed");

    let device = {
        let (client, task) = start_with_packages(root.path(), "first", data.path()).await;
        let (other, _other_task) = start(root.path(), "other", Registry::default()).await;
        client
            .command(Command::StageInstall {
                source: source_dir(&source),
            })
            .await
            .unwrap();
        until(&client, |s| s.staged.iter().any(|p| p.id == allowed)).await;
        client
            .command(Command::ConfirmInstall {
                plugin: allowed.clone(),
            })
            .await
            .unwrap();
        let installed = until(&client, |s| {
            s.plugins
                .iter()
                .any(|p| p.id == allowed && p.status == Running)
        })
        .await;
        let device = pair(&client, &other).await;

        // What the window offers is what the owner SAYS can be granted: for a device, and nothing else.
        let told = &installed
            .plugins
            .iter()
            .find(|p| p.id == allowed)
            .unwrap()
            .grantable;
        assert!(
            !told.per_device.is_empty(),
            "the owner offers a plugin that guards something nothing at all"
        );
        assert!(
            told.outright.is_empty(),
            "a plugin with no application guard was offered an outright grant"
        );
        let wanted = Grants::new(told.per_device.iter().cloned()).unwrap();

        // Turned on for the device from the window: accepted, and the snapshot shows it for that device.
        let answer = client
            .command(Command::SetGrants {
                plugin: allowed.clone(),
                peer: Some(device.clone()),
                grants: wanted.clone(),
            })
            .await;
        assert!(
            answer.is_ok(),
            "the owner refused what the Permissions tab offered: {:?}",
            answer.as_ref().err()
        );
        let shown = until(&client, |s| {
            s.peers
                .iter()
                .any(|p| p.id == device && p.grants.get(&allowed).is_some_and(|g| !g.is_empty()))
        })
        .await;
        let held = shown
            .peers
            .iter()
            .find(|p| p.id == device)
            .unwrap()
            .grants
            .get(&allowed)
            .unwrap()
            .clone();
        assert_eq!(held, wanted);

        // Turned off again: gone for that device.
        client
            .command(Command::SetGrants {
                plugin: allowed.clone(),
                peer: Some(device.clone()),
                grants: Grants::default(),
            })
            .await
            .unwrap();
        until(&client, |s| {
            s.peers
                .iter()
                .any(|p| p.id == device && p.grants.get(&allowed).is_none_or(|g| g.is_empty()))
        })
        .await;
        // And on once more, to see that it survives the owner being shut down.
        client
            .command(Command::SetGrants {
                plugin: allowed.clone(),
                peer: Some(device.clone()),
                grants: wanted.clone(),
            })
            .await
            .unwrap();
        client.command(Command::Shutdown {}).await.unwrap();
        task.await.unwrap();
        device
    };

    let found = peersus_app::plugin_manager::Manager::new(data.path()).load();
    let (client, _task) = start_loaded(root.path(), "first", data.path(), found).await;
    let after = until(&client, |s| s.plugins.iter().any(|p| p.id == allowed)).await;
    assert!(
        after
            .peers
            .iter()
            .any(|p| p.id == device && p.grants.get(&allowed).is_some_and(|g| !g.is_empty())),
        "what was allowed for the device was lost over a restart"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn giving_up_a_shown_package_installs_nothing_and_cannot_be_confirmed_afterwards() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let source = package_on_disk(parent.path(), "declined");
    let (client, _task) = start_with_packages(root.path(), "owner", data.path()).await;
    let declined = id("declined");

    client
        .command(Command::StageInstall {
            source: source_dir(&source),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == declined)).await;
    client
        .command(Command::CancelInstall {
            plugin: declined.clone(),
        })
        .await
        .unwrap();
    let after = until(&client, |s| s.staged.iter().all(|p| p.id != declined)).await;
    assert!(
        after.plugins.iter().all(|p| p.id != declined),
        "it was installed by being given up"
    );
    assert!(!data.path().join("plugins").join("declined").exists());
    // What was given up cannot be confirmed afterwards: it has to be chosen and shown again.
    assert!(
        client
            .command(Command::ConfirmInstall {
                plugin: declined.clone()
            })
            .await
            .is_err()
    );
    // And giving up what is not waiting is refused, not silently accepted.
    assert!(
        client
            .command(Command::CancelInstall { plugin: declined })
            .await
            .is_err()
    );
}

/// A package that names no files besides its manifest and program, with a file beside them that is
/// more than a package may hold, as the rest of a home folder would be.
fn package_with_a_huge_rest(parent: &std::path::Path, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut manifest = spec(name).manifest;
    manifest.files = Some(Vec::new());
    std::fs::write(
        dir.join("plugin.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::copy(plugin_binary(), dir.join("plugin")).unwrap();
    std::fs::set_permissions(dir.join("plugin"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let big = std::fs::File::create(dir.join("sources.bin")).unwrap();
    big.set_len(peersus_app::plugin_manager::MAX_BYTES + 1)
        .unwrap();
    std::fs::write(dir.join("README.md"), b"read me").unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_package_with_a_huge_rest_installs_from_a_folder_and_from_an_archive_without_the_rest()
 {
    use peersus_core::model::PluginStatus::Running;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let (client, _task) = start_with_packages(root.path(), "owner", data.path()).await;

    // As a folder.
    let folder = package_with_a_huge_rest(parent.path(), "asfolder");
    client
        .command(Command::StageInstall {
            source: source_dir(&folder),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == id("asfolder"))).await;
    client
        .command(Command::ConfirmInstall {
            plugin: id("asfolder"),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == id("asfolder") && p.status == Running)
    })
    .await;
    let installed = data.path().join("plugins").join("asfolder");
    assert!(installed.join("plugin").is_file());
    assert!(
        !installed.join("sources.bin").exists(),
        "the huge file was installed from a folder"
    );
    assert!(!installed.join("README.md").exists());

    // As an archive: the very same kind of package.
    let again = package_with_a_huge_rest(parent.path(), "asarchive");
    let archive = archive_of(&again, parent.path());
    // The archive really carries the huge file: otherwise leaving it out proves nothing.
    let mut listed = Vec::new();
    for entry in tar::Archive::new(flate2::read::GzDecoder::new(
        std::fs::File::open(&archive).unwrap(),
    ))
    .entries()
    .unwrap()
    {
        let entry = entry.unwrap();
        listed.push((
            entry.path().unwrap().to_string_lossy().into_owned(),
            entry.header().size().unwrap(),
        ));
    }
    assert!(
        listed
            .iter()
            .any(|(name, size)| name.ends_with("sources.bin")
                && *size > peersus_app::plugin_manager::MAX_BYTES),
        "the test archive does not hold the huge file, so it proves nothing: {listed:?}"
    );
    client
        .command(Command::StageInstall {
            source: source_dir(&archive),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.staged.iter().any(|p| p.id == id("asarchive"))
    })
    .await;
    client
        .command(Command::ConfirmInstall {
            plugin: id("asarchive"),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == id("asarchive") && p.status == Running)
    })
    .await;
    let installed = data.path().join("plugins").join("asarchive");
    assert!(installed.join("plugin").is_file());
    assert!(
        !installed.join("sources.bin").exists(),
        "the huge file was installed from an archive"
    );
    assert!(!installed.join("README.md").exists());
}

/// The folder `package_on_disk` made, as the one `.tar.gz` a person would be given: the folder is
/// the one thing at the top, as `tar czf name.tar.gz name/` makes it.
fn archive_of(folder: &std::path::Path, into: &std::path::Path) -> PathBuf {
    let name = folder.file_name().unwrap().to_string_lossy().into_owned();
    let file = into.join(format!("{name}.tar.gz"));
    let encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&file).unwrap(),
        flate2::Compression::fast(),
    );
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    builder.append_dir_all(&name, folder).unwrap();
    builder.into_inner().unwrap().finish().unwrap();
    file
}

/// What the owner holds in its own private directory while an archive is unpacked there.
fn unpacked_folders(private: &std::path::Path) -> usize {
    std::fs::read_dir(private)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".peersus-archive")
        })
        .count()
}

/// `spec(name)`'s manifest written by hand as TOML, with the real test plugin as the program.
fn package_with_toml_manifest(parent: &std::path::Path, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let toml = format!(
        r#"
manifest_version = 1
imports = []

[capabilities]
"{name}.use" = "Lets {name} do what it does."

[[exports]]
service = "{name}.service"
version = 1
description = "What {name} offers."
consumers = ["host"]

[exports.methods.share]
capability = "{name}.use"
description = "Shares."

[exports.methods.status]
description = "Says how it is."

[exports.events]

[[contributions.actions]]
id = "share"
label = "Share"
capability = "{name}.use"
needs_device = true
destructive = false
target = {{ service = "{name}.service", method = "share" }}

[[contributions.actions]]
id = "status"
label = "Status"
needs_device = false
destructive = false
target = {{ service = "{name}.service", method = "status" }}
"#
    );
    std::fs::write(dir.join("plugin.toml"), toml).unwrap();
    std::fs::copy(plugin_binary(), dir.join("plugin")).unwrap();
    std::fs::set_permissions(dir.join("plugin"), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_whose_manifest_is_toml_installs_and_runs_from_a_folder_and_from_an_archive() {
    use peersus_core::model::PluginStatus::Running;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let (client, _task) = start_with_packages(root.path(), "owner", data.path()).await;

    for (name, as_archive) in [("tomlfolder", false), ("tomlarchive", true)] {
        let folder = package_with_toml_manifest(parent.path(), name);
        let source = if as_archive {
            archive_of(&folder, parent.path())
        } else {
            folder
        };
        client
            .command(Command::StageInstall {
                source: source_dir(&source),
            })
            .await
            .unwrap();
        let shown = until(&client, |s| s.staged.iter().any(|p| p.id == id(name))).await;
        // What it asks for is shown from the TOML manifest, as it would be from a JSON one.
        let staged = shown.staged.iter().find(|p| p.id == id(name)).unwrap();
        assert_eq!(
            staged.manifest.exports.len(),
            1,
            "{name}: the TOML manifest was not read"
        );
        assert!(
            staged
                .manifest
                .capabilities
                .contains_key(&format!("{name}.use"))
        );
        client
            .command(Command::ConfirmInstall { plugin: id(name) })
            .await
            .unwrap();
        until(&client, |s| {
            s.plugins
                .iter()
                .any(|p| p.id == id(name) && p.status == Running)
        })
        .await;
        let installed = data.path().join("plugins").join(name);
        assert!(installed.join("plugin.toml").is_file() && !installed.join("plugin.json").exists());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_given_as_an_archive_is_shown_confirmed_and_run_like_a_folder_and_leaves_nothing()
{
    use peersus_core::model::PluginStatus::Running;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let folder = package_on_disk(parent.path(), "zipped");
    let archive = archive_of(&folder, parent.path());
    let (client, _task, private) = start_with_packages_in(root.path(), "owner", data.path()).await;
    let zipped = id("zipped");
    let before = unpacked_folders(&private);

    // Choosing the archive shows what the package asks for, as choosing the folder does, and
    // installs nothing.
    client
        .command(Command::StageInstall {
            source: source_dir(&archive),
        })
        .await
        .unwrap();
    let shown = until(&client, |s| s.staged.iter().any(|p| p.id == zipped)).await;
    assert!(
        shown.plugins.iter().all(|p| p.id != zipped),
        "it was installed by only being shown"
    );
    assert!(!data.path().join("plugins").join("zipped").exists());

    client
        .command(Command::ConfirmInstall {
            plugin: zipped.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == zipped && p.status == Running)
    })
    .await;
    // It is installed from what was unpacked, with the program runnable.
    assert!(
        data.path()
            .join("plugins")
            .join("zipped")
            .join("plugin")
            .is_file()
    );
    // And the unpacked copy is gone once the install is done. Counted only because the copy WAS
    // there while the package waited, which the other test shows.
    assert_eq!(
        unpacked_folders(&private),
        before,
        "an unpacked archive was left behind after installing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_archive_that_is_refused_installs_nothing_and_says_why_and_leaves_nothing() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let bad = parent.path().join("bad.tar.gz");
    std::fs::write(&bad, b"this is not an archive").unwrap();
    let (client, _task, private) = start_with_packages_in(root.path(), "owner", data.path()).await;
    let before = unpacked_folders(&private);

    assert!(
        client
            .command(Command::StageInstall {
                source: source_dir(&bad)
            })
            .await
            .is_err()
    );
    assert!(client.snapshot().staged.is_empty());
    assert_eq!(
        unpacked_folders(&private),
        before,
        "a refused archive left files"
    );

    // Giving up a shown archive also lets go of what was unpacked.
    let folder = package_on_disk(parent.path(), "given-up");
    let archive = archive_of(&folder, parent.path());
    let held = unpacked_folders(&private);
    client
        .command(Command::StageInstall {
            source: source_dir(&archive),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == id("given-up"))).await;
    assert!(
        unpacked_folders(&private) > held,
        "nothing was unpacked, so this proves nothing"
    );
    client
        .command(Command::CancelInstall {
            plugin: id("given-up"),
        })
        .await
        .unwrap();
    assert_eq!(
        unpacked_folders(&private),
        held,
        "giving up kept the unpacked files"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_archive_that_decodes_to_far_more_than_it_weighs_is_refused_by_the_owner_and_stages_nothing()
 {
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let (client, _task, private) = start_with_packages_in(root.path(), "owner", data.path()).await;
    let before = unpacked_folders(&private);

    // A GNU long-name record whose body is larger than the decoded stream the owner will read.
    let body = (peersus_app::archive::MAX_STREAM + 1) as usize;
    let mut header = tar::Header::new_gnu();
    let name = b"././@LongLink";
    header.as_old_mut().name[..name.len()].copy_from_slice(name);
    header.set_size(body as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::GNULongName);
    header.set_cksum();
    let mut raw = header.as_bytes().to_vec();
    raw.extend(std::iter::repeat_n(0u8, body));
    raw.resize(raw.len().div_ceil(512) * 512, 0);
    raw.extend_from_slice(&[0u8; 1024]);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&raw).unwrap();
    let bomb = encoder.finish().unwrap();
    assert!(
        (bomb.len() as u64) * 100 < peersus_app::archive::MAX_STREAM,
        "the bomb is not small, so this proves nothing"
    );
    let path = parent.path().join("bomb.tar.gz");
    std::fs::write(&path, &bomb).unwrap();

    assert!(
        client
            .command(Command::StageInstall {
                source: source_dir(&path)
            })
            .await
            .is_err()
    );
    assert!(client.snapshot().staged.is_empty());
    assert_eq!(
        unpacked_folders(&private),
        before,
        "a refused bomb left an unpacked folder"
    );
    assert!(
        !data.path().join("plugins").exists()
            || std::fs::read_dir(data.path().join("plugins"))
                .unwrap()
                .count()
                == 0
    );
}

/// The processes whose executable is exactly `path`, as `(pid, comm)`: how a process list names them.
fn processes_running(path: &std::path::Path) -> Vec<(u32, String)> {
    let wanted = std::fs::canonicalize(path).unwrap();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().filter_map(Result::ok) {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(exe) = std::fs::read_link(entry.path().join("exe")) else {
            continue;
        };
        if exe == wanted {
            let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
            found.push((pid, comm.trim().to_owned()));
        }
    }
    found
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_that_names_its_program_runs_as_a_process_the_system_lists_by_that_name() {
    use peersus_core::model::PluginStatus::Running;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let (client, _task) = start_with_packages(root.path(), "owner", data.path()).await;

    // The real test plugin, under a name of its own, with a manifest that says so.
    let dir = parent.path().join("named");
    std::fs::create_dir_all(&dir).unwrap();
    let mut manifest = spec("named").manifest;
    manifest.program = Some("peersus-named".into());
    std::fs::write(
        dir.join("plugin.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::copy(plugin_binary(), dir.join("peersus-named")).unwrap();
    std::fs::set_permissions(
        dir.join("peersus-named"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();

    client
        .command(Command::StageInstall {
            source: source_dir(&dir),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == id("named"))).await;
    client
        .command(Command::ConfirmInstall {
            plugin: id("named"),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == id("named") && p.status == Running)
    })
    .await;

    let installed = data.path().join("plugins").join("named");
    assert!(installed.join("peersus-named").is_file() && !installed.join("plugin").exists());
    let running = processes_running(&installed.join("peersus-named"));
    assert_eq!(
        running.len(),
        1,
        "the plugin is not running as exactly one process: {running:?}"
    );
    // The name a process list shows is the name of the file, so it is the plugin's own.
    assert_eq!(running[0].1, "peersus-named");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_switched_off_stays_off_and_listed_after_the_application_restarts() {
    use peersus_core::model::PluginStatus::Running;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let source = package_on_disk(parent.path(), "napper");
    let napper = id("napper");

    // First run: install it, see it run, switch it off, shut the application down.
    {
        let (client, task) = start_with_packages(root.path(), "first", data.path()).await;
        client
            .command(Command::StageInstall {
                source: source_dir(&source),
            })
            .await
            .unwrap();
        until(&client, |s| s.staged.iter().any(|p| p.id == napper)).await;
        client
            .command(Command::ConfirmInstall {
                plugin: napper.clone(),
            })
            .await
            .unwrap();
        until(&client, |s| {
            s.plugins
                .iter()
                .any(|p| p.id == napper && p.status == Running)
        })
        .await;
        client
            .command(Command::DisablePlugin {
                plugin: napper.clone(),
            })
            .await
            .unwrap();
        until(&client, |s| {
            s.plugins
                .iter()
                .any(|p| p.id == napper && p.status != Running)
        })
        .await;
        client.command(Command::Shutdown {}).await.unwrap();
        task.await.unwrap();
    }

    // Second run over the SAME directory, loaded the way the real application loads it.
    let found = peersus_app::plugin_manager::Manager::new(data.path()).load();
    assert!(
        found.disabled.contains(&napper),
        "the choice was lost over a restart"
    );
    let (client, _task) = start_loaded(root.path(), "second", data.path(), found).await;
    // It is listed, and it is NOT running: switched off survives the restart.
    let after = until(&client, |s| s.plugins.iter().any(|p| p.id == napper)).await;
    assert_ne!(
        after
            .plugins
            .iter()
            .find(|p| p.id == napper)
            .unwrap()
            .status,
        Running,
        "a plugin switched off came back to life at the restart"
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let later = client.snapshot();
    assert_ne!(
        later
            .plugins
            .iter()
            .find(|p| p.id == napper)
            .unwrap()
            .status,
        Running
    );

    // Switching it on starts it, and the choice is forgotten, so the next start runs it too.
    client
        .command(Command::EnablePlugin {
            plugin: napper.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == napper && p.status == Running)
    })
    .await;
    assert!(
        peersus_app::plugin_manager::Manager::new(data.path())
            .load()
            .disabled
            .is_empty()
    );
}

/// Like `start_with_packages`, but with what `Manager::load` found: what is installed and which of
/// it is switched off, as the real application starts.
async fn start_loaded(
    root: &std::path::Path,
    name: &str,
    data: &std::path::Path,
    found: peersus_app::discovery::Discovered,
) -> (Client, tokio::task::JoinHandle<()>) {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    let mut store = SettingsStore::open(path.join("preferences.json")).unwrap();
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
    let runtime = runtime
        .with_packages(peersus_app::plugin_manager::Manager::new(data))
        .with_disabled(found.disabled);
    let task = tokio::spawn(async move {
        runtime.run().await;
        drop(directory);
    });
    (client, task)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_switched_off_then_removed_and_installed_again_is_new_and_runs() {
    use peersus_core::model::PluginStatus::Running;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let source = package_on_disk(parent.path(), "reborn");
    let reborn = id("reborn");
    let (client, _task) = start_with_packages(root.path(), "owner", data.path()).await;

    client
        .command(Command::StageInstall {
            source: source_dir(&source),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == reborn)).await;
    client
        .command(Command::ConfirmInstall {
            plugin: reborn.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == reborn && p.status == Running)
    })
    .await;

    // Switched off, then removed for good.
    client
        .command(Command::DisablePlugin {
            plugin: reborn.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == reborn && p.status != Running)
    })
    .await;
    client
        .command(Command::RemovePackage {
            plugin: reborn.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| s.plugins.iter().all(|p| p.id != reborn)).await;

    // A package with the same name is a NEW plugin: it runs, and the record says it is not off.
    client
        .command(Command::StageInstall {
            source: source_dir(&source),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == reborn)).await;
    client
        .command(Command::ConfirmInstall {
            plugin: reborn.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == reborn && p.status == Running)
    })
    .await;
    assert!(
        peersus_app::plugin_manager::Manager::new(data.path())
            .load()
            .disabled
            .is_empty(),
        "the new plugin inherited being switched off"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn replacing_an_installed_plugin_runs_the_new_package_with_nothing_allowed() {
    use peersus_core::model::PluginStatus::Running;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let first = package_on_disk(parent.path(), "swapme");
    let swapme = id("swapme");
    let (client, _task) = start_with_packages(root.path(), "owner", data.path()).await;

    client
        .command(Command::StageInstall {
            source: source_dir(&first),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == swapme)).await;
    client
        .command(Command::ConfirmInstall {
            plugin: swapme.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == swapme && p.status == Running)
    })
    .await;
    // Something is allowed to the first one, which a replacement must not carry over.
    client
        .command(Command::SetGrants {
            plugin: swapme.clone(),
            peer: None,
            grants: Grants::new([permission("swapme", "swapme.use")]).unwrap(),
        })
        .await
        .ok();
    let before = std::fs::read(data.path().join("plugins").join("swapme").join("plugin")).unwrap();

    // A different package under the same name: the program differs by a trailing comment.
    let second_parent = tempfile::tempdir().unwrap();
    let second = package_on_disk(second_parent.path(), "swapme");
    let mut program = std::fs::read(second.join("plugin")).unwrap();
    program.extend_from_slice(b"\n# a different build\n");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(second.join("plugin"), &program).unwrap();
        std::fs::set_permissions(
            second.join("plugin"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    client
        .command(Command::StageInstall {
            source: source_dir(&second),
        })
        .await
        .unwrap();
    let shown = until(&client, |s| s.staged.iter().any(|p| p.id == swapme)).await;
    assert!(
        shown.plugins.iter().any(|p| p.id == swapme),
        "the first one went before it was confirmed"
    );

    client
        .command(Command::ConfirmInstall {
            plugin: swapme.clone(),
        })
        .await
        .unwrap();
    let after = until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == swapme && p.status == Running)
    })
    .await;
    let now = std::fs::read(data.path().join("plugins").join("swapme").join("plugin")).unwrap();
    assert_ne!(now, before, "the old program is still what is installed");
    assert_eq!(now, program, "what runs is not what was shown");
    assert!(
        after.grants.get(&swapme).is_none_or(|g| g.is_empty()),
        "the replacement inherited rights"
    );
    assert!(after.staged.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replacement_that_cannot_be_installed_leaves_the_old_plugin_as_it_was() {
    use peersus_core::model::PluginStatus::Running;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let first = package_on_disk(parent.path(), "keepme");
    let keepme = id("keepme");
    let (client, _task) = start_with_packages(root.path(), "owner", data.path()).await;
    client
        .command(Command::StageInstall {
            source: source_dir(&first),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == keepme)).await;
    client
        .command(Command::ConfirmInstall {
            plugin: keepme.clone(),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == keepme && p.status == Running)
    })
    .await;
    let before = std::fs::read(data.path().join("plugins").join("keepme").join("plugin")).unwrap();

    // A second, unrelated plugin is installed, offering `clash.service`.
    let other_parent = tempfile::tempdir().unwrap();
    let other = package_on_disk(other_parent.path(), "clash");
    client
        .command(Command::StageInstall {
            source: source_dir(&other),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == id("clash"))).await;
    client
        .command(Command::ConfirmInstall {
            plugin: id("clash"),
        })
        .await
        .unwrap();
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == id("clash") && p.status == Running)
    })
    .await;

    // A replacement for `keepme` that is perfectly readable, and is shown, but that offers the very
    // service `clash` offers. It is accepted at the look and refused only when it is put in, which
    // is AFTER the old `keepme` was taken out: the one case where undoing is what is being tried.
    let second_parent = tempfile::tempdir().unwrap();
    let second = package_on_disk(second_parent.path(), "keepme");
    let manifest = serde_json::to_string(&spec("clash").manifest).unwrap();
    std::fs::write(second.join("plugin.json"), manifest).unwrap();
    client
        .command(Command::StageInstall {
            source: source_dir(&second),
        })
        .await
        .unwrap();
    until(&client, |s| s.staged.iter().any(|p| p.id == keepme)).await;
    assert!(
        client
            .command(Command::ConfirmInstall {
                plugin: keepme.clone()
            })
            .await
            .is_err(),
        "a replacement that clashes with another plugin was installed"
    );

    // The old plugin is back: its files as they were, in the list, and running again.
    assert_eq!(
        std::fs::read(data.path().join("plugins").join("keepme").join("plugin")).unwrap(),
        before,
        "a failed replacement changed what is installed"
    );
    until(&client, |s| {
        s.plugins
            .iter()
            .any(|p| p.id == keepme && p.status == Running)
    })
    .await;
}
