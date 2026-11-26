//! Bounded transfer ownership shared by CLI and graphical hosts.
use crate::{
    announce::Readers,
    file_session::{self, Incoming},
    history::History,
    transfer::{Offer, Source},
};
use iroh::endpoint::Connection;
use peersus_core::model::{PeerId, Transfer, TransferState};
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, oneshot},
    task::{AbortHandle, JoinSet},
};

// Each direction has its own bounded window: full senders must still be able
// to receive each other's offers, or simultaneous selections deadlock.
const LIMIT: usize = 16;
pub(crate) const QUEUED_LIMIT: usize = 500;

struct Queued {
    record: Transfer,
    connection: Connection,
    source: Source,
    since: Instant,
}

struct Pending {
    record: Transfer,
    shown: bool,
    decision: Option<oneshot::Sender<PathBuf>>,
    progress: Arc<AtomicU64>,
    abort: AbortHandle,
}

pub struct TransferTasks {
    pending: BTreeMap<String, Pending>,
    queued: VecDeque<Queued>,
    tasks: JoinSet<(String, Result<Option<PathBuf>, file_session::Error>)>,
    offers_tx: mpsc::Sender<(String, Offer)>,
    offers_rx: mpsc::Receiver<(String, Offer)>,
    /// The announcement readers of the connections offers go out on: one for each connection, shared by every offer on it.
    readers: Readers,
    /// A deadline that the other device told for an offer made here: the offer's id, and how long that device waits. Read by `next`,
    /// which puts it on the offer's record, so that what is counted down is what is waited for.
    told_tx: mpsc::Sender<(String, Duration)>,
    told_rx: mpsc::Receiver<(String, Duration)>,
    /// How long an offer waits for an answer, from the preferences: set by the host whenever they are applied, and read when
    /// an offer is made, so that a change reaches the next offer at once and no offer already waiting is cut short by it.
    wait: Duration,
}

impl Default for TransferTasks {
    fn default() -> Self {
        let (offers_tx, offers_rx) = mpsc::channel(LIMIT);
        let (told_tx, told_rx) = mpsc::channel(LIMIT);
        Self {
            pending: BTreeMap::new(),
            queued: VecDeque::new(),
            tasks: JoinSet::new(),
            offers_tx,
            offers_rx,
            readers: Readers::default(),
            told_tx,
            told_rx,
            wait: Duration::from_secs(u64::from(peersus_core::config::DEFAULT_OFFER_TIMEOUT)),
        }
    }
}

impl TransferTasks {
    /// The wait for an answer to an offer, for the offers made from now on.
    pub fn set_wait(&mut self, seconds: u32) {
        self.wait = Duration::from_secs(u64::from(seconds));
    }

    /// Call only after the host admits the connection: as a remembered peer, or as one that was admitted to carry files for
    /// this one transfer and is not remembered. `peer_name` is what that device called itself while it crossed, kept as a
    /// label of the history record (a paired device's name is the paired record's, and is not copied here).
    ///
    /// `code` is the generation of the files code the connection was admitted with, and `None` when it was not admitted by one:
    /// it is recorded on the transfer, so that a screen that drew that code can tell which transfers came of it.
    pub fn send(
        &mut self,
        connection: Connection,
        peer_name: Option<String>,
        code: Option<u64>,
        source: Source,
        history: &mut History,
        now: u64,
    ) -> io::Result<String> {
        self.send_as(
            Self::fresh_id(),
            connection,
            peer_name,
            code,
            source,
            history,
            now,
        )
    }

    /// The identifier of a transfer that is not made yet: for one that is promised before its connection is admitted
    /// (see [`Self::send_as`]).
    pub fn fresh_id() -> String {
        hex::encode(rand::random::<[u8; 16]>())
    }

    /// [`Self::send`] with the identifier the transfer was promised under, so that who was told the identifier when
    /// the files were taken finds the transfer by it once the connection is admitted.
    #[allow(clippy::too_many_arguments)]
    pub fn send_as(
        &mut self,
        id: String,
        connection: Connection,
        peer_name: Option<String>,
        code: Option<u64>,
        source: Source,
        history: &mut History,
        now: u64,
    ) -> io::Result<String> {
        if !self.has_room() && self.queued.len() >= QUEUED_LIMIT {
            return Err(io::Error::other("The transfer queue is full"));
        }
        let record = Transfer {
            id: id.clone(),
            peer: PeerId(connection.remote_id().to_string()),
            peer_name,
            name: source.offer.name.clone(),
            folders: source.offer.folders().to_vec(),
            bytes: source.offer.bytes,
            completed_bytes: 0,
            incoming: false,
            state: TransferState::Offered,
            started_at: now,
            code,
            // A queued file has not been offered yet: its answer deadline starts only when a stream becomes available.
            expires_at: self
                .has_room()
                .then(|| now.saturating_add(self.wait.as_secs())),
            saved_as: None,
        };
        history.record(record.clone())?;
        if self.has_room() {
            self.start_send(record, connection, source);
        } else {
            self.queued.push_back(Queued {
                record,
                connection,
                source,
                since: Instant::now(),
            });
        }
        Ok(id)
    }

    fn start_send(&mut self, record: Transfer, connection: Connection, source: Source) {
        let id = record.id.clone();
        let progress = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&progress);
        let wait = self.wait;
        let task_id = id.clone();
        let announcements = self.readers.of(&connection);
        let told = self.told_tx.clone();
        let abort = self.tasks.spawn(async move {
            let told_id = task_id.clone();
            let result = file_session::send(
                &connection,
                source,
                wait,
                Some(&announcements),
                // Best effort: if the channel is full the deadline is not put on the record, and what is shown stays the sender's own wait.
                move |theirs| {
                    let _ = told.try_send((told_id.clone(), theirs));
                },
                |value| counter.store(value, Ordering::Relaxed),
            )
            .await;
            (task_id, result.map(|()| None))
        });
        self.pending.insert(
            id.clone(),
            Pending {
                record,
                shown: true,
                decision: None,
                progress,
                abort,
            },
        );
    }

    /// Stream metadata is untrusted; local IDs prevent cross-peer history collisions.
    pub fn receive(
        &mut self,
        peer: PeerId,
        peer_name: Option<String>,
        incoming: Incoming,
        connection: Connection,
        now: u64,
    ) -> io::Result<String> {
        self.capacity()?;
        let id = hex::encode(rand::random::<[u8; 16]>());
        let record = Transfer {
            id: id.clone(),
            peer,
            peer_name,
            name: String::new(),
            folders: Vec::new(),
            bytes: 0,
            completed_bytes: 0,
            incoming: true,
            state: TransferState::Offered,
            started_at: now,
            code: None,
            // The wait begins here, with the offer: the same instant `receive_offered` starts its own clock from.
            expires_at: Some(now.saturating_add(self.wait.as_secs())),
            saved_as: None,
        };
        let (decision, selected) = oneshot::channel();
        let progress = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&progress);
        let task_id = id.clone();
        let offers = self.offers_tx.clone();
        let wait = self.wait;
        let abort = self.tasks.spawn(async move {
            let (prompt, shown) = oneshot::channel();
            let (result, ()) = tokio::join!(
                file_session::receive_offered(
                    incoming,
                    prompt,
                    selected,
                    wait,
                    Some(connection),
                    |value| counter.store(value, Ordering::Relaxed)
                ),
                async {
                    if let Ok(offer) = shown.await {
                        let _ = offers.send((task_id.clone(), offer)).await;
                    }
                }
            );
            (task_id, result.map(Some))
        });
        self.pending.insert(
            id.clone(),
            Pending {
                record,
                shown: false,
                decision: Some(decision),
                progress,
                abort,
            },
        );
        Ok(id)
    }

    pub fn accept(
        &mut self,
        id: &str,
        directory: PathBuf,
        history: &mut History,
    ) -> io::Result<()> {
        let pending = self.pending.get_mut(id).ok_or_else(missing)?;
        if !pending.shown || pending.decision.is_none() || pending.abort.is_finished() {
            return Err(missing());
        }
        let mut record = pending.record.clone();
        record.state = TransferState::Running;
        record.expires_at = None;
        history.record(record.clone())?;
        pending.record = record;
        pending
            .decision
            .take()
            .ok_or_else(missing)?
            .send(directory)
            .map_err(|_| missing())
    }

    /// Cancellation is best effort near completion; never deletes a published file.
    pub fn cancel(&mut self, id: &str, history: &mut History) -> io::Result<()> {
        if let Some(index) = self.queued.iter().position(|queued| queued.record.id == id) {
            let mut record = self.queued[index].record.clone();
            record.state = TransferState::Cancelled;
            history.record(record)?;
            self.queued.remove(index);
            return Ok(());
        }
        let pending = self.pending.get(id).ok_or_else(missing)?;
        if pending.abort.is_finished() {
            return Err(missing());
        }
        let mut record = pending.record.clone();
        record.state = TransferState::Cancelled;
        record.expires_at = None;
        record.completed_bytes = pending.progress.load(Ordering::Relaxed);
        if pending.shown {
            history.record(record)?;
        }
        if let Some(pending) = self.pending.remove(id) {
            pending.abort.abort();
        }
        Ok(())
    }

    /// Stop every transfer still going with `peer` (waiting or moving), by the same path as one person's Cancel: each is recorded as
    /// cancelled, its task is aborted and removed, so that nothing of it is written after this. What `peer`'s device being forgotten
    /// needs, so that a task closing later cannot bring back a record that was forgotten. Returns how many were stopped.
    ///
    /// A history that cannot be written is an error and is NOT skipped: a transfer that could not be recorded as cancelled is still
    /// registered, and the caller must not go on to forget the device's records as if nothing of it were left to write.
    pub fn cancel_peer(&mut self, peer: &PeerId, history: &mut History) -> io::Result<usize> {
        let ids: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, pending)| &pending.record.peer == peer)
            .map(|(id, _)| id.clone())
            .chain(
                self.queued
                    .iter()
                    .filter(|queued| &queued.record.peer == peer)
                    .map(|queued| queued.record.id.clone()),
            )
            .collect();
        let mut stopped = 0;
        for id in ids {
            match self.cancel(&id, history) {
                Ok(()) => stopped += 1,
                // One that already finished has a result still waiting to be read by `next`, which would write its last state
                // here and bring the device's history back. Its entry is removed: with no entry, that result is ignored.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    self.pending.remove(&id);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(stopped)
    }

    /// Merge volatile counters without writing history for every data chunk.
    pub fn snapshot(&self, history: &History) -> Vec<Transfer> {
        history
            .entries()
            .iter()
            .map(|record| {
                let mut record = record.clone();
                if let Some(pending) = self.pending.get(&record.id) {
                    record.completed_bytes = pending.progress.load(Ordering::Relaxed);
                    if record.completed_bytes > 0 {
                        record.state = TransferState::Running;
                        // Bytes are moving: the other device answered, and nothing is waited for any more.
                        record.expires_at = None;
                    }
                }
                record
            })
            .collect()
    }

    pub async fn next(&mut self, history: &mut History) -> io::Result<Transfer> {
        loop {
            if let Some(started) = self.start_queued(history)? {
                return Ok(started);
            }
            tokio::select! {
                biased;
                Some((id, offer)) = self.offers_rx.recv() => {
                    let Some(pending) = self.pending.get_mut(&id) else { continue };
                    pending.record.folders = offer.folders().to_vec();
                    pending.record.name = offer.name;
                    pending.record.bytes = offer.bytes;
                    if let Err(error) = history.record(pending.record.clone()) {
                        pending.abort.abort();
                        self.pending.remove(&id);
                        return Err(error);
                    }
                    pending.shown = true;
                    return Ok(pending.record.clone());
                }
                Some((id, theirs)) = self.told_rx.recv() => {
                    let Some(pending) = self.pending.get_mut(&id) else { continue };
                    // Only an offer made here that is still waiting for its answer takes it: one that is moving or over has no deadline.
                    if !put_told_deadline(&mut pending.record, theirs) { continue }
                    if let Err(error) = history.record(pending.record.clone()) {
                        pending.abort.abort();
                        self.pending.remove(&id);
                        return Err(error);
                    }
                    return Ok(pending.record.clone());
                }
                Some(result) = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    let (id, result) = match result {
                        Ok(result) => result,
                        Err(error) => {
                            let id = self.pending.iter().find(|(_, pending)| pending.abort.id() == error.id()).map(|(id, _)| id.clone());
                            let Some(id) = id else { continue };
                            (id, Err(file_session::Error::Connection))
                        }
                    };
                    let Some(mut pending) = self.pending.remove(&id) else { continue };
                    if !pending.shown { return Err(io::Error::other("Invalid or interrupted incoming offer")); }
                    pending.record.completed_bytes = pending.progress.load(Ordering::Relaxed);
                    pending.record.expires_at = None;
                    pending.record.state = match result {
                        Ok(path) => {
                            pending.record.saved_as = path;
                            TransferState::Complete
                        }
                        Err(file_session::Error::Refused) => TransferState::Cancelled,
                        Err(error) => TransferState::Failed { reason: failure_reason(&error) },
                    };
                    history.record(pending.record.clone())?;
                    return Ok(pending.record);
                }
            }
        }
    }

    /// Whether a lazy producer can start a file now, without overtaking already selected files.
    pub fn has_room(&self) -> bool {
        self.active_in_direction(false) < LIMIT && self.queued.is_empty()
    }

    /// Capacity for an atomic producer batch without consuming any of its sources.
    pub(crate) fn can_queue(&self, count: usize) -> bool {
        let active = self.active_in_direction(false);
        let immediately = if self.queued.is_empty() {
            LIMIT.saturating_sub(active)
        } else {
            0
        };
        count <= QUEUED_LIMIT.saturating_sub(self.queued.len()) + immediately
    }

    pub(crate) fn can_receive(&self) -> bool {
        self.active_in_direction(true) < LIMIT
    }

    fn active_in_direction(&self, incoming: bool) -> usize {
        self.pending
            .values()
            .filter(|pending| pending.record.incoming == incoming)
            .count()
    }

    fn capacity(&self) -> io::Result<()> {
        if self.can_receive() {
            Ok(())
        } else {
            Err(io::Error::other("Too many active transfers"))
        }
    }

    fn start_queued(&mut self, history: &mut History) -> io::Result<Option<Transfer>> {
        let mut started = None;
        while self.active_in_direction(false) < LIMIT {
            let Some(queued) = self.queued.front() else {
                break;
            };
            let mut record = queued.record.clone();
            record.started_at = record
                .started_at
                .saturating_add(queued.since.elapsed().as_secs());
            record.expires_at = Some(record.started_at.saturating_add(self.wait.as_secs()));
            // Retain the source and its place in the queue if persistence fails.
            history.record(record.clone())?;
            let queued = self
                .queued
                .pop_front()
                .expect("front was retained while recording");
            started = Some(record.clone());
            self.start_send(record, queued.connection, queued.source);
        }
        Ok(started)
    }
}

impl Drop for TransferTasks {
    fn drop(&mut self) {
        self.tasks.abort_all();
    }
}

/// Preserve actionable failure categories, never provider messages that may contain private paths.
fn failure_reason(error: &file_session::Error) -> String {
    if let file_session::Error::Transfer(crate::transfer::Error::Io(error)) = error {
        return match error.raw_os_error() {
            Some(code) => format!("Could not read or write the selected file (OS error {code})"),
            None => format!(
                "Could not read or write the selected file ({:?})",
                error.kind()
            ),
        };
    }
    error.to_string()
}

fn missing() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "Transfer is no longer awaiting this action",
    )
}

/// Put the deadline the other device told on the record of an offer made here, if that offer is still waiting for its answer: the
/// moment it was made plus how long that device waits. Anything else (an offer that came in, one already moving or over) takes
/// nothing. Returns whether the record changed.
fn put_told_deadline(record: &mut Transfer, theirs: Duration) -> bool {
    if record.incoming || record.state != TransferState::Offered || record.expires_at.is_none() {
        return false;
    }
    let deadline = record.started_at.saturating_add(theirs.as_secs());
    if record.expires_at == Some(deadline) {
        return false;
    }
    record.expires_at = Some(deadline);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_diagnostics_preserve_categories_without_leaking_provider_paths() {
        let private = io::Error::new(
            io::ErrorKind::PermissionDenied,
            "/private/provider/document",
        );
        let reason = failure_reason(&file_session::Error::Transfer(crate::transfer::Error::Io(
            private,
        )));
        assert!(reason.contains("PermissionDenied"));
        assert!(!reason.contains("/private"));
        let numeric = failure_reason(&file_session::Error::Transfer(crate::transfer::Error::Io(
            io::Error::from_raw_os_error(13),
        )));
        assert!(numeric.contains("OS error 13"));
        assert!(
            failure_reason(&file_session::Error::Transfer(
                crate::transfer::Error::Integrity
            ))
            .contains("checksum")
        );
        assert!(
            failure_reason(&file_session::Error::Transport(
                crate::transport::Error::Timeout
            ))
            .contains("timed out")
        );
    }

    fn record(id: &str, peer: &str, state: TransferState) -> Transfer {
        Transfer {
            id: id.into(),
            peer: PeerId(peer.into()),
            peer_name: None,
            name: format!("{id}.bin"),
            folders: Vec::new(),
            bytes: 10,
            completed_bytes: 0,
            incoming: false,
            state,
            started_at: 1,
            code: None,
            expires_at: None,
            saved_as: None,
        }
    }

    /// A transfer that is registered with a task: one that is still waiting, or one whose task has finished and whose result nobody
    /// has read yet (what `next` finds queued in the `JoinSet`).
    fn register(tasks: &mut TransferTasks, id: &str, peer: &str, finished: bool) {
        let task_id = id.to_owned();
        let abort = if finished {
            tasks.tasks.spawn(async move { (task_id, Ok(None)) })
        } else {
            tasks.tasks.spawn(async move {
                std::future::pending::<()>().await;
                (task_id, Ok(None))
            })
        };
        tasks.pending.insert(
            id.to_owned(),
            Pending {
                record: record(id, peer, TransferState::Offered),
                shown: true,
                decision: None,
                progress: Arc::new(AtomicU64::new(0)),
                abort,
            },
        );
    }

    #[tokio::test]
    async fn stopping_a_device_removes_its_transfers_even_one_whose_task_finished_unread_and_leaves_another_device_alone()
     {
        let directory = tempfile::tempdir().unwrap();
        let mut history = History::open(directory.path().join("history.json")).unwrap();
        let mut tasks = TransferTasks::default();
        register(&mut tasks, "waiting-a", "a", false);
        register(&mut tasks, "finished-a", "a", true);
        register(&mut tasks, "waiting-b", "b", false);
        for id in ["waiting-a", "finished-a", "waiting-b"] {
            history
                .record(record(
                    id,
                    if id.ends_with('b') { "b" } else { "a" },
                    TransferState::Offered,
                ))
                .unwrap();
        }
        // Let the finished task actually finish, with its result left unread in the set.
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        tasks
            .cancel_peer(&PeerId("a".into()), &mut history)
            .unwrap();

        assert!(
            !tasks.pending.contains_key("waiting-a"),
            "a waiting transfer of the device was left registered"
        );
        assert!(
            !tasks.pending.contains_key("finished-a"),
            "a finished task of the device was left registered, to be written later"
        );
        assert!(
            tasks.pending.contains_key("waiting-b"),
            "another device's transfer was stopped"
        );
        // The queued result of the finished task finds no entry: it is read and ignored, and writes nothing to the history.
        let before: Vec<_> = history
            .entries()
            .iter()
            .map(|item| (item.id.clone(), item.state.clone()))
            .collect();
        let read = tokio::time::timeout(Duration::from_millis(200), tasks.next(&mut history)).await;
        assert!(
            read.is_err(),
            "a result of the stopped device was turned into a change"
        );
        let after: Vec<_> = history
            .entries()
            .iter()
            .map(|item| (item.id.clone(), item.state.clone()))
            .collect();
        assert_eq!(
            before, after,
            "the history changed when the finished task's result was read"
        );
        // The device's rows go too, all of them: none is left with no task to end it, to open as failed at the next start and be
        // noted again. The other device's row stays, here and after the history is opened again from its file.
        history.forget_peer(&PeerId("a".into())).unwrap();
        let rows = |history: &History| {
            history
                .entries()
                .iter()
                .map(|item| item.peer.0.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            rows(&history),
            ["b"],
            "a row of the forgotten device was left in the list"
        );
        let reopened = History::open(directory.path().join("history.json")).unwrap();
        assert_eq!(
            rows(&reopened),
            ["b"],
            "a row of the forgotten device came back when the history was opened again"
        );
    }
}
