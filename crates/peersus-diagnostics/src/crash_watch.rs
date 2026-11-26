//! Opt-in crash capture from outside: the program starts `peersus-diag` beside itself and names it as its tracer.
//!
//! Nothing of crash handling runs in the program: no signal handler, no dump code. The helper (`crates/peersus-diag`)
//! watches the program with `ptrace(2)` and writes a minidump when a signal would end it. This module only starts the
//! helper, allows it as tracer (`PR_SET_PTRACER`, which Yama's `ptrace_scope` 1 requires) and stops it again; every
//! failure is a reason the person can read, never a reason for the program not to run.
use std::path::{Path, PathBuf};

/// The folder crash reports are written to, inside the application's data directory (the same as the incident reports).
pub const FOLDER: &str = "diagnostics";

/// Why crash capture is not running. The words for each are the window's (`peersus-ui`), where they are translated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// Capture from outside exists on Linux only.
    System,
    /// No `peersus-diag` beside this program (an installation without it, or a build that did not make it).
    Missing,
    /// The helper could not be started or did not answer.
    NotStarted,
    /// The system refuses tracing (Yama `ptrace_scope` 2 or 3, or a debugger is already attached).
    Refused,
}

/// The folder crash reports of this installation go to.
pub fn folder(data_directory: &Path) -> PathBuf {
    data_directory.join(FOLDER)
}

/// The running helper. Dropping it stops the helper, which releases the program at once.
pub struct CrashWatch {
    #[cfg(target_os = "linux")]
    child: std::process::Child,
}

impl std::fmt::Debug for CrashWatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CrashWatch")
    }
}

#[cfg(target_os = "linux")]
impl Drop for CrashWatch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts watching this process for crashes, writing reports of `program` (a plain word: `peersus`, `peersus-ui`) into
/// `folder`. Blocks the calling thread until the helper answers, at most a few seconds: call it off any loop that must
/// stay responsive.
pub fn start(program: &str, folder: &Path) -> Result<CrashWatch, Unavailable> {
    let helper = std::env::current_exe()
        .map_err(|_| Unavailable::Missing)?
        // Named here as written so that `scripts/package-metadata.py` can prove every package ships it.
        .with_file_name(format!("peersus-diag{}", std::env::consts::EXE_SUFFIX));
    start_with(&helper, program, folder)
}

/// [`start`] with the helper at a given path (tests use the one Cargo built).
#[cfg(target_os = "linux")]
pub fn start_with(helper: &Path, program: &str, folder: &Path) -> Result<CrashWatch, Unavailable> {
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    if !helper.is_file() {
        return Err(Unavailable::Missing);
    }
    let child = Command::new(helper)
        .arg("watch")
        .arg(program)
        .arg(folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Unavailable::NotStarted)?;
    let mut watch = CrashWatch { child };
    let refused = |watch: CrashWatch, why| {
        // Dropping the watch stops the helper and waits for it.
        drop(watch);
        Err(why)
    };
    let pid = i32::try_from(watch.child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw);
    let Some(pid) = pid else {
        return refused(watch, Unavailable::NotStarted);
    };
    if rustix::process::set_ptracer(rustix::process::PTracer::ProcessID(pid)).is_err() {
        return refused(watch, Unavailable::Refused);
    }
    let (Some(mut input), Some(output)) = (watch.child_stdin(), watch.child_stdout()) else {
        return refused(watch, Unavailable::NotStarted);
    };
    if writeln!(input, "traced")
        .and_then(|()| input.flush())
        .is_err()
    {
        return refused(watch, Unavailable::NotStarted);
    }
    // The answer is read on its own thread so that a helper that never answers costs a bounded wait, not a hang.
    let (sender, answer) = mpsc::channel();
    std::thread::Builder::new()
        .name("crash-watch-start".into())
        .spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(output).read_line(&mut line);
            let _ = sender.send(line);
        })
        .map_err(|_| Unavailable::NotStarted)?;
    match answer.recv_timeout(Duration::from_secs(5)) {
        Ok(line) if line.trim() == "ready" => Ok(watch),
        Ok(line) if line.starts_with("denied") => refused(watch, Unavailable::Refused),
        _ => refused(watch, Unavailable::NotStarted),
    }
}

#[cfg(target_os = "linux")]
impl CrashWatch {
    fn child_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.stdin.take()
    }
    fn child_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }
}

#[cfg(not(target_os = "linux"))]
pub fn start_with(
    _helper: &Path,
    _program: &str,
    _folder: &Path,
) -> Result<CrashWatch, Unavailable> {
    Err(Unavailable::System)
}

/// How crash capture of one program stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The person has not switched it on.
    Off,
    /// Switched on; the helper is being started.
    Starting,
    /// The helper watches the program.
    Watching,
    /// Switched on, and it cannot run here.
    Unavailable(Unavailable),
}

/// Keeps crash capture of this process as the person wants it, without ever making the caller wait: the helper is
/// started and stopped on a thread of its own, and every change of [`Status`] is handed to `on_change` there.
pub struct Controller {
    wanted: std::sync::mpsc::Sender<bool>,
    status: std::sync::Arc<std::sync::Mutex<Status>>,
}

impl Controller {
    /// A controller for `program` (a plain word), writing into `folder`; capture is off until [`Controller::set`].
    pub fn new(
        program: &'static str,
        folder: PathBuf,
        on_change: impl Fn(Status) + Send + 'static,
    ) -> Self {
        let (wanted, requests) = std::sync::mpsc::channel::<bool>();
        let status = std::sync::Arc::new(std::sync::Mutex::new(Status::Off));
        let shared = status.clone();
        let publish = move |next: Status| {
            let changed = shared
                .lock()
                .map(|mut current| std::mem::replace(&mut *current, next) != next)
                .unwrap_or(false);
            if changed {
                on_change(next);
            }
        };
        // A thread that cannot be made leaves capture off; `set` then sends into a closed channel and nothing happens.
        let _ = std::thread::Builder::new()
            .name("crash-watch".into())
            .spawn(move || {
                let mut watch: Option<CrashWatch> = None;
                // Ends, stopping the helper, when the controller is dropped.
                while let Ok(first) = requests.recv() {
                    // Only the newest wish counts: a switch flipped several times while the helper started is one change.
                    let on = requests.try_iter().last().unwrap_or(first);
                    match (on, watch.is_some()) {
                        (true, false) => {
                            publish(Status::Starting);
                            match start(program, &folder) {
                                Ok(started) => {
                                    watch = Some(started);
                                    publish(Status::Watching);
                                }
                                Err(why) => publish(Status::Unavailable(why)),
                            }
                        }
                        (false, _) => {
                            watch = None;
                            publish(Status::Off);
                        }
                        (true, true) => {}
                    }
                }
            });
        Self { wanted, status }
    }

    /// Switches capture on or off. Returns at once; the change follows on the controller's thread.
    pub fn set(&self, on: bool) {
        let _ = self.wanted.send(on);
    }

    pub fn status(&self) -> Status {
        self.status.lock().map_or(Status::Off, |status| *status)
    }
}

/// One crash report in the folder: its dump's file name, size and when it was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub file: String,
    pub bytes: u64,
    pub unix_seconds: u64,
}

/// The crash reports in `folder`, newest first. A folder that does not exist holds none.
pub fn reports(folder: &Path) -> Vec<Report> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut found: Vec<Report> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            if !(file.starts_with("crash-") && file.ends_with(".dmp")) {
                return None;
            }
            let meta = entry.metadata().ok()?;
            let unix_seconds = meta
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            Some(Report {
                file,
                bytes: meta.len(),
                unix_seconds,
            })
        })
        .collect();
    found.sort_by(|a, b| {
        b.unix_seconds
            .cmp(&a.unix_seconds)
            .then_with(|| b.file.cmp(&a.file))
    });
    found
}

/// Deletes every crash report (dump and text) in `folder`, and nothing else. Returns how many reports went.
pub fn delete_reports(folder: &Path) -> usize {
    reports(folder)
        .iter()
        .filter(|report| {
            let dump = folder.join(&report.file);
            let _ = std::fs::remove_file(dump.with_extension("txt"));
            std::fs::remove_file(dump).is_ok()
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_the_crash_dumps_newest_first_and_deleting_takes_only_them() {
        let folder = tempfile::tempdir().unwrap();
        let at = |name: &str, seconds: u64| {
            let path = folder.path().join(name);
            std::fs::write(&path, b"dump").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
                .unwrap();
        };
        at("crash-peersus-1.dmp", 100);
        at("crash-peersus-ui-2.dmp", 200);
        std::fs::write(folder.path().join("crash-peersus-ui-2.txt"), b"text").unwrap();
        std::fs::write(folder.path().join("peersus-ui.log"), b"log").unwrap();
        std::fs::write(folder.path().join("stall-1.txt"), b"stall").unwrap();
        let found = reports(folder.path());
        assert_eq!(
            found
                .iter()
                .map(|report| report.file.as_str())
                .collect::<Vec<_>>(),
            ["crash-peersus-ui-2.dmp", "crash-peersus-1.dmp"]
        );
        assert_eq!(found[0].bytes, 4);
        assert_eq!(found[0].unix_seconds, 200);
        assert_eq!(delete_reports(folder.path()), 2);
        let mut left: Vec<String> = std::fs::read_dir(folder.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["peersus-ui.log", "stall-1.txt"]);
    }

    #[test]
    fn a_missing_folder_has_no_reports_and_a_missing_helper_is_named() {
        let folder = tempfile::tempdir().unwrap();
        assert!(reports(&folder.path().join("absent")).is_empty());
        let missing = start_with(
            &folder.path().join("peersus-diag"),
            "peersus",
            folder.path(),
        );
        let expected = if cfg!(target_os = "linux") {
            Unavailable::Missing
        } else {
            Unavailable::System
        };
        assert_eq!(missing.err(), Some(expected));
    }
}
