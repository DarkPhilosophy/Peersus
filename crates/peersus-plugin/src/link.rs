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
/// Most refusals a plugin is owed and could not be handed yet because its own queue was full; past it, it is dropped.
pub const MOST_OWED: usize = 4096;
/// Longest a plugin over its rate is left unread at once. Shorter than any runtime's write timeout (five seconds), so being
/// slowed down never looks to a plugin like the application went away.
pub const MOST_UNREAD: Duration = Duration::from_millis(500);
/// How often owed refusals are looked at while nothing else comes for the plugin.
const OWED_LOOK: Duration = Duration::from_millis(50);
/// Longest a refusal may wait to be handed to a plugin that does not read; past it, the plugin is dropped as stalled.
pub const OWED_LIMIT: Duration = Duration::from_secs(30);

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
    same_bytes(a.as_bytes(), b.as_bytes())
}

pub(crate) fn same_bytes(a: &[u8], b: &[u8]) -> bool {
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
    // Refusals the plugin is owed that its queue had no room for when they were made, in order. A plugin that floods without
    // reading fills its queue with refusals; it is not dropped for that, which is what its rate is there to deal with: they
    // wait here, and the writer below hands them over as the plugin reads. Bounded: past `MOST_OWED` the plugin is dropped.
    let owed = std::sync::Mutex::new(std::collections::VecDeque::<(
        u64,
        peersus_api::Error,
        std::time::Instant,
    )>::new());
    // Hand over what is owed while the queue has room. `Ok(true)` when nothing is left owed; `Err` when the oldest refusal
    // has waited past `OWED_LIMIT`: the plugin does not read what it is sent, and is dropped like any stalled one.
    let settle = || -> Result<bool, Ended> {
        let mut owed = owed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while let Some((id, refusal, since)) = owed.front() {
            if channel.reject(*id, refusal).is_err() {
                return if since.elapsed() > OWED_LIMIT {
                    Err(Ended::Misbehaved)
                } else {
                    Ok(false)
                };
            }
            owed.pop_front();
        }
        Ok(true)
    };
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
                        // Over its rate: what it sends next waits in its own connection until the hub would take one
                        // more, so a flood is slowed down by the extension's own writes and costs the application no
                        // refusal each. Never longer than `MOST_UNREAD` at once: a plugin's runtime gives up on a write
                        // that does not go through for a few seconds and takes it for the application gone, so a
                        // longer pause would end the plugin for what is only a slowdown.
                        let pause = (refusal == peersus_api::Error::RateLimited)
                            .then(|| channel.retry_after())
                            .flatten();
                        // A refusal happens before the call is recorded, so no timeout would ever answer it: the plugin
                        // is told, now if its queue has room, otherwise as soon as it has (see `owed`). A channel that is
                        // gone, a plugin owed more than `MOST_OWED` refusals or one that leaves a refusal unread past
                        // `OWED_LIMIT` ends the connection.
                        if let Some(id) = call_id {
                            let handed = match settle() {
                                Err(ended) => return ended,
                                Ok(settled) => settled && channel.reject(id, &refusal).is_ok(),
                            };
                            if !handed {
                                let mut owed = owed
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                if owed.len() >= MOST_OWED {
                                    return Ended::Misbehaved;
                                }
                                owed.push_back((id, refusal, std::time::Instant::now()));
                            }
                        }
                        // Let the writer below send that answer before the next operation is read.
                        tokio::task::yield_now().await;
                        if let Some(pause) = pause {
                            tokio::time::sleep(pause.min(MOST_UNREAD)).await;
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
            // While refusals are owed, look again now and then even when nothing new comes: what the plugin read made
            // room for them. `receive` loses nothing when the wait is cut short.
            let delivery = match settle() {
                Err(ended) => return ended,
                Ok(true) => channel.receive().await,
                Ok(false) => match tokio::time::timeout(OWED_LOOK, channel.receive()).await {
                    Ok(delivery) => delivery,
                    Err(_) => continue,
                },
            };
            match delivery {
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
            ..Manifest::new(peersus_api::manifest::Execution::Process)
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
            ..Manifest::new(peersus_api::manifest::Execution::Process)
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
            ..Manifest::new(peersus_api::manifest::Execution::Process)
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
    async fn a_refusal_its_queue_has_no_room_for_yet_is_answered_once_it_has_and_the_connection_stays()
     {
        use crate::hub::MAX_QUEUE;
        let hub = Hub::default();
        // `q` really provides `svc` to `p` but never answers anything, so every call `p`
        // makes to it stays pending until its own short deadline.
        let mut quiet = provider();
        quiet.exports[0].consumers = names(&["p"]);
        let q_generation = hub.register("q", quiet, BTreeSet::new()).unwrap();
        let _q = hub.connect("q", q_generation).unwrap();
        let p_generation = hub
            .register("p", calls_a_missing_provider("q"), BTreeSet::new())
            .unwrap();
        let channel = hub.connect("p", p_generation).unwrap();
        // Each accepted call reserves one slot of `p`'s own queue for its eventual result. Once all are taken there is
        // no room for the answer to a refused call until those calls end.
        for id in 1..=MAX_QUEUE as u64 {
            channel
                .submit(Operation::Call {
                    id,
                    provider: "q".into(),
                    service: "svc".into(),
                    version: 1,
                    method: "act".into(),
                    args: json!(null),
                    timeout_ms: 300,
                })
                .unwrap();
        }
        // Precondition, checked rather than assumed: the queue really is out of room.
        assert_eq!(
            channel.reject(9_999, &peersus_api::Error::Denied),
            Err(peersus_api::Error::QueueFull)
        );
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
        frame::write(&mut plugin, &doomed_call(7_777), CONTROL_LIMIT)
            .await
            .unwrap();
        // The pending calls end at their deadline; then the refusal of 7777 has room and arrives. Nothing is lost and the
        // plugin is not dropped for having been refused faster than it read.
        let mut refused = false;
        for _ in 0..=MAX_QUEUE {
            let answer = tokio::time::timeout(
                Duration::from_secs(5),
                frame::read::<Delivery>(&mut plugin, CONTROL_LIMIT),
            )
            .await
            .expect("every call is answered")
            .unwrap();
            if matches!(
                answer,
                Delivery::Result {
                    id: 7_777,
                    result: Outcome::Error { .. }
                }
            ) {
                refused = true;
            }
        }
        assert!(refused, "the refusal owed to the plugin was handed over");
        assert!(!link.is_finished(), "the plugin keeps its connection");
        drop(plugin);
        assert_eq!(link.await.unwrap(), Ended::Closed);
    }
}
