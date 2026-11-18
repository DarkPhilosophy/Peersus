//! One authenticated connection stream per explicitly accepted file.
use crate::{
    announce::{self, Announcements},
    streams,
    transfer::{self, Offer, Source},
    transport,
};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};
use tokio::sync::oneshot;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Reply {
    Accepted,
    Refused,
    Complete,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("File transfer was refused or cancelled")]
    Refused,
    #[error("File transfer connection failed")]
    Connection,
    #[error(transparent)]
    Transport(#[from] transport::Error),
    #[error(transparent)]
    Transfer(#[from] transfer::Error),
}

/// Success means the receiver confirmed verified, atomically published content.
/// `wait` is how long the other device may take to answer the offer: the same value, from the preferences, that the
/// receiving side waits for its person with, so that no time is chosen in here.
///
/// `announcements`, when there is one for this connection, is how the other device's own deadline is heard: a device that tells
/// how long it waits for its person (see [`announce`]) is waited for exactly that long, and `told` is called with it, so that what
/// is counted down is what is enforced. A device that tells nothing, as every older one, is waited for `wait`, which is what this
/// always was; nothing here depends on the announcement arriving.
pub async fn send(
    connection: &Connection,
    source: Source,
    wait: Duration,
    announcements: Option<&Announcements>,
    mut told: impl FnMut(Duration),
    progress: impl FnMut(u64),
) -> Result<(), Error> {
    let (mut tx, mut rx) = connection.open_bi().await.map_err(|_| Error::Connection)?;
    streams::send(
        &mut tx,
        &streams::Request::File {
            offer: source.offer.clone(),
        },
    )
    .await?;
    let offered = tokio::time::Instant::now();
    let mut announced = announcements.map(|announcements| announcements.ask(&source.offer.id));
    // Whatever was kept for this offer is let go of however the wait ends: answered, refused, timed out, or an error on the way out.
    let _let_go =
        announcements.map(|announcements| announce::LetGo::new(announcements, &source.offer.id));
    let mut deadline = offered + wait;
    // The answer is read by ONE future for the whole wait: a frame that is half read when an announcement is heard must not be
    // started again from its first byte, and a read that is dropped mid-frame cannot be resumed. It lives in a block of its own, so
    // that the stream it borrows is free again for what is read after the answer.
    let reply = {
        let answer = transport::receive_with_timeout::<Reply>(
            &mut rx,
            Duration::from_secs(u64::from(peersus_core::config::MAX_OFFER_TIMEOUT)),
        );
        tokio::pin!(answer);
        loop {
            tokio::select! {
                // The answer, or the end of the wait: whichever the deadline in force allows.
                // An answer that never came by the end of the wait is a refusal: nobody accepted. So is the other device ending the
                // stream with no word while the connection stands: that is what a device does whose person presses Cancel on the
                // offer (every build so far), and it was recorded here as a failure of the network. Only a break in the connection
                // is a failure.
                reply = &mut answer => break reply.map_err(|error| match error {
                    transport::Error::Timeout | transport::Error::Closed => Error::Refused,
                    other => Error::Transport(other),
                })?,
                () = tokio::time::sleep_until(deadline) => return Err(Error::Refused),
                // The other device told its own deadline: from then on it is the one waited for, counted from the offer.
                Some(Ok(theirs)) = async { match announced.as_mut() { Some(rx) => Some(rx.await), None => std::future::pending().await } } => {
                    announced = None;
                    deadline = offered + theirs;
                    told(theirs);
                }
            }
        }
    };
    match reply {
        Reply::Accepted => {}
        _ => return Err(Error::Refused),
    }
    source.send(&mut tx, progress).await?;
    match transport::receive(&mut rx).await {
        Ok(Reply::Complete) => Ok(()),
        // It accepted, took the bytes, and then says no, in a word or by ending the stream: it could not keep the file (one of
        // that name in the way, a digest that did not match). Not a refusal of the offer, which was accepted.
        Ok(_) | Err(transport::Error::Closed) => Err(Error::Transfer(transfer::Error::Stopped)),
        Err(error) => Err(error.into()),
    }
}

/// Called only for an authenticated, admitted peer. Dropped consent refuses.
pub async fn receive(
    tx: SendStream,
    mut rx: RecvStream,
    prompt: oneshot::Sender<Offer>,
    decision: oneshot::Receiver<PathBuf>,
    wait: Duration,
    progress: impl FnMut(u64),
) -> Result<PathBuf, Error> {
    let streams::Request::File { offer } = streams::receive(&mut rx).await? else {
        return Err(Error::Refused);
    };
    receive_offered(
        Incoming { tx, rx, offer },
        prompt,
        decision,
        wait,
        None,
        progress,
    )
    .await
}

/// An offer that came in: the stream it came on (both halves) and what it offers. One value, since the three are only ever
/// handled together.
pub struct Incoming {
    pub tx: SendStream,
    pub rx: RecvStream,
    pub offer: Offer,
}

/// The host already consumed and routed the typed stream preamble. `announce`, when the host has the connection the offer came on,
/// is where this device tells the other how long it will wait for its person (see [`announce`]), beside the offer and never in it.
pub(crate) async fn receive_offered(
    Incoming {
        mut tx,
        mut rx,
        offer,
    }: Incoming,
    prompt: oneshot::Sender<Offer>,
    decision: oneshot::Receiver<PathBuf>,
    wait: Duration,
    announce: Option<Connection>,
    progress: impl FnMut(u64),
) -> Result<PathBuf, Error> {
    offer.validate()?;
    prompt.send(offer.clone()).map_err(|_| Error::Refused)?;
    if let Some(connection) = announce {
        let (id, seconds) = (
            offer.id.clone(),
            u32::try_from(wait.as_secs()).unwrap_or(u32::MAX),
        );
        // Told on its own task and in its own time: the other device may not read it, and the offer does not wait to find out.
        tokio::spawn(async move { announce::tell(&connection, &id, seconds).await });
    }
    let answered = tokio::select! {
        biased;
        answered = tokio::time::timeout(wait, decision) => answered.ok().and_then(Result::ok),
        // The sender stopped listening for the answer before anybody gave one: it took the offer back, and there is nobody to
        // answer to. (A stream that is let go of is ended, not reset, on its writing half; the reading half is what says so, by
        // asking this device to stop sending.) Without this the offer stayed on the screen until its time ran out. A connection
        // that broke ends it the same way, as what it is.
        gone = tx.stopped() => return Err(match gone {
            Ok(_) => Error::Refused,
            Err(_) => Error::Connection,
        }),
    };
    let Some(directory) = answered else {
        let _ = transport::send(&mut tx, &Reply::Refused).await;
        let _ = tx.finish();
        return Err(Error::Refused);
    };
    transport::send(&mut tx, &Reply::Accepted).await?;
    let path = match transfer::receive(&mut rx, &offer, &directory, progress).await {
        Ok(path) => path,
        Err(error) => {
            // Said to the sender, which may have written everything and be waiting for the word that it arrived: without this
            // it read the end of the stream as a broken connection. Best effort: the failure here is what is returned.
            let _ = transport::send(&mut tx, &Reply::Refused).await;
            let _ = tx.finish();
            return Err(error.into());
        }
    };
    transport::send(&mut tx, &Reply::Complete).await?;
    tx.finish().map_err(|_| Error::Connection)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;
    use peersus_core::config::NetworkSettings;

    async fn exercise(accept: bool) {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("example.bin");
        let destination = directory.path().join("received");
        std::fs::create_dir(&destination).unwrap();
        let bytes = vec![42; 150_000];
        std::fs::write(&source_path, &bytes).unwrap();
        let source = Source::open(&source_path).await.unwrap();
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        };
        let a = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let b = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let (left, right) = tokio::join!(a.connect(b.addr(), transport::ALPN), async {
            b.accept().await.unwrap().await
        });
        let left = left.unwrap();
        let right = right.unwrap();
        let (prompt, shown) = oneshot::channel();
        let (decision, selected) = oneshot::channel();
        let (sent, received, ()) = tokio::join!(
            send(&left, source, Duration::from_secs(30), None, |_| {}, |_| {}),
            async {
                let (tx, rx) = right.accept_bi().await.unwrap();
                receive(tx, rx, prompt, selected, Duration::from_secs(30), |_| {}).await
            },
            async {
                let offer = shown.await.unwrap();
                assert_eq!(offer.bytes, bytes.len() as u64);
                assert!(!destination.join("example.bin").exists());
                if accept {
                    decision.send(destination.clone()).unwrap();
                } else {
                    drop(decision);
                }
            }
        );
        if accept {
            assert!(sent.is_ok());
            assert_eq!(std::fs::read(received.unwrap()).unwrap(), bytes);
        } else {
            assert!(matches!(sent, Err(Error::Refused)));
            assert!(matches!(received, Err(Error::Refused)));
            assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        }
        left.close(0_u32.into(), b"done");
        right.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn accepted_file_completes_only_after_verified_publication() {
        exercise(true).await;
    }

    #[tokio::test]
    async fn refused_file_never_creates_destination_content() {
        exercise(false).await;
    }

    /// Two endpoints on this machine and the connection between them, seen from each end.
    async fn connected() -> (iroh::Endpoint, iroh::Endpoint, Connection, Connection) {
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        };
        let a = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let b = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let (left, right) = tokio::join!(a.connect(b.addr(), transport::ALPN), async {
            b.accept().await.unwrap().await
        });
        (a, b, left.unwrap(), right.unwrap())
    }

    async fn file_of(directory: &std::path::Path, bytes: usize) -> Source {
        let path = directory.join("example.bin");
        std::fs::write(&path, vec![42; bytes]).unwrap();
        Source::open(&path).await.unwrap()
    }

    /// More than the connection lets a sender put on a stream nobody reads: a sending of it cannot have finished by itself.
    const MORE_THAN_GOES_UNREAD: usize = 48 * 1024 * 1024;
    const SOON: Duration = Duration::from_secs(10);

    /// The person at the receiving device presses Cancel on an offer: its task is stopped and its streams let go of with no word
    /// (that is what every build does). At the sender that is a refusal. It was recorded as a failure of the network.
    #[tokio::test]
    async fn an_offer_the_receiver_lets_go_of_without_a_word_is_a_refusal_and_not_a_broken_network()
    {
        let directory = tempfile::tempdir().unwrap();
        let source = file_of(directory.path(), 150_000).await;
        let (_a, _b, left, right) = connected().await;
        let (sent, ()) = tokio::join!(
            tokio::time::timeout(
                SOON,
                send(
                    &left,
                    source,
                    Duration::from_secs(120),
                    None,
                    |_| {},
                    |_| {}
                )
            ),
            async {
                let (tx, mut rx) = right.accept_bi().await.unwrap();
                let streams::Request::File { .. } = streams::receive(&mut rx).await.unwrap() else {
                    panic!("not an offer of a file");
                };
                drop((tx, rx));
            }
        );
        let sent = sent.expect("the sender waited on for an offer the other device had let go of");
        assert!(matches!(sent, Err(Error::Refused)), "{sent:?}");
    }

    /// The sender takes back an offer nobody answered: at the receiver it ends at once. It stayed on the screen until its time ran out.
    #[tokio::test]
    async fn an_offer_the_sender_takes_back_ends_at_the_receiver_at_once() {
        let directory = tempfile::tempdir().unwrap();
        let source = file_of(directory.path(), 150_000).await;
        let (_a, _b, left, right) = connected().await;
        // The connection outlives the sending, as it does in the application: only the offer is taken back.
        let on = left.clone();
        let sending = tokio::spawn(async move {
            send(&on, source, Duration::from_secs(120), None, |_| {}, |_| {}).await
        });
        let (prompt, shown) = oneshot::channel();
        // Kept: the person has not answered, and never does.
        let (_decision, selected) = oneshot::channel();
        let receiving = tokio::spawn(async move {
            let (tx, rx) = right.accept_bi().await.unwrap();
            receive(tx, rx, prompt, selected, Duration::from_secs(120), |_| {}).await
        });
        shown.await.expect("the offer was never shown");
        sending.abort();
        let received = tokio::time::timeout(SOON, receiving)
            .await
            .expect("an offer that was taken back was still waiting for an answer")
            .unwrap();
        assert!(matches!(received, Err(Error::Refused)), "{received:?}");
    }

    /// A file that was accepted and is moving, and the receiving device lets go of it (stopped by its person, or it cannot go on):
    /// the sender says the other device stopped it, not that the transfer was interrupted or timed out.
    #[tokio::test]
    async fn a_moving_file_the_receiver_lets_go_of_is_stopped_by_it_at_the_sender() {
        let directory = tempfile::tempdir().unwrap();
        let source = file_of(directory.path(), MORE_THAN_GOES_UNREAD).await;
        let (_a, _b, left, right) = connected().await;
        let (sent, ()) = tokio::join!(
            tokio::time::timeout(
                SOON,
                send(
                    &left,
                    source,
                    Duration::from_secs(120),
                    None,
                    |_| {},
                    |_| {}
                )
            ),
            async {
                let (mut tx, mut rx) = right.accept_bi().await.unwrap();
                streams::receive(&mut rx).await.unwrap();
                transport::send(&mut tx, &Reply::Accepted).await.unwrap();
                let mut some = [0_u8; 4096];
                rx.read_exact(&mut some).await.unwrap();
                drop((tx, rx));
            }
        );
        let sent = sent.expect("the sender went on writing to a device that had let go");
        assert!(
            matches!(sent, Err(Error::Transfer(transfer::Error::Stopped))),
            "{sent:?}"
        );
    }

    /// The other way round: the sender lets go of a file that is moving. The receiver says the other device stopped it, and nothing
    /// of the file is left where it was going.
    #[tokio::test]
    async fn a_moving_file_the_sender_lets_go_of_is_stopped_by_it_at_the_receiver_and_leaves_nothing()
     {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("received");
        std::fs::create_dir(&destination).unwrap();
        let source = file_of(directory.path(), MORE_THAN_GOES_UNREAD).await;
        let (_a, _b, left, right) = connected().await;
        // The connection outlives the sending, as it does in the application: only this file is let go of.
        let on = left.clone();
        let sending = tokio::spawn(async move {
            send(&on, source, Duration::from_secs(120), None, |_| {}, |_| {}).await
        });
        let (prompt, shown) = oneshot::channel();
        let (decision, selected) = oneshot::channel();
        let (moving, mut moved) = tokio::sync::mpsc::unbounded_channel();
        let receiving = tokio::spawn(async move {
            let (tx, rx) = right.accept_bi().await.unwrap();
            receive(
                tx,
                rx,
                prompt,
                selected,
                Duration::from_secs(120),
                move |bytes| {
                    let _ = moving.send(bytes);
                },
            )
            .await
        });
        shown.await.expect("the offer was never shown");
        decision.send(destination.clone()).unwrap();
        moved
            .recv()
            .await
            .expect("nothing of the file ever arrived");
        sending.abort();
        let received = tokio::time::timeout(SOON, receiving)
            .await
            .expect("the receiver waited on for a file its sender had let go of")
            .unwrap();
        assert!(
            matches!(received, Err(Error::Transfer(transfer::Error::Stopped))),
            "{received:?}"
        );
        assert_eq!(
            std::fs::read_dir(&destination).unwrap().count(),
            0,
            "part of a file that was stopped was left in the folder"
        );
    }

    /// A file that was accepted and wholly sent, with one of its name already in the folder: the receiver keeps what was there and
    /// says why, and the sender is told that the other device stopped it. It read that as a broken network.
    #[tokio::test]
    async fn a_file_the_receiver_cannot_keep_is_said_to_the_sender_and_what_was_there_stays() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("received");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("example.bin"), b"what was there").unwrap();
        let source = file_of(directory.path(), 150_000).await;
        let (_a, _b, left, right) = connected().await;
        let (prompt, shown) = oneshot::channel();
        let (decision, selected) = oneshot::channel();
        let (sent, received, ()) = tokio::join!(
            tokio::time::timeout(
                SOON,
                send(
                    &left,
                    source,
                    Duration::from_secs(120),
                    None,
                    |_| {},
                    |_| {}
                )
            ),
            async {
                let (tx, rx) = right.accept_bi().await.unwrap();
                receive(tx, rx, prompt, selected, Duration::from_secs(120), |_| {}).await
            },
            async {
                shown.await.unwrap();
                decision.send(destination.clone()).unwrap();
            }
        );
        assert!(
            matches!(received, Err(Error::Transfer(transfer::Error::Exists))),
            "{received:?}"
        );
        let sent = sent.expect("the sender was never told");
        assert!(
            matches!(sent, Err(Error::Transfer(transfer::Error::Stopped))),
            "{sent:?}"
        );
        assert_eq!(
            std::fs::read(destination.join("example.bin")).unwrap(),
            b"what was there"
        );
    }
}
