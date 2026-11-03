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
                // An answer that never came by the end of the wait is a refusal: nobody accepted. Only a break in the connection is a failure.
                reply = &mut answer => break reply.map_err(|error| match error {
                    transport::Error::Timeout => Error::Refused,
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
    match transport::receive(&mut rx).await? {
        Reply::Complete => Ok(()),
        _ => Err(Error::Refused),
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
    let directory = match tokio::time::timeout(wait, decision).await {
        Ok(Ok(directory)) => directory,
        _ => {
            let _ = transport::send(&mut tx, &Reply::Refused).await;
            let _ = tx.finish();
            return Err(Error::Refused);
        }
    };
    transport::send(&mut tx, &Reply::Accepted).await?;
    let path = transfer::receive(&mut rx, &offer, &directory, progress).await?;
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
}
