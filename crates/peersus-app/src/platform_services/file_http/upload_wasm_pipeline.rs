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
const FLOW_WAIT: Duration = Duration::from_secs(45);

struct PipelineUi {
    presented: Arc<Mutex<Vec<String>>>,
    keyed: bool,
}
impl Interaction for PipelineUi {
    fn choose(
        &self,
        _: PluginId,
        _: crate::platform_services::FileKind,
    ) -> UiResponse<(std::fs::File, String)> {
        Box::pin(async { Err("unexpected picker") })
    }
    fn configure(&self, _: PluginId, _: reqwest::Url) -> UiResponse<()> {
        Box::pin(async { Err("unexpected configuration") })
    }
    fn credential(&self, caller: PluginId, origin: reqwest::Url) -> CredentialResponse {
        let keyed = self.keyed;
        Box::pin(async move {
            assert_eq!(caller, PluginId::new("upload").unwrap());
            assert_eq!(origin.as_str(), "https://poo.wang/");
            Ok(keyed.then(|| Zeroizing::new(KEY.to_vec())))
        })
    }
    fn copy(&self, link: String) -> UiResponse<()> {
        let copied = self.presented.clone();
        Box::pin(async move {
            copied.lock().unwrap().push(link);
            Ok(())
        })
    }
}

pub(crate) fn artifact(name: &str) -> PathBuf {
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

/// A real request paused after its headers, with no route beyond the loopback fixture.
pub(crate) struct PausedUpload {
    pub(crate) platform: Arc<FileHttpServices>,
    headers_read: oneshot::Receiver<()>,
    read_body: oneshot::Sender<()>,
    server: Server,
}

impl PausedUpload {
    pub(crate) async fn catbox(scratch: PathBuf) -> Self {
        let (body_gate, headers_read, read_body) = gate();
        let server = Server::start_inner(
            vec![None],
            Some("catbox.moe:443"),
            Some((body_gate, true)),
            None,
        )
        .await;
        let (mut platform, _) = FileHttpServices::agreed(scratch, Arc::new(Ui));
        platform.test_root = Some(server.root.clone());
        platform.test_proxy = Some(server.origin.parse().unwrap());
        Self {
            platform: Arc::new(platform),
            headers_read,
            read_body,
            server,
        }
    }

    pub(crate) async fn wait_for_headers(&mut self) {
        (&mut self.headers_read).await.unwrap();
    }

    /// The platform generation and the counts of live files and authorizations.
    pub(crate) fn resources(&self, caller: &PluginId) -> (u64, usize, usize) {
        let state = self.platform.state().unwrap();
        (
            state.generation(caller),
            state.files.len(),
            state.authorizations.len(),
        )
    }

    pub(crate) async fn assert_aborted(self) {
        self.read_body.send(()).unwrap();
        assert!(
            self.server.finish().await.is_empty(),
            "cancelled upload sent a complete body or retried"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires explicit PEERSUS_UPLOAD_WASM and PEERSUS_WASM_HOST built artifacts"]
async fn compiled_upload_wasm_through_real_tls_multipart_and_chunks() {
    let module = artifact("PEERSUS_UPLOAD_WASM");
    let helper = artifact("PEERSUS_WASM_HOST");
    for (keyed, chunked) in [(true, false), (false, false), (true, true), (false, true)] {
        pipeline_case(module.clone(), helper.clone(), keyed, chunked).await;
    }
}

async fn pipeline_case(module: PathBuf, helper: PathBuf, keyed: bool, chunked: bool) {
    let mut success = json!({"success":true,"files":[{"id":"fixture","url":LINK}]});
    if !keyed {
        success["anonymous"] = json!(true);
    }
    let payload: Vec<u8> = if chunked {
        let size = if keyed { PART + 19 } else { 25 * 1024 * 1024 };
        (0..size).map(|i| (i % 251) as u8).collect()
    } else {
        SOURCE.to_vec()
    };
    let parts = if chunked {
        payload.len().div_ceil(PART)
    } else {
        0
    };
    let receipts: Vec<_> = (0..parts)
        .map(|i| format!("synthetic-opaque-receipt-{i:02}"))
        .collect();
    let mut replies = Vec::new();
    if keyed {
        // The key check precedes even file selection; anonymous mode never makes it.
        replies.push(Some(response("200 OK", "", "{}")));
    }
    if chunked {
        replies.push(Some(response(
            "200 OK",
            "",
            &json!({"token":SESSION,"partSize":PART,"parts":parts}).to_string(),
        )));
        for (index, receipt) in receipts.iter().enumerate() {
            let accepted = (payload.len() - index * PART).min(PART);
            replies.push(Some(response(
                "200 OK",
                "",
                &json!({"success":true,"index":index,"receipt":receipt,"acceptedBytes":accepted})
                    .to_string(),
            )));
        }
    }
    replies.push(Some(response("200 OK", "", &success.to_string())));
    let server = Server::start_connect(replies, "poo.wang:443").await;
    let directory = tempfile::Builder::new()
        .prefix("pwt-")
        .tempdir_in("/tmp")
        .unwrap();
    let snapshots = directory.path().join("snapshots");
    std::fs::create_dir(&snapshots).unwrap();
    let source = directory.path().join("selected.bin");
    std::fs::write(&source, &payload).unwrap();
    let presented = Arc::new(Mutex::new(Vec::new()));
    let (mut platform, seen) = FileHttpServices::agreed(
        snapshots,
        Arc::new(PipelineUi {
            presented: presented.clone(),
            keyed,
        }),
    );
    platform.test_root = Some(server.root.clone());
    // Explicit mandatory proxy: no origin DNS resolution and no bypass/fallback.
    platform.test_proxy = Some(server.origin.parse().unwrap());
    let caller = PluginId::new("upload").unwrap();
    let context = platform
        .select_for_action(
            &caller,
            crate::platform_services::SelectedInput::Path(source.clone()),
            if keyed { "upload" } else { "upload_keyless" },
        )
        .unwrap();
    let manifest_bytes = std::fs::read(artifact("PEERSUS_UPLOAD_MANIFEST")).unwrap();
    let package = peersus_api::package::split(&manifest_bytes).unwrap();
    let manifest: Manifest = serde_json::from_slice(&package.manifest).unwrap();
    manifest.validate().unwrap();
    assert_eq!(
        manifest.contributions.initially_withheld_actions,
        ["upload".to_owned(), "upload_keyless".to_owned()].into()
    );
    let host_manifest: Manifest = serde_json::from_value(json!({
        "manifest_version":1,"execution":{"kind":"process"},"capabilities":{},
        "imports":[{"provider":"upload","service":"upload.file","version":1,"methods":["choose","choose_keyless","choose_anonymous","configure"],"events":[]}],
        "exports":[
            {"service":"peersus.config","version":1,"description":"Effective test preference","consumers":["upload"],"methods":{"get":{"description":"Read"}}, "events":{}},
            {"service":"peersus.files","version":1,"description":"Real file authority","consumers":["upload"],"methods":{"selected":{"description":"Redeem"},"release":{"description":"Release"}},"events":{}},
            {"service":"peersus.http","version":1,"description":"Real HTTP authority","consumers":["upload"],"methods":{"configure":{"description":"Configure"},"saved":{"description":"Saved"},"check":{"description":"Check"},"authorize":{"description":"Consent"},"request":{"description":"Request"},"present":{"description":"Present"},"release":{"description":"Release"}},"events":{}},
            {"service":"peersus.actions","version":1,"description":"The owner's part, answered by this test","consumers":["upload"],"methods":{"withhold":{"description":"Withhold"}},"events":{}}
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
                    (HTTP_SERVICE.into(), 1, "saved".into()),
                    Duration::from_secs(600),
                ),
                (
                    (HTTP_SERVICE.into(), 1, "check".into()),
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
    let response_receipts = receipts.clone();
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
                method: if keyed { "choose" } else { "choose_keyless" }.into(),
                args: json!({"context":context}),
                timeout_ms: 40000,
            })
            .unwrap();
        let progress = platform.action_progress().unwrap();
        let total = payload.len() as u64;
        let mut observed = 0;
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
                    if service == "peersus.config" {
                        assert_eq!((method.as_str(), &args), ("get", &json!({})));
                        methods.push(format!("{service}/{method}"));
                        channel
                            .submit(Operation::Reply {
                                ticket,
                                result: Outcome::ok(
                                    json!({"settings":{"destination":"Choose on Send"}}),
                                ),
                            })
                            .unwrap();
                        continue;
                    }
                    if service == "peersus.actions" {
                        assert_eq!(
                            (method.as_str(), &args),
                            (
                                "withhold",
                                &json!({"actions":[if keyed { "upload_keyless" } else { "upload" }]})
                            )
                        );
                        methods.push(format!("{service}/{method}"));
                        channel
                            .submit(Operation::Reply {
                                ticket,
                                result: Outcome::ok(json!({})),
                            })
                            .unwrap();
                        continue;
                    }
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
                    if service == HTTP_SERVICE && method == "saved" {
                        assert_eq!(value, &json!({"saved":keyed}));
                    }
                    if service == HTTP_SERVICE && method == "check" {
                        assert!(keyed, "anonymous upload checked a key");
                        assert_eq!(value, &json!({"status":200}));
                    }
                    let wire = serde_json::to_string(&(args.clone(), value.clone())).unwrap();
                    for secret in [std::str::from_utf8(KEY).unwrap(), SESSION]
                        .into_iter()
                        .chain(response_receipts.iter().map(String::as_str))
                    {
                        assert!(
                            !wire.contains(secret),
                            "raw credential/session/receipt crossed guest boundary"
                        );
                    }
                    if service == FILES_SERVICE && method == "selected" {
                        assert_eq!(value["bytes"], payload.len());
                        assert_eq!(payload_progress(&progress.borrow()), Some((0, total)));
                        // Snapshot is created by selected, not by select(context).
                        std::fs::write(&source, b"changed and truncated after snapshot").unwrap();
                    }
                    if service == HTTP_SERVICE && method == "request" {
                        if let Some(offset) = args["part"]["offset"].as_u64() {
                            assert_eq!(observed, offset);
                            observed += args["part"]["length"].as_u64().unwrap();
                        } else if let Some(bytes) = args["multipart"]["length"].as_u64() {
                            observed += bytes;
                        }
                        assert_eq!(
                            payload_progress(&progress.borrow()),
                            Some((observed, total))
                        );
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
                                json!({"token":captures[0],"receipts":&captures[1..]})
                            );
                        }
                    }
                    if service == HTTP_SERVICE && method == "present" {
                        assert_eq!(observed, total);
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
        let requests = if chunked { parts + 2 } else { 1 };
        // Resolve the preference and offer once, revalidate the chosen credential mode before touching the file.
        let mut expected = vec![
            "peersus.config/get".to_owned(),
            format!("{HTTP_SERVICE}/saved"),
            "peersus.actions/withhold".to_owned(),
            format!("{HTTP_SERVICE}/saved"),
        ];
        if keyed {
            expected.push(format!("{HTTP_SERVICE}/check"));
        }
        expected.extend([
            format!("{FILES_SERVICE}/selected"),
            format!("{HTTP_SERVICE}/authorize"),
        ]);
        expected.extend((0..requests).map(|_| format!("{HTTP_SERVICE}/request")));
        expected.extend([
            format!("{HTTP_SERVICE}/present"),
            format!("{HTTP_SERVICE}/release"),
            format!("{FILES_SERVICE}/release"),
        ]);
        assert_eq!(methods, expected);
        assert_eq!(captures.len(), if chunked { parts + 1 } else { 0 });
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
    let mut requests = server.finish().await;
    if keyed {
        let check = requests.remove(0);
        assert_eq!(check.line, "GET /api/me HTTP/1.1");
        assert_eq!(
            check.headers["authorization"],
            format!("Bearer {}", std::str::from_utf8(KEY).unwrap())
        );
        assert_eq!(check.headers["host"], "poo.wang");
        assert!(check.body.is_empty());
    }
    let payload = driven
        .expect("upload pipeline timed out")
        .expect("compiled upload guest pipeline failed");
    assert_eq!(*presented.lock().unwrap(), vec![LINK.to_owned()]);
    // What the person was shown, in order: the consent for this file, this address and the header this extension named, then
    // the link; both under the extension, in the application's words.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for shown in seen.iter() {
        assert_eq!(
            shown.source,
            NotificationSource::Plugin(PluginId::new("upload").unwrap())
        );
    }
    let mut headers = vec!["x-upload-session".to_owned()];
    if !keyed {
        headers.push("x-poo-upload-client".to_owned());
    }
    assert_eq!(
        seen[0].notification,
        consent(
            "selected.bin",
            payload.len() as u64,
            &reqwest::Url::parse("https://poo.wang/").unwrap(),
            &headers,
            !keyed,
            None
        )
    );
    assert_eq!(seen[1].notification, result_link(LINK));
    drop(seen);
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
    assert!(
        observed_platform
            .action_progress()
            .unwrap()
            .borrow()
            .is_empty()
    );
    if chunked {
        assert_eq!(requests.len(), parts + 2);
        assert_eq!(requests[0].line, "POST /api/upload/session HTTP/1.1");
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[0].body).unwrap(),
            json!({"name":"selected.bin","type":"","size":payload.len(),"burnMode":"Burn after 24 hours","showcase":false})
        );
        for i in 0..parts {
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
            requests[parts + 1].line,
            "POST /api/upload/session/complete HTTP/1.1"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[parts + 1].body).unwrap(),
            json!({"token":SESSION,"receipts":receipts})
        );
        assert!(!requests[parts + 1].headers.contains_key("authorization"));
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
    if keyed {
        assert_eq!(
            requests[0].headers["authorization"],
            format!("Bearer {}", std::str::from_utf8(KEY).unwrap())
        );
        assert!(!requests[0].headers.contains_key("x-poo-upload-client"));
    } else {
        assert_eq!(requests[0].headers["x-poo-upload-client"], "native");
        assert!(requests.iter().all(|r| {
            ["authorization", "origin", "cookie"]
                .into_iter()
                .all(|header| !r.headers.contains_key(header))
        }));
    }
    assert!(
        requests[1..]
            .iter()
            .all(|r| !r.headers.contains_key("x-poo-upload-client"))
    );
    for request in requests {
        assert_eq!(request.headers["host"], "poo.wang");
    }
}

/// Where the anonymous destination puts its links, and what it answered in this run.
const ANONYMOUS_LINK: &str = "https://files.catbox.moe/fixture.png";

/// Catbox is the only destination in this run; any lookup of a key is a fixture failure.
struct AnonymousPipelineUi(Arc<Mutex<Vec<String>>>);
impl Interaction for AnonymousPipelineUi {
    fn choose(
        &self,
        _: PluginId,
        _: crate::platform_services::FileKind,
    ) -> UiResponse<(std::fs::File, String)> {
        Box::pin(async { Err("unexpected picker") })
    }
    fn configure(&self, _: PluginId, _: reqwest::Url) -> UiResponse<()> {
        Box::pin(async { Err("unexpected configuration") })
    }
    fn credential(&self, _: PluginId, _: reqwest::Url) -> CredentialResponse {
        Box::pin(async { panic!("Catbox-only mode should not look for a key") })
    }
    fn copy(&self, link: String) -> UiResponse<()> {
        let copied = self.0.clone();
        Box::pin(async move {
            copied.lock().unwrap().push(link);
            Ok(())
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires explicit PEERSUS_UPLOAD_WASM and PEERSUS_WASM_HOST built artifacts"]
async fn compiled_upload_wasm_catbox_anonymous_through_real_tls_single_request() {
    let module = artifact("PEERSUS_UPLOAD_WASM");
    let helper = artifact("PEERSUS_WASM_HOST");
    let server = Server::start_connect(
        vec![Some(response("200 OK", "", ANONYMOUS_LINK))],
        "catbox.moe:443",
    )
    .await;
    let directory = tempfile::Builder::new()
        .prefix("pwa-")
        .tempdir_in("/tmp")
        .unwrap();
    let snapshots = directory.path().join("snapshots");
    std::fs::create_dir(&snapshots).unwrap();
    let source = directory.path().join("selected.bin");
    std::fs::write(&source, SOURCE).unwrap();
    let presented = Arc::new(Mutex::new(Vec::new()));
    let (mut platform, seen) =
        FileHttpServices::agreed(snapshots, Arc::new(AnonymousPipelineUi(presented.clone())));
    platform.test_root = Some(server.root.clone());
    platform.test_proxy = Some(server.origin.parse().unwrap());
    let caller = PluginId::new("upload").unwrap();
    let context = platform
        .select_for_action(
            &caller,
            crate::platform_services::SelectedInput::Path(source.clone()),
            "upload_anonymous",
        )
        .unwrap();
    let manifest_bytes = std::fs::read(artifact("PEERSUS_UPLOAD_MANIFEST")).unwrap();
    let package = peersus_api::package::split(&manifest_bytes).unwrap();
    let manifest: Manifest = serde_json::from_slice(&package.manifest).unwrap();
    manifest.validate().unwrap();
    let host_manifest: Manifest = serde_json::from_value(json!({
        "manifest_version":1,"execution":{"kind":"process"},"capabilities":{},
        "imports":[{"provider":"upload","service":"upload.file","version":1,"methods":["choose","choose_anonymous","configure"],"events":[]}],
        "exports":[
            {"service":"peersus.config","version":1,"description":"Effective test preference","consumers":["upload"],"methods":{"get":{"description":"Read"}}, "events":{}},
            {"service":"peersus.files","version":1,"description":"Real file authority","consumers":["upload"],"methods":{"selected":{"description":"Redeem"},"release":{"description":"Release"}},"events":{}},
            {"service":"peersus.http","version":1,"description":"Real HTTP authority","consumers":["upload"],"methods":{"configure":{"description":"Configure"},"saved":{"description":"Saved"},"check":{"description":"Check"},"authorize":{"description":"Consent"},"request":{"description":"Request"},"present":{"description":"Present"},"release":{"description":"Release"}},"events":{}},
            {"service":"peersus.actions","version":1,"description":"The owner's part, answered by this test","consumers":["upload"],"methods":{"withhold":{"description":"Withhold"}},"events":{}}
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
                (FILES_SERVICE.into(), 1, "selected".into()),
                (FILES_SERVICE.into(), 1, "release".into()),
                (HTTP_SERVICE.into(), 1, "saved".into()),
                (HTTP_SERVICE.into(), 1, "authorize".into()),
                (HTTP_SERVICE.into(), 1, "request".into()),
                (HTTP_SERVICE.into(), 1, "present".into()),
                (HTTP_SERVICE.into(), 1, "release".into()),
            ]
            .into_iter()
            .map(|key| (key, Duration::from_secs(600)))
            .collect(),
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
                method: "choose_anonymous".into(),
                args: json!({"context":context}),
                timeout_ms: 40000,
            })
            .unwrap();
        let progress = platform.action_progress().unwrap();
        let total = SOURCE.len() as u64;
        let mut methods = Vec::new();
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
                    if service == "peersus.config" {
                        assert_eq!((method.as_str(), &args), ("get", &json!({})));
                        methods.push(format!("{service}/{method}"));
                        channel
                            .submit(Operation::Reply {
                                ticket,
                                result: Outcome::ok(json!({"settings":{"destination":"Catbox"}})),
                            })
                            .unwrap();
                        continue;
                    }
                    if service == "peersus.actions" {
                        assert_eq!(
                            (method.as_str(), &args),
                            (
                                "withhold",
                                &json!({"actions":["upload","upload_keyless","configure"]})
                            )
                        );
                        methods.push(format!("{service}/{method}"));
                        channel
                            .submit(Operation::Reply {
                                ticket,
                                result: Outcome::ok(json!({})),
                            })
                            .unwrap();
                        continue;
                    }
                    if service == HTTP_SERVICE && method == "authorize" {
                        // The one approval names the destination and the address its link will be on, and asks for no key.
                        assert_eq!(args["anonymous"], json!(true));
                        assert_eq!(args["result_origin"], json!("https://files.catbox.moe"));
                        assert_eq!(args["origin"], json!("https://catbox.moe"));
                        assert_eq!(args["headers"], json!([]));
                    }
                    if service == HTTP_SERVICE && method == "request" {
                        assert!(args.get("bearer").is_none(), "a bearer was asked for");
                        assert_eq!(args["response"], json!("text"));
                        assert_eq!(args["path"], json!("/user/api.php"));
                        assert_eq!(args["multipart"]["field"], json!("fileToUpload"));
                        assert_eq!(args["multipart"]["fields"], json!({"reqtype":"fileupload"}));
                    }
                    let result = platform
                        .dispatch(Call {
                            caller: PluginId::new(&caller).unwrap(),
                            service: service.clone(),
                            method: method.clone(),
                            args: args.clone(),
                        })
                        .await;
                    let Outcome::Ok { .. } = &result else {
                        panic!("platform operation failed: {service}/{method}: {result:?}")
                    };
                    if service == FILES_SERVICE && method == "selected" {
                        assert_eq!(payload_progress(&progress.borrow()), Some((0, total)));
                    }
                    if service == HTTP_SERVICE && method == "request" {
                        assert_eq!(payload_progress(&progress.borrow()), Some((total, total)));
                    }
                    methods.push(format!("{service}/{method}"));
                    channel.submit(Operation::Reply { ticket, result }).unwrap();
                }
                Delivery::Result { id: 1, result } => {
                    assert_eq!(
                        result,
                        Outcome::ok(json!({"status":"uploaded","url":ANONYMOUS_LINK}))
                    );
                    break;
                }
                other => panic!("unexpected delivery: {other:?}"),
            }
        }
        // Nothing of the keyed destination: no check of a key, no session, no part, and one file request.
        assert_eq!(
            methods,
            [
                "peersus.config/get".to_owned(),
                "peersus.actions/withhold".to_owned(),
                format!("{FILES_SERVICE}/selected"),
                format!("{HTTP_SERVICE}/authorize"),
                format!("{HTTP_SERVICE}/request"),
                format!("{HTTP_SERVICE}/present"),
                format!("{HTTP_SERVICE}/release"),
                format!("{FILES_SERVICE}/release"),
            ]
        );
    });
    let driven = match timeout(FLOW_WAIT, &mut driver).await {
        Ok(result) => Some(result),
        Err(_) => {
            driver.abort();
            let _ = driver.await;
            None
        }
    };
    timeout(Duration::from_secs(10), worker.shutdown())
        .await
        .expect("upload worker shutdown timed out");
    let mut requests = server.finish().await;
    driven
        .expect("anonymous pipeline timed out")
        .expect("compiled anonymous guest pipeline failed");
    assert_eq!(*presented.lock().unwrap(), vec![ANONYMOUS_LINK.to_owned()]);
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0].notification,
            consent(
                "selected.bin",
                SOURCE.len() as u64,
                &reqwest::Url::parse("https://catbox.moe/").unwrap(),
                &[],
                true,
                Some(&reqwest::Url::parse("https://files.catbox.moe").unwrap())
            )
        );
        assert_eq!(seen[1].notification, result_link(ANONYMOUS_LINK));
    }
    let state = observed_platform.state().unwrap();
    assert!(state.files.is_empty());
    assert!(
        state.authorizations.is_empty(),
        "release retained the authorization"
    );
    drop(state);
    assert!(
        observed_platform
            .action_progress()
            .unwrap()
            .borrow()
            .is_empty()
    );
    assert_eq!(requests.len(), 1, "one request for one file");
    let request = requests.remove(0);
    assert_eq!(request.line, "POST /user/api.php HTTP/1.1");
    assert_eq!(request.headers["host"], "catbox.moe");
    assert!(
        !request.headers.contains_key("authorization"),
        "the anonymous destination was sent a bearer"
    );
    let boundary = request.headers["content-type"]
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap();
    // The file part first, then the form field this destination reads its request type from, and nothing else.
    let mut expected = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"fileToUpload\"; filename=\"selected.bin\"\r\n\r\n"
    )
    .into_bytes();
    expected.extend_from_slice(SOURCE);
    expected.extend_from_slice(
        format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"reqtype\"\r\n\r\nfileupload\r\n--{boundary}--\r\n"
        )
        .as_bytes(),
    );
    assert_eq!(request.body, expected);
}
