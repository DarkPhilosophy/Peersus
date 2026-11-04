//! Single application owner shared by every frontend and the command line.
//!
//! The runtime owns the network host, saved preferences, the plugin hub and one
//! supervised process per plugin. It never names a plugin: inbound peer streams are
//! routed by the service a manifest exports, and what a *device* may do with a plugin is
//! decided here from the user's per-device grants. What a *plugin* may do to another
//! plugin is decided by the hub. A remote device is never a hub participant.
use crate::in_flight::{InFlight, Key, Refused};
use crate::parked::Parked;
use crate::plugin_host::{PluginSpec, Registry, Route, permitted};
use peersus_core::{
    model::{
        ActionFailure, ActionState, Command, FileDestination, Peer, PeerId, Snapshot, TransferState,
    },
    plugins::{Grants, PluginId, Value},
};
use peersus_net::{
    grants::StandaloneGrants,
    invitation::Invitation,
    network_host::{Event, NetworkHost},
    settings::SettingsStore,
    transfer::Source,
};
use peersus_plugin_api::{
    contributions::Scope,
    hub::{Channel, HOST, Hub},
    permission::Permission,
};
use peersus_plugin_host::{Accepted, Event as PluginEvent, Plugin, Service};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot, watch};

/// What a peer opened for a plugin: both halves of its stream, kept together until the
/// plugin comes to collect them.
pub(crate) struct Stream {
    pub(crate) send: iroh::endpoint::SendStream,
    pub(crate) receive: iroh::endpoint::RecvStream,
}

/// Which way a transfer carries bytes, and so which table its number belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    /// From a device into a plugin, numbered by the application's own call number.
    Inbound,
    /// From a plugin out to a device, numbered by the claim the application issued.
    Outbound,
}

/// Where a plugin's data connection goes once it arrives, for a send the application opened.
/// The task that opens the device's stream and carries the bytes waits on the other end.
#[cfg(unix)]
pub(crate) type Outbound = oneshot::Sender<tokio::net::UnixStream>;
/// There are no plugin processes here, so there is no connection to hand over.
#[cfg(not(unix))]
pub(crate) type Outbound = ();

/// How long a device has to let the application open a stream to it.
///
/// Opening is instant on a healthy connection, so this is generous. It exists for the device
/// that stays connected and never lets another stream be opened.
pub(crate) const OPEN_LIMIT: Duration = Duration::from_secs(15);

/// Wait for `step`, but no longer than `limit`: `None` if it did not finish in time.
pub(crate) async fn within<T>(limit: Duration, step: impl Future<Output = T>) -> Option<T> {
    tokio::time::timeout(limit, step).await.ok()
}

/// The whole life of one send, as one task: wait for the plugin's data connection, open the
/// device's stream, and carry the bytes across.
///
/// The stream is opened only once the plugin has actually come, so a device is never shown an
/// empty stream for a plugin that never starts, and a plugin that does not come in time ends
/// the task without the device having been contacted at all. One task, from the moment the
/// number is issued, is what lets withdrawing a permission or forgetting the device stop it
/// at any point: before the plugin comes, while the stream is being opened, or while bytes
/// are moving.
#[cfg(unix)]
async fn carry_out(
    connection: iroh::endpoint::Connection,
    request: SendRequest,
    plugin: oneshot::Receiver<tokio::net::UnixStream>,
    open_limit: Duration,
) -> crate::receive::Delivery {
    use crate::receive::Delivery;
    let wait = tokio::time::timeout(crate::parked::MAX_WAIT, plugin).await;
    let Ok(Ok(plugin)) = wait else {
        return Delivery::Failed("A plugin did not start sending in time");
    };
    // Opening a stream waits for the device to allow one more, and a device that stays
    // connected but never does would hold this task, and the plugin's socket with it, for
    // ever: the number has been spent, so expiry no longer reaches it, and the copy's own
    // idle limit has not begun. Only this step is bounded; a transfer that keeps making
    // progress is not given a total time.
    let Some(Ok((mut send, _receive))) = within(open_limit, connection.open_bi()).await else {
        return Delivery::Failed("A device could not be reached to send to it");
    };
    let preamble = peersus_net::streams::Request::Plugin {
        service: request.service,
        version: request.version,
    };
    if peersus_net::streams::send(&mut send, &preamble)
        .await
        .is_err()
    {
        return Delivery::Failed("A device could not be reached to send to it");
    }
    // `pump::copy` shuts the stream down when the plugin has finished, which is what ends
    // the device's side cleanly.
    crate::receive::send_out(
        plugin,
        send,
        crate::receive::DEFAULT_LIMIT,
        peersus_plugin_host::pump::IDLE_LIMIT,
    )
    .await
}

/// Invitations are deliberately not Debug: their short-lived bearer is private.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum Reply {
    Applied,
    Invitation(String),
    /// A files code: the code as text, the generation it is (what a snapshot's `files_code` and each transfer's `code` are
    /// compared with) and the second it stops admitting anyone. The owner publishes nothing when a code runs out unscanned, so
    /// the window needs the deadline to stop drawing it. Generation and deadline are no secret; the code is.
    FilesInvitation {
        code: String,
        generation: u64,
        expires_at: u64,
    },
    Transfers(Vec<String>),
    /// A bounded producer should wait for an owner update and retry; no source was consumed.
    QueueBusy,
    /// The pixels of a picture a window asked for by its digest.
    Picture(peersus_core::profile::Picture),
}
pub(crate) struct Request {
    pub(crate) command: Command,
    pub(crate) result: oneshot::Sender<Result<Reply, &'static str>>,
}

#[derive(Clone)]
pub struct Client {
    pub(crate) requests: mpsc::Sender<Request>,
    pub(crate) snapshots: watch::Receiver<Arc<Snapshot>>,
}
impl Client {
    /// Bounded channel; GUI callers spawn this future rather than block rendering.
    pub async fn command(&self, command: Command) -> Result<Reply, &'static str> {
        let (result, response) = oneshot::channel();
        self.requests
            .send(Request { command, result })
            .await
            .map_err(|_| "Application stopped")?;
        response.await.map_err(|_| "Application stopped")?
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.snapshots.borrow())
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.snapshots.clone()
    }
}

/// One running plugin process and the health we last heard about it.
struct Running {
    service: Service,
    health: watch::Receiver<PluginEvent>,
}

/// A place that keeps one secret line between runs, for the access token of a personal relay. The word of the store is
/// final: a token is used only once it is saved, so that what runs is what will be there after a restart.
pub trait TokenStore: Send + Sync {
    /// Save this token, or forget the saved one when it is empty.
    fn save(&self, token: &str) -> Result<(), &'static str>;
}

pub struct Runtime {
    network: NetworkHost,
    /// What the user granted plugins that is about no device. Owned here and not by the network
    /// host, which is about devices and connections.
    standalone: StandaloneGrants,
    settings: SettingsStore,
    plugins: Registry,
    requests: mpsc::Receiver<Request>,
    snapshots: watch::Sender<Arc<Snapshot>>,
    errors: Vec<String>,
    /// What the plugins were last told of the transfers, so that only a change is told. `None` means nobody has been listening or nothing has been seen
    /// yet: the next look is a FIRST look, which tells only what is still going and never the history (see [`crate::transfer_events::to_tell`]). It is
    /// `None` again whenever no plugin asks for the service, so a plugin that comes later is told what is going and not a stale difference. Interior,
    /// because `publish` takes `&self` and is called from everywhere.
    told_transfers: std::cell::RefCell<Option<Vec<peersus_core::model::Transfer>>>,
    /// What the plugins that listen to statuses were last told every device says of itself, so that only a change is told, once.
    told_statuses: std::cell::RefCell<Option<crate::status_service::Seen>>,
    hub: Hub,
    /// The application's own seat on the hub. Plugins reach the application through it.
    host: Channel,
    /// Private directory plugin sockets live in. Owned by the launcher, never created here.
    plugin_directory: PathBuf,
    running: HashMap<PluginId, Running>,
    launchers: HashMap<PluginId, Arc<dyn peersus_plugin_host::supervisor::Launcher>>,
    modules: Option<Arc<dyn peersus_plugin_host::module::ModuleFactory>>,
    /// Where the personal relay's access token is kept between runs, when this platform has such a place.
    token_store: Option<Arc<dyn TokenStore>>,
    /// Platform UI and I/O are awaited in separately owned, cancellable tasks.
    platform_services: Option<Arc<dyn crate::platform_services::PlatformServices>>,
    platform_calls: crate::platform_calls::PlatformCalls,
    /// Ids of calls the application makes through the hub. Never reused, never zero.
    next_call: u64,
    /// Calls to plugins that have not ended yet, to refuse a repeat and to withdraw them.
    in_flight: InFlight,
    /// What became of each action the user started, for the screen.
    board: crate::board::Board,
    /// What peers sent that is waiting for a plugin to collect it, by the call's number.
    parked: Parked<Stream>,
    /// Where every plugin's accepted data connections are handed over (a copy per plugin).
    handoff: mpsc::Sender<Accepted>,
    /// The receiving end of the above, read by the event loop.
    accepted: mpsc::Receiver<Accepted>,
    /// Transfers to plugins that are running. They run here, not in the event loop, so that
    /// carrying a large payload never stops commands, the network or the screen; and they
    /// belong to the runtime, so shutting it down ends every one of them.
    transfers: tokio::task::JoinSet<(Direction, u64, crate::receive::Delivery)>,
    /// The same transfers, by the call that announced them, with whose they are and a way to
    /// stop each, so that withdrawing a permission can reach one that is already running.
    carrying: crate::running::Running<tokio::task::AbortHandle>,
    /// What the application opened to a device for a plugin that asked to send, waiting for
    /// that plugin's data connection, by the number the application issued for it. Kept
    /// apart from `parked`, whose numbers are the application's own call numbers: the two
    /// kinds of number can never meet in one table.
    outbound: Parked<Outbound>,
    /// The sends that are running or about to, by that same number, with whose they are and
    /// a way to stop each. Separate from `carrying` for the same reason as `outbound`.
    carrying_out: crate::running::Running<tokio::task::AbortHandle>,
    /// The statuses plugins made for this device and which of them are on. Belongs to the plugin processes: it is emptied for a plugin when
    /// that plugin stops or starts again, so that nothing keeps saying what nobody keeps true.
    statuses: crate::statuses::Statuses,
    /// What each plugin calls each device. A secret that lives only in this run.
    aliases: crate::aliases::Aliases,
    /// The store of installed packages, when this owner was given one. Without it the commands
    /// that install and remove are refused, and nothing else about the owner changes.
    packages: Option<crate::plugin_manager::Manager>,
    /// Plugins the person switched off: registered and listed, and not started. Read from the
    /// record of installations at start, and kept in step with it when a plugin is switched.
    disabled: std::collections::BTreeSet<PluginId>,
    /// Plugins that were found in the store and are not run, with what each waits for. Not in the registry: only what may
    /// run is, so that a plugin that is not approved cannot be reached through it. Listed to the window, and removable.
    held_back: Vec<crate::discovery::HeldBack>,
    /// A package that was read and shown and is waiting to be confirmed, by plugin. Held here and
    /// not on disk, so that what is installed is exactly what was shown, and nothing else.
    staged: HashMap<PluginId, Staged>,
    /// Files that the system handed over already open, by the number each was kept under. Taken once, by `SendHeld`.
    held: peersus_net::held::Held,
    /// What each installed package says about itself, read from its folder. Kept so that `publish` does no
    /// disk work; refreshed whenever the set of installed packages can have changed. For the window only.
    installed_packages: std::collections::BTreeMap<PluginId, peersus_plugin_api::package::Package>,
    /// The extension store, when this owner was given one. Without it the store commands are refused
    /// and the window is told the store is not set up. Kept populated while an install runs: a
    /// snapshot published meanwhile must carry its real state.
    #[cfg(unix)]
    store: Option<StoreSlot>,
    /// Store downloads in flight (at most one: the service refuses a second). Each returns only
    /// unverified bytes, which the event loop verifies and installs.
    store_tasks: tokio::task::JoinSet<StoreOutcome>,
}

/// Declare, in the application's own manifest, a capability a plugin declared, carrying the
/// plugin's own description of it.
///
/// The words the user reads when granting are the ones the plugin's author wrote, copied and
/// never rewritten here: the application cannot shorten, soften or invent what a permission
/// means. A capability the plugin did not declare is not declared on its behalf; a registered
/// plugin cannot reach that case, because a manifest naming an undeclared capability is
/// refused when it is validated.
fn declare(
    manifest: &mut peersus_plugin_api::manifest::Manifest,
    plugin: &crate::plugin_host::PluginSpec,
    capability: &str,
) {
    if let Some(description) = plugin.manifest.capabilities.get(capability) {
        manifest
            .capabilities
            .insert(capability.to_owned(), description.clone());
    }
}

/// The application's own hub manifest, derived from what plugins declare.
///
/// For every action a plugin offers, the application imports exactly that method from
/// exactly that plugin, so the hub can route the user's choice to it, and declares the
/// capability that action needs so the user can grant it. Nothing is added beyond what
/// plugins themselves declared, and nothing is granted. Whether a call is then *allowed*
/// is still the hub's decision: the plugin must list the application as a consumer of
/// that export and the capability must have been granted, so a plugin that does not want
/// to be driven from the interface simply does not list it.
pub(crate) fn host_manifest(plugins: &Registry) -> peersus_plugin_api::manifest::Manifest {
    use peersus_plugin_api::manifest::{Import, Manifest};
    let mut manifest = Manifest::default();
    for spec in plugins.iter() {
        for action in &spec.manifest.contributions.actions {
            let service = &action.target.service;
            let Some(export) = spec.manifest.exports.iter().find(|e| &e.service == service) else {
                continue;
            };
            let position = manifest
                .imports
                .iter()
                .position(|i| i.provider == spec.id.as_str() && &i.service == service);
            let import = match position {
                Some(position) => &mut manifest.imports[position],
                None => {
                    manifest.imports.push(Import {
                        provider: spec.id.as_str().to_owned(),
                        service: service.clone(),
                        version: export.version,
                        methods: BTreeSet::new(),
                        events: BTreeSet::new(),
                    });
                    manifest.imports.last_mut().expect("just pushed")
                }
            };
            import.methods.insert(action.target.method.clone());
            // Declaring a capability only makes it possible to be granted one. Nothing is
            // granted here: that stays an explicit decision of the user.
            if let Some(capability) = &action.capability {
                declare(&mut manifest, spec, capability);
            }
        }
        // What a plugin takes in from peers is called by the application too, so the hub
        // must be able to route that call: import `receive` from every plugin that exports
        // it, and declare the capability that guards it so the user can grant it. Nothing
        // is imported for a plugin that offers no `receive`.
        for export in &spec.manifest.exports {
            let Some(guard) = export.methods.get(crate::plugin_host::RECEIVE_METHOD) else {
                continue;
            };
            let position = manifest
                .imports
                .iter()
                .position(|i| i.provider == spec.id.as_str() && i.service == export.service);
            let import = match position {
                Some(position) => &mut manifest.imports[position],
                None => {
                    manifest.imports.push(Import {
                        provider: spec.id.as_str().to_owned(),
                        service: export.service.clone(),
                        version: export.version,
                        methods: BTreeSet::new(),
                        events: BTreeSet::new(),
                    });
                    manifest.imports.last_mut().expect("just pushed")
                }
            };
            import
                .methods
                .insert(crate::plugin_host::RECEIVE_METHOD.to_owned());
            if let Some(capability) = &guard.capability {
                declare(&mut manifest, spec, capability);
            }
        }
    }
    // What the application offers plugins that asked for it. It obeys the rule it sets: the
    // permission that guards one of its methods is declared and described in its own manifest,
    // as a plugin's would be, and the words are the application's because it is the author.
    manifest.exports = host_exports(plugins);
    for (service, capability, description) in [
        (PEERS_SERVICE, PEERS_SEND_CAPABILITY, PEERS_SEND_DESCRIPTION),
        (
            PLUGINS_SERVICE,
            PLUGINS_RESTART_CAPABILITY,
            PLUGINS_RESTART_DESCRIPTION,
        ),
        (
            crate::transfer_events::TRANSFERS_SERVICE,
            crate::transfer_events::TRANSFERS_WATCH_CAPABILITY,
            crate::transfer_events::TRANSFERS_WATCH_DESCRIPTION,
        ),
        (
            crate::transfer_events::TRANSFERS_SERVICE,
            crate::transfer_events::TRANSFERS_NAMES_CAPABILITY,
            crate::transfer_events::TRANSFERS_NAMES_DESCRIPTION,
        ),
        (
            crate::status_service::STATUS_SERVICE,
            crate::status_service::STATUS_DEFINE_CAPABILITY,
            crate::status_service::STATUS_DEFINE_DESCRIPTION,
        ),
        (
            crate::status_service::STATUS_SERVICE,
            crate::status_service::STATUS_SET_CAPABILITY,
            crate::status_service::STATUS_SET_DESCRIPTION,
        ),
        (
            crate::status_service::STATUS_SERVICE,
            crate::status_service::STATUS_WATCH_CAPABILITY,
            crate::status_service::STATUS_WATCH_DESCRIPTION,
        ),
    ]
    .into_iter()
    .chain([
        (
            crate::platform_services::FILES_SERVICE,
            crate::platform_services::FILES_CAPABILITY,
            crate::platform_services::FILES_DESCRIPTION,
        ),
        (
            crate::platform_services::HTTP_SERVICE,
            crate::platform_services::HTTP_CAPABILITY,
            crate::platform_services::HTTP_DESCRIPTION,
        ),
    ]) {
        if manifest.exports.iter().any(|e| e.service == service) {
            manifest
                .capabilities
                .insert(capability.to_owned(), description.to_owned());
        }
    }
    manifest
}

/// The service the application offers plugins about the paired devices.
pub const PEERS_SERVICE: &str = "peersus.peers";
/// Read-only platform facts, not handles, paths or active privilege grants.
pub const PLATFORM_SERVICE: &str = "peersus.platform";
/// Caller-scoped effective configuration, never a dump of saved preferences.
pub const CONFIG_SERVICE: &str = "peersus.config";
/// The capability that guards `send`. It names no device: the user grants it to a plugin for
/// each device separately, exactly as for what a device may do through a plugin.
pub const PEERS_SEND_CAPABILITY: &str = "peers.send";
/// What that permission lets a plugin do, in the words a person reads when granting it. One
/// copy, because the application's manifest and anything that shows the permission must say
/// the same thing.
pub const PEERS_SEND_DESCRIPTION: &str =
    "Send data to a paired device you choose. It cannot see any device's key.";

/// The service the application offers plugins about the other plugins.
pub const PLUGINS_SERVICE: &str = "peersus.plugins";
/// The capability that guards `restart`. Unlike `peers.send` it is about no device at all, so
/// the user grants it to a plugin once, whether or not any device is paired.
pub const PLUGINS_RESTART_CAPABILITY: &str = "plugins.restart";
/// What that permission lets a plugin do. It is among the most a plugin can be allowed, so
/// the words say how much, in the same copy for the manifest and for whatever shows it.
pub const PLUGINS_RESTART_DESCRIPTION: &str = "Interrupt and restart ANY plugin, including ones you did not install for this purpose. \
     It cannot read their data or change what they are allowed to do.";

/// The event `peersus.plugins` publishes when a plugin's state changes.
pub const PLUGINS_STATE_CHANGED: &str = "state-changed";

/// The plugins that asked the application for version 1 of one of its services.
fn consumers_of(plugins: &Registry, service: &str) -> BTreeSet<String> {
    plugins
        .iter()
        .filter(|spec| {
            spec.manifest.imports.iter().any(|import| {
                import.provider == HOST && import.service == service && import.version == 1
            })
        })
        .map(|spec| spec.id.as_str().to_owned())
        .collect()
}

/// What the application exports to plugins, derived from which of them asked for it.
///
/// Each service is offered only to the plugins that imported it; with no such plugin that
/// service is not offered at all, and the two are independent. Only the methods the
/// application really has are offered. `peersus.peers` `list` shows a device's name and
/// whether it is online but never a key; `peersus.plugins` `list` shows a plugin's id and
/// state but never its manifest, permissions or location. `send` and `restart` are guarded,
/// so a plugin that asks for them can still do nothing until the user grants them.
pub(crate) fn host_exports(plugins: &Registry) -> Vec<peersus_plugin_api::manifest::Export> {
    use peersus_plugin_api::manifest::{Export, Member};
    use std::collections::BTreeMap;
    let mut exports = Vec::new();
    let consumers = consumers_of(plugins, PLATFORM_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: PLATFORM_SERVICE.into(),
            version: 1,
            description:
                "Portable operating system and architecture; not facilities or permission grants."
                    .into(),
            consumers,
            methods: BTreeMap::from([(
                "inspect".into(),
                Member {
                    capability: None,
                    description: "Reports conservative platform facts only.".into(),
                },
            )]),
            events: BTreeMap::new(),
        });
    }
    let consumers = consumers_of(plugins, CONFIG_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: CONFIG_SERVICE.into(),
            version: 1,
            description: "Reads only the caller's declared effective settings and peer aliases."
                .into(),
            consumers,
            methods: BTreeMap::from([(
                "get".into(),
                Member {
                    capability: None,
                    description: "Returns validated settings for this caller only.".into(),
                },
            )]),
            events: BTreeMap::new(),
        });
    }
    let consumers = consumers_of(plugins, PEERS_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: PEERS_SERVICE.to_owned(),
            version: 1,
            description: "Lets a plugin see which paired devices exist and send them data. It \
                          never shows a device's key."
                .to_owned(),
            consumers,
            methods: BTreeMap::from([
                (
                    "list".to_owned(),
                    Member {
                        capability: None,
                        description: "Lists paired devices by name and whether each is online."
                            .to_owned(),
                    },
                ),
                (
                    "send".to_owned(),
                    Member {
                        capability: Some(PEERS_SEND_CAPABILITY.to_owned()),
                        description: "Sends data to one paired device the user chose.".to_owned(),
                    },
                ),
            ]),
            events: BTreeMap::new(),
        });
    }
    let consumers = consumers_of(plugins, PLUGINS_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: PLUGINS_SERVICE.to_owned(),
            version: 1,
            description: "Lets a plugin see which plugins exist and what state each is in, and \
                          restart one. It never shows what a plugin declares or is allowed."
                .to_owned(),
            consumers,
            methods: BTreeMap::from([
                (
                    "list".to_owned(),
                    Member {
                        capability: None,
                        description: "Lists plugins by id and state.".to_owned(),
                    },
                ),
                (
                    "restart".to_owned(),
                    Member {
                        capability: Some(PLUGINS_RESTART_CAPABILITY.to_owned()),
                        description: "Restarts one plugin the caller names.".to_owned(),
                    },
                ),
            ]),
            events: BTreeMap::from([(
                PLUGINS_STATE_CHANGED.to_owned(),
                Member {
                    capability: None,
                    description: "A plugin's state changed; carries its id and new state."
                        .to_owned(),
                },
            )]),
        });
    }
    let consumers = consumers_of(plugins, crate::transfer_events::TRANSFERS_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: crate::transfer_events::TRANSFERS_SERVICE.to_owned(),
            version: 1,
            description: "Tells a plugin when a transfer changes, and what is going now: which one and what happened. A plugin only reads and listens: it \
                          cannot accept, refuse or cancel anything, and never sees where a file is saved, its contents, a device's key or history."
                .to_owned(),
            consumers,
            methods: BTreeMap::from([
                (
                    crate::transfer_events::TRANSFERS_ACTIVE.to_owned(),
                    Member {
                        capability: Some(crate::transfer_events::TRANSFERS_WATCH_CAPABILITY.to_owned()),
                        description: "The transfers going now, as `changed` says them: no name. For a plugin that starts late or restarts.".to_owned(),
                    },
                ),
                (
                    crate::transfer_events::TRANSFERS_ACTIVE_NAMED.to_owned(),
                    Member {
                        capability: Some(crate::transfer_events::TRANSFERS_NAMES_CAPABILITY.to_owned()),
                        description: "The transfers going now, as `changed-named` says them, with names; only for plugins allowed to see names.".to_owned(),
                    },
                ),
            ]),
            events: BTreeMap::from([
                (
                    crate::transfer_events::TRANSFERS_CHANGED.to_owned(),
                    Member {
                        capability: Some(crate::transfer_events::TRANSFERS_WATCH_CAPABILITY.to_owned()),
                        description: "A transfer started, moved, finished, was cancelled or failed; carries its id, direction, state and progress, and no name."
                            .to_owned(),
                    },
                ),
                (
                    crate::transfer_events::TRANSFERS_CHANGED_NAMED.to_owned(),
                    Member {
                        capability: Some(crate::transfer_events::TRANSFERS_NAMES_CAPABILITY.to_owned()),
                        description: "The same, with the name of the file and the name of the device; for the plugins the person allowed to see names."
                            .to_owned(),
                    },
                ),
            ]),
        });
    }
    let consumers = consumers_of(plugins, crate::status_service::STATUS_SERVICE);
    if !consumers.is_empty() {
        use crate::status_service as status;
        let guarded = |capability: &str, description: &str| Member {
            capability: Some(capability.to_owned()),
            description: description.to_owned(),
        };
        exports.push(Export {
            service: status::STATUS_SERVICE.to_owned(),
            version: 1,
            description: "Lets a plugin make statuses of its own for this device, say which are true now, and read what the paired devices say of \
                          themselves. The application owns three words (online, sleeping, offline) and no plugin can say them; everything else is the \
                          plugin's. A status always carries whose it is. A plugin can only turn on what it made, and never sees a device's key."
                .to_owned(),
            consumers,
            methods: BTreeMap::from([
                (status::STATUS_DEFINE.to_owned(), guarded(status::STATUS_DEFINE_CAPABILITY, "Creates or rewords one of the caller's own statuses. It says nothing about the device until it is turned on.")),
                (status::STATUS_SET.to_owned(), guarded(status::STATUS_SET_CAPABILITY, "Turns one of the caller's own statuses on: every paired device then shows it.")),
                (status::STATUS_CLEAR.to_owned(), guarded(status::STATUS_SET_CAPABILITY, "Turns one of the caller's own statuses off.")),
                (status::STATUS_LIST.to_owned(), guarded(status::STATUS_WATCH_CAPABILITY, "What every paired device says of itself now: how it is reachable and the statuses it shows, each with whose it is.")),
                (status::STATUS_OWN.to_owned(), Member { capability: None, description: "The statuses the caller made itself and which are on.".to_owned() }),
            ]),
            events: BTreeMap::from([(
                status::STATUS_CHANGED.to_owned(),
                guarded(status::STATUS_WATCH_CAPABILITY, "What a paired device says of itself changed; carries the same entry as `list` does for that device."),
            )]),
        });
    }
    for (service, description) in [
        (
            crate::platform_services::FILES_SERVICE,
            crate::platform_services::FILES_DESCRIPTION,
        ),
        (
            crate::platform_services::HTTP_SERVICE,
            crate::platform_services::HTTP_DESCRIPTION,
        ),
    ] {
        let consumers = consumers_of(plugins, service);
        if consumers.is_empty() {
            continue;
        }
        let methods = crate::platform_services::METHODS
            .iter()
            .filter(|(offered, _, _, _)| *offered == service)
            .map(|(_, method, capability, description)| {
                (
                    (*method).to_owned(),
                    Member {
                        capability: Some((*capability).to_owned()),
                        description: (*description).to_owned(),
                    },
                )
            })
            .collect();
        exports.push(Export {
            service: service.to_owned(),
            version: 1,
            description: description.to_owned(),
            consumers,
            methods,
            events: BTreeMap::new(),
        });
    }
    exports
}

/// What the application itself may call on the hub, derived from what the user has
/// granted devices.
///
/// The user never talks to the hub; they decide per device what a plugin may do. The
/// application can run a plugin's action only if the user allowed that capability to
/// that plugin for at least one device, so nothing is granted until they have decided.
/// Only capabilities the application will really present to that plugin are carried over,
/// those of its actions and the one guarding `receive`, and a grant made to a different
/// plugin says nothing about this one.
///
/// The result is computed from every device each time, never patched by the latest
/// change: withdrawing one device's grant must not erase what another device was given.
pub(crate) fn host_grants(plugins: &Registry, peers: &[Peer]) -> BTreeSet<Permission> {
    let mut granted = BTreeSet::new();
    for spec in plugins.iter() {
        // What the application presents to this plugin is its OWN guards (the ones on its
        // actions and on `receive`): permissions of the plugin. Each carries the plugin as
        // its owner, so a grant given to one plugin can never match another's guard that
        // happens to be named the same.
        let presented: BTreeSet<Permission> = presented_by_the_application(spec);
        for peer in peers {
            let Some(given) = peer.grants.get(&spec.id) else {
                continue;
            };
            granted.extend(presented.iter().filter(|p| given.allows(p)).cloned());
        }
    }
    granted
}

/// The guards of one plugin that the application will present when it calls the plugin: the
/// ones on its actions and the one on `receive`, each a permission OF THAT PLUGIN.
fn presented_by_the_application(spec: &PluginSpec) -> BTreeSet<Permission> {
    let owner = spec.id.as_str();
    let mut names: BTreeSet<&String> = spec
        .manifest
        .contributions
        .actions
        .iter()
        .filter_map(|action| action.capability.as_ref())
        .collect();
    names.extend(
        spec.manifest
            .exports
            .iter()
            .filter_map(|export| export.methods.get(crate::plugin_host::RECEIVE_METHOD))
            .filter_map(|member| member.capability.as_ref()),
    );
    names
        .into_iter()
        .filter_map(|name| Permission::new(owner, name.as_str()))
        .collect()
}

/// The guards of the application that this plugin asks it to enforce: the ones on the
/// methods it imports from the application, each a permission OF THE APPLICATION.
fn guarding_what_it_imports(spec: &PluginSpec) -> BTreeSet<Permission> {
    let calls = |service: &str, method: &str| {
        spec.manifest.imports.iter().any(|import| {
            import.provider == HOST
                && import.service == service
                && import.version == 1
                && import.methods.contains(method)
        })
    };
    // A permission can guard an event as well as a method: a plugin that listens to an event is the one that can be granted what guards it, and a
    // plugin that does not listen to it cannot be granted it, so that nobody is offered a permission for something it never asked to hear.
    let listens = |service: &str, event: &str| {
        spec.manifest.imports.iter().any(|import| {
            import.provider == HOST
                && import.service == service
                && import.version == 1
                && import.events.contains(event)
        })
    };
    use crate::status_service as status;
    use crate::transfer_events as transfers;
    // Every way a plugin can reach something the application guards, with the permission that guards it. A read of what is going now and the event
    // that says what changes are guarded by the same permission, so a plugin is offered it by whichever of the two it asked for, and the one that
    // guards NAMES is offered only to a plugin that asked for names, by either way.
    let by_method = [
        (PEERS_SERVICE, "send", PEERS_SEND_CAPABILITY),
        (PLUGINS_SERVICE, "restart", PLUGINS_RESTART_CAPABILITY),
        (
            transfers::TRANSFERS_SERVICE,
            transfers::TRANSFERS_ACTIVE,
            transfers::TRANSFERS_WATCH_CAPABILITY,
        ),
        (
            transfers::TRANSFERS_SERVICE,
            transfers::TRANSFERS_ACTIVE_NAMED,
            transfers::TRANSFERS_NAMES_CAPABILITY,
        ),
        // Making a status and turning it on are two permissions, because only the second is SEEN by the other devices. Reading what the
        // others say is a third. `own` is the plugin's own and `list` of presence is guarded by watch.
        (
            status::STATUS_SERVICE,
            status::STATUS_DEFINE,
            status::STATUS_DEFINE_CAPABILITY,
        ),
        (
            status::STATUS_SERVICE,
            status::STATUS_SET,
            status::STATUS_SET_CAPABILITY,
        ),
        (
            status::STATUS_SERVICE,
            status::STATUS_CLEAR,
            status::STATUS_SET_CAPABILITY,
        ),
        (
            status::STATUS_SERVICE,
            status::STATUS_LIST,
            status::STATUS_WATCH_CAPABILITY,
        ),
    ]
    .into_iter()
    .filter(|(service, method, _)| calls(service, method))
    .chain(
        crate::platform_services::METHODS
            .iter()
            .filter(|(service, method, _, _)| calls(service, method))
            .map(|(service, method, capability, _)| (*service, *method, *capability)),
    )
    .map(|(_, _, capability)| capability);
    let by_event = [
        (
            transfers::TRANSFERS_SERVICE,
            transfers::TRANSFERS_CHANGED,
            transfers::TRANSFERS_WATCH_CAPABILITY,
        ),
        (
            transfers::TRANSFERS_SERVICE,
            transfers::TRANSFERS_CHANGED_NAMED,
            transfers::TRANSFERS_NAMES_CAPABILITY,
        ),
        (
            status::STATUS_SERVICE,
            status::STATUS_CHANGED,
            status::STATUS_WATCH_CAPABILITY,
        ),
    ]
    .into_iter()
    .filter(|(service, event, _)| listens(service, event))
    .map(|(_, _, capability)| capability);
    by_method
        .chain(by_event)
        .filter_map(|capability| Permission::new(HOST, capability))
        .collect()
}

/// Whether a permission is about no device, so that it is granted to a plugin once and not
/// per device. The single definition: a permission is device-independent because the
/// application says so here, never because a plugin or a caller says so. Everything else is
/// about a device and is granted for one.
fn needs_no_device(permission: &Permission) -> bool {
    [
        PLUGINS_RESTART_CAPABILITY,
        crate::transfer_events::TRANSFERS_WATCH_CAPABILITY,
        crate::transfer_events::TRANSFERS_NAMES_CAPABILITY,
        // Statuses are about THIS device and what it says to all the others, never about one of them.
        crate::status_service::STATUS_DEFINE_CAPABILITY,
        crate::status_service::STATUS_SET_CAPABILITY,
        crate::status_service::STATUS_WATCH_CAPABILITY,
        crate::platform_services::FILES_CAPABILITY,
        crate::platform_services::HTTP_CAPABILITY,
    ]
    .into_iter()
    .filter_map(|capability| Permission::new(HOST, capability))
    .any(|about_no_device| about_no_device == *permission)
}

/// Every permission that can be granted to `spec`, and the only definition of it.
///
/// Derived from the ROLES the plugin plays and never from what it declares for itself: its own
/// guards on what it offers, and the application's guards on what it imports from the
/// application. A name the plugin merely listed in its `capabilities` is neither, and does
/// not become a permission, or anyone's, by being listed.
pub fn grantable(spec: &PluginSpec) -> BTreeSet<Permission> {
    let mut all = presented_by_the_application(spec);
    all.extend(guarding_what_it_imports(spec));
    all
}

/// The part of [`grantable`] that is granted once, for no device.
pub fn grantable_without_a_device(spec: &PluginSpec) -> BTreeSet<Permission> {
    let mut all = grantable(spec);
    all.retain(needs_no_device);
    all
}

/// The part of [`grantable`] that is granted for one device at a time.
pub fn grantable_for_a_device(spec: &PluginSpec) -> BTreeSet<Permission> {
    let mut all = grantable(spec);
    all.retain(|permission| !needs_no_device(permission));
    all
}

/// What a window is told `spec` can be granted: exactly the owner's two sets, split as they are, and
/// each in the order of its names so that the rows do not reshuffle between snapshots. The
/// window decides nothing of its own, so the rule has one definition.
pub fn grantable_for_window(spec: &PluginSpec) -> peersus_core::model::Grantable {
    peersus_core::model::Grantable {
        per_device: grantable_for_a_device(spec).into_iter().collect(),
        outright: grantable_without_a_device(spec).into_iter().collect(),
    }
}

/// `held` with exactly one permission turned on or off and every other one left as it was.
/// Allowing what is allowed and withdrawing what is not change nothing. The only way it can
/// fail is a set that grows past what a plugin may hold.
pub(crate) fn applied(
    held: &Grants,
    permission: &Permission,
    on: bool,
) -> Result<Grants, peersus_core::plugins::PluginStateError> {
    let others = held.iter().filter(|p| *p != permission).cloned();
    if on {
        Grants::new(others.chain(std::iter::once(permission.clone())))
    } else {
        Grants::new(others)
    }
}

/// What one plugin itself may present at the hub, derived from what the user granted it.
///
/// The counterpart of [`host_grants`], for the calls a plugin makes to the application: the
/// application's own permissions that guard what the plugin imports. A permission about a
/// device is held if any device granted it, which is the coarse layer: which device a plugin
/// may actually send to is decided separately, by the grant of that one device, when it asks.
/// A permission about no device is held if it was granted to the plugin outright. The union
/// is recomputed from every source each time, so withdrawing one grant never erases another.
pub(crate) fn plugin_grants(
    spec: &PluginSpec,
    peers: &[Peer],
    standalone: &BTreeMap<PluginId, Grants>,
) -> BTreeSet<Permission> {
    let wanted = guarding_what_it_imports(spec);
    let per_device = peers
        .iter()
        .filter_map(|peer| peer.grants.get(&spec.id))
        .flat_map(|given| {
            wanted
                .iter()
                .filter(|p| !needs_no_device(p) && given.allows(p))
                .cloned()
        });
    let outright = standalone.get(&spec.id).into_iter().flat_map(|given| {
        wanted
            .iter()
            .filter(|p| needs_no_device(p) && given.allows(p))
            .cloned()
    });
    per_device.chain(outright).collect()
}

/// The answer to `peersus.peers` `list`: the paired devices, as one plugin may know them.
///
/// Exactly three things per device: the alias this plugin uses to name it (what `send`
/// accepts back), the name the user gave it, and whether it can be reached now. Never the
/// network key, when it was paired or last seen, or what any plugin was granted for it:
/// none of that is needed to choose a destination, and a plugin that gets the key can
/// recognise the same device in every other plugin and in every later run.
pub(crate) fn peers_listing(
    aliases: &crate::aliases::Aliases,
    plugin: &PluginId,
    peers: &[Peer],
) -> serde_json::Value {
    serde_json::Value::Array(
        peers
            .iter()
            .map(|peer| {
                serde_json::json!({
                    "device": aliases.alias(plugin, &peer.id),
                    "name": peer.name,
                    "online": peer.online,
                })
            })
            .collect(),
    )
}

/// Filter stored state through the caller's manifest. Invalid saved values fall back to
/// declared defaults; undeclared keys and other plugins' values never cross this boundary.
pub(crate) fn configuration(
    spec: &PluginSpec,
    saved: Option<&peersus_core::plugins::Values>,
    aliases: &crate::aliases::Aliases,
    peers: &[Peer],
) -> serde_json::Value {
    use peersus_plugin_api::contributions::Scope;
    let empty = peersus_core::plugins::Values::default();
    let effective = |values: &peersus_core::plugins::Values, scope| {
        spec.manifest
            .contributions
            .settings
            .iter()
            .filter(|setting| setting.scope == scope)
            .map(|setting| {
                (
                    setting.id.clone(),
                    serde_json::to_value(values.effective(setting))
                        .unwrap_or(serde_json::Value::Null),
                )
            })
            .collect::<serde_json::Map<String, serde_json::Value>>()
    };
    let devices: Vec<_> = peers.iter().map(|peer| {
        serde_json::json!({
            "device": aliases.alias(&spec.id, &peer.id),
            "online": peer.online,
            "settings": effective(peer.plugin_values.get(&spec.id).unwrap_or(&empty), Scope::Device),
            "permissions": peer.grants.get(&spec.id).map(|grants| grants.iter().collect::<Vec<_>>()).unwrap_or_default(),
        })
    }).collect();
    serde_json::json!({
        "plugin": spec.id.as_str(),
        "settings": effective(saved.unwrap_or(&empty), Scope::Global),
        "peers": devices,
    })
}

/// A bidirectional service's own guard also applies when its provider initiates a send.
/// This closes stale-config races without naming a bundled plugin or inventing grants.
pub(crate) fn authorize_service_send(
    spec: &PluginSpec,
    peer: &Peer,
    service: &str,
    version: u32,
) -> Result<(), &'static str> {
    if let Some(member) = spec
        .manifest
        .exports
        .iter()
        .find(|export| export.service == service && export.version == version)
        .and_then(|export| export.methods.get(crate::plugin_host::RECEIVE_METHOD))
        && !crate::plugin_host::permitted(peer, &spec.id, member.capability.as_deref())
    {
        return Err(SEND_REFUSED);
    }
    Ok(())
}

/// The answer to `peersus.plugins` `list`: each plugin's id and what state it is in.
///
/// Exactly those two things. Not what a plugin declares, what it may do, what was granted it,
/// or where it lives: a plugin that wants to tell whether another is up has no need of any of
/// that, and a plugin that could read it could learn what every other one is allowed. The
/// state is the fixed vocabulary the application itself uses, never anything a plugin wrote.
pub(crate) fn plugins_listing(infos: &[peersus_core::model::PluginInfo]) -> serde_json::Value {
    serde_json::Value::Array(
        infos
            .iter()
            .map(|info| plugin_entry(&info.id, info.status))
            .collect(),
    )
}

/// One plugin as `peersus.plugins` says it, in `list` and in `state-changed` alike: its id and
/// its state. A single definition, so the event can never carry more than the listing does.
pub(crate) fn plugin_entry(
    id: &PluginId,
    status: peersus_core::model::PluginStatus,
) -> serde_json::Value {
    serde_json::json!({ "id": id.as_str(), "status": status })
}

/// What a plugin asks `peersus.plugins` `restart` for: which plugin.
#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RestartRequest {
    plugin: PluginId,
}

/// Read a `restart` request exactly. Anything beyond the one field, or an id that is not an
/// identifier, is refused before the application looks at what it names.
pub(crate) fn parse_restart(args: &serde_json::Value) -> Result<PluginId, &'static str> {
    serde_json::from_value::<RestartRequest>(args.clone())
        .map(|request| request.plugin)
        .map_err(|_| "A restart names one plugin")
}

/// What a plugin is told when the application hands it a device: its alias, under `peer`.
///
/// The same name `list` gives and `send` takes, for every way a plugin can be handed a
/// device (an action the user pressed, bytes a device sent), so that one name works for
/// everything and the device's network key never reaches a plugin by any route.
pub(crate) fn peer_argument(
    aliases: &crate::aliases::Aliases,
    plugin: &PluginId,
    device: &PeerId,
) -> serde_json::Value {
    serde_json::json!({ "peer": aliases.alias(plugin, device) })
}

/// The one thing said to a plugin whose `send` is refused, whatever the reason.
///
/// An alias that means nothing, a device that was never granted, and a device that is
/// granted but offline all read the same. A plugin told apart "no such device" from "not
/// allowed" could use `send` to find out which devices exist and what the user decided.
pub(crate) const SEND_REFUSED: &str = "That device cannot be sent to";

/// The device layer of a plugin's `send`: which device it may send to, if any.
///
/// `alias` must be one this plugin was given; the device must be one the user has paired,
/// must be reachable now, and must have granted this plugin `peers.send` itself. A grant
/// on one device says nothing about another, and what was granted to a different plugin
/// says nothing about this one. This is the decision made for each device separately; the
/// hub has already checked, coarsely, that the plugin holds the capability at all.
pub(crate) fn authorize_send<'a>(
    aliases: &crate::aliases::Aliases,
    plugin: &PluginId,
    alias: &str,
    peers: &'a [Peer],
) -> Result<&'a Peer, &'static str> {
    let id = aliases
        .resolve(plugin, alias, peers.iter().map(|peer| &peer.id))
        .ok_or(SEND_REFUSED)?;
    let peer = peers
        .iter()
        .find(|peer| &peer.id == id)
        .ok_or(SEND_REFUSED)?;
    // `send` is guarded by the APPLICATION's permission, so that is the one the device must
    // have granted this plugin. `permitted` answers a different question, about the plugin's
    // own guards, and would look up a same-named permission of the plugin instead.
    let guard = Permission::new(HOST, PEERS_SEND_CAPABILITY);
    let granted = guard.is_some_and(|guard| {
        peer.grants
            .get(plugin)
            .is_some_and(|grants| grants.allows(&guard))
    });
    if peer.online && granted {
        Ok(peer)
    } else {
        Err(SEND_REFUSED)
    }
}

/// What a plugin asks `peersus.peers` `send` for: where, and which service there.
#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendRequest {
    /// The alias this plugin was given for the device by `list`.
    pub(crate) device: String,
    /// The service on the device the stream is for.
    pub(crate) service: String,
    pub(crate) version: u32,
}

/// Decode the arguments of a `send`, refusing anything that is not exactly a
/// [`SendRequest`] with a service the wire would carry.
///
/// The wire's own rule decides what a service name and version may be, so the application
/// never reserves anything for a stream it could not open. Every failure reads as
/// [`SEND_REFUSED`]: what exactly was wrong with a request is not something to tell a plugin
/// that may be probing.
pub(crate) fn parse_send(args: &serde_json::Value) -> Result<SendRequest, &'static str> {
    let request: SendRequest = serde_json::from_value(args.clone()).map_err(|_| SEND_REFUSED)?;
    peersus_net::streams::Request::Plugin {
        service: request.service.clone(),
        version: request.version,
    }
    .validate()
    .map_err(|_| SEND_REFUSED)?;
    Ok(request)
}

/// What a user is told about a plugin, from what its supervisor last reported.
///
/// `None` means there is no supervisor entry at all, because the plugin could not be
/// launched. Nothing then says it is running, so it is reported as stopped: a screen must
/// never claim a plugin works on the strength of the absence of bad news.
pub(crate) fn status_of(event: Option<&PluginEvent>) -> peersus_core::model::PluginStatus {
    use peersus_core::model::PluginStatus;
    match event {
        // Launched is not usable: until the process has said hello the hub cannot reach it,
        // so it is only starting, however quickly that usually happens.
        Some(PluginEvent::Started) => PluginStatus::Starting,
        Some(PluginEvent::Connected) => PluginStatus::Running,
        Some(PluginEvent::Failed { .. }) => PluginStatus::Restarting,
        Some(PluginEvent::GaveUp) => PluginStatus::Failed,
        Some(PluginEvent::Stopped) | None => PluginStatus::Stopped,
    }
}

/// The report a supervisor has made since this was last asked, if there is one.
///
/// The obvious way to ask is `has_changed`, and it is wrong exactly when it matters: it
/// fails as soon as the supervisor has ended, before it looks at whether there was
/// something unseen. A supervisor that gives up sends `GaveUp` and ends in the same
/// instant, so that call would hide the one report a user most needs. `changed` looks at
/// whether there is something unseen first, and only then at whether the sender is gone,
/// so an unseen last report is delivered once and a finished supervisor then adds nothing.
///
/// It is polled a single time with a waker that does nothing: the answer is already
/// there or it is "nothing new", and it must not wait.
pub(crate) fn observe(health: &mut watch::Receiver<PluginEvent>) -> Option<PluginEvent> {
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    let mut context = Context::from_waker(Waker::noop());
    // The future borrows the receiver, so it is dropped before the value is read.
    let arrived = {
        let mut changed = pin!(health.changed());
        matches!(changed.as_mut().poll(&mut context), Poll::Ready(Ok(())))
    };
    arrived.then(|| health.borrow_and_update().clone())
}

/// What the user is told when the hub refuses to pass on an action. Every refusal the hub
/// can give has its own fixed wording, so a person is never sent looking for a crash that
/// did not happen: being refused is not the same as the plugin being gone.
pub(crate) fn refusal_text(error: &peersus_plugin_api::Error) -> &'static str {
    use peersus_plugin_api::Error;
    match error {
        Error::Denied => "The plugin does not allow this action",
        Error::QueueFull => "Too many operations are pending",
        Error::UnknownProvider | Error::Unavailable => "The plugin is not running",
        Error::UnknownMethod | Error::Version => "The plugin does not offer this action",
        _ => "The request could not be sent",
    }
}

/// Where an action stands, given what came back for its call.
///
/// Only the code is read, and only to match it against the codes this application and the
/// hub themselves use, exactly and in full. The message is never looked at, and a code that
/// is not one of those (anything a plugin made up, in any spelling) is `Other`, so nothing a
/// plugin wrote can reach what a person is shown.
pub(crate) fn action_state(
    result: &peersus_plugin_api::message::Outcome,
) -> peersus_core::model::ActionState {
    use peersus_core::model::{ActionFailure, ActionState};
    use peersus_plugin_api::message::Outcome;
    match result {
        Outcome::Ok { .. } => ActionState::Done,
        Outcome::Error { code, .. } if code == "cancelled" => ActionState::Cancelled,
        Outcome::Error { code, .. } => ActionState::Failed {
            reason: match code.as_str() {
                "denied" | "refused" => ActionFailure::NotAllowed,
                "unavailable" => ActionFailure::Unavailable,
                "timeout" | "invalid-deadline" => ActionFailure::TimedOut,
                "busy" => ActionFailure::Busy,
                _ => ActionFailure::Other,
            },
        },
    }
}

/// The fixed sentence a person is told when a plugin's call ended in an error, by the reason `action_state` read from its
/// code. Only the application's own words: what the plugin wrote is never in it. `None` for an action that was cancelled
/// or that did not fail (nothing to tell).
pub(crate) fn action_failure_sentence(
    result: &peersus_plugin_api::message::Outcome,
) -> Option<&'static str> {
    use peersus_core::model::{ActionFailure, ActionState};
    match action_state(result) {
        ActionState::Failed { reason } => Some(match reason {
            ActionFailure::NotAllowed => "A plugin was not allowed to complete the action",
            ActionFailure::Unavailable => {
                "A plugin could not complete the action: what it needs is not available"
            }
            ActionFailure::TimedOut => "A plugin did not complete the action in time",
            ActionFailure::Busy => "A plugin could not complete the action: it is busy",
            _ => "A plugin could not complete the action",
        }),
        _ => None,
    }
}

/// Whether a data right made by `caller` may collect a stream the application parked.
///
/// Only the application's own calls may: it is the application that set the stream aside
/// and numbered the call. The caller is the identity the hub stamped on the call, which no
/// plugin can choose or forge, unlike the call's number, which every participant picks for
/// its own calls and so can coincide.
pub(crate) fn may_collect(caller: &str) -> bool {
    caller == HOST
}

/// A device that may send a stream to a plugin, and where it is going.
#[derive(Debug)]
pub(crate) struct Admitted {
    pub(crate) plugin: PluginId,
    pub(crate) service: String,
    pub(crate) version: u32,
}

/// Decide whether `peer` may open a stream to the service `(service, version)`.
///
/// Everything that can refuse is here, in one place, before anything is kept: a stream from
/// a device that may not use the plugin is never set aside, so an unauthorised device cannot
/// make the application hold memory or a connection for it. The order is the order in which
/// the answers cost nothing: is the device known, is there a plugin for this exact service
/// and version, and did the user grant that plugin this capability for this device.
pub(crate) fn admit(
    plugins: &Registry,
    peers: &[Peer],
    peer: &PeerId,
    service: &str,
    version: u32,
) -> Result<Admitted, &'static str> {
    let peer = peers
        .iter()
        .find(|candidate| &candidate.id == peer)
        .ok_or("Device is not paired")?;
    let Route::Plugin { index, capability } = plugins.route(service, version) else {
        return Err("No plugin handles this request");
    };
    let plugin = plugins.get(index).ok_or("Plugin unavailable")?.id.clone();
    if !permitted(peer, &plugin, capability) {
        return Err("Plugin is not permitted for this device");
    }
    Ok(Admitted {
        plugin,
        service: service.to_owned(),
        version,
    })
}

/// A package the person chose and the owner read, kept until it is confirmed or replaced: where it
/// is, what it asks for, and its digest as it was when it was read, so that confirming can tell
/// whether it is still the package that was shown.
struct Staged {
    source: PathBuf,
    manifest: peersus_plugin_api::manifest::Manifest,
    digest: String,
    /// When the package came as an archive: what was unpacked from it, which `source` points into.
    /// Dropping this deletes it, so a confirmed, cancelled, replaced or refused package leaves
    /// nothing behind.
    _unpacked: Option<crate::archive::Unpacked>,
    /// What the package says about itself (author, link, version), shown beside what it asks for.
    package: Option<peersus_plugin_api::package::Package>,
}

/// The store as the owner holds it: the verified-catalog service and the thing that fetches bytes for it.
#[cfg(unix)]
struct StoreSlot {
    service: crate::store_service::StoreService,
    fetcher: Arc<dyn crate::store_runtime::StoreFetcher>,
}

/// What a store download task hands back. Only UNVERIFIED bytes: nothing here has been trusted yet.
enum StoreOutcome {
    Refreshed(Result<(Vec<u8>, Vec<u8>), crate::catalog::Refusal>),
    Downloaded {
        plugin: PluginId,
        bytes: Result<Vec<u8>, crate::catalog::Refusal>,
    },
}

impl Runtime {
    /// `plugin_directory` must be a private directory the launcher already created, since
    /// the sockets of plugin processes are made inside it.
    pub fn new(
        mut network: NetworkHost,
        standalone: StandaloneGrants,
        settings: SettingsStore,
        plugins: Registry,
        plugin_directory: PathBuf,
    ) -> std::io::Result<(Self, Client)> {
        // The wait for an answer to an offer is a preference: the host starts from the one that was saved, and `update_settings`
        // keeps it current, so that nothing in the transfer code holds a time of its own.
        network.set_offer_wait(settings.current().offer_timeout_seconds);
        let hub = Hub::default();
        let offered_host = host_manifest(&plugins);
        let platform_timeouts = crate::platform_services::method_timeouts(&offered_host);
        let generation = hub
            .register_host(HOST, offered_host, BTreeSet::new())
            .map_err(|_| std::io::Error::other("The application could not join the plugin hub"))?;
        let host = hub
            .connect(HOST, generation)
            .map_err(|_| std::io::Error::other("The application could not join the plugin hub"))?;
        host.set_host_method_timeouts(platform_timeouts)
            .map_err(|_| std::io::Error::other("Platform deadlines could not be applied"))?;
        let (snapshots, state) = watch::channel(Arc::new(Snapshot {
            settings: settings.current().clone(),
            peers: network.peers(),
            grants: standalone.all().clone(),
            plugins: plugins.info(),
            staged: Vec::new(),
            store: peersus_core::model::StoreState {
                status: peersus_core::model::StoreStatus::NotConfigured,
                ..Default::default()
            },
            actions: Vec::new(),
            transfers: network.transfers(),
            pairing: network.pairing(),
            errors: Vec::new(),
            profile_picture: network
                .own_picture()
                .map(peersus_core::profile::Picture::digest),
            peer_pictures: network.peer_digests(),
            files_code: network.files_code(),
            link_tests: network.link_tests(),
            typed_addresses: network.typed_addresses(),
            listening_port: network.listening_port(),
            relay: Some(network.relay_diagnostics()),
        }));
        let (commands, requests) = mpsc::channel(64);
        // Data connections of every plugin arrive on one channel. The application matches
        // each to what it set aside by the number of the call, which is the same whichever
        // plugin it was, so one place consuming them is enough.
        let (handoff, accepted) = mpsc::channel(16);
        Ok((
            Self {
                network,
                standalone,
                settings,
                plugins,
                requests,
                snapshots,
                errors: Vec::new(),
                told_transfers: std::cell::RefCell::new(None),
                told_statuses: std::cell::RefCell::new(None),
                hub,
                host,
                plugin_directory,
                running: HashMap::new(),
                launchers: HashMap::new(),
                modules: None,
                next_call: 0,
                in_flight: InFlight::default(),
                board: crate::board::Board::default(),
                parked: Parked::default(),
                aliases: crate::aliases::Aliases::new(),
                handoff,
                accepted,
                transfers: tokio::task::JoinSet::new(),
                carrying: crate::running::Running::default(),
                outbound: Parked::default(),
                carrying_out: crate::running::Running::default(),
                statuses: crate::statuses::Statuses::default(),
                packages: None,
                disabled: std::collections::BTreeSet::new(),
                held_back: Vec::new(),
                token_store: None,
                platform_services: None,
                platform_calls: crate::platform_calls::PlatformCalls::default(),
                staged: HashMap::new(),
                held: peersus_net::held::Held::default(),
                installed_packages: std::collections::BTreeMap::new(),
                #[cfg(unix)]
                store: None,
                store_tasks: tokio::task::JoinSet::new(),
            },
            Client {
                requests: commands,
                snapshots: state,
            },
        ))
    }

    /// Where the access token of a personal relay is kept between runs, which each platform decides: the system credential
    /// store on a desktop, the native keystore on a phone. Without one a token cannot be saved, and is refused, never
    /// kept only in memory where the person would believe it was safe.
    #[must_use]
    pub fn with_token_store(mut self, store: Arc<dyn TokenStore>) -> Self {
        self.token_store = Some(store);
        self
    }

    /// Attach trusted platform facilities. Extensions cannot choose an implementation.
    #[must_use]
    pub fn with_platform_services(
        mut self,
        services: Arc<dyn crate::platform_services::PlatformServices>,
    ) -> Self {
        self.platform_services = Some(services);
        self
    }

    /// Supply a platform launcher for a bundled plugin without pretending it is an executable.
    #[must_use]
    pub fn with_launcher(
        mut self,
        plugin: PluginId,
        launcher: Arc<dyn peersus_plugin_host::supervisor::Launcher>,
    ) -> Self {
        self.launchers.insert(plugin, launcher);
        self
    }

    /// Choose platform-owned executors for installed modules, including later installs.
    /// A module without its declared executor is refused, never run as an executable.
    #[must_use]
    pub fn with_modules(
        mut self,
        modules: Arc<dyn peersus_plugin_host::module::ModuleFactory>,
    ) -> Self {
        self.modules = Some(modules);
        self
    }

    /// Tell this owner which of the plugins it was given are switched off, as `Manager::load`
    /// reports them: they stay registered and listed, and are not started.
    #[must_use]
    pub fn with_disabled(mut self, disabled: std::collections::BTreeSet<PluginId>) -> Self {
        self.disabled = disabled;
        self
    }

    /// Tell this owner which plugins were found in the store and are not run (not approved, changed, broken), as
    /// `Manager::load` reports them. They are not registered and not started, and they are listed with what each waits for,
    /// so that none is ever invisible and each can be removed.
    #[must_use]
    pub fn with_held_back(mut self, held_back: Vec<crate::discovery::HeldBack>) -> Self {
        self.held_back = held_back;
        self
    }

    /// Give this owner the store of installed packages, so that the person can install, enable,
    /// disable and remove plugins from the window. Without it those commands are refused.
    #[must_use]
    pub fn with_packages(mut self, manager: crate::plugin_manager::Manager) -> Self {
        self.packages = Some(manager);
        self
    }

    /// Give this owner the registry in which the interface keeps the files that the system handed over, so that
    /// `SendHeld` can take them by number. Without it every number is unknown and such a command is refused. The
    /// interface and the owner each hold a clone of the one registry, which is how a file gets from one to the other
    /// without ever being in a command, which travels as text.
    #[must_use]
    pub fn with_held(mut self, held: peersus_net::held::Held) -> Self {
        self.held = held;
        self
    }

    /// Give this owner the extension store: the compiled-in trust root decides what is accepted and
    /// `fetcher` supplies bytes. Without it the store commands are refused. Packages must also be
    /// installable (`with_packages`), or an install is refused before anything is fetched.
    #[cfg(unix)]
    #[must_use]
    pub fn with_store(
        mut self,
        service: crate::store_service::StoreService,
        fetcher: Arc<dyn crate::store_runtime::StoreFetcher>,
    ) -> Self {
        self.store = Some(StoreSlot { service, fetcher });
        // The initial snapshot was published without a store; tell the window it exists now.
        self.publish();
        self
    }

    /// Start every registered plugin as its own supervised process. A plugin that cannot
    /// start is reported and skipped; one broken plugin never stops the application.
    fn start_plugins(&mut self) {
        let specs: Vec<PluginSpec> = self.plugins.iter().cloned().collect();
        for spec in specs {
            // Switched off by the person: it is registered and listed, and left stopped.
            if self.disabled.contains(&spec.id) {
                continue;
            }
            self.start_one(spec);
        }
    }

    /// Start one plugin as a supervised process, or report that it could not be started.
    fn start_one(&mut self, spec: PluginSpec) {
        let id = spec.id.clone();
        if spec.program.is_some()
            && crate::plugin_host::check_external_execution(spec.manifest.execution).is_err()
        {
            self.error("This extension format is not available in this distribution");
            return;
        }
        // Read before the spec is taken apart, so a plugin with its own patience keeps it.
        let policy = spec.restart_policy.unwrap_or_default();
        // What the user already decided, so a plugin that is started (or started again after
        // giving up) begins with the truth instead of with nothing. A restart of the running
        // process keeps the hub's record on its own.
        let grants = plugin_grants(&spec, &self.network.peers(), self.standalone.all());
        let plugin = Plugin {
            id: spec.id.as_str().to_owned(),
            manifest: spec.manifest,
            grants,
            program: spec.program,
            arguments: spec.arguments,
            directory: self.plugin_directory.clone(),
            policy,
            handoff: Some(self.handoff.clone()),
        };
        let started = match self.launchers.get(&id) {
            Some(launcher) => {
                Service::start_with_launcher(self.hub.clone(), plugin, Arc::clone(launcher))
            }
            None if matches!(
                plugin.manifest.execution,
                peersus_plugin_api::manifest::Execution::Process
            ) =>
            {
                Service::start(self.hub.clone(), plugin)
            }
            None => self
                .modules
                .as_ref()
                .ok_or_else(|| std::io::Error::other("module executor unavailable"))
                .and_then(|modules| {
                    let module = plugin
                        .program
                        .as_deref()
                        .ok_or_else(|| std::io::Error::other("module payload unavailable"))?;
                    modules.launcher(plugin.manifest.execution, module)
                })
                .and_then(|launcher| {
                    Service::start_with_launcher(self.hub.clone(), plugin, launcher)
                }),
        };
        match started {
            Ok(service) => {
                let health = service.events();
                self.running.insert(id, Running { service, health });
            }
            Err(_) => self.error("A plugin could not be started"),
        }
    }

    /// Notice plugins whose health changed and report the ones the user should know about.
    /// Text is fixed: nothing a plugin prints is ever shown. Returns whether anything
    /// changed, so the caller can publish it: a screen waiting for a plugin to show as
    /// failed must not have to wait for some unrelated event to learn it.
    fn check_plugin_health(&mut self) -> bool {
        let mut problems = Vec::new();
        let mut transitions = Vec::new();
        for (id, running) in &mut self.running {
            if let Some(event) = observe(&mut running.health) {
                transitions.push((id.clone(), status_of(Some(&event))));
                if event == PluginEvent::GaveUp {
                    problems.push(id.clone());
                }
            }
        }
        for _ in problems {
            self.error("A plugin stopped working and was turned off");
        }
        let changed = !transitions.is_empty();
        for (id, status) in transitions {
            if matches!(
                status,
                peersus_core::model::PluginStatus::Restarting
                    | peersus_core::model::PluginStatus::Failed
                    | peersus_core::model::PluginStatus::Stopped
            ) {
                self.revoke_platform_calls(&id);
            }
            // Whoever asked for `peersus.plugins` is told. A subscriber that cannot keep up
            // misses this one rather than stalling the application: the hub refuses the whole
            // delivery then, and `list` is always there to ask what is true now.
            let _ = self
                .host
                .submit(peersus_plugin_api::message::Operation::Publish {
                    service: PLUGINS_SERVICE.to_owned(),
                    version: 1,
                    event: PLUGINS_STATE_CHANGED.to_owned(),
                    data: plugin_entry(&id, status),
                });
        }
        changed
    }

    async fn stop_plugins(&mut self) {
        let plugins: Vec<_> = self.plugins.iter().map(|spec| spec.id.clone()).collect();
        for plugin in plugins {
            self.revoke_platform_calls(&plugin);
        }
        while self.platform_calls.next().await.is_some() {}
        for (_, running) in self.running.drain() {
            running.service.shutdown().await;
        }
    }

    pub async fn run(mut self) {
        let mut progress = tokio::time::interval(Duration::from_secs(1));
        progress.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The user's decisions survive a restart in the saved devices. Recover what the
        // application may call from them before any plugin can be asked for anything.
        self.sync_host_grants();
        self.start_plugins();
        // The snapshot made in `new` was taken before this owner was told what is switched off and what was found and is not
        // run, so a window that connects now would be shown a list without them until something else happened.
        self.publish();
        loop {
            // The folders being sent go on first, a few files at a time: each file is one short step, and after a few the events below get
            // their turn, so that a folder of a million files never keeps a request or a window waiting. What goes out is published.
            let mut offered = false;
            for _ in 0..Self::FOLDER_FILES_PER_TURN {
                if !self.network.feed_folders(now()).await {
                    break;
                }
                offered = true;
            }
            if offered {
                self.publish();
            }
            tokio::select! {
                request = self.requests.recv() => {
                    let Some(request) = request else { break };
                    if matches!(request.command, Command::Shutdown {}) {
                        self.requests.close();
                        self.stop_plugins().await;
                        self.network.close().await;
                        let _ = request.result.send(Ok(Reply::Applied));
                        return;
                    }
                    // Only these can change which packages are installed or at what version. Checked before the
                    // command is moved, and read once after, so `publish` itself never touches the disk.
                    let changes_installed = matches!(
                        request.command,
                        Command::RemovePackage { .. } | Command::ConfirmInstall { .. } | Command::ApprovePlugin { .. } | Command::StoreInstall { .. }
                    );
                    let result = self.execute(request.command).await;
                    if changes_installed { self.refresh_installed(); }
                    if let Err(reason) = result.as_ref() { self.error(reason); }
                    if !matches!(&result, Ok(Reply::QueueBusy)) { self.publish(); }
                    let _ = request.result.send(result);
                }
                event = self.network.next(now()) => {
                    match event {
                        Event::Changed => {}
                        Event::Error(reason) => self.error(reason),
                        Event::Plugin { peer, service, version, send, receive } => {
                            if let Err(reason) = self.route_inbound(&peer, &service, version, send, receive) {
                                self.error(reason);
                            }
                        }
                        // Nothing to do but publish: a device's picture is read through the snapshot's
                        // digest, and the pixels are fetched by the window only if it lacks them.
                        Event::Picture { .. } => {}
                    }
                    self.publish();
                }
                delivery = self.host.receive() => {
                    match delivery {
                        Ok(delivery) => self.handle_host_delivery(delivery),
                        // The application's seat on the hub was revoked, which only
                        // happens when the whole hub is going away.
                        Err(_) => break,
                    }
                    self.publish();
                }
                accepted = self.accepted.recv() => {
                    // The sender lives in the runtime itself, so this only ends with it.
                    if let Some(accepted) = accepted {
                        #[cfg(unix)]
                        self.begin_transfer(accepted);
                        #[cfg(not(unix))]
                        let _ = accepted;
                    }
                }
                Some(finished) = self.platform_calls.next(), if !self.platform_calls.is_empty() => {
                    if let Some(result) = finished.result {
                        let _ = self.host.submit(peersus_plugin_api::message::Operation::Reply {
                            ticket: finished.ticket,
                            result,
                        });
                    }
                }
                Some(finished) = self.transfers.join_next(), if !self.transfers.is_empty() => {
                    match finished {
                        Ok((direction, call, outcome)) => {
                            match direction {
                                Direction::Inbound => self.carrying.ended(call),
                                Direction::Outbound => self.carrying_out.ended(call),
                            };
                            match outcome {
                                crate::receive::Delivery::Done(_) => {}
                                crate::receive::Delivery::Failed(reason) => self.error(reason),
                            }
                        }
                        // A transfer that panicked is a defect, but the application goes on.
                        // One that was stopped on purpose ends here too and is not an error.
                        Err(error) if !error.is_cancelled() => {
                            self.error("A transfer to a plugin failed");
                        }
                        Err(_) => {}
                    }
                    self.publish();
                }
                // A store download finished. The task returned only unverified bytes; trusting and
                // installing them happen here and nowhere else.
                Some(finished) = self.store_tasks.join_next(), if !self.store_tasks.is_empty() => {
                    match finished {
                        Ok(outcome) => self.finish_store(outcome).await,
                        // A task that panicked or was lost never reports back, and the store would read as
                        // busy for good. Release it, and say so.
                        Err(error) => {
                            #[cfg(unix)]
                            if let Some(slot) = self.store.as_mut() {
                                slot.service.failed("The extension store failed");
                            }
                            if !error.is_cancelled() {
                                self.error("The extension store failed");
                            }
                        }
                    }
                    self.publish();
                }
                _ = progress.tick() => {
                    // Streams whose plugin never came are dropped here, which also ends the
                    // peer's side. Nothing else drives this: without it an entry would hold a
                    // peer's connection open for as long as the runtime lived.
                    self.parked.expire(std::time::Instant::now());
                    self.outbound.expire(std::time::Instant::now());
                    // Evaluated on its own line, not inside the condition below, because
                    // `||` would skip it whenever pairing or a transfer is active.
                    let plugins_changed = self.check_plugin_health();
                    if plugins_changed
                        || !self.network.pairing().is_empty()
                        || self.network.transfers().iter().any(|t| t.state == TransferState::Running)
                    {
                        self.publish();
                    }
                }
            }
        }
        self.stop_plugins().await;
        self.network.close().await;
    }

    fn peer(&self, id: &PeerId) -> Option<Peer> {
        self.network.peers().into_iter().find(|p| &p.id == id)
    }

    /// Withdraw the work already started for one device, without touching the plugin.
    ///
    /// The plugin keeps running and stays registered, so it carries on serving every other
    /// device. Only the calls this application made for `device` are withdrawn, by id, and
    /// the plugin is told each was cancelled. `plugin` limits it to one plugin; `None`
    /// withdraws the device's work from every plugin, as when the device is forgotten.
    fn withdraw_device_work(&mut self, plugin: Option<&PluginId>, device: &PeerId) {
        let mut ids = match plugin {
            Some(plugin) => self.in_flight.take_for_device(plugin, device),
            None => self.in_flight.take_all_for_device(device),
        };
        // What was withdrawn will never be answered, so it must not keep showing as running.
        self.board.forget_for(device, plugin);
        // What the device had sent for a plugin and nobody has collected yet is dropped too,
        // or a device whose permission was just withdrawn, or that was just forgotten, could
        // still have its bytes delivered by a plugin that connects within the waiting time.
        // The calls that announced those streams are withdrawn with them.
        ids.extend(self.parked.drop_for(device, plugin));
        // A transfer that has already started is out of reach of the line above: it is a task
        // copying the device's bytes, and it would go on until the device stopped sending,
        // whatever the user decided meanwhile.
        self.carrying.stop_for(device, plugin);
        // And what a plugin was sending to this device, started or not. Dropping the waiting
        // end fails the task that was about to open the stream; stopping the task covers one
        // already carrying bytes. The numbers dropped are claims, never call numbers, so they
        // are not added to `ids`.
        self.outbound.drop_for(device, plugin);
        self.carrying_out.stop_for(device, plugin);
        self.cancel_calls(ids);
    }

    /// Withdraw calls the application made, by number, at the hub. The plugin is told each
    /// was cancelled. A call that already ended is simply gone from the hub, which is fine.
    fn cancel_calls(&mut self, ids: Vec<u64>) {
        for id in ids {
            let _ = self
                .host
                .submit(peersus_plugin_api::message::Operation::Cancel { id });
        }
    }

    /// Restart a plugin so it starts again from its current configuration.
    ///
    /// This is for a change that only a new process can pick up, such as one of the
    /// plugin's own global settings. It is deliberately not used for a permission change on
    /// a device: that must never disturb what the plugin is doing for anyone else.
    ///
    /// The supervisor does it itself, so it costs nothing of the budget meant for real
    /// failures. A plugin that had already given up has no supervisor left to ask, which is
    /// exactly when the request is refused, so its finished service is dropped (nothing of it
    /// is still running) and the plugin is started again from scratch: changing its settings
    /// is a deliberate act and a fresh start is what the user would expect.
    fn restart_plugin(&mut self, plugin: &PluginId) {
        self.revoke_platform_calls(plugin);
        // Whatever was outstanding went with the old process. Forget it, or the same actions
        // could never be started again.
        self.in_flight.take_for_plugin(plugin);
        // Nothing the old process was doing will ever be answered, so none of it may keep
        // showing as running.
        self.board.forget_plugin(plugin);
        // The new process starts with no statuses and none on: what the old one said of this device is not true of a process that has not
        // yet said anything, and the plugin makes its statuses again when it runs.
        self.forget_statuses(plugin);
        // What peers had sent for the old process cannot be collected by the new one. The
        // streams are dropped, ending the peers' side, and the calls that announced them are
        // withdrawn so nothing waits on them.
        let announced = self.parked.drop_for_plugin(plugin);
        // A transfer already carrying bytes into the old process is stopped as well.
        self.carrying.stop_for_plugin(plugin);
        // The same for what this plugin was sending out. These are claim numbers, not call
        // numbers: they are dropped here and never handed to `cancel_calls`, or a random
        // number could withdraw an unrelated call at the hub.
        self.outbound.drop_for_plugin(plugin);
        self.carrying_out.stop_for_plugin(plugin);
        self.cancel_calls(announced);
        let asked = self
            .running
            .get(plugin)
            .is_some_and(|running| running.service.restart());
        if !asked
            && let Some(spec) = self
                .plugins
                .find(plugin)
                .and_then(|i| self.plugins.get(i))
                .cloned()
        {
            self.running.remove(plugin);
            self.start_one(spec);
        }
    }

    /// Everything a plugin's old process was doing is withdrawn, as when it is restarted, and the
    /// process itself is stopped and not started again.
    async fn stop_plugin(&mut self, plugin: &PluginId) {
        self.revoke_platform_calls(plugin);
        self.in_flight.take_for_plugin(plugin);
        self.board.forget_plugin(plugin);
        // What the plugin said of this device goes with it: nothing keeps it true any more, and another device must not go on showing it.
        self.forget_statuses(plugin);
        let announced = self.parked.drop_for_plugin(plugin);
        self.carrying.stop_for_plugin(plugin);
        self.outbound.drop_for_plugin(plugin);
        self.carrying_out.stop_for_plugin(plugin);
        self.cancel_calls(announced);
        if let Some(running) = self.running.remove(plugin) {
            running.service.shutdown().await;
        }
    }

    /// The store of packages, or the reason there is none.
    fn packages(&self) -> Result<&crate::plugin_manager::Manager, &'static str> {
        self.packages
            .as_ref()
            .ok_or("Plugins cannot be installed here")
    }

    /// Stop an installed plugin and keep it from running until it is enabled, also after the
    /// application restarts. It stays installed and keeps what it was granted.
    async fn disable_plugin(&mut self, plugin: PluginId) -> Result<(), &'static str> {
        let spec = self
            .plugins
            .find(&plugin)
            .and_then(|index| self.plugins.get(index))
            .ok_or("Unknown plugin")?;
        // Bundled services have no external folder. Keep their existing in-memory switch;
        // adding a package store must not make their lifecycle depend on a missing folder.
        if let Some(manager) = &self.packages
            && spec.program.is_some()
        {
            manager
                .set_disabled(plugin.as_str(), true)
                .map_err(|refusal| refusal.0)?;
        }
        self.disabled.insert(plugin.clone());
        self.stop_plugin(&plugin).await;
        Ok(())
    }

    /// Let an installed plugin run again, and remember that it is to run at the next start too. It
    /// keeps what it was granted.
    fn enable_plugin(&mut self, plugin: PluginId) -> Result<(), &'static str> {
        let spec = self
            .plugins
            .find(&plugin)
            .and_then(|index| self.plugins.get(index))
            .ok_or("Unknown plugin")?;
        if let Some(manager) = &self.packages
            && spec.program.is_some()
        {
            manager
                .set_disabled(plugin.as_str(), false)
                .map_err(|refusal| refusal.0)?;
        }
        self.disabled.remove(&plugin);
        self.start_if_stopped(&plugin)
    }

    /// Approve a plugin that was found in the store and is not run, as the person was shown it, and start it. The digest
    /// given is the one this owner reported with the plugin; the manager refuses if the program is not that one any more.
    ///
    /// What the plugin had been granted before is forgotten, as for a replacement: a program that was changed is a different
    /// program, and must never start with the rights the old one was given. A plugin approved for the first time has none.
    fn approve_plugin(&mut self, plugin: PluginId, program: &str) -> Result<(), &'static str> {
        let held = self
            .held_back
            .iter()
            .find(|held| held.id == plugin)
            .ok_or("Unknown plugin")?;
        // The owner reported a digest only for what can be approved, and the window can only give back what it was shown:
        // a digest that is not the one reported for this plugin is not what it saw.
        if !matches!(
            held.standing,
            peersus_core::model::Standing::NeedsApproval | peersus_core::model::Standing::Changed
        ) || held.program.as_deref() != Some(program)
        {
            return Err("The plugin is not what was shown; look at it again");
        }
        self.packages()?
            .approve(plugin.as_str(), program)
            .map_err(|refusal| refusal.0)?;
        // The old rights are forgotten before it runs, so it never runs with them.
        self.hub.forget(plugin.as_str());
        self.network
            .forget_plugin(&plugin)
            .map_err(|_| "Could not forget what it was granted")?;
        self.standalone
            .forget_plugin(&plugin)
            .map_err(|_| "Could not forget what it was granted")?;
        self.sync_host_grants();
        self.sync_plugin_grants();
        let manager = self.packages()?;
        let spec = crate::discovery::load_as(
            manager.store(),
            &manager.store().join(plugin.as_str()),
            plugin.as_str(),
        )?;
        self.plugins
            .register(spec.clone())
            .map_err(|_| "The plugin could not be registered")?;
        self.sync_host_grants();
        self.held_back.retain(|held| held.id != plugin);
        self.start_one(spec);
        Ok(())
    }

    /// Start the plugin if it is not running, without touching what is remembered about whether it
    /// is switched off. What puts a plugin back after a removal the store refused.
    fn start_if_stopped(&mut self, plugin: &PluginId) -> Result<(), &'static str> {
        let index = self.plugins.find(plugin).ok_or("Unknown plugin")?;
        let spec = self.plugins.get(index).cloned().ok_or("Unknown plugin")?;
        if self.running.contains_key(plugin) {
            return Ok(());
        }
        self.start_one(spec);
        Ok(())
    }

    /// Take a plugin out: stop it, delete its package, forget everything it was granted in the same
    /// step, and drop it from the registry. A package installed afterwards under the same name
    /// starts with nothing.
    async fn remove_package(&mut self, plugin: PluginId) -> Result<(), &'static str> {
        // Refused before anything is touched when there is no store or it is not installed.
        self.packages()?;
        if self
            .plugins
            .find(&plugin)
            .and_then(|index| self.plugins.get(index))
            .is_some_and(|spec| spec.program.is_none())
        {
            return Err("Bundled plugins cannot be removed");
        }
        // A plugin that was found in the store and is not run (not approved, changed, broken) is not in the registry and has
        // no process, and it is exactly the one a person most needs to remove: so it is known by the list of what was held
        // back, and everything below that is about a running process is skipped for it. The store removes it by the same
        // rule as any other.
        let held_back = self.held_back.iter().any(|held| held.id == plugin);
        if !held_back {
            self.plugins.find(&plugin).ok_or("Unknown plugin")?;
        }
        // The process is stopped FIRST, so that its files are never taken from under it while it
        // runs; then its files, what it was granted, and its place in the registry go.
        let was_running = self.running.contains_key(&plugin);
        self.stop_plugin(&plugin).await;
        if let Err(refusal) = self.packages()?.remove(plugin.as_str()) {
            // The store would not delete it: nothing was removed, so what was stopped to make room
            // for the removal is started again, and a refused "Remove" never leaves the plugin
            // quietly switched off.
            if was_running {
                let _ = self.start_if_stopped(&plugin);
            }
            return Err(refusal.0);
        }
        let _ = self.plugins.remove(&plugin);
        self.held_back.retain(|held| held.id != plugin);
        // The hub keeps what a participant was granted when its process ends, so that a restart
        // finds it. Removing a package is not a restart: what it was granted goes from memory too,
        // or a package installed under the same name in this same run would inherit it.
        self.hub.forget(plugin.as_str());
        // And it is no longer remembered as switched off: a package installed under the same name
        // is a new plugin, and starts as one.
        self.disabled.remove(&plugin);
        self.network
            .forget_plugin(&plugin)
            .map_err(|_| "Could not forget what it was granted")?;
        self.standalone
            .forget_plugin(&plugin)
            .map_err(|_| "Could not forget what it was granted")?;
        self.sync_host_grants();
        self.sync_plugin_grants();
        Ok(())
    }

    /// Read the package in `source` and keep it ready for confirmation. Nothing is installed.
    fn stage_install(&mut self, source: &std::path::Path) -> Result<(), &'static str> {
        self.packages()?;
        // A folder is the package itself. A file is an archive of one, unpacked into a private
        // folder that is kept for as long as the package waits, and read from there exactly as a
        // folder chosen by hand is: one set of rules, one set of refusals.
        let unpacked = match std::fs::metadata(source) {
            Ok(found) if found.is_file() => Some(
                crate::archive::unpack_in(source, &self.plugin_directory)
                    .map_err(|refused| refused.0)?,
            ),
            _ => None,
        };
        let source = unpacked.as_ref().map_or(source, |held| held.root());
        let id = crate::plugin_manager::package_id(source).map_err(|refusal| refusal.0)?;
        let plugin = PluginId::new(id).map_err(|_| "The package has no valid name")?;
        if self
            .plugins
            .find(&plugin)
            .and_then(|index| self.plugins.get(index))
            .is_some_and(|spec| spec.program.is_none())
        {
            return Err("An external package cannot replace a bundled plugin");
        }
        // What it asks for is read now and kept, so that what the person is shown is what is
        // installed if they confirm: confirming names no path and reads nothing again.
        let (manifest, package) = crate::discovery::read_package_and_info(source)?;
        crate::plugin_host::check_external_execution(manifest.execution)?;
        // And its digest now, so that confirming can tell if anything in the folder has changed
        // since: the person is shown a package, and a package is what they confirm.
        let digest = crate::plugin_manager::package_digest(source).map_err(|refusal| refusal.0)?;
        let source = source.to_path_buf();
        self.staged.insert(
            plugin,
            Staged {
                source,
                manifest,
                digest,
                _unpacked: unpacked,
                package,
            },
        );
        Ok(())
    }

    /// Let go of a package that was shown and is not going to be installed. Nothing of it is kept.
    fn cancel_install(&mut self, plugin: &PluginId) -> Result<(), &'static str> {
        self.staged
            .remove(plugin)
            .map(|_| ())
            .ok_or("Nothing was shown for that plugin")
    }

    /// Read what each installed package says about itself from its folder, and tell the store which version is
    /// installed under each name. Bounded: at most [`crate::discovery::MAX_PLUGINS`] manifests of at most
    /// [`crate::discovery::MAX_MANIFEST`] bytes. A folder that is broken or not approved still counts: an update
    /// should be offered for it. Called after anything that can change the installed set, never from `publish`.
    fn refresh_installed(&mut self) {
        let Some(manager) = &self.packages else {
            return;
        };
        let mut found = std::collections::BTreeMap::new();
        if let Ok(entries) = manager.list() {
            for entry in entries.into_iter().take(crate::discovery::MAX_PLUGINS) {
                let Ok(id) = PluginId::new(entry.id.as_str()) else {
                    continue;
                };
                if let Ok((_, Some(package))) =
                    crate::discovery::read_package_and_info(&manager.store().join(&entry.id))
                {
                    found.insert(id, package);
                }
            }
        }
        self.installed_packages = found;
        #[cfg(unix)]
        if let Some(slot) = self.store.as_mut() {
            let versions = self
                .installed_packages
                .iter()
                .map(|(id, package)| (id.clone(), package.version.clone()))
                .collect();
            slot.service.set_installed(&versions);
        }
    }

    /// The store's state as a window is told it. Without a configured store the store does not exist
    /// here, and the window is told it is not set up.
    fn store_state(&self) -> peersus_core::model::StoreState {
        #[cfg(unix)]
        if let Some(slot) = &self.store {
            return slot.service.state().clone();
        }
        peersus_core::model::StoreState {
            status: peersus_core::model::StoreStatus::NotConfigured,
            ..Default::default()
        }
    }

    /// Begin fetching the catalog. Returns at once: the download runs in a task and its result is
    /// verified on the event loop by [`Self::finish_store`], so a slow network never stalls the owner.
    #[cfg(unix)]
    fn start_store_refresh(&mut self) -> Result<(), &'static str> {
        let slot = self
            .store
            .as_mut()
            .ok_or("The extension store is not available here")?;
        slot.service.begin_refresh().map_err(|refusal| refusal.0)?;
        let fetcher = slot.fetcher.clone();
        self.store_tasks.spawn(async move {
            let fetched = async {
                let catalog = fetcher
                    .fetch(
                        crate::store::CATALOG_FILE.to_owned(),
                        crate::catalog::MAX_CATALOG_BYTES,
                    )
                    .await?;
                let signature = fetcher
                    .fetch(crate::store::SIGNATURE_FILE.to_owned(), 64)
                    .await?;
                Ok((catalog, signature))
            };
            StoreOutcome::Refreshed(fetched.await)
        });
        Ok(())
    }

    /// Begin downloading the approved package for `plugin`. What is fetched is the signed entry's, never
    /// the command's. Refused before anything is fetched when packages cannot be installed here.
    #[cfg(unix)]
    fn start_store_install(&mut self, plugin: PluginId) -> Result<(), &'static str> {
        self.packages()?;
        if self
            .plugins
            .find(&plugin)
            .and_then(|index| self.plugins.get(index))
            .is_some_and(|spec| spec.program.is_none())
        {
            return Err("An external package cannot replace a bundled plugin");
        }
        // A package the person staged by hand for this name is waiting on THEIR confirmation. A store
        // install must not silently replace it (the staged card would then install something else).
        if self.staged.contains_key(&plugin) {
            return Err(
                "A package for this name is waiting to be confirmed; confirm or cancel it first",
            );
        }
        let slot = self
            .store
            .as_mut()
            .ok_or("The extension store is not available here")?;
        let fetch = slot
            .service
            .begin_install(&plugin)
            .map_err(|refusal| refusal.0)?;
        let fetcher = slot.fetcher.clone();
        self.store_tasks.spawn(async move {
            StoreOutcome::Downloaded {
                plugin,
                bytes: fetcher.fetch(fetch.archive, fetch.limit).await,
            }
        });
        Ok(())
    }

    #[cfg(not(unix))]
    fn start_store_refresh(&mut self) -> Result<(), &'static str> {
        Err("The extension store is not available on this system")
    }

    #[cfg(not(unix))]
    fn start_store_install(&mut self, _plugin: PluginId) -> Result<(), &'static str> {
        Err("The extension store is not available on this system")
    }

    /// A download finished. Everything that trusts or installs happens here, on the event loop. Each
    /// branch computes one result and there is ONE exit that releases the store, so a failure can never
    /// leave it reading as busy. `self.store` is never taken out: an install awaits and may publish, and
    /// a snapshot published meanwhile must still carry the store's real state.
    #[cfg(unix)]
    async fn finish_store(&mut self, outcome: StoreOutcome) {
        let scratch = self.plugin_directory.clone();
        let Some(slot) = self.store.as_mut() else {
            return;
        };
        let result: Result<(), &'static str> = match outcome {
            StoreOutcome::Refreshed(Ok((catalog, signature))) => slot
                .service
                .finish_refresh(&catalog, &signature)
                .map_err(|refusal| refusal.0),
            StoreOutcome::Refreshed(Err(refusal))
            | StoreOutcome::Downloaded {
                bytes: Err(refusal),
                ..
            } => {
                slot.service.failed(refusal.0);
                Err(refusal.0)
            }
            StoreOutcome::Downloaded {
                plugin,
                bytes: Ok(bytes),
            } => {
                match slot.service.finish_install(&plugin, bytes, &scratch) {
                    // The service already recorded and released the failure.
                    Err(refusal) => Err(refusal.0),
                    // The ordinary confirm path: replacement, rollback and forgetting grants all live there.
                    Ok(verified) => {
                        let installed = self.confirm_verified(plugin, verified).await;
                        match (self.store.as_mut(), installed) {
                            (Some(slot), Ok(())) => {
                                slot.service.done();
                                Ok(())
                            }
                            (Some(slot), Err(reason)) => {
                                slot.service.failed(reason);
                                Err(reason)
                            }
                            (None, outcome) => outcome,
                        }
                    }
                }
            }
        };
        // Every outcome can change what is installed or what is listed: a refresh adopts new listings that need
        // their installed versions, and an install changes one. Read once here, off the publish path.
        self.refresh_installed();
        if let Err(reason) = result {
            self.error(reason);
        }
    }

    /// Install what the store verified through [`Self::confirm_install`]: the verified package takes the
    /// place of what a person would have chosen and been shown. The click on Install was the confirmation.
    #[cfg(unix)]
    async fn confirm_verified(
        &mut self,
        plugin: PluginId,
        verified: crate::store_service::Verified,
    ) -> Result<(), &'static str> {
        let source = verified.unpacked.root().to_path_buf();
        let (manifest, package) = crate::discovery::read_package_and_info(&source)?;
        crate::plugin_host::check_external_execution(manifest.execution)?;
        let digest = crate::plugin_manager::package_digest(&source).map_err(|refusal| refusal.0)?;
        // The unpacked folder moves into the staged entry so it lives until the install has finished.
        self.staged.insert(
            plugin.clone(),
            Staged {
                source,
                manifest,
                digest,
                _unpacked: Some(verified.unpacked),
                package,
            },
        );
        self.confirm_install(plugin).await
    }

    /// Install the package that was staged for `plugin`, and only that one, and start it. It
    /// starts with no permissions, and no restart is needed.
    async fn confirm_install(&mut self, plugin: PluginId) -> Result<(), &'static str> {
        let staged = self
            .staged
            .remove(&plugin)
            .ok_or("Nothing was shown for that plugin")?;
        // Taken out first, so that a refusal does not leave it waiting to be confirmed again as
        // if nothing had happened: the person has to choose it again and be shown it again.
        if !crate::plugin_manager::still_what_was_shown(
            &staged.digest,
            crate::plugin_manager::package_digest(&staged.source),
        ) {
            return Err("The package changed after it was shown; choose it again");
        }
        let source = staged.source;
        self.packages()?;
        // A name that is installed is REPLACED: the old plugin is taken out first (stopped, its files
        // and everything it was granted gone, as a removal does), and the new one goes in as a new
        // plugin that starts with nothing. Done by the same steps as `remove_package`, so there is
        // one way a plugin goes and no second one to drift.
        let replaced = self.plugins.find(&plugin).is_some();
        let kept_files = if replaced {
            Some(self.set_aside_installed(&plugin).await?)
        } else {
            None
        };
        match self.install_and_start(&plugin, &source) {
            Ok(()) => {
                // The old package is gone for good only now that the new one is in.
                if let Some(directory) = kept_files {
                    let _ = std::fs::remove_dir_all(directory);
                }
                Ok(())
            }
            Err(refusal) => {
                // Not installed: the old plugin is put back exactly as it was, and started again.
                if let Some(directory) = kept_files {
                    self.put_back_installed(&plugin, &directory);
                }
                Err(refusal)
            }
        }
    }

    /// Install the package in `source` as `plugin`, register it and start it.
    fn install_and_start(
        &mut self,
        plugin: &PluginId,
        source: &std::path::Path,
    ) -> Result<(), &'static str> {
        let manager = self.packages()?;
        manager.install(source).map_err(|refusal| refusal.0)?;
        let spec = crate::discovery::load_as(
            manager.store(),
            &manager.store().join(plugin.as_str()),
            plugin.as_str(),
        )?;
        self.plugins
            .register(spec.clone())
            .map_err(|_| "The plugin could not be registered")?;
        self.sync_host_grants();
        self.start_one(spec);
        Ok(())
    }

    /// The first half of a replacement: the plugin that is installed under this name is stopped,
    /// taken out of the store but KEPT, dropped from the registry, and everything it was granted
    /// forgotten, by the same steps as a removal. Returns where its files are kept.
    async fn set_aside_installed(&mut self, plugin: &PluginId) -> Result<PathBuf, &'static str> {
        self.stop_plugin(plugin).await;
        let kept = match self.packages()?.retire(plugin.as_str()) {
            Ok(kept) => kept,
            Err(refusal) => {
                // Nothing was taken out, so what was stopped to make room is started again.
                let _ = self.start_if_stopped(plugin);
                return Err(refusal.0);
            }
        };
        let _ = self.plugins.remove(plugin);
        self.hub.forget(plugin.as_str());
        self.disabled.remove(plugin);
        // A failure to forget what it was granted is a failure of the replacement, and the old one
        // is put back by the caller: a new plugin must never start with the old one's rights.
        let forgot = self
            .network
            .forget_plugin(plugin)
            .and_then(|_| self.standalone.forget_plugin(plugin));
        self.sync_host_grants();
        self.sync_plugin_grants();
        forgot
            .map(|_| kept)
            .map_err(|_| "Could not forget what it was granted")
    }

    /// The second half, when the new package could not go in: the old plugin is put back exactly as
    /// it was, registered again and started again. What it had been granted was forgotten and is not
    /// restored: a failed replacement leaves it with nothing allowed, never with more than it had.
    fn put_back_installed(&mut self, plugin: &PluginId, kept: &std::path::Path) {
        let Ok(manager) = self.packages() else { return };
        if manager.restore(plugin.as_str(), kept).is_err() {
            return;
        }
        let Ok(spec) = crate::discovery::load_as(
            manager.store(),
            &manager.store().join(plugin.as_str()),
            plugin.as_str(),
        ) else {
            return;
        };
        if self.plugins.register(spec).is_ok() {
            self.sync_host_grants();
            let _ = self.start_if_stopped(plugin);
        }
    }

    /// Route an inbound peer stream to the plugin whose manifest exports its service.
    ///
    /// The stream is not handed to the plugin: the plugin is a separate process that must be
    /// told about it and then connects back to fetch the bytes. So it is set aside under the
    /// number of a call to the plugin's `receive`, and the plugin names that call when it
    /// connects. If the hub refuses the call nobody will ever come for it, so it is taken
    /// back at once and dropped, which also ends the peer's side, instead of waiting for
    /// the deadline.
    fn route_inbound(
        &mut self,
        peer: &PeerId,
        service: &str,
        version: u32,
        send: iroh::endpoint::SendStream,
        receive: iroh::endpoint::RecvStream,
    ) -> Result<(), &'static str> {
        // Everything that may refuse comes before anything is kept, and the peer list is
        // read fresh: a device forgotten after transport routing must not be served.
        let admitted = admit(&self.plugins, &self.network.peers(), peer, service, version)?;
        self.next_call = self.next_call.checked_add(1).ok_or("Too many calls")?;
        let id = self.next_call;
        self.parked
            .park(
                id,
                crate::parked::Owner {
                    peer: peer.clone(),
                    plugin: admitted.plugin.clone(),
                },
                Stream { send, receive },
                std::time::Instant::now(),
                crate::parked::MAX_WAIT,
            )
            .map_err(|_| "Too many transfers are waiting for plugins")?;
        let sent = self
            .host
            .submit(peersus_plugin_api::message::Operation::Call {
                id,
                provider: admitted.plugin.as_str().to_owned(),
                service: admitted.service,
                version: admitted.version,
                method: crate::plugin_host::RECEIVE_METHOD.to_owned(),
                args: peer_argument(&self.aliases, &admitted.plugin, peer),
                timeout_ms: 15_000,
            });
        if let Err(error) = sent {
            // Taken back and dropped: the peer's stream ends now, not at the deadline.
            let _ = self.parked.claim(id, std::time::Instant::now());
            return Err(refusal_text(&error));
        }
        Ok(())
    }

    /// A plugin has connected back for the bytes of a call. Pair its connection with what
    /// was set aside for that call and carry them across, in the background.
    ///
    /// The pairing is by the number the application gave the call, but that number alone
    /// names no one: every participant numbers its own calls, so a plugin can call itself
    /// with the very number the application used, and hold a perfectly genuine right for
    /// that call. So the hub's stamp of who made the call is checked first, and only the
    /// application's own calls may collect what the application set aside. The stream can
    /// be claimed once. If nothing is parked under that number (it expired, was already
    /// taken, or the device was forgotten meanwhile) the plugin's connection is simply
    /// dropped, and the plugin learns it got nothing.
    #[cfg(unix)]
    fn begin_transfer(&mut self, accepted: Accepted) {
        let Accepted {
            plugin,
            right,
            stream,
        } = accepted;
        let right = match right {
            peersus_plugin_host::data::Right::Hub(right) => right,
            // A claim names a send the application opened for this plugin. Who is presenting
            // it is the channel the connection arrived on, never anything the plugin wrote,
            // and the entry is taken only if it was issued to exactly that plugin, so one
            // plugin cannot collect another's send or use up its number by trying.
            peersus_plugin_host::data::Right::Claim(claim) => {
                let Ok(plugin) = PluginId::new(&plugin) else {
                    return;
                };
                if let Some((_owner, waiting)) =
                    self.outbound
                        .claim_for(claim, &plugin, std::time::Instant::now())
                {
                    // The task that waits on the other end carries it from here. If it is
                    // already gone (withdrawn, or too late) the connection is simply dropped.
                    let _ = waiting.send(stream);
                }
                return;
            }
        };
        if !may_collect(right.caller()) {
            return;
        }
        let Some((owner, parked)) = self
            .parked
            .claim_owned(right.call_id(), std::time::Instant::now())
        else {
            return;
        };
        let call = right.call_id();
        let handle = self.transfers.spawn(async move {
            let Stream { send, receive } = parked;
            let outcome = crate::receive::deliver(
                receive,
                stream,
                crate::receive::DEFAULT_LIMIT,
                peersus_plugin_host::pump::IDLE_LIMIT,
            )
            .await;
            // The peer's sending half is finished either way, so its side ends cleanly
            // instead of waiting on a transfer that is over.
            let mut send = send;
            let _ = send.finish();
            (Direction::Inbound, call, outcome)
        });
        // Recorded under the device and the plugin, or a permission withdrawn later could
        // not reach a transfer that is already carrying bytes.
        self.carrying.started(call, owner, handle);
    }

    /// The most files one command may offer.
    const MOST_FILES_AT_ONCE: usize = 16;

    /// How many files of the folders being sent are offered at each turn of the loop, before the events get theirs.
    const FOLDER_FILES_PER_TURN: usize = 8;

    /// Offer `files` to `peer`, each under the name that goes with it. The one place that judges a send, so that
    /// every way of sending is judged the same: between one and sixteen files, every source opened before any is
    /// offered (so one bad file cannot make a partial offer of a command that asked for several), and what was
    /// already offered withdrawn if a later offer fails.
    fn offer_sources(
        &mut self,
        peer: &PeerId,
        sources: Vec<Source>,
    ) -> Result<Reply, &'static str> {
        let mut transfers = Vec::with_capacity(sources.len());
        for source in sources {
            match self.network.send(peer, source, now()) {
                Ok(id) => transfers.push(id),
                Err(_) => {
                    // A later offer failed: what was offered before it is withdrawn, so that the person does not
                    // find half of what they chose on the other device.
                    for id in &transfers {
                        let _ = self.network.cancel(id);
                    }
                    return Err("Could not offer selected files");
                }
            }
        }
        Ok(Reply::Transfers(transfers))
    }

    /// Every file the interface kept open by number, taken and judged BEFORE anything is offered, so that one that is unknown or
    /// fails (a name the rule refuses) never leaves a partial offer of a command that asked for several. Whatever is not taken
    /// when the command is refused (too many, a number that is gone, a file that fails) is let go of here, by every number of the
    /// command: the interface cannot know which the owner reached, and a number kept would fill the registry for the next choice.
    async fn take_held(&mut self, files: Vec<(u64, String)>) -> Result<Vec<Source>, &'static str> {
        let numbers: Vec<u64> = files.iter().map(|(number, _)| *number).collect();
        let result = self.take_held_sources(files).await;
        if result.is_err() {
            for number in numbers {
                self.held.release(number);
            }
        }
        result
    }

    async fn take_held_sources(
        &mut self,
        files: Vec<(u64, String)>,
    ) -> Result<Vec<Source>, &'static str> {
        if files.is_empty() || files.len() > Self::MOST_FILES_AT_ONCE {
            return Err("Choose between one and sixteen files");
        }
        let mut sources = Vec::with_capacity(files.len());
        for (number, name) in files {
            let file = self
                .held
                .take(number)
                .ok_or("A chosen file is no longer available")?;
            sources.push(
                Source::from_file(file, &name)
                    .await
                    .map_err(|_| "Could not open selected file")?,
            );
        }
        Ok(sources)
    }

    /// The code that hands over `sources`, as text. The files wait with the code, in the network host.
    fn files_code(&mut self, sources: Vec<Source>) -> Result<Reply, &'static str> {
        let invitation = self
            .network
            .invite_for_files(sources, now())
            .map_err(|_| "Could not create invitation")?;
        self.files_invitation_reply(invitation)
    }

    fn files_invitation_reply(&self, invitation: Invitation) -> Result<Reply, &'static str> {
        let generation = self
            .network
            .files_code()
            .map(|code| code.generation)
            .ok_or("Could not create invitation")?;
        let expires_at = invitation.expires_at;
        let code = invitation
            .encode()
            .map_err(|_| "Could not create invitation")?;
        Ok(Reply::FilesInvitation {
            code,
            generation,
            expires_at,
        })
    }

    fn can_queue_destination(
        &self,
        destination: &FileDestination,
        count: usize,
    ) -> Result<bool, &'static str> {
        match destination {
            FileDestination::Peer { peer } => self
                .network
                .can_queue_files(peer, count)
                .map_err(|_| "Device is offline"),
            FileDestination::Invitation { generation } => self
                .network
                .can_queue_selection(*generation, count, now())
                .map_err(|_| "Files selection is no longer active"),
        }
    }

    fn queue_destination(
        &mut self,
        destination: FileDestination,
        sources: Vec<Source>,
    ) -> Result<Reply, &'static str> {
        match destination {
            FileDestination::Peer { peer } => self.offer_sources(&peer, sources),
            FileDestination::Invitation { generation } => {
                self.network
                    .queue_selection(generation, sources, now())
                    .map_err(|_| "Could not queue selected files")?;
                Ok(Reply::Transfers(Vec::new()))
            }
        }
    }

    async fn execute(&mut self, command: Command) -> Result<Reply, &'static str> {
        match command {
            Command::CreateInvitation {} => {
                return self
                    .network
                    .invite(now())
                    .and_then(|invitation| invitation.encode())
                    .map(Reply::Invitation)
                    .map_err(|_| "Could not create invitation");
            }
            Command::JoinInvitation { invitation } => {
                let invitation = Invitation::decode(&invitation, now())
                    .map_err(|_| "Invalid or expired invitation")?;
                self.network
                    .join(invitation)
                    .map_err(|_| "Connection queue is full")?;
            }
            // A code to hand over files. Every file is opened and judged BEFORE the code exists, as for a send to a paired
            // device, so that a code is never shown for files that could not be offered. Nothing is sent now: the files
            // are parked with the code and offered when a device that scanned it is admitted.
            Command::CreateFilesInvitation { paths } => {
                if paths.is_empty() || paths.len() > Self::MOST_FILES_AT_ONCE {
                    return Err("Choose between one and sixteen files");
                }
                let mut sources = Vec::with_capacity(paths.len());
                for path in paths {
                    sources.push(
                        Source::open(&path)
                            .await
                            .map_err(|_| "Could not open selected file")?,
                    );
                }
                return self.files_code(sources);
            }
            Command::CreateHeldFilesInvitation { files } => {
                let sources = self.take_held(files).await?;
                return self.files_code(sources);
            }
            Command::BeginFilesInvitation { count } => {
                let invitation = self
                    .network
                    .begin_files_selection(count as usize, now())
                    .map_err(|_| "Could not create invitation")?;
                return self.files_invitation_reply(invitation);
            }
            Command::FinishFilesInvitation {
                generation,
                refused,
            } => {
                self.network
                    .finish_files_selection(generation, refused as usize)
                    .map_err(|_| "Files selection is not fully accounted for")?;
            }
            Command::ChoosePairing { request, number } => self.network.choose(&request, number)?,
            Command::RejectPairing { request } => {
                if !self.network.reject(&request) {
                    return Err("Pairing request is no longer active");
                }
            }
            Command::ForgetPeer { peer } => {
                self.network
                    .forget(&peer)
                    .map_err(|_| "Could not forget device")?;
                self.withdraw_device_work(None, &peer);
            }
            Command::QueueFiles { destination, paths } => {
                if paths.is_empty() || paths.len() > Self::MOST_FILES_AT_ONCE {
                    return Err("Choose between one and sixteen files");
                }
                if !self.can_queue_destination(&destination, paths.len())? {
                    return Ok(Reply::QueueBusy);
                }
                let mut sources = Vec::with_capacity(paths.len());
                for path in paths {
                    sources.push(
                        Source::open(&path)
                            .await
                            .map_err(|_| "Could not open selected file")?,
                    );
                }
                return self.queue_destination(destination, sources);
            }
            Command::QueueHeld { destination, files } => {
                if files.is_empty() || files.len() > Self::MOST_FILES_AT_ONCE {
                    return Err("Choose between one and sixteen files");
                }
                if !self.can_queue_destination(&destination, files.len())? {
                    return Ok(Reply::QueueBusy);
                }
                let sources = self.take_held(files).await?;
                return self.queue_destination(destination, sources);
            }
            Command::SendFiles { peer, paths } => {
                // The name an offer carries is the path's own. The count is judged before anything is opened.
                if paths.is_empty() || paths.len() > Self::MOST_FILES_AT_ONCE {
                    return Err("Choose between one and sixteen files");
                }
                let mut sources = Vec::with_capacity(paths.len());
                for path in paths {
                    sources.push(
                        Source::open(&path)
                            .await
                            .map_err(|_| "Could not open selected file")?,
                    );
                }
                return self.offer_sources(&peer, sources);
            }
            Command::SendHeld { peer, files } => {
                let sources = self.take_held(files).await?;
                return self.offer_sources(&peer, sources);
            }
            Command::SendFolder { peer, root } => {
                self.network
                    .send_folder(&peer, &root)
                    .map_err(|_| "Could not open selected folder or device is offline")?;
            }
            Command::AcceptTransfer {
                transfer,
                directory,
            } => self
                .network
                .accept(&transfer, directory)
                .map_err(|_| "Could not accept transfer")?,
            Command::CancelTransfer { transfer } => self
                .network
                .cancel(&transfer)
                .map_err(|_| "Could not cancel transfer")?,
            Command::ClearHistory {} => {
                self.network
                    .clear_history()
                    .map_err(|_| "Could not clear the history")?;
            }
            Command::UpdateSettings { settings, expected } => {
                if self.settings.current() != &*expected {
                    return Err("Preferences changed elsewhere; reload before saving");
                }
                self.apply_settings(*settings).await?;
            }
            Command::PatchSettings { patch } => {
                // Said as it is: a person told "conflict" cannot tell what to change, and the refusal knows.
                let settings = patch
                    .apply(self.settings.current())
                    .map_err(|error| error.words())?;
                self.apply_settings(settings).await?;
            }
            Command::SetGrants {
                plugin,
                peer,
                grants,
            } => self.set_grants(plugin, peer, grants)?,
            Command::Allow {
                plugin,
                peer,
                permission,
                on,
            } => {
                // What is held now, not what the window last saw: two flips in quick succession
                // each start from the other's result.
                let held = match &peer {
                    Some(peer) => self
                        .peer(peer)
                        .and_then(|found| found.grants.get(&plugin).cloned()),
                    None => self.standalone.get(&plugin).cloned(),
                }
                .unwrap_or_default();
                let grants = applied(&held, &permission, on)
                    .map_err(|_| "Could not change this permission")?;
                self.set_grants(plugin, peer, grants)?
            }
            Command::SetPluginSetting {
                plugin,
                peer,
                setting,
                value,
            } => {
                self.set_plugin_setting(plugin, peer, &setting, value)
                    .await?
            }
            Command::InvokeAction {
                plugin,
                action,
                peer,
            } => self.invoke(plugin, &action, peer, None)?,
            Command::InvokeActionWithContext {
                plugin,
                action,
                peer,
                context,
            } => {
                if context == 0 {
                    return Err("Action context is invalid");
                }
                self.invoke(plugin, &action, peer, Some(context))?;
            }
            Command::InvokeSelectedAction {
                plugin,
                action,
                path,
                held,
            } => {
                let key = Key {
                    plugin: plugin.clone(),
                    action: action.clone(),
                    device: None,
                };
                let result = (|| {
                    let services = self
                        .platform_services
                        .clone()
                        .ok_or("Selected input is unavailable")?;
                    let index = self.plugins.find(&plugin).ok_or("Unknown plugin")?;
                    let declaration = self
                        .plugins
                        .action(index, &action)
                        .ok_or("Unknown action")?;
                    if declaration.input
                        != peersus_plugin_api::contributions::ActionInput::SelectedFile
                    {
                        return Err("This action does not accept selected input");
                    }
                    let input = match (path, held) {
                        (Some(path), None) => crate::platform_services::SelectedInput::Path(path),
                        (None, Some((number, name))) => {
                            crate::platform_services::SelectedInput::Held {
                                registry: self.held.clone(),
                                number,
                                name,
                            }
                        }
                        _ => return Err("Choose exactly one file for this action"),
                    };
                    let context = services.select(&plugin, input)?;
                    if let Err(error) = self.invoke(plugin.clone(), &action, None, Some(context)) {
                        services.discard_selection(&plugin, context);
                        return Err(error);
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    tracing::warn!(reason = error, "Selected action refused before admission");
                    self.board.rejected(key);
                    return Err(error);
                }
            }
            Command::CancelAction {
                plugin,
                action,
                peer,
            } => {
                let key = Key {
                    plugin,
                    action,
                    device: peer,
                };
                if let Some(id) = self.in_flight.take_action(&key) {
                    let _ = self
                        .host
                        .submit(peersus_plugin_api::message::Operation::Cancel { id });
                    self.carrying.stop_call(id);
                    self.board.finished(&key, ActionState::Cancelled);
                }
            }
            Command::RestartPlugin { plugin } => {
                // A plugin the application does not know is refused before anything is
                // touched, so asking about one that does not exist can never disturb another.
                self.plugins.find(&plugin).ok_or("Unknown plugin")?;
                self.restart_plugin(&plugin);
            }
            Command::RemovePackage { plugin } => self.remove_package(plugin).await?,
            Command::EnablePlugin { plugin } => self.enable_plugin(plugin)?,
            Command::ApprovePlugin { plugin, program } => self.approve_plugin(plugin, &program)?,
            Command::DisablePlugin { plugin } => self.disable_plugin(plugin).await?,
            Command::StageInstall { source } => self.stage_install(source.as_path())?,
            Command::ConfirmInstall { plugin } => self.confirm_install(plugin).await?,
            Command::CancelInstall { plugin } => self.cancel_install(&plugin)?,
            Command::StoreRefresh {} => self.start_store_refresh()?,
            Command::StoreInstall { plugin } => self.start_store_install(plugin)?,
            Command::SetProfilePicture { picture } => {
                self.network
                    .set_picture(Some(picture))
                    .map_err(|_| "Could not keep the picture")?;
            }
            Command::ClearProfilePicture {} => {
                self.network
                    .set_picture(None)
                    .map_err(|_| "Could not remove the picture")?;
            }
            Command::FetchPicture { digest } => {
                return self
                    .picture_for(&digest)
                    .map(Reply::Picture)
                    .ok_or("No picture is held for that");
            }
            Command::SetAsleep { asleep } => self.network.set_asleep(asleep),
            // A test of the connection to a paired device: it is started and the answer comes later, in the snapshot, so a window never waits on
            // the network here. Only a device that is paired can be tested, and it is asked nothing but whether it answers and knows this one.
            Command::TestLink { peer } => self.network.test_link(&peer)?,
            Command::ClearLinkTest { peer } => self.network.clear_link_test(&peer),
            // Where a paired device can be found, as typed. Judged by the host and not by the window, so a client that sends anything is refused the
            // same way; the answer to "does it work" is a test of the connection, which the person asks for next.
            Command::SetAddress { peer, address } => {
                self.network.set_address(&peer, address.as_deref())?
            }
            Command::SetRelayToken { token } => self.set_relay_token(token.expose()).await?,
            Command::Shutdown {} => unreachable!("shutdown is handled by the owning event loop"),
        }
        Ok(Reply::Applied)
    }

    /// Persist new preferences. Only plugins whose own saved values changed have
    /// their work stopped, so one plugin's setting never interrupts another's.
    async fn apply_settings(
        &mut self,
        settings: peersus_core::Settings,
    ) -> Result<(), &'static str> {
        let before = self.settings.current().plugins.clone();
        self.network
            .update_settings(&mut self.settings, settings)
            .await?;
        let after = &self.settings.current().plugins;
        let changed: Vec<PluginId> = before
            .keys()
            .chain(after.keys())
            .filter(|id| before.get(*id) != after.get(*id))
            .cloned()
            .collect();
        for id in changed {
            // A setting of the plugin's own can only be picked up by a new process.
            self.restart_plugin(&id);
        }
        Ok(())
    }

    /// Use this token for the personal relay and keep it for next time, or forget it when it is empty.
    ///
    /// The order is the point: the transport that uses it is bound first, so a token that cannot be applied is never saved,
    /// and it is saved second, so that what runs is what will be there after a restart. If saving fails the transport goes
    /// back to the token it had, and the person is told, so that nothing is running on a token that is not kept.
    async fn set_relay_token(&mut self, token: &str) -> Result<(), &'static str> {
        let store = self
            .token_store
            .clone()
            .ok_or("This device cannot keep a relay token")?;
        let access = peersus_net::transport::RelayAccess::new(token);
        let previous = self.network.relay_access().cloned();
        self.network
            .set_relay_access(&mut self.settings, access)
            .await?;
        if let Err(refusal) = store.save(token) {
            // Back to what was kept, with the same guarded path. If even that is refused the first error is the one told.
            let _ = self
                .network
                .set_relay_access(&mut self.settings, previous)
                .await;
            return Err(refusal);
        }
        Ok(())
    }

    /// Save what one plugin may do, for one device or, with no device, outright.
    ///
    /// The two are different kinds of permission and each is checked against its own: what is
    /// about a device can be granted only for a device, and what is about no device only
    /// outright. Anything else is refused rather than stored somewhere it would mean nothing,
    /// or worse, would come to mean something later.
    fn set_grants(
        &mut self,
        plugin: PluginId,
        peer: Option<PeerId>,
        grants: Grants,
    ) -> Result<(), &'static str> {
        let index = self.plugins.find(&plugin).ok_or("Unknown plugin")?;
        let spec = self.plugins.get(index).ok_or("Unknown plugin")?;
        // Only what is grantable to this plugin can be granted: its own guards, and the
        // application's guards on what it imports. A name the plugin merely declared for
        // itself is neither, and does not become a permission by being granted.
        let allowed = if peer.is_some() {
            grantable_for_a_device(spec)
        } else {
            grantable_without_a_device(spec)
        };
        if let Some(peer) = &peer {
            self.peer(peer).ok_or("Device is not paired")?;
        }
        if grants
            .iter()
            .any(|permission| !allowed.contains(permission))
        {
            return Err("That permission cannot be granted to this plugin");
        }
        let id = plugin.clone();
        match &peer {
            Some(peer) => self.network.set_grants(peer, plugin, grants),
            None => self.standalone.set(plugin, grants),
        }
        .map_err(|_| "Could not save permissions")?;
        // The decision is saved. Recompute what the application itself may call from every
        // saved decision, so the hub never disagrees with what the user chose.
        self.sync_host_grants();
        self.sync_plugin_grants();
        // A change of permission for a device withdraws what was already started for it. The
        // plugin itself keeps running and keeps serving every other device. A permission about
        // no device has no such work: what it guards is checked at the hub on every call.
        if let Some(peer) = &peer {
            self.withdraw_device_work(Some(&id), peer);
        } else {
            // Saved standalone grants can authorize transient file/HTTP handles.
            // A permission change invalidates those handles before another call.
            self.revoke_platform_calls(&id);
        }
        Ok(())
    }

    /// Make the hub's record of what the application may call match what the user has
    /// granted, recomputed from every device. Idempotent, so it is safe after any change
    /// and again at start-up to restore the picture after a restart.
    fn sync_host_grants(&mut self) {
        let granted = host_grants(&self.plugins, &self.network.peers());
        let offered_host = host_manifest(&self.plugins);
        let platform_timeouts = crate::platform_services::method_timeouts(&offered_host);
        if self
            .host
            .update_host_manifest(offered_host)
            .and_then(|()| self.host.set_host_method_timeouts(platform_timeouts))
            .and_then(|()| self.hub.set_permissions(HOST, granted))
            .is_err()
        {
            self.error("Permissions could not be applied");
        }
    }

    /// Make the hub's record of what each plugin may present match what the user has
    /// granted, recomputed from every device and from what was granted outright. The hub
    /// records it whether or not the plugin's process is running at this moment, so a
    /// withdrawal made while a crashed plugin waits for its restart is as binding as any
    /// other. The only way it can fail is a permission the hub finds malformed, which is never
    /// something to ignore.
    fn sync_plugin_grants(&mut self) {
        let peers = self.network.peers();
        let failed = self
            .plugins
            .iter()
            .map(|spec| {
                self.hub.set_permissions(
                    spec.id.as_str(),
                    plugin_grants(spec, &peers, self.standalone.all()),
                )
            })
            .any(|result| result.is_err());
        if failed {
            self.error("Permissions could not be applied");
        }
    }

    async fn set_plugin_setting(
        &mut self,
        plugin: PluginId,
        peer: Option<PeerId>,
        setting: &str,
        value: Value,
    ) -> Result<(), &'static str> {
        let index = self.plugins.find(&plugin).ok_or("Unknown plugin")?;
        let declared = self
            .plugins
            .get(index)
            .ok_or("Unknown plugin")?
            .manifest
            .contributions
            .settings
            .clone();
        let scope = declared
            .iter()
            .find(|s| s.id == setting)
            .ok_or("Unknown setting")?
            .scope;
        match (scope, peer) {
            (Scope::Device, Some(peer)) => {
                self.peer(&peer).ok_or("Device is not paired")?;
                let id = plugin.clone();
                self.network
                    .set_plugin_value(&peer, plugin, &declared, setting, value)
                    .map_err(|_| "Invalid value for this setting")?;
                // A device-level value may change what the plugin does for that device, so
                // that device's running work is withdrawn. The plugin is left running.
                self.withdraw_device_work(Some(&id), &peer);
            }
            (Scope::Global, None) => {
                let mut next = self.settings.current().clone();
                next.plugins
                    .entry(plugin)
                    .or_default()
                    .set(&declared, Scope::Global, setting, value)
                    .map_err(|_| "Invalid value for this setting")?;
                self.apply_settings(next).await?;
            }
            (Scope::Device, None) => return Err("Choose a device for this setting"),
            (Scope::Global, Some(_)) => return Err("This setting is not per device"),
        }
        Ok(())
    }

    /// Run an action a plugin declared, on behalf of the user.
    ///
    /// Two independent layers decide. Here, the *device* layer: the user must have granted
    /// this plugin that capability for the chosen device, and the device must be online.
    /// Then the hub, the *plugin* layer: the plugin must list the application as a
    /// consumer of that export and the capability must be granted to the application.
    /// The device is data inside the call, never the identity it is made under.
    ///
    /// The reply acknowledges that the call was admitted. The plugin's answer arrives
    /// later and, if it is an error, is published as a fixed-text message.
    fn invoke(
        &mut self,
        plugin: PluginId,
        action: &str,
        peer: Option<PeerId>,
        context: Option<u64>,
    ) -> Result<(), &'static str> {
        let index = self.plugins.find(&plugin).ok_or("Unknown plugin")?;
        let action = self
            .plugins
            .action(index, action)
            .ok_or("Unknown action")?
            .clone();
        if (action.input == peersus_plugin_api::contributions::ActionInput::SelectedFile)
            != context.is_some()
        {
            return Err("This action requires its declared selected input");
        }
        let device = match (action.needs_device, peer) {
            (true, Some(peer)) => {
                let peer = self.peer(&peer).ok_or("Device is not paired")?;
                if !permitted(&peer, &plugin, action.capability.as_deref()) {
                    return Err("Action is not permitted for this device");
                }
                if self.network.connection(&peer.id).is_none() {
                    return Err("Device is offline");
                }
                Some(peer.id)
            }
            (true, None) => return Err("Choose a device for this action"),
            (false, None) => None,
            (false, Some(_)) => return Err("This action does not use a device"),
        };
        let version = self
            .plugins
            .get(index)
            .and_then(|spec| {
                spec.manifest
                    .exports
                    .iter()
                    .find(|e| e.service == action.target.service)
            })
            .map(|export| export.version)
            .ok_or("Unknown action")?;
        self.next_call = self.next_call.checked_add(1).ok_or("Too many calls")?;
        let id = self.next_call;
        let mut args = match &device {
            Some(peer) => peer_argument(&self.aliases, &plugin, peer),
            None => serde_json::Value::Null,
        };
        if let Some(context) = context {
            if args.is_null() {
                args = serde_json::json!({});
            }
            args.as_object_mut()
                .ok_or("Action arguments are invalid")?
                .insert("context".into(), serde_json::json!(context));
        }
        // Registered before it is sent, so a second press made while this one is still
        // running is already seen as a repeat. The key is the action the user pressed, not
        // the method it happens to call: two actions may share a method.
        let key = Key {
            plugin: plugin.clone(),
            action: action.id.clone(),
            device: device.clone(),
        };
        self.in_flight
            .start(key.clone(), id)
            .map_err(|refused| match refused {
                Refused::Pending => "That action is already running",
                Refused::Full => "Too many operations are pending",
            })?;
        self.board.running(key);
        let sent = self
            .host
            .submit(peersus_plugin_api::message::Operation::Call {
                id,
                provider: plugin.as_str().to_owned(),
                service: action.target.service,
                version,
                method: action.target.method,
                args,
                timeout_ms: action.timeout_ms,
            });
        if let Err(error) = sent {
            // The hub never recorded this call, so no result will ever come to close it.
            // Left registered, the action could never be started again.
            // The board must not be left saying "running" for a call that never started.
            if let Some(key) = self.in_flight.finish_key(id) {
                self.board.finished(
                    &key,
                    ActionState::Failed {
                        reason: ActionFailure::Unavailable,
                    },
                );
            }
            return Err(refusal_text(&error));
        }
        Ok(())
    }

    /// Deal with one thing the hub delivered to the application.
    ///
    /// Every call made *to* the application is answered, even though it offers nothing
    /// yet. A call left unanswered keeps a reserved slot of the caller's queue until its
    /// deadline, so ignoring one would let a plugin stall itself, and a flood of them
    /// could stall the application's own queue.
    fn handle_host_delivery(&mut self, delivery: peersus_plugin_api::message::Delivery) {
        use peersus_plugin_api::message::{Delivery, Operation};
        match delivery {
            Delivery::Result { id, result } => {
                // However it ended, the call is over, so the same action may run again. The
                // hub gives the caller a result for every outcome, including a timeout and a
                // provider that went away, so this is the one place a call is closed.
                // The board is told how it ended in the application's own words: only the
                // code is read, never anything the plugin wrote.
                // However the call ended, a transfer it announced is over with it. A peer that
                // keeps sending a little at a time would otherwise keep the plugin inside this
                // one call for as long as it liked. Only the inbound registry is told: it is
                // keyed by call numbers, which is what this is. What a plugin sends out is
                // keyed by claims, a different kind of number, and is never touched by this.
                self.carrying.stop_call(id);
                let finished = self.in_flight.finish_key(id);
                if let Some(key) = &finished {
                    self.board.finished(key, action_state(&result));
                }
                if let Some(sentence) = action_failure_sentence(&result) {
                    if let Some(key) = &finished {
                        // Opt-in context is manifest identity, not the plugin's error text.
                        tracing::debug!(call = id, plugin = ?key.plugin, action = %key.action, reason = sentence, "Plugin action failed");
                    }
                    self.error(sentence);
                }
            }
            // Only ever delivered to the provider of a call, never to its caller.
            Delivery::Cancelled { ticket } => self.platform_calls.cancel(ticket),
            Delivery::Event { .. } => {}
            Delivery::Call {
                ticket,
                caller,
                service,
                method,
                args,
                ..
            } => {
                if let Some((_, _, capability, _)) = crate::platform_services::METHODS
                    .iter()
                    .find(|(offered, name, _, _)| *offered == service && *name == method)
                {
                    let result = match (PluginId::new(&caller), self.platform_services.clone()) {
                        (Ok(caller), Some(platform))
                            if self.standalone.all().get(&caller).is_some_and(|grants| {
                                Permission::new(HOST, *capability)
                                    .is_some_and(|permission| grants.allows(&permission))
                            }) =>
                        {
                            self.platform_calls.start(ticket, caller.clone(), move || {
                                platform.dispatch(crate::platform_services::Call {
                                    caller,
                                    service,
                                    method,
                                    args,
                                })
                            })
                        }
                        _ => Err("This platform does not offer the requested facility"),
                    };
                    if let Err(reason) = result {
                        let _ = self.host.submit(Operation::Reply {
                            ticket,
                            result: peersus_plugin_api::message::Outcome::error(
                                "unavailable",
                                reason,
                            ),
                        });
                    }
                    return;
                }
                let (result, restart) = self.answer_plugin_call(&caller, &service, &method, &args);
                let _ = self.host.submit(Operation::Reply { ticket, result });
                // After the answer, never before: see `answer_plugin_call`.
                if let Some(target) = restart {
                    self.restart_plugin(&target);
                }
            }
        }
    }

    /// What the application answers a plugin that called it, and the plugin to restart once
    /// that answer is on its way, if the call asked for one. `caller` is stamped by the hub
    /// from the channel, never taken from the message. `peersus.peers` offers `list` and
    /// `send`, `peersus.plugins` offers `list` and `restart`; every other call is answered as
    /// unknown rather than as a success it did not perform.
    ///
    /// A restart is returned and not performed here, because the plugin asked to be restarted
    /// may be the caller itself. The answer is submitted first and the supervisor then gives
    /// the process a short, bounded window before it ends it, so a plugin that restarts itself
    /// usually sees its request succeed. That is best effort and NOT a guarantee: nothing
    /// acknowledges that the plugin read the answer, and a process still there when the window
    /// closes is stopped all the same. What is promised is the restart; a plugin that must
    /// know it happened asks `list` from its next process.
    fn answer_plugin_call(
        &mut self,
        caller: &str,
        service: &str,
        method: &str,
        args: &serde_json::Value,
    ) -> (peersus_plugin_api::message::Outcome, Option<PluginId>) {
        use peersus_plugin_api::message::Outcome;
        let unknown = || Outcome::error("unknown-method", "The application does not offer that");
        let Ok(plugin) = PluginId::new(caller) else {
            return (unknown(), None);
        };
        let answer = |outcome| (outcome, None);
        match (service, method) {
            (PLATFORM_SERVICE, "inspect")
                if args.as_object().is_some_and(|args| args.is_empty()) =>
            {
                answer(Outcome::ok(
                    serde_json::to_value(peersus_plugin_api::platform::HostPlatform::current())
                        .unwrap_or(serde_json::Value::Null),
                ))
            }
            (CONFIG_SERVICE, "get") if args.as_object().is_some_and(|args| args.is_empty()) => {
                let Some(spec) = self
                    .plugins
                    .find(&plugin)
                    .and_then(|index| self.plugins.get(index))
                else {
                    return answer(unknown());
                };
                answer(Outcome::ok(configuration(
                    spec,
                    self.settings.current().plugins.get(&plugin),
                    &self.aliases,
                    &self.network.peers(),
                )))
            }
            (PEERS_SERVICE, "list") => answer(Outcome::ok(peers_listing(
                &self.aliases,
                &plugin,
                &self.network.peers(),
            ))),
            (PEERS_SERVICE, "send") => answer(match self.begin_send(&plugin, args) {
                Ok(claim) => Outcome::ok(serde_json::json!({ "claim": claim })),
                Err(reason) => Outcome::error("refused", reason),
            }),
            (PLUGINS_SERVICE, "list") => answer(Outcome::ok(plugins_listing(&self.plugin_infos()))),
            (PLUGINS_SERVICE, "restart") => match parse_restart(args) {
                Err(reason) => answer(Outcome::error("refused", reason)),
                Ok(target) if self.plugins.find(&target).is_none() => {
                    answer(Outcome::error("refused", "There is no such plugin"))
                }
                Ok(target) => (Outcome::ok(serde_json::json!({})), Some(target)),
            },
            // What is going on now, for a plugin that has only just begun to listen or that was restarted: the transfers still under way, said exactly
            // as the events say them (see `transfer_events`), so the answer to `active` can never carry a name and the answer to `active-named` can
            // never carry less than `changed-named` does. The hub has already refused a caller that was not granted the permission that guards each.
            (
                crate::transfer_events::TRANSFERS_SERVICE,
                crate::transfer_events::TRANSFERS_ACTIVE,
            ) if args.as_object().is_some_and(|args| args.is_empty()) => answer(Outcome::ok(
                crate::transfer_events::active(&self.network.transfers(), false),
            )),
            (
                crate::transfer_events::TRANSFERS_SERVICE,
                crate::transfer_events::TRANSFERS_ACTIVE_NAMED,
            ) if args.as_object().is_some_and(|args| args.is_empty()) => answer(Outcome::ok(
                crate::transfer_events::active(&self.network.transfers(), true),
            )),
            // What plugins make of this device's statuses, and what they read of the others'. The caller is the plugin the HOST knows it is,
            // so the owner of a status is never a thing a plugin says; the hub has already refused one that was not granted the permission
            // that guards each method. A refusal says one sentence and changes nothing.
            (
                crate::status_service::STATUS_SERVICE,
                method @ (crate::status_service::STATUS_DEFINE
                | crate::status_service::STATUS_SET
                | crate::status_service::STATUS_CLEAR),
            ) => {
                let applied =
                    crate::status_service::apply(&mut self.statuses, &plugin, method, args);
                if applied.changed {
                    self.say_statuses();
                }
                answer(match applied.answer {
                    Ok(value) => Outcome::ok(value),
                    Err(reason) => Outcome::error("refused", reason),
                })
            }
            (crate::status_service::STATUS_SERVICE, crate::status_service::STATUS_LIST)
                if args.as_object().is_some_and(|args| args.is_empty()) =>
            {
                answer(Outcome::ok(crate::status_service::list(
                    &self.aliases,
                    &plugin,
                    &self.network.peers(),
                )))
            }
            (crate::status_service::STATUS_SERVICE, crate::status_service::STATUS_OWN)
                if args.as_object().is_some_and(|args| args.is_empty()) =>
            {
                answer(Outcome::ok(crate::status_service::own(
                    &self.statuses.defined_by(&plugin),
                    &self.statuses.active(),
                )))
            }
            _ => answer(unknown()),
        }
    }

    fn revoke_platform_calls(&mut self, plugin: &PluginId) {
        self.platform_calls.revoke(plugin);
        if let Some(platform) = &self.platform_services {
            platform.revoke(plugin);
        }
    }

    /// Tell every paired device what this one now says of itself besides being awake: the statuses that are on. Called after anything that
    /// can change them, and only then. A refusal changes nothing, so it is never told.
    fn say_statuses(&mut self) {
        // The set is bounded by `Statuses` and cannot hold one twice, so the network never refuses it; if it ever did, what the device says
        // stays as it was, which is the honest thing to say.
        let _ = self.network.set_statuses(self.statuses.active());
    }

    /// A plugin's statuses go, defined and on. The paired devices are told only if one of them was on: nothing else the device says
    /// changed, and a device must not be written to for nothing.
    fn forget_statuses(&mut self, plugin: &PluginId) {
        if self.statuses.forget_plugin(plugin) {
            self.say_statuses();
        }
    }

    /// Open a stream to a device for a plugin that asked to send, and set aside what the
    /// plugin needs to start writing into it. Returns the number it must present.
    ///
    /// Everything that can refuse comes first and reserves nothing: the arguments, then the
    /// device layer (`authorize_send`: this device, online, granted this plugin), then a live
    /// connection to it. Only then is a number issued and one task started. That task opens
    /// the device's stream, waits for the plugin's data connection, and carries the bytes;
    /// it is recorded under the device and the plugin from the start, so withdrawing a
    /// permission or forgetting the device stops it whether its stream is open yet or not.
    #[cfg(unix)]
    fn begin_send(
        &mut self,
        plugin: &PluginId,
        args: &serde_json::Value,
    ) -> Result<u64, &'static str> {
        use rand::Rng;
        let request = parse_send(args)?;
        let peers = self.network.peers();
        let peer = authorize_send(&self.aliases, plugin, &request.device, &peers)?;
        let spec = self
            .plugins
            .find(plugin)
            .and_then(|index| self.plugins.get(index))
            .ok_or(SEND_REFUSED)?;
        authorize_service_send(spec, peer, &request.service, request.version)?;
        let device = peer.id.clone();
        let connection = self.network.connection(&device).ok_or(SEND_REFUSED)?;
        let (tx, rx) = oneshot::channel();
        let owner = crate::parked::Owner {
            peer: device,
            plugin: plugin.clone(),
        };
        let claim = self
            .outbound
            .park_fresh(
                owner.clone(),
                tx,
                std::time::Instant::now(),
                crate::parked::MAX_WAIT,
                || rand::rng().next_u64(),
            )
            .map_err(|_| SEND_REFUSED)?;
        let handle = self.transfers.spawn(async move {
            let outcome = carry_out(connection, request, rx, OPEN_LIMIT).await;
            (Direction::Outbound, claim, outcome)
        });
        self.carrying_out.started(claim, owner, handle);
        Ok(claim)
    }

    /// There are no plugin processes on this platform, so there is nobody to send for.
    #[cfg(not(unix))]
    fn begin_send(
        &mut self,
        _plugin: &PluginId,
        _args: &serde_json::Value,
    ) -> Result<u64, &'static str> {
        Err(SEND_REFUSED)
    }

    fn error(&mut self, reason: &'static str) {
        // Only fixed application-owned sentences enter diagnostics, never plugin text.
        tracing::warn!(reason, "Owner operation failed");
        if self.errors.last().is_some_and(|last| last == reason) {
            return;
        }
        if self.errors.len() == 16 {
            self.errors.remove(0);
        }
        self.errors.push(reason.into());
    }

    /// Every plugin with what state it is in right now. The one place the registry's
    /// declarations and the supervisors' reports are put together, so that what a frontend is
    /// shown and what `peersus.plugins` tells a plugin can never disagree.
    fn plugin_infos(&self) -> Vec<peersus_core::model::PluginInfo> {
        let mut plugins = self.plugins.info();
        for info in &mut plugins {
            // Read the supervisor's last report fresh each time: a status taken once at
            // start would go stale the moment a plugin fails or is restarted.
            let event = self
                .running
                .get(&info.id)
                .map(|r| r.health.borrow().clone());
            info.status = status_of(event.as_ref());
            // What the folder allows is a different thing from what the process is doing, and is not overwritten by it:
            // the registry says every plugin in it is ready, and the person's switch is what makes one of them disabled.
            if self.disabled.contains(&info.id) {
                info.standing = peersus_core::model::Standing::Disabled;
            }
        }
        plugins
    }

    /// What the WINDOW is shown: every plugin in the registry, and after them every plugin that was found in the store and is
    /// not run, each with what its folder allows. A plugin that is not approved, changed or broken is therefore never
    /// missing from the window, and can be approved or removed from it.
    ///
    /// This is a separate list from [`Runtime::plugin_infos`], on purpose: that one is also what `peersus.plugins` tells a
    /// plugin about the others, and a plugin is not to learn which of the person's other plugins are blocked or unapproved.
    fn window_plugin_infos(&self) -> Vec<peersus_core::model::PluginInfo> {
        let mut plugins = self.plugin_infos();
        plugins.extend(
            self.held_back
                .iter()
                .map(|held| peersus_core::model::PluginInfo {
                    id: held.id.clone(),
                    manifest: held.manifest.clone(),
                    standing: held.standing,
                    // Nothing of it runs, so there is no process to speak of.
                    status: peersus_core::model::PluginStatus::Stopped,
                    // Not run, so nothing can be granted to it, and the window does not offer.
                    grantable: peersus_core::model::Grantable::default(),
                    // Given back by the window with `ApprovePlugin`, only for what can be approved: what is broken has nothing to
                    // approve, so no digest is offered for it.
                    program: match held.standing {
                        peersus_core::model::Standing::NeedsApproval
                        | peersus_core::model::Standing::Changed => held.program.clone(),
                        _ => None,
                    },
                    package: self.installed_packages.get(&held.id).cloned(),
                }),
        );
        // Author and link are for the PERSON. `plugin_infos` is also what a plugin is told about the others,
        // so the block is added here, to the window's list only.
        for info in &mut plugins {
            if info.package.is_none() {
                info.package = self.installed_packages.get(&info.id).cloned();
            }
        }
        plugins
    }

    /// The packages waiting to be confirmed, as the same shape as an installed plugin, in the order
    /// of their names so that the window does not reshuffle them between snapshots.
    fn staged_infos(&self) -> Vec<peersus_core::model::PluginInfo> {
        let mut infos: Vec<_> = self
            .staged
            .iter()
            .map(|(id, staged)| peersus_core::model::PluginInfo {
                id: id.clone(),
                manifest: staged.manifest.clone(),
                // Shown before it is installed, so it is not approved: the person is being asked, and that is what this is.
                standing: peersus_core::model::Standing::NeedsApproval,
                status: peersus_core::model::PluginStatus::Stopped,
                // Not installed yet, so nothing can be granted to it, and the window does not offer.
                grantable: peersus_core::model::Grantable::default(),
                program: None,
                package: staged.package.clone(),
            })
            .collect();
        infos.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        infos
    }

    /// The picture a window may be given for `digest`: this device's own, or the one a paired device
    /// shows now, and nothing else. What is kept in the cache but is nobody's picture any more is not
    /// produced, so a window cannot read through the folder for pictures it was never meant to see.
    fn picture_for(
        &self,
        digest: &peersus_core::profile::Digest,
    ) -> Option<peersus_core::profile::Picture> {
        if let Some(own) = self
            .network
            .own_picture()
            .filter(|own| own.digest() == *digest)
        {
            return Some(own.clone());
        }
        let peer = self
            .network
            .peer_digests()
            .into_iter()
            .find(|(_, held)| held == digest)?
            .0;
        self.network.peer_picture(&peer)
    }

    fn publish(&self) {
        let plugins = self.window_plugin_infos();
        let transfers = self.network.transfers();
        self.tell_plugins_of_transfers(&transfers);
        self.tell_plugins_of_statuses(&self.network.peers());
        self.snapshots.send_replace(Arc::new(Snapshot {
            settings: self.settings.current().clone(),
            peers: self.network.peers(),
            grants: self.standalone.all().clone(),
            plugins,
            staged: self.staged_infos(),
            store: self.store_state(),
            actions: self.board.statuses(),
            transfers,
            pairing: self.network.pairing(),
            errors: self.errors.clone(),
            profile_picture: self
                .network
                .own_picture()
                .map(peersus_core::profile::Picture::digest),
            peer_pictures: self.network.peer_digests(),
            files_code: self.network.files_code(),
            link_tests: self.network.link_tests(),
            typed_addresses: self.network.typed_addresses(),
            listening_port: self.network.listening_port(),
            relay: Some(self.network.relay_diagnostics()),
        }));
    }

    /// Say to the plugins that listen what changed in the transfers: what [`crate::transfer_events::to_tell`] decides, and nothing of its own. The
    /// record is the one the windows are shown (read once, in [`Self::publish`]), so a plugin and a window never see two versions of one change.
    fn tell_plugins_of_transfers(&self, transfers: &[peersus_core::model::Transfer]) {
        use crate::transfer_events as events;
        let anyone = !consumers_of(&self.plugins, events::TRANSFERS_SERVICE).is_empty();
        let mut told = self.told_transfers.borrow_mut();
        let (to_tell, keep) = events::to_tell(told.as_deref(), transfers, anyone);
        for transfer in to_tell {
            for (event, data) in [
                (events::TRANSFERS_CHANGED, events::changed(transfer)),
                (
                    events::TRANSFERS_CHANGED_NAMED,
                    events::changed_named(transfer),
                ),
            ] {
                // A subscriber that cannot keep up misses this one rather than stalling the application: the hub refuses the whole delivery
                // then, and `active` is always there to ask what is true now. The hub tells each event only to those granted its capability.
                let _ = self
                    .host
                    .submit(peersus_plugin_api::message::Operation::Publish {
                        service: events::TRANSFERS_SERVICE.to_owned(),
                        version: 1,
                        event: event.to_owned(),
                        data,
                    });
            }
        }
        *told = keep;
    }

    /// Say to the plugins that listen what a device now says of itself, once for each that changed: what [`crate::status_service::to_tell`]
    /// decides, and nothing of its own. One payload goes to everyone who listens, and the hub tells it only to those granted `status.watch`.
    fn tell_plugins_of_statuses(&self, peers: &[peersus_core::model::Peer]) {
        use crate::status_service as status;
        let anyone = !consumers_of(&self.plugins, status::STATUS_SERVICE).is_empty();
        let mut told = self.told_statuses.borrow_mut();
        let (to_tell, keep) = status::to_tell(told.as_ref(), peers, anyone);
        for data in to_tell {
            // A subscriber that cannot keep up misses this one rather than stalling the application: `list` always says what is true now.
            let _ = self
                .host
                .submit(peersus_plugin_api::message::Operation::Publish {
                    service: status::STATUS_SERVICE.to_owned(),
                    version: 1,
                    event: status::STATUS_CHANGED.to_owned(),
                    data,
                });
        }
        *told = keep;
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_plugin_api::{
        contributions::{Action, Source},
        manifest::{Export, Manifest, Member},
        message::{Delivery, Operation},
    };

    /// A member that needs `capability` (or nothing) and says what it does.
    fn member(capability: Option<&str>) -> Member {
        Member {
            capability: capability.map(str::to_owned),
            description: "Does what its name says.".to_owned(),
        }
    }

    /// A plugin offering one action that needs capability `cap`, open to the application.
    fn plugin_with_guarded_action() -> PluginSpec {
        let mut manifest = Manifest {
            capabilities: [("cap".to_owned(), "Lets the plugin go.".to_owned())].into(),
            exports: vec![Export {
                service: "svc".into(),
                version: 1,
                description: "A test service.".into(),
                consumers: [HOST.to_owned()].into(),
                methods: [("go".to_owned(), member(Some("cap")))].into(),
                events: Default::default(),
            }],
            ..Manifest::default()
        };
        manifest.contributions.actions.push(Action {
            id: "go".into(),
            label: "Go".into(),
            short_label: None,
            input: peersus_plugin_api::contributions::ActionInput::None,
            target: Source {
                service: "svc".into(),
                method: "go".into(),
            },
            capability: Some("cap".into()),
            needs_device: false,
            destructive: false,
            timeout_ms: peersus_plugin_api::contributions::default_action_timeout(),
        });
        PluginSpec {
            id: PluginId::new("alpha").unwrap(),
            manifest,
            program: Some(PathBuf::from("/nonexistent")),
            arguments: Vec::new(),
            restart_policy: None,
        }
    }

    /// A paired device and what it was granted: for each plugin, the permissions given as
    /// `(owner, name)` pairs.
    fn peer_granted(id: &str, grants: &[(&str, &[(&str, &str)])]) -> Peer {
        Peer {
            id: PeerId(id.into()),
            name: id.into(),
            paired_at: 0,
            last_seen: None,
            online: true,
            sleeping: false,
            statuses: Vec::new(),
            grants: grants
                .iter()
                .map(|(plugin, given)| {
                    (
                        PluginId::new(*plugin).unwrap(),
                        Grants::new(given.iter().map(|(o, n)| permission(o, n))).unwrap(),
                    )
                })
                .collect(),
            plugin_values: Default::default(),
        }
    }

    fn registry_with_alpha() -> Registry {
        let mut registry = Registry::default();
        registry.register(plugin_with_guarded_action()).unwrap();
        registry
    }

    fn permission(owner: &str, name: &str) -> peersus_plugin_api::permission::Permission {
        peersus_plugin_api::permission::Permission::new(owner, name).unwrap()
    }

    /// A set of permissions, each given as `(owner, name)`.
    fn set(values: &[(&str, &str)]) -> BTreeSet<peersus_plugin_api::permission::Permission> {
        values.iter().map(|(o, n)| permission(o, n)).collect()
    }

    #[test]
    fn the_application_holds_nothing_until_the_user_has_granted_something() {
        let registry = registry_with_alpha();
        assert_eq!(host_grants(&registry, &[]), set(&[]));
        // Paired devices with no grants at all change nothing: default-deny.
        let peers = [
            peer_granted("p1", &[]),
            peer_granted("p2", &[("alpha", &[])]),
        ];
        assert_eq!(host_grants(&registry, &peers), set(&[]));
    }

    #[test]
    fn one_devices_grant_lets_the_application_run_that_plugins_action() {
        let registry = registry_with_alpha();
        let peers = [peer_granted("p1", &[("alpha", &[("alpha", "cap")])])];
        assert_eq!(host_grants(&registry, &peers), set(&[("alpha", "cap")]));
    }

    #[test]
    fn the_result_is_the_union_over_every_device_not_the_last_one_changed() {
        let mut registry = Registry::default();
        let mut alpha = plugin_with_guarded_action();
        alpha.manifest.capabilities.insert(
            "other".into(),
            "Lets the plugin do the second thing.".into(),
        );
        alpha.manifest.exports[0]
            .methods
            .insert("two".into(), member(Some("other")));
        alpha.manifest.contributions.actions.push(Action {
            id: "two".into(),
            label: "Two".into(),
            short_label: None,
            input: peersus_plugin_api::contributions::ActionInput::None,
            target: Source {
                service: "svc".into(),
                method: "two".into(),
            },
            capability: Some("other".into()),
            needs_device: false,
            destructive: false,
            timeout_ms: peersus_plugin_api::contributions::default_action_timeout(),
        });
        registry.register(alpha).unwrap();
        // Each device grants a different capability. Revoking one device's grant must not
        // be able to wipe out what another device was given, so the whole picture is
        // recomputed from every device instead of patched with the latest change.
        let peers = [
            peer_granted("p1", &[("alpha", &[("alpha", "cap")])]),
            peer_granted("p2", &[("alpha", &[("alpha", "other")])]),
        ];
        assert_eq!(
            host_grants(&registry, &peers),
            set(&[("alpha", "cap"), ("alpha", "other")])
        );
        assert_eq!(
            host_grants(&registry, &peers[1..]),
            set(&[("alpha", "other")])
        );
    }

    #[test]
    fn a_grant_for_one_plugin_never_opens_another_plugins_guard_of_the_same_name() {
        // Two plugins each declare their OWN `cap`, with whatever meaning each chose. The user
        // grants `alpha`'s. That says nothing about `beta`'s, so what the application holds is
        // a permission OF alpha and not a bare name that beta's guard would also match.
        let mut registry = Registry::default();
        registry.register(plugin_with_guarded_action()).unwrap();
        let mut beta = plugin_with_guarded_action();
        beta.id = PluginId::new("beta").unwrap();
        beta.manifest.exports[0].service = "svc2".into();
        beta.manifest.contributions.actions[0].target.service = "svc2".into();
        registry.register(beta).unwrap();
        let peers = [peer_granted("p1", &[("alpha", &[("alpha", "cap")])])];
        let held = host_grants(&registry, &peers);
        assert!(
            held.contains(&permission("alpha", "cap")),
            "alpha's own permission was not carried: {held:?}"
        );
        assert!(
            !held.contains(&permission("beta", "cap")),
            "a grant for alpha opened beta's guard: {held:?}"
        );
        assert_eq!(held.len(), 1, "{held:?}");
    }

    #[test]
    fn every_refusal_the_hub_can_give_has_its_own_fixed_wording() {
        use peersus_plugin_api::Error;
        let words = [
            (Error::Denied, "The plugin does not allow this action"),
            (Error::QueueFull, "Too many operations are pending"),
            (Error::UnknownProvider, "The plugin is not running"),
            (Error::Unavailable, "The plugin is not running"),
            (
                Error::UnknownMethod,
                "The plugin does not offer this action",
            ),
            (Error::Version, "The plugin does not offer this action"),
            (Error::Timeout, "The request could not be sent"),
            (Error::Oversize, "The request could not be sent"),
        ];
        for (error, expected) in words {
            assert_eq!(refusal_text(&error), expected, "{error:?}");
        }
    }

    #[test]
    fn a_denial_is_never_described_as_the_plugin_not_running() {
        use peersus_plugin_api::Error;
        // Telling a user that a plugin is not running when it actually refused them sends
        // them looking for a crash that did not happen.
        assert_ne!(
            refusal_text(&Error::Denied),
            refusal_text(&Error::Unavailable)
        );
        assert_ne!(
            refusal_text(&Error::UnknownMethod),
            refusal_text(&Error::Unavailable)
        );
    }

    #[test]
    fn only_the_application_may_collect_what_the_application_parked() {
        assert!(may_collect(HOST));
        // Not another plugin, not something that merely looks like the host.
        for caller in ["me", "sink", "host2", "Host", "", " host"] {
            assert!(!may_collect(caller), "{caller:?} must not collect");
        }
    }

    #[test]
    fn a_plugin_using_the_applications_call_number_cannot_collect_its_stream() {
        use crate::parked::{Owner, Parked};
        let now = std::time::Instant::now();
        let mut parked: Parked<&str> = Parked::default();
        let owner = Owner {
            peer: PeerId("p".into()),
            plugin: PluginId::new("sink").unwrap(),
        };
        // The application parks a peer's stream under its call number 77.
        parked
            .park(77, owner, "the peer's stream", now, Duration::from_secs(10))
            .unwrap();
        // A plugin has made a call to itself and numbered it 77 too, so it holds a genuine
        // right whose call number matches. Collecting is decided on WHO made the call
        // first, so the stream is never even looked up for it.
        let impostor = ("me", 77u64);
        let taken = if may_collect(impostor.0) {
            parked.claim(impostor.1, now)
        } else {
            None
        };
        assert_eq!(
            taken, None,
            "a plugin collected a stream that was not its own"
        );
        // And the stream is still there for the call it really belongs to.
        assert!(may_collect(HOST));
        assert_eq!(parked.claim(77, now), Some("the peer's stream"));
    }

    fn receiving_registry() -> Registry {
        let mut registry = Registry::default();
        registry
            .register(plugin_that_receives(Some("sink.write")))
            .unwrap();
        registry
    }

    #[test]
    fn a_device_the_user_granted_is_admitted_to_the_plugin_that_exports_the_service() {
        let registry = receiving_registry();
        let peers = [peer_granted("p", &[("sink", &[("sink", "sink.write")])])];
        let admitted = admit(&registry, &peers, &PeerId("p".into()), "sink.service", 1)
            .expect("a granted device must be admitted");
        assert_eq!(admitted.plugin.as_str(), "sink");
        assert_eq!(admitted.service, "sink.service");
        assert_eq!(admitted.version, 1);
    }

    #[test]
    fn a_device_without_the_grant_is_refused_before_anything_is_set_aside() {
        let registry = receiving_registry();
        // Paired, but nothing granted: default-deny.
        let none = [peer_granted("p", &[])];
        assert_eq!(
            admit(&registry, &none, &PeerId("p".into()), "sink.service", 1).err(),
            Some("Plugin is not permitted for this device")
        );
        // Granted a different capability of the same plugin: still not this one.
        let wrong = [peer_granted("p", &[("sink", &[("sink", "sink.other")])])];
        assert_eq!(
            admit(&registry, &wrong, &PeerId("p".into()), "sink.service", 1).err(),
            Some("Plugin is not permitted for this device")
        );
        // Granted the right capability of ANOTHER plugin: says nothing about this one.
        let other = [peer_granted(
            "elsewhere",
            &[("elsewhere", &[("sink", "sink.write")])],
        )];
        assert_eq!(
            admit(
                &registry,
                &other,
                &PeerId("elsewhere".into()),
                "sink.service",
                1
            )
            .err(),
            Some("Plugin is not permitted for this device")
        );
    }

    #[test]
    fn an_unknown_device_service_or_version_is_refused_with_its_own_words() {
        let registry = receiving_registry();
        let peers = [peer_granted("p", &[("sink", &[("sink", "sink.write")])])];
        assert_eq!(
            admit(
                &registry,
                &peers,
                &PeerId("stranger".into()),
                "sink.service",
                1
            )
            .err(),
            Some("Device is not paired")
        );
        assert_eq!(
            admit(
                &registry,
                &peers,
                &PeerId("p".into()),
                "nobody.handles.this",
                1
            )
            .err(),
            Some("No plugin handles this request")
        );
        // The same service at another version is not a best-effort match.
        assert_eq!(
            admit(&registry, &peers, &PeerId("p".into()), "sink.service", 2).err(),
            Some("No plugin handles this request")
        );
    }

    #[test]
    fn what_the_supervisor_reports_maps_to_the_status_a_user_sees() {
        use peersus_core::model::PluginStatus::{Failed, Restarting, Running, Starting, Stopped};
        let pause = Duration::from_millis(250);
        assert_eq!(status_of(Some(&PluginEvent::Started)), Starting);
        assert_eq!(status_of(Some(&PluginEvent::Connected)), Running);
        assert_eq!(
            status_of(Some(&PluginEvent::Failed { restart_in: pause })),
            Restarting
        );
        assert_eq!(status_of(Some(&PluginEvent::GaveUp)), Failed);
        assert_eq!(status_of(Some(&PluginEvent::Stopped)), Stopped);
    }

    #[test]
    fn a_launched_process_is_not_reported_as_running_until_it_has_connected() {
        use peersus_core::model::PluginStatus::Running;
        // A screen that calls a launched-but-silent plugin "running" shows a button whose
        // every press fails, so only a completed handshake may earn that word.
        assert_ne!(status_of(Some(&PluginEvent::Started)), Running);
        assert_eq!(status_of(Some(&PluginEvent::Connected)), Running);
    }

    #[test]
    fn the_last_report_of_a_supervisor_that_has_ended_is_still_seen() {
        // A supervisor that gives up sends `GaveUp` and finishes in the same moment, which
        // drops its end of the channel. That last report is the one a user most needs, and
        // asking the channel whether anything changed fails once it is closed, so a closed
        // channel must not hide it.
        let (sender, mut receiver) = tokio::sync::watch::channel(PluginEvent::Stopped);
        sender.send_replace(PluginEvent::GaveUp);
        drop(sender);
        assert_eq!(observe(&mut receiver), Some(PluginEvent::GaveUp));
    }

    #[test]
    fn a_report_is_seen_once_and_a_finished_supervisor_adds_nothing_after_it() {
        let (sender, mut receiver) = tokio::sync::watch::channel(PluginEvent::Stopped);
        sender.send_replace(PluginEvent::GaveUp);
        drop(sender);
        assert_eq!(observe(&mut receiver), Some(PluginEvent::GaveUp));
        // Seen already: asking again must say there is nothing new, or the application would
        // announce the same failure every second for ever.
        assert_eq!(observe(&mut receiver), None);
    }

    #[test]
    fn a_live_supervisor_with_nothing_new_to_say_reports_nothing() {
        let (sender, mut receiver) = tokio::sync::watch::channel(PluginEvent::Started);
        // The value a receiver is created with counts as already seen.
        assert_eq!(observe(&mut receiver), None);
        sender.send_replace(PluginEvent::Connected);
        assert_eq!(observe(&mut receiver), Some(PluginEvent::Connected));
        assert_eq!(observe(&mut receiver), None);
    }

    #[test]
    fn a_plugin_that_was_never_started_is_stopped_not_running() {
        // No supervisor entry at all (it failed to launch, or was never registered to run):
        // nothing says it is running, so it must not be reported as such.
        assert_eq!(status_of(None), peersus_core::model::PluginStatus::Stopped);
    }

    #[test]
    fn a_grant_for_another_plugin_or_for_something_no_action_needs_never_leaks() {
        let registry = registry_with_alpha();
        let peers = [
            // Granted to a different plugin: says nothing about alpha.
            peer_granted("p1", &[("beta", &[("alpha", "cap")])]),
            // Granted to alpha, but no action of alpha needs it.
            peer_granted("p2", &[("alpha", &[("alpha", "unrelated")])]),
        ];
        assert_eq!(host_grants(&registry, &peers), set(&[]));
    }

    /// A plugin that takes what peers send, open to the application, with or without a
    /// capability guarding `receive`.
    fn plugin_that_receives(capability: Option<&str>) -> PluginSpec {
        PluginSpec {
            id: PluginId::new("sink").unwrap(),
            manifest: Manifest {
                capabilities: capability
                    .map(|c| (c.to_owned(), format!("Lets the plugin use {c}.")))
                    .into_iter()
                    .collect(),
                exports: vec![Export {
                    service: "sink.service".into(),
                    version: 1,
                    description: "A test service.".into(),
                    consumers: [HOST.to_owned()].into(),
                    methods: [(
                        crate::plugin_host::RECEIVE_METHOD.to_owned(),
                        member(capability),
                    )]
                    .into(),
                    events: Default::default(),
                }],
                ..Manifest::default()
            },
            program: Some(PathBuf::from("/nonexistent")),
            arguments: Vec::new(),
            restart_policy: None,
        }
    }

    #[test]
    fn what_the_user_grants_for_receive_is_what_lets_the_application_call_it() {
        let mut registry = Registry::default();
        registry
            .register(plugin_that_receives(Some("sink.write")))
            .unwrap();
        // The user allowed one device to use the plugin's `receive`. The hub only lets the
        // application call `receive` when it holds that capability, so it must be carried
        // over, exactly as the capability of an action is. A plugin whose only offer is
        // `receive` has no actions, so this cannot come from them.
        let peers = [peer_granted("p", &[("sink", &[("sink", "sink.write")])])];
        assert_eq!(
            host_grants(&registry, &peers),
            set(&[("sink", "sink.write")])
        );
    }

    #[test]
    fn nothing_is_carried_over_for_receive_until_the_user_has_granted_it() {
        let mut registry = Registry::default();
        registry
            .register(plugin_that_receives(Some("sink.write")))
            .unwrap();
        let nothing = [peer_granted("p", &[])];
        assert_eq!(host_grants(&registry, &nothing), set(&[]));
        // Another capability of the same plugin, or the right capability of another plugin,
        // says nothing about this one's `receive`.
        let wrong = [
            peer_granted("p", &[("sink", &[("sink", "sink.other")])]),
            peer_granted("q", &[("elsewhere", &[("sink", "sink.write")])]),
        ];
        assert_eq!(host_grants(&registry, &wrong), set(&[]));
    }

    #[test]
    fn the_application_can_call_receive_on_a_plugin_that_offers_it() {
        let mut registry = Registry::default();
        registry
            .register(plugin_that_receives(Some("sink.write")))
            .unwrap();
        let imported = host_manifest(&registry)
            .imports
            .into_iter()
            .find(|i| i.provider == "sink" && i.service == "sink.service")
            .expect("the application must import the service a plugin receives through");
        assert!(
            imported
                .methods
                .contains(crate::plugin_host::RECEIVE_METHOD),
            "without this import the hub refuses every delivery to the plugin"
        );
    }

    #[test]
    fn a_capability_that_guards_receive_is_declared_so_that_it_can_be_granted() {
        let mut registry = Registry::default();
        registry
            .register(plugin_that_receives(Some("sink.write")))
            .unwrap();
        assert!(
            host_manifest(&registry).declares("sink.write"),
            "an undeclared capability can never be granted, so deliveries would stay refused"
        );
    }

    #[test]
    fn the_applications_own_manifest_is_always_one_it_would_accept_from_a_plugin() {
        // The application obeys the rules it sets: its manifest is validated like any other,
        // or it could not join its own hub. That includes the permission it guards `send`
        // with, which it must declare and describe like a plugin would.
        let mut registry = Registry::default();
        assert!(
            host_manifest(&registry).validate().is_ok(),
            "with no plugins"
        );
        registry.register(plugin_with_guarded_action()).unwrap();
        assert!(
            host_manifest(&registry).validate().is_ok(),
            "with an action"
        );
        registry
            .register(plugin_wanting_peers("clip", &["list", "send"]))
            .unwrap();
        let manifest = host_manifest(&registry);
        assert!(
            manifest.validate().is_ok(),
            "with a plugin that asks to send"
        );
        // The permission that guards sending is declared, described, and by the application.
        let description = manifest
            .capabilities
            .get(PEERS_SEND_CAPABILITY)
            .expect("the permission guarding send is not declared");
        assert!(
            description.split_whitespace().count() >= 5,
            "{description:?}"
        );
    }

    #[test]
    fn a_plugin_that_does_not_receive_gets_no_receive_import() {
        let mut registry = Registry::default();
        registry.register(plugin_with_guarded_action()).unwrap();
        let manifest = host_manifest(&registry);
        assert!(
            manifest
                .imports
                .iter()
                .all(|i| !i.methods.contains(crate::plugin_host::RECEIVE_METHOD)),
            "nothing may be imported that the plugin never offered"
        );
    }

    #[test]
    fn the_application_can_be_granted_what_a_plugin_action_requires() {
        let mut registry = Registry::default();
        registry.register(plugin_with_guarded_action()).unwrap();
        let hub = Hub::default();
        let host_generation = hub
            .register_host(HOST, host_manifest(&registry), BTreeSet::new())
            .unwrap();
        let host = hub.connect(HOST, host_generation).unwrap();
        let spec = plugin_with_guarded_action();
        let generation = hub
            .register("alpha", spec.manifest.clone(), BTreeSet::new())
            .unwrap();
        let alpha = hub.connect("alpha", generation).unwrap();
        let call = |id| Operation::Call {
            id,
            provider: "alpha".into(),
            service: "svc".into(),
            version: 1,
            method: "go".into(),
            args: serde_json::Value::Null,
            timeout_ms: 5_000,
        };
        // Nothing is granted yet: refused, which is the safe default.
        assert_eq!(host.submit(call(1)), Err(peersus_plugin_api::Error::Denied));
        // The user's decision to allow it must be expressible. If the application's own
        // manifest did not declare the capability, no grant could ever be recorded and
        // every guarded action would stay refused no matter what the user chose.
        hub.set_permissions(HOST, [permission("alpha", "cap")].into())
            .expect("the application must be able to hold the capability an action needs");
        host.submit(call(2)).unwrap();
        assert!(matches!(
            alpha.try_recv().unwrap(),
            Some(Delivery::Call { caller, .. }) if caller == HOST
        ));
    }

    /// A plugin whose only import is `service` at `version` from `provider`.
    fn plugin_importing(
        name: &str,
        provider: &str,
        service: &str,
        version: u32,
        methods: &[&str],
    ) -> PluginSpec {
        use peersus_plugin_api::manifest::{Import, Manifest};
        PluginSpec {
            id: PluginId::new(name).unwrap(),
            manifest: Manifest {
                imports: vec![Import {
                    provider: provider.to_owned(),
                    service: service.to_owned(),
                    version,
                    methods: methods.iter().map(|m| (*m).to_owned()).collect(),
                    events: BTreeSet::new(),
                }],
                ..Manifest::default()
            },
            program: Some("unused".into()),
            arguments: Vec::new(),
            restart_policy: None,
        }
    }

    /// A plugin whose only import is the transfers service of the application, listening to these events.
    fn plugin_listening_to_transfers(name: &str, events: &[&str]) -> PluginSpec {
        use peersus_plugin_api::manifest::{Import, Manifest};
        PluginSpec {
            id: PluginId::new(name).unwrap(),
            manifest: Manifest {
                imports: vec![Import {
                    provider: HOST.to_owned(),
                    service: crate::transfer_events::TRANSFERS_SERVICE.to_owned(),
                    version: 1,
                    methods: BTreeSet::new(),
                    events: events.iter().map(|e| (*e).to_owned()).collect(),
                }],
                ..Manifest::default()
            },
            program: Some("unused".into()),
            arguments: Vec::new(),
            restart_policy: None,
        }
    }

    #[test]
    fn transfers_are_offered_only_to_the_plugins_that_asked_for_them_and_there_is_only_reading() {
        use crate::transfer_events as events;
        let mut registry = Registry::default();
        registry
            .register(plugin_listening_to_transfers("fireworks", &["changed"]))
            .unwrap();
        registry
            .register(plugin_wanting_peers("clip", &["list"]))
            .unwrap();
        let exports = host_exports(&registry);
        let transfers = exports
            .iter()
            .find(|e| e.service == events::TRANSFERS_SERVICE)
            .expect("the service was not offered to the plugin that asked");
        assert_eq!(
            transfers.consumers,
            ["fireworks".to_owned()].into(),
            "a plugin that did not ask was offered the transfers"
        );
        // A plugin can only READ about transfers: the two reads are the only methods, each behind a permission, and neither can change anything.
        let mut reads: Vec<&str> = transfers.methods.keys().map(String::as_str).collect();
        reads.sort_unstable();
        assert_eq!(
            reads,
            ["active", "active-named"],
            "something other than a read can be called on transfers"
        );
        assert!(
            transfers
                .methods
                .values()
                .all(|member| member.capability.is_some()),
            "a read of transfers is open to anyone"
        );
        assert!(!transfers.description.trim().is_empty());
        // Nobody asked: nothing is offered.
        let mut none = Registry::default();
        none.register(plugin_wanting_peers("clip", &["list"]))
            .unwrap();
        assert!(
            host_exports(&none)
                .iter()
                .all(|e| e.service != events::TRANSFERS_SERVICE)
        );
    }

    #[test]
    fn the_two_events_of_transfers_are_each_behind_a_permission_of_their_own_and_say_what_they_are_for()
     {
        use crate::transfer_events as events;
        let mut registry = Registry::default();
        registry
            .register(plugin_listening_to_transfers(
                "fireworks",
                &["changed", "changed-named"],
            ))
            .unwrap();
        let exports = host_exports(&registry);
        let transfers = exports
            .iter()
            .find(|e| e.service == events::TRANSFERS_SERVICE)
            .unwrap();
        let guard = |event: &str| {
            transfers
                .events
                .get(event)
                .map(|member| member.capability.as_deref())
        };
        assert_eq!(
            guard(events::TRANSFERS_CHANGED),
            Some(Some(events::TRANSFERS_WATCH_CAPABILITY))
        );
        assert_eq!(
            guard(events::TRANSFERS_CHANGED_NAMED),
            Some(Some(events::TRANSFERS_NAMES_CAPABILITY))
        );
        assert_ne!(
            events::TRANSFERS_WATCH_CAPABILITY,
            events::TRANSFERS_NAMES_CAPABILITY,
            "watching and seeing names must be two decisions"
        );
        assert!(
            transfers
                .events
                .values()
                .all(|member| !member.description.trim().is_empty())
        );
        // No event is open: a plugin can be told nothing about a transfer without a permission.
        assert!(
            transfers
                .events
                .values()
                .all(|member| member.capability.is_some())
        );
    }

    #[test]
    fn the_application_declares_both_permissions_so_a_person_can_be_asked_and_neither_is_granted_by_being_declared()
     {
        use crate::transfer_events as events;
        let mut registry = Registry::default();
        registry
            .register(plugin_listening_to_transfers(
                "fireworks",
                &["changed", "changed-named"],
            ))
            .unwrap();
        let manifest = host_manifest(&registry);
        assert!(manifest.validate().is_ok());
        for capability in [
            events::TRANSFERS_WATCH_CAPABILITY,
            events::TRANSFERS_NAMES_CAPABILITY,
        ] {
            assert!(
                manifest.declares(capability),
                "{capability} is not declared"
            );
        }
        // Declared is not granted: nothing is granted to the plugin until a person decides.
        assert!(
            host_grants(&registry, &[]).is_empty(),
            "a permission was granted without a decision"
        );
    }

    #[test]
    fn a_plugin_that_listens_only_to_the_unnamed_event_cannot_even_be_granted_the_permission_to_see_names()
     {
        use crate::transfer_events as events;
        assert_eq!(
            grantable(&plugin_listening_to_transfers("quiet", &["changed"])),
            set(&[(HOST, events::TRANSFERS_WATCH_CAPABILITY)]),
            "a plugin that never asked for names was offered the permission to see them"
        );
        assert_eq!(
            grantable(&plugin_listening_to_transfers(
                "nosy",
                &["changed", "changed-named"]
            )),
            set(&[
                (HOST, events::TRANSFERS_WATCH_CAPABILITY),
                (HOST, events::TRANSFERS_NAMES_CAPABILITY)
            ])
        );
    }

    /// A plugin whose only import is the status service of the application, calling these methods and listening to these events.
    fn plugin_using_statuses(name: &str, methods: &[&str], events: &[&str]) -> PluginSpec {
        use peersus_plugin_api::manifest::{Import, Manifest};
        PluginSpec {
            id: PluginId::new(name).unwrap(),
            manifest: Manifest {
                imports: vec![Import {
                    provider: HOST.to_owned(),
                    service: crate::status_service::STATUS_SERVICE.to_owned(),
                    version: 1,
                    methods: methods.iter().map(|m| (*m).to_owned()).collect(),
                    events: events.iter().map(|e| (*e).to_owned()).collect(),
                }],
                ..Manifest::default()
            },
            program: Some("unused".into()),
            arguments: Vec::new(),
            restart_policy: None,
        }
    }

    #[test]
    fn a_plugin_can_only_be_granted_the_status_permissions_for_what_it_asked_to_do() {
        use crate::status_service as status;
        // Making a status and turning it on are two permissions: only the second is SEEN by the other devices.
        assert_eq!(
            grantable(&plugin_using_statuses("maker", &["define"], &[])),
            set(&[(HOST, status::STATUS_DEFINE_CAPABILITY)]),
            "a plugin that only makes statuses was offered the permission to show them"
        );
        assert_eq!(
            grantable(&plugin_using_statuses(
                "shower",
                &["define", "set", "clear"],
                &[]
            )),
            set(&[
                (HOST, status::STATUS_DEFINE_CAPABILITY),
                (HOST, status::STATUS_SET_CAPABILITY)
            ]),
            "set and clear are one permission"
        );
        // Reading what the others say is a third, and it is offered by the read and by the event alike.
        for (methods, events) in [(&["list"][..], &[][..]), (&[][..], &["changed"][..])] {
            assert_eq!(
                grantable(&plugin_using_statuses("reader", methods, events)),
                set(&[(HOST, status::STATUS_WATCH_CAPABILITY)]),
                "{methods:?} {events:?}"
            );
        }
        // `own` is the plugin's own and asks nothing; a plugin that imports nothing of it can be granted nothing.
        assert_eq!(
            grantable(&plugin_using_statuses("own", &["own"], &[])),
            set(&[])
        );
        assert_eq!(
            grantable(&plugin_using_statuses("none", &[], &[])),
            set(&[])
        );
    }

    #[test]
    fn the_status_permissions_are_about_this_device_and_never_granted_for_one_of_the_others() {
        use crate::status_service as status;
        let everything =
            plugin_using_statuses("all", &["define", "set", "clear", "list"], &["changed"]);
        assert_eq!(
            grantable_without_a_device(&everything),
            set(&[
                (HOST, status::STATUS_DEFINE_CAPABILITY),
                (HOST, status::STATUS_SET_CAPABILITY),
                (HOST, status::STATUS_WATCH_CAPABILITY)
            ]),
            "a status permission was not offered once, for the whole application"
        );
        assert_eq!(
            grantable_for_a_device(&everything),
            set(&[]),
            "a status permission was offered for one device at a time"
        );
    }

    #[test]
    fn statuses_are_offered_only_to_the_plugins_that_asked_for_them_and_each_method_but_own_is_behind_a_permission()
     {
        use crate::status_service as status;
        let mut registry = Registry::default();
        registry
            .register(plugin_using_statuses(
                "player",
                &["define", "set", "clear", "own"],
                &[],
            ))
            .unwrap();
        registry
            .register(plugin_wanting_peers("clip", &["list"]))
            .unwrap();
        let exports = host_exports(&registry);
        let service = exports
            .iter()
            .find(|e| e.service == status::STATUS_SERVICE)
            .expect("the service was not offered to the plugin that asked");
        assert_eq!(
            service.consumers,
            ["player".to_owned()].into(),
            "a plugin that did not ask was offered statuses"
        );
        let guard = |method: &str| {
            service
                .methods
                .get(method)
                .unwrap_or_else(|| panic!("{method} is not offered"))
                .capability
                .clone()
        };
        assert_eq!(
            guard("define").as_deref(),
            Some(status::STATUS_DEFINE_CAPABILITY)
        );
        assert_eq!(guard("set").as_deref(), Some(status::STATUS_SET_CAPABILITY));
        assert_eq!(
            guard("clear").as_deref(),
            Some(status::STATUS_SET_CAPABILITY),
            "clearing is turning off what set turned on"
        );
        assert_eq!(
            guard("list").as_deref(),
            Some(status::STATUS_WATCH_CAPABILITY)
        );
        assert_eq!(
            guard("own"),
            None,
            "what a plugin made itself is its own to read"
        );
        assert_eq!(
            service
                .events
                .get("changed")
                .and_then(|member| member.capability.as_deref()),
            Some(status::STATUS_WATCH_CAPABILITY),
            "the event that tells what the others say is open to anyone"
        );
        // Nobody asked: nothing is offered.
        let mut nobody = Registry::default();
        nobody
            .register(plugin_wanting_peers("clip", &["list"]))
            .unwrap();
        assert!(
            host_exports(&nobody)
                .iter()
                .all(|e| e.service != status::STATUS_SERVICE),
            "a service nobody asked for was offered"
        );
    }

    #[test]
    fn the_application_describes_each_status_permission_in_its_own_manifest_in_the_words_of_the_one_copy()
     {
        use crate::status_service as status;
        let mut registry = Registry::default();
        registry
            .register(plugin_using_statuses(
                "all",
                &["define", "set", "list"],
                &["changed"],
            ))
            .unwrap();
        let manifest = host_manifest(&registry);
        for (capability, description) in [
            (
                status::STATUS_DEFINE_CAPABILITY,
                status::STATUS_DEFINE_DESCRIPTION,
            ),
            (
                status::STATUS_SET_CAPABILITY,
                status::STATUS_SET_DESCRIPTION,
            ),
            (
                status::STATUS_WATCH_CAPABILITY,
                status::STATUS_WATCH_DESCRIPTION,
            ),
        ] {
            assert_eq!(
                manifest.capabilities.get(capability).map(String::as_str),
                Some(description),
                "{capability}"
            );
        }
    }

    /// A plugin that asks the application for `peersus.peers` and nothing else.
    fn plugin_wanting_peers(name: &str, methods: &[&str]) -> PluginSpec {
        plugin_importing(name, HOST, PEERS_SERVICE, 1, methods)
    }

    #[test]
    fn the_application_offers_nothing_when_no_plugin_asked_for_it() {
        let mut registry = Registry::default();
        registry.register(plugin_with_guarded_action()).unwrap();
        assert!(
            host_exports(&registry).is_empty(),
            "a service nobody asked for is surface for nothing"
        );
    }

    #[test]
    fn the_application_offers_peers_only_to_the_plugins_that_asked_for_it() {
        let mut registry = Registry::default();
        registry
            .register(plugin_wanting_peers("clip", &["list", "send"]))
            .unwrap();
        registry
            .register(plugin_wanting_peers("lister", &["list"]))
            .unwrap();
        registry.register(plugin_with_guarded_action()).unwrap();
        let exports = host_exports(&registry);
        assert_eq!(exports.len(), 1);
        let peers = &exports[0];
        assert_eq!((peers.service.as_str(), peers.version), (PEERS_SERVICE, 1));
        // Exactly the plugins that imported it: not the one with only an action, and not
        // any plugin that happens to exist.
        assert_eq!(
            peers.consumers,
            ["clip".to_owned(), "lister".to_owned()].into()
        );
        // Listing exposes no key and is open to a consumer; sending is guarded.
        assert_eq!(
            peers.methods.get("list").map(|m| m.capability.as_deref()),
            Some(None)
        );
        assert_eq!(
            peers.methods.get("send").map(|m| m.capability.as_deref()),
            Some(Some(PEERS_SEND_CAPABILITY))
        );
        // And every part of what the application offers says what it is for.
        assert!(!peers.description.trim().is_empty());
        assert!(
            peers
                .methods
                .values()
                .all(|m| !m.description.trim().is_empty())
        );
    }

    #[test]
    fn the_transfers_export_and_its_permissions_follow_who_asks_for_them_in_both_directions() {
        use crate::transfer_events as events;
        // Nobody asks: the service is not in what the application offers.
        let mut registry = Registry::default();
        registry
            .register(plugin_wanting_peers("clip", &["list"]))
            .unwrap();
        assert!(
            host_exports(&registry)
                .iter()
                .all(|export| export.service != events::TRANSFERS_SERVICE)
        );
        // A plugin asks: the service appears with both events, and the manifest the application publishes declares what guards them.
        registry
            .register(plugin_listening_to_transfers(
                "fireworks",
                &["changed", "changed-named"],
            ))
            .unwrap();
        let exports = host_exports(&registry);
        let offered = exports
            .iter()
            .find(|export| export.service == events::TRANSFERS_SERVICE)
            .expect("asked for, and not offered");
        assert_eq!(offered.events.len(), 2);
        assert!(host_manifest(&registry).declares(events::TRANSFERS_WATCH_CAPABILITY));
        // The plugin is removed: the offer goes with it, from a manifest built again from the registry and not from one that was kept.
        registry.remove(&PluginId::new("fireworks").unwrap());
        assert!(
            host_exports(&registry)
                .iter()
                .all(|export| export.service != events::TRANSFERS_SERVICE),
            "a stale export went on offering transfers to nobody"
        );
    }

    #[test]
    fn through_the_real_hub_a_plugin_without_the_permission_cannot_even_ask_to_listen() {
        use crate::transfer_events as events;
        let (host, fireworks, _hub) = hub_with_transfers_plugin(&[]);
        for event in [events::TRANSFERS_CHANGED, events::TRANSFERS_CHANGED_NAMED] {
            assert_eq!(
                fireworks.submit(listen_to(event)),
                Err(peersus_plugin_api::error::Error::Denied),
                "{event} was open to a plugin with no permission"
            );
        }
        tell(
            &host,
            events::TRANSFERS_CHANGED,
            serde_json::json!({ "id": "t1" }),
        );
        assert!(
            fireworks.try_recv().unwrap().is_none(),
            "a plugin with no permission was told something"
        );
    }

    #[test]
    fn through_the_real_hub_watching_gets_the_unnamed_event_and_never_the_named_one() {
        use crate::transfer_events as events;
        let (host, fireworks, _hub) =
            hub_with_transfers_plugin(&[events::TRANSFERS_WATCH_CAPABILITY]);
        fireworks
            .submit(listen_to(events::TRANSFERS_CHANGED))
            .unwrap();
        assert_eq!(
            fireworks.submit(listen_to(events::TRANSFERS_CHANGED_NAMED)),
            Err(peersus_plugin_api::error::Error::Denied),
            "a plugin allowed only to watch could listen for names"
        );
        tell(
            &host,
            events::TRANSFERS_CHANGED,
            serde_json::json!({ "id": "t1", "state": "complete" }),
        );
        tell(
            &host,
            events::TRANSFERS_CHANGED_NAMED,
            serde_json::json!({ "id": "t1", "name": "secret.pdf" }),
        );
        let told = fireworks.try_recv().unwrap();
        assert!(
            matches!(&told, Some(Delivery::Event { data, .. }) if data["id"] == "t1" && data.get("name").is_none()),
            "{told:?}"
        );
        assert!(
            fireworks.try_recv().unwrap().is_none(),
            "the named event reached a plugin that was not allowed names"
        );
    }

    #[test]
    fn through_the_real_hub_the_permission_to_see_names_is_what_lets_a_plugin_hear_the_named_event()
    {
        use crate::transfer_events as events;
        let (host, fireworks, _hub) =
            hub_with_transfers_plugin(&[events::TRANSFERS_NAMES_CAPABILITY]);
        fireworks
            .submit(listen_to(events::TRANSFERS_CHANGED_NAMED))
            .unwrap();
        tell(
            &host,
            events::TRANSFERS_CHANGED_NAMED,
            serde_json::json!({ "id": "t1", "name": "secret.pdf" }),
        );
        assert!(
            matches!(fireworks.try_recv().unwrap(), Some(Delivery::Event { data, .. }) if data["name"] == "secret.pdf")
        );
    }

    #[test]
    fn through_the_real_hub_withdrawing_the_permission_ends_the_events_at_once() {
        use crate::transfer_events as events;
        let (host, fireworks, hub) =
            hub_with_transfers_plugin(&[events::TRANSFERS_WATCH_CAPABILITY]);
        fireworks
            .submit(listen_to(events::TRANSFERS_CHANGED))
            .unwrap();
        tell(
            &host,
            events::TRANSFERS_CHANGED,
            serde_json::json!({ "id": "t1" }),
        );
        hub.set_permissions("fireworks", BTreeSet::new()).unwrap();
        assert!(
            fireworks.try_recv().unwrap().is_none(),
            "an event queued before the withdrawal was delivered after it"
        );
        tell(
            &host,
            events::TRANSFERS_CHANGED,
            serde_json::json!({ "id": "t2" }),
        );
        assert!(
            fireworks.try_recv().unwrap().is_none(),
            "a plugin whose permission was withdrawn was still told"
        );
    }

    #[test]
    fn every_text_the_application_exports_fits_in_what_a_manifest_allows() {
        let mut registry = Registry::default();
        registry
            .register(plugin_listening_to_transfers(
                "fireworks",
                &["changed", "changed-named"],
            ))
            .unwrap();
        registry
            .register(plugin_wanting_peers("clip", &["list", "send"]))
            .unwrap();
        registry
            .register(plugin_managing("watchdog", &["list", "restart"]))
            .unwrap();
        let limit = peersus_plugin_api::manifest::MAX_DESCRIPTION;
        for export in host_exports(&registry) {
            let mut texts = vec![(
                format!("{} (service)", export.service),
                export.description.clone(),
            )];
            for (member, details) in export.methods.iter().chain(&export.events) {
                texts.push((
                    format!("{}.{member}", export.service),
                    details.description.clone(),
                ));
            }
            for (what, text) in texts {
                let length = text.chars().count();
                assert!(
                    length <= limit,
                    "{what} is {length} characters and a manifest allows {limit}"
                );
            }
        }
    }

    /// A plugin that imports `peersus.plugins` from the application, with these methods.
    fn plugin_managing(name: &str, methods: &[&str]) -> PluginSpec {
        plugin_importing(name, HOST, PLUGINS_SERVICE, 1, methods)
    }

    #[test]
    fn the_application_offers_the_plugin_service_only_to_plugins_that_asked_for_it() {
        let mut registry = Registry::default();
        registry
            .register(plugin_managing("watchdog", &["list", "restart"]))
            .unwrap();
        registry
            .register(plugin_wanting_peers("clip", &["list", "send"]))
            .unwrap();
        let exports = host_exports(&registry);
        let find = |service: &str| exports.iter().find(|e| e.service == service);
        // Each service is offered to exactly the plugins that imported THAT service: asking
        // for the devices gives no right to see the plugins, and the other way round.
        assert_eq!(
            find(PLUGINS_SERVICE).map(|e| e.consumers.clone()),
            Some(["watchdog".to_owned()].into())
        );
        assert_eq!(
            find(PEERS_SERVICE).map(|e| e.consumers.clone()),
            Some(["clip".to_owned()].into())
        );
    }

    #[test]
    fn nothing_about_plugins_is_offered_when_no_plugin_asked_for_it() {
        let mut registry = Registry::default();
        registry
            .register(plugin_wanting_peers("clip", &["list", "send"]))
            .unwrap();
        assert!(
            host_exports(&registry)
                .iter()
                .all(|e| e.service != PLUGINS_SERVICE)
        );
    }

    #[test]
    fn listing_plugins_is_open_but_restarting_one_is_guarded_and_both_are_described() {
        let mut registry = Registry::default();
        registry
            .register(plugin_managing("watchdog", &["list", "restart"]))
            .unwrap();
        let exports = host_exports(&registry);
        let plugins = exports
            .iter()
            .find(|e| e.service == PLUGINS_SERVICE)
            .expect("the plugin service was not offered");
        assert_eq!(
            plugins.methods.get("list").map(|m| m.capability.as_deref()),
            Some(None)
        );
        // Restarting can interrupt any plugin, so it is behind a permission of the
        // application's own, which a person must grant.
        assert_eq!(
            plugins
                .methods
                .get("restart")
                .map(|m| m.capability.as_deref()),
            Some(Some(PLUGINS_RESTART_CAPABILITY))
        );
        assert!(plugins.events.contains_key("state-changed"));
        assert!(!plugins.description.trim().is_empty());
        assert!(
            plugins
                .methods
                .values()
                .all(|m| !m.description.trim().is_empty())
        );
        assert!(
            plugins
                .events
                .values()
                .all(|m| !m.description.trim().is_empty())
        );
    }

    #[test]
    fn the_permission_to_restart_says_plainly_how_much_it_allows() {
        // It lets a plugin interrupt ANY other plugin, which is among the most a plugin can
        // be allowed. The words a person reads must not understate it.
        let text = PLUGINS_RESTART_DESCRIPTION.to_lowercase();
        assert!(text.contains("any"), "{PLUGINS_RESTART_DESCRIPTION}");
        assert!(text.contains("plugin"), "{PLUGINS_RESTART_DESCRIPTION}");
        assert!(
            text.contains("interrupt") || text.contains("stop"),
            "{PLUGINS_RESTART_DESCRIPTION}"
        );
    }

    #[test]
    fn the_applications_own_manifest_stays_valid_with_the_plugin_service_too() {
        let mut registry = Registry::default();
        registry
            .register(plugin_managing("watchdog", &["list", "restart"]))
            .unwrap();
        registry
            .register(plugin_wanting_peers("clip", &["list", "send"]))
            .unwrap();
        let manifest = host_manifest(&registry);
        assert!(manifest.validate().is_ok());
        // Each permission it guards a method with is declared and described by the
        // application, because the application is the author of both services.
        for capability in [PEERS_SEND_CAPABILITY, PLUGINS_RESTART_CAPABILITY] {
            assert!(
                manifest.declares(capability),
                "{capability} is not declared"
            );
        }
    }

    #[test]
    fn a_plugin_that_imports_the_plugin_service_can_be_granted_the_permission_to_restart() {
        // What can be granted is derived from the roles a plugin plays: importing `restart`
        // makes the application's guard on it grantable, and importing only `list` does not.
        assert_eq!(
            grantable(&plugin_managing("watchdog", &["list", "restart"])),
            set(&[(HOST, PLUGINS_RESTART_CAPABILITY)])
        );
        assert_eq!(grantable(&plugin_managing("viewer", &["list"])), set(&[]));
    }

    fn standalone(plugin: &str, granted: &[(&str, &str)]) -> BTreeMap<PluginId, Grants> {
        BTreeMap::from([(
            PluginId::new(plugin).unwrap(),
            Grants::new(granted.iter().map(|(o, n)| permission(o, n))).unwrap(),
        )])
    }

    #[test]
    fn restarting_is_granted_outright_and_never_for_a_device() {
        let spec = plugin_managing("watchdog", &["list", "restart"]);
        assert_eq!(
            grantable_without_a_device(&spec),
            set(&[(HOST, PLUGINS_RESTART_CAPABILITY)])
        );
        // It is about no device, so a device is not a place to grant it.
        assert_eq!(grantable_for_a_device(&spec), set(&[]));
        // And the other way round: sending is about a device and cannot be granted outright.
        let sender = plugin_wanting_peers("clip", &["list", "send"]);
        assert_eq!(grantable_without_a_device(&sender), set(&[]));
        assert_eq!(
            grantable_for_a_device(&sender),
            set(&[(HOST, PEERS_SEND_CAPABILITY)])
        );
    }

    #[test]
    fn the_two_kinds_never_overlap_and_together_are_everything_grantable() {
        let mut spec = plugin_managing("both", &["list", "restart"]);
        spec.manifest.imports.push(
            plugin_wanting_peers("both", &["list", "send"])
                .manifest
                .imports
                .remove(0),
        );
        let all = grantable(&spec);
        let device = grantable_for_a_device(&spec);
        let none = grantable_without_a_device(&spec);
        assert!(device.is_disjoint(&none));
        assert_eq!(device.union(&none).cloned().collect::<BTreeSet<_>>(), all);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn what_a_window_is_told_a_plugin_can_be_granted_is_exactly_the_owners_two_sets() {
        let mut spec = plugin_managing("both", &["list", "restart"]);
        spec.manifest.imports.push(
            plugin_wanting_peers("both", &["list", "send"])
                .manifest
                .imports
                .remove(0),
        );
        // A name the plugin merely listed in its own manifest: not grantable, so not shown.
        spec.manifest
            .capabilities
            .insert("just.listed".into(), "Only listed by the plugin.".into());

        let told = grantable_for_window(&spec);
        let device: BTreeSet<_> = told.per_device.iter().cloned().collect();
        let outright: BTreeSet<_> = told.outright.iter().cloned().collect();
        assert_eq!(device, grantable_for_a_device(&spec));
        assert_eq!(outright, grantable_without_a_device(&spec));
        assert!(
            device.is_disjoint(&outright),
            "a permission is offered both ways"
        );
        assert!(
            !device
                .iter()
                .chain(&outright)
                .any(|p| p.name == "just.listed"),
            "a name the plugin only listed was offered as a permission"
        );
        // The same plugin gives the same answer, so the window's rows do not reshuffle.
        assert_eq!(grantable_for_window(&spec), told);
    }

    #[test]
    fn two_quick_flips_started_from_the_same_picture_never_undo_each_other() {
        let a = Permission::new("host", "peers.send").unwrap();
        let b = Permission::new("host", "plugins.restart").unwrap();
        let start = Grants::new([a.clone()]).unwrap();

        // The window saw {A} and flipped A off, then, before the answer came, flipped B on. Each is
        // applied to what the owner holds when it ARRIVES, not to the picture the window had.
        let after_first = applied(&start, &a, false).unwrap();
        let after_second = applied(&after_first, &b, true).unwrap();
        assert!(
            !after_second.allows(&a),
            "a withdrawal that was made was undone by the next flip"
        );
        assert!(after_second.allows(&b));
        assert_eq!(after_second.iter().count(), 1);

        // The other order gives the same end: the flips are about different permissions.
        let other = applied(&applied(&start, &b, true).unwrap(), &a, false).unwrap();
        assert_eq!(other, after_second);
        // Allowing what is allowed, or withdrawing what is not, changes nothing.
        assert_eq!(applied(&start, &a, true).unwrap(), start);
        assert_eq!(applied(&start, &b, false).unwrap(), start);
    }

    #[test]
    fn a_plugin_holds_restart_at_the_hub_when_granted_outright_with_no_device_paired() {
        let spec = plugin_managing("watchdog", &["list", "restart"]);
        // No device is paired at all, which is the case that made this necessary.
        assert_eq!(
            plugin_grants(
                &spec,
                &[],
                &standalone("watchdog", &[(HOST, PLUGINS_RESTART_CAPABILITY)])
            ),
            set(&[(HOST, PLUGINS_RESTART_CAPABILITY)])
        );
        // Not granted, nothing held.
        assert_eq!(plugin_grants(&spec, &[], &BTreeMap::new()), set(&[]));
    }

    #[test]
    fn what_was_granted_outright_to_another_plugin_says_nothing_about_this_one() {
        let spec = plugin_managing("watchdog", &["list", "restart"]);
        assert_eq!(
            plugin_grants(
                &spec,
                &[],
                &standalone("someone-else", &[(HOST, PLUGINS_RESTART_CAPABILITY)])
            ),
            set(&[])
        );
    }

    #[test]
    fn a_plugin_that_never_imported_restart_does_not_hold_it_even_if_granted_outright() {
        let viewer = plugin_managing("viewer", &["list"]);
        assert_eq!(
            plugin_grants(
                &viewer,
                &[],
                &standalone("viewer", &[(HOST, PLUGINS_RESTART_CAPABILITY)])
            ),
            set(&[])
        );
    }

    #[test]
    fn a_plugin_holds_a_status_permission_at_the_hub_only_when_it_was_granted_outright_to_that_plugin()
     {
        use crate::status_service as status;
        let spec = plugin_using_statuses("player", &["define", "set", "clear"], &[]);
        let both = [
            (HOST, status::STATUS_DEFINE_CAPABILITY),
            (HOST, status::STATUS_SET_CAPABILITY),
        ];
        // No device is paired at all: the permissions are about this device, so this must hold.
        assert_eq!(
            plugin_grants(&spec, &[], &standalone("player", &both)),
            set(&both)
        );
        // Granted one of them: that one only, so making a status is not turning it on.
        assert_eq!(
            plugin_grants(
                &spec,
                &[],
                &standalone("player", &[(HOST, status::STATUS_DEFINE_CAPABILITY)])
            ),
            set(&[(HOST, status::STATUS_DEFINE_CAPABILITY)])
        );
        // Not granted, nothing held: a plugin can do nothing to a status until the person says so.
        assert_eq!(plugin_grants(&spec, &[], &BTreeMap::new()), set(&[]));
        // What was granted to another plugin says nothing about this one.
        assert_eq!(
            plugin_grants(&spec, &[], &standalone("someone-else", &both)),
            set(&[])
        );
    }

    #[test]
    fn a_plugin_that_never_imported_what_a_status_permission_guards_does_not_hold_it_even_if_granted()
     {
        use crate::status_service as status;
        // It reads and nothing else: being granted the permission to show a status gives it no way to do it.
        let reader = plugin_using_statuses("reader", &["list"], &[]);
        assert_eq!(
            plugin_grants(
                &reader,
                &[],
                &standalone(
                    "reader",
                    &[
                        (HOST, status::STATUS_SET_CAPABILITY),
                        (HOST, status::STATUS_DEFINE_CAPABILITY)
                    ]
                )
            ),
            set(&[]),
            "a plugin held a permission for something it never asked to do"
        );
        assert_eq!(
            plugin_grants(
                &reader,
                &[],
                &standalone("reader", &[(HOST, status::STATUS_WATCH_CAPABILITY)])
            ),
            set(&[(HOST, status::STATUS_WATCH_CAPABILITY)])
        );
    }

    #[test]
    fn a_device_saying_a_status_permission_was_granted_does_not_give_it_to_the_plugin() {
        use crate::status_service as status;
        // Nothing should ever store this on a device, but a stored file is only ever as good as what wrote it. These permissions are about
        // no device, so a device cannot supply them: otherwise a paired device could make a plugin show anything to everyone.
        let spec = plugin_using_statuses("player", &["define", "set"], &[]);
        let peer = granted_exactly("a", "player", HOST, status::STATUS_SET_CAPABILITY);
        assert_eq!(
            plugin_grants(&spec, std::slice::from_ref(&peer), &BTreeMap::new()),
            set(&[])
        );
    }

    #[test]
    fn a_device_saying_restart_is_granted_does_not_give_it_to_the_plugin() {
        // Nothing should ever store this on a device, but a stored file is only ever as good
        // as what wrote it. The permission is about no device, so a device cannot supply it.
        let spec = plugin_managing("watchdog", &["list", "restart"]);
        let peers = [peer_granted(
            "p",
            &[("watchdog", &[(HOST, PLUGINS_RESTART_CAPABILITY)])],
        )];
        assert_eq!(device_grants(&spec, &peers), set(&[]));
    }

    #[test]
    fn granting_outright_does_not_turn_a_device_permission_into_one_for_every_device() {
        // `peers.send` is about a device. Having it recorded outright must not make the
        // plugin hold it at the hub: that would grant it without any device's say.
        let spec = plugin_wanting_peers("clip", &["list", "send"]);
        assert_eq!(
            plugin_grants(
                &spec,
                &[],
                &standalone("clip", &[(HOST, PEERS_SEND_CAPABILITY)])
            ),
            set(&[])
        );
    }

    fn info(
        id: &str,
        status: peersus_core::model::PluginStatus,
    ) -> peersus_core::model::PluginInfo {
        // A manifest with recognisable content, so that a leak of any part of it is visible.
        let mut spec = plugin_wanting_peers(id, &["list", "send"]);
        spec.manifest.capabilities.insert(
            "secret.name".to_owned(),
            "A recognisable description.".to_owned(),
        );
        peersus_core::model::PluginInfo {
            id: PluginId::new(id).unwrap(),
            manifest: spec.manifest,
            standing: peersus_core::model::Standing::Ready,
            status,
            grantable: Default::default(),
            program: None,
            // A recognisable author and link, so that any leak of the package block into what plugins are told
            // about each other is caught by the test below.
            package: Some(peersus_plugin_api::package::Package {
                name: "Secret package".into(),
                version: "9.9.9".into(),
                description: String::new(),
                changelog: String::new(),
                author: "Secret author".into(),
                link: "https://secret.example/author".into(),
            }),
        }
    }

    #[test]
    fn listing_plugins_says_each_ones_id_and_state_and_nothing_else() {
        use peersus_core::model::PluginStatus::{Failed, Running};
        let listed = plugins_listing(&[info("clip", Running), info("sink", Failed)]);
        assert_eq!(
            listed,
            serde_json::json!([
                {"id": "clip", "status": "running"},
                {"id": "sink", "status": "failed"},
            ])
        );
        // Not the manifest, what it declared, what it may do, or where it lives.
        let text = listed.to_string();
        for leaked in [
            "secret.name",
            "recognisable",
            "peersus.peers",
            "send",
            "program",
            "Secret author",
            "secret.example",
            "9.9.9",
        ] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
    }

    #[test]
    fn listing_no_plugins_is_an_empty_list_not_an_error() {
        assert_eq!(plugins_listing(&[]), serde_json::json!([]));
    }

    #[test]
    fn a_restart_request_names_one_plugin_and_nothing_else_may_ride_along() {
        let ok = parse_restart(&serde_json::json!({"plugin": "clip"})).unwrap();
        assert_eq!(ok.as_str(), "clip");
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"plugin": "clip", "force": true}),
            serde_json::json!({"plugin": ""}),
            serde_json::json!({"plugin": "../etc"}),
            serde_json::json!({"plugin": "Has Space"}),
            serde_json::json!({"plugin": 7}),
            serde_json::json!({"plugin": ["clip"]}),
            serde_json::json!("clip"),
            serde_json::Value::Null,
        ] {
            assert!(parse_restart(&bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn a_plugin_that_imports_something_almost_the_same_is_not_a_consumer() {
        let mut registry = Registry::default();
        registry
            .register(plugin_wanting_peers("real", &["list"]))
            .unwrap();
        // Another provider that happens to offer the same service name.
        registry
            .register(plugin_importing(
                "lookalike",
                "someone",
                PEERS_SERVICE,
                1,
                &["list"],
            ))
            .unwrap();
        // The application, but a different service.
        registry
            .register(plugin_importing(
                "elsewhere",
                HOST,
                "peersus.other",
                1,
                &["list"],
            ))
            .unwrap();
        // The right service at a version the application does not offer.
        registry
            .register(plugin_importing("stale", HOST, PEERS_SERVICE, 2, &["list"]))
            .unwrap();
        let exports = host_exports(&registry);
        assert_eq!(exports.len(), 1);
        assert_eq!(
            exports[0].consumers,
            ["real".to_owned()].into(),
            "only a plugin that really asked the application for this version may be served"
        );
    }

    /// The application and one plugin that asked for `peersus.peers`, joined to a hub as the
    /// runtime joins them, with `granted` held by the plugin.
    fn hub_with_peers_plugin(
        granted: &[&str],
    ) -> (
        peersus_plugin_api::hub::Channel,
        peersus_plugin_api::hub::Channel,
    ) {
        let mut registry = Registry::default();
        let mut spec = plugin_wanting_peers("clip", &["list", "send"]);
        // A plugin can only be granted what its own manifest declares.
        spec.manifest.capabilities.insert(
            PEERS_SEND_CAPABILITY.to_owned(),
            "Sends data to a device.".to_owned(),
        );
        registry.register(spec.clone()).unwrap();
        let hub = Hub::default();
        let host_generation = hub
            .register_host(HOST, host_manifest(&registry), BTreeSet::new())
            .unwrap();
        let host = hub.connect(HOST, host_generation).unwrap();
        // What a plugin asking the application to send holds is the APPLICATION's permission.
        let granted: BTreeSet<Permission> =
            granted.iter().map(|name| permission(HOST, name)).collect();
        let generation = hub
            .register("clip", spec.manifest.clone(), granted)
            .unwrap();
        let clip = hub.connect("clip", generation).unwrap();
        (host, clip)
    }

    /// The application and one plugin that listens to both events of `peersus.transfers`, joined to a hub as the runtime joins them, with `granted`
    /// held by the plugin and the plugin already subscribed to the events its permissions allow it to ask for.
    fn hub_with_transfers_plugin(
        granted: &[&str],
    ) -> (
        peersus_plugin_api::hub::Channel,
        peersus_plugin_api::hub::Channel,
        Hub,
    ) {
        use crate::transfer_events as events;
        let mut registry = Registry::default();
        let mut spec = plugin_listening_to_transfers("fireworks", &["changed", "changed-named"]);
        // A plugin can only be granted what its own manifest declares.
        for capability in [
            events::TRANSFERS_WATCH_CAPABILITY,
            events::TRANSFERS_NAMES_CAPABILITY,
        ] {
            spec.manifest.capabilities.insert(
                capability.to_owned(),
                "Lets the plugin hear about transfers.".to_owned(),
            );
        }
        registry.register(spec.clone()).unwrap();
        let hub = Hub::default();
        let host_generation = hub
            .register_host(HOST, host_manifest(&registry), BTreeSet::new())
            .unwrap();
        let host = hub.connect(HOST, host_generation).unwrap();
        let granted: BTreeSet<Permission> =
            granted.iter().map(|name| permission(HOST, name)).collect();
        let generation = hub
            .register("fireworks", spec.manifest.clone(), granted)
            .unwrap();
        let fireworks = hub.connect("fireworks", generation).unwrap();
        (host, fireworks, hub)
    }

    fn listen_to(event: &str) -> Operation {
        Operation::Subscribe {
            provider: HOST.into(),
            service: crate::transfer_events::TRANSFERS_SERVICE.into(),
            version: 1,
            event: event.into(),
        }
    }

    fn tell(host: &peersus_plugin_api::hub::Channel, event: &str, data: serde_json::Value) {
        let _ = host.submit(Operation::Publish {
            service: crate::transfer_events::TRANSFERS_SERVICE.into(),
            version: 1,
            event: event.into(),
            data,
        });
    }

    fn call_peers(id: u64, method: &str) -> Operation {
        Operation::Call {
            id,
            provider: HOST.into(),
            service: PEERS_SERVICE.into(),
            version: 1,
            method: method.into(),
            args: serde_json::Value::Null,
            timeout_ms: 5_000,
        }
    }

    #[test]
    fn a_plugin_that_asked_can_list_devices_but_cannot_send_until_it_is_granted() {
        let (host, clip) = hub_with_peers_plugin(&[]);
        // `list` is open to a consumer and reaches the application.
        clip.submit(call_peers(1, "list")).unwrap();
        assert!(matches!(
            host.try_recv().unwrap(),
            Some(Delivery::Call { caller, method, .. }) if caller == "clip" && method == "list"
        ));
        // `send` is guarded: refused at the hub until the plugin holds the capability.
        assert_eq!(
            clip.submit(call_peers(2, "send")),
            Err(peersus_plugin_api::Error::Denied)
        );
    }

    #[test]
    fn a_granted_plugin_can_reach_send_and_an_ungranted_one_still_cannot() {
        let (host, clip) = hub_with_peers_plugin(&[PEERS_SEND_CAPABILITY]);
        clip.submit(call_peers(1, "send")).unwrap();
        assert!(matches!(
            host.try_recv().unwrap(),
            Some(Delivery::Call { caller, method, .. }) if caller == "clip" && method == "send"
        ));
    }

    fn clip_spec() -> PluginSpec {
        let mut spec = plugin_wanting_peers("clip", &["list", "send"]);
        spec.manifest.capabilities.insert(
            PEERS_SEND_CAPABILITY.to_owned(),
            "Sends data to a device.".to_owned(),
        );
        spec
    }

    // What can be granted to a plugin is derived from the ROLES it plays, never from what it
    // declares for itself: it is a permission of its own for what it offers, and a permission
    // of the application for what it asks the application to do.

    #[test]
    fn what_can_be_granted_is_its_own_guards_and_the_applications_guards_it_imports() {
        // A plugin that asks the application to send: the application's guard on `send`, and
        // nothing else, because it offers nothing the application would call.
        assert_eq!(
            grantable(&clip_spec()),
            set(&[(HOST, PEERS_SEND_CAPABILITY)])
        );
        // A plugin that takes what devices send, guarded by its own permission: that
        // permission is the plugin's own, and the application's `send` is not offered to it
        // because it imports nothing from the application.
        assert_eq!(
            grantable(&plugin_that_receives(Some("sink.write"))),
            set(&[("sink", "sink.write")])
        );
        // A plugin that offers nothing guarded and imports nothing guarded has nothing to grant.
        assert_eq!(grantable(&plugin_that_receives(None)), set(&[]));
    }

    #[test]
    fn a_permission_the_plugin_merely_declares_for_itself_is_not_grantable_as_the_applications() {
        // The plugin declares its own `peers.send` and imports nothing guarded by the
        // application. Granting that name must not produce the application's permission:
        // the name is the plugin's, with whatever meaning it chose.
        let mut spec = plugin_wanting_peers("evil", &["list"]);
        spec.manifest.capabilities.insert(
            PEERS_SEND_CAPABILITY.to_owned(),
            "Totally harmless.".to_owned(),
        );
        let got = grantable(&spec);
        assert!(
            !got.contains(&permission(HOST, PEERS_SEND_CAPABILITY)),
            "a declared name became the application's permission: {got:?}"
        );
    }

    /// What a plugin holds at the hub from what devices granted, when nothing was granted
    /// outright.
    fn device_grants(spec: &PluginSpec, peers: &[Peer]) -> BTreeSet<Permission> {
        plugin_grants(spec, peers, &BTreeMap::new())
    }

    #[test]
    fn declaring_the_applications_name_does_not_change_whose_permission_the_hub_is_told_of() {
        // A plugin that imports `send` AND also declares its own `peers.send`: what it holds at
        // the hub is still the application's permission, once granted, and only that.
        let spec = clip_spec();
        let grant = [peer_granted(
            "p",
            &[("clip", &[(HOST, PEERS_SEND_CAPABILITY)])],
        )];
        assert_eq!(
            device_grants(&spec, &grant),
            set(&[(HOST, PEERS_SEND_CAPABILITY)])
        );
        // And a grant of a permission of the same name but of the plugin itself is a
        // different thing, and carries nothing to the hub for `send`.
        let own = [peer_granted(
            "p",
            &[("clip", &[("clip", PEERS_SEND_CAPABILITY)])],
        )];
        assert_eq!(device_grants(&spec, &own), set(&[]));
    }

    #[test]
    fn a_plugin_holds_nothing_at_the_hub_until_a_device_granted_it_something() {
        let spec = clip_spec();
        assert_eq!(device_grants(&spec, &[]), set(&[]));
        let none = [peer_granted("p", &[])];
        assert_eq!(device_grants(&spec, &none), set(&[]));
    }

    #[test]
    fn a_grant_on_any_one_device_lets_the_plugin_reach_send_at_the_hub() {
        let spec = clip_spec();
        let peers = [
            peer_granted("a", &[]),
            peer_granted("b", &[("clip", &[(HOST, PEERS_SEND_CAPABILITY)])]),
        ];
        assert_eq!(
            device_grants(&spec, &peers),
            set(&[(HOST, PEERS_SEND_CAPABILITY)])
        );
    }

    #[test]
    fn what_was_granted_to_another_plugin_says_nothing_about_this_one() {
        let spec = clip_spec();
        let peers = [peer_granted(
            "a",
            &[("other", &[(HOST, PEERS_SEND_CAPABILITY)])],
        )];
        assert_eq!(device_grants(&spec, &peers), set(&[]));
    }

    #[test]
    fn only_capabilities_the_plugin_really_asks_the_application_for_are_carried() {
        // It declared and was granted a capability, but never imported anything guarded by it.
        let mut spec = plugin_wanting_peers("clip", &["list"]);
        spec.manifest
            .capabilities
            .insert("clip.private".to_owned(), "Keeps private notes.".to_owned());
        spec.manifest.capabilities.insert(
            PEERS_SEND_CAPABILITY.to_owned(),
            "Sends data to a device.".to_owned(),
        );
        let peers = [peer_granted(
            "a",
            &[(
                "clip",
                &[("clip", "clip.private"), (HOST, PEERS_SEND_CAPABILITY)],
            )],
        )];
        assert_eq!(
            device_grants(&spec, &peers),
            set(&[]),
            "a plugin that only lists must not hold the capability that guards sending"
        );
    }

    #[test]
    fn withdrawing_one_devices_grant_keeps_what_another_device_gave() {
        let spec = clip_spec();
        let both = [
            peer_granted("a", &[("clip", &[(HOST, PEERS_SEND_CAPABILITY)])]),
            peer_granted("b", &[("clip", &[(HOST, PEERS_SEND_CAPABILITY)])]),
        ];
        assert_eq!(
            device_grants(&spec, &both),
            set(&[(HOST, PEERS_SEND_CAPABILITY)])
        );
        let after = [
            peer_granted("a", &[]),
            peer_granted("b", &[("clip", &[(HOST, PEERS_SEND_CAPABILITY)])]),
        ];
        assert_eq!(
            device_grants(&spec, &after),
            set(&[(HOST, PEERS_SEND_CAPABILITY)])
        );
    }

    #[test]
    fn a_device_that_granted_something_else_does_not_give_the_plugin_send() {
        let spec = clip_spec();
        // The entry for this plugin exists, so it is consulted, but it does not allow sending.
        let peers = [peer_granted("a", &[("clip", &[("clip", "clip.other")])])];
        assert_eq!(
            device_grants(&spec, &peers),
            set(&[]),
            "holding another capability on a device must not carry the one that guards sending"
        );
    }

    /// A device whose every field is recognisable in text, so a leak cannot hide.
    fn recognisable_peer(online: bool) -> Peer {
        Peer {
            id: PeerId("KEY-9f3a77c1d2e4".into()),
            name: "Alex's laptop".into(),
            paired_at: 1_700_000_123,
            last_seen: Some(1_700_999_456),
            online,
            sleeping: false,
            statuses: Vec::new(),
            grants: [(
                PluginId::new("clip").unwrap(),
                Grants::new([permission(HOST, PEERS_SEND_CAPABILITY)]).unwrap(),
            )]
            .into(),
            plugin_values: Default::default(),
        }
    }

    #[test]
    fn the_listing_names_a_device_and_says_if_it_is_online_and_nothing_else() {
        let aliases = crate::aliases::Aliases::new();
        let clip = PluginId::new("clip").unwrap();
        let listing = peers_listing(&aliases, &clip, &[recognisable_peer(true)]);
        let text = listing.to_string();
        for secret in [
            "KEY-9f3a77c1d2e4",
            "9f3a77c1d2e4",
            "1700000123",
            "1700999456",
            "peers.send",
            "paired_at",
            "last_seen",
            "grants",
        ] {
            assert!(!text.contains(secret), "the listing leaks {secret}: {text}");
        }
        let entries = listing.as_array().expect("a list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "Alex's laptop");
        assert_eq!(entries[0]["online"], true);
        // Exactly three things: how to name it, what to call it, whether it is reachable.
        assert_eq!(entries[0].as_object().unwrap().len(), 3, "{text}");
    }

    #[test]
    fn the_alias_in_the_listing_is_the_one_send_will_resolve() {
        let aliases = crate::aliases::Aliases::new();
        let clip = PluginId::new("clip").unwrap();
        let peer = recognisable_peer(true);
        let listing = peers_listing(&aliases, &clip, std::slice::from_ref(&peer));
        let given = listing[0]["device"].as_str().expect("an alias").to_owned();
        assert_eq!(
            aliases.resolve(&clip, &given, [&peer.id]),
            Some(&peer.id),
            "what list hands out must be what send accepts"
        );
    }

    #[test]
    fn an_offline_device_is_listed_as_offline_not_hidden() {
        let aliases = crate::aliases::Aliases::new();
        let clip = PluginId::new("clip").unwrap();
        let listing = peers_listing(&aliases, &clip, &[recognisable_peer(false)]);
        assert_eq!(listing[0]["online"], false);
    }

    #[test]
    fn no_paired_devices_is_an_empty_list_not_an_error() {
        let aliases = crate::aliases::Aliases::new();
        let clip = PluginId::new("clip").unwrap();
        assert_eq!(peers_listing(&aliases, &clip, &[]), serde_json::json!([]));
    }

    #[test]
    fn two_plugins_are_given_different_names_for_the_same_device() {
        let aliases = crate::aliases::Aliases::new();
        let peer = recognisable_peer(true);
        let first = peers_listing(
            &aliases,
            &PluginId::new("clip").unwrap(),
            std::slice::from_ref(&peer),
        );
        let second = peers_listing(
            &aliases,
            &PluginId::new("other").unwrap(),
            std::slice::from_ref(&peer),
        );
        assert_ne!(first[0]["device"], second[0]["device"]);
    }

    #[test]
    fn a_plugin_that_answered_ok_has_done_it() {
        use peersus_core::model::ActionState;
        let ok = peersus_plugin_api::message::Outcome::ok(serde_json::json!({"anything": 1}));
        assert_eq!(action_state(&ok), ActionState::Done);
    }

    #[test]
    fn every_code_the_hub_gives_becomes_the_reason_it_stands_for() {
        use peersus_core::model::{ActionFailure::*, ActionState::Failed};
        use peersus_plugin_api::message::Outcome;
        let cases = [
            ("denied", NotAllowed),
            ("refused", NotAllowed),
            ("unavailable", Unavailable),
            ("timeout", TimedOut),
            ("invalid-deadline", TimedOut),
            ("busy", Busy),
            ("unknown-method", Other),
            ("version", Other),
            ("too-large", Other),
        ];
        for (code, reason) in cases {
            assert_eq!(
                action_state(&Outcome::error(code, "x")),
                Failed { reason },
                "{code}"
            );
        }
    }

    #[test]
    fn what_a_plugin_writes_in_its_own_words_never_becomes_part_of_the_state() {
        use peersus_core::model::{ActionFailure, ActionState};
        use peersus_plugin_api::message::Outcome;
        // Codes and messages are the plugin's own text. None of it is a code the application
        // knows, so all of it reads the same and none of it is carried.
        for code in [
            "disk-full",
            "/home/alex/secret.txt",
            "",
            "DENIED",
            "denied ",
            "timeout\n",
        ] {
            let state = action_state(&Outcome::error(code, "the password is hunter2"));
            assert_eq!(
                state,
                ActionState::Failed {
                    reason: ActionFailure::Other
                },
                "{code:?}"
            );
            assert!(!format!("{state:?}").contains("hunter2"));
        }
    }

    #[test]
    fn the_message_of_an_error_makes_no_difference_to_its_state() {
        use peersus_plugin_api::message::Outcome;
        assert_eq!(
            action_state(&Outcome::error("denied", "first wording")),
            action_state(&Outcome::error("denied", "completely different words")),
        );
    }

    #[test]
    fn a_failed_call_is_told_in_the_applications_own_words_by_the_reason_and_never_in_the_plugins()
    {
        use peersus_plugin_api::message::Outcome;
        let told =
            |code: &str| action_failure_sentence(&Outcome::error(code, "the password is hunter2"));
        assert_eq!(
            told("denied"),
            Some("A plugin was not allowed to complete the action")
        );
        assert_eq!(told("refused"), told("denied"));
        assert_eq!(
            told("timeout"),
            Some("A plugin did not complete the action in time")
        );
        assert_eq!(told("invalid-deadline"), told("timeout"));
        assert!(told("unavailable").unwrap().contains("not available"));
        assert!(told("busy").unwrap().contains("busy"));
        // A code nobody knows, in any spelling or shape, is the plain sentence: nothing it holds is in what is shown.
        for code in [
            "disk-full",
            "/home/alex/secret.txt",
            "",
            "DENIED",
            "timeout\n",
        ] {
            let sentence = told(code).unwrap();
            assert_eq!(
                sentence, "A plugin could not complete the action",
                "{code:?}"
            );
            assert!(!sentence.contains("hunter2") && !sentence.contains("secret"));
        }
    }

    #[test]
    fn a_call_that_was_cancelled_or_went_well_has_no_failure_to_tell() {
        use peersus_plugin_api::message::Outcome;
        assert_eq!(
            action_failure_sentence(&Outcome::error("cancelled", "x")),
            None
        );
        assert_eq!(
            action_failure_sentence(&Outcome::ok(serde_json::json!({}))),
            None
        );
    }

    /// A device `key` that `plugin` was granted `send` for, online or not.
    fn sendable(key: &str, plugin: &str, online: bool) -> Peer {
        let mut peer = peer_granted(key, &[(plugin, &[(HOST, PEERS_SEND_CAPABILITY)])]);
        peer.online = online;
        peer
    }

    /// A device that granted `plugin` exactly `permission`, online.
    fn granted_exactly(key: &str, plugin: &str, owner: &str, name: &str) -> Peer {
        peer_granted(key, &[(plugin, &[(owner, name)])])
    }

    #[test]
    fn sending_needs_the_applications_permission_and_not_a_same_named_one_of_the_plugin() {
        let aliases = crate::aliases::Aliases::new();
        // `send` is guarded by the APPLICATION's `peers.send`. A device that granted this
        // plugin the application's permission may be sent to.
        let app = granted_exactly("a", "clip", HOST, PEERS_SEND_CAPABILITY);
        let alias = aliases.alias(&clip(), &app.id);
        assert!(
            authorize_send(&aliases, &clip(), &alias, std::slice::from_ref(&app)).is_ok(),
            "a grant of the application's own permission was refused"
        );
        // A permission of the PLUGIN with the same name is another permission, with whatever
        // meaning the plugin gave it: it must not let anything be sent.
        let own = granted_exactly("b", "clip", "clip", PEERS_SEND_CAPABILITY);
        let alias = aliases.alias(&clip(), &own.id);
        assert!(
            authorize_send(&aliases, &clip(), &alias, std::slice::from_ref(&own)).is_err(),
            "a same-named permission of the plugin itself opened the application's send"
        );
    }

    fn clip() -> PluginId {
        PluginId::new("clip").unwrap()
    }

    #[test]
    fn a_send_goes_to_the_device_that_is_granted_online_and_named_by_its_alias() {
        let aliases = crate::aliases::Aliases::new();
        let peers = [sendable("a", "clip", true), sendable("b", "clip", true)];
        let alias = aliases.alias(&clip(), &peers[1].id);
        assert_eq!(
            authorize_send(&aliases, &clip(), &alias, &peers).map(|p| p.id.clone()),
            Ok(peers[1].id.clone())
        );
    }

    #[test]
    fn a_send_is_refused_unless_this_device_granted_this_plugin() {
        let aliases = crate::aliases::Aliases::new();
        let ungranted = peer_granted("a", &[]);
        let other_capability = peer_granted("b", &[("clip", &[("clip", "clip.other")])]);
        let other_plugin = peer_granted("c", &[("other", &[(HOST, PEERS_SEND_CAPABILITY)])]);
        for peer in [ungranted, other_capability, other_plugin] {
            let alias = aliases.alias(&clip(), &peer.id);
            assert!(
                authorize_send(&aliases, &clip(), &alias, std::slice::from_ref(&peer)).is_err(),
                "device {} must not be sendable to",
                peer.id.0
            );
        }
    }

    #[test]
    fn a_grant_on_one_device_does_not_open_another() {
        let aliases = crate::aliases::Aliases::new();
        let peers = [sendable("a", "clip", true), peer_granted("b", &[])];
        let alias_of_b = aliases.alias(&clip(), &peers[1].id);
        assert!(authorize_send(&aliases, &clip(), &alias_of_b, &peers).is_err());
    }

    #[test]
    fn a_send_to_a_device_that_is_not_online_is_refused() {
        let aliases = crate::aliases::Aliases::new();
        let peers = [sendable("a", "clip", false)];
        let alias = aliases.alias(&clip(), &peers[0].id);
        assert!(authorize_send(&aliases, &clip(), &alias, &peers).is_err());
    }

    #[test]
    fn an_alias_that_means_nothing_is_refused() {
        let aliases = crate::aliases::Aliases::new();
        let peers = [sendable("a", "clip", true)];
        for guess in ["", "a", "d1", "0000"] {
            assert!(authorize_send(&aliases, &clip(), guess, &peers).is_err());
        }
    }

    #[test]
    fn another_plugin_cannot_use_an_alias_it_was_not_given() {
        let aliases = crate::aliases::Aliases::new();
        let peer = sendable("a", "other", true);
        // The alias `clip` was given for this device; `other` is granted the device.
        let clips_alias = aliases.alias(&clip(), &peer.id);
        assert!(
            authorize_send(
                &aliases,
                &PluginId::new("other").unwrap(),
                &clips_alias,
                std::slice::from_ref(&peer)
            )
            .is_err()
        );
    }

    #[test]
    fn every_refusal_reads_the_same_so_a_plugin_cannot_probe_which_devices_exist() {
        let aliases = crate::aliases::Aliases::new();
        let granted_offline = sendable("a", "clip", false);
        let ungranted_online = peer_granted("b", &[]);
        let peers = [granted_offline.clone(), ungranted_online.clone()];
        let refusals: BTreeSet<_> = [
            authorize_send(
                &aliases,
                &clip(),
                &aliases.alias(&clip(), &granted_offline.id),
                &peers,
            ),
            authorize_send(
                &aliases,
                &clip(),
                &aliases.alias(&clip(), &ungranted_online.id),
                &peers,
            ),
            authorize_send(&aliases, &clip(), "never-an-alias", &peers),
        ]
        .into_iter()
        .map(|result| result.expect_err("all three are refused"))
        .collect();
        assert_eq!(
            refusals.len(),
            1,
            "different refusals would tell a plugin what exists: {refusals:?}"
        );
    }

    #[test]
    fn a_well_formed_send_names_a_device_a_service_and_a_version() {
        let request = parse_send(&serde_json::json!({
            "device": "abc123",
            "service": "sink.service",
            "version": 3,
        }))
        .expect("a complete request");
        assert_eq!(request.device, "abc123");
        assert_eq!(request.service, "sink.service");
        assert_eq!(request.version, 3);
    }

    #[test]
    fn a_send_that_is_not_exactly_those_three_things_is_refused() {
        let refused = [
            serde_json::Value::Null,
            serde_json::json!("abc"),
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!({"service": "s", "version": 1}),
            serde_json::json!({"device": "d", "version": 1}),
            serde_json::json!({"device": "d", "service": "s"}),
            serde_json::json!({"device": 1, "service": "s", "version": 1}),
            serde_json::json!({"device": "d", "service": 1, "version": 1}),
            serde_json::json!({"device": "d", "service": "s", "version": "1"}),
            serde_json::json!({"device": "d", "service": "s", "version": -1}),
            serde_json::json!({"device": "d", "service": "s", "version": 1.5}),
            // A fourth field is not silently ignored: the plugin is asking for something this
            // application does not offer.
            serde_json::json!({"device": "d", "service": "s", "version": 1, "limit": 9}),
        ];
        for args in refused {
            assert_eq!(
                parse_send(&args).err(),
                Some(SEND_REFUSED),
                "{args} must be refused"
            );
        }
    }

    #[test]
    fn a_send_for_a_service_name_the_wire_would_reject_is_refused_before_anything_is_reserved() {
        for service in ["", "has space", "slash/ed", "ünï", &"a".repeat(129)] {
            let args = serde_json::json!({"device": "d", "service": service, "version": 1});
            assert_eq!(parse_send(&args).err(), Some(SEND_REFUSED), "{service:?}");
        }
        let zero = serde_json::json!({"device": "d", "service": "s", "version": 0});
        assert_eq!(parse_send(&zero).err(), Some(SEND_REFUSED));
        let longest = "a".repeat(128);
        let ok = serde_json::json!({"device": "d", "service": longest, "version": 1});
        assert!(
            parse_send(&ok).is_ok(),
            "128 characters is the limit, not 127"
        );
    }

    #[test]
    fn every_malformed_send_reads_the_same_as_an_unknown_device() {
        // One wording for everything a plugin can get wrong, so that no refusal tells it
        // more than "no".
        let aliases = crate::aliases::Aliases::new();
        let peers = [peer_granted("a", &[])];
        let unknown_device =
            authorize_send(&aliases, &clip(), "nobody", &peers).expect_err("refused");
        let malformed = parse_send(&serde_json::Value::Null).expect_err("refused");
        assert_eq!(unknown_device, malformed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_step_that_never_finishes_ends_at_its_limit_and_not_before() {
        let started = tokio::time::Instant::now();
        // The outer ceiling is only a safety net for the test itself: without it, a `within`
        // that ignored its limit would make this test hang instead of fail.
        let outcome = tokio::time::timeout(
            Duration::from_secs(60),
            within(Duration::from_secs(10), std::future::pending::<u8>()),
        )
        .await
        .expect("within waited past its limit instead of giving up at it");
        assert_eq!(outcome, None);
        assert_eq!(started.elapsed(), Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn a_step_that_finishes_in_time_is_not_cut_short() {
        let outcome = within(Duration::from_secs(10), async {
            tokio::time::sleep(Duration::from_secs(9)).await;
            7u8
        })
        .await;
        assert_eq!(outcome, Some(7));
    }

    #[tokio::test(start_paused = true)]
    async fn a_step_that_is_already_done_costs_no_waiting() {
        let started = tokio::time::Instant::now();
        assert_eq!(
            within(Duration::from_secs(10), async { 1u8 }).await,
            Some(1)
        );
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[test]
    fn what_a_plugin_is_told_about_a_device_is_the_alias_it_was_given_never_the_key() {
        let aliases = crate::aliases::Aliases::new();
        let clip = PluginId::new("clip").unwrap();
        let device = PeerId("KEY-9f3a77c1d2e4".into());
        let told = peer_argument(&aliases, &clip, &device);
        let text = told.to_string();
        assert!(!text.contains("KEY-9f3a77c1d2e4"), "the key leaked: {text}");
        assert!(
            !text.contains("9f3a77c1d2e4"),
            "part of the key leaked: {text}"
        );
        assert_eq!(told["peer"], aliases.alias(&clip, &device));
        assert_eq!(told.as_object().unwrap().len(), 1, "{text}");
    }

    #[test]
    fn the_device_a_plugin_is_told_of_is_one_it_can_send_to() {
        // The one name has to work for everything: what an action or a receive hands over is
        // what `send` takes back, and what `list` gave.
        let aliases = crate::aliases::Aliases::new();
        let clip = PluginId::new("clip").unwrap();
        let peers = [sendable("a", "clip", true), sendable("b", "clip", true)];
        let told = peer_argument(&aliases, &clip, &peers[1].id);
        let alias = told["peer"].as_str().unwrap();
        assert_eq!(
            authorize_send(&aliases, &clip, alias, &peers).map(|p| p.id.clone()),
            Ok(peers[1].id.clone())
        );
        let listed = peers_listing(&aliases, &clip, &peers);
        assert!(
            listed
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["device"] == told["peer"]),
            "list and the argument must name a device the same way"
        );
    }

    #[test]
    fn two_plugins_are_told_different_names_for_the_same_device() {
        let aliases = crate::aliases::Aliases::new();
        let device = PeerId("shared".into());
        let first = peer_argument(&aliases, &PluginId::new("clip").unwrap(), &device);
        let second = peer_argument(&aliases, &PluginId::new("other").unwrap(), &device);
        assert_ne!(first["peer"], second["peer"]);
    }

    /// A device that stays connected but never lets another stream be opened to it: the very
    /// case the limit on opening exists for, on a real connection, going through the real
    /// `carry_out` rather than the helper alone.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_device_that_gives_no_stream_credit_ends_the_send_at_the_limit_and_frees_the_plugin()
    {
        use iroh::{
            SecretKey,
            endpoint::{Builder, QuicTransportConfig, VarInt, presets},
        };
        use tokio::io::AsyncReadExt;
        let policy = peersus_core::config::NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..peersus_core::config::NetworkSettings::default()
        };
        let application = peersus_net::transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        // The device allows the application no stream of its own to open.
        let device = Builder::new(presets::Minimal)
            .secret_key(SecretKey::generate())
            .alpns(vec![peersus_net::transport::ALPN.to_vec()])
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_bidi_streams(VarInt::from_u32(0))
                    .build(),
            )
            .clear_ip_transports()
            .bind_addr(std::net::SocketAddr::new("127.0.0.1".parse().unwrap(), 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let (outgoing, incoming) = tokio::join!(
            application.connect(device.addr(), peersus_net::transport::ALPN),
            async { device.accept().await.unwrap().await }
        );
        let connection = outgoing.unwrap();
        let _device_side = incoming.unwrap();

        // The plugin has already come: its end of the data connection is handed over.
        let (plugin_end, application_end) = tokio::net::UnixStream::pair().unwrap();
        let (deliver, receiver) = oneshot::channel();
        deliver.send(application_end).unwrap();
        let request = SendRequest {
            device: "unused".into(),
            service: "sink.service".into(),
            version: 1,
        };

        let started = std::time::Instant::now();
        // The outer ceiling is only a safety net for the test: without the limit under test
        // this would wait for ever.
        let outcome = tokio::time::timeout(
            Duration::from_secs(30),
            carry_out(connection, request, receiver, Duration::from_millis(600)),
        )
        .await
        .expect("opening a stream waited for ever instead of giving up at its limit");
        assert_eq!(
            outcome,
            crate::receive::Delivery::Failed("A device could not be reached to send to it")
        );
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(500),
            "gave up after {waited:?}, before the limit"
        );
        assert!(
            waited < Duration::from_secs(10),
            "took {waited:?}, far past the limit"
        );

        // And the plugin is freed: its end of the connection is closed, so it is told.
        let mut plugin_end = plugin_end;
        let mut byte = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(5), plugin_end.read(&mut byte))
            .await
            .expect("the plugin's socket was left open");
        assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    }

    #[test]
    fn configuration_returns_only_declared_caller_values_and_scoped_aliases() {
        use peersus_core::plugins::{Value, Values};
        use peersus_plugin_api::contributions::{Kind, Scope, Setting};
        let mut spec = plugin_importing("clip", HOST, CONFIG_SERVICE, 1, &["get"]);
        spec.manifest.contributions.settings = vec![
            Setting {
                id: "automatic".into(),
                label: "Automatic".into(),
                help: None,
                scope: Scope::Global,
                kind: Kind::Toggle { default: false },
            },
            Setting {
                id: "sync".into(),
                label: "Sync".into(),
                help: None,
                scope: Scope::Device,
                kind: Kind::Toggle { default: false },
            },
        ];
        let saved: Values = serde_json::from_value(
            serde_json::json!({"automatic":"invalid", "undeclared":"private"}),
        )
        .unwrap();
        let mut peer = peer_granted("actual-network-key", &[]);
        let mut device = Values::default();
        device
            .set(
                &spec.manifest.contributions.settings,
                Scope::Device,
                "sync",
                Value::Toggle(true),
            )
            .unwrap();
        peer.plugin_values.insert(spec.id.clone(), device);
        let foreign = PluginId::new("foreign").unwrap();
        peer.plugin_values.insert(
            foreign.clone(),
            serde_json::from_value(serde_json::json!({"private":"do not expose"})).unwrap(),
        );
        let aliases = crate::aliases::Aliases::new();
        let answer = configuration(&spec, Some(&saved), &aliases, &[peer.clone()]);
        assert_eq!(
            answer,
            serde_json::json!({
                "plugin": spec.id.as_str(),
                "settings": {"automatic": false},
                "peers": [{"device": aliases.alias(&spec.id, &peer.id), "online": peer.online, "settings": {"sync":true}, "permissions":[]}]
            })
        );
        assert_ne!(
            answer["peers"][0]["device"],
            aliases.alias(&foreign, &peer.id)
        );
        assert!(!answer.to_string().contains("actual-network-key"));
        assert!(!answer.to_string().contains("private"));
    }

    #[test]
    fn configuration_import_does_not_grant_send_or_expose_other_consumers() {
        let mut registry = Registry::default();
        let spec = plugin_importing("clip", HOST, CONFIG_SERVICE, 1, &["get"]);
        registry.register(spec.clone()).unwrap();
        registry.register(plugin_with_guarded_action()).unwrap();
        let exports = host_exports(&registry);
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].consumers, ["clip".to_owned()].into());
        assert_eq!(
            exports[0]
                .methods
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["get"]
        );
        assert!(grantable(&spec).is_empty());
        // Configuration is information, never authorization: the per-device send guard
        // is still rechecked by the same path explicit share calls use.
        let sending = clip_spec();
        let peer = peer_granted("device", &[]);
        let aliases = crate::aliases::Aliases::new();
        assert!(
            authorize_send(
                &aliases,
                &sending.id,
                &aliases.alias(&sending.id, &peer.id),
                &[peer]
            )
            .is_err()
        );
    }

    #[test]
    fn a_revoked_service_guard_blocks_a_new_send_even_with_stale_configuration() {
        let spec = plugin_that_receives(Some("sink.write"));
        let allowed = peer_granted(
            "device",
            &[(
                "sink",
                &[(HOST, PEERS_SEND_CAPABILITY), ("sink", "sink.write")],
            )],
        );
        assert!(
            authorize_service_send(&spec, &allowed, &spec.manifest.exports[0].service, 1).is_ok()
        );
        let revoked = peer_granted("device", &[("sink", &[(HOST, PEERS_SEND_CAPABILITY)])]);
        let aliases = crate::aliases::Aliases::new();
        let alias = aliases.alias(&spec.id, &revoked.id);
        assert!(authorize_send(&aliases, &spec.id, &alias, std::slice::from_ref(&revoked)).is_ok());
        assert!(
            authorize_service_send(&spec, &revoked, &spec.manifest.exports[0].service, 1).is_err()
        );
        assert!(authorize_service_send(&spec, &revoked, "an.unrelated.service", 1).is_ok());
    }
    #[test]
    fn platform_facilities_require_explicit_grants_for_each_new_call() {
        use crate::platform_services;
        use peersus_plugin_api::message::Outcome;
        for &(service, method, capability, _) in platform_services::METHODS {
            let spec = plugin_importing("upload", HOST, service, 1, &[method]);
            let grant = permission(HOST, capability);
            assert!(grantable_without_a_device(&spec).contains(&grant));
            assert!(!grantable_for_a_device(&spec).contains(&grant));
            let mut registry = Registry::default();
            registry.register(spec.clone()).unwrap();
            registry
                .register(plugin_wanting_peers("unrelated", &["list"]))
                .unwrap();
            let manifest = host_manifest(&registry);
            let export = manifest
                .exports
                .iter()
                .find(|export| export.service == service)
                .unwrap();
            assert_eq!(export.consumers, ["upload".to_owned()].into());
            let platform_timeouts = platform_services::method_timeouts(&manifest);
            let hub = Hub::default();
            let generation = hub.register_host(HOST, manifest, BTreeSet::new()).unwrap();
            let host = hub.connect(HOST, generation).unwrap();
            host.set_host_method_timeouts(platform_timeouts).unwrap();
            let generation = hub
                .register("upload", spec.manifest, BTreeSet::new())
                .unwrap();
            let guest = hub.connect("upload", generation).unwrap();
            let call = |id| Operation::Call {
                id,
                provider: HOST.to_owned(),
                service: service.to_owned(),
                version: 1,
                method: method.to_owned(),
                args: serde_json::json!({}),
                timeout_ms: 600_000,
            };
            assert_eq!(
                guest.submit(call(1)),
                Err(peersus_plugin_api::error::Error::Denied)
            );
            assert!(host.try_recv().unwrap().is_none());
            hub.set_permissions("upload", [grant].into()).unwrap();
            guest.submit(call(2)).unwrap();
            let Some(Delivery::Call { ticket, caller, .. }) = host.try_recv().unwrap() else {
                panic!("granted call did not reach the host");
            };
            assert_eq!(caller, "upload");
            hub.set_permissions("upload", BTreeSet::new()).unwrap();
            assert_eq!(
                guest.submit(call(3)),
                Err(peersus_plugin_api::error::Error::Denied)
            );
            // The broker preserves accepted-call commitments. Sensitive platform work
            // is cancelled by the owner's PlatformCalls, not by changing that contract.
            assert!(host.try_recv().unwrap().is_none());
            host.submit(Operation::Reply {
                ticket,
                result: Outcome::ok(serde_json::json!({})),
            })
            .unwrap();
            assert!(matches!(
                guest.try_recv().unwrap(),
                Some(Delivery::Result { id: 2, .. })
            ));
        }
    }
}

#[cfg(test)]
mod cancelled_action_regression {
    #[test]
    fn explicit_plugin_cancellation_is_not_a_failure() {
        assert_eq!(
            super::action_state(&peersus_plugin_api::message::Outcome::error(
                "cancelled",
                "ignored"
            )),
            peersus_core::model::ActionState::Cancelled
        );
    }
}
