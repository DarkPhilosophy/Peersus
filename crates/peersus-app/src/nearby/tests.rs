use super::*;
use peersus_core::nearby::{NEAR_AFTER_MS, NEAR_AT, PERIOD_SECONDS};

/// A radio that does what it is told and hears what the test says.
#[derive(Default)]
struct Told {
    announced: Option<[u8; TAG_BYTES]>,
    offered: Option<Vec<u8>>,
    listening: bool,
    air: Vec<Reading>,
    refuses: Option<Trouble>,
    short: Option<Trouble>,
    deaf: Option<Trouble>,
}

impl Radio for Told {
    async fn announce(&mut self, tag: Option<[u8; TAG_BYTES]>) -> Result<(), Trouble> {
        self.announced = tag;
        match (tag, self.refuses) {
            (Some(_), Some(trouble)) => Err(trouble),
            _ => Ok(()),
        }
    }
    async fn offer(&mut self, offer: Option<&[u8]>) -> Result<(), Trouble> {
        match (offer, self.short) {
            (Some(_), Some(trouble)) => {
                self.offered = None;
                Err(trouble)
            }
            _ => {
                self.offered = offer.map(<[u8]>::to_vec);
                Ok(())
            }
        }
    }
    async fn listen(&mut self, on: bool) -> Result<(), Trouble> {
        match (on, self.deaf) {
            (true, Some(trouble)) => {
                self.listening = false;
                Err(trouble)
            }
            _ => {
                self.listening = on;
                Ok(())
            }
        }
    }
    fn heard(&mut self) -> Vec<Reading> {
        std::mem::take(&mut self.air)
    }
}

fn peer(name: &str) -> PeerId {
    PeerId(name.to_owned())
}

const AT: u64 = 100 * PERIOD_SECONDS + 3;

fn tagged(peer: &PeerId, strength: i16) -> Reading {
    Reading {
        kind: Kind::Tag,
        data: tag(peer, period(AT)).to_vec(),
        strength,
    }
}

fn at(on: bool, in_front: bool, paired: &[PeerId], now_ms: u64) -> Now<'_> {
    Now {
        on,
        in_front,
        paired,
        seeking: false,
        offer: None,
        near_at: None,
        measuring: false,
        unix_seconds: AT,
        now_ms,
    }
}

/// A device that says it takes files: who it is, and what it puts on the air.
fn offering(name: &str, made_at: u64) -> (PeerId, Vec<u8>) {
    offering_as(name, made_at, Takes::Files)
}

/// A device that announces an offer for `takes`: who it is, and what it puts on the air.
fn offering_as(name: &str, made_at: u64, takes: Takes) -> (PeerId, Vec<u8>) {
    let address = iroh::EndpointAddr::new(iroh::SecretKey::generate().public())
        .with_ip_addr("192.0.2.12:4567".parse().unwrap());
    said_by(address, name, made_at, takes)
}

/// What the device at `address` puts on the air when it announces an offer for `takes`, and who it is.
fn said_by(
    address: iroh::EndpointAddr,
    name: &str,
    made_at: u64,
    takes: Takes,
) -> (PeerId, Vec<u8>) {
    let invitation = Invitation::create(address, made_at).unwrap();
    let data = offer(
        name,
        &invitation.to_radio(OFFER_INVITATION_BYTES).unwrap(),
        takes,
    )
    .unwrap();
    (PeerId(invitation.address.id.to_string()), data)
}

fn offer_of(data: &[u8], strength: i16) -> Reading {
    Reading {
        kind: Kind::Offer,
        data: data.to_vec(),
        strength,
    }
}

#[tokio::test]
async fn with_the_switch_off_nothing_is_announced_offered_or_listened_for() {
    let mut finder = Finder::new(peer("own"), Told::default());
    // On first, so that off is seen to undo it.
    let _ = finder
        .look(Now {
            seeking: true,
            offer: Some(b"this device"),
            ..at(true, true, &[], 0)
        })
        .await;
    assert!(finder.radio.announced.is_some() && finder.radio.listening);
    assert!(finder.radio.offered.is_some());
    let found = finder
        .look(Now {
            seeking: true,
            offer: Some(b"this device"),
            ..at(false, true, &[], 500)
        })
        .await;
    assert_eq!(found, Found::default());
    assert_eq!(finder.radio.announced, None);
    assert_eq!(finder.radio.offered, None);
    assert!(!finder.radio.listening);
}

#[tokio::test]
async fn it_announces_its_own_tag_of_now_and_listens_only_with_a_window_in_front() {
    let own = peer("own");
    let mut finder = Finder::new(own.clone(), Told::default());
    let found = finder.look(at(true, false, &[], 0)).await;
    assert_eq!(found, Found::default());
    assert_eq!(finder.radio.announced, Some(tag(&own, period(AT))));
    assert!(
        !finder.radio.listening,
        "it listened with no window to show it"
    );
    let _ = finder
        .look(Now {
            unix_seconds: AT + PERIOD_SECONDS,
            ..at(true, true, &[], 500)
        })
        .await;
    assert_eq!(
        finder.radio.announced,
        Some(tag(&own, period(AT) + 1)),
        "the tag did not turn with the period"
    );
    assert!(finder.radio.listening);
}

#[tokio::test]
async fn a_paired_device_heard_strongly_for_a_while_is_near_and_a_stranger_never_is() {
    let (paired, stranger) = (peer("paired"), peer("stranger"));
    let known = [paired.clone()];
    let mut finder = Finder::new(peer("own"), Told::default());
    let mut found = Found::default();
    for now in (0..=NEAR_AFTER_MS).step_by(500) {
        finder.radio.air = vec![
            tagged(&paired, NEAR_AT),
            tagged(&stranger, -20),
            Reading {
                kind: Kind::Tag,
                data: vec![1, 2, 3],
                strength: -20,
            },
            Reading {
                kind: Kind::Tag,
                data: Vec::new(),
                strength: -20,
            },
        ];
        found = finder.look(at(true, true, &known, now)).await;
        if now < NEAR_AFTER_MS {
            assert!(found.near.is_empty(), "near after only {now} ms");
        }
    }
    assert_eq!(
        found,
        Found {
            near: vec![paired],
            ..Found::default()
        }
    );
    // The window goes behind: what was near is not kept for when it comes back.
    let found = finder
        .look(at(true, false, &known, NEAR_AFTER_MS + 500))
        .await;
    assert!(found.near.is_empty());
    let found = finder
        .look(at(true, true, &known, NEAR_AFTER_MS + 1_000))
        .await;
    assert!(found.near.is_empty(), "nearness outlived the listening");
}

#[tokio::test]
async fn what_the_radio_refuses_is_told_and_what_it_can_still_do_is_done() {
    let paired = peer("paired");
    let known = [paired.clone()];
    // It cannot announce but can listen: the trouble is told, and a device is still found.
    let mut finder = Finder::new(
        peer("own"),
        Told {
            refuses: Some(Trouble::Failed),
            ..Told::default()
        },
    );
    let mut found = Found::default();
    for now in (0..=NEAR_AFTER_MS).step_by(500) {
        finder.radio.air = vec![tagged(&paired, -30)];
        found = finder.look(at(true, true, &known, now)).await;
    }
    assert_eq!(
        found,
        Found {
            near: vec![paired.clone()],
            trouble: Some(Trouble::Failed),
            ..Found::default()
        }
    );
    // It cannot listen: nothing is near, whatever is in the air, and the reason is told.
    let mut finder = Finder::new(
        peer("own"),
        Told {
            deaf: Some(Trouble::NotAllowed),
            ..Told::default()
        },
    );
    for now in (0..=NEAR_AFTER_MS).step_by(500) {
        finder.radio.air = vec![tagged(&paired, -30)];
        found = finder.look(at(true, true, &known, now)).await;
    }
    assert_eq!(
        found,
        Found {
            trouble: Some(Trouble::NotAllowed),
            ..Found::default()
        }
    );
    // With the switch off there is no trouble to tell.
    assert_eq!(
        finder.look(at(false, true, &known, 5_000)).await,
        Found::default()
    );
}

#[tokio::test]
async fn a_device_that_offers_to_pair_is_listed_only_while_this_one_looks_and_only_if_not_paired() {
    let (ana, ana_says) = offering_as("Ana", AT - 10, Takes::Pairing);
    let (known, known_says) = offering("Paired already", AT - 10);
    let (own, own_says) = offering("This one", AT - 10);
    let (_, old_says) = offering("Ran out", AT - 400);
    let paired = [known.clone()];
    let air = |strength: i16| {
        vec![
            offer_of(&ana_says, strength),
            offer_of(&known_says, -40),
            offer_of(&own_says, -40),
            offer_of(&old_says, -40),
            offer_of(&[1, 3, b'B', b'o', b'b'], -40),
            offer_of(&[], -40),
        ]
    };
    let mut finder = Finder::new(own, Told::default());
    // Nobody looks here: offers in the air list nobody, and this device offers nothing.
    for now in [0, 1_000, 2_000] {
        finder.radio.air = air(-78);
        let found = finder.look(at(true, true, &paired, now)).await;
        assert_eq!(found, Found::default());
        assert_eq!(finder.radio.offered, None);
    }
    // A window looks: this device offers, and Ana, heard twice at a strength that is far from brought near, is
    // listed by the name she gave, with her invitation kept for the owner.
    let mut found = Found::default();
    for now in [3_000, 4_500] {
        finder.radio.air = air(-78);
        found = finder
            .look(Now {
                seeking: true,
                offer: Some(b"this device's offer"),
                ..at(true, true, &paired, now)
            })
            .await;
    }
    assert_eq!(
        finder.radio.offered.as_deref(),
        Some(&b"this device's offer"[..])
    );
    assert_eq!(
        found,
        Found {
            strangers: vec![Stranger {
                id: ana.clone(),
                name: "Ana".to_owned(),
                receives: false,
                near: false,
            }],
            ..Found::default()
        }
    );
    let invitations = finder.invitations(&found.strangers);
    assert_eq!(invitations.len(), 1);
    assert_eq!(invitations[0].0, ana);
    assert!(
        Invitation::decode(&invitations[0].1, AT).is_ok_and(|invitation| {
            PeerId(invitation.address.id.to_string()) == ana && !invitation.address.addrs.is_empty()
        })
    );
    // She is paired meanwhile: no longer a stranger, though her offer is still in the air.
    let now_paired = [known.clone(), ana.clone()];
    finder.radio.air = air(-78);
    let found = finder
        .look(Now {
            seeking: true,
            offer: Some(b"this device's offer"),
            ..at(true, true, &now_paired, 5_000)
        })
        .await;
    assert!(found.strangers.is_empty());
    // The window stops looking: the offer is taken back and nobody is listed, whatever is in the air.
    finder.radio.air = air(-78);
    let found = finder.look(at(true, true, &paired, 5_500)).await;
    assert_eq!(found, Found::default());
    assert_eq!(finder.radio.offered, None);
}

#[tokio::test]
async fn the_air_full_of_offers_lists_no_more_than_a_person_reads_and_a_radio_that_cannot_offer_says_so()
 {
    let many: Vec<Vec<u8>> = (0..MOST_STRANGERS + 8)
        .map(|number| offering(&format!("Device {number}"), AT - 10).1)
        .collect();
    let mut finder = Finder::new(peer("own"), Told::default());
    let mut found = Found::default();
    for now in [0, 1_500] {
        finder.radio.air = many.iter().map(|data| offer_of(data, -60)).collect();
        found = finder
            .look(Now {
                seeking: true,
                offer: Some(b"offer"),
                ..at(true, true, &[], now)
            })
            .await;
    }
    assert_eq!(found.strangers.len(), MOST_STRANGERS);
    // A radio that has only the older kind of announcement: said, and it still lists who it hears.
    let (ana, ana_says) = offering("Ana", AT - 10);
    let mut finder = Finder::new(
        peer("own"),
        Told {
            short: Some(Trouble::Failed),
            ..Told::default()
        },
    );
    for now in [0, 1_500] {
        finder.radio.air = vec![offer_of(&ana_says, -60)];
        found = finder
            .look(Now {
                seeking: true,
                offer: Some(b"offer"),
                ..at(true, true, &[], now)
            })
            .await;
    }
    assert_eq!(found.trouble, Some(Trouble::Failed));
    assert_eq!(found.strangers.len(), 1);
    assert_eq!(found.strangers[0].id, ana);
}

#[tokio::test]
async fn a_device_that_takes_files_is_brought_near_by_its_tag_at_the_strength_the_person_set_and_one_that_only_pairs_never()
 {
    let (ana, ana_says) = offering("Ana", AT - 10);
    // To be paired with is another thing than to take files: such a device is listed, and nothing is sent to it.
    let (old, old_says) = offering_as("Pairs only", AT - 10, Takes::Pairing);
    let mut finder = Finder::new(peer("own"), Told::default());
    // Both are heard by their offers, always at the same strength, and by their tags at `strength`.
    async fn look(
        finder: &mut Finder<Told>,
        devices: [(&PeerId, &[u8]); 2],
        now: u64,
        offer_strength: i16,
        strength: i16,
        near_at: Option<i16>,
    ) -> Found {
        finder.radio.air = devices
            .iter()
            .flat_map(|(id, says)| [offer_of(says, offer_strength), tagged(id, strength)])
            .collect();
        finder
            .look(Now {
                seeking: true,
                offer: Some(b"offer"),
                near_at,
                ..at(true, true, &[], now)
            })
            .await
    }
    let says = [(&ana, &ana_says[..]), (&old, &old_says[..])];
    let near = |found: &Found| -> Vec<(PeerId, bool, bool)> {
        let mut all: Vec<_> = found
            .strangers
            .iter()
            .map(|stranger| (stranger.id.clone(), stranger.receives, stranger.near))
            .collect();
        all.sort_by_key(|(id, ..)| id != &ana);
        all
    };
    // Around, and far from near by the application's own threshold.
    let mut found = Found::default();
    for now in [0, 1_000, 2_000, 3_000] {
        found = look(&mut finder, says, now, -80, -66, None).await;
    }
    assert_eq!(
        near(&found),
        vec![(ana.clone(), true, false), (old.clone(), false, false)]
    );
    // The person set near where these two devices are: the one that takes files is near, the other is listed only.
    for now in [3_500, 4_500, 5_500] {
        found = look(&mut finder, says, now, -80, -66, Some(-70)).await;
    }
    assert_eq!(
        near(&found),
        vec![(ana.clone(), true, true), (old.clone(), false, false)]
    );
    // Moved away, past what keeps a near device near, for long enough: listed still, near no longer.
    for now in [6_500, 7_500, 8_500, 9_500] {
        found = look(&mut finder, says, now, -80, -88, Some(-70)).await;
    }
    assert_eq!(
        near(&found),
        vec![(ana.clone(), true, false), (old.clone(), false, false)]
    );
    // An offer heard strongly makes nobody near: near is said by the tag, which is what a person measures by. Nor
    // does a strength no radio hears at, which is a system's word for "not known".
    for now in [10_500, 11_500, 12_500, 13_500] {
        found = look(&mut finder, says, now, -40, -88, Some(-70)).await;
    }
    assert_eq!(
        near(&found),
        vec![(ana.clone(), true, false), (old.clone(), false, false)]
    );
    for now in [14_500, 15_500, 16_500, 17_500] {
        found = look(&mut finder, says, now, -80, 127, Some(-70)).await;
    }
    assert_eq!(near(&found), vec![(ana, true, false), (old, false, false)]);
}

#[tokio::test]
async fn a_window_open_on_a_page_that_lets_nobody_find_this_device_announces_no_offer_and_still_hears_them()
 {
    let (ana, ana_says) = offering("Ana", AT - 10);
    let mut finder = Finder::new(peer("own"), Told::default());
    let mut found = Found::default();
    for now in [0, 1_500] {
        finder.radio.air = vec![offer_of(&ana_says, -60)];
        found = finder
            .look(Now {
                seeking: true,
                ..at(true, true, &[], now)
            })
            .await;
    }
    assert_eq!(finder.radio.offered, None);
    assert_eq!(found.strangers.len(), 1);
    assert_eq!(found.strangers[0].id, ana);
}

#[tokio::test]
async fn a_paired_device_is_where_files_go_only_while_it_is_near_and_says_it_takes_files() {
    let address = iroh::EndpointAddr::new(iroh::SecretKey::generate().public())
        .with_ip_addr("192.0.2.12:4567".parse().unwrap());
    let (paired, takes_files) = said_by(address.clone(), "Paired", AT - 10, Takes::Files);
    let (_, pairs) = said_by(address, "Paired", AT - 10, Takes::Pairing);
    let known = [paired.clone()];
    let mut finder = Finder::new(peer("own"), Told::default());
    async fn look(
        finder: &mut Finder<Told>,
        known: &[PeerId],
        air: Vec<Reading>,
        now: u64,
    ) -> Found {
        finder.radio.air = air;
        finder
            .look(Now {
                seeking: true,
                ..at(true, true, known, now)
            })
            .await
    }
    // Near by its tag, and it says nothing of what it takes (its window shows a page that is neither): near, no more.
    let mut found = Found::default();
    for now in (0..=NEAR_AFTER_MS).step_by(500) {
        found = look(&mut finder, &known, vec![tagged(&paired, -40)], now).await;
    }
    assert_eq!(found.near, vec![paired.clone()]);
    assert!(found.receiving.is_empty());
    // It says it is there to be paired with, which is another thing: still nothing goes to it.
    let mut now = NEAR_AFTER_MS + 500;
    found = look(
        &mut finder,
        &known,
        vec![tagged(&paired, -40), offer_of(&pairs, -45)],
        now,
    )
    .await;
    assert!(found.receiving.is_empty());
    // Its person opens Receive: it says it takes files, and from that look on it is where files go.
    now += 500;
    found = look(
        &mut finder,
        &known,
        vec![tagged(&paired, -40), offer_of(&takes_files, -45)],
        now,
    )
    .await;
    assert_eq!(found.receiving, vec![paired.clone()]);
    assert!(
        found.strangers.is_empty(),
        "a paired device was listed to pair with"
    );
    // Its person leaves Receive: what it says now is believed at once.
    now += 500;
    found = look(
        &mut finder,
        &known,
        vec![tagged(&paired, -40), offer_of(&pairs, -45)],
        now,
    )
    .await;
    assert_eq!(found.near, vec![paired.clone()]);
    assert!(found.receiving.is_empty());
    // Receive again, and then its offer is heard no more (its window closed) while its tag still is: what it said
    // last holds for a moment and no longer.
    now += 500;
    found = look(
        &mut finder,
        &known,
        vec![tagged(&paired, -40), offer_of(&takes_files, -45)],
        now,
    )
    .await;
    assert_eq!(found.receiving, vec![paired.clone()]);
    let said_at = now;
    while now < said_at + TAKING_HEARD_WITHIN_MS {
        now += 500;
        found = look(&mut finder, &known, vec![tagged(&paired, -40)], now).await;
        if now < said_at + TAKING_HEARD_WITHIN_MS {
            assert_eq!(found.receiving, vec![paired.clone()], "at {now}");
        }
    }
    assert_eq!(found.near, vec![paired.clone()]);
    assert!(found.receiving.is_empty());
    // Taking files but not near (heard weakly by its tag): nothing goes to a device across the room.
    let mut finder = Finder::new(peer("own"), Told::default());
    for now in (0..=2 * NEAR_AFTER_MS).step_by(500) {
        found = look(
            &mut finder,
            &known,
            vec![tagged(&paired, -80), offer_of(&takes_files, -30)],
            now,
        )
        .await;
    }
    assert!(found.near.is_empty() && found.receiving.is_empty());
    // A device that is not paired and says it takes files is never told as a paired one that does.
    let (stranger, stranger_says) = offering("Stranger", AT - 10);
    let mut finder = Finder::new(peer("own"), Told::default());
    for now in (0..=2 * NEAR_AFTER_MS).step_by(500) {
        found = look(
            &mut finder,
            &known,
            vec![tagged(&stranger, -40), offer_of(&stranger_says, -45)],
            now,
        )
        .await;
    }
    assert!(found.receiving.is_empty());
    // This window stops being open: nothing that was heard is kept for when it is again.
    let mut finder = Finder::new(peer("own"), Told::default());
    for now in (0..=NEAR_AFTER_MS).step_by(500) {
        found = look(
            &mut finder,
            &known,
            vec![tagged(&paired, -40), offer_of(&takes_files, -45)],
            now,
        )
        .await;
    }
    assert_eq!(found.receiving, vec![paired.clone()]);
    finder.radio.air = vec![tagged(&paired, -40)];
    let found = finder
        .look(at(true, true, &known, NEAR_AFTER_MS + 500))
        .await;
    assert_eq!(found.near, vec![paired]);
    assert!(found.receiving.is_empty());
}

#[tokio::test]
async fn the_strongest_device_heard_is_told_only_while_a_person_measures() {
    let (paired, stranger) = (peer("paired"), peer("not paired"));
    let known = [paired.clone()];
    let mut finder = Finder::new(peer("own"), Told::default());
    let air = || {
        vec![
            tagged(&paired, -71),
            tagged(&stranger, -48),
            // What a system says when it does not know the strength: no reading, or it would be the strongest of all.
            tagged(&stranger, 127),
            // Not a tag of this application's: nothing to measure by, however strong.
            Reading {
                kind: Kind::Tag,
                data: vec![1, 2, 3],
                strength: -20,
            },
        ]
    };
    finder.radio.air = air();
    let found = finder.look(at(true, true, &known, 0)).await;
    assert_eq!(found.strongest, None);
    finder.radio.air = air();
    let measuring = |now_ms| Now {
        measuring: true,
        ..at(true, true, &known, now_ms)
    };
    let found = finder.look(measuring(500)).await;
    assert_eq!(found.strongest, Some(-48));
    assert!(!found.measuring, "an adapter said a person measures");
    // Not heard again: it counts for a moment, and then nothing is the strongest.
    assert_eq!(finder.look(measuring(1_500)).await.strongest, Some(-48));
    assert_eq!(finder.look(measuring(2_100)).await.strongest, None);
    // The person stopped: nothing is kept for the next time.
    finder.radio.air = air();
    let _ = finder.look(measuring(2_600)).await;
    assert_eq!(
        finder.look(at(true, true, &known, 3_000)).await.strongest,
        None
    );
}

#[test]
fn only_this_applications_two_services_are_known_and_each_with_its_own_length() {
    use peersus_core::nearby::{OFFER_BYTES, OFFER_SERVICE, SERVICE};
    assert_eq!(kind_of(SERVICE), Some((Kind::Tag, MOST_TAG)));
    assert_eq!(
        kind_of(&OFFER_SERVICE.to_ascii_uppercase()),
        Some((Kind::Offer, OFFER_BYTES))
    );
    assert_eq!(kind_of("0000180f-0000-1000-8000-00805f9b34fb"), None);
}
