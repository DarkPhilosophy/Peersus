//! The receiving device telling the sending one how long it will wait for its person to answer an offer.
//!
//! WHY A STREAM OF ITS OWN. What crosses on the stream an offer is made on (`Offer`, `Reply`) is read by every version there is, and
//! those versions refuse what they do not know: a new word there would end an offer made to or by an older device. So the deadline
//! is told on a unidirectional stream that the receiver opens beside it, which a device that never accepts such a stream (every
//! one that exists) does not see at all (see the gate test in `transport`). A device that does not hear it keeps to the wait it
//! has of its own; nothing depends on it arriving.
//!
//! WHAT IS TRUSTED. Nothing: it is bounded in size and in value, and it counts only if it names the offer it is about. A deadline
//! outside what the settings allow is not believed, so that the other device cannot make this one wait for ever, or give up at once.

use crate::transport;
use iroh::endpoint::Connection;
use peersus_core::config::{MAX_OFFER_TIMEOUT, MIN_OFFER_TIMEOUT};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The longest the announcement of a deadline may take to arrive: it is a courtesy, and never holds up anything else.
pub const ARRIVAL: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Announce {
    /// The offer this is about, by the id its sender gave it.
    pub offer: String,
    /// How long, in seconds from the moment the offer was shown, the receiving device waits for its person to answer.
    pub seconds: u32,
}

/// The wait an announcement tells, if it is one the settings allow; otherwise nothing. Which offer it is about is judged where it
/// is delivered (see [`Announcements`]).
pub fn allowed(announce: &Announce) -> Option<Duration> {
    (MIN_OFFER_TIMEOUT..=MAX_OFFER_TIMEOUT)
        .contains(&announce.seconds)
        .then(|| Duration::from_secs(u64::from(announce.seconds)))
}

/// Tell the other device, on a stream of its own, that this one waits `seconds` for `offer`. Best effort: a device that does not
/// read it, a connection that is gone, a stream that cannot be opened, all end here without a word, and the offer goes on.
pub async fn tell(connection: &Connection, offer: &str, seconds: u32) {
    let Ok(mut stream) = connection.open_uni().await else { return };
    let announce = Announce { offer: offer.to_owned(), seconds };
    if transport::send(&mut stream, &announce).await.is_ok() {
        let _ = stream.finish();
    }
}

/// The most announcements held for offers nobody has asked about yet, and the most waiting to be asked about at once. A bound, so
/// that a device that announces without end cannot make this one keep without end.
const MOST_HELD: usize = 64;

#[derive(Default)]
struct Book {
    /// Offers that are waiting to hear their deadline, by offer id.
    asking: std::collections::BTreeMap<String, tokio::sync::oneshot::Sender<Duration>>,
    /// Announcements that came before their offer asked: the sender may well announce first. Kept until asked, up to the bound.
    early: std::collections::BTreeMap<String, Duration>,
}

/// The one reader of announcements on a connection. Every outgoing offer on that connection asks it for ITS deadline by the offer's
/// id, and it hands each announcement to the offer it names, whatever order they come in, so that offers made at the same time do
/// not take each other's announcements. Nothing waits for it: an offer that is never announced simply keeps its own wait.
#[derive(Clone, Default)]
pub struct Announcements {
    book: std::sync::Arc<std::sync::Mutex<Book>>,
}

impl Announcements {
    /// Start the reader on `connection`, once for it. It ends when the connection does. For one reader per connection shared by every
    /// offer on it, use [`Readers`].
    pub fn listen(connection: &Connection) -> Self {
        let announcements = Self::default();
        let reader = announcements.clone();
        let connection = connection.clone();
        tokio::spawn(async move { reader.read_until_closed(&connection).await });
        announcements
    }

    /// Read the announcements the other device opens streams for, and hand each to the offer it names, until the connection ends.
    async fn read_until_closed(&self, connection: &Connection) {
        while let Ok(mut stream) = connection.accept_uni().await {
            let reader = self.clone();
            // Each stream is read on its own task and in bounded time, so that one that says nothing holds up no other.
            tokio::spawn(async move {
                let heard = tokio::time::timeout(ARRIVAL, transport::receive::<Announce>(&mut stream)).await;
                if let Ok(Ok(announce)) = heard
                    && let Some(wait) = allowed(&announce)
                {
                    reader.deliver(announce.offer, wait);
                }
            });
        }
    }

    /// An announcement arrived: to the offer asking for it, or held until one does.
    fn deliver(&self, offer: String, wait: Duration) {
        let Ok(mut book) = self.book.lock() else { return };
        if let Some(asking) = book.asking.remove(&offer) {
            let _ = asking.send(wait);
        } else if book.early.len() < MOST_HELD {
            book.early.insert(offer, wait);
        }
    }

    /// Ask for the deadline told for `offer`: at once if it was announced already, or when it is. The answer is a receiver that
    /// gives nothing if no announcement for this offer ever comes (the sender is dropped when the offer is over).
    pub fn ask(&self, offer: &str) -> tokio::sync::oneshot::Receiver<Duration> {
        let (give, get) = tokio::sync::oneshot::channel();
        if let Ok(mut book) = self.book.lock() {
            if let Some(wait) = book.early.remove(offer) {
                let _ = give.send(wait);
            } else if book.asking.len() < MOST_HELD {
                book.asking.insert(offer.to_owned(), give);
            }
        }
        get
    }

    /// The offer is over, announced or not: nothing is waited for, or kept, for it any more.
    pub fn forget(&self, offer: &str) {
        if let Ok(mut book) = self.book.lock() {
            book.asking.remove(offer);
            book.early.remove(offer);
        }
    }
}

/// The announcement readers of every connection a host has outgoing offers on: ONE reader for each connection, made the first time
/// an offer goes out on it, and gone when the connection is. Offers ask it for their deadline by their id, so that none of them
/// reads the connection's streams itself and takes another's announcement.
#[derive(Clone, Default)]
pub struct Readers {
    by_connection: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<usize, Announcements>>>,
}

impl Readers {
    /// The reader of `connection`: the one that is already running for it, or a new one, started now. Called from within the
    /// executor, as every offer is.
    pub fn of(&self, connection: &Connection) -> Announcements {
        let key = connection.stable_id();
        let Ok(mut readers) = self.by_connection.lock() else { return Announcements::default() };
        if let Some(existing) = readers.get(&key) {
            return existing.clone();
        }
        let announcements = Announcements::default();
        readers.insert(key, announcements.clone());
        let (reader, owner, connection) = (announcements.clone(), self.clone(), connection.clone());
        tokio::spawn(async move {
            reader.read_until_closed(&connection).await;
            // The connection is over: its reader goes with it, so that a connection that came and went leaves nothing behind.
            if let Ok(mut readers) = owner.by_connection.lock() {
                readers.remove(&key);
            }
        });
        announcements
    }

    /// How many connections have a reader now. What a test looks at to see that readers are not kept for connections that ended.
    pub fn len(&self) -> usize {
        self.by_connection.lock().map_or(0, |readers| readers.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Lets go of what [`Announcements`] holds for one offer when it is dropped, so that however the wait for that offer ends (answer,
/// refusal, time out, or an error that leaves the function early) nothing stays asking or held for it.
pub struct LetGo {
    announcements: Announcements,
    offer: String,
}

impl LetGo {
    pub fn new(announcements: &Announcements, offer: &str) -> Self {
        Self { announcements: announcements.clone(), offer: offer.to_owned() }
    }
}

impl Drop for LetGo {
    fn drop(&mut self) {
        self.announcements.forget(&self.offer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;
    use peersus_core::config::NetworkSettings;

    fn about(seconds: u32) -> Announce {
        Announce { offer: "one".into(), seconds }
    }

    #[test]
    fn only_a_wait_the_settings_allow_is_believed() {
        assert_eq!(allowed(&about(120)), Some(Duration::from_secs(120)));
        assert_eq!(allowed(&about(MIN_OFFER_TIMEOUT)), Some(Duration::from_secs(u64::from(MIN_OFFER_TIMEOUT))));
        assert_eq!(allowed(&about(MAX_OFFER_TIMEOUT)), Some(Duration::from_secs(u64::from(MAX_OFFER_TIMEOUT))));
        assert_eq!(allowed(&about(MIN_OFFER_TIMEOUT - 1)), None, "a wait shorter than the settings allow was believed");
        assert_eq!(allowed(&about(0)), None, "a deadline of nothing was believed");
        assert_eq!(allowed(&about(MAX_OFFER_TIMEOUT + 1)), None, "a wait longer than the settings allow was believed");
        assert_eq!(allowed(&about(u32::MAX)), None, "a deadline of ever was believed");
    }

    #[test]
    fn an_announcement_with_something_more_in_it_is_not_read() {
        let more = br#"{"offer":"one","seconds":120,"extra":true}"#;
        assert!(serde_json::from_slice::<Announce>(more).is_err(), "an announcement carrying more than it should was read");
        assert!(serde_json::from_slice::<Announce>(br#"{"offer":"one","seconds":120}"#).is_ok());
    }

    async fn pair() -> (iroh::Endpoint, iroh::Endpoint, Connection, Connection) {
        let policy = NetworkSettings { bind: Some("127.0.0.1".parse().unwrap()), allow_relay: false };
        let a = transport::bind(SecretKey::generate(), &policy).await.unwrap();
        let b = transport::bind(SecretKey::generate(), &policy).await.unwrap();
        let (left, right) = tokio::join!(a.connect(b.addr(), transport::ALPN), async { b.accept().await.unwrap().await });
        (a, b, left.unwrap(), right.unwrap())
    }

    async fn closed(a: iroh::Endpoint, b: iroh::Endpoint, left: Connection, right: Connection) {
        left.close(0_u32.into(), b"done");
        right.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }

    /// How long a test waits for a deadline that is expected: long for a machine under load, and nothing waits for it when it comes.
    const EXPECTED: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn offers_made_at_the_same_time_each_hear_their_own_deadline_whatever_order_they_are_announced_in() {
        let (a, b, left, right) = pair().await;
        let sender = Announcements::listen(&right);
        // Three offers on the one connection ask at once, and the other device announces them in the reverse order.
        let (first, second, third) = (sender.ask("offer-a"), sender.ask("offer-b"), sender.ask("offer-c"));
        tell(&left, "offer-c", 900).await;
        tell(&left, "offer-b", 600).await;
        tell(&left, "offer-a", 300).await;
        let heard = |asked: tokio::sync::oneshot::Receiver<Duration>| async move { tokio::time::timeout(EXPECTED, asked).await.unwrap().unwrap() };
        assert_eq!(heard(first).await, Duration::from_secs(300), "an offer was given the deadline of another");
        assert_eq!(heard(second).await, Duration::from_secs(600), "an offer was given the deadline of another");
        assert_eq!(heard(third).await, Duration::from_secs(900), "an offer was given the deadline of another");
        closed(a, b, left, right).await;
    }

    #[tokio::test]
    async fn an_announcement_that_comes_before_its_offer_asks_is_held_and_given_when_it_does() {
        let (a, b, left, right) = pair().await;
        let sender = Announcements::listen(&right);
        tell(&left, "offer-a", 450).await;
        // Let the announcement arrive and be held before anyone asks: it is looked for until it is there.
        let asked = tokio::time::timeout(EXPECTED, async {
            loop {
                let asked = sender.ask("offer-a");
                match tokio::time::timeout(Duration::from_millis(100), asked).await {
                    Ok(Ok(wait)) => return wait,
                    _ => sender.forget("offer-a"),
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("an announcement that came first was never given");
        assert_eq!(asked, Duration::from_secs(450));
        closed(a, b, left, right).await;
    }

    #[tokio::test]
    async fn offers_on_one_connection_share_one_reader_and_it_is_gone_when_the_connection_is() {
        let (a, b, left, right) = pair().await;
        let readers = Readers::default();
        let first = readers.of(&right);
        let second = readers.of(&right);
        assert_eq!(readers.len(), 1, "a second offer on the same connection started a second reader that would take its announcements");
        // Both are the same reader: an announcement for an offer is given to the one that asked, by whichever handle asks.
        let asked = first.ask("offer-a");
        tell(&left, "offer-a", 120).await;
        assert_eq!(tokio::time::timeout(EXPECTED, asked).await.unwrap().unwrap(), Duration::from_secs(120));
        drop(second);
        // The connection ends: the reader is removed, not kept for a connection that is gone.
        right.close(0_u32.into(), b"done");
        left.close(0_u32.into(), b"done");
        tokio::time::timeout(EXPECTED, async {
            while !readers.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the reader of a connection that ended was kept");
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn announcements_that_are_out_of_bounds_or_for_nobody_are_never_given_to_an_offer() {
        let (a, b, left, right) = pair().await;
        let sender = Announcements::listen(&right);
        let asked = sender.ask("offer-a");
        tell(&left, "offer-a", 0).await;
        tell(&left, "offer-a", MAX_OFFER_TIMEOUT + 1).await;
        tell(&left, "someone-elses", 600).await;
        // None of them is believed for this offer: it hears nothing, as a device that never announces gives it nothing.
        assert!(tokio::time::timeout(Duration::from_millis(600), asked).await.is_err(), "a wrong announcement was given to the offer");
        // The right one, after the wrong ones, is still heard by an offer that asks for it.
        let again = sender.ask("offer-a");
        tell(&left, "offer-a", 300).await;
        assert_eq!(tokio::time::timeout(EXPECTED, again).await.unwrap().unwrap(), Duration::from_secs(300));
        closed(a, b, left, right).await;
    }

    #[tokio::test]
    async fn a_device_that_announces_nothing_gives_the_offer_nothing_and_an_offer_that_is_over_is_let_go_of() {
        let (a, b, left, right) = pair().await;
        let sender = Announcements::listen(&right);
        let asked = sender.ask("offer-a");
        // `left` opens no stream, as a device of an older version never does.
        assert!(tokio::time::timeout(Duration::from_millis(300), asked).await.is_err(), "a device that said nothing gave a deadline");
        // The offer is over: what was kept for it goes, so a later announcement for that id is not held for nobody.
        let over = sender.ask("offer-b");
        sender.forget("offer-b");
        assert!(over.await.is_err(), "an offer that was let go of was still waiting");
        closed(a, b, left, right).await;
    }

    #[test]
    fn what_is_held_for_offers_nobody_asked_about_is_bounded() {
        let sender = Announcements::default();
        for n in 0..(MOST_HELD * 3) {
            sender.deliver(format!("offer-{n}"), Duration::from_secs(60));
        }
        let held = sender.book.lock().unwrap().early.len();
        assert_eq!(held, MOST_HELD, "a device that announces without end made this one keep without end");
    }

    #[test]
    fn what_is_kept_for_an_offer_is_let_go_of_when_the_guard_is_dropped_however_the_wait_ended() {
        let sender = Announcements::default();
        let asked = sender.ask("offer-a");
        sender.deliver("offer-early".into(), Duration::from_secs(60));
        {
            let _a = LetGo::new(&sender, "offer-a");
            let _early = LetGo::new(&sender, "offer-early");
            assert_eq!(sender.book.lock().unwrap().asking.len(), 1);
            assert_eq!(sender.book.lock().unwrap().early.len(), 1);
        }
        let book = sender.book.lock().unwrap();
        assert!(book.asking.is_empty(), "an offer that was over was left asking");
        assert!(book.early.is_empty(), "an announcement for an offer that was over was left held");
        drop(book);
        drop(asked);
    }
}
