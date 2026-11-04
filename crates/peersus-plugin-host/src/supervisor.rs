//! Starts one plugin as its own process, watches it and restarts it when it dies.
//!
//! This gives crash isolation, not a security sandbox: a plugin process still runs with
//! the user's rights. What it guarantees is that a plugin which panics, aborts, hangs,
//! runs out of memory or is killed cannot take the application down with it, and that a
//! plugin which keeps failing is stopped instead of restarted forever.
use std::{
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::process::{Child, Command};

/// One generation's private bootstrap. Deliberately not Debug: tokens must never be logged.
pub struct Bootstrap {
    pub socket: PathBuf,
    pub data_socket: PathBuf,
    pub token: String,
}

/// Platform-owned process creation. Android implementations bind a private Service,
/// not an executable and not an in-process plugin.
pub trait Launcher: Send + Sync {
    fn launch(&self, bootstrap: Bootstrap) -> std::io::Result<Box<dyn Lifecycle>>;
}

/// Ownership of one isolated plugin generation. Drop must request termination even when
/// the supervisor future is cancelled; stop must wait for that generation to end.
pub trait Lifecycle: Send {
    fn wait(&mut self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>>;
    fn stop(&mut self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

impl Lifecycle for Running {
    fn wait(&mut self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            Running::wait(self).await;
        })
    }
    fn stop(&mut self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(Running::stop(self))
    }
}

/// What to run. The program is always an absolute path the host chose, never a name
/// looked up through `PATH`, and the token travels only through the environment.
#[derive(Clone, Debug)]
pub struct Launch {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    /// Socket the plugin connects to, passed as `PEERSUS_PLUGIN_SOCKET`.
    pub socket: PathBuf,
    /// Socket for bulk data, passed as `PEERSUS_PLUGIN_DATA_SOCKET`. It accepts only a
    /// connection that names a call in flight and shows the same token.
    pub data_socket: PathBuf,
    /// Secret proving identity, passed as `PEERSUS_PLUGIN_TOKEN`.
    pub token: String,
}

/// Restart policy: pause doubles after each failure, and a plugin that fails too often
/// in a short time is given up on.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub first_delay: Duration,
    pub max_delay: Duration,
    /// More than this many failures inside `window` stops the plugin for good.
    pub max_failures: u32,
    pub window: Duration,
    /// A run at least this long counts as healthy and resets the pause.
    pub healthy_after: Duration,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            first_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(30),
            max_failures: 5,
            window: Duration::from_secs(60),
            healthy_after: Duration::from_secs(10),
        }
    }
}

/// Decides what to do after each exit. Pure, so every policy rule is testable without
/// starting a process.
#[derive(Debug)]
pub struct Backoff {
    policy: Policy,
    delay: Duration,
    failures: Vec<Instant>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Start again after this pause.
    Restart(Duration),
    /// Too many recent failures; leave it stopped and tell the user.
    GiveUp,
}

impl Backoff {
    pub fn new(policy: Policy) -> Self {
        Self {
            delay: policy.first_delay,
            policy,
            failures: Vec::new(),
        }
    }

    /// Record that the process ended after running for `ran`, at time `now`.
    pub fn exited(&mut self, ran: Duration, now: Instant) -> Verdict {
        if ran >= self.policy.healthy_after {
            // It was healthy for a while, so this failure starts a fresh history.
            self.delay = self.policy.first_delay;
            self.failures.clear();
        }
        self.failures
            .retain(|at| now.saturating_duration_since(*at) < self.policy.window);
        self.failures.push(now);
        if self.failures.len() as u32 > self.policy.max_failures {
            return Verdict::GiveUp;
        }
        let pause = self.delay;
        self.delay = (self.delay * 2).min(self.policy.max_delay);
        Verdict::Restart(pause)
    }
}

/// A running plugin process and the group it leads.
pub struct Running {
    child: Child,
    started: Instant,
    /// Set once the process was collected; the group id is then no longer ours to signal.
    reaped: bool,
    #[cfg(unix)]
    group: rustix::process::Pid,
}

impl Launch {
    /// Start the process in its own group so every descendant can be stopped with it.
    pub fn spawn(&self) -> std::io::Result<Running> {
        self.spawn_with_module(None)
    }

    /// Launch a host-selected helper with an exact module path, never through a shell.
    pub fn spawn_module(&self, module: &std::path::Path) -> std::io::Result<Running> {
        self.spawn_with_module(Some(module))
    }

    fn spawn_with_module(&self, module: Option<&std::path::Path>) -> std::io::Result<Running> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .env("PEERSUS_PLUGIN_SOCKET", &self.socket)
            .env("PEERSUS_PLUGIN_DATA_SOCKET", &self.data_socket)
            .env("PEERSUS_PLUGIN_TOKEN", &self.token)
            .stdin(Stdio::null())
            // Output is the plugin's own business and must never reach the protocol.
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(module) = module {
            command.arg(module);
        }
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn()?;
        #[cfg(unix)]
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
            .ok_or_else(|| std::io::Error::other("Plugin process has no usable id"))?;
        Ok(Running {
            child,
            started: Instant::now(),
            reaped: false,
            #[cfg(unix)]
            group,
        })
    }
}

impl Running {
    /// The live child identity, unavailable once it has been collected.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// How long the process has been running (or ran, once it has ended).
    pub fn ran_for(&self) -> Duration {
        self.started.elapsed()
    }

    /// Wait for the process to end and report how long it ran.
    pub async fn wait(&mut self) -> Duration {
        let _ = self.child.wait().await;
        // The leader has exited but its group lives while any child remains, so the id is
        // still ours: stop leftover children now. This also marks the group as handled.
        self.kill_group();
        self.ran_for()
    }

    /// Stop the process and everything it started. Safe to call more than once.
    ///
    /// On Unix the whole process group is signalled, so children the plugin started
    /// die with it. On other platforms only the process itself is stopped: there is no
    /// job-object containment yet, so descendants of a plugin may outlive it there.
    pub async fn stop(&mut self) {
        self.kill_group();
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }

    /// Signal the group at most once. After SIGKILL the group is doomed, and a second
    /// signal could only ever hit an unrelated process that received the recycled id, so
    /// this is marked done immediately rather than after the process is collected. That
    /// keeps every path safe: a stop cancelled mid-wait, a repeated stop, and Drop.
    fn kill_group(&mut self) {
        #[cfg(unix)]
        if !self.reaped {
            let _ = rustix::process::kill_process_group(self.group, rustix::process::Signal::KILL);
        }
        self.reaped = true;
    }
}

impl Drop for Running {
    /// A plugin handle that is abandoned (early return, panic, cancelled future) must not
    /// leave descendants behind.
    fn drop(&mut self) {
        self.kill_group();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            first_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
            max_failures: 3,
            window: Duration::from_secs(60),
            healthy_after: Duration::from_secs(10),
        }
    }

    #[test]
    fn pause_doubles_up_to_the_maximum() {
        let mut backoff = Backoff::new(policy());
        let start = Instant::now();
        let pauses: Vec<_> = (0..3)
            .map(|i| backoff.exited(Duration::from_millis(1), start + Duration::from_secs(i)))
            .collect();
        assert_eq!(
            pauses,
            [
                Verdict::Restart(Duration::from_millis(100)),
                Verdict::Restart(Duration::from_millis(200)),
                Verdict::Restart(Duration::from_millis(400)),
            ]
        );
    }

    #[test]
    fn too_many_quick_failures_give_up() {
        let mut backoff = Backoff::new(policy());
        let start = Instant::now();
        for i in 0..3 {
            assert!(matches!(
                backoff.exited(Duration::from_millis(1), start + Duration::from_secs(i)),
                Verdict::Restart(_)
            ));
        }
        assert_eq!(
            backoff.exited(Duration::from_millis(1), start + Duration::from_secs(4)),
            Verdict::GiveUp
        );
    }

    #[test]
    fn failures_outside_the_window_are_forgotten() {
        let mut backoff = Backoff::new(policy());
        let start = Instant::now();
        for i in 0..3 {
            backoff.exited(Duration::from_millis(1), start + Duration::from_secs(i));
        }
        // Long after, one more failure is a fresh problem, not the fourth in a row.
        assert!(matches!(
            backoff.exited(Duration::from_millis(1), start + Duration::from_secs(120)),
            Verdict::Restart(_)
        ));
    }

    #[test]
    fn a_healthy_run_resets_the_pause_and_the_failure_count() {
        let mut backoff = Backoff::new(policy());
        let start = Instant::now();
        for i in 0..3 {
            backoff.exited(Duration::from_millis(1), start + Duration::from_secs(i));
        }
        // It then ran fine for a while before dying: the history starts over.
        assert_eq!(
            backoff.exited(Duration::from_secs(30), start + Duration::from_secs(40)),
            Verdict::Restart(Duration::from_millis(100))
        );
    }
}
