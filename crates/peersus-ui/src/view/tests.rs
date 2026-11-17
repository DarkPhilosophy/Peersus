//! The view's rules: what is shown, said and offered for each state of the application.
use super::*;
use peersus_core::model::{PairingPrompt, PeerId};

/// What the overlay says of an offer, in English.
fn offer_words(offer: &Progress) -> String {
    offer_words_in(Language::ENGLISH, offer)
}

/// The lines under the circle for exactly these chosen files.
fn chosen_words(names: &[String]) -> Vec<String> {
    chosen_words_total_in(Language::ENGLISH, names, names.len())
}

/// One line of the history under a device, in English.
fn history_line(item: &Progress) -> String {
    history_line_in(Language::ENGLISH, item)
}

/// The Settings search as the page asks it, with nothing an installation adds.
fn search_settings(
    query: &str,
    view: &View,
    language: Language,
) -> Vec<crate::settings_search::Entry> {
    crate::settings_search::search(
        query,
        view,
        language,
        crate::settings_search::Features::default(),
    )
}

#[test]
fn a_size_is_said_in_the_unit_that_fits_it() {
    assert_eq!(size_in_words(0), "0 B");
    assert_eq!(size_in_words(1023), "1023 B");
    assert_eq!(size_in_words(1024), "1.0 KB");
    assert_eq!(size_in_words(1536), "1.5 KB");
    assert_eq!(size_in_words(5 * 1024 * 1024), "5.0 MB");
    assert_eq!(size_in_words(3 * 1024 * 1024 * 1024), "3.0 GB");
    // Past the last unit the number grows and the unit stays, never a unit that does not exist.
    assert_eq!(size_in_words(2048 * 1024 * 1024 * 1024 * 1024), "2048.0 TB");
}

#[test]
fn the_list_of_an_offer_holds_every_file_that_waits_and_only_those() {
    let waiting = |id: &str| Progress {
        bytes: 10,
        ..row(id, true, Phase::Waiting, None)
    };
    let view = View {
        transfers: vec![
            waiting("a"),
            Progress {
                ..row("done", true, Phase::Done, None)
            },
            waiting("b"),
            // Went out and waits for the other device to answer: not something this person answers.
            row("out", false, Phase::Waiting, None),
        ],
        ..View::default()
    };
    let offered = offered_now(&view).expect("two files wait");
    let ids: Vec<&str> = offered.files.iter().map(|file| file.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(offered.waiting, 2);
}

#[test]
fn the_fuse_of_a_code_burns_from_whole_to_nothing_over_the_life_the_owner_gave_it() {
    // A code shown at 1000 that the owner ends at 1300: five minutes, as the owner gives them today.
    assert_eq!(code_fuse(1_000, 1_300, 1_000), 1.);
    assert_eq!(code_fuse(1_000, 1_300, 1_150), 0.5);
    assert_eq!(code_fuse(1_000, 1_300, 1_300), 0.);
    // The life is the owner's, not a number of the window's: a code of ten minutes burns over ten, and one of a minute over one.
    assert_eq!(code_fuse(0, 600, 300), 0.5);
    assert_eq!(code_fuse(0, 60, 15), 0.75);
}

#[test]
fn the_fuse_never_goes_below_nothing_nor_above_whole_and_never_divides_by_nothing() {
    // A code long past its end, or a clock that went back before the code was shown: nothing, and a whole, and never outside them.
    assert_eq!(
        code_fuse(1_000, 1_300, 9_999),
        0.,
        "an old code gave a negative share"
    );
    assert_eq!(
        code_fuse(1_000, 1_300, 500),
        1.,
        "a clock that went back gave more than a whole"
    );
    // A deadline that is not after the moment of showing is a code with no life, never a division by zero or a whole fuse.
    assert_eq!(code_fuse(1_000, 1_000, 1_000), 0.);
    assert_eq!(
        code_fuse(1_000, 500, 1_000),
        0.,
        "a deadline before the code was shown gave a fuse"
    );
    // It only ever burns down as the clock goes on.
    let mut last = 1.;
    for now in 1_000..=1_300 {
        let left = code_fuse(1_000, 1_300, now);
        assert!(left <= last, "the fuse grew back at {now}");
        last = left;
    }
}

#[test]
fn the_tip_of_the_fuse_is_at_the_top_when_whole_and_goes_round_clockwise_as_it_burns() {
    let near = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
    // Whole: the tip is where it began, at the top of a ring of radius 100 centred on (100, 100).
    assert!(near(fuse_tip(1., 100., 100.), (100., 0.)));
    // Three quarters left: a quarter has burned, so the tip has gone a quarter round clockwise from the top, to the right.
    assert!(near(fuse_tip(0.75, 100., 100.), (200., 100.)));
    // Half left: at the bottom. A quarter left: at the left.
    assert!(near(fuse_tip(0.5, 100., 100.), (100., 200.)));
    assert!(near(fuse_tip(0.25, 100., 100.), (0., 100.)));
}

#[test]
fn the_lit_arc_starts_exactly_where_the_tip_is_and_sweeps_the_share_that_is_left() {
    let near = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-2 && (a.1 - b.1).abs() < 1e-2;
    // On a ring of radius 100 about (100, 100), the point a Skia angle (0 at three o'clock, clockwise) is at.
    let at = |degrees: f32| {
        let radians = degrees.to_radians();
        (100. + 100. * radians.cos(), 100. + 100. * radians.sin())
    };
    for left in [1., 0.9, 0.75, 0.5, 0.25, 0.1, 0.01] {
        let (start, sweep) = fuse_arc(left).unwrap();
        // The line begins on the tip: the same place the sparks fly from, for any share left.
        assert!(
            near(at(start), fuse_tip(left, 100., 100.)),
            "the arc began away from the tip at {left}"
        );
        assert!(
            (sweep - 360. * left).abs() < 1e-3,
            "it swept {sweep} degrees for a share of {left}"
        );
        // And its end is the top, where the burning began, for every share.
        assert!(
            near(at(start + sweep), (100., 0.)),
            "the arc did not end at the top at {left}"
        );
    }
}

#[test]
fn the_lit_arc_is_the_whole_turn_when_whole_and_nothing_when_gone_or_not_a_number() {
    assert_eq!(fuse_arc(1.), Some((-90., 360.)));
    // More than whole is no more than whole.
    assert_eq!(fuse_arc(7.), Some((-90., 360.)));
    for gone in [0., -0.0, -1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(fuse_arc(gone), None, "{gone} drew a line");
    }
}

#[test]
fn particles_leave_the_tip_in_every_direction_fall_and_fade_until_gone_and_are_the_same_for_the_same_frame()
 {
    let a = fuse_particles(300);
    let b = fuse_particles(300);
    assert_eq!(a, b, "the same frame gave other particles");
    assert!(!a.is_empty());
    for particle in &a {
        assert!(
            (0. ..=1.).contains(&particle.alpha),
            "alpha out of range: {}",
            particle.alpha
        );
        assert!(particle.size > 0.);
        assert!(particle.age < FUSE_PARTICLE_FRAMES);
    }
    // One that was born this very frame is on the tip, whole.
    let newborn: Vec<_> = a.iter().filter(|p| p.age == 0).collect();
    assert_eq!(newborn.len(), FUSE_BORN_PER_FRAME);
    assert!(
        newborn
            .iter()
            .all(|p| p.x == 0. && p.y == 0. && p.alpha == 1.)
    );
    // Over many frames they go off in every direction, not all one way: both sides and both above and below the tip are used.
    let all: Vec<_> = (0..40)
        .flat_map(|frame| fuse_particles(frame + 100))
        .collect();
    assert!(
        all.iter().any(|p| p.x > 5.) && all.iter().any(|p| p.x < -5.),
        "particles went only one way sideways"
    );
    assert!(
        all.iter().any(|p| p.y > 5.) && all.iter().any(|p| p.y < -5.),
        "particles went only one way up and down"
    );
    // Gravity: an older particle is lower than a straight flight would put it.
    assert!(a.iter().filter(|p| p.age > 20).count() > 0);
}

#[test]
fn nothing_of_the_fuse_can_be_drawn_on_the_disc_that_holds_the_code() {
    // A portal of the size of the prototype: the disc is 384 across, the ring of the fuse sits at 418 / 2 less 3.
    let (disc, ring, centre) = (384. / 2., 418. / 2. - 3., 420. / 2.);
    for step in 0..=100 {
        let left = step as f32 / 100.;
        let (tip_x, tip_y) = fuse_tip(left, ring, centre);
        // The tip itself is on the ring, which lies outside the disc by more than the size of the dot drawn on it.
        assert!(
            !point_on_disc(tip_x, tip_y, centre, disc, 8.),
            "the tip lay on the disc at {left}"
        );
        // And of the sparks that are born at it, none that would lie on the disc is let through.
        for frame in [66, 100, 150, 400] {
            for particle in fuse_particles(frame) {
                let (x, y) = (tip_x + particle.x, tip_y + particle.y);
                let drawn = !point_on_disc(x, y, centre, disc, 2.);
                if !drawn {
                    continue;
                }
                assert!(
                    (x - centre).hypot(y - centre) > disc,
                    "a spark that is drawn lay on the disc at {left}, frame {frame}"
                );
            }
        }
    }
}

fn transfer(bytes: u64, done: u64, state: TransferState) -> Transfer {
    Transfer {
        id: "t".into(),
        peer: PeerId("p".into()),
        peer_name: None,
        name: "file.bin".into(),
        folders: Vec::new(),
        bytes,
        completed_bytes: done,
        incoming: false,
        state,
        started_at: 0,
        code: None,
        expires_at: None,
        saved_as: None,
    }
}
fn percent(bytes: u64, done: u64, state: TransferState) -> u8 {
    progress(&transfer(bytes, done, state)).percent
}

/// A transfer with its own id, name and device, for the lists of the tests below.
fn listed(
    id: &str,
    name: &str,
    peer: &str,
    incoming: bool,
    bytes: u64,
    done: u64,
    state: TransferState,
) -> Progress {
    let mut made = transfer(bytes, done, state);
    made.id = id.into();
    made.name = name.into();
    made.peer = PeerId(peer.into());
    made.incoming = incoming;
    progress(&made)
}
fn ids(items: &[&Progress]) -> Vec<String> {
    items.iter().map(|item| item.id.clone()).collect()
}
fn history() -> Vec<Progress> {
    vec![
        listed(
            "a",
            "Holiday.mp4",
            "peer-laptop",
            false,
            100,
            100,
            TransferState::Complete,
        ),
        listed(
            "b",
            "invoice.pdf",
            "peer-phone",
            true,
            10,
            10,
            TransferState::Complete,
        ),
        listed(
            "c",
            "notes.txt",
            "peer-laptop",
            false,
            5,
            0,
            TransferState::Cancelled,
        ),
        listed(
            "d",
            "backup.zip",
            "peer-phone",
            true,
            50,
            0,
            TransferState::Failed {
                reason: "lost".into(),
            },
        ),
        listed(
            "e",
            "photo.jpg",
            "peer-phone",
            false,
            40,
            10,
            TransferState::Running,
        ),
    ]
}

#[test]
fn the_transfers_tab_lists_what_is_going_first_then_the_newest_of_what_is_over() {
    let items = history();
    assert_eq!(
        ids(&find_transfers(&items, "", TransferFilter::All)),
        ["e", "d", "c", "b", "a"]
    );
}

#[test]
fn each_filter_keeps_only_its_own_and_none_removes_anything_from_the_record() {
    let items = history();
    assert_eq!(
        ids(&find_transfers(&items, "", TransferFilter::Active)),
        ["e"]
    );
    assert_eq!(
        ids(&find_transfers(&items, "", TransferFilter::Done)),
        ["b", "a"]
    );
    assert_eq!(
        ids(&find_transfers(&items, "", TransferFilter::Stopped)),
        ["d", "c"]
    );
    assert_eq!(
        ids(&find_transfers(&items, "", TransferFilter::Incoming)),
        ["d", "b"]
    );
    assert_eq!(
        ids(&find_transfers(&items, "", TransferFilter::Outgoing)),
        ["e", "c", "a"]
    );
    assert_eq!(items.len(), 5, "a filter changed the record");
}

#[test]
fn every_filter_has_a_label_and_the_list_of_them_is_complete() {
    for filter in TransferFilter::ALL {
        assert!(!filter.label().is_empty());
    }
    // A new filter that is not in ALL could never be chosen: the match below stops compiling when one is added.
    for filter in TransferFilter::ALL {
        match filter {
            TransferFilter::All
            | TransferFilter::Active
            | TransferFilter::Done
            | TransferFilter::Stopped
            | TransferFilter::Incoming
            | TransferFilter::Outgoing => {}
        }
    }
    assert_eq!(TransferFilter::default(), TransferFilter::All);
}

#[test]
fn search_finds_by_file_name_whatever_the_case_and_ignores_the_spaces_around_it() {
    let items = history();
    assert_eq!(
        ids(&find_transfers(&items, "  HOLIDAY ", TransferFilter::All)),
        ["a"]
    );
    assert_eq!(
        ids(&find_transfers(&items, ".pdf", TransferFilter::All)),
        ["b"]
    );
    assert!(find_transfers(&items, "no such file", TransferFilter::All).is_empty());
}

#[test]
fn search_finds_by_the_name_the_sender_gave_and_by_the_start_of_the_device_identity() {
    let mut items = history();
    items[1].from = Some("Maria's phone".into());
    assert_eq!(
        ids(&find_transfers(&items, "maria", TransferFilter::All)),
        ["b"]
    );
    // The identity is matched from its start only: a letter in the middle of one is not a reason to list a device's files.
    assert_eq!(
        ids(&find_transfers(&items, "peer-lap", TransferFilter::All)),
        ["c", "a"]
    );
    assert!(find_transfers(&items, "laptop", TransferFilter::All).is_empty());
}

#[test]
fn search_and_filter_together_keep_only_what_passes_both() {
    let items = history();
    assert_eq!(
        ids(&find_transfers(
            &items,
            "peer-phone",
            TransferFilter::Incoming
        )),
        ["d", "b"]
    );
    assert_eq!(
        ids(&find_transfers(&items, "peer-phone", TransferFilter::Done)),
        ["b"]
    );
    assert!(find_transfers(&items, "holiday", TransferFilter::Incoming).is_empty());
}

#[test]
fn x_puts_away_what_is_over_and_only_when_nothing_is_going() {
    let items = history();
    assert!(
        put_away_now(&items).is_empty(),
        "X put things away in the middle of a transfer"
    );
    assert!(
        !can_put_away(&items, &[]),
        "X was offered in the middle of a transfer"
    );
    let over: Vec<Progress> = items
        .iter()
        .filter(|item| item.phase.is_over())
        .cloned()
        .collect();
    assert!(can_put_away(&over, &[]));
    assert_eq!(put_away_now(&over), ["a", "b", "c", "d"]);
}

#[test]
fn what_x_put_away_is_not_listed_but_is_never_removed_and_a_new_transfer_appears() {
    let over: Vec<Progress> = history()
        .into_iter()
        .filter(|item| item.phase.is_over())
        .collect();
    let put_away = put_away_now(&over);
    assert!(not_put_away(&over, &put_away).is_empty());
    assert!(
        !can_put_away(&over, &put_away),
        "X was offered with nothing left to put away"
    );
    // The record is still all there: the Transfers tab lists it.
    assert_eq!(find_transfers(&over, "", TransferFilter::All).len(), 4);
    // A new transfer has a new id, so it is listed, and X is not offered while it goes.
    let mut more = over.clone();
    more.push(listed(
        "f",
        "new.bin",
        "peer-laptop",
        false,
        8,
        2,
        TransferState::Running,
    ));
    assert_eq!(ids(&not_put_away(&more, &put_away)), ["f"]);
    assert!(!can_put_away(&more, &put_away));
    // When it ends it is over and not yet put away: listed, and X is offered again.
    more[4] = listed(
        "f",
        "new.bin",
        "peer-laptop",
        false,
        8,
        8,
        TransferState::Complete,
    );
    assert_eq!(ids(&not_put_away(&more, &put_away)), ["f"]);
    assert!(can_put_away(&more, &put_away));
}

#[test]
fn a_transfer_that_is_still_going_is_never_hidden_even_if_its_id_was_put_away() {
    let items = vec![listed(
        "x",
        "again.bin",
        "p",
        false,
        10,
        5,
        TransferState::Running,
    )];
    assert_eq!(ids(&not_put_away(&items, &["x".to_owned()])), ["x"]);
}

#[test]
fn the_circle_sums_only_what_is_going_and_names_the_one_moving_now() {
    let items = history();
    let all = overall(&items);
    // Only "photo.jpg" is going: 40 bytes, 25 % moved.
    assert_eq!(
        all,
        Overall {
            files: 1,
            bytes: 40,
            moved: 10,
            percent: 25,
            current: Some("photo.jpg".into())
        }
    );
}

#[test]
fn the_circle_adds_the_bytes_of_every_file_not_the_average_of_their_percentages() {
    let items = vec![
        listed("1", "small.bin", "p", false, 10, 10, TransferState::Running),
        listed("2", "large.bin", "p", false, 990, 0, TransferState::Running),
    ];
    let all = overall(&items);
    assert_eq!((all.files, all.bytes, all.moved), (2, 1000, 10));
    assert_eq!(
        all.percent, 1,
        "a tiny file that is done made the whole look half done"
    );
    assert_eq!(
        all.current.as_deref(),
        Some("small.bin"),
        "the first one that moves is the current one"
    );
}

#[test]
fn the_circle_names_the_first_waiting_file_when_none_is_moving_yet() {
    let items = vec![
        listed("1", "first.bin", "p", true, 10, 0, TransferState::Offered),
        listed("2", "second.bin", "p", true, 10, 0, TransferState::Offered),
    ];
    assert_eq!(overall(&items).current.as_deref(), Some("first.bin"));
}

#[test]
fn with_nothing_going_the_circle_has_nothing_to_say_and_never_divides_by_zero() {
    assert_eq!(
        overall(&[]),
        Overall {
            files: 0,
            bytes: 0,
            moved: 0,
            percent: 100,
            current: None
        }
    );
    let over = vec![listed(
        "a",
        "done.bin",
        "p",
        false,
        10,
        10,
        TransferState::Complete,
    )];
    assert_eq!(overall(&over).files, 0);
    // Files of nothing, going: complete, not NaN.
    let empty = vec![listed(
        "e",
        "empty.bin",
        "p",
        false,
        0,
        0,
        TransferState::Running,
    )];
    assert_eq!(overall(&empty).percent, 100);
}

#[test]
fn progress_is_the_share_of_the_bytes_that_have_moved() {
    assert_eq!(percent(200, 50, TransferState::Running), 25);
    assert_eq!(percent(200, 0, TransferState::Running), 0);
    assert_eq!(
        percent(3, 1, TransferState::Running),
        33,
        "rounds down, never up"
    );
}

fn going() -> Vec<Progress> {
    vec![
        listed("a", "done.bin", "p", false, 10, 10, TransferState::Complete),
        listed(
            "b",
            "moving.bin",
            "p",
            false,
            2048,
            1024,
            TransferState::Running,
        ),
    ]
}
fn all_over() -> Vec<Progress> {
    vec![
        listed("a", "one.bin", "p", false, 10, 10, TransferState::Complete),
        listed("b", "two.bin", "p", false, 10, 0, TransferState::Cancelled),
    ]
}

#[test]
fn while_something_is_going_the_button_only_folds_and_unfolds_and_never_hides() {
    let items = going();
    assert_eq!(
        header_action(&items, &[], false),
        Some(HeaderAction::Minimize)
    );
    assert_eq!(header_action(&items, &[], true), Some(HeaderAction::Expand));
    // Whatever was put away before changes nothing in the middle of a transfer.
    assert_eq!(
        header_action(&items, &["a".to_owned()], false),
        Some(HeaderAction::Minimize)
    );
}

#[test]
fn when_everything_is_over_the_button_is_x_and_it_is_not_there_once_all_is_put_away() {
    let items = all_over();
    assert_eq!(header_action(&items, &[], false), Some(HeaderAction::Hide));
    // Folded or not makes no difference when there is nothing going to fold.
    assert_eq!(header_action(&items, &[], true), Some(HeaderAction::Hide));
    let put_away = put_away_now(&items);
    assert_eq!(
        header_action(&items, &put_away, false),
        None,
        "a button that does nothing was shown"
    );
}

#[test]
fn with_nothing_at_all_there_is_no_button() {
    assert_eq!(header_action(&[], &[], false), None);
    assert_eq!(header_action(&[], &[], true), None);
}

#[test]
fn a_new_transfer_after_x_brings_the_fold_button_back_and_the_old_ones_stay_away() {
    let mut items = all_over();
    let put_away = put_away_now(&items);
    items.push(listed(
        "c",
        "new.bin",
        "p",
        false,
        100,
        10,
        TransferState::Running,
    ));
    assert_eq!(
        header_action(&items, &put_away, false),
        Some(HeaderAction::Minimize)
    );
    assert_eq!(ids(&not_put_away(&items, &put_away)), ["c"]);
    items[2] = listed(
        "c",
        "new.bin",
        "p",
        false,
        100,
        100,
        TransferState::Complete,
    );
    // The new one is over and not put away: X is offered again, for it alone.
    assert_eq!(
        header_action(&items, &put_away, false),
        Some(HeaderAction::Hide)
    );
    assert_eq!(ids(&not_put_away(&items, &put_away)), ["c"]);
}

#[test]
fn the_folded_line_says_how_many_how_far_how_big_and_which_one_is_moving() {
    assert_eq!(
        folded_line(&going()).as_deref(),
        Some("1 file · 50% of 2.0 KB · moving.bin")
    );
    let two = vec![
        listed(
            "a",
            "first.bin",
            "p",
            false,
            1024,
            0,
            TransferState::Running,
        ),
        listed(
            "b",
            "second.bin",
            "p",
            false,
            1024,
            0,
            TransferState::Offered,
        ),
    ];
    assert_eq!(
        folded_line(&two).as_deref(),
        Some("2 files · 0% of 2.0 KB · first.bin")
    );
}

#[test]
fn there_is_no_folded_line_when_nothing_is_going() {
    assert_eq!(folded_line(&[]), None);
    assert_eq!(folded_line(&all_over()), None);
}

#[test]
fn a_finished_transfer_reads_as_complete_whatever_the_counters_say() {
    assert_eq!(percent(200, 199, TransferState::Complete), 100);
}

#[test]
fn a_transfer_of_nothing_is_not_a_division_by_zero() {
    assert_eq!(percent(0, 0, TransferState::Running), 100);
}

#[test]
fn a_peer_that_reports_more_than_it_advertised_cannot_draw_past_the_end() {
    assert_eq!(percent(100, 5_000, TransferState::Running), 100);
}

#[test]
fn enormous_sizes_do_not_overflow() {
    // 2^63 bytes done of 2^63: a naive `done * 100` overflows u64.
    let huge = 1u64 << 63;
    assert_eq!(percent(huge, huge, TransferState::Running), 100);
    assert_eq!(percent(huge, huge / 2, TransferState::Running), 50);
}

fn ready(ids: &[&str]) -> SendState {
    SendState::Ready(ids.iter().map(|id| (*id).to_owned()).collect())
}

#[test]
fn no_device_picked_means_a_code_and_devices_picked_mean_those_devices_and_never_a_code() {
    assert_eq!(aim_of(&SendState::ByCode), Aim::Code);
    // The devices go in the order the person picked them, as `send_state` judged them.
    assert_eq!(
        aim_of(&ready(&["laptop", "phone"])),
        Aim::Devices(vec!["laptop".into(), "phone".into()])
    );
    assert_eq!(
        aim_of(&ready(&["phone"])),
        Aim::Devices(vec!["phone".into()])
    );
}

#[test]
fn a_file_that_waited_and_now_moves_or_arrived_was_accepted_and_nothing_else_was() {
    let file = |id: &str, incoming: bool, phase: Phase| Progress {
        id: id.into(),
        incoming,
        phase,
        ..sample_offer(0).offer
    };
    let waited = [
        file("a", true, Phase::Waiting),
        file("out", false, Phase::Waiting),
    ];
    assert!(accepted_between(&waited, &[file("a", true, Phase::Moving)]));
    // A small file can arrive whole between two views.
    assert!(accepted_between(&waited, &[file("a", true, Phase::Done)]));
    for not_accepted in [
        Phase::Waiting,
        Phase::Cancelled,
        Phase::Failed("refused".into()),
    ] {
        assert!(!accepted_between(&waited, &[file("a", true, not_accepted)]));
    }
    // One going out that the other device accepted is not this person's accepting.
    assert!(!accepted_between(
        &waited,
        &[file("out", false, Phase::Moving)]
    ));
    // One that was already on its way, or was never seen waiting, is not accepted now.
    assert!(!accepted_between(
        &[file("a", true, Phase::Moving)],
        &[file("a", true, Phase::Done)]
    ));
    assert!(!accepted_between(&[], &[file("new", true, Phase::Moving)]));
}

#[test]
fn only_a_code_nobody_scanned_gives_way_and_only_to_devices_that_are_picked() {
    let picked = Aim::Devices(vec!["phone".into()]);
    assert!(code_gives_way(Some(&CodeCircle::Code), &picked));
    assert!(code_gives_way(
        Some(&CodeCircle::Code),
        &Aim::Devices(vec!["phone".into(), "laptop".into()])
    ));
    // No device picked, or one that went away: the code stays, the files have nowhere else to go.
    assert!(!code_gives_way(Some(&CodeCircle::Code), &Aim::Code));
    assert!(!code_gives_way(Some(&CodeCircle::Code), &Aim::Held));
    // No code on show: nothing to take back.
    assert!(!code_gives_way(None, &picked));
    // Scanned, or with files on their way, or over: it is no longer this device's to take back.
    for spent in [
        CodeCircle::Scanned,
        CodeCircle::WaitingForApproval { files: 1 },
        CodeCircle::Sending {
            files: 1,
            percent: 10,
        },
        CodeCircle::Sent { files: 1 },
        CodeCircle::Stopped { files: 1, done: 0 },
        CodeCircle::Gone,
    ] {
        assert!(!code_gives_way(Some(&spent), &picked), "{spent:?} gave way");
    }
}

#[test]
fn a_picked_device_that_went_away_holds_everything_back_and_the_others_are_not_sent_to_behind_the_persons_back()
 {
    assert_eq!(aim_of(&SendState::ChosenUnavailable), Aim::Held);
    for instantly in [false, true] {
        assert_eq!(circle_does(true, &Aim::Held, instantly), CircleDoes::Held);
    }
}

#[test]
fn the_real_send_state_of_a_view_drives_the_aim_for_every_case_of_who_is_online() {
    // End to end over the real rule, not a hand-made state: online devices are a destination, an offline picked one holds the send back.
    let view = view_of(vec![peer("laptop", true), peer("phone", false)]);
    let owned = |ids: &[&str]| -> Vec<String> { ids.iter().map(|id| (*id).to_owned()).collect() };
    assert_eq!(
        aim_of(&view.send_state(&owned(&["id-laptop"]))),
        Aim::Devices(vec!["id-laptop".into()])
    );
    assert_eq!(
        aim_of(&view.send_state(&owned(&["id-phone"]))),
        Aim::Held,
        "an offline device was a destination"
    );
    assert_eq!(
        aim_of(&view.send_state(&owned(&["id-laptop", "id-phone"]))),
        Aim::Held,
        "one offline device must hold the send back"
    );
    assert_eq!(aim_of(&view.send_state(&[])), Aim::Code);
}

#[test]
fn the_circle_says_what_it_does_from_one_place_in_each_of_its_states() {
    let devices = Aim::Devices(vec!["laptop".into()]);
    // Nothing listed: the picker, whatever else is true.
    for aim in [&Aim::Code, &devices, &Aim::Held] {
        assert_eq!(circle_does(false, aim, false), CircleDoes::ChooseFiles);
        assert_eq!(circle_does(false, aim, true), CircleDoes::ChooseFiles);
    }
    // Files listed and no device picked: a tap makes a code, however Send instantly is set. A hold is never asked for here.
    assert_eq!(circle_does(true, &Aim::Code, false), CircleDoes::MakeCode);
    assert_eq!(circle_does(true, &Aim::Code, true), CircleDoes::MakeCode);
    // Files listed and devices picked: held to send, or one tap with Send instantly.
    assert_eq!(circle_does(true, &devices, false), CircleDoes::HoldToSend);
    assert_eq!(circle_does(true, &devices, true), CircleDoes::TapToSend);
}

#[test]
fn a_tap_never_opens_the_picker_again_once_files_are_listed() {
    let devices = Aim::Devices(vec!["laptop".into()]);
    for aim in [&Aim::Code, &devices, &Aim::Held] {
        for instantly in [false, true] {
            assert_ne!(circle_does(true, aim, instantly), CircleDoes::ChooseFiles);
        }
    }
}

#[test]
fn the_circle_fills_while_held_and_sends_exactly_when_full_not_before() {
    let mut touch = Hold::default();
    touch.down((50., 50.), 1_000);
    assert_eq!(fill_of(&touch, 1_000, true), 0.);
    let half = fill_of(&touch, 1_000 + CONFIRM_MS / 2, true);
    assert!((half - 0.5).abs() < 0.01, "half the time was {half}");
    assert!(!is_full(
        &touch,
        1_000 + CONFIRM_MS - 1,
        CircleDoes::HoldToSend
    ));
    assert!(
        is_full(&touch, 1_000 + CONFIRM_MS, CircleDoes::HoldToSend),
        "the boundary itself is full"
    );
    assert_eq!(
        fill_of(&touch, 1_000 + CONFIRM_MS * 4, true),
        1.,
        "the fill went past the end"
    );
}

#[test]
fn only_a_circle_that_is_held_ever_fills_or_sends_by_waiting() {
    let mut touch = Hold::default();
    touch.down((50., 50.), 0);
    for does in [
        CircleDoes::ChooseFiles,
        CircleDoes::MakeCode,
        CircleDoes::TapToSend,
        CircleDoes::Held,
    ] {
        assert!(
            !is_full(&touch, CONFIRM_MS * 10, does),
            "{does:?} sent by waiting"
        );
    }
}

#[test]
fn a_press_as_long_as_a_hold_takes_does_not_send_for_that_is_not_the_time_to_confirm() {
    let mut touch = Hold::default();
    touch.down((50., 50.), 0);
    assert!(touch.is_holding(HOLD_MS), "the hold boundary moved");
    assert!(
        !is_full(&touch, HOLD_MS, CircleDoes::HoldToSend),
        "resting a finger for the time of a press sent the files"
    );
    const { assert!(CONFIRM_MS > HOLD_MS) };
}

#[test]
fn dragging_empties_the_circle_for_good_however_long_the_finger_stays_down() {
    let mut touch = Hold::default();
    touch.down((50., 50.), 0);
    touch.moved((50., 50. + TAP_SLOP + 1.));
    assert_eq!(touch.held_for(CONFIRM_MS * 10), None);
    assert_eq!(fill_of(&touch, CONFIRM_MS * 10, true), 0.);
    // Coming back to where it began does not undo it.
    touch.moved((50., 50.));
    assert!(
        !is_full(&touch, CONFIRM_MS * 10, CircleDoes::HoldToSend),
        "a drag sent the files"
    );
}

#[test]
fn lifting_early_empties_the_circle_and_sends_nothing() {
    let mut touch = Hold::default();
    touch.down((50., 50.), 0);
    assert_eq!(touch.up((50., 50.), CONFIRM_MS - 1), Ended::Hold);
    assert_eq!(
        touch.held_for(CONFIRM_MS - 1),
        None,
        "a lifted finger kept filling the circle"
    );
    assert!(!is_full(&touch, CONFIRM_MS * 3, CircleDoes::HoldToSend));
}

#[test]
fn a_screen_without_sides_offers_top_and_bottom_and_marks_the_bottom_for_a_side() {
    use peersus_core::config::NoticePlace;
    let offered = |sides| -> Vec<NoticePlace> {
        notice_places(sides)
            .iter()
            .map(|(place, _)| *place)
            .collect()
    };
    assert_eq!(offered(false), [NoticePlace::Top, NoticePlace::Bottom]);
    assert_eq!(
        offered(true),
        [
            NoticePlace::Top,
            NoticePlace::Bottom,
            NoticePlace::Left,
            NoticePlace::Right
        ]
    );
    for saved in [
        NoticePlace::Top,
        NoticePlace::Bottom,
        NoticePlace::Left,
        NoticePlace::Right,
    ] {
        assert_eq!(notice_place_shown(saved, true), saved);
    }
    assert_eq!(
        notice_place_shown(NoticePlace::Top, false),
        NoticePlace::Top
    );
    // Saved on a desktop and read on a phone: the phone draws at the bottom for it, and that is what it marks.
    assert_eq!(
        notice_place_shown(NoticePlace::Left, false),
        NoticePlace::Bottom
    );
    assert_eq!(
        notice_place_shown(NoticePlace::Right, false),
        NoticePlace::Bottom
    );
}

#[test]
fn the_panel_of_an_offer_stands_at_its_edge_and_never_off_the_screen() {
    use peersus_core::config::NoticePlace;
    // A second screen to the right of a first: its corner is not the origin.
    let screen = ((1920, 0), (2560, 1440));
    let panel = (420, 200);
    assert_eq!(
        panel_origin(NoticePlace::Top, screen, panel, 40),
        (1920 + 1070, 40)
    );
    assert_eq!(
        panel_origin(NoticePlace::Bottom, screen, panel, 40),
        (1920 + 1070, 1440 - 200 - 40)
    );
    assert_eq!(
        panel_origin(NoticePlace::Left, screen, panel, 16),
        (1920 + 16, 620)
    );
    assert_eq!(
        panel_origin(NoticePlace::Right, screen, panel, 16),
        (1920 + 2560 - 420 - 16, 620)
    );
    // Taller than what the margin leaves: flush with the edge, not over it.
    let tall = (420, 1430);
    assert_eq!(panel_origin(NoticePlace::Top, screen, tall, 40).1, 10);
    assert_eq!(panel_origin(NoticePlace::Bottom, screen, tall, 40).1, 0);
    // Larger than the screen: at its corner, on it.
    for place in [
        NoticePlace::Top,
        NoticePlace::Bottom,
        NoticePlace::Left,
        NoticePlace::Right,
    ] {
        assert_eq!(panel_origin(place, screen, (3000, 2000), 40), (1920, 0));
    }
}

#[test]
fn with_send_instantly_a_whole_tap_sends_on_lifting_and_nothing_else_does() {
    assert!(sends_on_lift(Ended::Tap, CircleDoes::TapToSend));
    // A touch that was dragged, cancelled, or held long enough to be a hold is not a tap: no instant send from it.
    assert!(
        !sends_on_lift(Ended::Nothing, CircleDoes::TapToSend),
        "a drag or a cancelled touch sent"
    );
    assert!(
        !sends_on_lift(Ended::Hold, CircleDoes::TapToSend),
        "a long press sent instantly"
    );
}

#[test]
fn only_the_instant_circle_sends_on_a_tap_so_a_hold_circle_a_code_circle_and_a_held_back_one_do_not()
 {
    for does in [
        CircleDoes::HoldToSend,
        CircleDoes::MakeCode,
        CircleDoes::ChooseFiles,
        CircleDoes::Held,
    ] {
        assert!(!sends_on_lift(Ended::Tap, does), "{does:?} sent on a tap");
    }
}

#[test]
fn the_disc_that_fills_the_circle_grows_with_the_hold_and_is_the_whole_circle_exactly_when_it_sends()
 {
    const DISC: f32 = 300.;
    let mut touch = Hold::default();
    touch.down((50., 50.), 0);
    let size = |ms: u64| fill_disc(fill_of(&touch, ms, true), DISC);
    assert_eq!(
        size(0),
        0.,
        "a touch that has just begun already showed a fill"
    );
    assert_eq!(size(CONFIRM_MS / 2), DISC / 2.);
    assert!(
        size(CONFIRM_MS - 1) < DISC,
        "the circle was full before it sent"
    );
    assert_eq!(size(CONFIRM_MS), DISC);
    assert!(
        is_full(&touch, CONFIRM_MS, CircleDoes::HoldToSend),
        "the disc is whole and the circle did not send"
    );
    // Never more than the circle however long it is held, and growing all the way (never going back).
    assert_eq!(size(CONFIRM_MS * 10), DISC);
    let mut last = 0.;
    for ms in (0..=CONFIRM_MS).step_by(10) {
        assert!(size(ms) >= last, "the fill went back at {ms} ms");
        last = size(ms);
    }
}

#[test]
fn the_disc_shows_nothing_for_anything_that_is_not_a_fill() {
    for bad in [f32::NAN, f32::NEG_INFINITY, f32::INFINITY, -1., -0.0, 0.] {
        assert_eq!(fill_disc(bad, 300.), 0., "{bad} drew a fill");
    }
    assert_eq!(fill_disc(0.5, 0.), 0.);
    assert_eq!(fill_disc(0.5, f32::NAN), 0.);
    assert_eq!(fill_disc(1.5, 300.), 300.);
}

#[test]
fn while_files_go_the_circle_fills_with_how_far_the_batch_has_come_and_not_with_the_finger() {
    let going = |percent| BatchState::Going {
        files: 2,
        percent,
        current: None,
    };
    assert_eq!(
        circle_fill(&going(0), 0.9),
        0.,
        "a finger that is no longer the circle's business filled it"
    );
    assert_eq!(circle_fill(&going(40), 0.), 0.4);
    assert_eq!(circle_fill(&going(100), 0.), 1.);
    // A percent that went past the end is the whole circle, and never more.
    assert_eq!(circle_fill(&going(250), 0.), 1.);
    // It never goes back as the batch comes on.
    let mut last = 0.;
    for percent in 0..=100u8 {
        let now = circle_fill(&going(percent), 0.);
        assert!(now >= last, "the circle emptied at {percent}%");
        last = now;
    }
}

#[test]
fn a_circle_that_is_held_fills_with_the_finger_and_every_other_state_is_empty() {
    let quiet = [
        BatchState::None,
        BatchState::Sent { files: 2 },
        BatchState::Stopped { files: 2, done: 1 },
    ];
    for state in &quiet {
        // Held to send: the finger fills it.
        assert_eq!(circle_fill(state, 0.5), 0.5, "{state:?}");
        assert_eq!(circle_fill(state, 1.), 1.);
        // Not held: nothing is left filled in a circle nobody is acting on.
        assert_eq!(circle_fill(state, 0.), 0., "{state:?}");
    }
    // A number that went wrong upstream fills nothing, and a fill past the end is the whole circle.
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.] {
        assert_eq!(circle_fill(&BatchState::None, bad), 0., "{bad}");
    }
    assert_eq!(circle_fill(&BatchState::None, 5.), 1.);
}

#[test]
fn every_state_has_a_phase_and_a_failure_keeps_its_reason() {
    let phase = |state| progress(&transfer(10, 1, state)).phase;
    assert_eq!(phase(TransferState::Offered), Phase::Waiting);
    assert_eq!(phase(TransferState::Running), Phase::Moving);
    assert_eq!(phase(TransferState::Complete), Phase::Done);
    assert_eq!(phase(TransferState::Cancelled), Phase::Cancelled);
    assert_eq!(
        phase(TransferState::Failed {
            reason: "disk full".into()
        }),
        Phase::Failed("disk full".into())
    );
}

#[test]
fn an_offer_already_waiting_when_the_window_opens_is_found_from_that_snapshot_with_the_settings_that_decide_where_it_goes()
 {
    let mut waiting = transfer(10, 0, TransferState::Offered);
    waiting.incoming = true;
    waiting.peer_name = Some("Alex's phone".into());
    let mut snapshot = snapshot_with(
        vec![],
        vec![transfer(10, 10, TransferState::Complete), waiting],
    );
    snapshot.settings.auto_save = true;
    snapshot.settings.save_folder = Some(std::path::PathBuf::from("/home/me/Received"));
    // From the snapshot alone, with no change since: this is what the window opens on.
    let found = offered_now(&View::of(&snapshot)).expect("an offer that was waiting was not shown");
    assert!(found.offer.acceptable());
    assert_eq!(found.offer.from.as_deref(), Some("Alex's phone"));
    assert!(found.auto_save);
    assert_eq!(
        found.save_folder.as_deref(),
        Some(std::path::Path::new("/home/me/Received"))
    );
    // And nothing is shown when nothing waits.
    assert_eq!(
        offered_now(&View::of(&snapshot_with(
            vec![],
            vec![transfer(10, 10, TransferState::Complete)]
        ))),
        None
    );
    assert_eq!(offered_now(&View::default()), None);
}

#[test]
fn only_transfers_still_going_can_be_stopped() {
    let snapshot = snapshot_with(
        vec![],
        vec![
            transfer(10, 1, TransferState::Offered),
            transfer(10, 1, TransferState::Running),
            transfer(10, 10, TransferState::Complete),
            transfer(10, 1, TransferState::Cancelled),
            transfer(10, 1, TransferState::Failed { reason: "x".into() }),
        ],
    );
    let stoppable: Vec<bool> = View::of(&snapshot)
        .transfers
        .iter()
        .map(Progress::stoppable)
        .collect();
    // Offered, running, complete, cancelled, failed.
    assert_eq!(stoppable, [true, true, false, false, false]);
}

#[test]
fn only_an_incoming_offer_still_waiting_can_be_accepted() {
    let make = |incoming: bool, state: TransferState| {
        let mut t = transfer(10, 0, state);
        t.incoming = incoming;
        progress(&t)
    };
    // Waiting and coming in: the person decides.
    assert!(make(true, TransferState::Offered).acceptable());
    // Waiting but going out: it is the OTHER device that decides, not this one.
    assert!(!make(false, TransferState::Offered).acceptable());
    // Already moving, finished, stopped or failed: nothing left to accept.
    assert!(!make(true, TransferState::Running).acceptable());
    assert!(!make(true, TransferState::Complete).acceptable());
    assert!(!make(true, TransferState::Cancelled).acceptable());
    assert!(!make(true, TransferState::Failed { reason: "x".into() }).acceptable());
}

fn peer(name: &str, online: bool) -> Peer {
    Peer {
        id: PeerId(format!("id-{name}")),
        name: name.into(),
        paired_at: 0,
        last_seen: None,
        online,
        sleeping: false,
        statuses: Vec::new(),
        grants: Default::default(),
        plugin_values: Default::default(),
    }
}
/// A snapshot with nothing in it: the one place a test builds a whole `Snapshot` by hand, so a field
/// added to it is added here and nowhere else.
fn empty_snapshot() -> Snapshot {
    Snapshot {
        settings: Default::default(),
        peers: vec![],
        grants: Default::default(),
        plugins: vec![],
        staged: vec![],
        store: Default::default(),
        update: Default::default(),
        actions: vec![],
        transfers: vec![],
        pairing: vec![],
        extension_prompts: vec![],
        notifications: vec![],
        errors: vec![],
        errors_said: 0,
        profile_picture: None,
        peer_pictures: Default::default(),
        displays: Vec::new(),
        peer_displays: Default::default(),
        files_code: None,
        link_tests: Default::default(),
        typed_addresses: Default::default(),
        listening_port: None,
        relay: None,
        relay_probes: Default::default(),
        page_views: Vec::new(),
        nearby: Default::default(),
        languages: Default::default(),
    }
}
fn snapshot_with(peers: Vec<Peer>, transfers: Vec<Transfer>) -> Snapshot {
    Snapshot {
        peers,
        transfers,
        ..empty_snapshot()
    }
}

#[test]
fn devices_keep_their_order_and_say_whether_they_are_reachable() {
    let snapshot = snapshot_with(
        vec![
            peer("laptop", true),
            peer("phone", false),
            peer("tablet", true),
        ],
        vec![],
    );
    let view = View::of(&snapshot);
    assert_eq!(
        view.devices
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["laptop", "phone", "tablet"]
    );
    assert_eq!(
        view.reachable()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["laptop", "tablet"],
        "an offline device was offered as a destination"
    );
}

#[test]
fn a_device_is_named_to_commands_by_its_identifier_and_shown_by_its_name() {
    let view = View::of(&snapshot_with(vec![peer("laptop", true)], vec![]));
    assert_eq!(view.devices[0].id, "id-laptop");
    assert_eq!(view.devices[0].name, "laptop");
}

#[test]
fn a_pairing_request_carries_what_is_needed_to_decide_it() {
    let mut snapshot = snapshot_with(vec![], vec![]);
    snapshot.pairing = vec![PairingPrompt {
        id: "req-1".into(),
        name: "Alex's phone".into(),
        options: [11, 22, 33],
        selected: false,
        remaining_seconds: 42,
    }];
    let approval = &View::of(&snapshot).approvals[0];
    assert_eq!(approval.request, "req-1");
    assert_eq!(approval.name, "Alex's phone");
    assert_eq!(approval.options, [11, 22, 33]);
    assert!(!approval.answered);
    assert_eq!(approval.seconds_left, 42);
}

#[test]
fn the_newest_pairing_request_comes_first_whatever_order_the_owner_listed_them_in() {
    let prompt = |id: &str, seconds: u64| PairingPrompt {
        id: id.into(),
        name: "My device".into(),
        options: [1, 2, 3],
        selected: false,
        remaining_seconds: seconds,
    };
    let mut snapshot = snapshot_with(vec![], vec![]);
    // The old one is listed first, as the owner did in a real run.
    snapshot.pairing = vec![prompt("old", 17), prompt("new", 117), prompt("middle", 60)];
    let order: Vec<_> = View::of(&snapshot)
        .approvals
        .iter()
        .map(|a| a.request.clone())
        .collect();
    assert_eq!(order, ["new", "middle", "old"]);
}

#[test]
fn a_device_still_on_the_default_name_has_not_been_named_by_its_person() {
    let mut view = View::default();
    assert!(view.needs_a_name(), "a fresh install has the default name");
    view.settings.device_name = "Alex's laptop".into();
    assert!(!view.needs_a_name());
    // Only the exact default counts; a name that merely contains it was chosen.
    view.settings.device_name = "My device 2".into();
    assert!(!view.needs_a_name());
    // And the default spelled with spaces around it is still the default.
    view.settings.device_name = format!("  {}  ", Settings::default().device_name);
    assert!(view.needs_a_name());
}

#[test]
fn what_the_owner_reported_is_passed_on_unchanged() {
    let mut snapshot = snapshot_with(vec![], vec![]);
    snapshot.errors = vec!["A plugin stopped working and was turned off".into()];
    assert_eq!(
        View::of(&snapshot).errors,
        ["A plugin stopped working and was turned off"]
    );
}

#[test]
fn nothing_known_is_an_empty_view_not_an_error() {
    assert_eq!(View::of(&snapshot_with(vec![], vec![])), View::default());
}

#[test]
fn a_snapshot_that_shows_nothing_new_is_not_a_change() {
    let snapshot = snapshot_with(vec![peer("laptop", true)], vec![]);
    let mut current = View::of(&snapshot);
    // The owner republishes the same state every second.
    assert!(!refresh(&mut current, &snapshot));
    assert!(!refresh(&mut current, &snapshot.clone()));
    assert_eq!(current, View::of(&snapshot));
}

#[test]
fn a_snapshot_that_shows_something_new_replaces_the_view() {
    let mut current = View::of(&snapshot_with(vec![peer("laptop", true)], vec![]));
    let later = snapshot_with(vec![peer("laptop", false)], vec![]);
    assert!(refresh(&mut current, &later));
    assert_eq!(current, View::of(&later));
    assert_eq!(current.devices[0].presence, Presence::Offline);
}

#[test]
fn snapshot_preparation_preserves_startup_history_and_only_reports_real_changes() {
    let snapshot = snapshot_with(vec![peer("laptop", true)], vec![]);
    let mut current = View::of(&snapshot);
    current.transfers_at_start = vec!["earlier-transfer".into()];
    let mut prepared = View::of(&snapshot);
    prepared.transfers_at_start = current.transfers_at_start.clone();
    assert_eq!(prepared, current);
    let later = snapshot_with(vec![peer("laptop", false)], vec![]);
    let mut prepared = View::of(&later);
    prepared.transfers_at_start = current.transfers_at_start.clone();
    assert_ne!(prepared, current);
    assert_eq!(prepared.transfers_at_start, ["earlier-transfer"]);
    assert_eq!(prepared.devices[0].presence, Presence::Offline);
}

fn destination_of(view: &View, chosen: &[&str]) -> Option<Vec<String>> {
    let chosen: Vec<String> = chosen.iter().map(|id| (*id).to_owned()).collect();
    view.destinations(&chosen)
        .map(|devices| devices.iter().map(|device| device.name.clone()).collect())
}
fn view_of(peers: Vec<Peer>) -> View {
    View::of(&snapshot_with(peers, vec![]))
}

#[test]
fn with_nothing_picked_there_is_no_destination_however_many_devices_are_reachable() {
    // One reachable, one not.
    let view = view_of(vec![peer("laptop", true), peer("phone", false)]);
    assert_eq!(destination_of(&view, &[]), None);
    // Several reachable.
    let view = view_of(vec![peer("laptop", true), peer("tablet", true)]);
    assert_eq!(destination_of(&view, &[]), None);
    // None reachable, and none paired.
    assert_eq!(
        destination_of(&view_of(vec![peer("laptop", false)]), &[]),
        None
    );
    assert_eq!(destination_of(&view_of(vec![]), &[]), None);
}

#[test]
fn picked_devices_are_the_destinations_in_the_order_they_were_picked_while_all_are_reachable() {
    let view = view_of(vec![
        peer("laptop", true),
        peer("tablet", true),
        peer("phone", true),
    ]);
    assert_eq!(
        destination_of(&view, &["id-tablet"]),
        Some(vec!["tablet".to_owned()])
    );
    // The order is the person's, not the order the devices are listed in.
    assert_eq!(
        destination_of(&view, &["id-phone", "id-laptop"]),
        Some(vec!["phone".to_owned(), "laptop".to_owned()])
    );
}

#[test]
fn a_picked_device_that_goes_offline_is_never_swapped_for_another_nor_left_out() {
    // The person picked the laptop. It drops off the network while the tablet is online:
    // the next files must NOT quietly go to the tablet.
    let before = view_of(vec![peer("laptop", true), peer("tablet", true)]);
    assert_eq!(
        destination_of(&before, &["id-laptop"]),
        Some(vec!["laptop".to_owned()])
    );
    let after = view_of(vec![peer("laptop", false), peer("tablet", true)]);
    assert_eq!(
        destination_of(&after, &["id-laptop"]),
        None,
        "the person's choice was replaced by another device"
    );
    // With two picked and one gone, the one that is left does not get the files alone.
    assert_eq!(
        destination_of(&after, &["id-laptop", "id-tablet"]),
        None,
        "part of the choice was sent while the rest was dropped"
    );
    // And with nothing picked, nothing is chosen for the person either.
    assert_eq!(destination_of(&after, &[]), None);
}

#[test]
fn a_picked_device_that_was_forgotten_is_not_a_destination() {
    let view = view_of(vec![peer("tablet", true)]);
    assert_eq!(destination_of(&view, &["id-laptop"]), None);
    assert_eq!(destination_of(&view, &["id-tablet", "id-laptop"]), None);
}

#[test]
fn a_send_goes_to_devices_only_when_the_person_picked_them_otherwise_it_is_by_code() {
    use SendState::*;
    let owned = |ids: &[&str]| -> Vec<String> { ids.iter().map(|id| (*id).to_owned()).collect() };
    let state = |peers: Vec<Peer>, chosen: &[&str]| view_of(peers).send_state(&owned(chosen));
    // Nothing picked: always a code, whatever is paired or online. Even a single online
    // device is NOT chosen for the person: what is being sent is their own files.
    assert_eq!(state(vec![], &[]), ByCode);
    assert_eq!(state(vec![peer("a", false)], &[]), ByCode);
    assert_eq!(state(vec![peer("a", true)], &[]), ByCode);
    assert_eq!(state(vec![peer("a", true), peer("b", true)], &[]), ByCode);
    // Picked and reachable: straight to it, or to all of them.
    assert_eq!(
        state(vec![peer("a", true), peer("b", true)], &["id-b"]),
        Ready(owned(&["id-b"]))
    );
    assert_eq!(
        state(vec![peer("a", true), peer("b", true)], &["id-b", "id-a"]),
        Ready(owned(&["id-b", "id-a"]))
    );
    // One of those picked went away while another is online: say THAT, do not pick for
    // the person and do not quietly switch to a code.
    assert_eq!(
        state(vec![peer("a", false), peer("b", true)], &["id-a"]),
        ChosenUnavailable
    );
    assert_eq!(
        state(vec![peer("a", false), peer("b", true)], &["id-a", "id-b"]),
        ChosenUnavailable
    );
    // Picked, then forgotten altogether: the same, there is nothing to send to.
    assert_eq!(state(vec![peer("b", true)], &["id-a"]), ChosenUnavailable);
}

#[test]
fn listed_files_go_at_once_to_the_one_paired_device_held_near_with_receive_open() {
    let owned = |ids: &[&str]| -> Vec<String> { ids.iter().map(|id| (*id).to_owned()).collect() };
    let view = |peers: Vec<Peer>, near: &[&str], receiving: &[&str], on: bool| {
        let ids = |ids: &[&str]| -> Vec<PeerId> {
            ids.iter().map(|id| PeerId((*id).to_owned())).collect()
        };
        let mut snapshot = snapshot_with(peers, vec![]);
        snapshot.settings.nearby_send = on;
        snapshot.nearby.near = ids(near);
        snapshot.nearby.receiving = ids(receiving);
        View::of(&snapshot)
    };
    let goes = |view: &View, chosen: &[&str], listed: bool| {
        view.receiving_near(&owned(chosen), listed)
            .map(|device| device.id.clone())
    };
    let waits = |view: &View, chosen: &[&str], listed: bool| {
        view.near_waiting(&owned(chosen), listed)
            .map(|device| device.id.clone())
    };
    let both = || vec![peer("a", true), peer("b", true)];
    let one = view(both(), &["id-b"], &["id-b"], true);
    assert_eq!(goes(&one, &[], true), Some("id-b".to_owned()));
    assert_eq!(waits(&one, &[], true), None);
    // With no files listed there is nothing to send.
    assert_eq!(goes(&one, &[], false), None);
    // What the person picked is never replaced, not even by the same device.
    assert_eq!(goes(&one, &["id-a"], true), None);
    assert_eq!(goes(&one, &["id-b"], true), None);
    // The switch off: nothing, whatever the owner says.
    let off = view(both(), &["id-b"], &["id-b"], false);
    assert_eq!(goes(&off, &[], true), None);
    assert_eq!(waits(&off, &[], true), None);
    // Near with any other page open: nothing goes, and it is named as the one waited for.
    let closed = view(both(), &["id-b"], &[], true);
    assert_eq!(goes(&closed, &[], true), None);
    assert_eq!(waits(&closed, &[], true), Some("id-b".to_owned()));
    assert_eq!(waits(&closed, &[], false), None);
    assert_eq!(waits(&closed, &["id-a"], true), None);
    // Two near with Receive open say nothing of which was meant.
    let two = view(both(), &["id-a", "id-b"], &["id-a", "id-b"], true);
    assert_eq!(goes(&two, &[], true), None);
    assert_eq!(waits(&two, &[], true), None);
    // Two near and only one of them with Receive open: that one said which.
    let said = view(both(), &["id-a", "id-b"], &["id-b"], true);
    assert_eq!(goes(&said, &[], true), Some("id-b".to_owned()));
    // Two near and neither with Receive open: nobody is named.
    assert_eq!(
        waits(&view(both(), &["id-a", "id-b"], &[], true), &[], true),
        None
    );
    // One that cannot be sent to now is given nothing, and does not count against the one that can.
    let away = view(
        vec![peer("a", false), peer("b", true)],
        &["id-a"],
        &["id-a"],
        true,
    );
    assert_eq!(goes(&away, &[], true), None);
    let mixed = view(
        vec![peer("a", false), peer("b", true)],
        &["id-a", "id-b"],
        &["id-a", "id-b"],
        true,
    );
    assert_eq!(goes(&mixed, &[], true), Some("id-b".to_owned()));
    // A device that is not paired is nobody here.
    assert_eq!(
        goes(&view(both(), &["id-z"], &["id-z"], true), &[], true),
        None
    );
}

#[test]
fn peers_lists_only_the_devices_nearby_that_said_they_are_there_to_be_paired_with() {
    use peersus_core::nearby::Stranger;
    let stranger = |id: &str, receives: bool| Stranger {
        id: PeerId(id.to_owned()),
        name: id.to_owned(),
        receives,
        near: true,
    };
    let mut snapshot = snapshot_with(vec![], vec![]);
    snapshot.nearby.strangers = vec![stranger("pairs", false), stranger("takes-files", true)];
    let listed: Vec<String> = View::of(&snapshot)
        .pairable()
        .into_iter()
        .map(|stranger| stranger.id.0)
        .collect();
    assert_eq!(listed, ["pairs"]);
}

#[test]
fn an_offer_is_said_to_be_from_a_paired_device_by_the_name_it_is_kept_under_here() {
    let incoming = |peer: &str, said: Option<&str>| Transfer {
        peer: PeerId(peer.to_owned()),
        peer_name: said.map(str::to_owned),
        incoming: true,
        ..transfer(10, 0, TransferState::Offered)
    };
    let view = View::of(&snapshot_with(
        vec![peer("Ana's phone", true)],
        vec![
            // Paired, and it gave no name with the offer: a send over the connection that authenticates it gives none.
            incoming("id-Ana's phone", None),
            // Paired, and it calls itself something else now: it is still the device kept under this name.
            incoming("id-Ana's phone", Some("Somebody")),
            // Not paired: only what it said of itself, or nothing.
            incoming("stranger", Some("Passer-by")),
            incoming("stranger", None),
        ],
    ));
    let from: Vec<Option<&str>> = view
        .transfers
        .iter()
        .map(|row| row.from.as_deref())
        .collect();
    assert_eq!(
        from,
        [
            Some("Ana's phone"),
            Some("Ana's phone"),
            Some("Passer-by"),
            None
        ]
    );
    assert_eq!(
        offer_words(&view.transfers[0]),
        "Ana's phone wants to send you a file"
    );
}

#[test]
fn listed_files_go_to_a_device_that_is_not_paired_only_when_it_alone_was_brought_near_after_they_were_listed()
 {
    use peersus_core::nearby::Stranger;
    let owned = |ids: &[&str]| -> Vec<String> { ids.iter().map(|id| (*id).to_owned()).collect() };
    let stranger = |id: &str, receives: bool, near: bool| Stranger {
        id: PeerId(id.to_owned()),
        name: id.to_owned(),
        receives,
        near,
    };
    let view = |strangers: Vec<Stranger>, paired_near: &[&str], on: bool| {
        let mut snapshot = snapshot_with(vec![peer("a", true)], vec![]);
        snapshot.settings.nearby_send = on;
        snapshot.nearby.strangers = strangers;
        snapshot.nearby.near = paired_near
            .iter()
            .map(|id| PeerId((*id).to_owned()))
            .collect();
        View::of(&snapshot)
    };
    let brought = |view: &View, chosen: &[&str], before: &[&str], files: usize| {
        view.brought_near(&owned(chosen), &owned(before), files)
            .map(|stranger| stranger.id.0.clone())
    };
    let one = view(vec![stranger("x", true, true)], &[], true);
    assert_eq!(brought(&one, &[], &[], 1), Some("x".to_owned()));
    assert_eq!(brought(&one, &[], &[], 16), Some("x".to_owned()));
    // Nothing listed, or more than one offer carries: nothing goes.
    assert_eq!(brought(&one, &[], &[], 0), None);
    assert_eq!(brought(&one, &[], &[], 17), None);
    // It lay there when the files were listed: listing files is not bringing a device near.
    assert_eq!(brought(&one, &[], &["x"], 1), None);
    // What the person picked is where the files go, and nowhere else.
    assert_eq!(brought(&one, &["id-a"], &[], 1), None);
    // The switch off: nothing, whatever the owner says.
    assert_eq!(
        brought(
            &view(vec![stranger("x", true, true)], &[], false),
            &[],
            &[],
            1
        ),
        None
    );
    // Around but not near, or near and does not take files: listed in Peers and no more.
    assert_eq!(
        brought(
            &view(vec![stranger("x", true, false)], &[], true),
            &[],
            &[],
            1
        ),
        None
    );
    assert_eq!(
        brought(
            &view(vec![stranger("x", false, true)], &[], true),
            &[],
            &[],
            1
        ),
        None
    );
    // A paired device that is near is the one sent to or waited for; a device nobody verified is not sent to over it.
    assert_eq!(
        brought(
            &view(vec![stranger("x", true, true)], &["id-a"], true),
            &[],
            &[],
            1
        ),
        None
    );
    // Two brought at once say nothing of which was meant; one that lay there does not count against the one brought.
    let two = view(
        vec![stranger("x", true, true), stranger("y", true, true)],
        &[],
        true,
    );
    assert_eq!(brought(&two, &[], &[], 1), None);
    assert_eq!(brought(&two, &[], &["y"], 1), Some("x".to_owned()));
    // What is remembered: with no files, all that are near; with files, only those of them that stayed.
    assert_eq!(two.near_before_kept(&[], false), owned(&["x", "y"]));
    assert_eq!(
        two.near_before_kept(&owned(&["y", "z"]), true),
        owned(&["y"])
    );
    let gone = view(vec![stranger("x", true, false)], &[], true);
    assert!(gone.near_before_kept(&owned(&["x"]), true).is_empty());
}

#[test]
fn settings_has_these_tabs_in_this_order_with_distinct_names_and_opens_on_the_first() {
    let titles: Vec<&str> = SettingsTab::ALL.iter().map(|tab| tab.title()).collect();
    assert_eq!(
        titles,
        [
            "Overview",
            "Device",
            "Appearance",
            "Languages",
            "Network",
            "Extensions",
            "Permissions",
            "Binds",
            "About"
        ]
    );
    let mut sorted = titles.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), titles.len(), "two tabs share a name");
    assert_eq!(SettingsTab::default(), SettingsTab::ALL[0]);
}

#[test]
fn a_popup_opens_from_transparent_and_a_little_high_to_opaque_and_in_place() {
    let start = Popup::at(0.);
    let end = Popup::at(1.);
    assert_eq!((start.opacity, start.lift), (0., Popup::RISE));
    assert_eq!((end.opacity, end.lift), (1., 0.));
    // Never beyond its ends, whatever the animation hands over.
    assert_eq!(Popup::at(-3.), start);
    assert_eq!(Popup::at(9.), end);
    let middle = Popup::at(0.5);
    assert!(middle.opacity > 0. && middle.opacity < 1.);
    assert!(middle.lift > 0. && middle.lift < Popup::RISE);
}

#[test]
fn a_thing_that_is_still_is_whole_and_in_place_from_the_first_frame() {
    assert_eq!(
        Popup::still(),
        Popup {
            opacity: 1.,
            lift: 0.,
            scale: 1.
        }
    );
    // And it is exactly where a finished opening ends, so reduced motion shows the same picture.
    assert_eq!(Popup::still(), Popup::at(1.));
}

#[test]
fn a_popup_also_grows_from_a_little_smaller_to_its_full_size() {
    assert_eq!(Popup::at(0.).scale, Popup::SMALLEST);
    assert_eq!(Popup::at(1.).scale, 1.);
    assert_eq!(Popup::still().scale, 1.);
    let middle = Popup::at(0.5).scale;
    assert!(middle > Popup::SMALLEST && middle < 1.);
}

#[test]
fn the_dashboard_menu_in_the_sidebar_has_these_nine_entries_once_each_in_this_order() {
    assert_eq!(
        SettingsTab::ALL,
        [
            SettingsTab::Overview,
            SettingsTab::Device,
            SettingsTab::Appearance,
            SettingsTab::Languages,
            SettingsTab::Network,
            SettingsTab::Extensions,
            SettingsTab::Permissions,
            SettingsTab::Binds,
            SettingsTab::About,
        ]
    );
    // It opens on its first entry, an overview of how things stand.
    assert_eq!(SettingsTab::default(), SettingsTab::ALL[0]);
    // The places of the header are not in it: Send, Receive and Peers stay where they are.
    for tab in SettingsTab::ALL {
        assert!(!matches!(tab.title(), "Send" | "Receive" | "Peers"));
    }
    // A desktop lists them all; a phone, which has no such keys, all but Binds, in the same order.
    assert_eq!(SettingsTab::listed(false), SettingsTab::ALL);
    let phone: Vec<_> = SettingsTab::ALL
        .into_iter()
        .filter(|tab| *tab != SettingsTab::Binds)
        .collect();
    assert_eq!(SettingsTab::listed(true), phone);
}

#[test]
fn about_sits_at_the_foot_of_the_sidebar_and_the_other_eight_stay_at_the_top() {
    assert_eq!(
        Sidebar::TOP,
        [
            SettingsTab::Overview,
            SettingsTab::Device,
            SettingsTab::Appearance,
            SettingsTab::Languages,
            SettingsTab::Network,
            SettingsTab::Extensions,
            SettingsTab::Permissions,
            SettingsTab::Binds,
        ]
    );
    assert_eq!(Sidebar::FOOT, SettingsTab::About);
    // Together they are exactly the nine entries, in the same order, none lost and none twice.
    let mut all = Sidebar::TOP.to_vec();
    all.push(Sidebar::FOOT);
    assert_eq!(all, SettingsTab::ALL);
}

#[test]
fn the_binds_page_lists_what_a_key_can_run_and_refuses_a_chord_that_is_taken() {
    use peersus_api::contributions::{Action, ActionInput, Source};
    use peersus_core::bindings::{AppAction, BindTarget};
    use peersus_core::model::{PluginInfo, PluginStatus, Standing};
    use peersus_core::plugins::PluginId;
    let action = |id: &str, needs_device: bool, destructive: bool, chord: Option<&str>| Action {
        id: id.into(),
        label: format!("Do {id}"),
        short_label: None,
        input: ActionInput::None,
        target: Source {
            service: "svc".into(),
            method: id.into(),
        },
        capability: None,
        needs_device,
        destructive,
        timeout_ms: peersus_api::contributions::default_action_timeout(),
        chord: chord.map(|text| text.parse().unwrap()),
        help: (id == "go").then(|| "Goes, exactly as its extension says.".to_owned()),
    };
    let mut manifest = peersus_api::manifest::Manifest::default();
    manifest.contributions.actions = vec![
        action("go", false, false, Some("ctrl+alt+g")),
        action("take", true, false, None),
        action("wipe", false, true, None),
    ];
    let mut view = View::default();
    view.plugins.push(PluginInfo {
        id: PluginId::new("demo").unwrap(),
        manifest,
        standing: Standing::Ready,
        status: PluginStatus::Running,
        grantable: peersus_core::model::Grantable {
            per_device: vec![],
            outright: vec![],
        },
        program: None,
        package: None,
        agreed: false,
        from_store: false,
    });
    for (id, name) in [("one", "Laptop"), ("two", "Desk")] {
        view.devices.push(Device {
            id: id.into(),
            name: name.into(),
            presence: Presence::Online,
            grants: Default::default(),
            picture: None,
            paired_at: 0,
            last_seen: None,
            link_test: None,
            typed_address: None,
        });
    }
    let rows = bind_rows(&view);
    // The application's four, then `go`, then `take` once per device; the destructive one is not offered.
    assert_eq!(rows.len(), 4 + 1 + 2);
    assert!(rows[..4].iter().all(|row| row.from.is_none()));
    assert_eq!(rows[4].label, "Do go");
    // A description under each: the application's own, the extension's words, none where the extension wrote none.
    assert!(rows[..4].iter().all(|row| row.help.is_some()));
    assert_eq!(
        rows[4].help.as_deref(),
        Some("Goes, exactly as its extension says.")
    );
    assert_eq!(rows[5].help, None);
    assert_eq!(
        rows[5..]
            .iter()
            .map(|row| row.device.as_deref())
            .collect::<Vec<_>>(),
        [Some("Laptop"), Some("Desk")]
    );
    assert!(rows.iter().all(|row| !row.label.contains("wipe")));

    // Bound: the row says it, and the suggestion is no longer offered.
    let go = rows[4].target.clone();
    let chord = |text: &str| -> peersus_api::chord::Chord { text.parse().unwrap() };
    view.settings.bindings =
        rebound(&view.settings.bindings, &rows, chord("ctrl+alt+g"), &go).unwrap();
    let rows = bind_rows(&view);
    assert_eq!(rows[4].chords, [chord("ctrl+alt+g")]);
    // More chords are added beside it, up to three; then the row is full and a fourth is refused.
    for more in ["ctrl+alt+h", "alt+f2"] {
        view.settings.bindings = rebound(&view.settings.bindings, &rows, chord(more), &go).unwrap();
    }
    let rows = bind_rows(&view);
    assert_eq!(rows[4].chords.len(), 3);
    assert!(rows[4].full());
    assert_eq!(
        rebound(&view.settings.bindings, &rows, chord("alt+f3"), &go),
        Err(BindRefusal::Full)
    );
    // A chord that runs something else is refused with the row that holds it.
    let send = BindTarget::App {
        action: AppAction::ShowSend,
    };
    let Err(BindRefusal::Taken(held)) =
        rebound(&view.settings.bindings, &rows, chord("ctrl+alt+h"), &send)
    else {
        panic!("a chord that runs something else was given to another");
    };
    assert_eq!(held.target, go);
    assert_eq!(held.from.as_deref(), Some("demo"));
    // One taken out, the others stay, and it is free for another action.
    view.settings.bindings = unbound(&view.settings.bindings, chord("ctrl+alt+h"));
    let rows = bind_rows(&view);
    assert_eq!(rows[4].chords, [chord("alt+f2"), chord("ctrl+alt+g")]);
    view.settings.bindings =
        rebound(&view.settings.bindings, &rows, chord("ctrl+alt+h"), &send).unwrap();
}

#[test]
fn permission_rows_are_what_the_owner_says_can_be_granted_never_what_a_manifest_lists() {
    use peersus_api::{manifest::Manifest, permission::Permission};
    use peersus_core::model::{Grantable, PluginInfo, PluginStatus, Standing};
    use peersus_core::plugins::{Grants, PluginId};
    let mut manifest = Manifest::default();
    manifest
        .capabilities
        .insert("send.use".into(), "Send things for you".into());
    // Listed in the manifest, and not grantable: the owner does not offer it, so it is no row.
    manifest
        .capabilities
        .insert("just.listed".into(), "Only listed".into());
    let info = PluginInfo {
        id: PluginId::new("syncer").unwrap(),
        manifest,
        standing: Standing::Ready,
        status: PluginStatus::Running,
        grantable: Grantable {
            per_device: vec![Permission::new("syncer", "send.use").unwrap()],
            outright: vec![],
        },
        program: None,
        package: None,
        agreed: false,
        from_store: false,
    };

    let rows = PermissionRows::of(&info, &info.grantable.per_device, &Grants::default());
    assert_eq!(rows.len(), 1, "a name that was only listed became a row");
    assert_eq!(
        rows[0].permission,
        Permission::new("syncer", "send.use").unwrap()
    );
    assert_eq!(rows[0].description, "Send things for you");
    assert!(!rows[0].allowed);
    // Granted: the row is on.
    let held = Grants::new([Permission::new("syncer", "send.use").unwrap()]).unwrap();
    assert!(PermissionRows::of(&info, &info.grantable.per_device, &held)[0].allowed);
}

#[test]
fn a_permission_of_the_application_is_described_in_the_applications_own_words() {
    use peersus_api::permission::Permission;
    use peersus_core::model::{Grantable, PluginInfo, PluginStatus, Standing};
    use peersus_core::plugins::{Grants, PluginId};
    let mut info = PluginInfo {
        id: PluginId::new("sender").unwrap(),
        manifest: Default::default(),
        standing: Standing::Ready,
        status: PluginStatus::Running,
        grantable: Grantable::default(),
        program: None,
        package: None,
        agreed: false,
        from_store: false,
    };
    // Not in the plugin's manifest at all: its owner is the application, which wrote the words.
    let send = Permission::new("host", "peers.send").unwrap();
    let rows = PermissionRows::of(&info, std::slice::from_ref(&send), &Grants::default());
    assert_eq!(
        rows[0].description,
        peersus_app::runtime::PEERS_SEND_DESCRIPTION
    );
    let restart = Permission::new("host", "plugins.restart").unwrap();
    let rows = PermissionRows::of(&info, std::slice::from_ref(&restart), &Grants::default());
    assert_eq!(
        rows[0].description,
        peersus_app::runtime::PLUGINS_RESTART_DESCRIPTION
    );
    // An imported host permission must never borrow a same-named description
    // from the extension asking for it, including in translated search results.
    for (capability, description) in [
        (
            peersus_app::platform_services::FILES_CAPABILITY,
            peersus_app::platform_services::FILES_DESCRIPTION,
        ),
        (
            peersus_app::platform_services::HTTP_CAPABILITY,
            peersus_app::platform_services::HTTP_DESCRIPTION,
        ),
    ] {
        info.manifest.capabilities.insert(
            capability.to_owned(),
            "Extension-supplied description, not the host's consent wording".to_owned(),
        );
        let permission = Permission::new("host", capability).unwrap();
        for language in [
            crate::i18n::Language::ENGLISH,
            crate::i18n::fixture::language("ro"),
        ] {
            let rows = PermissionRows::of_in(
                language,
                &info,
                std::slice::from_ref(&permission),
                &Grants::default(),
            );
            assert_eq!(
                rows[0].description,
                crate::i18n::translate(language, description),
                "host-owned consent wording was missing or replaced by extension text",
            );
        }
    }
    // A permission nobody described is still shown, and says so, rather than nothing at all.
    let mystery = Permission::new("sender", "unknown.use").unwrap();
    let rows = PermissionRows::of(&info, std::slice::from_ref(&mystery), &Grants::default());
    assert!(!rows[0].description.trim().is_empty());
}

#[test]
fn every_permission_the_application_declares_has_its_own_words_on_the_screen_and_can_be_translated()
{
    use peersus_api::permission::Permission;
    use peersus_core::model::{Grantable, PluginInfo, PluginStatus, Standing};
    use peersus_core::plugins::PluginId;
    let info = PluginInfo {
        id: PluginId::new("asker").unwrap(),
        manifest: Default::default(),
        standing: Standing::Ready,
        status: PluginStatus::Running,
        grantable: Grantable::default(),
        program: None,
        package: None,
        agreed: false,
        from_store: false,
    };
    assert!(peersus_app::runtime::HOST_CAPABILITIES.len() >= 14);
    for &(_, capability, description) in peersus_app::runtime::HOST_CAPABILITIES {
        let permission = Permission::new("host", capability).unwrap();
        let words = PermissionRows::words_in(crate::i18n::Language::ENGLISH, &info, &permission);
        assert_eq!(words, description);
        // What a person consents to can be said in the language they read: every description is a message of the
        // template that translators start from.
        assert!(
            crate::i18n::has(description),
            "{capability}'s description is not in the template"
        );
    }
}

#[test]
fn each_device_in_the_view_keeps_what_it_granted_each_plugin() {
    use peersus_api::permission::Permission;
    use peersus_core::model::Peer;
    use peersus_core::plugins::{Grants, PluginId};
    let plugin = PluginId::new("syncer").unwrap();
    let mut peer = Peer {
        id: peersus_core::model::PeerId("peer-a".into()),
        name: "Phone".into(),
        paired_at: 1,
        last_seen: None,
        online: true,
        sleeping: false,
        statuses: Vec::new(),
        grants: Default::default(),
        plugin_values: Default::default(),
    };
    peer.grants.insert(
        plugin.clone(),
        Grants::new([Permission::new("syncer", "send.use").unwrap()]).unwrap(),
    );
    let snapshot = Snapshot {
        peers: vec![peer],
        ..empty_snapshot()
    };
    let view = View::of(&snapshot);
    assert_eq!(view.devices.len(), 1);
    let held = view.devices[0]
        .grants
        .get(&plugin)
        .expect("what it granted is kept");
    assert!(held.allows(&Permission::new("syncer", "send.use").unwrap()));
    // A device that granted a plugin nothing has no entry for it.
    assert!(
        !view.devices[0]
            .grants
            .contains_key(&PluginId::new("other").unwrap())
    );
}

#[test]
fn the_view_keeps_what_each_plugin_is_allowed_outright() {
    use peersus_api::permission::Permission;
    use peersus_core::plugins::{Grants, PluginId};
    let mut snapshot = empty_snapshot();
    let id = PluginId::new("syncer").unwrap();
    snapshot.grants.insert(
        id.clone(),
        Grants::new([Permission::new("syncer", "send.use").unwrap()]).unwrap(),
    );
    let view = View::of(&snapshot);
    assert!(
        view.grants
            .get(&id)
            .is_some_and(|g| g.allows(&Permission::new("syncer", "send.use").unwrap()))
    );
    // A plugin with nothing allowed has no entry, and that is what the window shows as all off.
    assert!(!view.grants.contains_key(&PluginId::new("nothing").unwrap()));
}

#[test]
fn a_click_on_the_profile_opens_the_device_tab_where_the_name_is_edited() {
    assert_eq!(Sidebar::PROFILE, SettingsTab::Device);
    // It is one of the entries of the dashboard, so the page always has somewhere to go.
    assert!(SettingsTab::ALL.contains(&Sidebar::PROFILE));
    // And it is not the entry at the foot: the profile is not About.
    assert_ne!(Sidebar::PROFILE, Sidebar::FOOT);
}

#[test]
fn the_view_carries_the_own_picture_digest_and_one_for_each_device_that_showed_one() {
    use peersus_core::profile::Picture;
    let own = Picture::new(1, 1, vec![1, 2, 3, 255]).unwrap().digest();
    let theirs = Picture::new(1, 1, vec![9, 8, 7, 255]).unwrap().digest();
    let mut snapshot = snapshot_with(vec![peer("laptop", true), peer("phone", true)], vec![]);
    snapshot.profile_picture = Some(own);
    snapshot
        .peer_pictures
        .insert(snapshot.peers[0].id.clone(), theirs);
    let view = View::of(&snapshot);
    assert_eq!(view.profile_picture, Some(own));
    assert_eq!(
        view.devices[0].picture,
        Some(theirs),
        "the device that showed a picture does not carry its digest"
    );
    assert_eq!(
        view.devices[1].picture, None,
        "a device that showed none was given one"
    );
    // A picture for a device that is not in the list names nobody and is not carried anywhere.
    snapshot
        .peer_pictures
        .insert(PeerId("stranger".into()), own);
    let view = View::of(&snapshot);
    assert!(view.devices.iter().all(|d| d.id != "stranger"));
    assert_eq!(
        view.devices.iter().filter(|d| d.picture.is_some()).count(),
        1
    );
}

#[test]
fn a_view_refreshes_when_only_a_picture_changed_and_not_when_nothing_did() {
    use peersus_core::profile::Picture;
    let mut snapshot = snapshot_with(vec![peer("laptop", true)], vec![]);
    let mut view = View::of(&snapshot);
    assert!(
        !refresh(&mut view, &snapshot),
        "a quiet second redrew the window"
    );
    snapshot.profile_picture = Some(Picture::new(1, 1, vec![1, 1, 1, 255]).unwrap().digest());
    assert!(
        refresh(&mut view, &snapshot),
        "a new own picture did not refresh the view"
    );
    assert!(!refresh(&mut view, &snapshot));
    let id = snapshot.peers[0].id.clone();
    snapshot
        .peer_pictures
        .insert(id, Picture::new(1, 1, vec![2, 2, 2, 255]).unwrap().digest());
    assert!(
        refresh(&mut view, &snapshot),
        "a device's new picture did not refresh the view"
    );
}

#[test]
fn a_snapshot_makes_the_window_ask_once_for_each_picture_it_names_and_for_no_other() {
    use crate::shelf::{Shelf, named_by};
    use peersus_core::profile::Picture;
    let (own, theirs) = (
        Picture::new(1, 1, vec![1, 2, 3, 255]).unwrap(),
        Picture::new(1, 1, vec![9, 8, 7, 255]).unwrap(),
    );
    let mut snapshot = snapshot_with(vec![peer("laptop", true), peer("phone", true)], vec![]);
    snapshot.profile_picture = Some(own.digest());
    // Two devices showing the same picture name it once: it is fetched once.
    for device in &snapshot.peers.clone() {
        snapshot
            .peer_pictures
            .insert(device.id.clone(), theirs.digest());
    }
    let named = named_by(&View::of(&snapshot));
    assert_eq!(named, [own.digest(), theirs.digest()].into_iter().collect());

    let mut shelf = Shelf::default();
    let mut asked = shelf.follow(&named);
    asked.sort();
    let mut both = vec![own.digest(), theirs.digest()];
    both.sort();
    assert_eq!(asked, both);
    assert!(
        shelf.follow(&named).is_empty(),
        "the same snapshot made the window ask again while it waited"
    );

    // One arrives; the same snapshot again asks only for the other.
    shelf.arrived(own.digest(), own.clone());
    assert_eq!(
        shelf.follow(&named),
        Vec::<peersus_core::profile::Digest>::new()
    );

    // A picture dropped by the snapshot and named again later is asked for again: it was let go first.
    shelf.arrived(theirs.digest(), theirs.clone());
    assert!(shelf.get(&theirs.digest()).is_some());
    let without: std::collections::BTreeSet<_> = [own.digest()].into_iter().collect();
    assert!(shelf.follow(&without).is_empty());
    assert!(
        shelf.get(&theirs.digest()).is_none(),
        "a picture no snapshot names was kept"
    );
    assert_eq!(
        shelf.follow(&named),
        vec![theirs.digest()],
        "a picture named again was not asked for again"
    );
}

#[test]
fn a_package_waiting_to_be_confirmed_says_whether_it_is_new_or_replaces_one_installed() {
    use peersus_core::model::{PluginInfo, PluginStatus, Standing};
    use peersus_core::plugins::PluginId;
    let info = |name: &str| PluginInfo {
        id: PluginId::new(name).unwrap(),
        manifest: Default::default(),
        standing: Standing::Ready,
        status: PluginStatus::Stopped,
        grantable: Default::default(),
        program: None,
        package: None,
        agreed: false,
        from_store: false,
    };
    let installed = vec![info("alpha")];
    assert_eq!(Staging::of(&info("beta"), &installed), Staging::New);
    // The same name as one installed: confirming would replace it, and the person is told.
    assert_eq!(Staging::of(&info("alpha"), &installed), Staging::Replaces);
    assert_eq!(Staging::New.button(), "Install");
    assert_eq!(Staging::Replaces.button(), "Replace");
}

#[test]
fn a_platform_setting_for_a_service_is_wanted_only_while_an_installed_extension_imports_it_from_the_application()
 {
    use peersus_api::manifest::Import;
    use peersus_core::model::{PluginInfo, PluginStatus, Standing};
    use peersus_core::plugins::PluginId;
    let extension = |name: &str, provider: &str, service: &str| {
        let mut info = PluginInfo {
            id: PluginId::new(name).unwrap(),
            manifest: Default::default(),
            standing: Standing::Ready,
            status: PluginStatus::Stopped,
            grantable: Default::default(),
            program: None,
            package: None,
            agreed: false,
            from_store: false,
        };
        info.manifest.imports.push(Import {
            provider: provider.to_owned(),
            service: service.to_owned(),
            version: 1,
            methods: Default::default(),
            events: Default::default(),
        });
        info
    };
    // Nothing installed: no row about a service nobody uses.
    assert!(!wants_service(&[], "some.service"));
    // Another service of the application, or the same name offered by another extension, is not this service.
    let others = [
        extension("one", peersus_api::HOST, "other.service"),
        extension("two", "three", "some.service"),
    ];
    assert!(!wants_service(&others, "some.service"));
    // Any extension that imports it from the application is enough, whatever it is called.
    let with = [
        extension("one", peersus_api::HOST, "other.service"),
        extension("anything", peersus_api::HOST, "some.service"),
    ];
    assert!(wants_service(&with, "some.service"));
}

#[test]
fn what_a_plugin_card_offers_follows_what_its_folder_allows_and_then_what_its_process_does() {
    use peersus_core::model::{PluginInfo, PluginStatus, PluginStatus::*, Standing};
    use peersus_core::plugins::PluginId;
    let card = |standing, status, program: Option<&str>| {
        PluginActions::for_plugin(&PluginInfo {
            id: PluginId::new("thing").unwrap(),
            manifest: Default::default(),
            standing,
            status,
            grantable: Default::default(),
            program: program.map(str::to_owned),
            package: None,
            agreed: false,
            from_store: false,
        })
    };
    let main = |standing, status: PluginStatus, program| card(standing, status, program).main;
    // May run: its process decides. One that runs can be disabled; one that does not can be enabled.
    for status in [Running, Starting, Restarting] {
        assert_eq!(
            main(Standing::Ready, status, None),
            PluginMain::Disable,
            "{status:?}"
        );
    }
    for status in [Stopped, Failed] {
        assert_eq!(
            main(Standing::Ready, status, None),
            PluginMain::Enable,
            "{status:?}"
        );
    }
    // Only a plugin that is running has anything to restart.
    assert!(card(Standing::Ready, Running, None).can_restart);
    assert!(!card(Standing::Ready, Stopped, None).can_restart);
    assert!(!card(Standing::Ready, Failed, None).can_restart);
    // Switched off: it is enabled, whatever its (stopped) process says.
    assert_eq!(main(Standing::Disabled, Stopped, None), PluginMain::Enable);
    assert!(!card(Standing::Disabled, Stopped, None).can_restart);
    // Waiting or changed: approved, as the digest it was shown, and nothing is switched or restarted.
    for standing in [Standing::NeedsApproval, Standing::Changed] {
        assert_eq!(
            main(standing, Stopped, Some("abc")),
            PluginMain::Approve {
                program: "abc".into()
            },
            "{standing:?}"
        );
        assert!(!card(standing, Stopped, Some("abc")).can_restart);
        // Nothing was measured: nothing was shown, so there is nothing to approve, and it is not offered.
        assert_eq!(
            main(standing, Stopped, None),
            PluginMain::None,
            "{standing:?} without a digest"
        );
    }
    // Broken: only to be removed, even if a digest happens to be there.
    assert_eq!(
        main(Standing::Broken, Stopped, Some("abc")),
        PluginMain::None
    );
    assert!(
        !card(Standing::Broken, Running, None).can_restart,
        "a broken plugin cannot be restarted"
    );
}

#[test]
fn a_plugin_that_is_not_allowed_to_run_says_so_and_is_never_called_merely_stopped() {
    use peersus_core::model::{PluginInfo, PluginStatus, PluginStatus::*, Standing};
    use peersus_core::plugins::PluginId;
    let info = |standing, status: PluginStatus| PluginInfo {
        id: PluginId::new("thing").unwrap(),
        manifest: Default::default(),
        standing,
        status,
        grantable: Default::default(),
        program: None,
        package: None,
        agreed: false,
        from_store: false,
    };
    // What the folder says wins over the process, whatever the process reports.
    for status in [Running, Starting, Restarting, Failed, Stopped] {
        assert_eq!(
            plugin_word(&info(Standing::NeedsApproval, status)),
            ("NEEDS APPROVAL", Tone::Ask),
            "{status:?}"
        );
        assert_eq!(
            plugin_word(&info(Standing::Changed, status)),
            ("CHANGED", Tone::Bad),
            "{status:?}"
        );
        assert_eq!(
            plugin_word(&info(Standing::Broken, status)),
            ("BROKEN", Tone::Bad),
            "{status:?}"
        );
        assert_eq!(
            plugin_word(&info(Standing::Disabled, status)),
            ("OFF", Tone::Quiet),
            "{status:?}"
        );
    }
    // One that may run is described by its process.
    assert_eq!(
        plugin_word(&info(Standing::Ready, Running)),
        ("RUNNING", Tone::Good)
    );
    assert_eq!(
        plugin_word(&info(Standing::Ready, Starting)),
        ("STARTING", Tone::Good)
    );
    assert_eq!(
        plugin_word(&info(Standing::Ready, Restarting)),
        ("RESTARTING", Tone::Bad)
    );
    assert_eq!(
        plugin_word(&info(Standing::Ready, Failed)),
        ("FAILED", Tone::Bad)
    );
    assert_eq!(
        plugin_word(&info(Standing::Ready, Stopped)),
        ("STOPPED", Tone::Bad)
    );
    // A sentence is given exactly where the plugin is not running because of its folder, and each says its own thing.
    let notes: Vec<_> = [Standing::NeedsApproval, Standing::Changed, Standing::Broken]
        .into_iter()
        .map(|standing| {
            plugin_note(&info(standing, Stopped)).expect("no sentence for a plugin held back")
        })
        .collect();
    assert!(
        notes[0].contains("approved")
            && notes[1].contains("forgotten")
            && notes[2].contains("removed"),
        "{notes:?}"
    );
    assert_eq!(
        notes
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "two states share a sentence"
    );
    assert_eq!(plugin_note(&info(Standing::Ready, Running)), None);
    assert_eq!(plugin_note(&info(Standing::Disabled, Stopped)), None);
}

#[test]
fn with_saving_on_its_own_off_every_file_asks_and_nothing_is_said_to_be_lost() {
    let folder = std::path::Path::new("/home/me/Received");
    // Whatever the folder is like, off asks, and says nothing about a folder: nothing was lost, it is only asked.
    for (chosen, state) in [
        (Some(folder), Folder::Usable),
        (Some(folder), Folder::Unusable),
        (None, Folder::Usable),
    ] {
        assert_eq!(
            destination(false, chosen, state),
            Destination::Ask { lost: None }
        );
    }
}

#[test]
fn a_chosen_folder_that_can_be_used_is_where_the_file_goes() {
    let folder = std::path::Path::new("/home/me/Received");
    assert_eq!(
        destination(true, Some(folder), Folder::Usable),
        Destination::Save(folder.to_path_buf())
    );
}

#[test]
fn a_chosen_folder_that_is_lost_is_asked_about_by_name_and_never_replaced_by_another_place() {
    let folder = std::path::Path::new("/home/me/Received");
    // The file does not go anywhere on its own, and the folder that was chosen is named so the person is told.
    assert_eq!(
        destination(true, Some(folder), Folder::Unusable),
        Destination::Ask {
            lost: Some(folder.to_path_buf())
        }
    );
}

#[test]
fn with_saving_on_but_no_folder_chosen_there_is_nowhere_to_save_to_and_the_file_asks() {
    // There is no default folder: a place nobody chose and nobody can see. Whatever the disk says of "no folder".
    for state in [Folder::Usable, Folder::Unusable] {
        assert_eq!(
            destination(true, None, state),
            Destination::Ask { lost: None }
        );
    }
}

#[test]
fn a_real_writable_folder_is_usable_and_nothing_is_left_behind_in_it() {
    let folder = tempfile::tempdir().unwrap();
    assert_eq!(look_at(folder.path()), Folder::Usable);
    // Looking made a file to prove it can, and took it away: nothing of the looking stays in the person's folder.
    assert_eq!(
        std::fs::read_dir(folder.path()).unwrap().count(),
        0,
        "the probe was left behind"
    );
}

#[test]
fn a_folder_that_is_gone_or_is_not_a_folder_is_unusable() {
    let folder = tempfile::tempdir().unwrap();
    assert_eq!(look_at(&folder.path().join("never-made")), Folder::Unusable);
    let file = folder.path().join("a-file");
    std::fs::write(&file, b"x").unwrap();
    assert_eq!(
        look_at(&file),
        Folder::Unusable,
        "a file was taken for a folder"
    );
}

#[cfg(unix)]
#[test]
fn a_link_is_unusable_even_when_it_leads_to_a_folder_that_can_be_written() {
    // A link could lead anywhere but where the person chose, and a file saved through it would land somewhere else.
    let (real, links) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let link = links.path().join("chosen");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();
    assert_eq!(look_at(real.path()), Folder::Usable);
    assert_eq!(
        look_at(&link),
        Folder::Unusable,
        "a link was used as the chosen folder"
    );
    assert_eq!(
        std::fs::read_dir(real.path()).unwrap().count(),
        0,
        "the link was written through"
    );
}

#[cfg(unix)]
#[test]
fn a_folder_that_cannot_be_written_to_is_unusable_whatever_it_looks_like() {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::tempdir().unwrap();
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    // Running as the owner without special rights, a folder with no write bit refuses a new file. (As root it would not,
    // so the claim is only made where a file really cannot be made.)
    let can_write = std::fs::File::create(folder.path().join("probe")).is_ok();
    if !can_write {
        assert_eq!(look_at(folder.path()), Folder::Unusable);
    }
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn showing_a_code_and_taking_one_are_never_on_screen_together() {
    // Whatever sequence of presses, at most one of the two circles is ever wanted.
    let all = [PairMode::Closed, PairMode::Showing, PairMode::Taking];
    for start in all {
        for next in [start.after_pair(), start.after_connect()] {
            assert!(
                !(next.shows_code() && next.takes_code()),
                "{start:?} led to both open"
            );
        }
    }
}

#[test]
fn each_button_closes_the_other_side_and_connect_is_a_switch() {
    // Pair shows the code, and closes the taking side if it was open.
    assert_eq!(PairMode::Closed.after_pair(), PairMode::Showing);
    assert_eq!(PairMode::Taking.after_pair(), PairMode::Showing);
    // Pressed again while shown, it stays shown and hides nothing (whether it asks for a new code is `pair_asks_for_a_code`'s to say).
    assert_eq!(PairMode::Showing.after_pair(), PairMode::Showing);
    // Connect takes a code, and closes the shown one if it was open.
    assert_eq!(PairMode::Closed.after_connect(), PairMode::Taking);
    assert_eq!(PairMode::Showing.after_connect(), PairMode::Taking);
    // Pressed again while open, it closes: a way back to the two buttons.
    assert_eq!(PairMode::Taking.after_connect(), PairMode::Closed);
    // Nothing is open to begin with.
    assert_eq!(PairMode::default(), PairMode::Closed);
    assert!(!PairMode::Closed.shows_code() && !PairMode::Closed.takes_code());
}

#[test]
fn pair_asks_for_a_code_only_when_none_is_on_show_and_a_second_press_does_not_replace_a_valid_one()
{
    // Nothing shown yet, whichever side was open: the press asks for the code.
    assert!(PairMode::Closed.pair_asks_for_a_code(false));
    assert!(PairMode::Taking.pair_asks_for_a_code(false));
    // Showing and a code is on screen: pressing again changes nothing. This is the press that used to put a new picture under a camera.
    assert!(
        !PairMode::Showing.pair_asks_for_a_code(true),
        "a valid code on show was replaced by a press"
    );
    // Showing, but the code is not there (it ran out, or the owner has not answered yet): it asks, or the circle would stay empty.
    assert!(PairMode::Showing.pair_asks_for_a_code(false));
    // Back from the other side with the old code still held: the mode was not showing, so a new one is asked for, and never the stale one reused.
    assert!(PairMode::Taking.pair_asks_for_a_code(true));
    assert!(PairMode::Closed.pair_asks_for_a_code(true));
}

#[test]
fn the_words_under_the_buttons_name_both_and_say_what_a_file_by_code_is_not() {
    assert!(
        PAIR_EXPLAINS.contains("Pair") && PAIR_EXPLAINS.contains("Connect"),
        "{PAIR_EXPLAINS}"
    );
    assert!(
        PAIR_EXPLAINS.to_lowercase().contains("scan")
            && PAIR_EXPLAINS.to_lowercase().contains("paste")
    );
    // Pairing is the trust; sending a file by code is not.
    assert!(
        PAIR_WARNS.to_lowercase().contains("trust") && PAIR_WARNS.to_lowercase().contains("file"),
        "{PAIR_WARNS}"
    );
}

#[test]
fn removing_a_plugin_takes_two_presses_and_the_second_must_be_for_the_same_plugin() {
    use peersus_core::plugins::PluginId;
    let a = PluginId::new("alpha").unwrap();
    let b = PluginId::new("beta").unwrap();
    // Nothing is being removed until the first press.
    assert_eq!(
        RemoveAsk::None.pressed(&a),
        (RemoveAsk::Asking(a.clone()), false)
    );
    // Pressing the same plugin again, while it is being asked about, removes it.
    assert_eq!(
        RemoveAsk::Asking(a.clone()).pressed(&a),
        (RemoveAsk::None, true)
    );
    // Pressing another plugin changes the question to that one and removes nothing.
    assert_eq!(
        RemoveAsk::Asking(a.clone()).pressed(&b),
        (RemoveAsk::Asking(b.clone()), false)
    );
    // Asking can be given up without removing anything.
    assert_eq!(RemoveAsk::Asking(a).cancelled(), RemoveAsk::None);
}

#[test]
fn a_profile_name_that_is_too_long_is_cut_with_an_ellipsis_and_a_short_one_is_left_alone() {
    assert_eq!(Sidebar::fit_name("Alex", 14), "Alex");
    assert_eq!(Sidebar::fit_name("exactly-14-chrs", 15), "exactly-14-chrs");
    // Cut to fit, with the ellipsis counted in the room.
    let cut = Sidebar::fit_name("A very long device name indeed", 14);
    assert_eq!(cut.chars().count(), 14);
    assert!(cut.ends_with('\u{2026}'));
    assert!(cut.starts_with("A very long "));
    // Counted in characters, not bytes: an accented name is not cut in the middle of a letter.
    let accented = Sidebar::fit_name("\u{218}tefan \u{218}tefanescu Popescu", 10);
    assert_eq!(accented.chars().count(), 10);
    assert!(accented.ends_with('\u{2026}'));
    // Nothing at all to show, and a room too small for anything, give an empty name, not a panic.
    assert_eq!(Sidebar::fit_name("", 8), "");
    assert_eq!(Sidebar::fit_name("Alex", 0), "");
    // The initial for the picture: the first letter, in capitals, or a question mark.
    assert_eq!(Sidebar::initial("alex"), 'A');
    assert_eq!(Sidebar::initial("  \u{219}tefan"), '\u{218}');
    assert_eq!(Sidebar::initial(""), '?');
}

#[test]
fn the_sidebar_is_hidden_outside_settings_expands_inside_it_and_keeps_its_icons_if_asked() {
    use peersus_core::config::Navigation::{Expanded, Hidden, Icons};
    // By default it is not there: Send, Receive and Peers are pages without a sidebar.
    assert_eq!(Sidebar::state(false, false), Hidden);
    // Inside Settings it is expanded, whatever the option says.
    assert_eq!(Sidebar::state(true, false), Expanded);
    assert_eq!(Sidebar::state(true, true), Expanded);
    // The one option: outside Settings the sidebar stays as its icons instead of going away.
    assert_eq!(Sidebar::state(false, true), Icons);
}

#[test]
fn only_the_first_button_of_a_mouse_presses_and_a_finger_always_does() {
    // The button that opens a menu (the second) is not also a click on what is under it.
    assert!(presses(Some(Some(PointerButton::First))));
    assert!(!presses(Some(Some(PointerButton::Other))));
    // A mouse that does not say which button it was, and a finger, which has none.
    assert!(presses(Some(None)));
    assert!(presses(None));
}

#[test]
fn the_bar_is_one_width_and_each_state_names_where_it_is_heading() {
    use peersus_core::config::Navigation;
    // Hidden is no width at all, the pictures are the narrow width, expanded is the full one.
    assert_eq!(Sidebar::width_for(Navigation::Hidden), 0.);
    assert_eq!(Sidebar::width_for(Navigation::Icons), Sidebar::NARROW);
    assert_eq!(Sidebar::width_for(Navigation::Expanded), Sidebar::WIDE);
    // Every move is toward one of these, so going from any state to any is one continuous move
    // of the same value, and expanded is reached from hidden and from the pictures alike.
    // The names show only past the pictures' width, and fully at the full one.
    assert_eq!(Sidebar::names_at(0.), 0.);
    assert_eq!(Sidebar::names_at(Sidebar::NARROW), 0.);
    assert_eq!(Sidebar::names_at(Sidebar::WIDE), 1.);
    let between = Sidebar::names_at((Sidebar::NARROW + Sidebar::WIDE) / 2.);
    assert!(between > 0. && between < 1.);
    // Whatever width is handed over, a name is never more or less than fully there or not.
    assert_eq!(Sidebar::names_at(-50.), 0.);
    assert_eq!(Sidebar::names_at(10_000.), 1.);
    // An icon alone fits the narrow width with room around it.
    const { assert!(Sidebar::NARROW >= 48.) };
}

#[test]
fn a_move_of_the_bar_takes_time_in_proportion_to_the_distance_it_has_to_go() {
    let ms = |from: f32, to: f32| Sidebar::duration_ms(from, to, false);
    // The whole way, from nothing to the full width, takes the whole time.
    assert_eq!(ms(0., Sidebar::WIDE), Motion::DURATION_MS);
    assert_eq!(ms(Sidebar::WIDE, 0.), Motion::DURATION_MS);
    // A shorter way is shorter, at the same speed: the pictures to the full width is part of it.
    let part = ms(Sidebar::NARROW, Sidebar::WIDE);
    assert!(part > 0 && part < Motion::DURATION_MS);
    // Turned round in the middle, only the distance that is left is travelled.
    assert_eq!(
        ms(Sidebar::WIDE / 2., Sidebar::WIDE),
        Motion::DURATION_MS / 2
    );
    // Already there is nothing to do, and reduced motion is always at once.
    assert_eq!(ms(Sidebar::NARROW, Sidebar::NARROW), 0);
    assert_eq!(Sidebar::duration_ms(0., Sidebar::WIDE, true), 0);
}

#[test]
fn a_fitted_line_is_as_large_as_fits_never_wider_than_its_room_until_its_smallest_size() {
    let fit = Fit::CAPTION;
    // A short name in a wide place is at the size captions are written in.
    assert_eq!(fit.size_in(200., 4), fit.largest);
    // The same place, a longer name: smaller, and still inside the place.
    for characters in [9, 11, 14, 18] {
        for room in [56., 60., 72., 82., 103.] {
            let size = fit.size_in(room, characters);
            assert!((fit.smallest..=fit.largest).contains(&size));
            assert!(
                fit.width_of(characters, size) <= room + 0.01 || size == fit.smallest,
                "{characters} characters at {size} in {room}"
            );
            // More room never makes it smaller, and a longer name never makes it larger.
            assert!(fit.size_in(room + 10., characters) >= size);
            assert!(fit.size_in(room, characters + 1) <= size);
        }
    }
    // Nothing measured yet, or nothing at all: the smallest, never a size of zero or one outside the box.
    assert_eq!(fit.size_in(0., 11), fit.smallest);
    assert_eq!(fit.size_in(-5., 0), fit.smallest);
}

#[test]
fn a_headline_shrinks_with_the_room_it_has_and_never_goes_below_a_readable_size() {
    // The longest line of a headline is "Make files" (10 characters) in a bold, wide face.
    let at = |room: f32| Headline::size_for(room, 10);
    assert_eq!(at(10_000.), Headline::LARGEST);
    // The narrower the room, the smaller: never larger than with more room.
    assert!(at(500.) <= at(700.));
    assert!(at(300.) <= at(500.));
    // The line fits the room it is given, as far as that is possible at all.
    for room in [320., 420., 520., 640., 780.] {
        let size = at(room);
        assert!(
            Headline::width_of(10, size) <= room || size == Headline::SMALLEST,
            "{size} in {room}"
        );
    }
    // And there is a floor, so a very narrow window gets a small title, not an invisible one.
    assert_eq!(at(10.), Headline::SMALLEST);
}

#[test]
fn a_title_on_a_phone_is_held_below_what_the_room_alone_would_allow_and_a_desktop_is_unchanged() {
    // The Asus: a page 346 wide minus its margins. "Ready when" has 10 characters.
    let room = 346. - 2. * Form::Compact.page_side_margin();
    let by_room = Headline::size_for(room, 10);
    let on_phone = Headline::size_in(Form::Compact, room, 10);
    assert!(
        on_phone < by_room,
        "the phone's title ({on_phone}) is as large as the room alone makes it ({by_room})"
    );
    assert!(on_phone <= Headline::largest_in(Form::Compact));
    // The desktop is exactly what it was: the ceiling is the old one, so the rule gives the old answer.
    for room in [320., 640., 900., 10_000.] {
        assert_eq!(
            Headline::size_in(Form::Wide, room, 10),
            Headline::size_for(room, 10),
            "room {room}"
        );
    }
    assert_eq!(Headline::largest_in(Form::Wide), Headline::LARGEST);
    // A phone's ceiling is lower than the desktop's, and not under the floor, or a title would not be one.
    assert!(Headline::largest_in(Form::Compact) < Headline::largest_in(Form::Wide));
    assert!(Headline::largest_in(Form::Compact) >= Headline::SMALLEST);
    // A phone turned narrower than its ceiling's room still shrinks the title, down to the floor.
    assert!(
        Headline::size_in(Form::Compact, 150., 10) <= Headline::size_in(Form::Compact, 300., 10)
    );
    assert_eq!(
        Headline::size_in(Form::Compact, 10., 10),
        Headline::SMALLEST
    );
}
#[test]
fn a_turn_in_the_middle_takes_only_the_time_of_the_distance_that_is_left() {
    // Open or close all the way: the whole duration.
    assert_eq!(Motion::duration_ms(0., 1., false), Motion::DURATION_MS);
    assert_eq!(Motion::duration_ms(1., 0., false), Motion::DURATION_MS);
    // Turned back at the half: half the distance, half the time, so the speed is the same.
    assert_eq!(Motion::duration_ms(0.5, 0., false), Motion::DURATION_MS / 2);
    assert_eq!(
        Motion::duration_ms(0.25, 1., false),
        Motion::DURATION_MS * 3 / 4
    );
    // Already there: nothing to do.
    assert_eq!(Motion::duration_ms(1., 1., false), 0);
    // And with motion reduced it is always at once, whatever the distance.
    assert_eq!(Motion::duration_ms(0., 1., true), 0);
}

#[test]
fn a_menu_is_mounted_while_it_is_open_or_still_closing_and_only_then() {
    // Open: shown. Closing and not yet at its end: still shown, or it would vanish at once.
    assert!(Motion::mounted(true, 0.0));
    assert!(Motion::mounted(false, 0.4));
    // Closed and arrived at nothing: gone.
    assert!(!Motion::mounted(false, 0.0));
}

#[test]
fn what_is_dropped_on_the_extensions_page_is_one_package_or_nothing_and_says_why() {
    use std::path::PathBuf;
    let one = |kind| Install::of_drop(vec![PathBuf::from("/a/clip")], |_| kind);
    // A folder and a file are both packages: the owner tells which by looking at the path.
    assert_eq!(
        one(DropKind::Folder),
        Install::Take(PathBuf::from("/a/clip"))
    );
    assert_eq!(
        one(DropKind::File { length: 10 }),
        Install::Take(PathBuf::from("/a/clip"))
    );
    // Nothing to install in an empty file, a path that is gone, or something that is neither.
    assert_eq!(
        one(DropKind::File { length: 0 }),
        Install::Refuse(Install::EMPTY)
    );
    assert_eq!(
        one(DropKind::Unreadable),
        Install::Refuse(Install::UNREADABLE)
    );
    assert_eq!(
        one(DropKind::Other),
        Install::Refuse(Install::NOT_A_PACKAGE)
    );
    // Nothing dropped is not an error to shout about: there is simply nothing to do.
    assert_eq!(
        Install::of_drop(Vec::new(), |_| DropKind::Folder),
        Install::Nothing
    );
    // Two or more at once is ambiguous (which one is the package?), so none is taken.
    let two = vec![PathBuf::from("/a/one"), PathBuf::from("/a/two")];
    assert_eq!(
        Install::of_drop(two, |_| DropKind::Folder),
        Install::Refuse(Install::ONE_AT_A_TIME)
    );
    // The same path twice is still one package.
    let same = vec![PathBuf::from("/a/clip"), PathBuf::from("/a/clip")];
    assert_eq!(
        Install::of_drop(same, |_| DropKind::Folder),
        Install::Take(PathBuf::from("/a/clip"))
    );
}

#[test]
fn the_interface_follows_orientation_portrait_is_compact_and_landscape_or_square_is_wide() {
    // A phone held upright, in the sizes phones come in.
    for (width, height) in [
        (360., 780.),
        (393., 852.),
        (412., 915.),
        (430., 932.),
        (320., 568.),
    ] {
        for held in [false, true] {
            assert_eq!(
                Form::of(width, height, held),
                Form::Compact,
                "{width} x {height} is upright"
            );
        }
    }
    // The same phone on its side is landscape, and is not given the portrait arrangement: the
    // width alone (900 here) would have said wide too, but 360 on its side would not. Held in the hands it is not
    // given the desktop's either; a window of that shape on a desk is.
    for (width, height) in [(780., 360.), (915., 412.), (932., 430.)] {
        assert_eq!(
            Form::of(width, height, true),
            Form::Side,
            "{width} x {height} is a phone on its side"
        );
        assert_eq!(
            Form::of(width, height, false),
            Form::Wide,
            "{width} x {height} is on its side"
        );
    }
    // The desktop window, at its smallest and its usual size, is landscape.
    for (width, height) in [(760., 560.), (1100., 720.), (1920., 1080.)] {
        assert_eq!(
            Form::of(width, height, false),
            Form::Wide,
            "{width} x {height} is a desktop window"
        );
    }
    // Square is not taller than wide, so it is not portrait.
    assert_eq!(Form::of(600., 600., false), Form::Wide);
    // A narrow window is not a phone unless it is also taller than it is wide, and the one that is
    // taller than wide is given the arrangement made for that shape, narrow or not.
    assert_eq!(Form::of(500., 400., false), Form::Wide);
    assert_eq!(Form::of(800., 900., false), Form::Compact);
    // Not yet measured, or not numbers: the desktop arrangement, never a flash of the other; and on a screen that is
    // held, never the desktop's, whose buttons are for a window a phone does not have.
    for unknown in [0., -1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(
            Form::of(unknown, 800., false),
            Form::Wide,
            "width {unknown} was taken for a phone"
        );
        assert_eq!(
            Form::of(400., unknown, false),
            Form::Wide,
            "height {unknown} was taken for a phone"
        );
        assert_eq!(Form::of(unknown, 800., true), Form::Side);
        assert_eq!(Form::of(400., unknown, true), Form::Side);
    }
}

#[test]
fn a_phone_on_its_side_has_its_places_down_the_left_edge_no_window_buttons_and_settings_from_the_top()
 {
    // The same places, in the same order, as when it is upright: only where the bar stands changes.
    assert_eq!(Form::Side.bottom_places(), Form::Compact.bottom_places());
    assert!(Form::Side.places_are_at_the_side());
    assert!(!Form::Compact.places_are_at_the_side());
    assert!(!Form::Wide.places_are_at_the_side());
    // Nothing of a desktop window: no buttons for it, no "click", and Settings is the sheet a phone has (there is no right button,
    // and no bar of Settings at the side).
    assert!(!Form::Side.header_has_window_buttons());
    assert!(Form::Side.settings_places_come_from_the_top());
    assert!(Form::Side.header_hides_on_scroll());
    assert_eq!(Form::Side.circle_invitation(), "tap to choose files");
    // The pages themselves have the width of a landscape screen, and are laid out for it.
    assert!(!Form::Side.send_is_the_circle_alone());
    assert!(Form::Side.page_puts_portal_beside_text());
    assert!(Form::Side.nearby_list_is_beside());
    assert_eq!(
        Form::Side.device_card_width(),
        Form::Wide.device_card_width()
    );
}

#[test]
fn the_phone_shows_the_circle_alone_with_all_four_places_at_the_bottom_and_the_desktop_none_of_that()
 {
    // On a phone: Send is the circle, and every place has a tab at the bottom, in the order the desktop has
    // them across the top. Without Send there would be no way back to it from the others.
    assert_eq!(
        Form::Compact.bottom_places(),
        &[
            BottomPlace::Send,
            BottomPlace::Receive,
            BottomPlace::Peers,
            BottomPlace::Transfers
        ]
    );
    let places = Form::Compact.bottom_places();
    for (index, place) in places.iter().enumerate() {
        assert!(
            !places[..index].contains(place),
            "{place:?} is in the bar twice"
        );
    }
    assert!(Form::Compact.send_is_the_circle_alone());
    assert_eq!(Form::Compact.circle_invitation(), "tap to choose files");
    // On the desktop nothing changes: the places stay across the top.
    assert!(Form::Wide.bottom_places().is_empty());
    assert!(!Form::Wide.send_is_the_circle_alone());
    assert_eq!(
        Form::Wide.circle_invitation(),
        "click to browse your computer"
    );
    // A phone is touched, so it never says "click" or names a computer.
    let phone = Form::Compact.circle_invitation();
    assert!(!phone.contains("click") && !phone.contains("computer"));
}

#[test]
fn the_phone_header_keeps_only_what_a_phone_has_room_and_use_for() {
    // A phone has no window of its own to minimise or close, and says connected by its dot alone.
    assert!(!Form::Compact.header_has_window_buttons());
    assert!(!Form::Compact.header_says_connected_in_words());
    // The desktop header is exactly what it was.
    assert!(Form::Wide.header_has_window_buttons());
    assert!(Form::Wide.header_says_connected_in_words());
    // Only a phone gives its short screen back by hiding the bar while reading down; the desktop keeps it.
    assert!(Form::Compact.header_hides_on_scroll());
    assert!(!Form::Wide.header_hides_on_scroll());
    // A message at the top starts under the band the header's pieces lie in, on both phone forms: over the page's top
    // corners they hid its start and covered its close button. The desktop's bar takes its own room.
    assert!(Form::Compact.clear_of_header() >= HEADER_TAB);
    assert!(Form::Side.clear_of_header() >= HEADER_TAB);
    assert_eq!(Form::Wide.clear_of_header(), 0.);
    // Less room at the sides on a phone, and a side never negative or empty.
    assert!(Form::Compact.header_side_padding() < Form::Wide.header_side_padding());
    assert!(Form::Compact.header_side_padding() > 0.);
    assert_eq!(Form::Wide.header_side_padding(), 28.);
}

#[test]
fn the_portal_is_never_wider_than_the_room_it_has_and_is_its_own_size_when_there_is_room() {
    // The narrowest phones, and one wide enough: the portal is cut to the width, or left as it is.
    assert_eq!(Form::portal_extent(420., 360.), 360.);
    assert_eq!(Form::portal_extent(420., 412.), 412.);
    assert_eq!(Form::portal_extent(420., 430.), 420.);
    assert_eq!(Form::portal_extent(420., 1100.), 420.);
    // The width itself is the limit, not a little under it: equal means it fits exactly.
    assert_eq!(Form::portal_extent(420., 420.), 420.);
    // A width that is not yet known, or not a number: the portal keeps its own size.
    for unknown in [0., -1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(
            Form::portal_extent(420., unknown),
            420.,
            "width {unknown} changed the portal"
        );
    }
}

#[test]
fn the_line_round_the_disc_is_whole_device_pixels_and_never_thinner_than_two() {
    // Whatever the screen, what is drawn is a whole number of device pixels, so it lands on the pixels and not between them.
    for scale in [1., 1.5, 2., 2.625, 3., 3.125, 3.5, 4.] {
        let device = f64::from(Form::portal_line(scale)) * scale;
        assert!(
            (device - device.round()).abs() < 1e-3,
            "at {scale} the line is {device} device pixels, not whole"
        );
        assert!(
            device.round() >= 2.,
            "at {scale} the line is {device} device pixels, thinner than two"
        );
    }
    // The phone this was measured on (a scale of 3.125): one logical pixel there is 3.125 device pixels and went missing at
    // times; the line is now five.
    assert!((f64::from(Form::portal_line(3.125)) * 3.125 - 5.).abs() < 1e-3);
    // A screen of the plain kind keeps it at two device pixels.
    assert_eq!(Form::portal_line(1.), 2.);
    // A scale that cannot be used gives what was measured to hold, not nothing and not a number that is not one.
    for unusable in [0., -2., f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            Form::portal_line(unusable),
            2.,
            "scale {unusable} changed the line"
        );
    }
}

#[test]
fn the_naming_screen_is_smaller_and_stacked_on_a_phone_and_unchanged_on_a_desktop() {
    // The title is smaller on a phone, where 56 px does not fit "Name this device" on a line.
    assert!(Form::Compact.ask_title_size() < Form::Wide.ask_title_size());
    assert_eq!(Form::Wide.ask_title_size(), 56.);
    // Field and button stand one above the other on a phone only; the desktop keeps them side by side.
    assert!(Form::Compact.ask_stacks_field_and_button());
    assert!(!Form::Wide.ask_stacks_field_and_button());
    assert_eq!(Form::Wide.ask_side_margin(), 0.);
    assert!(Form::Compact.ask_side_margin() > 0.);
}

#[test]
fn a_page_keeps_a_small_margin_on_a_phone_and_its_wide_one_on_a_desktop_and_stacks_its_portal() {
    // 56 px at each side takes 112 of a 346-wide screen: a third. On a phone the margin is a fraction of it.
    assert!(
        Form::Compact.page_side_margin() * 2. < 346. / 6.,
        "a phone's margins take too much of its width"
    );
    assert!(Form::Compact.page_side_margin() > 0.);
    // The desktop is exactly what it was.
    assert_eq!(Form::Wide.page_side_margin(), 56.);
    // Text and portal side by side only where there is the width for both.
    assert!(Form::Wide.page_puts_portal_beside_text());
    assert!(!Form::Compact.page_puts_portal_beside_text());
}

#[test]
fn the_safe_area_is_what_the_system_leaves_free_divided_by_the_screens_own_scale() {
    // The Asus, measured on the device: 1080 x 2400 and a status bar of 105 px at the top. The scale is a
    // parameter, as it is in the program: here an example of one, not the phone's.
    let scale = 3.125;
    let area = SafeArea::from_physical((0, 105, 1080, 2400), (1080, 2400), scale);
    assert!(
        (area.top - 105. / scale as f32).abs() < 1e-3,
        "the status bar was {} logical px",
        area.top
    );
    assert_eq!((area.left, area.right, area.bottom), (0., 0., 0.));
    // The same status bar on a screen of lower density is taller in logical pixels: the answer follows
    // the scale and is not a number kept in the program.
    let coarse = SafeArea::from_physical((0, 105, 1080, 2400), (1080, 2400), 1.0);
    assert!((coarse.top - 105.).abs() < 1e-3);
    assert!(coarse.top > area.top);
    // A navigation bar below and a notch at one side are carried the same way.
    let both = SafeArea::from_physical((60, 105, 1080, 2300), (1080, 2400), 2.0);
    assert_eq!(
        (both.top, both.left, both.right, both.bottom),
        (52.5, 30.0, 0.0, 50.0)
    );
}

#[test]
fn a_rectangle_that_cannot_be_right_leaves_no_margin_rather_than_a_wrong_one() {
    let none = SafeArea::default();
    let screen = (1080, 2400);
    // Not yet given by the system: all zero, which is what the Android entry reads at its very start.
    assert_eq!(SafeArea::from_physical((0, 0, 0, 0), screen, 3.0), none);
    // Empty, inverted, outside the screen, or negative.
    assert_eq!(
        SafeArea::from_physical((0, 105, 1080, 105), screen, 3.0),
        none
    );
    assert_eq!(
        SafeArea::from_physical((500, 105, 100, 2400), screen, 3.0),
        none
    );
    assert_eq!(
        SafeArea::from_physical((0, 105, 1081, 2400), screen, 3.0),
        none
    );
    assert_eq!(
        SafeArea::from_physical((0, 105, 1080, 2401), screen, 3.0),
        none
    );
    assert_eq!(
        SafeArea::from_physical((-1, 105, 1080, 2400), screen, 3.0),
        none
    );
    assert_eq!(
        SafeArea::from_physical((0, -5, 1080, 2400), screen, 3.0),
        none
    );
    // A scale that is not a positive number cannot divide anything.
    for scale in [0., -1., f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            SafeArea::from_physical((0, 105, 1080, 2400), screen, scale),
            none,
            "scale {scale}"
        );
    }
    // And the whole screen is no margin at all.
    assert_eq!(
        SafeArea::from_physical((0, 0, 1080, 2400), screen, 3.0),
        none
    );
}

#[test]
fn while_the_device_has_no_name_there_is_no_sidebar_and_no_way_into_a_page() {
    use peersus_core::config::Navigation;
    // Inside Settings, asked for, with every option, on either form: without a name the sidebar is not
    // there at all. This is the state in which the menu's Settings stood beside the question and covered it.
    for form in [Form::Wide, Form::Compact] {
        for keep_icons in [false, true] {
            for inside_settings in [false, true] {
                assert_eq!(
                    Sidebar::state_for(false, inside_settings, keep_icons, form),
                    Navigation::Hidden,
                    "a sidebar was shown beside the question ({form:?}, settings {inside_settings}, icons {keep_icons})"
                );
            }
        }
    }
    // With a name, on a desktop, nothing is different from before: it is exactly the rule that was.
    for keep_icons in [false, true] {
        for inside_settings in [false, true] {
            assert_eq!(
                Sidebar::state_for(true, inside_settings, keep_icons, Form::Wide),
                Sidebar::state(inside_settings, keep_icons)
            );
        }
    }
    // Opening a page from the menu leads nowhere until there is a name, and to the page after.
    assert_eq!(Sidebar::page_opened(false, "settings"), None);
    assert_eq!(Sidebar::page_opened(true, "settings"), Some("settings"));
}

#[test]
fn a_phones_page_is_closer_above_and_between_its_sections_and_a_desktops_is_what_it_was() {
    // The desktop is exactly what it was before this rule existed.
    assert_eq!(Form::Wide.page_vertical_padding(), 28.);
    assert_eq!(Form::Wide.page_section_gap(), 28.);
    // A phone is closer, but never touching and never negative: a page is still a page.
    assert!(Form::Compact.page_vertical_padding() < Form::Wide.page_vertical_padding());
    assert!(Form::Compact.page_section_gap() < Form::Wide.page_section_gap());
    assert!(Form::Compact.page_vertical_padding() > 0.);
    assert!(Form::Compact.page_section_gap() > 0.);
}

#[test]
fn on_a_phone_settings_has_no_bar_at_the_side_and_its_places_come_down_from_the_top() {
    use peersus_core::config::Navigation;
    // The desktop shows names beside the pictures in Settings.
    assert_eq!(
        Sidebar::state_for(true, true, false, Form::Wide),
        Navigation::Expanded
    );
    assert_eq!(
        Sidebar::state_for(true, true, true, Form::Wide),
        Navigation::Expanded
    );
    // A phone has no room for a bar at the side, and so none: its places are a sheet that comes down from the top.
    assert_eq!(
        Sidebar::state_for(true, true, false, Form::Compact),
        Navigation::Hidden
    );
    assert!(Form::Compact.settings_places_come_from_the_top());
    assert!(!Form::Wide.settings_places_come_from_the_top());
    // A previously saved Keep-icons preference must not resurrect the redundant mobile sidebar.
    assert_eq!(
        Sidebar::state_for(true, true, true, Form::Compact),
        Navigation::Hidden
    );
    // Outside Settings the phone has no sidebar, as the desktop has none: nothing else is touched.
    assert_eq!(
        Sidebar::state_for(true, false, false, Form::Compact),
        Navigation::Hidden
    );
    assert_eq!(
        Sidebar::state_for(true, false, true, Form::Compact),
        Navigation::Hidden
    );
    // What the phone's bar is wide enough for, and what the page keeps of a 346-wide screen.
    assert!(Sidebar::width_for(Navigation::Icons) < 346. / 4.);
    assert!(
        Sidebar::width_for(Navigation::Expanded) > 346. / 2.5,
        "the desktop's bar is no longer too wide for a phone, so this rule is not needed"
    );
}

#[test]
fn the_sheet_from_the_top_starts_wholly_above_the_screen_and_ends_in_its_place() {
    let (height, top) = (500., 24.);
    // Closed, its lower edge is at the top of the usable area and nothing of it shows.
    assert_eq!(TopSheet::top(0., height, top), top - height);
    // Open, it is exactly in place.
    assert_eq!(TopSheet::top(1., height, top), top);
    // It never goes above the closed position nor below the open one, whatever progress it is handed.
    assert_eq!(TopSheet::top(-4., height, top), top - height);
    assert_eq!(TopSheet::top(7., height, top), top);
    // And it only ever goes one way: down while opening.
    let mut last = f32::MIN;
    for step in 0..=20 {
        let now = TopSheet::top(step as f32 / 20., height, top);
        assert!(now >= last, "the sheet went back up at step {step}");
        last = now;
    }
}

#[test]
fn the_dark_layer_under_the_sheet_grows_with_it_and_never_hides_the_page_completely() {
    assert_eq!(TopSheet::scrim(0.), 0);
    assert!(
        TopSheet::scrim(1.) > 0 && TopSheet::scrim(1.) < 255,
        "the page must stay visible under the layer"
    );
    assert!(TopSheet::scrim(0.5) < TopSheet::scrim(1.));
    assert_eq!(TopSheet::scrim(-3.), 0);
    assert_eq!(TopSheet::scrim(9.), TopSheet::scrim(1.));
}

#[test]
fn the_sheet_is_never_taller_than_most_of_the_screen_it_comes_down_on() {
    for room in [320., 640., 900.] {
        let height = TopSheet::height(room);
        assert!(height <= room * 0.9 + 0.01, "{height} of {room}");
        assert!(height > 0.);
    }
    assert_eq!(TopSheet::height(0.), 0.);
    assert_eq!(TopSheet::height(-5.), 0.);
}

#[test]
fn turning_the_relays_on_is_accepted_even_when_an_address_was_fixed_and_returns_routing_to_the_systems()
 {
    use peersus_core::config::SettingsPatch;
    // The case in the screenshot: routing fixed to one local address (as the command line can do), so relays
    // are off. Asking for them on with only "relays on" was refused: the owner will not hold both.
    let mut fixed = Settings::default();
    fixed.network.bind = Some("192.168.1.20".parse().unwrap());
    fixed.network.allow_relay = false;
    assert!(
        fixed.validate().is_ok(),
        "the starting point is not itself valid"
    );
    let only_relays = SettingsPatch {
        allow_relay: Some(true),
        ..SettingsPatch::default()
    };
    assert!(
        only_relays.apply(&fixed).is_err(),
        "the premise is gone: relays alone are no longer refused"
    );
    // What the switch sends now is accepted, and leaves routing the system's and the relays on.
    let accepted = relay_patch(true)
        .apply(&fixed)
        .expect("the relay switch was refused with an address fixed");
    assert!(accepted.network.allow_relay);
    assert_eq!(
        accepted.network.bind, None,
        "routing was not returned to the system's"
    );
    // On a device with nothing fixed it changes nothing but the relays, which are on by default.
    let plain = relay_patch(true).apply(&Settings::default()).unwrap();
    assert_eq!(plain, Settings::default());
    // Off changes only the relays, and leaves an address that was fixed exactly where it was.
    let mut routed = Settings::default();
    routed.network.bind = Some("192.168.1.20".parse().unwrap());
    routed.network.allow_relay = false;
    let off = relay_patch(false).apply(&routed).unwrap();
    assert!(!off.network.allow_relay);
    assert_eq!(
        off.network.bind, routed.network.bind,
        "turning the relays off moved the address"
    );
    // And the relays are on by default: the internet is what a new device reaches.
    assert!(Settings::default().network.allow_relay);
    assert_eq!(Settings::default().network.bind, None);
}

#[test]
fn desktop_batches_preserve_every_selected_path_in_owner_sized_commands() {
    let paths: Vec<_> = (0..40)
        .map(|number| std::path::PathBuf::from(format!("selected-{number}.bin")))
        .collect();
    let batches: Vec<_> = path_sends(paths.clone()).collect();
    assert!(
        batches.iter().all(|batch| (1..=16).contains(&batch.len())),
        "desktop selection exceeded command capacity"
    );
    assert_eq!(
        batches.into_iter().flatten().collect::<Vec<_>>(),
        paths,
        "desktop selection was dropped, duplicated or reordered"
    );
    assert!(path_sends(Vec::new()).next().is_none());
}

#[test]
fn the_files_a_picker_hands_over_are_sent_to_the_chosen_device_in_batches_under_cleaned_names() {
    let held: Vec<(u64, String)> = (0..40).map(|n| (n, format!("../f{n}.bin"))).collect();
    let plan = held_sends(&held, "laptop");
    // Never more in one command than the owner accepts, and every file is in exactly one command.
    assert!(
        plan.iter().all(|files| (1..=16).contains(&files.len())),
        "a batch the owner would refuse"
    );
    assert_eq!(plan.iter().map(Vec::len).sum::<usize>(), 40);
    let numbers: Vec<u64> = plan.iter().flatten().map(|(number, _)| *number).collect();
    assert_eq!(
        numbers,
        (0..40).collect::<Vec<_>>(),
        "files were lost, repeated or put out of order"
    );
    // The name that goes is the cleaned one: the provider's own name is not trusted, and a path in it is dropped.
    assert!(
        plan.iter()
            .flatten()
            .all(|(_, name)| !name.contains('/') && name.ends_with(".bin"))
    );
    assert_eq!(plan[0][0].1, "f0.bin");
    // One file is one batch of one; none is none.
    assert_eq!(held_sends(&[(7, "a.jpg".to_owned())], "x").len(), 1);
    assert!(held_sends(&[], "x").is_empty());
}

#[test]
fn a_name_the_provider_did_not_give_is_a_plain_one_and_not_an_empty_offer() {
    let plan = held_sends(
        &[
            (1, String::new()),
            (2, "..".to_owned()),
            (3, "  ".to_owned()),
        ],
        "x",
    );
    let names: Vec<&str> = plan
        .iter()
        .flatten()
        .map(|(_, name)| name.as_str())
        .collect();
    assert_eq!(
        names,
        ["file", "file", "file"],
        "a file with no name was offered under an empty or dotted one"
    );
}

#[test]
fn a_device_card_keeps_its_width_where_cards_stand_side_by_side_and_none_where_they_would_not_fit()
{
    // The desktop is as it was.
    assert_eq!(Form::Wide.device_card_width(), Some(280.));
    // A phone keeps none: the card is as wide as the screen, with the cards one under another.
    assert_eq!(Form::Compact.device_card_width(), None);
    // Why: two cards of the desktop's width, with the gap between them, are wider than the narrowest phones
    // in common use, so the second would be pushed off the screen.
    let own = Form::Wide.device_card_width().unwrap();
    for screen in [320., 345.6, 360., 412.] {
        assert!(
            own * 2. + 14. + 2. * Form::Compact.page_side_margin() > screen - 1.,
            "two cards fit {screen}: the rule is no longer needed"
        );
    }
}

#[test]
fn the_buttons_of_a_device_card_stand_under_its_name_on_a_phone_and_beside_it_on_the_desktop() {
    // The desktop is exactly as it was: the buttons in the row of the card.
    assert!(!Form::Wide.device_card_buttons_stack());
    // A phone has no room for them beside the name, which would be left a column of one letter.
    assert!(Form::Compact.device_card_buttons_stack());
    // The same shape as the rule for a field and its button, so the two cannot disagree about what a phone is.
    for form in [Form::Wide, Form::Compact] {
        assert_eq!(
            form.device_card_buttons_stack(),
            form.ask_stacks_field_and_button()
        );
    }
}

#[test]
fn every_look_of_the_watcher_at_the_camera_makes_the_state_it_should() {
    let code = || Some("peersus:1:ab".to_owned());
    // Shut: nothing the camera says matters, since no one asked it anything. Nothing is joined, nothing closed.
    for (found, refused) in [(None, false), (None, true), (code(), false), (code(), true)] {
        let tick = look(Looking::Shut, found, refused);
        assert_eq!(
            tick,
            Tick {
                looking: Looking::Shut,
                join: None,
                close: false
            }
        );
    }
    // Scanning, nothing yet: it goes on, and the camera stays open.
    assert_eq!(
        look(Looking::Scanning, None, false),
        Tick {
            looking: Looking::Scanning,
            join: None,
            close: false
        }
    );
    // Scanning and a code is read: joined, and the camera is closed, so that it does not read the same code again.
    assert_eq!(
        look(Looking::Scanning, code(), false),
        Tick {
            looking: Looking::Shut,
            join: code(),
            close: true
        }
    );
    // Scanning and refused: said in the circle, and the camera is closed.
    assert_eq!(
        look(Looking::Scanning, None, true),
        Tick {
            looking: Looking::Refused,
            join: None,
            close: true
        }
    );
    // A code and a refusal together: the code wins, since the person did get a code.
    assert_eq!(
        look(Looking::Scanning, code(), true),
        Tick {
            looking: Looking::Shut,
            join: code(),
            close: true
        }
    );
    // Refused already: it stays so with nothing new, and a code that comes late is still joined.
    assert_eq!(
        look(Looking::Refused, None, true),
        Tick {
            looking: Looking::Refused,
            join: None,
            close: false
        }
    );
    assert_eq!(
        look(Looking::Refused, code(), false),
        Tick {
            looking: Looking::Shut,
            join: code(),
            close: true
        }
    );
}

#[test]
fn while_the_camera_is_shown_the_circle_has_no_words_and_the_other_states_say_what_to_do() {
    // A scan with a picture of the camera: the camera, and no words over it; whatever the state is before.
    assert_eq!(circle_for(Looking::Scanning, true), Circle::Camera);
    // A scan with no picture yet: a clear circle with one small line, never a headline like "looking".
    match circle_for(Looking::Scanning, false) {
        Circle::Starting(line) => {
            assert!(!line.is_empty() && !line.to_lowercase().contains("looking"))
        }
        other => panic!("a scan with no picture yet was {other:?}"),
    }
    // Shut: an invitation to tap, in words, whether or not a picture is left over from before.
    for has_picture in [false, true] {
        match circle_for(Looking::Shut, has_picture) {
            Circle::Words(headline, line) => {
                assert!(!headline.is_empty() && line.contains("tap"))
            }
            other => panic!("a shut camera was {other:?}"),
        }
        // Told no: where to put it right, and a picture left from before does not hide it.
        match circle_for(Looking::Refused, has_picture) {
            Circle::Words(_, line) => assert!(line.contains("settings")),
            other => panic!("a refused camera was {other:?}"),
        }
    }
    // The shut and the refused words are not the same words.
    assert_ne!(
        circle_for(Looking::Shut, false),
        circle_for(Looking::Refused, false)
    );
    assert_eq!(Looking::default(), Looking::Shut);
}

#[test]
fn of_the_files_chosen_for_a_picture_the_first_is_used_and_every_other_is_let_go_of() {
    let file = |number: u64| (number, format!("{number}.png"));
    // One: it is used and nothing is left over.
    assert_eq!(
        picture_choice(&[file(7)]),
        Some(((7, "7.png".to_owned()), Vec::new()))
    );
    // Several (a provider may hand over more than was asked for): the first in the order they came is used, and
    // each of the others, which nobody will take, is to be released, none is lost and none is used twice.
    let (used, rest) = picture_choice(&[file(3), file(9), file(4)]).unwrap();
    assert_eq!(used, file(3));
    assert_eq!(rest, vec![file(9), file(4)]);
    // None: nothing to use and nothing to let go of.
    assert_eq!(picture_choice(&[]), None);
}

#[test]
fn stopping_ends_a_scan_that_is_on_and_does_nothing_to_one_that_is_not() {
    // On: back to shut, with the camera closed, and nothing joined.
    assert_eq!(
        stop(Looking::Scanning),
        Tick {
            looking: Looking::Shut,
            join: None,
            close: true
        }
    );
    // Told no: stop is also the way back, closing what may still be open and returning to the invitation to tap.
    assert_eq!(
        stop(Looking::Refused),
        Tick {
            looking: Looking::Shut,
            join: None,
            close: true
        }
    );
    // Shut already: there is nothing to close, and closing a camera that is not open is not asked of the platform.
    assert_eq!(
        stop(Looking::Shut),
        Tick {
            looking: Looking::Shut,
            join: None,
            close: false
        }
    );
    // Nothing is ever joined by stopping.
    for now in [Looking::Shut, Looking::Scanning, Looking::Refused] {
        assert_eq!(stop(now).join, None);
    }
}

fn row(id: &str, incoming: bool, phase: Phase, from: Option<&str>) -> Progress {
    Progress {
        id: id.into(),
        peer: format!("peer-{id}"),
        name: format!("{id}.bin"),
        folders: Vec::new(),
        bytes: 0,
        incoming,
        from: from.map(str::to_owned),
        percent: 0,
        code: None,
        expires_at: None,
        saved_as: None,
        phase,
    }
}

#[test]
fn the_totals_count_only_files_that_moved_whole_and_say_which_way() {
    let view = View {
        transfers: vec![
            row("a", false, Phase::Done, None),
            row("b", false, Phase::Done, None),
            row("c", true, Phase::Done, None),
            // Not moved: refused, cancelled, failed, or still going. None of them is a file that went anywhere.
            row("d", false, Phase::Cancelled, None),
            row("e", true, Phase::Failed("x".into()), None),
            row("f", true, Phase::Moving, None),
            row("g", true, Phase::Waiting, None),
        ],
        ..View::default()
    };
    assert_eq!(
        view.file_totals(),
        FileTotals {
            sent: 2,
            received: 1,
            total: 3
        }
    );
    assert_eq!(
        View::default().file_totals(),
        FileTotals {
            sent: 0,
            received: 0,
            total: 0
        }
    );
}

#[test]
fn a_few_letters_in_order_find_a_row_and_a_wrong_order_does_not() {
    assert!(fuzzy_score("bkg", "Run in the background").is_some());
    assert!(fuzzy_score("RUN", "Run in the background").is_some());
    assert!(
        fuzzy_score("gkb", "Run in the background").is_none(),
        "letters out of order matched"
    );
    assert!(fuzzy_score("zzz", "Run in the background").is_none());
    // Nothing typed is no search: not a list of everything.
    assert_eq!(fuzzy_score("", "Run in the background"), None);
    assert_eq!(fuzzy_score("   ", "Run in the background"), None);
}

#[test]
fn a_match_that_reads_as_the_words_beats_one_scattered_across_them() {
    // "round" read straight off "background", and the same five letters found one here and one there inside other words: none of the
    // scattered ones begins a word, so only the straight run is rewarded.
    let straight = fuzzy_score("round", "Run in the background").unwrap();
    let scattered = fuzzy_score("round", "aaroaubanadoo").unwrap();
    assert!(straight > scattered, "{straight} should beat {scattered}");
}

#[test]
fn the_search_leads_to_the_tab_of_the_row_and_puts_a_title_before_a_note() {
    let found = search_settings("reduce", &View::default(), Language::ENGLISH);
    assert_eq!(
        found.first().map(|entry| entry.title.as_str()),
        Some("Reduce motion")
    );
    assert_eq!(
        found.first().map(|entry| entry.target.tab()),
        Some(SettingsTab::Appearance)
    );
    // Nothing typed: nothing found.
    assert!(search_settings("", &View::default(), Language::ENGLISH).is_empty());
    let romanian = search_settings(
        "limb",
        &View::default(),
        crate::i18n::fixture::language("ro"),
    );
    assert!(
        romanian.iter().any(|entry| entry.title == "Limbă"),
        "the translated language row was not found"
    );
}

#[test]
fn settings_search_can_reach_every_group() {
    for tab in SettingsTab::ALL {
        assert!(
            search_settings(tab.title(), &View::default(), Language::ENGLISH)
                .iter()
                .any(|entry| entry.target.tab() == tab),
            "search could not reach {tab:?}"
        );
    }
}

/// An outgoing transfer that came of the files code of generation `code`, `percent` of the way.
fn of_code(id: &str, code: u64, phase: Phase, percent: u8) -> Progress {
    Progress {
        code: Some(code),
        percent,
        ..row(id, false, phase, None)
    }
}
fn on_show(generation: u64, live: bool) -> Option<FilesCode> {
    Some(FilesCode {
        generation,
        live,
        pending: false,
    })
}

fn arrived(id: &str, at: &str) -> Progress {
    Progress {
        saved_as: Some(std::path::PathBuf::from(at)),
        ..row(id, true, Phase::Done, None)
    }
}

/// A transfer of the device `peer`, in the one list that every page reads.
fn with(id: &str, peer: &str, name_of_peer: &str, phase: Phase) -> Progress {
    Progress {
        peer: peer.into(),
        from: Some(name_of_peer.into()),
        name: format!("{id}.txt"),
        ..row(id, false, phase, None)
    }
}

#[test]
fn the_history_of_a_device_is_what_was_with_that_identity_newest_first_and_not_with_one_of_the_same_name()
 {
    let list = [
        with("a", "peer-1", "Phone", Phase::Done),
        with("b", "peer-2", "Phone", Phase::Done),
        with("c", "peer-1", "Phone", Phase::Done),
    ];
    let names = |peer: &str| {
        history_with(peer, &list)
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names("peer-1"),
        ["c", "a"],
        "two devices of one name were mixed together, or the newest was not first"
    );
    assert_eq!(names("peer-2"), ["b"]);
    assert!(
        names("peer-3").is_empty(),
        "a device with no history got someone else's"
    );
}

#[test]
fn a_device_has_no_history_of_its_own_once_the_list_forgets_what_was_with_it() {
    // ONE list: Send, Receive, Settings and a device's page all read it. When Clear forgets what is over there is nothing left
    // under the device either, and a device met only by a code is not listed any more.
    let before = [
        with("a", "peer-1", "Phone", Phase::Done),
        with("b", "peer-1", "Phone", Phase::Failed("x".into())),
    ];
    assert_eq!(history_with("peer-1", &before).len(), 2);
    assert_eq!(unpaired_with_history(&[], &before).len(), 1);
    let after: Vec<Progress> = Vec::new();
    assert!(
        history_with("peer-1", &after).is_empty(),
        "a device kept a history the list had forgotten"
    );
    assert!(
        unpaired_with_history(&[], &after).is_empty(),
        "a device with nothing in the list was still listed"
    );
}

#[test]
fn what_is_still_going_with_a_device_is_not_a_line_of_its_history() {
    let list = [
        with("a", "peer-1", "Phone", Phase::Done),
        with("b", "peer-1", "Phone", Phase::Moving),
        with("c", "peer-1", "Phone", Phase::Waiting),
    ];
    let ids: Vec<_> = history_with("peer-1", &list)
        .iter()
        .map(|item| item.id.clone())
        .collect();
    assert_eq!(ids, ["a"], "a transfer still going was listed as history");
    // A device with only something going is not a device met by a code with a history yet.
    assert!(unpaired_with_history(&[], &list[1..]).is_empty());
}

#[test]
fn a_device_page_names_its_latest_five_and_says_how_many_more_there_are() {
    let many: Vec<Progress> = (0..8)
        .map(|n| with(&format!("t{n}"), "peer-1", "Phone", Phase::Done))
        .collect();
    let (shown, more) = history_shown("peer-1", &many);
    let ids: Vec<_> = shown.iter().map(|item| item.id.clone()).collect();
    assert_eq!(
        ids,
        ["t7", "t6", "t5", "t4", "t3"],
        "the latest were not the ones named, newest first"
    );
    assert_eq!(more, 3);
    let (few, none) = history_shown("peer-1", &many[..2]);
    assert_eq!(
        (few.len(), none),
        (2, 0),
        "a device with fewer than the cap was cut or said it had more"
    );
}

#[test]
fn every_result_of_a_link_test_is_told_in_every_language_and_only_a_working_connection_is_told_as_good()
 {
    use peersus_core::model::LinkTest;
    for (test, tone) in [
        (LinkTest::Trying, Tone::Going),
        (LinkTest::Paired, Tone::Good),
        (LinkTest::NeedsPairing, Tone::Bad),
        (LinkTest::Unreachable, Tone::Bad),
    ] {
        for relays in [true, false] {
            let (title, advice, said) = link_test_words_in(Language::ENGLISH, test, relays);
            assert!(
                !title.is_empty() && !advice.is_empty(),
                "{test:?} says nothing"
            );
            assert_eq!(said, tone, "{test:?} with relays {relays}");
            // Every sentence can be translated: it is a message of the template.
            assert!(crate::i18n::has(&title), "{title:?} is not in the template");
            assert!(
                crate::i18n::has(&advice),
                "{advice:?} is not in the template"
            );
        }
    }
    // The two results that need something done ask for different things.
    let (_, pair_again, _) = link_test_words_in(Language::ENGLISH, LinkTest::NeedsPairing, true);
    let (_, wait, _) = link_test_words_in(Language::ENGLISH, LinkTest::Unreachable, true);
    assert_ne!(pair_again, wait);
    assert!(pair_again.contains("pair"), "{pair_again}");
}

#[test]
fn a_device_that_does_not_answer_is_told_to_turn_the_relays_on_only_when_they_are_off_and_never_to_look_for_an_address()
 {
    use peersus_core::model::LinkTest;
    let (_, with_relays, _) = link_test_words_in(Language::ENGLISH, LinkTest::Unreachable, true);
    let (_, without, _) = link_test_words_in(Language::ENGLISH, LinkTest::Unreachable, false);
    // Off: the cause that is probably true is named, and so is what mends it.
    assert!(
        without.contains("relays") && without.contains("Settings"),
        "{without}"
    );
    // On: it must not tell a person to turn on what is on.
    assert!(!with_relays.contains("Turn on"), "{with_relays}");
    // Neither sends anyone to type an address: across networks a known address does not let a connection in.
    for words in [&with_relays, &without] {
        assert!(
            !words.to_lowercase().contains("ip address")
                && !words.to_lowercase().contains("type an"),
            "{words}"
        );
    }
}

#[test]
fn the_result_of_a_link_test_in_the_snapshot_reaches_the_device_it_is_about_and_no_other() {
    use peersus_core::model::{LinkTest, PeerId};
    let mut snapshot = snapshot_with(vec![peer("Alex", true), peer("Claude", false)], vec![]);
    let tested = snapshot.peers[1].id.clone();
    snapshot.link_tests.insert(tested, LinkTest::NeedsPairing);
    snapshot
        .link_tests
        .insert(PeerId("nobody".into()), LinkTest::Paired);
    let view = View::of(&snapshot);
    assert_eq!(
        view.devices[0].link_test, None,
        "a device that was not tested was given a result"
    );
    assert_eq!(view.devices[1].link_test, Some(LinkTest::NeedsPairing));
}

#[test]
fn a_device_met_only_by_code_is_listed_once_per_identity_and_a_saved_one_is_not() {
    let saved = Device {
        id: "peer-saved".into(),
        name: "Laptop".into(),
        presence: Presence::Offline,
        grants: Default::default(),
        picture: None,
        paired_at: 0,
        last_seen: None,
        link_test: None,
        typed_address: None,
    };
    let list = [
        with("a", "peer-code-1", "Phone", Phase::Done),
        with("b", "peer-saved", "Laptop", Phase::Done),
        // Same name as the first, another identity: a different device, not merged.
        with("c", "peer-code-2", "Phone", Phase::Done),
        with("d", "peer-code-1", "Phone", Phase::Done),
    ];
    let met = unpaired_with_history(std::slice::from_ref(&saved), &list);
    assert_eq!(
        met,
        vec![
            ("peer-code-1", Some("Phone")),
            ("peer-code-2", Some("Phone"))
        ]
    );
    assert!(unpaired_with_history(&[saved], &[]).is_empty());
}

#[test]
fn the_bounded_transfer_list_keeps_every_row_and_puts_active_files_before_newest_history() {
    let list = [
        row("old", false, Phase::Done, None),
        row("waiting", true, Phase::Waiting, None),
        row("cancelled", false, Phase::Cancelled, None),
        row("moving", false, Phase::Moving, None),
        row("failed", true, Phase::Failed("offline".into()), None),
    ];
    let shown: Vec<_> = transfer_list(&list).map(|item| item.id.as_str()).collect();
    assert_eq!(shown, ["waiting", "moving", "failed", "cancelled", "old"]);
    assert_eq!(
        list[0].id, "old",
        "presentation must not reorder the shared model"
    );
    assert!(transfer_list(&[]).next().is_none());
}

#[test]
fn more_history_never_extends_the_transfer_viewport() {
    for count in 0..=3 {
        assert_eq!(transfer_list_height(count), None);
    }
    assert_eq!(transfer_list_height(4), Some(360.));
    assert_eq!(transfer_list_height(4), transfer_list_height(usize::MAX));
}

#[test]
fn a_history_line_says_which_way_the_file_went_and_how_it_ended() {
    let sent = Progress {
        name: "a.txt".into(),
        ..row("a", false, Phase::Done, None)
    };
    assert_eq!(history_line(&sent), "Sent \u{b7} a.txt \u{b7} done");
    let failed = Progress {
        name: "a.txt".into(),
        ..row("a", true, Phase::Failed("x".into()), None)
    };
    assert_eq!(history_line(&failed), "Received \u{b7} a.txt \u{b7} failed");
    let cancelled = Progress {
        name: "a.txt".into(),
        ..row("a", true, Phase::Cancelled, None)
    };
    assert_eq!(
        history_line(&cancelled),
        "Received \u{b7} a.txt \u{b7} cancelled"
    );
}

#[test]
fn a_window_drawn_again_does_not_repeat_what_the_owner_said_before_it_looked() {
    let said = |texts: &[&str]| texts.iter().map(|t| (*t).to_owned()).collect::<Vec<_>>();
    // Two old messages of the owner, then how an extension is now.
    let list = said(&["old one", "old two", "[ext] stopped"]);
    let seen = notices_already_said(&list, 2);
    assert_eq!(seen, said(&["old one", "old two"]));
    // First look: the old messages stay unsaid, the state is said.
    assert_eq!(
        fresh_notices(&seen, 7, &list, 2, 7),
        said(&["[ext] stopped"])
    );

    // Something new is said while the window is there.
    let next = said(&["old one", "old two", "new", "[ext] stopped"]);
    assert_eq!(fresh_notices(&list, 7, &next, 3, 8), said(&["new"]));
    // The same sentence said again changes only the count, and is shown again.
    assert_eq!(fresh_notices(&next, 8, &next, 3, 9), said(&["new"]));
    // Nothing said, nothing shown.
    assert!(fresh_notices(&next, 9, &next, 3, 9).is_empty());
    // A count far ahead of a short list never reaches outside it.
    assert_eq!(
        fresh_notices(&next, 0, &next, 3, u64::MAX),
        said(&["old one", "old two", "new"])
    );
    assert!(fresh_notices(&[], 0, &[], 5, 3).is_empty());
}

#[test]
fn a_question_keeps_its_card_and_an_extension_is_named_as_installed() {
    use peersus_api::notification::{Button, Notification};
    use peersus_core::model::{
        Grantable, NotificationSource, PluginInfo, PluginStatus, PostedNotification, Standing,
    };
    use peersus_core::plugins::PluginId;
    let extension = |id: &str, name: Option<&str>| PluginInfo {
        id: PluginId::new(id).unwrap(),
        manifest: Default::default(),
        standing: Standing::Ready,
        status: PluginStatus::Running,
        grantable: Grantable::default(),
        program: None,
        package: name.map(|name| {
            serde_json::from_value(serde_json::json!({"name": name, "version": "1.0.0"})).unwrap()
        }),
        agreed: false,
        from_store: false,
    };
    let plugins = [
        extension("named", Some("Shown Name")),
        extension("bare", None),
    ];
    let from = |id: &str| NotificationSource::Plugin(PluginId::new(id).unwrap());
    let button = |id: &str, label: &str| Button {
        id: id.into(),
        label: label.into(),
    };
    let says = |id, source, text: &str| PostedNotification {
        id,
        source,
        notification: Notification::saying(text),
    };
    let asks = |id, source, buttons: Vec<Button>| PostedNotification {
        id,
        source,
        notification: Notification {
            buttons,
            ..Notification::saying("Question")
        },
    };
    let posted = vec![
        asks(
            1,
            from("named"),
            vec![button("send", "Trimite"), button("keep", "Nu acum")],
        ),
        says(2, NotificationSource::App, "first"),
        says(3, from("bare"), "second"),
        says(4, from("unknown"), "third"),
        says(5, NotificationSource::App, "fourth"),
    ];
    let cards = notification_cards(&posted, &plugins);
    // The question, though the oldest, is there; of the sentences only the two newest have room.
    assert_eq!(
        cards.iter().map(|card| card.id).collect::<Vec<_>>(),
        [1, 4, 5]
    );
    assert_eq!(cards[0].from.as_deref(), Some("Shown Name"));
    assert_eq!(cards[0].buttons, posted[0].notification.buttons);
    assert!(cards[0].buttons_in_a_row());
    // One the window has no installed name for is named by its id, never by words of its own.
    assert_eq!(cards[1].from.as_deref(), Some("unknown"));
    assert_eq!(cards[2].from, None);
    assert_eq!(cards[2].text, "fourth");
    assert_eq!(
        notification_cards(&posted[2..3], &plugins)[0]
            .from
            .as_deref(),
        Some("bare")
    );

    // More questions than cards: the oldest are shown, and no sentence takes a place from them.
    let many: Vec<_> = (1..=5)
        .map(|id| asks(id, NotificationSource::App, vec![button("ok", "OK")]))
        .chain([says(6, NotificationSource::App, "late")])
        .collect();
    assert_eq!(
        notification_cards(&many, &plugins)
            .iter()
            .map(|card| card.id)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(notification_cards(&[], &plugins).is_empty());

    // Long answers each get a line.
    let long = asks(
        9,
        NotificationSource::App,
        vec![
            button("a", "Trimite acum fișierul"),
            button("b", "Păstrează-l aici"),
        ],
    );
    assert!(!notification_cards(&[long], &plugins)[0].buttons_in_a_row());

    // Outside the window only what its sender asked to reach there is drawn, however many others are on show.
    let mut snapshot = empty_snapshot();
    snapshot.plugins = plugins.to_vec();
    snapshot.notifications = posted;
    assert!(outside_cards(&snapshot).is_empty());
    snapshot.notifications[2].notification.reach = peersus_api::notification::Reach::Outside;
    let outside = outside_cards(&snapshot);
    assert_eq!(outside.iter().map(|card| card.id).collect::<Vec<_>>(), [3]);
    assert_eq!(outside[0].from.as_deref(), Some("bare"));
}

#[test]
fn a_file_that_was_not_delivered_stays_said_whatever_other_file_arrives_with_it_or_after() {
    let ok = ("b".to_owned(), true);
    let bad = ("a".to_owned(), false);
    let said = delivery_note(None, &[bad.clone(), ok.clone()]);
    assert!(
        said.is_some(),
        "a failure was lost when a success came in the same look"
    );
    assert_eq!(
        delivery_note(None, &[ok.clone(), bad]),
        said,
        "the order changed what was said"
    );
    assert_eq!(
        delivery_note(said.clone(), std::slice::from_ref(&ok)),
        said,
        "another file's success took the failure away"
    );
    assert_eq!(
        delivery_note(None, &[ok]),
        None,
        "a success alone said something went wrong"
    );
}

#[test]
fn a_received_file_that_arrived_whole_is_handed_to_the_folder_chosen_for_it_and_only_then() {
    let list = [
        arrived("a", "/stage/a.bin"),
        // Not done yet: its file is partial or not there, so it is not handed over.
        Progress {
            saved_as: Some("/stage/b.bin".into()),
            ..row("b", true, Phase::Moving, None)
        },
        // Went out, not in: nothing of ours to put anywhere.
        Progress {
            saved_as: Some("/stage/c.bin".into()),
            ..row("c", false, Phase::Done, None)
        },
        // Done but no file reported: nothing to hand over.
        row("d", true, Phase::Done, None),
        // Done, with a file, but accepted to a path the program can write: no tree, so nothing to do.
        arrived("e", "/stage/e.bin"),
    ];
    let trees = |id: &str| (id != "e").then(|| format!("tree-{id}"));
    let handed = handoffs(&list, trees);
    assert_eq!(
        handed,
        vec![peersus_net::folder_picked::Handoff {
            transfer: "a".into(),
            path: "/stage/a.bin".into(),
            name: "a.bin".into(),
            folders: Vec::new(),
            tree: "tree-a".into()
        }]
    );
}

#[test]
fn nested_delivery_keeps_offer_directories_instead_of_flattening_the_staged_file() {
    let mut record = transfer(6, 6, TransferState::Complete);
    record.incoming = true;
    record.folders = vec!["pictures".into(), "nested".into(), "deeper".into()];
    record.saved_as = Some("/private/staging/pictures/nested/deeper/file.bin".into());
    let list = [progress(&record)];
    let handed = handoffs(&list, |_| Some("chosen-tree".into()));
    assert_eq!(handed[0].folders, ["pictures", "nested", "deeper"]);
    assert_eq!(handed[0].name, "file.bin");
    assert_eq!(handed[0].tree, "chosen-tree");
}

#[test]
fn a_transfer_is_handed_over_once_because_its_folder_is_given_up_when_asked() {
    let picked = peersus_net::folder_picked::FolderPicked::default();
    picked.destined("a", "tree-a".into());
    let list = [arrived("a", "/stage/a.bin")];
    assert_eq!(handoffs(&list, |id| picked.tree_for(id)).len(), 1);
    assert!(
        handoffs(&list, |id| picked.tree_for(id)).is_empty(),
        "the same finished transfer was handed over again"
    );
}

#[test]
fn the_time_left_counts_down_to_the_deadline_and_stops_at_zero_and_says_nothing_without_one() {
    assert_eq!(
        time_left(None, 1_000),
        None,
        "an offer with no deadline was given one"
    );
    assert_eq!(time_left(Some(1_120), 1_000), Some(120));
    assert_eq!(time_left(Some(1_120), 1_119), Some(1));
    assert_eq!(
        time_left(Some(1_120), 1_120),
        Some(0),
        "the deadline itself still said there was time"
    );
    // Gone by: never a negative count, and never a wrapped one.
    assert_eq!(time_left(Some(1_120), 5_000), Some(0));
    assert_eq!(time_left(Some(0), u64::MAX), Some(0));
}

#[test]
fn the_clock_is_read_as_minutes_and_two_digit_seconds_and_an_hour_is_sixty_minutes() {
    for (seconds, words) in [
        (0, "0:00"),
        (9, "0:09"),
        (59, "0:59"),
        (60, "1:00"),
        (65, "1:05"),
        (120, "2:00"),
        (3_600, "60:00"),
        (86_400, "1440:00"),
    ] {
        assert_eq!(clock_words(seconds), words, "{seconds} s");
    }
}

#[test]
fn the_window_carries_the_owners_deadline_as_it_is_and_not_a_count() {
    let offered = Transfer {
        expires_at: Some(1_700_000_120),
        ..transfer(10, 0, TransferState::Offered)
    };
    assert_eq!(progress(&offered).expires_at, Some(1_700_000_120));
    assert_eq!(
        progress(&transfer(10, 0, TransferState::Offered)).expires_at,
        None
    );
}

#[test]
fn only_a_file_coming_in_that_waits_for_an_answer_is_shown_over_the_window() {
    // Going out and waiting: it is the OTHER device that decides, so there is nothing for this person to answer.
    assert!(incoming_offer(&[row("a", false, Phase::Waiting, None)]).is_none());
    // Coming in, but already answered, finished or stopped: nothing left to ask.
    for phase in [
        Phase::Moving,
        Phase::Done,
        Phase::Cancelled,
        Phase::Failed("x".into()),
    ] {
        assert!(incoming_offer(&[row("a", true, phase, None)]).is_none());
    }
    // Coming in and waiting: the one thing that is asked.
    assert_eq!(
        incoming_offer(&[row("a", true, Phase::Waiting, None)]).map(|offer| offer.id.as_str()),
        Some("a")
    );
    assert!(incoming_offer(&[]).is_none());
}

#[test]
fn the_sample_offer_is_one_the_window_would_show_for_a_real_one_and_has_the_default_wait() {
    let sample = sample_offer(1_000);
    // The rule that picks what to show over the window picks it, and it can be answered.
    let list = [sample.offer.clone()];
    assert_eq!(
        incoming_offer(&list).map(|offer| offer.id.as_str()),
        Some("sample")
    );
    assert!(sample.offer.acceptable());
    assert_eq!(sample.waiting, 1);
    assert_eq!(
        time_left(sample.offer.expires_at, 1_000),
        Some(u64::from(peersus_core::config::DEFAULT_OFFER_TIMEOUT))
    );
}

#[test]
fn an_action_with_no_target_is_done_on_the_second_press_and_never_on_the_first() {
    let (asked, now) = Confirm::default().pressed();
    assert_eq!(
        (asked, now),
        (Confirm::Asking, false),
        "the first press did it"
    );
    let (done, now) = asked.pressed();
    assert_eq!(
        (done, now),
        (Confirm::Idle, true),
        "the second press did not do it"
    );
    // After it is done, the next press asks again: one confirmation is not a standing permission.
    assert_eq!(done.pressed(), (Confirm::Asking, false));
    // Given up, then pressed: asks again and does nothing.
    assert_eq!(asked.cancelled().pressed(), (Confirm::Asking, false));
}

#[test]
fn with_several_waiting_the_oldest_comes_first_and_the_next_follows_when_it_is_answered() {
    let mut list = vec![
        row("done", true, Phase::Done, None),
        row("first", true, Phase::Waiting, None),
        row("second", true, Phase::Waiting, None),
    ];
    assert_eq!(
        incoming_offer(&list).map(|offer| offer.id.as_str()),
        Some("first"),
        "a finished one, or a later one, came first"
    );
    list[1].phase = Phase::Moving;
    assert_eq!(
        incoming_offer(&list).map(|offer| offer.id.as_str()),
        Some("second")
    );
    list[2].phase = Phase::Cancelled;
    assert!(incoming_offer(&list).is_none());
}

/// A finger on a circle 200 px across, as the toolkit reports it: where it is in the circle's box, and in the window.
/// The box sits at (50, 300) in the window, so the two differ by that.
fn at_box(x: f32, y: f32) -> ((f32, f32), (f32, f32)) {
    ((x, y), (x + 50., y + 300.))
}

#[test]
fn a_circle_is_pressed_to_its_actuation_point_like_a_key_and_a_brush_of_the_screen_is_not_a_press()
{
    let (b, w) = at_box(100., 100.);
    let mut touch = Touch::new(200.);
    touch.down(b, w, 1_000);
    assert!(
        !touch.up(b, w, 1_000),
        "a contact of no time pressed the circle"
    );
    touch.down(b, w, 1_000);
    assert!(
        !touch.up(b, w, 1_000 + MIN_TOUCH_MS - 1),
        "a brush pressed the circle"
    );
    touch.down(b, w, 1_000);
    assert!(
        touch.up(b, w, 1_000 + MIN_TOUCH_MS),
        "the actuation point itself is a press"
    );
    // Resting on it long is still a press for a control that has no hold of its own: the circle of Receive.
    touch.down(b, w, 1_000);
    assert!(
        touch.up(b, w, 1_000 + HOLD_MS * 4),
        "a long rest was not a press"
    );
}

#[test]
fn a_clock_that_runs_backwards_does_not_press_a_circle() {
    let (b, w) = at_box(100., 100.);
    let mut touch = Touch::new(200.);
    touch.down(b, w, 10_000);
    assert!(
        !touch.up(b, w, 5),
        "a clock that ran backwards pressed the circle"
    );
}

#[test]
fn the_circle_of_send_asks_the_geometry_alone_for_its_own_clock_judges_the_time() {
    let (b, w) = at_box(100., 100.);
    let mut touch = Touch::new(200.);
    touch.down(b, w, 0);
    assert!(
        touch.lifted_inside(b, w),
        "a lift inside the round was refused"
    );
    let (corner, corner_win) = at_box(2., 2.);
    touch.down(b, w, 0);
    assert!(
        !touch.lifted_inside(corner, corner_win),
        "a lift from a corner counted"
    );
}

#[test]
fn a_touch_in_the_middle_that_hardly_moves_is_a_press() {
    let mut touch = Touch::new(200.);
    let (down_box, down_win) = at_box(100., 100.);
    touch.down(down_box, down_win, 0);
    let (up_box, up_win) = at_box(104., 103.);
    assert!(
        touch.up(up_box, up_win, MIN_TOUCH_MS),
        "a finger that moved a little was not a tap"
    );
    // A move of exactly the slop is still a tap.
    let mut touch = Touch::new(200.);
    touch.down(down_box, down_win, 0);
    let (up_box, up_win) = at_box(100. + TAP_SLOP, 100.);
    assert!(
        touch.up(up_box, up_win, MIN_TOUCH_MS),
        "a move of exactly the slop was refused"
    );
}

#[test]
fn a_touch_that_begins_in_a_corner_of_the_box_is_outside_the_circle_and_presses_nothing() {
    for corner in [(2., 2.), (198., 2.), (2., 198.), (198., 198.)] {
        let mut touch = Touch::new(200.);
        let (b, w) = at_box(corner.0, corner.1);
        touch.down(b, w, 0);
        assert!(
            !touch.up(b, w, MIN_TOUCH_MS),
            "{corner:?} pressed the circle from its corner"
        );
    }
    // Down in the circle and lifted in a corner is not a press either.
    let mut touch = Touch::new(200.);
    let (db, dw) = at_box(100., 100.);
    touch.down(db, dw, 0);
    let (ub, uw) = at_box(2., 2.);
    assert!(!touch.up(ub, uw, MIN_TOUCH_MS));
}

#[test]
fn a_drag_presses_nothing_even_when_it_begins_and_ends_on_the_circle() {
    let mut touch = Touch::new(200.);
    let (db, dw) = at_box(100., 60.);
    touch.down(db, dw, 0);
    let (ub, uw) = at_box(100., 140.);
    assert!(
        !touch.up(ub, uw, MIN_TOUCH_MS),
        "a swipe was taken for a tap"
    );
    // Just past the slop is already a drag.
    let mut touch = Touch::new(200.);
    let (db, dw) = at_box(100., 100.);
    touch.down(db, dw, 0);
    let (ub, uw) = at_box(100. + TAP_SLOP + 1., 100.);
    assert!(
        !touch.up(ub, uw, MIN_TOUCH_MS),
        "a move just past the slop was a tap"
    );
}

#[test]
fn a_swipe_that_goes_out_and_comes_back_is_still_a_drag_and_presses_nothing() {
    let mut touch = Touch::new(200.);
    let (db, dw) = at_box(100., 100.);
    touch.down(db, dw, 0);
    // Down 70 px (a swipe), then back to exactly where it began, and lifted there: both ends are the same point.
    touch.moved(at_box(100., 170.).1);
    let (ub, uw) = at_box(100., 100.);
    assert!(
        !touch.up(ub, uw, MIN_TOUCH_MS),
        "an out-and-back swipe ended where it began and was taken for a tap"
    );
}

#[test]
fn a_page_scrolling_under_a_still_finger_is_not_the_finger_moving_but_the_finger_moving_is() {
    // The page scrolls 80 px: the circle's box moves up under a finger that stays still. In the window the finger has not
    // moved; inside the box it appears to have moved 80 px. That is not a drag by the finger, and it must not decide.
    let mut touch = Touch::new(200.);
    touch.down((100., 100.), (150., 400.), 0);
    touch.moved((150., 400.));
    // The finger is where it was, in the window. In the box it reads 80 px further up, which is still inside the circle.
    assert!(
        touch.up((100., 20.), (150., 400.), MIN_TOUCH_MS),
        "scrolling under a still finger cancelled a tap"
    );
    // The same distance moved by the FINGER, in the window, is a drag.
    let mut touch = Touch::new(200.);
    touch.down((100., 100.), (150., 400.), 0);
    touch.moved((150., 480.));
    assert!(
        !touch.up((100., 100.), (150., 480.), MIN_TOUCH_MS),
        "the finger moved 80 px and it was a tap"
    );
}

#[test]
fn the_edge_of_the_circle_is_where_the_press_ends() {
    // On the rim (distance from the middle equal to the radius) is in; a hair past it is out.
    let mut touch = Touch::new(200.);
    let (b, w) = at_box(200., 100.);
    touch.down(b, w, 0);
    assert!(touch.up(b, w, MIN_TOUCH_MS), "the rim itself was refused");
    let mut touch = Touch::new(200.);
    let (b, w) = at_box(200.5, 100.);
    touch.down(b, w, 0);
    assert!(
        !touch.up(b, w, MIN_TOUCH_MS),
        "a point past the rim pressed"
    );
}

#[test]
fn both_ends_of_the_touch_must_be_in_the_circle_so_one_end_outside_with_a_tiny_move_is_not_a_press()
{
    // Down on the rim (inside), up 3 px outward (outside): the move is inside the slop, so only the place where the
    // finger came up can refuse it.
    let mut touch = Touch::new(200.);
    let (db, dw) = at_box(200., 100.);
    touch.down(db, dw, 0);
    let (ub, uw) = at_box(203., 100.);
    assert!(
        !touch.up(ub, uw, MIN_TOUCH_MS),
        "a touch that lifted outside the circle pressed"
    );
    // The other way: down just outside, up on the rim. Only the place where it began can refuse it.
    let mut touch = Touch::new(200.);
    let (db, dw) = at_box(203., 100.);
    touch.down(db, dw, 0);
    let (ub, uw) = at_box(200., 100.);
    assert!(
        !touch.up(ub, uw, MIN_TOUCH_MS),
        "a touch that began outside the circle pressed"
    );
}

#[test]
fn a_cancelled_gesture_presses_nothing_and_a_lift_with_no_touch_before_it_presses_nothing() {
    let mut touch = Touch::new(200.);
    let (b, w) = at_box(100., 100.);
    touch.down(b, w, 0);
    touch.cancel();
    assert!(!touch.up(b, w, MIN_TOUCH_MS), "a cancelled gesture pressed");
    // A lift that nothing began: the finger came onto the circle from outside and was lifted there.
    let mut touch = Touch::new(200.);
    assert!(
        !touch.up(b, w, MIN_TOUCH_MS),
        "a lift with no touch before it pressed"
    );
    // And one gesture does not carry into the next: after a press the next lift alone is not one.
    let mut touch = Touch::new(200.);
    touch.down(b, w, 0);
    assert!(touch.up(b, w, MIN_TOUCH_MS));
    assert!(
        !touch.up(b, w, MIN_TOUCH_MS),
        "a second lift pressed from the first touch"
    );
}

#[test]
fn the_words_name_who_sends_and_never_pretend_when_it_gave_no_name() {
    assert_eq!(
        offer_words(&row("a", true, Phase::Waiting, Some("Alex's phone"))),
        "Alex's phone wants to send you a file"
    );
    // Nothing, or only blanks: it is not made up, and no empty name leaves a sentence that begins with a space.
    for none in [None, Some(""), Some("   ")] {
        assert_eq!(
            offer_words(&row("a", true, Phase::Waiting, none)),
            "A device wants to send you a file"
        );
    }
}

#[test]
fn the_queue_is_said_on_a_line_of_its_own_and_only_when_there_is_more_than_one() {
    assert_eq!(queue_words(0), None);
    assert_eq!(queue_words(1), None, "a queue of one was announced");
    assert_eq!(
        queue_words(2).as_deref(),
        Some("2 files are waiting in all")
    );
    assert_eq!(
        queue_words(16).as_deref(),
        Some("16 files are waiting in all")
    );
}

#[test]
fn a_count_is_never_put_to_the_name_of_one_sender_when_the_files_come_from_several() {
    // Alice offered one file and Bob two. The prompt is about the FIRST file, which is what Accept answers, so its
    // sentence names Alice and says "a file"; the total is a separate line, and it is three because that is what waits.
    let mut view = View {
        transfers: vec![
            row("done", true, Phase::Done, Some("Carol")),
            row("out", false, Phase::Waiting, Some("Dan")),
            row("alice-1", true, Phase::Waiting, Some("Alice")),
            row("bob-1", true, Phase::Waiting, Some("Bob")),
            row("bob-2", true, Phase::Waiting, Some("Bob")),
        ],
        ..View::default()
    };
    let offered = offered_now(&view).expect("three were waiting");
    assert_eq!(offered.offer.id, "alice-1");
    assert_eq!(
        offered.waiting, 3,
        "a finished or an outgoing file was counted as waiting"
    );
    assert_eq!(
        offer_words(&offered.offer),
        "Alice wants to send you a file",
        "the count was put to one sender's name"
    );
    assert_eq!(
        queue_words(offered.waiting).as_deref(),
        Some("3 files are waiting in all")
    );
    // Accept answers the first one; when it is answered the next is about Bob, with two waiting.
    view.transfers[2].phase = Phase::Moving;
    let next = offered_now(&view).expect("two were waiting");
    assert_eq!((next.offer.id.as_str(), next.waiting), ("bob-1", 2));
    assert_eq!(offer_words(&next.offer), "Bob wants to send you a file");
}

#[test]
fn the_code_is_drawn_only_while_it_is_the_one_on_show_and_can_still_admit_a_device() {
    assert_eq!(code_circle(3, on_show(3, true), &[]), CodeCircle::Code);
    // Scanned: the owner says it admits no more, and no file is reported for it yet. NOT drawn.
    assert_eq!(
        code_circle(3, on_show(3, false), &[]),
        CodeCircle::Scanned,
        "a scanned code stayed on the screen"
    );
    // Another code is on show, or none (revoked with the network): what was drawn is gone.
    assert_eq!(
        code_circle(3, on_show(4, true), &[]),
        CodeCircle::Gone,
        "a replaced code stayed on the screen"
    );
    assert_eq!(
        code_circle(3, None, &[]),
        CodeCircle::Gone,
        "a revoked code stayed on the screen"
    );
}

#[test]
fn a_file_that_came_of_the_code_says_where_it_stands_whatever_the_owner_says_of_the_code() {
    let waiting = [
        of_code("a", 3, Phase::Waiting, 0),
        of_code("b", 3, Phase::Waiting, 0),
    ];
    assert_eq!(
        code_circle(3, on_show(3, false), &waiting),
        CodeCircle::WaitingForApproval { files: 2 }
    );
    // One accepted and is crossing: sending, the mean of the two.
    let sending = [
        of_code("a", 3, Phase::Moving, 80),
        of_code("b", 3, Phase::Waiting, 0),
    ];
    assert_eq!(
        code_circle(3, on_show(3, false), &sending),
        CodeCircle::Sending {
            files: 2,
            percent: 40
        }
    );
    // Another code may have been made since: the files of the first still say where THEY stand.
    assert_eq!(
        code_circle(3, on_show(4, true), &sending),
        CodeCircle::Sending {
            files: 2,
            percent: 40
        }
    );
    let sent = [
        of_code("a", 3, Phase::Done, 100),
        of_code("b", 3, Phase::Done, 100),
    ];
    assert_eq!(code_circle(3, None, &sent), CodeCircle::Sent { files: 2 });
    let stopped = [
        of_code("a", 3, Phase::Done, 100),
        of_code("b", 3, Phase::Cancelled, 10),
    ];
    assert_eq!(
        code_circle(3, None, &stopped),
        CodeCircle::Stopped { files: 2, done: 1 }
    );
    let failed = [of_code("a", 3, Phase::Failed("x".into()), 0)];
    assert_eq!(
        code_circle(3, None, &failed),
        CodeCircle::Stopped { files: 1, done: 0 }
    );
    // Some still crossing while another failed: still going, not stopped.
    let mixed = [
        of_code("a", 3, Phase::Moving, 50),
        of_code("b", 3, Phase::Failed("x".into()), 0),
    ];
    assert_eq!(
        code_circle(3, None, &mixed),
        CodeCircle::Sending {
            files: 2,
            percent: 25
        }
    );
}

#[test]
fn what_did_not_come_of_the_code_never_moves_its_circle() {
    // Files sent to a device that is paired (no code), files of another code, and files coming IN, in any number and in
    // any order, are not this code's.
    let others = [
        row("paired", false, Phase::Moving, None),
        of_code("older", 2, Phase::Done, 100),
        of_code("newer", 4, Phase::Waiting, 0),
        Progress {
            code: Some(3),
            ..row("in", true, Phase::Done, Some("Alex"))
        },
    ];
    assert_eq!(
        code_circle(3, on_show(3, true), &others),
        CodeCircle::Code,
        "a transfer of no code was taken for this one's"
    );
    assert_eq!(
        code_circle(3, on_show(3, false), &others),
        CodeCircle::Scanned
    );
}

#[test]
fn a_code_is_taken_off_the_screen_at_its_deadline_without_waiting_for_the_owner() {
    assert!(!code_expired(1_000, 999));
    assert!(
        code_expired(1_000, 1_000),
        "a code was still drawn at its deadline"
    );
    assert!(code_expired(1_000, 5_000));
}

#[test]
fn the_circle_says_what_is_happening_to_the_files_in_words_and_says_nothing_while_it_is_the_code() {
    // The code is the picture, and a code that is gone asks for files again: neither has words of its own.
    assert_eq!(circle_words_in(Language::ENGLISH, &CodeCircle::Code), None);
    assert_eq!(circle_words_in(Language::ENGLISH, &CodeCircle::Gone), None);
    // Scanned: said at once, before any file is reported, and it says it is the other device that is to answer.
    let (_, headline, line) = circle_words_in(Language::ENGLISH, &CodeCircle::Scanned).unwrap();
    assert_eq!(headline, "SCANNED");
    assert!(line.contains("Waiting for it to accept"), "{line}");
    let (kicker, headline, line) = circle_words_in(
        Language::ENGLISH,
        &CodeCircle::WaitingForApproval { files: 1 },
    )
    .unwrap();
    assert_eq!((kicker, headline.as_str()), ("WAITING FOR APPROVAL", "1"));
    assert_eq!(line, "Waiting for the other device to accept 1 file.");
    assert_eq!(
        circle_words_in(
            Language::ENGLISH,
            &CodeCircle::WaitingForApproval { files: 3 }
        )
        .unwrap()
        .2,
        "Waiting for the other device to accept 3 files."
    );
    // Sending: the percent is the headline, so that how far it is can be read at a glance.
    let (kicker, headline, line) = circle_words_in(
        Language::ENGLISH,
        &CodeCircle::Sending {
            files: 2,
            percent: 40,
        },
    )
    .unwrap();
    assert_eq!(
        (kicker, headline.as_str(), line.as_str()),
        ("SENDING", "40%", "Sending 2 files.")
    );
    let (_, headline, line) =
        circle_words_in(Language::ENGLISH, &CodeCircle::Sent { files: 2 }).unwrap();
    assert_eq!(
        (headline.as_str(), line.as_str()),
        ("2 SENT", "2 files arrived.")
    );
    // Stopped says how many got there, not just that something went wrong.
    let (_, headline, line) = circle_words_in(
        Language::ENGLISH,
        &CodeCircle::Stopped { files: 3, done: 1 },
    )
    .unwrap();
    assert_eq!(headline, "1/3");
    assert!(line.starts_with("1 of 3 files arrived."), "{line}");
}

#[test]
fn the_circle_counts_down_to_the_earliest_deadline_of_the_files_of_its_code_that_still_wait_and_only_then()
 {
    let waiting = CodeCircle::WaitingForApproval { files: 2 };
    let with_deadline = |id: &str, code: u64, phase: Phase, at: Option<u64>| Progress {
        expires_at: at,
        ..of_code(id, code, phase, 0)
    };
    let list = [
        with_deadline("late", 3, Phase::Waiting, Some(1_200)),
        with_deadline("soon", 3, Phase::Waiting, Some(1_090)),
        // Another code's file, a file already moving and one with no deadline say nothing about this circle's time.
        with_deadline("other", 4, Phase::Waiting, Some(1_005)),
        with_deadline("moving", 3, Phase::Moving, Some(1_001)),
        with_deadline("none", 3, Phase::Waiting, None),
    ];
    assert_eq!(
        circle_time_in(Language::ENGLISH, &waiting, 3, &list, 1_000).as_deref(),
        Some("ENDS IN 1:30"),
        "not the earliest deadline of this code"
    );
    assert_eq!(
        circle_time_in(Language::ENGLISH, &waiting, 3, &list, 1_085).as_deref(),
        Some("ENDS IN 0:05")
    );
    // At the deadline the clock stops at zero (the same contract as the offer's): the owner ends the offer then, and the circle
    // leaves this state by itself. A deadline long past is not a negative number.
    assert_eq!(
        circle_time_in(Language::ENGLISH, &waiting, 3, &list, 1_090).as_deref(),
        Some("ENDS IN 0:00"),
        "the clock did not stop at zero"
    );
    assert_eq!(
        circle_time_in(Language::ENGLISH, &waiting, 3, &list, 9_999).as_deref(),
        Some("ENDS IN 0:00"),
        "a deadline long past went below zero"
    );
    // Only while waiting: every other state has its own words and no clock.
    for circle in [
        CodeCircle::Code,
        CodeCircle::Scanned,
        CodeCircle::Sending {
            files: 1,
            percent: 5,
        },
        CodeCircle::Sent { files: 1 },
        CodeCircle::Gone,
    ] {
        assert_eq!(
            circle_time_in(Language::ENGLISH, &circle, 3, &list, 1_000),
            None,
            "{circle:?} showed a countdown"
        );
    }
    assert_eq!(
        circle_time_in(Language::ENGLISH, &waiting, 3, &[], 1_000),
        None,
        "a circle with no waiting file showed a time"
    );
}

#[test]
fn the_circle_names_the_chosen_files_up_to_a_bound_and_counts_the_rest_and_says_nothing_for_none() {
    let names = |count: usize| (0..count).map(|n| format!("f{n}.txt")).collect::<Vec<_>>();
    assert!(
        chosen_words(&[]).is_empty(),
        "something was said when nothing was chosen"
    );
    assert_eq!(chosen_words(&names(1)), ["f0.txt"]);
    // Exactly the bound: all named, nothing counted.
    assert_eq!(
        chosen_words(&names(NAMED_UNDER_CIRCLE)).len(),
        NAMED_UNDER_CIRCLE
    );
    assert!(
        !chosen_words(&names(NAMED_UNDER_CIRCLE))
            .last()
            .unwrap()
            .starts_with('+'),
        "a file that fits was counted instead of named"
    );
    // One past the bound: the first ones are named and the rest are counted, so that nothing chosen is hidden without a word.
    let many = chosen_words(&names(NAMED_UNDER_CIRCLE + 3));
    assert_eq!(many.len(), NAMED_UNDER_CIRCLE + 1);
    assert_eq!(many[0], "f0.txt");
    assert_eq!(many.last().unwrap(), "+ 3 more");
}

#[test]
fn what_is_left_of_a_codes_life_runs_to_its_deadline_and_never_goes_below_nothing() {
    assert_eq!(
        until_expiry(1_300, 1_000),
        std::time::Duration::from_secs(300)
    );
    assert_eq!(until_expiry(1_000, 1_000), std::time::Duration::ZERO);
    // A clock that is ahead of the deadline is not a debt.
    assert_eq!(until_expiry(1_000, 9_000), std::time::Duration::ZERO);
    // The same rule as `code_expired`: no time left exactly when it is expired.
    for now in [0, 999, 1_000, 1_001] {
        assert_eq!(
            until_expiry(1_000, now).is_zero(),
            code_expired(1_000, now),
            "at {now}"
        );
    }
}

#[test]
fn scrolling_only_decides_and_the_bar_turns_once_the_page_has_gone_far_enough_one_way() {
    let start = HeaderHide::default();
    assert!(!start.hidden(), "a page not yet scrolled hid the bar");
    // Less than the turn decides nothing; reaching it, however small each step, decides the bar goes.
    let little = start.after(HeaderHide::TURN - 1.);
    assert!(!little.hidden());
    let enough = little.after(HeaderHide::TURN);
    assert!(
        enough.hidden(),
        "the page went the turn down and the bar stayed"
    );
    // Going further changes nothing: the bar is gone, not more gone.
    assert!(enough.after(400.).hidden());
    // Up, anywhere on the page: a little decides nothing, the turn brings it back.
    let far = enough.after(400.);
    assert!(far.after(400. - (HeaderHide::TURN - 1.)).hidden());
    assert!(
        !far.after(400. - HeaderHide::TURN).hidden(),
        "the page went the turn up and the bar stayed away"
    );
}

#[test]
fn the_bar_is_wanted_at_the_very_top_whatever_came_before() {
    let hidden = HeaderHide::default().after(500.);
    assert!(hidden.hidden());
    // Back at the top in one jump (a tap on the status bar, a page that was left and entered again): nothing above to read.
    assert!(!hidden.after(0.).hidden());
    // And a position that went past the top (a pull that stretches the page) is the top, not a negative one.
    assert!(!hidden.after(-40.).hidden());
}

#[test]
fn a_tremble_decides_nothing_and_a_position_that_is_not_a_number_changes_nothing() {
    // A finger that trembles: back and forth by less than the turn, many times, from either state, around where the page is.
    for start in [
        HeaderHide::default().after(100.),
        HeaderHide::default().after(200.).after(100.),
    ] {
        let mut at = start;
        for step in 0..40 {
            at = at.after(
                100. + if step % 2 == 0 {
                    HeaderHide::TURN - 1.
                } else {
                    0.
                },
            );
            assert_eq!(
                at.hidden(),
                start.hidden(),
                "a tremble turned the bar at step {step}"
            );
        }
    }
    // A turn of direction starts the count again: down 11, up 1, down 11 is not 21 down.
    let a = HeaderHide::default().after(100.);
    let b = a.after(80.).after(91.).after(90.).after(101.);
    assert_eq!(b.hidden(), a.after(80.).hidden());
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(a.after(bad), a, "{bad} changed the bar");
    }
}

#[test]
fn the_name_is_written_and_rubbed_out_in_whole_letters() {
    // All of it, none of it, and whole letters between: never more than the name, and never a letter less at the whole.
    assert_eq!(name_shown(1.), NAME);
    assert_eq!(name_shown(0.), "");
    assert_eq!(name_shown(0.5), "eer");
    let mut last = 0;
    for step in 0..=100 {
        let drawn = name_shown(step as f32 / 100.);
        assert!(
            NAME.starts_with(drawn),
            "{drawn} is not the start of the name"
        );
        assert!(
            drawn.len() >= last,
            "the name lost a letter as more of it was shown"
        );
        last = drawn.len();
    }
    // Past either end is that end, and what is not a number is the whole name.
    assert_eq!(name_shown(-3.), "");
    assert_eq!(name_shown(7.), NAME);
    for unknown in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(name_shown(unknown), NAME);
    }
}

#[test]
fn the_mark_nods_before_the_name_is_written_and_an_idle_window_waits_unevenly() {
    // At rest at both ends, deepest in the middle of its share, and at rest for all of the letters' share.
    assert_eq!(mark_nod(0.), 0.);
    assert!((mark_nod(MARK_NODS / 2.) - 1.).abs() < 1e-5);
    for step in 0..=100 {
        let progress = step as f32 / 100.;
        let nod = mark_nod(progress);
        assert!((0. ..=1.).contains(&nod), "{nod} at {progress}");
        if progress >= MARK_NODS {
            assert!(nod < 1e-5, "the mark still nods at {progress}");
        } else {
            // No letter while the mark nods: one thing moves at a time.
            assert_eq!(name_shown(name_part(progress)), "");
        }
    }
    assert_eq!(name_shown(name_part(1.)), NAME);
    assert_eq!(name_shown(name_part(-1.)), "");
    for unknown in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(mark_nod(unknown), 0.);
        assert_eq!(name_shown(name_part(unknown)), NAME);
    }
    // The waits of an idle window: always within their bounds, the same for the same start, and not a fixed beat.
    let waits: Vec<u64> = (0..200).map(|round| idle_name_after(7, round)).collect();
    assert!(waits.iter().all(|wait| (25_000..=90_000).contains(wait)));
    assert_eq!(
        waits,
        (0..200)
            .map(|round| idle_name_after(7, round))
            .collect::<Vec<_>>()
    );
    let mut distinct = waits.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(
        distinct.len() > 190,
        "only {} different waits in 200",
        distinct.len()
    );
    // Spread over the range and not crowded at one end: some short, some long.
    assert!(waits.iter().any(|wait| *wait < 40_000) && waits.iter().any(|wait| *wait > 75_000));
    assert_ne!(idle_name_after(7, 0), idle_name_after(8, 0));
    // The far ends of what can be asked do not overflow.
    let _ = (idle_name_after(u64::MAX, u32::MAX), idle_name_after(0, 0));
}

#[test]
fn completed_first_batches_never_finish_a_selection_that_is_still_being_opened() {
    let early = [of_code("first", 3, Phase::Done, 100)];
    let producing = Some(FilesCode {
        generation: 3,
        live: false,
        pending: true,
    });
    assert_eq!(
        code_circle_selection(3, producing, &early, 80),
        CodeCircle::Sending {
            files: 80,
            percent: 1
        }
    );
    assert!(matches!(
        code_circle_selection(3, producing, &early, 80),
        CodeCircle::Sending { .. }
    ));
    // Expiry, closure or producer failure must not turn the partial result into success.
    assert_eq!(
        code_circle_selection(3, on_show(3, false), &early, 80),
        CodeCircle::Stopped { files: 80, done: 1 }
    );
    // Once unreadable choices were accounted for, the successful remainder can finish.
    assert_eq!(
        code_circle_selection(3, on_show(3, false), &early, 1),
        CodeCircle::Sent { files: 1 }
    );
}

#[test]
fn pending_source_work_cannot_show_one_hundred_percent_even_if_all_visible_rows_are_done() {
    let complete = [of_code("first", 3, Phase::Done, 100)];
    let producing = Some(FilesCode {
        generation: 3,
        live: false,
        pending: true,
    });
    assert_eq!(
        code_circle_selection(3, producing, &complete, 1),
        CodeCircle::Sending {
            files: 1,
            percent: 99
        }
    );
    assert_eq!(
        code_circle_selection(3, on_show(3, false), &complete, 1),
        CodeCircle::Sent { files: 1 }
    );
}

fn listing(
    name: &str,
    version: &str,
    installed: Option<&str>,
) -> peersus_core::model::StoreListing {
    peersus_core::model::StoreListing {
        plugin: peersus_core::plugins::PluginId::new(name).unwrap(),
        version: version.into(),
        author: "Someone".into(),
        link: "https://example.org/x".into(),
        description: "Copies text between devices".into(),
        bytes: 0,
        installed_version: installed.map(str::to_owned),
    }
}

#[test]
fn opening_the_application_names_damaged_failed_and_outdated_extensions() {
    use peersus_core::model::{PluginStatus, Standing, StoreState, StoreStatus};
    let plugin = |name: &str, standing, status| PluginInfo {
        id: peersus_core::plugins::PluginId::new(name).unwrap(),
        manifest: Default::default(),
        standing,
        status,
        grantable: Default::default(),
        program: None,
        package: None,
        agreed: false,
        from_store: false,
    };
    let plugins = [
        plugin("healthy", Standing::Ready, PluginStatus::Running),
        plugin("damaged", Standing::Broken, PluginStatus::Stopped),
        plugin("crashed", Standing::Ready, PluginStatus::Failed),
    ];
    let store = StoreState {
        status: StoreStatus::Ready,
        listings: vec![
            listing("old", "2.0.0", Some("1.0.0")),
            listing("current", "1.0.0", Some("1.0.0")),
        ],
        error: None,
        asked: None,
    };
    let said = extension_health_notices(&plugins, &store, peersus_core::config::Language::ENGLISH);
    let named: Vec<&str> = said
        .iter()
        .map(|line| &line[1..line.find(']').unwrap()])
        .collect();
    assert_eq!(named, ["damaged", "crashed", "old"]);
    assert!(said[2].ends_with("2.0.0"));
}

#[test]
fn damaged_extensions_offer_reinstallation_even_at_the_catalog_version() {
    use peersus_core::model::{PluginStatus, Standing};
    let listing = listing("repairme", "1.0.0", Some("1.0.0"));
    let mut plugin = PluginInfo {
        id: listing.plugin.clone(),
        manifest: Default::default(),
        standing: Standing::Ready,
        status: PluginStatus::Stopped,
        grantable: Default::default(),
        program: None,
        package: None,
        agreed: false,
        from_store: false,
    };
    assert_eq!(
        StoreRow::of_installed(&listing, &plugin).action,
        StoreAction::Installed
    );
    for standing in [Standing::Broken, Standing::Changed] {
        plugin.standing = standing;
        assert_eq!(
            StoreRow::of_installed(&listing, &plugin).action,
            StoreAction::Update
        );
    }
}

#[test]
fn unified_extensions_keep_catalog_positions_across_install_and_removal() {
    use peersus_core::model::{PluginStatus, Standing, StoreState, StoreStatus};
    let plugin = |name: &str| PluginInfo {
        id: peersus_core::plugins::PluginId::new(name).unwrap(),
        manifest: Default::default(),
        standing: Standing::Ready,
        status: PluginStatus::Stopped,
        grantable: Default::default(),
        program: None,
        package: None,
        agreed: false,
        from_store: false,
    };
    let store = StoreState {
        status: StoreStatus::Ready,
        listings: vec![
            listing("echo", "1.0.0", None),
            listing("other", "1.0.0", None),
        ],
        error: None,
        asked: None,
    };
    let plugins = [plugin("local"), plugin("echo")];
    let rows = extension_rows(&plugins, &store, "", &[]);
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["echo", "other", "local"]
    );
    assert!(rows[0].installed.is_some());
    assert!(rows[0].listing.is_some());
    assert!(rows[1].installed.is_none());
    assert!(rows[2].listing.is_none());
    let after = extension_rows(&plugins[..1], &store, "", &[]);
    assert_eq!(
        after.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["echo", "other", "local"]
    );
    assert!(after[0].installed.is_none());
    assert!(after[0].listing.is_some());
    for status in [StoreStatus::Loading, StoreStatus::Failed] {
        let cached = StoreState {
            status,
            ..store.clone()
        };
        assert_eq!(
            extension_rows(&plugins, &cached, "", &[])
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["echo", "other", "local"]
        );
    }
    assert_eq!(
        extension_rows(&plugins, &store, " ECHO ", &[])
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["echo"]
    );
    assert_eq!(
        extension_rows(&plugins, &store, "local", &[])
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["local"]
    );
    assert_eq!(
        extension_rows(&plugins, &store, "someone", &[])
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["echo", "other"]
    );
    assert!(extension_rows(&plugins, &store, "missing", &[]).is_empty());
}

#[test]
fn a_package_waiting_for_confirmation_takes_its_rows_place_and_is_never_a_second_card() {
    use peersus_core::model::{PluginStatus, Standing, StoreState, StoreStatus};
    let plugin = |name: &str, from_store: bool| PluginInfo {
        id: peersus_core::plugins::PluginId::new(name).unwrap(),
        manifest: Default::default(),
        standing: Standing::Ready,
        status: PluginStatus::Stopped,
        grantable: Default::default(),
        program: None,
        package: None,
        agreed: false,
        from_store,
    };
    let store = StoreState {
        status: StoreStatus::Ready,
        listings: vec![listing("test", "0.1.2", None)],
        error: None,
        asked: None,
    };
    // The store's Test was asked for and read (the case seen on a phone as a copy of it), and a new package came from a file.
    let staged = [plugin("test", true), plugin("fresh", false)];
    let installed = [plugin("echo", false)];
    let rows = extension_rows(&installed, &store, "", &[]);
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["test", "echo"],
        "one row for Test, whatever waits"
    );
    assert_eq!(
        staged_apart(&staged, &rows)
            .iter()
            .map(|package| package.id.as_str())
            .collect::<Vec<_>>(),
        ["fresh"],
        "only a name with no row gets a card of its own"
    );
    // A search that leaves Test's row out still shows what waits for the person.
    let searched = extension_rows(&installed, &store, "echo", &[]);
    assert_eq!(staged_apart(&staged, &searched).len(), 2);
}

#[test]
fn extensions_installed_this_session_come_first_newest_first_and_nothing_else_moves() {
    use peersus_core::model::{PluginStatus, Standing, StoreState, StoreStatus};
    let id = |name: &str| peersus_core::plugins::PluginId::new(name).unwrap();
    let plugin = |name: &str| PluginInfo {
        id: id(name),
        manifest: Default::default(),
        standing: Standing::Ready,
        status: PluginStatus::Stopped,
        grantable: Default::default(),
        program: None,
        package: None,
        agreed: false,
        from_store: false,
    };
    let store = StoreState {
        status: StoreStatus::Ready,
        listings: vec![
            listing("echo", "1.0.0", None),
            listing("other", "1.0.0", None),
        ],
        error: None,
        asked: None,
    };
    let plugins = [plugin("local"), plugin("echo"), plugin("upload")];
    let order = |recent: &[peersus_core::plugins::PluginId]| {
        extension_rows(&plugins, &store, "", recent)
            .iter()
            .map(|row| row.id.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&[]), ["echo", "other", "local", "upload"]);
    assert_eq!(order(&[id("upload")]), ["upload", "echo", "other", "local"]);
    assert_eq!(
        order(&[id("local"), id("upload")]),
        ["local", "upload", "echo", "other"]
    );
    // A catalog entry that is not installed is never pulled forward, even if it was named.
    assert_eq!(order(&[id("other")]), ["echo", "other", "local", "upload"]);
}

#[test]
fn a_store_row_offers_install_update_or_nothing_from_the_two_versions() {
    use StoreAction::{Install, Installed, NewerInstalled, Update};
    let action = |offered: &str, installed: Option<&str>| {
        StoreRow::of(&listing("echo", offered, installed)).action
    };
    assert_eq!(action("1.2.0", None), Install);
    assert_eq!(action("1.2.0", Some("1.1.9")), Update);
    assert_eq!(
        action("1.2.0", Some("1.2.0")),
        Installed,
        "the same version is not an update"
    );
    assert_eq!(
        action("1.2.0", Some("1.3.0")),
        NewerInstalled,
        "a newer one is never downgraded by a click"
    );
    // By number and not by text: 1.10.0 is later than 1.9.0 even though "1.10.0" sorts first as text.
    assert_eq!(action("1.10.0", Some("1.9.0")), Update);
    assert_eq!(action("1.9.0", Some("1.10.0")), NewerInstalled);
    // A version that cannot be compared is never turned into an offered replacement.
    assert_eq!(action("1.2.0", Some("not-a-version")), Installed);
    assert_eq!(
        action("1.2.0", Some("01.2.0")),
        Installed,
        "a leading zero is not a version"
    );
}

#[test]
fn only_install_and_update_have_a_button_and_an_update_says_what_it_costs() {
    use StoreAction::{Install, Installed, NewerInstalled, Update};
    assert_eq!(
        (Install.button(), Update.button()),
        (Some("Install"), Some("Update"))
    );
    assert_eq!((Installed.button(), NewerInstalled.button()), (None, None));
    // The cost of an update is said where it is pressed: what the old version was allowed is forgotten.
    assert!(Update.note().is_some_and(|words| words.contains("forgets")));
    assert!(Install.note().is_none() && Installed.note().is_none());
}

#[test]
fn the_store_says_where_it_has_got_to_in_fixed_words_and_accepts_a_click_only_when_it_can() {
    use peersus_core::model::{StoreState, StoreStatus::*};
    let state = |status, listings: usize| StoreState {
        status,
        listings: (0..listings)
            .map(|n| listing(&format!("p{n}"), "1.0.0", None))
            .collect(),
        error: Some("what a download said".into()),
        asked: None,
    };
    assert_eq!(
        store_notice(&state(NotConfigured, 0)),
        Some("The extension store is not set up in this build.")
    );
    assert!(store_notice(&state(Idle, 0)).is_some() && store_notice(&state(Loading, 0)).is_some());
    assert_eq!(
        store_notice(&state(Ready, 0)),
        Some("The maintainer has not approved any extension yet.")
    );
    assert_eq!(
        store_notice(&state(Ready, 2)),
        None,
        "a list needs no sentence above it"
    );
    // Whatever a download or a package said (`error`) is never repeated: only the application's own words.
    for status in [NotConfigured, Idle, Loading, Ready, Failed] {
        assert!(
            store_notice(&state(status, 0)).is_none_or(|words| !words.contains("download said"))
        );
    }
    assert_eq!(
        [NotConfigured, Idle, Loading, Ready, Failed].map(|s| store_can_act(&state(s, 0))),
        [false, true, false, true, true]
    );
}
