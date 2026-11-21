#![cfg(unix)]
use peersus_api::{
    manifest::{EXECUTION_ABI_VERSION, Execution, Export, Import, Manifest, Member},
    message::{Delivery, Operation, Outcome},
};
use peersus_plugin::hub::{Channel, Hub};
use peersus_plugin::{
    Event, Plugin, Service,
    module::ModuleLauncher,
    supervisor::{Bootstrap, Launcher, Lifecycle, Policy},
};
use serde_json::json;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

struct Observed {
    launcher: ModuleLauncher,
    pids: Arc<Mutex<Vec<u32>>>,
}
impl Launcher for Observed {
    fn launch(&self, bootstrap: Bootstrap) -> std::io::Result<Box<dyn Lifecycle>> {
        let running = self.launcher.spawn(bootstrap)?;
        self.pids.lock().unwrap().push(running.id().unwrap());
        Ok(Box::new(running))
    }
}

fn names(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|s| (*s).into()).collect()
}
fn provider() -> Manifest {
    Manifest {
        capabilities: [("use".into(), "Use this test extension.".into())].into(),
        exports: vec![Export {
            service: "svc".into(),
            version: 1,
            description: "Process test.".into(),
            consumers: names(&["app"]),
            methods: [(
                "act".into(),
                Member {
                    capability: Some("use".into()),
                    description: "Run the action.".into(),
                },
            )]
            .into(),
            events: Default::default(),
        }],
        ..Manifest::new(Execution::Wasm {
            abi: EXECUTION_ABI_VERSION,
        })
    }
}
fn consumer() -> Manifest {
    Manifest {
        imports: ["one", "two"]
            .iter()
            .map(|id| Import {
                provider: (*id).into(),
                service: "svc".into(),
                version: 1,
                methods: names(&["act"]),
                events: Default::default(),
            })
            .collect(),
        ..Manifest::new(Execution::Process)
    }
}

fn fixture(data: bool) -> Vec<u8> {
    let prefix = r#"{"op":"reply","ticket":"#;
    let suffix = r#", "result":{"status":"ok","value":"actual wasm action"}}"#;
    let escaped = |value: &[u8]| {
        value
            .iter()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    let needle = u64::from_le_bytes(*b"\"ticket\"");
    let data_action = if data {
        "i32.const 0 local.get $ticket call $open local.set $stream
         local.get $stream i32.const 8192 i32.const 9 call $write drop
         local.get $stream call $shutdown i32.eqz if else i32.const 1 return end
         local.get $stream i32.const 8208 i32.const 1 call $read i32.eqz if else i32.const 1 return end
         local.get $stream call $close drop"
    } else {
        ""
    };
    wat::parse_str(format!(r#"(module
        (import "peersus_v1" "receive" (func $receive (param i32 i32) (result i32)))
        (import "peersus_v1" "send" (func $send (param i32 i32) (result i32)))
        (import "peersus_v1" "data_open" (func $open (param i32 i64) (result i32)))
        (import "peersus_v1" "data_write" (func $write (param i32 i32 i32) (result i32)))
        (import "peersus_v1" "data_read" (func $read (param i32 i32 i32) (result i32)))
        (import "peersus_v1" "data_shutdown" (func $shutdown (param i32) (result i32)))
        (import "peersus_v1" "data_close" (func $close (param i32) (result i32)))
        (memory (export "memory") 1)
        (data (i32.const 4096) "{}")
        (data (i32.const 6000) "{}")
        (data (i32.const 8192) "real data")
        (func (export "peersus_main") (result i32)
            (local $length i32) (local $at i32) (local $digit i32) (local $out i32) (local $ticket i64) (local $stream i32)
            (loop $again
                i32.const 0 i32.const 4096 call $receive local.tee $length i32.eqz if i32.const 0 return end
                i32.const 0 local.set $at
                (block $found (loop $scan
                    local.get $at i32.const 8 i32.add local.get $length i32.gt_u if i32.const 1 return end
                    local.get $at i64.load i64.const {needle} i64.eq br_if $found
                    local.get $at i32.const 1 i32.add local.set $at br $scan))
                local.get $at i32.const 9 i32.add local.set $at
                i64.const 0 local.set $ticket
                i32.const {prefix_length} local.set $out
                (block $digits_done (loop $digits
                    local.get $at i32.load8_u local.tee $digit i32.const 48 i32.lt_u br_if $digits_done
                    local.get $digit i32.const 57 i32.gt_u br_if $digits_done
                    local.get $out i32.const 4096 i32.add local.get $digit i32.store8
                    local.get $ticket i64.const 10 i64.mul local.get $digit i32.const 48 i32.sub i64.extend_i32_u i64.add local.set $ticket
                    local.get $out i32.const 1 i32.add local.set $out
                    local.get $at i32.const 1 i32.add local.set $at br $digits))
                {data_action}
                local.get $out i32.const 4096 i32.add i32.const 6000 i32.const {suffix_length} memory.copy
                i32.const 4096 local.get $out i32.const {suffix_length} i32.add call $send drop
                br $again)
            i32.const 0))"#, escaped(prefix.as_bytes()), escaped(suffix.as_bytes()), prefix_length=prefix.len(), suffix_length=suffix.len())).unwrap()
}

async fn connected(service: &Service) {
    let mut events = service.events();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if *events.borrow_and_update() == Event::Connected {
                return;
            }
            events.changed().await.unwrap();
        }
    })
    .await
    .expect("real worker did not authenticate");
}

fn start(
    hub: &Hub,
    id: &str,
    directory: &Path,
    data: bool,
    handoff: Option<tokio::sync::mpsc::Sender<peersus_plugin::Accepted>>,
) -> (Service, Arc<Mutex<Vec<u32>>>) {
    let module = directory.join(format!("{id}.wasm"));
    std::fs::write(&module, fixture(data)).unwrap();
    start_module(
        hub,
        id,
        directory,
        provider(),
        module,
        PathBuf::from(env!("CARGO_BIN_EXE_peersus-wasm-host")),
        handoff,
    )
}

fn start_module(
    hub: &Hub,
    id: &str,
    directory: &Path,
    manifest: Manifest,
    module: PathBuf,
    helper: PathBuf,
    handoff: Option<tokio::sync::mpsc::Sender<peersus_plugin::Accepted>>,
) -> (Service, Arc<Mutex<Vec<u32>>>) {
    let pids = Arc::new(Mutex::new(Vec::new()));
    let launcher = Observed {
        launcher: ModuleLauncher::new(helper, module.clone()).unwrap(),
        pids: pids.clone(),
    };
    let plugin = Plugin {
        id: id.into(),
        manifest,
        grants: Default::default(),
        program: Some(module),
        arguments: Vec::new(),
        directory: directory.to_owned(),
        policy: Policy {
            first_delay: Duration::from_millis(20),
            max_delay: Duration::from_millis(100),
            ..Policy::default()
        },
        handoff,
    };
    (
        Service::start_with_launcher(hub.clone(), plugin, Arc::new(launcher)).unwrap(),
        pids,
    )
}

fn compile_c(output: &Path, inputs: &[&str], shared: bool) {
    let mut command =
        std::process::Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()));
    command.current_dir(env!("CARGO_MANIFEST_DIR")).args([
        "-std=c11",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-O2",
        "-fstack-protector-strong",
    ]);
    // Match `cargo xtask native-host` on each platform.
    let apple = cfg!(target_os = "macos");
    if shared {
        command.args(["-fPIC", "-shared"]);
    } else {
        command.arg("-fPIE");
        if !apple {
            command.arg("-pie");
        }
    }
    command.args(inputs);
    if !apple {
        command.arg("-Wl,-z,relro,-z,now");
    }
    let output = command
        .args(["-ldl", "-o"])
        .arg(output)
        .output()
        .expect("native module tests require a C compiler");
    assert!(
        output.status.success(),
        "C SDK compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn start_native(
    hub: &Hub,
    directory: &Path,
    handoff: tokio::sync::mpsc::Sender<peersus_plugin::Accepted>,
) -> (Service, Arc<Mutex<Vec<u32>>>) {
    let helper = directory.join("native-host");
    let module = directory.join("native.so");
    compile_c(&helper, &["native/host.c"], false);
    compile_c(&module, &["tests/native_fixture.c"], true);
    let mut manifest = provider();
    manifest.execution = Execution::NativeLibrary {
        abi: EXECUTION_ABI_VERSION,
    };
    start_module(
        hub,
        "one",
        directory,
        manifest,
        module,
        helper,
        Some(handoff),
    )
}

fn call(app: &Channel, provider: &str, id: u64) -> Result<(), peersus_api::Error> {
    app.submit(Operation::Call {
        id,
        provider: provider.into(),
        service: "svc".into(),
        version: 1,
        method: "act".into(),
        args: json!(null),
        timeout_ms: 5000,
    })
}
async fn reply(app: &Channel, id: u64) {
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), app.receive())
            .await
            .unwrap()
            .unwrap(),
        Delivery::Result {
            id,
            result: Outcome::ok(json!("actual wasm action"))
        }
    );
}

async fn exchange_data(
    incoming: &mut tokio::sync::mpsc::Receiver<peersus_plugin::Accepted>,
    call_id: u64,
) {
    use tokio::io::AsyncReadExt;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut connection = incoming.recv().await.expect("data handoff closed");
        assert_eq!(connection.plugin, "one");
        let peersus_plugin::data::Right::Hub(right) = &connection.right else {
            panic!("a module must use the existing hub authority");
        };
        assert_eq!(right.caller(), "app");
        assert_eq!(right.call_id(), call_id);
        let mut bytes = Vec::new();
        connection.stream.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"real data");
        // Match the owner's receive/pump behavior: consume until the guest's write EOF,
        // then drop this connection. There is no extra application acknowledgment byte.
    })
    .await
    .expect("real bidirectional module data exchange timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_real_module_processes_obey_grants_exchange_data_and_restart_independently() {
    module_lifecycle(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_shared_library_and_wasm_keep_the_same_authority_and_independent_processes() {
    module_lifecycle(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_library_publishes_finished_frames_on_one_canvas_claim() {
    finished_canvas_frames(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn standalone_process_publishes_finished_frames_on_one_canvas_claim() {
    finished_canvas_frames(true).await;
}

async fn finished_canvas_frames(process: bool) {
    use peersus_api::canvas::{
        FRAME_PREFIX_BYTES, FrameHeader, FrameLayout, MAX_FRAME_HEADER_BYTES, OPEN, SERVICE,
    };
    use peersus_plugin::{
        data::{Right, canvas},
        hub::HOST,
        transport,
    };
    use tokio::io::AsyncReadExt;

    // The top bit is set on owner-issued canvas claims; the C ABI must not truncate it.
    const CLAIM: u64 = (1 << 63) | 42;
    let directory = tempfile::Builder::new()
        .prefix("pc")
        .tempdir_in("/tmp")
        .unwrap();
    let helper = directory.path().join("native-host");
    let program = directory.path().join(if process {
        "canvas-plugin"
    } else {
        "canvas.so"
    });
    let fixture = "tests/native_canvas_fixture.c";
    if process {
        compile_c(
            &program,
            &["-DPEERSUS_LINKED_MODULE", "native/host.c", fixture],
            false,
        );
    } else {
        compile_c(&helper, &["native/host.c"], false);
        compile_c(&program, &[fixture], true);
    }

    let hub = Hub::default();
    let generation = hub
        .register_host(
            HOST,
            Manifest {
                exports: vec![Export {
                    service: SERVICE.into(),
                    version: 1,
                    description: "Canvas claim fixture.".into(),
                    consumers: names(&["canvas-fixture"]),
                    methods: [(
                        OPEN.into(),
                        Member {
                            capability: None,
                            description: "Open a canvas claim.".into(),
                        },
                    )]
                    .into(),
                    events: Default::default(),
                }],
                ..Manifest::new(Execution::Process)
            },
            Default::default(),
        )
        .unwrap();
    let host = hub.connect(HOST, generation).unwrap();
    let (handoff, mut incoming) = tokio::sync::mpsc::channel(1);
    let plugin = Plugin {
        id: "canvas-fixture".into(),
        manifest: Manifest {
            imports: vec![Import {
                provider: HOST.into(),
                service: SERVICE.into(),
                version: 1,
                methods: names(&[OPEN]),
                events: Default::default(),
            }],
            ..Manifest::new(if process {
                Execution::Process
            } else {
                Execution::NativeLibrary {
                    abi: EXECUTION_ABI_VERSION,
                }
            })
        },
        grants: Default::default(),
        program: Some(program.clone()),
        arguments: Vec::new(),
        directory: directory.path().to_owned(),
        policy: Policy::default(),
        handoff: Some(handoff),
    };
    let worker = if process {
        Service::start(hub, plugin)
    } else {
        let launcher = ModuleLauncher::new(helper, program).unwrap();
        Service::start_with_launcher(hub, plugin, Arc::new(launcher))
    }
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        let Delivery::Call {
            ticket,
            caller,
            service,
            version,
            method,
            args,
        } = host.receive().await.unwrap()
        else {
            panic!("the extension did not ask for a canvas claim");
        };
        assert_eq!(
            (caller.as_str(), service.as_str(), version, method.as_str()),
            ("canvas-fixture", SERVICE, 1, OPEN)
        );
        assert_eq!(args, json!({"page":"demo","canvas":"map"}));
        host.submit(Operation::Reply {
            ticket,
            result: Outcome::ok(json!({"claim": CLAIM})),
        })
        .unwrap();
        let mut accepted = incoming.recv().await.unwrap();
        assert_eq!(accepted.plugin, "canvas-fixture");
        assert!(matches!(accepted.right, Right::Claim(CLAIM)));
        let listener = transport::Listener::bind(&directory.path().join("window")).unwrap();
        let token = [5; canvas::TOKEN_BYTES];
        let redirect =
            canvas::Redirect::new(listener.endpoint().to_str().unwrap().into(), token).unwrap();
        redirect.write(&mut accepted.stream).await.unwrap();
        drop(accepted);
        let mut direct = listener.accept().await.unwrap();
        canvas::authenticate(&mut direct, &token).await.unwrap();
        for (seq, color) in [(1, [20u8, 40, 60, 255]), (2, [20, 0, 10, 128])] {
            let mut wire = [0; MAX_FRAME_HEADER_BYTES];
            let (prefix, rest) = wire.split_first_chunk_mut::<FRAME_PREFIX_BYTES>().unwrap();
            direct.read_exact(prefix).await.unwrap();
            let end = FrameHeader::encoded_len(prefix).unwrap();
            direct
                .read_exact(&mut rest[..end - FRAME_PREFIX_BYTES])
                .await
                .unwrap();
            let header = FrameHeader::decode(&wire[..end]).unwrap();
            assert_eq!(
                header,
                FrameHeader {
                    id: "map",
                    layout: FrameLayout::tight(256, 65).unwrap(),
                    seq,
                    settled: seq + 41,
                }
            );
            let mut pixels = vec![0; header.layout.byte_len()];
            direct.read_exact(&mut pixels).await.unwrap();
            assert!(
                pixels
                    .chunks_exact(4)
                    .all(|pixel| pixel == color.as_slice()),
                "unexpected raster bytes for frame {seq}"
            );
        }
        assert_eq!(direct.read(&mut [0]).await.unwrap(), 0);
        assert!(
            incoming.try_recv().is_err(),
            "a second data stream was opened"
        );
        worker.shutdown().await;
    })
    .await
    .expect("native canvas claim or frames did not arrive");
}

async fn module_lifecycle(native: bool) {
    let directory = tempfile::Builder::new()
        .prefix("pw")
        .tempdir_in("/tmp")
        .unwrap();
    let hub = Hub::default();
    let generation = hub.register("app", consumer(), Default::default()).unwrap();
    let app = hub.connect("app", generation).unwrap();
    let (handoff, mut incoming) = tokio::sync::mpsc::channel(8);
    let (one, pids_one) = if native {
        start_native(&hub, directory.path(), handoff)
    } else {
        start(&hub, "one", directory.path(), true, Some(handoff))
    };
    let (two, pids_two) = start(&hub, "two", directory.path(), false, None);
    connected(&one).await;
    connected(&two).await;
    assert!(
        call(&app, "one", 1).is_err(),
        "no grant must remain denied for interpreted code"
    );
    hub.set_permissions(
        "app",
        [
            peersus_api::permission::Permission::new("one", "use").unwrap(),
            peersus_api::permission::Permission::new("two", "use").unwrap(),
        ]
        .into(),
    )
    .unwrap();
    let first = pids_one.lock().unwrap()[0];
    let sibling = pids_two.lock().unwrap()[0];
    assert_ne!(first, sibling);
    assert_ne!(first, std::process::id());
    call(&app, "one", 2).unwrap();
    exchange_data(&mut incoming, 2).await;
    reply(&app, 2).await;
    call(&app, "two", 3).unwrap();
    reply(&app, 3).await;
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(first as i32).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while pids_one.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    connected(&one).await;
    assert_eq!(pids_two.lock().unwrap().as_slice(), &[sibling]);
    call(&app, "two", 4).unwrap();
    reply(&app, 4).await;
    call(&app, "one", 5).unwrap();
    exchange_data(&mut incoming, 5).await;
    reply(&app, 5).await;
    let restarted = pids_one.lock().unwrap()[1];
    assert_ne!(first, restarted);
    tokio::time::timeout(Duration::from_secs(10), one.shutdown())
        .await
        .expect("first module did not stop");
    tokio::time::timeout(Duration::from_secs(10), two.shutdown())
        .await
        .expect("second module did not stop");
    for pid in [first, sibling, restarted] {
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "a worker survived shutdown"
        );
    }
}

/// Runs the compiled production guest through the real authenticated worker/hub ABI.
/// Picker, consent and provider replies are synthetic: this never performs an upload.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires PEERSUS_UPLOAD_WASM pointing to the cross-built upload guest"]
async fn real_upload_guest_uses_host_services_and_returns_the_verified_link() {
    let module =
        PathBuf::from(std::env::var_os("PEERSUS_UPLOAD_WASM").expect("cross-built guest path"));
    let manifest_bytes = std::fs::read(
        std::env::var_os("PEERSUS_UPLOAD_MANIFEST").expect("explicit upload manifest required"),
    )
    .unwrap();
    let package = peersus_api::package::split(&manifest_bytes).unwrap();
    let manifest: Manifest = serde_json::from_slice(&package.manifest).unwrap();
    manifest.validate().unwrap();
    let host_manifest: Manifest = serde_json::from_value(json!({
        "manifest_version":1,"execution":{"kind":"process"}, "capabilities":{},
        "imports":[{"provider":"upload","service":"upload.file","version":1,"methods":["choose","configure"],"events":[]}],
        "exports":[
            {"service":"peersus.files","version":1,"description":"Synthetic selection","consumers":["upload"],"methods":{"selected":{"description":"Redeem selection"},"release":{"description":"Release"}},"events":{}},
            {"service":"peersus.http","version":1,"description":"Synthetic HTTPS","consumers":["upload"],"methods":{"configure":{"description":"Configure"},"saved":{"description":"Saved"},"check":{"description":"Check"},"authorize":{"description":"Consent"},"request":{"description":"Request"},"present":{"description":"Present validated result"},"release":{"description":"Release"}},"events":{}},
            {"service":"peersus.actions","version":1,"description":"Synthetic offers","consumers":["upload"],"methods":{"withhold":{"description":"Withhold"}},"events":{}}
        ]
    })).unwrap();
    let hub = Hub::default();
    let generation = hub
        .register_host("host", host_manifest, Default::default())
        .unwrap();
    let host = hub.connect("host", generation).unwrap();
    host.set_host_method_timeouts(
        [
            (
                ("peersus.http".into(), 1, "configure".into()),
                Duration::from_secs(600),
            ),
            (
                ("peersus.http".into(), 1, "saved".into()),
                Duration::from_secs(600),
            ),
            (
                ("peersus.http".into(), 1, "check".into()),
                Duration::from_secs(600),
            ),
            (
                ("peersus.files".into(), 1, "selected".into()),
                Duration::from_secs(600),
            ),
            (
                ("peersus.files".into(), 1, "release".into()),
                Duration::from_secs(600),
            ),
            (
                ("peersus.http".into(), 1, "authorize".into()),
                Duration::from_secs(600),
            ),
            (
                ("peersus.http".into(), 1, "request".into()),
                Duration::from_secs(600),
            ),
            (
                ("peersus.http".into(), 1, "present".into()),
                Duration::from_secs(600),
            ),
            (
                ("peersus.http".into(), 1, "release".into()),
                Duration::from_secs(600),
            ),
        ]
        .into(),
    )
    .unwrap();
    let directory = tempfile::Builder::new()
        .prefix("upload-guest-")
        .tempdir_in("/tmp")
        .unwrap();
    let (service, _) = start_module(
        &hub,
        "upload",
        directory.path(),
        manifest,
        module,
        PathBuf::from(env!("CARGO_BIN_EXE_peersus-wasm-host")),
        None,
    );
    connected(&service).await;
    host.submit(Operation::Call {
        id: 1,
        provider: "upload".into(),
        service: "upload.file".into(),
        version: 1,
        method: "choose".into(),
        args: json!({"context":42}),
        timeout_ms: 20000,
    })
    .unwrap();
    let mut calls = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match host.receive().await.unwrap() {
                Delivery::Call { ticket, caller, service, method, args, .. } => {
                    assert_eq!(caller, "upload");
                    let value = match (service.as_str(), method.as_str()) {
                        // Asked when it starts and again when the upload begins: a key is saved, so nothing is taken back.
                        ("peersus.http", "saved") => {
                            assert_eq!(args, json!({"origin":"https://poo.wang"}));
                            json!({"saved":true})
                        }
                        ("peersus.actions", "withhold") => {
                            assert_eq!(args, json!({"actions":[]}));
                            json!({})
                        }
                        // The provider is asked about the key before the file is.
                        ("peersus.http", "check") => {
                            assert_eq!(args, json!({"origin":"https://poo.wang","path":"/api/me"}));
                            json!({"status":200})
                        }
                        ("peersus.files", "selected") => {
                            assert_eq!(args["context"], 42);
                            json!({"file":"selected","name":"fixture.txt","bytes":7})
                        }
                        ("peersus.http", "authorize") => {
                            assert_eq!(args, json!({"file":"selected","origin":"https://poo.wang","headers":["x-upload-session"]}));
                            json!({"authorization":"approved"})
                        }
                        ("peersus.http", "request") => {
                            assert_eq!(args["authorization"], "approved");
                            assert_eq!(args["multipart"]["field"], "files");
                            assert_eq!(args["multipart"]["length"], 7);
                            assert!(args.get("present_link").is_none());
                            json!({"status":200,"json":{"success":true,"files":[{"id":"fixture","url":"https://poo.wang/f/fixture"}]}})
                        }
                        ("peersus.http", "present") => {
                            assert_eq!(args, json!({"authorization":"approved","link":"https://poo.wang/f/fixture"}));
                            json!({"presented":true})
                        }
                        ("peersus.http", "release") => {
                            assert_eq!(args["authorization"], "approved");
                            json!({"released":true})
                        }
                        ("peersus.files", "release") => {
                            assert_eq!(args["file"], "selected");
                            json!({"released":true})
                        }
                        _ => panic!("unexpected guest operation"),
                    };
                    calls.push((service, method));
                    host.submit(Operation::Reply { ticket, result:Outcome::ok(value) }).unwrap();
                }
                Delivery::Result { id:1, result } => {
                    assert_eq!(result, Outcome::ok(json!({"status":"uploaded","url":"https://poo.wang/f/fixture"})));
                    break;
                }
                _ => panic!("unexpected broker delivery"),
            }
        }
    }).await.expect("real upload guest did not finish");
    assert_eq!(
        calls
            .iter()
            .map(|(_, method)| method.as_str())
            .collect::<Vec<_>>(),
        [
            "saved",
            "withhold",
            "saved",
            "withhold",
            "check",
            "selected",
            "authorize",
            "request",
            "present",
            "release",
            "release"
        ]
    );
    host.submit(Operation::Call {
        id: 2,
        provider: "upload".into(),
        service: "upload.file".into(),
        version: 1,
        method: "configure".into(),
        args: json!({}),
        timeout_ms: 20000,
    })
    .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        match host.receive().await.unwrap() {
            Delivery::Call {
                ticket,
                service,
                method,
                ..
            } => {
                assert_eq!(
                    (service.as_str(), method.as_str()),
                    ("peersus.http", "configure")
                );
                host.submit(Operation::Reply {
                    ticket,
                    result: Outcome::error("cancelled", "Cancelled"),
                })
                .unwrap();
            }
            _ => panic!("configuration selected a file or performed an unexpected operation"),
        }
        match host.receive().await.unwrap() {
            Delivery::Result { id: 2, result } => {
                assert_eq!(result, Outcome::error("cancelled", "Upload cancelled"));
            }
            _ => panic!("configuration cancellation was not returned to the caller"),
        }
    })
    .await
    .expect("configuration cancellation did not finish");
    tokio::time::timeout(Duration::from_secs(10), service.shutdown())
        .await
        .expect("upload worker did not stop");
}
