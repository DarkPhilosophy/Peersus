//! Keeps one plugin alive: socket, process, link and restarts, until told to stop.
use crate::hub::Hub;
use crate::transport::Listener;
use crate::{
    data,
    link::{self, Ended},
    supervisor::{Backoff, Bootstrap, Launch, Launcher, Lifecycle, Policy, Verdict},
};
use peersus_api::manifest::Manifest;
use std::{collections::BTreeSet, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

/// What the user and the application can learn about a plugin's health. Fixed text only:
/// anything a plugin prints is never relayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The process was launched. It may not have connected yet, and a plugin that never
    /// does stays in this state: a launched process is not a usable plugin.
    Started,
    /// The process connected and identified itself, so the hub can reach it. Only now can
    /// a call be answered; this is what a screen should call "running".
    Connected,
    /// The process ended or its link broke; a restart is scheduled after the pause.
    Failed {
        restart_in: Duration,
    },
    /// Too many recent failures; the plugin stays stopped until the user restarts it.
    GaveUp,
    Stopped,
}
/// A data connection that proved who it came from and what it is entitled to name.
pub struct Accepted {
    /// The plugin this connection belongs to. Taken from the channel the connection was
    /// accepted for, so a plugin can never present itself as another.
    pub plugin: String,
    /// What the connection named: the hub's right to a call it answers, or a number the
    /// application issued.
    pub right: data::Right,
    /// The connection itself, ready for bytes. Carries nothing yet.
    pub stream: crate::transport::Stream,
}

/// How many data handshakes may be in progress at once for one plugin.
const MAX_HANDSHAKES: usize = 16;
/// How long the data listener is left alone after an accept that failed. An accept can keep
/// failing at once (the process is out of descriptors, the socket is gone): asked again
/// without a pause, the loop would take a processor for as long as that lasts.
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);

/// The connection an accept gave, or nothing once the pause after a failed one has passed.
async fn taken<T>(accepted: std::io::Result<T>) -> Option<T> {
    match accepted {
        Ok(connection) => Some(connection),
        Err(_) => {
            tokio::time::sleep(ACCEPT_PAUSE).await;
            None
        }
    }
}

/// Everything needed to host one plugin.
pub struct Plugin {
    /// Identity the hub knows it by.
    pub id: String,
    pub manifest: Manifest,
    /// Permissions the user granted this plugin, each OF WHOEVER REQUIRES IT. Rechecked by
    /// the hub on every operation. It only seeds a plugin the user never decided about; once
    /// a decision exists the hub keeps it across restarts.
    pub grants: BTreeSet<peersus_api::permission::Permission>,
    pub program: Option<PathBuf>,
    pub arguments: Vec<String>,
    /// Private directory the plugin's socket is created in (mode 0700, owned by us).
    pub directory: PathBuf,
    pub policy: Policy,
    /// Where accepted data connections are handed over. The application, which holds what a
    /// peer sent, pairs each with it by the call id the right names. `None` means this
    /// plugin takes no data connections and any that arrive are closed.
    pub handoff: Option<mpsc::Sender<Accepted>>,
}

struct Executable {
    program: PathBuf,
    arguments: Vec<String>,
}

impl Launcher for Executable {
    fn launch(&self, bootstrap: Bootstrap) -> std::io::Result<Box<dyn Lifecycle>> {
        Launch {
            program: self.program.clone(),
            arguments: self.arguments.clone(),
            socket: bootstrap.socket,
            data_socket: bootstrap.data_socket,
            token: bootstrap.token,
        }
        .spawn()
        .map(|running| Box::new(running) as Box<dyn Lifecycle>)
    }
}

/// A running, supervised plugin. Dropping it stops the plugin.
pub struct Service {
    task: Option<JoinHandle<()>>,
    events: watch::Receiver<Event>,
    stop: watch::Sender<bool>,
    restart: watch::Sender<u64>,
}

impl Service {
    /// Start supervising `plugin` on `hub`.
    ///
    /// Precondition: no other live `Service` uses the same `plugin.id` on this hub. A
    /// service cannot see its siblings, and two with one id would each revoke the
    /// other's registration on every restart and never settle. The owner of the services
    /// (the application's plugin host) must keep one service per id and refuse duplicates
    /// before calling this.
    pub fn start(hub: Hub, plugin: Plugin) -> std::io::Result<Self> {
        let launcher = Self::process_launcher(&plugin)?;
        Self::start_with_launcher(hub, plugin, launcher)
    }

    /// What [`Service::start`] launches `plugin` with: its program as a process of its own. Given apart so that the
    /// application can put a check of its own in front of every launch, the restarts included.
    pub fn process_launcher(plugin: &Plugin) -> std::io::Result<Arc<dyn Launcher>> {
        if !matches!(
            plugin.manifest.execution,
            peersus_api::manifest::Execution::Process
        ) {
            return Err(std::io::Error::other(
                "a module requires its declared executor",
            ));
        }
        let program = plugin
            .program
            .clone()
            .ok_or_else(|| std::io::Error::other("an external plugin needs a launcher"))?;
        Ok(Arc::new(Executable {
            program,
            arguments: plugin.arguments.clone(),
        }))
    }

    /// Start through a platform-owned isolated lifecycle, retaining the same sockets,
    /// authentication, generation revocation and restart policy as desktop processes.
    pub fn start_with_launcher(
        hub: Hub,
        plugin: Plugin,
        launcher: Arc<dyn Launcher>,
    ) -> std::io::Result<Self> {
        let (events_tx, events) = watch::channel(Event::Stopped);
        let (stop, stop_rx) = watch::channel(false);
        let (restart, restart_rx) = watch::channel(0u64);
        let plugin = Arc::new(plugin);
        let task = tokio::spawn(supervise(
            hub, plugin, launcher, events_tx, stop_rx, restart_rx,
        ));
        Ok(Self {
            task: Some(task),
            events,
            stop,
            restart,
        })
    }

    pub fn events(&self) -> watch::Receiver<Event> {
        self.events.clone()
    }

    /// Ask the supervisor to end the running process and start a new one.
    ///
    /// For a change that only a new process can pick up. Unlike a crash it uses up none of
    /// the budget meant for real failures and waits out no pause. Requests made while one is
    /// already being carried out merge into it, which is what asking twice should mean.
    /// Returns whether the supervisor was still running when asked. One that gave up or was
    /// stopped has finished and this cannot bring it back. The supervisor can still end in
    /// the instant between the check and the request, so `true` means "was running", not a
    /// promise that a new process will start.
    pub fn restart(&self) -> bool {
        if self.task.as_ref().is_some_and(|task| !task.is_finished()) {
            self.restart.send_modify(|count| *count += 1);
            true
        } else {
            false
        }
    }

    /// Stop the plugin and wait until its process and socket are gone.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

fn token() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Bring the plugin's socket directory back when something outside this process emptied it.
/// `Err` when the directory cannot be made private again; the caller treats it like any other
/// start failure.
fn restore_socket_directory(directory: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

async fn supervise(
    hub: Hub,
    plugin: Arc<Plugin>,
    launcher: Arc<dyn Launcher>,
    events: watch::Sender<Event>,
    mut stop: watch::Receiver<bool>,
    mut restart: watch::Receiver<u64>,
) {
    let mut backoff = Backoff::new(plugin.policy);
    loop {
        if *stop.borrow() {
            break;
        }
        // A request made before this cycle began is already answered by it starting; only
        // one made from now on may end it.
        restart.borrow_and_update();
        let outcome = run_once(&hub, &plugin, &*launcher, &events, &mut stop, &mut restart).await;
        // A new generation is registered on every start, so whatever the old process
        // held (calls, subscriptions, its channel) is revoked rather than inherited.
        hub.revoke(&plugin.id);
        let ran = match outcome {
            Cycle::Stopped => break,
            // On purpose, so it neither counts as a failure nor waits out a pause.
            Cycle::Restarted => continue,
            Cycle::Exited(ran) => ran,
            Cycle::CouldNotStart => Duration::ZERO,
        };
        match backoff.exited(ran, std::time::Instant::now()) {
            Verdict::GiveUp => {
                peersus_diagnostics::dev_trace!(
                    stage = "supervisor-suspend",
                    outcome = "failure-budget",
                    "Plugin lifecycle suspended"
                );
                events.send_replace(Event::GaveUp);
                return;
            }
            Verdict::Restart(pause) => {
                peersus_diagnostics::dev_trace!(
                    stage = "supervisor-backoff",
                    delay_ms = pause.as_millis() as u64,
                    "Plugin restart delayed"
                );
                events.send_replace(Event::Failed { restart_in: pause });
                tokio::select! {
                    () = tokio::time::sleep(pause) => {}
                    _ = stop.changed() => break,
                }
            }
        }
    }
    events.send_replace(Event::Stopped);
}

/// How long a requested restart waits for what was already on its way to the plugin before it
/// ends the process. A window, not a guarantee: see `run_once`.
const RESTART_WINDOW: Duration = Duration::from_millis(300);

/// How long a process whose link just ended is given to end by itself before it is stopped. A process that dies takes
/// its link with it and the link is usually seen first; without this moment the process would be killed before its own
/// ending (its exit code, the signal that ended it) could be read, and nothing would say why the extension stopped.
const EXIT_GRACE: Duration = Duration::from_millis(250);

/// How one start-to-exit cycle ended.
enum Cycle {
    /// The host asked to stop; this is not a failure.
    Stopped,
    /// The process ran, then ended on its own after this long.
    Exited(Duration),
    /// It could not be started at all (bad program path, socket not creatable, manifest
    /// refused). Counted like an instant crash, so it backs off and eventually gives up
    /// instead of looking like a clean stop.
    CouldNotStart,
    /// A restart was requested. Not counted as a failure, because nothing went wrong.
    Restarted,
}

/// One start-to-exit cycle.
async fn run_once(
    hub: &Hub,
    plugin: &Plugin,
    launcher: &dyn Launcher,
    events: &watch::Sender<Event>,
    stop: &mut watch::Receiver<bool>,
    restart: &mut watch::Receiver<u64>,
) -> Cycle {
    let Ok(generation) = hub.register(&plugin.id, plugin.manifest.clone(), plugin.grants.clone())
    else {
        peersus_diagnostics::dev_trace!(
            stage = "supervisor-start-failed",
            step = "register",
            "Plugin generation could not start"
        );
        return Cycle::CouldNotStart;
    };
    let Ok(channel) = hub.connect(&plugin.id, generation) else {
        peersus_diagnostics::dev_trace!(
            stage = "supervisor-start-failed",
            step = "connect",
            generation,
            "Plugin generation could not start"
        );
        return Cycle::CouldNotStart;
    };
    peersus_diagnostics::dev_trace!(
        stage = "supervisor-start",
        generation,
        "Plugin generation starting"
    );
    // The socket files live under the plugin directory, which anything outside this process can
    // empty (a /tmp cleanup, a person). A listener keeps working over its descriptor, but nothing
    // can connect to it once its path is gone: the process it serves is told the path once, so a
    // deleted path means a healthy-looking plugin that can never open a data stream again. The
    // directory is restored before every bind, and a watcher below asks each loop to rebind if
    // its socket file disappears while the cycle runs.
    if let Err(reason) = restore_socket_directory(&plugin.directory) {
        peersus_diagnostics::dev_trace!(
            stage = "supervisor-start-failed",
            step = "socket-directory",
            generation,
            reason = reason.to_string(),
            "Plugin generation could not start"
        );
        return Cycle::CouldNotStart;
    }
    // `@` is never in a plugin id, so one plugin's data socket can never be another plugin's control socket (ids may hold
    // `-`: the data socket of `a` was once `a-data.sock`, the control socket of a plugin called `a-data`).
    let Ok(listener) = Listener::bind(&plugin.directory.join(format!("{}.sock", plugin.id))) else {
        peersus_diagnostics::dev_trace!(
            stage = "supervisor-start-failed",
            step = "control-listener",
            generation,
            "Plugin generation could not start"
        );
        return Cycle::CouldNotStart;
    };
    let Ok(data_listener) =
        Listener::bind(&plugin.directory.join(format!("{}@data.sock", plugin.id)))
    else {
        peersus_diagnostics::dev_trace!(
            stage = "supervisor-start-failed",
            step = "data-listener",
            generation,
            "Plugin generation could not start"
        );
        return Cycle::CouldNotStart;
    };
    let secret = token();
    let launch = Bootstrap {
        socket: listener.endpoint().to_owned(),
        data_socket: data_listener.endpoint().to_owned(),
        token: secret.clone(),
    };
    let started = std::time::Instant::now();
    let Ok(mut process) = launcher.launch(launch) else {
        peersus_diagnostics::dev_trace!(
            stage = "supervisor-start-failed",
            step = "launch",
            generation,
            "Plugin generation could not start"
        );
        return Cycle::CouldNotStart;
    };
    events.send_replace(Event::Started);
    peersus_diagnostics::dev_trace!(
        stage = "supervisor-spawned",
        generation,
        "Plugin process started"
    );
    // Data connections are accepted for as long as this cycle lasts. They are owned by this
    // block, so ending the cycle drops every handshake still in progress instead of leaving
    // tasks behind that hold a revoked channel.
    // A deleted socket path makes every new connection fail (ENOENT) while the listener's
    // descriptor keeps accepting nothing; a deleted parent directory is restored by the watcher
    // first, and this arm rebinds the file at the same path, which is the only path the plugin
    // knows. Rebinding replaces the descriptor, not the path: established connections are not
    // touched.
    let mut control_fix = mpsc::unbounded_channel::<()>();
    let mut data_fix = mpsc::unbounded_channel::<()>();
    let control_path = listener.endpoint().to_owned();
    let data_path = data_listener.endpoint().to_owned();
    let mut data_listener = data_listener;
    let mut listener = listener;
    let data_loop = async {
        let mut handshakes = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = data_listener.accept() => {
                    let Some(mut stream) = taken(accepted).await else { continue };
                    // A local flood of connections that never speak must not grow without
                    // limit: past the cap, further connections are simply closed.
                    if handshakes.len() >= MAX_HANDSHAKES {
                        continue;
                    }
                    let (channel, secret, handoff) =
                        (channel.clone(), secret.clone(), plugin.handoff.clone());
                    handshakes.spawn(async move {
                        let Some(handoff) = handoff else { return };
                        if let Ok(right) = data::accept(&mut stream, &channel, &secret).await {
                            // Who is presenting is the channel this connection was accepted
                            // for, never something the plugin wrote.
                            let plugin = channel.owner().to_owned();
                            // The receiver may be gone, and then there is nobody to give it
                            // to; the right and the connection are simply dropped.
                            let _ = handoff
                                .send(Accepted {
                                    plugin,
                                    right,
                                    stream,
                                })
                                .await;
                        }
                    });
                }
                Some(_) = data_fix.1.recv() => {
                    if !data_path.exists()
                        && let Ok(rebound) = Listener::bind(&data_path)
                    {
                        // The listener being replaced must NOT be dropped: its Drop removes
                        // the socket file, which is now the new listener's file. One forgotten
                        // descriptor per repair is the price of keeping the path.
                        std::mem::forget(std::mem::replace(&mut data_listener, rebound));
                    }
                }
                Some(_) = handshakes.join_next() => {}
            }
        }
    };
    let link = async {
        // Only the process we started can complete the handshake; anything else that
        // reaches the socket is dropped and the next connection is tried.
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                Some(_) = control_fix.1.recv() => {
                    if !control_path.exists()
                        && let Ok(rebound) = Listener::bind(&control_path)
                    {
                        // As in the data loop: forgetting, not dropping, the replaced
                        // listener, so its Drop cannot unlink the file the new one just bound.
                        std::mem::forget(std::mem::replace(&mut listener, rebound));
                    }
                    continue;
                }
            };
            let Ok(stream) = accepted else {
                return Ended::Misbehaved;
            };
            let connected = || {
                events.send_replace(Event::Connected);
                peersus_diagnostics::dev_trace!(
                    stage = "supervisor-connected",
                    generation,
                    "Plugin control link authenticated"
                );
            };
            match link::run_with(stream, channel.clone(), &secret, connected).await {
                Ended::Misbehaved => continue,
                ended => return ended,
            }
        }
    };
    // A socket file gone for a watcher period is re-bound; a whole directory gone is restored
    // first, so bind has a place to put it. The watcher's period is short enough that a cleanup
    // empties the directory once and the plugins recover on their own.
    #[cfg(unix)]
    let repair = {
        const SOCKET_WATCH: Duration = Duration::from_secs(5);
        let directory = plugin.directory.clone();
        let (control_path, data_path) = (control_path.clone(), data_path.clone());
        async move {
            loop {
                tokio::time::sleep(SOCKET_WATCH).await;
                if control_path.exists() && data_path.exists() {
                    continue;
                }
                peersus_diagnostics::dev_trace!(
                    stage = "supervisor-sockets",
                    generation,
                    "Plugin socket file missing; restoring"
                );
                let _ = restore_socket_directory(&directory);
                let _ = control_fix.0.send(());
                let _ = data_fix.0.send(());
            }
        }
    };
    // Socket files can only vanish where they are files: on other platforms the endpoint is a
    // loopback port, and there is nothing to watch.
    #[cfg(not(unix))]
    let repair = std::future::pending::<()>();
    /// How the running part of a cycle ended, before the process has been dealt with. Private:
    /// it never leaves this function, so `Cycle` only holds what the supervisor decides on.
    enum Ran {
        /// The process ended by itself after this long.
        Exited(Duration),
        /// The link ended, and how; the process may still be there.
        LinkEnded(Ended),
        /// The host asked to stop.
        Stop,
        /// The host asked for a restart.
        Restart,
    }
    let ran = {
        let mut running = std::pin::pin!(async {
            tokio::select! {
                _ = data_loop => unreachable!("the data accept loop only ends when the cycle does"),
                () = process.wait() => Ran::Exited(started.elapsed()),
                ended = link => Ran::LinkEnded(ended),
                _ = repair => unreachable!("the socket watcher only ends when the cycle does"),
            }
        });
        tokio::select! {
            ran = &mut running => ran,
            _ = stop.changed() => Ran::Stop,
            _ = restart.changed() => {
                // Asked for, so it is neither a crash nor a clean stop. What was already on
                // its way to the plugin is given a short window to arrive before the process
                // is ended: the link keeps running meanwhile, and a plugin that leaves by
                // itself in that time has done what was asked. This is best effort and says
                // so: there is no acknowledgement, so nothing here can know the plugin READ
                // what it was sent, and one still there at the end of the window is stopped
                // all the same. What the application promises is the restart, not the read.
                let _ = tokio::time::timeout(RESTART_WINDOW, &mut running).await;
                Ran::Restart
            }
        }
    };
    peersus_diagnostics::dev_trace!(
        stage = "supervisor-end",
        generation,
        elapsed_ms = started.elapsed().as_millis() as u64,
        outcome = match &ran {
            Ran::Exited(_) => "exited",
            Ran::LinkEnded(_) => "link-ended",
            Ran::Stop => "stop",
            Ran::Restart => "restart",
        },
        "Plugin generation ending"
    );
    let cycle = match ran {
        Ran::Exited(ran) => Cycle::Exited(ran),
        Ran::LinkEnded(Ended::Revoked) => {
            // The hub withdrew the channel (the plugin was replaced or removed): the process is no longer wanted.
            process.stop().await;
            Cycle::Exited(started.elapsed())
        }
        Ran::LinkEnded(ended) => {
            // The plugin's side of the link ended first. A process that is ending says how in `Running::wait`; one still
            // there after `EXIT_GRACE` is stopped, and why its link ended is written instead: closed words, never what
            // the plugin sent.
            if tokio::time::timeout(EXIT_GRACE, process.wait())
                .await
                .is_err()
            {
                peersus_diagnostics::tracing::warn!(
                    link = if ended == Ended::Closed {
                        "closed"
                    } else {
                        "misbehaved"
                    },
                    ran_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    "A plugin's link ended while its process still ran; the process is stopped"
                );
                process.stop().await;
            }
            Cycle::Exited(started.elapsed())
        }
        Ran::Stop => {
            process.stop().await;
            Cycle::Stopped
        }
        Ran::Restart => {
            // Whether it left during the window or is still there, it ends here, and as a
            // restart: the budget meant for real failures is not touched.
            process.stop().await;
            Cycle::Restarted
        }
    };
    // Dropping the listeners removes their socket files.
    drop((listener, data_listener));
    cycle
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_api::manifest::{EXECUTION_ABI_VERSION, Execution};

    #[tokio::test(start_paused = true)]
    async fn an_accept_that_keeps_failing_is_asked_again_only_after_a_pause() {
        let began = tokio::time::Instant::now();
        // A connection costs no time.
        assert_eq!(taken(Ok(7)).await, Some(7));
        assert_eq!(began.elapsed(), Duration::ZERO);
        // An accept that fails at once, ten times over, is ten pauses and not a spin.
        for _ in 0..10 {
            let failed: std::io::Result<u8> = Err(std::io::ErrorKind::Other.into());
            assert_eq!(taken(failed).await, None);
        }
        assert_eq!(began.elapsed(), ACCEPT_PAUSE * 10);
    }

    #[tokio::test]
    async fn module_formats_never_fall_back_to_executing_the_payload_as_a_program() {
        for execution in [
            Execution::Wasm {
                abi: EXECUTION_ABI_VERSION,
            },
            Execution::NativeLibrary {
                abi: EXECUTION_ABI_VERSION,
            },
        ] {
            let directory = tempfile::tempdir().unwrap();
            let plugin = Plugin {
                id: "module".into(),
                manifest: Manifest {
                    execution,
                    ..Manifest::new(peersus_api::manifest::Execution::Process)
                },
                grants: BTreeSet::new(),
                program: Some(PathBuf::from("/bin/sh")),
                arguments: vec!["-c".into(), "exit 0".into()],
                directory: directory.path().to_owned(),
                policy: Policy::default(),
                handoff: None,
            };
            assert!(
                Service::start(Hub::default(), plugin).is_err(),
                "a module without its explicit executor must not be spawned as native code"
            );
        }
    }
}
