//! Authenticated QUIC transport and bounded, length-delimited control messages.
use iroh::{
    Endpoint, SecretKey,
    endpoint::{Builder, Connection, RecvStream, SendStream, presets},
};
use peersus_core::NetworkSettings;
use serde::{Serialize, de::DeserializeOwned};
use std::{net::SocketAddr, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const ALPN: &[u8] = b"peersus/1";
pub const MAX_CONTROL: usize = 48 * 1024;
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "this transport cannot bind internet relay traffic to a selected address; disable relays or use automatic routing"
    )]
    RestrictedRelay,
    #[error("network operation failed")]
    Network,
    #[error("could not initialize the selected transport")]
    Bind(#[source] iroh::endpoint::BindError),
    #[error("control message exceeds the supported size")]
    Oversized,
    #[error("invalid control message")]
    Invalid,
    #[error("network operation timed out")]
    Timeout,
}

/// No auxiliary public discovery/relay sockets are created for explicit binding.
/// Unsupported restrictions fail before an endpoint is started.
pub async fn bind(key: SecretKey, policy: &NetworkSettings) -> Result<Endpoint, Error> {
    if policy.bind.is_some() && policy.allow_relay {
        return Err(Error::RestrictedRelay);
    }
    let mut builder = if policy.allow_relay {
        Builder::new(presets::N0)
    } else {
        Builder::new(presets::Minimal)
    }
    .secret_key(key)
    .alpns(vec![ALPN.to_vec(), crate::reconnect::ALPN.to_vec()])
    .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled);
    if let Some(address) = policy.bind {
        if address.is_unspecified() || address.is_multicast() {
            return Err(Error::Invalid);
        }
        builder = builder
            .clear_ip_transports()
            .bind_addr(SocketAddr::new(address, 0))
            .map_err(|_| Error::Invalid)?;
    }
    builder.bind().await.map_err(Error::Bind)
}

pub fn exporter(connection: &Connection) -> Result<[u8; 32], Error> {
    let mut bytes = [0; 32];
    connection
        .export_keying_material(&mut bytes, b"peersus/pairing/1", ALPN)
        .map_err(|_| Error::Network)?;
    Ok(bytes)
}

pub async fn send<T: Serialize>(stream: &mut SendStream, value: &T) -> Result<(), Error> {
    // A bounded writer prevents serialization from first allocating a huge buffer.
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_CONTROL.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("control limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut data = Bounded(Vec::new());
    serde_json::to_writer(&mut data, value).map_err(|_| Error::Oversized)?;
    tokio::time::timeout(CONTROL_TIMEOUT, async {
        stream
            .write_u32(data.0.len() as u32)
            .await
            .map_err(|_| Error::Network)?;
        stream.write_all(&data.0).await.map_err(|_| Error::Network)
    })
    .await
    .map_err(|_| Error::Timeout)?
}

pub async fn receive<T: DeserializeOwned>(stream: &mut RecvStream) -> Result<T, Error> {
    receive_with_timeout(stream, CONTROL_TIMEOUT).await
}

/// How long a stream may wait for a human's answer when `requested` is asked for: the request, never past the longest wait the
/// settings allow (`MAX_OFFER_TIMEOUT`), which is the one place that number is decided. A wait the person chose is therefore kept
/// whole, and one that was never validated cannot make a stream wait for ever.
pub fn consent_wait(requested: Duration) -> Duration {
    requested.min(Duration::from_secs(u64::from(peersus_core::config::MAX_OFFER_TIMEOUT)))
}

/// Human consent can wait longer than ordinary control traffic, but remains bounded (see [`consent_wait`]).
pub async fn receive_with_timeout<T: DeserializeOwned>(
    stream: &mut RecvStream,
    timeout: Duration,
) -> Result<T, Error> {
    let timeout = consent_wait(timeout);
    tokio::time::timeout(timeout, async {
        let length = stream.read_u32().await.map_err(|_| Error::Network)? as usize;
        if length == 0 || length > MAX_CONTROL {
            return Err(Error::Oversized);
        }
        let mut bytes = vec![0; length];
        stream
            .read_exact(&mut bytes)
            .await
            .map_err(|_| Error::Network)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
    })
    .await
    .map_err(|_| Error::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_the_settings_allow_is_kept_whole_and_nothing_waits_past_the_longest_they_allow() {
        use peersus_core::config::{DEFAULT_OFFER_TIMEOUT, MAX_OFFER_TIMEOUT, MIN_OFFER_TIMEOUT};
        for seconds in [MIN_OFFER_TIMEOUT, DEFAULT_OFFER_TIMEOUT, 600, 3600, MAX_OFFER_TIMEOUT] {
            let asked = Duration::from_secs(u64::from(seconds));
            assert_eq!(consent_wait(asked), asked, "a wait of {seconds} s that the settings allow was cut short");
        }
        let past = Duration::from_secs(u64::from(MAX_OFFER_TIMEOUT) + 1);
        assert_eq!(consent_wait(past), Duration::from_secs(u64::from(MAX_OFFER_TIMEOUT)), "a stream was allowed to wait past the maximum");
        assert_eq!(consent_wait(Duration::MAX), Duration::from_secs(u64::from(MAX_OFFER_TIMEOUT)));
    }

    /// GATE for announcing a deadline on a stream of its own (unidirectional) without changing what version 1 understands: a device
    /// that never accepts a unidirectional stream (every one that exists today) must not be slowed or stopped by one that does open
    /// them, however many, and the exchange it does read (bidirectional, request and reply) must go on as before. Tried both ways,
    /// between two local endpoints, and with as many announcements as offers are allowed on a connection.
    #[tokio::test]
    async fn announcements_on_streams_the_other_side_never_reads_do_not_hold_up_the_exchange_it_does_read() {
        const ANNOUNCEMENTS: usize = 16;
        let policy = NetworkSettings { bind: Some("127.0.0.1".parse().unwrap()), allow_relay: false };
        let a = bind(SecretKey::generate(), &policy).await.unwrap();
        let b = bind(SecretKey::generate(), &policy).await.unwrap();
        let (a_conn, b_conn) = tokio::join!(a.connect(b.addr(), ALPN), async { b.accept().await.unwrap().await });
        let (a_conn, b_conn) = (a_conn.unwrap(), b_conn.unwrap());
        // Each direction in turn: `announcer` opens streams and writes to them and never waits for anything; `reader` is a version-1
        // device that accepts bidirectional streams only, as the listeners of today do, and answers each request.
        for (announcer, reader) in [(&a_conn, &b_conn), (&b_conn, &a_conn)] {
            let announce = async {
                for n in 0..ANNOUNCEMENTS {
                    let mut stream = announcer.open_uni().await.unwrap();
                    send(&mut stream, &format!("deadline-{n}")).await.unwrap();
                    stream.finish().unwrap();
                }
            };
            let exchange = async {
                for n in 0..ANNOUNCEMENTS {
                    let (mut tx, mut rx) = announcer.open_bi().await.unwrap();
                    send(&mut tx, &n).await.unwrap();
                    tx.finish().unwrap();
                    assert_eq!(receive::<usize>(&mut rx).await.unwrap(), n * 2, "the reply to request {n} was not what was asked");
                }
            };
            let read_only_bidirectional = async {
                for _ in 0..ANNOUNCEMENTS {
                    let (mut tx, mut rx) = reader.accept_bi().await.unwrap();
                    let asked: usize = receive(&mut rx).await.unwrap();
                    send(&mut tx, &(asked * 2)).await.unwrap();
                    tx.finish().unwrap();
                }
            };
            tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(announce, exchange, read_only_bidirectional) })
                .await
                .expect("announcements nobody reads held up the exchange that is read");
        }
        a_conn.close(0_u32.into(), b"done");
        b_conn.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn encrypted_local_connection_binds_exporter_and_exchanges_frames() {
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
        };
        let a = bind(SecretKey::generate(), &policy).await.unwrap();
        let b = bind(SecretKey::generate(), &policy).await.unwrap();
        let (outgoing, incoming) = tokio::join!(a.connect(b.addr(), ALPN), async {
            b.accept().await.unwrap().await
        });
        let outgoing = outgoing.unwrap();
        let incoming = incoming.unwrap();
        assert_eq!(outgoing.remote_id(), b.id());
        assert_eq!(incoming.remote_id(), a.id());
        assert_eq!(exporter(&outgoing).unwrap(), exporter(&incoming).unwrap());
        let (client, ()) = tokio::join!(
            async {
                let (mut tx, mut rx) = outgoing.open_bi().await.unwrap();
                send(&mut tx, &vec![1_u8, 3, 5]).await.unwrap();
                tx.finish().unwrap();
                receive::<String>(&mut rx).await.unwrap()
            },
            async {
                let (mut tx, mut rx) = incoming.accept_bi().await.unwrap();
                let message: Vec<u8> = receive(&mut rx).await.unwrap();
                assert_eq!(message, [1, 3, 5]);
                send(&mut tx, &"accepted").await.unwrap();
                tx.finish().unwrap();
            }
        );
        assert_eq!(client, "accepted");
        outgoing.close(0_u32.into(), b"done");
        incoming.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn strict_binding_never_silently_enables_unbound_relay_traffic() {
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: true,
        };
        assert!(matches!(
            bind(SecretKey::generate(), &policy).await,
            Err(Error::RestrictedRelay)
        ));
    }
}
