//! Peersus on Android.
//!
//! Two things are here and nothing else: the entry the system calls, and the owner. The interface is
//! `peersus-ui`, drawn by the same code as on a desktop.
//!
//! THE OWNER RUNS INSIDE THE APPLICATION. On a desktop the owner is a program of its own that the window
//! starts and attaches to. An Android application cannot start another executable, so the owner is started
//! here, on this process's own executor, listening on the same control socket; the interface then finds it
//! listening and attaches to it exactly as it does anywhere else. Nothing in `peersus-ui` knows the
//! difference.
//!
//! THE IDENTITY IS A FILE IN THE APPLICATION'S OWN FOLDER, for now. It is made once and kept (`identity.key`, readable by the
//! owner only), so that this phone is the same device the next time it starts: paired devices stay paired, and what was sent
//! to and from it stays under it. The folder is the application's private one, which no other application can read without
//! root, and which the system removes with the application. The key belongs, in the end, in the Android Keystore, which keeps
//! it where even the application cannot read it out; that is a piece of its own (JNI), and it replaces `load_identity` below
//! and nothing else. Bundled plugins run in a separate private Android Service process, supervised
//! through the same authenticated control/data protocol as desktop plugins.
//!
//! Empty on every other target, so that building the whole workspace on a desktop needs no Android
//! toolchain.
#![cfg(target_os = "android")]

mod bridge;
mod camera_bridge;
mod notifications;
mod plugins;
mod store;

use freya::winit::platform::android::{EventLoopBuilderExtAndroid, activity::AndroidApp};
use freya::{prelude::*, winit::event_loop::EventLoop};
use iroh::SecretKey;
use peersus_app::{ipc, lifetime::Presence, runtime::Runtime};
use peersus_net::{
    grants::StandaloneGrants, history::History, network_host::NetworkHost, peers::Peers,
    pictures::Pictures, settings::SettingsStore, transport,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// The device's key: the one `peersus_net::key_file` keeps in `identity.key` in `directory`, made once and the same for every start
/// (see there for why a start at the same moment as another, or a damaged file, can never make this another device).
fn load_identity(directory: &Path) -> Result<SecretKey, String> {
    peersus_net::key_file::load(directory).map_err(str::to_owned)
}

/// Where this phone keeps the access token of a personal relay: a file of the application's own folder, for its owner alone, as it keeps the
/// device's key (see [`load_identity`]). It belongs, in the end, in the Android Keystore with the key, and replaces this and nothing else.
struct FileTokens(PathBuf);

impl peersus_app::runtime::TokenStore for FileTokens {
    fn save(&self, token: &str) -> Result<(), &'static str> {
        peersus_net::token_file::save(&self.0, token)
    }
}

/// The network belongs to the process, not to an activity's event loop. Android may
/// recreate an activity while this process, its JNI references and its UI state survive.
struct Owner {
    _executor: tokio::runtime::Runtime,
    socket: PathBuf,
    held: peersus_net::held::Held,
    client: peersus_app::runtime::Client,
}

static OWNER: Mutex<Option<Arc<Owner>>> = Mutex::new(None);

fn owner_at(directory: &Path) -> Result<Arc<Owner>, String> {
    let mut slot = OWNER
        .lock()
        .map_err(|_| "The application owner could not be accessed".to_owned())?;
    if let Some(owner) = slot.as_ref() {
        return Ok(Arc::clone(owner));
    }
    // Single-flight initialization also prevents a second activity from replacing the
    // live owner's control socket or starting another identity/network in this process.
    let executor = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "The executor could not start".to_owned())?;
    // Where a second copy of a chosen file is made when a phone will not let one be opened twice (sending to several devices): a folder of
    // the program's own, since a phone gives no shared temporary folder. Without it a send to one device is not affected.
    let copies = directory.join("held-copies");
    let held = match std::fs::create_dir_all(&copies) {
        Ok(()) => peersus_net::held::Held::default().with_copies_in(copies),
        Err(_) => peersus_net::held::Held::default(),
    };
    let (socket, client) = start_owner(&executor, directory, held.clone())?;
    let owner = Arc::new(Owner {
        _executor: executor,
        socket,
        held,
        client,
    });
    *slot = Some(Arc::clone(&owner));
    Ok(owner)
}

/// The owner: the network, the saved state and the control service, on `executor`, in `directory`.
///
/// Returns the control socket once it is listening, so that the interface which opens next is told where.
fn start_owner(
    executor: &tokio::runtime::Runtime,
    directory: &Path,
    held: peersus_net::held::Held,
) -> Result<(PathBuf, peersus_app::runtime::Client), String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("The data directory could not be made: {error}"))?;
    let settings = SettingsStore::open(directory.join("preferences.json"))
        .map_err(|error| format!("Preferences: {error}"))?;
    let key = load_identity(directory)?;
    // The token of a personal relay, only when one is used: the file is not read for a secret that nothing will use. One that cannot be read is
    // said (for `adb logcat`) and the program carries on without it, as with preferences it could not use: the person is told by the relay refusing
    // this device, and not left with an application that will not start.
    let access = if settings.current().network.has_custom_relay() {
        match peersus_net::token_file::load(directory) {
            Ok(access) => access,
            Err(reason) => {
                eprintln!("peersus: {reason}");
                None
            }
        }
    } else {
        None
    };
    executor.block_on(async {
        let endpoint = transport::bind_with(key, &settings.current().network, access.as_ref())
            .await
            .map_err(|_| "The network could not be opened".to_owned())?;
        let mut devices = Peers::open(directory.join("peers.json"))
            .map_err(|_| "Remembered devices could not be read".to_owned())?;
        let mut standalone = StandaloneGrants::open(directory.join("grants.json"))
            .map_err(|_| "Plugin permissions could not be read".to_owned())?;
        let found = plugins::registry(directory)?;
        peersus_app::plugin_manager::forget_removed(&found, &mut devices, &mut standalone)
            .map_err(|_| "Removed plugin permissions could not be forgotten".to_owned())?;
        let network = NetworkHost::new(
            endpoint,
            settings.current().device_name.clone(),
            devices,
            History::open(directory.join("history.json"))
                .map_err(|_| "Transfer history could not be read".to_owned())?,
        )
        .with_network_settings(&settings.current().network)
        .with_pictures(Pictures::new(directory))
        // What the endpoint was bound with, so that saving the same token again is no change and disturbs no connection.
        .with_relay_access(access);
        // Which picture each device showed, kept between starts; an unreadable file is left as it is and the host starts without it.
        let network = match peersus_net::picture_owners::PictureOwners::open(
            directory.join("picture-owners.json"),
        ) {
            Ok(owners) => network.with_picture_owners(owners),
            Err(_) => network,
        };
        let registry = found.registry;
        let clipboard = peersus_core::plugins::PluginId::new("clipboard")
            .map_err(|_| "The bundled plugin identity is invalid".to_owned())?;
        let (runtime, client) = Runtime::new(
            network,
            standalone,
            settings,
            registry,
            directory.to_path_buf(),
        )
        .map(|(runtime, client)| {
            (
                runtime
                    .with_held(held)
                    .with_launcher(clipboard, Arc::new(plugins::AndroidLauncher))
                    .with_modules(Arc::new(plugins::AndroidModules))
                    .with_token_store(Arc::new(FileTokens(directory.to_path_buf())))
                    .with_packages(plugins::packages(directory))
                    .with_store(
                        peersus_app::store_service::StoreService::new(
                            peersus_app::store_trust::MAINTAINER_PUBLIC_KEY,
                            directory,
                        ),
                        Arc::new(store::AndroidFetcher(directory.to_path_buf())),
                    )
                    .with_disabled(found.disabled)
                    .with_held_back(found.held_back),
                client,
            )
        })
        .map_err(|_| "The application could not start".to_owned())?;
        let socket = directory.join("control.sock");
        // A socket left by an earlier run of this very application belongs to no one now: the system
        // ended that process. It is the application's own directory, so it is safe to clear.
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket)
            .map_err(|error| format!("The control socket could not be opened: {error}"))?;
        tokio::spawn(runtime.run());
        tokio::spawn(protect_transfers(
            client.clone(),
            directory.join("incoming"),
        ));
        tokio::spawn(ipc::serve(listener, client.clone(), Presence::default()));
        Ok::<_, String>((socket, client))
    })
}

/// Observe process-owned snapshots while the activity is paused or absent. Background mode
/// protects idle availability too; receiving an offer does not grant file-acceptance consent.
async fn protect_transfers(client: peersus_app::runtime::Client, staging: PathBuf) {
    let mut snapshots = client.subscribe();
    let mut protected = None;
    let mut notifications = notifications::Tracker::default();
    let mut language = None;
    let mut told_without_window = None;
    loop {
        let current = Arc::clone(&snapshots.borrow_and_update());
        if language != Some(current.settings.language)
            && bridge::language(current.settings.language.code()).is_ok()
        {
            language = Some(current.settings.language);
            notifications.resend();
        }
        let active = current.files_code.is_some_and(|code| code.pending)
            || current.transfers.iter().any(|transfer| {
                transfer.state.is_active()
                    && (!transfer.incoming
                        || transfer.state == peersus_core::model::TransferState::Running)
            });
        let policy = (current.settings.background, active);
        if protected != Some(policy) && bridge::background_state(policy.0, policy.1).is_ok() {
            protected = Some(policy);
        }
        // Whether an Accept can be served with no window: saving on their own is on and a folder is chosen. Told to Java BEFORE the notification is
        // made, since what the Accept button is made of is settled then; and only when it changes, since Java makes the notification again with it.
        let saved_tree = if current.settings.auto_save {
            bridge::saved_folder()
        } else {
            None
        };
        let without_window = saved_tree.is_some();
        if told_without_window != Some(without_window)
            && bridge::accept_without_window(without_window).is_ok()
        {
            told_without_window = Some(without_window);
        }
        notifications.update(
            &current.transfers,
            peersus_ui::view::size_in_words,
            |waiting, completed, newest, files| {
                bridge::transfer_notifications(waiting, completed, newest, files).is_ok()
            },
        );
        // A Cancel pressed on the notification is an answer of the person to that one offer: it is refused, the same command the
        // Cancel of the offer bar sends. An offer that is gone already is refused by the owner and nothing more happens.
        for action in
            bridge::take_offer_actions(|action| matches!(action, bridge::OfferAction::Cancel(_)))
        {
            if let bridge::OfferAction::Cancel(transfer) = action {
                let _ = client
                    .command(peersus_core::model::Command::CancelTransfer { transfer })
                    .await;
            }
        }
        // An Accept for an offer that is not waiting any more (it ran out, or was answered elsewhere) is nothing to serve and nothing to ask the
        // window for: it is dropped, so that it cannot keep the window being brought forward.
        let waiting: std::collections::BTreeSet<&str> = current
            .transfers
            .iter()
            .filter(|transfer| {
                transfer.incoming && transfer.state == peersus_core::model::TransferState::Offered
            })
            .map(|transfer| transfer.id.as_str())
            .collect();
        let dropped = bridge::take_offer_actions(
            |action| matches!(action, bridge::OfferAction::Accept(transfer) if !waiting.contains(transfer.as_str())),
        );
        if !dropped.is_empty() {
            // Said, so that an Accept that did nothing can be told from one that was never seen: its offer was not waiting in what the owner holds.
            eprintln!(
                "peersus: {} accept(s) dropped, their offer is not waiting ({} offer(s) are)",
                dropped.len(),
                waiting.len()
            );
        }
        // A file that arrived whole is put in the folder chosen for it by the platform, with the window closed as with it open. The same rule as the
        // window's own (`view::handoffs`), and the folder is given up when asked (taken once), so whichever asks first hands it over and the other
        // is given nothing: a file is never handed over twice. One that could not be put there stays in the staging.
        {
            let folders = bridge::folders();
            for handoff in
                peersus_ui::view::handoffs(&peersus_ui::view::View::of(&current).transfers, |id| {
                    folders.tree_for(id)
                })
            {
                if let Err(reason) = bridge::deliver(&handoff) {
                    eprintln!(
                        "peersus: a received file could not be handed to the folder: {reason}"
                    );
                    folders.delivered(handoff.transfer.clone(), false);
                }
            }
            // A transfer that ended without arriving goes nowhere: the folder remembered for it is let go of.
            for item in peersus_ui::view::View::of(&current)
                .transfers
                .iter()
                .filter(|item| {
                    item.incoming
                        && matches!(
                            item.phase,
                            peersus_ui::view::Phase::Cancelled | peersus_ui::view::Phase::Failed(_)
                        )
                })
            {
                folders.forget(&item.id);
            }
        }
        // An Accept pressed with the window closed is served here when it needs no window: saving on their own is on and the person chose a
        // folder, so the file is received into the program's own folder and the system puts it in the chosen one once it is whole, the same as
        // an Accept in the window. Otherwise the press is left where it is, for the window that the panel brings forward and that asks for a folder.
        if let Some(tree) = saved_tree {
            for action in bridge::take_offer_actions(|action| {
                matches!(action, bridge::OfferAction::Accept(_))
            }) {
                let bridge::OfferAction::Accept(transfer) = action else {
                    continue;
                };
                let directory = staging.join(&transfer);
                if std::fs::create_dir_all(&directory).is_err() {
                    continue;
                }
                bridge::folders().destined(&transfer, tree.clone());
                let _ = client
                    .command(peersus_core::model::Command::AcceptTransfer {
                        transfer,
                        directory,
                    })
                    .await;
            }
        } else if bridge::offer_actions_waiting(|action| {
            matches!(action, bridge::OfferAction::Accept(_))
        }) {
            // An Accept that needs a window (no folder is kept, or saving on their own is off): the window is asked for, and it takes the press
            // and asks where the file goes. The press stays where it is until the window has it.
            eprintln!("peersus: an accept needs the window: asking for it");
            if let Err(reason) = bridge::bring_window_forward() {
                eprintln!("peersus: the window could not be brought forward: {reason}");
            }
        }
        tokio::select! {
            changed = snapshots.changed() => {
                if changed.is_err() {
                    let _ = bridge::background_state(false, false);
                    return;
                }
            }
            () = bridge::offer_action_arrived().notified() => {}
            () = bridge::platform_ready().notified() => {
                protected = None;
                notifications.resend();
            }
            () = bridge::asleep_changed().notified() => {
                let asleep = bridge::asleep_now();
                let told = client.command(peersus_core::model::Command::SetAsleep { asleep }).await;
                eprintln!("peersus: the screen {} and the owner {} it", if asleep { "went off" } else { "is on" }, if told.is_ok() { "took" } else { "refused" });
            }
            () = bridge::background_timeout().notified() => {
                // Do not block the service's mandatory stopSelf callback on network work.
                let transfers: Vec<_> = client.subscribe().borrow().transfers.iter()
                    .filter(|transfer| transfer.state.is_active())
                    .map(|transfer| transfer.id.clone()).collect();
                for transfer in transfers {
                    let _ = client.command(peersus_core::model::Command::CancelTransfer { transfer }).await;
                }
            }
        }
    }
}

/// What the system calls when the activity starts. The only entry; the one place this crate is told
/// anything by the platform.
// The one `unsafe` in the crate. The system finds this function by its exact name in the shared library, so
// the name must not be mangled; Rust makes that attribute `unsafe` because a duplicate of the same name in
// another library would be resolved by the linker without any check. SAFETY: this library exports no other
// symbol of this name, and the host loads only this library for the activity.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    // Diagnostics for `adb logcat`: what goes wrong at start is otherwise invisible on a phone.
    let say = |text: String| eprintln!("peersus: {text}");

    let Some(directory) = app.internal_data_path() else {
        say("the application has no data directory of its own".to_owned());
        return;
    };
    // Where the circle of the Send page writes its diagnostic log: the interface cannot find the directory of an Android application by itself.
    peersus_ui::link::set_circle_log_directory(directory.clone());
    let owner = match owner_at(&directory) {
        Ok(owner) => owner,
        Err(reason) => {
            say(reason);
            return;
        }
    };
    let picked = bridge::shared(owner.held.clone());
    // So that the picker can be opened later from any thread: the activity, and the virtual machine it lives in.
    // SAFETY: both pointers are the ones this very `AndroidApp` was started with, and live as long as the process.
    if let Err(reason) =
        unsafe { bridge::remember_activity(app.vm_as_ptr(), app.activity_as_ptr()) }
    {
        say(format!("the picker will not be available: {reason}"));
    }
    let socket = owner.socket.clone();
    // The interface is told where the owner listens. It does not look for it, and nothing about the
    // process's environment is changed to make it find one.
    let mut builder = EventLoop::with_user_event();
    builder.with_android_app(app.clone());
    let event_loop = match builder.build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            say(format!("the event loop could not be made: {error}"));
            return;
        }
    };
    // Where the system says the usable area is, asked afresh each time: it is empty until the window has been laid out and it changes when the bars or
    // the keyboard come and go. The interface turns it into margins at the window's own scale; nothing about the bars is written down here.
    //
    // NEVER from `app.content_rect()`: this is asked from a thread of its own, and that call takes a lock inside the native glue which is destroyed with
    // the activity, so a thread that asked after (or while) the activity went away aborted the process. The activity answers through the bridge, and when
    // it cannot (it is being torn down) the last answer stands, so a passing failure never moves the layout.
    let safe_area: peersus_ui::link::SafeAreaSource = {
        let last = std::sync::Arc::new(std::sync::Mutex::new((0, 0, 0, 0)));
        std::sync::Arc::new(move || {
            let Ok(mut remembered) = last.lock() else {
                return (0, 0, 0, 0);
            };
            if let Some(now) = bridge::usable_area() {
                *remembered = now;
            }
            *remembered
        })
    };
    // The platform's own file picker. The toolkit has no dialog here, and a chosen file is not a path: it comes back
    // as an open file, kept by number in the registry the owner takes from, and sent from where it is.
    let file_picker = peersus_ui::link::FilePicker {
        open: std::sync::Arc::new(|several| {
            if let Err(reason) = bridge::open_picker(several) {
                eprintln!("peersus: the picker could not be opened: {reason}");
            }
        }),
        picked,
    };
    // The platform's camera, for reading the code of another device. It gives frames, which the interface reads.
    let camera = peersus_ui::link::CameraControl {
        open: std::sync::Arc::new(|scanner| {
            if let Err(reason) = camera_bridge::open_camera(scanner) {
                eprintln!("peersus: the camera could not be opened: {reason}");
            }
        }),
        close: std::sync::Arc::new(camera_bridge::close_camera),
        refused: std::sync::Arc::new(camera_bridge::take_refusal),
        turns: std::sync::Arc::new(camera_bridge::turns),
    };
    // Where a person's chosen folder is not a path the program can write, a file is received into a folder of the program's
    // own, and the system puts it in the chosen one when it has arrived whole.
    let staging = directory.join("incoming");
    let folder_sink = peersus_ui::link::FolderSink {
        staging,
        picked: bridge::folders(),
        choose: std::sync::Arc::new(|| {
            if let Err(reason) = bridge::open_folder_picker() {
                eprintln!("peersus: the folder picker could not be opened: {reason}");
            }
        }),
        deliver: std::sync::Arc::new(|handoff| {
            if let Err(reason) = bridge::deliver(handoff) {
                eprintln!("peersus: a received file could not be handed to the folder: {reason}");
                bridge::folders().delivered(handoff.transfer.clone(), false);
            }
        }),
        saved: std::sync::Arc::new(bridge::saved_folder),
        choose_saved: std::sync::Arc::new(|| {
            if let Err(reason) = bridge::open_saved_folder_picker() {
                eprintln!(
                    "peersus: the folder picker for incoming files could not be opened: {reason}"
                );
            }
        }),
        forget_saved: std::sync::Arc::new(|| {
            if let Err(reason) = bridge::forget_saved_folder() {
                eprintln!("peersus: the saved folder could not be let go of: {reason}");
            }
        }),
    };
    // Whether the system is in its dark mode, from the activity's own resources through the bridge (see `bridge::dark_mode_now`). The window the toolkit
    // makes has no theme of its own to report on Android, so without this the first frame (and every one after it) would be drawn light.
    //
    // NEVER from `app.config()`: this is asked from a thread of its own every half second, and that call takes a lock inside the native glue which is
    // destroyed with the activity, so a thread that asked after (or while) the activity went away aborted the process. The first answer, asked before
    // the window exists, is the configuration the application was given; after that the bridge answers, and when it cannot (the activity is being
    // torn down) the last answer stands, so a passing failure never flips the theme.
    let dark: peersus_ui::link::DarkSource = {
        use freya::winit::platform::android::activity::ndk::configuration::UiModeNight;
        let first = match app.config().ui_mode_night() {
            UiModeNight::Yes => Some(true),
            UiModeNight::No => Some(false),
            _ => None,
        };
        let last = std::sync::Arc::new(std::sync::Mutex::new(first));
        let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        std::sync::Arc::new(move || {
            // The very first call is made right here on the thread that owns the application, before any other thread exists: it answers with
            // what was read just above and touches nothing.
            if !started.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return *last.lock().ok()?;
            }
            let mut remembered = last.lock().ok()?;
            if let Some(now) = bridge::dark_mode_now() {
                *remembered = Some(now);
            }
            *remembered
        })
    };
    // The notifications, which the person can block in the system's settings: Settings shows the real state and the way to allow them.
    let notifications = peersus_ui::link::NotificationsControl {
        allowed: std::sync::Arc::new(bridge::notifications_allowed),
        open: std::sync::Arc::new(|| {
            if let Err(reason) = bridge::open_notifications() {
                eprintln!(
                    "peersus: the settings of the notifications could not be opened: {reason}"
                );
            }
        }),
    };
    // Which camera reads a code: the phone lists its back cameras and the person chooses, since the one the program picks is not always the one
    // that sees a screen well.
    let camera_choice = peersus_ui::link::CameraChoice {
        list: std::sync::Arc::new(bridge::back_cameras),
        choose: std::sync::Arc::new(|id| {
            if let Err(reason) = bridge::choose_camera(id) {
                eprintln!("peersus: the camera could not be chosen: {reason}");
            }
        }),
    };
    // The colours the interface is drawn with, which the notification of an offer is drawn with as well, so that it follows the theme.
    let appearance = peersus_ui::link::Appearance {
        set: std::sync::Arc::new(|colours| {
            if let Err(reason) = bridge::offer_appearance(colours) {
                eprintln!("peersus: the colours could not be given to the notification: {reason}");
            }
        }),
    };
    // What the buttons of the notification of an offer ask of the window: View files opens the list of the files when the window comes, and an
    // Accept that needed a window (to ask for a folder) is asked for here, by the ids of the offers.
    let offer_buttons = peersus_ui::link::OfferButtons {
        take_view: std::sync::Arc::new(bridge::take_view_files),
        take_accepts: std::sync::Arc::new(bridge::take_accepted_ids),
    };
    // The permission to draw the panel of an offer over other applications: given by hand on a screen of the system, which Settings shows the
    // state of and opens.
    let overlay = peersus_ui::link::OverlayControl {
        allowed: std::sync::Arc::new(bridge::overlay_allowed),
        open: std::sync::Arc::new(|| {
            if let Err(reason) = bridge::open_overlay_permission() {
                eprintln!(
                    "peersus: the screen of the permission to draw over other applications could not be opened: {reason}"
                );
            }
        }),
    };
    // Whether the system leaves this program out of its battery optimisation, which Settings shows and offers the screen for.
    let battery = peersus_ui::link::BatteryControl {
        unrestricted: std::sync::Arc::new(bridge::battery_unrestricted),
        open: std::sync::Arc::new(|| {
            if let Err(reason) = bridge::open_battery_settings() {
                eprintln!(
                    "peersus: the screen of the battery optimisation could not be opened: {reason}"
                );
            }
        }),
    };
    let platform = peersus_ui::link::Platform {
        safe_area: Some(safe_area),
        file_picker: Some(file_picker),
        camera: Some(camera),
        folder_sink: Some(folder_sink),
        dark: Some(dark),
        notifications: Some(notifications),
        camera_choice: Some(camera_choice),
        appearance: Some(appearance),
        offer_buttons: Some(offer_buttons),
        overlay: Some(overlay),
        battery: Some(battery),
        open_url: Some(peersus_ui::link::UrlOpener(std::sync::Arc::new(
            bridge::open_external_url,
        ))),
        plugin_picker: Some(peersus_ui::link::PluginPicker(std::sync::Arc::new(|| {
            bridge::open_plugin_picker().map_err(|_| "The plugin picker could not be opened")
        }))),
        clipboard: bridge::background_clipboard_supported().then(|| {
            peersus_ui::link::ClipboardControl {
                status: std::sync::Arc::new(bridge::background_clipboard_status),
                open: std::sync::Arc::new(bridge::open_background_clipboard),
            }
        }),
    };
    match peersus_ui::config_at(socket, platform) {
        Ok(config) => {
            // The process-level owner keeps its executor alive across activity transitions.
            launch(
                config
                    .with_plugin(freya_android::AndroidPlugin::new(app))
                    .with_event_loop(event_loop),
            );
        }
        Err(reason) => say(reason.to_owned()),
    }
}
