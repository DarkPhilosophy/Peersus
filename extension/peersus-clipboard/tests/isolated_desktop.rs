//! The clipboard plugin as a real process, against a real clipboard.
//!
//! Everything else tests the plugin's logic with a clipboard that is a string. This runs the
//! binary the application would run, supervised by the real supervisor, talking to the real
//! hub, with the operating system's clipboard on the other side, so that what no fake can
//! show is seen: that the library really reads and writes it, from the thread that owns it.
//!
//! IT MUST NEVER TOUCH THE USER'S OWN CLIPBOARD. It only runs inside a virtual X server this
//! test is started in on purpose, and refuses to do anything unless told so and unless the
//! display it would use is not the one the session already had:
//!
//! ```text
//! PEERSUS_ISOLATED_CLIPBOARD=1 xvfb-run -a env -u WAYLAND_DISPLAY \
//!     cargo test -p peersus-clipboard --test isolated_desktop
//! ```
//!
//! Without that it passes without doing anything, and says so, rather than risk the real one.
// LINUX ONLY, on purpose. A virtual X server isolates the clipboard only where the library
// talks X11. On macOS it talks to the system pasteboard whatever `DISPLAY` says, so a test
// that wrote to "the clipboard" there would write to the user's real one.
#![cfg(target_os = "linux")]

use peersus_clipboard::wire::{self, Item};
use peersus_plugin::hub::{Channel, Hub};
use peersus_api::{
    manifest::{Export, Import, Manifest},
    message::{Delivery, Operation, Outcome},
    permission::Permission,
};
use peersus_plugin::{Accepted, Event, Plugin, Service, data::Right, supervisor::Policy};
use serde_json::json;
use std::{
    collections::BTreeSet,
    io::Cursor,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// `Some` only when it is certain that the clipboard in reach is a throwaway one.
fn isolated() -> Option<()> {
    if std::env::var("PEERSUS_ISOLATED_CLIPBOARD").as_deref() != Ok("1") {
        eprintln!("not run: PEERSUS_ISOLATED_CLIPBOARD=1 is not set");
        return None;
    }
    // Wayland would be chosen over X11 by the library, and it is the user's real session.
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        eprintln!("not run: WAYLAND_DISPLAY is set, so the real clipboard is in reach");
        return None;
    }
    // A virtual server from `xvfb-run -a` takes a number from 99 up; a real session is `:0`
    // or `:1`. A display this test was not started on is not one it may use.
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let number: Option<u32> = display
        .strip_prefix(':')
        .and_then(|rest| rest.split('.').next())
        .and_then(|n| n.parse().ok());
    if number.is_none_or(|n| n < 99) {
        eprintln!("not run: DISPLAY is not a virtual server");
        return None;
    }
    Some(())
}

fn names(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}

fn private_directory() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::Builder::new()
        .prefix("pc")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

/// Capabilities, each described: a name alone is refused.
fn capabilities(values: &[&str]) -> std::collections::BTreeMap<String, String> {
    values
        .iter()
        .map(|c| ((*c).to_owned(), format!("Lets the plugin use {c}.")))
        .collect()
}

/// A member that needs `capability` (or nothing) and says what it does.
fn member(capability: Option<&str>) -> peersus_api::manifest::Member {
    peersus_api::manifest::Member {
        capability: capability.map(str::to_owned),
        description: "Does what its name says.".to_owned(),
    }
}

/// What the plugin's manifest says, in the form the hub takes.
fn manifest() -> Manifest {
    Manifest {
        capabilities: capabilities(&["clipboard.use", "peers.send"]),
        imports: vec![Import {
            provider: "host".into(),
            service: "peersus.peers".into(),
            version: 1,
            methods: names(&["send"]),
            events: BTreeSet::new(),
        }],
        exports: vec![Export {
            service: "clipboard.text".into(),
            version: 1,
            description: "Shares clipboard text with a paired device.".into(),
            consumers: names(&["host"]),
            methods: [
                ("share".into(), member(Some("clipboard.use"))),
                ("receive".into(), member(Some("clipboard.use"))),
            ]
            .into(),
            events: Default::default(),
        }],
        ..Manifest::default()
    }
}

/// The application's own manifest: it offers `peersus.peers` `send` to the plugin, and may
/// call the plugin's `clipboard.text`.
fn host_manifest() -> Manifest {
    Manifest {
        capabilities: capabilities(&["clipboard.use", "peers.send"]),
        imports: vec![Import {
            provider: "clipboard".into(),
            service: "clipboard.text".into(),
            version: 1,
            methods: names(&["share", "receive"]),
            events: BTreeSet::new(),
        }],
        exports: vec![Export {
            service: "peersus.peers".into(),
            version: 1,
            description: "Lets a plugin send data to a paired device.".into(),
            consumers: names(&["clipboard"]),
            methods: [("send".into(), member(Some("peers.send")))].into(),
            events: Default::default(),
        }],
        ..Manifest::default()
    }
}

struct Running {
    app: Channel,
    service: Service,
    accepted: tokio::sync::mpsc::Receiver<Accepted>,
    _directory: tempfile::TempDir,
    /// Declared last so it is released last, after the plugin has been shut down.
    _exclusive: tokio::sync::MutexGuard<'static, ()>,
}

/// The scenarios share the one clipboard of the virtual server, so they cannot overlap: one
/// would read what another had just put there. The default test runner runs them at once, so
/// this is held by each for its whole life, whatever the number of threads.
static CLIPBOARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn start() -> Running {
    let exclusive = CLIPBOARD.lock().await;
    let directory = private_directory();
    let hub = Hub::default();
    // What each side is allowed to do to the other, each permission OF WHOEVER REQUIRES IT:
    // the application may call the plugin's `share` and `receive`, which the plugin guards
    // with its own `clipboard.use`; the plugin may call the application's `send`, which the
    // application guards with its own `peers.send`.
    let application_holds =
        BTreeSet::from([Permission::new("clipboard", "clipboard.use").unwrap()]);
    let plugin_holds = BTreeSet::from([Permission::new("host", "peers.send").unwrap()]);
    hub.register_host("host", host_manifest(), application_holds)
        .unwrap();
    let app = hub.connect("host", 1).unwrap();
    let (handoff, accepted) = tokio::sync::mpsc::channel(8);
    let service = Service::start(
        hub.clone(),
        Plugin {
            id: "clipboard".into(),
            manifest: manifest(),
            grants: plugin_holds,
            program: Some(PathBuf::from(env!("CARGO_BIN_EXE_peersus-clipboard"))),
            arguments: Vec::new(),
            directory: directory.path().to_path_buf(),
            policy: Policy {
                first_delay: Duration::from_millis(50),
                max_delay: Duration::from_millis(200),
                max_failures: 3,
                window: Duration::from_secs(60),
                healthy_after: Duration::from_secs(10),
            },
            handoff: Some(handoff),
        },
    )
    .unwrap();
    let mut events = service.events();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if *events.borrow_and_update() == Event::Connected {
                return;
            }
            events.changed().await.unwrap();
        }
    })
    .await
    .expect("the plugin never connected");
    Running {
        app,
        service,
        accepted,
        _directory: directory,
        _exclusive: exclusive,
    }
}

/// Put text on the clipboard the way another application would.
fn put(text: &str) {
    let mut child = Command::new("xclip")
        .args(["-selection", "clipboard", "-i"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("xclip");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    // xclip stays alive owning the selection until something else takes it; it is left to
    // the virtual server's end to clean up.
    std::mem::forget(child);
    std::thread::sleep(Duration::from_millis(300));
}

/// What another application would paste.
fn get() -> String {
    let out = Command::new("xclip")
        .args(["-selection", "clipboard", "-o"])
        .output()
        .expect("xclip");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

async fn answered(app: &Channel, id: u64) -> Outcome {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match app.receive().await.unwrap() {
                Delivery::Result { id: got, result } if got == id => return result,
                _ => {}
            }
        }
    })
    .await
    .expect("the plugin never answered")
}

/// The application's side of `peersus.peers` `send`: it answers with a claim.
async fn serve_send(app: &Channel, claim: u64) -> (u64, serde_json::Value) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Delivery::Call {
                ticket,
                method,
                args,
                ..
            } = app.receive().await.unwrap()
            {
                assert_eq!(method, "send");
                app.submit(Operation::Reply {
                    ticket,
                    result: Outcome::ok(json!({ "claim": claim })),
                })
                .unwrap();
                return (ticket, args);
            }
        }
    })
    .await
    .expect("the plugin never asked to send")
}

/// Every `peersus.peers` call the plugin makes within `window`, without answering any: what it
/// asked for is the whole point, not whether it was given it.
async fn sends_asked_for(app: &Channel, window: Duration) -> Vec<serde_json::Value> {
    let mut asked = Vec::new();
    let _ = tokio::time::timeout(window, async {
        loop {
            if let Delivery::Call { method, args, .. } = app.receive().await.unwrap()
                && method == "send"
            {
                asked.push(args);
            }
        }
    })
    .await;
    asked
}

/// A `receive` that has begun and is waiting for a device that has not finished sending, and a
/// `share` queued behind it with `share_deadline_ms` to live. Returns the connection holding
/// the receive open.
async fn receive_in_progress_with_share_queued(
    running: &mut Running,
    share_deadline_ms: u32,
) -> Accepted {
    running
        .app
        .submit(Operation::Call {
            id: 50,
            provider: "clipboard".into(),
            service: "clipboard.text".into(),
            version: 1,
            method: "receive".into(),
            args: json!({ "peer": "a-device" }),
            timeout_ms: 30_000,
        })
        .unwrap();
    // The plugin connects for it and waits for the device's bytes. They do not come yet.
    let held = tokio::time::timeout(Duration::from_secs(20), running.accepted.recv())
        .await
        .expect("the receive never opened a data connection")
        .expect("handoff closed");
    running
        .app
        .submit(Operation::Call {
            id: 51,
            provider: "clipboard".into(),
            service: "clipboard.text".into(),
            version: 1,
            method: "share".into(),
            args: json!({ "peer": "a-device" }),
            timeout_ms: share_deadline_ms,
        })
        .unwrap();
    held
}

#[tokio::test(flavor = "multi_thread")]
async fn a_share_that_expired_while_waiting_behind_a_receive_never_asks_to_send() {
    if isolated().is_none() {
        return;
    }
    let mut running = start().await;
    put("text the user no longer wants sent");
    let mut held = receive_in_progress_with_share_queued(&mut running, 1_500).await;
    // The share's deadline passes while the receive still holds the plugin: the application
    // is told it timed out, so the user has already seen it fail.
    assert!(matches!(
        answered(&running.app, 51).await,
        Outcome::Error { .. }
    ));
    // Only now does the other device finish, and the plugin is free to go on.
    let item = wire::encode(&Item::Text("the device's text".into())).unwrap();
    held.stream.write_all(&item).await.unwrap();
    held.stream.shutdown().await.unwrap();
    assert!(matches!(
        answered(&running.app, 50).await,
        Outcome::Ok { .. }
    ));
    // The share is long past. Reading the clipboard and asking to send it now would send,
    // to a device, text that the user was already told had not been sent.
    let asked = sends_asked_for(&running.app, Duration::from_secs(4)).await;
    assert!(
        asked.is_empty(),
        "an expired share still asked to send: {asked:?}"
    );
    running.service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_share_with_time_to_spare_does_ask_to_send_once_the_receive_is_done() {
    // The control for the test above: nothing differs but the share's deadline. It shows that
    // this scenario can see a send at all, so the silence above is the fix and not a blind spot.
    if isolated().is_none() {
        return;
    }
    let mut running = start().await;
    put("text the user still wants sent");
    let mut held = receive_in_progress_with_share_queued(&mut running, 30_000).await;
    tokio::time::sleep(Duration::from_millis(1_800)).await;
    let item = wire::encode(&Item::Text("the device's text".into())).unwrap();
    held.stream.write_all(&item).await.unwrap();
    held.stream.shutdown().await.unwrap();
    let asked = sends_asked_for(&running.app, Duration::from_secs(6)).await;
    assert_eq!(
        asked,
        [json!({ "device": "a-device", "service": "clipboard.text", "version": 1 })],
        "a share with time left was not carried out"
    );
    running.service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sharing_sends_what_another_application_put_on_the_real_clipboard() {
    if isolated().is_none() {
        return;
    }
    let mut running = start().await;
    put("copied by another application ✓");
    running
        .app
        .submit(Operation::Call {
            id: 10,
            provider: "clipboard".into(),
            service: "clipboard.text".into(),
            version: 1,
            method: "share".into(),
            args: json!({ "peer": "a-device" }),
            timeout_ms: 15_000,
        })
        .unwrap();
    // The plugin asks the application to send, naming the device it was given.
    let (_ticket, args) = serve_send(&running.app, 4242).await;
    assert_eq!(
        args,
        json!({ "device": "a-device", "service": "clipboard.text", "version": 1 })
    );
    // It then opens a data connection naming the claim it was issued, and sends the text.
    let mut connection = tokio::time::timeout(Duration::from_secs(20), running.accepted.recv())
        .await
        .expect("no data connection")
        .expect("handoff closed");
    let Right::Claim(claim) = connection.right else {
        panic!("a claim was expected");
    };
    assert_eq!(claim, 4242);
    let mut bytes = Vec::new();
    connection.stream.read_to_end(&mut bytes).await.unwrap();
    // The host confirms the delivered byte count before the plugin may report
    // success. Merely reading its write half to EOF is not a delivery receipt.
    peersus_plugin::frame::write(&mut connection.stream, &(bytes.len() as u64), 1024)
        .await
        .unwrap();
    assert_eq!(
        wire::decode(Cursor::new(bytes)).unwrap(),
        Item::Text("copied by another application ✓".into())
    );
    assert!(matches!(
        answered(&running.app, 10).await,
        Outcome::Ok { .. }
    ));
    running.service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn receiving_puts_what_a_device_sent_on_the_real_clipboard() {
    if isolated().is_none() {
        return;
    }
    let mut running = start().await;
    put("what was here before");
    running
        .app
        .submit(Operation::Call {
            id: 20,
            provider: "clipboard".into(),
            service: "clipboard.text".into(),
            version: 1,
            method: "receive".into(),
            args: json!({ "peer": "a-device" }),
            timeout_ms: 15_000,
        })
        .unwrap();
    // The plugin collects the stream over a data connection naming the call.
    let mut connection = tokio::time::timeout(Duration::from_secs(20), running.accepted.recv())
        .await
        .expect("no data connection")
        .expect("handoff closed");
    let Right::Hub(right) = &connection.right else {
        panic!("the hub's right was expected");
    };
    assert_eq!(right.call_id(), 20);
    let item = wire::encode(&Item::Text("sent from the other device ✓".into())).unwrap();
    connection.stream.write_all(&item).await.unwrap();
    connection.stream.shutdown().await.unwrap();
    assert!(matches!(
        answered(&running.app, 20).await,
        Outcome::Ok { .. }
    ));
    assert_eq!(get(), "sent from the other device ✓");
    running.service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_sends_something_that_is_not_text_leaves_the_clipboard_alone() {
    if isolated().is_none() {
        return;
    }
    let mut running = start().await;
    put("must survive");
    running
        .app
        .submit(Operation::Call {
            id: 30,
            provider: "clipboard".into(),
            service: "clipboard.text".into(),
            version: 1,
            method: "receive".into(),
            args: json!({ "peer": "a-device" }),
            timeout_ms: 15_000,
        })
        .unwrap();
    let mut connection = tokio::time::timeout(Duration::from_secs(20), running.accepted.recv())
        .await
        .expect("no data connection")
        .expect("handoff closed");
    connection
        .stream
        .write_all(b"this is not a clipboard item at all")
        .await
        .unwrap();
    connection.stream.shutdown().await.unwrap();
    assert!(matches!(
        answered(&running.app, 30).await,
        Outcome::Error { .. }
    ));
    assert_eq!(get(), "must survive");
    running.service.shutdown().await;
}

// WHAT THE HEADLESS SCENARIO PROVES, AND WHAT IT DOES NOT. With no display the library fails
// to open (checked: `Err`, "X11 server connection timed out because it was unreachable"), so
// the owner thread holds no clipboard and every call is answered with the fixed words. It
// proves the process stays connected and answers instead of ending, which is seen to fail if
// the process dies on the main task (the provider then disconnects). It does NOT prove that
// the clipboard thread itself surviving an open that PANICS matters: `open` runs on that
// thread (owner.rs), so a panic there only ends the thread, sends then fail, and the answer
// is the same words. That mutation is equivalent, not undetected, and is left as it is.

/// Only when there is no display of any kind, so there is no clipboard it could reach.
fn headless() -> Option<()> {
    if std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        eprintln!("not run: a display is set, so this is not a machine without one");
        return None;
    }
    Some(())
}

#[tokio::test(flavor = "multi_thread")]
async fn on_a_machine_with_no_display_the_plugin_stays_up_and_says_there_is_nothing_to_share() {
    if headless().is_none() {
        return;
    }
    let running = start().await;
    // Starting at all, and being connected to the hub, is the first thing shown: the library
    // failing to find a clipboard must not be a plugin that fails to start.
    running
        .app
        .submit(Operation::Call {
            id: 40,
            provider: "clipboard".into(),
            service: "clipboard.text".into(),
            version: 1,
            method: "share".into(),
            args: json!({ "peer": "a-device" }),
            timeout_ms: 15_000,
        })
        .unwrap();
    // And it says so in its own fixed words, without ever asking the application to send.
    let outcome = answered(&running.app, 40).await;
    let Outcome::Error { message, .. } = outcome else {
        panic!("nothing could have been shared, yet it said it was: {outcome:?}");
    };
    assert_eq!(message, "There is no text to share");
    // It is still the same live process: a second call is answered too, not met with a
    // plugin that has died and is being restarted.
    running
        .app
        .submit(Operation::Call {
            id: 41,
            provider: "clipboard".into(),
            service: "clipboard.text".into(),
            version: 1,
            method: "share".into(),
            args: json!({ "peer": "a-device" }),
            timeout_ms: 15_000,
        })
        .unwrap();
    assert!(matches!(
        answered(&running.app, 41).await,
        Outcome::Error { .. }
    ));
    running.service.shutdown().await;
}
