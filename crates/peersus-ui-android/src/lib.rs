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
mod extension_authorization;
mod extension_clipboard;
mod extension_files;
mod extension_services;
mod input_clipboard;
mod nearby;
mod notifications;
mod plugins;
mod store;
mod text_input;
mod tls_init;
mod update;

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

/// The private folder plugin sockets and store scratch live in: made for this user only, apart from every file of the
/// application. A short name, since a socket path is limited in length.
fn plugin_socket_directory(directory: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let sockets = directory.join("ps");
    match std::fs::DirBuilder::new().mode(0o700).create(&sockets) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "The plugin socket folder could not be made: {error}"
            ));
        }
    }
    let metadata = std::fs::symlink_metadata(&sockets)
        .map_err(|error| format!("The plugin socket folder could not be read: {error}"))?;
    if !metadata.is_dir() {
        return Err("The plugin socket folder is not a folder".to_owned());
    }
    std::fs::set_permissions(&sockets, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("The plugin socket folder could not be made private: {error}"))?;
    Ok(sockets)
}

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
    if let Err(reason) = peersus_diagnostics::init("android") {
        eprintln!("Diagnostic initialization failed: {reason}");
    }
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
            Err(_) => {
                tracing::warn!(
                    code = "relay-access-unavailable",
                    "Personal relay access could not be loaded"
                );
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
        // This device as its paired devices know it: what its announcement to them is made from (see `nearby`).
        let own = peersus_core::model::PeerId(endpoint.id().to_string());
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
        // Plugin sockets are named for the plugin, which chooses its own id: in a folder of their own, so that no id can
        // ever name one of the application's own files (a plugin called `control` once took the owner's socket).
        let plugin_sockets = plugin_socket_directory(directory)?;
        let (runtime, client) =
            Runtime::new(network, standalone, settings, registry, plugin_sockets)
                .map(|(runtime, client)| {
                    use peersus_app::app_update::{self, Installer as _};
                    let installer = update::AndroidInstaller::new(directory);
                    let updates = app_update::UpdateService::new(
                        app_update::UPDATE_PUBLIC_KEY,
                        app_update::CHANNEL,
                        app_update::Build::running(),
                        installer.target(),
                        directory,
                    );
                    (
                        runtime
                            .with_held(held)
                            .with_modules(Arc::new(plugins::AndroidModules))
                            .with_platform_services(Arc::new(
                                extension_services::AndroidServices::new(directory.to_path_buf()),
                            ))
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
                            .with_held_back(found.held_back)
                            .with_updates(updates, Arc::new(installer))
                            .with_languages(peersus_app::languages::Languages::new(
                                directory,
                                peersus_app::languages::RELEASE,
                            )),
                        client,
                    )
                })
                .map_err(|_| "The application could not start".to_owned())?;
        #[cfg(feature = "private-store")]
        let runtime = runtime
            .with_private_store(Arc::new(store::PrivatePlatform(directory.to_path_buf())))
            .map_err(str::to_owned)?;
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
        tokio::spawn(tell_screen(client.clone()));
        tokio::spawn(tell_language(client.clone()));
        tokio::spawn(peersus_app::nearby::follow(
            client.clone(),
            own,
            nearby::PhoneRadio::default(),
            bridge::window_in_front,
        ));
        let client_for_serve = client.clone();
        tokio::spawn(async move {
            if let Err(error) = ipc::serve(listener, client_for_serve, Presence::default()).await {
                tracing::error!(kind = ?error.kind(), "The local control service stopped");
            }
        });
        Ok::<_, String>((socket, client))
    })
}

/// The phone's screen, told as its one display, so that a computer's layout of devices can place it and move its pointer onto
/// it; told again when it changes (the phone is turned), and taken back (no displays) while the remote control service is off,
/// so that no computer is invited to move a pointer this phone cannot show. Asked every few seconds: Android says no change of
/// size to a process without a window, and a few seconds late is no harm to a layout.
async fn tell_screen(client: peersus_app::runtime::Client) {
    // `None` until something was told: the first answer is always said, even "no screen".
    let mut told: Option<Option<(u32, u32)>> = None;
    loop {
        let size = tokio::task::spawn_blocking(bridge::screen_size)
            .await
            .ok()
            .flatten();
        if told != Some(size) {
            let displays = size
                .map(|(width, height)| {
                    vec![peersus_core::layout::Display {
                        id: "screen".to_owned(),
                        x: 0,
                        y: 0,
                        width,
                        height,
                    }]
                })
                .unwrap_or_default();
            if client
                .command(peersus_core::model::Command::SetDisplays { displays })
                .await
                .is_ok()
            {
                told = Some(size);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// The phone's own language, told to the owner whenever it changes (a person chose another in the system's settings), so that
/// a person who chose none is shown in it. Asked every few seconds, as the screen is: the system tells no change of
/// configuration to a process without a window.
async fn tell_language(client: peersus_app::runtime::Client) {
    let mut told = None;
    loop {
        let language = tokio::task::spawn_blocking(bridge::device_language)
            .await
            .ok()
            .flatten();
        // Only what the system named: "could not ask" is never told as "no language".
        if language.is_some()
            && told != language
            && client
                .command(peersus_core::model::Command::SetDeviceLanguage { language })
                .await
                .is_ok()
        {
            told = language;
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Observe process-owned snapshots while the activity is paused or absent. Background mode
/// protects idle availability too; receiving an offer does not grant file-acceptance consent.
async fn protect_transfers(client: peersus_app::runtime::Client, staging: PathBuf) {
    let mut snapshots = client.subscribe();
    let mut protected = None;
    let mut notifications = notifications::Tracker::default();
    let mut language = None;
    let mut told_without_window = None;
    let mut told_place = None;
    let mut told_input = None;
    let mut told_posted: Option<String> = None;
    let input_permission = peersus_api::permission::Permission::new(
        "host",
        peersus_app::platform_services::INPUT_CAPABILITY,
    );
    loop {
        let current = Arc::clone(&snapshots.borrow_and_update());
        // The language file the owner says is shown, read here too: notifications are worded while no window is open.
        let mut shown = peersus_ui::i18n::shown(&current.languages);
        if peersus_ui::i18n::needs_text(&current.languages)
            && let Ok(peersus_app::runtime::Reply::LanguageText(text)) = client
                .command(peersus_core::model::Command::LanguageText {})
                .await
        {
            shown = peersus_ui::i18n::take_text(&current.languages, &text);
        }
        if language != Some(shown) && bridge::language(shown.language.tag()).is_ok() {
            language = Some(shown);
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
        // Any ready, live extension holding the application's input permission, outright or for any device, needs accessibility here.
        let input_wanted = input_permission.as_ref().is_some_and(|permission| {
            current.plugins.iter().any(|plugin| {
                plugin.standing == peersus_core::model::Standing::Ready
                    && matches!(
                        plugin.status,
                        peersus_core::model::PluginStatus::Starting
                            | peersus_core::model::PluginStatus::Running
                            | peersus_core::model::PluginStatus::Restarting
                    )
                    && (current
                        .grants
                        .get(&plugin.id)
                        .is_some_and(|grants| grants.allows(permission))
                        || current.peers.iter().any(|peer| {
                            peer.grants
                                .get(&plugin.id)
                                .is_some_and(|grants| grants.allows(permission))
                        }))
            })
        });
        if told_input != Some(input_wanted) && bridge::remote_input_wanted(input_wanted).is_ok() {
            told_input = Some(input_wanted);
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
        // Where the panel of an offer is drawn, told before the offer is announced so that it is drawn there the first time.
        let place = current.settings.notice_place;
        if told_place != Some(place) && bridge::notice_place(place).is_ok() {
            told_place = Some(place);
        }
        notifications.update(
            &current.transfers,
            peersus_ui::view::size_in_words,
            |waiting, completed, newest, files| {
                bridge::transfer_notifications(waiting, completed, newest, files).is_ok()
            },
        );
        // The notifications that reach outside, as the window would draw them, for the system to show while the window is not in front.
        let cards = outside_cards_json(&current);
        if told_posted.as_ref() != Some(&cards) && bridge::posted_notifications(&cards).is_ok() {
            told_posted = Some(cards);
        }
        // An answer given on one of them is the same command as the window's; one the owner no longer holds is refused there.
        for (id, button) in bridge::take_notification_answers() {
            let _ = client
                .command(peersus_core::model::Command::AnswerNotification { id, button })
                .await;
        }
        // A store link that reached the running activity: the owner tells every window, which opens the store at that extension.
        // Anything that is not exactly a store link names nothing and is dropped here.
        for link in bridge::take_store_links() {
            if let Some(plugin) = peersus_core::model::store_link_plugin(&link) {
                let _ = client
                    .command(peersus_core::model::Command::ShowStoreEntry { plugin })
                    .await;
            }
        }
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
            tracing::debug!(
                dropped = dropped.len(),
                waiting = waiting.len(),
                "Stale transfer accepts discarded"
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
                if bridge::deliver(&handoff).is_err() {
                    tracing::warn!(
                        operation = "deliver-file",
                        "Android platform operation failed"
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
            tracing::debug!("Transfer acceptance requests the window");
            if bridge::bring_window_forward().is_err() {
                tracing::warn!(
                    operation = "bring-window-forward",
                    "Android platform operation failed"
                );
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
            () = bridge::notification_answered().notified() => {}
            () = bridge::store_link_arrived().notified() => {}
            () = bridge::platform_ready().notified() => {
                protected = None;
                told_input = None;
                told_posted = None;
                notifications.resend();
            }
            () = bridge::asleep_changed().notified() => {
                let asleep = bridge::asleep_now();
                let told = client.command(peersus_core::model::Command::SetAsleep { asleep }).await;
                tracing::debug!(asleep, accepted = told.is_ok(), "Screen state reported to owner");
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

/// The cards the window would draw for the notifications that reach outside (`view::outside_cards`), as the JSON Java reads. The
/// application's own words are put in the person's language here, as the window does; an extension's are shown as it wrote them.
fn outside_cards_json(snapshot: &peersus_core::model::Snapshot) -> String {
    let language = snapshot.languages.shown;
    let words = |own: bool, text: &str| -> String {
        if own {
            peersus_ui::i18n::translate(language, text).to_owned()
        } else {
            text.to_owned()
        }
    };
    let cards: Vec<serde_json::Value> = peersus_ui::view::outside_cards(snapshot)
        .into_iter()
        .map(|card| {
            let own = card.from.is_none();
            serde_json::json!({
                "id": card.id,
                "from": card.from,
                "text": words(own, &card.text),
                "buttons": card.buttons.iter().map(|button| serde_json::json!({
                    "id": button.id,
                    "label": words(own, &button.label),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::Value::Array(cards).to_string()
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
    if let Err(reason) = peersus_diagnostics::init("android") {
        eprintln!("Diagnostic initialization failed: {reason}");
    }

    let Some(directory) = app.internal_data_path() else {
        tracing::error!(
            code = "data-directory-unavailable",
            "Android UI initialization failed"
        );
        return;
    };
    // So that the picker can be opened later from any thread: the activity, and the virtual machine it lives in.
    // SAFETY: both pointers are the ones this very `AndroidApp` was started with, and live as long as the process.
    match unsafe { bridge::remember_activity(app.vm_as_ptr(), app.activity_as_ptr()) } {
        // A clipboard request made before this moment was refused for want of an activity, and a guest that was refused its
        // first request waits for a change notice that nothing could cause. Telling it now wakes it to look again: the signal
        // carries no text, and a guest with nobody consenting reads nothing for it.
        Ok(()) => extension_clipboard::announce_change(),
        Err(_) => tracing::warn!(
            operation = "remember-activity",
            "Android platform operation failed"
        ),
    }
    let owner = match owner_at(&directory) {
        Ok(owner) => owner,
        Err(_) => {
            tracing::error!(
                code = "owner-start-failed",
                "Android UI initialization failed"
            );
            return;
        }
    };
    let picked = bridge::shared(owner.held.clone());
    let socket = owner.socket.clone();
    // The interface is told where the owner listens. It does not look for it, and nothing about the
    // process's environment is changed to make it find one.
    let mut builder = EventLoop::with_user_event();
    builder.with_android_app(app.clone());
    let event_loop = match builder.build() {
        Ok(event_loop) => event_loop,
        Err(_) => {
            tracing::error!(
                code = "event-loop-unavailable",
                "Android UI initialization failed"
            );
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
            if bridge::open_picker(several).is_err() {
                tracing::warn!(
                    operation = "open-file-picker",
                    "Android platform operation failed"
                );
            }
        }),
        picked,
    };
    // The platform's camera, for reading the code of another device. It gives frames, which the interface reads.
    let camera = peersus_ui::link::CameraControl {
        open: std::sync::Arc::new(|scanner| {
            if camera_bridge::open_camera(scanner).is_err() {
                tracing::warn!(
                    operation = "open-camera",
                    "Android platform operation failed"
                );
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
            if bridge::open_folder_picker().is_err() {
                tracing::warn!(
                    operation = "open-folder-picker",
                    "Android platform operation failed"
                );
            }
        }),
        deliver: std::sync::Arc::new(|handoff| {
            if bridge::deliver(handoff).is_err() {
                tracing::warn!(
                    operation = "deliver-file",
                    "Android platform operation failed"
                );
                bridge::folders().delivered(handoff.transfer.clone(), false);
            }
        }),
        saved: std::sync::Arc::new(bridge::saved_folder),
        choose_saved: std::sync::Arc::new(|| {
            if bridge::open_saved_folder_picker().is_err() {
                tracing::warn!(
                    operation = "open-saved-folder-picker",
                    "Android platform operation failed"
                );
            }
        }),
        forget_saved: std::sync::Arc::new(|| {
            if bridge::forget_saved_folder().is_err() {
                tracing::warn!(
                    operation = "forget-saved-folder",
                    "Android platform operation failed"
                );
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
            if bridge::open_notifications().is_err() {
                tracing::warn!(
                    operation = "open-notification-settings",
                    "Android platform operation failed"
                );
            }
        }),
    };
    // Which camera reads a code: the phone lists its back cameras and the person chooses, since the one the program picks is not always the one
    // that sees a screen well.
    let camera_choice = peersus_ui::link::CameraChoice {
        list: std::sync::Arc::new(bridge::back_cameras),
        choose: std::sync::Arc::new(|id| {
            if bridge::choose_camera(id).is_err() {
                tracing::warn!(
                    operation = "choose-camera",
                    "Android platform operation failed"
                );
            }
        }),
    };
    // The colours the interface is drawn with, which the notification of an offer is drawn with as well, so that it follows the theme.
    let appearance = peersus_ui::link::Appearance {
        set: std::sync::Arc::new(|colours| {
            if bridge::offer_appearance(colours).is_err() {
                tracing::warn!(
                    operation = "set-notification-appearance",
                    "Android platform operation failed"
                );
            }
        }),
    };
    // What the notification of an offer asks of the window: a touch of it opens the list of the files when the window comes, and an
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
            if bridge::open_overlay_permission().is_err() {
                tracing::warn!(
                    operation = "open-overlay-settings",
                    "Android platform operation failed"
                );
            }
        }),
    };
    // Whether the system leaves this program out of its battery optimisation, which Settings shows and offers the screen for.
    let battery = peersus_ui::link::BatteryControl {
        unrestricted: std::sync::Arc::new(bridge::battery_unrestricted),
        open: std::sync::Arc::new(|| {
            if bridge::open_battery_settings().is_err() {
                tracing::warn!(
                    operation = "open-battery-settings",
                    "Android platform operation failed"
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
        language_picker: Some(peersus_ui::link::LanguagePicker(std::sync::Arc::new(
            || bridge::open_language_picker().map_err(|_| "The file picker could not be opened"),
        ))),
        // An action on the file chosen on Send names a file this process holds, which no frame may carry: it is given to the
        // owner here, in process, as shared text is. What the owner refuses it says on the action's own line.
        selected_action: Some(peersus_ui::link::SelectedAction({
            let owner = Arc::clone(&owner);
            Arc::new(move |plugin, action, path, held| {
                let client = owner.client.clone();
                drop(owner._executor.spawn(async move {
                    let _ = client
                        .command(peersus_core::model::Command::InvokeSelectedAction {
                            plugin,
                            action,
                            path,
                            held,
                        })
                        .await;
                }));
            })
        })),
        // Each serves one of the application's services to extensions, and Settings shows it only while an installed extension
        // uses that service. The words a person may look for them by name the system-level providers this adapter works with.
        service_settings: bridge::background_clipboard_supported()
            .then(|| peersus_ui::link::ServiceSetting {
                service: peersus_app::platform_services::CLIPBOARD_SERVICE,
                title: "Background clipboard access",
                search: "Stellar Shizuku optional provider permission background synchronization",
                status: std::sync::Arc::new(bridge::background_clipboard_status),
                open: std::sync::Arc::new(bridge::open_background_clipboard),
                switch: peersus_ui::link::PlatformSwitch {
                    on: std::sync::Arc::new(bridge::background_clipboard_on),
                    set: std::sync::Arc::new(bridge::set_background_clipboard),
                },
            })
            .into_iter()
            .chain(std::iter::once(peersus_ui::link::ServiceSetting {
                service: peersus_app::platform_services::INPUT_SERVICE,
                title: "Remote input accessibility",
                search: "Accessibility Stellar Shizuku automatic recovery remote control",
                status: std::sync::Arc::new(bridge::remote_input_status),
                open: std::sync::Arc::new(bridge::open_remote_input_settings),
                switch: peersus_ui::link::PlatformSwitch {
                    on: std::sync::Arc::new(bridge::remote_input_recovery_on),
                    set: std::sync::Arc::new(bridge::set_remote_input_recovery),
                },
            }))
            .collect(),
        // The store link the activity was started with: this window opens on it once it is attached.
        opened_with: bridge::take_store_link(),
        // The system's Back closes what is open on top of the window; with nothing open, the activity goes behind the others.
        back: Some(peersus_ui::link::BackControl {
            presses: bridge::back_presses(),
            leave: std::sync::Arc::new(bridge::to_background),
            // Asked once: a system that gives Back as a key (before Android 13); if it cannot be asked, the key is taken as Back,
            // so that Back still works there.
            by_key: bridge::call_activity_bool("backComesAsKey").unwrap_or(true),
        }),
    };
    match peersus_ui::config_at(socket, platform) {
        Ok(config) => {
            // The process-level owner keeps its executor alive across activity transitions.
            // The toolkit's own clipboard on Android is a no-op (reads empty, writes nothing), so paste and copy did nothing in every
            // input. This one asks the activity. It is made here, on the thread that runs the interface, because the toolkit keeps
            // its state per thread; it is inserted after the toolkit's own (see `with_global_context`), so it replaces it.
            launch(
                config
                    .with_global_context(freya::clipboard::Clipboard::create(Some(Box::new(
                        input_clipboard::InputClipboard::new(),
                    ))))
                    .with_global_context(text_input::pressed_again())
                    .with_plugin(freya_android::AndroidPlugin::new(app))
                    .with_plugin(text_input::TextInputPlugin)
                    .with_event_loop(event_loop),
            );
        }
        Err(_) => tracing::error!(
            code = "ui-config-unavailable",
            "Android UI initialization failed"
        ),
    }
}
