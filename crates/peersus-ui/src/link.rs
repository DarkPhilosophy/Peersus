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
    /// A short title and one sentence of explanation.
    pub fn words(&self) -> (String, String) {
        match self {
            Offline::NoDirectory => (
                "Nowhere to look".to_owned(),
                "This account has no data directory, so the application cannot be found.".to_owned(),
            ),
            Offline::NotRunning => (
                "Peersus stopped".to_owned(),
                "The part of Peersus that keeps your devices connected has ended. Close this window and open Peersus again.".to_owned(),
            ),
            Offline::NotStarted(reason) => (
                "Peersus could not start".to_owned(),
                match reason {
                    Some(reason) => format!(
                        "The part of Peersus that keeps your devices connected did not start. It said: {reason}"
                    ),
                    None => "The part of Peersus that keeps your devices connected did not start, and did not say why.".to_owned(),
                },
            ),
        }
    }
}

/// The last line of what the owner wrote, cleaned for a screen: no control characters (a log may
/// hold colour codes) and at most 200 characters. `None` if it wrote nothing.
#[cfg(not(target_os = "android"))]
fn last_line(log: &str) -> Option<String> {
    let line = log
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let clean: String = line.chars().filter(|c| !c.is_control()).take(200).collect();
    (!clean.trim().is_empty()).then_some(clean)
}

/// The last line of what was written to `log` at or after byte `offset`: what the owner started now said, and
/// nothing an earlier run left in the same file (which is appended to, never emptied, and would otherwise be shown
/// as the reason for a failure it has nothing to do with). An offset that is not a character boundary, or past the
/// end, says nothing.
#[cfg(not(target_os = "android"))]
fn last_line_after(log: &str, offset: usize) -> Option<String> {
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
#[derive(Clone)]
pub struct Link {
    client: Client,
    executor: tokio::runtime::Handle,
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

    /// Apply an edit of the preferences and say how it went, with the owner's own sentence when it would not (a transfer is going, an address is
    /// not usable). For a choice the person makes and then looks at the result of; `send` is for the ones whose effect is the next snapshot.
    pub fn patch_settings(
        &self,
        patch: peersus_core::config::SettingsPatch,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static {
        let client = self.client.clone();
        async move {
            client
                .command(Command::PatchSettings { patch })
                .await
                .map(|_| ())
                .map_err(|reason| reason.to_string())
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Path selections use the same serial, bounded handoff as native descriptors. Each device is sent every path: a path is opened again
    /// for each, so that the devices do not share a reading position. Answers with what the owner said of it (see [`Sent`]): how many files it did
    /// not take, and the ids of the transfers it made, which are the batch.
    pub fn send_paths(
        &self,
        peers: Vec<peersus_core::model::PeerId>,
        paths: Vec<PathBuf>,
    ) -> tokio::sync::oneshot::Receiver<Sent> {
        let (finished, answer) = tokio::sync::oneshot::channel();
        let client = self.client.clone();
        self.executor.spawn(async move {
            let mut sent = Sent::default();
            for peer in peers {
                for paths in crate::view::path_sends(paths.clone()) {
                    let count = u32::try_from(paths.len()).unwrap_or(u32::MAX);
                    match queue_batch(
                        &client,
                        Command::QueueFiles {
                            destination: FileDestination::Peer { peer: peer.clone() },
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

    /// Send files that were gathered and only listed (an Android selection that waits for the person to say where it goes) to the devices
    /// that were picked and, when `with_code`, behind a code too: ALL IN ONE hand-over, so that every destination is given the same held
    /// files (the first the files themselves, each other one a set of its own, see [`peersus_net::picked::Picked::reopened`]) and none is
    /// left with files another already took. A code is made BEFORE anything is queued, and is finished after, as for a code on its own.
    ///
    /// What is gathered is moved out of `gathered` first, so that it lets go of nothing twice. The `CodeProduction` is for the code, if one
    /// was asked for, and says when it is shown; the number is how many files were not handed over, counted for each destination that missed
    /// them.
    pub fn send_gathered(
        &self,
        peers: Vec<peersus_core::model::PeerId>,
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
            let mut destinations: Vec<FileDestination> = peers
                .into_iter()
                .map(|peer| FileDestination::Peer { peer })
                .collect();
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

/// Where the platform says the application's usable area is, in physical pixels as
/// `(left, top, right, bottom)`. Asked again and again, because a platform has no answer until its window
/// has been laid out and the answer changes when its bars come and go. All zero means "not known yet".
pub type SafeAreaSource = Arc<dyn Fn() -> (i32, i32, i32, i32) + Send + Sync>;

/// Ask `source` for the usable area every `every` and publish it on the returned channel when it changes.
///
/// This runs on a plain thread of its own and not on the interface's tasks: those have no timer and no
/// runtime, and a timer made there panics, which is how an earlier version of this crashed on the phone the
/// moment it opened. The interface only waits on the channel, as it does for the owner's snapshots. The
/// thread ends when every receiver has been dropped, that is when the window has closed.
pub fn watch_safe_area(
    source: SafeAreaSource,
    every: std::time::Duration,
) -> tokio::sync::watch::Receiver<(i32, i32, i32, i32)> {
    let (sender, receiver) = tokio::sync::watch::channel(source());
    std::thread::Builder::new()
        .name("safe-area".to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(every);
                if sender.is_closed() {
                    break;
                }
                // Only a change is published: a receiver wakes the interface, and nothing changed.
                sender.send_if_modified(|last| {
                    let now = source();
                    let changed = *last != now;
                    *last = now;
                    changed
                });
            }
        })
        .ok();
    receiver
}

/// Whether the system is in its dark mode now: `Some(true)` for dark, `Some(false)` for light, `None` when the platform cannot say.
/// Asked at once for the first frame and again and again for a change of the system's choice, as the usable area is.
pub type DarkSource = Arc<dyn Fn() -> Option<bool> + Send + Sync>;

/// Ask `source` whether the system is dark every `every` and publish it on the returned channel when it changes. The first answer is
/// there at once, so that the very first frame is drawn in the right theme and not in the toolkit's default one.
///
/// A plain thread of its own for the reason [`watch_safe_area`] is, ending when every receiver has been dropped.
pub fn watch_dark(
    source: DarkSource,
    every: std::time::Duration,
) -> tokio::sync::watch::Receiver<Option<bool>> {
    let (sender, receiver) = tokio::sync::watch::channel(source());
    std::thread::Builder::new()
        .name("dark-mode".to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(every);
                if sender.is_closed() {
                    break;
                }
                sender.send_if_modified(|last| {
                    let now = source();
                    let changed = *last != now;
                    *last = now;
                    changed
                });
            }
        })
        .ok();
    receiver
}

/// The second now, as Unix seconds, from `source`, published on the returned channel each time it changes.
///
/// Every countdown on screen (the time left of an offer, in the bar over the window and in the circle of the device that sent
/// it) reads this one channel, so that none of them has a timer of its own and no thread is made per card. It is a plain thread
/// for the reason `watch_safe_area` is: the interface's tasks have no timer. `every` bounds how late a new second is seen, and
/// the thread ends when every receiver is gone. Only a CHANGED second is published, so a screen is woken once a second, not
/// as often as the thread looks.
pub fn clock(
    source: impl Fn() -> u64 + Send + 'static,
    every: std::time::Duration,
) -> tokio::sync::watch::Receiver<u64> {
    let (sender, receiver) = tokio::sync::watch::channel(source());
    std::thread::Builder::new()
        .name("clock".to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(every);
                if sender.is_closed() {
                    break;
                }
                sender.send_if_modified(|last| {
                    let now = source();
                    let changed = *last != now;
                    *last = now;
                    changed
                });
            }
        })
        .ok();
    receiver
}

/// Tell the interface when `after` has passed, once, on the returned channel.
///
/// The interface has no timer (see [`watch_safe_area`]), so the waiting is on a plain thread of its own. It looks at the
/// channel every `every` and ends as soon as nobody waits on it, so a code that was replaced or a page that was left does not
/// leave a thread asleep for minutes. `every` bounds how late the answer to "nobody waits" is, never how late the deadline is
/// told.
pub fn after(
    after: std::time::Duration,
    every: std::time::Duration,
) -> tokio::sync::oneshot::Receiver<()> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("deadline".to_owned())
        .spawn(move || {
            let until = std::time::Instant::now() + after;
            loop {
                let left = until.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    let _ = sender.send(());
                    break;
                }
                if sender.is_closed() {
                    break;
                }
                std::thread::sleep(left.min(every));
            }
        })
        .ok();
    receiver
}

/// Wait on a plain thread of its own for what a platform's picker hands over, and publish each batch on the returned
/// channel, until the picker says it is done or the interface has gone.
///
/// The answers come on a thread that is not the interface's, at a moment nobody can predict, and the interface's
/// tasks have no timer to wait with, so the waiting is done here, as `watch_safe_area` does it, and the interface
/// only waits on the channel. Every batch is sent once; the last one is the one that says the picker is closed.
/// A picker that never says it is done, because its platform side vanished, is given up on after `patience`, so that
/// no thread waits for ever.
pub fn await_picked(
    picked: peersus_net::picked::Picked,
    every: std::time::Duration,
    patience: std::time::Duration,
) -> Batches {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let thread_picked = picked.clone();
    let generation = picked.generation();
    std::thread::Builder::new()
        .name("picker".to_owned())
        .spawn(move || {
            let picked = thread_picked;
            let started = std::time::Instant::now();
            let mut count_seen = None;
            loop {
                std::thread::sleep(every);
                let gave_up = started.elapsed() >= patience;
                if gave_up {
                    picked.finished_for(generation);
                }
                let taken = picked.take_for(generation);
                let count = picked.selected_count(generation);
                let declared = count != count_seen;
                count_seen = count;
                let finished = !taken.still_open;
                if (!taken.files.is_empty() || taken.refused > 0 || declared || finished || gave_up)
                    && let Err(undelivered) = sender.send(taken)
                {
                    // Nobody reads: what was taken is let go of here, the rest was the reader's, and is let go of with it.
                    picked.release(&undelivered.0.files);
                    break;
                }
                if finished || gave_up {
                    break;
                }
                if sender.is_closed() {
                    break;
                }
            }
        })
        .ok();
    Batches {
        receiver,
        picked,
        generation,
    }
}

/// Wait for the person to answer the platform's folder picker FOR `transfer`, and tell the interface once, on the returned channel.
///
/// Like [`await_picked`]: the answer comes on a thread that is not the interface's, at a moment nobody can predict, and the
/// interface has no timer, so the looking is done here and the interface only waits on the channel. Only the answer made for
/// `transfer` is taken (see `FolderPicked::take_for`). Looks every `every`; a picker whose platform side vanished is given up on
/// after `patience`, answered as backing out and its question let go of, so that no thread waits for ever and no later question is
/// blocked by it. The thread ends when nobody waits on the channel, and lets go of the question then too.
pub fn await_folder(
    picked: peersus_net::folder_picked::FolderPicked,
    transfer: String,
    every: std::time::Duration,
    patience: std::time::Duration,
) -> tokio::sync::oneshot::Receiver<Option<String>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("folder-picker".to_owned())
        .spawn(move || {
            let mut sender = Some(sender);
            let started = std::time::Instant::now();
            loop {
                std::thread::sleep(every);
                let answer = match picked.take_for(&transfer) {
                    peersus_net::folder_picked::Taken::Answered { folder, .. } => Some(folder),
                    peersus_net::folder_picked::Taken::Waiting if started.elapsed() >= patience => {
                        picked.abandon(&transfer);
                        Some(None)
                    }
                    peersus_net::folder_picked::Taken::Waiting => None,
                };
                if let Some(answer) = answer {
                    if let Some(sender) = sender.take() {
                        let _ = sender.send(answer);
                    }
                    break;
                }
                if sender
                    .as_ref()
                    .is_some_and(tokio::sync::oneshot::Sender::is_closed)
                {
                    picked.abandon(&transfer);
                    break;
                }
            }
        })
        .ok();
    receiver
}

/// What a picker handed over, as a reader takes it. A batch is the reader's from the moment it is read. What was sent and never
/// read is the receiver's, and is let go of when the receiver is dropped: that is the page being left, and it holds whether the
/// picker had finished or not and whether the thread had ended or not, which a thread watching the channel cannot promise.
pub struct Batches {
    receiver: tokio::sync::mpsc::UnboundedReceiver<peersus_net::picked::Taken>,
    picked: peersus_net::picked::Picked,
    generation: u64,
}

impl Batches {
    /// The next batch, or `None` when the picker is done and everything was read.
    pub async fn recv(&mut self) -> Option<peersus_net::picked::Taken> {
        self.receiver.recv().await
    }

    /// The next batch without waiting, for a thread that has no runtime.
    #[cfg(test)]
    pub fn blocking_recv(&mut self) -> Option<peersus_net::picked::Taken> {
        self.receiver.blocking_recv()
    }
}

impl Drop for Batches {
    fn drop(&mut self) {
        // Closing first, so that nothing more is sent behind the draining; then every batch that was never read.
        self.receiver.close();
        self.picked.finished_for(self.generation);
        let pending = self.picked.take_for(self.generation);
        self.picked.release(&pending.files);
        while let Ok(unread) = self.receiver.try_recv() {
            self.picked.release(&unread.files);
        }
    }
}

/// The files an Android picker handed over and that wait to be offered under a code: kept by number in the registry. They are
/// let go of when this ends, whatever ended it (a code made, a refusal, the page left, the wait given up on), so that a batch
/// the owner did not take never stays open. A number the owner already took is nothing to let go of.
pub struct Gathered {
    picked: peersus_net::picked::Picked,
    files: Vec<(u64, String)>,
}

impl Gathered {
    pub fn new(picked: peersus_net::picked::Picked) -> Self {
        Self {
            picked,
            files: Vec::new(),
        }
    }

    pub fn add(&mut self, files: Vec<(u64, String)>) {
        self.files.extend(files);
    }

    /// What is gathered, to be offered; `None` when nothing is.
    pub fn files(&self) -> Option<Vec<(u64, String)>> {
        (!self.files.is_empty()).then(|| self.files.clone())
    }

    /// The names the files go by, in the order they were gathered: what the page lists. Never where they were, and never their numbers.
    pub fn names(&self) -> Vec<String> {
        self.files.iter().map(|(_, name)| name.clone()).collect()
    }

    /// Move what is gathered out, leaving this EMPTY: what it held is then the caller's to hand over, and this lets go of nothing when it
    /// ends. Letting go twice of numbers the owner already took could revoke a send that is under way.
    pub fn take_files(&mut self) -> Vec<(u64, String)> {
        std::mem::take(&mut self.files)
    }

    /// The file at `index` is taken away and let go of in the registry, once. An index that is not there changes nothing: the list may have changed
    /// between the touch and the press. Returns whether a file was taken away.
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.files.len() {
            return false;
        }
        let gone = self.files.remove(index);
        self.picked.release(std::slice::from_ref(&gone));
        true
    }

    /// Everything gathered is let go of in the registry, once, and the list is empty.
    pub fn clear(&mut self) {
        let everything = std::mem::take(&mut self.files);
        self.picked.release(&everything);
    }

    /// Take everything `other` holds into this guard, after what this one holds, and leave `other` EMPTY so that ending it lets go of nothing: the
    /// numbers are held once, by this guard. For a second selection that adds to the first instead of replacing it.
    pub fn absorb(&mut self, mut other: Gathered) {
        self.files.extend(other.take_files());
    }
}

impl Drop for Gathered {
    fn drop(&mut self) {
        self.picked.release(&self.files);
    }
}

/// Watch a scan on a plain thread of its own, and publish what each look at the camera makes of the state, until the scan
/// is over, the camera was refused, or `patience` has run out.
///
/// Like `await_picked`, it is on its own thread because the interface's tasks have no timer to wait with. What each
/// look means is [`crate::view::look`], which is tried without a phone: this only asks it, and passes on what it says.
/// Only a look that changes something is published (a code read, a refusal, the end), so that the interface is not woken
/// ten times a second to learn that nothing happened. A scan that is never answered is given up on after `patience`,
/// and closed, so that no camera stays open for ever with nobody looking at it.
pub fn await_scan(
    scanner: crate::scanner::Scanner,
    camera: CameraControl,
    every: std::time::Duration,
    patience: std::time::Duration,
) -> tokio::sync::mpsc::UnboundedReceiver<crate::view::Tick> {
    await_scan_shown(scanner, camera, every, patience, 0).0
}

/// [`await_scan`], and with it what the circle is to show of the camera: the picture of what it sees, with the codes it
/// has located marked on it, `side` pixels across, as a second channel.
///
/// The picture is made here, on the watcher's own thread, and never on the interface's: it costs a pass over the frame.
/// One is made only when there is a frame newer than the last made, so a camera that has stopped giving frames costs
/// nothing, and a picture is not made twice for one frame. Only the latest picture is kept for the reader.
pub fn await_scan_shown(
    scanner: crate::scanner::Scanner,
    camera: CameraControl,
    every: std::time::Duration,
    patience: std::time::Duration,
    side: u32,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<crate::view::Tick>,
    tokio::sync::watch::Receiver<Option<Arc<crate::preview::Shown>>>,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    // A watch and not a queue: only the latest picture matters, and a reader that is slow must never be handed a pile of
    // pictures a megabyte each to go through; it finds the newest one, and the ones between are gone.
    let (shown_sender, shown_receiver) = tokio::sync::watch::channel(None);
    std::thread::Builder::new()
        .name("scan".to_owned())
        .spawn(move || {
            let started = std::time::Instant::now();
            let mut looking = crate::view::Looking::Scanning;
            let mut last_shown = 0_u64;
            // The picture is made often, about thirty times a second, because that is what keeps what the person sees up with where the camera
            // is; reading for a code and looking at the state of the scan stay at `every`, which is all they need and what they cost.
            let picture_every = std::time::Duration::from_millis(33).min(every);
            let mut last_look = std::time::Instant::now();
            loop {
                std::thread::sleep(picture_every);
                if side > 0
                    && let Some(view) = scanner.view_after(last_shown)
                {
                    last_shown = view.serial;
                    if let Some(shown) = crate::preview::show(&view, side, (camera.turns)()) {
                        // A picture nobody is left to see is not a reason to stop: the ticks decide that.
                        shown_sender.send_replace(Some(Arc::new(shown)));
                    }
                }
                if last_look.elapsed() < every {
                    continue;
                }
                last_look = std::time::Instant::now();
                let tick = crate::view::look(looking, scanner.take(), (camera.refused)());
                let changed = tick.looking != looking || tick.join.is_some() || tick.close;
                looking = tick.looking;
                let over =
                    tick.close || tick.looking == crate::view::Looking::Shut || scanner.is_over();
                let gave_up = started.elapsed() >= patience;
                if changed && sender.send(tick).is_err() {
                    // The interface is gone: the camera is closed, since nobody is left to read what it sees.
                    (camera.close)();
                    break;
                }
                if over || gave_up {
                    // Every end closes the camera, here and once: a code read, a refusal, or a wait given up on. The
                    // page's own closing on leaving is the second line, for a page left before any of these.
                    (camera.close)();
                    if gave_up && !over {
                        let _ = sender.send(crate::view::Tick {
                            looking: crate::view::Looking::Shut,
                            join: None,
                            close: true,
                        });
                    }
                    break;
                }
            }
        })
        .ok();
    (receiver, shown_receiver)
}

/// How often the interface looks for what a platform's picker or camera has handed over. The platform answers on a thread of its own
/// at a moment nobody can predict, and the interface has no timer, so it looks at this rhythm: fast enough that a choice shows at
/// once, slow enough to cost nothing. A rhythm of the interface and not a wait of the person's.
pub const LOOK_EVERY: std::time::Duration = std::time::Duration::from_millis(150);

/// How long a wait on a picker (of files or of a folder) goes on before it is given up as unanswered. Ten minutes is far more than
/// choosing takes, and it ends a wait that nothing will answer, so that no thread waits for ever.
pub const PICKER_PATIENCE: std::time::Duration = std::time::Duration::from_secs(600);

/// How long a camera is left on for a code before it is closed as unpointed. Two minutes is far more than reading a code takes.
pub const CAMERA_PATIENCE: std::time::Duration = std::time::Duration::from_secs(120);

/// A platform's file picker, for a platform with no file dialog the toolkit can open (Android).
///
/// It hands over files and not bytes or paths: the system's picker returns a handle that only the system's own
/// calls can read, so the platform opens the file and gives the program the open file, which is kept (see
/// [`peersus_net::held::Held`]) and sent from where it is. `open` shows the picker and returns at once; what was
/// chosen arrives in `picked`, which the interface asks for until the picker says it is done.
#[derive(Clone)]
pub struct FilePicker {
    /// Show the picker, for one file or for several. Does nothing the interface waits on.
    pub open: Arc<dyn Fn(bool) + Send + Sync>,
    /// What the picker handed over, and whether it is still open.
    pub picked: peersus_net::picked::Picked,
}

impl PartialEq for FilePicker {
    /// The same picker is the same bridge to the platform: the call that opens it is shared, and compared by
    /// identity. A closure cannot be compared by what it does, and the picked state is the same for the same bridge.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.open, &other.open)
    }
}

/// A platform's camera, for reading the invitation of another device (Android, where every phone has one).
///
/// It gives frames and nothing else: what becomes of them is [`crate::scanner::Scanner`], tried without a phone.
/// `open` starts the camera for a scan and returns at once; the frames reach that scan on the camera's own thread.
/// `close` ends it. `refused` says, once, that the permission was refused, so that the circle can say so and not
/// wait for ever for frames that will not come.
#[derive(Clone)]
pub struct CameraControl {
    /// Start the camera for this scan. Anything open before is closed, and its scan ended.
    pub open: Arc<dyn Fn(crate::scanner::Scanner) + Send + Sync>,
    /// Close the camera and end the scan in progress. Safe when nothing is open.
    pub close: Arc<dyn Fn() + Send + Sync>,
    /// Whether the camera said it could not be used, taken once.
    pub refused: Arc<dyn Fn() -> bool + Send + Sync>,
    /// How many quarter turns clockwise a frame must be turned to stand upright for the person holding the phone now
    /// (the sensor's mounting and the screen's turn, see [`crate::preview::turns_to_upright`]). Asked afresh each time,
    /// since the phone may be turned while the camera is open.
    pub turns: Arc<dyn Fn() -> u32 + Send + Sync>,
}

impl PartialEq for CameraControl {
    /// The same camera is the same bridge: compared by the identity of the call that opens it.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.open, &other.open)
    }
}

/// A platform's notifications, which the person can allow or block outside the program (Android): the program cannot know by itself
/// whether an offer that arrives will be shown, so Settings asks the platform, and offers the way to allow them.
#[derive(Clone)]
pub struct NotificationsControl {
    /// Whether the notifications of this program can be shown now. Asked afresh each time it is drawn, since the person may change it
    /// in the system's settings and come back.
    pub allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Ask for the permission if the system will still ask, and otherwise open the settings where it can be given. Returns at once.
    pub open: Arc<dyn Fn() + Send + Sync>,
}

impl PartialEq for NotificationsControl {
    /// The same notifications are the same bridge: compared by the identity of the call that opens the settings.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.open, &other.open)
    }
}

/// The permission to draw the panel of an offer over other applications (Android), which the person gives by hand on a screen of the system
/// and the program cannot ask for with a dialog: Settings asks the platform whether it is given, and offers the way to that screen.
#[derive(Clone)]
pub struct OverlayControl {
    /// Whether the panel can be drawn over other applications now. Asked afresh each time it is drawn, since the person gives it on a screen
    /// of the system and comes back.
    pub allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Open the screen of the system where the person allows it. Returns at once.
    pub open: Arc<dyn Fn() + Send + Sync>,
}

impl PartialEq for OverlayControl {
    /// The same permission is the same bridge: compared by the identity of the call that opens the screen.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.open, &other.open)
    }
}

/// Whether the system leaves this program out of its battery optimisation (Android), which decides if the background service keeps its
/// network while the phone is idle. The person chooses it on a screen of the system: Settings shows the state and offers the way there.
#[derive(Clone)]
pub struct BatteryControl {
    /// Whether the program is left out of the optimisation now. Asked afresh while the row is on the screen, since the person decides it on
    /// a screen of the system and comes back.
    pub unrestricted: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Open the screen of the system where the person chooses. Returns at once.
    pub open: Arc<dyn Fn() + Send + Sync>,
}

impl PartialEq for BatteryControl {
    /// The same choice is the same bridge: compared by the identity of the call that opens the screen.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.open, &other.open)
    }
}

/// Optional platform-owned background clipboard consent; never substitutes for peer grants.
#[derive(Clone)]
pub struct ClipboardControl {
    pub status: Arc<dyn Fn() -> String + Send + Sync>,
    pub open: Arc<dyn Fn() + Send + Sync>,
}

impl PartialEq for ClipboardControl {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.open, &other.open)
    }
}

/// The choice of which camera reads a code, for a phone that has several (Android): the camera the program picks is not always the one that
/// sees a screen well, so the person chooses, and sees the result in the circle of scanning.
#[derive(Clone)]
pub struct CameraChoice {
    /// The back cameras the phone has, one line each (id, focal length in tenths of a millimetre, nearest focus in centimetres, 1 if
    /// chosen), tab separated. Asked afresh each time it is drawn.
    pub list: Arc<dyn Fn() -> String + Send + Sync>,
    /// Choose a camera by its id; an empty id leaves the choice to the program. A camera that is open is started again on the new one.
    pub choose: Arc<dyn Fn(&str) + Send + Sync>,
}

impl PartialEq for CameraChoice {
    /// The same choice is the same bridge: compared by the identity of the call that chooses.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.choose, &other.choose)
    }
}

pub type AppearanceSetter = Arc<dyn Fn(&[u32; 9]) + Send + Sync>;

/// The colours the interface is drawn with now, which a platform draws its own surfaces with as well (Android: the notification of an offer),
/// so that they follow the theme the person chose and the one they made, and are not colours written in the platform's resources.
#[derive(Clone)]
pub struct Appearance {
    /// Tell the platform the colours in use, as 0xRRGGBB, in the order: surface, raised, border, text, muted, accent, bad, button,
    /// button text. Called when they change, and kept by the platform for the times the interface is not drawn.
    pub set: AppearanceSetter,
}

impl PartialEq for Appearance {
    /// The same appearance is the same bridge: compared by the identity of the call.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.set, &other.set)
    }
}

/// What a button of the notification of an offer asked that only the window can do (Android): View files pressed with the window closed
/// brings the window forward, and the list of the files has to be open when it comes, or the person presses View files twice.
#[derive(Clone)]
pub struct OfferButtons {
    /// Whether View files was pressed since the last time this was asked, taken once.
    pub take_view: Arc<dyn Fn() -> bool + Send + Sync>,
    /// The offers whose Accept was pressed on the notification or the panel and could not be served without a window (no folder is kept, or
    /// saving on their own is off), taken once: the window is brought forward for them and asks where each goes.
    pub take_accepts: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

impl PartialEq for OfferButtons {
    /// The same buttons are the same bridge: compared by the identity of the call.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.take_view, &other.take_view)
    }
}

/// The program's executor and what it found when it looked for the owner. Kept alive for as
/// long as the window is open: dropping the executor ends the connection's background task.
pub struct Session {
    // Held only so that it is dropped last, with the window.
    _executor: tokio::runtime::Runtime,
    pub link: Result<Link, Offline>,
    /// What the platform offers beyond the window: where the bars are, a picker for files, a camera. Empty on a desktop.
    pub platform: Platform,
    pub system_theme: tokio::sync::watch::Receiver<Option<bool>>,
}

/// A platform's way to put a received file in a folder the person chose, for a platform where such a folder is not a path
/// the program can write (Android): the system's picker gives an address that only the system's own calls can write through.
///
/// The program receives into `staging`, a folder it can write; the person's choice is asked for when they accept, and when the
/// file has arrived whole the platform puts it in that folder, then removes the staged copy. The address is text the program
/// never reads as a path.
#[derive(Clone)]
pub struct FolderSink {
    /// A folder the program can write, where an accepted file is received before it is put in the chosen one.
    pub staging: PathBuf,
    /// What the person answered, and the deliveries that finished.
    pub picked: peersus_net::folder_picked::FolderPicked,
    /// Show the platform's folder picker. Returns at once; the answer arrives in `picked`.
    pub choose: Arc<dyn Fn() + Send + Sync>,
    /// Put a finished transfer's file in the folder chosen for it. Returns at once; how it went arrives in `picked`.
    pub deliver: Arc<dyn Fn(&peersus_net::folder_picked::Handoff) + Send + Sync>,
    /// The folder chosen for the files that come in on their own, as the tree address the system gave, or `None`. Not a path: on this
    /// platform the setting of a folder that is a path cannot say it, so the platform keeps it and is asked.
    pub saved: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    /// Show the platform's folder picker for the saved folder. Returns at once; the platform keeps what is chosen.
    pub choose_saved: Arc<dyn Fn() + Send + Sync>,
    /// Let go of the saved folder: files that come in ask where they go again.
    pub forget_saved: Arc<dyn Fn() + Send + Sync>,
}

impl PartialEq for FolderSink {
    /// The same sink is the same bridge: compared by the identity of the call that opens the picker.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.choose, &other.choose)
    }
}

/// A platform-provided HTTPS browser launch. False means no launch was dispatched.
#[derive(Clone)]
pub struct UrlOpener(pub Arc<dyn Fn(&str) -> bool + Send + Sync>);
impl UrlOpener {
    pub fn open(&self, url: &str) -> bool {
        (self.0)(url)
    }
}
impl PartialEq for UrlOpener {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Opens the platform's package picker; its adapter stages the selected archive through the owner.
#[derive(Clone)]
pub struct PluginPicker(pub Arc<dyn Fn() -> Result<(), &'static str> + Send + Sync>);
impl PartialEq for PluginPicker {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// What a platform offers that the toolkit does not: each ability is present on the platform that needs it, and absent
/// where the toolkit already does the thing. One value, so that a fourth ability is a field and not another parameter
/// on every function that builds a window.
#[derive(Clone, Default)]
pub struct Platform {
    /// Present on a platform whose system draws bars over the window (Android); absent on a desktop, whose
    /// window manager already keeps the window clear of them.
    pub safe_area: Option<SafeAreaSource>,
    /// Present on a platform whose files are not paths (Android), where the toolkit's own dialog does not exist.
    pub file_picker: Option<FilePicker>,
    /// Present on a platform with a camera that the program can read frames from (Android).
    pub camera: Option<CameraControl>,
    /// Present on a platform where a folder the person chooses is not a path the program can write (Android).
    pub folder_sink: Option<FolderSink>,
    /// Present on a platform where the toolkit cannot say whether the system is in its dark mode (Android, where the window has no
    /// theme to give): the answer comes from the platform itself, and is what the first frame is drawn with.
    pub dark: Option<DarkSource>,
    /// Present on a platform whose notifications the person can block (Android): Settings shows their state and the way to allow them.
    pub notifications: Option<NotificationsControl>,
    /// Present on a platform with several cameras for the person to choose from (Android).
    pub camera_choice: Option<CameraChoice>,
    /// Present on a platform that draws surfaces of its own with the colours of the interface (Android).
    pub appearance: Option<Appearance>,
    /// Present on a platform whose notification of an offer has buttons that need the window (Android).
    pub offer_buttons: Option<OfferButtons>,
    /// Present on a platform where the panel of an offer is drawn over other applications by a permission the person gives by hand (Android).
    pub overlay: Option<OverlayControl>,
    /// Present on a platform where the system's battery optimisation can stop the background service from reaching the network (Android).
    pub battery: Option<BatteryControl>,
    pub open_url: Option<UrlOpener>,
    pub plugin_picker: Option<PluginPicker>,
    pub clipboard: Option<ClipboardControl>,
}

impl PartialEq for Platform {
    /// The same platform is the same bridges: each ability is compared by the identity of the call behind it, since a
    /// call cannot be compared by what it does, and an absent ability equals only an absent one.
    fn eq(&self, other: &Self) -> bool {
        let same_area = match (&self.safe_area, &other.safe_area) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        let same_dark = match (&self.dark, &other.dark) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        same_area
            && same_dark
            && self.file_picker == other.file_picker
            && self.camera == other.camera
            && self.folder_sink == other.folder_sink
            && self.notifications == other.notifications
            && self.camera_choice == other.camera_choice
            && self.appearance == other.appearance
            && self.offer_buttons == other.offer_buttons
            && self.overlay == other.overlay
            && self.battery == other.battery
            && self.open_url == other.open_url
            && self.plugin_picker == other.plugin_picker
            && self.clipboard == other.clipboard
    }
}

#[cfg(not(target_os = "android"))]
fn control_socket() -> Result<PathBuf, Offline> {
    peersus_app::instance::data_directory()
        .map(|directory| directory.join("control.sock"))
        .ok_or(Offline::NoDirectory)
}

/// Where the owner program is: in the directory this program is in, under its own name, and
/// nowhere else. It is never looked for on the search path, where something else of that name
/// could be, nor in a build directory of tests.
#[cfg(not(target_os = "android"))]
fn owner_next_to(window: &Path) -> Option<PathBuf> {
    let directory = window
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())?;
    Some(directory.join("peersus"))
}

/// Whether a failed attempt to connect means "nobody is there", and so an owner should be
/// started. Any other failure (a socket that belongs to someone else, a timeout) is not cured by
/// starting another and would only hide what is wrong.
#[cfg(not(target_os = "android"))]
fn should_start_owner(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// Whether this run may start an owner. A test or a screenshot sets `PEERSUS_NO_AUTOSTART` so
/// that a window pointed at a throwaway directory can never launch one on the person's own.
#[cfg(not(target_os = "android"))]
fn autostart_allowed(setting: Option<&std::ffi::OsStr>) -> bool {
    setting.is_none_or(|value| value.is_empty())
}

/// Start the owner, which has no window of its own, and leave it running on its own: it outlives
/// this window, as it must to receive while the window is closed. No argument is passed: the
/// owner reads the person's saved preferences, and a flag such as `--background` would be
/// written into them. Nor is any flag of a test passed, such as `--bind`.
///
/// What the owner prints is kept in `log`, not thrown away: when it ends at once, that is the
/// only place the reason can be read, and the person is shown the last line of it.
#[cfg(not(target_os = "android"))]
fn start_owner(log: &Path) -> io::Result<std::process::Child> {
    let window = std::env::current_exe()?;
    let owner = owner_next_to(&window)
        .filter(|owner| owner.is_file())
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    let output = open_log(log)?;
    let mut command = std::process::Command::new(owner);
    command
        .stdin(std::process::Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output);
    // Marks the owner as started for a window, so that it ends when its last window goes, unless the person
    // asked for the background. Only that it is set matters. An owner started any other way carries no mark
    // and is never ended by this, and this window never stops one it did not start.
    #[cfg(unix)]
    command.env(peersus_app::lifetime::STARTED_FOR_A_WINDOW, "1");
    command.spawn()
}

/// Open the owner's log for appending, making its directory first if this is the very first
/// start and there is none yet. The owner makes that directory itself, but only once it runs,
/// and the log has to be there before it does. It is made the way the owner requires, private to
/// the person (a directory that is not is refused). One that already exists is left as it is:
/// mending someone's permissions is not this window's place, and the owner will say so.
#[cfg(not(target_os = "android"))]
fn open_log(log: &Path) -> io::Result<std::fs::File> {
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
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Private to the person, like the rest of the application's directory.
        options.mode(0o600);
    }
    options.open(log)
}

/// Where the owner's output is kept, beside the rest of its data.
#[cfg(not(target_os = "android"))]
fn owner_log(socket: &Path) -> PathBuf {
    socket.with_file_name("owner.log")
}

impl Session {
    /// Attach to the owner listening at `socket`, which the caller already started in this very process. It
    /// never looks for a program to start and never reads the environment: the place is given. This is how
    /// Android opens, where the owner is part of the application and there is no other executable to start.
    ///
    /// `safe_area` is where the platform says the usable area is, asked again and again (see
    /// [`SafeAreaSource`]); `None` where nothing draws over the window. `file_picker` is the platform's own way to
    /// choose files where the toolkit has no dialog; `None` where it has.
    ///
    /// Returns the error text when not even an executor could be started.
    pub fn open_at(socket: PathBuf, platform: Platform) -> Result<Arc<Session>, &'static str> {
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
            .block_on(ipc::attach(socket))
            .map(|(client, _task)| Link {
                client,
                executor: handle,
            })
            .map_err(|_| Offline::NotStarted(None));
        Ok(Arc::new(Session {
            _executor: executor,
            link,
            platform,
            system_theme,
        }))
    }

    /// Start the executor and attach to the owner, starting it first if nothing is listening.
    /// Returns the error text when not even an executor could be started.
    ///
    /// Not on Android, where the owner is part of the application and [`Session::open_at`] is told where it is.
    #[cfg(not(target_os = "android"))]
    pub fn open() -> Result<Arc<Session>, &'static str> {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| "Cannot start the interface's executor")?;
        let handle = executor.handle().clone();
        let system_theme = crate::system_theme::watch(&handle);
        let link = control_socket().and_then(|path| {
            let attach = |path: PathBuf| executor.block_on(ipc::attach(path));
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
                            // for a bounded while, and stop asking the moment it has ended.
                            let mut result = Err(Offline::NotStarted(None));
                            // What the last try to attach said, kept for when the owner itself said nothing: an owner
                            // that runs but cannot be attached to (the two programs do not understand each other) writes no line.
                            let mut attach_error: Option<String> = None;
                            for _ in 0..40 {
                                std::thread::sleep(std::time::Duration::from_millis(250));
                                match attach(path.clone()) {
                                    Ok(attached) => {
                                        result = Ok(attached);
                                        break;
                                    }
                                    Err(error) => attach_error = Some(attach_failure_words(&error)),
                                }
                                if matches!(child.try_wait(), Ok(Some(_))) {
                                    result = Err(Offline::NotStarted(reason(&log, &attach_error)));
                                    break;
                                }
                            }
                            if result.is_err() {
                                result = Err(Offline::NotStarted(reason(&log, &attach_error)));
                            }
                            result
                        }
                    }
                }
                Err(error) => Err(Offline::NotStarted(Some(attach_failure_words(&error)))),
            };
            attached
                // The task keeps the snapshots fresh for as long as the executor lives.
                .map(|(client, _task)| Link {
                    client,
                    executor: handle,
                })
        });
        Ok(Arc::new(Session {
            _executor: executor,
            link,
            platform: Platform::default(),
            system_theme,
        }))
    }
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_owner_is_looked_for_next_to_the_window_and_nowhere_else() {
        let owner = owner_next_to(Path::new("/opt/peersus/peersus-ui")).unwrap();
        assert_eq!(owner, Path::new("/opt/peersus/peersus"));
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
    fn the_not_running_screen_names_no_command_line_flag() {
        for offline in [Offline::NotRunning, Offline::NotStarted(None)] {
            let (title, text) = offline.words();
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
    fn an_offset_that_is_not_inside_the_log_says_nothing() {
        assert_eq!(last_line_after("abc", 99), None);
        // In the middle of a character: no panic, nothing said.
        assert_eq!(last_line_after("é", 1), None);
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
        let with = Offline::NotStarted(Some("preferences are invalid".into())).words();
        assert!(with.1.contains("preferences are invalid"), "{with:?}");
        let without = Offline::NotStarted(None).words();
        assert!(!without.1.is_empty());
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

    #[cfg(target_os = "linux")]
    #[test]
    fn a_clock_nobody_listens_to_any_more_ends_its_thread_so_a_circle_nobody_touches_has_no_timer()
    {
        // How many threads of this process are named "clock".
        let named = || {
            std::fs::read_dir("/proc/self/task")
                .unwrap()
                .filter_map(|task| std::fs::read_to_string(task.unwrap().path().join("comm")).ok())
                .filter(|name| name.trim() == "clock")
                .count()
        };
        let before = named();
        // What a finger down on the circle starts: a clock, listened to.
        let listening = clock(|| 0, std::time::Duration::from_millis(5));
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while named() <= before && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            named() > before,
            "the thread never started, so this test proves nothing"
        );
        // The finger is lifted: the one receiver is dropped, and nothing else may hold the channel open.
        drop(listening);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while named() > before && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            named() <= before,
            "the clock went on running after nobody listened to it"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_deadline_nobody_waits_for_ends_its_thread_long_before_its_time() {
        // How many threads of this process carry the name the helper gives its thread.
        let named = || {
            std::fs::read_dir("/proc/self/task")
                .unwrap()
                .filter_map(|task| std::fs::read_to_string(task.unwrap().path().join("comm")).ok())
                .filter(|name| name.trim() == "deadline")
                .count()
        };
        let before = named();
        let told = after(
            std::time::Duration::from_secs(3600),
            std::time::Duration::from_millis(10),
        );
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while named() <= before && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            named() > before,
            "the thread never started, so this test proves nothing"
        );
        // What a replaced code or a page that was left does.
        drop(told);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while named() > before && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            named() <= before,
            "the thread went on sleeping toward an hour after nobody waited for it"
        );
    }

    async fn start_test_owner(
        root: &Path,
        name: &str,
        held: peersus_net::held::Held,
    ) -> (Client, tokio::task::JoinHandle<()>) {
        use peersus_core::{Settings, config::NetworkSettings};
        use peersus_net::{
            grants::StandaloneGrants, history::History, network_host::NetworkHost, peers::Peers,
            settings::SettingsStore, transport,
        };
        let root = root.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let settings = Settings {
            network: NetworkSettings {
                bind: Some("127.0.0.1".parse().unwrap()),
                allow_relay: false,
                ..NetworkSettings::default()
            },
            device_name: name.to_owned(),
            ..Settings::default()
        };
        let mut store = SettingsStore::open(root.join("preferences.json")).unwrap();
        store.replace(settings.clone()).unwrap();
        let endpoint = transport::bind(iroh::SecretKey::generate(), &settings.network)
            .await
            .unwrap();
        let network = NetworkHost::new(
            endpoint,
            settings.device_name,
            Peers::open(root.join("peers.json")).unwrap(),
            History::open(root.join("history.json")).unwrap(),
        );
        let sockets = tempfile::tempdir().unwrap();
        let (owner, client) = peersus_app::runtime::Runtime::new(
            network,
            StandaloneGrants::open(root.join("grants.json")).unwrap(),
            store,
            Default::default(),
            sockets.path().to_path_buf(),
        )
        .unwrap();
        let task = tokio::spawn(async move {
            owner.with_held(held).run().await;
            drop(sockets);
        });
        (client, task)
    }

    async fn collect_test_files(client: &Client, directory: &Path, expected: usize) {
        use peersus_core::model::TransferState;
        let mut changes = client.subscribe();
        let mut accepted = std::collections::BTreeSet::new();
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
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
        };
        picked.opened_streaming();
        let generation = picked.generation();
        picked.selected_for(generation, 80);
        let production = link.code_picked(await_picked(
            picked.clone(),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_secs(20),
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
    async fn pair_test_owners(a: &Client, b: &Client) {
        use peersus_app::runtime::Reply;
        let wait = |client: &Client| {
            let client = client.clone();
            async move {
                let mut changes = client.subscribe();
                tokio::time::timeout(std::time::Duration::from_secs(20), async {
                    loop {
                        if let Some(prompt) = client.snapshot().pairing.first() {
                            return prompt.clone();
                        }
                        changes.changed().await.unwrap();
                    }
                })
                .await
                .expect("no pairing prompt in time")
            }
        };
        let Reply::Invitation(invitation) = a.command(Command::CreateInvitation {}).await.unwrap()
        else {
            panic!("no invitation")
        };
        b.command(Command::JoinInvitation { invitation })
            .await
            .unwrap();
        let (prompt_a, prompt_b) = (wait(a).await, wait(b).await);
        a.command(Command::ChoosePairing {
            request: prompt_a.id.clone(),
            number: prompt_a.options[0],
        })
        .await
        .unwrap();
        b.command(Command::ChoosePairing {
            request: prompt_b.id.clone(),
            number: prompt_a.options[0],
        })
        .await
        .unwrap();
        for client in [a, b] {
            let mut changes = client.subscribe();
            tokio::time::timeout(std::time::Duration::from_secs(20), async {
                while !client.snapshot().peers.iter().any(|peer| peer.online) {
                    changes.changed().await.unwrap();
                }
            })
            .await
            .expect("the pairing did not come online");
        }
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
        };
        picked.opened_streaming();
        let generation = picked.generation();
        picked.selected_for(generation, 4);
        for index in 0..4u8 {
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&[index + 1; 37]).unwrap();
            file.rewind().unwrap();
            picked.file_for(generation, file, format!("both-{index}.bin"));
        }
        picked.finished_for(generation);
        let mut gathered = Gathered::new(picked.clone());
        gathered.add(picked.take_for(generation).files);
        // The device and the code TOGETHER: the first destination is given the held files, the second a set of its own.
        let (production, answer) = link.send_gathered(vec![friend_id], true, gathered);
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
            link.send_paths(vec![friend_id], paths),
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
        };
        // An empty selection is a code the owner refuses (there is nothing to offer behind it): the one way the code fails with a live owner.
        let (production, answer) = link.send_gathered(vec![friend_id], true, Gathered::new(picked));
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
