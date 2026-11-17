//! Connects one plugin process to its hub [`Channel`].
//!
//! The plugin proves it is the process the application started by sending the secret
//! token it was given through its environment, before a single message reaches the
//! hub. After that the link only moves frames: deliveries from the hub go to the
//! plugin, operations from the plugin go to the hub. It never inspects a message, so
//! every rule stays in the hub.
use crate::frame::{self, CONTROL_LIMIT};
use crate::hub::Channel;
use peersus_api::message::{Delivery, Operation};
use serde::{Deserialize, Serialize};
use std::{io, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};

/// The plugin has this long to identify itself before the connection is dropped.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// First frame a plugin sends. Anything else, or a wrong token, ends the connection.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub token: String,
}

/// Why a link ended. Distinguishes a plugin that left from one that misbehaved so the
/// supervisor can decide whether a restart makes sense.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// The plugin closed its connection.
    Closed,
    /// The hub revoked this channel (plugin replaced or removed).
    Revoked,
    /// The plugin sent an invalid frame or never identified itself.
    Misbehaved,
}

/// Compare without leaking how many leading bytes matched.
pub(crate) fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut difference = a.len() ^ b.len();
    for (x, y) in a.iter().zip(b) {
        difference |= usize::from(x ^ y);
    }
    difference == 0
}

/// Run the link until either side ends it. The caller owns `channel`, so dropping this
/// future disconnects the plugin from the hub.
pub async fn run(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    channel: Channel,
    token: &str,
) -> Ended {
    run_with(stream, channel, token, || {}).await
}

/// Like [`run`], and calls `on_connected` once, immediately after the plugin has proved it
/// is the process that was started. It is not called for a bare connection, a wrong token
/// or a silent peer, so it is a reliable sign that the hub can now reach the plugin.
pub async fn run_with(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    channel: Channel,
    token: &str,
    on_connected: impl FnOnce(),
) -> Ended {
    let hello = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        frame::read::<Hello>(&mut stream, CONTROL_LIMIT),
    )
    .await;
    match hello {
        Ok(Ok(hello)) if same(&hello.token, token) => on_connected(),
        _ => return Ended::Misbehaved,
    }
    let (mut reader, mut writer) = tokio::io::split(stream);
    let inbound = async {
        loop {
            match frame::read::<Operation>(&mut reader, CONTROL_LIMIT).await {
                Ok(operation) => {
                    // Read the call id first: submitting moves the operation, and a
                    // refused call has to be answered under the id the plugin chose.
                    let call_id = match &operation {
                        Operation::Call { id, .. } => Some(*id),
                        _ => None,
                    };
                    if let Err(refusal) = channel.submit(operation) {
                        // A refusal happens before the call is recorded, so no timeout
                        // would ever answer it. Tell the plugin now. If even that cannot
                        // be delivered (its queue is full or its channel is gone) the
                        // connection is dropped, because a plugin left waiting on an
                        // answer that cannot come is worse than a closed connection.
                        if let Some(id) = call_id
                            && channel.reject(id, &refusal).is_err()
                        {
                            return Ended::Misbehaved;
                        }
                        // Let the writer below send that answer before the next operation is
                        // read. Without this an extension whose calls are refused faster than
                        // they are answered fills its own queue and loses its connection for
                        // what its rate is there to deal with.
                        tokio::task::yield_now().await;
                        // Over its rate: what it sends next waits in its own connection until
                        // the hub would take one more, so a flood is slowed down by the
                        // extension's own writes and costs the application no refusal each.
                        if refusal == peersus_api::Error::RateLimited
                            && let Some(pause) = channel.retry_after()
                        {
                            tokio::time::sleep(pause).await;
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    return Ended::Closed;
                }
                Err(_) => return Ended::Misbehaved,
            }
        }
    };
    let outbound = async {
        loop {
            match channel.receive().await {
                Ok(delivery) => {
                    if frame::write::<Delivery>(&mut writer, &delivery, CONTROL_LIMIT)
                        .await
                        .is_err()
                    {
                        return Ended::Closed;
                    }
                }
                Err(_) => return Ended::Revoked,
            }
        }
    };
    tokio::select! {
        ended = inbound => ended,
        ended = outbound => ended,
    }
}

#[cfg(test)]
mod tests {
    /// The bootstrap token the host expects in these tests, and one it does not.
    const TOKEN: &str = "secret";
    const WRONG_TOKEN: &str = "guess";
    use super::*;
    use crate::hub::Hub;
    use peersus_api::{
        manifest::{Export, Import, Manifest},
        message::Outcome,
    };
    use serde_json::json;
    use std::collections::BTreeSet;
    use tokio::io::duplex;

    fn names(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|s| (*s).to_owned()).collect()
    }
    fn provider() -> Manifest {
        Manifest {
            exports: vec![Export {
                service: "svc".into(),
                version: 1,
                description: "A test service.".into(),
                consumers: names(&["a"]),
                methods: [(
                    "act".into(),
                    peersus_api::manifest::Member {
                        capability: None,
                        description: "Does the act.".into(),
                    },
                )]
                .into(),
                events: Default::default(),
            }],
            ..Manifest::default()
        }
    }
    fn consumer() -> Manifest {
        Manifest {
            imports: vec![Import {
                provider: "b".into(),
                service: "svc".into(),
                version: 1,
                methods: names(&["act"]),
                events: BTreeSet::new(),
            }],
            ..Manifest::default()
        }
    }

    #[test]
    fn token_comparison_is_exact() {
        assert!(same("abc", "abc"));
        for wrong in ["abd", "ab", "abcd", "", "ABC"] {
            assert!(!same(wrong, "abc"), "{wrong}");
        }
    }

    #[tokio::test]
    async fn a_wrong_token_never_reaches_the_hub() {
        let hub = Hub::default();
        let generation = hub.register("b", provider(), BTreeSet::new()).unwrap();
        let channel = hub.connect("b", generation).unwrap();
        let (mut plugin, host) = duplex(4096);
        let task = tokio::spawn(run(host, channel, TOKEN));
        frame::write(
            &mut plugin,
            &Hello {
                token: WRONG_TOKEN.into(),
            },
            1024,
        )
        .await
        .unwrap();
        assert_eq!(task.await.unwrap(), Ended::Misbehaved);
    }

    #[tokio::test]
    async fn a_silent_connection_is_dropped_after_the_handshake_deadline() {
        tokio::time::pause();
        let hub = Hub::default();
        let generation = hub.register("b", provider(), BTreeSet::new()).unwrap();
        let channel = hub.connect("b", generation).unwrap();
        let (_plugin, host) = duplex(4096);
        let task = tokio::spawn(run(host, channel, TOKEN));
        tokio::time::advance(HANDSHAKE_TIMEOUT + Duration::from_millis(1)).await;
        assert_eq!(task.await.unwrap(), Ended::Misbehaved);
    }

    #[tokio::test]
    async fn a_plugin_process_answers_a_call_routed_by_the_hub() {
        let hub = Hub::default();
        let b_gen = hub.register("b", provider(), BTreeSet::new()).unwrap();
        let a_gen = hub.register("a", consumer(), BTreeSet::new()).unwrap();
        let b_channel = hub.connect("b", b_gen).unwrap();
        let a_channel = hub.connect("a", a_gen).unwrap();
        // `b` is a real "process": it is only reachable through its framed stream.
        let (mut plugin, host) = duplex(65536);
        let link = tokio::spawn(run(host, b_channel, TOKEN));
        frame::write(
            &mut plugin,
            &Hello {
                token: TOKEN.into(),
            },
            1024,
        )
        .await
        .unwrap();
        a_channel
            .submit(Operation::Call {
                id: 1,
                provider: "b".into(),
                service: "svc".into(),
                version: 1,
                method: "act".into(),
                args: json!({"n": 2}),
                timeout_ms: 5000,
            })
            .unwrap();
        let Delivery::Call {
            ticket,
            caller,
            args,
            ..
        } = frame::read::<Delivery>(&mut plugin, CONTROL_LIMIT)
            .await
            .unwrap()
        else {
            panic!("plugin did not receive the call")
        };
        assert_eq!(caller, "a");
        assert_eq!(args, json!({"n": 2}));
        frame::write(
            &mut plugin,
            &Operation::Reply {
                ticket,
                result: Outcome::ok(json!(4)),
            },
            CONTROL_LIMIT,
        )
        .await
        .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), a_channel.receive())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result,
            Delivery::Result {
                id: 1,
                result: Outcome::ok(json!(4))
            }
        );
        drop(plugin);
        assert_eq!(link.await.unwrap(), Ended::Closed);
    }

    #[tokio::test]
    async fn a_plugin_flooding_garbage_ends_only_its_own_link() {
        let hub = Hub::default();
        let b_gen = hub.register("b", provider(), BTreeSet::new()).unwrap();
        let a_gen = hub.register("a", consumer(), BTreeSet::new()).unwrap();
        let b_channel = hub.connect("b", b_gen).unwrap();
        let a_channel = hub.connect("a", a_gen).unwrap();
        let (mut plugin, host) = duplex(65536);
        let link = tokio::spawn(run(host, b_channel, TOKEN));
        frame::write(
            &mut plugin,
            &Hello {
                token: TOKEN.into(),
            },
            1024,
        )
        .await
        .unwrap();
        tokio::io::AsyncWriteExt::write_u32(&mut plugin, u32::MAX)
            .await
            .unwrap();
        assert_eq!(link.await.unwrap(), Ended::Misbehaved);
        // The hub and every other participant are untouched by it.
        assert!(a_channel.try_recv().unwrap().is_none());
    }

    #[tokio::test]
    async fn revoking_the_plugin_ends_its_link() {
        let hub = Hub::default();
        let generation = hub.register("b", provider(), BTreeSet::new()).unwrap();
        let channel = hub.connect("b", generation).unwrap();
        let (mut plugin, host) = duplex(4096);
        let link = tokio::spawn(run(host, channel, TOKEN));
        frame::write(
            &mut plugin,
            &Hello {
                token: TOKEN.into(),
            },
            1024,
        )
        .await
        .unwrap();
        hub.revoke("b");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), link)
                .await
                .unwrap()
                .unwrap(),
            Ended::Revoked
        );
    }

    /// Counts how many times the link reported a successful handshake.
    fn counter() -> (
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        impl FnOnce() + Send + 'static,
    ) {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        (count, move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    }
    fn seen(count: &std::sync::atomic::AtomicUsize) -> usize {
        count.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test]
    async fn the_connected_signal_fires_after_a_correct_handshake_and_not_before() {
        let hub = Hub::default();
        let generation = hub.register("b", provider(), BTreeSet::new()).unwrap();
        let channel = hub.connect("b", generation).unwrap();
        let (mut plugin, host) = duplex(4096);
        let (count, signal) = counter();
        let link = tokio::spawn(run_with(host, channel, TOKEN, signal));
        // The plugin is merely connected: it has not yet proved who it is.
        tokio::task::yield_now().await;
        assert_eq!(seen(&count), 0, "a bare connection is not a handshake");
        frame::write(
            &mut plugin,
            &Hello {
                token: TOKEN.into(),
            },
            1024,
        )
        .await
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while seen(&count) == 0 && std::time::Instant::now() < deadline {
            tokio::task::yield_now().await;
        }
        assert_eq!(seen(&count), 1);
        drop(plugin);
        assert_eq!(link.await.unwrap(), Ended::Closed);
        assert_eq!(seen(&count), 1, "ending the link does not signal again");
    }

    #[tokio::test]
    async fn a_wrong_token_never_raises_the_connected_signal() {
        let hub = Hub::default();
        let generation = hub.register("b", provider(), BTreeSet::new()).unwrap();
        let channel = hub.connect("b", generation).unwrap();
        let (mut plugin, host) = duplex(4096);
        let (count, signal) = counter();
        let link = tokio::spawn(run_with(host, channel, TOKEN, signal));
        frame::write(
            &mut plugin,
            &Hello {
                token: WRONG_TOKEN.into(),
            },
            1024,
        )
        .await
        .unwrap();
        assert_eq!(link.await.unwrap(), Ended::Misbehaved);
        assert_eq!(
            seen(&count),
            0,
            "a stranger must not make a plugin look usable"
        );
    }

    #[tokio::test]
    async fn a_silent_connection_never_raises_the_connected_signal() {
        tokio::time::pause();
        let hub = Hub::default();
        let generation = hub.register("b", provider(), BTreeSet::new()).unwrap();
        let channel = hub.connect("b", generation).unwrap();
        let (_plugin, host) = duplex(4096);
        let (count, signal) = counter();
        let link = tokio::spawn(run_with(host, channel, TOKEN, signal));
        tokio::time::advance(HANDSHAKE_TIMEOUT + Duration::from_millis(1)).await;
        assert_eq!(link.await.unwrap(), Ended::Misbehaved);
        assert_eq!(seen(&count), 0);
    }

    /// A manifest that imports `svc` from `provider_name`, which nobody provides.
    fn calls_a_missing_provider(provider_name: &str) -> Manifest {
        Manifest {
            imports: vec![Import {
                provider: provider_name.into(),
                service: "svc".into(),
                version: 1,
                methods: names(&["act"]),
                events: BTreeSet::new(),
            }],
            ..Manifest::default()
        }
    }
    fn doomed_call(id: u64) -> Operation {
        Operation::Call {
            id,
            provider: "nobody".into(),
            service: "svc".into(),
            version: 1,
            method: "act".into(),
            args: json!(null),
            timeout_ms: 25_000,
        }
    }

    #[tokio::test]
    async fn a_refused_call_is_answered_on_the_wire_under_the_plugins_own_id() {
        let hub = Hub::default();
        let generation = hub
            .register("p", calls_a_missing_provider("nobody"), BTreeSet::new())
            .unwrap();
        let channel = hub.connect("p", generation).unwrap();
        let (mut plugin, host) = duplex(65536);
        let link = tokio::spawn(run(host, channel, TOKEN));
        frame::write(
            &mut plugin,
            &Hello {
                token: TOKEN.into(),
            },
            1024,
        )
        .await
        .unwrap();
        frame::write(&mut plugin, &doomed_call(41), CONTROL_LIMIT)
            .await
            .unwrap();
        // The call was refused before the hub ever recorded it, so no timeout covers it.
        // The plugin must still hear back, and under the id it chose, within moments, not
        // after the 60 s it asked to wait.
        let answer = tokio::time::timeout(
            Duration::from_secs(2),
            frame::read::<Delivery>(&mut plugin, CONTROL_LIMIT),
        )
        .await
        .expect("a refused call must be answered, not left waiting")
        .unwrap();
        assert!(matches!(
            answer,
            Delivery::Result { id: 41, result: Outcome::Error { code, .. } } if code == "unavailable"
        ));
        // The connection stays usable: one refused call is not a fatal mistake.
        frame::write(&mut plugin, &doomed_call(42), CONTROL_LIMIT)
            .await
            .unwrap();
        let second = frame::read::<Delivery>(&mut plugin, CONTROL_LIMIT)
            .await
            .unwrap();
        assert!(matches!(second, Delivery::Result { id: 42, .. }));
        drop(plugin);
        assert_eq!(link.await.unwrap(), Ended::Closed);
    }

    #[tokio::test]
    async fn an_extension_over_its_rate_is_told_so_and_keeps_its_connection() {
        use crate::hub::BURST;
        let hub = Hub::default();
        let generation = hub
            .register("p", calls_a_missing_provider("nobody"), BTreeSet::new())
            .unwrap();
        let channel = hub.connect("p", generation).unwrap();
        let (mut plugin, host) = duplex(1 << 20);
        let link = tokio::spawn(run(host, channel, TOKEN));
        frame::write(
            &mut plugin,
            &Hello {
                token: TOKEN.into(),
            },
            1024,
        )
        .await
        .unwrap();
        // More at once than a burst allows. Every call is answered, in order; the ones over
        // the rate say so, and are not mistaken for anything else.
        let sent = u64::from(BURST) * 2;
        for id in 1..=sent {
            frame::write(&mut plugin, &doomed_call(id), CONTROL_LIMIT)
                .await
                .unwrap();
        }
        let mut limited = 0;
        for id in 1..=sent {
            let answer = tokio::time::timeout(
                Duration::from_secs(5),
                frame::read::<Delivery>(&mut plugin, CONTROL_LIMIT),
            )
            .await
            .expect("every call is answered")
            .unwrap();
            let Delivery::Result {
                id: answered,
                result: Outcome::Error { code, .. },
            } = answer
            else {
                panic!("expected a refusal, got {answer:?}")
            };
            assert_eq!(answered, id);
            if code == "rate-limited" {
                limited += 1;
            } else {
                assert_eq!(code, "unavailable");
            }
        }
        assert!(limited > 0, "a burst twice the allowance must be held back");
        assert!(
            limited < sent - u64::from(BURST) + 1,
            "the whole allowance was let through"
        );
        // The connection is whole, and after a moment the extension is served as before.
        tokio::time::sleep(Duration::from_millis(50)).await;
        frame::write(&mut plugin, &doomed_call(sent + 1), CONTROL_LIMIT)
            .await
            .unwrap();
        let after = frame::read::<Delivery>(&mut plugin, CONTROL_LIMIT)
            .await
            .unwrap();
        assert!(matches!(
            after,
            Delivery::Result { result: Outcome::Error { code, .. }, .. } if code == "unavailable"
        ));
        drop(plugin);
        assert_eq!(link.await.unwrap(), Ended::Closed);
    }

    #[tokio::test]
    async fn a_refusal_that_cannot_be_delivered_drops_the_connection() {
        use crate::hub::MAX_QUEUE;
        let hub = Hub::default();
        // `q` really provides `svc` to `p` but never answers anything, so every call `p`
        // makes to it stays pending for as long as the test runs.
        let mut quiet = provider();
        quiet.exports[0].consumers = names(&["p"]);
        let q_generation = hub.register("q", quiet, BTreeSet::new()).unwrap();
        let _q = hub.connect("q", q_generation).unwrap();
        let p_generation = hub
            .register("p", calls_a_missing_provider("q"), BTreeSet::new())
            .unwrap();
        let channel = hub.connect("p", p_generation).unwrap();
        // Each accepted call reserves one slot of `p`'s own queue for its eventual
        // result. A reserved slot is a promise, not a delivery, so the link's writer has
        // nothing to drain: no scheduling order can free it. Once all are taken there is
        // deterministically no room left for the answer to a refused call.
        for id in 1..=MAX_QUEUE as u64 {
            channel
                .submit(Operation::Call {
                    id,
                    provider: "q".into(),
                    service: "svc".into(),
                    version: 1,
                    method: "act".into(),
                    args: json!(null),
                    timeout_ms: 25_000,
                })
                .unwrap();
        }
        // Precondition, checked rather than assumed: the queue really is out of room.
        assert_eq!(
            channel.reject(9_999, &peersus_api::Error::Denied),
            Err(peersus_api::Error::QueueFull)
        );
        let (mut plugin, host) = duplex(65536);
        let link = tokio::spawn(run(host, channel, TOKEN));
        frame::write(
            &mut plugin,
            &Hello {
                token: TOKEN.into(),
            },
            1024,
        )
        .await
        .unwrap();
        frame::write(&mut plugin, &doomed_call(7), CONTROL_LIMIT)
            .await
            .unwrap();
        // The answer has nowhere to go, so the link must end rather than leave the plugin
        // waiting for a reply that can never arrive.
        let ended = tokio::time::timeout(Duration::from_secs(2), link)
            .await
            .expect("an undeliverable refusal must end the link")
            .unwrap();
        assert_eq!(ended, Ended::Misbehaved);
    }
}
