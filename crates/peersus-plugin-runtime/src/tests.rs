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

/// A connection the other side refuses is an answer to the guest (-1), not a fault: a guest that controls another device lived
/// only until that device said no, and every plugin with it was restarted, over and over.
#[test]
fn a_connection_that_cannot_be_had_is_told_to_the_guest_and_does_not_end_it() {
    let module = wasm(
        r#"(module
        (import "peersus_v1" "data_open" (func $open (param i32 i64) (result i32)))
        (memory (export "memory") 1)
        (func (export "peersus_main") (result i32)
            i32.const 1 i64.const 5 call $open i32.const -1 i32.ne))"#,
    );
    run(&module, &mut Queue::default()).expect("the guest saw -1 and ended well");
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

/// The built guest at `path`, or what its absence means. A test that is skipped passes, and a guest nobody built is a guest
/// nobody checked, so where the quality gate demands them (`PEERSUS_REQUIRE_GUESTS=1`) a missing one FAILS.
///
/// Only the 32-bit guest is demanded: it is what is shipped, and the gate's toolchain builds it. The 64-bit one needs a nightly
/// toolchain that builds the standard library itself, which the gate does not install, so there its absence is a loud skip,
/// as it is on any machine that has not built them. It is still checked wherever it exists.
fn real_guest(
    path: &std::path::Path,
    target: &str,
    source: &str,
    directory: &str,
) -> Option<Vec<u8>> {
    let command = format!(
        "cargo build --release --manifest-path {source}/Cargo.toml --target {target} --target-dir {directory}"
    );
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(_)
            if std::env::var("PEERSUS_REQUIRE_GUESTS").as_deref() == Ok("1")
                && target == "wasm32-unknown-unknown" =>
        {
            panic!(
                "the {target} guest of {source} was not built, and this run requires it: build it with `{command}`"
            )
        }
        Err(_) => {
            eprintln!("SKIPPED {target}: build it first with `{command}`");
            None
        }
    }
}

/// Genuine compiler output for both widths, built from the clipboard guest.
///
/// The interpreter knows no plugin: nothing in its code names one. Its TESTS name two real guests because a proof that a real
/// module links and runs needs a real module, and these are the ones shipped. They are examples, chosen the way a test
/// chooses an input, not a list of what the interpreter supports: any module that obeys the ABI runs the same way.
#[test]
fn real_guests_in_both_widths_link_start_and_stop_when_the_application_goes_away() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/clipboard-guest");
    let mut checked = 0;
    for (target, width) in [
        ("wasm32-unknown-unknown", Bits::Thirty32),
        ("wasm64-unknown-unknown", Bits::Sixty64),
    ] {
        let path = root
            .join(target)
            .join("release/peersus_clipboard_wasm.wasm");
        let Some(bytes) = real_guest(
            &path,
            target,
            "crates/peersus-clipboard/wasm",
            "target/clipboard-guest",
        ) else {
            continue;
        };
        let engine = wasmi::Engine::default();
        let module = Module::new(&engine, &bytes[..]).unwrap();
        assert_eq!(memory_width(&module).unwrap(), width, "{target}");
        // With nothing to receive, a real guest reads end-of-input and returns: it links, starts and stops cleanly.
        let mut broker = Broker::default();
        assert!(
            run(&bytes, &mut broker).is_ok(),
            "{target} must link and run against the real imports"
        );
        // The first thing it says unasked is its single subscription to the application's clipboard change notice, in exactly
        // the form the hub accepts. Its startup pass may then ask what it is configured to do; with nobody to send to it must
        // never read the clipboard. A second subscription, or any read, is a regression.
        let subscription: serde_json::Value = serde_json::json!({
            "op":"subscribe","provider":"host","service":"peersus.clipboard","version":1,"event":"changed"
        });
        let said: Vec<serde_json::Value> = broker
            .sent
            .iter()
            .map(|bytes| serde_json::from_slice(bytes).unwrap())
            .collect();
        assert_eq!(said.first(), Some(&subscription), "{target}");
        for later in &said[1..] {
            assert_ne!(later["op"], "subscribe", "{target} subscribed twice");
            assert!(
                !matches!(later["method"].as_str(), Some("read" | "observe" | "chunk")),
                "{target} read the clipboard with nobody to send to: {later}"
            );
        }
        checked += 1;
    }
    eprintln!("checked {checked} real guest module(s)");
}

/// The same for the upload guest, which is a different plugin built the same way. Neither width may be an empty module,
/// which is what a guest compiled out by an architecture condition used to become. It says nothing until it is called.
#[test]
fn the_real_upload_guest_in_both_widths_links_starts_and_says_nothing_unasked() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/upload-guest");
    let mut checked = 0;
    for (target, width) in [
        ("wasm32-unknown-unknown", Bits::Thirty32),
        ("wasm64-unknown-unknown", Bits::Sixty64),
    ] {
        let path = root.join(target).join("release/peersus_upload_wasm.wasm");
        let Some(bytes) = real_guest(
            &path,
            target,
            "crates/peersus-upload/wasm",
            "target/upload-guest",
        ) else {
            continue;
        };
        assert!(
            bytes.len() > 1024,
            "{target} is {} bytes: a guest compiled out by a condition is an empty module",
            bytes.len()
        );
        let engine = wasmi::Engine::default();
        let module = Module::new(&engine, &bytes[..]).unwrap();
        assert_eq!(memory_width(&module).unwrap(), width, "{target}");
        let mut broker = Broker::default();
        assert!(
            run(&bytes, &mut broker).is_ok(),
            "{target} must link and run against the real imports"
        );
        assert!(
            broker.sent.is_empty(),
            "{target} said something before being called"
        );
        checked += 1;
    }
    eprintln!("checked {checked} real upload guest module(s)");
}

/// A transport that hands out several messages in order and then end-of-input, for a guest that answers more than one call.
#[derive(Default)]
struct Queue {
    incoming: std::collections::VecDeque<Vec<u8>>,
    sent: Vec<Vec<u8>>,
}

impl Transport for Queue {
    type Stream = io::Cursor<Vec<u8>>;

    fn receive(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let Some(message) = self.incoming.pop_front() else {
            return Ok(0);
        };
        into[..message.len()].copy_from_slice(&message);
        Ok(message.len())
    }

    fn send(&mut self, message: &[u8]) -> io::Result<()> {
        self.sent.push(message.to_vec());
        Ok(())
    }

    fn open_data(&mut self, _request: DataRequest) -> io::Result<Self::Stream> {
        Err(io::Error::other("the template opens no data stream"))
    }

    fn shutdown_data(&mut self, _stream: &mut Self::Stream) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no data stream"))
    }
}

/// Drives a guest named by `PEERSUS_PROBE_WASM` through the interpreter the application uses, with the calls in
/// `PEERSUS_PROBE_CALLS` (a JSON array of deliveries), and prints what it answered and how it ended. Run by hand to see
/// why a real guest stops: `PEERSUS_PROBE_WASM=… PEERSUS_PROBE_CALLS='[…]' cargo test -p peersus-plugin-runtime probe -- --ignored --nocapture`.
#[test]
#[ignore = "needs a guest named by PEERSUS_PROBE_WASM"]
fn probe_a_guest() {
    let bytes = std::fs::read(std::env::var("PEERSUS_PROBE_WASM").unwrap()).unwrap();
    let calls: Vec<serde_json::Value> =
        serde_json::from_str(&std::env::var("PEERSUS_PROBE_CALLS").unwrap()).unwrap();
    let mut queue = Queue {
        incoming: calls
            .iter()
            .map(|call| serde_json::to_vec(call).unwrap())
            .collect(),
        sent: Vec::new(),
    };
    let ended = run(&bytes, &mut queue);
    for sent in &queue.sent {
        println!(
            "sent {} bytes: {:.200}",
            sent.len(),
            String::from_utf8_lossy(sent)
        );
    }
    println!("ended: {ended:?}");
}

/// The template an author starts from, built and run through the interpreter the application uses. A template that stops
/// working is a broken promise to everyone who copies it, so it is checked like a shipped guest: it must say nothing until
/// called, answer the one method it offers, and refuse any other. Only the 32-bit module is demanded, as for the others.
#[test]
fn the_template_guest_answers_its_one_method_and_refuses_any_other() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/hello-guest/wasm32-unknown-unknown/release/hello_guest.wasm");
    let Some(bytes) = real_guest(
        &path,
        "wasm32-unknown-unknown",
        "templates/hello-guest",
        "target/hello-guest",
    ) else {
        return;
    };
    let call = |ticket: u64, method: &str| {
        serde_json::to_vec(&serde_json::json!({
            "type": "call", "ticket": ticket, "caller": "host",
            "service": "hello.greeting", "version": 1, "method": method, "args": {}
        }))
        .unwrap()
    };
    // Nothing is called: it starts, finds the application gone, stops, and has said nothing.
    let mut silent = Queue::default();
    assert!(
        run(&bytes, &mut silent).is_ok(),
        "the template must link and run against the real imports"
    );
    assert!(
        silent.sent.is_empty(),
        "a guest says nothing until it is called"
    );
    // A notice is not for it, a known method is answered, and an unknown one is refused, each by the ticket it came with.
    let mut queue = Queue::default();
    queue.incoming.extend([
        serde_json::to_vec(&serde_json::json!({"type": "event", "service": "x", "event": "y"}))
            .unwrap(),
        call(7, "greet"),
        call(8, "no-such-method"),
    ]);
    assert!(run(&bytes, &mut queue).is_ok());
    let replies: Vec<serde_json::Value> = queue
        .sent
        .iter()
        .map(|sent| serde_json::from_slice(sent).unwrap())
        .collect();
    assert_eq!(
        replies.len(),
        2,
        "one reply per call and none for the notice: {replies:?}"
    );
    assert_eq!(replies[0]["op"], "reply");
    assert_eq!(replies[0]["ticket"], 7);
    assert_eq!(replies[0]["result"]["status"], "ok");
    assert_eq!(replies[1]["ticket"], 8);
    assert_eq!(replies[1]["result"]["status"], "error");
}

/// The same exchange as the 32-bit one, from a module whose memory and pointers are 64-bit.
#[test]
fn a_64_bit_module_is_detected_from_its_own_memory_and_exchanges_control_and_data() {
    let module = wasm(
        r#"(module
        (import "peersus_v1" "receive" (func $receive (param i64 i64) (result i32)))
        (import "peersus_v1" "send" (func $send (param i64 i64) (result i32)))
        (import "peersus_v1" "data_open" (func $open (param i32 i64) (result i32)))
        (import "peersus_v1" "data_read" (func $read (param i32 i64 i64) (result i32)))
        (import "peersus_v1" "data_close" (func $close (param i32) (result i32)))
        (memory (export "memory") i64 1)
        (func (export "peersus_main") (result i32) (local $length i32) (local $stream i32)
            i64.const 0 i64.const 4096 call $receive local.set $length
            i64.const 0 local.get $length i64.extend_i32_u call $send drop
            i32.const 0 i64.const 17 call $open local.set $stream
            local.get $stream i64.const 4096 i64.const 1024 call $read local.set $length
            i64.const 4096 local.get $length i64.extend_i32_u call $send drop
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
fn the_width_comes_from_the_module_never_from_what_it_imports_or_claims() {
    for (memory, import, expected) in [
        ("(memory (export \"memory\") 1)", "i32 i32", true),
        ("(memory (export \"memory\") i64 1)", "i64 i64", true),
        // A 32-bit memory with 64-bit imports, and the other way round: neither links, and neither runs.
        ("(memory (export \"memory\") 1)", "i64 i64", false),
        ("(memory (export \"memory\") i64 1)", "i32 i32", false),
    ] {
        let module = wasm(&format!(
            r#"(module
            (import "peersus_v1" "send" (func $send (param {import}) (result i32)))
            {memory}
            (func (export "peersus_main") (result i32) i32.const 0))"#
        ));
        assert_eq!(
            run(&module, &mut Broker::default()).is_ok(),
            expected,
            "memory {memory} with imports ({import})"
        );
    }
}

#[test]
fn a_64_bit_module_gets_the_same_hostile_input_refusals_and_none_of_the_wrapping() {
    // Offsets and lengths that would be a valid small address if a 64-bit value were silently truncated.
    for (offset, length) in [
        (65530_i64, 16_i64),
        (-1, 1),
        (0, -1),
        (0, 65537),
        (0x1_0000_0000, 1),
        (0x7FFF_FFFF_FFFF_FFFF, 1),
        (i64::MIN, 1),
        (1, i64::MAX),
        // 2^32 + 16 would be 16 if the high half were dropped: it must be refused, not read.
        (0x1_0000_0010, 4),
    ] {
        let module = wasm(&format!(
            r#"(module
            (import "peersus_v1" "send" (func $send (param i64 i64) (result i32)))
            (memory (export "memory") i64 1)
            (func (export "peersus_main") (result i32) i64.const {offset} i64.const {length} call $send))"#
        ));
        let mut broker = Broker::default();
        assert!(
            run(&module, &mut broker).is_err(),
            "offset {offset} length {length} must be refused"
        );
        assert!(
            broker.sent.is_empty(),
            "an invalid 64-bit range reached the transport: {offset} {length}"
        );
    }
}

#[test]
fn a_64_bit_module_gets_no_more_memory_than_a_32_bit_one() {
    // Wide addressing is not more memory: the bound is the host's, and it is the same.
    for body in [
        r#"(module (memory (export "memory") i64 65536) (func (export "peersus_main") (result i32) i32.const 0))"#,
        r#"(module (memory (export "memory") i64 4294967296) (func (export "peersus_main") (result i32) i32.const 0))"#,
    ] {
        assert!(run(&wasm(body), &mut Broker::default()).is_err(), "{body}");
    }
    let grows = wasm(
        r#"(module (memory (export "memory") i64 1)
        (func (export "peersus_main") (result i32) i64.const 65535 memory.grow i64.const -1 i64.ne))"#,
    );
    assert!(
        run(&grows, &mut Broker::default()).is_ok(),
        "growth past the bound must fail without trapping the host"
    );
}

#[test]
fn a_module_without_a_memory_export_is_refused_whatever_its_width() {
    for body in [
        r#"(module (memory 1) (func (export "peersus_main") (result i32) i32.const 0))"#,
        r#"(module (memory i64 1) (func (export "peersus_main") (result i32) i32.const 0))"#,
    ] {
        assert!(run(&wasm(body), &mut Broker::default()).is_err(), "{body}");
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
    let message = peersus_api::message::Delivery::Call {
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
    let replies: Vec<peersus_api::message::Operation> = broker
        .sent
        .iter()
        .map(|reply| serde_json::from_slice(reply).unwrap())
        .collect();
    assert_eq!(
        replies,
        [peersus_api::message::Operation::Reply {
            ticket: 7,
            result: peersus_api::message::Outcome::ok(serde_json::json!("portable echo"))
        }]
    );
}
