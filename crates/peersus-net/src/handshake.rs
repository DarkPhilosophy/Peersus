//! Invitation admission precedes all human pairing prompts.
use crate::{admission::Admission, invitation::Invitation, transport};
use iroh::{Endpoint, EndpointAddr, endpoint::Connection};
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    capability: [u8; 32],
    name: String,
    address: EndpointAddr,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Welcome {
    nonce: [u8; 32],
    name: String,
    purpose: Purpose,
}

/// What an admitted connection may become. Decided by the host that showed the code and told to the other side in
/// `Welcome`, never claimed by the device that connects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Purpose {
    /// The code was one shown in Peers: the two devices go on to the pairing ceremony, and only then are paired.
    Pairing,
    /// The code was one shown for files: the connection carries the offered files and nothing else. It is never a
    /// pairing and never remembers the device.
    Files,
    /// A larger files-only selection. The authenticated host fixes the allowance
    /// from its selected set; a scanner cannot enlarge it. Legacy selections keep
    /// using `Files`, so old receivers never silently accept only part of a batch.
    FileSelection { files: u32 },
}

pub struct Admitted {
    pub connection: Connection,
    pub nonce: [u8; 32],
    pub name: String,
    pub address: EndpointAddr,
    pub purpose: Purpose,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invitation connection failed")]
    Connection,
    #[error("Invitation was refused or already used")]
    Refused,
    #[error("Device name is invalid")]
    Name,
    #[error(transparent)]
    Transport(#[from] transport::Error),
}

fn validate_name(name: &str) -> Result<(), Error> {
    if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(Error::Name);
    }
    Ok(())
}

/// The decoded invitation pins the remote public identity in QUIC.
pub async fn join(
    endpoint: &Endpoint,
    invitation: Invitation,
    name: String,
) -> Result<Admitted, Error> {
    validate_name(&name)?;
    let address = invitation.address.clone();
    let connection = tokio::time::timeout(
        transport::CONTROL_TIMEOUT,
        endpoint.connect(invitation.address, transport::ALPN),
    )
    .await
    .map_err(|_| Error::Connection)?
    .map_err(|_| Error::Connection)?;
    let result = async {
        let (mut tx, mut rx) = connection.open_bi().await.map_err(|_| Error::Connection)?;
        transport::send(
            &mut tx,
            &Hello {
                capability: invitation.capability,
                name,
                address: endpoint.addr(),
            },
        )
        .await?;
        tx.finish().map_err(|_| Error::Connection)?;
        let welcome: Welcome = transport::receive(&mut rx).await?;
        validate_name(&welcome.name)?;
        Ok::<_, Error>((welcome.nonce, welcome.name, welcome.purpose))
    }
    .await;
    match result {
        Ok((nonce, name, purpose)) => Ok(Admitted {
            connection,
            nonce,
            name,
            address,
            purpose,
        }),
        Err(error) => {
            connection.close(1_u32.into(), b"invitation refused");
            Err(error)
        }
    }
}

/// The single host owner serializes access to the admission gate.
pub async fn accept(
    connection: Connection,
    gate: &mut Admission,
    name: String,
) -> Result<Admitted, Error> {
    // A bare gate is one code shown to pair: that is the only thing it has ever been.
    accept_with(
        connection,
        |capability| gate.consume(capability, Instant::now()).then_some(Purpose::Pairing),
        name,
    )
    .await
}

/// The gate callback is synchronous, so a shared host gate is never locked over I/O.
pub(crate) async fn accept_with(
    connection: Connection,
    consume: impl FnOnce(&[u8; 32]) -> Option<Purpose>,
    name: String,
) -> Result<Admitted, Error> {
    validate_name(&name)?;
    let result = async {
        let (mut tx, mut rx) =
            tokio::time::timeout(std::time::Duration::from_secs(30), connection.accept_bi())
                .await
                .map_err(|_| Error::Connection)?
                .map_err(|_| Error::Connection)?;
        let hello: Hello = transport::receive(&mut rx).await?;
        if hello.address.id != connection.remote_id() {
            return Err(Error::Refused);
        }
        // Which gate the capability opened is decided here, by the host that showed the code, and told to the other
        // side: nothing the connecting device says about itself can choose it.
        let purpose = consume(&hello.capability).ok_or(Error::Refused)?;
        validate_name(&hello.name)?;
        let nonce = rand::random();
        transport::send(&mut tx, &Welcome { nonce, name, purpose }).await?;
        tx.finish().map_err(|_| Error::Connection)?;
        Ok::<_, Error>((nonce, hello.name, hello.address, purpose))
    }
    .await;
    match result {
        Ok((nonce, name, address, purpose)) => Ok(Admitted {
            connection,
            nonce,
            name,
            address,
            purpose,
        }),
        Err(error) => {
            connection.close(1_u32.into(), b"invitation refused");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;
    use peersus_core::config::NetworkSettings;

    #[test]
    fn bulk_selection_fails_closed_for_old_receivers_without_changing_legacy_purposes() {
        #[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
        #[serde(rename_all = "kebab-case")]
        enum LegacyPurpose { Pairing, Files }
        for (current, legacy) in [(Purpose::Pairing, LegacyPurpose::Pairing), (Purpose::Files, LegacyPurpose::Files)] {
            let encoded = serde_json::to_vec(&current).unwrap();
            assert_eq!(serde_json::from_slice::<LegacyPurpose>(&encoded).unwrap(), legacy);
            assert_eq!(serde_json::from_slice::<Purpose>(&serde_json::to_vec(&legacy).unwrap()).unwrap(), current);
        }
        let selection = Purpose::FileSelection { files: 40 };
        let encoded = serde_json::to_vec(&selection).unwrap();
        assert_eq!(serde_json::from_slice::<Purpose>(&encoded).unwrap(), selection);
        assert!(serde_json::from_slice::<LegacyPurpose>(&encoded).is_err(),
            "an old receiver silently downgraded a larger authenticated selection");
    }

    #[tokio::test]
    async fn invitation_pins_identity_and_replay_cannot_enter_pairing() {
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
        };
        let a = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let b = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let invitation = Invitation::create(b.addr(), 100).unwrap();
        let mut gate = Admission::new(&invitation, 100, Instant::now()).unwrap();
        let (_, refused) = tokio::join!(
            async {
                let connection = a.connect(b.addr(), transport::ALPN).await.unwrap();
                let (mut tx, mut rx) = connection.open_bi().await.unwrap();
                transport::send(
                    &mut tx,
                    &Hello {
                        capability: invitation.capability,
                        name: "Sender".into(),
                        address: EndpointAddr::new(SecretKey::generate().public()),
                    },
                )
                .await
                .unwrap();
                tx.finish().unwrap();
                assert!(transport::receive::<Welcome>(&mut rx).await.is_err());
            },
            async {
                accept(
                    b.accept().await.unwrap().await.unwrap(),
                    &mut gate,
                    "Recipient".into(),
                )
                .await
            }
        );
        assert!(matches!(refused, Err(Error::Refused)));
        let (left, right) = tokio::join!(join(&a, invitation.clone(), "Sender".into()), async {
            let connection = b.accept().await.unwrap().await.unwrap();
            accept(connection, &mut gate, "Recipient".into()).await
        });
        let left = left.unwrap();
        let right = right.unwrap();
        assert_eq!(left.connection.remote_id(), b.id());
        assert_eq!(right.connection.remote_id(), a.id());
        assert_eq!(left.nonce, right.nonce);
        assert_eq!(left.name, "Recipient");
        assert_eq!(right.name, "Sender");
        assert_eq!(left.address, b.addr());
        assert_eq!(right.address, a.addr());
        left.connection.close(0_u32.into(), b"done");
        right.connection.close(0_u32.into(), b"done");
        let (replay, refused) = tokio::join!(join(&a, invitation, "Sender".into()), async {
            let connection = b.accept().await.unwrap().await.unwrap();
            accept(connection, &mut gate, "Recipient".into()).await
        });
        assert!(replay.is_err());
        assert!(matches!(refused, Err(Error::Refused)));
        a.close().await;
        b.close().await;
    }
}
