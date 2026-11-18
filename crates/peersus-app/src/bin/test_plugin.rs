//! A small plugin used only by the application's integration tests, which run it as a real
//! process under the real supervisor and hub. Built only with the `test-plugin` feature; it
//! is never shipped.
//!
//! By default it answers every call with a fixed marker, so a test can tell that the call
//! really travelled application -> hub -> socket -> this process and back.
//!
//! Given a log path as its first argument it also appends one line per event it observes
//! (`start <pid>`, `call <method>`, `cancelled`) and does NOT answer calls to the method
//! `share`, leaving them open. That lets a test see, from outside, that a call arrived,
//! that it was later withdrawn, and that the same process lived through it, which is how
//! the application's promise "withdrawing a permission stops that work but never restarts
//! the plugin" is checked against a real process.
#[cfg(all(unix, feature = "test-plugin"))]
fn main() {
    use peersus_api::message::{Delivery, Operation, Outcome};
    use peersus_plugin::{frame, link::Hello};
    use std::io::Write;

    let log_path = std::env::args().nth(1);
    let note = |line: String| {
        if let Some(path) = &log_path {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .expect("log");
            // One write for the line and its end: `writeln!` makes two, and a plugin stopped between them (a restart) left its
            // line without an end, joined to the first line of the next process (seen on macOS).
            file.write_all(format!("{line}\n").as_bytes())
                .expect("write log");
        }
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let socket = std::env::var("PEERSUS_PLUGIN_SOCKET").expect("socket");
        let token = std::env::var("PEERSUS_PLUGIN_TOKEN").expect("token");
        let mut stream = peersus_plugin::transport::connect(&socket)
            .await
            .expect("connect");
        frame::write(&mut stream, &Hello { token }, frame::CONTROL_LIMIT)
            .await
            .expect("hello");
        note(format!("start {}", std::process::id()));
        if std::env::args().nth(3).as_deref() == Some("send-out") {
            // Ask who can be sent to. The rest of the exchange continues from the answer.
            let call = Operation::Call {
                id: 1,
                provider: "host".into(),
                service: "peersus.peers".into(),
                version: 1,
                method: "list".into(),
                args: serde_json::Value::Null,
                timeout_ms: 5_000,
            };
            let _ = frame::write(&mut stream, &call, frame::CONTROL_LIMIT).await;
        }
        if std::env::args().nth(3).as_deref() == Some("ask-host") {
            // Ask the application for what it offers plugins, and record how each call ended
            // (below), so a test can see which layer let it through or refused it. With a fourth
            // argument, only once that file exists: a test allows a device first.
            if let Some(gate) = std::env::args().nth(4) {
                while !std::path::Path::new(&gate).exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
            for (id, method) in [(1u64, "list"), (2u64, "send")] {
                let call = Operation::Call {
                    id,
                    provider: "host".into(),
                    service: "peersus.peers".into(),
                    version: 1,
                    method: method.into(),
                    args: serde_json::Value::Null,
                    timeout_ms: 5_000,
                };
                let _ = frame::write(&mut stream, &call, frame::CONTROL_LIMIT).await;
            }
        }
        if std::env::args().nth(3).as_deref() == Some("ask") {
            // One call to a service of the application, named by the next three arguments (service, method, the
            // arguments as JSON), made once the file named by the argument after them exists: a test grants what
            // the call needs and then lets it go. How it ended is recorded below, with what was answered.
            if let Some(gate) = std::env::args().nth(7) {
                while !std::path::Path::new(&gate).exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }
            let call = Operation::Call {
                id: 1,
                provider: "host".into(),
                service: std::env::args().nth(4).unwrap_or_default(),
                version: 1,
                method: std::env::args().nth(5).unwrap_or_default(),
                args: std::env::args()
                    .nth(6)
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or_default(),
                timeout_ms: 30_000,
            };
            let _ = frame::write(&mut stream, &call, frame::CONTROL_LIMIT).await;
        }
        if std::env::args().nth(3).as_deref() == Some("flood") {
            // Far more at once than any extension's rate allows, of something the hub refuses
            // anyway: what is counted is the asking. Nothing is read meanwhile, as a guest
            // caught in a loop reads nothing.
            let publish = Operation::Publish {
                service: "nobody.service".into(),
                version: 1,
                event: "nothing".into(),
                data: serde_json::Value::Null,
            };
            for _ in 0..5_000 {
                if frame::write(&mut stream, &publish, frame::CONTROL_LIMIT)
                    .await
                    .is_err()
                {
                    break;
                }
            }
            note("flooded".to_owned());
        }
        if std::env::args().nth(3).as_deref() == Some("watch") {
            // Ask to be told when a plugin's state changes, and record each event delivered
            // (below). Whether the subscription was accepted is not told to the plugin: it is
            // seen by whether events ever arrive, which is exactly what a test checks.
            let subscribe = Operation::Subscribe {
                provider: "host".into(),
                service: std::env::args()
                    .nth(4)
                    .unwrap_or_else(|| "peersus.plugins".into()),
                version: 1,
                event: std::env::args()
                    .nth(5)
                    .unwrap_or_else(|| "state-changed".into()),
            };
            let _ = frame::write(&mut stream, &subscribe, frame::CONTROL_LIMIT).await;
            note("subscribed".to_owned());
        }
        if std::env::args().nth(3).as_deref() == Some("manage") {
            // Ask the application about the other plugins and then to restart one, and record
            // how each call ended (below). The target is the fourth argument. It asks ONCE,
            // remembered by a marker file beside the log, because the plugin named may be this
            // very one: without that, a plugin restarting itself would do so for ever, and a
            // test could not tell one request from a loop.
            let target = std::env::args().nth(4).unwrap_or_default();
            let dies = std::env::args().nth(5).as_deref() == Some("die");
            let marker = log_path.as_ref().map(|path| format!("{path}.managed"));
            if !dies
                && marker
                    .as_ref()
                    .is_some_and(|m| std::path::Path::new(m).exists())
            {
                note("already asked".to_owned());
            } else {
                if let (false, Some(marker)) = (dies, &marker) {
                    let _ = std::fs::write(marker, b"1");
                }
                for (id, method, args) in [
                    (1u64, "list", serde_json::Value::Null),
                    (2u64, "restart", serde_json::json!({ "plugin": target })),
                ] {
                    let call = Operation::Call {
                        id,
                        provider: "host".into(),
                        service: "peersus.plugins".into(),
                        version: 1,
                        method: method.into(),
                        args,
                        timeout_ms: 5_000,
                    };
                    let _ = frame::write(&mut stream, &call, frame::CONTROL_LIMIT).await;
                }
            }
        }
        if std::env::args().nth(3).as_deref() == Some("thief") {
            // Present a number that was issued to a different plugin. It is read from the file
            // named by the second argument, which the other plugin writes.
            let path = std::env::args().nth(2).unwrap_or_default();
            let mut claim = None;
            for _ in 0..200 {
                if let Ok(text) = std::fs::read_to_string(&path)
                    && let Ok(number) = text.trim().parse::<u64>()
                {
                    claim = Some(number);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            if let Some(claim) = claim {
                note(format!("thief has {claim}"));
                let data_socket = std::env::var("PEERSUS_PLUGIN_DATA_SOCKET").expect("data socket");
                let real = std::env::var("PEERSUS_PLUGIN_TOKEN").expect("token");
                if let Ok(mut data) = peersus_plugin::transport::connect(&data_socket).await {
                    let hello = peersus_plugin::data::DataHello {
                        token: real,
                        ticket: 0,
                        claim: Some(claim),
                    };
                    if frame::write(&mut data, &hello, 1024).await.is_ok() {
                        use tokio::io::AsyncWriteExt;
                        let wrote = data.write_all(&[0xEE; 5000]).await.is_ok();
                        let _ = data.shutdown().await;
                        note(format!("thief presented, wrote {wrote}"));
                    }
                }
            }
        }
        while let Ok(delivery) = frame::read::<Delivery>(&mut stream, frame::CONTROL_LIMIT).await {
            match delivery {
                Delivery::Call {
                    ticket,
                    caller,
                    method,
                    args,
                    ..
                } => {
                    note(format!("call {method}"));
                    // The argument exactly as the application sent it, so a test can look at
                    // what the plugin was told, for a device or anything else.
                    note(format!("arg {args}"));
                    if method == "share"
                        && std::env::args().nth(3).as_deref() == Some("share-to-peer")
                    {
                        // What the plugin was told is recorded above, for every call.
                        let device = args["peer"].as_str().unwrap_or("").to_owned();
                        let call = Operation::Call {
                            id: 2,
                            provider: "host".into(),
                            service: "peersus.peers".into(),
                            version: 1,
                            method: "send".into(),
                            args: serde_json::json!({
                                "device": device,
                                "service": "sink.service",
                                "version": 1,
                            }),
                            timeout_ms: 5_000,
                        };
                        let _ = frame::write(&mut stream, &call, frame::CONTROL_LIMIT).await;
                    }
                    if method == "receive" {
                        // The application announced bytes a peer sent. Fetch them over the data
                        // connection, naming the call, and keep them where a test can read.
                        use tokio::io::AsyncReadExt;
                        // Attack mode (third argument `steal`). The application numbers its own
                        // calls from 1, so a call this plugin makes to ITSELF under each of
                        // 1..=32 has, for one of them, the very number the application parked
                        // the peer's stream under. The hub gives each of those calls a genuine
                        // right for this plugin. Every one is claimed BEFORE the legitimate
                        // connection below, so a defence that only compares call numbers would
                        // hand this plugin the stream. What it manages to read is kept apart,
                        // in the file named by the fourth argument.
                        let mut stolen: Vec<u8> = Vec::new();
                        if std::env::args().nth(3).as_deref() == Some("steal") {
                            for id in 1..=32u64 {
                                let call = Operation::Call {
                                    id,
                                    provider: "sink".into(),
                                    service: "attack.service".into(),
                                    version: 1,
                                    method: "echo".into(),
                                    args: serde_json::Value::Null,
                                    timeout_ms: 20_000,
                                };
                                if frame::write(&mut stream, &call, frame::CONTROL_LIMIT)
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            let mut own_tickets = Vec::new();
                            while own_tickets.len() < 32 {
                                match frame::read::<Delivery>(&mut stream, frame::CONTROL_LIMIT)
                                    .await
                                {
                                    Ok(Delivery::Call { ticket, caller, .. })
                                        if caller == "sink" =>
                                    {
                                        own_tickets.push(ticket)
                                    }
                                    Ok(_) => {}
                                    Err(_) => return,
                                }
                            }
                            let token = std::env::var("PEERSUS_PLUGIN_TOKEN").expect("token");
                            let data_socket =
                                std::env::var("PEERSUS_PLUGIN_DATA_SOCKET").expect("data socket");
                            // The host takes at most 16 data handshakes at once and closes any
                            // beyond that, so the rights are claimed in batches of 8, each
                            // read out before the next begins, which keeps every one under
                            // the cap. A claim that fails is counted as not made rather than
                            // being allowed to end this process, which would only restart it
                            // and hide the result.
                            let mut claimed = 0usize;
                            for batch in own_tickets.chunks(8) {
                                let mut open = Vec::new();
                                for own in batch {
                                    let Ok(mut attempt) =
                                        peersus_plugin::transport::connect(&data_socket).await
                                    else {
                                        continue;
                                    };
                                    let hello = peersus_plugin::data::DataHello {
                                        token: token.clone(),
                                        ticket: *own,
                                        claim: None,
                                    };
                                    if frame::write(&mut attempt, &hello, 1024).await.is_ok() {
                                        claimed += 1;
                                        open.push(attempt);
                                    }
                                }
                                for mut attempt in open {
                                    let mut got = Vec::new();
                                    let _ = tokio::time::timeout(
                                        std::time::Duration::from_millis(500),
                                        attempt.read_to_end(&mut got),
                                    )
                                    .await;
                                    stolen.extend_from_slice(&got);
                                }
                            }
                            note(format!("sprayed {claimed}"));
                            if let Some(path) = std::env::args().nth(4) {
                                std::fs::write(path, &stolen).expect("stolen file");
                            }
                            note(format!("stole {}", stolen.len()));
                        }
                        let mode = std::env::args().nth(3);
                        if mode.as_deref() == Some("late") {
                            // Hold the right without using it, so a test can change the
                            // world between the announcement and the collection.
                            note("late waiting".to_owned());
                            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                        }
                        let data_socket =
                            std::env::var("PEERSUS_PLUGIN_DATA_SOCKET").expect("data socket");
                        let real = std::env::var("PEERSUS_PLUGIN_TOKEN").expect("token");
                        let mut data = peersus_plugin::transport::connect(&data_socket)
                            .await
                            .expect("data connect");
                        frame::write(
                            &mut data,
                            &peersus_plugin::data::DataHello {
                                token: real,
                                ticket,
                                claim: None,
                            },
                            1024,
                        )
                        .await
                        .expect("data hello");
                        if matches!(mode.as_deref(), Some("trickle" | "late")) {
                            // Progressive mode: each chunk is appended to the file the moment
                            // it arrives, so what was received at any instant can be seen from
                            // outside, which a single write at the end cannot show.
                            let mut got = 0usize;
                            let mut chunk = vec![0u8; 4096];
                            let path = std::env::args().nth(2);
                            loop {
                                match data.read(&mut chunk).await {
                                    Ok(0) | Err(_) => break,
                                    Ok(read) => {
                                        got += read;
                                        if let Some(path) = &path {
                                            let mut file = std::fs::OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open(path)
                                                .expect("received file");
                                            file.write_all(&chunk[..read]).expect("append");
                                        }
                                    }
                                }
                            }
                            note(format!("received {got}"));
                        } else {
                            let mut bytes = Vec::new();
                            let _ = data.read_to_end(&mut bytes).await;
                            if let Some(path) = std::env::args().nth(2) {
                                std::fs::write(path, &bytes).expect("received file");
                            }
                            note(format!("received {}", bytes.len()));
                        }
                    }
                    if log_path.is_some() && method == "share" {
                        // Left open on purpose; only a cancellation or the end of the
                        // process will close it.
                        continue;
                    }
                    // Echo who the hub says asked, so a test can check the identity was
                    // stamped.
                    let reply = Operation::Reply {
                        ticket,
                        result: Outcome::ok(serde_json::json!({ "answered_for": caller })),
                    };
                    if frame::write(&mut stream, &reply, frame::CONTROL_LIMIT)
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Delivery::Event {
                    service,
                    event,
                    data,
                    ..
                } => note(format!("event {service} {event} {data}")),
                Delivery::Cancelled { .. } => note("cancelled".to_owned()),
                Delivery::Result { id, result } => {
                    // Only meaningful for a plugin that made calls; the log is how a test sees
                    // whether the application answered and which layer refused.
                    let mode = std::env::args().nth(3);
                    match &result {
                        Outcome::Ok { value }
                            if mode.as_deref() == Some("ask")
                                || (mode.as_deref() == Some("manage") && id == 1) =>
                        {
                            // What was answered (a listing, a person's answer), so a test can see what it carried.
                            note(format!("result {id} ok {value}"));
                        }
                        Outcome::Ok { .. } => note(format!("result {id} ok")),
                        Outcome::Error { code, .. } => note(format!("result {id} {code}")),
                    }
                    // `die` (fifth argument of `manage`): one ask per process, then a crash, so
                    // the supervisor's pause between two processes is where a test can act.
                    if mode.as_deref() == Some("manage")
                        && id == 2
                        && std::env::args().nth(5).as_deref() == Some("die")
                    {
                        std::process::exit(3);
                    }
                    if matches!(mode.as_deref(), Some("send-out" | "share-to-peer")) {
                        match (id, result) {
                            // The listing came back. Look for the device to send to: the one
                            // named by the fourth argument, or the first if none is named.
                            // If it is not listed yet, ask again a moment later. A refused
                            // listing (no device allowed this plugin to see it yet) is the same.
                            (1, outcome) => {
                                let value = match outcome {
                                    Outcome::Ok { value } => value,
                                    Outcome::Error { .. } => serde_json::Value::Array(Vec::new()),
                                };
                                // Claim tests release this gate only after the host grant is
                                // installed. Pairing becoming visible is not that barrier.
                                let ready = std::env::args()
                                    .nth(8)
                                    .is_none_or(|path| std::path::Path::new(&path).exists());
                                let wanted = std::env::args().nth(4).filter(|name| name != "*");
                                let found =
                                    value.as_array().filter(|_| ready).and_then(|entries| {
                                        entries.iter().find(|entry| match &wanted {
                                            Some(name) => entry["name"].as_str() == Some(name),
                                            None => true,
                                        })
                                    });
                                // `once` (sixth argument): a plugin that has already sent does
                                // not send again after a restart, so a test can tell the old
                                // process's transfer from a new process starting over.
                                if found.is_some()
                                    && std::env::args().nth(6).as_deref() == Some("once")
                                    && let Some(path) = &log_path
                                {
                                    let marker = format!("{path}.sent");
                                    if std::path::Path::new(&marker).exists() {
                                        note("already sent".to_owned());
                                        continue;
                                    }
                                    let _ = std::fs::write(&marker, b"1");
                                }
                                let call = match found {
                                    Some(entry) => Operation::Call {
                                        id: 2,
                                        provider: "host".into(),
                                        service: "peersus.peers".into(),
                                        version: 1,
                                        method: "send".into(),
                                        args: serde_json::json!({
                                            "device": entry["device"].as_str().unwrap_or(""),
                                            "service": "sink.service",
                                            "version": 1,
                                        }),
                                        timeout_ms: 5_000,
                                    },
                                    None => {
                                        tokio::time::sleep(std::time::Duration::from_millis(150))
                                            .await;
                                        Operation::Call {
                                            id: 1,
                                            provider: "host".into(),
                                            service: "peersus.peers".into(),
                                            version: 1,
                                            method: "list".into(),
                                            args: serde_json::Value::Null,
                                            timeout_ms: 5_000,
                                        }
                                    }
                                };
                                let _ =
                                    frame::write(&mut stream, &call, frame::CONTROL_LIMIT).await;
                            }
                            // The application opened the stream and issued a claim: push the
                            // payload over the data connection, naming it.
                            (2, Outcome::Ok { value }) => {
                                let claim = value["claim"].as_u64().unwrap_or(0);
                                if std::env::args().nth(7).as_deref() == Some("share-claim")
                                    && let Some(path) = &log_path
                                {
                                    // Make the number available to another plugin, and give it
                                    // time to try before this plugin uses it itself.
                                    let _ =
                                        std::fs::write(format!("{path}.claim"), claim.to_string());
                                    tokio::time::sleep(std::time::Duration::from_millis(1500))
                                        .await;
                                }
                                let data_socket = std::env::var("PEERSUS_PLUGIN_DATA_SOCKET")
                                    .expect("data socket");
                                let real = std::env::var("PEERSUS_PLUGIN_TOKEN").expect("token");
                                if let Ok(mut data) =
                                    peersus_plugin::transport::connect(&data_socket).await
                                {
                                    let hello = peersus_plugin::data::DataHello {
                                        token: real,
                                        ticket: 0,
                                        claim: Some(claim),
                                    };
                                    if frame::write(&mut data, &hello, 1024).await.is_ok() {
                                        use tokio::io::AsyncWriteExt;
                                        let payload = std::env::args()
                                            .nth(2)
                                            .and_then(|path| std::fs::read(path).ok())
                                            .unwrap_or_default();
                                        if std::env::args().nth(5).as_deref() == Some("slow") {
                                            // Slices with a pause between them, so there is a
                                            // long moment when some bytes have gone and the
                                            // rest have not. A failed write is recorded, which
                                            // is how a test sees the transfer was cut.
                                            let mut written = 0usize;
                                            for slice in payload.chunks(10_000) {
                                                if data.write_all(slice).await.is_err() {
                                                    note(format!("write failed at {written}"));
                                                    break;
                                                }
                                                written += slice.len();
                                                note(format!("wrote {written}"));
                                                tokio::time::sleep(
                                                    std::time::Duration::from_millis(400),
                                                )
                                                .await;
                                            }
                                        } else {
                                            let _ = data.write_all(&payload).await;
                                        }
                                        let _ = data.shutdown().await;
                                        note(format!("pushed {}", payload.len()));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    });
}

#[cfg(not(all(unix, feature = "test-plugin")))]
fn main() {}
