//! Authenticated QUIC transport and bounded, length-delimited control messages.
use futures_util::StreamExt;
use iroh::{
    Endpoint, SecretKey,
    endpoint::{
        AfterHandshakeOutcome, Builder, Connection, EndpointHooks, RecvStream, SendStream, presets,
    },
};
use peersus_core::{NetworkSettings, relay::RelayTarget};
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

/// The access token of a personal relay. It is a secret, so it has no `Serialize`, and `Debug` never shows it: it
/// cannot be written into a settings file, a snapshot or a log by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct RelayAccess(zeroize::Zeroizing<String>);

impl RelayAccess {
    /// `None` for an empty token, because a relay that needs no token must not be sent an empty one.
    pub fn new(token: &str) -> Option<Self> {
        let token = token.trim();
        (!token.is_empty() && token.len() <= 512 && !token.chars().any(char::is_control))
            .then(|| Self(zeroize::Zeroizing::new(token.to_owned())))
    }
}

impl std::fmt::Debug for RelayAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RelayAccess(..)")
    }
}

/// No auxiliary public discovery/relay sockets are created for explicit binding.
/// Unsupported restrictions fail before an endpoint is started.
pub async fn bind(key: SecretKey, policy: &NetworkSettings) -> Result<Endpoint, Error> {
    bind_with(key, policy, None).await
}

/// Bind the first configured relay tier. Relay candidates are not a priority-sorted pool:
/// the live coordinator advances tiers explicitly without recreating this endpoint.
pub async fn bind_with(
    key: SecretKey,
    policy: &NetworkSettings,
    access: Option<&RelayAccess>,
) -> Result<Endpoint, Error> {
    if policy.bind.is_some() && policy.allow_relay {
        return Err(Error::RestrictedRelay);
    }
    let relay_policy = policy.effective_relay_policy();
    if !relay_policy.is_valid() {
        return Err(Error::Invalid);
    }
    let mut builder = if relay_policy.has_enabled_targets() {
        Builder::new(presets::N0)
    } else {
        Builder::new(presets::Minimal)
    }
    .secret_key(key)
    .alpns(vec![ALPN.to_vec(), crate::reconnect::ALPN.to_vec()])
    .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled);
    if relay_policy.has_enabled_targets() {
        builder = builder.hooks(DirectFirst {
            wait: Duration::from_millis(u64::from(relay_policy.direct_wait_ms)),
        });
    }
    if let Some(mode) = relay_mode(policy, access)? {
        builder = builder.relay_mode(mode);
    }
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

/// The relays iroh is given at start. In every mode but `Automatic` that is one tier, the first enabled, and the coordinator moves
/// between tiers itself so that iroh's own latency choice never reorders what the person chose; `Automatic` gives iroh every enabled
/// relay and lets it pick. Personal credentials belong only to their target.
fn relay_mode(
    policy: &NetworkSettings,
    access: Option<&RelayAccess>,
) -> Result<Option<iroh::RelayMode>, Error> {
    let policy = policy.effective_relay_policy();
    let Some((first, _)) = policy.enabled_targets().next() else {
        return Ok(Some(iroh::RelayMode::Disabled));
    };
    let map = if policy.order == peersus_core::relay::RelayOrder::Automatic {
        every_enabled_map(&policy, access)?
    } else {
        target_map(&policy, first, access)?
    };
    Ok(Some(iroh::RelayMode::Custom(map)))
}

/// The map of the target at `index` of `policy`, carrying the stored token only if that target is the one it belongs to.
pub(crate) fn target_map(
    policy: &peersus_core::relay::RelayPolicy,
    index: usize,
    access: Option<&RelayAccess>,
) -> Result<iroh::RelayMap, Error> {
    let access = access.filter(|_| policy.token_target() == Some(index));
    relay_map(&policy.targets[index], access)
}

/// Every enabled relay of `policy` in one map, each with only its own credential.
pub(crate) fn every_enabled_map(
    policy: &peersus_core::relay::RelayPolicy,
    access: Option<&RelayAccess>,
) -> Result<iroh::RelayMap, Error> {
    let map = iroh::RelayMap::empty();
    for (index, _) in policy.enabled_targets() {
        map.extend(&target_map(policy, index, access)?);
    }
    Ok(map)
}

pub(crate) fn relay_map(
    target: &RelayTarget,
    access: Option<&RelayAccess>,
) -> Result<iroh::RelayMap, Error> {
    let Some(url) = target.url() else {
        return Ok(iroh::RelayMode::Default.relay_map());
    };
    let url: iroh::RelayUrl = url.parse().map_err(|_| Error::Invalid)?;
    let mut map = iroh::RelayMap::from(url);
    // `target_map` passes access only for the relay the token belongs to; the public pool never takes one.
    if let Some(access) = access {
        map = map.with_auth_token(access.0.as_str());
    }
    Ok(map)
}

/// Signalling may use a relay to negotiate NAT traversal, but application streams
/// are withheld while a direct path is being established. Both accepted and dialled
/// connections pass this gate, including remembered-peer reconnects.
#[derive(Debug)]
struct DirectFirst {
    wait: Duration,
}

impl EndpointHooks for DirectFirst {
    async fn after_handshake(&self, connection: &Connection) -> AfterHandshakeOutcome {
        if selected_ip(connection) {
            return AfterHandshakeOutcome::Accept;
        }
        let mut paths = connection.paths_stream();
        let direct = tokio::time::timeout(self.wait, async {
            while let Some(paths) = paths.next().await {
                if paths.iter().any(|path| path.is_selected() && path.is_ip()) {
                    return true;
                }
            }
            false
        })
        .await;
        match direct {
            Ok(false) => AfterHandshakeOutcome::Reject {
                error_code: 1_u32.into(),
                reason: b"connection closed during direct negotiation".to_vec(),
            },
            // A usable direct path wins immediately. Only exhausting the bounded
            // opportunity permits encrypted application data on the relay backup.
            Ok(true) | Err(_) => AfterHandshakeOutcome::Accept,
        }
    }
}

pub(crate) fn selected_ip(connection: &Connection) -> bool {
    connection
        .paths()
        .iter()
        .any(|path| path.is_selected() && path.is_ip())
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
    requested.min(Duration::from_secs(u64::from(
        peersus_core::config::MAX_OFFER_TIMEOUT,
    )))
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
        for seconds in [
            MIN_OFFER_TIMEOUT,
            DEFAULT_OFFER_TIMEOUT,
            600,
            3600,
            MAX_OFFER_TIMEOUT,
        ] {
            let asked = Duration::from_secs(u64::from(seconds));
            assert_eq!(
                consent_wait(asked),
                asked,
                "a wait of {seconds} s that the settings allow was cut short"
            );
        }
        let past = Duration::from_secs(u64::from(MAX_OFFER_TIMEOUT) + 1);
        assert_eq!(
            consent_wait(past),
            Duration::from_secs(u64::from(MAX_OFFER_TIMEOUT)),
            "a stream was allowed to wait past the maximum"
        );
        assert_eq!(
            consent_wait(Duration::MAX),
            Duration::from_secs(u64::from(MAX_OFFER_TIMEOUT))
        );
    }

    /// GATE for announcing a deadline on a stream of its own (unidirectional) without changing what version 1 understands: a device
    /// that never accepts a unidirectional stream (every one that exists today) must not be slowed or stopped by one that does open
    /// them, however many, and the exchange it does read (bidirectional, request and reply) must go on as before. Tried both ways,
    /// between two local endpoints, and with as many announcements as offers are allowed on a connection.
    #[tokio::test]
    async fn announcements_on_streams_the_other_side_never_reads_do_not_hold_up_the_exchange_it_does_read()
     {
        const ANNOUNCEMENTS: usize = 16;
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        };
        let a = bind(SecretKey::generate(), &policy).await.unwrap();
        let b = bind(SecretKey::generate(), &policy).await.unwrap();
        let (a_conn, b_conn) = tokio::join!(a.connect(b.addr(), ALPN), async {
            b.accept().await.unwrap().await
        });
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
                    assert_eq!(
                        receive::<usize>(&mut rx).await.unwrap(),
                        n * 2,
                        "the reply to request {n} was not what was asked"
                    );
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
            tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(announce, exchange, read_only_bidirectional)
            })
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
            ..NetworkSettings::default()
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
            ..NetworkSettings::default()
        };
        assert!(matches!(
            bind(SecretKey::generate(), &policy).await,
            Err(Error::RestrictedRelay)
        ));
    }

    #[test]
    fn the_relay_token_never_shows_in_debug_and_an_unusable_one_is_not_made() {
        let access = RelayAccess::new("  s3cret-token  ").expect("a plain token is kept");
        let shown = format!("{access:?} {:?}", Some(&access));
        assert!(!shown.contains("s3cret"), "{shown}");
        for unusable in ["", "   ", "tab\there", "line\nbreak", &"x".repeat(513)] {
            assert!(RelayAccess::new(unusable).is_none(), "{unusable:?}");
        }
        assert_eq!(
            RelayAccess::new("  s3cret-token  "),
            RelayAccess::new("s3cret-token"),
            "surrounding space is not part of it"
        );
    }

    #[tokio::test]
    async fn a_personal_relay_address_that_is_not_usable_is_refused_before_any_socket_is_made() {
        for bad in ["not a url", "", "http://", "https://exa mple.org"] {
            let policy = NetworkSettings {
                relay_url: Some(bad.to_owned()),
                ..NetworkSettings::default()
            };
            assert!(
                matches!(
                    bind_with(SecretKey::generate(), &policy, None).await,
                    Err(Error::Invalid)
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_personal_relay_is_the_only_relay_and_carries_exactly_the_token() {
        let policy = NetworkSettings {
            relay_url: Some("https://relay.example.org".to_owned()),
            ..NetworkSettings::default()
        };
        let access = RelayAccess::new("s3cret-token");
        let mode = relay_mode(&policy, access.as_ref())
            .unwrap()
            .expect("a personal relay was named");
        let map = mode.relay_map();
        assert_eq!(
            map.len(),
            1,
            "nothing public is left beside the personal relay"
        );
        let url: iroh::RelayUrl = "https://relay.example.org".parse().unwrap();
        let config = map
            .get(&url)
            .expect("the relay of the settings is the one in the map");
        assert_eq!(config.auth_token.as_deref(), Some("s3cret-token"));

        // Without a token nothing is sent: a relay that wants none must not be given an empty one.
        let open = relay_mode(&policy, None).unwrap().unwrap().relay_map();
        assert_eq!(open.get(&url).unwrap().auth_token, None);
    }

    #[test]
    fn legacy_public_settings_keep_the_public_pool_without_a_personal_token() {
        let map = relay_mode(
            &NetworkSettings::default(),
            RelayAccess::new("unused").as_ref(),
        )
        .unwrap()
        .unwrap()
        .relay_map();
        assert!(!map.is_empty());
        assert!(
            map.relays::<Vec<_>>()
                .iter()
                .all(|config| config.auth_token.is_none())
        );
    }

    #[tokio::test]
    async fn an_endpoint_with_a_well_formed_personal_relay_starts_without_reaching_it() {
        let policy = NetworkSettings {
            relay_url: Some("https://relay.example.org".to_owned()),
            ..NetworkSettings::default()
        };
        let endpoint = bind_with(SecretKey::generate(), &policy, RelayAccess::new("s3cret-token").as_ref())
            .await
            .expect("a well-formed personal relay is enough to start; it is only reached once there is traffic");
        endpoint.close().await;
    }

    #[tokio::test]
    async fn local_tls_relay_obeys_direct_window_then_cancels_negotiation_on_close() {
        use iroh_relay::{
            server::{CertConfig, RelayConfig, Server, ServerConfig, TlsConfig, testing},
            tls::CaTlsConfig,
        };
        use std::net::Ipv4Addr;
        tokio::time::timeout(Duration::from_secs(20), async {
            let (certificates, tls) = testing::self_signed_tls_certs_and_config();
            let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
            relay.tls = Some(TlsConfig::new(
                (Ipv4Addr::LOCALHOST, 0),
                CertConfig::Manual { server_config: tls },
            ));
            let mut config = ServerConfig::default();
            config.relay = Some(relay);
            let server = Server::spawn(config).await.unwrap();
            let url = server.https_url().unwrap();
            let builder = || {
                Builder::new(presets::Minimal)
                    .secret_key(SecretKey::generate())
                    .alpns(vec![ALPN.to_vec()])
                    .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(url.clone())))
                    .ca_tls_config(CaTlsConfig::custom_roots(certificates.clone()))
                    .clear_ip_transports()
            };
            let window = Duration::from_millis(200);
            let a = builder()
                .hooks(DirectFirst { wait: window })
                .bind()
                .await
                .unwrap();
            let b = builder().bind().await.unwrap();
            tokio::join!(a.online(), b.online());
            let started = tokio::time::Instant::now();
            let (outgoing, incoming) = tokio::join!(a.connect(b.addr(), ALPN), async {
                b.accept().await.unwrap().await
            });
            let (outgoing, incoming) = (outgoing.unwrap(), incoming.unwrap());
            assert!(
                !selected_ip(&outgoing),
                "this scenario must actually use the local relay"
            );
            assert!(
                started.elapsed() >= window,
                "application obtained a relay connection before its direct opportunity elapsed"
            );
            let ((), received) = tokio::join!(
                async {
                    let mut stream = outgoing.open_uni().await.unwrap();
                    stream.write_all(b"encrypted fallback").await.unwrap();
                    stream.finish().unwrap();
                },
                async {
                    incoming
                        .accept_uni()
                        .await
                        .unwrap()
                        .read_to_end(64)
                        .await
                        .unwrap()
                }
            );
            assert_eq!(received, b"encrypted fallback");

            let gate = DirectFirst {
                wait: Duration::from_secs(10),
            };
            let mut negotiation = Box::pin(gate.after_handshake(&outgoing));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut negotiation)
                    .await
                    .is_err()
            );
            outgoing.close(0_u32.into(), b"local test cancellation");
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(1), negotiation)
                    .await
                    .unwrap(),
                AfterHandshakeOutcome::Reject { .. }
            ));
            a.close().await;
            b.close().await;
            server.shutdown().await.unwrap();
        })
        .await
        .expect("local relay direct-window/cancellation scenario exceeded its deadline");
    }
}
