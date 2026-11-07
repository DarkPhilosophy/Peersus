//! What the clipboard plugin does when the application calls it, apart from where its bytes come
//! from and go.
//!
//! The operating system's clipboard and the sockets to the application are both passed in, so
//! everything that decides what is sent, what is accepted and what is answered can be tested
//! without a desktop and without a running application. `main` only joins this to the real ones.
//!
//! Two calls are handled:
//!
//! * `share`: the user chose a device. The text on the clipboard is read, sent to that device
//!   through the application's `peersus.peers` service, and nothing else is ever read or sent.
//! * `receive`: the application announces text a device sent. It is collected over the data
//!   connection, checked, and only then placed on the clipboard.
use crate::wire::{self, Item};
use peersus_api::message::{Delivery, Operation, Outcome};
use peersus_plugin::{
    data::DataHello,
    frame::{self, CONTROL_LIMIT},
};
use serde_json::{Value, json};
use std::{collections::VecDeque, io::Cursor};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The service on the other device the text is for, and its version.
pub const SERVICE: &str = "clipboard.text";
pub const VERSION: u32 = 1;

/// Largest data handshake the host accepts.
const HELLO_LIMIT: usize = 1024;

/// Most bytes of one item ever read from a data connection: its header and the largest text,
/// and one more, so that something longer is seen to be longer without reading the rest.
const MOST_READ: u64 = wire::HEADER as u64 + wire::MAX_TEXT as u64 + 1;

pub use peersus_clipboard_platform::{Clipboard, Reading};

/// Only this wrapper can consume the selected payload. The ordinary action stays independent.
struct Selected<'a, C> {
    clipboard: &'a mut C,
    context: u64,
}
impl<C: Clipboard> Clipboard for Selected<'_, C> {
    async fn text(&mut self) -> Option<String> {
        self.clipboard.selected_text(self.context).await
    }
    async fn set_text(&mut self, _: &str) -> bool {
        false
    }
    async fn can_share(&mut self) -> bool {
        self.clipboard.selected_valid(self.context).await
    }
}

/// What a call is answered with. Fixed words: nothing read from the clipboard, a device or the
/// network is repeated back, because the answer reaches the user's screen and the log.
pub mod answer {
    pub const NOTHING: &str = "There is no text to share";
    pub const REFUSED: &str = "That device cannot be sent to";
    pub const FAILED: &str = "The text could not be sent";
    pub const NOT_ACCEPTED: &str = "What was sent was not accepted";
    pub const NOT_PLACED: &str = "The text could not be placed on the clipboard";
    pub const UNKNOWN: &str = "Nothing here answers that";
}

/// The device an action was pressed for, as the application names it to this plugin.
fn peer(args: &Value) -> Option<&str> {
    args.get("peer")?.as_str()
}

/// Everything one `share` needs from the connection to the application.
pub struct Control<'a, S> {
    pub stream: &'a mut S,
    /// The secret that proves this process is the one the application started.
    pub token: &'a str,
    /// What arrived while `share` waited for its own answer, to be handled by the caller.
    pub waiting: &'a mut VecDeque<Delivery>,
}

/// Send the clipboard's text to `device`: ask the application to open a stream, then push the
/// encoded text over a data connection naming the number it issued.
///
/// `connect` opens a data connection. It is a parameter so a test does not need a socket.
pub async fn share<C, S, D, F, Fut>(
    clipboard: &mut C,
    control: Control<'_, S>,
    device: &str,
    connect: F,
) -> Result<(), &'static str>
where
    C: Clipboard,
    S: AsyncRead + AsyncWrite + Unpin,
    D: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<D>>,
{
    share_until(clipboard, control, device, connect, None).await
}

/// Automatic sends expire rather than replaying a capture after a slow control response.
pub async fn share_until<C, S, D, F, Fut>(
    clipboard: &mut C,
    control: Control<'_, S>,
    device: &str,
    connect: F,
    expires: Option<std::time::Instant>,
) -> Result<(), &'static str>
where
    C: Clipboard,
    S: AsyncRead + AsyncWrite + Unpin,
    D: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::io::Result<D>>,
{
    let text = clipboard.text().await.ok_or(answer::NOTHING)?;
    // Encoding is what enforces the size limit, before anything is asked of the application.
    let bytes = wire::encode(&Item::Text(text)).map_err(|_| answer::NOTHING)?;
    let call = Operation::Call {
        id: 1,
        provider: "host".into(),
        service: "peersus.peers".into(),
        version: 1,
        method: "send".into(),
        args: json!({ "device": device, "service": SERVICE, "version": VERSION }),
        timeout_ms: 5_000,
    };
    frame::write(&mut *control.stream, &call, CONTROL_LIMIT)
        .await
        .map_err(|_| answer::FAILED)?;
    let claim = loop {
        match frame::read::<Delivery>(&mut *control.stream, CONTROL_LIMIT).await {
            Ok(Delivery::Result { id: 1, result }) => match result {
                Outcome::Ok { value } => {
                    break value
                        .get("claim")
                        .and_then(Value::as_u64)
                        .ok_or(answer::FAILED)?;
                }
                // Whatever the reason, the application said the same thing, and so does this.
                Outcome::Error { .. } => return Err(answer::REFUSED),
            },
            // Anything else that arrives while waiting is somebody's call, and is not this
            // one's to throw away: a device may send its own text just as the user shares.
            // It is kept, in the order it came, for the caller to answer afterwards.
            Ok(other) if control.waiting.len() < 32 => control.waiting.push_back(other),
            Ok(_) => return Err(answer::FAILED),
            Err(_) => return Err(answer::FAILED),
        }
    };
    let now = std::time::Instant::now();
    let remaining = match expires {
        Some(expires) => expires
            .checked_duration_since(now)
            .ok_or(answer::NOTHING)?
            .min(std::time::Duration::from_secs(2)),
        None => std::time::Duration::from_secs(8),
    };
    // Freshness bounds claiming and writing the captured text, not waiting for a
    // transfer already admitted by the host to finish over a slower peer path.
    let mut data = tokio::time::timeout(remaining, async {
        let mut data = connect().await.map_err(|_| answer::FAILED)?;
        let hello = DataHello {
            token: control.token.to_owned(),
            ticket: 0,
            claim: Some(claim),
        };
        frame::write(&mut data, &hello, HELLO_LIMIT)
            .await
            .map_err(|_| answer::FAILED)?;
        if !clipboard.can_share().await {
            return Err(answer::NOTHING);
        }
        data.write_all(&bytes).await.map_err(|_| answer::FAILED)?;
        data.shutdown().await.map_err(|_| answer::FAILED)?;
        Ok::<_, &'static str>(data)
    })
    .await
    .map_err(|_| answer::FAILED)??;
    // A host that fails or reaches its pump idle limit closes this socket. Keep a
    // separate ceiling as well, without applying the capture's two-second budget.
    let delivered: u64 = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        frame::read(&mut data, HELLO_LIMIT),
    )
    .await
    .map_err(|_| answer::FAILED)?
    .map_err(|_| answer::FAILED)?;
    if delivered != bytes.len() as u64 {
        return Err(answer::FAILED);
    }
    Ok(())
}

/// Collect the text a device sent over a data connection answering call `ticket`, and place
/// it on the clipboard.
///
/// What is read is limited before it is looked at, so a device that sends without end costs
/// a bounded amount; what was read must be exactly one valid item, and only text is taken.
pub async fn receive<C, D, F, Fut>(
    clipboard: &mut C,
    ticket: u64,
    token: &str,
    connect: F,
) -> Result<(), &'static str>
where
    C: Clipboard,
    D: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<D>>,
{
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(11);
    let mut data = connect().await.map_err(|_| answer::NOT_ACCEPTED)?;
    let hello = DataHello {
        token: token.to_owned(),
        ticket,
        claim: None,
    };
    frame::write(&mut data, &hello, HELLO_LIMIT)
        .await
        .map_err(|_| answer::NOT_ACCEPTED)?;
    let mut bytes = Vec::new();
    // A byte bound alone cannot stop a peer that sends a header then never closes.
    // Finish before the host's 15-second inbound-call deadline.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        (&mut data).take(MOST_READ).read_to_end(&mut bytes).await
    })
    .await
    .map_err(|_| answer::NOT_ACCEPTED)?
    .map_err(|_| answer::NOT_ACCEPTED)?;
    let mut cursor = Cursor::new(bytes);
    let item = wire::decode(&mut cursor);
    // One item and nothing after it: a sender that attaches more is not sending what was
    // agreed, and the extra byte of the read bound is what lets that be seen.
    let whole = cursor.position() == cursor.get_ref().len() as u64;
    match item {
        Ok(Item::Text(text)) if whole => {
            if clipboard.set_text_until(&text, deadline).await {
                Ok(())
            } else {
                Err(answer::NOT_PLACED)
            }
        }
        // A picture, anything else, or an item with something after it, is not taken.
        Ok(_) | Err(_) => Err(answer::NOT_ACCEPTED),
    }
}

/// Take off `queue` every call the application has since cancelled, and the notices that said
/// so.
///
/// A call waits its turn behind whatever is running, and its deadline can pass while it does:
/// the application then sends `Cancelled` for it. Running it anyway would read the clipboard
/// and ask for a send that nobody is waiting for, so those are removed first. A notice for
/// something that is not in the queue (already answered, or running) is simply dropped; it
/// never removes a different call, because calls are matched by ticket and only by ticket.
pub fn withdraw_cancelled(queue: &mut VecDeque<Delivery>) {
    let given_up: Vec<u64> = queue
        .iter()
        .filter_map(|delivery| match delivery {
            Delivery::Cancelled { ticket } => Some(*ticket),
            _ => None,
        })
        .collect();
    queue.retain(|delivery| match delivery {
        Delivery::Cancelled { .. } => false,
        Delivery::Call { ticket, .. } => !given_up.contains(ticket),
        _ => true,
    });
}

/// Bring into `waiting` everything the application has already sent and that is sitting
/// unread on `stream`, without waiting for anything more. Returns `false` when the
/// application has gone and the plugin should end.
///
/// While a call runs, nothing reads the connection, so what arrives meanwhile, including the
/// notice that a queued call has been given up on, just sits there. Reading it before the
/// next call is chosen is what lets `withdraw_cancelled` see it.
///
/// It never blocks on an empty connection, and it never abandons a delivery halfway: the
/// first byte is taken with a read that cannot wait (a readiness report can be wrong, and
/// then there is simply nothing, so it returns), and only once one byte is really there is the
/// rest of that one delivery waited for, which is brief because the application writes a
/// delivery in one piece.
#[cfg(unix)]
pub async fn drain(stream: &mut tokio::net::UnixStream, waiting: &mut VecDeque<Delivery>) -> bool {
    use std::io::ErrorKind;
    loop {
        // The runtime only knows the connection is readable once its reactor has seen it, and
        // a read that cannot wait would report "nothing" before then. So it is given a very
        // short moment to say so. Waiting for readiness can safely be abandoned (it reads
        // nothing), unlike a read of a delivery, which never is.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(5), stream.readable()).await;
        let mut first = [0u8; 1];
        match stream.try_read(&mut first) {
            // End of the connection: the application has gone.
            Ok(0) => return false,
            Ok(_) => {}
            // Nothing there, whatever the readiness said: all that was waiting is in.
            Err(error) if error.kind() == ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
        // One byte of the length is taken; the other three and the body complete the delivery.
        let mut rest = [0u8; 3];
        if stream.read_exact(&mut rest).await.is_err() {
            return false;
        }
        let length = u32::from_be_bytes([first[0], rest[0], rest[1], rest[2]]) as usize;
        if length == 0 || length > CONTROL_LIMIT {
            return false;
        }
        let mut body = vec![0u8; length];
        if stream.read_exact(&mut body).await.is_err() {
            return false;
        }
        match serde_json::from_slice::<Delivery>(&body) {
            Ok(delivery) if waiting.len() < 32 => waiting.push_back(delivery),
            Ok(_) => return false,
            Err(_) => return false,
        }
    }
}

/// Receiving never reads control frames itself. While it waits on data/platform work,
/// listen for revocation without ever abandoning a partially consumed control frame.
async fn receive_action<C, S, D, F, Fut>(
    clipboard: &mut C,
    stream: &mut S,
    waiting: &mut VecDeque<Delivery>,
    ticket: u64,
    token: &str,
    connect: F,
) -> Option<Outcome>
where
    C: Clipboard,
    S: AsyncRead + AsyncWrite + Unpin,
    D: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::io::Result<D>>,
{
    let receiving = receive(clipboard, ticket, token, connect);
    tokio::pin!(receiving);
    loop {
        tokio::select! {
            biased;
            // One byte is cancellation-safe: if pending, it consumed nothing; if ready,
            // finish that exact frame before polling the platform operation again.
            first = stream.read_u8() => {
                let Ok(first) = first else { return None };
                let mut prefix = [0; 3];
                if stream.read_exact(&mut prefix).await.is_err() {
                    return None;
                }
                let size = u32::from_be_bytes([first, prefix[0], prefix[1], prefix[2]]) as usize;
                if size == 0 || size > CONTROL_LIMIT { return None; }
                let mut body = vec![0; size];
                if stream.read_exact(&mut body).await.is_err() {
                    return None;
                }
                let Ok(delivery) = serde_json::from_slice::<Delivery>(&body) else {
                    return None;
                };
                if matches!(&delivery, Delivery::Cancelled { ticket:cancelled } if *cancelled == ticket) {
                    // Returning drops receiving before the caller writes its reply; the
                    // Android lease is synchronously revoked even if JNI is still queued.
                    return Some(Outcome::error("cancelled", "The operation was cancelled"));
                }
                if waiting.len() >= 32 { return None; }
                waiting.push_back(delivery);
            }
            result = &mut receiving => return Some(match result {
                Ok(()) => Outcome::ok(json!(true)),
                Err(reason) => Outcome::error("failed", reason),
            }),
        }
    }
}

/// Answer one delivery from the application. Returns `false` when the connection is no longer
/// usable and the plugin should end.
///
/// `waiting` collects what arrives while a `share` waits for its own answer; the caller
/// answers those next, in order, before it reads anything new.
///
/// `connect` opens a fresh data connection each time it is called.
pub async fn handle<C, S, D, F, Fut>(
    clipboard: &mut C,
    stream: &mut S,
    token: &str,
    waiting: &mut VecDeque<Delivery>,
    delivery: Delivery,
    connect: F,
) -> bool
where
    C: Clipboard,
    S: AsyncRead + AsyncWrite + Unpin,
    D: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<D>>,
{
    let Delivery::Call {
        ticket,
        method,
        args,
        ..
    } = delivery
    else {
        return true;
    };
    let outcome = match method.as_str() {
        "share" => match peer(&args) {
            Some(device) => {
                let control = Control {
                    stream: &mut *stream,
                    token,
                    waiting,
                };
                let result = if let Some(context) = args.get("context") {
                    match context.as_u64().filter(|context| *context > 0) {
                        Some(context) => {
                            share(
                                &mut Selected { clipboard, context },
                                control,
                                device,
                                connect,
                            )
                            .await
                        }
                        None => Err(answer::NOTHING),
                    }
                } else {
                    share(clipboard, control, device, connect).await
                };
                match result {
                    Ok(()) => Outcome::ok(json!(true)),
                    Err(reason) => Outcome::error("failed", reason),
                }
            }
            None => Outcome::error("failed", answer::REFUSED),
        },
        "receive" => match receive_action(clipboard, stream, waiting, ticket, token, connect).await
        {
            Some(outcome) => outcome,
            None => return false,
        },
        _ => Outcome::error("unknown", answer::UNKNOWN),
    };
    frame::write(
        stream,
        &Operation::Reply {
            ticket,
            result: outcome,
        },
        CONTROL_LIMIT,
    )
    .await
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{DuplexStream, duplex};

    /// A clipboard that is a string; `readable`/`writable` make it refuse.
    struct Fake {
        held: Option<String>,
        writable: bool,
    }

    impl Fake {
        fn with(text: &str) -> Self {
            Self {
                held: Some(text.to_owned()),
                writable: true,
            }
        }
    }

    impl Clipboard for Fake {
        async fn text(&mut self) -> Option<String> {
            self.held.clone()
        }
        async fn set_text(&mut self, text: &str) -> bool {
            if self.writable {
                self.held = Some(text.to_owned());
            }
            self.writable
        }
    }

    /// The application's side of the control connection.
    async fn read_operation(app: &mut DuplexStream) -> Operation {
        frame::read(app, CONTROL_LIMIT).await.unwrap()
    }

    async fn deliver(app: &mut DuplexStream, delivery: &Delivery) {
        frame::write(app, delivery, CONTROL_LIMIT).await.unwrap();
    }

    fn call(ticket: u64, method: &str, args: Value) -> Delivery {
        Delivery::Call {
            ticket,
            caller: "host".into(),
            service: "clipboard".into(),
            version: 1,
            method: method.into(),
            args,
        }
    }

    fn sent_ok(claim: u64) -> Delivery {
        Delivery::Result {
            id: 1,
            result: Outcome::ok(json!({ "claim": claim })),
        }
    }

    /// A data connection whose far end is returned for the test to read or write.
    fn data_pair() -> (DuplexStream, DuplexStream) {
        duplex(4 * 1024 * 1024)
    }

    #[tokio::test]
    async fn share_asks_the_application_to_send_and_pushes_the_encoded_text_under_its_claim() {
        let (mut app, mut plugin) = duplex(64 * 1024);
        let (mut far, near) = data_pair();
        let mut waiting = VecDeque::new();
        let mut clipboard = Fake::with("héllo 🙂");
        let share = share(
            &mut clipboard,
            Control {
                stream: &mut plugin,
                token: "secret",
                waiting: &mut waiting,
            },
            "a-device",
            || async { Ok(near) },
        );
        let application = async {
            // What was asked of the application, exactly.
            let Operation::Call {
                id,
                provider,
                service,
                version,
                method,
                args,
                ..
            } = read_operation(&mut app).await
            else {
                panic!("not a call");
            };
            assert_eq!((id, provider.as_str()), (1, "host"));
            assert_eq!((service.as_str(), version), ("peersus.peers", 1));
            assert_eq!(method, "send");
            assert_eq!(
                args,
                json!({ "device": "a-device", "service": SERVICE, "version": VERSION })
            );
            deliver(&mut app, &sent_ok(77)).await;
            // Then the data connection: the handshake names the claim and carries the token.
            let hello: DataHello = frame::read(&mut far, HELLO_LIMIT).await.unwrap();
            assert_eq!(hello.token, "secret");
            assert_eq!(hello.claim, Some(77));
            assert_eq!(hello.ticket, 0);
            let mut body = Vec::new();
            far.read_to_end(&mut body).await.unwrap();
            frame::write(&mut far, &(body.len() as u64), HELLO_LIMIT)
                .await
                .unwrap();
            wire::decode(Cursor::new(body)).unwrap()
        };
        let (sent, item) = tokio::join!(share, application);
        assert_eq!(sent, Ok(()));
        assert_eq!(item, Item::Text("héllo 🙂".into()));
    }

    #[tokio::test]
    async fn local_write_without_host_delivery_confirmation_is_not_success() {
        for acknowledgement in [None, Some(0u64)] {
            let (mut app, mut plugin) = duplex(64 * 1024);
            let (mut far, near) = data_pair();
            let mut waiting = VecDeque::new();
            let mut clipboard = Fake::with("not yet delivered");
            let sending = share(
                &mut clipboard,
                Control {
                    stream: &mut plugin,
                    token: "secret",
                    waiting: &mut waiting,
                },
                "a-device",
                || async { Ok(near) },
            );
            let application = async {
                let _ = read_operation(&mut app).await;
                deliver(&mut app, &sent_ok(77)).await;
                let _: DataHello = frame::read(&mut far, HELLO_LIMIT).await.unwrap();
                let mut body = Vec::new();
                far.read_to_end(&mut body).await.unwrap();
                assert!(!body.is_empty());
                if let Some(bytes) = acknowledgement {
                    let _ = frame::write(&mut far, &bytes, HELLO_LIMIT).await;
                }
                far.shutdown().await.unwrap();
            };
            let (result, ()) = tokio::join!(sending, application);
            assert_eq!(result, Err(answer::FAILED));
        }
    }
    #[tokio::test]
    async fn share_with_nothing_on_the_clipboard_asks_the_application_for_nothing() {
        let (mut app, mut plugin) = duplex(64 * 1024);
        let mut waiting = VecDeque::new();
        let mut clipboard = Fake {
            held: None,
            writable: true,
        };
        let result = share(
            &mut clipboard,
            Control {
                stream: &mut plugin,
                token: "t",
                waiting: &mut waiting,
            },
            "d",
            || async { Ok(data_pair().0) },
        )
        .await;
        assert_eq!(result, Err(answer::NOTHING));
        drop(plugin);
        // Nothing at all was written to the application.
        let mut rest = Vec::new();
        app.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty(), "something was asked of the application");
    }

    #[tokio::test]
    async fn text_over_the_limit_is_never_asked_about() {
        let (mut app, mut plugin) = duplex(64 * 1024);
        let mut waiting = VecDeque::new();
        let mut clipboard = Fake::with(&"x".repeat(wire::MAX_TEXT as usize + 1));
        let result = share(
            &mut clipboard,
            Control {
                stream: &mut plugin,
                token: "t",
                waiting: &mut waiting,
            },
            "d",
            || async { Ok(data_pair().0) },
        )
        .await;
        assert_eq!(result, Err(answer::NOTHING));
        drop(plugin);
        let mut rest = Vec::new();
        app.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
    }

    #[tokio::test]
    async fn a_refusal_from_the_application_says_the_same_thing_whatever_its_reason() {
        for message in ["no such device", "not granted", "offline"] {
            let (mut app, mut plugin) = duplex(64 * 1024);
            let mut waiting = VecDeque::new();
            let mut clipboard = Fake::with("hi");
            let share = share(
                &mut clipboard,
                Control {
                    stream: &mut plugin,
                    token: "t",
                    waiting: &mut waiting,
                },
                "d",
                || async { Ok(data_pair().0) },
            );
            let application = async {
                read_operation(&mut app).await;
                deliver(
                    &mut app,
                    &Delivery::Result {
                        id: 1,
                        result: Outcome::error("refused", message),
                    },
                )
                .await;
            };
            let (result, ()) = tokio::join!(share, application);
            assert_eq!(result, Err(answer::REFUSED), "reason {message}");
        }
    }

    #[tokio::test]
    async fn a_call_that_arrives_while_share_waits_is_kept_not_lost() {
        // A device can send its own text just as the user shares: the `receive` arrives on
        // the same connection while `share` waits for its answer, and must not be dropped.
        let (mut app, mut plugin) = duplex(64 * 1024);
        let (mut far, near) = data_pair();
        let mut waiting = VecDeque::new();
        let mut clipboard = Fake::with("mine");
        let share = share(
            &mut clipboard,
            Control {
                stream: &mut plugin,
                token: "t",
                waiting: &mut waiting,
            },
            "d",
            || async { Ok(near) },
        );
        let application = async {
            read_operation(&mut app).await;
            // First somebody else's call, then the answer.
            deliver(&mut app, &call(9, "receive", Value::Null)).await;
            deliver(&mut app, &sent_ok(5)).await;
            let _: DataHello = frame::read(&mut far, HELLO_LIMIT).await.unwrap();
            let mut body = Vec::new();
            far.read_to_end(&mut body).await.unwrap();
            frame::write(&mut far, &(body.len() as u64), HELLO_LIMIT)
                .await
                .unwrap();
        };
        let (result, ()) = tokio::join!(share, application);
        assert_eq!(result, Ok(()));
        assert_eq!(
            waiting.len(),
            1,
            "the call that arrived while waiting was lost"
        );
        assert!(matches!(
            waiting.front(),
            Some(Delivery::Call { ticket: 9, .. })
        ));
    }

    #[tokio::test]
    async fn deliveries_set_aside_keep_the_order_they_came_in() {
        let (mut app, mut plugin) = duplex(64 * 1024);
        let (mut far, near) = data_pair();
        let mut waiting = VecDeque::new();
        let mut clipboard = Fake::with("mine");
        let share = share(
            &mut clipboard,
            Control {
                stream: &mut plugin,
                token: "t",
                waiting: &mut waiting,
            },
            "d",
            || async { Ok(near) },
        );
        let application = async {
            read_operation(&mut app).await;
            for ticket in [3u64, 1, 2] {
                deliver(&mut app, &call(ticket, "receive", Value::Null)).await;
            }
            deliver(&mut app, &sent_ok(5)).await;
            let _: DataHello = frame::read(&mut far, HELLO_LIMIT).await.unwrap();
            let mut body = Vec::new();
            far.read_to_end(&mut body).await.unwrap();
            frame::write(&mut far, &(body.len() as u64), HELLO_LIMIT)
                .await
                .unwrap();
        };
        let (result, ()) = tokio::join!(share, application);
        assert_eq!(result, Ok(()));
        let order: Vec<u64> = waiting
            .iter()
            .map(|d| match d {
                Delivery::Call { ticket, .. } => *ticket,
                _ => 0,
            })
            .collect();
        assert_eq!(order, [3, 1, 2]);
    }

    /// The far side of a data connection that offers `bytes`, as a peer's stream would.
    async fn offer(far: &mut DuplexStream, bytes: &[u8]) -> DataHello {
        let hello: DataHello = frame::read(far, HELLO_LIMIT).await.unwrap();
        far.write_all(bytes).await.unwrap();
        far.shutdown().await.unwrap();
        hello
    }

    #[tokio::test]
    async fn receive_places_text_a_device_sent_on_the_clipboard() {
        let (mut far, near) = data_pair();
        let mut clipboard = Fake::with("before");
        let item = wire::encode(&Item::Text("from a device".into())).unwrap();
        let receive = receive(&mut clipboard, 42, "secret", || async { Ok(near) });
        let (result, hello) = tokio::join!(receive, offer(&mut far, &item));
        assert_eq!(result, Ok(()));
        assert_eq!(clipboard.held.as_deref(), Some("from a device"));
        // It named the call it was answering, and gave no claim.
        assert_eq!(
            (hello.token.as_str(), hello.ticket, hello.claim),
            ("secret", 42, None)
        );
    }

    #[tokio::test]
    async fn a_peer_that_sends_text_but_never_closes_cannot_pin_the_plugin_forever() {
        let (mut far, near) = data_pair();
        let mut clipboard = Fake::with("untouched");
        let bytes = wire::encode(&Item::Text("not finished".into())).unwrap();
        let sender = async {
            let _: DataHello = frame::read(&mut far, HELLO_LIMIT).await.unwrap();
            far.write_all(&bytes).await.unwrap();
            // Keep the write side open until the receive deadline has actually fired.
            std::future::pending::<()>().await;
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(12), async {
            tokio::select! {
                result = receive(&mut clipboard, 1, "t", || async { Ok(near) }) => result,
                () = sender => unreachable!(),
            }
        })
        .await
        .expect("the plugin waited indefinitely for EOF");
        assert_eq!(result, Err(answer::NOT_ACCEPTED));
        assert_eq!(clipboard.held.as_deref(), Some("untouched"));
    }

    #[tokio::test]
    async fn receive_refuses_everything_that_is_not_exactly_one_text_and_leaves_the_clipboard() {
        let mut huge = wire::encode(&Item::Text("x".into())).unwrap();
        // A header that promises more text than is ever accepted.
        huge[5..9].copy_from_slice(&(wire::MAX_TEXT + 1).to_be_bytes());
        let picture = wire::encode(&Item::Png(vec![1, 2, 3])).unwrap();
        let mut truncated = wire::encode(&Item::Text("hello".into())).unwrap();
        truncated.truncate(truncated.len() - 2);
        let mut bad_text = wire::encode(&Item::Text("ab".into())).unwrap();
        let last = bad_text.len() - 1;
        bad_text[last] = 0xFF;
        let cases: [(&str, Vec<u8>); 6] = [
            ("empty", Vec::new()),
            ("not an item", b"hello world, not an item".to_vec()),
            ("a picture", picture),
            ("an oversized promise", huge),
            ("cut short", truncated),
            ("not valid text", bad_text),
        ];
        for (name, bytes) in cases {
            let (mut far, near) = data_pair();
            let mut clipboard = Fake::with("untouched");
            let receive = receive(&mut clipboard, 1, "t", || async { Ok(near) });
            let (result, _) = tokio::join!(receive, offer(&mut far, &bytes));
            assert_eq!(result, Err(answer::NOT_ACCEPTED), "{name}");
            assert_eq!(clipboard.held.as_deref(), Some("untouched"), "{name}");
        }
    }

    #[tokio::test]
    async fn receive_reads_a_bounded_amount_from_a_sender_that_never_stops() {
        // A device that sends without end must not make the plugin read without end. The pipe
        // is far smaller than the stream offered, so reading everything would hang the test
        // until its ceiling; reading only what is allowed returns.
        let (mut far, near) = duplex(64 * 1024);
        let mut clipboard = Fake::with("untouched");
        let mut item = wire::encode(&Item::Text("x".into())).unwrap();
        // A well-formed header for a full-size text, then more bytes than are allowed.
        item[5..9].copy_from_slice(&wire::MAX_TEXT.to_be_bytes());
        let receive = receive(&mut clipboard, 1, "t", || async { Ok(near) });
        let sender = async {
            let _: DataHello = frame::read(&mut far, HELLO_LIMIT).await.unwrap();
            let _ = far.write_all(&item[..wire::HEADER]).await;
            let chunk = vec![b'x'; 64 * 1024];
            // Far more than the limit; stops when the plugin stops reading.
            for _ in 0..(wire::MAX_TEXT as usize * 4 / chunk.len()) {
                if far.write_all(&chunk).await.is_err() {
                    break;
                }
            }
        };
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            tokio::join!(receive, sender).0
        })
        .await
        .expect("the plugin kept reading without end");
        // What it did read was a valid text of exactly the promised size, so it is placed;
        // what matters is that it stopped, not what it made of the bytes.
        assert!(outcome == Ok(()) || outcome == Err(answer::NOT_ACCEPTED));
    }

    #[tokio::test]
    async fn the_largest_text_is_taken_whole_and_one_byte_more_is_not() {
        // Both sides of the limit, so a bound one byte too tight or too loose is seen: the
        // text of exactly the maximum must arrive, and a header promising one more must not.
        let largest = "x".repeat(wire::MAX_TEXT as usize);
        let (mut far, near) = duplex(4 * 1024 * 1024);
        let mut clipboard = Fake::with("before");
        let item = wire::encode(&Item::Text(largest.clone())).unwrap();
        let first = receive(&mut clipboard, 1, "t", || async { Ok(near) });
        let (result, _) = tokio::join!(first, offer(&mut far, &item));
        assert_eq!(result, Ok(()), "the largest allowed text was refused");
        assert_eq!(clipboard.held.as_deref(), Some(largest.as_str()));

        let (mut far, near) = duplex(4 * 1024 * 1024);
        let mut clipboard = Fake::with("before");
        let mut over = wire::encode(&Item::Text(largest)).unwrap();
        over[5..9].copy_from_slice(&(wire::MAX_TEXT + 1).to_be_bytes());
        over.push(b'x');
        let second = receive(&mut clipboard, 1, "t", || async { Ok(near) });
        let (result, _) = tokio::join!(second, offer(&mut far, &over));
        assert_eq!(result, Err(answer::NOT_ACCEPTED), "one byte over was taken");
        assert_eq!(clipboard.held.as_deref(), Some("before"));
    }

    #[tokio::test]
    async fn a_valid_text_followed_by_anything_more_is_not_taken() {
        // Each of these is a perfectly good text with something after it, so the header and
        // the body are fine and only "exactly one item" can refuse them.
        let good = wire::encode(&Item::Text("hello".into())).unwrap();
        for extra in [&b"x"[..], b"\0", b"a whole second item".as_slice()] {
            let mut bytes = good.clone();
            bytes.extend_from_slice(extra);
            let (mut far, near) = data_pair();
            let mut clipboard = Fake::with("untouched");
            let attempt = receive(&mut clipboard, 1, "t", || async { Ok(near) });
            let (result, _) = tokio::join!(attempt, offer(&mut far, &bytes));
            assert_eq!(result, Err(answer::NOT_ACCEPTED), "extra {extra:?}");
            assert_eq!(
                clipboard.held.as_deref(),
                Some("untouched"),
                "extra {extra:?}"
            );
        }
        // And the same text with nothing after it is taken, so it is the extra that refused.
        let (mut far, near) = data_pair();
        let mut clipboard = Fake::with("untouched");
        let attempt = receive(&mut clipboard, 1, "t", || async { Ok(near) });
        let (result, _) = tokio::join!(attempt, offer(&mut far, &good));
        assert_eq!(result, Ok(()));
        assert_eq!(clipboard.held.as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn a_clipboard_that_will_not_take_the_text_says_so() {
        let (mut far, near) = data_pair();
        let mut clipboard = Fake {
            held: Some("before".into()),
            writable: false,
        };
        let item = wire::encode(&Item::Text("new".into())).unwrap();
        let receive = receive(&mut clipboard, 1, "t", || async { Ok(near) });
        let (result, _) = tokio::join!(receive, offer(&mut far, &item));
        assert_eq!(result, Err(answer::NOT_PLACED));
    }

    #[tokio::test]
    async fn handle_answers_each_call_with_its_own_ticket_and_only_fixed_words() {
        let (mut app, mut plugin) = duplex(64 * 1024);
        let mut waiting = VecDeque::new();
        let mut clipboard = Fake::with("hi");
        // An unknown method and a share with no device named.
        for (ticket, method, args, expect) in [
            (4u64, "frobnicate", Value::Null, answer::UNKNOWN),
            (5, "share", json!({}), answer::REFUSED),
        ] {
            let kept = handle(
                &mut clipboard,
                &mut plugin,
                "t",
                &mut waiting,
                call(ticket, method, args),
                || async { Ok(data_pair().0) },
            )
            .await;
            assert!(kept);
            let Operation::Reply {
                ticket: got,
                result,
            } = read_operation(&mut app).await
            else {
                panic!("not a reply");
            };
            assert_eq!(got, ticket);
            let Outcome::Error { message, .. } = result else {
                panic!("not an error");
            };
            assert_eq!(message, expect);
        }
    }

    #[test]
    fn the_shipped_manifest_says_what_this_code_does() {
        // The service, its version and the two methods are written twice: here, and in the
        // manifest the application reads. If they drift apart nothing fails to compile, and
        // the plugin would send to a service nobody routes to. This is what ties them.
        let split = peersus_api::package::split(include_bytes!("../plugin.json"))
            .expect("plugin.json has invalid package metadata");
        let manifest: peersus_api::manifest::Manifest =
            serde_json::from_slice(&split.manifest).expect("plugin.json is not a manifest");
        manifest
            .validate()
            .expect("the application would refuse this manifest");
        let export = manifest
            .exports
            .iter()
            .find(|export| export.service == SERVICE)
            .expect("the manifest does not export the service this code sends to");
        assert_eq!(export.version, VERSION);
        for method in ["share", "receive"] {
            // Present AND guarded. `receive` is what lets another device write to this
            // machine's clipboard: with no permission on it, the user's decision about a
            // device would not apply to it at all.
            assert_eq!(
                export
                    .methods
                    .get(method)
                    .map(|member| member.capability.as_deref()),
                Some(Some("clipboard.use")),
                "{method} is not exported behind the permission the user grants"
            );
        }
        assert!(manifest.declares("clipboard.use"));
        // Nothing is exported that this code does not answer, because the application would
        // route a call to it and get "nothing here answers that".
        assert_eq!(export.methods.len(), 2);
        // Device listing/sending and caller-only configuration are the only imported services.
        let import = manifest
            .imports
            .iter()
            .find(|import| import.provider == "host" && import.service == "peersus.peers")
            .expect("the manifest does not import the service this code calls");
        assert_eq!(import.version, 1);
        assert!(import.methods.contains("send"));
        assert_eq!(
            import.methods,
            ["list".to_owned(), "send".to_owned()].into()
        );
        let config = manifest
            .imports
            .iter()
            .find(|import| import.provider == "host" && import.service == "peersus.config")
            .unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.methods, ["get".to_owned()].into());
        // `peers.send` is the APPLICATION's permission, not this plugin's: it guards the
        // application's `send`, so the plugin imports `send` and does not declare a
        // permission of that name. If it did, it would be offering its own meaning for a name
        // the application already uses, which is exactly the confusion owners exist to
        // remove; and the words a person reads for it are the application's, not a plugin's.
        assert!(
            !manifest.declares("peers.send"),
            "the plugin declares a permission named like the application's"
        );
        // What a person is shown when deciding is real text for every permission and every
        // part of what this plugin offers. A description is disclosure, not prevention, but a
        // permission nobody can read is one nobody can decide about.
        for (capability, description) in &manifest.capabilities {
            assert!(
                description.split_whitespace().count() >= 5,
                "the description of {capability} is too thin to decide on: {description:?}"
            );
        }
        assert!(export.description.split_whitespace().count() >= 3);
        for (name, member) in &export.methods {
            assert!(
                member.description.split_whitespace().count() >= 3,
                "{name} does not say what it does"
            );
        }
    }

    async fn write_delivery(stream: &mut tokio::net::UnixStream, delivery: &Delivery) {
        frame::write(stream, delivery, CONTROL_LIMIT).await.unwrap();
    }

    #[tokio::test]
    async fn what_has_already_arrived_is_brought_in_without_waiting_for_more() {
        let (mut app, mut plugin) = tokio::net::UnixStream::pair().unwrap();
        let mut waiting = VecDeque::new();
        write_delivery(&mut app, &call(2, "share", json!({ "peer": "d" }))).await;
        write_delivery(&mut app, &cancelled(2)).await;
        write_delivery(&mut app, &call(3, "receive", Value::Null)).await;
        // The application is still connected and has nothing more to say.
        assert!(drain(&mut plugin, &mut waiting).await);
        let order: Vec<String> = waiting
            .iter()
            .map(|d| match d {
                Delivery::Call { ticket, .. } => format!("call {ticket}"),
                Delivery::Cancelled { ticket } => format!("cancelled {ticket}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(order, ["call 2", "cancelled 2", "call 3"]);
    }

    #[tokio::test]
    async fn with_nothing_to_bring_in_it_returns_at_once_instead_of_waiting() {
        // The plugin has work to do or is about to wait for the next delivery itself; if this
        // waited for something to arrive it would hold up everything behind it.
        let (_app, mut plugin) = tokio::net::UnixStream::pair().unwrap();
        let mut waiting = VecDeque::new();
        let drained = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            drain(&mut plugin, &mut waiting),
        )
        .await
        .expect("drain waited for something that was not there");
        assert!(drained);
        assert!(waiting.is_empty());
    }

    #[tokio::test]
    async fn a_delivery_that_arrives_in_two_pieces_is_read_whole_not_lost() {
        // Reading must never be abandoned halfway through a delivery: that would drop the
        // bytes read so far and leave the stream in the middle of one.
        use tokio::io::AsyncWriteExt;
        let (mut app, mut plugin) = tokio::net::UnixStream::pair().unwrap();
        let mut waiting = VecDeque::new();
        let whole = {
            let body = serde_json::to_vec(&call(8, "receive", Value::Null)).unwrap();
            let mut out = (body.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(&body);
            out
        };
        let (first, second) = whole.split_at(6);
        app.write_all(first).await.unwrap();
        app.flush().await.unwrap();
        let finish = async {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            app.write_all(second).await.unwrap();
            app.flush().await.unwrap();
        };
        let (kept, ()) = tokio::join!(drain(&mut plugin, &mut waiting), finish);
        assert!(kept);
        assert_eq!(
            tickets(&waiting),
            [8],
            "half a delivery was lost or mangled"
        );
        // And what follows still reads correctly, so the stream was not left misaligned.
        write_delivery(&mut app, &call(9, "receive", Value::Null)).await;
        assert!(drain(&mut plugin, &mut waiting).await);
        assert_eq!(tickets(&waiting), [8, 9]);
    }

    #[tokio::test]
    async fn a_length_over_the_limit_is_refused_even_if_its_low_bytes_would_pass() {
        // The first byte of the length is what makes this 16 MiB and more. A reader that
        // ignored it would see only the low three bytes, a perfectly small length, and take
        // the well-formed delivery that follows as valid.
        use tokio::io::AsyncWriteExt;
        let (mut app, mut plugin) = tokio::net::UnixStream::pair().unwrap();
        let mut waiting = VecDeque::new();
        let body = serde_json::to_vec(&call(6, "receive", Value::Null)).unwrap();
        let low = (body.len() as u32).to_be_bytes();
        assert_eq!(
            low[0], 0,
            "the fixture must be a small length to begin with"
        );
        let hostile = [0x01, low[1], low[2], low[3]];
        app.write_all(&hostile).await.unwrap();
        app.write_all(&body).await.unwrap();
        app.flush().await.unwrap();
        assert!(
            !drain(&mut plugin, &mut waiting).await,
            "a delivery announcing 16 MiB was accepted"
        );
        assert!(
            waiting.is_empty(),
            "something was taken from a refused frame"
        );
    }

    #[tokio::test]
    async fn it_says_so_when_the_application_has_gone() {
        let (app, mut plugin) = tokio::net::UnixStream::pair().unwrap();
        let mut waiting = VecDeque::new();
        drop(app);
        assert!(!drain(&mut plugin, &mut waiting).await);
    }

    fn cancelled(ticket: u64) -> Delivery {
        Delivery::Cancelled { ticket }
    }

    fn tickets(queue: &VecDeque<Delivery>) -> Vec<u64> {
        queue
            .iter()
            .filter_map(|d| match d {
                Delivery::Call { ticket, .. } => Some(*ticket),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_call_the_application_gave_up_on_is_taken_off_the_queue_before_it_can_run() {
        // A share waits behind a slow receive and its deadline passes; the application says
        // so with `Cancelled`. Running it anyway would read the clipboard and ask for a send
        // nobody is waiting for any more.
        let mut queue: VecDeque<Delivery> = [
            call(2, "share", json!({ "peer": "a-device" })),
            call(3, "share", json!({ "peer": "a-device" })),
            cancelled(2),
        ]
        .into();
        withdraw_cancelled(&mut queue);
        assert_eq!(tickets(&queue), [3], "a cancelled call was left to run");
        // The notice itself is spent, not left behind to be mistaken for work.
        assert!(
            queue.iter().all(|d| matches!(d, Delivery::Call { .. })),
            "{queue:?}"
        );
    }

    #[test]
    fn calls_nobody_cancelled_keep_their_order() {
        let mut queue: VecDeque<Delivery> = [
            call(5, "receive", Value::Null),
            call(1, "share", json!({ "peer": "d" })),
            call(9, "receive", Value::Null),
        ]
        .into();
        withdraw_cancelled(&mut queue);
        assert_eq!(tickets(&queue), [5, 1, 9]);
    }

    #[test]
    fn a_cancellation_for_something_not_queued_changes_nothing() {
        // It may be for a call already answered or one that is running; either way the
        // queue is not the place it applies to, and it must not remove a different call.
        let mut queue: VecDeque<Delivery> =
            [call(4, "share", json!({ "peer": "d" })), cancelled(77)].into();
        withdraw_cancelled(&mut queue);
        assert_eq!(tickets(&queue), [4]);
    }

    #[tokio::test]
    async fn handle_ends_when_the_application_has_gone() {
        let (app, mut plugin) = duplex(64);
        drop(app);
        let mut waiting = VecDeque::new();
        let mut clipboard = Fake::with("hi");
        let kept = handle(
            &mut clipboard,
            &mut plugin,
            "t",
            &mut waiting,
            call(1, "frobnicate", Value::Null),
            || async { Ok(data_pair().0) },
        )
        .await;
        assert!(!kept);
    }

    #[tokio::test(start_paused = true)]
    async fn claimed_fresh_text_can_wait_for_delivery_beyond_its_capture_deadline() {
        let (mut app, mut stream) = duplex(64 * 1024);
        let (near, mut far) = duplex(64 * 1024);
        let mut clipboard = Fake::with("fresh text");
        let mut waiting = VecDeque::new();
        let host = async {
            read_operation(&mut app).await;
            deliver(&mut app, &sent_ok(12)).await;
            let _: DataHello = frame::read(&mut far, HELLO_LIMIT).await.unwrap();
            let mut body = Vec::new();
            far.read_to_end(&mut body).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            frame::write(&mut far, &(body.len() as u64), HELLO_LIMIT)
                .await
                .unwrap();
        };
        let send = share_until(
            &mut clipboard,
            Control {
                stream: &mut stream,
                token: "test",
                waiting: &mut waiting,
            },
            "device",
            || async { Ok(near) },
            Some(std::time::Instant::now() + std::time::Duration::from_secs(2)),
        );
        let (result, ()) = tokio::join!(send, host);
        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn expired_automatic_text_is_not_connected_or_written_after_the_claim() {
        let (mut app, mut stream) = duplex(64 * 1024);
        let mut clipboard = Fake::with("old capture");
        let mut waiting = VecDeque::new();
        let host = async {
            assert!(matches!(
                read_operation(&mut app).await,
                Operation::Call { .. }
            ));
            deliver(&mut app, &sent_ok(12)).await;
        };
        let send = share_until(
            &mut clipboard,
            Control {
                stream: &mut stream,
                token: "test",
                waiting: &mut waiting,
            },
            "device",
            || async {
                panic!("expired data must never connect");
                #[allow(unreachable_code)]
                Ok::<_, std::io::Error>(data_pair().0)
            },
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        );
        let (result, ()) = tokio::join!(send, host);
        assert_eq!(result, Err(answer::NOTHING));
    }

    #[tokio::test]
    async fn blocked_bulk_write_times_out_without_corrupting_the_control_connection() {
        let (mut app, mut stream) = duplex(64 * 1024);
        let (near, _blocked) = duplex(1);
        let mut clipboard = Fake::with("text");
        let mut waiting = VecDeque::new();
        let host = async {
            read_operation(&mut app).await;
            deliver(&mut app, &sent_ok(12)).await;
            deliver(&mut app, &call(99, "unknown", json!({}))).await;
        };
        let send = share_until(
            &mut clipboard,
            Control {
                stream: &mut stream,
                token: "test",
                waiting: &mut waiting,
            },
            "device",
            || async { Ok(near) },
            Some(std::time::Instant::now() + std::time::Duration::from_secs(2)),
        );
        let (result, ()) = tokio::join!(send, host);
        assert_eq!(result, Err(answer::FAILED));
        let delivery = frame::read(&mut stream, CONTROL_LIMIT).await.unwrap();
        assert!(
            handle(
                &mut clipboard,
                &mut stream,
                "test",
                &mut waiting,
                delivery,
                || async { Ok(data_pair().0) }
            )
            .await
        );
        assert!(matches!(
            read_operation(&mut app).await,
            Operation::Reply { ticket: 99, .. }
        ));
    }

    #[tokio::test]
    async fn explicit_share_can_finish_a_slow_data_write_beyond_automatic_freshness_budget() {
        let (mut app, mut stream) = duplex(64 * 1024);
        let (near, mut far) = duplex(1);
        let mut clipboard = Fake::with("explicit text");
        let mut waiting = VecDeque::new();
        let host = async {
            read_operation(&mut app).await;
            deliver(&mut app, &sent_ok(12)).await;
            tokio::time::sleep(std::time::Duration::from_millis(2_100)).await;
            let _: DataHello = frame::read(&mut far, HELLO_LIMIT).await.unwrap();
            let mut data = Vec::new();
            far.read_to_end(&mut data).await.unwrap();
            frame::write(&mut far, &(data.len() as u64), HELLO_LIMIT)
                .await
                .unwrap();
            assert_eq!(
                wire::decode(&mut Cursor::new(data)).unwrap(),
                Item::Text("explicit text".into())
            );
        };
        let send = share(
            &mut clipboard,
            Control {
                stream: &mut stream,
                token: "test",
                waiting: &mut waiting,
            },
            "device",
            || async { Ok(near) },
        );
        let (result, ()) = tokio::join!(send, host);
        assert_eq!(result, Ok(()));
    }

    struct SelectedFake {
        context: u64,
        selected: Option<String>,
        live: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl Clipboard for SelectedFake {
        async fn text(&mut self) -> Option<String> {
            panic!("selected actions must not read the ordinary clipboard")
        }
        async fn selected_text(&mut self, context: u64) -> Option<String> {
            if context != self.context {
                return None;
            }
            self.selected.take()
        }
        async fn selected_valid(&mut self, context: u64) -> bool {
            context == self.context && self.live.load(std::sync::atomic::Ordering::SeqCst)
        }
        async fn set_text(&mut self, _: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn absent_expired_or_malformed_selection_never_falls_back_to_ordinary_clipboard() {
        for context in [json!(0), json!(99), json!("1"), Value::Null, json!(1)] {
            let (mut app, mut stream) = duplex(64 * 1024);
            let mut waiting = VecDeque::new();
            let mut clipboard = SelectedFake {
                context: 1,
                selected: None,
                live: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            };
            assert!(
                handle(
                    &mut clipboard,
                    &mut stream,
                    "test",
                    &mut waiting,
                    call(
                        31,
                        "share",
                        json!({"peer":"destination", "context":context})
                    ),
                    || async {
                        panic!("rejected selection must not connect");
                        #[allow(unreachable_code)]
                        Ok::<_, std::io::Error>(data_pair().0)
                    },
                )
                .await
            );
            assert!(matches!(
                read_operation(&mut app).await,
                Operation::Reply {
                    ticket: 31,
                    result: Outcome::Error { .. }
                }
            ));
        }
    }

    #[tokio::test]
    async fn selected_action_sends_only_its_exact_one_shot_payload() {
        let (mut app, mut stream) = duplex(64 * 1024);
        let (near, mut far) = data_pair();
        let mut waiting = VecDeque::new();
        let mut clipboard = SelectedFake {
            context: 17,
            selected: Some("exact selected text".into()),
            live: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        let host = async {
            assert!(matches!(
                read_operation(&mut app).await,
                Operation::Call { .. }
            ));
            deliver(&mut app, &sent_ok(5)).await;
            let _: DataHello = frame::read(&mut far, HELLO_LIMIT).await.unwrap();
            let mut body = Vec::new();
            far.read_to_end(&mut body).await.unwrap();
            frame::write(&mut far, &(body.len() as u64), HELLO_LIMIT)
                .await
                .unwrap();
            assert_eq!(
                wire::decode(Cursor::new(body)).unwrap(),
                Item::Text("exact selected text".into())
            );
            assert!(matches!(
                read_operation(&mut app).await,
                Operation::Reply {
                    ticket: 32,
                    result: Outcome::Ok { .. }
                }
            ));
        };
        let handled = handle(
            &mut clipboard,
            &mut stream,
            "test",
            &mut waiting,
            call(32, "share", json!({"peer":"destination", "context":17})),
            || async { Ok(near) },
        );
        let (handled, ()) = tokio::join!(handled, host);
        assert!(handled);
        assert!(clipboard.selected.is_none());
    }

    #[tokio::test]
    async fn pause_or_expiry_after_capture_before_claim_writes_no_payload() {
        let (mut app, mut stream) = duplex(64 * 1024);
        let (near, mut far) = data_pair();
        let mut waiting = VecDeque::new();
        let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mut clipboard = SelectedFake {
            context: 17,
            selected: Some("do not send".into()),
            live: live.clone(),
        };
        let host = async {
            read_operation(&mut app).await;
            live.store(false, std::sync::atomic::Ordering::SeqCst);
            deliver(&mut app, &sent_ok(5)).await;
            let _: DataHello = frame::read(&mut far, HELLO_LIMIT).await.unwrap();
            let mut body = Vec::new();
            far.read_to_end(&mut body).await.unwrap();
            assert!(body.is_empty(), "revoked selected text escaped");
            assert!(matches!(
                read_operation(&mut app).await,
                Operation::Reply {
                    ticket: 33,
                    result: Outcome::Error { .. }
                }
            ));
        };
        let handled = handle(
            &mut clipboard,
            &mut stream,
            "test",
            &mut waiting,
            call(33, "share", json!({"peer":"destination", "context":17})),
            || async { Ok(near) },
        );
        let (handled, ()) = tokio::join!(handled, host);
        assert!(handled);
    }

    struct QueuedWrite {
        ready: std::sync::Arc<tokio::sync::Notify>,
        active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    struct WritePermit(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for WritePermit {
        fn drop(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl Clipboard for QueuedWrite {
        async fn text(&mut self) -> Option<String> {
            None
        }
        async fn set_text(&mut self, _: &str) -> bool {
            panic!("receive must carry its original deadline")
        }
        async fn set_text_until(&mut self, _: &str, deadline: std::time::Instant) -> bool {
            assert!(deadline > std::time::Instant::now());
            let _permit = WritePermit(self.active.clone());
            self.active.store(true, std::sync::atomic::Ordering::SeqCst);
            self.ready.notify_one();
            std::future::pending::<bool>().await
        }
    }

    #[tokio::test]
    async fn host_revocation_drops_a_running_receive_and_revokes_its_queued_write_permit() {
        let (mut app, mut stream) = duplex(64 * 1024);
        let (near, mut far) = data_pair();
        let mut waiting = VecDeque::new();
        let ready = std::sync::Arc::new(tokio::sync::Notify::new());
        let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut clipboard = QueuedWrite {
            ready: ready.clone(),
            active: active.clone(),
        };
        let host = async {
            let _: DataHello = frame::read(&mut far, HELLO_LIMIT).await.unwrap();
            far.write_all(&wire::encode(&Item::Text("queued incoming".into())).unwrap())
                .await
                .unwrap();
            far.shutdown().await.unwrap();
            ready.notified().await;
            assert!(active.load(std::sync::atomic::Ordering::SeqCst));
            deliver(&mut app, &Delivery::Cancelled { ticket: 44 }).await;
            assert!(matches!(read_operation(&mut app).await,
                Operation::Reply { ticket:44, result:Outcome::Error { code, .. } } if code == "cancelled"));
            assert!(
                !active.load(std::sync::atomic::Ordering::SeqCst),
                "queued write permit survived revocation"
            );
        };
        let handled = handle(
            &mut clipboard,
            &mut stream,
            "test",
            &mut waiting,
            call(44, "receive", json!({})),
            || async { Ok(near) },
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let (handled, ()) = tokio::join!(handled, host);
            assert!(handled);
        })
        .await
        .expect("revocation was not processed until the outer receive deadline");
    }
}
