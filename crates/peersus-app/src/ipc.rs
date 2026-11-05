//! Same-user local control, forwarding commands to the one application owner.
//! The launcher owns socket creation/removal; this module never deletes a path.
//! A lost response is an uncertain outcome, not permission to retry a command.
#[cfg(windows)]
use crate::local_pipe::Listener;
use crate::runtime::{Client, Reply};
use peersus_core::model::{Command, Snapshot};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{io, path::Path, time::Duration};
#[cfg(windows)]
use tokio::net::windows::named_pipe::NamedPipeClient as Stream;
#[cfg(unix)]
use tokio::net::{UnixListener as Listener, UnixStream as Stream};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    task::JoinSet,
};

/// The largest request a local client may send: that of the largest picture a window can set, which is
/// `MAX_EDGE` squared pixels of four bytes written in hex, with a little for the command around it.
/// Anything larger cannot be a valid command and is turned away before it is read.
pub const REQUEST_LIMIT: usize =
    2 * 4 * (peersus_core::profile::MAX_EDGE as usize).pow(2) + 4 * 1024;
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(15);

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
enum Request {
    Snapshot,
    /// Boxed: a command carries a whole settings edit, far larger than the other two requests, which carry nothing.
    /// `quiet`: answer with the outcome and no snapshot. For a window that watches for changes anyway, which
    /// would otherwise be sent the whole state for every sample of a finger.
    Command {
        command: Box<Command>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        quiet: bool,
    },
    /// A window saying it is attached, and staying so for as long as it holds the connection open.
    Lease,
    /// Answered with the state once it is newer than generation `since`, or when [`WATCH_WAIT`] has passed
    /// with no change, with the state as it is then: a window keeps one of these open instead of asking
    /// for the state on a clock, so a change reaches it at once and an unchanged state costs nothing.
    Watch {
        since: u64,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    result: Result<Option<Reply>, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snapshot: Option<Snapshot>,
    /// Which state `snapshot` is: counts up with every change the owner publishes. Said only to a
    /// client that watches (and so knows the field); an older window reads every other answer as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<u64>,
}
/// How long a watch waits for a change before it answers with the state unchanged.
const WATCH_WAIT: Duration = Duration::from_secs(10);
#[cfg(unix)]
fn authenticate(stream: &Stream) -> io::Result<()> {
    if stream.peer_cred()?.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Local service belongs to another user",
        ));
    }
    Ok(())
}

/// A connection to the owner, checked to be this user's own where the system can say whose it is
/// (on Windows the pipe's name is only in this user's folder; see [`crate::local_pipe`]).
async fn connect(path: &Path) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        let stream = Stream::connect(path).await?;
        authenticate(&stream)?;
        Ok(stream)
    }
    #[cfg(windows)]
    crate::local_pipe::connect(path).await
}
async fn read_frame<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
    maximum: usize,
) -> io::Result<T> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid local frame size",
        ));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid local frame"))
}
async fn write_frame<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &T,
    maximum: usize,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        // serde says where and which kind of value failed, never the value.
        tracing::warn!(classification = ?error.classify(), line = error.line(), column = error.column(), "A local frame could not be encoded");
        io::Error::other("Could not encode local frame")
    })?;
    if bytes.len() > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Local frame exceeds limit",
        ));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await
}
/// How many ordinary requests are served at once, whatever number of windows are attached.
const REQUESTS_AT_ONCE: usize = 16;
/// How many leases are held at once: a person has a few windows, and one that holds more than this is not
/// a window. One over is answered with a refusal and closed.
const MOST_LEASES: u32 = 8;

async fn handle(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    client: Client,
    presence: crate::lifetime::Presence,
    mut stopping: tokio::sync::watch::Receiver<std::sync::Arc<Snapshot>>,
    mut generation: tokio::sync::watch::Receiver<u64>,
) -> io::Result<()> {
    // Only the reading and answering of a request is under the deadline: a lease is made to last.
    let request = tokio::time::timeout(DEADLINE, read_frame(&mut stream, REQUEST_LIMIT))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    let lease = matches!(request, Request::Lease);
    if lease && presence.windows() >= MOST_LEASES {
        let refused = Response {
            result: Err("Too many windows are attached".to_owned()),
            snapshot: Some((*client.snapshot()).clone()),
            generation: None,
        };
        return write_frame(&mut stream, &refused, RESPONSE_LIMIT).await;
    }
    // Taken before the answer is written and held until this function ends, however it ends: the window
    // closing, an error, or the task being cancelled all drop it.
    let held = lease.then(|| presence.attach());
    let (mut quiet, mut watched) = (false, false);
    let result = match request {
        Request::Snapshot | Request::Lease => Ok(None),
        Request::Watch { since } => {
            watched = true;
            // Answered on a change, or unchanged after a while: either way with what the state is now.
            let _ = tokio::time::timeout(WATCH_WAIT, generation.wait_for(|now| *now > since)).await;
            Ok(None)
        }
        Request::Command { command, quiet: wanted } => {
            quiet = wanted;
            let answer = client.command(*command);
            tokio::pin!(answer);
            let answer = match tokio::time::timeout(DEADLINE, &mut answer).await {
                Ok(answer) => answer,
                Err(_) => {
                    tracing::warn!("A command has waited for the owner longer than the deadline");
                    answer.await
                }
            };
            answer.map(Some).map_err(str::to_owned)
        }
    };
    // The generation is read before the state, so a client never learns a generation newer than the state it was given.
    let generation = *generation.borrow();
    let response = Response {
        result,
        snapshot: (!quiet).then(|| (*client.snapshot()).clone()),
        generation: watched.then(|| generation),
    };
    tokio::time::timeout(
        DEADLINE,
        write_frame(&mut stream, &response, RESPONSE_LIMIT),
    )
    .await
    .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    if held.is_some() {
        // The window is attached for as long as it keeps this open. It sends nothing more, so a read that
        // ends, by end of stream or by an error, is the window going. The owner stopping ends it too:
        // otherwise stopping would wait for every window that is still open to close first.
        let mut ignored = [0_u8; 64];
        loop {
            tokio::select! {
                read = stream.read(&mut ignored) => { if !read.is_ok_and(|read| read > 0) { break; } }
                stopped = stopping.changed() => { if stopped.is_err() { break; } }
            }
        }
    }
    drop(held);
    Ok(())
}
/// Bounded clients, bounded frames and timeouts; dropping this future aborts clients.
/// Stops accepting when the application owner exits, then drains in-flight replies.
///
/// A window's lease lives as long as the window and is not cut by the deadline of an ordinary request. It
/// is counted in `presence` and takes none of the places ordinary requests have: the places for requests
/// are [`REQUESTS_AT_ONCE`], and each open lease adds one more, so that windows attached to the owner can
/// never leave a command line client waiting. At most [`MOST_LEASES`] are held; one over is turned away.
pub async fn serve(
    listener: Listener,
    client: Client,
    presence: crate::lifetime::Presence,
) -> io::Result<()> {
    #[cfg(windows)]
    let mut listener = listener;
    let mut tasks = JoinSet::new();
    let mut state = client.subscribe();
    // Counts the states published, so a watching client can say which one it saw last.
    let (generations, generation) = tokio::sync::watch::channel(1_u64);
    loop {
        let room = REQUESTS_AT_ONCE + presence.windows() as usize;
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() { break; }
                generations.send_modify(|generation| *generation += 1);
            }
            accepted = listener.accept(), if tasks.len() < room => {
                // One connection that could not be taken (a descriptor limit for a moment, a client gone before it was
                // taken) is that connection's failure, never the end of every window's way to the owner.
                let stream = match accepted {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        tracing::warn!(kind = ?error.kind(), "A local connection could not be accepted");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                #[cfg(unix)]
                if let Err(error) = authenticate(&stream) {
                    tracing::warn!(kind = ?error.kind(), "A local connection was refused");
                    continue;
                }
                let (client, presence, stopping, generation) = (client.clone(), presence.clone(), client.subscribe(), generation.clone());
                tasks.spawn(async move {
                    // Only the kind and this crate's own fixed words are said, never what was asked.
                    if let Err(error) = handle(stream, client, presence, stopping, generation).await
                        && error.kind() != io::ErrorKind::UnexpectedEof
                    {
                        tracing::warn!(kind = ?error.kind(), reason = %error, "A local request failed");
                    }
                });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    while tasks.join_next().await.is_some() {}
    Ok(())
}
async fn exchange(path: &Path, request: Request) -> io::Result<Response> {
    tokio::time::timeout(DEADLINE, async {
        let mut stream = connect(path).await?;
        write_frame(&mut stream, &request, REQUEST_LIMIT).await?;
        read_frame(&mut stream, RESPONSE_LIMIT).await
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Local request timed out; command outcome may be unknown",
        )
    })?
}
fn given(snapshot: Option<Snapshot>) -> io::Result<Snapshot> {
    snapshot.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing local state"))
}
/// Read current state without creating a second network host.
pub async fn snapshot(path: &Path) -> io::Result<Snapshot> {
    let response = exchange(path, Request::Snapshot).await?;
    response.result.map_err(io::Error::other)?;
    given(response.snapshot)
}
/// The state and its generation, once it is newer than `since` or [`WATCH_WAIT`] has passed.
async fn watch(path: &Path, since: u64) -> io::Result<(Snapshot, u64)> {
    let response = tokio::time::timeout(WATCH_WAIT + DEADLINE, async {
        let mut stream = connect(path).await?;
        write_frame(&mut stream, &Request::Watch { since }, REQUEST_LIMIT).await?;
        read_frame::<Response>(&mut stream, RESPONSE_LIMIT).await
    })
    .await
    .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    response.result.map_err(io::Error::other)?;
    Ok((given(response.snapshot)?, response.generation.unwrap_or(0)))
}
async fn ask(path: &Path, command: Command, quiet: bool) -> io::Result<(Reply, Option<Snapshot>)> {
    let response = exchange(
        path,
        Request::Command {
            command: Box::new(command),
            quiet,
        },
    )
    .await?;
    let reply = response
        .result
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing command response"))?;
    Ok((reply, response.snapshot))
}
/// Execute once; the returned snapshot includes acknowledged preference changes.
pub async fn command(path: &Path, command: Command) -> io::Result<(Reply, Snapshot)> {
    let (reply, snapshot) = ask(path, command, false).await?;
    Ok((reply, given(snapshot)?))
}

/// Tell the owner that a window is attached, and keep the connection open: the owner counts it for as long as
/// it lasts. Drop the returned stream to let go.
///
/// * `Ok(Some(stream))`: counted.
/// * `Ok(None)`: the owner is too old to know the request. It does not count windows and does not end itself
///   by them either, so a window without a lease is not at risk.
/// * `Err`: the owner understood and turned it away (it holds as many as it allows), or did not answer in
///   time. The window must not be made: the owner would not know of it.
///
/// What the owner said is told apart by its shape and never by its words: a well-formed answer that carries
/// an error is a refusal; a connection closed without an answer is an owner without the request (one that
/// does not know a request refuses it by closing). An answer that is there but is not a response is NOT an owner
/// without the request: it is an owner this window does not understand, and a window made on it would be one the
/// owner does not count, which a started owner then ends under it. It is an error, as the answer to the snapshot is.
///
/// On every system the window holds its lease on the same kind of connection as its requests.
async fn open_lease(path: &Path) -> io::Result<Option<Stream>> {
    let mut stream = connect(path).await?;
    let answered = tokio::time::timeout(DEADLINE, async {
        write_frame(&mut stream, &Request::Lease, REQUEST_LIMIT).await?;
        read_frame::<Response>(&mut stream, RESPONSE_LIMIT).await
    })
    .await
    .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?;
    match answered {
        Ok(response) => match response.result {
            Ok(_) => Ok(Some(stream)),
            Err(refusal) => Err(io::Error::other(refusal)),
        },
        // Closed without an answer: an owner that does not have this request.
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
}

/// CLI connection to an existing owner, with fresh state on every query.
pub struct LocalControl {
    path: std::path::PathBuf,
}
impl LocalControl {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}
impl LocalControl {
    /// The state once it is newer than `since` (or unchanged after a while), with its generation: for a script that
    /// follows changes instead of asking again and again.
    pub async fn watch(&self, since: u64) -> Result<(Snapshot, u64), String> {
        watch(&self.path, since)
            .await
            .map_err(|_| "Could not read local application state".into())
    }
}
impl crate::cli::Control for LocalControl {
    async fn state(&self) -> Result<std::sync::Arc<Snapshot>, String> {
        snapshot(&self.path)
            .await
            .map(std::sync::Arc::new)
            .map_err(|_| "Could not read local application state".into())
    }
    async fn command(&self, request: Command) -> Result<Reply, String> {
        command(&self.path, request)
            .await
            .map(|(reply, _)| reply)
            .map_err(|error| match error.kind() {
                io::ErrorKind::Other => error.to_string(),
                io::ErrorKind::TimedOut => {
                    "Local request timed out; command outcome may be unknown".into()
                }
                _ => "Local connection failed; command outcome may be unknown".into(),
            })
    }
}

/// How many looks at the owner in a row may fail before the window takes it that the owner is gone. The window
/// looks once a second, so this is about this many seconds of no answer: more than a socket busy for a moment
/// or a phone's screen waking up, and not so long that a window stays for minutes on an owner that has died.
const FAILURES_BEFORE_GIVING_UP: u32 = 3;

// Held when the build runs, so that the number cannot be changed to a wrong one without the build stopping: more
// than one look, or a single hiccup ends the window for good (which this was written to stop); and no more than ten,
// since the window looks once a second and would stay that many seconds on an owner that is not there.
const _: () = assert!(FAILURES_BEFORE_GIVING_UP > 1);
const _: () = assert!(FAILURES_BEFORE_GIVING_UP <= 10);

/// Whether `failures` looks in a row without an answer is enough to take the owner as gone.
fn gives_up(failures: u32) -> bool {
    failures >= FAILURES_BEFORE_GIVING_UP
}

/// Connect a window to the existing owner without another endpoint or store.
/// The returned task belongs to the window and must be joined or aborted on exit.
pub async fn attach(path: std::path::PathBuf) -> io::Result<(Client, tokio::task::JoinHandle<()>)> {
    use std::sync::Arc;
    use tokio::sync::{mpsc, watch};
    let initial = Arc::new(snapshot(&path).await?);
    let (snapshots, state) = watch::channel(initial);
    let snapshots = Arc::new(snapshots);
    let (commands, mut requests) = mpsc::channel::<crate::runtime::Request>(64);
    // A window is not made if the owner has not counted it. An owner that counts windows and does not have
    // room for this one, or does not answer, ends the attach with an error: a window the owner does not know
    // of would be an open window whose owner can end under it when the others go.
    let lease = open_lease(&path).await?;
    let task = tokio::spawn({
        let (snapshots, path) = (snapshots.clone(), path.clone());
        async move {
        // This window's hold on the owner: kept for as long as this task lives, and let go of however the
        // task ends (the window dropping its client, an error, the process being killed: the system closes
        // the connection). `None` is an owner too old to count windows, which also does not end itself
        // by them, so there is nothing to hold.
        let _lease = lease;
        // Changes come by the watcher below; a command's answer carries none, so that a sample of a
        // finger is not answered with the whole state each time.
        let watcher = tokio::spawn(follow(path.clone(), snapshots.clone()));
        // An older owner does not know a quiet command and closes without an answer, or answers with
        // something this does not read: the command never ran, so it is asked once more the old way.
        let mut quiet = true;
        while let Some(request) = requests.recv().await {
            let mut answer = ask(&path, request.command.clone(), quiet).await;
            if quiet && answer.as_ref().is_err_and(|e| matches!(e.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData)) {
                quiet = false;
                answer = ask(&path, request.command, false).await;
            }
            match answer {
                Ok((reply, state)) => {
                    if let Some(state) = state {
                        snapshots.send_replace(Arc::new(state));
                    }
                    let _ = request.result.send(Ok(reply));
                }
                Err(error) => {
                    let message = if error.kind() == io::ErrorKind::TimedOut {
                        "Local request timed out; command outcome may be unknown"
                    } else {
                        "Local command failed; command outcome may be unknown"
                    };
                    let _ = request.result.send(Err(message));
                }
            }
            if watcher.is_finished() {
                break;
            }
        }
        watcher.abort();
    }});
    Ok((
        Client {
            requests: commands,
            snapshots: state,
        },
        task,
    ))
}

/// Keep `snapshots` the owner's latest: a watch answered on every change, at most about sixty times a
/// second, so a drag on a page is drawn as it goes and nothing is asked while nothing changes. An owner
/// that does not know the watch (an older one) is asked for its state once a second instead. Ends when the
/// owner has really not answered for a while.
async fn follow(path: std::path::PathBuf, snapshots: std::sync::Arc<tokio::sync::watch::Sender<std::sync::Arc<Snapshot>>>) {
    const PACE: Duration = Duration::from_millis(16);
    let mut seen = 0_u64;
    let mut failures = 0_u32;
    let mut polling = false;
    loop {
        let started = tokio::time::Instant::now();
        let answer = if polling {
            tokio::time::sleep(Duration::from_secs(1)).await;
            snapshot(&path).await.map(|state| (state, 0))
        } else {
            watch(&path, seen).await
        };
        match answer {
            Ok((state, generation)) => {
                failures = 0;
                seen = generation;
                snapshots.send_replace(std::sync::Arc::new(state));
            }
            // Closed without an answer, or an answer it could not read: an owner without the watch.
            Err(error) if !polling && matches!(error.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData) => {
                polling = true;
                continue;
            }
            Err(error) => {
                // Counted, not final: one look that failed (a socket busy for a moment, a screen that
                // woke up) is not the owner being gone. Said, so that what ends the bridge is never a
                // guess: the window shows "stopped" only after the owner has really not answered.
                failures += 1;
                tracing::warn!(failures, kind = ?error.kind(), "Owner snapshot request failed");
                if gives_up(failures) {
                    return;
                }
            }
        }
        tokio::time::sleep_until(started + PACE).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_look_at_the_owner_that_fails_is_not_the_owner_being_gone() {
        // Not at once, and not after a couple: a socket busy for a moment or a screen waking is not an owner that died.
        for failures in 0..FAILURES_BEFORE_GIVING_UP {
            assert!(
                !gives_up(failures),
                "{failures} failures in a row ended the bridge"
            );
        }
        // At the threshold, and past it, the owner is taken as gone.
        assert!(gives_up(FAILURES_BEFORE_GIVING_UP));
        assert!(gives_up(FAILURES_BEFORE_GIVING_UP + 1));
        assert!(gives_up(u32::MAX));
    }

    /// The shapes an older owner or window has, which the current ones must still read and write.
    #[test]
    fn ordinary_answers_and_requests_keep_the_older_wire_shape() {
        #[derive(Serialize, Deserialize)]
        #[serde(tag = "operation", deny_unknown_fields)]
        enum OldRequest { Snapshot, Command { command: Box<Command> }, Lease }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct OldResponse { #[allow(dead_code)] result: Result<Option<Reply>, String>, #[allow(dead_code)] snapshot: Option<serde_json::Value> }
        // What this owner answers to anything but a watch has no field an older window does not know.
        let answer = serde_json::to_value(Response { result: Ok(None), snapshot: None, generation: None }).unwrap();
        assert_eq!(answer.as_object().unwrap().keys().collect::<Vec<_>>(), ["result"]);
        assert!(serde_json::from_value::<OldResponse>(answer).is_ok());
        // What an older owner answers carries no generation: this window reads it as none.
        let read: Response = serde_json::from_value(serde_json::json!({ "result": { "Ok": null }, "snapshot": null })).unwrap();
        assert_eq!(read.generation, None);
        // A plain command from this window is exactly what an older owner reads; a quiet one it refuses, which the bridge retries.
        let plain = serde_json::to_vec(&Request::Command { command: Box::new(Command::Shutdown {}), quiet: false }).unwrap();
        assert!(serde_json::from_slice::<OldRequest>(&plain).is_ok());
        let quiet = serde_json::to_vec(&Request::Command { command: Box::new(Command::Shutdown {}), quiet: true }).unwrap();
        assert!(serde_json::from_slice::<OldRequest>(&quiet).is_err());
        let old_request = serde_json::to_vec(&OldRequest::Command { command: Box::new(Command::Shutdown {}) }).unwrap();
        assert!(matches!(serde_json::from_slice::<Request>(&old_request).unwrap(), Request::Command { quiet: false, .. }));
    }
}
