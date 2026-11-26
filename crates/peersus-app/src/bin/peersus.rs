//! The `peersus` program.
//!
//! The first instance owns the network, the preferences and the plugins, and serves a local
//! control socket. Any later invocation is a client of that owner: it never opens a second
//! network host or a second copy of the saved state.
//!
//! A bare invocation opens the adjacent window. `peersus owner` runs headless.

use clap::Parser;
use peersus_app::{
    cli::{Action, Cli, Output},
    identity,
    instance::Instance,
    runtime::{Exit, Reply, Runtime},
};
use peersus_core::model::Command;
use peersus_net::{
    grants::StandaloneGrants, history::History, network_host::NetworkHost, peers::Peers,
    settings::SettingsStore, transport,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

/// Where the application keeps everything of its own, private to the user.
fn directory() -> Result<PathBuf, String> {
    peersus_app::instance::data_directory()
        .ok_or_else(|| "Application data directory unavailable".to_owned())
}

/// Print what a command returned, for a person or a script.
fn output(value: Output) -> Result<(), String> {
    let value = match value {
        Output::Settings(value) => serde_json::to_value(value),
        Output::Peers(value) => serde_json::to_value(value),
        Output::History(value) => serde_json::to_value(value),
        Output::Plugins(value) => serde_json::to_value(value),
        Output::Store(value) => serde_json::to_value(value),
        Output::Update(value) => serde_json::to_value(value),
        Output::Status(value) => Ok(value),
        // An invitation is the one thing meant to be copied by hand.
        Output::Reply(Reply::Invitation(value)) => {
            println!("{value}");
            return Ok(());
        }
        Output::Reply(Reply::FilesInvitation { code, .. }) => {
            println!("{code}");
            return Ok(());
        }
        Output::Reply(Reply::Transfers(value)) => serde_json::to_value(value),
        Output::Reply(Reply::Applied) => return Ok(()),
        Output::Reply(reply @ Reply::DevelopmentInstalled { .. }) => serde_json::to_value(reply),
        Output::Reply(Reply::MissingPermission { permission }) => {
            Ok(serde_json::json!({"status": "missing-permission", "permission": permission}))
        }
        Output::Reply(Reply::QueueBusy) => {
            return Err("The transfer queue is full; retry when a transfer finishes".to_owned());
        }
        // No command line asks for a picture, and one is not something to print: it is a window's.
        Output::Reply(Reply::Picture(_) | Reply::KeptPicture(_)) => {
            return Err("A picture is for a window, not a terminal".to_owned());
        }
        // The file as it is held, to be saved or read: a language file is text.
        Output::Reply(Reply::LanguageText(text)) => {
            print!("{text}");
            return Ok(());
        }
        // What extensions keep for their drawings is read by a window, to draw them: it is not for a terminal.
        Output::Reply(Reply::LayerIndex(_) | Reply::Layers(_)) => {
            return Err(
                "What extensions keep for their drawings is for a window, not a terminal"
                    .to_owned(),
            );
        }
        // No command line asks what is announced nearby: only what drives the radio does.
        Output::Reply(Reply::Nearby(_)) => {
            return Err("What is announced nearby is for the radio, not a terminal".to_owned());
        }
    }
    .map_err(|_| "Cannot encode command result")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|_| "Cannot encode command result")?
    );
    Ok(())
}

/// Carry out one of the commands that work on the plugin files.
fn plugin_command(directory: &std::path::Path, action: &Action) -> Result<(), String> {
    use peersus_app::plugin_manager::{Installed, Manager, Standing};
    let manager = Manager::new(directory);
    match action {
        Action::PluginInstall {
            source,
            accept,
            accept_outside_store_risk,
        } => {
            let source =
                std::path::absolute(source).map_err(|_| "Could not resolve the package path")?;
            // What a person must read comes first, and is all that is done without both acceptances: the package's licences and notice, its
            // digest, and the warning that a package from a folder is from outside the store. Nothing is installed until the digest of what
            // was read, and the acknowledgement of the risk, are given back.
            let agreements = manager
                .agreement_of(&source)
                .map_err(|refusal| refusal.to_string())?;
            let digest = peersus_core::agreements::digest(&agreements);
            if accept.as_deref() != Some(digest.as_str()) || !*accept_outside_store_risk {
                println!(
                    "{}\n{}",
                    peersus_app::plugin_agreements::OUTSIDE_STORE_WARNING,
                    peersus_app::plugin_agreements::as_text(&agreements)
                );
                println!(
                    "\nTo install it you accept all of the above, the warning included:\n  peersus plugin-install {} --accept {digest} --accept-outside-store-risk",
                    source.display()
                );
                return Err(
                    "Nothing was installed: the licence, the notice and the risk were not accepted"
                        .into(),
                );
            }
            match manager
                .install_accepted(&source, &digest)
                .map_err(|refusal| refusal.to_string())?
            {
                Installed::New(id) => println!(
                    "Installed {id}. It has no permissions yet, whatever an earlier package of that name was allowed; restart the application to use it.",
                ),
                Installed::Unchanged(id) => {
                    println!("{id} is already installed, unchanged.")
                }
            }
        }
        Action::PluginRemove { plugin } => {
            manager
                .remove(plugin)
                .map_err(|refusal| refusal.to_string())?;
            println!(
                "Removed {plugin}. What it was allowed is forgotten when the application starts again; restart it for the plugin to stop."
            );
        }
        Action::PluginList => {
            let entries = manager.list().map_err(|refusal| refusal.to_string())?;
            if entries.is_empty() {
                println!("No plugins are installed.");
            }
            for entry in entries {
                let standing = match entry.standing {
                    Standing::Ready => "installed",
                    Standing::Disabled => "installed, switched off",
                    Standing::Changed => "CHANGED since it was approved",
                    Standing::NeedsApproval => "NOT APPROVED: put there by hand, not installed",
                    Standing::Broken => "BROKEN: cannot be used; remove it",
                };
                println!("{}  {standing}", entry.id);
            }
        }
        _ => return Err("Not a plugin package command".into()),
    }
    Ok(())
}

/// Carry out one of the commands that work on the identity records of the system keyring.
fn identity_command(directory: &std::path::Path, action: &Action) -> Result<(), String> {
    match action {
        Action::IdentityList => {
            let records = identity::list(directory).map_err(str::to_owned)?;
            if records.is_empty() {
                println!("No identity record is in the keyring: the next start makes one.");
            }
            for record in &records {
                println!(
                    "{}  {}",
                    record.fingerprint,
                    if record.in_use { "in use" } else { "" }
                );
            }
            if records.len() > 1 {
                println!(
                    "\nSeveral records exist. Choose one with `peersus identity-use <start of the record>`, or start over with `peersus identity-reset --yes`."
                );
            }
        }
        Action::IdentityUse { record } => {
            let chosen = identity::choose(directory, record).map_err(str::to_owned)?;
            println!("{chosen} is the identity now. Start the application again.");
        }
        Action::IdentityReset { yes } => {
            if !*yes {
                return Err(
                    "Nothing was deleted. This deletes every identity record in the keyring for good and sets the pairings aside; add --yes if that is what you want"
                        .into(),
                );
            }
            identity::reset(directory).map_err(str::to_owned)?;
            println!(
                "The identity records were deleted and the pairings put aside (peers.json.old-*). Start the application: it makes a new identity, and the devices must be paired again."
            );
        }
        _ => return Err("Not an identity command".into()),
    }
    Ok(())
}

/// Preferences are saved and may have changed; only the unsaved service mode survives a restart.
fn restart_command(program: PathBuf, session_service: bool) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.arg("owner");
    if session_service {
        command.arg("--service");
    }
    command
}

/// Run the updated program in place of this process. Returns only on failure.
#[cfg(unix)]
fn start_again(mut command: std::process::Command) -> String {
    use std::os::unix::process::CommandExt;
    let error = command.exec();
    format!("The application could not be started again: {error}")
}

// Leave time for normal service teardown within the stop client's 12-second deadline.
const OWNER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(4);

struct OwnerLifetime {
    // Keep this order even on unwinding: the executor owns work that can still use the data directory.
    executor: tokio::runtime::Runtime,
    instance: Instance,
}

impl OwnerLifetime {
    fn shutdown(self, exit_code: i32) {
        let Self { executor, instance } = self;
        let (finished, waiting) = std::sync::mpsc::channel::<()>();
        let deadline = Instant::now() + OWNER_SHUTDOWN_TIMEOUT;
        let watchdog = std::thread::Builder::new().spawn(move || {
            let left = deadline.saturating_duration_since(Instant::now());
            if waiting.recv_timeout(left) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                Self::end_process(
                    exit_code,
                    b"Application worker shutdown timed out; ending the application\n",
                );
            }
        });
        let watchdog = watchdog.unwrap_or_else(|_| {
            Self::end_process(
                exit_code,
                b"Cannot watch application worker shutdown; ending the application\n",
            )
        });

        // Tokio's shutdown_timeout does not report expiry and can leave blocking workers alive.
        // Join them normally; only process exit may release the lock if they will not finish.
        drop(executor);
        drop(finished);
        let _ = watchdog.join();
        drop(instance);
    }

    fn end_process(exit_code: i32, message: &'static [u8]) -> ! {
        // Stderr may itself be blocked (for example, a full pipe). Give the fixed line a chance,
        // but don't let logging hold the instance lock after the shutdown deadline.
        let (logged, waiting) = std::sync::mpsc::channel::<()>();
        if std::thread::Builder::new()
            .spawn(move || {
                let _ = std::io::Write::write_all(&mut std::io::stderr(), message);
                drop(logged);
            })
            .is_ok()
        {
            let _ = waiting.recv_timeout(Duration::from_millis(100));
        }
        std::process::exit(exit_code)
    }
}

fn run(
    mut cli: Cli,
    program: Option<PathBuf>,
    directory: PathBuf,
) -> Result<Option<std::process::Command>, String> {
    let session_service = cli.service();
    if matches!(cli.action, Some(Action::Owner { .. })) {
        cli.action = None;
    }
    // A launch agent is only a user-session owner. Never open another account's profile,
    // create a key in a daemon's keychain, or prompt on the login screen.
    #[cfg(target_os = "macos")]
    let _no_keychain_ui = if session_service {
        if rustix::process::geteuid().is_root() {
            return Err("A user service cannot run as root".into());
        }
        Some(
            security_framework::os::macos::keychain::SecKeychain::disable_user_interaction()
                .map_err(|_| "Cannot prevent keychain prompts; the service did not start")?,
        )
    } else {
        None
    };
    let executor = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot initialize application executor")?;
    // Plugin packages are files. Installing, removing and listing need nothing an owner
    // holds and must work before one has ever run, so they are carried out here, before the
    // program decides whether it is an owner or a client. The manager's own lock keeps two
    // of them from changing the store at once.
    if let Some(action) = cli.action.as_ref().filter(|a| a.is_local_plugin_command()) {
        plugin_command(&directory, action)?;
        return Ok(None);
    }
    if let Some(action) = cli
        .action
        .as_ref()
        .filter(|a| a.is_local_identity_command())
    {
        identity_command(&directory, action)?;
        return Ok(None);
    }
    // A test build keeps the identity in memory and never opens the system credential store.
    #[cfg(feature = "test-identity")]
    keyring::set_default_credential_builder(keyring::mock::default_credential_builder());

    let (instance, service_key) = if session_service {
        // The session's service waits for the keyring and for another owner to go before it touches the directory.
        // A signal that asks it to stop while it waits ends it with success and starts nothing.
        #[cfg(target_os = "linux")]
        {
            match executor.block_on(peersus_app::service::acquire(&directory))? {
                Some((instance, key)) => (instance, Some(key)),
                None => return Ok(None),
            }
        }
        #[cfg(any(target_os = "macos", windows))]
        {
            match executor.block_on(peersus_app::service::acquire(&directory))? {
                Some(instance) => {
                    let key = identity::load_for_service(&directory).map_err(|reason| {
                        format!("{}{reason}", peersus_app::IDENTITY_STARTUP_ERROR_PREFIX)
                    })?;
                    (instance, Some(key))
                }
                None => return Ok(None),
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            unreachable!("the service flag is not available on this platform")
        }
    } else {
        let Some(instance) = Instance::acquire(&directory)
            .map_err(|_| "Cannot acquire private application directory")?
        else {
            // Another instance owns everything. Started by a window to be the owner (two windows found none at the same
            // moment, and each started one): there is nothing to do and nothing to say, and that window attaches to the
            // other. It ends with success, which is how the window tells this from an owner that could not start.
            if cli.action.is_none()
                && peersus_app::lifetime::marked(
                    std::env::var_os(peersus_app::lifetime::STARTED_FOR_A_WINDOW).as_deref(),
                )
            {
                return Ok(None);
            }
            // Otherwise this is only a client of it.
            let path = directory.join("control.sock");
            let done = executor.block_on(async {
                let control = peersus_app::ipc::LocalControl::new(path);
                if matches!(
                    cli.action,
                    Some(peersus_app::cli::Action::State { watch: true })
                ) {
                    // One compact line per published state, until interrupted or the application stops.
                    let mut since = 0;
                    loop {
                        let (state, generation) = control.watch(since).await?;
                        if generation != since {
                            println!(
                                "{}",
                                serde_json::to_string(&state)
                                    .map_err(|_| "Cannot encode the state")?
                            );
                            since = generation;
                        }
                    }
                }
                output(cli.execute(&control).await?).map(|()| None)
            });
            return done;
        };
        (instance, None)
    };
    let mut lifetime = OwnerLifetime { instance, executor };
    let result = run_owner(cli, directory, service_key, session_service, &mut lifetime);
    // A forced stop is still a stop, but it did not carry out a requested in-place restart.
    let exit_code = i32::from(!matches!(result, Ok(None | Some(Exit::Stopped))));
    lifetime.shutdown(exit_code);
    Ok(match result? {
        Some(Exit::Restart) => program.map(|program| restart_command(program, session_service)),
        Some(Exit::Stopped) | None => None,
    })
}

fn run_owner(
    cli: Cli,
    directory: PathBuf,
    service_key: Option<iroh::SecretKey>,
    session_service: bool,
    lifetime: &mut OwnerLifetime,
) -> Result<Option<Exit>, String> {
    let OwnerLifetime { instance, executor } = lifetime;
    let mut settings = SettingsStore::open(directory.join("preferences.json"))
        .map_err(|error| error.to_string())?;
    // A preferences file that could not be used was put aside and the program carries on with
    // the defaults. Say so where the person can read it, and where it is.
    if let Some(aside) = settings.set_aside() {
        eprintln!(
            "Your saved preferences could not be used and were reset. The old file was kept as {}",
            aside.display()
        );
    }
    // Saving or previewing preferences needs no owner: answer from the saved file.
    if let Some(Action::Settings { save }) = cli.action {
        let candidate = cli
            .preferences
            .patch()
            .apply(settings.current())
            .map_err(|error| error.to_string())?;
        if save {
            settings
                .replace(candidate.clone())
                .map_err(|error| error.to_string())?;
        }
        output(Output::Settings(Box::new(candidate)))?;
        return Ok(None);
    }
    // Every other command is carried out by a running owner, and there is none.
    if cli.action.is_some() {
        return Err("Start the application before sending control commands".into());
    }
    settings
        .update(&cli.preferences.patch())
        .map_err(|error| error.to_string())?;

    #[cfg(unix)]
    let (mut terminate, mut interrupt) = {
        let _entered = executor.enter();
        use tokio::signal::unix::{SignalKind, signal};
        (
            signal(SignalKind::terminate()).map_err(|_| "Cannot register termination handler")?,
            signal(SignalKind::interrupt()).map_err(|_| "Cannot register interrupt handler")?,
        )
    };
    // Windows: the console closing, the system shutting down, and Ctrl-C.
    #[cfg(windows)]
    let console_signals = if session_service {
        None
    } else {
        let _entered = executor.enter();
        use tokio::signal::windows::{ctrl_c, ctrl_close, ctrl_shutdown};
        Some((
            ctrl_close().map_err(|_| "Cannot register termination handler")?,
            ctrl_shutdown().map_err(|_| "Cannot register termination handler")?,
            ctrl_c().map_err(|_| "Cannot register interrupt handler")?,
        ))
    };

    // Only what was installed and is still exactly what was installed is started. The manager
    // is given the DATA directory and makes `plugins/` inside it, so it is never handed the
    // plugins directory itself, which would put the store at `plugins/plugins`.
    let found = peersus_app::plugin_manager::Manager::new(&directory).load();
    // A plugin that could not be loaded is told to the user, by its directory's name and a
    // fixed sentence, and never stops the others or the application.
    for problem in &found.problems {
        eprintln!("plugin {}: {}", problem.plugin, problem.reason);
    }
    // The sockets of plugin processes live beside the rest of the application's state, never
    // in a shared temporary directory: anything that empties /tmp (a cleanup, a person) would
    // unlink the files under the live listeners, and the plugins could never open a data
    // stream again — they were told a path that no longer exists — while their long-lived
    // control connection kept them looking healthy. A plugin that restarts after the
    // directory is emptied would still find nothing to bind to, so the supervisor repairs a
    // missing directory itself (service.rs).
    let sockets = directory.join("run");
    std::fs::create_dir_all(&sockets).map_err(|_| "Cannot create the plugin socket directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sockets, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot make the plugin socket directory private")?;
    }

    let key = match service_key {
        Some(key) => key,
        None => identity::load(&directory)
            .map_err(|reason| format!("{}{reason}", peersus_app::IDENTITY_STARTUP_ERROR_PREFIX))?,
    };
    // When this owner started, for the time it waits for its first window; and the windows attached to it.
    let started = std::time::Instant::now();
    let presence = peersus_app::lifetime::Presence::default();
    let (client, owner, service, own) = executor
        .block_on(async {
            // The token of a personal relay, only when one is used: the credential store is not asked for a secret that nothing will use.
            // One that cannot be read is said and the program carries on without it, as with preferences it could not use: a relay that
            // wants a token will then refuse this device, and the person is told why, and not left with an application that will not start.
            let access = if settings.current().network.has_custom_relay() {
                match peersus_app::relay_token::load() {
                    Ok(access) => access,
                    Err(reason) => {
                        eprintln!("{reason}");
                        None
                    }
                }
            } else {
                None
            };
            let endpoint = transport::bind_with(key, &settings.current().network, access.as_ref())
                .await
                .map_err(|_| "Cannot bind selected network")?;
            // This device as its paired devices know it: what its announcement to them is made from (see `nearby`).
            let own = peersus_core::model::PeerId(endpoint.id().to_string());
            let mut devices = Peers::open(directory.join("peers.json"))
                .map_err(|_| "Cannot read remembered devices")?;
            let mut standalone = StandaloneGrants::open(directory.join("grants.json"))
                .map_err(|_| "Cannot read plugin permissions")?;
            // Before anything is registered: what was allowed to a plugin that has since been
            // removed, or under the name of one installed from the command line since the last
            // start, is not inherited by whatever is installed under that name now.
            peersus_app::plugin_manager::forget_removed(&found, &mut devices, &mut standalone)
                .map_err(|_| "Cannot forget the permissions of removed plugins")?;
            peersus_app::plugin_manager::forget_new_installs(
                &peersus_app::plugin_manager::Manager::new(&directory),
                &mut devices,
                &mut standalone,
            )
            .map_err(|_| "Cannot forget the permissions of removed plugins")?;
            let network = NetworkHost::new(
                endpoint,
                settings.current().device_name.clone(),
                devices,
                History::open(directory.join("history.json"))
                    .map_err(|_| "Cannot read transfer history")?,
            )
            .with_network_settings(&settings.current().network)
            // The pictures live beside everything else of the application's own, private to the user.
            .with_pictures(peersus_net::pictures::Pictures::new(&directory))
            // What the endpoint was bound with, so that saving the same token again is no change and disturbs no connection.
            .with_relay_access(access);
            // Which picture each device showed, so that every device has its face from the first moment and not only once it is online. A
            // file that cannot be read is left exactly as it is and the host starts without it: a picture is not worth a start that
            // does not happen, nor an empty list written over what was there.
            let network = match peersus_net::picture_owners::PictureOwners::open(
                directory.join("picture-owners.json"),
            ) {
                Ok(owners) => network.with_picture_owners(owners),
                Err(_) => network,
            };
            let (runtime, client) = Runtime::new(
                network,
                standalone,
                settings,
                found.registry,
                sockets.clone(),
            )
            .map_err(|_| "Cannot start the application")?;
            let application = std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .map_err(|_| "Cannot locate module executors")?;
            let modules = peersus_plugin::module::DesktopModules::beside(&application)
                .map_err(|_| "Cannot locate module executors")?;
            #[cfg(target_os = "linux")]
            let modules = modules.with_window_mmap_default(
                !session_service
                    && std::env::var_os(peersus_app::lifetime::STARTED_FOR_A_WINDOW).as_deref()
                        == Some("1".as_ref())
                    && std::env::var_os("PEERSUS_LAUNCH_MMAP_DEFAULT").as_deref()
                        == Some("window-owner".as_ref()),
            );
            for helper in modules.missing() {
                eprintln!(
                    "{} is not beside the application: the extensions that need it cannot start",
                    helper
                        .file_name()
                        .unwrap_or(helper.as_os_str())
                        .to_string_lossy()
                );
            }
            // The store of installed packages is the same directory the loader read above and the
            // command line writes: what is installed from the window is what is loaded at start.
            // And what the person switched off is what that same load reported, so it stays off. What was found and is not
            // run (not approved, changed, broken) is told to the owner too, so that the window lists it and it can be
            // removed: a plugin in the store is never invisible.
            let mut runtime = runtime
                .with_modules(std::sync::Arc::new(modules))
                .with_platform_services(peersus_app::desktop_services::DesktopServices::new(
                    sockets.clone(),
                    directory.clone(),
                ))
                .with_packages(peersus_app::plugin_manager::Manager::new(&directory))
                .with_disabled(found.disabled)
                .with_held_back(found.held_back)
                .with_token_store(std::sync::Arc::new(peersus_app::relay_token::Keyring));
            // The extension store. Its rollback floor is durable, so it lives in the application's data
            // directory; what the helper downloads is scratch, so it goes in the private runtime directory.
            // With no maintainer key compiled in, the store reports itself as not set up and refuses
            // everything: there is no fallback that trusts an unsigned catalog.
            {
                let fetcher =
                    peersus_app::store_runtime::HelperFetcher::beside(&application, &sockets)
                        .map_err(|_| "Cannot locate the store helper")?;
                runtime = runtime.with_store(
                    peersus_app::store_service::StoreService::new(
                        peersus_app::store_trust::MAINTAINER_PUBLIC_KEY,
                        &directory,
                    ),
                    std::sync::Arc::new(fetcher),
                );
            }
            // The application's languages, kept in the data directory and fetched through the store's fetcher. The desktop's own
            // language is the environment's until a window says otherwise.
            {
                let mut languages = peersus_app::languages::Languages::new(
                    &directory,
                    peersus_app::languages::RELEASE,
                );
                languages.set_device(peersus_app::languages::environment_language());
                runtime = runtime.with_languages(languages);
            }
            // Updates of the application itself, verified with their own key and applied by
            // replacing the programs beside this one. With no key compiled in they report
            // themselves as not set up; an installation kept by something else says by what.
            {
                use peersus_app::app_update::{self, Installer as _};
                let installer = peersus_app::update_install::ProgramFolder::beside(
                    &application,
                    peersus_app::update_install::managed(),
                )
                .map_err(|_| "Cannot locate the application's programs")?;
                runtime = runtime.with_updates(
                    app_update::UpdateService::new(
                        app_update::UPDATE_PUBLIC_KEY,
                        app_update::CHANNEL,
                        app_update::Build::running(),
                        installer.target(),
                        &directory,
                    ),
                    std::sync::Arc::new(installer),
                );
            }
            // Crash capture of this owner by `peersus-diag` beside it, following the preference (off by default); reports go to
            // the diagnostics folder of the data directory.
            runtime = runtime.with_crash_capture(peersus_app::crash_capture::CrashCapture::new(
                "peersus", &directory,
            ));
            #[cfg(feature = "private-store")]
            {
                runtime = runtime.with_private_store(std::sync::Arc::new(
                    peersus_app::private_store::Desktop {
                        executable: application.clone(),
                        scratch: sockets.clone(),
                        directory: directory.clone(),
                    },
                ))?;
            }
            let listener = instance
                .listen()
                .map_err(|_| "Cannot open local control socket; existing path was preserved")?;
            let owner = tokio::spawn(runtime.run());
            let service = tokio::spawn(peersus_app::ipc::serve(
                listener,
                client.clone(),
                presence.clone(),
            ));
            Ok::<_, &'static str>((client, owner, service, own))
        })
        .map_err(str::to_owned)?;

    let shutdown = client.clone();
    let signals = executor.spawn(async move {
        #[cfg(unix)]
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
        #[cfg(windows)]
        if let Some((mut terminate, mut ending, mut interrupt)) = console_signals {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = ending.recv() => {}
                _ = interrupt.recv() => {}
            }
        } else {
            peersus_app::service::stopping().await;
        }
        let _ = shutdown.command(Command::Shutdown {}).await;
    });
    // Whether this computer sleeps, told to the paired devices. A real suspend only, from the login manager: see `sleep`.
    #[cfg(target_os = "linux")]
    let sleeping = executor.spawn(peersus_app::sleep::follow(client.clone()));
    // The displays of this computer's desktop, told to the paired devices for their layouts: see `displays`.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let arranged = executor.spawn(peersus_app::displays::follow(client.clone()));
    // The pointer and keyboard carried across the layout's edges: see `input_follow`.
    #[cfg(target_os = "linux")]
    let passing = executor.spawn(peersus_app::input_follow::follow(
        client.clone(),
        directory.clone(),
    ));
    // An offer announced outside the window while no window is in front: see `offer_panel`.
    #[cfg(target_os = "linux")]
    let announcing = executor.spawn(peersus_app::offer_panel::follow(
        client.clone(),
        presence.clone(),
        std::env::current_exe().unwrap_or_default(),
    ));
    // Paired devices brought near, found over Bluetooth while the person's switch is on: see `nearby`.
    #[cfg(target_os = "linux")]
    let finding = executor.spawn(peersus_app::nearby::follow(
        client.clone(),
        own,
        peersus_app::nearby::linux::BlueZ::default(),
        {
            let presence = presence.clone();
            move || presence.in_front()
        },
    ));
    #[cfg(not(target_os = "linux"))]
    drop(own);
    // An owner that a window started ends when its last window goes, unless the person asked for the
    // background, however that window ends (closed, killed, crashed): every window keeps a lease on the
    // owner for as long as it lives, and the owner counts them. The preference is read each time, so
    // turning it on or off while a window is open is honoured when the last one goes. An owner that was
    // not started by a window carries no mark, and is never ended here. A session service is such an owner even if a
    // mark somehow reached it: it was started by the system, not by a window.
    // Every system keeps windows' leases on the control channel, so this is the same everywhere.
    let watcher = (!session_service
        && peersus_app::lifetime::marked(
            std::env::var_os(peersus_app::lifetime::STARTED_FOR_A_WINDOW).as_deref(),
        ))
    .then(|| {
        let client = client.clone();
        let presence = presence.clone();
        executor.spawn(async move {
            let mut every = tokio::time::interval(peersus_app::lifetime::WATCH_EVERY);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                every.tick().await;
                let background = client.snapshot().settings.background;
                if peersus_app::lifetime::should_end(
                    true,
                    presence.windows(),
                    presence.ever_had_a_window(),
                    started.elapsed(),
                    background,
                ) {
                    let _ = client.command(Command::Shutdown {}).await;
                    break;
                }
            }
        })
    });
    // The owner ends on `peersus stop`, on a signal, or with the window that started it.
    let exit = executor.block_on(async {
        let exit = owner.await.map_err(|_| "Application owner failed")?;
        signals.abort();
        let _ = signals.await;
        // Let go of the lock on sleep with the owner: a computer must never be held back from sleeping by an owner that is gone.
        #[cfg(target_os = "linux")]
        {
            sleeping.abort();
            let _ = sleeping.await;
            passing.abort();
            let _ = passing.await;
            announcing.abort();
            let _ = announcing.await;
            finding.abort();
            let _ = finding.await;
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            arranged.abort();
            let _ = arranged.await;
        }
        // The watcher of a window-started owner, if this is one.
        if let Some(watcher) = watcher {
            watcher.abort();
            let _ = watcher.await;
        }
        service
            .await
            .map_err(|_| "Local control task failed")?
            .map_err(|_| "Local control service failed")?;
        Ok::<_, String>(exit)
    })?;
    Ok(Some(exit))
}

fn open_window(
    program: Option<&std::path::Path>,
    args: &[std::ffi::OsString],
) -> Result<(), &'static str> {
    #[cfg(target_os = "linux")]
    if ["DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .into_iter()
        .all(|key| std::env::var_os(key).is_none_or(|value| value.is_empty()))
    {
        return Err(
            "No graphical display is available; use `peersus owner` to run without a window",
        );
    }
    let executable = program
        .map(|program| {
            program.with_file_name(format!("peersus-ui{}", std::env::consts::EXE_SUFFIX))
        })
        .filter(|window| window.is_file())
        .ok_or("Cannot find the Peersus window beside the application. Start `peersus owner` once (it puts missing programs of an installed release back), or install again: curl -fsSL https://peersus.axxa.dev/install.sh | sh")?;
    let mut command = std::process::Command::new(executable);
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let _ = command.exec();
        Err("Cannot start the adjacent Peersus window")
    }
    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .map_err(|_| "Cannot start the adjacent Peersus window")?;
        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
        Ok(())
    }
}

/// `peersus install`: the programs beside this one, for this user, where the desktop finds them.
#[cfg(target_os = "linux")]
fn install(
    program: Option<&std::path::Path>,
    prefix: Option<&std::path::Path>,
) -> Result<(), String> {
    use peersus_app::user_install;
    let source = program
        .and_then(std::path::Path::parent)
        .ok_or("Where this program is cannot be found")?;
    let prefix = match prefix {
        Some(prefix) => std::path::absolute(prefix).map_err(|_| "Could not resolve the prefix")?,
        None => std::env::home_dir()
            .ok_or("The home folder is not known; give --prefix")?
            .join(".local"),
    };
    let installed = user_install::install(source, &prefix)?;
    for file in &installed.files {
        println!("{}", prefix.join(file).display());
    }
    if installed.replaced_plain_launcher {
        println!("The launcher of an older layout was a plain file; it is now the link.");
    }
    println!("Peersus is installed. Open it from the applications menu.");
    if !user_install::on_path(&prefix) {
        println!(
            "{} is not where the shell looks for programs; run it by that path, or add that folder to PATH.",
            prefix.join("bin/peersus").display()
        );
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn install(
    _program: Option<&std::path::Path>,
    _prefix: Option<&std::path::Path>,
) -> Result<(), String> {
    Err(peersus_app::user_install::UNSUPPORTED.into())
}

fn main() -> std::process::ExitCode {
    // Where this program is, asked before anything else: once an update has replaced it, the
    // system no longer answers with the place the new one is at.
    let program = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty()
        || matches!(args.as_slice(), [link] if link.to_str().is_some_and(|link| peersus_core::model::store_link_plugin(link).is_some()))
    {
        return match open_window(program.as_deref(), &args) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(reason) => {
                eprintln!("{reason}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let cli = Cli::parse_from(std::iter::once(std::ffi::OsString::from("peersus")).chain(args));
    // Installing works on files beside this program: no owner, no data folder, no log of the owner's.
    if let Some(Action::Install { prefix }) = &cli.action {
        return match install(program.as_deref(), prefix.as_deref()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(reason) => {
                eprintln!("{reason}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let diagnostics = match peersus_diagnostics::init("owner") {
        Ok(guard) => Some(guard),
        Err(reason) => {
            eprintln!("{reason}");
            None
        }
    };
    #[cfg(windows)]
    if cli.install_session_template {
        return match peersus_app::service::install_template() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(reason) => {
                eprintln!("{reason}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    if cli.action.is_none() {
        eprintln!("Supply a subcommand (use `peersus owner` to run without a window)");
        return std::process::ExitCode::FAILURE;
    }
    #[cfg(windows)]
    if cli.service() {
        // SCM supplies the account and logon instance, not a password or a user-supplied profile path.
        return match peersus_app::service::dispatch(|| {
            let cli = Cli::try_parse_from(["peersus", "owner", "--service"])
                .map_err(|e| e.to_string())?;
            let directory = directory()?;
            if peersus_diagnostics::DEVELOPMENT {
                peersus_diagnostics::incident::install("peersus", &directory);
            }
            run(cli, None, directory).map(|_| ())
        }) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(reason) => {
                eprintln!("{reason}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let directory = directory();
    // Development builds: a text report of a panic goes to `<data folder>/diagnostics/`. Inert in every other build.
    if peersus_diagnostics::DEVELOPMENT
        && let Ok(directory) = &directory
    {
        peersus_diagnostics::incident::install("peersus", directory);
    }
    match directory.and_then(|directory| run(cli, program, directory)) {
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
        Ok(None) => std::process::ExitCode::SUCCESS,
        Ok(Some(command)) => {
            // What is still to be written is written before this process becomes the next one.
            drop(diagnostics);
            #[cfg(unix)]
            eprintln!("{}", start_again(command));
            #[cfg(not(unix))]
            eprintln!(
                "An update is in place; start the application again ({})",
                std::path::Path::new(command.get_program()).display()
            );
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn stop_without_an_owner_is_refused_without_starting_one() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("peersus");
        let cli = Cli::try_parse_from(["peersus", "stop"]).unwrap();
        assert_eq!(
            run(cli, None, directory.clone()).unwrap_err(),
            "Start the application before sending control commands"
        );
        assert!(Instance::acquire(&directory).unwrap().is_some());
        assert!(!directory.join("control.sock").exists());
    }

    #[test]
    fn stop_releases_the_lock_even_if_a_blocking_worker_never_returns() {
        use std::{
            io::Read,
            process::{Child, Command, Stdio},
            time::{Duration, Instant},
        };

        struct StuckOwner(Child);
        impl Drop for StuckOwner {
            fn drop(&mut self) {
                if matches!(self.0.try_wait(), Ok(None)) {
                    let _ = self.0.kill();
                }
                let _ = self.0.wait();
            }
        }

        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("peersus");
        let socket = directory.join("control.sock");
        let mut owner = StuckOwner(
            Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "tests::stuck_blocking_owner"])
                .env_clear()
                .env("PEERSUS_TEST_STUCK_OWNER_DATA", &directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let ready = Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            assert!(
                owner.0.try_wait().unwrap().is_none(),
                "owner exited at startup"
            );
            assert!(Instant::now() < ready, "owner never opened its socket");
            std::thread::sleep(Duration::from_millis(20));
        }

        let began = Instant::now();
        let cli = Cli::try_parse_from(["peersus", "stop"]).unwrap();
        assert!(run(cli, None, directory.clone()).unwrap().is_none());
        assert!(
            began.elapsed() >= OWNER_SHUTDOWN_TIMEOUT,
            "stop did not wait for the worker deadline"
        );
        assert!(Instance::acquire(&directory).unwrap().is_some());
        let status = loop {
            assert!(
                began.elapsed() < Duration::from_secs(10),
                "owner did not end before the client deadline"
            );
            if let Some(status) = owner.0.try_wait().unwrap() {
                break status;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success());
        let mut stderr = String::new();
        owner
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert_eq!(
            stderr,
            "Application worker shutdown timed out; ending the application\n"
        );
    }

    #[test]
    #[ignore = "subprocess fixture for a blocked owner"]
    fn stuck_blocking_owner() {
        use tokio::io::AsyncReadExt;

        let directory = PathBuf::from(std::env::var_os("PEERSUS_TEST_STUCK_OWNER_DATA").unwrap());
        let mut lifetime = OwnerLifetime {
            executor: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap(),
            instance: Instance::acquire(&directory).unwrap().unwrap(),
        };
        let (ready, started) = tokio::sync::oneshot::channel();
        let _worker = lifetime.executor.spawn_blocking(move || {
            ready.send(()).unwrap();
            loop {
                std::thread::park();
            }
        });
        lifetime.executor.block_on(async {
            started.await.unwrap();
            let listener = lifetime.instance.listen().unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            let size = stream.read_u32().await.unwrap() as usize;
            let mut input = [0_u8; 256];
            stream.read_exact(&mut input[..size]).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&input[..size]).unwrap(),
                serde_json::json!({"operation": "Command", "command": {"command": "shutdown"}})
            );
        });
        lifetime.shutdown(0);
        panic!("the owner returned while the blocking worker was alive");
    }

    #[test]
    fn the_owner_keeps_its_lock_until_blocking_file_work_finishes() {
        use std::{io::Write, sync::mpsc, time::Duration};

        const WAIT: Duration = Duration::from_secs(5);
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("peersus");
        let selected = directory.join("selected");
        std::thread::scope(|threads| {
            let (directory, selected) = (&directory, &selected);
            let (worker_started, started) = mpsc::channel();
            let (finish_work, wait_for_finish) = mpsc::channel();
            let (task_alive, task_dropped) = mpsc::channel::<()>();
            let owner = threads.spawn(move || {
                let instance = Instance::acquire(directory).unwrap().unwrap();
                let executor = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .build()
                    .unwrap();
                let lifetime = OwnerLifetime { instance, executor };
                let mut file = std::fs::File::create(selected).unwrap();
                let _worker = lifetime.executor.spawn_blocking(move || {
                    worker_started.send(()).unwrap();
                    if wait_for_finish.recv().is_ok() {
                        file.write_all(b"finished").unwrap();
                    }
                });
                let _task = lifetime.executor.spawn(async move {
                    let _alive = task_alive;
                    std::future::pending::<()>().await;
                });
                started.recv_timeout(WAIT).unwrap();
                // A timeout here must fail the test runner, not make the suite exit with success.
                lifetime.shutdown(1);
            });

            // The executor drops pending async tasks on shutdown, even if a blocking worker is still running.
            assert_eq!(
                task_dropped.recv_timeout(WAIT),
                Err(mpsc::RecvTimeoutError::Disconnected)
            );
            assert!(
                Instance::acquire(directory).unwrap().is_none(),
                "another owner could start while a blocking worker still held a file"
            );
            finish_work.send(()).unwrap();
            owner.join().unwrap();
        });
        assert_eq!(std::fs::read(selected).unwrap(), b"finished");
        assert!(Instance::acquire(&directory).unwrap().is_some());
    }

    #[test]
    fn the_restarted_owner_keeps_only_its_service_mode() {
        for service in [false, true] {
            let mut original = vec!["peersus", "--background", "owner"];
            if service {
                original.push("--service");
            }
            let first = Cli::try_parse_from(original).unwrap();
            let command = restart_command("peersus".into(), first.service());
            let restarted = Cli::try_parse_from(
                std::iter::once(command.get_program()).chain(command.get_args()),
            )
            .unwrap();
            assert_eq!(restarted.service(), service);
            assert_eq!(
                restarted.preferences.patch(),
                peersus_core::config::SettingsPatch::default()
            );
        }
    }
}
