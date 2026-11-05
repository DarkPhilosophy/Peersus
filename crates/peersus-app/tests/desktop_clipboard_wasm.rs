#![cfg(target_os = "linux")]
use iroh::SecretKey;
use peersus_api::permission::Permission;
use peersus_app::{
    desktop_clipboard::DesktopClipboard,
    discovery::discover,
    runtime::{Client, Reply, Runtime},
};
use peersus_clipboard_platform::protocol::{Answer, Request};
use peersus_core::{
    Settings,
    config::NetworkSettings,
    model::{Command, Snapshot},
    plugins::{Grants, PluginId},
};
use peersus_net::{
    grants::StandaloneGrants, history::History, network_host::NetworkHost, peers::Peers,
    settings::SettingsStore, transport,
};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command as Process},
};

// All clipboard access in this test is directed at X servers it starts itself,
// never the user's display. The helper wrappers clear WAYLAND_DISPLAY as well.
struct Display {
    _server: Child,
    helper: PathBuf,
    probe: Child,
}
impl Display {
    async fn start(root: &Path, name: &str) -> Self {
        let mut server = Process::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "800x600x24",
                "-nolisten",
                "tcp",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("Xvfb is required for the isolated desktop test");
        let mut line = String::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            BufReader::new(server.stdout.take().unwrap()).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let number: u32 = line
            .trim()
            .parse()
            .expect("a freshly created virtual display");
        let helper = root.join(format!("helper-{name}"));
        let executable = env!("CARGO_BIN_EXE_peersus");
        std::fs::write(&helper, format!("#!/bin/sh\nunset WAYLAND_DISPLAY\nexport DISPLAY=:{number}\nexec '{executable}' \"$@\"\n")).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let probe = Process::new(&helper)
            .arg("--clipboard-platform-helper")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            _server: server,
            helper,
            probe,
        }
    }
    async fn request(&mut self, request: Request) -> Answer {
        let bytes = serde_json::to_vec(&request).unwrap();
        let input = self.probe.stdin.as_mut().unwrap();
        input.write_u32(bytes.len() as u32).await.unwrap();
        input.write_all(&bytes).await.unwrap();
        input.flush().await.unwrap();
        let output = self.probe.stdout.as_mut().unwrap();
        let count = output.read_u32().await.unwrap() as usize;
        assert!(count <= peersus_clipboard_platform::protocol::MAX_FRAME);
        let mut bytes = vec![0; count];
        output.read_exact(&mut bytes).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    async fn copy(&mut self, text: &str) {
        assert!(self.request(Request::Write { text: text.into() }).await.ok);
    }
    async fn reaches(&mut self, text: &str) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if self
                    .request(Request::Read { observe: false })
                    .await
                    .text
                    .as_deref()
                    == Some(text)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("a real remote clipboard write must reach the other isolated display");
    }
    async fn stop(&mut self) {
        self.probe.kill().await.unwrap();
        self._server.kill().await.unwrap();
    }
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
    .expect("expected runtime state")
}

/// This process's voluntary and involuntary context switches summed over all its threads: how often it was woken. Counters only.
fn wakeups() -> (u64, u64) {
    let mut totals = (0, 0);
    for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let Ok(status) = std::fs::read_to_string(task.path().join("status")) else {
            continue;
        };
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("voluntary_ctxt_switches:") {
                totals.0 += value.trim().parse::<u64>().unwrap_or(0);
            } else if let Some(value) = line.strip_prefix("nonvoluntary_ctxt_switches:") {
                totals.1 += value.trim().parse::<u64>().unwrap_or(0);
            }
        }
    }
    totals
}

async fn start(
    root: &Path,
    helper: PathBuf,
    guest: &Path,
) -> (Client, tokio::task::JoinHandle<()>) {
    std::fs::create_dir_all(root).unwrap();
    let plugins = root.join("plugins");
    let package = plugins.join("clipboard");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("plugin.json"),
        include_bytes!("../../peersus-clipboard/wasm/plugin.json"),
    )
    .unwrap();
    std::fs::copy(guest, package.join("peersus_clipboard_wasm.wasm")).unwrap();
    let found = discover(&plugins);
    assert!(
        found.problems.is_empty(),
        "the real WASM package must be discoverable"
    );
    assert_eq!(found.registry.len(), 1);
    let mut settings = SettingsStore::open(root.join("preferences.json")).unwrap();
    let preferences = Settings {
        network: NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..Default::default()
        },
        ..Default::default()
    };
    settings.replace(preferences.clone()).unwrap();
    let endpoint = transport::bind(SecretKey::generate(), &preferences.network)
        .await
        .unwrap();
    let network = NetworkHost::new(
        endpoint,
        preferences.device_name,
        Peers::open(root.join("peers.json")).unwrap(),
        History::open(root.join("history.json")).unwrap(),
    );
    let sockets = tempfile::Builder::new()
        .prefix("pw")
        .tempdir_in("/tmp")
        .unwrap();
    let grants = StandaloneGrants::open(root.join("grants.json")).unwrap();
    let (owner, client) = Runtime::new(
        network,
        grants,
        settings,
        found.registry,
        sockets.path().into(),
    )
    .unwrap();
    let owner = owner
        .with_platform_services(DesktopClipboard::new(helper))
        .with_modules(Arc::new(
            peersus_plugin::module::DesktopModules::beside(Path::new(env!(
                "CARGO_BIN_EXE_peersus"
            )))
            .unwrap(),
        ));
    let task = tokio::spawn(async move {
        owner.run().await;
        drop(sockets);
    });
    (client, task)
}

#[tokio::test(flavor = "multi_thread")]
async fn wasm_clipboard_synchronizes_both_directions_between_real_isolated_desktops() {
    // Expensive OS integration is opt-in, but once requested missing artifacts fail
    // loudly rather than pretending to have exercised a guest.
    if std::env::var("PEERSUS_DESKTOP_WASM_TEST").as_deref() != Ok("1") {
        return;
    }
    let guest = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../target/clip-wasm-rel/wasm32-unknown-unknown/release/peersus_clipboard_wasm.wasm",
    );
    assert!(
        guest.is_file(),
        "build the real clipboard WASM guest before running this test"
    );
    let root = tempfile::tempdir().unwrap();
    let mut left = Display::start(root.path(), "a").await;
    let mut right = Display::start(root.path(), "b").await;
    left.copy("left initial baseline").await;
    right.copy("right initial baseline").await;
    let (a, ta) = start(&root.path().join("a"), left.helper.clone(), &guest).await;
    let (b, tb) = start(&root.path().join("b"), right.helper.clone(), &guest).await;
    let Reply::Invitation(invitation) = b.command(Command::CreateInvitation {}).await.unwrap()
    else {
        panic!("invitation");
    };
    a.command(Command::JoinInvitation { invitation })
        .await
        .unwrap();
    let pa = until(&a, |s| !s.pairing.is_empty()).await;
    let pb = until(&b, |s| !s.pairing.is_empty()).await;
    let number = pa.pairing[0].options[0];
    for (client, prompt) in [(&a, &pa), (&b, &pb)] {
        client
            .command(Command::ChoosePairing {
                request: prompt.pairing[0].id.clone(),
                number,
            })
            .await
            .unwrap();
    }
    let sa = until(&a, |s| s.peers.iter().any(|p| p.online)).await;
    let sb = until(&b, |s| s.peers.iter().any(|p| p.online)).await;
    let plugin = PluginId::new("clipboard").unwrap();
    // A control run (PEERSUS_IDLE_CONTROL=1) keeps both owners, the pairing and the plugin, and only turns the clipboard features off, so
    // that the idle wake-ups with and without them can be compared and the difference is the cost of the feature.
    let control = std::env::var("PEERSUS_IDLE_CONTROL").as_deref() == Ok("1");
    for (client, peer) in [(&a, sa.peers[0].id.clone()), (&b, sb.peers[0].id.clone())] {
        client
            .command(Command::SetGrants {
                plugin: plugin.clone(),
                peer: Some(peer.clone()),
                grants: Grants::new([
                    Permission::new("clipboard", "clipboard.use").unwrap(),
                    Permission::new("host", "peers.send").unwrap(),
                ])
                .unwrap(),
            })
            .await
            .unwrap();
        client
            .command(Command::SetGrants {
                plugin: plugin.clone(),
                peer: None,
                grants: Grants::new([
                    Permission::new("host", "clipboard.read").unwrap(),
                    Permission::new("host", "clipboard.write").unwrap(),
                    Permission::new("host", "clipboard.watch").unwrap(),
                ])
                .unwrap(),
            })
            .await
            .unwrap();
        client
            .command(Command::SetPluginSetting {
                plugin: plugin.clone(),
                peer: Some(peer),
                setting: "sync".into(),
                value: peersus_core::plugins::Value::Toggle(!control),
            })
            .await
            .unwrap();
        client
            .command(Command::SetPluginSetting {
                plugin: plugin.clone(),
                peer: None,
                setting: "automatic".into(),
                value: peersus_core::plugins::Value::Toggle(!control),
            })
            .await
            .unwrap();
    }
    // Give the guests a baseline. Several distinct copies in both directions then
    // prove that initial contents and self-write echoes do not replace new copies.
    tokio::time::sleep(Duration::from_secs(2)).await;
    // Optional: nothing is copied for this long and the number of times the process woke is reported. Scheduling counters only, read from
    // this test's own process; no clipboard contents. A periodic tick shows up here as wake-ups that no copy asked for.
    if let Some(seconds) = std::env::var("PEERSUS_IDLE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        let before = wakeups();
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        let after = wakeups();
        eprintln!(
            "IDLE_WAKEUPS seconds={seconds} voluntary={} involuntary={}",
            after.0 - before.0,
            after.1 - before.1
        );
    }
    for i in 0..3 {
        let text = format!("left to right {i}: é🙂");
        left.copy(&text).await;
        right.reaches(&text).await;
        let text = format!("right to left {i}: λ🌍");
        right.copy(&text).await;
        left.reaches(&text).await;
    }
    for (client, task) in [(a, ta), (b, tb)] {
        client.command(Command::Shutdown {}).await.unwrap();
        task.await.unwrap();
    }
    left.stop().await;
    right.stop().await;
}
