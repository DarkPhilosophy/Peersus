//! Finding a paired device that was brought near, and devices nearby that are not paired yet (experimental; see
//! `docs/nearby-send.md`).
//!
//! Only the rules are here, as pure functions: what a device announces, which paired device an announcement that was
//! heard names, what an offer to pair is on the air, and when a device counts as near. What announces and listens is a
//! platform's adapter, which holds no rule: it is told what to announce, and tells what it heard with the strength it
//! heard it at.
//!
//! An announcement is a hint for choosing a destination and never an authentication. The tag has no secret in it:
//! whoever knows a device's identifier can compute its tags. What it can do at worst is make the wrong PAIRED device be
//! proposed; the connection that follows is the authenticated one, to that device's key.
//!
//! An offer names a device nobody has verified: the name in it is whatever its maker wrote. It makes a device appear
//! in a list and nothing more; pairing with it is the ceremony two people go through, as with a code, and a file sent
//! to it is a file its person is asked about, one by one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::peer::PeerId;

/// Why nothing is found by nearness though the switch is on, as far as the platform said.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trouble {
    /// This device has no Bluetooth Low Energy radio the application can use.
    NoRadio,
    /// Bluetooth is switched off.
    Off,
    /// The person did not allow the application to use Bluetooth for devices nearby.
    NotAllowed,
    /// The system refused to announce or to listen, and did not say more that a person could act on.
    Failed,
    /// The system lets this device listen for devices nearby but will not announce this application's data to them, so
    /// the other devices cannot find this one by Bluetooth though it finds theirs. Apple's Core Bluetooth documents that
    /// a program may announce only a name and service identifiers (no service data); Microsoft's publisher documents the
    /// service-data sections as reserved, and a Windows system that refuses one says this too.
    ListenOnly,
}

/// What a device lets devices nearby do with it, which is what its offer says. Two different things, and an offer is
/// one of them, never both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Takes {
    /// "I am here, to be paired with": its person looks at the devices nearby, and it is listed by them.
    Pairing,
    /// "I am here and I take files": its person has Receive open.
    Files,
}

/// A device nearby that is not paired, as it says of itself: nobody has checked the name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stranger {
    pub id: PeerId,
    pub name: String,
    /// Whether what it said is that it takes files ([`Takes::Files`]), each of them its person's to accept; otherwise
    /// it said that it is there to be paired with.
    #[serde(default, skip_serializing_if = "is_false")]
    pub receives: bool,
    /// Whether it was brought near, by the rule a paired device is near by.
    #[serde(default, skip_serializing_if = "is_false")]
    pub near: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// What nearness says now, as the owner tells its windows. Empty and without trouble while the switch is off.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    /// The paired devices that are near, in the order of their identifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub near: Vec<PeerId>,
    /// Those of them whose person has Receive open now ([`Takes::Files`]), in the order of their identifiers: where
    /// listed files go without a further press.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receiving: Vec<PeerId>,
    /// The devices nearby that offer to pair and are not paired, in the order of their identifiers. Only while a
    /// person looks for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strangers: Vec<Stranger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trouble: Option<Trouble>,
    /// Whether a person is setting how near "near" is (see [`near_at_from`]). Said by the owner, never by an adapter.
    #[serde(default, skip_serializing_if = "is_false")]
    pub measuring: bool,
    /// The strength, in dBm, of the device of this application that is heard strongest. Only while `measuring`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strongest: Option<i16>,
}

/// The service the tag is announced as the data of: a 128-bit identifier of this application's own, made once at random.
pub const SERVICE: &str = "6f1d3c52-8a0b-4e9f-9c57-2b6e41d07a13";
/// How many bytes a tag is.
pub const TAG_BYTES: usize = 8;
/// How long one tag is announced before the next takes its place.
pub const PERIOD_SECONDS: u64 = 15 * 60;
/// What the hash of a tag starts with, so that it is a hash of nothing else in this application.
const CONTEXT: &[u8] = b"peersus nearby tag v1\0";

/// The strength, in dBm, at or above which a device may be near.
pub const NEAR_AT: i16 = -55;
/// The strength under which a device that was near is on its way to being gone.
pub const GONE_BELOW: i16 = -70;
/// How long the strength must stay at or above [`NEAR_AT`], unbroken, before the device is near.
pub const NEAR_AFTER_MS: u64 = 1_500;
/// How long a near device may be weak or unheard before it is gone.
pub const GONE_AFTER_MS: u64 = 3_000;
/// The longest silence between two strong readings that still counts as unbroken: announcements are missed now and
/// then. Measured between a phone and a computer a few metres apart: half of the silences under 0.3 s, nine in ten
/// under 0.85 s, the longest of 88 was 1.5 s.
pub const UNBROKEN_WITHIN_MS: u64 = 2_000;
/// How much under the strength a person measured, with the two devices held as they will hold them, "near" begins:
/// the strength of one device held still moves by about this much from one reading to the next.
pub const MEASURED_MARGIN: i16 = 8;
/// The bounds of a threshold a person set: never so weak that a device across the room is near, never so strong that
/// no device can be.
pub const NEAR_AT_WEAKEST: i16 = -85;
pub const NEAR_AT_STRONGEST: i16 = -30;
/// The strengths, in dBm, a radio can hear an announcement at. A system says "not known" with a number outside them
/// (Android says 127), and that is no reading: taken as one it would be the strongest thing ever heard.
pub const STRENGTHS: std::ops::RangeInclusive<i16> = -127..=20;

/// The threshold for near that a measured strength gives.
pub fn near_at_from(strongest: i16) -> i16 {
    strongest
        .saturating_sub(MEASURED_MARGIN)
        .clamp(NEAR_AT_WEAKEST, NEAR_AT_STRONGEST)
}

/// When a device counts as near and when as gone: two thresholds and two whiles (see [`Nearby`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub near_at: i16,
    pub gone_below: i16,
    pub near_after_ms: u64,
    pub gone_after_ms: u64,
    pub unbroken_within_ms: u64,
}

impl Rule {
    /// Held right next to this device: what stands for choosing it as where files go.
    pub const BROUGHT_NEAR: Self = Self {
        near_at: NEAR_AT,
        gone_below: GONE_BELOW,
        near_after_ms: NEAR_AFTER_MS,
        gone_after_ms: GONE_AFTER_MS,
        unbroken_within_ms: UNBROKEN_WITHIN_MS,
    };
    /// Held right next to this device, with the threshold a person set for it, or the one this application has when
    /// they set none. What keeps a near device near stays as far under the threshold as it is in [`Self::BROUGHT_NEAR`].
    pub fn brought_near(near_at: Option<i16>) -> Self {
        let near_at = near_at.map_or(NEAR_AT, |at| at.clamp(NEAR_AT_WEAKEST, NEAR_AT_STRONGEST));
        Self {
            near_at,
            gone_below: near_at - (NEAR_AT - GONE_BELOW),
            ..Self::BROUGHT_NEAR
        }
    }
    /// Somewhere around, as far as a list of devices to pair with needs: heard, twice, not at the edge of hearing.
    /// An offer is announced less often than a tag, so the whiles are longer.
    pub const AROUND: Self = Self {
        near_at: -88,
        gone_below: -94,
        near_after_ms: 1_000,
        gone_after_ms: 8_000,
        unbroken_within_ms: 4_000,
    };
}

/// The service an offer is announced as the data of: another identifier of this application's own.
pub const OFFER_SERVICE: &str = "6e1bcff1-7b65-4c9e-acca-319711224d9c";
/// The most bytes an offer is on the air. It needs an announcement of the extended kind (Bluetooth 5), which carries
/// more than this; a radio that has only the older kind cannot offer.
pub const OFFER_BYTES: usize = 200;
/// The most bytes of a device's name in an offer; a longer name is cut at a whole character.
pub const OFFER_NAME_BYTES: usize = 32;
/// The most bytes the invitation in an offer may be: what is left beside the version, the name's length and the name.
pub const OFFER_INVITATION_BYTES: usize = OFFER_BYTES - 2 - OFFER_NAME_BYTES;
/// The first byte of an offer, which says what the device that announces it takes ([`Takes`]).
const OFFER_PAIRS: u8 = 1;
const OFFER_TAKES_FILES: u8 = 2;

/// An offer that was heard, read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer<'a> {
    pub name: String,
    /// The invitation as the network layer made it for a radio.
    pub invitation: &'a [u8],
    pub takes: Takes,
}

/// An offer as it is announced: what the device takes, its name with its length before it, and the invitation as
/// the network layer made it for a radio, which this layer carries and does not read. `None` when it does not fit.
pub fn offer(name: &str, invitation: &[u8], takes: Takes) -> Option<Vec<u8>> {
    let mut end = name.len().min(OFFER_NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let name = &name.as_bytes()[..end];
    if invitation.is_empty() || invitation.len() > OFFER_INVITATION_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(2 + name.len() + invitation.len());
    bytes.push(match takes {
        Takes::Pairing => OFFER_PAIRS,
        Takes::Files => OFFER_TAKES_FILES,
    });
    bytes.push(u8::try_from(name.len()).ok()?);
    bytes.extend_from_slice(name);
    bytes.extend_from_slice(invitation);
    Some(bytes)
}

/// What an offer that was heard says, from anyone and of any length. A name is text without control characters, cut
/// of the space around it and not empty; anything else is no offer.
pub fn offered(heard: &[u8]) -> Option<Offer<'_>> {
    let (&version, rest) = heard.split_first()?;
    let (&length, rest) = rest.split_first()?;
    let takes = match version {
        OFFER_PAIRS => Takes::Pairing,
        OFFER_TAKES_FILES => Takes::Files,
        _ => return None,
    };
    if heard.len() > OFFER_BYTES || usize::from(length) > OFFER_NAME_BYTES {
        return None;
    }
    let (name, invitation) = rest.split_at_checked(usize::from(length))?;
    let name = std::str::from_utf8(name).ok()?;
    // Before the space around it is cut: a line end is a control character and is also space.
    if name.chars().any(char::is_control) || invitation.is_empty() {
        return None;
    }
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(Offer {
        name: name.to_owned(),
        invitation,
        takes,
    })
}

/// The number of the period `unix_seconds` falls in.
pub fn period(unix_seconds: u64) -> u64 {
    unix_seconds / PERIOD_SECONDS
}

/// What the device `peer` announces during `period`.
pub fn tag(peer: &PeerId, period: u64) -> [u8; TAG_BYTES] {
    let mut hash = Sha256::new();
    hash.update(CONTEXT);
    hash.update(peer.0.as_bytes());
    hash.update(period.to_be_bytes());
    let digest = hash.finalize();
    let mut tag = [0; TAG_BYTES];
    tag.copy_from_slice(&digest[..TAG_BYTES]);
    tag
}

/// The paired device an announcement that was heard names, or none. `heard` is what arrived, of any length and from
/// anyone: only exactly a tag of one of `paired`, for the period now or the one on either side of it (two clocks are
/// never the same clock), names a device.
pub fn named<'a>(
    heard: &[u8],
    paired: impl IntoIterator<Item = &'a PeerId>,
    unix_seconds: u64,
) -> Option<&'a PeerId> {
    if heard.len() != TAG_BYTES {
        return None;
    }
    let now = period(unix_seconds);
    let periods = [now.checked_sub(1), Some(now), now.checked_add(1)];
    paired.into_iter().find(|peer| {
        periods
            .iter()
            .flatten()
            .any(|&period| tag(peer, period).as_slice() == heard)
    })
}

/// What was heard of one device, as far as the rule needs it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Heard {
    /// When the unbroken run of strong readings began, and when its last reading was.
    strong: Option<(u64, u64)>,
    /// When it was last heard at a strength that keeps a near device near.
    kept_at: u64,
    near: bool,
}

impl Heard {
    fn reading(&mut self, rule: &Rule, at: u64, strength: i16) {
        if strength >= rule.near_at {
            let since = match self.strong {
                Some((since, last)) if at.saturating_sub(last) <= rule.unbroken_within_ms => since,
                _ => at,
            };
            self.strong = Some((since, at));
            if at.saturating_sub(since) >= rule.near_after_ms {
                self.near = true;
            }
        } else {
            self.strong = None;
        }
        if strength >= rule.gone_below {
            self.kept_at = at;
        }
    }

    fn near_at(&mut self, rule: &Rule, now: u64) -> bool {
        if self.near && now.saturating_sub(self.kept_at) >= rule.gone_after_ms {
            *self = Self::default();
        }
        self.near
    }
}

/// Which devices are near, from the readings an adapter tells, by a [`Rule`]. Times are milliseconds of one clock that
/// does not go back; nothing here reads a clock.
///
/// The rule is coarse on purpose and has two thresholds, so that a device at the edge does not flicker: near once it
/// was heard at `near_at` or stronger for `near_after_ms` unbroken, gone once it was weaker than `gone_below` or
/// unheard for `gone_after_ms`. It holds one small record for each device it was told of; who it is told of, and how
/// many, is its caller's to bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nearby {
    rule: Rule,
    heard: BTreeMap<PeerId, Heard>,
}

impl Nearby {
    pub fn new(rule: Rule) -> Self {
        Self {
            rule,
            heard: BTreeMap::new(),
        }
    }

    /// Judge by another rule from now on. What was heard is kept: it is judged anew at the next reading.
    pub fn rule(&mut self, rule: Rule) {
        self.rule = rule;
    }

    /// A device was heard at `strength` dBm.
    pub fn reading(&mut self, peer: &PeerId, at: u64, strength: i16) {
        self.heard
            .entry(peer.clone())
            .or_default()
            .reading(&self.rule, at, strength);
    }

    /// How many devices something is held of.
    pub fn held(&self) -> usize {
        self.heard.len()
    }

    /// The devices that are near at `now`, in the order of their identifiers. What was not heard for long enough to be
    /// gone is forgotten.
    pub fn near(&mut self, now: u64) -> Vec<PeerId> {
        let rule = self.rule;
        self.heard.retain(|_, heard| {
            heard.near_at(&rule, now)
                || heard
                    .strong
                    .is_some_and(|(_, last)| now.saturating_sub(last) <= rule.unbroken_within_ms)
        });
        self.heard
            .iter()
            .filter(|(_, heard)| heard.near)
            .map(|(peer, _)| peer.clone())
            .collect()
    }

    /// Forget everything: listening stopped, and what was near then says nothing of now.
    pub fn clear(&mut self) {
        self.heard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str) -> PeerId {
        PeerId(name.to_owned())
    }

    #[test]
    fn a_tag_names_its_device_for_its_period_and_the_ones_beside_it_only() {
        let (a, b) = (peer("device-a"), peer("device-b"));
        let paired = [a.clone(), b.clone()];
        let at = 40 * PERIOD_SECONDS + 7;
        let now = period(at);
        assert_eq!(named(&tag(&a, now), &paired, at), Some(&a));
        assert_eq!(named(&tag(&b, now - 1), &paired, at), Some(&b));
        assert_eq!(named(&tag(&b, now + 1), &paired, at), Some(&b));
        assert_eq!(named(&tag(&a, now - 2), &paired, at), None);
        assert_eq!(named(&tag(&a, now + 2), &paired, at), None);
        // A device that is not paired is nobody, whatever it announces.
        assert_eq!(named(&tag(&peer("stranger"), now), &paired, at), None);
        // The tag changes with the period and with the device.
        assert_ne!(tag(&a, now), tag(&a, now + 1));
        assert_ne!(tag(&a, now), tag(&b, now));
    }

    #[test]
    fn what_is_not_exactly_a_tag_names_nobody() {
        let a = peer("device-a");
        let paired = [a.clone()];
        let whole = tag(&a, 0);
        assert_eq!(named(&[], &paired, 0), None);
        assert_eq!(named(&whole[..7], &paired, 0), None);
        let mut longer = whole.to_vec();
        longer.push(0);
        assert_eq!(named(&longer, &paired, 0), None);
        // The first period has none before it; that is not an overflow.
        assert_eq!(named(&whole, &paired, 0), Some(&a));
        assert_eq!(
            named(&tag(&a, u64::MAX / PERIOD_SECONDS), &paired, u64::MAX),
            Some(&a)
        );
    }

    #[test]
    fn near_takes_an_unbroken_strong_while_and_gone_takes_a_longer_weak_one() {
        let a = peer("device-a");
        let mut nearby = Nearby::new(Rule::BROUGHT_NEAR);
        // One strong reading is not near, and neither is a second before the while has passed.
        nearby.reading(&a, 0, -40);
        assert!(nearby.near(0).is_empty());
        nearby.reading(&a, 1_000, -50);
        assert!(nearby.near(1_000).is_empty());
        nearby.reading(&a, 1_500, NEAR_AT);
        assert_eq!(nearby.near(1_500), vec![a.clone()]);
        // Between the two thresholds it stays near for as long as it is heard.
        for at in (2_000..12_000).step_by(1_000) {
            nearby.reading(&a, at, -65);
            assert_eq!(nearby.near(at), vec![a.clone()], "at {at}");
        }
        // Weaker than the lower threshold: still near until that has lasted.
        nearby.reading(&a, 12_000, -80);
        nearby.reading(&a, 13_000, -80);
        assert_eq!(nearby.near(13_999), vec![a.clone()]);
        nearby.reading(&a, 14_000, -80);
        assert!(nearby.near(14_000).is_empty());
        // Gone is gone: one strong reading does not bring it back.
        nearby.reading(&a, 14_100, -40);
        assert!(nearby.near(14_100).is_empty());
    }

    #[test]
    fn a_weak_reading_or_a_silence_breaks_the_strong_while() {
        let a = peer("device-a");
        let mut nearby = Nearby::new(Rule::BROUGHT_NEAR);
        nearby.reading(&a, 0, -40);
        nearby.reading(&a, 800, -60);
        nearby.reading(&a, 1_600, -40);
        assert!(
            nearby.near(1_600).is_empty(),
            "a weak reading in between did not break it"
        );
        // After a silence the while begins again: it is counted from the reading after it, not from the first.
        let mut nearby = Nearby::new(Rule::BROUGHT_NEAR);
        let after = UNBROKEN_WITHIN_MS + 1;
        nearby.reading(&a, 0, -40);
        nearby.reading(&a, after, -40);
        nearby.reading(&a, after + NEAR_AFTER_MS - 1, -40);
        assert!(
            nearby.near(after + NEAR_AFTER_MS - 1).is_empty(),
            "a silence did not break it"
        );
        nearby.reading(&a, after + NEAR_AFTER_MS, -40);
        assert_eq!(nearby.near(after + NEAR_AFTER_MS), vec![a]);
    }

    #[test]
    fn a_near_device_that_is_no_longer_heard_is_gone_and_forgotten() {
        let (a, b) = (peer("device-a"), peer("device-b"));
        let mut nearby = Nearby::new(Rule::BROUGHT_NEAR);
        for at in [0, 500, 1_000, 1_500] {
            nearby.reading(&a, at, -40);
            nearby.reading(&b, at, -90);
        }
        assert_eq!(nearby.near(1_500), vec![a.clone()]);
        assert_eq!(nearby.near(1_500 + GONE_AFTER_MS - 1), vec![a]);
        assert!(nearby.near(1_500 + GONE_AFTER_MS).is_empty());
        assert_eq!(nearby.held(), 0, "what is gone was kept");
    }

    #[test]
    fn a_device_around_is_found_at_a_strength_that_is_far_from_brought_near() {
        let a = peer("device-a");
        let (mut around, mut brought) =
            (Nearby::new(Rule::AROUND), Nearby::new(Rule::BROUGHT_NEAR));
        for at in [0, 1_500, 3_000] {
            around.reading(&a, at, -78);
            brought.reading(&a, at, -78);
        }
        assert_eq!(around.near(3_000), vec![a.clone()]);
        assert!(brought.near(3_000).is_empty());
        // At the edge of hearing it is not listed, and one that was listed goes after its longer while.
        let mut edge = Nearby::new(Rule::AROUND);
        for at in [0, 1_500, 3_000] {
            edge.reading(&a, at, -92);
        }
        assert!(edge.near(3_000).is_empty());
        assert_eq!(around.near(3_000 + Rule::AROUND.gone_after_ms - 1), vec![a]);
        assert!(around.near(3_000 + Rule::AROUND.gone_after_ms).is_empty());
    }

    #[test]
    fn an_offer_carries_a_name_and_an_invitation_and_what_is_not_one_is_nothing() {
        let invitation = [9_u8; 90];
        let made = offer("Ana's phone", &invitation, Takes::Files).unwrap();
        assert!(made.len() <= OFFER_BYTES);
        assert_eq!(
            offered(&made),
            Some(Offer {
                name: "Ana's phone".to_owned(),
                invitation: &invitation[..],
                takes: Takes::Files,
            })
        );
        // To be paired with is another thing, said by another first byte, and it takes no files.
        let pairs = offer("Ana's phone", &invitation, Takes::Pairing).unwrap();
        assert_ne!(pairs[0], made[0]);
        assert_eq!(pairs[1..], made[1..]);
        assert_eq!(
            offered(&pairs).map(|offer| offer.takes),
            Some(Takes::Pairing)
        );
        // A long name is cut at a whole character and never in the middle of one.
        let long = "ș".repeat(40);
        let name = offered(&offer(&long, &invitation, Takes::Pairing).unwrap())
            .map(|offer| offer.name)
            .unwrap();
        assert_eq!(name, "ș".repeat(OFFER_NAME_BYTES / 2));
        // What does not fit is not announced.
        assert_eq!(
            offer("a", &[0; OFFER_INVITATION_BYTES + 1], Takes::Pairing),
            None
        );
        assert_eq!(offer("a", &[], Takes::Files), None);
        assert!(
            offer(&"n".repeat(64), &[0; OFFER_INVITATION_BYTES], Takes::Files)
                .unwrap()
                .len()
                == OFFER_BYTES
        );
        // From the air: another version, a length past the end, a name that is not text, or empty, or with control
        // characters, no invitation, or more bytes than an offer has.
        assert_eq!(offered(&[]), None);
        assert_eq!(offered(&[1]), None);
        let mut other = made.clone();
        other[0] = 3;
        assert_eq!(offered(&other), None);
        other[0] = 0;
        assert_eq!(offered(&other), None);
        assert_eq!(offered(&[1, 5, b'a', b'b']), None);
        assert_eq!(offered(&[1, 2, 0xff, 0xfe, 7]), None);
        assert_eq!(offered(&[1, 0, 7]), None);
        assert_eq!(offered(&[1, 2, b' ', b' ', 7]), None);
        assert_eq!(offered(&[1, 2, b'a', b'\n', 7]), None);
        assert_eq!(offered(&[1, 1, b'a']), None);
        assert_eq!(
            offered(
                &[1, 33]
                    .iter()
                    .copied()
                    .chain([b'a'; 40])
                    .collect::<Vec<_>>()
            ),
            None
        );
        let mut longer = offer("a", &[0; OFFER_INVITATION_BYTES], Takes::Pairing).unwrap();
        longer.extend_from_slice(&[0; 40]);
        assert_eq!(offered(&longer), None);
    }

    #[test]
    fn a_measured_strength_sets_how_near_near_is_within_bounds() {
        assert_eq!(near_at_from(-40), -48);
        assert_eq!(near_at_from(-10), NEAR_AT_STRONGEST);
        assert_eq!(near_at_from(-99), NEAR_AT_WEAKEST);
        assert_eq!(near_at_from(i16::MIN), NEAR_AT_WEAKEST);
        assert_eq!(Rule::brought_near(None), Rule::BROUGHT_NEAR);
        let set = Rule::brought_near(Some(-70));
        assert_eq!((set.near_at, set.gone_below), (-70, -85));
        assert_eq!(Rule::brought_near(Some(0)).near_at, NEAR_AT_STRONGEST);
        // A device that is not near by the application's own threshold is near by one a person set lower, and the other
        // way about.
        let a = peer("device-a");
        let mut nearby = Nearby::new(Rule::BROUGHT_NEAR);
        for at in [0, 1_000, 2_000] {
            nearby.reading(&a, at, -66);
        }
        assert!(nearby.near(2_000).is_empty());
        nearby.rule(set);
        for at in [2_500, 3_500, 4_500] {
            nearby.reading(&a, at, -66);
        }
        assert_eq!(nearby.near(4_500), vec![a]);
    }
}
