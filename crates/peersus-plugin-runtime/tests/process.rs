#![cfg(unix)]
use peersus_plugin_api::{
    hub::{Channel, Hub},
    manifest::{EXECUTION_ABI_VERSION, Execution, Export, Import, Manifest, Member},
    message::{Delivery, Operation, Outcome},
};
use peersus_plugin_host::{
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
        execution: Execution::Wasm {
            abi: EXECUTION_ABI_VERSION,
        },
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
        ..Manifest::default()
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
        ..Manifest::default()
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
    handoff: Option<tokio::sync::mpsc::Sender<peersus_plugin_host::Accepted>>,
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
    handoff: Option<tokio::sync::mpsc::Sender<peersus_plugin_host::Accepted>>,
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

fn start_native(
    hub: &Hub,
    directory: &Path,
    handoff: tokio::sync::mpsc::Sender<peersus_plugin_host::Accepted>,
) -> (Service, Arc<Mutex<Vec<u32>>>) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let helper = directory.join("native-host");
    let module = directory.join("native.so");
    let compile = |input: &Path, output: &Path, shared: bool| {
        let mut command =
            std::process::Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()));
        command.args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-O2",
            "-fstack-protector-strong",
        ]);
        if shared {
            command.args(["-fPIC", "-shared"]);
        } else {
            command.args(["-fPIE", "-pie"]);
        }
        let output = command
            .arg(input)
            .args(["-Wl,-z,relro,-z,now", "-ldl", "-o"])
            .arg(output)
            .output()
            .expect("native module tests require a C compiler");
        assert!(
            output.status.success(),
            "C SDK compilation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    compile(&source.join("native/host.c"), &helper, false);
    compile(&source.join("tests/native_fixture.c"), &module, true);
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

fn call(app: &Channel, provider: &str, id: u64) -> Result<(), peersus_plugin_api::Error> {
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
    incoming: &mut tokio::sync::mpsc::Receiver<peersus_plugin_host::Accepted>,
    call_id: u64,
) {
    use tokio::io::AsyncReadExt;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut connection = incoming.recv().await.expect("data handoff closed");
        assert_eq!(connection.plugin, "one");
        let peersus_plugin_host::data::Right::Hub(right) = &connection.right else {
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
            peersus_plugin_api::permission::Permission::new("one", "use").unwrap(),
            peersus_plugin_api::permission::Permission::new("two", "use").unwrap(),
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
