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
    assert!(stderr.contains("fixture debug detail"));
    assert!(stderr.contains("count=3"));
    assert!(!stderr.contains("unreviewed dependency payload"));
}

#[test]
fn invalid_configuration_keeps_safe_output_without_echoing_its_value() {
    let output = run("[invalid-private-filter");
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid-diagnostic-filter"));
    assert_eq!(stderr.matches("operation rejected").count(), 1);
    assert!(!stderr.contains("fixture debug detail"));
    assert!(!stderr.contains("invalid-private-filter"));
}

#[test]
fn global_trace_still_excludes_dependency_payloads() {
    let output = run("trace");
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("fixture debug detail"));
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
    assert!(!stderr.contains("fixture debug detail"));
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
