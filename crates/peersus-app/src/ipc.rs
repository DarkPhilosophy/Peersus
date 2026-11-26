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

mod frames;
pub use frames::{FrameKey, FramePoll, FrameReader, ReceivedFrame};

/// The largest request a local client may send: that of the largest picture a window can set, which is
/// `MAX_EDGE` squared pixels of four bytes written in hex, or of the largest language file a person imports, each byte of
/// which JSON may write as six (`\u0001`); with a little for the command around it.
/// Anything larger cannot be a valid command and is turned away before it is read.
pub const REQUEST_LIMIT: usize = {
    let picture = 2 * 4 * (peersus_core::profile::MAX_EDGE as usize).pow(2);
    let language = 6 * peersus_api::language::MAX_FILE_BYTES;
    (if picture > language {
        picture
    } else {
        language
    }) + 4 * 1024
};
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
// The largest kept picture goes to the window in one reply, its pixels written in hex.
const _: () =
    assert!(2 * 4 * (peersus_core::profile::KEPT_EDGE as usize).pow(2) + 4 * 1024 < RESPONSE_LIMIT);
const DEADLINE: Duration = Duration::from_secs(15);

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
enum Request {
    Snapshot,
    /// Boxed: a command carries a whole settings edit, far larger than the other two requests, which carry nothing.
    /// `quiet`: answer with the outcome and no snapshot. For a window that watches for changes anyway, which
    /// would otherwise be sent the whole state for every sample of a finger.
    /// `keep`: the window sends its next command on this same connection instead of making one for each, and
    /// the owner leaves it open for that. A sample of a finger is then a frame each way, with no connection
    /// made, checked, taken and closed around it. An owner that does not know the word refuses the request
    /// unread, as it does a quiet one, and the window asks again without it.
    Command {
        command: Box<Command>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        quiet: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        keep: bool,
    },
    /// A mounted element's report on the window's kept line, never a CLI command.
    PageObservation {
        command: Box<Command>,
    },
    /// A window saying it is attached, and staying so for as long as it holds the connection open.
    Lease,
    /// Answered with the state once its generation is not `since`, or when [`WATCH_WAIT`] has passed with no
    /// change, with the state as it is then: a window keeps one of these open instead of asking for the
    /// state on a clock, so a change reaches it at once and an unchanged state costs nothing. A `since` this
    /// owner has not reached is another owner's count (one that ran before it): answered at once.
    Watch {
        since: u64,
    },
    MountCanvas {
        plugin: peersus_core::plugins::PluginId,
        page: String,
        canvas: String,
        endpoint: String,
    },
}

impl Request {
    fn keeps_connection(&self) -> bool {
        matches!(
            self,
            Self::Command { keep: true, .. } | Self::PageObservation { .. }
        )
    }
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
    read_body(stream, length, maximum).await
}
/// The rest of a frame whose length has been read.
async fn read_body<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
    length: usize,
    maximum: usize,
) -> io::Result<T> {
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
/// The next command on a connection its window keeps. It is waited for with no deadline, since a window
/// with nothing to say says nothing, and read under the deadline once it has begun. Only a command that
/// keeps the connection may follow on it.
async fn next_command(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Request> {
    let length = stream.read_u32().await? as usize;
    let request: Request = tokio::time::timeout(DEADLINE, read_body(stream, length, REQUEST_LIMIT))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    if request.keeps_connection() {
        Ok(request)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Only a command follows on a kept connection",
        ))
    }
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
pub(crate) const REQUESTS_AT_ONCE: usize = 16;
/// What a window writes on its lease when it comes in front of the person, and [`BEHIND`] when it no longer is
/// (minimised, under another window). One byte and no frame: an owner made before windows said this reads and
/// ignores it, and a window made before says nothing and counts as in front.
pub const FRONT: u8 = b'1';
pub const BEHIND: u8 = b'0';
/// What the owner writes on every lease as it ends because it was told to (see `Client::told_to_stop`): its last
/// word to each window, which then closes with it. An owner that ends in any other way writes nothing, and its
/// windows start it again. One byte and no frame, as above: a window made before this never reads its lease.
pub const TOLD: u8 = b'T';
/// How many leases are held at once: a person has a few windows, and one that holds more than this is not
/// a window. One over is answered with a refusal and closed.
const MOST_LEASES: u32 = 8;
/// How many connections are kept open for commands at once: one for each window there can be. One over is
/// closed with no answer and its command not run, which its client reads as an owner that keeps none.
const MOST_KEPT: usize = MOST_LEASES as usize;
/// Frame readers have their own pool and never consume a window's kept command line.
const MOST_FRAME_LINES: usize = MOST_KEPT * peersus_api::canvas::MAX_CANVASES_PER_EXTENSION;

/// The count of connections kept open for commands, which the service watches: each adds a place beside
/// those for ordinary requests, as a lease does, so that windows never leave a command line client waiting.
#[derive(Clone)]
struct Kept(tokio::sync::watch::Sender<usize>);
/// One place among the kept connections, given back when it is dropped.
struct Keeping(tokio::sync::watch::Sender<usize>);
impl Kept {
    fn take(&self, limit: usize) -> Option<Keeping> {
        self.0
            .send_if_modified(|kept| {
                let room = *kept < limit;
                *kept += usize::from(room);
                room
            })
            .then(|| Keeping(self.0.clone()))
    }
}
impl Drop for Keeping {
    fn drop(&mut self) {
        self.0.send_modify(|kept| *kept -= 1);
    }
}

async fn handle(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    client: Client,
    presence: crate::lifetime::Presence,
    mut stopping: tokio::sync::watch::Receiver<std::sync::Arc<Snapshot>>,
    mut generation: tokio::sync::watch::Receiver<u64>,
    kept: Kept,
    frame_lines: Kept,
) -> io::Result<()> {
    // Only the reading and answering of a request is under the deadline: a lease is made to last.
    let mut request = tokio::time::timeout(DEADLINE, read_frame(&mut stream, REQUEST_LIMIT))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    if let Request::MountCanvas {
        plugin,
        page,
        canvas,
        endpoint,
    } = request
    {
        let place = frame_lines.take(MOST_FRAME_LINES);
        use crate::page_frames::MountError;
        let watched = match (place.as_ref(), client.page_frames.as_ref()) {
            (None, _) => Err(MountError::Busy),
            (Some(_), None) => Err(MountError::Closed),
            (Some(_), Some(frames)) => frames.mount_direct(
                FrameKey {
                    plugin,
                    page,
                    canvas,
                },
                endpoint,
            ),
        };
        return tokio::select! {
            result = frames::serve(&mut stream, watched) => result,
            _ = async { while stopping.changed().await.is_ok() {} } => Ok(()),
        };
    }
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
    let mut held = lease.then(|| presence.attach());
    // The owner stopping. Every state it publishes wakes this; only the end of them all is the owner gone.
    let gone = async { while stopping.changed().await.is_ok() {} };
    tokio::pin!(gone);
    // This connection's place among the kept ones, from the first command that asks to keep it.
    let mut keeping = None;
    loop {
        if request.keeps_connection() && keeping.is_none() {
            // No place left: closed before the command is run, with no answer.
            let Some(place) = kept.take(MOST_KEPT) else {
                return Ok(());
            };
            keeping = Some(place);
        }
        let (mut quiet, mut watched) = (false, false);
        let result = match request {
            Request::Snapshot | Request::Lease => Ok(None),
            Request::Watch { since } => {
                watched = true;
                // Answered on a change, or unchanged after a while: either way with what the state is now. The
                // owner stopping answers it too, so that stopping never waits for a watch to run out.
                tokio::select! {
                    _ = tokio::time::timeout(WATCH_WAIT, generation.wait_for(|now| *now != since)) => {}
                    _ = &mut gone => {}
                }
                Ok(None)
            }
            Request::Command {
                command,
                quiet: wanted,
                ..
            } => {
                quiet = wanted;
                answer_command(client.command(*command)).await
            }
            Request::PageObservation { command } => {
                quiet = true;
                if presence.windows() == 0 {
                    Err(crate::runtime::WINDOW_VISIBILITY_ONLY.to_owned())
                } else {
                    answer_command(client.observe_page(*command)).await
                }
            }
            Request::MountCanvas { .. } => return Err(io::ErrorKind::InvalidData.into()),
        };
        // The generation is read before the state, so a client never learns a generation newer than the state it was given.
        let now = *generation.borrow();
        let response = Response {
            result,
            snapshot: (!quiet).then(|| (*client.snapshot()).clone()),
            generation: watched.then_some(now),
        };
        tokio::time::timeout(
            DEADLINE,
            write_frame(&mut stream, &response, RESPONSE_LIMIT),
        )
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
        if keeping.is_none() {
            break;
        }
        // The window's next command, whenever it has one. The window closing ends it, and so does the owner
        // stopping: otherwise stopping would wait for every window that is still open to close first.
        request = tokio::select! {
            next = next_command(&mut stream) => match next {
                Ok(request) => request,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(error) => return Err(error),
            },
            _ = &mut gone => return Ok(()),
        };
    }
    if let Some(held) = &mut held {
        // The window is attached for as long as it keeps this open. All it sends is whether it is in front, a
        // byte each time that changes (see [`FRONT`]); anything else is ignored. A read that ends, by end of
        // stream or by an error, is the window going. The owner stopping ends it too: otherwise stopping would
        // wait for every window that is still open to close first.
        let mut said = [0_u8; 64];
        loop {
            tokio::select! {
                read = stream.read(&mut said) => match read {
                    Ok(read) if read > 0 => {
                        // Only the last word counts: what the window is now.
                        if let Some(in_front) = said[..read].iter().rev().find_map(|byte| match *byte {
                            FRONT => Some(true),
                            BEHIND => Some(false),
                            _ => None,
                        }) {
                            held.set_front(in_front);
                        }
                    }
                    _ => break,
                },
                _ = &mut gone => {
                    if client.told_to_stop() {
                        let _ = tokio::time::timeout(DEADLINE, stream.write_all(&[TOLD])).await;
                    }
                    break;
                }
            }
        }
    }
    drop(held);
    Ok(())
}

async fn answer_command(
    answer: impl Future<Output = Result<Reply, &'static str>>,
) -> Result<Option<Reply>, String> {
    tokio::pin!(answer);
    let result = match tokio::time::timeout(DEADLINE, &mut answer).await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!("A command has waited for the owner longer than the deadline");
            answer.await
        }
    };
    result.map(Some).map_err(str::to_owned)
}

/// Bounded clients, bounded frames and timeouts; dropping this future aborts clients.
/// Stops accepting when the application owner exits, then drains in-flight replies.
///
/// A window's lease lives as long as the window and is not cut by the deadline of an ordinary request. It
/// is counted in `presence` and takes none of the places ordinary requests have: the places for requests
/// are [`REQUESTS_AT_ONCE`], and each open lease adds one more, so that windows attached to the owner can
/// never leave a command line client waiting. At most [`MOST_LEASES`] are held; one over is turned away.
/// A connection kept open for a window's commands adds a place in the same way, and at most [`MOST_KEPT`] are.
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
    let (kept, mut keeping) = tokio::sync::watch::channel(0_usize);
    let kept = Kept(kept);
    let (frame_lines, mut frame_keeping) = tokio::sync::watch::channel(0_usize);
    let frame_lines = Kept(frame_lines);
    loop {
        let room = REQUESTS_AT_ONCE
            + presence.windows() as usize
            + *keeping.borrow_and_update()
            + *frame_keeping.borrow_and_update();
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
                let (client, presence, stopping, generation, kept) = (client.clone(), presence.clone(), client.subscribe(), generation.clone(), kept.clone());
                let frame_lines = frame_lines.clone();
                tasks.spawn(async move {
                    // Only the kind and this crate's own fixed words are said, never what was asked.
                    if let Err(error) = handle(stream, client, presence, stopping, generation, kept, frame_lines).await
                        && error.kind() != io::ErrorKind::UnexpectedEof
                    {
                        tracing::warn!(kind = ?error.kind(), reason = %error, "A local request failed");
                    }
                });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
            // A connection kept or let go changes how many may be taken: counted again at the top.
            _ = keeping.changed() => {}
            _ = frame_keeping.changed() => {}
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

/// The state and its generation, once that is not `since` or [`WATCH_WAIT`] has passed.
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
            keep: false,
        },
    )
    .await?;
    let reply = response
        .result
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing command response"))?;
    Ok((reply, response.snapshot))
}
fn turned_away(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData
    )
}

/// How a window asks its commands, by what the owner has shown it knows. It starts with the most this window
/// knows and steps down once for each form the owner turns away.
#[derive(Clone, Copy, PartialEq)]
enum Way {
    /// One connection for all of them, and no snapshot in the answers.
    Kept,
    /// A connection for each, and no snapshot in the answers.
    Quiet,
    /// A connection for each, answered with the whole state: what the oldest owner knows.
    Plain,
}
/// The connection a window's commands go on, for an owner that keeps one.
#[derive(Default)]
struct Line {
    /// Open, and with nothing on it left to read.
    stream: Option<Stream>,
    /// The owner has answered on the connection last used. One that then ends without an answer is the owner
    /// going, and not an owner that did not read the request: the command is not asked a second time. A
    /// connection just made has proved nothing, so an owner that turns it away is still asked the older way.
    proven: bool,
}
/// Whether the other end of a connection with nothing on it to read still holds it.
fn still_open(stream: &Stream) -> bool {
    stream
        .try_read(&mut [0_u8; 1])
        .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
}
/// Ask on the kept connection, making it first if there is none. It is put back only after a whole answer
/// was read: after anything else what is on it is not known, and the next command makes a new one.
async fn ask_kept(
    path: &Path,
    line: &mut Line,
    command: Command,
    observation: bool,
) -> io::Result<Reply> {
    let command = Box::new(command);
    let request = if observation {
        Request::PageObservation { command }
    } else {
        Request::Command {
            command,
            quiet: true,
            keep: true,
        }
    };
    // One the owner has closed since (it stopped; another may have started) is found so before anything is
    // written on it: the command then goes on a new one, and is still asked once.
    let held = line.stream.take().filter(still_open);
    line.proven &= held.is_some();
    let (stream, response) = tokio::time::timeout(DEADLINE, async {
        let mut stream = match held {
            Some(stream) => stream,
            None => connect(path).await?,
        };
        write_frame(&mut stream, &request, REQUEST_LIMIT).await?;
        let response = read_frame::<Response>(&mut stream, RESPONSE_LIMIT).await?;
        io::Result::Ok((stream, response))
    })
    .await
    .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    (line.stream, line.proven) = (Some(stream), true);
    response
        .result
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing command response"))
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
    /// The state once its generation is not `since` (or unchanged after a while), with its generation: for a script that
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
    // The instance lock exists only where a separate owner process does; elsewhere the trait's default applies.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    async fn stop(&self) -> Result<Reply, String> {
        let directory = self
            .path
            .parent()
            .ok_or("Application data directory unavailable")?;
        // Leave time for the desktop deployer's 15-second subprocess deadline.
        tokio::time::timeout(Duration::from_secs(12), async {
            let mut stream = connect(&self.path)
                .await
                .map_err(|_| "Could not connect to the running application")?;
            // Use the original command shape. An older owner may not know `quiet` or `keep`.
            write_frame(
                &mut stream,
                &Request::Command {
                    command: Box::new(Command::Shutdown {}),
                    quiet: false,
                    keep: false,
                },
                REQUEST_LIMIT,
            )
            .await
            .map_err(|_| "Could not send a stop request; its outcome may be unknown")?;

            let stopped = async {
                loop {
                    if crate::instance::Instance::acquire(directory)
                        .map_err(|_| "Cannot acquire private application directory")?
                        .is_some()
                    {
                        return Ok::<_, String>(Reply::Applied);
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            };
            tokio::pin!(stopped);

            // Drain the response so a large snapshot cannot block teardown, but don't decode it.
            // Neither a reply nor a closed command socket proves that the owner released its lock.
            let mut response = stream.take((size_of::<u32>() + RESPONSE_LIMIT) as u64);
            let mut sink = tokio::io::sink();
            tokio::select! {
                result = &mut stopped => result,
                _ = tokio::io::copy(&mut response, &mut sink) => stopped.await,
            }
        })
        .await
        .map_err(|_| "The application did not stop in time")?
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

/// A window's word to the owner on whether it is in front of the person. The owner announces an offer outside
/// the window only while no window is (see `offer_panel`). Saying the same thing again sends nothing.
#[derive(Clone)]
pub struct Front(tokio::sync::watch::Sender<Option<bool>>);

impl Front {
    pub fn tell(&self, in_front: bool) {
        self.0.send_if_modified(|said| {
            let changed = *said != Some(in_front);
            *said = Some(in_front);
            changed
        });
    }
}

/// Connect a window to the existing owner without another endpoint or store.
/// The returned task belongs to the window and must be joined or aborted on exit.
pub async fn attach(
    path: std::path::PathBuf,
) -> io::Result<(Client, tokio::task::JoinHandle<bool>)> {
    let (client, task, _front) = attach_as(path, true).await?;
    Ok((client, task))
}

/// Attach to the control service as usual, but share the same-process owner's Canvas relay.
/// Completed frames go straight to the window without a kept frame socket or a second buffer.
pub async fn attach_with_local_frames(
    path: std::path::PathBuf,
    owner: &Client,
) -> io::Result<(Client, tokio::task::JoinHandle<bool>)> {
    let frames = owner
        .page_frames
        .clone()
        .ok_or(io::ErrorKind::InvalidInput)?;
    let (mut client, task) = attach(path).await?;
    client.page_frames = Some(frames);
    Ok((client, task))
}

/// [`attach`], with the way for the window to say whether it is in front.
pub async fn attach_window(
    path: std::path::PathBuf,
) -> io::Result<(Client, tokio::task::JoinHandle<bool>, Front)> {
    attach_as(path, true).await
}

/// Connect a surface that is not a window of the application (the panel of an offer): the same state and the same
/// commands, and no lease, so that it is never what keeps an owner running, nor a window that is in front.
pub async fn attach_beside(
    path: std::path::PathBuf,
) -> io::Result<(Client, tokio::task::JoinHandle<bool>)> {
    let (client, task, _front) = attach_as(path, false).await?;
    Ok((client, task))
}

async fn attach_as(
    path: std::path::PathBuf,
    counted: bool,
) -> io::Result<(Client, tokio::task::JoinHandle<bool>, Front)> {
    use std::sync::Arc;
    use tokio::sync::{mpsc, watch};
    let initial = Arc::new(snapshot(&path).await?);
    let (snapshots, state) = watch::channel(initial);
    let snapshots = Arc::new(snapshots);
    let (commands, mut requests) = mpsc::channel::<crate::runtime::Request>(64);
    let (front, mut fronts) = watch::channel(None::<bool>);
    // A window is not made if the owner has not counted it. An owner that counts windows and does not have
    // room for this one, or does not answer, ends the attach with an error: a window the owner does not know
    // of would be an open window whose owner can end under it when the others go.
    let lease = if counted {
        open_lease(&path).await?
    } else {
        None
    };
    let task = tokio::spawn({
        let (snapshots, path) = (snapshots.clone(), path.clone());
        async move {
            // This window's hold on the owner: kept for as long as this task lives, and let go of however the
            // task ends (the window dropping its client, an error, the process being killed: the system closes
            // the connection). `None` is an owner too old to count windows, which also does not end itself
            // by them, so there is nothing to hold.
            let mut lease = lease;
            // Whether the window can still say where it is: not once it has let go of its way to.
            let mut says = true;
            // Changes come by the watcher below; a command's answer carries none, so that a sample of a
            // finger is not answered with the whole state each time.
            let mut watcher = tokio::spawn(follow(path.clone(), snapshots.clone()));
            // An older owner does not know a kept or a quiet command and closes without an answer, or answers
            // with something this does not read: the command never ran, so it is asked once more the older way.
            let (mut way, mut line) = (Way::Kept, Line::default());
            // Whether the loop ended because the owner went, and not because the window let go of its client.
            let mut owner_gone = false;
            loop {
                // The watcher ends when the owner has really gone: the bridge ends with it, whether or not
                // the window is sending anything, so a window left open sees its owner stop.
                let request = tokio::select! {
                    request = requests.recv() => match request { Some(request) => request, None => break },
                    _ = &mut watcher => {
                        owner_gone = true;
                        break;
                    }
                    said = fronts.changed(), if says => {
                        says = said.is_ok();
                        let word = *fronts.borrow_and_update();
                        // One byte on the lease (see [`FRONT`]). A lease that cannot be written is one the owner
                        // has closed: the watcher sees the owner go, and nothing is done about it here.
                        if let (true, Some(stream), Some(in_front)) = (says, lease.as_mut(), word) {
                            let _ = stream.write_all(&[if in_front { FRONT } else { BEHIND }]).await;
                        }
                        continue;
                    }
                };
                if request.observation {
                    // An observation cannot fall back to a plain command on an older owner.
                    let result = if lease.is_some() && way == Way::Kept {
                        ask_kept(&path, &mut line, request.command, true)
                            .await
                            .map_err(|_| "Window observation was not accepted")
                    } else {
                        Err("Window observation is unavailable")
                    };
                    let _ = request.result.send(result);
                    continue;
                }
                let answer = loop {
                    let answer = match way {
                        Way::Kept => ask_kept(&path, &mut line, request.command.clone(), false)
                            .await
                            .map(|reply| (reply, None)),
                        Way::Quiet => ask(&path, request.command.clone(), true).await,
                        Way::Plain => ask(&path, request.command.clone(), false).await,
                    };
                    let turned_away = answer.as_ref().is_err_and(turned_away);
                    match way {
                        Way::Kept if turned_away && !line.proven => way = Way::Quiet,
                        Way::Quiet if turned_away => way = Way::Plain,
                        _ => break answer,
                    }
                };
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
            }
            watcher.abort();
            // The owner's last word on the lease, read only once it has gone: everything it wrote is there by then.
            match lease.as_mut() {
                Some(lease) if owner_gone => told(lease).await,
                _ => false,
            }
        }
    });
    Ok((
        Client {
            requests: commands,
            snapshots: state,
            page_frames: None,
            frame_window: (counted && cfg!(not(target_os = "android")))
                .then(|| crate::page_frames::Window::new(path)),
            // A window is not what feeds captured input: nothing is ever said on this one.
            surface_failures: tokio::sync::broadcast::channel(1).0,
            // Nor is a window what is told to stop: only the owner's own client is asked that.
            told_to_stop: Arc::default(),
        },
        task,
        Front(front),
    ))
}

/// Whether an owner that has gone wrote [`TOLD`] on this lease before it went. Bounded: a lease whose end is not
/// seen in a second says nothing.
async fn told(lease: &mut (impl AsyncRead + Unpin)) -> bool {
    let mut said = [0_u8; 64];
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match lease.read(&mut said).await {
                Ok(read) if read > 0 => {
                    if said[..read].contains(&TOLD) {
                        return true;
                    }
                }
                _ => return false,
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// Keep `snapshots` the owner's latest: a watch answered on every change, at most about sixty times a
/// second, so a drag on a page is drawn as it goes and nothing is asked while nothing changes. An owner
/// that does not know the watch (an older one) is asked for its state once a second instead. Ends when the
/// owner has really not answered for a while.
async fn follow(
    path: std::path::PathBuf,
    snapshots: std::sync::Arc<tokio::sync::watch::Sender<std::sync::Arc<Snapshot>>>,
) {
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
            Err(error) if !polling && turned_away(&error) => {
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

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_waits_for_an_owner_with_an_incompatible_snapshot_to_release_its_lock() {
        use crate::{
            cli::{Cli, Output},
            instance::Instance,
        };
        use clap::Parser;

        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("peersus");
        let mut instance = Instance::acquire(&directory).unwrap().unwrap();
        let listener = instance.listen().unwrap();
        let path = directory.join("control.sock");
        let (answered, response_written) = tokio::sync::oneshot::channel();
        let (release, may_exit) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(async move {
            let mut state_requests = 0;
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let result = match read_frame::<Request>(&mut stream, REQUEST_LIMIT)
                    .await
                    .unwrap()
                {
                    Request::Snapshot => {
                        state_requests += 1;
                        None
                    }
                    Request::Command {
                        command,
                        quiet: false,
                        keep: false,
                    } if matches!(*command, Command::Shutdown {}) => Some(Reply::Applied),
                    _ => panic!("unexpected request to the old owner"),
                };
                let stopping = result.is_some();
                let response = serde_json::json!({
                    "result": { "Ok": result },
                    "snapshot": { "settings": "legacy format" },
                });
                write_frame(&mut stream, &response, RESPONSE_LIMIT)
                    .await
                    .unwrap();
                drop(stream);
                if stopping {
                    answered.send(()).unwrap();
                    may_exit.await.unwrap();
                    break;
                }
            }
            drop(listener);
            drop(instance);
            state_requests
        });
        assert_eq!(
            snapshot(&path).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let control = LocalControl::new(path);
        let stop = Cli::try_parse_from(["peersus", "stop"])
            .unwrap()
            .execute(&control);
        tokio::pin!(stop);
        tokio::select! {
            outcome = &mut stop => panic!("stop returned before owner's exit: {:?}", outcome.err()),
            written = response_written => written.unwrap(),
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut stop)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        assert!(matches!(stop.await, Ok(Output::Reply(Reply::Applied))));
        assert_eq!(
            owner.await.unwrap(),
            1,
            "only the explicit probe should ask for state"
        );
        assert!(Instance::acquire(&directory).unwrap().is_some());
    }

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
        enum OldRequest {
            Snapshot,
            Command { command: Box<Command> },
            Lease,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct OldResponse {
            #[allow(dead_code)]
            result: Result<Option<Reply>, String>,
            #[allow(dead_code)]
            snapshot: Option<serde_json::Value>,
        }
        // What this owner answers to anything but a watch has no field an older window does not know.
        let answer = serde_json::to_value(Response {
            result: Ok(None),
            snapshot: None,
            generation: None,
        })
        .unwrap();
        assert_eq!(
            answer.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["result"]
        );
        assert!(serde_json::from_value::<OldResponse>(answer).is_ok());
        // What an older owner answers carries no generation: this window reads it as none.
        let read: Response = serde_json::from_value(
            serde_json::json!({ "result": { "Ok": null }, "snapshot": null }),
        )
        .unwrap();
        assert_eq!(read.generation, None);
        // A plain command from this window is exactly what an older owner reads; a quiet one it refuses, which the bridge retries.
        let command = |quiet, keep| {
            serde_json::to_vec(&Request::Command {
                command: Box::new(Command::Shutdown {}),
                quiet,
                keep,
            })
            .unwrap()
        };
        assert!(serde_json::from_slice::<OldRequest>(&command(false, false)).is_ok());
        assert!(serde_json::from_slice::<OldRequest>(&command(true, false)).is_err());
        let old_request = serde_json::to_vec(&OldRequest::Command {
            command: Box::new(Command::Shutdown {}),
        })
        .unwrap();
        assert!(matches!(
            serde_json::from_slice::<Request>(&old_request).unwrap(),
            Request::Command {
                quiet: false,
                keep: false,
                ..
            }
        ));
        // An owner that knows a quiet command and not a kept one reads the first and refuses the second, and
        // the oldest refuses both: each is then asked again one step older.
        #[derive(Deserialize)]
        #[serde(tag = "operation", deny_unknown_fields)]
        enum QuietRequest {
            #[allow(dead_code)]
            Command {
                command: Box<Command>,
                #[serde(default)]
                quiet: bool,
            },
        }
        assert!(serde_json::from_slice::<QuietRequest>(&command(true, false)).is_ok());
        assert!(serde_json::from_slice::<QuietRequest>(&command(true, true)).is_err());
        assert!(serde_json::from_slice::<OldRequest>(&command(true, true)).is_err());
    }
}
