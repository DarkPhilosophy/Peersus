//! Telling a paired device which picture is mine, without sending it every time.
//!
//! THE EXCHANGE, on one stream, in the direction of the picture:
//!
//! ```text
//! sender   -> receiver : Announce { digest }     the digest of the picture, or of having none
//! receiver -> sender   : Have                    already kept: nothing else is sent, by anyone
//!                      | Send                    not kept: the picture follows
//! sender   -> receiver : Header { w, h }, then exactly w*h*4 raw bytes      only after Send
//! ```
//!
//! WHY IT IS ARRANGED SO. A device that is online again an hour later holds the same picture it held
//! then, so the digest, 32 bytes, is all that has to cross. The picture, up to 64 kilobytes of pixels,
//! crosses only when the receiver does not hold that digest, which is the first time and whenever the
//! picture was changed. The receiver decides; the sender never pushes.
//!
//! WHAT THE RECEIVER NEVER TAKES ON TRUST. The digest it keeps a picture under is computed from the
//! pixels that arrived, and has to be the one that was announced. A sender that announced one picture
//! and sent another is refused and nothing of it is kept, so a digest can never come to name a picture
//! it is not the digest of.
//!
//! WHAT IT BOUNDS. The header's sides are checked against the picture limits BEFORE one pixel is read,
//! and exactly the announced number of bytes is read and no more, so a sender cannot make a receiver
//! read a large amount by claiming a large picture or by sending past what it claimed.
//!
//! WHAT IT DOES NOT DECIDE. Whether the sender is a device to listen to at all: the host admits a
//! stream only from a paired device, as for every other stream, and this takes what it is given.

use crate::{pictures::Pictures, transport};
use iroh::endpoint::{RecvStream, SendStream};
use peersus_core::profile::{Digest, MAX_EDGE, Picture};
use serde::{Deserialize, Serialize};

/// What the sender says first: only which picture it holds.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Announce {
    /// The 32 bytes of the digest, as numbers.
    pub digest: [u8; 32],
}

/// What the receiver answers.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Reply {
    /// It already holds that digest.
    Have,
    /// It does not: send the picture.
    Send,
}

/// What the sender says before the pixels: their shape, so that how many follow is known.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub width: u32,
    pub height: u32,
}

/// What came of an exchange, as the receiver sees it.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The sender holds no picture; any it was shown before is to be forgotten by the host.
    None,
    /// The picture is held already; nothing was transferred.
    Known(Digest),
    /// The picture was not held and was received and kept.
    Received(Digest),
}

/// Why an exchange was not completed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Transport(#[from] transport::Error),
    #[error("the picture could not be kept")]
    Keep,
    #[error("the picture that was sent is not the one that was announced")]
    Other,
    #[error("the picture is larger than a profile picture may be")]
    TooBig,
    #[error("the picture could not be read")]
    Stream,
}

/// The sender's side: announce `own`, and send the picture if, and only if, it is asked for. Says
/// whether the picture itself was sent: `false` when the other side already held it.
pub async fn announce(
    send: &mut SendStream,
    receive: &mut RecvStream,
    own: Option<&Picture>,
) -> Result<bool, Error> {
    let digest = own.map_or_else(Picture::digest_of_none, Picture::digest);
    transport::send(
        send,
        &Announce {
            digest: *digest.as_bytes(),
        },
    )
    .await?;
    let sent = match transport::receive::<Reply>(receive).await? {
        Reply::Have => false,
        Reply::Send => {
            let Some(picture) = own else {
                // Asked for a picture that was never announced: nothing to give.
                return Err(Error::Other);
            };
            transport::send(
                send,
                &Header {
                    width: picture.width(),
                    height: picture.height(),
                },
            )
            .await?;
            send.write_all(picture.rgba())
                .await
                .map_err(|_| Error::Stream)?;
            true
        }
    };
    send.finish().map_err(|_| Error::Stream)?;
    Ok(sent)
}

/// The receiver's side: read the announcement, answer, and take the picture only if it was asked for.
pub async fn serve(
    send: &mut SendStream,
    receive: &mut RecvStream,
    pictures: &Pictures,
) -> Result<Outcome, Error> {
    let announced = Digest::from_bytes(transport::receive::<Announce>(receive).await?.digest);
    if announced == Picture::digest_of_none() {
        transport::send(send, &Reply::Have).await?;
        send.finish().map_err(|_| Error::Stream)?;
        return Ok(Outcome::None);
    }
    if pictures.has(&announced) {
        transport::send(send, &Reply::Have).await?;
        send.finish().map_err(|_| Error::Stream)?;
        return Ok(Outcome::Known(announced));
    }
    transport::send(send, &Reply::Send).await?;
    send.finish().map_err(|_| Error::Stream)?;

    let header: Header = transport::receive(receive).await?;
    // The shape is judged before a pixel is read or a buffer is made for one.
    if header.width == 0
        || header.height == 0
        || header.width > MAX_EDGE
        || header.height > MAX_EDGE
    {
        return Err(Error::TooBig);
    }
    let length = (header.width * header.height * 4) as usize;
    let mut pixels = vec![0u8; length];
    receive
        .read_exact(&mut pixels)
        .await
        .map_err(|_| Error::Stream)?;
    // Exactly the announced number and not one more: a sender that goes on is not trusted.
    let mut extra = [0u8; 1];
    if receive
        .read(&mut extra)
        .await
        .map_err(|_| Error::Stream)?
        .is_some_and(|n| n > 0)
    {
        return Err(Error::Stream);
    }
    let picture = Picture::new(header.width, header.height, pixels).map_err(|_| Error::TooBig)?;
    // Kept only if it is the picture that was announced.
    if picture.digest() != announced {
        return Err(Error::Other);
    }
    let kept = pictures.keep(&picture).map_err(|_| Error::Keep)?;
    Ok(Outcome::Received(kept))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{SecretKey, endpoint::Connection};
    use peersus_core::config::NetworkSettings;

    fn picture(seed: u8) -> Picture {
        let rgba: Vec<u8> = (0..8 * 8 * 4)
            .map(|n| (n as u8).wrapping_mul(7).wrapping_add(seed))
            .collect();
        Picture::new(8, 8, rgba).unwrap()
    }

    /// Two real endpoints, joined by one connection each way.
    async fn pair() -> (Connection, Connection) {
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
        // The endpoints must outlive the test body: leaking them is fine in a test.
        std::mem::forget(a);
        std::mem::forget(b);
        (left.unwrap(), right.unwrap())
    }

    /// One exchange, the sender holding `own`, the receiver keeping its pictures in `pictures`.
    async fn exchange(
        left: &Connection,
        right: &Connection,
        own: Option<&Picture>,
        pictures: &Pictures,
    ) -> (Result<(), Error>, Result<Outcome, Error>) {
        let sending = async {
            let (mut tx, mut rx) = left.open_bi().await.unwrap();
            announce(&mut tx, &mut rx, own).await.map(|_| ())
        };
        let receiving = async {
            let (mut tx, mut rx) = right.accept_bi().await.unwrap();
            serve(&mut tx, &mut rx, pictures).await
        };
        tokio::join!(sending, receiving)
    }

    #[tokio::test]
    async fn a_picture_the_receiver_lacks_is_sent_kept_and_not_sent_again_the_next_time() {
        let (left, right) = pair().await;
        let directory = tempfile::tempdir().unwrap();
        let pictures = Pictures::new(directory.path());
        let own = picture(1);

        let (sent, got) = exchange(&left, &right, Some(&own), &pictures).await;
        sent.unwrap();
        assert_eq!(got.unwrap(), Outcome::Received(own.digest()));
        assert_eq!(pictures.get(&own.digest()).unwrap(), Some(own.clone()));

        // The next time the two are online, the receiver already holds it: nothing but the digest crosses.
        let (sent, got) = exchange(&left, &right, Some(&own), &pictures).await;
        sent.unwrap();
        assert_eq!(got.unwrap(), Outcome::Known(own.digest()));
    }

    #[tokio::test]
    async fn announcing_says_whether_the_picture_itself_crossed() {
        let (left, right) = pair().await;
        let directory = tempfile::tempdir().unwrap();
        let pictures = Pictures::new(directory.path());
        let own = picture(8);
        let mut crossed = Vec::new();
        for _ in 0..2 {
            let sending = async {
                let (mut tx, mut rx) = left.open_bi().await.unwrap();
                announce(&mut tx, &mut rx, Some(&own)).await.unwrap()
            };
            let receiving = async {
                let (mut tx, mut rx) = right.accept_bi().await.unwrap();
                serve(&mut tx, &mut rx, &pictures).await.unwrap()
            };
            let (sent, _) = tokio::join!(sending, receiving);
            crossed.push(sent);
        }
        // The first time the picture crossed; the second time, the receiver held it, so it did not.
        assert_eq!(crossed, [true, false]);
        // Holding no picture never sends one.
        let sending = async {
            let (mut tx, mut rx) = left.open_bi().await.unwrap();
            announce(&mut tx, &mut rx, None).await.unwrap()
        };
        let receiving = async {
            let (mut tx, mut rx) = right.accept_bi().await.unwrap();
            serve(&mut tx, &mut rx, &pictures).await.unwrap()
        };
        assert!(!tokio::join!(sending, receiving).0);
    }

    #[tokio::test]
    async fn when_the_picture_changes_the_new_one_is_asked_for_and_the_old_one_is_left_alone() {
        let (left, right) = pair().await;
        let directory = tempfile::tempdir().unwrap();
        let pictures = Pictures::new(directory.path());
        let (first, second) = (picture(1), picture(2));
        exchange(&left, &right, Some(&first), &pictures)
            .await
            .1
            .unwrap();

        let (sent, got) = exchange(&left, &right, Some(&second), &pictures).await;
        sent.unwrap();
        assert_eq!(got.unwrap(), Outcome::Received(second.digest()));
        assert!(pictures.has(&first.digest()) && pictures.has(&second.digest()));
        // And changing back to the first needs nothing sent: it is still held.
        let (sent, got) = exchange(&left, &right, Some(&first), &pictures).await;
        sent.unwrap();
        assert_eq!(got.unwrap(), Outcome::Known(first.digest()));
    }

    #[tokio::test]
    async fn a_device_that_holds_no_picture_says_so_and_nothing_is_kept() {
        let (left, right) = pair().await;
        let directory = tempfile::tempdir().unwrap();
        let pictures = Pictures::new(directory.path());
        let (sent, got) = exchange(&left, &right, None, &pictures).await;
        sent.unwrap();
        assert_eq!(got.unwrap(), Outcome::None);
        assert!(
            !directory.path().join("device-pictures").exists(),
            "something was kept for no picture"
        );
    }

    #[tokio::test]
    async fn a_picture_that_is_already_held_is_answered_have_and_no_pixels_are_expected() {
        let (left, right) = pair().await;
        let directory = tempfile::tempdir().unwrap();
        let pictures = Pictures::new(directory.path());
        let own = picture(3);
        pictures.keep(&own).unwrap();

        let sender = async {
            let (mut tx, mut rx) = left.open_bi().await.unwrap();
            transport::send(
                &mut tx,
                &Announce {
                    digest: *own.digest().as_bytes(),
                },
            )
            .await
            .unwrap();
            let reply: Reply = transport::receive(&mut rx).await.unwrap();
            // After Have the sender writes nothing more, and the receiver must not wait for pixels.
            tx.finish().unwrap();
            reply
        };
        let receiver = async {
            let (mut tx, mut rx) = right.accept_bi().await.unwrap();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                serve(&mut tx, &mut rx, &pictures),
            )
            .await
        };
        let (reply, outcome) = tokio::join!(sender, receiver);
        assert_eq!(reply, Reply::Have);
        let outcome = outcome.expect("the receiver waited for pixels it had said it did not need");
        assert_eq!(outcome.unwrap(), Outcome::Known(own.digest()));
    }

    #[tokio::test]
    async fn a_sender_that_announces_one_picture_and_sends_another_is_refused_and_nothing_is_kept()
    {
        let (left, right) = pair().await;
        let directory = tempfile::tempdir().unwrap();
        let pictures = Pictures::new(directory.path());
        let (announced, sent_instead) = (picture(1), picture(2));

        let liar = async {
            let (mut tx, mut rx) = left.open_bi().await.unwrap();
            transport::send(
                &mut tx,
                &Announce {
                    digest: *announced.digest().as_bytes(),
                },
            )
            .await
            .unwrap();
            assert_eq!(
                transport::receive::<Reply>(&mut rx).await.unwrap(),
                Reply::Send
            );
            transport::send(
                &mut tx,
                &Header {
                    width: 8,
                    height: 8,
                },
            )
            .await
            .unwrap();
            tx.write_all(sent_instead.rgba()).await.unwrap();
            tx.finish().unwrap();
        };
        let receiver = async {
            let (mut tx, mut rx) = right.accept_bi().await.unwrap();
            serve(&mut tx, &mut rx, &pictures).await
        };
        let (_, outcome) = tokio::join!(liar, receiver);
        assert!(matches!(outcome, Err(Error::Other)), "got {outcome:?}");
        assert!(
            !pictures.has(&announced.digest()),
            "the picture that was only announced was kept"
        );
        assert!(
            !pictures.has(&sent_instead.digest()),
            "the picture that was sent instead was kept"
        );
    }

    #[tokio::test]
    async fn a_header_over_the_limits_is_refused_before_any_pixel_is_read() {
        for (width, height) in [
            (MAX_EDGE + 1, 1),
            (1, MAX_EDGE + 1),
            (0, 5),
            (5, 0),
            (u32::MAX, u32::MAX),
        ] {
            let (left, right) = pair().await;
            let directory = tempfile::tempdir().unwrap();
            let pictures = Pictures::new(directory.path());
            let announced = picture(4).digest();

            let sender = async {
                let (mut tx, mut rx) = left.open_bi().await.unwrap();
                transport::send(
                    &mut tx,
                    &Announce {
                        digest: *announced.as_bytes(),
                    },
                )
                .await
                .unwrap();
                assert_eq!(
                    transport::receive::<Reply>(&mut rx).await.unwrap(),
                    Reply::Send
                );
                transport::send(&mut tx, &Header { width, height })
                    .await
                    .unwrap();
                // No pixels at all: if the receiver tried to read them it would wait.
                tx.finish().unwrap();
            };
            let receiver = async {
                let (mut tx, mut rx) = right.accept_bi().await.unwrap();
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    serve(&mut tx, &mut rx, &pictures),
                )
                .await
            };
            let (_, outcome) = tokio::join!(sender, receiver);
            let outcome =
                outcome.expect("the receiver waited for pixels of a header it should have refused");
            assert!(
                matches!(outcome, Err(Error::TooBig)),
                "{width}x{height}: {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_sender_that_sends_more_pixels_than_it_said_is_refused() {
        let (left, right) = pair().await;
        let directory = tempfile::tempdir().unwrap();
        let pictures = Pictures::new(directory.path());
        let own = picture(5);

        let greedy = async {
            let (mut tx, mut rx) = left.open_bi().await.unwrap();
            transport::send(
                &mut tx,
                &Announce {
                    digest: *own.digest().as_bytes(),
                },
            )
            .await
            .unwrap();
            assert_eq!(
                transport::receive::<Reply>(&mut rx).await.unwrap(),
                Reply::Send
            );
            transport::send(
                &mut tx,
                &Header {
                    width: 8,
                    height: 8,
                },
            )
            .await
            .unwrap();
            tx.write_all(own.rgba()).await.unwrap();
            tx.write_all(b"and some more that nobody asked for")
                .await
                .unwrap();
            tx.finish().unwrap();
        };
        let receiver = async {
            let (mut tx, mut rx) = right.accept_bi().await.unwrap();
            serve(&mut tx, &mut rx, &pictures).await
        };
        let (_, outcome) = tokio::join!(greedy, receiver);
        assert!(
            outcome.is_err(),
            "a sender that went on past what it said was accepted"
        );
        assert!(!pictures.has(&own.digest()));
    }

    #[tokio::test]
    async fn a_sender_that_stops_short_is_refused_and_does_not_leave_the_receiver_waiting() {
        let (left, right) = pair().await;
        let directory = tempfile::tempdir().unwrap();
        let pictures = Pictures::new(directory.path());
        let own = picture(6);

        let short = async {
            let (mut tx, mut rx) = left.open_bi().await.unwrap();
            transport::send(
                &mut tx,
                &Announce {
                    digest: *own.digest().as_bytes(),
                },
            )
            .await
            .unwrap();
            assert_eq!(
                transport::receive::<Reply>(&mut rx).await.unwrap(),
                Reply::Send
            );
            transport::send(
                &mut tx,
                &Header {
                    width: 8,
                    height: 8,
                },
            )
            .await
            .unwrap();
            tx.write_all(&own.rgba()[..own.rgba().len() / 2])
                .await
                .unwrap();
            tx.finish().unwrap();
        };
        let receiver = async {
            let (mut tx, mut rx) = right.accept_bi().await.unwrap();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                serve(&mut tx, &mut rx, &pictures),
            )
            .await
        };
        let (_, outcome) = tokio::join!(short, receiver);
        let outcome = outcome.expect("the receiver waited for pixels that were never coming");
        assert!(outcome.is_err());
        assert!(!pictures.has(&own.digest()));
    }

    #[tokio::test]
    async fn a_sender_that_holds_no_picture_gives_none_even_when_asked_for_one() {
        // The sender holds none and announces the digest of none. A peer that answers Send anyway is
        // asking for something that does not exist: nothing is given, and it is an error.
        let (a, b) = pair().await;
        let driver = async {
            let (mut tx, mut rx) = a.open_bi().await.unwrap();
            announce(&mut tx, &mut rx, None).await.map(|_| ())
        };
        let asker = async {
            let (mut tx, mut rx) = b.accept_bi().await.unwrap();
            let _: Announce = transport::receive(&mut rx).await.unwrap();
            transport::send(&mut tx, &Reply::Send).await.unwrap();
            tx.finish().unwrap();
        };
        let (result, ()) = tokio::join!(driver, asker);
        assert!(
            matches!(result, Err(Error::Other)),
            "a picture that does not exist was given"
        );
    }
}
