//! Separate processes isolate the global subscriber and environment.
use std::process::Command;

fn run(level: &str) -> std::process::Output {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "diagnostic_child", "--nocapture"])
        .env("PEERSUS_DIAGNOSTIC_CHILD", "1")
        .env("PEERSUS_LOG", level)
        .output()
        .unwrap()
}

/// Without `PEERSUS_LOG` the child takes the build's own defaults: reviewed
/// development events on a development build, warnings/errors elsewhere.
fn run_unset() -> std::process::Output {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "diagnostic_child", "--nocapture"])
        .env("PEERSUS_DIAGNOSTIC_CHILD", "1")
        .env_remove("PEERSUS_LOG")
        .output()
        .unwrap()
}

#[test]
fn diagnostic_child() {
    if std::env::var_os("PEERSUS_DIAGNOSTIC_CHILD").is_none() {
        return;
    }
    if std::env::var_os("PEERSUS_EXISTING_LOGGER").is_some() {
        struct ExistingLogger;
        impl log::Log for ExistingLogger {
            fn enabled(&self, _: &log::Metadata<'_>) -> bool {
                false
            }
            fn log(&self, _: &log::Record<'_>) {}
            fn flush(&self) {}
        }
        static LOGGER: ExistingLogger = ExistingLogger;
        log::set_logger(&LOGGER).unwrap();
    }
    let _first = peersus_diagnostics::init("test").unwrap();
    let _second = peersus_diagnostics::init("test").unwrap();
    tracing::warn!(target: "peersus::runtime", code = "fixture-refused", "operation rejected");
    tracing::debug!(target: "peersus::runtime", count = 3, "fixture debug detail");
    peersus_diagnostics::dev_trace!(
        call = 7_u64,
        direction = "out",
        stage = "dispatch",
        outcome = "admitted",
        "fixture action dispatched"
    );
    peersus_diagnostics::dev_trace!(
        call = 7_u64,
        direction = "in",
        stage = "result",
        outcome = "failed-not-allowed",
        "fixture action answered"
    );
    tracing::trace!(target: "third_party::dev", "unreviewed dev detail");
    log::warn!(target: "peersus::legacy", "legacy facade fixture");
    tracing::warn!(target: "third_party", "unreviewed dependency payload");
}

#[test]
fn repeated_initialization_keeps_one_live_output_and_respects_levels() {
    let output = run("warn");
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.matches("operation rejected").count(), 1);
    assert!(stderr.contains("fixture-refused"));
    assert_eq!(stderr.matches("legacy facade fixture").count(), 1);
    assert!(!stderr.contains("fixture debug detail"));
    assert!(!stderr.contains("unreviewed dependency payload"));
}

#[test]
fn targeted_debug_records_structured_fields_without_enabling_dependencies() {
    let output = run("peersus::runtime=debug");
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr.contains("fixture debug detail"),
        peersus_diagnostics::DEVELOPMENT
    );
    assert_eq!(stderr.contains("count=3"), peersus_diagnostics::DEVELOPMENT);
    assert!(!stderr.contains("unreviewed dependency payload"));
}

#[test]
fn invalid_configuration_keeps_safe_output_without_echoing_its_value() {
    let output = run("[invalid-private-filter");
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid-diagnostic-filter"));
    assert_eq!(stderr.matches("operation rejected").count(), 1);
    assert_eq!(
        stderr.contains("fixture action dispatched"),
        peersus_diagnostics::DEVELOPMENT
    );
    assert!(!stderr.contains("invalid-private-filter"));
}

#[test]
fn global_trace_still_excludes_dependency_payloads() {
    let output = run("trace");
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr.contains("fixture debug detail"),
        peersus_diagnostics::DEVELOPMENT
    );
    assert!(!stderr.contains("unreviewed dependency payload"));
}

#[cfg(unix)]
#[test]
fn non_unicode_filter_keeps_safe_output() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "diagnostic_child", "--nocapture"])
        .env("PEERSUS_DIAGNOSTIC_CHILD", "1")
        .env("PEERSUS_LOG", OsString::from_vec(vec![0xff]))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid-diagnostic-filter"));
    assert_eq!(stderr.matches("operation rejected").count(), 1);
    assert_eq!(
        stderr.contains("fixture action dispatched"),
        peersus_diagnostics::DEVELOPMENT
    );
}

#[test]
fn existing_legacy_logger_does_not_disable_tracing_output() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "diagnostic_child", "--nocapture"])
        .env("PEERSUS_DIAGNOSTIC_CHILD", "1")
        .env("PEERSUS_EXISTING_LOGGER", "1")
        .env("PEERSUS_LOG", "warn")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("legacy-logger-present"));
    assert_eq!(stderr.matches("operation rejected").count(), 1);
}

/// Development builds emit bounded action events by default; a build without
/// development support emits none of them even though the same code ran.
#[test]
fn development_defaults_emit_bounded_action_events() {
    let output = run_unset();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr.contains("fixture action dispatched"),
        peersus_diagnostics::DEVELOPMENT
    );
    if peersus_diagnostics::DEVELOPMENT {
        assert!(stderr.contains("call=7"));
        assert!(stderr.contains("fixture action answered"));
    }
    assert!(!stderr.contains("unreviewed dev detail"));
    assert!(!stderr.contains("unreviewed dependency payload"));
}

/// A warning-level filter keeps every development event out: this is what
/// production output looks like. A real production binary also has the event
/// compiled out entirely, so this is a lower bound on the guarantee.
#[test]
fn warning_level_output_holds_no_development_events() {
    let output = run("warn");
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("fixture action dispatched"));
    assert!(!stderr.contains("fixture action answered"));
    assert!(!stderr.contains("unreviewed dev detail"));
}

/// Asking for the reviewed development target yields it where development
/// events exist and nothing where they do not; either way no unreviewed
/// target ever propagates, under any directive.
#[test]
fn development_target_is_bounded_and_never_widens() {
    let output = run("peersus::dev=trace");
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr.contains("fixture action dispatched"),
        peersus_diagnostics::DEVELOPMENT
    );
    assert!(!stderr.contains("unreviewed dev detail"));
    assert!(!stderr.contains("unreviewed dependency payload"));
}
