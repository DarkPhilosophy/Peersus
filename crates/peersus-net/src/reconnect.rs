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

/// How long a name is given to be looked up. Past this the typed address is left out and the device is tried at the addresses it already
/// has: a name that does not answer must not hold up a connection that would have worked without it.
const LOOKUP_FOR: std::time::Duration = std::time::Duration::from_secs(5);

/// The most addresses taken from one name, so that a name with many records cannot make the list of a device grow without bound.
const MOST_FROM_A_NAME: usize = 4;

/// Where to try a remembered device: the addresses it is already known at, and, when the person typed one, what that gives now. A name is
/// looked up HERE, at each connection and off the event loop (the lookup blocks), so a device whose address changed is found again by the
/// same words. What is typed only ADDS places to try: it never replaces what was learnt, because a typed address that is wrong must not
/// make a device that was reachable unreachable. The port is the one typed, or else the one the device was last seen at; with neither,
/// the name cannot be tried and is left out.
pub(crate) async fn address_to_try(remembered: &Remembered) -> iroh::EndpointAddr {
    let mut address = remembered.address.clone();
    let Some(typed) = remembered.typed.as_ref() else {
        return address;
    };
    let Some(port) = typed.port.or_else(|| {
        remembered
            .address
            .ip_addrs()
            .map(|known| known.port())
            .next()
    }) else {
        return address;
    };
    use peersus_core::address::Host;
    let found: Vec<std::net::SocketAddr> = match &typed.host {
        Host::Ip(ip) => vec![std::net::SocketAddr::new(*ip, port)],
        Host::Name(name) => {
            let name = name.clone();
            let lookup = tokio::task::spawn_blocking(move || {
                std::net::ToSocketAddrs::to_socket_addrs(&(name.as_str(), port))
                    .map(|found| found.collect::<Vec<_>>())
            });
            match tokio::time::timeout(LOOKUP_FOR, lookup).await {
                Ok(Ok(Ok(found))) => found,
                _ => Vec::new(),
            }
        }
    };
    for place in found
        .into_iter()
        .filter(|place| !place.ip().is_unspecified() && !place.ip().is_multicast())
        .take(MOST_FROM_A_NAME)
    {
        address = address.with_ip_addr(place);
    }
    address
}

/// The host rechecks membership when this asynchronous admission completes.
pub(crate) async fn connect_remembered(
    endpoint: &Endpoint,
    remembered: &Remembered,
) -> Result<Connection, transport::Error> {
    let connection = tokio::time::timeout(
        transport::CONTROL_TIMEOUT,
        endpoint.connect(address_to_try(remembered).await, ALPN),
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

/// The code a device closes a connection with when it does not know the one that came: `accept_known` gives it, and a test of the
/// connection reads it. A code and not the words that go with it, because words are for people and a code is for this to read.
pub const REFUSED: u32 = 1;

/// Whether a connection to a paired device works, and if not, why, without keeping it: the same `Resume` and `Accepted` as
/// [`connect_remembered`], and then the connection is let go. Three things can be told apart, because each is dealt with differently:
///
/// * it answered and knows this device: `Paired`;
/// * it answered and does NOT know this device (it closed the connection with [`REFUSED`]): `NeedsPairing`;
/// * there was no answer in time, or no route: `Unreachable`.
///
/// A connection that closed for any other reason is `Unreachable` and not `NeedsPairing`: only the refusal of the other side says that it
/// does not know this device, and anything else (a network that went, a device that stopped) must not send a person to pair again.
pub async fn probe(endpoint: &Endpoint, remembered: &Remembered) -> peersus_core::model::LinkTest {
    use peersus_core::model::LinkTest;
    let connection = match tokio::time::timeout(
        transport::CONTROL_TIMEOUT,
        endpoint.connect(address_to_try(remembered).await, ALPN),
    )
    .await
    {
        Ok(Ok(connection)) => connection,
        // No route, or nobody there: the device never answered.
        _ => return LinkTest::Unreachable,
    };
    let exchange = async {
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
    let found = match exchange {
        Ok(()) => LinkTest::Paired,
        Err(_) => classify(connection.close_reason()),
    };
    // A test never keeps what it opened: the session that is kept is the one the host installs, and this is not it.
    connection.close(0_u32.into(), b"link tested");
    found
}

/// What a connection that ended says of the device at the other end, from why it closed. Pure, so it is tried without a network: only the
/// close of the other side with [`REFUSED`] says that it does not know this device.
pub fn classify(closed: Option<iroh::endpoint::ConnectionError>) -> peersus_core::model::LinkTest {
    use iroh::endpoint::ConnectionError;
    use peersus_core::model::LinkTest;
    match closed {
        Some(ConnectionError::ApplicationClosed(close)) if close.error_code == REFUSED.into() => {
            LinkTest::NeedsPairing
        }
        _ => LinkTest::Unreachable,
    }
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
        connection.close(REFUSED.into(), b"session refused");
        return Err(error);
    }
    Ok(connection)
}
