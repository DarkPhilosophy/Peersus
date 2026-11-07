//! Runs the real automatic session against authenticated local socket protocol, not a UI.
#![cfg(unix)]
use peersus_clipboard::{plugin::Clipboard, session};
use peersus_api::message::{Delivery, Operation, Outcome};
use peersus_plugin::{
    frame::{self, CONTROL_LIMIT},
    link::Hello,
    supervisor::Bootstrap,
};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::net::UnixListener;

struct Local {
    text: Arc<Mutex<String>>,
    reads: Arc<AtomicUsize>,
}
impl Clipboard for Local {
    async fn text(&mut self) -> Option<String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Some(self.text.lock().unwrap().clone())
    }
    async fn set_text(&mut self, text: &str) -> bool {
        *self.text.lock().unwrap() = text.into();
        true
    }
}
#[tokio::test]
async fn automatic_opt_in_baseline_duplicate_and_refusal_keep_the_live_session_usable() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let text = Arc::new(Mutex::new("pre-existing".to_owned()));
    let reads = Arc::new(AtomicUsize::new(0));
    let local = Local {
        text: Arc::clone(&text),
        reads: Arc::clone(&reads),
    };
    let host = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let hello: Hello = frame::read(&mut stream, CONTROL_LIMIT).await.unwrap();
        assert_eq!(hello.token, "private-test-bootstrap");
        let mut tick = 0;
        let mut sends = 0;
        let mut copied = false;
        loop {
            let call: Operation = frame::read(&mut stream, CONTROL_LIMIT).await.unwrap();
            let Operation::Call {
                id,
                service,
                method,
                ..
            } = call
            else {
                panic!("unexpected operation")
            };
            match (service.as_str(), method.as_str()) {
                ("peersus.config", "get") => {
                    if tick == 1 {
                        assert_eq!(
                            reads.load(Ordering::SeqCst),
                            0,
                            "disabled automatic sync read the OS clipboard"
                        );
                    }
                    // Eligibility reset no longer reads the clipboard. Establish the
                    // first actual snapshot before simulating a fresh local copy.
                    let observed = reads.load(Ordering::SeqCst);
                    if observed == 1 && !copied {
                        assert_eq!(sends, 0, "pre-existing clipboard was sent");
                        *text.lock().unwrap() = "fresh local copy".into();
                        copied = true;
                    }
                    if observed >= 3 {
                        assert!(copied);
                        assert_eq!(sends, 1, "duplicates were sent or the fresh copy was lost");
                        break;
                    }
                    frame::write(&mut stream, &Delivery::Result { id, result: Outcome::ok(json!({
                        "plugin": "clipboard",
                        "settings": {"automatic": tick != 0},
                        "peers": [{"device":"device-alias","online":true,"settings":{"sync":true}, "permissions":[
                            {"owner":"clipboard","name":"clipboard.use"},{"owner":"host","name":"peers.send"}
                        ]}]
                    })) }, CONTROL_LIMIT).await.unwrap();
                    tick += 1;
                }
                ("peersus.peers", "send") => {
                    sends += 1;
                    // Per-device grant refusal is normal, not a plugin crash or an offline queue.
                    frame::write(
                        &mut stream,
                        &Delivery::Result {
                            id,
                            result: Outcome::error("refused", "Not granted"),
                        },
                        CONTROL_LIMIT,
                    )
                    .await
                    .unwrap();
                }
                _ => panic!("unexpected host call"),
            }
        }
    };
    let plugin = session::run(
        local,
        Bootstrap {
            socket,
            data_socket: directory.path().join("data.sock"),
            token: "private-test-bootstrap".into(),
        },
    );
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        tokio::join!(host, plugin);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_host_without_config_keeps_explicit_calls_alive_and_never_reads_automatically() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let local = Local {
        text: Arc::new(Mutex::new("private".into())),
        reads: Arc::clone(&reads),
    };
    let host = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _: Hello = frame::read(&mut stream, CONTROL_LIMIT).await.unwrap();
        let Operation::Call { id, .. } = frame::read(&mut stream, CONTROL_LIMIT).await.unwrap()
        else {
            panic!("expected config")
        };
        frame::write(
            &mut stream,
            &Delivery::Result {
                id,
                result: Outcome::error("unavailable", "No configuration service"),
            },
            CONTROL_LIMIT,
        )
        .await
        .unwrap();
        frame::write(
            &mut stream,
            &Delivery::Call {
                ticket: 81,
                caller: "host".into(),
                service: "clipboard.text".into(),
                version: 1,
                method: "unknown".into(),
                args: json!({}),
            },
            CONTROL_LIMIT,
        )
        .await
        .unwrap();
        loop {
            match frame::read::<Operation>(&mut stream, CONTROL_LIMIT)
                .await
                .unwrap()
            {
                Operation::Reply {
                    ticket: 81,
                    result: Outcome::Error { .. },
                } => break,
                Operation::Call { id, .. } => {
                    frame::write(
                        &mut stream,
                        &Delivery::Result {
                            id,
                            result: Outcome::error("unavailable", "No configuration service"),
                        },
                        CONTROL_LIMIT,
                    )
                    .await
                    .unwrap();
                }
                _ => panic!("unexpected operation"),
            }
        }
        assert_eq!(reads.load(Ordering::SeqCst), 0);
    };
    let plugin = session::run(
        local,
        Bootstrap {
            socket,
            data_socket: directory.path().join("data.sock"),
            token: "private-test".into(),
        },
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(host, plugin);
    })
    .await
    .unwrap();
}

async fn a_slow_delivery_ack_keeps_the_session_alive(automatic: bool) {
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("slow-control.sock");
    let data_socket = directory.path().join("slow-data.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let data_listener = UnixListener::bind(&data_socket).unwrap();
    let text = Arc::new(Mutex::new("baseline".to_owned()));
    let reads = Arc::new(AtomicUsize::new(0));
    let local = Local { text: text.clone(), reads: reads.clone() };
    let plugin = tokio::spawn(session::run(local, Bootstrap {
        socket, data_socket, token: "synthetic-bootstrap".into(),
    }));
    let (completed, completion) = tokio::sync::oneshot::channel();
    let host = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _: Hello = frame::read(&mut stream, CONTROL_LIMIT).await.unwrap();
        if !automatic {
            frame::write(&mut stream, &Delivery::Call {
                ticket: 41, caller: "host".into(), service: "clipboard.text".into(),
                version: 1, method: "share".into(), args: json!({"peer":"device-alias"}),
            }, CONTROL_LIMIT).await.unwrap();
        }
        let mut sending = false;
        let mut completed = Some(completed);
        while let Ok(operation) = frame::read::<Operation>(&mut stream, CONTROL_LIMIT).await {
            match operation {
                Operation::Call { id, service, method, .. }
                    if service == "peersus.config" && method == "get" =>
                {
                    if automatic && sending {
                        let _ = completed.take().unwrap().send(true);
                        break;
                    }
                    if reads.load(Ordering::SeqCst) > 0 {
                        *text.lock().unwrap() = "fresh copy".into();
                    }
                    frame::write(&mut stream, &Delivery::Result { id, result: Outcome::ok(json!({
                        "plugin":"clipboard", "settings":{"automatic":automatic},
                        "peers":[{"device":"device-alias","online":true,"settings":{"sync":true},
                            "permissions":[{"owner":"clipboard","name":"clipboard.use"},
                                           {"owner":"host","name":"peers.send"}]}]
                    })) }, CONTROL_LIMIT).await.unwrap();
                }
                Operation::Call { id, service, method, .. }
                    if service == "peersus.peers" && method == "send" =>
                {
                    sending = true;
                    frame::write(&mut stream, &Delivery::Result {
                        id, result: Outcome::ok(json!({"claim":7})),
                    }, CONTROL_LIMIT).await.unwrap();
                }
                Operation::Reply { ticket: 41, result } if !automatic => {
                    let _ = completed.take().unwrap().send(matches!(result, Outcome::Ok { .. }));
                    break;
                }
                _ => panic!("unexpected session operation"),
            }
        }
    });
    let (mut data, _) = tokio::time::timeout(Duration::from_secs(5), data_listener.accept())
        .await.expect("session never began the transfer").unwrap();
    let hello: peersus_plugin::data::DataHello = frame::read(&mut data, CONTROL_LIMIT).await.unwrap();
    assert_eq!(hello.claim, Some(7));
    let mut payload = Vec::new();
    data.read_to_end(&mut payload).await.unwrap();
    tokio::time::sleep(Duration::from_secs(13)).await;
    assert!(!plugin.is_finished(), "outer session deadline preempted a valid delivery acknowledgement");
    frame::write(&mut data, &(payload.len() as u64), CONTROL_LIMIT).await.unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(3), completion).await.unwrap().unwrap());
    host.await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), plugin).await.unwrap().unwrap();
}

#[tokio::test]
async fn automatic_delivery_ack_after_nine_seconds_does_not_restart_the_session() {
    a_slow_delivery_ack_keeps_the_session_alive(true).await;
}

#[tokio::test]
async fn explicit_delivery_ack_after_twelve_seconds_does_not_restart_the_session() {
    a_slow_delivery_ack_keeps_the_session_alive(false).await;
}
