//! Private, development-only desktop panic and loop-stall reports.
use std::{
    collections::{BinaryHeap, VecDeque},
    fmt::{self, Write as _},
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant, SystemTime},
};

const KEEP: usize = 20;
const REPORT_LIMIT: usize = 4096;
const STAGES: usize = 32;
const POLL: Duration = Duration::from_secs(1);
const STALL_AFTER: Duration = Duration::from_secs(10);
static REPORTER: OnceLock<Reporter> = OnceLock::new();
static RECENT: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

pub use crate::INCIDENT_FOLDER_ENV as FOLDER_ENV;

#[derive(Clone)]
struct Reporter {
    program: &'static str,
    folder: PathBuf,
}

/// Configure this program's report directory. No files are opened until an incident occurs.
pub fn install(program: &'static str, data_directory: &Path) {
    let _ = REPORTER.set(Reporter {
        program,
        folder: data_directory.join("diagnostics"),
    });
}

/// Configure a trusted helper using the directory supplied by the owner.
pub fn install_from_environment(program: &'static str) {
    if let Some(folder) = std::env::var_os(FOLDER_ENV).filter(|value| !value.is_empty()) {
        let _ = REPORTER.set(Reporter {
            program,
            folder: folder.into(),
        });
    }
}

/// The report directory, once configured by the process entry point.
pub fn report_folder() -> Option<&'static Path> {
    REPORTER.get().map(|reporter| reporter.folder.as_path())
}

pub(crate) fn panic_report(thread: &str, place: &str) {
    if let Some(reporter) = REPORTER.get() {
        // std does not expose stable structured frames; don't serialize a raw backtrace.
        let _ = reporter.write("panic", format_args!("thread: {thread}\nplace: {place}\n"));
    }
}

impl Reporter {
    fn write(&self, kind: &str, detail: fmt::Arguments<'_>) -> io::Result<()> {
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(&self.folder)?;
        let metadata = fs::symlink_metadata(&self.folder)?;
        if !metadata.is_dir() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        // All Peersus processes share the retention limit. Don't wait on another writer in a
        // panic or hang; losing an incident is better than holding up a failing process.
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .clone()
            .create(true)
            .truncate(false)
            .open(self.folder.join(".incident-lock"))?;
        lock.try_lock()?;
        self.prune()?;

        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::UNIX_EPOCH
            .elapsed()
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let serial = SERIAL.fetch_add(1, Relaxed);
        let path = self
            .folder
            .join(format!("incident-{nanos:039}-{pid:010}-{serial:020}.txt"));
        let mut file = options.create_new(true).open(path)?;
        let build =
            option_env!("PEERSUS_BUILD_NUMBER").and_then(|number| number.parse::<u64>().ok());
        let mut body = ReportText {
            bytes: [0; REPORT_LIMIT],
            len: 0,
        };
        let _ = write!(
            body,
            "kind: {kind}\nprogram: {}\nversion: {}\nbuild: {}\nprofile: {}\nplatform: {}-{}\nprocess: {pid}\nunix-nanos: {nanos}\n",
            self.program,
            env!("CARGO_PKG_VERSION"),
            build.map_or_else(|| "unassigned".to_owned(), |n| n.to_string()),
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "optimized-development"
            },
            std::env::consts::OS,
            std::env::consts::ARCH,
        );
        let _ = body.write_fmt(detail);
        file.write_all(&body.bytes[..body.len])
    }

    fn prune(&self) -> io::Result<()> {
        // Keep the newest 19 existing reports, then write the twentieth. Bound memory even if the
        // directory contains extra files. Only accept precisely the names generated above.
        let mut newest = BinaryHeap::with_capacity(KEEP);
        for entry in fs::read_dir(&self.folder)? {
            let entry = entry?;
            let name = entry.file_name();
            if !name.to_str().is_some_and(report_name) || !entry.file_type()?.is_file() {
                continue;
            }
            newest.push(std::cmp::Reverse(name));
            if newest.len() >= KEEP
                && let Some(std::cmp::Reverse(oldest)) = newest.pop()
            {
                fs::remove_file(self.folder.join(oldest))?;
            }
        }
        Ok(())
    }
}

struct ReportText {
    bytes: [u8; REPORT_LIMIT],
    len: usize,
}

impl fmt::Write for ReportText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let keep = text.floor_char_boundary((self.bytes.len() - self.len).min(text.len()));
        self.bytes[self.len..self.len + keep].copy_from_slice(&text.as_bytes()[..keep]);
        self.len += keep;
        if keep < text.len() {
            Err(fmt::Error)
        } else {
            Ok(())
        }
    }
}

fn report_name(name: &str) -> bool {
    let Some(fields) = name
        .strip_prefix("incident-")
        .and_then(|s| s.strip_suffix(".txt"))
    else {
        return false;
    };
    let mut fields = fields.split('-');
    [39, 10, 20].into_iter().all(|length| {
        fields
            .next()
            .is_some_and(|field| field.len() == length && field.bytes().all(|b| b.is_ascii_digit()))
    }) && fields.next().is_none()
}

pub(crate) struct StageLayer;

struct Stage(Option<String>);

impl tracing::field::Visit for Stage {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "stage"
            && !value.is_empty()
            && value.len() <= 48
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            self.0 = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn fmt::Debug) {}
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for StageLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if REPORTER.get().is_none() || event.metadata().target() != "peersus::dev" {
            return;
        }
        let mut stage = Stage(None);
        event.record(&mut stage);
        if let (Some(stage), Ok(mut recent)) = (stage.0, RECENT.try_lock()) {
            if recent.len() == STAGES {
                recent.pop_front();
            }
            recent.push_back(stage);
        }
    }
}

/// One development-only loop heartbeat. Dropping it disarms its watchdog.
pub struct Heartbeat(Option<Arc<AtomicBool>>);

impl Heartbeat {
    /// Mark progress without locking, allocating or consulting the clock.
    pub fn tick(&self) {
        if let Some(beat) = &self.0 {
            beat.store(true, Relaxed);
        }
    }
}

/// Watch one loop. The watchdog uses a separate thread and never stops the program.
pub fn watch(name: &'static str) -> Heartbeat {
    Heartbeat(
        REPORTER
            .get()
            .and_then(|reporter| watch_loop(name, reporter.clone(), POLL, STALL_AFTER)),
    )
}

fn watch_loop(
    name: &'static str,
    reporter: Reporter,
    poll: Duration,
    limit: Duration,
) -> Option<Arc<AtomicBool>> {
    let beat = Arc::new(AtomicBool::new(false));
    let observed = Arc::downgrade(&beat);
    std::thread::Builder::new()
        .name("diagnostic-watchdog".to_owned())
        .spawn(move || {
            let mut last = Instant::now();
            let mut reported = false;
            loop {
                std::thread::sleep(poll);
                let Some(beat) = observed.upgrade() else {
                    break;
                };
                let ticked = beat.swap(false, Relaxed);
                drop(beat);
                if ticked {
                    last = Instant::now();
                    reported = false;
                } else if !reported && last.elapsed() >= limit {
                    reported = true;
                    let stages = RECENT
                        .try_lock()
                        .map(|ring| ring.iter().cloned().collect::<Vec<_>>().join(", "))
                        .unwrap_or_default();
                    let _ = reporter.write(
                        "hang",
                        format_args!(
                            "loop: {name}\nno progress: {} ms\nlast trace stages: {}\n",
                            last.elapsed().as_millis(),
                            if stages.is_empty() {
                                "none available"
                            } else {
                                &stages
                            }
                        ),
                    );
                }
            }
        })
        .ok()?;
    Some(beat)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reports(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_str().is_some_and(report_name))
            .map(|entry| fs::read_to_string(entry.path()).unwrap())
            .collect()
    }

    #[test]
    fn the_newest_twenty_reports_share_one_limit_and_leave_other_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let reporter = Reporter {
            program: "test",
            folder: dir.path().join("diagnostics"),
        };
        reporter
            .write("panic", format_args!("entry: 00\n"))
            .unwrap();
        fs::write(reporter.folder.join("incident-personal.txt"), "keep me").unwrap();
        let oversized = "é".repeat(REPORT_LIMIT);
        for i in 1..25 {
            reporter
                .write("hang", format_args!("entry: {i:02}\n{oversized}"))
                .unwrap();
        }
        let kept = reports(&reporter.folder);
        assert_eq!(kept.len(), 20);
        for i in 5..25 {
            assert!(
                kept.iter()
                    .any(|report| report.contains(&format!("entry: {i:02}")))
            );
        }
        assert!(kept.iter().all(|report| report.len() <= REPORT_LIMIT));
        assert_eq!(
            fs::read_to_string(reporter.folder.join("incident-personal.txt")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn a_stall_reports_once_then_rearms_on_the_next_tick() {
        let dir = tempfile::tempdir().unwrap();
        let reporter = Reporter {
            program: "test",
            folder: dir.path().join("diagnostics"),
        };
        // Drive the same layer that observes reviewed development stages in each process.
        use tracing_subscriber::prelude::*;
        install("test", dir.path());
        tracing::subscriber::with_default(tracing_subscriber::registry().with(StageLayer), || {
            crate::dev_trace!(
                stage = "reviewed-stage",
                payload = "synthetic-marker",
                "recent event"
            );
            crate::dev_trace!(stage = "/synthetic-private/path", "not a stage name");
        });
        let poll = Duration::from_millis(10);
        let limit = Duration::from_millis(250);
        let stalled = Heartbeat(watch_loop("stalled", reporter.clone(), poll, limit));
        let live = Heartbeat(watch_loop("live", reporter.clone(), poll, limit));
        let tick_for = |duration| {
            let start = Instant::now();
            while start.elapsed() < duration {
                live.tick();
                std::thread::sleep(poll);
            }
        };
        tick_for(limit * 3);
        let first = reports(&reporter.folder);
        assert_eq!(first.len(), 1);
        assert!(first[0].contains("loop: stalled"));
        assert!(first[0].contains("last trace stages: reviewed-stage"));
        assert!(!first[0].contains("synthetic"));
        stalled.tick();
        tick_for(limit * 3);
        let again = reports(&reporter.folder);
        assert_eq!(again.len(), 2);
        assert!(again.iter().all(|report| report.contains("loop: stalled")));
    }
}
