#[path = "../../peersus-ui-android/src/notifications.rs"]
mod notifications;

use peersus_core::model::{PeerId, Transfer, TransferState};

fn incoming(id: &str, state: TransferState) -> Transfer {
    Transfer {
        id: id.into(), peer: PeerId("synthetic-peer".into()), peer_name: None,
        name: "synthetic-file".into(), folders: vec![], bytes: 1, completed_bytes: 0,
        incoming: true, state, started_at: 0, code: None, expires_at: None, saved_as: None,
    }
}

/// A size said as the number of bytes: the tracker is given how to say it, and what it makes of it is not what is tried here.
fn bytes(size: u64) -> String { format!("{size} B") }

#[test]
fn language_refresh_reissues_pending_count_without_replaying_receipts() {
    let mut tracker = notifications::Tracker::default();
    let mut transfers = vec![incoming("old-receipt", TransferState::Complete), incoming("waiting", TransferState::Offered)];
    let mut deliveries = vec![];
    tracker.update(&transfers, bytes, |waiting, completed, _, _| { deliveries.push((waiting, completed)); true });
    assert_eq!(deliveries, [(1, 0)], "history is not a new receipt");
    tracker.update(&transfers, bytes, |_, _, _, _| panic!("unchanged counts must be suppressed"));
    transfers.push(incoming("new-receipt", TransferState::Complete));
    tracker.update(&transfers, bytes, |waiting, completed, _, _| { deliveries.push((waiting, completed)); true });
    assert_eq!(deliveries.last(), Some(&(1, 1)));
    tracker.resend();
    tracker.update(&transfers, bytes, |waiting, completed, _, _| { deliveries.push((waiting, completed)); true });
    assert_eq!(deliveries.last(), Some(&(1, 0)), "a locale change refreshes pending requests, not completed history");
    tracker.update(&transfers, bytes, |_, _, _, _| panic!("a successful refresh is committed"));
}

#[test]
fn a_failed_locale_refresh_is_retried_without_losing_new_receipts() {
    let mut tracker = notifications::Tracker::default();
    tracker.update(&[], bytes, |_, _, _, _| true);
    tracker.resend();
    let transfers = [incoming("new-receipt", TransferState::Complete)];
    tracker.update(&transfers, bytes, |waiting, completed, _, _| { assert_eq!((waiting, completed), (0, 1)); false });
    tracker.update(&transfers, bytes, |waiting, completed, _, _| { assert_eq!((waiting, completed), (0, 1)); true });
    tracker.update(&transfers, bytes, |_, _, _, _| panic!("a delivered receipt must not be repeated"));
}

/// The panel in Java answers for each file by the first field of its line, and shows the other two: this is the shape it reads.
#[test]
fn each_waiting_file_is_a_line_of_its_id_its_place_and_its_size_in_the_order_of_the_places() {
    let mut tracker = notifications::Tracker::default();
    let mut beta = incoming("id-b", TransferState::Offered);
    beta.name = "beta.txt".into();
    beta.bytes = 2048;
    let mut alpha = incoming("id-a", TransferState::Offered);
    alpha.name = "alpha.txt".into();
    alpha.folders = vec!["pictures".into(), "trip".into()];
    alpha.bytes = 10;
    // Listed with the folder first, which is the other way round from the order of the places, and a file that is not waiting is not on
    // the list.
    let transfers = [alpha, beta, incoming("done", TransferState::Complete)];
    let mut seen = String::new();
    tracker.update(&transfers, bytes, |_, _, _, files| { seen = files.to_owned(); true });
    assert_eq!(seen, "id-b\tbeta.txt\t2048 B\nid-a\tpictures/trip/alpha.txt\t10 B");
}
