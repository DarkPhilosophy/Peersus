//! Run mutual consent on a connection whose invitation was already authenticated.
use crate::transport;
use iroh::{EndpointId, endpoint::Connection};
use peersus_core::pairing::{AuthorizedPair, PairingSession, Role};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Pairing was refused or its interface closed")]
    Refused,
    #[error("Pairing expired")]
    Expired,
    #[error("Pairing connection failed")]
    Connection,
    #[error(transparent)]
    Protocol(#[from] peersus_core::pairing::PairingError),
    #[error(transparent)]
    Transport(#[from] transport::Error),
}

/// The host authenticates and consumes the invitation capability before calling.
/// Choices come only from the local UI/CLI; dropping the channel refuses consent.
pub async fn run(
    connection: &Connection,
    local: EndpointId,
    role: Role,
    invitation_nonce: [u8; 32],
    prompt: oneshot::Sender<[u8; 3]>,
    choice: oneshot::Receiver<u8>,
) -> Result<AuthorizedPair, Error> {
    tokio::time::timeout(Duration::from_secs(120), async {
        let start = Instant::now();
        let remote = connection.remote_id();
        let (initiator, responder) = match role {
            Role::Initiator => (local, remote),
            Role::Responder => (remote, local),
        };
        let mut session = PairingSession::new(
            transport::exporter(connection)?,
            *initiator.as_bytes(),
            *responder.as_bytes(),
            invitation_nonce,
            role,
            0,
            120,
        )?;
        prompt.send(session.options()).map_err(|_| Error::Refused)?;
        let number = choice.await.map_err(|_| Error::Refused)?;
        let commitment = session.choose(number, rand::random(), start.elapsed().as_secs())?;
        let (mut tx, mut rx) = match role {
            Role::Initiator => connection.open_bi().await,
            Role::Responder => connection.accept_bi().await,
        }
        .map_err(|_| Error::Connection)?;
        transport::send(&mut tx, &commitment).await?;
        session.receive_commitment(
            // The other device may still be waiting for its person's choice.
            // The outer ceremony timeout remains the absolute 120-second deadline.
            transport::receive_with_timeout(&mut rx, Duration::from_secs(120)).await?,
            start.elapsed().as_secs(),
        )?;
        let reveal = session.reveal(start.elapsed().as_secs())?;
        transport::send(&mut tx, &reveal).await?;
        let ready = session.receive_reveal(
            transport::receive(&mut rx).await?,
            start.elapsed().as_secs(),
        )?;
        transport::send(&mut tx, &ready).await?;
        session.receive_ready(
            transport::receive(&mut rx).await?,
            start.elapsed().as_secs(),
        )?;
        tx.finish().map_err(|_| Error::Connection)?;
        session
            .finish(start.elapsed().as_secs())
            .map_err(Error::from)
    })
    .await
    .map_err(|_| Error::Expired)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;
    use peersus_core::config::NetworkSettings;

    async fn exchange(matching: bool, delay: Duration) {
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
        let (outgoing, incoming) = tokio::join!(a.connect(b.addr(), transport::ALPN), async {
            b.accept().await.unwrap().await
        });
        let outgoing = outgoing.unwrap();
        let incoming = incoming.unwrap();
        let (pa, ra) = oneshot::channel();
        let (pb, rb) = oneshot::channel();
        let (ca, sa) = oneshot::channel();
        let (cb, sb) = oneshot::channel();
        let (left, right, ()) = tokio::join!(
            run(&outgoing, a.id(), Role::Initiator, [7; 32], pa, sa),
            run(&incoming, b.id(), Role::Responder, [7; 32], pb, sb),
            async {
                let options_a = ra.await.unwrap();
                let options_b = rb.await.unwrap();
                // The same three numbers, and no number at the same place on both: what each person presses is a number.
                assert!(options_a.iter().all(|number| options_b.contains(number)));
                assert!((0..3).all(|place| options_a[place] != options_b[place]));
                ca.send(options_a[0]).unwrap();
                tokio::time::sleep(delay).await;
                cb.send(options_a[usize::from(!matching)]).unwrap();
            }
        );
        if matching {
            assert!(left.is_ok(), "initiator: {left:?}");
            assert!(right.is_ok(), "responder: {right:?}");
        } else {
            assert!(matches!(
                left,
                Err(Error::Protocol(
                    peersus_core::pairing::PairingError::Mismatch
                ))
            ));
            assert!(matches!(
                right,
                Err(Error::Protocol(
                    peersus_core::pairing::PairingError::Mismatch
                ))
            ));
        }
        outgoing.close(0_u32.into(), b"done");
        incoming.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn both_matching_local_choices_authorize_over_quic() {
        exchange(true, Duration::ZERO).await;
    }

    #[tokio::test]
    async fn differing_local_choices_refuse_both_endpoints() {
        exchange(false, Duration::ZERO).await;
    }

    #[tokio::test]
    async fn confirmation_after_the_control_timeout_still_pairs_within_the_displayed_deadline() {
        exchange(true, transport::CONTROL_TIMEOUT + Duration::from_secs(1)).await;
    }
}
