//! The data connection of a plugin: where bulk bytes travel, bound to one accepted call.
//!
//! Control messages are small and carry no content. A plugin that is handed a large item,
//! such as an image or a file, asks for a data connection instead, naming the call it is
//! answering. The connection is authenticated twice over, with different things:
//!
//! * the plugin's secret token, which only the supervised process has, proves WHO is
//!   connecting. A ticket alone could not: tickets are small sequential numbers that any
//!   local process can try.
//! * the ticket, resolved by the hub, proves WHICH accepted call it belongs to and that this
//!   plugin is the provider of it. The hub applies the same imports, allowlists and grants
//!   that admitted the call, so no second authority exists to drift from the first.
//!
//! The token is checked before the hub is asked anything, so a stranger learns nothing
//! about which tickets exist.
use crate::hub::{Channel, DataRight};
use crate::{frame, link};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

pub mod canvas;

/// How long a data connection has to identify itself before it is dropped: as long as a control connection has, and one value.
pub const HANDSHAKE_TIMEOUT: Duration = crate::link::HANDSHAKE_TIMEOUT;

/// Largest handshake frame. It holds a token and a number, so it is tiny.
const HELLO_LIMIT: usize = 1024;
/// First frame of a data connection.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataHello {
    pub token: String,
    /// The hub's ticket for a call this plugin is answering. Unused (zero) when claiming.
    pub ticket: u64,
    /// A number the application issued, when the plugin collects something the application
    /// set aside for it instead of answering a call. Absent in every older plugin.
    /// For an outbound send, half-close the payload's write side and then read one
    /// length-prefixed JSON `u64` (maximum frame: 1024 bytes). The application writes
    /// the delivered byte count only after its remote pump finishes successfully.
    /// Missing, mismatched or late confirmation is not successful delivery.
    #[serde(default)]
    pub claim: Option<u64>,
}

/// Why a data connection was refused. Deliberately coarse: it distinguishes nothing a
/// stranger could use to learn which tickets exist or why one failed.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// No valid handshake arrived in time, or the token was wrong.
    Unauthenticated,
    /// The token was right but the hub gave no right for that ticket.
    NoRight,
}

/// What a genuine data connection is entitled to name.
#[derive(Debug)]
pub enum Right {
    /// The hub's right to the data of a call this plugin is answering.
    Hub(DataRight),
    /// A number the application issued for something it set aside for this plugin. The host
    /// does not know what it stands for; the application checks it against its own record.
    Claim(u64),
}

/// Read the handshake from a new data connection and, if it is genuine, claim the right.
///
/// `provider` is the channel of the plugin whose process this connection must belong to:
/// the hub only grants the right to the provider of the call, so a plugin that names the
/// ticket of a call delivered to a different plugin gets nothing.
pub async fn accept(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    provider: &Channel,
    token: &str,
) -> Result<Right, Refused> {
    let hello = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        frame::read::<DataHello>(stream, HELLO_LIMIT),
    )
    .await;
    let Ok(Ok(hello)) = hello else {
        return Err(Refused::Unauthenticated);
    };
    // The token first, so the ticket of a connection that cannot prove who it is is never
    // even looked up.
    if !link::same(&hello.token, token) {
        return Err(Refused::Unauthenticated);
    }
    // A claim is not the hub's to judge: it names something the application set aside for
    // this plugin. Presenting it only needed the token above, which proved who is asking,
    // and the application decides whether that plugin may have what it names. The hub's own
    // right for a ticket is neither looked up nor used up by a claim.
    if let Some(claim) = hello.claim {
        return Ok(Right::Claim(claim));
    }
    provider
        .open_data(hello.ticket)
        .map(Right::Hub)
        .map_err(|_| Refused::NoRight)
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
        message::{Delivery, Operation, Outcome},
    };
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

    /// `a` has an accepted call in flight to `b`; returns what a data connection needs.
    fn setup() -> (Hub, Channel, Channel, u64) {
        let hub = Hub::default();
        let b = hub.register("b", provider(), BTreeSet::new()).unwrap();
        let a = hub.register("a", consumer(), BTreeSet::new()).unwrap();
        let b = hub.connect("b", b).unwrap();
        let a = hub.connect("a", a).unwrap();
        a.submit(Operation::Call {
            id: 9,
            provider: "b".into(),
            service: "svc".into(),
            version: 1,
            method: "act".into(),
            args: serde_json::json!(null),
            timeout_ms: 5_000,
        })
        .unwrap();
        let Some(Delivery::Call { ticket, .. }) = b.try_recv().unwrap() else {
            panic!("the provider got no call")
        };
        (hub, a, b, ticket)
    }

    async fn hello(stream: &mut (impl AsyncWrite + Unpin), token: &str, ticket: u64) {
        let hello = DataHello {
            token: token.into(),
            ticket,
            claim: None,
        };
        frame::write(stream, &hello, HELLO_LIMIT).await.unwrap();
    }

    async fn hello_claiming(stream: &mut (impl AsyncWrite + Unpin), token: &str, claim: u64) {
        let hello = DataHello {
            token: token.into(),
            ticket: 0,
            claim: Some(claim),
        };
        frame::write(stream, &hello, HELLO_LIMIT).await.unwrap();
    }

    #[tokio::test]
    async fn the_right_token_and_the_ticket_of_its_own_call_yield_the_right() {
        let (_hub, _a, b, ticket) = setup();
        let (mut plugin, mut host) = duplex(4096);
        hello(&mut plugin, TOKEN, ticket).await;
        let Right::Hub(right) = accept(&mut host, &b, TOKEN).await.unwrap() else {
            panic!("a ticket from the hub must give the hub's right")
        };
        assert_eq!(right.ticket(), ticket);
        assert_eq!(right.caller(), "a");
        assert_eq!(right.call_id(), 9);
    }

    #[tokio::test]
    async fn a_wrong_token_is_refused_before_the_ticket_is_looked_up() {
        let (_hub, _a, b, ticket) = setup();
        let (mut plugin, mut host) = duplex(4096);
        hello(&mut plugin, WRONG_TOKEN, ticket).await;
        assert_eq!(
            accept(&mut host, &b, TOKEN).await.err(),
            Some(Refused::Unauthenticated)
        );
        // The refusal must not have used up the right: the real plugin can still claim it.
        let (mut plugin, mut host) = duplex(4096);
        hello(&mut plugin, TOKEN, ticket).await;
        assert!(accept(&mut host, &b, TOKEN).await.is_ok());
    }

    #[tokio::test]
    async fn the_ticket_of_a_call_delivered_to_another_plugin_gives_nothing() {
        let (hub, _a, _b, ticket) = setup();
        // A second plugin, with a perfectly valid token of its own, names the ticket of a
        // call that was delivered to `b`, not to it.
        let other = hub.register("other", provider(), BTreeSet::new()).unwrap();
        let other = hub.connect("other", other).unwrap();
        let (mut plugin, mut host) = duplex(4096);
        hello(&mut plugin, "other-secret", ticket).await;
        assert_eq!(
            accept(&mut host, &other, "other-secret").await.err(),
            Some(Refused::NoRight)
        );
    }

    #[tokio::test]
    async fn a_ticket_nobody_was_given_and_an_ended_call_give_nothing() {
        let (_hub, _a, b, ticket) = setup();
        let (mut plugin, mut host) = duplex(4096);
        hello(&mut plugin, TOKEN, ticket + 500).await;
        assert_eq!(
            accept(&mut host, &b, TOKEN).await.err(),
            Some(Refused::NoRight)
        );
        // Once the call is answered its right is gone with it.
        b.submit(Operation::Reply {
            ticket,
            result: Outcome::ok(serde_json::json!(1)),
        })
        .unwrap();
        let (mut plugin, mut host) = duplex(4096);
        hello(&mut plugin, TOKEN, ticket).await;
        assert_eq!(
            accept(&mut host, &b, TOKEN).await.err(),
            Some(Refused::NoRight)
        );
    }

    #[tokio::test]
    async fn the_right_can_be_claimed_once_even_by_the_genuine_plugin() {
        let (_hub, _a, b, ticket) = setup();
        let (mut first, mut host_one) = duplex(4096);
        hello(&mut first, TOKEN, ticket).await;
        assert!(accept(&mut host_one, &b, TOKEN).await.is_ok());
        let (mut second, mut host_two) = duplex(4096);
        hello(&mut second, TOKEN, ticket).await;
        assert_eq!(
            accept(&mut host_two, &b, TOKEN).await.err(),
            Some(Refused::NoRight)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_connection_is_dropped_at_the_handshake_deadline() {
        let (_hub, _a, b, _ticket) = setup();
        let (_plugin, mut host) = duplex(4096);
        let started = tokio::time::Instant::now();
        assert_eq!(
            accept(&mut host, &b, TOKEN).await.err(),
            Some(Refused::Unauthenticated)
        );
        assert_eq!(started.elapsed(), HANDSHAKE_TIMEOUT);
    }

    #[tokio::test]
    async fn an_oversized_or_malformed_handshake_is_refused_without_allocating_it() {
        let (_hub, _a, b, _ticket) = setup();
        let (mut plugin, mut host) = duplex(4096);
        tokio::io::AsyncWriteExt::write_u32(&mut plugin, u32::MAX)
            .await
            .unwrap();
        assert_eq!(
            accept(&mut host, &b, TOKEN).await.err(),
            Some(Refused::Unauthenticated)
        );
        let (mut plugin, mut host) = duplex(4096);
        tokio::io::AsyncWriteExt::write_u32(&mut plugin, 3)
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut plugin, b"{{{")
            .await
            .unwrap();
        assert_eq!(
            accept(&mut host, &b, TOKEN).await.err(),
            Some(Refused::Unauthenticated)
        );
    }

    #[tokio::test]
    async fn a_claim_with_the_right_token_is_passed_on_for_the_application_to_check() {
        let (_hub, _a, b, _ticket) = setup();
        let (mut plugin, mut host) = duplex(4096);
        hello_claiming(&mut plugin, TOKEN, 424_242).await;
        // The host does not know what a claim means; it only proves who is presenting it.
        assert!(matches!(
            accept(&mut host, &b, TOKEN).await,
            Ok(Right::Claim(424_242))
        ));
    }

    #[tokio::test]
    async fn a_claim_without_the_plugins_token_is_never_passed_on() {
        let (_hub, _a, b, _ticket) = setup();
        let (mut plugin, mut host) = duplex(4096);
        hello_claiming(&mut plugin, WRONG_TOKEN, 424_242).await;
        assert!(matches!(
            accept(&mut host, &b, TOKEN).await,
            Err(Refused::Unauthenticated)
        ));
    }

    #[tokio::test]
    async fn presenting_a_claim_does_not_touch_the_hubs_rights() {
        let (_hub, _a, b, ticket) = setup();
        // One frame naming BOTH the hub's real ticket and a claim: only the claim may count.
        let (mut plugin, mut host) = duplex(4096);
        let both = DataHello {
            token: TOKEN.into(),
            ticket,
            claim: Some(7),
        };
        frame::write(&mut plugin, &both, HELLO_LIMIT).await.unwrap();
        assert!(matches!(
            accept(&mut host, &b, TOKEN).await,
            Ok(Right::Claim(7))
        ));
        // The hub's own right for that very ticket was neither used nor consumed.
        let (mut plugin, mut host) = duplex(4096);
        hello(&mut plugin, TOKEN, ticket).await;
        assert!(
            matches!(accept(&mut host, &b, TOKEN).await, Ok(Right::Hub(_))),
            "presenting a claim spent the hub's right for the ticket named beside it"
        );
    }

    #[tokio::test]
    async fn a_plugin_that_never_sends_a_claim_field_behaves_exactly_as_before() {
        // An older plugin knows only `token` and `ticket`.
        let (_hub, _a, b, ticket) = setup();
        let (mut plugin, mut host) = duplex(4096);
        let old = serde_json::json!({ "token": TOKEN, "ticket": ticket });
        frame::write(&mut plugin, &old, HELLO_LIMIT).await.unwrap();
        assert!(matches!(
            accept(&mut host, &b, TOKEN).await,
            Ok(Right::Hub(_))
        ));
    }
}
