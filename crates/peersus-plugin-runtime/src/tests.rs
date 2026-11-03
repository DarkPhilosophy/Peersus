use super::*;
use std::io;

#[derive(Default)]
struct Broker {
    incoming: Vec<u8>,
    sent: Vec<Vec<u8>>,
    streams: Vec<Vec<u8>>,
    requests: Vec<DataRequest>,
}

impl Transport for Broker {
    type Stream = io::Cursor<Vec<u8>>;

    fn receive(&mut self, into: &mut [u8]) -> io::Result<usize> {
        if self.incoming.is_empty() {
            return Ok(0);
        }
        let message = std::mem::take(&mut self.incoming);
        into[..message.len()].copy_from_slice(&message);
        Ok(message.len())
    }

    fn send(&mut self, message: &[u8]) -> io::Result<()> {
        self.sent.push(message.to_vec());
        Ok(())
    }

    fn open_data(&mut self, request: DataRequest) -> io::Result<Self::Stream> {
        self.requests.push(request);
        self.streams
            .pop()
            .map(io::Cursor::new)
            .ok_or_else(|| io::Error::other("no data stream"))
    }

    fn shutdown_data(&mut self, _stream: &mut Self::Stream) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "an in-memory test cursor is not a socket",
        ))
    }
}

fn wasm(body: &str) -> Vec<u8> {
    wat::parse_str(body).unwrap()
}

#[test]
fn an_actual_module_exchanges_control_and_data_through_only_the_broker() {
    let module = wasm(
        r#"(module
        (import "peersus_v1" "receive" (func $receive (param i32 i32) (result i32)))
        (import "peersus_v1" "send" (func $send (param i32 i32) (result i32)))
        (import "peersus_v1" "data_open" (func $open (param i32 i64) (result i32)))
        (import "peersus_v1" "data_read" (func $read (param i32 i32 i32) (result i32)))
        (import "peersus_v1" "data_close" (func $close (param i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "peersus_main") (result i32) (local $length i32) (local $stream i32)
            i32.const 0 i32.const 4096 call $receive local.set $length
            i32.const 0 local.get $length call $send drop
            i32.const 0 i64.const 17 call $open local.set $stream
            local.get $stream i32.const 4096 i32.const 1024 call $read local.set $length
            i32.const 4096 local.get $length call $send drop
            local.get $stream call $close drop
            i32.const 0))"#,
    );
    let mut broker = Broker {
        incoming: b"{\"call\":1}".to_vec(),
        streams: vec![b"actual bulk bytes".to_vec()],
        ..Broker::default()
    };
    run(&module, &mut broker).unwrap();
    assert_eq!(
        broker.sent,
        [b"{\"call\":1}".to_vec(), b"actual bulk bytes".to_vec()]
    );
    assert_eq!(broker.requests, [DataRequest::Ticket(17)]);
}

#[test]
fn an_infinite_guest_loop_exhausts_fuel_instead_of_hanging_the_host() {
    let module = wasm(
        "(module (memory (export \"memory\") 1) (func (export \"peersus_main\") (result i32) (loop $spin br $spin) i32.const 0))",
    );
    assert!(run(&module, &mut Broker::default()).is_err());
}

#[test]
fn eof_does_not_refill_a_busy_guests_fuel() {
    let module = wasm(
        r#"(module
        (import "peersus_v1" "receive" (func $receive (param i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "peersus_main") (result i32)
            (loop $spin i32.const 0 i32.const 10 call $receive drop br $spin) i32.const 0))"#,
    );
    assert!(run(&module, &mut Broker::default()).is_err());
}

#[test]
fn host_memory_ranges_and_negative_lengths_are_never_trusted() {
    for (offset, length) in [(65530, 16), (-1, 1), (0, -1), (0, 65537)] {
        let module = wasm(&format!(
            r#"(module
            (import "peersus_v1" "send" (func $send (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (func (export "peersus_main") (result i32) i32.const {offset} i32.const {length} call $send))"#
        ));
        let mut broker = Broker::default();
        assert!(run(&module, &mut broker).is_err());
        assert!(
            broker.sent.is_empty(),
            "invalid memory must never reach the transport"
        );
    }
}

#[test]
fn ambient_os_imports_and_start_functions_are_refused() {
    for body in [
        r#"(module (import "wasi_snapshot_preview1" "fd_write" (func)) (memory (export "memory") 1) (func (export "peersus_main") (result i32) i32.const 0))"#,
        r#"(module (func $start) (start $start) (memory (export "memory") 1) (func (export "peersus_main") (result i32) i32.const 0))"#,
    ] {
        assert!(run(&wasm(body), &mut Broker::default()).is_err());
    }
}

#[test]
fn memory_growth_and_initial_memory_are_bounded() {
    for body in [
        r#"(module (memory (export "memory") 65536) (func (export "peersus_main") (result i32) i32.const 0))"#,
        r#"(module (memory (export "memory") 1) (func (export "peersus_main") (result i32) i32.const 65535 memory.grow i32.const -1 i32.ne))"#,
    ] {
        let result = run(&wasm(body), &mut Broker::default());
        if body.contains("memory.grow") {
            assert!(
                result.is_ok(),
                "growth must fail without exceeding the memory bound"
            );
        } else {
            assert!(result.is_err());
        }
    }
}

#[test]
fn leaked_data_handles_are_bounded_and_closed_handles_cannot_be_reused() {
    let module = wasm(
        r#"(module
        (import "peersus_v1" "data_open" (func $open (param i32 i64) (result i32)))
        (import "peersus_v1" "data_close" (func $close (param i32) (result i32)))
        (import "peersus_v1" "data_read" (func $read (param i32 i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "peersus_main") (result i32) (local $first i32)
            i32.const 0 i64.const 1 call $open local.set $first
            local.get $first call $close drop
            i32.const 1 i64.const 2 call $open drop
            local.get $first i32.const 0 i32.const 1 call $read))"#,
    );
    let mut broker = Broker {
        streams: vec![vec![1], vec![2]],
        ..Broker::default()
    };
    assert!(run(&module, &mut broker).is_err());
}

#[test]
#[ignore = "requires clang with wasm32 linker; run explicitly when validating the portable SDK"]
fn the_freestanding_c_sdk_example_answers_an_actual_broker_delivery() {
    let directory = tempfile::tempdir().unwrap();
    let module = directory.path().join("echo.wasm");
    let compiled = std::process::Command::new("clang")
        .args([
            "--target=wasm32",
            "-std=c11",
            "-O2",
            "-ffreestanding",
            "-fno-builtin",
            "-nostdlib",
            "-Wl,--no-entry",
            "-Wl,--export-memory",
            "-Wl,--max-memory=67108864",
        ])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sdk/echo.c"))
        .arg("-o")
        .arg(&module)
        .output()
        .expect("clang is required for the SDK test");
    assert!(
        compiled.status.success(),
        "C SDK compilation failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let message = peersus_plugin_api::message::Delivery::Call {
        ticket: 7,
        caller: "host".into(),
        service: "echo".into(),
        version: 1,
        method: "echo".into(),
        args: serde_json::Value::Null,
    };
    let mut broker = Broker {
        incoming: serde_json::to_vec(&message).unwrap(),
        ..Broker::default()
    };
    run(&std::fs::read(&module).unwrap(), &mut broker).unwrap();
    let replies: Vec<peersus_plugin_api::message::Operation> = broker
        .sent
        .iter()
        .map(|reply| serde_json::from_slice(reply).unwrap())
        .collect();
    assert_eq!(
        replies,
        [peersus_plugin_api::message::Operation::Reply {
            ticket: 7,
            result: peersus_plugin_api::message::Outcome::ok(serde_json::json!("portable echo"))
        }]
    );
}
