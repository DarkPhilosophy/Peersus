//! The program's executor and the owner it found: where the owner listens, starting it when it is not there, healing a window that
//! cannot reach it, and the read-only demo.
use super::*;

/// The program's executor and what it found when it looked for the owner. Kept alive for as
/// long as the window is open: dropping the executor ends the connection's background task.
pub struct Session {
    // Held only so that it is dropped last, with the window.
    _executor: tokio::runtime::Runtime,
    pub link: Result<Link, Offline>,
    /// What the platform offers beyond the window: where the bars are, a picker for files, a camera. Empty on a desktop.
    pub platform: Platform,
    pub system_theme: tokio::sync::watch::Receiver<Option<bool>>,
    /// Where the window opens: `None` is the usual rule (the Send screen, or what
    /// `PEERSUS_PAGE` says for a screenshot). Only the read-only demo sets it.
    pub(crate) start: Option<(crate::Page, crate::view::SettingsTab)>,
    /// The way this window says to the owner whether it is in front of the person, which decides whether an offer is announced
    /// outside the window. `None` where the window does not say: a platform with its own announcement (Android), the demo, a panel.
    pub(crate) front: Option<ipc::Front>,
}

#[cfg(not(target_os = "android"))]
pub(super) fn control_socket() -> Result<PathBuf, Offline> {
    peersus_app::instance::data_directory()
        .map(|directory| directory.join("control.sock"))
        .ok_or(Offline::NoDirectory)
}

/// Where this program was started from, asked when the session opens: once an update has replaced
/// the program, the system no longer answers with the place the new one is at.
#[cfg(all(unix, not(target_os = "android")))]
pub(super) static STARTED_FROM: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// The name under which the times of the starts nobody asked for are handed to the program that takes this one's
/// place. Read by this program only; anything in it that is not such a time is ignored.
#[cfg(not(target_os = "android"))]
pub(super) const HEALED: &str = "PEERSUS_HEALED";
/// How many times a window starts the application again by itself within [`HEALS_WITHIN`] seconds. One more and it
/// stops and says so: an owner that ends every time it starts is not cured by starting it for ever.
#[cfg(not(target_os = "android"))]
pub(super) const HEALS: usize = 3;
#[cfg(not(target_os = "android"))]
pub(super) const HEALS_WITHIN: u64 = 300;

/// Whether one more start by itself is allowed at `now` (seconds), given the times of the earlier ones as they
/// were handed on: `Some` with the times to hand on, this one added, or `None` when [`HEALS`] were made within
/// [`HEALS_WITHIN`]. Times that are older, in the future (a clock set back) or not numbers do not count.
#[cfg(not(target_os = "android"))]
pub(super) fn heal_budget(before: Option<&str>, now: u64) -> Option<String> {
    let mut times: Vec<u64> = before
        .unwrap_or_default()
        .split(',')
        .filter_map(|time| time.parse().ok())
        .filter(|time| *time <= now && now - time < HEALS_WITHIN)
        .collect();
    if times.len() >= HEALS {
        return None;
    }
    times.push(now);
    Some(
        times
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Start this program afresh in place of this window because a person pressed for it: what the button of a window
/// with no owner does. The count of starts nobody asked for begins again. Returns only when it could not be done.
#[cfg(not(target_os = "android"))]
pub fn start_afresh() {
    replace_this_program(None);
}

/// Put this program in this process's place, handing on `healed` (see [`HEALED`]). Returns only when that could
/// not be done.
///
/// The program is the one this window was started from, as for an update: once the file has been replaced, the system no longer
/// names it. On Unix it takes this process's place, where a new process beside one that then ends would be ended with it by
/// whatever started the window as a unit (a service manager ends everything a unit started). Elsewhere a new process is started
/// and this one ends.
#[cfg(not(target_os = "android"))]
pub(super) fn replace_this_program(healed: Option<String>) {
    #[cfg(unix)]
    let program = STARTED_FROM
        .get()
        .cloned()
        .flatten()
        .or_else(|| std::env::current_exe().ok());
    #[cfg(not(unix))]
    let program = std::env::current_exe().ok();
    let Some(program) = program else {
        return;
    };
    let mut command = std::process::Command::new(program);
    command.args(std::env::args_os().skip(1));
    match healed {
        Some(healed) => command.env(HEALED, healed),
        None => command.env_remove(HEALED),
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let _ = command.exec();
    }
    #[cfg(not(unix))]
    if command.spawn().is_ok() {
        std::process::exit(0);
    }
}

/// The task that carries this window's commands to the owner, kept for what it says when it ends: whether the
/// owner was told to stop (see [`Link::owner_ending`]).
#[cfg(not(target_os = "android"))]
pub(super) static BRIDGE: std::sync::Mutex<Option<tokio::task::JoinHandle<bool>>> =
    std::sync::Mutex::new(None);

/// Where the owner program is: in the directory this program is in, under its own name, and
/// nowhere else. It is never looked for on the search path, where something else of that name
/// could be, nor in a build directory of tests.
#[cfg(not(target_os = "android"))]
pub(super) fn owner_next_to(window: &Path) -> Option<PathBuf> {
    let directory = window
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())?;
    Some(directory.join(format!("peersus{}", std::env::consts::EXE_SUFFIX)))
}

/// Whether a failed attempt to connect means "nobody is there", and so an owner should be
/// started. Any other failure (a socket that belongs to someone else, a timeout) is not cured by
/// starting another and would only hide what is wrong.
#[cfg(not(target_os = "android"))]
pub(super) fn should_start_owner(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// Whether this run may start an owner. A test or a screenshot sets `PEERSUS_NO_AUTOSTART` so
/// that a window pointed at a throwaway directory can never launch one on the person's own.
#[cfg(not(target_os = "android"))]
pub(super) fn autostart_allowed(setting: Option<&std::ffi::OsStr>) -> bool {
    setting.is_none_or(|value| value.is_empty())
}

/// Start the owner, which has no window of its own, and leave it running on its own: it outlives
/// this window, as it must to receive while the window is closed. Only `owner` is passed:
/// the owner reads saved preferences rather than writing CLI overrides into them.
///
/// What the owner prints is kept in `log`, not thrown away: when it ends at once, that is the
/// only place the reason can be read, and the person is shown the last line of it.
#[cfg(not(target_os = "android"))]
pub(super) fn start_owner(log: &Path) -> io::Result<std::process::Child> {
    let window = std::env::current_exe()?;
    let owner = owner_next_to(&window)
        .filter(|owner| owner.is_file())
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    let output = open_log(log)?;
    let mut command = std::process::Command::new(owner);
    command
        .arg("owner")
        .stdin(std::process::Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output);
    // Where that log lives, so that the owner's bound on its size can keep the previous generation beside it
    // (`owner.log.1`) instead of emptying it. A file descriptor knows no path.
    command.env(peersus_diagnostics::LOG_FILE, log);
    // Marks the owner as started for a window, so that it ends when its last window goes, unless the person
    // asked for the background. Only that it is set matters. An owner started any other way carries no mark
    // and is never ended by this, and this window never stops one it did not start.
    command.env(peersus_app::lifetime::STARTED_FOR_A_WINDOW, "1");
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    crate::launch_env::owner_env(&mut command);
    // The owner has no window: on Windows it would otherwise open a console of its own.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn()
}

/// Open the owner's log to go on at its end, making its directory first if this is the very first
/// start and there is none yet. The owner makes that directory itself, but only once it runs,
/// and the log has to be there before it does. It is made the way the owner requires, private to
/// the person (a directory that is not is refused). One that already exists is left as it is:
/// mending someone's permissions is not this window's place, and the owner will say so. The file
/// itself is opened as the owner's own bound on its size needs (`peersus_diagnostics::open_log`).
#[cfg(not(target_os = "android"))]
pub(super) fn open_log(log: &Path) -> io::Result<std::fs::File> {
    if let Some(directory) = log.parent().filter(|directory| !directory.exists()) {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(directory)?;
    }
    peersus_diagnostics::open_log(log)
}

/// Where the owner's output is kept, beside the rest of its data.
#[cfg(not(target_os = "android"))]
pub(super) fn owner_log(socket: &Path) -> PathBuf {
    socket.with_file_name("owner.log")
}

impl Session {
    /// Create the watchdog wakeup timer on the existing session executor. Its future is
    /// polled on Freya's event loop, so waking the timer alone never marks UI progress.
    pub(crate) fn diagnostic_clock(&self) -> tokio::time::Interval {
        let _entered = self._executor.enter();
        let mut clock = tokio::time::interval(std::time::Duration::from_secs(1));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        clock
    }

    /// Attach to an owner already listening at `socket`. For an owner in this process, share its
    /// completed Canvas frames without passing them through the control service.
    ///
    /// Returns the error text when not even an executor could be started.
    pub(crate) fn open_at(
        socket: PathBuf,
        platform: Platform,
        canvas_owner: Option<&Client>,
    ) -> Result<Arc<Session>, &'static str> {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| "Cannot start the interface's executor")?;
        let handle = executor.handle().clone();
        // A platform that can say whether the system is dark says it itself, at once and again when it changes; the toolkit's window has no
        // theme to give there and would be drawn light. Elsewhere the desktop's portal is followed.
        let system_theme = match platform.dark.clone() {
            Some(source) => watch_dark(source, std::time::Duration::from_millis(500)),
            None => crate::system_theme::watch(&handle),
        };
        let link = executor
            .block_on(async {
                match canvas_owner {
                    Some(owner) => ipc::attach_with_local_frames(socket, owner).await,
                    None => ipc::attach(socket).await,
                }
            })
            .map(|(client, _task)| Link {
                client,
                executor: handle,
                read_only: false,
            })
            .map_err(|_| Offline::NotStarted(None));
        Ok(Arc::new(Session {
            _executor: executor,
            link,
            platform,
            system_theme,
            start: None,
            front: None,
        }))
    }

    /// Start the executor and attach to the owner, starting it first if nothing is listening.
    /// Returns the error text when not even an executor could be started.
    ///
    /// Not on Android, where the owner is part of the application and [`Session::open_at`] is told where it is.
    #[cfg(not(target_os = "android"))]
    pub fn open() -> Result<Arc<Session>, &'static str> {
        Self::open_with(None)
    }

    /// [`Session::open`] for a window the system opened with an address (a store link, `peersus://store/<id>`): the window
    /// shows what the address asks for once it is attached ([`Platform::opened_with`]).
    #[cfg(not(target_os = "android"))]
    pub fn open_with(opened_with: Option<String>) -> Result<Arc<Session>, &'static str> {
        #[cfg(unix)]
        let _ = STARTED_FROM.set(std::env::current_exe().ok());
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| "Cannot start the interface's executor")?;
        let handle = executor.handle().clone();
        let system_theme = crate::system_theme::watch(&handle);
        let link = control_socket().and_then(|path| {
            let attach = |path: PathBuf| executor.block_on(ipc::attach_window(path));
            let attached = match attach(path.clone()) {
                Ok(attached) => Ok(attached),
                Err(error)
                    if should_start_owner(&error)
                        && autostart_allowed(
                            std::env::var_os("PEERSUS_NO_AUTOSTART").as_deref(),
                        ) =>
                {
                    let log = owner_log(&path);
                    // Where the log ends before this owner starts: what is after it is what this owner said. The file is only
                    // ever appended to, so an earlier run's last line stays at its end and is not this owner's reason.
                    let before = std::fs::metadata(&log).map_or(0, |meta| meta.len() as usize);
                    let reason = |log: &Path, attach_error: &Option<String>| {
                        std::fs::read_to_string(log)
                            .ok()
                            .and_then(|text| last_line_after(&text, before))
                            .or_else(|| attach_error.clone())
                    };
                    match start_owner(&log) {
                        Err(_) => Err(Offline::NotStarted(None)),
                        Ok(mut child) => {
                            // The socket appears a moment after the program starts: ask again
                            // for a bounded while, and stop asking soon after it has ended.
                            let mut result = Err(Offline::NotStarted(None));
                            // What the last try to attach said, kept for when the owner itself said nothing: an owner
                            // that runs but cannot be attached to (the two programs do not understand each other) writes no line.
                            let mut attach_error: Option<String> = None;
                            // Native credential dialogs need time for a person to answer;
                            // the former ten-second wait could abandon a healthy owner.
                            let started = std::time::Instant::now();
                            while started.elapsed() < std::time::Duration::from_secs(90) {
                                std::thread::sleep(std::time::Duration::from_millis(250));
                                match attach(path.clone()) {
                                    Ok(attached) => {
                                        result = Ok(attached);
                                        break;
                                    }
                                    Err(error) => attach_error = Some(attach_failure_words(&error)),
                                }
                                match child.try_wait() {
                                    // Ended with success before it could be attached to: another already was the owner (two
                                    // windows that found none each start one, and only one of the two can be it). This window
                                    // goes on asking for the other, which may still be waiting on a person.
                                    Ok(Some(ended)) if ended.success() => {}
                                    // It ended saying it could not start: what it said is the reason.
                                    Ok(Some(_)) => break,
                                    _ => {}
                                }
                            }
                            if result.is_err() {
                                result = Err(Offline::NotStarted(reason(&log, &attach_error)));
                            }
                            // Waited for whenever it ends, so that it is never left as a process nobody waited for.
                            std::thread::spawn(move || {
                                let _ = child.wait();
                            });
                            result
                        }
                    }
                }
                Err(error) => Err(Offline::NotStarted(Some(attach_failure_words(&error)))),
            };
            attached.map(|(client, bridge, front)| {
                // The bridge keeps the snapshots fresh for as long as the executor lives.
                if let Ok(mut kept) = BRIDGE.lock() {
                    *kept = Some(bridge);
                }
                (
                    Link {
                        client,
                        executor: handle,
                        read_only: false,
                    },
                    front,
                )
            })
        });
        let (link, front) = match link {
            Ok((link, front)) => (Ok(link), Some(front)),
            Err(offline) => (Err(offline), None),
        };
        Ok(Arc::new(Session {
            _executor: executor,
            link,
            platform: Platform {
                opened_with,
                ..Platform::default()
            },
            system_theme,
            start: None,
            front,
        }))
    }

    /// The session of the panel that announces an offer outside the window (see `peersus_app::offer_panel`): attached to the
    /// owner that started it, beside its windows. It is not counted as a window, never starts an owner, and without one there
    /// is nothing for it to show, which is the error.
    #[cfg(target_os = "linux")]
    pub(crate) fn beside() -> Result<Arc<Session>, &'static str> {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| "Cannot start the interface's executor")?;
        let handle = executor.handle().clone();
        let system_theme = crate::system_theme::watch(&handle);
        let path = control_socket().map_err(|_| "The application's directory cannot be found")?;
        let (client, _task) = executor
            .block_on(ipc::attach_beside(path))
            .map_err(|_| "The application is not running")?;
        Ok(Arc::new(Session {
            link: Ok(Link {
                client,
                executor: handle,
                read_only: false,
            }),
            _executor: executor,
            platform: Platform::default(),
            system_theme,
            start: None,
            front: None,
        }))
    }

    /// The read-only demo's session: the same `Link` and the same snapshot channel, held by a
    /// client that answers a fixed synthetic snapshot and refuses every command (see
    /// [`Client::fixture`]). It does not look for an owner, does not start one, and wires no
    /// platform service: no socket, no saved state, no dialog, no clipboard, no D-Bus — the
    /// theme channel's `None` stands for the toolkit's own answer for the demo's whole life.
    ///
    /// `spec` was already judged by [`crate::demo::parse`]. Built only for the `demo` feature
    /// and on the desktop, where its flags exist.
    #[cfg(all(feature = "demo", not(target_os = "android")))]
    pub(crate) fn demo(spec: &crate::demo::Spec) -> Result<Arc<Session>, &'static str> {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| "Cannot start the interface's executor")?;
        let handle = executor.handle().clone();
        if !spec.extension.is_empty() {
            return Self::extension_preview(executor, &spec.extension, spec);
        }
        let (_theme, system_theme) = tokio::sync::watch::channel(spec.dark);
        let mut frames = crate::demo::frames(spec).into_iter().map(Arc::new);
        let first = frames
            .next()
            .unwrap_or_else(|| Arc::new(crate::demo::snapshot_sized(spec.peers, spec.transfers)));
        // There is no owner to ask for the language file: the demo read it, and its words are installed before the first frame.
        if let Some(demo) = &spec.language {
            crate::i18n::take_text(&first.languages, &demo.text);
        }
        // A still demo is a replay that never turns: one snapshot for the whole of its life.
        let every = match spec.replay_hz {
            0 => std::time::Duration::ZERO,
            hz => std::time::Duration::from_secs(1) / hz as u32,
        };
        Ok(Arc::new(Session {
            link: Ok(Link {
                client: Client::replaying(first, frames.collect(), every, &handle),
                executor: handle,
                read_only: true,
            }),
            platform: Platform {
                opened_with: spec.link.clone(),
                ..Platform::default()
            },
            system_theme,
            _executor: executor,
            start: spec.page,
            front: None,
        }))
    }

    /// The demo of extensions as they really run: two owners of the demo's own in a thrown-away folder (this device and a
    /// paired one), each extension's package shown to the first as a package the person chose, and the window driving it. The
    /// person accepts its licences and allows what they want there, as for any install. In the language of `--demo-language`
    /// when one was given, imported into that owner. Nothing of the person's is read or written; the folder goes with the window.
    #[cfg(all(feature = "demo", not(target_os = "android")))]
    fn extension_preview(
        executor: tokio::runtime::Runtime,
        sources: &[std::path::PathBuf],
        spec: &crate::demo::Spec,
    ) -> Result<Arc<Session>, &'static str> {
        use crate::demo_owner;
        let root = tempfile::tempdir().map_err(|_| "The demo's folder could not be made")?;
        let client = executor.block_on(async {
            let (own, _) =
                demo_owner::start(root.path(), "Demo device", Default::default()).await?;
            let (other, _) =
                demo_owner::start(root.path(), "Demo peer", Default::default()).await?;
            demo_owner::pair(&own, &other).await?;
            if let Some(demo) = &spec.language {
                own.command(Command::LanguageImport {
                    text: demo.text.clone(),
                })
                .await?;
                own.command(Command::PatchSettings {
                    patch: Box::new(peersus_core::config::SettingsPatch {
                        language: Some(Some(demo.language)),
                        ..Default::default()
                    }),
                })
                .await?;
            }
            for source in sources {
                demo_owner::stage(&own, source).await?;
            }
            Ok::<_, &'static str>(own)
        })?;
        // The folder lives as long as the executor, which is the window's: its owners' files are in it.
        executor.spawn(async move {
            let _folder = root;
            std::future::pending::<()>().await;
        });
        let (_theme, system_theme) = tokio::sync::watch::channel(spec.dark);
        Ok(Arc::new(Session {
            link: Ok(Link {
                client,
                executor: executor.handle().clone(),
                read_only: false,
            }),
            // A store link given to the demo (`--demo-link`) is shown as the desktop entry shows one: Settings, Extensions, there.
            platform: Platform {
                opened_with: spec.link.clone(),
                ..Platform::default()
            },
            system_theme,
            _executor: executor,
            start: spec.page,
            front: None,
        }))
    }

    /// For a test that drives the real window: a session on an owner that runs in this very process, with an identity
    /// of its own in a directory of the test's, paired with a second such owner. No socket, no platform service and
    /// nothing of the person's. Both owners run on the session's executor, so they end with the window. Returns the
    /// session, its owner, and the other owner.
    #[cfg(all(test, feature = "headless", not(target_os = "android")))]
    pub(crate) fn paired_for_test(root: &Path) -> (Arc<Session>, Client, Client) {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let handle = executor.handle().clone();
        let (own, other) = executor.block_on(async {
            let (own, _) = tests::start_test_owner(root, "this-device", Default::default()).await;
            let (other, _) =
                tests::start_test_owner(root, "other-device", Default::default()).await;
            tests::pair_test_owners(&own, &other).await;
            (own, other)
        });
        let (_theme, system_theme) = tokio::sync::watch::channel(None);
        let session = Arc::new(Session {
            link: Ok(Link {
                client: own.clone(),
                executor: handle,
                read_only: false,
            }),
            platform: Platform::default(),
            system_theme,
            _executor: executor,
            start: None,
            front: None,
        });
        (session, own, other)
    }

    /// For a test of one screen on made-up data of its own: the demo's read-only client serving `snapshot`, but a window that is NOT
    /// read-only, so that it takes a file listed on Send as a real one does. No owner stands behind it: every command it sends is
    /// refused, so nothing the test presses is carried out.
    #[cfg(all(test, feature = "headless", not(target_os = "android")))]
    pub(crate) fn serving(snapshot: peersus_core::model::Snapshot) -> Arc<Session> {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let handle = executor.handle().clone();
        let (_theme, system_theme) = tokio::sync::watch::channel(Some(true));
        Arc::new(Session {
            link: Ok(Link {
                client: Client::fixture(Arc::new(snapshot), &handle),
                executor: handle,
                read_only: false,
            }),
            platform: Platform::default(),
            system_theme,
            _executor: executor,
            start: None,
            front: None,
        })
    }
}
