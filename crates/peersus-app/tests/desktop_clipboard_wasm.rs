#![cfg(target_os = "linux")]
//! Two real owners, each the `peersus` program on a virtual X display of its own, pair and keep
//! their clipboards in step through the real clipboard WASM extension. The test drives them as a
//! person's scripts do (the program's command line and its control socket) and copies and reads
//! with `xclip` on each display. Nothing touches the display or data of the person running it.
use peersus_api::permission::Permission;
use peersus_app::{ipc, runtime::Reply};
use peersus_core::{
    model::{Command, Snapshot},
    plugins::{Grants, PluginId},
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command as Process},
};

const PROGRAM: &str = env!("CARGO_BIN_EXE_peersus");

/// A virtual display, and an owner whose data and display are its own.
struct Side {
    _server: Child,
    display: String,
    data: PathBuf,
    owner: Option<Child>,
}

impl Side {
    async fn start(root: &Path, name: &str, guest: &Path) -> Self {
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
        let display = format!(":{}", line.trim());
        let data = root.join(name);
        std::fs::create_dir_all(&data).unwrap();
        let mut side = Self {
            _server: server,
            display,
            data,
            owner: None,
        };
        // The real extension package, installed the way a person installs one from a folder: its licence, notice and the
        // warning for a package from outside the store accepted by the digest the program would print for them.
        let package = root.join(format!("package-{name}")).join("clipboard");
        std::fs::create_dir_all(&package).unwrap();
        let manifest = std::fs::read(
            std::env::var_os("PEERSUS_CLIPBOARD_MANIFEST")
                .expect("explicit clipboard manifest required"),
        )
        .expect("clipboard manifest must be readable");
        std::fs::write(package.join("plugin.json"), &manifest).unwrap();
        let split = peersus_api::package::split(&manifest).expect("a manifest");
        let read: peersus_api::manifest::Manifest =
            serde_json::from_slice(&split.manifest).expect("a manifest");
        let digest = peersus_core::agreements::digest(
            read.agreements
                .as_ref()
                .expect("the manifest carries its licence and notice"),
        );
        std::fs::copy(guest, package.join("peersus_clipboard_wasm.wasm")).unwrap();
        let installed = side
            .program()
            .arg("plugin-install")
            .arg(&package)
            .args(["--accept", &digest, "--accept-outside-store-risk"])
            .output()
            .await
            .unwrap();
        assert!(
            installed.status.success(),
            "the real WASM package must install: {}",
            String::from_utf8_lossy(&installed.stderr)
        );
        // The owner's log goes to its own folder, and a failed step prints its end: an end-to-end test that only says
        // "it did not arrive" cannot be acted on.
        let log = std::fs::File::create(side.data.join("owner.log")).unwrap();
        side.owner = Some(
            side.program()
                .env("PEERSUS_LOG", "info")
                .args([
                    "--background",
                    "--bind=127.0.0.1",
                    "--allow-relay=false",
                    &format!("--device-name={name}"),
                    "owner",
                ])
                .stdout(std::process::Stdio::null())
                .stderr(log)
                .kill_on_drop(true)
                .spawn()
                .unwrap(),
        );
        tokio::time::timeout(Duration::from_secs(20), async {
            while ipc::snapshot(&side.socket()).await.is_err() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("the owner must start");
        side
    }

    /// The program with this side's private data and display, and no way to the person's own.
    fn program(&self) -> Process {
        let mut program = Process::new(PROGRAM);
        program
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.data)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.data)
            .env("DISPLAY", &self.display);
        program
    }

    fn socket(&self) -> PathBuf {
        self.data.join("peersus").join("control.sock")
    }

    async fn command(&self, command: Command) -> (Reply, Snapshot) {
        ipc::command(&self.socket(), command).await.unwrap()
    }

    async fn until(&self, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Ok(snapshot) = ipc::snapshot(&self.socket()).await
                    && predicate(&snapshot)
                {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("expected owner state")
    }

    fn xclip(&self) -> Process {
        let mut xclip = Process::new("xclip");
        xclip
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("DISPLAY", &self.display)
            .args(["-selection", "clipboard"]);
        xclip
    }

    async fn copy(&self, text: &str) {
        let mut xclip = self
            .xclip()
            .arg("-i")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("xclip is required for the isolated desktop test");
        let mut input = xclip.stdin.take().unwrap();
        input.write_all(text.as_bytes()).await.unwrap();
        drop(input);
        // xclip stays to serve the selection; it is ended when the display is.
        tokio::spawn(async move {
            let _ = xclip.wait().await;
        });
    }

    /// Whether `text` was on this side's clipboard within 15 s.
    async fn reaches(&self, text: &str) -> bool {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let read = self.xclip().arg("-o").output().await.unwrap();
                if read.status.success() && read.stdout == text.as_bytes() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .is_ok()
    }

    /// What this side's owner says of its plugins: running or not, what each may do and its settings.
    async fn plugins_report(&self) -> String {
        let out = self.program().arg("plugins").output().await.unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    /// What this side's clipboard holds now, as text.
    async fn holds(&self) -> String {
        let read = self.xclip().arg("-o").output().await.unwrap();
        String::from_utf8_lossy(&read.stdout).into_owned()
    }

    /// The last lines of this side's owner log.
    fn log_tail(&self) -> String {
        let log = std::fs::read_to_string(self.data.join("owner.log")).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        lines[lines.len().saturating_sub(40)..].join("\n")
    }

    async fn stop(&mut self) {
        let _ = self.command(Command::Shutdown {}).await;
        if let Some(mut owner) = self.owner.take() {
            tokio::time::timeout(Duration::from_secs(10), owner.wait())
                .await
                .expect("the owner must stop")
                .unwrap();
        }
    }
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
    // The application runs a WASM guest in its own host program, which it looks for beside itself.
    let host = Path::new(PROGRAM).with_file_name("peersus-wasm-host");
    assert!(
        host.is_file(),
        "build the WASM host beside the program first: cargo build -p peersus-plugin-runtime --bin peersus-wasm-host \
         (same target directory)"
    );
    let root = tempfile::tempdir().unwrap();
    let mut left = Side::start(root.path(), "a", &guest).await;
    let mut right = Side::start(root.path(), "b", &guest).await;
    left.copy("left initial baseline").await;
    right.copy("right initial baseline").await;
    let (Reply::Invitation(invitation), _) = right.command(Command::CreateInvitation {}).await
    else {
        panic!("invitation");
    };
    left.command(Command::JoinInvitation { invitation }).await;
    let pa = left.until(|s| !s.pairing.is_empty()).await;
    let pb = right.until(|s| !s.pairing.is_empty()).await;
    let number = pa.pairing[0].options[0];
    for (side, prompt) in [(&left, &pa), (&right, &pb)] {
        side.command(Command::ChoosePairing {
            request: prompt.pairing[0].id.clone(),
            number,
        })
        .await;
    }
    let plugin = PluginId::new("clipboard").unwrap();
    for side in [&left, &right] {
        let peer = side.until(|s| s.peers.iter().any(|p| p.online)).await.peers[0]
            .id
            .clone();
        side.command(Command::SetGrants {
            plugin: plugin.clone(),
            peer: Some(peer.clone()),
            grants: Grants::new([
                Permission::new("clipboard", "clipboard.use").unwrap(),
                Permission::new("host", "peers.send").unwrap(),
                // Seeing the device at all: without it the extension is told of no device, and has none to send to.
                Permission::new("host", "peers.watch").unwrap(),
            ])
            .unwrap(),
        })
        .await;
        side.command(Command::SetGrants {
            plugin: plugin.clone(),
            peer: None,
            grants: Grants::new([
                Permission::new("host", "clipboard.read").unwrap(),
                Permission::new("host", "clipboard.write").unwrap(),
                Permission::new("host", "clipboard.watch").unwrap(),
            ])
            .unwrap(),
        })
        .await;
        side.command(Command::SetPluginSetting {
            plugin: plugin.clone(),
            peer: Some(peer),
            setting: "sync".into(),
            value: peersus_core::plugins::Value::Toggle(true),
        })
        .await;
        side.command(Command::SetPluginSetting {
            plugin: plugin.clone(),
            peer: None,
            setting: "automatic".into(),
            value: peersus_core::plugins::Value::Toggle(true),
        })
        .await;
    }
    // Give the guests a baseline. Several distinct copies in both directions then
    // prove that initial contents and self-write echoes do not replace new copies.
    tokio::time::sleep(Duration::from_secs(2)).await;
    for i in 0..3 {
        for (from, to, text) in [
            (&left, &right, format!("left to right {i}: é🙂")),
            (&right, &left, format!("right to left {i}: λ🌍")),
        ] {
            from.copy(&text).await;
            if !to.reaches(&text).await {
                panic!(
                    "a real remote clipboard write must reach the other isolated display: {text:?}; \
                     the sender holds {:?}, the receiver {:?}\n\
                     --- sender's owner ---\n{}\n{}\n--- receiver's owner ---\n{}\n{}",
                    from.holds().await,
                    to.holds().await,
                    from.plugins_report().await,
                    from.log_tail(),
                    to.plugins_report().await,
                    to.log_tail()
                );
            }
        }
    }
    left.stop().await;
    right.stop().await;
}
