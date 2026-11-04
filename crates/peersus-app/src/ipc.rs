//! Same-user local control, forwarding commands to the one application owner.
//! The launcher owns socket creation/removal; this module never deletes a path.
//! A lost response is an uncertain outcome, not permission to retry a command.
use crate::runtime::{Client, Reply};
use peersus_core::model::{Command, Snapshot};
#[cfg(windows)]
use peersus_windows::Listener;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{io, path::Path, time::Duration};
#[cfg(unix)]
use tokio::net::{UnixListener as Listener, UnixStream};
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
    Command {
        command: Box<Command>,
    },
    /// A window saying it is attached, and staying so for as long as it holds the connection open.
    Lease,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    result: Result<Option<Reply>, String>,
    snapshot: Snapshot,
}
#[cfg(unix)]
fn authenticate(stream: &UnixStream) -> io::Result<()> {
    if stream.peer_cred()?.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Local service belongs to another user",
        ));
    }
    Ok(())
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
    let bytes =
        serde_json::to_vec(value).map_err(|_| io::Error::other("Could not encode local frame"))?;
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
) -> io::Result<()> {
    // Only the reading and answering of a request is under the deadline: a lease is made to last.
    let request = tokio::time::timeout(DEADLINE, read_frame(&mut stream, REQUEST_LIMIT))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    let lease = matches!(request, Request::Lease);
    if lease && presence.windows() >= MOST_LEASES {
        let refused = Response {
            result: Err("Too many windows are attached".to_owned()),
            snapshot: (*client.snapshot()).clone(),
        };
        return write_frame(&mut stream, &refused, RESPONSE_LIMIT).await;
    }
    // Taken before the answer is written and held until this function ends, however it ends: the window
    // closing, an error, or the task being cancelled all drop it.
    let held = lease.then(|| presence.attach());
    let result = match request {
        Request::Snapshot | Request::Lease => Ok(None),
        Request::Command { command } => client
            .command(*command)
            .await
            .map(Some)
            .map_err(str::to_owned),
    };
    let response = Response {
        result,
        snapshot: (*client.snapshot()).clone(),
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
    loop {
        let room = REQUESTS_AT_ONCE + presence.windows() as usize;
        tokio::select! {
            changed = state.changed() => { if changed.is_err() { break; } }
            accepted = listener.accept(), if tasks.len() < room => {
                let (stream, _) = accepted?;
                #[cfg(unix)]
                if authenticate(&stream).is_err() { continue; }
                let (client, presence, stopping) = (client.clone(), presence.clone(), client.subscribe());
                tasks.spawn(async move { let _ = handle(stream, client, presence, stopping).await; });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    while tasks.join_next().await.is_some() {}
    Ok(())
}
async fn exchange(path: &Path, request: Request) -> io::Result<Response> {
    tokio::time::timeout(DEADLINE, async {
        #[cfg(unix)]
        let mut stream = {
            let stream = UnixStream::connect(path).await?;
            authenticate(&stream)?;
            stream
        };
        #[cfg(windows)]
        let mut stream = peersus_windows::connect(path).await?;
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
/// Read current state without creating a second network host.
pub async fn snapshot(path: &Path) -> io::Result<Snapshot> {
    let response = exchange(path, Request::Snapshot).await?;
    response.result.map_err(io::Error::other)?;
    Ok(response.snapshot)
}
/// Execute once; the returned snapshot includes acknowledged preference changes.
pub async fn command(path: &Path, command: Command) -> io::Result<(Reply, Snapshot)> {
    let response = exchange(
        path,
        Request::Command {
            command: Box::new(command),
        },
    )
    .await?;
    let reply = response
        .result
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing command response"))?;
    Ok((reply, response.snapshot))
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
/// Unix only for now: the Windows transport is not part of this workspace, so there is nothing here to
/// build it on, and on Windows a window holds no lease, which only means the owner is not ended with it.
#[cfg(unix)]
async fn open_lease(path: &Path) -> io::Result<Option<UnixStream>> {
    let mut stream = UnixStream::connect(path).await?;
    authenticate(&stream)?;
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
    let (commands, mut requests) = mpsc::channel::<crate::runtime::Request>(64);
    // A window is not made if the owner has not counted it. An owner that counts windows and does not have
    // room for this one, or does not answer, ends the attach with an error: a window the owner does not know
    // of would be an open window whose owner can end under it when the others go.
    #[cfg(unix)]
    let lease = open_lease(&path).await?;
    let task = tokio::spawn(async move {
        // This window's hold on the owner: kept for as long as this task lives, and let go of however the
        // task ends (the window dropping its client, an error, the process being killed: the system closes
        // the connection). `None` is an owner too old to count windows, which also does not end itself
        // by them, so there is nothing to hold.
        #[cfg(unix)]
        let _lease = lease;
        let mut refresh = tokio::time::interval(Duration::from_secs(1));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut failures = 0_u32;
        loop {
            tokio::select! {
                request = requests.recv() => {
                    let Some(request) = request else { break };
                    match command(&path, request.command).await {
                        Ok((reply, state)) => {
                            snapshots.send_replace(Arc::new(state));
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
                }
                _ = refresh.tick() => {
                    match snapshot(&path).await {
                        Ok(state) => {
                            failures = 0;
                            snapshots.send_replace(Arc::new(state));
                        }
                        Err(error) => {
                            // Counted, not final: one look that failed (a socket busy for a moment, a screen that
                            // woke up) is not the owner being gone. Said, so that what ends the bridge is never a
                            // guess: the window shows "stopped" only after the owner has really not answered.
                            failures += 1;
                            tracing::warn!(failures, kind = ?error.kind(), "Owner snapshot request failed");
                            if gives_up(failures) {
                                break;
                            }
                        }
                    }
                }
            }
        }
    });
    Ok((
        Client {
            requests: commands,
            snapshots: state,
        },
        task,
    ))
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
}
