//! Shared diagnostic emission configuration; applications own initialization.
//!
//! Initialize once at each process entry, before spawning tasks. Keep the returned
//! guard until shutdown: desktop output uses a bounded off-thread writer. Android
//! writes directly to its native Logcat backend, never through an assumed stderr
//! redirect. Diagnostic fields must be explicitly safe at the emission site.
//!
//! Development builds (see [`DEVELOPMENT`]) also write bounded text reports of a panic and of a stalled loop
//! (see [`incident`]); in every other build those names are inert and nothing of them is compiled in.
use std::{fmt, sync::Mutex};
use tracing_subscriber::{
    EnvFilter,
    filter::{LevelFilter, Targets},
    prelude::*,
};

// rustdoc does not inherit the release profile's debug-assertions setting for doctests.
#[cfg(all(
    feature = "production",
    any(feature = "development", all(debug_assertions, not(doctest)))
))]
compile_error!("release preparation must not enable development diagnostics");

#[cfg(all(
    any(target_os = "android", test),
    any(debug_assertions, feature = "development")
))]
mod android_dev;
pub mod crash_watch;
#[cfg(all(
    not(target_os = "android"),
    any(debug_assertions, feature = "development")
))]
pub mod incident;
mod place;

/// What the programs of a build that is not a development build see of [`incident`]: the same names, doing nothing,
/// so that no program has to ask which kind of build it is, and none of the real code is compiled in.
#[cfg(not(all(
    not(target_os = "android"),
    any(debug_assertions, feature = "development")
)))]
pub mod incident {
    use std::path::Path;

    pub use super::INCIDENT_FOLDER_ENV as FOLDER_ENV;

    /// Does nothing.
    #[derive(Clone, Copy, Debug)]
    pub struct Heartbeat;

    impl Heartbeat {
        /// Does nothing.
        #[inline]
        pub fn tick(&self) {}
    }

    /// Does nothing.
    #[inline]
    pub fn install(_program: &'static str, _data_directory: &Path) {}

    /// Does nothing.
    #[inline]
    pub fn install_from_environment(_program: &'static str) {}

    /// Does nothing.
    #[inline]
    pub fn watch(_name: &'static str) -> Heartbeat {
        Heartbeat
    }

    /// No folder: no helper is told one.
    #[inline]
    pub fn report_folder() -> Option<&'static Path> {
        None
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn panic_report(_thread: &str, _place: &str) {}
}

/// Where the owner tells its trusted Rust helpers to write development reports.
pub const INCIDENT_FOLDER_ENV: &str = "PEERSUS_DIAGNOSTICS_DIR";

static INITIALIZED: Mutex<bool> = Mutex::new(false);

/// Re-exported so `dev_trace!` needs no extra dependency in the crates it is used in.
#[doc(hidden)]
pub use tracing;

/// `true` only where development diagnostics can exist at all: a build with debug assertions (the Cargo `dev`
/// profile) and a build made with this crate's `development` feature (an optimized development build: the Android
/// development libraries and the desktop programs `./dev.sh deploy` installs; each program's
/// `development-diagnostics` feature enables it). Release preparation (`./build.sh`, `cargo xtask build`) enables
/// neither. This is evaluated when THIS crate is compiled, so in a production build every `dev_trace!` site is a
/// dead branch: no `PEERSUS_LOG` value can surface a development event from a production binary, because the
/// emission itself is compiled out.
pub const DEVELOPMENT: bool = cfg!(any(debug_assertions, feature = "development"));

/// One bounded development event on the reviewed `peersus::dev` target.
///
/// Fields must stay categorical and numeric: stage/direction words and outcome
/// words from the application's own closed sets, call/ticket/handle numbers,
/// counts, durations and byte sizes. Never payloads, paths, peer or plugin
/// names, endpoints, keystrokes, clipboard data, or text supplied by a plugin
/// or the platform. Behind `DEVELOPMENT`, so a production binary never emits.
#[macro_export]
macro_rules! dev_trace {
    ($($field:tt)*) => {
        if $crate::DEVELOPMENT && $crate::tracing::enabled!(target: "peersus::dev", $crate::tracing::Level::TRACE) {
            $crate::tracing::trace!(target: "peersus::dev", process = std::process::id(), $($field)*);
        }
    };
}

/// A closed diagnostic vocabulary. Guest-supplied codes and messages never pass through.
pub fn outcome(result: &peersus_api::message::Outcome) -> &'static str {
    match result {
        peersus_api::message::Outcome::Ok { .. } => "ok",
        peersus_api::message::Outcome::MissingPermission { .. } => "missing-permission",
        peersus_api::message::Outcome::NotAccepted {} => "not-accepted",
        peersus_api::message::Outcome::Error { code, .. } => match code.as_str() {
            "cancelled" => "cancelled",
            "denied" => "denied",
            "refused" => "refused",
            "unavailable" => "unavailable",
            "unknown-method" => "unknown-method",
            "version" => "version",
            "timeout" | "invalid-deadline" => "timeout",
            "busy" => "busy",
            "too-large" => "too-large",
            "platform-refused" => "platform-refused",
            "rate-limited" => "rate-limited",
            "credential-rejected" => "credential-rejected",
            "not-shown" => "not-shown",
            _ => "failed",
        },
    }
}

/// No manifest names, service names, or method text from an extension enter diagnostics.
/// Unknown methods remain distinguishable by the broker's numeric service/method slots.
pub fn method(method: &str) -> &'static str {
    match method {
        "inspect" => "inspect",
        "inject" => "inject",
        "release" => "release",
        "send" => "send",
        "get" => "get",
        "set" => "set",
        "list" => "list",
        _ => "extension-method",
    }
}

/// Only fixed codes enter diagnostics; do not log permission identities or guest data.
pub fn refusal(error: &peersus_api::Error) -> &'static str {
    use peersus_api::Error;
    match error {
        Error::InvalidManifest => "invalid-manifest",
        Error::UnknownProvider => "unknown-provider",
        Error::UnknownMethod => "unknown-method",
        Error::Version => "version",
        Error::Denied => "denied",
        Error::MissingPermission(_) => "missing-permission",
        Error::Oversize => "oversize",
        Error::Timeout => "timeout",
        Error::Cancelled => "cancelled",
        Error::Busy => "busy",
        Error::RateLimited => "rate-limited",
        Error::QueueFull => "queue-full",
        Error::PluginPanicked => "plugin-panicked",
        Error::InvalidPayload => "invalid-payload",
        Error::Unavailable => "unavailable",
        Error::Backend => "backend",
    }
}

/// Reasons are deliberately fixed: invalid environment values are never echoed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitError {
    InvalidRole,
    Unavailable,
    AlreadyConfigured,
}

impl fmt::Display for InitError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(match self {
            Self::InvalidRole => "The diagnostic process role is invalid",
            Self::Unavailable => "The diagnostic backend is unavailable",
            Self::AlreadyConfigured => "Another diagnostic subscriber is already installed",
        })
    }
}
impl std::error::Error for InitError {}

/// The first desktop initializer owns the writer; retain it for the process lifetime.
/// Repeated initializers do not replace a subscriber or create another worker.
pub struct Guard {
    #[cfg(not(target_os = "android"))]
    _worker: Option<tracing_appender::non_blocking::WorkerGuard>,
    #[cfg(not(target_os = "android"))]
    dropped: Option<tracing_appender::non_blocking::ErrorCounter>,
}

impl Guard {
    /// Desktop queue overflow is observable, never silently described as lossless.
    pub fn dropped_lines(&self) -> usize {
        #[cfg(not(target_os = "android"))]
        {
            self.dropped
                .as_ref()
                .map_or(0, |counter| counter.dropped_lines())
        }
        #[cfg(target_os = "android")]
        {
            0
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(not(target_os = "android"))]
        if let Some(counter) = &self.dropped {
            let dropped = counter.dropped_lines();
            if dropped != 0 {
                // The tracing queue itself overflowed; reporting through it could hide loss.
                eprintln!("Diagnostic queue dropped {dropped} records");
            }
        }
    }
}

fn repeated() -> Guard {
    Guard {
        #[cfg(not(target_os = "android"))]
        _worker: None,
        #[cfg(not(target_os = "android"))]
        dropped: None,
    }
}

/// The line a log file starts with after it passed its size limit and was emptied. What it held went to the previous
/// generation beside it (`owner.log.1`) when that was possible. Whoever shows the last thing a program said skips it:
/// it is said about the file, not by the program about itself.
pub const LOG_EMPTIED: &str =
    "The log passed its size limit and was emptied; what follows is newer";

/// The environment variable that tells a program where the log it writes to as standard error lives. Whoever hands a
/// log to a program (the window starting the owner) sets it to the path it opened with [`open_log`], so that the
/// program can keep the previous generation beside it: a file descriptor knows no path, and finding one from it
/// takes `unsafe` or a platform-specific lookup. A program whose standard error is not that file (checked where the
/// system gives a file an identity) ignores it and empties the log as before.
pub const LOG_FILE: &str = "PEERSUS_LOG_FILE";

/// What the log and its previous generation may hold together: a log is rolled into `<log>.1` at half of this, so the
/// pair stays near it (plus what is written between two looks, [`LOG_CHECK`]), however long the program runs.
#[cfg(not(target_os = "android"))]
const LOG_LIMIT: u64 = 32 * 1024 * 1024;
/// The size is asked again after this much was written, not at every line.
#[cfg(not(target_os = "android"))]
const LOG_CHECK: u64 = 256 * 1024;

/// The previous generation of the log at `path`: the same name with `.1` after it.
#[cfg(not(target_os = "android"))]
fn previous_generation(path: &std::path::Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    name.into()
}

/// Whether the file at `path` is the file `held` is a second holding of.
#[cfg(all(unix, not(target_os = "android")))]
fn is_same_file(held: &std::fs::File, path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (held.metadata(), std::fs::metadata(path)) {
        (Ok(held), Ok(found)) => held.dev() == found.dev() && held.ino() == found.ino(),
        _ => false,
    }
}
/// The standard library gives a file no identity here: the path is taken as told, by the program that opened the log.
#[cfg(all(not(unix), not(target_os = "android")))]
fn is_same_file(_held: &std::fs::File, path: &std::path::Path) -> bool {
    path.is_file()
}

/// Lock the older generation before copying it, then hold that lock through the active-log
/// truncate. The window and both WASM hosts may have writers for the same inherited log.
#[cfg(not(target_os = "android"))]
fn archive_log(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::{fs, io};
    let path = previous_generation(path);
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
            }
            fs::OpenOptions::new().read(true).write(true).open(&path)?
        }
        Err(error) => return Err(error),
    };
    file.try_lock()?;
    Ok(file)
}

#[cfg(not(target_os = "android"))]
fn keep_previous(
    log: &std::path::Path,
    end: u64,
    limit: u64,
    archive: &mut std::fs::File,
) -> std::io::Result<()> {
    use std::io::{BufRead, Read, Seek, SeekFrom};
    let mut source = std::fs::File::open(log)?;
    let start = end.saturating_sub(limit);
    // Start on a whole line unless the tail is part of a single oversized record.
    source.seek(SeekFrom::Start(start.saturating_sub(1)))?;
    let mut source = std::io::BufReader::new(source.take(end - start.saturating_sub(1)));
    if start > 0 {
        source.skip_until(b'\n')?;
    }
    archive.set_len(0)?;
    std::io::copy(&mut source, archive)?;
    Ok(())
}

/// What diagnostics are written through. When standard error is a file (the owner's log, kept by the window that
/// started it) the file is kept from growing without end: nothing rotates it for an owner, which may run for weeks.
/// Past its limit the log is rolled: its last lines become the one previous generation (`<log>.1`, replacing the one
/// before), it is emptied, says so, and the newest lines follow, so the evidence of what happened just before the
/// roll is kept. The previous generation is as large as the limit at most. When the log's path is not known (see
/// [`LOG_FILE`]) the log is emptied and nothing is kept. A terminal or a pipe is no file and is left alone. What others
/// write to the same file (a helper that inherited it) is counted too, since it is the file's own size that is asked.
#[cfg(not(target_os = "android"))]
struct Bounded<W> {
    out: W,
    /// The file `out` writes to, held a second time to ask its size and empty it. `None` when `out` is no file, or
    /// when emptying it was refused once (it is not tried again).
    file: Option<std::fs::File>,
    /// The path of that file, when it is known and is that file: where the previous generation is kept beside.
    log: Option<std::path::PathBuf>,
    /// A log is rolled when it passes this.
    limit: u64,
    /// How much was written since the size was last asked.
    unchecked: u64,
}

#[cfg(not(target_os = "android"))]
impl Bounded<std::io::Stderr> {
    fn stderr() -> Self {
        let out = std::io::stderr();
        #[cfg(unix)]
        let file = {
            use std::os::fd::AsFd;
            out.as_fd()
                .try_clone_to_owned()
                .ok()
                .map(std::fs::File::from)
        };
        #[cfg(windows)]
        let file = {
            use std::os::windows::io::AsHandle;
            out.as_handle()
                .try_clone_to_owned()
                .ok()
                .map(std::fs::File::from)
        };
        #[cfg(not(any(unix, windows)))]
        let file: Option<std::fs::File> = None;
        let log = std::env::var_os(LOG_FILE)
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from);
        let limit = if log.is_some() {
            LOG_LIMIT / 2
        } else {
            LOG_LIMIT
        };
        Self::over(out, file, log, limit)
    }
}

#[cfg(not(target_os = "android"))]
impl<W: std::io::Write> Bounded<W> {
    /// `limit` is what the log may grow to before it is rolled; `log` is its path, if the caller knows it.
    fn over(
        out: W,
        file: Option<std::fs::File>,
        log: Option<std::path::PathBuf>,
        limit: u64,
    ) -> Self {
        let file = file.filter(|file| file.metadata().is_ok_and(|found| found.is_file()));
        let log = log.filter(|log| file.as_ref().is_some_and(|file| is_same_file(file, log)));
        let mut bounded = Self {
            out,
            file,
            log,
            limit,
            // Asked again at the first write, as before.
            unchecked: LOG_CHECK,
        };
        // Asked now too, before anything is written: a file left large by an earlier run is rolled when the program
        // starts. Asked only at the first line, an owner with nothing to say kept such a file for as long as it ran
        // (seen on 2026-11-15: 243 MB, an owner of this bound running over it and silent).
        bounded.trim();
        bounded
    }

    fn trim(&mut self) {
        use std::io::Seek;
        let Some(mut file) = self.file.as_ref() else {
            return;
        };
        match file.metadata() {
            Ok(found) if found.len() > self.limit => {
                // A verified owner.log path keeps the recent tail BEFORE truncating the same
                // active inode. Never truncate when the copy or lock fails; a later check retries.
                let _archive =
                    if let Some(log) = self.log.as_ref().filter(|log| is_same_file(file, log)) {
                        let Ok(mut archive) = archive_log(log) else {
                            return;
                        };
                        let Ok(metadata) = file.metadata() else {
                            return;
                        };
                        if metadata.len() <= self.limit
                            || keep_previous(log, metadata.len(), self.limit, &mut archive).is_err()
                        {
                            return;
                        }
                        Some(archive)
                    } else {
                        None
                    };
                // Emptied, and its place set back to the start: the second holding shares the place with `out`, so a
                // file that was not opened for appending does not go on at the old place with a hole before it.
                if file.set_len(0).is_err() || file.seek(std::io::SeekFrom::Start(0)).is_err() {
                    self.file = None;
                    return;
                }
                let _ = self
                    .out
                    .write_all(LOG_EMPTIED.as_bytes())
                    .and_then(|()| self.out.write_all(b"\n"));
            }
            Ok(_) => {}
            Err(_) => self.file = None,
        }
    }
}

#[cfg(not(target_os = "android"))]
impl<W: std::io::Write> std::io::Write for Bounded<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.out.write(bytes)?;
        self.unchecked = self.unchecked.saturating_add(written as u64);
        if self.unchecked >= LOG_CHECK.min(self.limit) {
            self.unchecked = 0;
            self.trim();
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// Open the log file at `path` so that what is written goes on at its end and the file can be kept from growing without
/// end (see [`LOG_EMPTIED`]): made if it is not there, and private to the person where files have such a mode. Its
/// directory has to exist. Whoever hands a file to a program of this application as its standard error opens it here.
///
/// On Windows a file opened for appending may not have its length changed, so it could never be emptied (seen in a
/// Windows guest: both tests of the bound failed). There it is opened for writing and placed at its end once; the
/// programs that inherit it share that place, as they share the file.
#[cfg(not(target_os = "android"))]
pub fn open_log(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.append(true).mode(0o600).open(path)
    }
    #[cfg(not(unix))]
    {
        use std::io::Seek;
        let mut file = options.write(true).open(path)?;
        file.seek(std::io::SeekFrom::End(0))?;
        Ok(file)
    }
}

/// The filter for a `PEERSUS_LOG` value: what it names is added to the warnings of everything, never put in their
/// place, so a value naming one target does not silence every other target's warnings. A level the value gives for
/// everything comes later and replaces that. The development target is not kept under a value: `warn` alone is
/// what production output looks like, so development events are shown only when the value names them.
fn filter_for(value: &str) -> Option<EnvFilter> {
    EnvFilter::builder()
        .with_regex(false)
        .parse(format!("warn,{value}"))
        .ok()
}

/// Configure reviewed first-party diagnostics. `PEERSUS_LOG` accepts ordinary
/// tracing level/target directives, added to the defaults; dependencies remain
/// excluded even at trace.
/// Production output is warn/error only and no environment can enable the
/// development events, which are compiled out there. Development builds default
/// to the reviewed `peersus::dev` event target; additional reviewed first-party
/// targets can be selected explicitly without enabling dependency logs.
/// Invalid filters retain these safe defaults and report a fixed warning.
/// This function never changes permissions, opens a listener or reads payloads.
pub fn init(role: &'static str) -> Result<Guard, InitError> {
    let mut initialized = INITIALIZED.lock().map_err(|_| InitError::Unavailable)?;
    if *initialized {
        return Ok(repeated());
    }
    if role.is_empty()
        || role.len() > 14
        || !role
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
    {
        return Err(InitError::InvalidRole);
    }
    let default_filter = if DEVELOPMENT {
        "warn,peersus::dev=trace"
    } else {
        "warn"
    };
    let mut invalid_filter = false;
    let filter = match std::env::var("PEERSUS_LOG") {
        Ok(value) => filter_for(&value).unwrap_or_else(|| {
            invalid_filter = true;
            EnvFilter::new(default_filter)
        }),
        Err(std::env::VarError::NotPresent) => EnvFilter::new(default_filter),
        Err(_) => {
            invalid_filter = true;
            EnvFilter::new(default_filter)
        }
    };
    let subscriber = tracing_subscriber::registry()
        .with(Targets::new().with_target("peersus", LevelFilter::TRACE))
        .with(if DEVELOPMENT {
            LevelFilter::TRACE
        } else {
            LevelFilter::WARN
        })
        .with(filter);
    #[cfg(all(
        not(target_os = "android"),
        any(debug_assertions, feature = "development")
    ))]
    let subscriber = subscriber.with(incident::StageLayer);
    #[cfg(not(target_os = "android"))]
    let guard = {
        let (writer, worker) = tracing_appender::non_blocking::NonBlockingBuilder::default()
            .buffered_lines_limit(1024)
            .lossy(true)
            .thread_name("diagnostic-writer")
            .finish(Bounded::stderr());
        let dropped = writer.error_counter();
        let subscriber = subscriber.with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer),
        );
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|_| InitError::AlreadyConfigured)?;
        place::install(move |info| {
            use std::io::Write as _;
            let location = place::of(info);
            let thread = place::thread_name();
            let _ = writeln!(
                std::io::stderr().lock(),
                "Peersus {role}: panic on thread {thread} at {location}"
            );
            incident::panic_report(thread, &location);
        });
        Guard {
            _worker: Some(worker),
            dropped: Some(dropped),
        }
    };
    #[cfg(target_os = "android")]
    let guard = {
        let tag = format!("peersus.{role}");
        let layer = tracing_android::layer(&tag).map_err(|_| InitError::Unavailable)?;
        #[cfg(any(debug_assertions, feature = "development"))]
        let subscriber = {
            use tracing_subscriber::{filter::filter_fn, layer::Layer};

            fn monotonic_ms() -> u64 {
                let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
                (now.tv_sec as u64).saturating_mul(1000) + (now.tv_nsec as u64 / 1_000_000)
            }

            let dev =
                tracing_android::layer(android_dev::TAG).map_err(|_| InitError::Unavailable)?;
            let sink = tracing::Dispatch::new(tracing_subscriber::registry().with(dev));
            subscriber
                .with(layer.with_filter(filter_fn(|event| !android_dev::reviewed(event))))
                .with(android_dev::Sink::new(role, sink, monotonic_ms))
        };
        #[cfg(not(any(debug_assertions, feature = "development")))]
        let subscriber = subscriber.with(layer);
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|_| InitError::AlreadyConfigured)?;
        // Android sends stderr nowhere, so a panic in a background task would vanish without a word. Its place in the
        // source is said, never its message, which could carry what was being handled. The place is worked out, and the
        // hook chained, by the same code that writes a development report on the desktop.
        place::install(|info| {
            tracing::error!(code = "panic", place = %place::of(info), "A task panicked");
        });
        repeated()
    };
    // Initialize the facade bridge separately: a pre-existing framework logger
    // must not discard an otherwise successfully installed subscriber/writer.
    if tracing_log::LogTracer::init().is_err() {
        tracing::warn!(
            code = "legacy-logger-present",
            "Legacy log bridge unavailable"
        );
    }
    *initialized = true;
    if invalid_filter {
        tracing::warn!(
            code = "invalid-diagnostic-filter",
            "Invalid diagnostic filter; safe defaults applied"
        );
    }
    tracing::info!(role, "Process diagnostics initialized");
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn action_outcome_categories_do_not_carry_guest_text() {
        use peersus_api::message::Outcome;
        for (code, expected) in [
            ("not-shown", "not-shown"),
            ("refused", "refused"),
            ("too-large", "too-large"),
            ("rate-limited", "rate-limited"),
            ("credential-rejected", "credential-rejected"),
            ("not-shown ", "failed"),
        ] {
            assert_eq!(
                outcome(&Outcome::error(code, "Do not log this text")),
                expected
            );
        }
    }

    struct Seen(Arc<Mutex<Vec<String>>>);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Seen {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let metadata = event.metadata();
            self.0
                .lock()
                .unwrap()
                .push(format!("{} {}", metadata.level(), metadata.target()));
        }
    }

    /// What gets through the filter for `value`, of a warning and a debug event of one target and a debug event of another.
    fn through(value: &str) -> Vec<String> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry()
            .with(filter_for(value).expect("a valid filter"))
            .with(Seen(Arc::clone(&seen)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "peersus::other", "a warning");
            tracing::debug!(target: "peersus::other", "a detail");
            tracing::debug!(target: "peersus::named", "a detail");
        });
        seen.lock().unwrap().clone()
    }

    /// A log file of this test alone, opened the way the window opens the owner's.
    fn log(name: &str) -> (std::path::PathBuf, std::fs::File) {
        let path = std::env::temp_dir().join(format!(
            "peersus-diagnostics-{}-{name}.log",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let file = open_log(&path).unwrap();
        (path, file)
    }

    #[test]
    fn a_log_file_of_unknown_path_past_its_limit_is_emptied_and_goes_on_with_the_newest_lines() {
        use std::io::Write;
        let (path, file) = log("bounded");
        let mut out = Bounded::over(file.try_clone().unwrap(), Some(file), None, 1000);
        for line in 0..400 {
            out.write_all(format!("line {line:04}\n").as_bytes())
                .unwrap();
        }
        let kept = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        // 400 lines of ten bytes are four times the limit: the file was emptied on the way and never stood far past it.
        assert!(kept.len() as u64 <= 1000 + 100, "{} bytes kept", kept.len());
        assert!(kept.starts_with("The log passed its size limit"));
        assert!(kept.ends_with("line 0399\n"), "the newest line is the last");
        assert!(!kept.contains("line 0000"));
        assert!(
            !previous_generation(&path).exists(),
            "no path was known to keep one beside"
        );
    }

    #[test]
    fn a_log_past_its_limit_keeps_exactly_one_previous_generation_and_the_pair_stays_bounded() {
        use std::io::Write;
        let (path, file) = log("generation");
        let previous = previous_generation(&path);
        let _ = std::fs::remove_file(&previous);
        let limit = 1000_u64;
        let mut out = Bounded::over(
            file.try_clone().unwrap(),
            Some(file),
            Some(path.clone()),
            limit,
        );
        let mut widest = 0;
        let mut rolled = false;
        for line in 0..400 {
            out.write_all(format!("line {line:04}\n").as_bytes())
                .unwrap();
            let held = std::fs::metadata(&path).unwrap().len()
                + std::fs::metadata(&previous).map_or(0, |found| found.len());
            widest = widest.max(held);
            rolled |= previous.exists();
        }
        let current = std::fs::read_to_string(&path).unwrap();
        let before = std::fs::read_to_string(&previous).unwrap();
        let mut names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with(path.file_name().unwrap().to_str().unwrap()))
            .collect();
        names.sort();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&previous).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&previous);
        assert!(rolled);
        // The log and its one previous generation, nothing else beside them (no half-written copy is left).
        assert_eq!(names.len(), 2, "{names:?}");
        // The previous generation holds the limit at most; the log is asked about once per check interval (here the
        // limit; 256 KiB in the program), so it stood at most that far past it.
        assert!(
            widest <= limit + 2 * limit + 200,
            "{widest} bytes at the widest"
        );
        assert!(before.len() as u64 <= limit);
        // Neither lost the line at the roll: the previous generation ends where the log goes on.
        let last_before: u32 = before.lines().last().unwrap()[5..].parse().unwrap();
        let first_now = current.lines().nth(1).unwrap()[5..].parse::<u32>().unwrap();
        assert_eq!(first_now, last_before + 1);
        assert!(current.starts_with("The log passed its size limit"));
        assert!(current.ends_with("line 0399\n"));
        // Only one generation: what the one before it held is gone.
        assert!(!before.contains("line 0000"));
    }

    #[test]
    fn a_file_left_large_by_an_earlier_run_is_rolled_when_the_program_starts_before_it_says_anything()
     {
        use std::io::Write;
        let (path, mut file) = log("inherited");
        let previous = previous_generation(&path);
        let _ = std::fs::remove_file(&previous);
        for line in 0..500 {
            writeln!(file, "old {line:04}").unwrap();
        }
        let mut out = Bounded::over(
            file.try_clone().unwrap(),
            Some(file),
            Some(path.clone()),
            1000,
        );
        let emptied = std::fs::read_to_string(&path).unwrap();
        assert_eq!(emptied, format!("{LOG_EMPTIED}\n"));
        let before = std::fs::read_to_string(&previous).unwrap();
        // Only the last 1000 bytes, starting at a whole line, were kept.
        assert!(
            before.len() <= 1000 && before.len() > 900,
            "{} bytes",
            before.len()
        );
        assert!(before.starts_with("old "));
        assert!(before.ends_with("old 0499\n"));
        writeln!(out, "first").unwrap();
        let kept = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&previous);
        assert_eq!(kept, format!("{LOG_EMPTIED}\nfirst\n"));
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_the_standard_error_file_keeps_nothing_beside_it() {
        use std::io::Write;
        let (path, file) = log("held");
        let (elsewhere, other) = log("elsewhere");
        drop(other);
        let mut out = Bounded::over(
            file.try_clone().unwrap(),
            Some(file),
            Some(elsewhere.clone()),
            1000,
        );
        for line in 0..400 {
            writeln!(out, "line {line:04}").unwrap();
        }
        let (held, wrong) = (path.clone(), previous_generation(&elsewhere));
        let rolled = (previous_generation(&path).exists(), wrong.exists());
        let _ = std::fs::remove_file(&held);
        let _ = std::fs::remove_file(&elsewhere);
        assert_eq!(rolled, (false, false));
    }

    #[test]
    fn what_is_not_a_file_is_written_and_never_measured() {
        use std::io::Write;
        let mut out = Bounded::over(Vec::new(), None, None, 10);
        out.write_all(&[b'y'; 100]).unwrap();
        assert_eq!(out.out.len(), 100);
    }

    #[test]
    fn naming_one_target_keeps_the_warnings_of_every_other() {
        assert_eq!(
            through("peersus::named=debug"),
            ["WARN peersus::other", "DEBUG peersus::named"]
        );
    }

    #[test]
    fn a_level_for_everything_replaces_the_default_and_an_empty_value_keeps_it() {
        assert_eq!(
            through("debug"),
            [
                "WARN peersus::other",
                "DEBUG peersus::other",
                "DEBUG peersus::named"
            ]
        );
        assert_eq!(through(""), ["WARN peersus::other"]);
        assert_eq!(through("off"), [] as [&str; 0]);
    }

    #[test]
    fn a_value_that_is_no_filter_is_refused() {
        assert!(filter_for("peersus=loud").is_none());
    }
}
