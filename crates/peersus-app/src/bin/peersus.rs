//! The `peersus` program.
//!
//! The first instance owns the network, the preferences and the plugins, and serves a local
//! control socket. Any later invocation is a client of that owner: it never opens a second
//! network host or a second copy of the saved state.
//!
//! There is no window in this build. `--foreground` and `--background` therefore both run the
//! owner in this terminal until it is stopped (`peersus stop`, Ctrl-C or SIGTERM), and say so;
//! they will differ when a window exists.

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
use std::path::PathBuf;

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
        Output::Reply(Reply::QueueBusy) => {
            return Err("The transfer queue is full; retry when a transfer finishes".to_owned());
        }
        // No command line asks for a picture, and one is not something to print: it is a window's.
        Output::Reply(Reply::Picture(_)) => {
            return Err("A picture is for a window, not a terminal".to_owned());
        }
        // The file as it is held, to be saved or read: a language file is text.
        Output::Reply(Reply::LanguageText(text)) => {
            print!("{text}");
            return Ok(());
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

/// Start this program again from where it was started, in place of this process, so that an
/// update that was put in place runs. No argument is passed: whatever the first start was given
/// is in the saved preferences, and giving it again would undo what the person changed since.
/// Returns only when the program could not be started.
#[cfg(unix)]
fn start_again(program: &std::path::Path) -> String {
    use std::os::unix::process::CommandExt;
    let error = std::process::Command::new(program).exec();
    format!("The application could not be started again: {error}")
}

fn run(cli: Cli, program: Option<PathBuf>) -> Result<Option<PathBuf>, String> {
    let directory = directory()?;
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
    let Some(mut instance) = Instance::acquire(&directory)
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
        let done: Result<Option<PathBuf>, String> = executor.block_on(async {
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
                            serde_json::to_string(&state).map_err(|_| "Cannot encode the state")?
                        );
                        since = generation;
                    }
                }
            }
            output(cli.execute(&control).await?).map(|()| None)
        });
        return done;
    };

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
    let (mut terminate, mut ending, mut interrupt) = {
        let _entered = executor.enter();
        use tokio::signal::windows::{ctrl_c, ctrl_close, ctrl_shutdown};
        (
            ctrl_close().map_err(|_| "Cannot register termination handler")?,
            ctrl_shutdown().map_err(|_| "Cannot register termination handler")?,
            ctrl_c().map_err(|_| "Cannot register interrupt handler")?,
        )
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

    // A test build keeps the identity in memory (a new one each start), so a test can run the
    // program without reading or creating the user's real identity in the system credential
    // store. It must come before the identity is first asked for. A product build has no
    // such line.
    #[cfg(feature = "test-identity")]
    keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
    let key = identity::load(&directory)
        .map_err(|reason| format!("{}{reason}", peersus_app::IDENTITY_STARTUP_ERROR_PREFIX))?;
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
            let application =
                std::env::current_exe().map_err(|_| "Cannot locate module executors")?;
            let modules = peersus_plugin::module::DesktopModules::beside(&application)
                .map_err(|_| "Cannot locate module executors")?;
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
        tokio::select! {
            _ = terminate.recv() => {}
            _ = ending.recv() => {}
            _ = interrupt.recv() => {}
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
    // not started by a window carries no mark, and is never ended here.
    // Every system keeps windows' leases on the control channel, so this is the same everywhere.
    let watcher = peersus_app::lifetime::marked(
        std::env::var_os(peersus_app::lifetime::STARTED_FOR_A_WINDOW).as_deref(),
    )
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
    drop(sockets);
    drop(instance);
    // Everything this owner held is let go; only now may another take its place.
    Ok(match exit {
        Exit::Restart => program,
        Exit::Stopped => None,
    })
}

fn main() -> std::process::ExitCode {
    // Where this program is, asked before anything else: once an update has replaced it, the
    // system no longer answers with the place the new one is at.
    let program = std::env::current_exe().ok();
    let cli = Cli::parse();
    let diagnostics = match peersus_diagnostics::init("owner") {
        Ok(guard) => Some(guard),
        Err(reason) => {
            eprintln!("{reason}");
            None
        }
    };
    match run(cli, program) {
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
        Ok(None) => std::process::ExitCode::SUCCESS,
        Ok(Some(program)) => {
            // What is still to be written is written before this process becomes the next one.
            drop(diagnostics);
            #[cfg(unix)]
            eprintln!("{}", start_again(&program));
            #[cfg(not(unix))]
            eprintln!(
                "An update is in place; start the application again ({})",
                program.display()
            );
            std::process::ExitCode::FAILURE
        }
    }
}
