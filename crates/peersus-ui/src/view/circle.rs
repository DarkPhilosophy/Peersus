//! The send circle: what one batch is, the stage it is at, the rows and the numbers drawn under it, what waits for it, and how
//! every circle answers a touch alike. Pure rules, no toolkit.
use super::*;

#[cfg(test)]
mod circle_view_tests {
    use super::*;

    fn one(id: &str, phase: Phase) -> Progress {
        Progress {
            id: id.into(),
            name: format!("{id}.txt"),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn devices() -> Aim {
        Aim::Devices(vec!["laptop".into()])
    }
    fn view_of(
        listed: usize,
        aim: Aim,
        code: Option<CodeCircle>,
        batch: &[Progress],
    ) -> CircleView {
        circle_view(&CircleInputs {
            listed,
            aim,
            send_instantly: false,
            code,
            batch,
            handed_over: 0,
        })
    }

    #[test]
    fn with_nothing_at_all_it_asks_for_files_and_with_files_listed_it_does_what_they_call_for() {
        assert_eq!(
            view_of(0, Aim::Code, None, &[]).does,
            Some(CircleDoes::ChooseFiles)
        );
        assert_eq!(
            view_of(2, Aim::Code, None, &[]).does,
            Some(CircleDoes::MakeCode)
        );
        assert_eq!(
            view_of(2, devices(), None, &[]).does,
            Some(CircleDoes::HoldToSend)
        );
    }

    #[test]
    fn while_a_transfer_goes_the_circle_is_shut_and_says_what_goes_and_never_asks_for_files() {
        let batch = [one("a", Phase::Moving)];
        let shut = view_of(0, Aim::Code, None, &batch);
        assert_eq!(shut.does, None);
        assert!(
            matches!(shut.batch, BatchState::Going { files: 1, .. }),
            "{:?}",
            shut.batch
        );
    }

    #[test]
    fn after_a_cancel_the_circle_is_open_again_and_still_says_how_it_ended_and_a_touch_chooses_new_files()
     {
        let batch = [one("a", Phase::Cancelled)];
        let open = view_of(0, Aim::Code, None, &batch);
        assert_eq!(
            open.does,
            Some(CircleDoes::ChooseFiles),
            "a circle with nothing going was dead"
        );
        assert_eq!(open.batch, BatchState::Stopped { files: 1, done: 0 });
    }

    #[test]
    fn new_files_chosen_after_a_cancel_make_the_circle_what_they_call_for_whatever_failed_before() {
        let batch = [one("a", Phase::Cancelled)];
        // The failure: a stale code said Sending and the circle stayed on what it saw before, instead of what is chosen now.
        let stale = Some(CodeCircle::Sending {
            files: 1,
            percent: 99,
        });
        assert_eq!(
            view_of(1, devices(), stale.clone(), &batch).does,
            Some(CircleDoes::HoldToSend)
        );
        assert_eq!(
            view_of(1, Aim::Code, stale, &batch).does,
            Some(CircleDoes::MakeCode)
        );
    }

    #[test]
    fn a_code_that_is_drawn_is_the_circle_and_a_scanned_one_waits_and_neither_takes_a_new_choice() {
        let drawn = view_of(3, devices(), Some(CodeCircle::Code), &[]);
        assert!(drawn.shows_code);
        assert_eq!(drawn.does, None);
        let scanned = view_of(3, devices(), Some(CodeCircle::Scanned), &[]);
        assert!(!scanned.shows_code);
        assert_eq!(scanned.does, None);
    }

    #[test]
    fn the_files_a_code_was_made_of_stay_named_under_it_until_it_is_no_longer_drawn() {
        let names =
            |all: &[&str]| -> Vec<String> { all.iter().map(|&name| name.to_owned()).collect() };
        let behind = names(&["a.txt", "b.txt"]);
        // From the touch that makes the code, and while the code is drawn.
        assert_eq!(names_under_circle(Vec::new(), Some(&behind), None), behind);
        assert_eq!(names_under_circle(Vec::new(), None, Some(&behind)), behind);
        // A code that is being made names its own files, not those of the one still on show before it.
        let older = names(&["old.txt"]);
        assert_eq!(
            names_under_circle(Vec::new(), Some(&behind), Some(&older)),
            behind
        );
        // Scanned, run out or taken back: nothing is named, the transfers are the rows.
        assert!(names_under_circle(Vec::new(), None, None).is_empty());
        // Files that are listed are what is named, whatever code there is.
        let listed = names(&["new.txt"]);
        assert_eq!(
            names_under_circle(listed.clone(), None, Some(&behind)),
            listed
        );
        // One list seen in two places: the names of a selection, or the last part of each path.
        let paths = ["/somewhere/a.txt", "b.txt"].map(std::path::PathBuf::from);
        assert_eq!(names_listed(&paths, &[]), behind);
        assert_eq!(names_listed(&[], &listed), listed);
    }

    #[test]
    fn a_code_that_is_still_sending_with_no_transfer_yet_holds_the_circle_shut_until_the_transfers_say_otherwise()
     {
        let sending = Some(CodeCircle::Sending {
            files: 2,
            percent: 10,
        });
        assert_eq!(view_of(0, Aim::Code, sending.clone(), &[]).does, None);
        // Once its transfers have all ended, the code's word no longer holds it shut.
        let ended = [one("a", Phase::Done), one("b", Phase::Cancelled)];
        assert_eq!(
            view_of(0, Aim::Code, sending, &ended).does,
            Some(CircleDoes::ChooseFiles)
        );
    }

    #[test]
    fn a_batch_that_arrived_whole_says_so_and_a_touch_chooses_the_next() {
        let batch = [one("a", Phase::Done)];
        let done = view_of(0, Aim::Code, None, &batch);
        assert_eq!(done.batch, BatchState::Sent { files: 1 });
        assert_eq!(done.does, Some(CircleDoes::ChooseFiles));
    }

    #[test]
    fn the_same_inputs_always_give_the_same_answer() {
        let batch = [one("a", Phase::Moving), one("b", Phase::Done)];
        let a = view_of(
            2,
            devices(),
            Some(CodeCircle::Sending {
                files: 2,
                percent: 50,
            }),
            &batch,
        );
        let b = view_of(
            2,
            devices(),
            Some(CodeCircle::Sending {
                files: 2,
                percent: 50,
            }),
            &batch,
        );
        assert_eq!(a, b);
    }
}

#[cfg(test)]
mod circle_view_words_tests {
    use super::*;

    fn one(phase: Phase) -> Progress {
        Progress {
            id: "t".into(),
            name: "a.txt".into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 40,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn words(listed: usize, aim: Aim, code: Option<CodeCircle>, batch: &[Progress]) -> CircleWords {
        let circle = circle_view(&CircleInputs {
            listed,
            aim,
            send_instantly: false,
            code: code.clone(),
            batch,
            handed_over: 0,
        });
        circle_view_words_in(Language::ENGLISH, &circle, code.as_ref(), 1, true)
    }

    #[test]
    fn with_nothing_at_all_it_asks_for_files_in_the_large_form() {
        let w = words(0, Aim::Code, None, &[]);
        assert!(w.asks_for_files);
        assert_eq!(w.headline, "CHOOSE FILES");
    }

    #[test]
    fn while_a_file_goes_it_says_so_and_never_asks_for_files() {
        let w = words(0, Aim::Code, None, &[one(Phase::Moving)]);
        assert!(
            !w.asks_for_files,
            "a circle with a transfer going asked for files"
        );
        assert_eq!(w.kicker, "SENDING");
        assert_eq!(w.headline, "40%");
    }

    #[test]
    fn after_a_cancel_it_says_how_it_ended_and_does_not_ask_for_files_in_the_large_form() {
        let w = words(0, Aim::Code, None, &[one(Phase::Cancelled)]);
        assert!(!w.asks_for_files);
        assert_eq!(w.kicker, "STOPPED");
        assert_eq!(w.headline, "0/1");
    }

    #[test]
    fn files_listed_say_what_the_circle_will_do_whatever_ended_before() {
        let hold = words(
            2,
            Aim::Devices(vec!["x".into()]),
            None,
            &[one(Phase::Cancelled)],
        );
        assert_eq!(hold.kicker, "HOLD TO SEND");
        assert!(!hold.asks_for_files);
        let code = words(2, Aim::Code, None, &[one(Phase::Cancelled)]);
        assert_eq!(code.kicker, "NO DEVICE PICKED");
    }

    #[test]
    fn a_code_that_waits_for_the_other_device_says_so_in_the_codes_own_words() {
        let w = words(0, Aim::Code, Some(CodeCircle::Scanned), &[]);
        assert!(!w.asks_for_files);
        assert!(!w.headline.is_empty());
    }
}

#[cfg(test)]
mod tap_tests {
    use super::*;
    #[test]
    fn tap_and_small_jitter_select_but_swiping_and_returning_never_select() {
        let mut tap = Tap::default();
        tap.down((20., 20.));
        assert!(tap.up((20. + TAP_SLOP, 20.)));
        assert!(!tap.up((20., 20.)), "one gesture selected twice");
        tap.down((20., 20.));
        tap.moved((20., 200.));
        tap.moved((20., 20.));
        assert!(!tap.up((20., 20.)), "scrolling selected a search result");
        tap.down((20., 20.));
        tap.cancel();
        assert!(!tap.up((20., 20.)));
    }

    #[test]
    fn a_short_touch_is_a_tap_and_a_long_one_is_a_hold_and_never_both() {
        let mut hold = Hold::default();
        hold.down((50., 50.), 1_000);
        assert_eq!(hold.up((50., 50.), 1_000 + HOLD_MS - 1), Ended::Tap);
        hold.down((50., 50.), 1_000);
        assert_eq!(
            hold.up((50., 50.), 1_000 + HOLD_MS),
            Ended::Hold,
            "the boundary itself is a hold"
        );
        // One touch ends once: a second lift with no touch before it is nothing.
        assert_eq!(hold.up((50., 50.), 9_999), Ended::Nothing);
    }

    #[test]
    fn a_touch_that_strays_is_a_drag_and_never_a_hold_however_long_it_stays_down() {
        let mut hold = Hold::default();
        hold.down((50., 50.), 0);
        hold.moved((50., 50. + TAP_SLOP + 1.));
        assert!(!hold.is_holding(HOLD_MS * 10), "a drag was held");
        // Coming back to where it began does not undo it, as for a tap.
        hold.moved((50., 50.));
        assert_eq!(hold.up((50., 50.), HOLD_MS * 10), Ended::Nothing);
    }

    #[test]
    fn small_jitter_while_holding_is_still_a_hold() {
        let mut hold = Hold::default();
        hold.down((50., 50.), 0);
        hold.moved((50. + TAP_SLOP, 50.));
        assert!(hold.is_holding(HOLD_MS));
        assert_eq!(hold.up((50. + TAP_SLOP, 50.), HOLD_MS + 40), Ended::Hold);
    }

    #[test]
    fn a_finger_is_not_holding_until_the_time_is_up_and_not_at_all_without_a_touch() {
        let mut hold = Hold::default();
        assert!(!hold.is_holding(HOLD_MS * 10), "held with no touch");
        hold.down((10., 10.), 100);
        assert!(!hold.is_holding(100 + HOLD_MS - 1));
        assert!(hold.is_holding(100 + HOLD_MS));
    }

    #[test]
    fn a_cancelled_touch_is_nothing_and_does_not_leak_into_the_next() {
        let mut hold = Hold::default();
        hold.down((10., 10.), 0);
        hold.cancel();
        assert!(!hold.is_holding(HOLD_MS * 2));
        assert_eq!(hold.up((10., 10.), HOLD_MS * 2), Ended::Nothing);
        hold.down((10., 10.), 5_000);
        assert_eq!(
            hold.up((10., 10.), 5_000 + MIN_TOUCH_MS),
            Ended::Tap,
            "the cancelled touch's clock was kept"
        );
    }

    #[test]
    fn a_clock_that_runs_backwards_never_makes_a_hold() {
        let mut hold = Hold::default();
        hold.down((10., 10.), 10_000);
        assert_eq!(
            hold.up((10., 10.), 5),
            Ended::Nothing,
            "a clock that ran backwards made a touch"
        );
    }

    #[test]
    fn a_brush_of_the_screen_is_not_a_touch_and_a_meant_one_is_whatever_the_circle() {
        let mut hold = Hold::default();
        hold.down((50., 50.), 1_000);
        assert_eq!(
            hold.up((50., 50.), 1_000),
            Ended::Nothing,
            "a contact of no time was a tap"
        );
        hold.down((50., 50.), 1_000);
        assert_eq!(
            hold.up((50., 50.), 1_000 + MIN_TOUCH_MS - 1),
            Ended::Nothing,
            "a brush was a tap"
        );
        hold.down((50., 50.), 1_000);
        assert_eq!(
            hold.up((50., 50.), 1_000 + MIN_TOUCH_MS),
            Ended::Tap,
            "the boundary itself is a meant touch"
        );
        const {
            assert!(
                MIN_TOUCH_MS * 5 < HOLD_MS,
                "the guard is so long that a person tapping would feel it"
            )
        };
    }
}

#[cfg(test)]
mod list_under_the_circle_tests {
    use super::*;

    fn one(id: &str, phase: Phase) -> Progress {
        Progress {
            id: id.to_owned(),
            name: format!("{id}.bin"),
            folders: Vec::new(),
            bytes: 10,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn the_file_that_is_going_is_first_so_nobody_has_to_scroll_for_it_and_what_is_over_follows_newest_first()
     {
        // The record keeps transfers in the order they were made: the oldest first.
        let all = vec![
            one("a", Phase::Done),
            one("b", Phase::Done),
            one("c", Phase::Moving),
            one("d", Phase::Done),
            one("e", Phase::Waiting),
        ];
        let ids: Vec<&str> = transfer_list(&all).map(|item| item.id.as_str()).collect();
        assert_eq!(
            &ids[..2],
            ["c", "e"],
            "a file that is going was not on top: {ids:?}"
        );
        assert_eq!(
            &ids[2..],
            ["d", "b", "a"],
            "what is over was not newest first: {ids:?}"
        );
    }

    #[test]
    fn after_the_transfer_ends_the_file_stays_with_its_status_until_it_is_hidden_and_then_it_is_gone_from_the_list_not_from_the_record()
     {
        let start: Vec<String> = Vec::new();
        let mut all = vec![one("now", Phase::Moving)];
        assert_eq!(of_this_run(&all, &start).len(), 1);
        all[0].phase = Phase::Failed("the other device left".into());
        let kept = of_this_run(&all, &start);
        assert_eq!(kept.len(), 1, "a failed file vanished by itself");
        assert_eq!(
            kept[0].phase,
            Phase::Failed("the other device left".into()),
            "the status was lost"
        );
        // X puts away what is over; the record itself is untouched.
        let hidden = put_away_now(&all);
        assert!(
            not_put_away(&all, &hidden).is_empty(),
            "the hidden file was still listed"
        );
        assert_eq!(all.len(), 1, "hiding changed the record");
    }
}

#[cfg(test)]
mod fingers_tests {
    use super::*;

    #[test]
    fn one_finger_is_not_two_and_the_second_finger_is_two_once() {
        let mut fingers = Fingers::default();
        assert!(!fingers.went_down(1, 0));
        assert!(
            fingers.went_down(2, 10),
            "the second finger was not a touch of two"
        );
        assert!(!fingers.went_down(3, 20), "a third finger said two again");
    }

    #[test]
    fn a_finger_that_was_never_reported_lifted_does_not_make_every_later_touch_a_touch_of_two() {
        // The failure: Settings closes under a finger, its lift never reaches the window, and the ghost is counted for ever.
        let mut fingers = Fingers::default();
        assert!(!fingers.went_down(7, 0));
        // A later, ordinary touch, long after, with one finger only.
        let later = FINGER_GONE_MS + 1;
        assert!(
            !fingers.went_down(9, later),
            "a ghost finger made an ordinary touch open Settings"
        );
        assert_eq!(fingers.count(), 1);
    }

    #[test]
    fn a_lifted_finger_is_gone_at_once_so_the_next_touch_is_alone() {
        let mut fingers = Fingers::default();
        fingers.went_down(1, 0);
        fingers.went_up(1);
        assert!(!fingers.went_down(2, 5));
        assert_eq!(fingers.count(), 1);
    }

    #[test]
    fn the_system_giving_a_number_again_is_one_finger_and_not_two() {
        let mut fingers = Fingers::default();
        fingers.went_down(4, 0);
        assert!(
            !fingers.went_down(4, 100),
            "the same finger twice was a touch of two"
        );
        assert_eq!(fingers.count(), 1);
    }

    #[test]
    fn a_finger_that_keeps_moving_is_still_down_however_long_it_stays() {
        let mut fingers = Fingers::default();
        fingers.went_down(1, 0);
        for step in 1..=5 {
            fingers.moved(1, step * (FINGER_GONE_MS - 1));
        }
        // Alive: a second finger now is a touch of two.
        assert!(fingers.went_down(2, 5 * (FINGER_GONE_MS - 1) + 1));
    }

    #[test]
    fn clearing_forgets_every_finger() {
        let mut fingers = Fingers::default();
        fingers.went_down(1, 0);
        fingers.went_down(2, 1);
        fingers.clear();
        assert_eq!(fingers.count(), 0);
        assert!(!fingers.went_down(3, 2));
    }
}

#[cfg(test)]
mod one_batch_tests {
    use super::*;

    fn one(id: &str, name: &str, phase: Phase) -> Progress {
        Progress {
            id: id.to_owned(),
            name: name.to_owned(),
            folders: Vec::new(),
            bytes: 10,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn choosing_new_files_starts_a_new_list_and_the_cancelled_or_sent_batch_before_it_is_not_in_it()
    {
        let mut all = vec![
            one("t1", "a.txt", Phase::Cancelled),
            one("t2", "b.txt", Phase::Done),
        ];
        // The person chooses c.txt: the batch before is no longer the circle's list.
        let before = ids_at_start(&all);
        let rows = list_under_circle(&["c.txt".into()], &of_this_run(&all, &before));
        assert_eq!(
            rows,
            [ListRow::Prepared("c.txt".into())],
            "an earlier batch stayed under the circle: {rows:?}"
        );
        // c.txt is sent and goes: its transfer is the batch's, and only it.
        all.push(one("t3", "c.txt", Phase::Moving));
        let rows = list_under_circle(&[], &of_this_run(&all, &before));
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], ListRow::Transfer(item) if item.id == "t3"));
    }

    #[test]
    fn a_file_chosen_again_after_it_was_cancelled_is_one_prepared_row_and_not_a_cancelled_one_beside_it()
     {
        let all = vec![one("t1", "a.txt", Phase::Cancelled)];
        let before = ids_at_start(&all);
        let rows = list_under_circle(&["a.txt".into()], &of_this_run(&all, &before));
        assert_eq!(
            rows,
            [ListRow::Prepared("a.txt".into())],
            "the cancelled file stayed beside the prepared one: {rows:?}"
        );
    }

    #[test]
    fn a_transfer_still_going_from_the_batch_before_stays_in_the_list_until_it_ends() {
        let all = vec![one("t1", "a.txt", Phase::Moving)];
        let before = ids_at_start(&all);
        let rows = list_under_circle(&["b.txt".into()], &of_this_run(&all, &before));
        assert_eq!(rows.len(), 2, "a transfer in progress was hidden: {rows:?}");
    }

    #[test]
    fn the_list_never_holds_more_than_the_batch_however_many_were_sent_before() {
        let history: Vec<Progress> = (0..5_000)
            .map(|n| one(&format!("old-{n}"), &format!("f{n}.bin"), Phase::Done))
            .collect();
        let before = ids_at_start(&history);
        assert!(list_under_circle(&[], &of_this_run(&history, &before)).is_empty());
        assert_eq!(
            list_under_circle(
                &["x.txt".into(), "y.txt".into()],
                &of_this_run(&history, &before)
            )
            .len(),
            2
        );
    }
}

#[cfg(test)]
mod list_of_the_circle_tests {
    use super::*;

    fn sent(id: &str, name: &str, phase: Phase) -> Progress {
        Progress {
            id: id.to_owned(),
            name: name.to_owned(),
            folders: Vec::new(),
            bytes: 10,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn names(rows: &[ListRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                ListRow::Prepared(name) => format!("prepared:{name}"),
                ListRow::Transfer(item) => format!("{:?}:{}", item.phase, item.name),
            })
            .collect()
    }

    #[test]
    fn files_that_are_only_listed_are_rows_with_their_status_prepared_before_anything_is_sent() {
        let rows = list_under_circle(&["a.txt".into(), "b.txt".into()], &of_this_run(&[], &[]));
        assert_eq!(names(&rows), ["prepared:a.txt", "prepared:b.txt"]);
    }

    #[test]
    fn a_transfer_that_has_begun_has_its_own_row_and_the_files_still_listed_stay_prepared() {
        // a.txt was sent (it is no longer listed); b.txt is still listed.
        let all = vec![sent("t1", "a.txt", Phase::Moving)];
        let rows = list_under_circle(&["b.txt".into()], &of_this_run(&all, &[]));
        assert_eq!(names(&rows), ["Moving:a.txt", "prepared:b.txt"]);
    }

    #[test]
    fn the_file_that_is_going_is_first_then_what_is_over_then_what_is_still_to_go() {
        let all = vec![
            sent("t1", "a.txt", Phase::Done),
            sent("t2", "b.txt", Phase::Moving),
        ];
        let rows = list_under_circle(&["c.txt".into()], &of_this_run(&all, &[]));
        assert_eq!(
            names(&rows),
            ["Moving:b.txt", "Done:a.txt", "prepared:c.txt"]
        );
    }

    #[test]
    fn two_listed_files_with_one_name_are_two_rows() {
        let rows = list_under_circle(
            &["same.txt".into(), "same.txt".into()],
            &of_this_run(&[], &[]),
        );
        assert_eq!(names(&rows), ["prepared:same.txt", "prepared:same.txt"]);
    }

    #[test]
    fn the_history_of_the_start_is_never_in_it_and_a_failed_file_keeps_its_reason() {
        let old: Vec<Progress> = (0..50)
            .map(|n| sent(&format!("old-{n}"), "old.bin", Phase::Done))
            .collect();
        let start = ids_at_start(&old);
        let mut all = old;
        all.push(sent(
            "now",
            "a.txt",
            Phase::Failed("the other device left".into()),
        ));
        // Sent, so no longer listed: the file is the transfer, with its reason.
        let rows = list_under_circle(&[], &of_this_run(&all, &start));
        assert_eq!(rows.len(), 1, "history was listed: {rows:?}");
        assert_eq!(names(&rows), ["Failed(\"the other device left\"):a.txt"]);
    }

    #[test]
    fn a_file_that_is_chosen_again_after_its_transfer_was_cancelled_is_listed_again() {
        // The person chose a.txt, cancelled its transfer, and chose a.txt again: the new choice is a file that has not been sent.
        let all = vec![sent("t1", "a.txt", Phase::Cancelled)];
        let rows = list_under_circle(&["a.txt".into()], &of_this_run(&all, &[]));
        assert_eq!(
            names(&rows),
            ["Cancelled:a.txt", "prepared:a.txt"],
            "a file chosen again was taken for the cancelled one"
        );
    }

    #[test]
    fn a_file_chosen_while_others_are_going_or_over_is_listed_after_them_and_is_never_lost() {
        let all = vec![
            sent("t1", "a.txt", Phase::Cancelled),
            sent("t2", "c.txt", Phase::Moving),
        ];
        let rows = list_under_circle(&["b.txt".into(), "d.txt".into()], &of_this_run(&all, &[]));
        assert_eq!(
            names(&rows),
            [
                "Moving:c.txt",
                "Cancelled:a.txt",
                "prepared:b.txt",
                "prepared:d.txt"
            ]
        );
    }

    #[test]
    fn a_file_that_came_in_is_not_a_file_that_was_listed_to_go_out() {
        let mut incoming = sent("in", "a.txt", Phase::Moving);
        incoming.incoming = true;
        let rows = list_under_circle(&["a.txt".into()], &of_this_run(&[incoming], &[]));
        assert_eq!(
            names(&rows),
            ["Moving:a.txt", "prepared:a.txt"],
            "a file received took the place of one to be sent"
        );
    }
}

#[cfg(test)]
mod this_run_tests {
    use super::*;

    fn transfer(id: &str, phase: Phase) -> Progress {
        Progress {
            id: id.to_owned(),
            name: format!("{id}.bin"),
            folders: Vec::new(),
            bytes: 0,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn the_history_that_was_there_at_the_start_is_never_listed_however_long_it_is() {
        let history: Vec<Progress> = (0..5_000)
            .map(|n| transfer(&format!("old-{n}"), Phase::Done))
            .collect();
        let start = ids_at_start(&history);
        assert!(
            of_this_run(&history, &start).is_empty(),
            "the old record was listed on Send or Receive"
        );
    }

    #[test]
    fn one_file_sent_now_is_one_row_not_the_record_and_it_stays_when_it_is_over() {
        let mut all: Vec<Progress> = (0..200)
            .map(|n| transfer(&format!("old-{n}"), Phase::Done))
            .collect();
        let start = ids_at_start(&all);
        all.push(transfer("now", Phase::Moving));
        let going = of_this_run(&all, &start);
        assert_eq!(going.len(), 1);
        assert_eq!(going[0].id, "now");
        all.last_mut().unwrap().phase = Phase::Done;
        let over = of_this_run(&all, &start);
        assert_eq!(over.len(), 1, "the transfer vanished the moment it ended");
        assert_eq!(over[0].id, "now");
    }

    #[test]
    fn a_transfer_begun_before_the_start_and_still_going_is_listed_until_it_ends_and_then_it_is_history()
     {
        let mut all = vec![transfer("early", Phase::Moving)];
        let start = ids_at_start(&all);
        assert_eq!(
            of_this_run(&all, &start).len(),
            1,
            "a transfer in progress was hidden"
        );
        all[0].phase = Phase::Done;
        assert!(of_this_run(&all, &start).is_empty());
    }
}

#[cfg(test)]
mod batch_state_tests {
    use super::*;

    fn one(id: &str, name: &str, phase: Phase, percent: u8) -> Progress {
        Progress {
            id: id.to_owned(),
            name: name.to_owned(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn no_transfer_is_no_batch_and_a_received_file_is_not_a_batch_of_this_persons() {
        assert_eq!(batch_state(&[]), BatchState::None);
        let mut incoming = one("in", "a.txt", Phase::Moving, 10);
        incoming.incoming = true;
        assert_eq!(
            batch_state(&[incoming]),
            BatchState::None,
            "a file that came in made a batch that went out"
        );
    }

    #[test]
    fn while_a_file_goes_the_batch_is_going_with_its_percent_and_the_file_that_is_going() {
        let batch = [
            one("a", "a.txt", Phase::Done, 100),
            one("b", "b.txt", Phase::Moving, 40),
        ];
        let BatchState::Going {
            files,
            percent,
            current,
        } = batch_state(&batch)
        else {
            panic!("not going")
        };
        assert_eq!(files, 2);
        assert_eq!(current.as_deref(), Some("b.txt"));
        assert!(percent > 0 && percent < 100, "{percent}");
    }

    #[test]
    fn a_file_still_waiting_to_be_accepted_is_going_too_so_the_circle_does_not_ask_for_files() {
        let batch = [one("a", "a.txt", Phase::Waiting, 0)];
        assert!(matches!(
            batch_state(&batch),
            BatchState::Going { files: 1, .. }
        ));
    }

    #[test]
    fn when_all_arrived_it_is_sent_and_when_some_did_not_it_says_how_many_did() {
        assert_eq!(
            batch_state(&[
                one("a", "a", Phase::Done, 100),
                one("b", "b", Phase::Done, 100)
            ]),
            BatchState::Sent { files: 2 }
        );
        assert_eq!(
            batch_state(&[
                one("a", "a", Phase::Done, 100),
                one("b", "b", Phase::Cancelled, 10)
            ]),
            BatchState::Stopped { files: 2, done: 1 }
        );
        assert_eq!(
            batch_state(&[one("a", "a", Phase::Failed("x".into()), 0)]),
            BatchState::Stopped { files: 1, done: 0 }
        );
    }
}

#[cfg(test)]
mod batch_words_tests {
    use super::*;

    #[test]
    fn every_state_of_a_batch_has_words_and_no_batch_has_none() {
        assert_eq!(batch_words_in(Language::ENGLISH, &BatchState::None), None);
        for state in [
            BatchState::Going {
                files: 2,
                percent: 40,
                current: Some("b.txt".into()),
            },
            BatchState::Going {
                files: 2,
                percent: 40,
                current: None,
            },
            BatchState::Sent { files: 1 },
            BatchState::Sent { files: 3 },
            BatchState::Stopped { files: 3, done: 1 },
        ] {
            let (kicker, headline, line) =
                batch_words_in(Language::ENGLISH, &state).expect("a batch had no words");
            assert!(
                !kicker.is_empty() && !headline.is_empty() && !line.is_empty(),
                "{state:?} left a line empty"
            );
        }
    }

    #[test]
    fn while_it_goes_the_percent_and_the_file_that_is_going_are_said() {
        let (kicker, headline, line) = batch_words_in(
            Language::ENGLISH,
            &BatchState::Going {
                files: 3,
                percent: 40,
                current: Some("b.txt".into()),
            },
        )
        .unwrap();
        assert_eq!(
            (kicker.as_str(), headline.as_str(), line.as_str()),
            ("SENDING", "40%", "b.txt")
        );
    }

    #[test]
    fn when_it_stopped_it_says_how_many_arrived_and_not_a_bare_error() {
        let (kicker, headline, line) = batch_words_in(
            Language::ENGLISH,
            &BatchState::Stopped { files: 1, done: 0 },
        )
        .unwrap();
        assert_eq!((kicker.as_str(), headline.as_str()), ("STOPPED", "0/1"));
        assert!(line.contains("0 of 1"), "{line}");
    }
}

#[cfg(test)]
mod circle_words_tests {
    use super::*;

    #[test]
    fn every_thing_the_circle_can_do_has_its_own_words_and_none_is_left_empty() {
        for does in [
            CircleDoes::ChooseFiles,
            CircleDoes::MakeCode,
            CircleDoes::HoldToSend,
            CircleDoes::TapToSend,
            CircleDoes::Held,
        ] {
            let (kicker, headline, line) = circle_does_words_in(Language::ENGLISH, does, 2);
            assert!(
                !kicker.is_empty() && !headline.is_empty() && !line.is_empty(),
                "{does:?} left a line empty"
            );
        }
    }

    #[test]
    fn a_code_a_hold_an_instant_send_and_a_held_back_circle_each_say_something_different() {
        let words = |does| circle_does_words_in(Language::ENGLISH, does, 1);
        let all = [
            CircleDoes::MakeCode,
            CircleDoes::HoldToSend,
            CircleDoes::TapToSend,
            CircleDoes::Held,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(words(*a), words(*b), "{a:?} and {b:?} say the same");
            }
        }
    }

    #[test]
    fn the_number_of_devices_is_said_when_there_is_more_than_one() {
        let (_, one, _) = circle_does_words_in(Language::ENGLISH, CircleDoes::HoldToSend, 1);
        let (_, three, _) = circle_does_words_in(Language::ENGLISH, CircleDoes::HoldToSend, 3);
        assert!(three.contains('3'), "{three}");
        assert_ne!(one, three);
    }
}

#[cfg(test)]
mod circle_touch_tests {
    use super::*;
    const MID: (f32, f32) = (50., 50.);
    const HOLD: CircleDoes = CircleDoes::HoldToSend;

    fn down_at(touch: &mut CircleTouch, in_box: (f32, f32), at: u64) {
        touch.down(in_box, (200., 300.), at);
    }

    /// A finger that went down at `from_ms` and rests on the circle until `until_ms`, heard of every 100 ms as a real one is (it trembles, and the
    /// toolkit says so): the last thing the clock said.
    fn held_until(
        touch: &mut CircleTouch,
        from_ms: u64,
        until_ms: u64,
        does: CircleDoes,
    ) -> CircleAct {
        let mut last = CircleAct::Nothing;
        let mut at = from_ms;
        while at < until_ms {
            at = (at + 100).min(until_ms);
            touch.seen(at);
            let act = touch.tick(at, does);
            if act != CircleAct::Nothing {
                last = act;
            }
        }
        last
    }

    #[test]
    fn holding_fills_and_sends_once_when_full_without_waiting_for_the_lift_and_the_lift_then_sends_nothing()
     {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 1_000);
        assert_eq!(
            held_until(&mut touch, 1_000, 1_000 + CONFIRM_MS - 1, HOLD),
            CircleAct::Nothing
        );
        assert_eq!(
            held_until(&mut touch, 1_000 + CONFIRM_MS - 1, 1_000 + CONFIRM_MS, HOLD),
            CircleAct::Send
        );
        assert_eq!(
            touch.tick(1_000 + CONFIRM_MS + 50, HOLD),
            CircleAct::Nothing,
            "a touch sent twice"
        );
        assert_eq!(
            touch.up(MID, (200., 300.), 1_000 + CONFIRM_MS + 80, HOLD),
            CircleAct::Nothing,
            "the lift sent a second time"
        );
    }

    #[test]
    fn lifting_before_it_is_full_sends_nothing_and_leaves_no_fill() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert!(touch.fill(CONFIRM_MS / 2, HOLD) > 0.4);
        assert_eq!(
            touch.up(MID, (200., 300.), CONFIRM_MS - 1, HOLD),
            CircleAct::Nothing
        );
        assert_eq!(
            touch.fill(CONFIRM_MS * 3, HOLD),
            0.,
            "a lifted finger kept the circle filling"
        );
        assert!(!touch.is_down());
    }

    #[test]
    fn a_touch_in_the_corner_of_the_box_outside_the_round_never_starts_a_hold() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, (2., 2.), 0);
        assert!(!touch.is_down(), "a corner of the square started a hold");
        assert_eq!(touch.tick(CONFIRM_MS * 5, HOLD), CircleAct::Nothing);
        assert_eq!(
            touch.up(MID, (200., 300.), CONFIRM_MS * 5, CircleDoes::TapToSend),
            CircleAct::Nothing
        );
    }

    #[test]
    fn a_drag_ends_the_hold_for_good_even_if_the_finger_comes_back() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        touch.moved((200., 300. + TAP_SLOP + 1.));
        touch.moved((200., 300.));
        assert!(!touch.is_down());
        assert_eq!(
            touch.tick(CONFIRM_MS * 2, HOLD),
            CircleAct::Nothing,
            "a drag sent the files"
        );
        assert_eq!(
            touch.up(MID, (200., 300.), CONFIRM_MS * 2, HOLD),
            CircleAct::Nothing
        );
    }

    #[test]
    fn with_no_device_picked_a_whole_tap_makes_a_code_and_nothing_fills_or_waits() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.fill(CONFIRM_MS, CircleDoes::MakeCode),
            0.,
            "a circle for a code filled"
        );
        assert_eq!(
            touch.tick(CONFIRM_MS * 3, CircleDoes::MakeCode),
            CircleAct::Nothing,
            "a code was made by waiting"
        );
        // The person has to touch it for the code: a whole tap.
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.up(MID, (200., 300.), 80, CircleDoes::MakeCode),
            CircleAct::MakeCode
        );
        // A long press is not the tap that asks for it, and a drag never is.
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.up(MID, (200., 300.), HOLD_MS + 10, CircleDoes::MakeCode),
            CircleAct::Nothing,
            "a long press made a code"
        );
    }

    #[test]
    fn with_devices_picked_a_tap_does_not_send_for_holding_is_the_confirmation() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.up(MID, (200., 300.), 80, HOLD),
            CircleAct::Nothing,
            "a tap sent without the hold the person kept on"
        );
    }

    #[test]
    fn a_circle_held_back_by_a_device_that_went_away_does_nothing_at_all() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(touch.fill(CONFIRM_MS, CircleDoes::Held), 0.);
        assert_eq!(
            touch.tick(CONFIRM_MS * 3, CircleDoes::Held),
            CircleAct::Nothing
        );
        assert_eq!(
            touch.up(MID, (200., 300.), 80, CircleDoes::Held),
            CircleAct::Nothing
        );
    }

    #[test]
    fn with_nothing_listed_a_tap_chooses_files_and_a_long_hold_does_not() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.up(MID, (200., 300.), 100, CircleDoes::ChooseFiles),
            CircleAct::Choose
        );
        down_at(&mut touch, MID, 0);
        assert_eq!(
            touch.up(MID, (200., 300.), HOLD_MS + 10, CircleDoes::ChooseFiles),
            CircleAct::Nothing
        );
    }

    #[test]
    fn send_instantly_sends_on_a_whole_tap_only_and_never_fills() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(touch.fill(CONFIRM_MS * 4, CircleDoes::TapToSend), 0.);
        assert_eq!(
            held_until(&mut touch, 0, CONFIRM_MS * 4, CircleDoes::TapToSend),
            CircleAct::Nothing,
            "a finger resting on the circle sent in the instant mode"
        );
        // The touch above went on for so long that it was a hold: a new one is the tap.
        down_at(&mut touch, MID, 10_000);
        assert_eq!(
            touch.up(MID, (200., 300.), 10_080, CircleDoes::TapToSend),
            CircleAct::Send
        );
        down_at(&mut touch, MID, 20_000);
        assert_eq!(
            touch.up(
                MID,
                (200., 300.),
                20_000 + HOLD_MS + 10,
                CircleDoes::TapToSend
            ),
            CircleAct::Nothing,
            "a long press sent instantly"
        );
    }

    #[test]
    fn a_new_touch_after_a_send_can_send_again_and_a_cancel_sends_nothing() {
        let mut touch = CircleTouch::new(100.);
        down_at(&mut touch, MID, 0);
        assert_eq!(held_until(&mut touch, 0, CONFIRM_MS, HOLD), CircleAct::Send);
        touch.up(MID, (200., 300.), CONFIRM_MS + 1, HOLD);
        down_at(&mut touch, MID, 5_000);
        touch.cancel();
        assert_eq!(
            touch.tick(5_000 + CONFIRM_MS * 2, HOLD),
            CircleAct::Nothing,
            "a cancelled touch sent"
        );
        down_at(&mut touch, MID, 9_000);
        assert_eq!(
            held_until(&mut touch, 9_000, 9_000 + CONFIRM_MS, HOLD),
            CircleAct::Send
        );
    }
}

#[cfg(test)]
mod finger_life_tests {
    use super::*;
    const MID: (f32, f32) = (50., 50.);

    #[test]
    fn a_finger_that_is_no_longer_heard_of_never_sends_however_long_the_clock_runs() {
        // The finger went down and its lift never reached the circle: nothing says it is gone but the silence.
        let mut touch = CircleTouch::new(100.);
        touch.down(MID, (200., 300.), 0);
        assert_eq!(
            touch.tick(FINGER_LIVE_MS + 1, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
        assert!(
            !touch.is_down(),
            "a silent finger was still taken to be down"
        );
        assert_eq!(
            touch.tick(CONFIRM_MS * 5, CircleDoes::HoldToSend),
            CircleAct::Nothing,
            "the circle sent files nobody was holding"
        );
    }

    #[test]
    fn a_silent_finger_leaves_the_circle_empty_so_it_does_not_go_on_charging() {
        let mut touch = CircleTouch::new(100.);
        touch.down(MID, (200., 300.), 0);
        touch.seen(100);
        assert!(touch.fill(100 + FINGER_LIVE_MS / 2, CircleDoes::HoldToSend) > 0.);
        // Silence: the next tick ends it, and what the circle shows is empty, for good.
        touch.tick(100 + FINGER_LIVE_MS + 1, CircleDoes::HoldToSend);
        assert_eq!(
            touch.fill(100 + FINGER_LIVE_MS + 2, CircleDoes::HoldToSend),
            0.,
            "the circle went on charging for a finger that was gone"
        );
        assert!(!touch.is_down());
    }

    #[test]
    fn a_sign_of_a_finger_that_is_not_down_brings_nothing_back() {
        let mut touch = CircleTouch::new(100.);
        touch.seen(1_000);
        assert!(!touch.is_down());
        assert_eq!(
            touch.tick(1_000 + CONFIRM_MS, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
    }

    #[test]
    fn the_life_of_a_finger_is_shorter_than_the_hold_so_a_lost_lift_is_noticed_before_it_could_send()
     {
        const { assert!(FINGER_LIVE_MS < HOLD_MS && HOLD_MS < CONFIRM_MS) };
    }

    #[test]
    fn a_mouse_held_perfectly_still_fills_and_sends_for_it_says_nothing_while_it_rests_and_that_is_not_a_lost_lift()
     {
        let mut mouse = CircleTouch::new(100.);
        mouse.down(MID, (200., 300.), 0);
        mouse.steady();
        // No sign of it at all, from the moment it went down to the moment the circle is full, as with a hand that does not move.
        let mut sent_at = None;
        for ms in (0..=CONFIRM_MS + 100).step_by(33) {
            if mouse.tick(ms, CircleDoes::HoldToSend) == CircleAct::Send {
                sent_at = Some(ms);
                break;
            }
            assert!(
                mouse.is_down(),
                "a mouse that rests was taken to be gone at {ms} ms"
            );
        }
        let sent_at = sent_at.expect("a mouse held for the whole time never sent");
        assert!(
            (CONFIRM_MS..CONFIRM_MS + 100).contains(&sent_at),
            "it sent at {sent_at} ms, not when it was full"
        );
    }

    #[test]
    fn a_mouse_hold_still_ends_when_the_button_is_lifted_and_when_it_strays() {
        let mut lifted = CircleTouch::new(100.);
        lifted.down(MID, (200., 300.), 0);
        lifted.steady();
        lifted.cancel();
        assert!(!lifted.is_down(), "a released mouse was still down");
        // After a release the mark is gone: the next touch is judged on its own, by the pointer that makes it.
        lifted.down(MID, (200., 300.), 5_000);
        assert_eq!(
            lifted.tick(5_000 + FINGER_LIVE_MS + 1, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
        assert!(
            !lifted.is_down(),
            "the mark of a mouse outlived its touch and spared a finger"
        );
        let mut strayed = CircleTouch::new(100.);
        strayed.down(MID, (200., 300.), 0);
        strayed.steady();
        strayed.moved((900., 900.));
        assert!(
            !strayed.is_down(),
            "a mouse that left the circle was still held"
        );
        assert_eq!(
            strayed.tick(CONFIRM_MS * 5, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
    }

    #[test]
    fn a_finger_is_still_judged_by_its_silence_for_only_a_finger_can_lose_its_lift() {
        let mut finger = CircleTouch::new(100.);
        finger.down(MID, (200., 300.), 0);
        assert_eq!(
            finger.tick(FINGER_LIVE_MS + 1, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
        assert!(!finger.is_down(), "a finger that went silent was kept");
    }

    #[test]
    fn a_second_down_on_a_circle_whose_mouse_is_held_is_not_a_new_touch_and_steady_needs_a_touch() {
        let mut mouse = CircleTouch::new(100.);
        mouse.down(MID, (200., 300.), 0);
        mouse.steady();
        assert!(
            !mouse.down(MID, (200., 300.), 10_000),
            "a second pointer restarted a hold that was in progress"
        );
        // Said of a touch that is not down, it marks nothing: a later finger is not spared by it.
        let mut none = CircleTouch::new(100.);
        none.steady();
        none.down(MID, (200., 300.), 0);
        assert_eq!(
            none.tick(FINGER_LIVE_MS + 1, CircleDoes::HoldToSend),
            CircleAct::Nothing
        );
        assert!(
            !none.is_down(),
            "steady, said before any touch, spared the finger that came after"
        );
    }
}

#[cfg(test)]
mod nothing_to_send_tests {
    use super::*;

    fn one(phase: Phase) -> Progress {
        Progress {
            id: "t".into(),
            name: "a.txt".into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn with_nothing_listed_the_circle_is_never_held_to_send_or_asked_for_a_code_whoever_is_picked_and_whatever_ended()
     {
        let devices = Aim::Devices(vec!["laptop".into()]);
        for aim in [Aim::Code, devices, Aim::Held] {
            for batch in [
                vec![],
                vec![one(Phase::Cancelled)],
                vec![one(Phase::Done)],
                vec![one(Phase::Failed("x".into()))],
            ] {
                for send_instantly in [false, true] {
                    let view = circle_view(&CircleInputs {
                        listed: 0,
                        aim: aim.clone(),
                        send_instantly,
                        code: None,
                        batch: &batch,
                        handed_over: 0,
                    });
                    assert!(
                        !matches!(
                            view.does,
                            Some(
                                CircleDoes::HoldToSend
                                    | CircleDoes::TapToSend
                                    | CircleDoes::MakeCode
                                    | CircleDoes::Held
                            )
                        ),
                        "a circle with nothing listed offered to send or make a code ({aim:?}, {batch:?}, instantly={send_instantly}): {:?}",
                        view.does
                    );
                }
            }
        }
    }

    #[test]
    fn after_a_cancel_with_devices_still_picked_a_touch_chooses_files_and_does_not_start_charging()
    {
        let view = circle_view(&CircleInputs {
            listed: 0,
            aim: Aim::Devices(vec!["laptop".into()]),
            send_instantly: false,
            code: None,
            batch: &[one(Phase::Cancelled)],
            handed_over: 0,
        });
        assert_eq!(view.does, Some(CircleDoes::ChooseFiles));
        // And nothing in the touch model charges for it: only a circle that is held to send ever fills.
        let mut touch = CircleTouch::new(100.);
        touch.down((50., 50.), (200., 300.), 0);
        for at in (100..=3_000).step_by(100) {
            touch.seen(at);
            assert_eq!(
                touch.tick(at, view.does.unwrap()),
                CircleAct::Nothing,
                "it sent with nothing to send at {at}"
            );
            assert_eq!(
                touch.fill(at, view.does.unwrap()),
                0.,
                "the circle charged with nothing to send at {at}"
            );
        }
    }
}

#[cfg(test)]
mod second_down_tests {
    use super::*;

    #[test]
    fn a_second_down_while_the_first_is_alive_is_refused_and_leaves_the_touch_as_it_was() {
        let mut touch = CircleTouch::new(100.);
        assert!(
            touch.down((50., 50.), (200., 300.), 0),
            "the first down was refused"
        );
        touch.seen(100);
        assert!(
            !touch.down((50., 50.), (200., 300.), 150),
            "a second finger down on a live touch restarted it"
        );
        // The hold goes on from the FIRST down: it sends at the confirmation time counted from 0, and only once.
        let mut sent = Vec::new();
        for at in (200..=CONFIRM_MS + 300).step_by(100) {
            touch.seen(at);
            if touch.tick(at, CircleDoes::HoldToSend) == CircleAct::Send {
                sent.push(at);
            }
        }
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(
            sent[0] >= CONFIRM_MS && sent[0] < CONFIRM_MS + 150,
            "{sent:?}"
        );
    }

    #[test]
    fn after_the_touch_ended_the_next_down_is_taken() {
        let mut touch = CircleTouch::new(100.);
        assert!(touch.down((50., 50.), (200., 300.), 0));
        touch.up((50., 50.), (200., 300.), 200, CircleDoes::HoldToSend);
        assert!(
            touch.down((50., 50.), (200., 300.), 300),
            "a down after the lift was refused"
        );
    }

    #[test]
    fn a_down_outside_the_round_is_not_taken_so_it_never_starts_anything() {
        let mut touch = CircleTouch::new(100.);
        assert!(!touch.down((2., 2.), (200., 300.), 0));
        assert!(!touch.is_down());
    }
}

/// Where the circle of Send is in its sequence. One stage at any moment, decided from what the application already knows (the files listed, the code that
/// was made, the transfers of the batch chosen now); the circle keeps no state of its own about it. The stages follow one another: Empty, then Ready
/// when files are chosen, then Going when they are sent, then Ended when every transfer of the batch is over, and Ready again when the person chooses
/// new files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Nothing is listed and there is no batch: the circle asks for files.
    Empty,
    /// `files` are listed and wait for the circle; `aim` says where they would go.
    Ready { files: usize, aim: Aim },
    /// A code is drawn (or scanned and waiting): the circle is the code.
    Code { scanned: bool },
    /// The batch goes: `files` of it, `percent` of the way, `current` the file that is moving now.
    Going {
        files: usize,
        percent: u8,
        current: Option<String>,
    },
    /// Every transfer of the batch is over: how it ended.
    Ended { files: usize, arrived: usize },
}

impl Stage {
    /// How many files this stage is about: what the circle says of the count, from the one place that knows it for the stage.
    pub fn files(&self) -> usize {
        match self {
            Stage::Empty | Stage::Code { .. } => 0,
            Stage::Ready { files, .. }
            | Stage::Going { files, .. }
            | Stage::Ended { files, .. } => *files,
        }
    }
}

/// The stage, from the inputs. The order is the sequence: a code that is shown first, then a batch that goes, then files that are listed (a new
/// choice after a batch ended starts the next lap, whatever ended), then a batch that ended, then nothing.
pub fn stage_of(input: &CircleInputs) -> Stage {
    match &input.code {
        Some(CodeCircle::Code) => return Stage::Code { scanned: false },
        Some(CodeCircle::Scanned) => return Stage::Code { scanned: true },
        _ => {}
    }
    let batch = batch_state(input.batch);
    let code_goes = matches!(
        input.code,
        Some(CodeCircle::WaitingForApproval { .. } | CodeCircle::Sending { .. })
    ) && !matches!(batch, BatchState::Sent { .. } | BatchState::Stopped { .. });
    if let BatchState::Going {
        files,
        percent,
        current,
    } = &batch
    {
        return Stage::Going {
            files: *files,
            percent: *percent,
            current: current.clone(),
        };
    }
    if code_goes {
        let (files, percent) = match input.code {
            Some(CodeCircle::WaitingForApproval { files }) => (files, 0),
            Some(CodeCircle::Sending { files, percent }) => (files, percent),
            _ => (0, 0),
        };
        return Stage::Going {
            files,
            percent,
            current: None,
        };
    }
    if input.listed > 0 {
        return Stage::Ready {
            files: input.listed,
            aim: input.aim.clone(),
        };
    }
    match batch {
        BatchState::Sent { files } => Stage::Ended {
            files,
            arrived: files,
        },
        BatchState::Stopped { files, done } => Stage::Ended {
            files,
            arrived: done,
        },
        // Nothing of the batch is in the record yet, but files were just handed over and the owner has not answered: they are on their way, and the
        // circle says so instead of going back to asking for files. The owner's own transfers, once there, take over from this count.
        BatchState::None | BatchState::Going { .. } if input.handed_over > 0 => Stage::Going {
            files: input.handed_over,
            percent: 0,
            current: None,
        },
        BatchState::None | BatchState::Going { .. } => Stage::Empty,
    }
}

#[cfg(test)]
mod stage_tests {
    use super::*;

    fn one(id: &str, phase: Phase, percent: u8) -> Progress {
        Progress {
            id: id.into(),
            name: format!("{id}.txt"),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn stage(listed: usize, aim: Aim, code: Option<CodeCircle>, batch: &[Progress]) -> Stage {
        stage_of(&CircleInputs {
            listed,
            aim,
            send_instantly: false,
            code,
            batch,
            handed_over: 0,
        })
    }

    #[test]
    fn one_whole_lap_the_circle_goes_empty_ready_going_ended_and_ready_again_each_with_its_own_count()
     {
        let devices = Aim::Devices(vec!["laptop".into()]);
        assert_eq!(stage(0, devices.clone(), None, &[]), Stage::Empty);
        let ready = stage(2, devices.clone(), None, &[]);
        assert_eq!(
            ready,
            Stage::Ready {
                files: 2,
                aim: devices.clone()
            }
        );
        assert_eq!(ready.files(), 2);
        let going = stage(
            0,
            devices.clone(),
            None,
            &[one("a", Phase::Moving, 50), one("b", Phase::Waiting, 0)],
        );
        assert!(matches!(going, Stage::Going { files: 2, .. }), "{going:?}");
        assert_eq!(going.files(), 2);
        let ended = stage(
            0,
            devices.clone(),
            None,
            &[one("a", Phase::Cancelled, 10), one("b", Phase::Done, 100)],
        );
        assert_eq!(
            ended,
            Stage::Ended {
                files: 2,
                arrived: 1
            }
        );
        let again = stage(
            3,
            devices.clone(),
            None,
            &[one("a", Phase::Cancelled, 10), one("b", Phase::Done, 100)],
        );
        assert_eq!(
            again,
            Stage::Ready {
                files: 3,
                aim: devices
            }
        );
        assert_eq!(again.files(), 3);
    }

    #[test]
    fn the_count_is_the_one_of_the_stage_and_never_the_one_of_another_stage() {
        let batch = [one("a", Phase::Done, 100), one("b", Phase::Cancelled, 10)];
        let ready = stage(1, Aim::Code, None, &batch);
        assert_eq!(
            ready.files(),
            1,
            "the circle counted the files of the batch that ended"
        );
        let going = stage(0, Aim::Code, None, &[one("c", Phase::Moving, 20)]);
        assert_eq!(going.files(), 1);
    }

    #[test]
    fn a_cancel_with_nothing_listed_ends_the_lap_and_never_goes_back_to_ready_with_nothing_to_send()
    {
        let ended = stage(
            0,
            Aim::Devices(vec!["x".into()]),
            None,
            &[one("a", Phase::Cancelled, 5)],
        );
        assert_eq!(
            ended,
            Stage::Ended {
                files: 1,
                arrived: 0
            }
        );
        assert!(
            !matches!(ended, Stage::Ready { .. }),
            "a circle with nothing listed was Ready to send"
        );
    }

    #[test]
    fn a_code_is_its_own_stage_and_beats_everything_while_it_is_shown() {
        assert_eq!(
            stage(4, Aim::Code, Some(CodeCircle::Code), &[]),
            Stage::Code { scanned: false }
        );
        assert_eq!(
            stage(4, Aim::Code, Some(CodeCircle::Scanned), &[]),
            Stage::Code { scanned: true }
        );
    }

    #[test]
    fn a_code_still_producing_is_going_even_before_its_transfers_are_in_the_record_and_not_after_they_all_ended()
     {
        let sending = Some(CodeCircle::Sending {
            files: 5,
            percent: 20,
        });
        assert!(matches!(
            stage(0, Aim::Code, sending.clone(), &[]),
            Stage::Going {
                files: 5,
                percent: 20,
                ..
            }
        ));
        let ended = [one("a", Phase::Done, 100), one("b", Phase::Done, 100)];
        assert_eq!(
            stage(0, Aim::Code, sending, &ended),
            Stage::Ended {
                files: 2,
                arrived: 2
            }
        );
    }
}

/// The transfers that are the batch sent now: those the owner named when it took the files (`ids`), and those that came of the code the batch was
/// made behind (`code`, the generation the owner gave it). Taken from the record as it is now, so each carries its own state. Nothing is deduced from what
/// was there before: a transfer is the batch's because the owner said so or because it came of the batch's code, and for no other reason.
pub fn batch_of(transfers: &[Progress], ids: &[String], code: Option<u64>) -> Vec<Progress> {
    transfers
        .iter()
        .filter(|item| {
            !item.incoming && (ids.contains(&item.id) || (code.is_some() && item.code == code))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod batch_of_tests {
    use super::*;

    fn one(id: &str, name: &str, phase: Phase, code: Option<u64>) -> Progress {
        Progress {
            id: id.into(),
            name: name.into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn the_batch_is_exactly_the_transfers_the_owner_named_and_nothing_older_or_newer() {
        let record = vec![
            one("old", "old.txt", Phase::Done, None),
            one("a", "a.txt", Phase::Moving, None),
            one("b", "b.txt", Phase::Cancelled, None),
            one("later", "later.txt", Phase::Moving, None),
        ];
        let batch = batch_of(&record, &["a".into(), "b".into()], None);
        let ids: Vec<&str> = batch.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            ["a", "b"],
            "the batch took a transfer the owner did not name: {ids:?}"
        );
    }

    #[test]
    fn each_transfer_of_the_batch_carries_the_state_it_has_now() {
        let record = vec![one("a", "a.txt", Phase::Cancelled, None)];
        let batch = batch_of(&record, &["a".into()], None);
        assert_eq!(
            batch[0].phase,
            Phase::Cancelled,
            "the batch kept a state from when it was named"
        );
    }

    #[test]
    fn the_transfers_of_the_batchs_code_are_its_own_by_the_generation_and_another_codes_are_not() {
        let record = vec![
            one("a", "a.txt", Phase::Moving, Some(3)),
            one("b", "b.txt", Phase::Moving, Some(4)),
            one("c", "c.txt", Phase::Done, None),
        ];
        let batch = batch_of(&record, &[], Some(3));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, "a");
        assert!(
            batch_of(&record, &[], None).is_empty(),
            "no ids and no code made a batch out of the record"
        );
    }

    #[test]
    fn a_file_received_is_never_part_of_the_batch_that_was_sent() {
        let mut incoming = one("in", "a.txt", Phase::Moving, None);
        incoming.incoming = true;
        assert!(batch_of(&[incoming], &["in".into()], None).is_empty());
    }

    #[test]
    fn a_record_of_thousands_costs_nothing_the_batch_is_only_what_was_named() {
        let record: Vec<Progress> = (0..5_000)
            .map(|n| one(&format!("old-{n}"), "x.bin", Phase::Done, None))
            .collect();
        assert!(batch_of(&record, &[], None).is_empty());
        assert_eq!(batch_of(&record, &["old-7".into()], None).len(), 1);
    }
}

/// The ids of the batch that stay when new files are chosen: only the transfers that still go (moving, or waiting for an answer). What is over (done,
/// cancelled, failed) is the lap that ended and is let go of; what still goes is not forgotten, so the circle and the list never lose a send in flight
/// because the person began choosing the next files.
pub fn ids_kept_on_new_choice(
    transfers: &[Progress],
    ids: &[String],
    code: Option<u64>,
) -> Vec<String> {
    batch_of(transfers, ids, code)
        .into_iter()
        .filter(|item| !item.phase.is_over())
        .map(|item| item.id)
        .collect()
}

#[cfg(test)]
mod new_choice_tests {
    use super::*;

    fn one(id: &str, phase: Phase) -> Progress {
        Progress {
            id: id.into(),
            name: format!("{id}.txt"),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }

    #[test]
    fn choosing_new_files_keeps_what_still_goes_and_lets_go_of_what_is_over() {
        let record = vec![
            one("a", Phase::Moving),
            one("b", Phase::Cancelled),
            one("c", Phase::Done),
            one("d", Phase::Waiting),
        ];
        let kept = ids_kept_on_new_choice(
            &record,
            &["a".into(), "b".into(), "c".into(), "d".into()],
            None,
        );
        assert_eq!(
            kept,
            ["a", "d"],
            "a send in flight was forgotten, or one that was over was kept: {kept:?}"
        );
    }

    #[test]
    fn a_batch_that_is_all_over_is_let_go_of_entirely() {
        let record = vec![
            one("a", Phase::Cancelled),
            one("b", Phase::Failed("x".into())),
        ];
        assert!(ids_kept_on_new_choice(&record, &["a".into(), "b".into()], None).is_empty());
    }
}

#[cfg(test)]
mod handed_over_not_yet_answered_tests {
    use super::*;

    #[test]
    fn between_the_send_and_the_owners_answer_the_circle_is_going_and_never_asks_for_files() {
        // Nothing is listed any more (the files were taken), no transfer is named yet, no code: only the knowledge that N files were handed over.
        let inputs = CircleInputs {
            listed: 0,
            aim: Aim::Code,
            send_instantly: false,
            code: None,
            batch: &[],
            handed_over: 3,
        };
        let stage = stage_of(&inputs);
        assert_eq!(
            stage,
            Stage::Going {
                files: 3,
                percent: 0,
                current: None
            },
            "the circle fell back to asking for files in the gap"
        );
        assert_eq!(
            circle_view(&inputs).does,
            None,
            "a circle in the gap answered a touch"
        );
    }

    #[test]
    fn once_the_transfers_are_named_the_real_batch_takes_over_from_the_count() {
        let moving = Progress {
            id: "a".into(),
            name: "a.txt".into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: "peer".into(),
            percent: 40,
            code: None,
            expires_at: None,
            saved_as: None,
            phase: Phase::Moving,
        };
        let batch = [moving];
        let inputs = CircleInputs {
            listed: 0,
            aim: Aim::Code,
            send_instantly: false,
            code: None,
            batch: &batch,
            handed_over: 3,
        };
        assert!(
            matches!(
                stage_of(&inputs),
                Stage::Going {
                    files: 1,
                    percent: 40,
                    ..
                }
            ),
            "{:?}",
            stage_of(&inputs)
        );
    }

    #[test]
    fn nothing_handed_over_and_nothing_else_is_empty() {
        let inputs = CircleInputs {
            listed: 0,
            aim: Aim::Code,
            send_instantly: false,
            code: None,
            batch: &[],
            handed_over: 0,
        };
        assert_eq!(stage_of(&inputs), Stage::Empty);
    }
}

#[cfg(test)]
mod three_files_two_devices_tests {
    use super::*;

    fn transfer(device: &str, n: usize, phase: Phase, percent: u8) -> Progress {
        Progress {
            id: format!("{device}-{n}"),
            name: format!("f{n}.bin"),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: device.into(),
            percent,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn batch_of_six(phase_of: impl Fn(&str) -> Phase, percent: u8) -> Vec<Progress> {
        ["laptop", "phone"]
            .iter()
            .flat_map(|device| (0..3).map(move |n| (*device, n)))
            .map(|(device, n)| transfer(device, n, phase_of(device), percent))
            .collect()
    }
    fn inputs<'a>(batch: &'a [Progress], handed_over: usize) -> CircleInputs<'a> {
        CircleInputs {
            listed: 0,
            aim: Aim::Devices(vec!["laptop".into(), "phone".into()]),
            send_instantly: false,
            code: None,
            batch,
            handed_over,
        }
    }

    #[test]
    fn the_circle_says_three_files_in_the_gap_while_it_goes_and_when_it_ends_never_six() {
        assert!(matches!(
            stage_of(&inputs(&[], 3)),
            Stage::Going { files: 3, .. }
        ));
        let going = batch_of_six(|_| Phase::Moving, 40);
        let stage = stage_of(&inputs(&going, 0));
        assert!(
            matches!(
                stage,
                Stage::Going {
                    files: 3,
                    percent: 40,
                    ..
                }
            ),
            "the circle counted a file once for each device: {stage:?}"
        );
        let done = batch_of_six(|_| Phase::Done, 100);
        assert_eq!(
            stage_of(&inputs(&done, 0)),
            Stage::Ended {
                files: 3,
                arrived: 3
            }
        );
    }

    #[test]
    fn when_one_device_refused_the_batch_stopped_and_says_how_many_files_arrived_everywhere() {
        let mixed = batch_of_six(
            |device| {
                if device == "laptop" {
                    Phase::Done
                } else {
                    Phase::Cancelled
                }
            },
            100,
        );
        assert_eq!(
            stage_of(&inputs(&mixed, 0)),
            Stage::Ended {
                files: 3,
                arrived: 0
            },
            "a file refused by one device was counted as arrived"
        );
        let devices = batch_by_device(&mixed);
        assert_eq!(devices[0].state(), BatchState::Sent { files: 3 });
        assert_eq!(
            devices[1].state(),
            BatchState::Stopped { files: 3, done: 0 }
        );
    }
}

#[cfg(test)]
mod uneven_devices_tests {
    use super::*;

    fn transfer(device: &str, name: &str) -> Progress {
        Progress {
            id: format!("{device}-{name}"),
            name: name.into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: device.into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase: Phase::Moving,
        }
    }

    #[test]
    fn a_device_that_was_given_fewer_files_does_not_make_the_batch_smaller() {
        // Three files; the laptop got all three, the phone only one (the others were refused before a transfer was made for them).
        let batch = vec![
            transfer("laptop", "a"),
            transfer("laptop", "b"),
            transfer("laptop", "c"),
            transfer("phone", "a"),
        ];
        assert_eq!(
            batch_files(&batch),
            3,
            "the count followed the device that had the fewest transfers or the most"
        );
        // And it does not follow the device with the MOST either: the phone alone had three different files.
        let other = vec![
            transfer("phone", "x"),
            transfer("phone", "y"),
            transfer("laptop", "x"),
        ];
        assert_eq!(batch_files(&other), 2);
    }

    #[test]
    fn two_different_files_with_one_name_and_different_sizes_are_two_files() {
        let mut small = transfer("laptop", "same.bin");
        small.bytes = 10;
        let mut large = transfer("laptop", "same.bin");
        large.id = "other".into();
        large.bytes = 20;
        assert_eq!(batch_files(&[small, large]), 2);
    }
}

/// One row of the list under the circle, as the model orders it: a device's own heading (who, and how that device stands), or a transfer of the batch.
/// The drawing only walks these and draws each; it decides nothing about grouping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchRow {
    /// The heading of one device's part. Only when the batch went to more than one device: with one, the device is not news. `name` is what the device
    /// can be shown by (the name its own transfers carry, or its id when none does).
    Device {
        peer: String,
        name: String,
        state: BatchState,
    },
    /// A transfer of the batch, for the device named just above it when there is one.
    Transfer(Progress),
}

impl DeviceBatch {
    /// What the device is shown by: the name its transfers carry (the device's own word for itself, kept on each), else the id the owner gave it.
    pub fn shown_name(&self) -> String {
        self.transfers
            .iter()
            .find_map(|item| item.from.clone().filter(|name| !name.is_empty()))
            .unwrap_or_else(|| self.peer.clone())
    }
}

/// The rows of the batch in the order the list shows them: with one device, its transfers as [`transfer_list`] has them and no heading; with more than
/// one, a heading for each device (in the order they first appear) followed by that device's own transfers, going first.
pub fn batch_rows(batch: &[Progress]) -> Vec<BatchRow> {
    let devices = batch_by_device(batch);
    if devices.len() < 2 {
        return transfer_list(batch)
            .cloned()
            .map(BatchRow::Transfer)
            .collect();
    }
    let mut rows = Vec::new();
    for device in &devices {
        rows.push(BatchRow::Device {
            peer: device.peer.clone(),
            name: device.shown_name(),
            state: device.state(),
        });
        rows.extend(
            transfer_list(&device.transfers)
                .cloned()
                .map(BatchRow::Transfer),
        );
    }
    rows
}

#[cfg(test)]
mod batch_rows_tests {
    use super::*;

    fn transfer(device: &str, name: &str, phase: Phase) -> Progress {
        Progress {
            id: format!("{device}-{name}"),
            name: name.into(),
            folders: Vec::new(),
            bytes: 100,
            incoming: false,
            from: None,
            peer: device.into(),
            percent: 0,
            code: None,
            expires_at: None,
            saved_as: None,
            phase,
        }
    }
    fn shape(rows: &[BatchRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                BatchRow::Device { name, .. } => format!("[{name}]"),
                BatchRow::Transfer(item) => item.name.clone(),
            })
            .collect()
    }

    #[test]
    fn with_two_devices_each_gets_a_heading_and_its_own_files_together() {
        let batch = vec![
            transfer("laptop", "a", Phase::Done),
            transfer("phone", "a", Phase::Moving),
            transfer("laptop", "b", Phase::Done),
            transfer("phone", "b", Phase::Moving),
        ];
        // Inside each device the order is the list's own everywhere (going first, then what is over, newest first).
        assert_eq!(
            shape(&batch_rows(&batch)),
            ["[laptop]", "b", "a", "[phone]", "a", "b"]
        );
    }

    #[test]
    fn a_heading_says_how_that_device_stands_by_itself() {
        let batch = vec![
            transfer("laptop", "a", Phase::Done),
            transfer("phone", "a", Phase::Cancelled),
        ];
        let rows = batch_rows(&batch);
        assert_eq!(
            rows[0],
            BatchRow::Device {
                peer: "laptop".into(),
                name: "laptop".into(),
                state: BatchState::Sent { files: 1 }
            }
        );
        assert!(
            matches!(&rows[2], BatchRow::Device { peer, state: BatchState::Stopped { files: 1, done: 0 }, .. } if peer == "phone"),
            "{:?}",
            rows[2]
        );
    }

    #[test]
    fn with_one_device_the_rows_are_as_they_always_were_with_no_heading() {
        let batch = vec![
            transfer("laptop", "a", Phase::Done),
            transfer("laptop", "b", Phase::Moving),
        ];
        assert_eq!(shape(&batch_rows(&batch)), ["b", "a"]);
    }

    #[test]
    fn a_heading_shows_the_name_the_device_gave_itself_and_falls_back_to_its_id() {
        let mut named = transfer("id-1", "a", Phase::Done);
        named.from = Some("Alex's laptop".into());
        let batch = vec![named, transfer("id-2", "a", Phase::Done)];
        let rows = batch_rows(&batch);
        assert!(
            matches!(&rows[0], BatchRow::Device { name, .. } if name == "Alex's laptop"),
            "{:?}",
            rows[0]
        );
        assert!(
            matches!(&rows[2], BatchRow::Device { name, .. } if name == "id-2"),
            "no name known, so the id: {:?}",
            rows[2]
        );
    }

    #[test]
    fn every_transfer_is_in_the_rows_exactly_once_and_no_batch_has_no_rows() {
        let batch = vec![
            transfer("laptop", "a", Phase::Done),
            transfer("phone", "a", Phase::Cancelled),
            transfer("tablet", "a", Phase::Moving),
        ];
        let transfers = batch_rows(&batch)
            .into_iter()
            .filter(|row| matches!(row, BatchRow::Transfer(_)))
            .count();
        assert_eq!(transfers, 3);
        assert!(batch_rows(&[]).is_empty());
    }
}

/// The number of a touch on the circle: every finger that goes down is given the next one, and everything that belongs to that finger (its clock,
/// what it sends) carries it and acts only while it is still the number of the touch that is current. A clock of an earlier touch that is still
/// running when the next one begins is therefore of no consequence: it finds that it is not the current touch, and does nothing and ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TouchNumber(u64);

impl TouchNumber {
    /// The touch that comes after this one.
    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }

    /// Whether `claimed` is still the touch that is current: the one a clock was started for.
    pub fn is_current(self, claimed: Self) -> bool {
        self == claimed
    }
}

#[cfg(test)]
mod touch_number_tests {
    use super::*;

    /// What the circle does with a clock: it ticks the touch it finds, but only while the number it started with is still the current one.
    fn tick_as_the_page_does(
        current: TouchNumber,
        touch: &mut CircleTouch,
        claimed: TouchNumber,
        now: u64,
    ) -> Option<CircleAct> {
        if !current.is_current(claimed) {
            return None;
        }
        Some(touch.tick(now, CircleDoes::HoldToSend))
    }

    #[test]
    fn a_clock_of_an_earlier_touch_does_nothing_to_the_touch_that_came_after_it() {
        let mut current = TouchNumber::default();
        let mut touch = CircleTouch::new(100.);
        // First touch, held a while; its clock is still running when the person lifts and touches again at once.
        current = current.next();
        let first = current;
        touch.down((50., 50.), (200., 300.), 0);
        current = current.next();
        touch.cancel();
        touch.down((50., 50.), (200., 300.), 600);
        // The old clock beats at 900 ms and later: it must not fill, send or empty the new touch.
        assert_eq!(
            tick_as_the_page_does(current, &mut touch, first, 900),
            None,
            "a clock of the earlier touch acted on the new one"
        );
        assert_eq!(
            tick_as_the_page_does(current, &mut touch, first, 1_500),
            None
        );
        // The new touch's own clock does act, from its own start (held 400 ms at 1,000 ms: not full yet).
        touch.seen(1_000);
        assert_eq!(
            tick_as_the_page_does(current, &mut touch, current, 1_000),
            Some(CircleAct::Nothing)
        );
    }

    #[test]
    fn each_touch_has_its_own_number_and_only_the_latest_is_current() {
        let one = TouchNumber::default().next();
        let two = one.next();
        assert!(two.is_current(two));
        assert!(!two.is_current(one));
        assert_ne!(one, two);
    }

    #[test]
    fn the_number_wraps_rather_than_failing_after_a_very_long_run() {
        let last = TouchNumber(u64::MAX);
        assert_ne!(
            last,
            last.next(),
            "the number did not move on at the end of its range"
        );
    }
}

/// The files that wait for the circle, as one list the person can change: more can be added, one can be taken away, all can be cleared. Held by
/// path (a desktop selection) and in the same order as shown; the page keeps no rule about it of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Waiting {
    pub(super) files: Vec<std::path::PathBuf>,
}

impl Waiting {
    pub fn new(files: Vec<std::path::PathBuf>) -> Self {
        Self { files }
    }

    pub fn files(&self) -> &[std::path::PathBuf] {
        &self.files
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// More files were chosen: they join the list, after what is there, and a file that is already in it is not listed twice. Returns how many
    /// were really new, for the person to be told how many were added.
    pub fn add(&mut self, more: Vec<std::path::PathBuf>) -> usize {
        let before = self.files.len();
        for path in more {
            if !self.files.contains(&path) {
                self.files.push(path);
            }
        }
        self.files.len() - before
    }

    /// The file at `index` is taken away. An index that is not there changes nothing (the list may have changed between the touch and the press).
    pub fn remove(&mut self, index: usize) -> bool {
        if index < self.files.len() {
            self.files.remove(index);
            true
        } else {
            false
        }
    }

    /// Everything is taken away.
    pub fn clear(&mut self) {
        self.files.clear();
    }
}

#[cfg(test)]
mod waiting_tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<std::path::PathBuf> {
        names
            .iter()
            .map(|name| std::path::PathBuf::from(format!("/files/{name}")))
            .collect()
    }

    #[test]
    fn more_files_join_the_list_after_what_is_there_and_do_not_replace_it() {
        let mut waiting = Waiting::new(paths(&["a", "b"]));
        assert_eq!(waiting.add(paths(&["c", "d"])), 2);
        assert_eq!(waiting.files(), paths(&["a", "b", "c", "d"]).as_slice());
    }

    #[test]
    fn a_file_chosen_again_is_not_listed_twice_and_is_not_counted_as_added() {
        let mut waiting = Waiting::new(paths(&["a", "b"]));
        assert_eq!(
            waiting.add(paths(&["b", "c", "a"])),
            1,
            "a file already in the list was counted as new"
        );
        assert_eq!(waiting.files(), paths(&["a", "b", "c"]).as_slice());
    }

    #[test]
    fn taking_a_file_away_keeps_the_order_of_the_rest_and_a_missing_index_changes_nothing() {
        let mut waiting = Waiting::new(paths(&["a", "b", "c"]));
        assert!(waiting.remove(1));
        assert_eq!(waiting.files(), paths(&["a", "c"]).as_slice());
        assert!(
            !waiting.remove(9),
            "an index that is not there removed something"
        );
        assert_eq!(waiting.files().len(), 2);
    }

    #[test]
    fn taking_the_last_file_away_leaves_nothing_waiting() {
        let mut waiting = Waiting::new(paths(&["only"]));
        assert!(!waiting.is_empty());
        assert!(waiting.remove(0));
        assert!(waiting.is_empty());
    }

    #[test]
    fn clearing_empties_everything_at_once() {
        let mut waiting = Waiting::new(paths(&["a", "b", "c"]));
        waiting.clear();
        assert!(waiting.is_empty());
    }

    #[test]
    fn adding_to_an_empty_list_is_the_first_choice_and_adding_nothing_changes_nothing() {
        let mut waiting = Waiting::default();
        assert_eq!(waiting.add(Vec::new()), 0);
        assert!(waiting.is_empty());
        assert_eq!(waiting.add(paths(&["a"])), 1);
    }
}

#[cfg(test)]
mod every_circle_answers_alike_tests {
    use super::*;
    const MID: (f32, f32) = (50., 50.);

    /// What a lift does on the circle after a touch of `held_ms`, for a circle whose act is `does`.
    fn lift_after(held_ms: u64, does: CircleDoes) -> CircleAct {
        let mut touch = CircleTouch::new(100.);
        touch.down(MID, (200., 300.), 0);
        touch.up(MID, (200., 300.), held_ms, does)
    }

    #[test]
    fn a_short_tap_on_a_press_only_circle_answers_at_the_actuation_point_and_not_later() {
        // The circle of Receive: its one act is its press. It answers a tap as soon as it is a meant touch, with no waiting for anything else.
        assert_eq!(
            lift_after(MIN_TOUCH_MS - 1, CircleDoes::ChooseFiles),
            CircleAct::Nothing,
            "a brush pressed it"
        );
        assert_eq!(
            lift_after(MIN_TOUCH_MS, CircleDoes::ChooseFiles),
            CircleAct::Choose,
            "a meant tap did not press it"
        );
        assert_eq!(
            lift_after(HOLD_MS - 1, CircleDoes::ChooseFiles),
            CircleAct::Choose,
            "a tap just short of a hold did not press it"
        );
    }

    #[test]
    fn a_press_that_lasts_as_long_as_a_hold_is_not_a_tap_for_any_circle_that_only_chooses() {
        assert_eq!(
            lift_after(HOLD_MS, CircleDoes::ChooseFiles),
            CircleAct::Nothing
        );
        assert_eq!(
            lift_after(HOLD_MS + 200, CircleDoes::ChooseFiles),
            CircleAct::Nothing
        );
    }
}
