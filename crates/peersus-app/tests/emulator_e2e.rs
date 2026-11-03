//! Opt-in interactive end-to-end check against an explicitly selected disposable Android emulator.
//! Before running, open Peers > Pair and keep its fresh QR code fully visible.
//! The test decodes that real screen without logging or saving its invitation. Select the reported
//! matching number, then accept the three offers into Download/e2e-received using the system picker.
use peersus_app::{plugin_host::Registry, runtime::{Client, Runtime}};
use peersus_core::{Settings, model::{Command, Snapshot, TransferState}};
use peersus_net::{history::History, network_host::NetworkHost, peers::Peers, settings::SettingsStore, transport};
use std::{path::Path, process::{Command as Process, Output}, sync::Arc, time::Duration};

fn adb(executable: &str, device: &str, arguments: &[&str]) -> Output {
    let output = Process::new(executable).args(["-s", device]).args(arguments).output().expect("adb could not run");
    assert!(output.status.success(), "adb test operation failed: {}", String::from_utf8_lossy(&output.stderr));
    output
}

fn qr_on_screen(executable: &str, device: &str) -> String {
    try_qr_on_screen(executable, device).expect("keep a fresh QR fully visible on the dedicated emulator")
}

fn try_qr_on_screen(executable: &str, device: &str) -> Option<String> {
    let screen = adb(executable, device, &["exec-out", "screencap", "-p"]).stdout;
    let picture = image::load_from_memory_with_format(&screen, image::ImageFormat::Png)
        .expect("emulator screen is a PNG").to_luma8();
    let (width, height) = (picture.width() as usize, picture.height() as usize);
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| picture[(x as u32, y as u32)][0]);
    prepared.detect_grids().iter().filter_map(|grid| grid.decode().ok())
        .map(|(_, text)| text)
        .find(|text| text.starts_with("PEERSUS:Q2:") || text.to_ascii_lowercase().starts_with("peersus:1:"))
}

fn invitation_on_screen(executable: &str, device: &str) -> String {
    let text = qr_on_screen(executable, device);
    peersus_net::invitation::Invitation::expand_qr(&text).unwrap_or_else(|_| text.to_ascii_lowercase())
}

async fn until(client: &Client, predicate: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    let mut updates = client.subscribe();
    let mut last_errors = Vec::new();
    tokio::time::timeout(Duration::from_secs(240), async {
        loop {
            let snapshot = Arc::clone(&updates.borrow_and_update());
            if snapshot.errors != last_errors {
                eprintln!("E2E_RECEIVER_ERRORS: {:?}", snapshot.errors);
                last_errors = snapshot.errors.clone();
            }
            if predicate(&snapshot) { return snapshot; }
            updates.changed().await.expect("test runtime stopped");
        }
    }).await.unwrap_or_else(|_| {
        let snapshot = client.snapshot();
        panic!("Android UI step or transfer did not finish: {} online, {} incoming, errors: {:?}",
            snapshot.peers.iter().filter(|peer| peer.online).count(),
            snapshot.transfers.iter().filter(|transfer| transfer.incoming).count(),
            snapshot.errors);
    })
}

#[tokio::test]
#[ignore = "requires an explicitly selected disposable emulator and real UI consent"]
async fn paired_desktop_to_android_preserves_nested_files_in_the_system_selected_tree() {
    let device = std::env::var("PEERSUS_E2E_EMULATOR").expect("set PEERSUS_E2E_EMULATOR to a dedicated test emulator");
    assert!(device.starts_with("emulator-"), "physical devices are not accepted by this test");
    let executable = std::env::var("PEERSUS_ADB").unwrap_or_else(|_| "adb".into());
    let boot = adb(&executable, &device, &["shell", "getprop", "sys.boot_completed"]);
    assert_eq!(String::from_utf8_lossy(&boot.stdout).trim(), "1");
    let directory = tempfile::tempdir().unwrap();
    let sockets = tempfile::Builder::new().prefix("pe2e").tempdir_in("/tmp").unwrap();
    let mut settings = SettingsStore::open(directory.path().join("preferences.json")).unwrap();
    settings.replace(Settings { device_name: "E2E-Desktop".into(), ..Settings::default() }).unwrap();
    let endpoint = transport::bind(iroh::SecretKey::generate(), &settings.current().network).await.unwrap();
    let network = NetworkHost::new(endpoint, "E2E-Desktop".into(),
        Peers::open(directory.path().join("peers.json")).unwrap(),
        History::open(directory.path().join("history.json")).unwrap());
    let grants = peersus_net::grants::StandaloneGrants::open(directory.path().join("grants.json")).unwrap();
    let (runtime, client) = Runtime::new(network, grants, settings, Registry::default(), sockets.path().to_owned()).unwrap();
    let owner = tokio::spawn(runtime.run());
    // Decode the actual displayed QR instead of simulating thousands of key events.
    // The capability stays in memory: neither the screenshot nor its decoded text is logged or saved.
    let invitation = invitation_on_screen(&executable, &device);
    client.command(Command::JoinInvitation { invitation }).await.unwrap();
    println!("E2E_INVITATION_SCANNED: matching-number confirmation follows in the Android UI");
    let prompted = until(&client, |snapshot| !snapshot.pairing.is_empty()).await;
    let prompt = &prompted.pairing[0];
    let number = prompt.options[0];
    println!("E2E_PAIRING_NUMBER={number}: choose the same number in the emulator UI");
    client.command(Command::ChoosePairing { request: prompt.id.clone(), number }).await.unwrap();
    let paired = until(&client, |snapshot| snapshot.peers.iter().any(|peer| peer.online)).await;
    let peer = paired.peers.iter().find(|peer| peer.online).unwrap().id.clone();
    let root_name = format!("folder-e2e-{:08x}", rand::random::<u32>());
    let root = directory.path().join(&root_name);
    let files: [(&str, &[u8]); 3] = [("root.txt", b"root payload"), ("nested/deeper/child.txt", b"nested payload"), ("nested/empty.txt", b"")];
    for (name, bytes) in files {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    adb(&executable, &device, &["shell", "mkdir", "-p", "/sdcard/Download/e2e-received"]);
    client.command(Command::SendFolder { peer, root }).await.unwrap();
    println!("E2E_FOLDER_OFFERED: accept the three files into Download/e2e-received through the Android picker");
    let finished = until(&client, |snapshot| snapshot.transfers.len() == files.len()
        && (snapshot.transfers.iter().all(|transfer| transfer.state == TransferState::Complete)
            || snapshot.transfers.iter().any(|transfer| matches!(transfer.state, TransferState::Failed { .. } | TransferState::Cancelled)))).await;
    assert!(finished.transfers.iter().all(|transfer| transfer.state == TransferState::Complete),
        "a real Android file transfer failed or was cancelled before SAF delivery");
    // Network completion precedes the asynchronous SAF handoff. Wait only for this exact fixture's bytes.
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let delivered = files.iter().all(|(name, bytes)| {
                let path = Path::new("/sdcard/Download/e2e-received").join(&root_name).join(name);
                let output = Process::new(&executable).args(["-s", &device, "exec-out", "cat"])
                    .arg(path).output().unwrap();
                output.status.success() && output.stdout == *bytes
            });
            if delivered { break; }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }).await.expect("network finished but Android SAF did not preserve fixture bytes and nested paths");
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    println!("E2E_VERIFIED: pairing, encrypted folder transfer, nested SAF paths and empty file");
}

#[tokio::test]
#[ignore = "requires explicit dedicated-emulator UI pairing and selection of the generated 80 test files"]
async fn android_multi_selection_streams_more_than_the_descriptor_window_without_losing_files() {
    let device = std::env::var("PEERSUS_E2E_EMULATOR").expect("select a dedicated test emulator");
    assert!(device.starts_with("emulator-"), "physical devices are not accepted");
    let executable = std::env::var("PEERSUS_ADB").unwrap_or_else(|_| "adb".into());
    let directory = tempfile::tempdir().unwrap();
    let sockets = tempfile::Builder::new().prefix("pe2e").tempdir_in("/tmp").unwrap();
    let nonce = rand::random::<u32>();
    let receiver_name = format!("E2E-80-{nonce:08x}");
    let mut settings = SettingsStore::open(directory.path().join("preferences.json")).unwrap();
    settings.replace(Settings { device_name: receiver_name.clone(), ..Settings::default() }).unwrap();
    let endpoint = transport::bind(iroh::SecretKey::generate(), &settings.current().network).await.unwrap();
    let network = NetworkHost::new(endpoint, receiver_name.clone(),
        Peers::open(directory.path().join("peers.json")).unwrap(),
        History::open(directory.path().join("history.json")).unwrap());
    let grants = peersus_net::grants::StandaloneGrants::open(directory.path().join("grants.json")).unwrap();
    let (runtime, client) = Runtime::new(network, grants, settings, Registry::default(), sockets.path().to_owned()).unwrap();
    let fixture_name = format!("picker-e2e-{nonce:08x}");
    let source = directory.path().join(&fixture_name);
    std::fs::create_dir(&source).unwrap();
    for index in 0..80 {
        std::fs::write(source.join(format!("selected-{index:03}.txt")), format!("verified selection payload {index}\n")).unwrap();
    }
    let source_text = source.to_str().unwrap();
    adb(&executable, &device, &["push", source_text, "/sdcard/Download/e2e-received/"]);
    let received = directory.path().join("received");
    std::fs::create_dir(&received).unwrap();
    let files_code = std::env::var("PEERSUS_E2E_FILES_CODE").as_deref() == Ok("1");
    let previous = files_code.then(|| android_exchange(&executable, &device, serde_json::json!({"operation": "Snapshot"})));
    let owner = tokio::spawn(runtime.run());
    if let Some(previous) = previous.as_ref() {
        let generation = previous.files_code.map(|code| code.generation);
        println!("E2E_SELECT_80: choose Download/e2e-received/{fixture_name} with no destination selected");
        let invitation = tokio::time::timeout(Duration::from_secs(240), async {
            loop {
                let current = android_exchange(&executable, &device, serde_json::json!({"operation": "Snapshot"}));
                if current.files_code.is_some_and(|code| code.live && Some(code.generation) != generation)
                    && let Some(text) = try_qr_on_screen(&executable, &device) {
                    break peersus_net::invitation::Invitation::expand_qr(&text)
                        .unwrap_or_else(|_| text.to_ascii_lowercase());
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }).await.expect("the actual picker did not produce a fresh, readable files QR");
        client.command(Command::JoinInvitation { invitation }).await.unwrap();
        println!("E2E_FILES_CODE_SCANNED: one actual UI code, without pairing");
    } else {
        let invitation = invitation_on_screen(&executable, &device);
        client.command(Command::JoinInvitation { invitation }).await.unwrap();
        let prompted = until(&client, |snapshot| !snapshot.pairing.is_empty()).await;
        let prompt = &prompted.pairing[0];
        let number = prompt.options[0];
        println!("E2E_PAIRING_NUMBER={number}: confirm in the emulator");
        client.command(Command::ChoosePairing { request: prompt.id.clone(), number }).await.unwrap();
        until(&client, |snapshot| snapshot.peers.iter().any(|peer| peer.online)).await;
        println!("E2E_SELECT_80: send every file in Download/e2e-received/{fixture_name} to {receiver_name}");
    }
    let background = std::env::var("PEERSUS_E2E_BACKGROUND").as_deref() == Ok("1");
    if background {
        until(&client, |snapshot| snapshot.transfers.iter().filter(|transfer| transfer.incoming).count() >= 16).await;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let state = adb(&executable, &device, &["shell", "dumpsys", "activity", "services", "ro.peersus"]);
                if String::from_utf8_lossy(&state.stdout).contains("isForeground=true") { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await.expect("user-started transfer did not start foreground protection");
        adb(&executable, &device, &["shell", "input", "keyevent", "KEYCODE_HOME"]);
        // Deliberately spend time backgrounded before receiver consent; the offer deadline
        // must remain intact and the UI cannot be what keeps the owner or its queue alive.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let state = adb(&executable, &device, &["shell", "dumpsys", "activity", "services", "ro.peersus"]);
        assert!(String::from_utf8_lossy(&state.stdout).contains("isForeground=true"),
            "transfer protection stopped when the application went into the background");
        println!("E2E_BACKGROUND: foreground protection remains active after Home");
    }
    let mut updates = client.subscribe();
    let mut accepted = std::collections::BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(240), async {
        loop {
            let snapshot = Arc::clone(&updates.borrow_and_update());
            for transfer in &snapshot.transfers {
                assert!(!matches!(transfer.state, TransferState::Failed { .. } | TransferState::Cancelled),
                    "a real Android multi-selection transfer failed or was cancelled");
                if transfer.incoming && transfer.state == TransferState::Offered && accepted.insert(transfer.id.clone()) {
                    client.command(Command::AcceptTransfer { transfer: transfer.id.clone(), directory: received.clone() }).await.unwrap();
                }
            }
            if snapshot.transfers.len() == 80 && snapshot.transfers.iter().all(|transfer| transfer.state == TransferState::Complete) {
                break;
            }
            updates.changed().await.expect("test owner stopped");
        }
    }).await.expect("80 selected files did not arrive through the real Android picker");
    assert_eq!(accepted.len(), 80);
    assert_eq!(std::fs::read_dir(&received).unwrap().count(), 80);
    for index in 0..80 {
        let name = format!("selected-{index:03}.txt");
        assert_eq!(std::fs::read(received.join(&name)).unwrap(), std::fs::read(source.join(name)).unwrap());
    }
    if let Some(previous) = previous {
        let current = android_exchange(&executable, &device, serde_json::json!({"operation": "Snapshot"}));
        let peers = |snapshot: &Snapshot| snapshot.peers.iter().map(|peer| peer.id.clone()).collect::<std::collections::BTreeSet<_>>();
        assert!(client.snapshot().peers.is_empty(), "a files-only code paired its receiver");
        assert!(peers(&previous) == peers(&current), "a files-only code changed Android trust");
    }
    if background {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let state = adb(&executable, &device, &["shell", "dumpsys", "activity", "services", "ro.peersus"]);
                if !String::from_utf8_lossy(&state.stdout).contains("isForeground=true") { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await.expect("foreground protection kept running after all transfers completed");
        adb(&executable, &device, &["shell", "am", "start", "-n", "ro.peersus/dev.peersus.app.MainActivity"]);
        println!("E2E_BACKGROUND_VERIFIED: transfers finished while backgrounded and protection stopped when idle");
    }
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    println!("E2E_VERIFIED: all 80 real Android selections arrived exactly once with original bytes");
}

#[test]
#[ignore = "reads a pairing QR from an explicitly selected disposable emulator; prints aggregate sizes only"]
fn emulator_qr_payload_diagnostics() {
    let device = std::env::var("PEERSUS_E2E_EMULATOR").expect("select a dedicated emulator");
    assert!(device.starts_with("emulator-"));
    let executable = std::env::var("PEERSUS_ADB").unwrap_or_else(|_| "adb".into());
    let displayed = qr_on_screen(&executable, &device);
    assert!(displayed.starts_with("PEERSUS:Q2:"), "default Android QR must actually use the compact encoding");
    println!("QR_DISPLAYED: compact=true encoded_characters={}", displayed.len());
    let invitation = invitation_on_screen(&executable, &device);
    let bytes = hex::decode(invitation.strip_prefix("peersus:1:").unwrap()).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let qr = qrcodegen::QrCode::encode_text(&invitation.to_ascii_uppercase(), qrcodegen::QrCodeEcc::Medium).unwrap();
    println!("QR_DIAGNOSTICS: encoded_characters={} payload_bytes={} modules_per_side={}",
        invitation.len(), bytes.len(), qr.size());
    let compact = peersus_net::invitation::Invitation::compact_for_qr(&invitation).unwrap();
    let compact_qr = qrcodegen::QrCode::encode_text(&compact, qrcodegen::QrCodeEcc::Medium).unwrap();
    let reduction = 100. * (1. - f64::from(compact_qr.size().pow(2)) / f64::from(qr.size().pow(2)));
    println!("QR_COMPACT: encoded_characters={} modules_per_side={} module_area_reduction={reduction:.1}%",
        compact.len(), compact_qr.size());
    for (field, value) in body.as_object().unwrap() {
        println!("QR_FIELD_SIZE: {field}={}", serde_json::to_vec(value).unwrap().len());
    }
}

/// IPC is the real owner entry, not injected state. All payloads stay in memory.
fn android_command(executable: &str, device: &str, command: Command) -> Snapshot {
    android_exchange(executable, device, serde_json::json!({"operation": "Command", "command": command}))
}

fn android_exchange(executable: &str, device: &str, request: serde_json::Value) -> Snapshot {
    use std::io::{Read, Write};
    let request = serde_json::to_vec(&request).unwrap();
    let mut process = Process::new(executable)
        .args(["-s", device, "shell", "-T", "run-as", "ro.peersus", "toybox", "nc", "-U", "-w", "5", "-W", "2", "files/control.sock"])
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    let mut input = process.stdin.take().unwrap();
    input.write_all(&(request.len() as u32).to_be_bytes()).unwrap();
    input.write_all(&request).unwrap();
    input.flush().unwrap();
    // toybox nc treats stdin EOF as immediate shutdown: keep the request pipe alive
    // until the complete response has been read, not merely until writes have finished.
    let mut output = process.stdout.take().unwrap();
    let mut header = [0; 4];
    output.read_exact(&mut header).expect("Android owner did not reply");
    let length = u32::from_be_bytes(header) as usize;
    assert!(length <= 4 * 1024 * 1024, "oversized owner response");
    let mut bytes = vec![0; length];
    output.read_exact(&mut bytes).unwrap();
    drop(input);
    assert!(process.wait().unwrap().success(), "Android owner command transport failed");
    let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(response["result"].get("Ok").is_some(), "Android owner rejected fixture command");
    serde_json::from_value(response["snapshot"].clone()).unwrap()
}

fn has_notification(executable: &str, device: &str, id: u32) -> bool {
    let output = adb(executable, device, &["shell", "cmd", "notification", "list"]);
    String::from_utf8_lossy(&output.stdout).lines()
        .any(|line| line.contains("|ro.peersus|") && line.contains(&format!("|{id}|")))
}

#[tokio::test]
#[ignore = "dedicated emulator: real QR pairing and interactive Android notification permission must already be approved"]
async fn android_idle_background_remains_online_receives_offer_and_posts_system_notifications() {
    use peersus_core::config::SettingsPatch;
    let device = std::env::var("PEERSUS_E2E_EMULATOR").expect("select a dedicated emulator");
    assert!(device.starts_with("emulator-"), "physical devices are not accepted");
    let executable = std::env::var("PEERSUS_ADB").unwrap_or_else(|_| "adb".into());
    let directory = tempfile::tempdir().unwrap();
    let sockets = tempfile::Builder::new().prefix("pe2e").tempdir_in("/tmp").unwrap();
    let mut settings = SettingsStore::open(directory.path().join("preferences.json")).unwrap();
    settings.replace(Settings { device_name: "E2E-Background-Desktop".into(), ..Default::default() }).unwrap();
    let endpoint = transport::bind(iroh::SecretKey::generate(), &settings.current().network).await.unwrap();
    let network = NetworkHost::new(endpoint, "E2E-Background-Desktop".into(),
        Peers::open(directory.path().join("peers.json")).unwrap(),
        History::open(directory.path().join("history.json")).unwrap());
    let grants = peersus_net::grants::StandaloneGrants::open(directory.path().join("grants.json")).unwrap();
    let (runtime, client) = Runtime::new(network, grants, settings, Registry::default(), sockets.path().to_owned()).unwrap();
    let owner = tokio::spawn(runtime.run());
    let invitation = invitation_on_screen(&executable, &device);
    client.command(Command::JoinInvitation { invitation }).await.unwrap();
    let prompted = until(&client, |snapshot| !snapshot.pairing.is_empty()).await;
    let prompt = &prompted.pairing[0];
    let number = prompt.options[0];
    println!("E2E_PAIRING_NUMBER={number}: confirm in the emulator");
    client.command(Command::ChoosePairing { request: prompt.id.clone(), number }).await.unwrap();
    let paired = until(&client, |snapshot| snapshot.peers.iter().any(|peer| peer.online)).await;
    let peer = paired.peers.iter().find(|peer| peer.online).unwrap().id.clone();
    android_command(&executable, &device, Command::PatchSettings { patch: SettingsPatch { background: Some(true), ..Default::default() } });
    println!("E2E_NOTIFICATION_PERMISSION: approve the Android dialog if it is displayed");
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let permission = adb(&executable, &device, &["shell", "dumpsys", "package", "ro.peersus"]);
            if String::from_utf8_lossy(&permission.stdout).lines()
                .any(|line| line.contains("android.permission.POST_NOTIFICATIONS: granted=true")) { break; }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }).await.expect("notification permission requires real user approval; never auto-grant");
    adb(&executable, &device, &["shell", "input", "keyevent", "KEYCODE_HOME"]);
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert!(client.subscribe().borrow().peers.iter().any(|current| current.id == peer && current.online),
        "idle peer went offline after Home");
    let services = adb(&executable, &device, &["shell", "dumpsys", "activity", "services", "ro.peersus"]);
    assert!(String::from_utf8_lossy(&services.stdout).contains("isForeground=true"), "idle availability has no service");
    assert!(has_notification(&executable, &device, 7101), "background availability notification missing");
    let name = format!("background-{:08x}.txt", rand::random::<u32>());
    let source = directory.path().join(&name);
    let bytes = b"background transfer: verified encrypted bytes\n";
    std::fs::write(&source, bytes).unwrap();
    client.command(Command::SendFiles { peer, paths: vec![source] }).await.unwrap();
    until(&client, |snapshot| snapshot.transfers.iter().any(|transfer| transfer.state == TransferState::Offered)).await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while !has_notification(&executable, &device, 7102) { tokio::time::sleep(Duration::from_millis(200)).await; }
    }).await.expect("incoming offer did not produce a system notification while backgrounded");
    // Transfer IDs are local to each owner. Accept the Android-side offer, not the sender's ID.
    let incoming = android_exchange(&executable, &device, serde_json::json!({"operation": "Snapshot"}));
    let transfer = incoming.transfers.iter().find(|transfer| transfer.incoming
        && transfer.name == name && transfer.state == TransferState::Offered)
        .expect("Android notification has no matching incoming offer").id.clone();
    android_command(&executable, &device, Command::AcceptTransfer {
        transfer, directory: "/data/user/0/ro.peersus/files/incoming".into(),
    });
    let completed = until(&client, |snapshot| snapshot.transfers.iter().any(|transfer| transfer.state == TransferState::Complete
        || matches!(transfer.state, TransferState::Failed { .. } | TransferState::Cancelled))).await;
    assert_eq!(completed.transfers[0].state, TransferState::Complete);
    tokio::time::timeout(Duration::from_secs(20), async {
        while !has_notification(&executable, &device, 7103) { tokio::time::sleep(Duration::from_millis(200)).await; }
    }).await.expect("completed receipt did not produce a system notification");
    let destination = format!("files/incoming/{name}");
    let received = adb(&executable, &device, &["exec-out", "run-as", "ro.peersus", "cat", &destination]);
    assert_eq!(received.stdout, bytes);
    assert!(has_notification(&executable, &device, 7101), "idle background mode stopped after receiving");
    adb(&executable, &device, &["shell", "am", "start", "-n", "ro.peersus/dev.peersus.app.MainActivity"]);
    android_command(&executable, &device, Command::PatchSettings { patch: SettingsPatch { background: Some(false), ..Default::default() } });
    tokio::time::timeout(Duration::from_secs(10), async {
        while has_notification(&executable, &device, 7101) { tokio::time::sleep(Duration::from_millis(200)).await; }
    }).await.expect("disabling background mode left the idle foreground service active");
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    println!("E2E_VERIFIED: idle online presence, background offer, external notifications, exact bytes and service stop");
}
