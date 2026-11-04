//! A deliberately broken plugin, used only to prove that a failing plugin cannot harm the
//! application. Built only with the `crash-test-plugin` feature; it is never shipped.
//!
//! The behaviour is chosen by the first argument. Every failing mode first connects and
//! identifies itself, so the application has a live connection that then dies abruptly.
use peersus_plugin_api::message::{Delivery, Operation, Outcome};
use peersus_plugin_host::{frame, link::Hello};
use std::{process::Command, time::Duration};
use tokio::net::UnixStream;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ok".into());
    let socket = std::env::var("PEERSUS_PLUGIN_SOCKET").expect("socket");
    let token = std::env::var("PEERSUS_PLUGIN_TOKEN").expect("token");
    if mode == "never-connect" {
        std::thread::sleep(Duration::from_secs(3600));
        return;
    }
    let mut stream = UnixStream::connect(&socket).await.expect("connect");
    frame::write(&mut stream, &Hello { token }, frame::CONTROL_LIMIT)
        .await
        .expect("hello");
    // If a path is given as the last argument of the normal mode, record who we are, so a
    // test can tell a restarted plugin from the old one by its process id.
    if matches!(mode.as_str(), "ok" | "hang" | "pidlog-exit")
        && let Some(path) = std::env::args().nth(2)
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("pid log");
        writeln!(file, "{}", std::process::id()).expect("pid write");
    }
    // The first run creates the marker and aborts; every later run finds it and behaves,
    // so a test can observe a real restart that comes back healthy.
    let mode = if mode == "crash-once" {
        let marker = std::env::args().nth(2).expect("marker path");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)
        {
            Ok(_) => "abort".to_owned(),
            Err(_) => "ok".to_owned(),
        }
    } else {
        mode
    };
    match mode.as_str() {
        // A real abort: no unwinding, no destructors, no chance to catch it in-process.
        "abort" => std::process::abort(),
        "exit" => std::process::exit(7),
        // Stops answering entirely. Only an outside force can end this.
        "hang" => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        // Records its pid (above), reads ONE delivery, then leaves by itself and cleanly: the
        // plugin that, told to restart, simply exits. That is what was asked of it and must
        // never be counted as a failure.
        "pidlog-exit" => {
            let _ = frame::read::<Delivery>(&mut stream, frame::CONTROL_LIMIT).await;
            std::process::exit(0);
        }
        // Takes its time, then reads ONE delivery and records that it arrived, to the path in
        // the second argument. The delay is what makes a stop that does not wait for pending
        // deliveries lose this one every time, instead of only when it is unlucky.
        "ack" => {
            let path = std::env::args().nth(2).expect("ack path");
            tokio::time::sleep(Duration::from_millis(150)).await;
            if let Ok(Delivery::Call { .. }) =
                frame::read::<Delivery>(&mut stream, frame::CONTROL_LIMIT).await
            {
                std::fs::write(path, "got").expect("ack write");
            }
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        // Starts a grandchild that would survive if only the leader were killed.
        "orphan" => {
            // Never waited on, on purpose: reaping it would end the orphan here and the
            // test would no longer prove that the supervisor's group kill is what does.
            #[expect(
                clippy::zombie_processes,
                reason = "the orphan is the thing under test"
            )]
            let child = Command::new("sleep").arg("3600").spawn().expect("sleep");
            // Tell the test which pid to check, through the only channel it controls.
            if let Some(path) = std::env::args().nth(2) {
                std::fs::write(path, child.id().to_string()).expect("pid file");
            }
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        // Sends the call's argument as bytes over a real data connection, the way a plugin
        // that was handed bulk content would, then answers. `data-wrong-token` does the same
        // with a token it made up, which must get it nowhere.
        "data" | "data-wrong-token" => loop {
            match frame::read::<Delivery>(&mut stream, frame::CONTROL_LIMIT).await {
                Ok(Delivery::Call { ticket, args, .. }) => {
                    use tokio::io::AsyncWriteExt;
                    let data_socket = std::env::var("PEERSUS_PLUGIN_DATA_SOCKET").expect("data");
                    let real = std::env::var("PEERSUS_PLUGIN_TOKEN").expect("token");
                    let token = if mode == "data" {
                        real
                    } else {
                        "made-up".to_owned()
                    };
                    let mut data = UnixStream::connect(&data_socket)
                        .await
                        .expect("data connect");
                    frame::write(
                        &mut data,
                        &peersus_plugin_host::data::DataHello {
                            token,
                            ticket,
                            claim: None,
                        },
                        1024,
                    )
                    .await
                    .expect("data hello");
                    let text = args.as_str().unwrap_or("").to_owned();
                    let _ = data.write_all(text.as_bytes()).await;
                    let _ = data.shutdown().await;
                    // Wait until the host has dealt with this connection. A refused one is
                    // closed by the host at once; an accepted one is closed when the
                    // application has read what was sent. Either way this returns only
                    // after the host has decided, so that a test that checks for a handed
                    // over connection afterwards is not racing the handshake.
                    let mut sink = Vec::new();
                    let _ = tokio::io::AsyncReadExt::read_to_end(&mut data, &mut sink).await;
                    let reply = Operation::Reply {
                        ticket,
                        result: Outcome::ok(serde_json::json!(true)),
                    };
                    if frame::write(&mut stream, &reply, frame::CONTROL_LIMIT)
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => return,
            }
        },
        // Answers every call with its argument doubled, so a healthy plugin is observable.
        _ => loop {
            match frame::read::<Delivery>(&mut stream, frame::CONTROL_LIMIT).await {
                Ok(Delivery::Call { ticket, args, .. }) => {
                    let n = args.as_i64().unwrap_or(0);
                    let reply = Operation::Reply {
                        ticket,
                        result: Outcome::ok((n * 2).into()),
                    };
                    if frame::write(&mut stream, &reply, frame::CONTROL_LIMIT)
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => return,
            }
        },
    }
}
