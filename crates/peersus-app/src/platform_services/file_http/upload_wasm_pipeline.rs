//! Included as a child module of transport_tests; deliberately no public traffic.
use super::*;
use peersus_api::{
    manifest::Manifest,
    message::{Delivery, Operation},
};
use peersus_plugin::hub::Hub;
use peersus_plugin::{Event, Plugin, Service, module::ModuleLauncher, supervisor::Policy};
use std::path::PathBuf;

const PART: usize = 3 * 1024 * 1024;
const LINK: &str = "https://poo.wang/f/fixture";
const RECEIPTS: [&str; 2] = ["synthetic-receipt-zero", "synthetic-receipt-one"];
const FLOW_WAIT: Duration = Duration::from_secs(45);

struct PipelineUi(Arc<Mutex<Vec<String>>>);
impl Interaction for PipelineUi {
    fn choose(&self, _: PluginId) -> UiResponse<(std::fs::File, String)> {
        Box::pin(async { Err("unexpected picker") })
    }
    fn configure(&self, _: PluginId, _: reqwest::Url) -> UiResponse<()> {
        Box::pin(async { Err("unexpected configuration") })
    }
    fn authorize(
        &self,
        caller: PluginId,
        origin: reqwest::Url,
        name: String,
        bytes: u64,
    ) -> UiResponse<Zeroizing<Vec<u8>>> {
        Box::pin(async move {
            assert_eq!(caller, PluginId::new("upload").unwrap());
            assert_eq!(origin.as_str(), "https://poo.wang/");
            assert_eq!(name, "selected.bin");
            assert!(bytes > 0);
            Ok(Zeroizing::new(KEY.to_vec()))
        })
    }
    fn present(&self, caller: PluginId, link: String) -> UiResponse<()> {
        let presented = self.0.clone();
        Box::pin(async move {
            assert_eq!(caller, PluginId::new("upload").unwrap());
            assert_eq!(link, LINK);
            presented.lock().unwrap().push(link);
            Ok(())
        })
    }
}

fn artifact(name: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(name)
            .unwrap_or_else(|| panic!("{name} must name an explicit built artifact")),
    );
    assert!(
        path.is_absolute() && path.is_file(),
        "{name} must be an existing absolute file path"
    );
    path
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires explicit PEERSUS_UPLOAD_WASM and PEERSUS_WASM_HOST built artifacts"]
async fn compiled_upload_wasm_through_real_tls_multipart_and_chunks() {
    let module = artifact("PEERSUS_UPLOAD_WASM");
    let helper = artifact("PEERSUS_WASM_HOST");
    for chunked in [false, true] {
        pipeline_case(module.clone(), helper.clone(), chunked).await;
    }
}

async fn pipeline_case(module: PathBuf, helper: PathBuf, chunked: bool) {
    let success =
        r#"{"success":true,"files":[{"id":"fixture","url":"https://poo.wang/f/fixture"}]}"#;
    let replies = if chunked {
        vec![
            Some(response(
                "200 OK",
                "",
                &json!({"token":SESSION,"partSize":PART,"parts":2}).to_string(),
            )),
            Some(response(
                "200 OK",
                "",
                &json!({"success":true,"index":0,"receipt":RECEIPTS[0]}).to_string(),
            )),
            Some(response(
                "200 OK",
                "",
                &json!({"success":true,"index":1,"receipt":RECEIPTS[1]}).to_string(),
            )),
            Some(response("200 OK", "", success)),
        ]
    } else {
        vec![Some(response("200 OK", "", success))]
    };
    let server = Server::start_connect(replies).await;
    let directory = tempfile::Builder::new()
        .prefix("pwt-")
        .tempdir_in("/tmp")
        .unwrap();
    let snapshots = directory.path().join("snapshots");
    std::fs::create_dir(&snapshots).unwrap();
    let source = directory.path().join("selected.bin");
    let payload: Vec<u8> = if chunked {
        (0..PART + 19).map(|i| (i % 251) as u8).collect()
    } else {
        SOURCE.to_vec()
    };
    std::fs::write(&source, &payload).unwrap();
    let presented = Arc::new(Mutex::new(Vec::new()));
    let mut platform = FileHttpServices::new(snapshots, Arc::new(PipelineUi(presented.clone())));
    platform.test_root = Some(server.root.clone());
    // Explicit mandatory proxy: no origin DNS resolution and no bypass/fallback.
    platform.test_proxy = Some(server.origin.parse().unwrap());
    let caller = PluginId::new("upload").unwrap();
    let context = platform
        .select(
            &caller,
            crate::platform_services::SelectedInput::Path(source.clone()),
        )
        .unwrap();
    let package = peersus_api::package::split(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../peersus-upload/wasm/plugin.json"
    )))
    .unwrap();
    let manifest: Manifest = serde_json::from_slice(&package.manifest).unwrap();
    manifest.validate().unwrap();
    let host_manifest: Manifest = serde_json::from_value(json!({
        "manifest_version":1,"capabilities":{},
        "imports":[{"provider":"upload","service":"upload.file","version":1,"methods":["choose","configure"],"events":[]}],
        "exports":[
            {"service":"peersus.files","version":1,"description":"Real file authority","consumers":["upload"],"methods":{"selected":{"description":"Redeem"},"release":{"description":"Release"}},"events":{}},
            {"service":"peersus.http","version":1,"description":"Real HTTP authority","consumers":["upload"],"methods":{"configure":{"description":"Configure"},"authorize":{"description":"Consent"},"request":{"description":"Request"},"present":{"description":"Present"},"release":{"description":"Release"}},"events":{}}
        ]
    })).unwrap();
    let hub = Hub::default();
    let generation = hub
        .register_host("host", host_manifest, Default::default())
        .unwrap();
    let channel = hub.connect("host", generation).unwrap();
    channel
        .set_host_method_timeouts(
            [
                (
                    (FILES_SERVICE.into(), 1, "selected".into()),
                    Duration::from_secs(600),
                ),
                (
                    (FILES_SERVICE.into(), 1, "release".into()),
                    Duration::from_secs(600),
                ),
                (
                    (HTTP_SERVICE.into(), 1, "authorize".into()),
                    Duration::from_secs(600),
                ),
                (
                    (HTTP_SERVICE.into(), 1, "request".into()),
                    Duration::from_secs(600),
                ),
                (
                    (HTTP_SERVICE.into(), 1, "present".into()),
                    Duration::from_secs(600),
                ),
                (
                    (HTTP_SERVICE.into(), 1, "release".into()),
                    Duration::from_secs(600),
                ),
            ]
            .into(),
        )
        .unwrap();
    let launcher = ModuleLauncher::new(helper, module.clone()).unwrap();
    let worker = Service::start_with_launcher(
        hub,
        Plugin {
            id: "upload".into(),
            manifest,
            grants: Default::default(),
            program: Some(module),
            arguments: Vec::new(),
            directory: directory.path().to_owned(),
            policy: Policy {
                first_delay: Duration::from_millis(20),
                max_delay: Duration::from_millis(100),
                ..Policy::default()
            },
            handoff: None,
        },
        Arc::new(launcher),
    )
    .unwrap();
    let mut events = worker.events();
    let observed_platform = platform.clone();
    // Run assertions inside an owned task so a failure still reaches worker shutdown
    // and fixture join. No detached driver survives timeout or panic.
    let mut driver = tokio::spawn(async move {
        loop {
            if *events.borrow_and_update() == Event::Connected {
                break;
            }
            events.changed().await.unwrap();
        }
        channel
            .submit(Operation::Call {
                id: 1,
                provider: "upload".into(),
                service: "upload.file".into(),
                version: 1,
                method: "choose".into(),
                args: json!({"context":context}),
                timeout_ms: 40000,
            })
            .unwrap();
        let mut methods = Vec::new();
        let mut captures = Vec::new();
        loop {
            match channel.receive().await.unwrap() {
                Delivery::Call {
                    ticket,
                    caller,
                    service,
                    method,
                    args,
                    ..
                } => {
                    assert_eq!(caller, "upload");
                    // Real production authority/transport; only UI consent is synthetic.
                    let result = platform
                        .dispatch(Call {
                            caller: PluginId::new(&caller).unwrap(),
                            service: service.clone(),
                            method: method.clone(),
                            args: args.clone(),
                        })
                        .await;
                    let Outcome::Ok { value } = &result else {
                        panic!("platform operation failed: {service}/{method}: {result:?}")
                    };
                    let wire = serde_json::to_string(&(args.clone(), value.clone())).unwrap();
                    for secret in [
                        std::str::from_utf8(KEY).unwrap(),
                        SESSION,
                        RECEIPTS[0],
                        RECEIPTS[1],
                    ] {
                        assert!(
                            !wire.contains(secret),
                            "raw credential/session/receipt crossed guest boundary"
                        );
                    }
                    if service == FILES_SERVICE && method == "selected" {
                        assert_eq!(value["bytes"], payload.len());
                        // Snapshot is created by selected, not by select(context).
                        std::fs::write(&source, b"changed and truncated after snapshot").unwrap();
                    }
                    if service == HTTP_SERVICE && method == "request" {
                        if args.get("capture_secrets").is_some() {
                            let field = if args["method"] == "PUT" {
                                "receipt"
                            } else {
                                "token"
                            };
                            let opaque = value["json"][field].clone();
                            assert_eq!(opaque.as_object().unwrap().len(), 1);
                            assert!(opaque["secret"].as_str().unwrap().starts_with("secret-"));
                            captures.push(opaque);
                        }
                        if args["method"] == "PUT" {
                            assert_eq!(args["headers"]["x-upload-session"], captures[0]);
                        }
                        if args["path"] == "/api/upload/session/complete" {
                            assert_eq!(
                                args["json"],
                                json!({"token":captures[0],"receipts":[captures[1],captures[2]]})
                            );
                        }
                    }
                    methods.push(format!("{service}/{method}"));
                    channel.submit(Operation::Reply { ticket, result }).unwrap();
                }
                Delivery::Result { id: 1, result } => {
                    assert_eq!(result, Outcome::ok(json!({"status":"uploaded","url":LINK})));
                    break;
                }
                other => panic!("unexpected delivery: {other:?}"),
            }
        }
        let requests = if chunked { 4 } else { 1 };
        let mut expected = vec![
            format!("{FILES_SERVICE}/selected"),
            format!("{HTTP_SERVICE}/authorize"),
        ];
        expected.extend((0..requests).map(|_| format!("{HTTP_SERVICE}/request")));
        expected.extend([
            format!("{HTTP_SERVICE}/present"),
            format!("{HTTP_SERVICE}/release"),
            format!("{FILES_SERVICE}/release"),
        ]);
        assert_eq!(methods, expected);
        assert_eq!(captures.len(), if chunked { 3 } else { 0 });
        payload
    });
    let driven = match timeout(FLOW_WAIT, &mut driver).await {
        Ok(result) => Some(result),
        Err(_) => {
            driver.abort();
            let _ = driver.await;
            None
        }
    };
    // Always join the real worker before inspecting a driver failure.
    timeout(Duration::from_secs(10), worker.shutdown())
        .await
        .expect("upload worker shutdown timed out");
    let requests = server.finish().await;
    let payload = driven
        .expect("upload pipeline timed out")
        .expect("compiled upload guest pipeline failed");
    assert_eq!(*presented.lock().unwrap(), vec![LINK.to_owned()]);
    let state = observed_platform.state().unwrap();
    assert!(state.files.is_empty());
    assert!(
        state.authorizations.is_empty(),
        "release retained credentials/opaque receipts"
    );
    assert!(
        state.contexts.is_empty(),
        "selection context was not consumed"
    );
    drop(state);
    if chunked {
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].line, "POST /api/upload/session HTTP/1.1");
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[0].body).unwrap(),
            json!({"name":"selected.bin","type":"","size":payload.len(),"burnMode":"Burn after 24 hours","showcase":false})
        );
        for i in 0..2 {
            assert_eq!(
                requests[i + 1].line,
                format!("PUT /api/upload/session/part?index={i} HTTP/1.1")
            );
            assert_eq!(requests[i + 1].headers["x-upload-session"], SESSION);
            assert_eq!(
                requests[i + 1].headers["content-type"],
                "application/octet-stream"
            );
            assert_eq!(
                requests[i + 1].body,
                payload[i * PART..payload.len().min((i + 1) * PART)]
            );
            assert!(!requests[i + 1].headers.contains_key("authorization"));
        }
        assert_eq!(
            requests[3].line,
            "POST /api/upload/session/complete HTTP/1.1"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[3].body).unwrap(),
            json!({"token":SESSION,"receipts":RECEIPTS})
        );
        assert!(!requests[3].headers.contains_key("authorization"));
    } else {
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].line, "POST /api/upload HTTP/1.1");
        let boundary = requests[0].headers["content-type"]
            .strip_prefix("multipart/form-data; boundary=")
            .unwrap();
        let mut expected = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"selected.bin\"\r\n\r\n").into_bytes();
        expected.extend_from_slice(&payload);
        expected.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"burnMode\"\r\n\r\nBurn after 24 hours\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"showcase\"\r\n\r\nfalse\r\n--{boundary}--\r\n").as_bytes());
        assert_eq!(requests[0].body, expected);
    }
    assert_eq!(
        requests[0].headers["authorization"],
        format!("Bearer {}", std::str::from_utf8(KEY).unwrap())
    );
    for request in requests {
        assert_eq!(request.headers["host"], "poo.wang");
    }
}
