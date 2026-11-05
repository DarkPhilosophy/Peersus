//! Shared diagnostic emission configuration; applications own initialization.
//!
//! Initialize once at each process entry, before spawning tasks. Keep the returned
//! guard until shutdown: desktop output uses a bounded off-thread writer. Android
//! writes directly to its native Logcat backend, never through an assumed stderr
//! redirect. Diagnostic fields must be explicitly safe at the emission site.
use std::{fmt, sync::Mutex};
use tracing_subscriber::{
    EnvFilter,
    filter::{LevelFilter, Targets},
    prelude::*,
};

static INITIALIZED: Mutex<bool> = Mutex::new(false);

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

fn repeated() -> Guard {
    Guard {
        #[cfg(not(target_os = "android"))]
        _worker: None,
        #[cfg(not(target_os = "android"))]
        dropped: None,
    }
}

/// Configure reviewed first-party diagnostics. `PEERSUS_LOG` accepts ordinary
/// tracing level/target directives; dependencies remain excluded even at trace.
/// Release output is warn/error only; Android debug builds also expose owner
/// context and gesture traces without requiring process environment injection.
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
    let default_filter = if cfg!(all(
        target_os = "android",
        any(debug_assertions, feature = "android-debug")
    )) {
        "warn,peersus_app::runtime=debug,peersus_net::picked=debug,peersus_android=debug,peersus_ui::gesture=trace"
    } else {
        "warn"
    };
    let mut invalid_filter = false;
    let filter = match std::env::var("PEERSUS_LOG") {
        Ok(value) => EnvFilter::builder()
            .with_regex(false)
            .with_default_directive(LevelFilter::WARN.into())
            .parse(value)
            .unwrap_or_else(|_| {
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
        .with(filter);
    #[cfg(not(target_os = "android"))]
    let guard = {
        let (writer, worker) = tracing_appender::non_blocking::NonBlockingBuilder::default()
            .buffered_lines_limit(1024)
            .lossy(true)
            .thread_name("diagnostic-writer")
            .finish(std::io::stderr());
        let dropped = writer.error_counter();
        let subscriber = subscriber.with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer),
        );
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|_| InitError::AlreadyConfigured)?;
        Guard {
            _worker: Some(worker),
            dropped: Some(dropped),
        }
    };
    #[cfg(target_os = "android")]
    let guard = {
        let tag = format!("peersus.{role}");
        let layer = tracing_android::layer(&tag).map_err(|_| InitError::Unavailable)?;
        tracing::subscriber::set_global_default(subscriber.with(layer))
            .map_err(|_| InitError::AlreadyConfigured)?;
        // Android sends stderr nowhere, so a panic in a background task would vanish without a word. Its place in the
        // source is said, never its message, which could carry what was being handled.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let place = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_default();
            tracing::error!(code = "panic", place = %place, "A task panicked");
            previous(info);
        }));
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
