//! Real interpreter calls and subprocess output: no global subscriber shared by tests.
use peersus_plugin_runtime::{DataRequest, Transport, run};
use std::{
    io::{self, Cursor},
    process::Command,
};

struct Offline;
impl Transport for Offline {
    type Stream = Cursor<Vec<u8>>;
    fn receive(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
    fn send(&mut self, _: &[u8]) -> io::Result<()> {
        panic!("diagnostics must not send control messages")
    }
    fn open_data(&mut self, _: DataRequest) -> io::Result<Self::Stream> {
        panic!("diagnostics must not open data streams")
    }
    fn shutdown_data(&mut self, _: &mut Self::Stream) -> io::Result<()> {
        panic!("diagnostics must not touch streams")
    }
}

fn module(body: &str) -> Vec<u8> {
    wat::parse_str(format!(r#"(module
        (import "peersus_v1" "diagnostic" (func $diagnostic (param i32 i32 i64 i64 i64) (result i32)))
        (memory (export "memory") 1)
        (func (export "peersus_main") (result i32) {body} (i32.const 0)))"#)).unwrap()
}

fn call(stage: i32, outcome: i32, correlation: i64, actual: i64, expected: i64) -> String {
    format!(
        "(call $diagnostic (i32.const {stage}) (i32.const {outcome}) (i64.const {correlation}) (i64.const {actual}) (i64.const {expected}))"
    )
}

/// A stage and an outcome no vocabulary has. Nine digits, so that the search for it in the child's log cannot be
/// answered by a timestamp (six digits of a second at most) or a process number (seven), as a shorter one was.
const UNKNOWN: i32 = 987_654_321;

#[test]
fn diagnostic_child() {
    if std::env::var_os("PEERSUS_GUEST_DIAGNOSTIC_CHILD").is_none() {
        return;
    }
    let _guard = peersus_diagnostics::init("test").unwrap();
    let accepted = if peersus_diagnostics::DEVELOPMENT {
        0
    } else {
        1
    };
    let mut body = format!(
        "(if (i32.ne {} (i32.const {accepted})) (then unreachable))",
        call(4, 6, 7, 12, 16)
    );
    // Unknown vocabulary and impossible counters are refused without guest text or a trap.
    for args in [
        (UNKNOWN, 0, 7, 0, 0),
        (4, UNKNOWN, 7, 0, 0),
        (4, 0, -1, 0, 0),
        (4, 0, 7, -1, 0),
        (4, 0, 7, 0, i64::MAX),
    ] {
        body.push_str(&format!(
            "(if (i32.ne {} (i32.const -1)) (then unreachable))",
            call(args.0, args.1, args.2, args.3, args.4)
        ));
    }
    run(&module(&body), &mut Offline).unwrap();
}

#[test]
fn guest_decisions_are_closed_and_disabled_in_production() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "diagnostic_child", "--nocapture"])
        .env("PEERSUS_GUEST_DIAGNOSTIC_CHILD", "1")
        .env("PEERSUS_LOG", "trace")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr.contains("Guest decision"),
        peersus_diagnostics::DEVELOPMENT
    );
    if peersus_diagnostics::DEVELOPMENT {
        assert!(stderr.contains("guest_stage=\"acknowledge\""));
        assert!(stderr.contains("outcome=\"count-mismatch\""));
        assert!(stderr.contains("correlation=7"));
        assert!(stderr.contains("actual=12"));
        assert!(stderr.contains("expected=16"));
        assert_eq!(stderr.matches("Guest decision").count(), 1);
    }
    assert!(!stderr.contains(&UNKNOWN.to_string()));
}

#[test]
fn guest_diagnostic_spam_is_refused_without_renewing_compute_fuel() {
    let expected = if peersus_diagnostics::DEVELOPMENT {
        -2
    } else {
        1
    };
    let mut body = String::new();
    // One second permits a 64-event burst; diagnostics never replenish compute fuel.
    for _ in 0..64 {
        body.push_str(&format!("(drop {})", call(1, 0, 0, 0, 0)));
    }
    body.push_str(&format!(
        "(if (i32.ne {} (i32.const {expected})) (then unreachable))",
        call(1, 0, 0, 0, 0)
    ));
    run(&module(&body), &mut Offline).unwrap();
    let looping = module(&format!(
        "(loop $again (drop {}) (br $again))",
        call(1, 0, 0, 0, 0)
    ));
    let error = run(&looping, &mut Offline).unwrap_err();
    assert!(
        error.as_trap_code().is_some(),
        "diagnostic spam must exhaust interpreter fuel"
    );
}
