//! The helper against real crashes: this test binary runs itself again as the watched program, which starts
//! `peersus-diag` through `peersus_diagnostics::crash_watch` exactly as the owner and the window do, then crashes.
#![cfg(target_os = "linux")]

use std::{
    path::Path,
    process::{Command, ExitStatus},
    time::Duration,
};

const ROLE: &str = "PEERSUS_DIAG_TEST_ROLE";
const FOLDER: &str = "PEERSUS_DIAG_TEST_FOLDER";

#[allow(unconditional_recursion)]
fn deeper(depth: u64) -> u64 {
    let pad = std::hint::black_box([depth; 64]);
    deeper(depth.wrapping_add(pad[(depth % 64) as usize])).wrapping_add(1)
}

/// The watched program: starts capture, starts a few threads, then does what its role says.
fn watched(role: &str) {
    let folder = std::env::var(FOLDER).expect("the folder is given");
    let helper = Path::new(env!("CARGO_BIN_EXE_peersus-diag"));
    let watch =
        peersus_diagnostics::crash_watch::start_with(helper, "peersus-test", Path::new(&folder))
            .expect("capture starts");
    for n in 0..3u64 {
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(5 + n));
            }
        });
    }
    std::thread::sleep(Duration::from_millis(100));
    match role {
        "abort" => std::process::abort(),
        // A fatal signal sent from outside (`kill -ABRT`), which no instruction would send again by itself.
        "killed" => {
            let _ = Command::new("kill")
                .args(["-ABRT", &std::process::id().to_string()])
                .status();
            std::thread::sleep(Duration::from_secs(10));
        }
        "overflow" => {
            let _ = std::thread::Builder::new()
                .name("deep".into())
                .spawn(|| deeper(1))
                .expect("a thread")
                .join();
        }
        "stop" => {
            drop(watch);
            std::thread::sleep(Duration::from_millis(100));
            std::process::abort();
        }
        _ => {}
    }
}

/// Runs this test binary again as the watched program with `role`, and returns how it ended and the folder.
fn run(role: &str) -> (ExitStatus, tempfile::TempDir) {
    let folder = tempfile::tempdir().unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "watched_program",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(ROLE, role)
        .env(FOLDER, folder.path())
        .status()
        .unwrap();
    (status, folder)
}

fn files(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// The minidump's exception record: (thread id, code). See `report::mark_signal` for the layout.
fn exception(dump: &[u8]) -> (u32, u32) {
    let word = |at: usize| u32::from_le_bytes(dump[at..at + 4].try_into().unwrap());
    let (streams, directory) = (word(8), word(12) as usize);
    let stream = (0..streams as usize)
        .map(|index| directory + index * 12)
        .find(|entry| word(*entry) == 6)
        .map(|entry| word(entry + 8) as usize)
        .expect("an exception stream");
    (word(stream), word(stream + 8))
}

#[test]
fn watched_program() {
    if let Ok(role) = std::env::var(ROLE) {
        watched(&role);
    }
}

fn crashed_with_report(role: &str, signal: i32) {
    use std::os::unix::process::ExitStatusExt;
    let (status, folder) = run(role);
    // The program ends exactly as it would have without the helper.
    assert_eq!(status.signal(), Some(libc_sigabrt()), "{role}: {status:?}");
    let names = files(folder.path());
    assert_eq!(names.len(), 2, "{role}: {names:?}");
    let dump_name = names.iter().find(|name| name.ends_with(".dmp")).unwrap();
    assert!(dump_name.starts_with("crash-peersus-test-"));
    let dump = std::fs::read(folder.path().join(dump_name)).unwrap();
    assert_eq!(&dump[..4], b"MDMP");
    let (thread, code) = exception(&dump);
    assert_eq!(code, signal as u32, "{role}: the dump names the signal");
    assert_ne!(thread, 0);
    let text =
        std::fs::read_to_string(folder.path().join(dump_name).with_extension("txt")).unwrap();
    assert!(text.contains("program: peersus-test"), "{text}");
    assert!(
        text.contains("minidump beside this file was written"),
        "{text}"
    );
    use std::os::unix::fs::PermissionsExt;
    for name in &names {
        let mode = std::fs::metadata(folder.path().join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }
}

fn libc_sigabrt() -> i32 {
    6
}

#[test]
fn an_abort_is_captured_and_the_program_still_ends_with_it() {
    crashed_with_report("abort", 6);
}

/// The program would have ended at a `kill -ABRT`; with capture on it still does, after the dump.
#[test]
fn a_fatal_signal_sent_from_outside_is_captured_and_still_ends_the_program() {
    crashed_with_report("killed", 6);
}

/// A real segmentation fault on the guard page. Rust's own handler catches it and aborts; the helper hands the fault
/// back to the program and captures the abort, from the thread that overflowed.
#[test]
fn a_stack_overflow_is_left_to_the_program_and_its_abort_is_captured() {
    crashed_with_report("overflow", 6);
}

#[test]
fn a_program_that_ends_normally_leaves_nothing() {
    let (status, folder) = run("clean");
    assert!(status.success(), "{status:?}");
    assert!(files(folder.path()).is_empty());
}

#[test]
fn after_capture_is_stopped_a_crash_is_not_captured() {
    use std::os::unix::process::ExitStatusExt;
    let (status, folder) = run("stop");
    assert_eq!(status.signal(), Some(libc_sigabrt()));
    assert!(files(folder.path()).is_empty());
}
