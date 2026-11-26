//! Real local TLS checks with in-memory synthetic credentials and certificates.
use super::*;
use std::{collections::BTreeMap, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpSocket,
    sync::oneshot,
    task::JoinHandle,
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, rustls};

const KEY: &[u8] = b"synthetic-bearer-for-transport-test";
const SESSION: &str = "synthetic-session-for-transport-test";
const SOURCE: &[u8] = b"prefix\0\xff\r\nselected payload\nsuffix";
const EXPECTED_USER_AGENT: &str = concat!("Peersus/", env!("CARGO_PKG_VERSION"));
const WAIT: Duration = Duration::from_secs(5);

struct Ui;
impl Interaction for Ui {
    fn choose(
        &self,
        _: PluginId,
        _: crate::platform_services::FileKind,
    ) -> UiResponse<(std::fs::File, String)> {
        Box::pin(async { Err("unexpected chooser") })
    }
    fn configure(&self, _: PluginId, _: reqwest::Url) -> UiResponse<()> {
        Box::pin(async { Ok(()) })
    }
    fn credential(&self, _: PluginId, _: reqwest::Url) -> CredentialResponse {
        Box::pin(async { Ok(Some(Zeroizing::new(KEY.to_vec()))) })
    }
    fn copy(&self, _: String) -> UiResponse<()> {
        Box::pin(async { Err("unexpected presentation") })
    }
}

#[derive(Debug)]
struct Received {
    line: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct Gate {
    reached: oneshot::Sender<()>,
    resume: oneshot::Receiver<()>,
}

impl Gate {
    async fn pause(self) {
        self.reached.send(()).unwrap();
        self.resume.await.unwrap();
    }
}

fn gate() -> (Gate, oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (reached, reached_receive) = oneshot::channel();
    let (resume, resume_receive) = oneshot::channel();
    (
        Gate {
            reached,
            resume: resume_receive,
        },
        reached_receive,
        resume,
    )
}

// HTTP/1.1 only, one request per TLS connection. Production requests under test
// have known byte lengths; rejecting chunked framing avoids a misleading parser.
async fn read_request(
    stream: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    body_gate: Option<(Gate, bool)>,
) -> Option<Received> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        assert!(
            bytes.len() < 16 * 1024,
            "request headers exceeded fixture bound"
        );
        let mut chunk = [0; 1024];
        let n = stream.read(&mut chunk).await.unwrap();
        assert_ne!(n, 0, "request ended before headers");
        bytes.extend_from_slice(&chunk[..n]);
    };
    let head = std::str::from_utf8(&bytes[..header_end]).unwrap();
    let mut lines = head.split("\r\n");
    let line = lines.next().unwrap().to_owned();
    let headers: BTreeMap<_, _> = lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    assert!(!headers.contains_key("transfer-encoding"));
    let length: usize = headers
        .get("content-length")
        .map(|value| value.parse().unwrap())
        .unwrap_or(0);
    assert!(length <= 7 * 1024 * 1024);
    let mut cancelled = false;
    if let Some((gate, expect_cancel)) = body_gate {
        gate.pause().await;
        cancelled = expect_cancel;
    }
    while bytes.len() < header_end + length {
        let mut chunk = [0; 4096];
        let n = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) if cancelled => return None,
            result => result.unwrap(),
        };
        assert_ne!(n, 0, "request ended before body");
        bytes.extend_from_slice(&chunk[..n]);
    }
    assert!(
        !cancelled,
        "a cancelled partial request still sent the whole body"
    );
    assert_eq!(bytes.len(), header_end + length);
    Some(Received {
        line,
        headers,
        body: bytes[header_end..].to_vec(),
    })
}

// A None response closes a fully received TLS request without HTTP response.
// Extra requests are recorded and answered, so redirects/retries cause assertions
// rather than hanging while waiting for a missing scripted response.
struct Server {
    origin: String,
    root: reqwest::Certificate,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Vec<Received>>,
}
impl Server {
    async fn start(replies: Vec<Option<String>>) -> Self {
        Self::start_inner(replies, None, None, None).await
    }
    /// A fixture reached through an explicit proxy, for the one authority it serves: the connection's `CONNECT` line is checked
    /// against it, so a request that went anywhere else fails the test rather than reaching a real server.
    #[cfg(unix)]
    async fn start_connect(replies: Vec<Option<String>>, authority: &'static str) -> Self {
        Self::start_inner(replies, Some(authority), None, None).await
    }
    async fn start_inner(
        replies: Vec<Option<String>>,
        connect: Option<&'static str>,
        mut body_gate: Option<(Gate, bool)>,
        mut reply_gate: Option<Gate>,
    ) -> Self {
        let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec![
            match connect {
                Some(authority) => authority.split(':').next().unwrap(),
                None => "localhost",
            }
            .into(),
        ])
        .unwrap();
        let root = reqwest::Certificate::from_der(cert.der().as_ref()).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpSocket::new_v4().unwrap();
        if body_gate.is_some() {
            // Bound how much loopback can buffer while this fixture is not reading.
            listener.set_recv_buffer_size(64 * 1024).unwrap();
        }
        listener
            .bind((std::net::Ipv4Addr::LOCALHOST, 0).into())
            .unwrap();
        let listener = listener.listen(32).unwrap();
        let port = listener.local_addr().unwrap().port();
        let origin = if connect.is_some() {
            format!("http://127.0.0.1:{port}/")
        } else {
            format!("https://localhost:{port}/")
        };
        let (stop, mut stopping) = oneshot::channel();
        let task = tokio::spawn(async move {
            let connect = connect;
            let mut received = Vec::new();
            let mut stopped = false;
            loop {
                let accepted = if stopped {
                    match timeout(Duration::from_millis(300), listener.accept()).await {
                        Ok(result) => result.unwrap(),
                        Err(_) => break,
                    }
                } else {
                    tokio::select! {
                        result = listener.accept() => result.unwrap(),
                        _ = &mut stopping => { stopped = true; continue; }
                    }
                };
                assert!(received.len() < 64, "fixture connection bound exceeded");
                timeout(WAIT, async {
                    let mut socket = accepted.0;
                    if let Some(authority) = connect {
                        // Consume exactly the CONNECT header, not TLS bytes.
                        // No upstream socket, DNS request or generic tunnel.
                        let mut head = Vec::new();
                        while !head.ends_with(b"\r\n\r\n") {
                            assert!(head.len() < 4096, "CONNECT header bound exceeded");
                            head.push(socket.read_u8().await.unwrap());
                        }
                        let head = std::str::from_utf8(&head).unwrap();
                        assert_eq!(
                            head.lines().next(),
                            Some(format!("CONNECT {authority} HTTP/1.1").as_str())
                        );
                        assert!(!head.to_ascii_lowercase().contains("proxy-authorization:"));
                        socket
                            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                            .await
                            .unwrap();
                        socket.flush().await.unwrap();
                    }
                    let mut stream = acceptor.accept(socket).await.unwrap();
                    let Some(request) = read_request(&mut stream, body_gate.take()).await else {
                        return;
                    };
                    let index = received.len();
                    received.push(request);
                    if let Some(gate) = reply_gate.take() {
                        gate.pause().await;
                    }
                    let fallback = Some(response("200 OK", "", "{}"));
                    let reply = replies.get(index).unwrap_or(&fallback);
                    if let Some(reply) = reply {
                        stream.write_all(reply.as_bytes()).await.unwrap();
                        stream.flush().await.unwrap();
                    }
                    // Drop without HTTP response for scripted transport failure.
                })
                .await
                .expect("fixture connection timed out");
            }
            received
        });
        Self {
            origin,
            root,
            stop: Some(stop),
            task,
        }
    }
    async fn finish(mut self) -> Vec<Received> {
        self.stop.take().unwrap().send(()).unwrap();
        match timeout(WAIT, &mut self.task).await {
            Ok(result) => result.expect("TLS fixture panicked"),
            Err(_) => {
                self.task.abort();
                let _ = (&mut self.task).await;
                panic!("TLS fixture failed to join");
            }
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        // Panic/cancellation path cannot leave a detached listener task.
        self.task.abort();
    }
}
fn response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    )
}
async fn service_outcome(
    host: &FileHttpServices,
    service: &str,
    method: &str,
    args: Value,
) -> Outcome {
    timeout(
        WAIT,
        host.dispatch(Call {
            caller: PluginId::new("transport-test").unwrap(),
            service: service.into(),
            method: method.into(),
            args,
        }),
    )
    .await
    .expect("platform operation timed out")
}

async fn invoke(
    host: &FileHttpServices,
    service: &str,
    method: &str,
    args: Value,
) -> Result<Value, String> {
    match service_outcome(host, service, method, args).await {
        Outcome::Ok { value } => Ok(value),
        Outcome::Error { message, .. } => Err(message),
        Outcome::MissingPermission { .. } | Outcome::NotAccepted {} => {
            panic!("transport adapter is below permission admission")
        }
    }
}
async fn selected(server: &Server) -> (tempfile::TempDir, FileHttpServices, String) {
    selected_with_source(server, SOURCE).await
}

async fn selected_with_source(
    server: &Server,
    payload: &[u8],
) -> (tempfile::TempDir, FileHttpServices, String) {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("selected.bin");
    std::fs::write(&source, payload).unwrap();
    std::fs::create_dir_all(scratch.path().join("snapshots")).unwrap();
    let mut host = FileHttpServices::agreed(scratch.path().join("snapshots"), Arc::new(Ui)).0;
    host.test_root = Some(server.root.clone());
    let context = host
        .select_for_action(
            &PluginId::new("transport-test").unwrap(),
            crate::platform_services::SelectedInput::Path(source.clone()),
            "upload",
        )
        .unwrap();
    let file = invoke(&host, FILES_SERVICE, "selected", json!({"context":context}))
        .await
        .unwrap();
    assert_eq!(file["bytes"], json!(payload.len()));
    // Change AND truncate the original after selection, before authorization/upload.
    std::fs::write(source, b"changed").unwrap();
    let auth = invoke(
        &host,
        HTTP_SERVICE,
        "authorize",
        json!({"file":file["file"],"origin":server.origin,"headers":["x-example-session"]}),
    )
    .await
    .unwrap();
    let authorization = auth["authorization"].as_str().unwrap().to_owned();
    (scratch, host, authorization)
}

fn payload_progress(updates: &super::super::ActionProgress) -> Option<(u64, u64)> {
    assert!(updates.len() <= 1);
    updates.values().next().and_then(|meter| meter.sample())
}

#[tokio::test]
async fn progress_counts_payload_once_even_without_a_success_response() {
    let server = Server::start(vec![
        Some(response("200 OK", "", "{}")),
        Some(response("503 Service Unavailable", "", "{}")),
        Some(response("200 OK", "", "invalid JSON")),
        Some(response("200 OK", "", "{}")),
        Some(response("200 OK", "", "{}")),
    ])
    .await;
    let (_scratch, host, auth) = selected(&server).await;
    let mut progress = host.action_progress().unwrap();
    assert_eq!(
        payload_progress(&progress.borrow_and_update()),
        Some((0, SOURCE.len() as u64))
    );
    for (offset, length, expected, status, wakes) in [
        (0, 10, 10, Some(200), true),
        (10, 10, 20, Some(503), true),
        (20, 5, 25, None, true),
        (0, 10, 25, Some(200), false),
        (
            10,
            SOURCE.len() as u64 - 10,
            SOURCE.len() as u64,
            Some(200),
            true,
        ),
    ] {
        let reply = invoke(
            &host,
            HTTP_SERVICE,
            "request",
            json!({
                "authorization":auth, "method":"POST", "path":"/upload",
                "part":{"offset":offset,"length":length}
            }),
        )
        .await;
        assert_eq!(
            reply.ok().and_then(|value| value["status"].as_u64()),
            status
        );
        if wakes {
            timeout(WAIT, progress.changed()).await.unwrap().unwrap();
        } else {
            assert!(!progress.has_changed().unwrap());
        }
        assert_eq!(
            payload_progress(&progress.borrow_and_update()),
            Some((expected, SOURCE.len() as u64))
        );
    }
    host.revoke(&PluginId::new("transport-test").unwrap());
    timeout(WAIT, progress.changed()).await.unwrap().unwrap();
    assert!(progress.borrow_and_update().is_empty());
    assert_eq!(server.finish().await.len(), 5);
}

#[tokio::test]
async fn raw_and_multipart_payload_progress_obeys_backpressure_before_the_reply() {
    for multipart in [false, true] {
        gated_upload(multipart, false).await;
    }
}

#[tokio::test]
async fn cancelling_a_platform_call_mid_body_does_not_finish_or_reply() {
    gated_upload(false, true).await;
}

async fn gated_upload(multipart: bool, cancel: bool) {
    let (body_gate, headers_read, read_body) = gate();
    let (reply_gate, body_read, send_reply) = gate();
    let server = Server::start_inner(
        vec![Some(response("200 OK", "", "{}"))],
        None,
        Some((body_gate, cancel)),
        (!cancel).then_some(reply_gate),
    )
    .await;
    let payload = vec![0xa5; 6 * 1024 * 1024];
    let total = payload.len() as u64;
    let (_scratch, host, auth) = selected_with_source(&server, &payload).await;
    let mut progress = host.action_progress().unwrap();
    assert_eq!(
        payload_progress(&progress.borrow_and_update()),
        Some((0, total))
    );
    let caller = PluginId::new("transport-test").unwrap();
    let mut args = json!({"authorization":auth, "method":"POST", "path":"/upload"});
    if multipart {
        args["multipart"] =
            json!({"field":"file","offset":0,"length":total,"fields":{"mode":"private"}});
    } else {
        args["part"] = json!({"offset":0,"length":total});
    }
    let mut calls = crate::platform_calls::PlatformCalls::default();
    calls
        .start(
            1,
            caller.clone(),
            peersus_api::permission::Permission::new(
                peersus_plugin::hub::HOST,
                crate::platform_services::HTTP_CAPABILITY,
            ),
            {
                let host = host.clone();
                let caller = caller.clone();
                move || {
                    host.dispatch(Call {
                        caller,
                        service: HTTP_SERVICE.into(),
                        method: "request".into(),
                        args,
                    })
                }
            },
        )
        .unwrap();
    timeout(WAIT, headers_read).await.unwrap().unwrap();
    timeout(WAIT, async {
        while timeout(Duration::from_millis(100), progress.changed())
            .await
            .is_ok()
        {
            let _ = payload_progress(&progress.borrow_and_update());
        }
    })
    .await
    .expect("payload kept moving while the receiver was blocked");
    let (partial, snapshot_total) = payload_progress(&progress.borrow_and_update()).unwrap();
    assert_eq!(snapshot_total, total);
    assert!(
        (0 < partial) && (partial < total),
        "expected a partial body, got {partial}/{total}"
    );
    if cancel {
        calls.cancel(1);
        let completion = timeout(WAIT, calls.next()).await.unwrap().unwrap();
        assert_eq!(completion.ticket, 1);
        assert_eq!(completion.result, None);
        host.revoke(&caller);
        timeout(WAIT, progress.changed()).await.unwrap().unwrap();
        assert!(progress.borrow_and_update().is_empty());
        read_body.send(()).unwrap();
        assert!(
            server.finish().await.is_empty(),
            "the server got a complete or retried upload"
        );
        return;
    }
    read_body.send(()).unwrap();
    timeout(WAIT, body_read).await.unwrap().unwrap();
    assert_eq!(
        payload_progress(&progress.borrow_and_update()),
        Some((total, total))
    );
    send_reply.send(()).unwrap();
    let completion = timeout(WAIT, calls.next()).await.unwrap().unwrap();
    assert_eq!(
        completion.result,
        Some(Outcome::ok(json!({"status":200,"json":{}})))
    );
    let mut received = server.finish().await;
    assert_eq!(received.len(), 1);
    let request = received.pop().unwrap();
    assert_eq!(
        request.headers["content-length"],
        request.body.len().to_string()
    );
    if multipart {
        let start = request.body.iter().position(|byte| *byte == 0xa5).unwrap();
        assert_eq!(&request.body[start..start + payload.len()], payload);
        assert!(request.body.len() > payload.len());
        assert!(
            std::str::from_utf8(&request.body[..start])
                .unwrap()
                .contains("name=\"file\"")
        );
    } else {
        assert_eq!(request.body, payload);
    }
}

#[tokio::test]
async fn tls_multipart_uses_snapshot_and_keeps_captured_session_opaque() {
    let server = Server::start(vec![
        Some(response(
            "200 OK",
            "",
            &format!(r#"{{"session":"{SESSION}","ready":true}}"#),
        )),
        Some(response("200 OK", "", r#"{"uploaded":true}"#)),
        Some(response(
            "200 OK",
            "",
            &format!(r#"{{"echo":"prefix-{SESSION}-suffix"}}"#),
        )),
        Some(response(
            "200 OK",
            "",
            &format!(r#"{{"{}":"echo"}}"#, std::str::from_utf8(KEY).unwrap()),
        )),
    ])
    .await;
    let (_scratch, host, auth) = selected(&server).await;
    let session = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({
            "authorization":auth,"method":"POST","path":"/session","bearer":true,
            "json":{"start":true},"capture_secrets":["/session"]
        }),
    )
    .await
    .unwrap();
    let opaque = &session["json"]["session"];
    assert_eq!(opaque.as_object().unwrap().len(), 1);
    assert!(opaque["secret"].as_str().unwrap().starts_with("secret-"));
    assert!(!session.to_string().contains(SESSION));
    assert!(
        !session
            .to_string()
            .contains(std::str::from_utf8(KEY).unwrap())
    );
    // A header that was not named when the person was asked is refused, and nothing is sent: the next request the
    // server sees is still the upload below.
    assert_eq!(
        invoke(
            &host,
            HTTP_SERVICE,
            "request",
            json!({
                "authorization":auth,"method":"POST","path":"/upload","bearer":true,
                "headers":{"x-other-session":opaque},
                "multipart":{"field":"file","offset":6,"length":18,"fields":{"mode":"private"}}
            }),
        )
        .await
        .unwrap_err(),
        "Unsupported request header"
    );
    let uploaded = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({
            "authorization":auth,"method":"POST","path":"/upload","bearer":true,
            "headers":{"x-example-session":opaque},
            "multipart":{"field":"file","offset":6,"length":18,"fields":{"mode":"private"}}
        }),
    )
    .await
    .unwrap();
    assert_eq!(uploaded, json!({"status":200,"json":{"uploaded":true}}));
    assert_eq!(
        payload_progress(&host.action_progress().unwrap().borrow()),
        Some((18, SOURCE.len() as u64))
    );
    let echo = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({
            "authorization":auth,"method":"POST","path":"/echo",
            "json":{"nested":[{"session":opaque}]}
        }),
    )
    .await;
    let key_echo = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({
            "authorization":auth,"method":"GET","path":"/credential-echo"
        }),
    )
    .await;
    // A distinct scheme or a different origin port is rejected before transport.
    let cleartext = server.origin.replacen("https:", "http:", 1);
    for path in [cleartext, "https://localhost:1/escape".into()] {
        assert_eq!(
            invoke(
                &host,
                HTTP_SERVICE,
                "request",
                json!({
                    "authorization":auth,"method":"GET","path":path
                })
            )
            .await
            .unwrap_err(),
            "The request must stay within the approved origin"
        );
    }
    let requests = server.finish().await;
    assert_eq!(requests.len(), 4);
    assert_eq!(
        echo.unwrap_err(),
        "The response attempted to disclose a credential"
    );
    assert_eq!(
        key_echo.unwrap_err(),
        "The response attempted to disclose a credential"
    );
    assert_eq!(requests[0].line, "POST /session HTTP/1.1");
    assert_eq!(requests[0].body, br#"{"start":true}"#);
    for request in &requests[..2] {
        assert_eq!(
            request.headers["authorization"],
            format!("Bearer {}", std::str::from_utf8(KEY).unwrap())
        );
    }
    assert_eq!(requests[1].line, "POST /upload HTTP/1.1");
    assert_eq!(requests[1].headers["x-example-session"], SESSION);
    let boundary = requests[1].headers["content-type"]
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap();
    let mut expected = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"selected.bin\"\r\n\r\n").into_bytes();
    expected.extend_from_slice(&SOURCE[6..24]);
    expected.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"mode\"\r\n\r\nprivate\r\n--{boundary}--\r\n").as_bytes());
    assert_eq!(requests[1].body, expected);
    assert_eq!(requests[2].line, "POST /echo HTTP/1.1");
    assert_eq!(
        requests[2].body,
        format!(r#"{{"nested":[{{"session":"{SESSION}"}}]}}"#).into_bytes()
    );
    assert!(!requests[2].headers.contains_key("authorization"));
}

#[tokio::test]
async fn tls_redirect_is_returned_without_following_or_forwarding_bearer() {
    let server = Server::start(vec![Some(response(
        "307 Temporary Redirect",
        "Location: /must-not-follow\r\n",
        "",
    ))])
    .await;
    let (_scratch, host, auth) = selected(&server).await;
    let result = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({
            "authorization":auth,"method":"POST","path":"/redirect","bearer":true,"json":{"one":1}
        }),
    )
    .await;
    let requests = server.finish().await;
    assert_eq!(result.unwrap(), json!({"status":307}));
    assert_eq!(requests.len(), 1, "redirect issued an extra request");
    assert_eq!(requests[0].line, "POST /redirect HTTP/1.1");
    assert_eq!(requests[0].body, br#"{"one":1}"#);
}

#[tokio::test]
async fn tls_transport_failure_is_not_retried() {
    let server = Server::start(vec![None]).await;
    let (_scratch, host, auth) = selected(&server).await;
    let result = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({
            "authorization":auth,"method":"GET","path":"/drop","bearer":true
        }),
    )
    .await;
    let requests = server.finish().await;
    assert_eq!(result.unwrap_err(), "HTTPS failed; no retry was attempted");
    assert_eq!(requests.len(), 1, "failed request was retried");
    assert_eq!(requests[0].line, "GET /drop HTTP/1.1");
    assert!(requests[0].body.is_empty());
}

#[tokio::test]
async fn tls_large_upload_receipts_fit_but_secret_storage_remains_bounded() {
    // 128 distinct receipts exercise the intended bounded cap and exceed the
    // 85 handles needed by a 250 MiB / 3 MiB upload plus its session token.
    // Each response respects the separate four-captures-per-request limit.
    let secret = |index: usize| format!("synthetic-receipt-{index:04x}-end");
    let mut replies = Vec::new();
    for batch in 0..32 {
        let values: Vec<_> = (batch * 4..batch * 4 + 4).map(&secret).collect();
        replies.push(Some(response(
            "200 OK",
            "",
            &serde_json::to_string(&json!({"receipts":values})).unwrap(),
        )));
    }
    replies.push(Some(response(
        "200 OK",
        "",
        &serde_json::to_string(&json!({"overflow":secret(128)})).unwrap(),
    )));
    replies.push(Some(response("200 OK", "", r#"{"accepted":true}"#)));
    let server = Server::start(replies).await;
    let (_scratch, host, auth) = selected(&server).await;
    let mut handles = Vec::new();
    for batch in 0..32 {
        let result = invoke(
            &host,
            HTTP_SERVICE,
            "request",
            json!({
                "authorization":auth,"method":"POST","path":format!("/receipts/{batch}"),
                "json":{"batch":batch},
                "capture_secrets":["/receipts/0","/receipts/1","/receipts/2","/receipts/3"]
            }),
        )
        .await
        .unwrap();
        let receipts = result["json"]["receipts"].as_array().unwrap();
        for (slot, receipt) in receipts.iter().enumerate() {
            assert!(!result.to_string().contains(&secret(batch * 4 + slot)));
            assert!(receipt["secret"].as_str().unwrap().starts_with("secret-"));
            handles.push(receipt.clone());
        }
    }
    let overflow = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({
            "authorization":auth,"method":"GET","path":"/overflow",
            "capture_secrets":["/overflow"]
        }),
    )
    .await;
    // Overflow must not invalidate previously issued opaque handles.
    let reused = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({
            "authorization":auth,"method":"POST","path":"/reuse",
            "json":{"first":handles[0],"last":handles[127]}
        }),
    )
    .await;
    let requests = server.finish().await;
    assert_eq!(overflow.unwrap_err(), "Too many response secrets");
    assert_eq!(
        reused.unwrap(),
        json!({"status":200,"json":{"accepted":true}})
    );
    assert_eq!(requests.len(), 34);
    assert_eq!(requests[33].line, "POST /reuse HTTP/1.1");
    assert_eq!(
        requests[33].body,
        serde_json::to_vec(&json!({"first":secret(0),"last":secret(127)})).unwrap()
    );
}

#[tokio::test]
async fn authorization_lost_while_waiting_for_worker_never_reaches_tls_server() {
    for action in ["revoke", "release-authorization", "release-file"] {
        let server = Server::start(Vec::new()).await;
        let (_scratch, host, auth) = selected(&server).await;
        let file = host.state().unwrap().authorizations[&auth].file.clone();
        let permits = host.workers.clone().acquire_many_owned(2).await.unwrap();
        let mut pending = host.dispatch(Call {
            caller: PluginId::new("transport-test").unwrap(),
            service: HTTP_SERVICE.into(),
            method: "request".into(),
            args: json!({
                "authorization":auth,"method":"POST","path":"/must-not-upload",
                "bearer":true,"part":{"offset":0,"length":4}
            }),
        });
        // Poll exactly once, deterministically parking the valid request on the
        // exhausted worker semaphore after its initial authorization lookup.
        // No sleep or detached request task is needed.
        std::future::poll_fn(|cx| {
            assert!(
                std::future::Future::poll(pending.as_mut(), cx).is_pending(),
                "request did not wait for a worker"
            );
            std::task::Poll::Ready(())
        })
        .await;
        match action {
            "revoke" => host.revoke(&PluginId::new("transport-test").unwrap()),
            "release-authorization" => {
                invoke(
                    &host,
                    HTTP_SERVICE,
                    "release",
                    json!({"authorization":auth}),
                )
                .await
                .unwrap();
            }
            "release-file" => {
                invoke(&host, FILES_SERVICE, "release", json!({"file":file}))
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        drop(permits);
        let result = timeout(WAIT, pending)
            .await
            .expect("blocked request did not finish");
        let requests = server.finish().await;
        assert!(
            matches!(result, Outcome::Error { .. }),
            "lost authority unexpectedly succeeded for {action}"
        );
        assert!(
            requests.is_empty(),
            "lost authority initiated network for {action}"
        );
    }
}

struct Keyed(Result<Option<&'static [u8]>, &'static str>, bool);
impl Interaction for Keyed {
    fn choose(
        &self,
        _: PluginId,
        _: crate::platform_services::FileKind,
    ) -> UiResponse<(std::fs::File, String)> {
        Box::pin(async { Err("unexpected chooser") })
    }
    fn configure(&self, _: PluginId, _: reqwest::Url) -> UiResponse<()> {
        Box::pin(async { Err("unexpected configuration") })
    }
    fn copy(&self, _: String) -> UiResponse<()> {
        Box::pin(async { Err("unexpected presentation") })
    }
    fn credential(&self, _: PluginId, _: reqwest::Url) -> CredentialResponse {
        assert!(
            !self.1,
            "`saved` tried to read the key instead of asking about presence"
        );
        let credential = self.0;
        Box::pin(async move { credential.map(|key| key.map(|key| Zeroizing::new(key.to_vec()))) })
    }
    fn credential_saved(&self, _: PluginId, _: reqwest::Url) -> UiResponse<bool> {
        let saved = self.0.map(|credential| credential.is_some());
        Box::pin(async move { saved })
    }
}

/// A host with no selected file: what the key check must work without.
fn bare_host(
    server: &Server,
    credential: Option<&'static [u8]>,
) -> (tempfile::TempDir, FileHttpServices) {
    let scratch = tempfile::tempdir().unwrap();
    let mut host = FileHttpServices::agreed(
        scratch.path().to_owned(),
        Arc::new(Keyed(Ok(credential), false)),
    )
    .0;
    host.test_root = Some(server.root.clone());
    (scratch, host)
}

#[tokio::test]
async fn key_check_sends_the_bearer_and_returns_only_the_status_even_for_a_rejection() {
    for (status, body) in [
        ("200 OK", r#"{"files":["private-name"]}"#),
        ("401 Unauthorized", r#"{"error":"nope"}"#),
    ] {
        let server = Server::start(vec![Some(response(status, "", body))]).await;
        let (_scratch, host) = bare_host(&server, Some(KEY));
        let reply = invoke(
            &host,
            HTTP_SERVICE,
            "check",
            json!({"origin":server.origin,"path":"/api/me"}),
        )
        .await
        .unwrap();
        let code: u64 = status[..3].parse().unwrap();
        // Only the status crosses back: a body is the provider's data and a key is never echoed.
        assert_eq!(reply, json!({"status":code}), "reply for {status}");
        let requests = server.finish().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].line, "GET /api/me HTTP/1.1");
        assert_eq!(
            requests[0].headers.get("user-agent").map(String::as_str),
            Some(EXPECTED_USER_AGENT)
        );
        assert_eq!(
            requests[0].headers.get("authorization").cloned(),
            Some(format!("Bearer {}", std::str::from_utf8(KEY).unwrap()))
        );
        assert!(requests[0].body.is_empty());
    }
}

#[tokio::test]
async fn key_check_does_not_follow_a_redirect_and_stays_in_the_approved_origin() {
    let server = Server::start(vec![Some(response(
        "307 Temporary Redirect",
        "Location: https://elsewhere.invalid/steal\r\n",
        "",
    ))])
    .await;
    let (_scratch, host) = bare_host(&server, Some(KEY));
    let reply = invoke(
        &host,
        HTTP_SERVICE,
        "check",
        json!({"origin":server.origin,"path":"/api/me"}),
    )
    .await
    .unwrap();
    assert_eq!(reply, json!({"status":307}));
    assert_eq!(
        server.finish().await.len(),
        1,
        "a redirect issued another request"
    );

    let server = Server::start(vec![]).await;
    let (_scratch, host) = bare_host(&server, Some(KEY));
    for path in [
        "https://elsewhere.invalid/api/me",
        "//elsewhere.invalid/api/me",
        "/api/me#fragment",
    ] {
        let refused = invoke(
            &host,
            HTTP_SERVICE,
            "check",
            json!({"origin":server.origin,"path":path}),
        )
        .await;
        assert!(refused.is_err(), "{path} left the approved origin");
    }
    assert!(
        server.finish().await.is_empty(),
        "a refused path reached the network"
    );
}

#[tokio::test]
async fn saved_reports_false_only_for_confirmed_absence() {
    let server = Server::start(vec![]).await;
    for (credential, expected) in [
        (Ok(Some(KEY)), Outcome::ok(json!({"saved":true}))),
        (Ok(None), Outcome::ok(json!({"saved":false}))),
        (
            Err("synthetic platform read failure"),
            Outcome::error("unavailable", "The credential could not be read"),
        ),
    ] {
        let scratch = tempfile::tempdir().unwrap();
        let (host, seen) =
            FileHttpServices::agreed(scratch.path().to_owned(), Arc::new(Keyed(credential, true)));
        assert_eq!(
            service_outcome(
                &host,
                HTTP_SERVICE,
                "saved",
                json!({"origin":server.origin})
            )
            .await,
            expected
        );
        assert!(seen.lock().unwrap().is_empty(), "saved asked for consent");
    }
    assert!(server.finish().await.is_empty());
}

#[tokio::test]
async fn missing_or_unreadable_credentials_stop_keyed_calls_before_prompt_or_network() {
    let server = Server::start(vec![]).await;
    for (credential, expected) in [
        (
            Ok(None),
            Outcome::error(
                "platform-refused",
                "Configure the API key in this extension's settings first",
            ),
        ),
        (
            Err("synthetic platform read failure"),
            Outcome::error("unavailable", "The credential could not be read"),
        ),
    ] {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("selected.bin");
        std::fs::write(&source, SOURCE).unwrap();
        let (mut host, seen) = FileHttpServices::agreed(
            scratch.path().to_owned(),
            Arc::new(Keyed(credential, false)),
        );
        host.test_root = Some(server.root.clone());
        let context = host
            .select_for_action(
                &PluginId::new("transport-test").unwrap(),
                crate::platform_services::SelectedInput::Path(source),
                "test",
            )
            .unwrap();
        let file = invoke(&host, FILES_SERVICE, "selected", json!({"context":context}))
            .await
            .unwrap();
        assert_eq!(
            service_outcome(
                &host,
                HTTP_SERVICE,
                "check",
                json!({"origin":server.origin,"path":"/api/me"})
            )
            .await,
            expected
        );
        assert_eq!(
            service_outcome(
                &host,
                HTTP_SERVICE,
                "authorize",
                json!({"file":file["file"],"origin":server.origin})
            )
            .await,
            expected
        );
        assert!(
            seen.lock().unwrap().is_empty(),
            "the person was asked to allow a request that could not work"
        );
        assert!(host.state().unwrap().authorizations.is_empty());
    }
    assert!(server.finish().await.is_empty());
}

#[tokio::test]
async fn key_check_refuses_an_origin_that_is_not_plain_https_before_any_network() {
    let server = Server::start(vec![]).await;
    let (_scratch, host) = bare_host(&server, Some(KEY));
    for origin in [
        "http://localhost:1/",
        "https://user:pass@localhost:1/",
        "https://localhost:1/api",
        "https://localhost:1/?x=1",
        "https://localhost:1/#frag",
        "ftp://localhost/",
        "not a url",
    ] {
        let refused = invoke(
            &host,
            HTTP_SERVICE,
            "check",
            json!({"origin":origin,"path":"/api/me"}),
        )
        .await;
        assert!(refused.is_err(), "{origin} was accepted as an origin");
    }
    assert!(
        server.finish().await.is_empty(),
        "a refused origin reached the network"
    );
}

#[tokio::test]
async fn key_check_never_returns_what_the_server_sends_even_when_it_echoes_the_key() {
    let echoed = format!(
        r#"{{"echo":"{}","token":"{}"}}"#,
        std::str::from_utf8(KEY).unwrap(),
        std::str::from_utf8(KEY).unwrap()
    );
    let server = Server::start(vec![Some(response(
        "200 OK",
        "Set-Cookie: s=secret\r\n",
        &echoed,
    ))])
    .await;
    let (_scratch, host) = bare_host(&server, Some(KEY));
    let reply = invoke(
        &host,
        HTTP_SERVICE,
        "check",
        json!({"origin":server.origin,"path":"/api/me"}),
    )
    .await
    .unwrap();
    // Exactly the status: no body, no headers, and so no key and no cookie, whatever the server chose to say.
    assert_eq!(reply, json!({"status":200}));
    assert!(
        !reply
            .to_string()
            .contains(std::str::from_utf8(KEY).unwrap())
    );
    server.finish().await;
}

/// Where an anonymous destination puts its links: a second address, allowed for the result and never requested.
const RESULTS: &str = "https://links.example";

/// The UI of a destination that needs no key: reading one there is the fault these tests exist to catch.
struct AnonymousUi;
impl Interaction for AnonymousUi {
    fn choose(
        &self,
        _: PluginId,
        _: crate::platform_services::FileKind,
    ) -> UiResponse<(std::fs::File, String)> {
        Box::pin(async { Err("unexpected chooser") })
    }
    fn configure(&self, _: PluginId, _: reqwest::Url) -> UiResponse<()> {
        Box::pin(async { Err("unexpected configuration") })
    }
    fn credential(&self, _: PluginId, _: reqwest::Url) -> CredentialResponse {
        Box::pin(async { Err("an anonymous destination is never asked for a key") })
    }
    fn copy(&self, _: String) -> UiResponse<()> {
        Box::pin(async { Ok(()) })
    }
}

async fn anonymous_host(
    server: &Server,
) -> (
    tempfile::TempDir,
    FileHttpServices,
    String,
    crate::notifications::Seen,
) {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("selected.png");
    std::fs::write(&source, SOURCE).unwrap();
    std::fs::create_dir_all(scratch.path().join("snapshots")).unwrap();
    let (mut host, seen) =
        FileHttpServices::agreed(scratch.path().join("snapshots"), Arc::new(AnonymousUi));
    host.test_root = Some(server.root.clone());
    let context = host
        .select_for_action(
            &PluginId::new("transport-test").unwrap(),
            crate::platform_services::SelectedInput::Path(source),
            "upload",
        )
        .unwrap();
    let file = invoke(&host, FILES_SERVICE, "selected", json!({"context":context}))
        .await
        .unwrap();
    let auth = invoke(
        &host,
        HTTP_SERVICE,
        "authorize",
        json!({
            "file":file["file"],"origin":server.origin,"headers":[],
            "anonymous":true,"result_origin":RESULTS
        }),
    )
    .await
    .unwrap();
    let authorization = auth["authorization"].as_str().unwrap().to_owned();
    (scratch, host, authorization, seen)
}

#[tokio::test]
async fn an_anonymous_authorization_reads_no_key_sends_none_and_returns_its_text_whole() {
    let answer = format!("{RESULTS}/fixture.png");
    let server = Server::start(vec![Some(response("200 OK", "", &answer))]).await;
    let (_scratch, host, authorization, seen) = anonymous_host(&server).await;
    // What the person allowed says both addresses and that no key is used, in the application's words.
    {
        let shown = seen.lock().unwrap();
        assert_eq!(shown.len(), 1, "one consent and nothing else was asked");
        assert_eq!(
            shown[0].notification,
            consent(
                "selected.png",
                SOURCE.len() as u64,
                &reqwest::Url::parse(&server.origin).unwrap(),
                &[],
                true,
                Some(&reqwest::Url::parse(RESULTS).unwrap())
            )
        );
    }
    assert_eq!(
        invoke(
            &host,
            HTTP_SERVICE,
            "request",
            json!({
                "authorization":authorization,"method":"POST","path":"/user/api.php","response":"text",
                "headers":{"user-agent":"guest-override"}
            }),
        )
        .await
        .unwrap_err(),
        "Unsupported request header"
    );
    let reply = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({
            "authorization":authorization,"method":"POST","path":"/user/api.php","response":"text",
            "multipart":{"field":"fileToUpload","offset":0,"length":SOURCE.len(),"fields":{"reqtype":"fileupload"}}
        }),
    )
    .await
    .unwrap();
    // The body is the destination's own text, whole and unparsed: not a `json` field, which would be `null` for it.
    assert_eq!(reply, json!({"status":200,"text":answer}));
    let presented = invoke(
        &host,
        HTTP_SERVICE,
        "present",
        json!({"authorization":authorization,"link":answer}),
    )
    .await
    .unwrap();
    assert_eq!(presented, json!({"presented":true}));
    let requests = server.finish().await;
    assert_eq!(requests.len(), 1, "one request for one file");
    assert_eq!(requests[0].line, "POST /user/api.php HTTP/1.1");
    assert_eq!(
        requests[0].headers.get("user-agent").map(String::as_str),
        Some(EXPECTED_USER_AGENT)
    );
    assert!(
        !requests[0].headers.contains_key("authorization"),
        "an anonymous destination was sent a bearer"
    );
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(
        body.contains("name=\"reqtype\"") && body.contains("fileupload"),
        "{body}"
    );
    assert!(
        body.contains("name=\"fileToUpload\"; filename=\"selected.png\""),
        "{body}"
    );
}

#[tokio::test]
async fn an_anonymous_authorization_never_sends_a_bearer_and_never_reaches_the_result_address() {
    let server = Server::start(vec![]).await;
    let (_scratch, host, authorization, _seen) = anonymous_host(&server).await;
    let refused = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({"authorization":authorization,"method":"POST","path":"/user/api.php","bearer":true,"json":{"a":1}}),
    )
    .await
    .unwrap_err();
    assert_eq!(refused, "No credential is saved for this destination");
    // A path that leaves the destination for the result address is refused by the origin rule before anything is sent.
    for path in [
        format!("{RESULTS}/fixture.png"),
        "https://links.example/fixture.png".to_owned(),
    ] {
        let refused = invoke(
            &host,
            HTTP_SERVICE,
            "request",
            json!({"authorization":authorization,"method":"POST","path":path,"json":{"a":1}}),
        )
        .await;
        assert!(refused.is_err(), "{path} left the approved origin");
    }
    // The result address shows only what the destination answered, and the destination's own address is not it.
    let refused = invoke(
        &host,
        HTTP_SERVICE,
        "present",
        json!({"authorization":authorization,"link":format!("{}/fixture.png", server.origin)}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        refused,
        "The result link must stay within the approved origin"
    );
    assert!(
        server.finish().await.is_empty(),
        "a refused request reached the network"
    );
}

#[tokio::test]
async fn a_text_response_is_bounded_and_holds_only_printed_characters() {
    for (body, expected) in [
        ("a".repeat(4097), "HTTPS response exceeds its bound"),
        ("h\u{e9}llo".to_owned(), "Invalid text response"),
        (format!("{RESULTS}/ok\u{0}"), "Invalid text response"),
    ] {
        let server = Server::start(vec![Some(response("200 OK", "", &body))]).await;
        let (_scratch, host, authorization, _seen) = anonymous_host(&server).await;
        let refused = invoke(
            &host,
            HTTP_SERVICE,
            "request",
            json!({"authorization":authorization,"method":"POST","path":"/user/api.php","response":"text"}),
        )
        .await
        .unwrap_err();
        assert_eq!(refused, expected);
        server.finish().await;
    }
    // A text answer carries no secret fields, and asking for one is refused before anything is sent.
    let server = Server::start(vec![]).await;
    let (_scratch, host, authorization, _seen) = anonymous_host(&server).await;
    let refused = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({
            "authorization":authorization,"method":"POST","path":"/user/api.php","response":"text",
            "json":{"a":1},"capture_secrets":["/token"]
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(refused, "A text response carries no secret fields");
    // A JSON answer is still parsed and still comes back under `json`, with no `text` field beside it.
    let server = Server::start(vec![Some(response("200 OK", "", r#"{"ok":true}"#))]).await;
    let (_scratch, host, authorization, _seen) = anonymous_host(&server).await;
    let reply = invoke(
        &host,
        HTTP_SERVICE,
        "request",
        json!({"authorization":authorization,"method":"POST","path":"/user/api.php","json":{"a":1}}),
    )
    .await
    .unwrap();
    assert_eq!(reply, json!({"status":200,"json":{"ok":true}}));
    server.finish().await;
}

#[tokio::test]
async fn a_result_origin_named_for_one_authorization_changes_nothing_of_the_one_before_it() {
    let server = Server::start(vec![Some(response("200 OK", "", "{}"))]).await;
    let (_scratch, host, authorization) = selected(&server).await;
    let refused = invoke(
        &host,
        HTTP_SERVICE,
        "present",
        json!({"authorization":authorization,"link":format!("{RESULTS}/fixture")}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        refused,
        "The result link must stay within the approved origin"
    );
    server.finish().await;
}

#[cfg(unix)]
#[path = "upload_wasm_pipeline.rs"]
pub(super) mod upload_wasm_pipeline;
