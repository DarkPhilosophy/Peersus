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
use peersus_api::{contributions::Scope, permission::Permission};
use peersus_core::{
    config::Language,
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
use peersus_plugin::hub::{Channel, HOST, Hub};
use peersus_plugin::{Accepted, Event as PluginEvent, Plugin, Service};
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
pub(crate) type Outbound = oneshot::Sender<peersus_plugin::transport::Stream>;

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
async fn carry_out(
    connection: iroh::endpoint::Connection,
    request: SendRequest,
    plugin: oneshot::Receiver<peersus_plugin::transport::Stream>,
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
    let (read, mut acknowledgement) = plugin.into_split();
    let outcome = crate::receive::send_out(
        read,
        send,
        crate::receive::DEFAULT_LIMIT,
        peersus_plugin::pump::IDLE_LIMIT,
    )
    .await;
    if let Delivery::Done(bytes) = outcome {
        // A plugin's half-close only proves its local socket accepted the bytes.
        // Confirm the bounded remote pump completed before it may report success.
        let _ = within(
            peersus_plugin::data::HANDSHAKE_TIMEOUT,
            peersus_plugin::frame::write(&mut acknowledgement, &bytes, 1024),
        )
        .await;
    }
    outcome
}

/// Said when this owner was given no place to keep languages, or no way to fetch them.
const NO_LANGUAGES: &str = "Languages are not available here";

/// Invitations are deliberately not Debug: their short-lived bearer is private.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum Reply {
    Applied,
    Invitation(String),
    /// A window is open, so devices nearby are listened for; and what this device announces to them, if the page
    /// that window shows lets it be found: the invitation as text, and what it is for.
    Nearby(Option<(String, peersus_core::nearby::Takes)>),
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
    /// The text of the language file the windows are shown in.
    LanguageText(String),
}
pub(crate) struct Request {
    pub(crate) command: Command,
    pub(crate) result: oneshot::Sender<Result<Reply, &'static str>>,
}

#[derive(Clone)]
pub struct Client {
    pub(crate) requests: mpsc::Sender<Request>,
    pub(crate) snapshots: watch::Receiver<Arc<Snapshot>>,
    /// Says each surface whose input just failed to reach its extension (see [`Client::surface_failures`]). Read only
    /// where this application feeds a surface from the desktop's own input, which is Linux today.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) surface_failures: tokio::sync::broadcast::Sender<crate::surfaces::SurfaceKey>,
    /// Set once a command to stop has been given through any copy of this client (see [`Client::told_to_stop`]).
    pub(crate) told_to_stop: Arc<std::sync::atomic::AtomicBool>,
}
impl Client {
    /// Bounded channel; GUI callers spawn this future rather than block rendering.
    pub async fn command(&self, command: Command) -> Result<Reply, &'static str> {
        if matches!(command, Command::Shutdown {}) {
            self.told_to_stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
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
    /// Whether this application was told to stop (`peersus stop`, a signal it honours, its last window going): what
    /// tells an owner that ends because it was asked to from one that ends in any other way. The windows are told so
    /// on their leases (see `ipc::TOLD`), and close instead of starting the application again.
    pub fn told_to_stop(&self) -> bool {
        self.told_to_stop.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// Each surface whose input did not reach its extension, said when that is known, which is later
    /// than the command that carried it was accepted. Whoever feeds a surface listens, so it stops
    /// feeding one that leads nowhere: a pointer taken from this desktop is given back.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn surface_failures(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::surfaces::SurfaceKey> {
        self.surface_failures.subscribe()
    }
}

/// What every command a read-only demo window issues is answered with: the demo shows but
/// never acts, and nothing it does reaches an owner, a file or the network.
#[cfg(feature = "demo")]
pub const DEMO_READ_ONLY: &str = "Read-only demo: this window is not connected to an owner";

#[cfg(feature = "demo")]
impl Client {
    /// A client that serves one snapshot, unchanged for as long as it lives, and answers
    /// every command with [`DEMO_READ_ONLY`]. For the interface's read-only demo: no owner,
    /// no socket, nothing held but the picture of what the interface would show.
    ///
    /// The snapshot's sender is kept inside the task that answers commands: while the client
    /// lives, `subscribe()` waits for a change that never comes instead of reporting the
    /// owner gone. When the last client clone is gone the task's channel closes, the task
    /// ends and the sender is let go.
    pub fn fixture(snapshot: Arc<Snapshot>, executor: &tokio::runtime::Handle) -> Self {
        Self::replaying(snapshot, Vec::new(), Duration::ZERO, executor)
    }

    /// The same read-only client, serving `first` and then each of `then`, a new one every
    /// `every`, round and round: a window with work to do, for measuring what a changing
    /// snapshot costs it. The frames are the caller's synthetic ones; nothing is read from
    /// anywhere.
    ///
    /// With nothing in `then`, or a zero `every`, this is [`Client::fixture`]: `first`,
    /// unchanged for as long as the client lives.
    pub fn replaying(
        first: Arc<Snapshot>,
        then: Vec<Arc<Snapshot>>,
        every: Duration,
        executor: &tokio::runtime::Handle,
    ) -> Self {
        let (requests, mut inbox) = mpsc::channel::<Request>(REQUESTS_WAITING);
        let (snapshots, receiver) = watch::channel(Arc::clone(&first));
        let moves = !then.is_empty() && !every.is_zero();
        executor.spawn(async move {
            // Keeping `snapshots` alive is the point of the task as much as answering is:
            // without it the channel would look like an owner that went away.
            let mut next = then.into_iter().chain(std::iter::once(first)).cycle();
            let mut turn = tokio::time::interval_at(
                tokio::time::Instant::now() + every,
                every.max(Duration::from_millis(1)),
            );
            // A window too busy to keep up is shown the next frame late, never a burst of them.
            turn.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    request = inbox.recv() => match request {
                        Some(request) => {
                            let _ = request.result.send(Err(DEMO_READ_ONLY));
                        }
                        None => break,
                    },
                    _ = turn.tick(), if moves => {
                        if let Some(frame) = next.next() {
                            snapshots.send_replace(frame);
                        }
                    }
                }
            }
            drop(snapshots);
        });
        Client {
            requests,
            snapshots: receiver,
            surface_failures: tokio::sync::broadcast::channel(1).0,
            told_to_stop: Arc::default(),
        }
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
    surface_failures: tokio::sync::broadcast::Sender<crate::surfaces::SurfaceKey>,
    errors: Vec<String>,
    /// Every time something was said, including a repeat the list already ends with (see `Snapshot::errors_said`).
    errors_said: u64,
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
    launchers: HashMap<PluginId, Arc<dyn peersus_plugin::supervisor::Launcher>>,
    modules: Option<Arc<dyn peersus_plugin::module::ModuleFactory>>,
    /// Where the personal relay's access token is kept between runs, when this platform has such a place.
    token_store: Option<Arc<dyn TokenStore>>,
    /// Platform UI and I/O are awaited in separately owned, cancellable tasks.
    platform_services: Option<Arc<dyn crate::platform_services::PlatformServices>>,
    platform_calls: crate::platform_calls::PlatformCalls,
    /// Which platform service changed, aggregated from every service the platform can watch. Carries no contents.
    platform_changes: Option<tokio::sync::mpsc::Receiver<&'static str>>,
    extension_prompts: Option<Arc<crate::extension_prompts::ExtensionPrompts>>,
    extension_prompt_changes: Option<watch::Receiver<u64>>,
    /// The notifications on show, the application's own and extensions'.
    notifications: Arc<crate::notifications::Notifications>,
    notification_changes: watch::Receiver<u64>,
    action_progress_changes: Option<watch::Receiver<crate::platform_services::ActionProgress>>,
    /// Ids of calls the application makes through the hub. Never reused, never zero.
    next_call: u64,
    /// Calls to plugins that have not ended yet, to refuse a repeat and to withdraw them.
    in_flight: InFlight,
    /// Input from pages' surfaces on its way to plugins, one call at a time per surface.
    surfaces: crate::surfaces::Surfaces,
    /// What extensions show on their own pages, and the events on their way to them.
    page_views: crate::page_views::PageViews,
    /// What the platform's Bluetooth adapter last said of nearness (see `nearby`); told to windows for paired devices only.
    nearby: peersus_core::nearby::Found,
    /// The invitation each device nearby that is not paired announced, as text. Never told to a window.
    nearby_offers: Vec<(PeerId, String)>,
    /// When a window last said it is open (see [`Command::SeekNearby`]), and what the page it shows lets this device
    /// be found for.
    nearby_sought: Option<std::time::Instant>,
    nearby_takes: Option<peersus_core::nearby::Takes>,
    /// When a window last said a person sets how near "near" is (see [`Command::MeasureNearby`]).
    nearby_measured: Option<std::time::Instant>,
    /// The invitation this device announces to devices nearby, as text, when it runs out, and what it is for.
    nearby_offer: Option<(String, u64, peersus_core::nearby::Takes)>,
    /// Caller-owned additional placements of pages already declared in its manifest.
    page_offers: crate::page_offers::PageOffers,
    /// The plugin each `receive` call the application made is for, by call: so that how it ended is said of that plugin.
    receiving: HashMap<u64, PluginId>,
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
    /// What this device was opened for while it keeps to itself (the `private_mode` preference): who may send to which
    /// device, and since when each was last at work. Empty and unused while the mode is off.
    permits: crate::permits::Permits,
    /// The statuses plugins made for this device and which of them are on. Belongs to the plugin processes: it is emptied for a plugin when
    /// that plugin stops or starts again, so that nothing keeps saying what nobody keeps true.
    statuses: crate::statuses::Statuses,
    /// What each plugin calls each device. A secret that lives only in this run.
    aliases: crate::aliases::Aliases,
    /// The store of installed packages, when this owner was given one. Without it the commands
    /// that install and remove are refused, and nothing else about the owner changes.
    packages: Option<crate::plugin_manager::Manager>,
    /// What THIS person accepted of each installed extension's licences and notice: the digest of the agreement, by
    /// extension. Read from the person's own data directory when the store is attached, and kept in step with that file
    /// ([`crate::plugin_agreements`]). An extension learns only whether it holds (`extension.tos.accepted`).
    accepted_agreements: std::collections::BTreeMap<String, String>,
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
    installed_packages: std::collections::BTreeMap<PluginId, peersus_api::package::Package>,
    /// Extensions an automatic update was already tried for since the catalog was last read, so a
    /// failing update is reported once per refresh instead of being retried in a loop.
    auto_update_tried: std::collections::BTreeSet<PluginId>,
    /// The extension store, when this owner was given one. Without it the store commands are refused
    /// and the window is told the store is not set up. Kept populated while an install runs: a
    /// snapshot published meanwhile must carry its real state.
    store: Option<StoreSlot>,
    /// The last entry a store link asked every window to show (`Command::ShowStoreEntry`).
    store_asked: Option<peersus_core::model::StoreAsked>,
    #[cfg(feature = "private-store")]
    private_store: Option<Arc<dyn crate::private_store::Platform>>,
    /// Store downloads in flight (at most one: the service refuses a second). Each returns only
    /// unverified bytes, which the event loop verifies and installs.
    store_tasks: tokio::task::JoinSet<StoreOutcome>,
    /// Updates of the application itself, when this owner was given a way to apply them. Without it
    /// the update commands are refused and the window is told updates are not set up.
    update: Option<UpdateSlot>,
    /// The one step of an application update in flight: a check, a download or the installer.
    update_tasks: tokio::task::JoinSet<UpdateOutcome>,
    /// When the owner ends so that an installed update runs. A moment after it was decided, so that
    /// every window has been told why the owner goes before it goes.
    restart: Option<tokio::time::Instant>,
    /// The application's languages, when this owner was given a place to keep them. Without it every window is shown in
    /// English and the language commands are refused.
    languages: Option<crate::languages::Languages>,
    /// The one fetch of languages in flight. It returns only unchecked bytes; they are checked and kept on the loop.
    language_tasks:
        tokio::task::JoinSet<Result<crate::languages::Fetched, crate::catalog::Refusal>>,
}

// What the application offers extensions and how it decides who may use it, and the parts of the running loop by
// concern; the loop itself (`run`, `execute`) stays here.
mod admission;
mod host_contract;
mod outcomes;
mod packages;
mod permissions;
mod plugin_calls;
mod privacy;
mod publishing;
mod settings;
mod transfers;
pub(crate) use admission::*;
pub use host_contract::*;
pub(crate) use outcomes::*;
pub use permissions::*;

/// A package the person chose and the owner read, kept until it is confirmed or replaced: where it
/// is, what it asks for, and its digest as it was when it was read, so that confirming can tell
/// whether it is still the package that was shown.
struct Staged {
    /// Whether this package came through the signed store (verified against the maintainer's catalog) and not from a place
    /// the person chose. A package from outside the store is installed only after the person has acknowledged, on the
    /// window's own warning, that nobody vouches for it.
    from_store: bool,
    source: PathBuf,
    manifest: peersus_api::manifest::Manifest,
    digest: String,
    /// When the package came as an archive: what was unpacked from it, which `source` points into.
    /// Dropping this deletes it, so a confirmed, cancelled, replaced or refused package leaves
    /// nothing behind.
    _unpacked: Option<crate::archive::Unpacked>,
    /// What the package says about itself (author, link, version), shown beside what it asks for.
    package: Option<peersus_api::package::Package>,
}

/// The store as the owner holds it: the verified-catalog service and the thing that fetches bytes for it.
struct StoreSlot {
    service: crate::store_service::StoreService,
    fetcher: Arc<dyn crate::store_runtime::StoreFetcher>,
    refresh: crate::store_runtime::RefreshSchedule,
}

/// What a store download task hands back. Only UNVERIFIED bytes: nothing here has been trusted yet.
enum StoreOutcome {
    Refreshed(Result<(Vec<u8>, Vec<u8>), crate::catalog::Refusal>),
    Discovered(Result<crate::store_branch::Index, crate::catalog::Refusal>),
    Downloaded {
        plugin: PluginId,
        bytes: Result<Vec<u8>, crate::catalog::Refusal>,
    },
}

/// Why the owner's loop ended, for whatever started the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// It was asked to stop, or everything that could ask it anything is gone.
    Stopped,
    /// An update was put in place and the application is to start again so that it runs.
    Restart,
}

/// Application updates as the owner holds them: what was verified, and how this installation applies it.
struct UpdateSlot {
    service: crate::app_update::UpdateService,
    installer: Arc<dyn crate::app_update::Installer>,
    schedule: crate::store_runtime::RefreshSchedule,
    /// Whether a person asked for the step in flight. What nobody asked for fails quietly: it is
    /// kept in the update's own state, and no message is raised over whatever the person is doing.
    asked: bool,
    /// The offered build an automatic install was already started for, so that one that fails is
    /// not tried again until something else is offered or the person presses the button.
    tried: Option<String>,
}

/// What one step of an application update hands back. The first two are UNVERIFIED bytes.
enum UpdateOutcome {
    Checked(Result<Option<crate::app_update::Found>, crate::catalog::Refusal>),
    Downloaded(Result<Vec<u8>, crate::catalog::Refusal>),
    Installed(Result<(), &'static str>),
}

/// How often a release is looked for without being asked. A release is rare and the answer is one
/// small signed document, so a few times a day is plenty.
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// How long the windows are given to hear that the owner restarts before it ends.
const RESTART_NOTICE: Duration = Duration::from_millis(750);
/// How many requests of the clients may wait for the owner at once; one more waits to be handed over.
const REQUESTS_WAITING: usize = 64;

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
        // What a process killed while it unpacked an extension left in its scratch folder: this owner is the only one that
        // unpacks there, and it has not started to, so whatever is left is nobody's.
        crate::archive::sweep_leftovers(&plugin_directory);
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
            plugins: plugins.info(grantable_for_window),
            staged: Vec::new(),
            store: peersus_core::model::StoreState {
                status: peersus_core::model::StoreStatus::NotConfigured,
                ..Default::default()
            },
            update: Default::default(),
            actions: Vec::new(),
            transfers: network.transfers(),
            pairing: network.pairing(),
            extension_prompts: Vec::new(),
            notifications: Vec::new(),
            errors: Vec::new(),
            errors_said: 0,
            profile_picture: network
                .own_picture()
                .map(peersus_core::profile::Picture::digest),
            peer_pictures: network.peer_digests(),
            displays: network.own_displays().to_vec(),
            peer_displays: network.peer_displays(),
            files_code: network.files_code(),
            link_tests: network.link_tests(),
            typed_addresses: network.typed_addresses(),
            listening_port: network.listening_port(),
            relay: Some(network.relay_diagnostics()),
            relay_probes: network.relay_probes(),
            page_views: Vec::new(),
            nearby: Default::default(),
            languages: Default::default(),
        }));
        let (commands, requests) = mpsc::channel(REQUESTS_WAITING);
        // Data connections of every plugin arrive on one channel. The application matches
        // each to what it set aside by the number of the call, which is the same whichever
        // plugin it was, so one place consuming them is enough.
        let (handoff, accepted) = mpsc::channel(16);
        let (surface_failures, _) = tokio::sync::broadcast::channel(16);
        let notifications = Arc::new(crate::notifications::Notifications::default());
        Ok((
            Self {
                network,
                standalone,
                settings,
                plugins,
                requests,
                snapshots,
                surface_failures: surface_failures.clone(),
                errors: Vec::new(),
                errors_said: 0,
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
                surfaces: crate::surfaces::Surfaces::default(),
                receiving: HashMap::new(),
                page_views: crate::page_views::PageViews::default(),
                nearby: Default::default(),
                nearby_offers: Vec::new(),
                nearby_sought: None,
                nearby_takes: None,
                nearby_measured: None,
                nearby_offer: None,
                page_offers: crate::page_offers::PageOffers::default(),
                board: crate::board::Board::default(),
                parked: Parked::default(),
                aliases: crate::aliases::Aliases::new(),
                handoff,
                accepted,
                transfers: tokio::task::JoinSet::new(),
                carrying: crate::running::Running::default(),
                outbound: Parked::default(),
                carrying_out: crate::running::Running::default(),
                permits: crate::permits::Permits::default(),
                statuses: crate::statuses::Statuses::default(),
                packages: None,
                accepted_agreements: std::collections::BTreeMap::new(),
                disabled: std::collections::BTreeSet::new(),
                held_back: Vec::new(),
                token_store: None,
                platform_services: None,
                platform_calls: crate::platform_calls::PlatformCalls::default(),
                platform_changes: None,
                extension_prompts: None,
                extension_prompt_changes: None,
                notification_changes: notifications.subscribe(),
                notifications,
                action_progress_changes: None,
                staged: HashMap::new(),
                held: peersus_net::held::Held::default(),
                installed_packages: std::collections::BTreeMap::new(),
                auto_update_tried: std::collections::BTreeSet::new(),
                store: None,
                store_asked: None,
                #[cfg(feature = "private-store")]
                private_store: None,
                store_tasks: tokio::task::JoinSet::new(),
                update: None,
                update_tasks: tokio::task::JoinSet::new(),
                restart: None,
                languages: None,
                language_tasks: tokio::task::JoinSet::new(),
            },
            Client {
                requests: commands,
                snapshots: state,
                surface_failures,
                told_to_stop: Arc::default(),
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
        self.extension_prompts = services.extension_prompts();
        if let Some(notifications) = services.notifications() {
            self.notification_changes = notifications.subscribe();
            self.notifications = notifications;
        }
        self.extension_prompt_changes = self.extension_prompts.as_ref().map(|p| p.subscribe());
        self.action_progress_changes = services.action_progress();
        self.platform_services = Some(services);
        self
    }

    /// Supply a platform launcher for a bundled plugin without pretending it is an executable.
    #[must_use]
    pub fn with_launcher(
        mut self,
        plugin: PluginId,
        launcher: Arc<dyn peersus_plugin::supervisor::Launcher>,
    ) -> Self {
        self.launchers.insert(plugin, launcher);
        self
    }

    /// Choose platform-owned executors for installed modules, including later installs.
    /// A module without its declared executor is refused, never run as an executable.
    #[must_use]
    pub fn with_modules(mut self, modules: Arc<dyn peersus_plugin::module::ModuleFactory>) -> Self {
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
        self.accepted_agreements = crate::plugin_agreements::load(manager.data_directory());
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
    #[must_use]
    pub fn with_store(
        mut self,
        service: crate::store_service::StoreService,
        fetcher: Arc<dyn crate::store_runtime::StoreFetcher>,
    ) -> Self {
        self.store = Some(StoreSlot {
            service,
            fetcher,
            refresh: Default::default(),
        });
        // The initial snapshot was published without a store; tell the window it exists now.
        self.publish();
        self
    }

    /// Give this owner updates of the application itself: `service` decides what is accepted (its
    /// own compiled-in key) and `installer` is how this installation replaces itself. Bytes come
    /// from the store's fetcher, so there is no second way out to the network.
    #[must_use]
    pub fn with_updates(
        mut self,
        service: crate::app_update::UpdateService,
        installer: Arc<dyn crate::app_update::Installer>,
    ) -> Self {
        self.update = Some(UpdateSlot {
            service,
            installer,
            schedule: crate::store_runtime::RefreshSchedule::every(UPDATE_CHECK_EVERY),
            asked: false,
            tried: None,
        });
        self.publish();
        self
    }

    /// Give this owner the application's languages, kept under the data directory. Files come from the store's fetcher,
    /// so there is no second way out to the network.
    #[must_use]
    pub fn with_languages(mut self, languages: crate::languages::Languages) -> Self {
        self.languages = Some(languages);
        self.publish();
        self
    }

    #[cfg(feature = "private-store")]
    pub fn with_private_store(
        mut self,
        platform: Arc<dyn crate::private_store::Platform>,
    ) -> Result<Self, &'static str> {
        let setup = platform.load().and_then(|source| {
            source
                .map(|source| crate::private_store::prepare(platform.as_ref(), &source))
                .transpose()
        });
        match setup {
            Ok(Some(setup)) => {
                self.store = Some(StoreSlot {
                    service: setup.service,
                    fetcher: setup.fetcher,
                    refresh: Default::default(),
                })
            }
            Ok(None) => {}
            Err(words) => {
                self.errors_said = self.errors_said.wrapping_add(1);
                self.errors.push(words.to_owned())
            }
        }
        self.private_store = Some(platform);
        self.publish();
        Ok(self)
    }

    #[cfg(feature = "private-store")]
    async fn configure_private_store(
        &mut self,
        source: peersus_core::private_store::PrivateCatalog,
    ) -> Result<(), &'static str> {
        if !self.store_tasks.is_empty() {
            return Err("Wait for the current catalog operation to finish");
        }
        let platform = self
            .private_store
            .clone()
            .ok_or("Private catalogs are not enabled on this device")?;
        let setup = tokio::task::spawn_blocking(move || {
            let setup = crate::private_store::prepare(platform.as_ref(), &source)?;
            platform.save(&source)?;
            Ok::<_, &'static str>(setup)
        })
        .await
        .map_err(|_| "Private catalog configuration could not be saved")??;
        self.store = Some(StoreSlot {
            service: setup.service,
            fetcher: setup.fetcher,
            refresh: Default::default(),
        });
        let _ = self.start_store_refresh();
        Ok(())
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
        // Input between devices is carried by one extension: a second one that declares it is
        // not run beside the first, and the person is told, instead of one of them winning silently.
        let carriers = self
            .running
            .keys()
            .filter(|running| **running != id)
            .filter_map(|running| self.plugins.find(running))
            .filter_map(|index| self.plugins.get(index))
            .map(|other| &other.manifest);
        if crate::pass_through::carrier_taken(&spec.manifest, carriers) {
            self.plugin_error(
                &id,
                "Another extension already carries input between devices. Turn that one off to use this one",
            );
            return;
        }
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
        let launcher = match self.launchers.get(&id) {
            Some(launcher) => Ok(Arc::clone(launcher)),
            None if matches!(
                plugin.manifest.execution,
                peersus_api::manifest::Execution::Process
            ) =>
            {
                Service::process_launcher(&plugin)
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
                }),
        };
        // A plugin installed in the store is started, and started again, only while its folder is what was registered.
        let folder = plugin.program.as_ref().and_then(|program| {
            let folder = self.packages().ok()?.store().join(id.as_str());
            program.starts_with(&folder).then_some(folder)
        });
        let started = launcher
            .map(|inner| match folder {
                Some(folder) => Arc::new(crate::plugin_folder::Registered { inner, folder })
                    as Arc<dyn peersus_plugin::supervisor::Launcher>,
                None => inner,
            })
            .and_then(|launcher| Service::start_with_launcher(self.hub.clone(), plugin, launcher));
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
        // The hub suspends an extension that keeps asking for more than its rate, for a while and by
        // itself; each suspension is told once. The extension stays installed and running.
        let suspended = self.hub.take_suspended();
        for _ in &suspended {
            self.error(
                "An extension asked for too much, too fast, and is paused for a short while",
            );
        }
        let changed = !transitions.is_empty() || !suspended.is_empty();
        for (id, status) in transitions {
            if matches!(
                status,
                peersus_core::model::PluginStatus::Restarting
                    | peersus_core::model::PluginStatus::Failed
                    | peersus_core::model::PluginStatus::Stopped
            ) {
                self.revoke_platform_calls(&id);
                let removed: Vec<_> = self
                    .page_offers
                    .pages(&id)
                    .iter()
                    .map(|page| page.id.clone())
                    .collect();
                self.page_offers.forget(&id);
                let calls = self.page_views.forget_pages(&id, &removed);
                self.cancel_calls(calls);
            }
            // Whoever asked for `peersus.plugins` is told. A subscriber that cannot keep up
            // misses this one rather than stalling the application: the hub refuses the whole
            // delivery then, and `list` is always there to ask what is true now.
            let _ = self.host.submit(peersus_api::message::Operation::Publish {
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
            self.page_offers.forget(&plugin);
        }
        while self.platform_calls.next().await.is_some() {}
        for (_, running) in self.running.drain() {
            running.service.shutdown().await;
        }
    }

    pub async fn run(mut self) -> Exit {
        let mut progress = tokio::time::interval(Duration::from_secs(1));
        progress.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // One tiny task per service the platform can watch, all feeding one channel, so the loop waits on one thing. Started for every
        // such service whether or not a plugin listens yet: a plugin installed later must still be told.
        if let Some(services) = self.platform_services.clone() {
            let (changed, receiver) = tokio::sync::mpsc::channel(8);
            for service in crate::platform_services::watched() {
                if let Some(mut counter) = services.changes(service) {
                    let changed = changed.clone();
                    let service: &'static str = service;
                    tokio::spawn(async move {
                        while counter.changed().await.is_ok() {
                            // A full channel already holds a pending notice for this loop: dropping this one loses nothing.
                            let _ = changed.try_send(service);
                        }
                    });
                }
            }
            self.platform_changes = Some(receiver);
        }
        // The user's decisions survive a restart in the saved devices. Recover what the
        // application may call from them before any plugin can be asked for anything.
        self.sync_host_grants();
        // Before anything is connected: a device that keeps to itself must not say how it is to the first one that connects.
        self.apply_private_mode();
        self.start_plugins();
        // The snapshot made in `new` was taken before this owner was told what is switched off and what was found and is not
        // run, so a window that connects now would be shown a list without them until something else happened.
        self.publish();
        self.auto_store_refresh();
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
            let paced = self.page_views.due(std::time::Instant::now());
            tokio::select! {
                _ = async { match paced { Some(wait) => tokio::time::sleep(wait).await, None => std::future::pending().await } } => {
                    for (key, event) in self.page_views.flush(std::time::Instant::now()) {
                        if let Err(sentence) = self.send_page_event(&key, event) {
                            self.plugin_error(&key.plugin, sentence);
                            self.publish_pages();
                        }
                    }
                }
                request = self.requests.recv() => {
                    let Some(request) = request else { break };
                    if matches!(request.command, Command::Shutdown {}) {
                        self.requests.close();
                        self.stop_plugins().await;
                        self.network.close().await;
                        let _ = request.result.send(Ok(Reply::Applied));
                        return Exit::Stopped;
                    }
                    // Only these can change which packages are installed or at what version. Checked before the
                    // command is moved, and read once after, so `publish` itself never touches the disk.
                    let changes_installed = matches!(
                        request.command,
                        Command::RemovePackage { .. } | Command::ConfirmInstall { .. } | Command::ApprovePlugin { .. } | Command::StoreInstall { .. }
                    );
                    let configuration_changed = matches!(
                        request.command,
                        Command::SetGrants { .. }
                            | Command::Allow { .. }
                            | Command::SetPluginSetting { .. }
                            | Command::UpdateSettings { .. }
                            | Command::PatchSettings { .. }
                            | Command::ForgetPeer { .. }
                            | Command::SetBindings { .. }
                    );
                    // Pointer traffic changes no snapshot. Avoid cloning and publishing the
                    // application's state for every finger sample; errors still reach the window.
                    let surface_input = matches!(request.command, Command::ForwardInput { .. } | Command::PageEvent { .. });
                    let about = command_plugin(&request.command);
                    let result = self.execute(request.command).await;
                    if changes_installed { self.refresh_installed(); }
                    if configuration_changed && result.is_ok() {
                        self.publish_configuration_change();
                    }
                    if let Err(reason) = result.as_ref() {
                        match &about {
                            Some(plugin) => self.plugin_error(plugin, reason),
                            None => self.error(reason),
                        }
                    }
                    if !matches!(&result, Ok(Reply::QueueBusy))
                        && (!surface_input || result.is_err())
                    {
                        self.publish();
                    }
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
                        Ok(delivery) => {
                            if self.handle_host_delivery(delivery).await { self.publish(); }
                        },
                        // The application's seat on the hub was revoked, which only
                        // happens when the whole hub is going away.
                        Err(_) => break,
                    }
                }
                accepted = self.accepted.recv() => {
                    // The sender lives in the runtime itself, so this only ends with it.
                    if let Some(accepted) = accepted {
                        self.begin_transfer(accepted);
                    }
                }
                Some(finished) = self.platform_calls.next(), if !self.platform_calls.is_empty() => {
                    if let Some(result) = finished.result {
                        let _ = self.host.submit(peersus_api::message::Operation::Reply {
                            ticket: finished.ticket,
                            result,
                        });
                    }
                }
                Ok(()) = async {
                    match self.extension_prompt_changes.as_mut() {
                        Some(changes) => changes.changed().await,
                        None => std::future::pending().await,
                    }
                } => self.publish(),
                Ok(()) = self.notification_changes.changed() => self.publish(),
                Ok(()) = async {
                    match self.action_progress_changes.as_mut() {
                        Some(changes) => changes.changed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    let progress = self.action_progress_changes.as_mut()
                        .map(|changes| changes.borrow_and_update().clone()).unwrap_or_default();
                    let mut changed = false;
                    for status in self.board.statuses() {
                        if status.state == peersus_core::model::ActionState::Running && status.peer.is_none()
                            && let Some(&(sent, total)) = progress.get(&(status.plugin.clone(), status.action.clone()))
                        {
                            changed |= self.board.progress(&Key {
                                plugin: status.plugin, action: status.action, device: None,
                            }, sent, total);
                        }
                    }
                    if changed { self.publish(); }
                }
                Some(service) = async {
                    match self.platform_changes.as_mut() {
                        Some(changes) => changes.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.publish_platform_change(service);
                }
                Some(finished) = self.transfers.join_next(), if !self.transfers.is_empty() => {
                    match finished {
                        Ok((direction, call, outcome)) => {
                            peersus_diagnostics::dev_trace!(
                                stage = "transfer-end", direction = match direction { Direction::Inbound => "in", Direction::Outbound => "out" },
                                call = if matches!(direction, Direction::Inbound) { call } else { 0 },
                                outcome = match &outcome { crate::receive::Delivery::Done(_) => "ok", crate::receive::Delivery::Failed(_) => "failed" },
                                "Plugin transfer ended"
                            );
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
                // One step of an application update finished. Verifying happens here, on the loop.
                Some(finished) = self.update_tasks.join_next(), if !self.update_tasks.is_empty() => {
                    match finished {
                        Ok(outcome) => self.finish_update(outcome),
                        // A step that panicked or was lost never reports back: release the update, and say so.
                        Err(error) => {
                            if let Some(slot) = self.update.as_mut() {
                                slot.service.failed("The update failed");
                            }
                            if !error.is_cancelled() {
                                self.error("The update failed");
                            }
                        }
                    }
                    self.automatic_update();
                    self.publish();
                }
                // A fetch of languages finished: what it brought is checked and kept here.
                Some(finished) = self.language_tasks.join_next(), if !self.language_tasks.is_empty() => {
                    let result = finished.unwrap_or(Err(crate::catalog::Refusal("The languages could not be fetched")));
                    if let Some(languages) = self.languages.as_mut() {
                        languages.finish(result, tokio::time::Instant::now());
                    }
                    self.publish();
                }
                // The windows have been told: end, so that whatever started this owner starts the new one.
                _ = async { match self.restart { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => {
                    self.requests.close();
                    self.stop_plugins().await;
                    self.network.close().await;
                    return Exit::Restart;
                }
                _ = progress.tick() => {
                    // Streams whose plugin never came are dropped here, which also ends the
                    // peer's side. Nothing else drives this: without it an entry would hold a
                    // peer's connection open for as long as the runtime lived.
                    self.parked.expire(std::time::Instant::now());
                    self.outbound.expire(std::time::Instant::now());
                    self.close_idle_devices();
                    self.auto_store_refresh();
                    self.automatic_update();
                    self.automatic_languages(None);
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
        Exit::Stopped
    }

    /// Grants, settings or the devices changed: every service that says when it changes says so
    /// again, so its subscribers look once more at what they may do and need no timer merely to
    /// discover a new consent or device eligibility decision. Which services those are is read
    /// from their table; none is named here.
    fn publish_configuration_change(&self) {
        for service in crate::platform_services::watched() {
            self.publish_platform_change(service);
        }
    }

    /// A prompt to reconsider the guarded service state, never its private contents.
    fn publish_platform_change(&self, service: &str) {
        if consumers_of(&self.plugins, service).is_empty() {
            return;
        }
        for (offered, event, _, _) in crate::platform_services::EVENTS {
            if *offered == service {
                let _ = self.host.submit(peersus_api::message::Operation::Publish {
                    service: service.to_owned(),
                    version: 1,
                    event: (*event).to_owned(),
                    data: serde_json::json!({}),
                });
            }
        }
    }

    fn peer(&self, id: &PeerId) -> Option<Peer> {
        self.network.peers().into_iter().find(|p| &p.id == id)
    }

    /// Whether `target` can be bound: an application action always; an extension's action only when it is installed, a key can
    /// run it as it is, and a device is named exactly when it needs one and that device is paired.
    fn bindable(&self, target: &peersus_core::bindings::BindTarget) -> Result<(), &'static str> {
        let peersus_core::bindings::BindTarget::Plugin {
            plugin,
            action,
            peer,
        } = target
        else {
            return Ok(());
        };
        let declared = self
            .plugins
            .find(plugin)
            .and_then(|index| self.plugins.get(index))
            .and_then(|spec| {
                spec.manifest
                    .contributions
                    .actions
                    .iter()
                    .find(|declared| &declared.id == action)
                    .cloned()
            })
            .ok_or("That extension action is not installed")?;
        if !declared.bindable() {
            return Err("That action cannot be run by a key");
        }
        match peer {
            Some(peer) if declared.needs_device && self.peer(peer).is_some() => Ok(()),
            None if !declared.needs_device => Ok(()),
            Some(_) if declared.needs_device => Err("Unknown device"),
            _ => Err("That action needs a device for its key, and only then"),
        }
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
        ids.extend(self.surfaces.forget_device(plugin, device));
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
                .submit(peersus_api::message::Operation::Cancel { id });
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
        self.surfaces.forget(plugin);
        self.page_views.forget(plugin);
        self.page_offers.forget(plugin);
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
        self.surfaces.forget(plugin);
        self.page_views.forget(plugin);
        self.page_offers.forget(plugin);
        self.board.forget_plugin(plugin);
        // What the plugin said of this device goes with it: nothing keeps it true any more, and another device must not go on showing it.
        self.forget_statuses(plugin);
        self.forget_permits(plugin);
        let announced = self.parked.drop_for_plugin(plugin);
        self.carrying.stop_for_plugin(plugin);
        self.outbound.drop_for_plugin(plugin);
        self.carrying_out.stop_for_plugin(plugin);
        self.cancel_calls(announced);
        if let Some(running) = self.running.remove(plugin) {
            running.service.shutdown().await;
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
            Command::WithdrawFilesInvitation { generation } => {
                if !self.network.withdraw_files_code(generation) {
                    return Err("The code was already used or is no longer shown");
                }
            }
            Command::ChoosePairing { request, number } => self.network.choose(&request, number)?,
            Command::RejectPairing { request } => {
                if !self.network.reject(&request) {
                    return Err("Pairing request is no longer active");
                }
            }
            Command::AnswerExtensionPrompt {
                id,
                accepted,
                secret,
                remove,
            } => {
                self.extension_prompts
                    .as_ref()
                    .ok_or("Extension prompts unavailable")?
                    .answer(
                        id,
                        crate::extension_prompts::Answer {
                            accepted,
                            secret,
                            remove,
                        },
                    )?;
            }
            Command::AnswerNotification { id, button } => {
                self.notifications.answer(id, button)?;
            }
            Command::ForgetPeer { peer } => {
                self.network
                    .forget(&peer)
                    .map_err(|_| "Could not forget device")?;
                self.withdraw_device_work(None, &peer);
                self.change_bindings(|bindings| {
                    peersus_core::bindings::forget_peer(bindings, &peer)
                })
                .await;
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
                self.opened_by_person(&peer, None);
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
            Command::UpdateSettings {
                mut settings,
                expected,
            } => {
                if self.settings.current() != &*expected {
                    return Err("Preferences changed elsewhere; reload before saving");
                }
                // Bindings are changed by `SetBindings` alone, which checks them; a whole-settings save keeps what is bound.
                settings
                    .bindings
                    .clone_from(&self.settings.current().bindings);
                self.apply_settings(*settings).await?;
            }
            Command::PatchSettings { patch } => {
                // Said as it is: a person told "conflict" cannot tell what to change, and the refusal knows.
                let settings = patch
                    .apply(self.settings.current())
                    .map_err(|error| error.words())?;
                let enabled_update = patch.auto_update.as_ref().is_some_and(|change| change.on);
                self.apply_settings(settings).await?;
                if enabled_update {
                    self.start_automatic_update();
                }
            }
            Command::SetBindings { bindings } => {
                if cfg!(target_os = "android") {
                    return Err("Key bindings are not available on this device");
                }
                if !peersus_core::bindings::within_limits(&bindings) {
                    return Err("An action can have up to three key bindings");
                }
                for target in bindings.values() {
                    self.bindable(target)?;
                }
                let mut settings = self.settings.current().clone();
                settings.bindings = bindings;
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
            Command::ForwardInput {
                plugin,
                peer,
                events,
                end,
            } => self.forward_input(plugin, peer, events, end)?,
            Command::PageEvent {
                plugin,
                page,
                event,
            } => self.page_event(plugin, page, event)?,
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
            Command::InvokeSelectedPathAction {
                plugin,
                action,
                path,
            } => {
                self.invoke_selected(plugin, action, Some(path), None)?;
            }
            Command::InvokeSelectedAction {
                plugin,
                action,
                path,
                held,
            } => {
                self.invoke_selected(plugin, action, path, held)?;
            }
            Command::InvokeSharedText {
                plugin,
                action,
                peer,
                text,
            } => {
                let key = Key {
                    plugin: plugin.clone(),
                    action: action.clone(),
                    device: Some(peer.clone()),
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
                    if declaration.input != peersus_api::contributions::ActionInput::SharedText {
                        return Err("This action does not accept shared text");
                    }
                    if text.is_empty() || text.len() > crate::platform_services::MAX_CLIPBOARD_BYTES
                    {
                        return Err("The text to share is empty or too large");
                    }
                    let context = services
                        .select(&plugin, crate::platform_services::SelectedInput::Text(text))?;
                    if let Err(error) =
                        self.invoke(plugin.clone(), &action, Some(peer), Some(context))
                    {
                        services.discard_selection(&plugin, context);
                        return Err(error);
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    tracing::warn!(reason = error, "Shared text refused before admission");
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
                        .submit(peersus_api::message::Operation::Cancel { id });
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
            Command::ConfirmInstall {
                plugin,
                agreement,
                risk_acknowledged,
            } => {
                self.confirm_install(plugin, agreement, risk_acknowledged)
                    .await?
            }
            Command::CancelInstall { plugin } => self.cancel_install(&plugin)?,
            Command::StoreRefresh {} => self.start_store_refresh()?,
            Command::StoreInstall { plugin } => self.start_store_install(plugin)?,
            Command::ShowStoreEntry { plugin } => self.show_store_entry(plugin)?,
            Command::UpdateCheck {} => self.start_update_check(true)?,
            Command::UpdateInstall {} => self.start_update_install(true)?,
            Command::UpdateRestart {} => self.start_restart()?,
            Command::SetDeviceLanguage { language } => {
                let languages = self.languages.as_mut().ok_or(NO_LANGUAGES)?;
                if languages.set_device(language) {
                    self.automatic_languages(None);
                }
            }
            Command::LanguagesRefresh {} => {
                self.languages.as_mut().ok_or(NO_LANGUAGES)?.ask();
                self.automatic_languages(None);
            }
            Command::LanguageDownload { language } => {
                if language.is_english() {
                    return Err("English is the application's own language and needs no file");
                }
                if self.languages.is_none() || self.store.is_none() {
                    return Err(NO_LANGUAGES);
                }
                if !self.language_tasks.is_empty() {
                    return Err("Languages are being fetched; try again when that has finished");
                }
                self.automatic_languages(Some(language));
            }
            Command::LanguageImport { text } => {
                self.languages.as_mut().ok_or(NO_LANGUAGES)?.import(&text)?;
            }
            Command::LanguageText {} => {
                let chosen = self.settings.current().language;
                return self
                    .languages
                    .as_ref()
                    .ok_or(NO_LANGUAGES)?
                    .shown_text(chosen)
                    .map(Reply::LanguageText);
            }
            Command::LanguageRemove { language, imported } => {
                self.languages
                    .as_mut()
                    .ok_or(NO_LANGUAGES)?
                    .remove(language, imported)?;
            }
            #[cfg(feature = "private-store")]
            Command::ConfigurePrivateStore { source } => {
                self.configure_private_store(source).await?
            }
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
            Command::SetDisplays { displays } => {
                self.network.set_displays(displays);
            }
            Command::SetNearby { found, offers } => {
                self.nearby = found;
                self.nearby_offers = offers;
            }
            Command::SeekNearby { takes } => {
                if self.settings.current().nearby_send {
                    self.nearby_sought = Some(std::time::Instant::now());
                    self.nearby_takes = takes;
                }
            }
            Command::MeasureNearby {} => {
                if self.settings.current().nearby_send {
                    self.nearby_measured = Some(std::time::Instant::now());
                }
            }
            Command::NearbyOffer { front } => {
                use peersus_core::nearby::Takes;
                use peersus_net::invitations::Announced;
                let sought = self.settings.current().nearby_send
                    && self
                        .nearby_sought
                        .is_some_and(|at| at.elapsed() < NEARBY_SOUGHT_FOR);
                let takes = self.nearby_takes.filter(|_| sought && front);
                // What is out for anything other than what the page shown now, in front, lets this device be found
                // for admits no one from now on: to take files and to be paired with are two things, and neither
                // outlives its page being looked at.
                if self
                    .nearby_offer
                    .as_ref()
                    .is_some_and(|(_, _, of)| Some(*of) != takes)
                {
                    self.nearby_offer = None;
                    self.network.withdraw_nearby_invitation();
                }
                if !sought {
                    self.nearby_sought = None;
                    self.nearby_takes = None;
                    return Ok(Reply::Applied);
                }
                let Some(takes) = takes else {
                    return Ok(Reply::Nearby(None));
                };
                let announced = match takes {
                    Takes::Pairing => Announced::Pairing,
                    Takes::Files => Announced::Files,
                };
                let now = now();
                // One is announced until it was used, was revoked with the network, or is about to run out.
                let kept = self.network.nearby_invitation_live(announced)
                    && self
                        .nearby_offer
                        .as_ref()
                        .is_some_and(|(_, expires_at, _)| {
                            expires_at.saturating_sub(now) > NEARBY_OFFER_RENEWED_BEFORE
                        });
                if !kept {
                    let invitation = self
                        .network
                        .invite_nearby(now, announced)
                        .map_err(|_| "Could not create invitation")?;
                    let text = invitation
                        .encode()
                        .map_err(|_| "Could not create invitation")?;
                    self.nearby_offer = Some((text, invitation.expires_at, takes));
                }
                return self
                    .nearby_offer
                    .as_ref()
                    .map(|(text, _, takes)| Reply::Nearby(Some((text.clone(), *takes))))
                    .ok_or("Could not create invitation");
            }
            Command::PairNearby { peer } => {
                let invitation = self
                    .nearby_offers
                    .iter()
                    .find(|(id, _)| id == &peer)
                    .and_then(|(_, text)| Invitation::decode(text, now()).ok())
                    .ok_or("That device is no longer nearby")?;
                self.network
                    .join(invitation)
                    .map_err(|_| "Connection queue is full")?;
            }
            // A test of the connection to a paired device: it is started and the answer comes later, in the snapshot, so a window never waits on
            // the network here. Only a device that is paired can be tested, and it is asked nothing but whether it answers and knows this one.
            Command::TestLink { peer } => self.network.test_link(&peer)?,
            Command::ClearLinkTest { peer } => self.network.clear_link_test(&peer),
            // How quickly each configured relay answers; started here, answered later in the snapshot.
            Command::ProbeRelays { only } => {
                // Every relay the list shows, enabled or not: the configured ones and the built-in ones that may be switched off.
                let mut policy = self.settings.current().network.effective_relay_policy();
                for target in peersus_core::relay::RelayPolicy::default().targets {
                    if !policy.targets.contains(&target) {
                        policy.targets.push(target);
                    }
                }
                if let Some(only) = only {
                    policy.targets.retain(|target| target.probe_key() == only);
                }
                self.network.probe_relays(&policy);
            }
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
}

/// How long after a window last said it shows the list of devices nearby this device still looks for them: a window
/// says so every few seconds, and one that went without a word has stopped.
const NEARBY_SOUGHT_FOR: std::time::Duration = std::time::Duration::from_secs(10);
/// How long before the invitation announced nearby runs out the next is made, in seconds: a device that heard the
/// old one a moment ago still has time to use it.
const NEARBY_OFFER_RENEWED_BEFORE: u64 = 90;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cancelled_action_regression {
    #[test]
    fn explicit_plugin_cancellation_is_not_a_failure() {
        assert_eq!(
            super::action_state(&peersus_api::message::Outcome::error(
                "cancelled",
                "ignored"
            )),
            peersus_core::model::ActionState::Cancelled
        );
    }
}
