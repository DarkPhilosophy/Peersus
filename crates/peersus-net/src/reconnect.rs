//! Mutual remembered-identity admission, separate from invitation pairing.
use crate::{
    peers::{Peers, Remembered},
    transport,
};
use iroh::{Endpoint, EndpointId, endpoint::Connection};
use peersus_core::model::PeerId;
use serde::{Deserialize, Serialize};

pub const ALPN: &[u8] = b"peersus/remembered/1";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Message {
    Resume,
    Accepted,
}

/// Stored routing hints are only hints; QUIC authenticates the pinned identity.
/// A transport connection is not online until the remote also admits us.
pub async fn connect(
    endpoint: &Endpoint,
    peers: &Peers,
    peer: &PeerId,
) -> Result<Connection, transport::Error> {
    let remembered = peers.get(peer).ok_or(transport::Error::Invalid)?;
    connect_remembered(endpoint, remembered).await
}

/// The host rechecks membership when this asynchronous admission completes.
pub(crate) async fn connect_remembered(
    endpoint: &Endpoint,
    remembered: &Remembered,
) -> Result<Connection, transport::Error> {
    let connection = tokio::time::timeout(
        transport::CONTROL_TIMEOUT,
        endpoint.connect(remembered.address.clone(), ALPN),
    )
    .await
    .map_err(|_| transport::Error::Timeout)?
    .map_err(|_| transport::Error::Network)?;
    let result = async {
        let (mut tx, mut rx) = connection
            .open_bi()
            .await
            .map_err(|_| transport::Error::Network)?;
        transport::send(&mut tx, &Message::Resume).await?;
        tx.finish().map_err(|_| transport::Error::Network)?;
        match transport::receive(&mut rx).await? {
            Message::Accepted => Ok(()),
            _ => Err(transport::Error::Invalid),
        }
    }
    .await;
    if let Err(error) = result {
        connection.close(1_u32.into(), b"session refused");
        return Err(error);
    }
    Ok(connection)
}

/// Unknown or forgotten devices cannot regain admission merely by reconnecting.
pub async fn accept(connection: Connection, peers: &Peers) -> Result<Connection, transport::Error> {
    let identity = peers
        .get(&PeerId(connection.remote_id().to_string()))
        .map(|remembered| remembered.address.id);
    accept_known(connection, identity).await
}
pub(crate) async fn accept_known(
    connection: Connection,
    identity: Option<EndpointId>,
) -> Result<Connection, transport::Error> {
    let result = async {
        if connection.alpn() != ALPN || identity != Some(connection.remote_id()) {
            return Err(transport::Error::Invalid);
        }
        let (mut tx, mut rx) =
            tokio::time::timeout(transport::CONTROL_TIMEOUT, connection.accept_bi())
                .await
                .map_err(|_| transport::Error::Timeout)?
                .map_err(|_| transport::Error::Network)?;
        match transport::receive(&mut rx).await? {
            Message::Resume => {}
            _ => return Err(transport::Error::Invalid),
        }
        transport::send(&mut tx, &Message::Accepted).await?;
        tx.finish().map_err(|_| transport::Error::Network)?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        connection.close(1_u32.into(), b"session refused");
        return Err(error);
    }
    Ok(connection)
}
