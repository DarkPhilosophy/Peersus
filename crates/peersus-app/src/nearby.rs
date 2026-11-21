//! Finding paired devices that were brought near, and devices nearby to pair with, for as long as the owner runs
//! (experimental; see `docs/nearby-send.md`).
//!
//! The rules are `peersus_core::nearby`'s. This module joins them to a platform's Bluetooth adapter ([`Radio`]) and to
//! the owner: it says what the radio does from the person's switch, from whether a window is in front and from whether
//! one looks for devices to pair with, turns what the radio heard into readings of devices, and tells the owner what
//! was found whenever that changes.
//!
//! With the switch off nothing is announced, nothing is listened for and nothing is asked on a clock.
#[cfg(target_os = "linux")]
pub mod linux;

use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, Instant};

use peersus_core::model::{Command, PeerId};
use peersus_core::nearby::{
    Found, Nearby, OFFER_INVITATION_BYTES, Rule, Stranger, TAG_BYTES, Takes, Trouble, named, offer,
    offered, period, tag,
};
use peersus_net::invitation::Invitation;

use crate::runtime::Reply;

/// How often what was heard is taken from the radio while it listens.
const LISTEN_EVERY: Duration = Duration::from_millis(500);
/// How often the switch being on is looked at again while no window is in front: only for a window coming to the front.
const IDLE_EVERY: Duration = Duration::from_secs(1);
/// How often the owner is asked what this device announces to devices nearby, while a window is in front: at every
/// look. What a page lets the device be found for changes the moment a person opens or leaves it, and devices nearby
/// are told within this long. The asking is a word to the owner in this process and costs nothing on the air: the
/// radio is given the same announcement until it changes.
const OFFER_ASKED_EVERY: Duration = Duration::from_millis(500);
/// The most announcements taken from the radio in one look. A place full of devices is not a reason to do more work.
const MOST_HEARD: usize = 256;
/// The most devices that are not paired held at once: a list a person reads, and whoever fills the air with offers
/// fills no more than this.
const MOST_STRANGERS: usize = 16;
/// For how long a paired device that said it takes files is held to say so without being heard again. A phone's
/// announcements were heard about every 0.3 s, and the longest silence among 88 of its tags was 1.5 s (how long its
/// offers go unheard was not measured apart). One that leaves Receive stops announcing it, or says another thing, so
/// this is how long files could still go to a device whose person has just left Receive: kept little over that.
const TAKING_HEARD_WITHIN_MS: u64 = 2_500;
/// For how long a strength that was heard counts towards the strongest one, while a person measures: long enough
/// that the number they read does not jump with every announcement.
const MEASURED_OVER_MS: u64 = 1_500;
/// The most strengths kept for that.
const MOST_MEASURED: usize = 64;

/// Which of the two things this application announces an announcement is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The data of [`peersus_core::nearby::SERVICE`]: what a paired device is recognised by.
    Tag,
    /// The data of [`peersus_core::nearby::OFFER_SERVICE`]: an offer to pair.
    Offer,
}

/// One announcement that was heard: its data, from anyone and of any length the adapter carries, and the strength it
/// was heard at, in dBm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    pub kind: Kind,
    pub data: Vec<u8>,
    pub strength: i16,
}

/// The longest data of the tag's service an adapter keeps: more than a tag is nobody's.
#[cfg(any(target_os = "linux", test))]
pub(crate) const MOST_TAG: usize = 32;

/// Which of this application's two services `service` is (a 128-bit identifier as text, in any case), and the most bytes
/// the data of that service may be: `None` for any other service, which is nobody's business here.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn kind_of(service: &str) -> Option<(Kind, usize)> {
    use peersus_core::nearby::{OFFER_BYTES, OFFER_SERVICE, SERVICE};
    if service.eq_ignore_ascii_case(SERVICE) {
        Some((Kind::Tag, MOST_TAG))
    } else if service.eq_ignore_ascii_case(OFFER_SERVICE) {
        Some((Kind::Offer, OFFER_BYTES))
    } else {
        None
    }
}

/// A platform's Bluetooth Low Energy adapter, as far as nearness needs it. It holds no rule.
///
/// `announce`, `offer` and `listen` say how things should be and are called again and again with the same word: an
/// adapter does nothing when it is already so, and makes it so again when the system undid it (Bluetooth was switched
/// off and on).
pub trait Radio: Send {
    /// Announce `tag` as the data of [`peersus_core::nearby::SERVICE`], or announce nothing.
    fn announce(
        &mut self,
        tag: Option<[u8; TAG_BYTES]>,
    ) -> impl Future<Output = Result<(), Trouble>> + Send;
    /// Announce `offer` as the data of [`peersus_core::nearby::OFFER_SERVICE`], beside the tag, or stop. It is longer
    /// than the older kind of announcement carries: a radio without the extended kind refuses.
    fn offer(&mut self, offer: Option<&[u8]>) -> impl Future<Output = Result<(), Trouble>> + Send;
    /// Listen for announcements of both services, or stop listening.
    fn listen(&mut self, on: bool) -> impl Future<Output = Result<(), Trouble>> + Send;
    /// What was heard since this was last asked; bounded by the adapter.
    fn heard(&mut self) -> Vec<Reading>;
}

/// How things are at one look.
#[derive(Clone, Copy, Debug)]
pub struct Now<'a> {
    /// The person's switch.
    pub on: bool,
    /// Whether a window is there to show what is found.
    pub in_front: bool,
    pub paired: &'a [PeerId],
    /// Whether a window of this device is open: only then are the offers of devices nearby listened for.
    pub seeking: bool,
    /// What this device announces to devices nearby, when the page its window shows lets it be found (to be paired
    /// with, or to be sent files); nothing on any other page.
    pub offer: Option<&'a [u8]>,
    /// The strength at which a device is brought near, as the person set it; the application's own when they set none.
    pub near_at: Option<i16>,
    /// Whether a person is setting that strength now: only then is the strongest device heard told.
    pub measuring: bool,
    /// The wall clock, which the tags and the invitations turn on.
    pub unix_seconds: u64,
    /// A clock that does not go back, which the rules count on.
    pub now_ms: u64,
}

/// A device that is not paired, as it was last heard.
struct Offered {
    /// What it announced, to know it again without reading it again.
    data: Vec<u8>,
    name: String,
    receives: bool,
    /// Its invitation as text, as the owner takes one.
    invitation: String,
    expires_at: u64,
    heard_at: u64,
}

/// What a paired device last announced, as far as it matters: whether its person has Receive open.
struct Taking {
    /// What it announced, to know it again without reading it again.
    data: Vec<u8>,
    files: bool,
    heard_at: u64,
}

/// The rules and the radio together: one look says what the radio does and gives what nearness says now.
pub struct Finder<R> {
    own: PeerId,
    radio: R,
    nearby: Nearby,
    around: Nearby,
    /// Which of the devices that are not paired were brought near, by the rule a paired one is near by.
    brought: Nearby,
    strangers: BTreeMap<PeerId, Offered>,
    /// What each paired device that is heard says it takes. Never more entries than there are paired devices.
    taking: BTreeMap<PeerId, Taking>,
    /// When and how strongly devices of this application were heard, while a person measures.
    measured: Vec<(u64, i16)>,
}

impl<R: Radio> Finder<R> {
    /// `own` is this device's identifier as its paired devices know it: what its tag is made from.
    pub fn new(own: PeerId, radio: R) -> Self {
        Self {
            own,
            radio,
            nearby: Nearby::new(Rule::BROUGHT_NEAR),
            around: Nearby::new(Rule::AROUND),
            brought: Nearby::new(Rule::BROUGHT_NEAR),
            strangers: BTreeMap::new(),
            taking: BTreeMap::new(),
            measured: Vec::new(),
        }
    }

    fn forget_strangers(&mut self) {
        self.around.clear();
        self.brought.clear();
        self.strangers.clear();
        self.taking.clear();
    }

    /// An offer was heard. Whoever made it, it is read only if it reads as an offer with an invitation that is live.
    /// Of a paired device, only what it takes is kept: the name and the invitation in it are of no use, since the
    /// device is known and a send to it goes over the connection that authenticates it. Of a device that is not
    /// paired, and is not this one, it is held while there is room.
    fn offer_heard(&mut self, data: Vec<u8>, strength: i16, now: &Now<'_>) {
        if let Some(held) = self.taking.values_mut().find(|held| held.data == data) {
            held.heard_at = now.now_ms;
            return;
        }
        let known = self
            .strangers
            .iter_mut()
            .find(|(_, held)| held.data == data)
            .map(|(id, held)| {
                held.heard_at = now.now_ms;
                id.clone()
            });
        let id = match known {
            Some(id) => id,
            None => {
                let Some(heard) = offered(&data) else {
                    return;
                };
                let (name, takes, invitation) = (heard.name, heard.takes, heard.invitation);
                let Ok(invitation) = Invitation::from_radio(invitation, now.unix_seconds) else {
                    return;
                };
                let id = PeerId(invitation.address.id.to_string());
                if id == self.own {
                    return;
                }
                if now.paired.contains(&id) {
                    self.taking.insert(
                        id,
                        Taking {
                            data,
                            files: takes == Takes::Files,
                            heard_at: now.now_ms,
                        },
                    );
                    return;
                }
                if !self.strangers.contains_key(&id) && self.strangers.len() >= MOST_STRANGERS {
                    return;
                }
                let Ok(text) = invitation.encode() else {
                    return;
                };
                self.strangers.insert(
                    id.clone(),
                    Offered {
                        data,
                        name,
                        receives: takes == Takes::Files,
                        invitation: text,
                        expires_at: invitation.expires_at,
                        heard_at: now.now_ms,
                    },
                );
                id
            }
        };
        // Listed by the offer; near or not is said by the tag, as for a paired device (see `look`).
        self.around.reading(&id, now.now_ms, strength);
    }

    pub async fn look(&mut self, now: Now<'_>) -> Found {
        if !now.on {
            // Nothing can be told to a person who turned it off, and nothing is tried twice here: the next look says it again.
            let _ = self.radio.announce(None).await;
            let _ = self.radio.offer(None).await;
            let _ = self.radio.listen(false).await;
            self.nearby.clear();
            self.forget_strangers();
            self.measured.clear();
            return Found::default();
        }
        let listening = now.in_front;
        let seeking = listening && now.seeking;
        let rule = Rule::brought_near(now.near_at);
        self.nearby.rule(rule);
        self.brought.rule(rule);
        let announced = self
            .radio
            .announce(Some(tag(&self.own, period(now.unix_seconds))))
            .await;
        let offering = self
            .radio
            .offer(if seeking { now.offer } else { None })
            .await;
        let listened = self.radio.listen(listening).await;
        let trouble = announced.err().or(listened.err()).or(offering.err());
        if !seeking {
            self.forget_strangers();
        }
        if !listening || listened.is_err() {
            // What was near while it was listened for says nothing of now.
            self.nearby.clear();
            self.forget_strangers();
            self.measured.clear();
            let _ = self.radio.heard();
            return Found {
                trouble,
                ..Found::default()
            };
        }
        if !now.measuring {
            self.measured.clear();
        }
        for reading in self.radio.heard().into_iter().take(MOST_HEARD) {
            if !peersus_core::nearby::STRENGTHS.contains(&reading.strength) {
                continue;
            }
            // Whatever announces a tag of this application's is one of its devices as far as measuring goes: it sets
            // a distance and admits no one.
            if now.measuring
                && reading.kind == Kind::Tag
                && reading.data.len() == TAG_BYTES
                && self.measured.len() < MOST_MEASURED
            {
                self.measured.push((now.now_ms, reading.strength));
            }
            match reading.kind {
                // Near is said by the tag alone, for a paired device and for one that is not: it is what a person
                // measures by, and a phone was heard some 5 dB weaker by its offer than by its tag from the same
                // place. A device that is not paired is known by the identity its offer gave, which its tag is made
                // of like anyone's.
                Kind::Tag => {
                    if let Some(peer) = named(&reading.data, now.paired, now.unix_seconds) {
                        self.nearby.reading(peer, now.now_ms, reading.strength);
                    } else if let Some(peer) =
                        named(&reading.data, self.strangers.keys(), now.unix_seconds)
                    {
                        self.brought.reading(peer, now.now_ms, reading.strength);
                    }
                }
                Kind::Offer if seeking => self.offer_heard(reading.data, reading.strength, &now),
                Kind::Offer => {}
            }
        }
        self.measured
            .retain(|(at, _)| now.now_ms.saturating_sub(*at) < MEASURED_OVER_MS);
        let around = self.around.near(now.now_ms);
        let brought = self.brought.near(now.now_ms);
        // What ran out, was paired meanwhile, or was not heard for as long as makes a listed device gone, is let go of.
        let silent_for = Rule::AROUND.gone_after_ms;
        self.strangers.retain(|id, held| {
            held.expires_at > now.unix_seconds
                && !now.paired.contains(id)
                && now.now_ms.saturating_sub(held.heard_at) < silent_for
        });
        self.taking.retain(|id, held| {
            now.paired.contains(id)
                && now.now_ms.saturating_sub(held.heard_at) < TAKING_HEARD_WITHIN_MS
        });
        let near = self.nearby.near(now.now_ms);
        Found {
            // Near by its tag, and its person has Receive open by its offer: both, or the files stay where they are.
            receiving: near
                .iter()
                .filter(|id| self.taking.get(*id).is_some_and(|held| held.files))
                .cloned()
                .collect(),
            near,
            strangers: around
                .into_iter()
                .filter_map(|id| {
                    let held = self.strangers.get(&id)?;
                    Some(Stranger {
                        near: held.receives && brought.contains(&id),
                        receives: held.receives,
                        name: held.name.clone(),
                        id,
                    })
                })
                .collect(),
            trouble,
            measuring: false,
            strongest: self.measured.iter().map(|(_, strength)| *strength).max(),
        }
    }

    /// The invitation of each of `strangers`, as text: what the owner pairs with one of them by.
    pub fn invitations(&self, strangers: &[Stranger]) -> Vec<(PeerId, String)> {
        strangers
            .iter()
            .filter_map(|stranger| {
                let held = self.strangers.get(&stranger.id)?;
                Some((stranger.id.clone(), held.invitation.clone()))
            })
            .collect()
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// What the owner said of this device and devices nearby: whether a window is open (their offers are listened for),
/// and what this device announces to them, from the invitation the owner made for it and what it is for. Nothing is
/// announced when the owner made none (the page shown lets this device be found for nothing) or it does not fit an
/// announcement.
fn framed(name: &str, reply: Result<Reply, &'static str>) -> (bool, Option<Vec<u8>>) {
    let Ok(Reply::Nearby(said)) = reply else {
        return (false, None);
    };
    let offer = said.and_then(|(text, takes)| {
        let invitation = Invitation::decode(&text, unix_seconds())
            .ok()?
            .to_radio(OFFER_INVITATION_BYTES)
            .ok()?;
        offer(name, &invitation, takes)
    });
    (true, offer)
}

/// Tell the owner what nearness says, now and whenever it changes, for as long as it runs. `in_front` says whether a
/// window is in front now. Dropping the future drops the radio, which lets go of what it held of the system.
pub async fn follow<R: Radio>(
    client: crate::runtime::Client,
    own: PeerId,
    radio: R,
    in_front: impl Fn() -> bool + Send,
) {
    let mut states = client.subscribe();
    let started = Instant::now();
    let mut finder = Finder::new(own, radio);
    let mut told = (Found::default(), Vec::new());
    let mut offer: Option<Vec<u8>> = None;
    let mut seeking = false;
    let mut asked: Option<Instant> = None;
    loop {
        let (on, paired, name, near_at, measuring) = {
            let state = states.borrow_and_update();
            let on = state.settings.nearby_send;
            let paired: Vec<PeerId> = if on {
                state.peers.iter().map(|peer| peer.id.clone()).collect()
            } else {
                Vec::new()
            };
            (
                on,
                paired,
                state.settings.device_name.clone(),
                state.settings.nearby_near_at,
                state.nearby.measuring,
            )
        };
        let front = on && in_front();
        if !front {
            if asked.take().is_some() {
                // The window went behind, or away: nothing is announced, and what was announced for the page it
                // showed is taken back now, not when it runs out. Said once; nothing is asked while it stays so.
                let _ = client.command(Command::NearbyOffer { front: false }).await;
            }
            offer = None;
            seeking = false;
        } else if asked.is_none_or(|at| at.elapsed() >= OFFER_ASKED_EVERY) {
            asked = Some(Instant::now());
            (seeking, offer) = framed(
                &name,
                client.command(Command::NearbyOffer { front: true }).await,
            );
        }
        let found = finder
            .look(Now {
                on,
                in_front: front,
                paired: &paired,
                seeking,
                offer: offer.as_deref(),
                near_at,
                measuring,
                unix_seconds: unix_seconds(),
                now_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            })
            .await;
        let offers = finder.invitations(&found.strangers);
        if found != told.0 || offers != told.1 {
            if client
                .command(Command::SetNearby {
                    found: found.clone(),
                    offers: offers.clone(),
                })
                .await
                .is_err()
            {
                return;
            }
            told = (found, offers);
        }
        if on {
            tokio::time::sleep(if front { LISTEN_EVERY } else { IDLE_EVERY }).await;
        } else if states.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
