//! Keeps one plugin alive: socket, process, link and restarts, until told to stop.
use crate::{
    data,
    link::{self, Ended},
    supervisor::{Backoff, Bootstrap, Launch, Launcher, Lifecycle, Policy, Verdict},
};
use peersus_plugin_api::{hub::Hub, manifest::Manifest};
use std::{collections::BTreeSet, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    net::UnixListener,
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
    pub stream: tokio::net::UnixStream,
}

/// How many data handshakes may be in progress at once for one plugin.
const MAX_HANDSHAKES: usize = 16;

/// Everything needed to host one plugin.
pub struct Plugin {
    /// Identity the hub knows it by.
    pub id: String,
    pub manifest: Manifest,
    /// Permissions the user granted this plugin, each OF WHOEVER REQUIRES IT. Rechecked by
    /// the hub on every operation. It only seeds a plugin the user never decided about; once
    /// a decision exists the hub keeps it across restarts.
    pub grants: BTreeSet<peersus_plugin_api::permission::Permission>,
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
        if !matches!(
            plugin.manifest.execution,
            peersus_plugin_api::manifest::Execution::Process
        ) {
            return Err(std::io::Error::other(
                "a module requires its declared executor",
            ));
        }
        let program = plugin
            .program
            .clone()
            .ok_or_else(|| std::io::Error::other("an external plugin needs a launcher"))?;
        let launcher = Arc::new(Executable {
            program,
            arguments: plugin.arguments.clone(),
        });
        Self::start_with_launcher(hub, plugin, launcher)
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
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
                events.send_replace(Event::GaveUp);
                return;
            }
            Verdict::Restart(pause) => {
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
        return Cycle::CouldNotStart;
    };
    let Ok(channel) = hub.connect(&plugin.id, generation) else {
        return Cycle::CouldNotStart;
    };
    let socket = plugin.directory.join(format!("{}.sock", plugin.id));
    let data_socket = plugin.directory.join(format!("{}-data.sock", plugin.id));
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&data_socket);
    let Ok(listener) = UnixListener::bind(&socket) else {
        return Cycle::CouldNotStart;
    };
    let Ok(data_listener) = UnixListener::bind(&data_socket) else {
        let _ = std::fs::remove_file(&socket);
        return Cycle::CouldNotStart;
    };
    let secret = token();
    let launch = Bootstrap {
        socket: socket.clone(),
        data_socket: data_socket.clone(),
        token: secret.clone(),
    };
    let started = std::time::Instant::now();
    let Ok(mut process) = launcher.launch(launch) else {
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(&data_socket);
        return Cycle::CouldNotStart;
    };
    events.send_replace(Event::Started);
    // Data connections are accepted for as long as this cycle lasts. They are owned by this
    // block, so ending the cycle drops every handshake still in progress instead of leaving
    // tasks behind that hold a revoked channel.
    let data_loop = async {
        let mut handshakes = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = data_listener.accept() => {
                    let Ok((mut stream, _)) = accepted else { continue };
                    // A local flood of connections that never speak must not grow without
                    // limit: past the cap, further connections are simply closed.
                    if !same_user(&stream) || handshakes.len() >= MAX_HANDSHAKES {
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
                Some(_) = handshakes.join_next() => {}
            }
        }
    };
    let link = async {
        // Only the process we started can complete the handshake; anything else that
        // reaches the socket is dropped and the next connection is tried.
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return Ended::Misbehaved;
            };
            if !same_user(&stream) {
                continue;
            }
            let connected = || {
                events.send_replace(Event::Connected);
            };
            match link::run_with(stream, channel.clone(), &secret, connected).await {
                Ended::Misbehaved => continue,
                ended => return ended,
            }
        }
    };
    /// How the running part of a cycle ended, before the process has been dealt with. Private:
    /// it never leaves this function, so `Cycle` only holds what the supervisor decides on.
    enum Ran {
        /// The process ended by itself after this long.
        Exited(Duration),
        /// The link ended; the process may still be there.
        LinkEnded,
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
                _ = link => Ran::LinkEnded,
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
    let cycle = match ran {
        Ran::Exited(ran) => Cycle::Exited(ran),
        Ran::LinkEnded => {
            // The link ended first (the plugin closed or was revoked): make sure the
            // process is really gone before deciding what to do next.
            process.stop().await;
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
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&data_socket);
    cycle
}

fn same_user(stream: &tokio::net::UnixStream) -> bool {
    stream
        .peer_cred()
        .is_ok_and(|cred| cred.uid() == rustix::process::geteuid().as_raw())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_plugin_api::manifest::{EXECUTION_ABI_VERSION, Execution};

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
                    ..Manifest::default()
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
