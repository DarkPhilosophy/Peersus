//! The window's connection to the owner (`peersus`), which holds the network, the saved state and
//! the plugins. The window renders its `Snapshot` and sends it `Command`s; it owns nothing else.
//!
//! The toolkit runs futures on its own event loop, which is not a tokio runtime, and the
//! owner's client needs one. So this program owns an executor and hands the client's work to it;
//! what comes back arrives as the next snapshot, the only thing the screens are drawn from.
use peersus_app::{ipc, runtime::Client};
use peersus_core::model::{Command, FileDestination};
#[cfg(not(target_os = "android"))]
use std::{io, path::Path};
use std::{path::PathBuf, sync::Arc};

mod platform;
mod session;
mod watch;
pub use {platform::*, session::*, watch::*};

/// Why there is no connection, in words for a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offline {
    /// No directory to look in, so no owner can be found.
    NoDirectory,
    /// The owner was connected and has since gone away.
    NotRunning,
    /// The owner could not be started, or started and ended at once. Carries the last thing it
    /// said, when it said anything.
    NotStarted(Option<String>),
}

impl Offline {
    /// A short title and one sentence of explanation, in `language`. The owner's own reason is an English sentence of the
    /// application's, translated like every other.
    pub fn words(&self, language: crate::i18n::Language) -> (String, String) {
        let tr = |english: &str| crate::i18n::translate(language, english).to_owned();
        match self {
            Offline::NoDirectory => (
                tr("Nowhere to look"),
                tr("This account has no data directory, so the application cannot be found."),
            ),
            Offline::NotRunning => (
                tr("Peersus stopped"),
                tr(
                    "The part of Peersus that keeps your devices connected has ended, and starting it again by itself did not keep it running.",
                ),
            ),
            Offline::NotStarted(Some(reason))
                if reason.starts_with(peersus_app::IDENTITY_STARTUP_ERROR_PREFIX) =>
            {
                (
                    tr("Identity unavailable"),
                    tr(&reason[peersus_app::IDENTITY_STARTUP_ERROR_PREFIX.len()..]),
                )
            }
            Offline::NotStarted(reason) => (
                tr("Peersus could not start"),
                match reason {
                    Some(reason) => crate::i18n::format(
                        language,
                        "The part of Peersus that keeps your devices connected did not start. It said: {reason}",
                        &[("reason", &tr(reason))],
                    ),
                    None => tr(
                        "The part of Peersus that keeps your devices connected did not start, and did not say why.",
                    ),
                },
            ),
        }
    }
}

/// The last line of what the owner wrote, cleaned for a screen: no control characters (a log may
/// hold colour codes) and at most 200 characters. `None` if it wrote nothing. The line that says the
/// log was emptied is not something the owner said about itself, and is never the answer.
#[cfg(not(target_os = "android"))]
fn last_line(log: &str) -> Option<String> {
    let line = log
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty() && *line != peersus_diagnostics::LOG_EMPTIED)?;
    let clean: String = line.chars().filter(|c| !c.is_control()).take(200).collect();
    (!clean.trim().is_empty()).then_some(clean)
}

/// The last line of what was written to `log` at or after byte `offset`: what the owner started now said, and
/// nothing an earlier run left in the same file (which is appended to, and would otherwise be shown as the reason
/// for a failure it has nothing to do with). A log shorter than `offset` was emptied since (an owner empties a log
/// that passed its size limit when it starts writing), so all of it was written after: the whole of it is read.
/// An offset inside the log that is not a character boundary says nothing.
#[cfg(not(target_os = "android"))]
fn last_line_after(log: &str, offset: usize) -> Option<String> {
    if offset > log.len() {
        return last_line(log);
    }
    last_line(log.get(offset..)?)
}

/// What a failed try to attach to the owner is called on the screen: a fixed sentence by the kind of the failure, never
/// the system's own text (which can hold a path).
#[cfg(not(target_os = "android"))]
fn attach_failure_words(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => "it did not open its connection".to_owned(),
        io::ErrorKind::TimedOut => "it did not answer in time".to_owned(),
        io::ErrorKind::InvalidData => "it answered in a way this window does not understand (the two programs are not the same version)".to_owned(),
        _ => "the window could not connect to it".to_owned(),
    }
}

/// One connection to the owner and the executor its work runs on. Cheap to clone; every clone is
/// the same connection, so two of them always compare equal.
/// What the window says where a normal run would open a dialog, read the clipboard or touch a
/// file, when the link is read-only. The same sentence the fixture client answers every command
/// with; under a build without `demo` nothing can ever be read-only, but the words are kept
/// identical so the two cannot drift.
#[cfg(feature = "demo")]
pub const READ_ONLY: &str = peersus_app::runtime::DEMO_READ_ONLY;
/// The same sentence for a build without `demo`: present so the guards need no `cfg`, and never
/// reached because no `Link` of such a build can be read-only.
#[cfg(not(feature = "demo"))]
pub const READ_ONLY: &str = "Read-only demo: this window is not connected to an owner";

#[derive(Clone)]
pub struct Link {
    client: Client,
    executor: tokio::runtime::Handle,
    read_only: bool,
}

impl PartialEq for Link {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

/// A code is available before its producer has opened the whole selection.
/// Neither this value nor the channels carrying it may log the bearer code.
pub struct ReadyCode {
    pub code: String,
    pub generation: u64,
    pub expires_at: u64,
    pub total: u32,
    pub names: Vec<String>,
}

pub struct CodeProduction {
    pub ready: tokio::sync::oneshot::Receiver<Result<Option<ReadyCode>, &'static str>>,
    pub finished: tokio::sync::oneshot::Receiver<u32>,
}

/// What the owner said of a send once everything was handed over: how many files it did not take, and the ids of the transfers it made of the rest.
/// The ids are what a screen knows the batch it sent by, from the owner itself and not from a guess; a destination that is a code makes its transfers
/// later (when the code is scanned), and those are known by the code's generation instead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sent {
    pub refused: u32,
    pub transfers: Vec<String>,
}

impl Link {
    /// Send one command and forget the answer: the effect arrives as the next snapshot, and a
    /// refusal shows up there as an error.
    pub fn send(&self, command: Command) {
        let client = self.client.clone();
        self.executor.spawn(async move {
            let _ = client.command(command).await;
        });
    }

    /// The owner ended so that an update runs, and starts again by itself. Wait for the new owner
    /// to answer, then become the new window in place of this one: this program was replaced too.
    /// The future ends only when that could not be done; the window then says the owner stopped,
    /// as for any other ending. Only asks for a state: no owner is started and no lease is held, so
    /// the new owner is never ended by the window that is about to be replaced.
    #[cfg(all(unix, not(target_os = "android")))]
    pub fn follow_restart(&self) -> impl std::future::Future<Output = ()> + 'static {
        let read_only = self.read_only;
        let waiting = self.executor.spawn(async move {
            let (false, Some(program), Ok(socket)) = (
                read_only,
                STARTED_FROM.get().cloned().flatten(),
                control_socket(),
            ) else {
                return;
            };
            // A minute: an owner that asks the system for its identity may wait on a person.
            for _ in 0..240 {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                let answered = ipc::snapshot(&socket).await.is_ok_and(|snapshot| {
                    snapshot.update.status != peersus_core::model::UpdateStatus::Restarting
                });
                if answered {
                    use std::os::unix::process::CommandExt;
                    // Returns only when the program could not be started.
                    let _ = std::process::Command::new(&program)
                        .args(std::env::args_os().skip(1))
                        .exec();
                    return;
                }
            }
        });
        async move {
            let _ = waiting.await;
        }
    }

    /// How the owner this window was attached to ended, asked once the connection has: `Some(true)` because it was
    /// told to (`peersus stop`, a signal it honours), so the application was closed and the window closes with it;
    /// `Some(false)` in any other way, which is an owner to start again. The owner says which on the window's lease
    /// (`ipc::TOLD`), so it is known for an owner this window did not start as well. `None` for anything that is not
    /// a window attached to an owner by [`Session::open`] (a demo, a panel, a test), and when asked a second time:
    /// nothing is known, and nothing is to be done.
    #[cfg(not(target_os = "android"))]
    pub fn owner_ending(&self) -> impl std::future::Future<Output = Option<bool>> + 'static {
        let bridge = BRIDGE.lock().ok().and_then(|mut bridge| bridge.take());
        let waiting = self.executor.spawn(async move {
            // The bridge ends with the owner; bounded all the same.
            Some(
                tokio::time::timeout(std::time::Duration::from_secs(5), bridge?)
                    .await
                    .is_ok_and(|told| told.unwrap_or(false)),
            )
        });
        async move { waiting.await.ok().flatten() }
    }

    /// Start the application again in place of this window, with nobody asking: what a window does when its owner
    /// ended without being told to, so that a person never has to. Returns when it was not done: a surface that
    /// only looks or a run that may not start an owner (a test, a screenshot), too many such starts already (see
    /// [`heal_budget`]), or a program that could not be started. The window then shows that the owner stopped,
    /// with the button to try by hand, and from then on rejoins an owner that is there again (another window
    /// started one, or a person did): looked for every two seconds for as long as this window is open, which is
    /// one try to connect each time and nothing else, and never started from here.
    #[cfg(not(target_os = "android"))]
    pub fn start_again_unasked(&self) {
        if self.read_only || !autostart_allowed(std::env::var_os("PEERSUS_NO_AUTOSTART").as_deref())
        {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        if let Some(healed) = heal_budget(std::env::var(HEALED).ok().as_deref(), now) {
            replace_this_program(Some(healed));
        }
        #[cfg(unix)]
        if let Ok(socket) = control_socket() {
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
                        // Joining an owner is not a start nobody asked for: the count is handed on as it is.
                        replace_this_program(std::env::var(HEALED).ok());
                        // Still here: the program could not be started, and asking again would not change that.
                        return;
                    }
                }
            });
        }
    }

    /// Submit a declared private-text action without exposing its value to guest arguments.
    pub fn invoke_with_secret(
        &self,
        plugin: peersus_core::plugins::PluginId,
        action: String,
        secret: peersus_core::model::ExtensionSecret,
        request: Option<u64>,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static {
        let client = self.client.clone();
        async move {
            let mut updates = client.subscribe();
            let mut secret = Some(secret);
            let mut answered = false;
            if let Some(id) = request {
                client
                    .command(Command::AnswerExtensionPrompt {
                        id,
                        accepted: true,
                        secret: secret.take(),
                        remove: false,
                    })
                    .await
                    .map_err(str::to_owned)?;
                answered = true;
            } else {
                client
                    .command(Command::InvokeAction {
                        plugin: plugin.clone(),
                        action: action.clone(),
                        peer: None,
                    })
                    .await
                    .map_err(str::to_owned)?;
            }
            loop {
                let current = updates.borrow_and_update().clone();
                if secret.is_some()
                    && let Some(prompt) = current
                        .extension_prompts
                        .iter()
                        .find(|prompt| prompt.plugin == plugin)
                {
                    client
                        .command(Command::AnswerExtensionPrompt {
                            id: prompt.id,
                            accepted: true,
                            secret: secret.take(),
                            remove: false,
                        })
                        .await
                        .map_err(str::to_owned)?;
                    answered = true;
                    continue;
                }
                if let Some(status) = current.actions.iter().find(|status| {
                    status.plugin == plugin && status.action == action && status.peer.is_none()
                }) {
                    match status.state {
                        peersus_core::model::ActionState::Running => {}
                        peersus_core::model::ActionState::Done if answered => return Ok(()),
                        _ => return Err("The action did not accept the value".into()),
                    }
                }
                updates
                    .changed()
                    .await
                    .map_err(|_| "Application stopped".to_owned())?;
            }
        }
    }

    /// Join the other device's invitation and say how it went: `Ok` once the owner took it up (the connection itself goes on after, and
    /// shows as a snapshot), or the owner's own words for why not (the code is expired, or too many connections are waiting). Not
    /// forgotten as `send` does it: a code that was shown and scanned and then did nothing is a button that seems to do nothing.
    pub fn join(
        &self,
        invitation: String,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static {
        let client = self.client.clone();
        async move {
            client
                .command(Command::JoinInvitation { invitation })
                .await
                .map(|_| ())
                .map_err(|reason| reason.to_string())
        }
    }

    /// Say where a paired device can be found, as typed, or clear it with `None`, and say how it went: `Ok` once the owner took it, or the
    /// owner's own sentence for why not (it is not an IP address or a name, the port is wrong, or no port is known and one must be written).
    /// Not forgotten as `send` does it: an address that was typed and then did nothing is a field that seems to do nothing.
    pub fn set_address(
        &self,
        peer: peersus_core::model::PeerId,
        address: Option<String>,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static {
        let client = self.client.clone();
        async move {
            client
                .command(Command::SetAddress { peer, address })
                .await
                .map(|_| ())
                .map_err(|reason| reason.to_string())
        }
    }

    /// Hand the owner the access token of a personal relay, or an empty one to forget it, and say how it went: `Ok` once it was applied
    /// and kept, or the owner's own sentence for why not (this device cannot keep one, or transfers are going and the network cannot change
    /// now). Not forgotten as `send` does it: a token that was typed and then did nothing must not be told as "sent". The token is a secret,
    /// so it travels as a [`SecretText`] whose `Debug` shows nothing, and it is not kept here.
    pub fn set_relay_token(
        &self,
        token: peersus_core::model::SecretText,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static {
        let client = self.client.clone();
        async move {
            client
                .command(Command::SetRelayToken { token })
                .await
                .map(|_| ())
                .map_err(|reason| reason.to_string())
        }
    }

    #[cfg(feature = "private-store")]
    pub fn configure_private_store(
        &self,
        source: peersus_core::private_store::PrivateCatalog,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static {
        let client = self.client.clone();
        async move {
            client
                .command(Command::ConfigurePrivateStore { source })
                .await
                .map(|_| ())
                .map_err(|reason| reason.to_string())
        }
    }

    /// Apply an edit of the preferences and say how it went, with the owner's own sentence when it would not (a transfer is going, an address is
    /// not usable). For a choice the person makes and then looks at the result of; `send` is for the ones whose effect is the next snapshot.
    pub fn patch_settings(
        &self,
        patch: peersus_core::config::SettingsPatch,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static {
        let client = self.client.clone();
        async move {
            client
                .command(Command::PatchSettings {
                    patch: Box::new(patch),
                })
                .await
                .map(|_| ())
                .map_err(|reason| reason.to_string())
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Whether this window must act on nothing outside itself: no dialog, clipboard, filesystem
    /// or owner — the read-only demo's `Link`. `false` everywhere else, and in a build without
    /// `demo` (where no read-only `Link` can be made) always `false`, so the guards it feeds ask
    /// one question with no `cfg`.
    pub(crate) fn read_only(&self) -> bool {
        self.read_only
    }

    /// Path selections use the same serial, bounded handoff as native descriptors. Each destination is sent every path: a path is opened
    /// again for each, so that they do not share a reading position. Answers with what the owner said of it (see [`Sent`]): how many files it
    /// did not take, and the ids of the transfers it made, which are the batch.
    pub fn send_paths(
        &self,
        destinations: Vec<FileDestination>,
        paths: Vec<PathBuf>,
    ) -> tokio::sync::oneshot::Receiver<Sent> {
        let (finished, answer) = tokio::sync::oneshot::channel();
        let client = self.client.clone();
        self.executor.spawn(async move {
            let mut sent = Sent::default();
            for destination in destinations {
                for paths in crate::view::path_sends(paths.clone()) {
                    let count = u32::try_from(paths.len()).unwrap_or(u32::MAX);
                    match queue_batch(
                        &client,
                        Command::QueueFiles {
                            destination: destination.clone(),
                            paths,
                        },
                    )
                    .await
                    {
                        Ok(ids) => sent.transfers.extend(ids),
                        Err(_) => {
                            sent.refused = sent.refused.saturating_add(count);
                            break;
                        }
                    }
                }
            }
            let _ = finished.send(sent);
        });
        answer
    }

    /// Once selected, files belong to the owner's executor, not to the drawing loop.
    /// Await each handoff before releasing its descriptors or asking for the next batch. Every device is sent every file, each from a
    /// handle of its own (see [`peersus_net::picked::Picked::reopened`]); the number given back is how many files were not handed over,
    /// counted for each device that missed them.
    pub fn send_picked(
        &self,
        peers: Vec<peersus_core::model::PeerId>,
        batches: Batches,
    ) -> tokio::sync::oneshot::Receiver<u32> {
        let (finished, answer) = tokio::sync::oneshot::channel();
        let client = self.client.clone();
        self.executor.spawn(async move {
            let destinations = peers
                .into_iter()
                .map(|peer| FileDestination::Peer { peer })
                .collect();
            let refused = pump_picked(&client, destinations, batches, None).await;
            // The window may be paused or gone; that cannot cancel the handoffs above.
            let _ = finished.send(refused);
        });
        answer
    }

    /// Send files that were gathered and only listed (an Android selection that waits for the person to say where it goes) to the
    /// `destinations` (the devices that were picked, or the one that was brought near) and, when `with_code`, behind a code too: ALL IN ONE
    /// hand-over, so that every destination is given the same held files (the first the files themselves, each other one a set of its own,
    /// see [`peersus_net::picked::Picked::reopened`]) and none is left with files another already took. A code is made BEFORE anything is
    /// queued, and is finished after, as for a code on its own.
    ///
    /// What is gathered is moved out of `gathered` first, so that it lets go of nothing twice. The `CodeProduction` is for the code, if one
    /// was asked for, and says when it is shown; the number is how many files were not handed over, counted for each destination that missed
    /// them.
    pub fn send_gathered(
        &self,
        mut destinations: Vec<FileDestination>,
        with_code: bool,
        mut gathered: Gathered,
    ) -> (Option<CodeProduction>, tokio::sync::oneshot::Receiver<Sent>) {
        let (finished, answer) = tokio::sync::oneshot::channel();
        let (ready, ready_answer) = tokio::sync::oneshot::channel();
        let (code_finished, code_completion) = tokio::sync::oneshot::channel();
        let mut code_finished = Some(code_finished);
        let client = self.client.clone();
        let files = gathered.take_files();
        let picked = gathered.picked.clone();
        self.executor.spawn(async move {
            let mut generation = None;
            if with_code {
                let total = u32::try_from(files.len()).unwrap_or(u32::MAX);
                let names = files
                    .iter()
                    .take(crate::view::NAMED_UNDER_CIRCLE)
                    .map(|(_, name)| name.clone())
                    .collect();
                match begin_code(&client, total, names).await {
                    Ok(code) => {
                        generation = Some(code.generation);
                        destinations.push(FileDestination::Invitation {
                            generation: code.generation,
                        });
                        let _ = ready.send(Ok(Some(code)));
                    }
                    Err(reason) => {
                        // The code could not be made, which says nothing about the devices that were picked: as on a desktop, they are still sent
                        // to, and the code alone is reported as missing. Only with no device either is there nowhere for the held files to go,
                        // and they are let go of, not left held.
                        let _ = ready.send(Err(reason));
                        let _ = code_finished.take().map(|sender| sender.send(total));
                        if destinations.is_empty() {
                            picked.release(&files);
                            let _ = finished.send(Sent {
                                refused: total,
                                transfers: Vec::new(),
                            });
                            return;
                        }
                    }
                }
            }
            let (refused, made) = hand_over(&client, &picked, &destinations, files).await;
            if let Some(generation) = generation {
                let _ = client
                    .command(Command::FinishFilesInvitation {
                        generation,
                        refused,
                    })
                    .await;
                let _ = code_finished.take().map(|sender| sender.send(refused));
            }
            // The window may be paused or gone; that cannot cancel the handoffs above.
            let _ = finished.send(Sent {
                refused,
                transfers: made,
            });
        });
        (
            with_code.then_some(CodeProduction {
                ready: ready_answer,
                finished: code_completion,
            }),
            answer,
        )
    }

    /// One capability for the whole choice; bounded batches never replace or renew it.
    pub fn code_paths(&self, paths: Vec<PathBuf>) -> CodeProduction {
        let (ready, answer) = tokio::sync::oneshot::channel();
        let (finished, completion) = tokio::sync::oneshot::channel();
        let client = self.client.clone();
        self.executor.spawn(async move {
            let Ok(total) = u32::try_from(paths.len()) else {
                let _ = ready.send(Err("The selection is too large"));
                let _ = finished.send(u32::MAX);
                return;
            };
            let names = paths
                .iter()
                .take(crate::view::NAMED_UNDER_CIRCLE)
                .filter_map(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .collect();
            let generation = match begin_code(&client, total, names).await {
                Ok(code) => {
                    let generation = code.generation;
                    let _ = ready.send(Ok(Some(code)));
                    generation
                }
                Err(reason) => {
                    let _ = ready.send(Err(reason));
                    let _ = finished.send(total);
                    return;
                }
            };
            let mut refused = 0_u32;
            for paths in crate::view::path_sends(paths) {
                let count = paths.len() as u32;
                if queue_batch(
                    &client,
                    Command::QueueFiles {
                        destination: FileDestination::Invitation { generation },
                        paths,
                    },
                )
                .await
                .is_err()
                {
                    refused = refused.saturating_add(count);
                }
            }
            let _ = client
                .command(Command::FinishFilesInvitation {
                    generation,
                    refused,
                })
                .await;
            let _ = finished.send(refused);
        });
        CodeProduction {
            ready: answer,
            finished: completion,
        }
    }

    /// Take back the files code of `generation` while nobody has scanned it (see `Command::WithdrawFilesInvitation`). Answers whether the
    /// owner took it back: `false` when it was already scanned or is gone, and then nothing of it changed.
    pub fn withdraw_code(&self, generation: u64) -> tokio::sync::oneshot::Receiver<bool> {
        let (taken_back, answer) = tokio::sync::oneshot::channel();
        let client = self.client.clone();
        self.executor.spawn(async move {
            let done = client
                .command(Command::WithdrawFilesInvitation { generation })
                .await
                .is_ok();
            let _ = taken_back.send(done);
        });
        answer
    }

    /// Metadata arrives before descriptors: show the code immediately and keep pumping
    /// on the owner executor even if the window pauses or leaves this page.
    pub fn code_picked(&self, mut batches: Batches) -> CodeProduction {
        let (ready, answer) = tokio::sync::oneshot::channel();
        let (finished, completion) = tokio::sync::oneshot::channel();
        let client = self.client.clone();
        self.executor.spawn(async move {
            let Some(first) = batches.recv().await else {
                let _ = ready.send(Ok(None));
                let _ = finished.send(0);
                return;
            };
            let count = batches.picked.selected_count(batches.generation);
            let Some(total) = count.filter(|count| *count > 0) else {
                batches.picked.release(&first.files);
                let cancelled = first.files.is_empty() && !first.still_open;
                let _ = ready.send(if cancelled {
                    Ok(None)
                } else {
                    Err("The file picker did not report its selection count")
                });
                let _ = finished.send(first.refused);
                return;
            };
            let names = first
                .files
                .iter()
                .take(crate::view::NAMED_UNDER_CIRCLE)
                .map(|(_, name)| name.clone())
                .collect();
            let generation = match begin_code(&client, total, names).await {
                Ok(code) => {
                    let generation = code.generation;
                    let _ = ready.send(Ok(Some(code)));
                    generation
                }
                Err(reason) => {
                    batches.picked.release(&first.files);
                    let _ = ready.send(Err(reason));
                    let _ = finished.send(total);
                    return;
                }
            };
            let refused = pump_picked(
                &client,
                vec![FileDestination::Invitation { generation }],
                batches,
                Some(first),
            )
            .await;
            let _ = client
                .command(Command::FinishFilesInvitation {
                    generation,
                    refused,
                })
                .await;
            let _ = finished.send(refused);
        });
        CodeProduction {
            ready: answer,
            finished: completion,
        }
    }

    /// Run `job`, which may block, on the program's own executor, and give back a receiver for
    /// what it returns. The window's event loop is not a tokio runtime, so a blocking job cannot
    /// be started from it directly; the receiver, however, can be awaited from anywhere.
    pub fn blocking<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> tokio::sync::oneshot::Receiver<T> {
        run_blocking(&self.executor, job)
    }
}

async fn begin_code(
    client: &Client,
    total: u32,
    names: Vec<String>,
) -> Result<ReadyCode, &'static str> {
    match client
        .command(Command::BeginFilesInvitation { count: total })
        .await?
    {
        peersus_app::runtime::Reply::FilesInvitation {
            code,
            generation,
            expires_at,
        } => Ok(ReadyCode {
            code,
            generation,
            expires_at,
            total,
            names,
        }),
        _ => Err("The application answered with something else than a code"),
    }
}

/// Hand one set of held files to every destination, once. The first destination is handed the files themselves; each other one is handed a
/// set of its own, made BEFORE anything is queued (a send takes its files from the registry, and a handle that was only duplicated would
/// share its reading position with the first). A destination whose set cannot be made is given nothing of this set, never part of it, and its
/// files are counted as not handed over; the other destinations are not held back by it. Returns how many files were not handed over.
async fn hand_over(
    client: &Client,
    picked: &peersus_net::picked::Picked,
    destinations: &[FileDestination],
    files: Vec<(u64, String)>,
) -> (u32, Vec<String>) {
    let mut refused = 0_u32;
    // The ids of the transfers the owner made of what was handed over: the batch, by what the owner itself says.
    let mut made: Vec<String> = Vec::new();
    let mut owned = Gathered::new(picked.clone());
    owned.add(files);
    // One set of files for each destination: the held ones for the first, copies for the others.
    let mut sets: Vec<Result<Vec<(u64, String)>, String>> = Vec::with_capacity(destinations.len());
    for index in 0..destinations.len() {
        sets.push(if index == 0 {
            Ok(owned.files.clone())
        } else {
            picked.reopened(&owned.files)
        });
    }
    for (destination, set) in destinations.iter().zip(sets) {
        let set = match set {
            Ok(set) => set,
            Err(_) => {
                tracing::warn!(
                    files = owned.files.len(),
                    code = "held-reopen-failed",
                    "Files could not be reopened for another destination"
                );
                refused = refused.saturating_add(owned.files.len() as u32);
                continue;
            }
        };
        let mut handed = Gathered::new(picked.clone());
        handed.add(set);
        for files in crate::view::held_sends(&handed.files, "") {
            // Retain the guard and exact file numbers throughout backpressure.
            let count = files.len() as u32;
            match queue_batch(
                client,
                Command::QueueHeld {
                    destination: destination.clone(),
                    files,
                },
            )
            .await
            {
                Ok(ids) => made.extend(ids),
                Err(_) => refused = refused.saturating_add(count),
            }
        }
        // What the owner took is gone from the registry and is nothing to let go of; what it did not take is closed when `handed`
        // ends here, copies and originals alike.
    }
    (refused, made)
}

/// Hand every batch the picker gave to every destination, as each arrives (see [`hand_over`]).
async fn pump_picked(
    client: &Client,
    destinations: Vec<FileDestination>,
    mut batches: Batches,
    mut first: Option<peersus_net::picked::Taken>,
) -> u32 {
    let mut refused = 0_u32;
    loop {
        let taken = match first.take() {
            Some(taken) => taken,
            None => match batches.recv().await {
                Some(taken) => taken,
                None => break,
            },
        };
        let (handed_refused, _made) =
            hand_over(client, &batches.picked, &destinations, taken.files).await;
        refused = refused.saturating_add(handed_refused);
        refused = refused.saturating_add(taken.refused);
        if !taken.still_open {
            break;
        }
    }
    refused
}

/// Subscribe before trying, so capacity released during the command cannot be missed.
/// A busy reply publishes no snapshot and consumes no source. Returns the ids of the transfers the owner created, which are what a screen
/// knows the batch it sent by: for a destination that is a code the owner queues the files behind it and says none, and the transfers come
/// of that code (their `code` is its generation).
async fn queue_batch(client: &Client, command: Command) -> Result<Vec<String>, &'static str> {
    let mut updates = client.subscribe();
    loop {
        match client.command(command.clone()).await? {
            peersus_app::runtime::Reply::QueueBusy => {
                updates.changed().await.map_err(|_| "The owner stopped")?;
            }
            peersus_app::runtime::Reply::Transfers(ids) => return Ok(ids),
            _ => return Err("Unexpected file handoff reply"),
        }
    }
}

/// [`Link::blocking`] on any executor handle, so that it can be tried without a connection.
fn run_blocking<T: Send + 'static>(
    executor: &tokio::runtime::Handle,
    job: impl FnOnce() -> T + Send + 'static,
) -> tokio::sync::oneshot::Receiver<T> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    executor.spawn_blocking(move || {
        // The window may have gone while the job ran: then there is nobody to tell.
        let _ = sender.send(job());
    });
    receiver
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;
    use std::path::Path;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_private_input_action_returns_without_leaving_a_prompt() {
        let root = tempfile::tempdir().unwrap();
        let (client, owner) =
            start_test_owner(root.path(), "private-input", Default::default()).await;
        let link = Link {
            client: client.clone(),
            executor: tokio::runtime::Handle::current(),
            read_only: false,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            link.invoke_with_secret(
                peersus_core::plugins::PluginId::new("missing").unwrap(),
                "configure".into(),
                "synthetic-input".to_owned().try_into().unwrap(),
                None,
            ),
        )
        .await;
        let no_prompt = client.snapshot().extension_prompts.is_empty();
        client.command(Command::Shutdown {}).await.unwrap();
        owner.await.unwrap();
        assert!(
            result
                .expect("a refused action stranded its input")
                .is_err()
        );
        assert!(no_prompt);
    }

    #[test]
    fn the_owner_is_looked_for_next_to_the_window_and_nowhere_else() {
        let directory = Path::new("application-directory");
        let owner = owner_next_to(&directory.join("peersus-ui")).unwrap();
        let filename = if cfg!(windows) {
            "peersus.exe"
        } else {
            "peersus"
        };
        assert_eq!(owner, directory.join(filename));
    }

    #[test]
    fn a_blocking_job_runs_on_the_executor_and_its_answer_reaches_a_thread_with_no_runtime() {
        // The window's loop is such a thread: nothing there has entered a tokio runtime.
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "the test thread must have no runtime"
        );
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let here = std::thread::current().id();
        let answer = run_blocking(executor.handle(), move || {
            (std::thread::current().id(), 6 * 7)
        });
        let (ran_on, value) = answer.blocking_recv().expect("the job answered");
        assert_eq!(value, 42);
        assert_ne!(
            ran_on, here,
            "the job ran on the thread that asked, which is the thread that draws"
        );
    }

    fn pathless() -> std::fs::File {
        #[cfg(windows)]
        {
            // Anonymous Windows tempfiles deny reopening; picker files permit a new reader.
            // Keep the fixture unnamed while preserving the real picker's sharing semantics.
            tempfile::NamedTempFile::new().unwrap().reopen().unwrap()
        }
        #[cfg(not(windows))]
        tempfile::tempfile().unwrap()
    }

    #[test]
    fn files_gathered_and_not_taken_are_let_go_of_so_a_refused_batch_never_fills_the_registry() {
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        // A batch is chosen and gathered, and the owner refuses it without taking any (more than it accepts at once).
        picked.opened();
        for n in 0..20 {
            picked.file(pathless(), format!("f{n}"));
        }
        let mut gathered = Gathered::new(picked.clone());
        gathered.add(picked.take().files);
        assert_eq!(held.len(), 20);
        assert_eq!(gathered.files().map(|files| files.len()), Some(20));
        drop(gathered);
        assert!(
            held.is_empty(),
            "a refused batch stayed held: {} files",
            held.len()
        );
        // The next choice, of one file, is not turned away.
        picked.opened();
        picked.file(pathless(), "one".into());
        assert_eq!(picked.take().files.len(), 1);
        assert_eq!(picked.take().refused, 0);
    }

    #[test]
    fn a_number_the_owner_already_took_is_not_let_go_of_twice_and_nothing_gathered_is_none() {
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        assert!(Gathered::new(picked.clone()).files().is_none());
        picked.opened();
        picked.file(pathless(), "a".into());
        picked.file(pathless(), "b".into());
        let taken = picked.take().files;
        let mut gathered = Gathered::new(picked.clone());
        gathered.add(taken.clone());
        // The owner takes the first by its number, as it does when it makes the code.
        assert!(picked.take_file(taken[0].0).is_some());
        drop(gathered);
        assert!(
            held.is_empty(),
            "the one that was not taken stayed held, or the one taken was let go of wrongly"
        );
    }

    #[test]
    fn the_clock_publishes_each_new_second_once_and_not_the_same_second_again() {
        let seconds = Arc::new(std::sync::atomic::AtomicU64::new(100));
        let source = Arc::clone(&seconds);
        let mut ticks = clock(
            move || source.load(std::sync::atomic::Ordering::SeqCst),
            std::time::Duration::from_millis(5),
        );
        assert_eq!(
            *ticks.borrow(),
            100,
            "it did not start from the second it was made in"
        );
        // The same second, however often the thread looks: nothing is published.
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(
            !ticks.has_changed().unwrap(),
            "an unchanged second woke the window"
        );
        // A new second is published, and read as the new value.
        seconds.store(101, std::sync::atomic::Ordering::SeqCst);
        until_true("a new second was never published", || {
            ticks.has_changed().unwrap()
        });
        assert_eq!(*ticks.borrow_and_update(), 101);
        assert!(!ticks.has_changed().unwrap());
    }

    #[test]
    fn the_clock_thread_ends_when_the_window_is_gone() {
        let seconds = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let counted = Arc::clone(&seconds);
        let ticks = clock(
            move || counted.load(std::sync::atomic::Ordering::SeqCst),
            std::time::Duration::from_millis(5),
        );
        // The thread holds one handle to the source, and this test the other: it is let go of when the thread has ended.
        assert_eq!(Arc::strong_count(&seconds), 2);
        drop(ticks);
        until_true(
            "the clock thread stayed alive with nobody reading it",
            || Arc::strong_count(&seconds) == 1,
        );
    }

    /// Wait, polling, until `done` holds: the thread under test runs on its own clock, and a fixed sleep would be a guess.
    fn until_true(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn a_page_left_before_it_read_what_the_picker_handed_over_lets_those_files_go() {
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        picked.opened();
        for n in 0..3 {
            picked.file(pathless(), format!("f{n}"));
        }
        let every = std::time::Duration::from_millis(5);
        let reader = await_picked(picked.clone(), every, std::time::Duration::from_secs(60));
        // Several looks of the thread: the batch has left the picker for the channel, unread, and is still held.
        std::thread::sleep(every * 20);
        assert_eq!(
            held.len(),
            3,
            "the files were let go of while the page could still read them"
        );
        // The page is left: nobody will ever read what is in the channel.
        drop(reader);
        until_true("files nobody read stayed held", || held.is_empty());
        // The next choice is not turned away.
        picked.opened();
        picked.file(pathless(), "one".into());
        assert_eq!(picked.take().refused, 0);
    }

    #[test]
    fn dropping_an_old_picker_watcher_neither_consumes_nor_finishes_the_new_choice() {
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        picked.opened_streaming();
        let old = await_picked(
            picked.clone(),
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(60),
        );
        picked.opened_streaming();
        let generation = picked.generation();
        picked.file_for(generation, pathless(), "new-choice".into());
        drop(old);
        let result = picked.take_for(generation);
        assert!(result.still_open);
        assert_eq!(
            result
                .files
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>(),
            ["new-choice"]
        );
        picked.release(&result.files);
        picked.finished_for(generation);
    }

    #[test]
    fn abandoning_a_streaming_picker_stops_its_producer_and_closes_pending_descriptors() {
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        picked.opened_streaming();
        let generation = picked.generation();
        let reader = await_picked(
            picked.clone(),
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(60),
        );
        picked.file_for(generation, pathless(), "not-sent".into());
        drop(reader);
        until_true("abandoned picker retained a descriptor", || held.is_empty());
        assert_eq!(picked.producer_state(generation), -1);
    }

    #[test]
    fn a_page_that_stays_keeps_what_the_picker_handed_over_until_it_has_dealt_with_it() {
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        picked.opened();
        picked.file(pathless(), "a".into());
        picked.finished();
        let mut reader = await_picked(
            picked.clone(),
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(60),
        );
        let taken = reader.blocking_recv().expect("the batch was not delivered");
        assert_eq!(taken.files.len(), 1);
        assert!(!taken.still_open);
        // The reader has it and the picker is done: the thread ends, and what the reader holds is the reader's to deal with.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            held.len(),
            1,
            "files the page still held were let go of under it"
        );
    }

    #[test]
    fn a_page_left_after_the_picker_finished_but_before_it_read_the_last_batch_lets_those_files_go()
    {
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        picked.opened();
        picked.file(pathless(), "a".into());
        picked.file(pathless(), "b".into());
        // The picker is done before the watcher looks: its one batch is also the last, and the thread ends after sending it.
        picked.finished();
        let every = std::time::Duration::from_millis(5);
        let reader = await_picked(picked.clone(), every, std::time::Duration::from_secs(60));
        // Long enough for the thread to have sent the batch and ended; nobody has read it.
        std::thread::sleep(every * 20);
        assert_eq!(
            held.len(),
            2,
            "the files were let go of while the page could still read them"
        );
        drop(reader);
        until_true(
            "the last batch, never read, stayed held after the thread ended",
            || held.is_empty(),
        );
        picked.opened();
        picked.file(pathless(), "next".into());
        assert_eq!(picked.take().refused, 0);
    }

    #[test]
    fn a_program_with_no_directory_has_no_owner_beside_it() {
        // A bare name has no parent to look in; it must not be searched for on the path.
        assert_eq!(owner_next_to(Path::new("")), None);
    }

    #[test]
    fn the_owner_is_started_only_when_nothing_listens_not_for_any_other_failure() {
        use std::io::ErrorKind::*;
        // Nothing there, or nothing listening: start it.
        assert!(should_start_owner(&io::Error::from(NotFound)));
        assert!(should_start_owner(&io::Error::from(ConnectionRefused)));
        // Someone else's socket, a timeout, anything else: starting a second owner would not
        // fix it and could hide what is wrong.
        assert!(!should_start_owner(&io::Error::from(PermissionDenied)));
        assert!(!should_start_owner(&io::Error::from(TimedOut)));
        assert!(!should_start_owner(&io::Error::other("boom")));
    }

    #[test]
    fn a_screenshot_or_test_run_can_forbid_starting_an_owner() {
        assert!(!autostart_allowed(Some("1".as_ref())));
        assert!(autostart_allowed(None));
        // Only a non-empty value forbids it, so an unset-but-exported empty one does not.
        assert!(autostart_allowed(Some("".as_ref())));
    }

    #[test]
    fn a_window_starts_the_application_again_by_itself_only_so_many_times() {
        // The first three within five minutes are allowed, each handing on the times so far.
        let first = heal_budget(None, 1_000).unwrap();
        assert_eq!(first, "1000");
        let second = heal_budget(Some(&first), 1_010).unwrap();
        let third = heal_budget(Some(&second), 1_020).unwrap();
        assert_eq!(third, "1000,1010,1020");
        // A fourth is not: the owner keeps ending, and the window says so instead.
        assert_eq!(heal_budget(Some(&third), 1_030), None);
        // Once the oldest is more than five minutes back there is room again, and it is forgotten.
        assert_eq!(
            heal_budget(Some(&third), 1_301).as_deref(),
            Some("1010,1020,1301")
        );
        // What is not a time of an earlier start counts for nothing: words, a time ahead of the clock.
        assert_eq!(
            heal_budget(Some("x,,-4,99999999999999999999,5000"), 1_000).as_deref(),
            Some("1000")
        );
    }

    #[test]
    fn the_not_running_screen_names_no_command_line_flag() {
        for offline in [Offline::NotRunning, Offline::NotStarted(None)] {
            let (title, text) = offline.words(crate::i18n::Language::ENGLISH);
            assert!(!text.contains("--"), "{text}");
            assert!(!text.contains("peersus "), "{text}");
            assert!(!title.is_empty());
        }
    }

    #[test]
    fn the_safe_area_watcher_publishes_the_first_answer_and_only_a_change_after_it() {
        use std::sync::atomic::{AtomicI32, Ordering};
        let top = Arc::new(AtomicI32::new(0));
        let source: SafeAreaSource = {
            let top = top.clone();
            Arc::new(move || (0, top.load(Ordering::SeqCst), 1080, 2400))
        };
        let mut receiver = watch_safe_area(source, std::time::Duration::from_millis(10));
        // The first answer is there at once, before the platform has laid anything out: not known yet.
        assert_eq!(*receiver.borrow_and_update(), (0, 0, 1080, 2400));
        // The platform then says where the status bar ends, and the watcher says so.
        top.store(105, Ordering::SeqCst);
        // `changed` is a plain channel wait: no timer, no reactor. This test runs it on a thread with
        // neither, as the interface's tasks do, and that it does not panic is the point of it.
        let waited = std::thread::spawn(move || {
            let waker = std::task::Waker::noop();
            let mut context = std::task::Context::from_waker(waker);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let result = {
                let mut changed = std::pin::pin!(receiver.changed());
                loop {
                    if let std::task::Poll::Ready(result) = changed.as_mut().poll(&mut context) {
                        break result;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the watcher never published the change"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            };
            result.map(|()| *receiver.borrow())
        });
        let seen = waited
            .join()
            .expect("waiting on the channel panicked")
            .expect("the channel closed");
        assert_eq!(seen, (0, 105, 1080, 2400));
    }

    #[test]
    fn the_safe_area_watcher_stops_when_the_window_has_closed() {
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let source: SafeAreaSource = {
            let polls = polls.clone();
            Arc::new(move || {
                polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (0, 0, 0, 0)
            })
        };
        let receiver = watch_safe_area(source, std::time::Duration::from_millis(10));
        // Closing the window drops every receiver: the thread must notice and end, not poll for ever.
        drop(receiver);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let after_close = polls.load(std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            polls.load(std::sync::atomic::Ordering::SeqCst),
            after_close,
            "the watcher kept polling after the window closed"
        );
    }

    #[test]
    fn the_last_line_the_owner_wrote_is_what_the_person_is_told() {
        let log = "starting\nlistening\npreferences are invalid; the existing file was preserved\n";
        assert_eq!(
            last_line(log).as_deref(),
            Some("preferences are invalid; the existing file was preserved")
        );
    }

    #[test]
    fn blank_lines_and_an_empty_log_say_nothing() {
        assert_eq!(last_line(""), None);
        assert_eq!(last_line("\n\n  \n"), None);
        assert_eq!(last_line("first\n\n").as_deref(), Some("first"));
    }

    #[test]
    fn a_very_long_line_is_cut_so_it_cannot_fill_the_screen() {
        let line = last_line(&"x".repeat(5000)).unwrap();
        assert!(
            line.chars().count() <= 200,
            "{} characters",
            line.chars().count()
        );
    }

    #[test]
    fn control_characters_in_the_log_never_reach_the_screen() {
        let line = last_line("bad \u{1b}[31mred\u{7} text").unwrap();
        assert!(line.chars().all(|c| !c.is_control()), "{line:?}");
    }

    #[test]
    fn a_line_an_earlier_run_left_in_the_log_is_never_the_reason_for_this_start() {
        // The log is appended to for ever. The earlier run's last line (a refused invitation, say) stays at its end, and
        // an owner that ended without a word must not be blamed with it.
        let earlier = "peersus: 1793594083 The invitation was refused\n";
        assert_eq!(
            last_line_after(earlier, earlier.len()),
            None,
            "an earlier run's line was shown for a start it had nothing to do with"
        );
        // What this owner wrote after that point is the reason, and only that.
        let now = format!("{earlier}preferences are invalid\n");
        assert_eq!(
            last_line_after(&now, earlier.len()).as_deref(),
            Some("preferences are invalid")
        );
        // From the start of an empty log, the same as the whole log.
        assert_eq!(
            last_line_after("first\nsecond\n", 0).as_deref(),
            Some("second")
        );
    }

    #[test]
    fn an_offset_in_the_middle_of_a_character_says_nothing() {
        // No panic, nothing said.
        assert_eq!(last_line_after("é", 1), None);
    }

    #[test]
    fn a_log_emptied_since_is_read_whole_and_the_line_that_says_so_is_never_the_reason() {
        // The log was longer when this owner was started than it is now: the owner emptied it, so every line in it is
        // this owner's.
        let emptied = format!(
            "{}\npreferences are invalid\n",
            peersus_diagnostics::LOG_EMPTIED
        );
        assert_eq!(
            last_line_after(&emptied, 40 * 1024 * 1024).as_deref(),
            Some("preferences are invalid")
        );
        // An owner that said nothing after emptying it said nothing.
        let only = format!("{}\n", peersus_diagnostics::LOG_EMPTIED);
        assert_eq!(last_line_after(&only, 40 * 1024 * 1024), None);
        assert_eq!(last_line(&only), None);
    }

    #[test]
    fn a_failed_attach_is_told_by_its_kind_and_never_by_the_systems_words() {
        let mismatch = attach_failure_words(&io::Error::new(
            io::ErrorKind::InvalidData,
            "/home/someone/.local/share/peersus/control.sock",
        ));
        assert!(mismatch.contains("not the same version"), "{mismatch}");
        assert!(
            !mismatch.contains("/home"),
            "a path reached the screen: {mismatch}"
        );
        assert!(
            attach_failure_words(&io::Error::from(io::ErrorKind::TimedOut)).contains("in time")
        );
        assert!(
            !attach_failure_words(&io::Error::from(io::ErrorKind::PermissionDenied)).is_empty()
        );
    }

    #[test]
    fn the_screen_for_a_failed_start_carries_the_reason_when_there_is_one() {
        let with = Offline::NotStarted(Some("preferences are invalid".into()))
            .words(crate::i18n::Language::ENGLISH);
        assert!(with.1.contains("preferences are invalid"), "{with:?}");
        let without = Offline::NotStarted(None).words(crate::i18n::Language::ENGLISH);
        assert!(!without.1.is_empty());
    }

    #[test]
    fn a_keyring_failure_is_not_presented_as_peer_offline_or_network_failure() {
        let reason =
            "System keyring unlock was cancelled; existing identity and pairings were not changed";
        let (title, text) = Offline::NotStarted(Some(format!(
            "{}{reason}",
            peersus_app::IDENTITY_STARTUP_ERROR_PREFIX
        )))
        .words(crate::i18n::Language::ENGLISH);
        assert_eq!(title, "Identity unavailable");
        assert!(text.contains(reason));
        assert!(!text.contains("It said:"));
    }

    #[test]
    fn an_unrelated_startup_error_is_not_classified_by_identity_words_in_its_payload() {
        let (title, text) = Offline::NotStarted(Some("plugin rejected an identity field".into()))
            .words(crate::i18n::Language::ENGLISH);
        assert_eq!(title, "Peersus could not start");
        assert!(text.contains("plugin rejected an identity field"));
    }

    #[cfg(unix)]
    #[test]
    fn the_log_opens_on_a_first_start_when_the_data_directory_does_not_exist_yet() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("peersus").join("owner.log");
        let file = open_log(&log).expect("a first start has no directory yet; it must be made");
        drop(file);
        assert!(log.is_file());
        // The owner refuses a directory that is not private to the person, so it is made so.
        let mode = std::fs::metadata(log.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "directory mode is {mode:o}");
        let mode = std::fs::metadata(&log).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "log mode is {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_already_exists_is_used_as_it_is_never_changed() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("peersus");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o750)).unwrap();
        drop(open_log(&directory.join("owner.log")).unwrap());
        // Not the window's place to mend it: the owner will say it is not private.
        let mode = std::fs::metadata(&directory).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o750);
    }

    #[test]
    fn a_log_that_cannot_be_opened_is_an_error_not_a_panic() {
        // Its "directory" is a file.
        let root = tempfile::tempdir().unwrap();
        let blocker = root.path().join("peersus");
        std::fs::write(&blocker, b"in the way").unwrap();
        assert!(open_log(&blocker.join("owner.log")).is_err());
    }

    /// A camera that is not one: it counts how many times it was closed and says whether it was refused, as it is told.
    fn fake_camera() -> (
        CameraControl,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let closed = Arc::new(AtomicUsize::new(0));
        let refused = Arc::new(AtomicBool::new(false));
        let camera = CameraControl {
            open: Arc::new(|_scanner| {}),
            close: {
                let closed = closed.clone();
                Arc::new(move || {
                    closed.fetch_add(1, Ordering::SeqCst);
                })
            },
            refused: {
                let refused = refused.clone();
                Arc::new(move || refused.swap(false, Ordering::SeqCst))
            },
            turns: Arc::new(|| 0),
        };
        (camera, closed, refused)
    }

    fn invitation_frame() -> (Vec<u8>, usize) {
        let text = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let raster = crate::qr::raster(&crate::qr::for_code(&text)).unwrap();
        let side = raster.side as usize;
        (
            (0..side * side)
                .map(|index| raster.rgba[index * 4])
                .collect(),
            side,
        )
    }

    /// Wait on a plain thread for the next tick, up to a few seconds: the watcher is on its own thread.
    fn next_tick(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<crate::view::Tick>,
    ) -> Option<crate::view::Tick> {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < until {
            if let Ok(tick) = receiver.try_recv() {
                return Some(tick);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn a_code_read_by_the_camera_is_joined_once_and_closes_the_camera() {
        let (camera, closed, _) = fake_camera();
        let scanner = crate::scanner::Scanner::new();
        let mut ticks = await_scan(
            scanner.clone(),
            camera,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(30),
        );
        // The camera's thread gives a frame with the invitation in it.
        let (frame, side) = invitation_frame();
        scanner.frame(&frame, side, side, std::time::Instant::now());
        let tick = next_tick(&mut ticks).expect("the watcher never said a code was read");
        assert_eq!(tick.looking, crate::view::Looking::Shut);
        assert!(
            tick.join
                .as_deref()
                .is_some_and(|code| code.starts_with("peersus:1:"))
        );
        assert!(tick.close);
        // And nothing more: the code is not joined again, since the watcher is done, and its thread is over, which
        // the channel says by closing, rather than by waiting out the patience (half a minute here).
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match ticks.try_recv() {
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                    if std::time::Instant::now() < until =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    panic!("the watcher's thread went on after the code was read")
                }
                Ok(_) => panic!("a second tick came after the code was read"),
            }
        }
        // The thread has ended, and the camera was closed by it, once: it is not left open until the page is left.
        assert_eq!(
            closed.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a code was read and the camera was not closed once"
        );
    }

    #[test]
    fn a_camera_that_was_refused_is_said_so_and_a_scan_nobody_answers_is_given_up_on_and_closed() {
        use std::sync::atomic::Ordering;
        // Refused: the camera says so, and the watcher passes it on with the camera closed.
        let (camera, closed, refused) = fake_camera();
        let scanner = crate::scanner::Scanner::new();
        let mut ticks = await_scan(
            scanner,
            camera,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(30),
        );
        refused.store(true, Ordering::SeqCst);
        let tick = next_tick(&mut ticks).expect("the refusal was never said");
        assert_eq!(tick.looking, crate::view::Looking::Refused);
        assert!(tick.join.is_none() && tick.close);
        // The camera that could not be used is closed by the watcher too, so that a tap does not open one that was
        // never closed. The channel closing says the thread has ended.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !matches!(
            ticks.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        ) && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            closed.load(Ordering::SeqCst),
            1,
            "a refused camera was not closed once"
        );

        // Nobody answers: given up on after the patience, and the camera is closed, not left open for ever.
        let (camera, closed, _) = fake_camera();
        let scanner = crate::scanner::Scanner::new();
        let mut ticks = await_scan(
            scanner,
            camera,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_millis(60),
        );
        let tick = next_tick(&mut ticks).expect("a scan nobody answered was never given up on");
        assert_eq!(tick.looking, crate::view::Looking::Shut);
        assert!(tick.close && tick.join.is_none());
        assert!(
            closed.load(Ordering::SeqCst) >= 1,
            "the camera was left open after the wait was given up"
        );
    }

    #[test]
    fn a_watcher_whose_interface_is_gone_closes_the_camera_and_ends() {
        use std::sync::atomic::Ordering;
        let (camera, closed, refused) = fake_camera();
        let scanner = crate::scanner::Scanner::new();
        let ticks = await_scan(
            scanner,
            camera,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(30),
        );
        // The page was left: nobody reads the ticks any more.
        drop(ticks);
        // Something happens that the watcher would have said.
        refused.store(true, Ordering::SeqCst);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while closed.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            closed.load(Ordering::SeqCst) >= 1,
            "the camera stayed open with nobody left to read it"
        );
    }

    /// Wait on a plain thread until the picture channel holds a picture newer than `after`, up to a few seconds.
    fn next_picture(
        receiver: &mut tokio::sync::watch::Receiver<Option<Arc<crate::preview::Shown>>>,
        after: u64,
    ) -> Option<(u64, Arc<crate::preview::Shown>)> {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < until {
            // The value stays in the channel for the next look: only a pointer to it is taken, and its serial compared.
            let current = receiver.borrow().clone();
            if let Some(shown) = current
                && shown.serial > after
            {
                return Some((shown.serial, shown));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        None
    }

    /// A frame with a code in it that the decoder locates and cannot read (its middle is wiped): what the person sees
    /// when the camera has the code and is too far from it, which is what the marks are for.
    fn seen_but_unreadable_frame() -> (Vec<u8>, usize) {
        let (mut frame, side) = invitation_frame();
        let (from, to) = (side * 3 / 10, side * 7 / 10);
        for y in from..to {
            for x in from..to {
                frame[y * side + x] = 255;
            }
        }
        (frame, side)
    }

    #[test]
    fn a_frame_given_to_the_scan_becomes_a_picture_of_the_side_asked_for_with_the_code_seen_marked_on_it()
     {
        let (camera, closed, _) = fake_camera();
        let scanner = crate::scanner::Scanner::new();
        let (_ticks, mut pictures) = await_scan_shown(
            scanner.clone(),
            camera,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(30),
            48,
        );
        let (frame, side) = seen_but_unreadable_frame();
        scanner.frame(&frame, side, side, std::time::Instant::now());
        let (serial, shown) =
            next_picture(&mut pictures, 0).expect("a frame was given and no picture was made");
        assert!(serial >= 1);
        assert_eq!(shown.picture.side, 48);
        assert_eq!(shown.serial, serial);
        // The code is seen and cannot be read: it is marked, and the scan goes on with the camera open.
        assert_eq!(
            shown.codes.len(),
            1,
            "the code that was seen was not marked: {} marks",
            shown.codes.len()
        );
        assert!(
            !scanner.is_over(),
            "a code that could not be read ended the scan"
        );
        assert_eq!(
            closed.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the camera was closed with nothing read"
        );
    }

    #[test]
    fn a_code_that_is_read_ends_the_scan_and_closes_the_camera_whether_or_not_a_picture_was_made_of_it()
     {
        let (camera, closed, _) = fake_camera();
        let scanner = crate::scanner::Scanner::new();
        let (mut ticks, _pictures) = await_scan_shown(
            scanner.clone(),
            camera,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(30),
            48,
        );
        let (frame, side) = invitation_frame();
        scanner.frame(&frame, side, side, std::time::Instant::now());
        let tick = next_tick(&mut ticks).expect("a readable code was never joined");
        assert!(tick.join.is_some() && tick.close);
        // The scan is over, so what is shown of it is nothing (a picture of a shut camera would be wrong), and the camera
        // was closed by the watcher.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while closed.load(std::sync::atomic::Ordering::SeqCst) == 0
            && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(closed.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(scanner.view_after(0).is_none());
    }

    #[test]
    fn with_no_side_asked_for_no_picture_is_ever_made() {
        let (camera, _closed, _) = fake_camera();
        let scanner = crate::scanner::Scanner::new();
        let (_ticks, pictures) = await_scan_shown(
            scanner.clone(),
            camera,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(30),
            0,
        );
        scanner.frame(&vec![100; 32 * 32], 32, 32, std::time::Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert!(
            pictures.borrow().is_none(),
            "a picture was made though none was asked for"
        );
    }

    #[test]
    fn the_turns_are_asked_afresh_for_each_picture_so_that_a_phone_that_is_turned_is_followed() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let turns = Arc::new(AtomicU32::new(0));
        let (mut camera, _closed, _) = fake_camera();
        camera.turns = {
            let turns = turns.clone();
            Arc::new(move || turns.load(Ordering::SeqCst))
        };
        let scanner = crate::scanner::Scanner::new();
        let (_ticks, mut pictures) = await_scan_shown(
            scanner.clone(),
            camera,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(30),
            32,
        );
        // A frame whose top left quarter is bright: where the bright quarter ends up says which turn was used.
        let frame: Vec<u8> = (0..40)
            .flat_map(|y| (0..40).map(move |x| if x < 20 && y < 20 { 250_u8 } else { 10 }))
            .collect();
        let bright_is_top_left = |shown: &crate::preview::Shown| {
            let at =
                |x: u32, y: u32| shown.picture.rgba[((y * shown.picture.side + x) * 4) as usize];
            at(8, 8) > 128 && at(24, 24) < 128
        };
        scanner.frame(&frame, 40, 40, std::time::Instant::now());
        let (serial, first) = next_picture(&mut pictures, 0).expect("no first picture");
        assert!(
            bright_is_top_left(&first),
            "turn 0 did not leave the bright quarter at the top left"
        );
        // The phone is turned: the next frame is shown turned, and not as the first was.
        turns.store(2, Ordering::SeqCst);
        scanner.frame(
            &frame,
            40,
            40,
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        );
        let (_, second) = next_picture(&mut pictures, serial).expect("no second picture");
        assert!(
            !bright_is_top_left(&second),
            "the turn changed to 2 and the picture was not turned"
        );
    }

    #[test]
    fn a_scan_that_gets_no_new_frame_makes_no_new_picture_and_only_the_latest_picture_is_kept() {
        let (camera, _closed, _) = fake_camera();
        let scanner = crate::scanner::Scanner::new();
        let (_ticks, mut pictures) = await_scan_shown(
            scanner.clone(),
            camera,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(30),
            32,
        );
        let start = std::time::Instant::now();
        scanner.frame(&vec![100; 32 * 32], 32, 32, start);
        let (first, _) = next_picture(&mut pictures, 0).expect("no picture for the first frame");
        // Nothing new for a good while: the picture the reader holds is the same one, not made again.
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert_eq!(
            pictures.borrow().as_ref().map(|shown| shown.serial),
            Some(first),
            "a picture was made without a new frame"
        );
        // Several frames come before the reader looks: it finds the newest, and not a queue of them.
        for step in 1..=5 {
            scanner.frame(
                &vec![100; 32 * 32],
                32,
                32,
                start + std::time::Duration::from_millis(200 * step),
            );
        }
        let latest = scanner.view_after(0).unwrap().serial;
        let (serial, _) = next_picture(&mut pictures, first).expect("no newer picture");
        let _ = serial;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pictures.borrow().as_ref().map(|shown| shown.serial) != Some(latest)
            && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            pictures.borrow().as_ref().map(|shown| shown.serial),
            Some(latest)
        );
    }

    /// The two tests of the deadline helper count and wait on threads of one name, so they take this one at a time.
    static DEADLINES: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_deadline_is_told_after_its_time_and_not_before() {
        let _one_at_a_time = DEADLINES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let started = std::time::Instant::now();
        let mut told = after(
            std::time::Duration::from_millis(150),
            std::time::Duration::from_millis(10),
        );
        assert!(
            matches!(
                told.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "told before its time"
        );
        let until = started + std::time::Duration::from_secs(5);
        let answer = loop {
            match told.try_recv() {
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                    if std::time::Instant::now() < until =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                other => break other,
            }
        };
        assert_eq!(
            answer,
            Ok(()),
            "the deadline passed and the interface was not told"
        );
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(150),
            "told {:?} after the start, before its time",
            started.elapsed()
        );
    }

    /// The loop the circle runs while a finger is held, driven by the real clock thread and the real model, as the page does it: the receiver is
    /// listened to, the touch is ticked on each change, and the send is said once.
    async fn held_circle(
        lift_after: Option<std::time::Duration>,
    ) -> (Vec<crate::view::CircleAct>, std::time::Duration) {
        use crate::view::{CircleAct, CircleDoes, CircleTouch};
        let start = std::time::Instant::now();
        let now_ms = move || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut receiver = clock(now_ms, std::time::Duration::from_millis(33));
        let mut touch = CircleTouch::new(100.);
        touch.down((50., 50.), (200., 300.), now_ms());
        let mut acts = Vec::new();
        let mut sent_at = std::time::Duration::ZERO;
        let limit = lift_after.unwrap_or(std::time::Duration::from_secs(4));
        while start.elapsed() < limit {
            let left = limit
                .saturating_sub(start.elapsed())
                .max(std::time::Duration::from_millis(1));
            let Ok(Ok(())) = tokio::time::timeout(left, receiver.changed()).await else {
                break;
            };
            let now = *receiver.borrow_and_update();
            // A resting finger is heard of on every tick (it trembles, and the toolkit says so): the page renews its life on each sign.
            touch.seen(now);
            let act = touch.tick(now, CircleDoes::HoldToSend);
            if act != CircleAct::Nothing {
                sent_at = start.elapsed();
                acts.push(act);
                break;
            }
        }
        if lift_after.is_some() {
            let up = touch.up((50., 50.), (200., 300.), now_ms(), CircleDoes::HoldToSend);
            if up != CircleAct::Nothing {
                acts.push(up);
            }
        }
        (acts, sent_at)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_finger_held_on_the_circle_sends_once_through_the_real_clock_at_about_the_confirmation_time()
     {
        let (acts, at) = held_circle(None).await;
        assert_eq!(
            acts,
            [crate::view::CircleAct::Send],
            "the held circle did not send exactly once"
        );
        let confirm = std::time::Duration::from_millis(crate::view::CONFIRM_MS);
        assert!(
            at >= confirm,
            "it sent before the confirmation time: {at:?}"
        );
        assert!(
            at < confirm + std::time::Duration::from_millis(400),
            "it took far longer than the confirmation time: {at:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_finger_lifted_before_the_circle_is_full_sends_nothing_through_the_real_clock() {
        let (acts, _) = held_circle(Some(std::time::Duration::from_millis(
            crate::view::CONFIRM_MS / 2,
        )))
        .await;
        assert!(
            acts.is_empty(),
            "a lift before the circle was full sent: {acts:?}"
        );
    }

    /// The ids of this process's threads named `name`. By id and not a count: other tests of this process start and end threads of the
    /// same names at the same time (a circle pressed on a screen), and a count of them all said "never started" or "went on running".
    #[cfg(target_os = "linux")]
    fn threads_named(name: &str) -> std::collections::BTreeSet<String> {
        std::fs::read_dir("/proc/self/task")
            .unwrap()
            .filter_map(|task| {
                let path = task.ok()?.path();
                let comm = std::fs::read_to_string(path.join("comm")).ok()?;
                if comm.trim() != name {
                    return None;
                }
                path.file_name()?.to_str().map(str::to_owned)
            })
            .collect()
    }

    /// Starts what `start` starts, waits for the thread named `name` it begins, lets go of what it handed back and waits for that
    /// thread, and only that one, to end.
    #[cfg(target_os = "linux")]
    fn its_thread_ends_when_let_go<T>(name: &str, start: impl FnOnce() -> T, still_running: &str) {
        let before = threads_named(name);
        let held = start();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut started = std::collections::BTreeSet::new();
        while started.is_empty() && std::time::Instant::now() < until {
            started = threads_named(name).difference(&before).cloned().collect();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            !started.is_empty(),
            "the {name} thread never started, so this test proves nothing"
        );
        drop(held);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !threads_named(name).is_disjoint(&started) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(threads_named(name).is_disjoint(&started), "{still_running}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_clock_nobody_listens_to_any_more_ends_its_thread_so_a_circle_nobody_touches_has_no_timer()
    {
        // What a finger down on the circle starts: a clock, listened to. The finger lifted drops the one receiver, and nothing else may
        // hold the channel open.
        its_thread_ends_when_let_go(
            "clock",
            || clock(|| 0, std::time::Duration::from_millis(5)),
            "the clock went on running after nobody listened to it",
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_deadline_nobody_waits_for_ends_its_thread_long_before_its_time() {
        // Let go as a replaced code or a page that was left lets go of it.
        its_thread_ends_when_let_go(
            "deadline",
            || {
                after(
                    std::time::Duration::from_secs(3600),
                    std::time::Duration::from_millis(10),
                )
            },
            "the thread went on sleeping toward an hour after nobody waited for it",
        );
    }

    pub(super) async fn start_test_owner(
        root: &Path,
        name: &str,
        held: peersus_net::held::Held,
    ) -> (Client, tokio::task::JoinHandle<()>) {
        crate::demo_owner::start(root, name, held).await.unwrap()
    }

    async fn collect_test_files(client: &Client, directory: &Path, expected: usize) {
        use peersus_core::model::TransferState;
        let mut changes = client.subscribe();
        let mut accepted = std::collections::BTreeSet::new();
        // Every change of every transfer is written to the history and forced to disk before it is published; where that takes
        // ~45 ms, eighty files take about half a minute. The bound is about delivering, not speed.
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                let snapshot = client.snapshot();
                for transfer in &snapshot.transfers {
                    assert!(!matches!(
                        transfer.state,
                        TransferState::Failed { .. } | TransferState::Cancelled
                    ));
                    if transfer.incoming
                        && transfer.state == TransferState::Offered
                        && accepted.insert(transfer.id.clone())
                    {
                        client
                            .command(Command::AcceptTransfer {
                                transfer: transfer.id.clone(),
                                directory: directory.to_path_buf(),
                            })
                            .await
                            .unwrap();
                    }
                }
                if snapshot.transfers.len() == expected
                    && snapshot
                        .transfers
                        .iter()
                        .all(|transfer| transfer.state == TransferState::Complete)
                {
                    break;
                }
                changes.changed().await.unwrap();
            }
        })
        .await
        .expect("the producer did not deliver the complete selection");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_native_code_precedes_descriptors_and_survives_gaps_between_eighty_choices() {
        use std::io::{Seek, Write};
        let root = tempfile::tempdir().unwrap();
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        let (sender, sender_task) = start_test_owner(root.path(), "sender", held.clone()).await;
        let (receiver, receiver_task) =
            start_test_owner(root.path(), "receiver", Default::default()).await;
        let link = Link {
            client: sender.clone(),
            executor: tokio::runtime::Handle::current(),
            read_only: false,
        };
        picked.opened_streaming();
        let generation = picked.generation();
        picked.selected_for(generation, 80);
        let production = link.code_picked(await_picked(
            picked.clone(),
            std::time::Duration::from_millis(1),
            // From the start of the whole selection, not between two choices: ten batches, each written to a history forced to
            // disk before it is published, can take more than half a minute on a slow disk.
            std::time::Duration::from_secs(120),
        ));
        let code = tokio::time::timeout(std::time::Duration::from_secs(5), production.ready)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .expect("no code before descriptors");
        assert_eq!(code.total, 80);
        assert!(sender.snapshot().transfers.is_empty());
        assert!(sender.snapshot().files_code.unwrap().pending);
        receiver
            .command(Command::JoinInvitation {
                invitation: code.code,
            })
            .await
            .unwrap();
        // Losing the window and its completion observer must not cancel the producer.
        drop(link);
        let destination = tempfile::tempdir().unwrap();
        for offset in (0..80).step_by(8) {
            for index in offset..offset + 8 {
                let mut file = tempfile::tempfile().unwrap();
                file.write_all(&[index as u8; 29]).unwrap();
                file.rewind().unwrap();
                picked.file_for(generation, file, format!("selected-{index:02}.bin"));
            }
            collect_test_files(&receiver, destination.path(), offset + 8).await;
            assert!(
                held.is_empty(),
                "an acknowledged batch retained picker descriptors"
            );
            assert!(
                sender.snapshot().files_code.unwrap().pending,
                "completed early batches ended the selection"
            );
        }
        picked.finished_for(generation);
        assert_eq!(production.finished.await.unwrap(), 0);
        assert!(sender.snapshot().peers.is_empty() && receiver.snapshot().peers.is_empty());
        for index in 0..80 {
            assert_eq!(
                std::fs::read(destination.path().join(format!("selected-{index:02}.bin"))).unwrap(),
                [index as u8; 29]
            );
        }
        sender.command(Command::Shutdown {}).await.unwrap();
        receiver.command(Command::Shutdown {}).await.unwrap();
        sender_task.await.unwrap();
        receiver_task.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_desktop_code_delivers_eighty_paths_after_its_window_observers_are_dropped() {
        let root = tempfile::tempdir().unwrap();
        let (sender, sender_task) =
            start_test_owner(root.path(), "sender", Default::default()).await;
        let (receiver, receiver_task) =
            start_test_owner(root.path(), "receiver", Default::default()).await;
        let paths = (0..80)
            .map(|index| {
                let path = root.path().join(format!("selected-{index:02}.bin"));
                std::fs::write(&path, [index as u8; 29]).unwrap();
                path
            })
            .collect();
        let link = Link {
            client: sender.clone(),
            executor: tokio::runtime::Handle::current(),
            read_only: false,
        };
        let production = link.code_paths(paths);
        let code = production.ready.await.unwrap().unwrap().unwrap();
        assert_eq!(code.total, 80);
        drop(production.finished);
        drop(link);
        receiver
            .command(Command::JoinInvitation {
                invitation: code.code,
            })
            .await
            .unwrap();
        let destination = tempfile::tempdir().unwrap();
        collect_test_files(&receiver, destination.path(), 80).await;
        assert!(sender.snapshot().peers.is_empty() && receiver.snapshot().peers.is_empty());
        for index in 0..80 {
            assert_eq!(
                std::fs::read(destination.path().join(format!("selected-{index:02}.bin"))).unwrap(),
                [index as u8; 29]
            );
        }
        sender.command(Command::Shutdown {}).await.unwrap();
        receiver.command(Command::Shutdown {}).await.unwrap();
        sender_task.await.unwrap();
        receiver_task.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gathered_files_that_were_only_listed_are_delivered_whole_behind_a_code_and_leave_nothing_open()
     {
        use std::io::{Seek, Write};
        let root = tempfile::tempdir().unwrap();
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        let (sender, sender_task) = start_test_owner(root.path(), "sender", held.clone()).await;
        let (receiver, receiver_task) =
            start_test_owner(root.path(), "receiver", Default::default()).await;
        let link = Link {
            client: sender.clone(),
            executor: tokio::runtime::Handle::current(),
            read_only: false,
        };
        // A selection that was chosen, only LISTED: its files are held by number, nothing was sent and no code was made.
        picked.opened_streaming();
        let generation = picked.generation();
        picked.selected_for(generation, 3);
        for index in 0..3u8 {
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&[index; 31]).unwrap();
            file.rewind().unwrap();
            picked.file_for(generation, file, format!("listed-{index}.bin"));
        }
        picked.finished_for(generation);
        let mut gathered = Gathered::new(picked.clone());
        gathered.add(picked.take_for(generation).files);
        assert_eq!(held.len(), 3, "listing the files did not hold them");
        assert!(
            sender.snapshot().transfers.is_empty() && sender.snapshot().files_code.is_none(),
            "listing started something"
        );
        // Confirmed: behind a code, to no device. One hand-over.
        let (production, answer) = link.send_gathered(Vec::new(), true, gathered);
        let code = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            production.expect("a code was asked for").ready,
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .expect("no code");
        assert_eq!(code.total, 3);
        receiver
            .command(Command::JoinInvitation {
                invitation: code.code,
            })
            .await
            .unwrap();
        let destination = tempfile::tempdir().unwrap();
        collect_test_files(&receiver, destination.path(), 3).await;
        assert_eq!(
            answer.await.unwrap().refused,
            0,
            "files were counted as not handed over"
        );
        assert!(held.is_empty(), "files that were handed over stayed open");
        for index in 0..3u8 {
            assert_eq!(
                std::fs::read(destination.path().join(format!("listed-{index}.bin"))).unwrap(),
                [index; 31]
            );
        }
        sender.command(Command::Shutdown {}).await.unwrap();
        receiver.command(Command::Shutdown {}).await.unwrap();
        sender_task.await.unwrap();
        receiver_task.await.unwrap();
    }

    #[test]
    fn taking_the_gathered_files_leaves_the_guard_empty_so_nothing_is_let_go_of_twice() {
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        picked.opened_streaming();
        let generation = picked.generation();
        picked.file_for(generation, pathless(), "one.bin".into());
        picked.finished_for(generation);
        let mut gathered = Gathered::new(picked.clone());
        gathered.add(picked.take_for(generation).files);
        let files = gathered.take_files();
        assert_eq!(files.len(), 1);
        assert!(
            gathered.files().is_none(),
            "the guard still held what it had given away"
        );
        drop(gathered);
        // The files are the caller's now: ending the guard must not close them.
        assert_eq!(held.len(), 1, "the guard let go of files it had given away");
        picked.release(&files);
        assert!(held.is_empty());
    }

    /// A guard with `names` held in a real registry, as a selection that was only listed.
    fn listed(
        names: &[&str],
    ) -> (
        peersus_net::held::Held,
        peersus_net::picked::Picked,
        Gathered,
    ) {
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        picked.opened_streaming();
        let generation = picked.generation();
        for name in names {
            picked.file_for(generation, pathless(), (*name).to_owned());
        }
        picked.finished_for(generation);
        let mut gathered = Gathered::new(picked.clone());
        gathered.add(picked.take_for(generation).files);
        (held, picked, gathered)
    }

    #[test]
    fn taking_one_listed_file_away_lets_go_of_that_one_in_the_registry_and_keeps_the_rest_in_order()
    {
        let (held, _picked, mut gathered) = listed(&["a.bin", "b.bin", "c.bin"]);
        assert_eq!(held.len(), 3);
        assert!(gathered.remove(1));
        assert_eq!(
            gathered.names(),
            ["a.bin", "c.bin"],
            "the wrong file was taken away or the order changed"
        );
        assert_eq!(
            held.len(),
            2,
            "the file taken away stayed open in the registry"
        );
    }

    #[test]
    fn an_index_that_is_not_there_changes_nothing_and_lets_go_of_nothing() {
        let (held, _picked, mut gathered) = listed(&["a.bin"]);
        assert!(!gathered.remove(5));
        assert_eq!(gathered.names(), ["a.bin"]);
        assert_eq!(held.len(), 1);
    }

    #[test]
    fn clearing_lets_go_of_every_listed_file_once_and_the_guard_ending_afterwards_lets_go_of_nothing_more()
     {
        let (held, _picked, mut gathered) = listed(&["a.bin", "b.bin", "c.bin"]);
        gathered.clear();
        assert!(gathered.files().is_none());
        assert!(
            held.is_empty(),
            "a file stayed open after the list was cleared"
        );
        // Ending the guard now must not let go again of numbers that are already gone.
        drop(gathered);
        assert!(held.is_empty());
    }

    #[test]
    fn taking_the_last_file_away_leaves_an_empty_guard_that_lets_the_picker_be_used_again() {
        let (held, _picked, mut gathered) = listed(&["only.bin"]);
        assert!(gathered.remove(0));
        assert!(gathered.files().is_none());
        assert!(held.is_empty());
    }

    /// A second selection of the same registry, as a second use of the picker makes.
    fn second_selection(held: &peersus_net::held::Held, names: &[&str]) -> Gathered {
        let picked = peersus_net::picked::Picked::new(held.clone());
        picked.opened_streaming();
        let generation = picked.generation();
        for name in names {
            picked.file_for(generation, pathless(), (*name).to_owned());
        }
        picked.finished_for(generation);
        let mut gathered = Gathered::new(picked.clone());
        gathered.add(picked.take_for(generation).files);
        gathered
    }

    #[test]
    fn a_second_selection_joins_the_first_in_order_and_nothing_is_let_go_of_or_held_twice() {
        let (held, _picked, mut first) = listed(&["a.bin", "b.bin"]);
        let second = second_selection(&held, &["c.bin"]);
        assert_eq!(held.len(), 3);
        first.absorb(second);
        assert_eq!(
            first.names(),
            ["a.bin", "b.bin", "c.bin"],
            "the new file did not come after the ones that were there"
        );
        assert_eq!(held.len(), 3, "adding let go of a file or held one twice");
        // Ending the guard lets every file go, once.
        drop(first);
        assert!(held.is_empty());
    }

    #[test]
    fn removing_one_after_two_selections_were_joined_lets_go_of_exactly_that_one() {
        let (held, _picked, mut first) = listed(&["a.bin"]);
        let second = second_selection(&held, &["b.bin"]);
        first.absorb(second);
        assert!(first.remove(0));
        assert_eq!(first.names(), ["b.bin"]);
        assert_eq!(held.len(), 1, "the file taken away stayed open");
    }

    /// Two people pair the way they do: an invitation made by one, joined by the other, and the same number chosen on both.
    pub(super) async fn pair_test_owners(a: &Client, b: &Client) {
        crate::demo_owner::pair(a, b).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_listed_selection_goes_whole_to_a_paired_device_and_behind_a_code_in_one_hand_over() {
        use std::io::{Seek, Write};
        let root = tempfile::tempdir().unwrap();
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        let (sender, sender_task) = start_test_owner(root.path(), "sender", held.clone()).await;
        let (friend, friend_task) =
            start_test_owner(root.path(), "friend", Default::default()).await;
        let (stranger, stranger_task) =
            start_test_owner(root.path(), "stranger", Default::default()).await;
        pair_test_owners(&sender, &friend).await;
        let friend_id = sender
            .snapshot()
            .peers
            .iter()
            .find(|peer| peer.online)
            .unwrap()
            .id
            .clone();
        let link = Link {
            client: sender.clone(),
            executor: tokio::runtime::Handle::current(),
            read_only: false,
        };
        picked.opened_streaming();
        let generation = picked.generation();
        picked.selected_for(generation, 4);
        for index in 0..4u8 {
            let mut file = pathless();
            file.write_all(&[index + 1; 37]).unwrap();
            file.rewind().unwrap();
            picked.file_for(generation, file, format!("both-{index}.bin"));
        }
        picked.finished_for(generation);
        let mut gathered = Gathered::new(picked.clone());
        gathered.add(picked.take_for(generation).files);
        // The device and the code TOGETHER: the first destination is given the held files, the second a set of its own.
        let (production, answer) = link.send_gathered(
            vec![FileDestination::Peer { peer: friend_id }],
            true,
            gathered,
        );
        let code = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            production.expect("a code was asked for").ready,
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .expect("no code");
        assert_eq!(code.total, 4);
        stranger
            .command(Command::JoinInvitation {
                invitation: code.code,
            })
            .await
            .unwrap();
        let (by_device, by_code) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        collect_test_files(&friend, by_device.path(), 4).await;
        collect_test_files(&stranger, by_code.path(), 4).await;
        let sent = answer.await.unwrap();
        assert_eq!(sent.refused, 0, "a destination was not handed every file");
        // The owner named the transfers it made for the DEVICE (the code's come later, when it is scanned): exactly the four files, each one a real
        // transfer of the sender's record, outgoing, and none of them invented.
        assert_eq!(
            sent.transfers.len(),
            4,
            "the owner's answer did not name every transfer it made for the device: {:?}",
            sent.transfers
        );
        let record = sender.snapshot().transfers.clone();
        for id in &sent.transfers {
            assert!(
                record
                    .iter()
                    .any(|transfer| transfer.id == *id && !transfer.incoming),
                "an id the owner named is not a transfer of the record: {id}"
            );
        }
        assert!(
            held.is_empty(),
            "files stayed open after both destinations were given theirs"
        );
        for index in 0..4u8 {
            for directory in [by_device.path(), by_code.path()] {
                assert_eq!(
                    std::fs::read(directory.join(format!("both-{index}.bin"))).unwrap(),
                    [index + 1; 37]
                );
            }
        }
        for (client, task) in [
            (sender, sender_task),
            (friend, friend_task),
            (stranger, stranger_task),
        ] {
            client.command(Command::Shutdown {}).await.unwrap();
            task.await.unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_desktop_send_names_exactly_the_transfers_the_owner_made_one_per_file() {
        let root = tempfile::tempdir().unwrap();
        let (sender, sender_task) =
            start_test_owner(root.path(), "sender", Default::default()).await;
        let (friend, friend_task) =
            start_test_owner(root.path(), "friend", Default::default()).await;
        pair_test_owners(&sender, &friend).await;
        let friend_id = sender
            .snapshot()
            .peers
            .iter()
            .find(|peer| peer.online)
            .unwrap()
            .id
            .clone();
        let link = Link {
            client: sender.clone(),
            executor: tokio::runtime::Handle::current(),
            read_only: false,
        };
        let files = tempfile::tempdir().unwrap();
        let paths: Vec<std::path::PathBuf> = (0..2u8)
            .map(|n| {
                let path = files.path().join(format!("one-{n}.bin"));
                std::fs::write(&path, [n + 1; 17]).unwrap();
                path
            })
            .collect();
        let sent = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            link.send_paths(vec![FileDestination::Peer { peer: friend_id }], paths),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(sent.refused, 0);
        assert_eq!(
            sent.transfers.len(),
            2,
            "the owner did not name one transfer per file: {:?}",
            sent.transfers
        );
        // The batch built from what the owner named is exactly those two files, each from the sender's own record.
        let record: Vec<crate::view::Progress> = sender
            .snapshot()
            .transfers
            .iter()
            .map(crate::view::progress)
            .collect();
        let batch = crate::view::batch_of(&record, &sent.transfers, None);
        assert_eq!(
            batch.len(),
            2,
            "the batch was not exactly what the owner named: {batch:?}"
        );
        let mut names: Vec<&str> = batch.iter().map(|item| item.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["one-0.bin", "one-1.bin"]);
        for (client, task) in [(sender, sender_task), (friend, friend_task)] {
            client.command(Command::Shutdown {}).await.unwrap();
            task.await.unwrap();
        }
    }

    /// Experimental (see `docs/nearby-send.md`): files go to a device that is not paired only while the owner holds it as brought near, under
    /// the ids the owner promised when it took them, and its person accepts each. Nobody is paired by it, and what that device announced admits
    /// once.
    #[tokio::test(flavor = "multi_thread")]
    async fn files_go_to_a_device_that_is_not_paired_only_while_it_is_near_and_pair_nobody() {
        use peersus_app::runtime::Reply;
        use peersus_core::model::PeerId;
        use peersus_core::nearby::{Found, Stranger, Takes};
        let root = tempfile::tempdir().unwrap();
        let (sender, sender_task) =
            start_test_owner(root.path(), "sender", Default::default()).await;
        let (stranger, stranger_task) =
            start_test_owner(root.path(), "stranger", Default::default()).await;
        for client in [&sender, &stranger] {
            client
                .command(Command::PatchSettings {
                    patch: Box::new(peersus_core::config::SettingsPatch {
                        nearby_send: Some(true),
                        ..Default::default()
                    }),
                })
                .await
                .unwrap();
        }
        // What the other device announces is the page its window shows, asked of its owner the way its adapter asks. A window that is
        // open on a page that is neither Peers nor Receive: devices nearby are listened for, and nothing is announced.
        let announced = |takes: Option<Takes>| {
            let stranger = stranger.clone();
            async move {
                stranger
                    .command(Command::SeekNearby { takes })
                    .await
                    .unwrap();
                match stranger
                    .command(Command::NearbyOffer { front: true })
                    .await
                    .unwrap()
                {
                    Reply::Nearby(said) => said,
                    _ => panic!("the owner did not say what it announces"),
                }
            }
        };
        assert!(announced(None).await.is_none());
        // Peers is shown: it is there to be paired with, which is not to take files.
        let (to_pair, said) = announced(Some(Takes::Pairing))
            .await
            .expect("nothing was announced with Peers shown");
        assert_eq!(said, Takes::Pairing);
        // Receive is shown: it takes files, by another invitation than the one it was to be paired by.
        let (behind, said) = announced(Some(Takes::Files))
            .await
            .expect("nothing was announced with Receive shown");
        assert_eq!(said, Takes::Files);
        assert_ne!(behind, to_pair);
        // The window goes behind another: nothing is announced, and what was announced is taken back at once, so that
        // the window coming to the front again announces another.
        assert!(matches!(
            stranger
                .command(Command::NearbyOffer { front: false })
                .await
                .unwrap(),
            Reply::Nearby(None)
        ));
        let (offer, _) = announced(Some(Takes::Files))
            .await
            .expect("nothing was announced with Receive in front again");
        assert_ne!(offer, behind);
        // Asked again, as its adapter asks twice a second: the same one, for as long as nobody used it. One made
        // anew at every asking would have the radio begin its announcement again every time, and say nothing.
        for _ in 0..3 {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let again = announced(Some(Takes::Files)).await.map(|(text, _)| text);
            assert_eq!(
                again.as_ref(),
                Some(&offer),
                "what is announced changed while nobody had used it"
            );
        }
        // What the sender's adapter tells its owner it heard: that device, around or near.
        let id = PeerId("heard".to_owned());
        let heard = |near: bool| Command::SetNearby {
            found: Found {
                strangers: vec![Stranger {
                    id: id.clone(),
                    name: "stranger".to_owned(),
                    receives: true,
                    near,
                }],
                ..Found::default()
            },
            offers: vec![(id.clone(), offer.clone())],
        };
        let link = Link {
            client: sender.clone(),
            executor: tokio::runtime::Handle::current(),
            read_only: false,
        };
        let files = tempfile::tempdir().unwrap();
        let paths: Vec<std::path::PathBuf> = (0..2u8)
            .map(|n| {
                let path = files.path().join(format!("near-{n}.bin"));
                std::fs::write(&path, [n + 1; 23]).unwrap();
                path
            })
            .collect();
        let send = |paths: Vec<std::path::PathBuf>| {
            let offered =
                link.send_paths(vec![FileDestination::Nearby { peer: id.clone() }], paths);
            async move {
                tokio::time::timeout(std::time::Duration::from_secs(10), offered)
                    .await
                    .unwrap()
                    .unwrap()
            }
        };
        // Around and not near: the owner gives it nothing, whatever a window asks.
        sender.command(heard(false)).await.unwrap();
        let sent = send(paths.clone()).await;
        assert_eq!((sent.refused, sent.transfers.len()), (2, 0));
        // Brought near: one transfer promised for each file, before the device has admitted anyone.
        sender.command(heard(true)).await.unwrap();
        let sent = send(paths.clone()).await;
        assert_eq!(sent.refused, 0);
        assert_eq!(sent.transfers.len(), 2, "{:?}", sent.transfers);
        // Its person accepts each, and the files arrive whole.
        let saved = tempfile::tempdir().unwrap();
        collect_test_files(&stranger, saved.path(), 2).await;
        for n in 0..2u8 {
            assert_eq!(
                std::fs::read(saved.path().join(format!("near-{n}.bin"))).unwrap(),
                [n + 1; 23]
            );
        }
        // The transfers the sender made are exactly the ones it promised.
        let made: Vec<String> = sender
            .snapshot()
            .transfers
            .iter()
            .filter(|transfer| !transfer.incoming)
            .map(|transfer| transfer.id.clone())
            .collect();
        let mut promised = sent.transfers.clone();
        promised.sort_unstable();
        let mut made_sorted = made.clone();
        made_sorted.sort_unstable();
        assert_eq!(made_sorted, promised);
        // Nobody was paired by it.
        assert!(sender.snapshot().peers.is_empty());
        assert!(stranger.snapshot().peers.is_empty());
        // What that device announced admitted once: the owner let go of it, and a second send by it is refused.
        let again = send(paths).await;
        assert_eq!((again.refused, again.transfers.len()), (2, 0));
        for (client, task) in [(sender, sender_task), (stranger, stranger_task)] {
            client.command(Command::Shutdown {}).await.unwrap();
            task.await.unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_code_that_cannot_be_made_does_not_hold_back_the_devices_that_were_picked() {
        let root = tempfile::tempdir().unwrap();
        let held = peersus_net::held::Held::default();
        let picked = peersus_net::picked::Picked::new(held.clone());
        let (sender, sender_task) = start_test_owner(root.path(), "sender", held.clone()).await;
        let (friend, friend_task) =
            start_test_owner(root.path(), "friend", Default::default()).await;
        pair_test_owners(&sender, &friend).await;
        let friend_id = sender
            .snapshot()
            .peers
            .iter()
            .find(|peer| peer.online)
            .unwrap()
            .id
            .clone();
        let link = Link {
            client: sender.clone(),
            executor: tokio::runtime::Handle::current(),
            read_only: false,
        };
        // An empty selection is a code the owner refuses (there is nothing to offer behind it): the one way the code fails with a live owner.
        let (production, answer) = link.send_gathered(
            vec![FileDestination::Peer { peer: friend_id }],
            true,
            Gathered::new(picked),
        );
        let production = production.expect("a code was asked for");
        assert!(
            production.ready.await.unwrap().is_err(),
            "a code for nothing was made"
        );
        assert_eq!(production.finished.await.unwrap(), 0);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), answer)
                .await
                .unwrap()
                .unwrap()
                .refused,
            0,
            "the failed code stopped the hand-over to the device"
        );
        assert!(held.is_empty());
        for (client, task) in [(sender, sender_task), (friend, friend_task)] {
            client.command(Command::Shutdown {}).await.unwrap();
            task.await.unwrap();
        }
    }
}
