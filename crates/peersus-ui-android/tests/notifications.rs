#[path = "../src/notifications.rs"]
mod notifications;
use notifications::Tracker;
use peersus_core::model::{PeerId, Transfer, TransferState};

fn transfer(id: &str, state: TransferState, incoming: bool) -> Transfer {
    Transfer {
        id: id.into(),
        peer: PeerId("test-peer".into()),
        peer_name: None,
        name: "test.bin".into(),
        folders: Vec::new(),
        bytes: 10,
        completed_bytes: 0,
        incoming,
        state,
        started_at: 0,
        code: None,
        expires_at: None,
        saved_as: None,
    }
}

fn size(bytes: u64) -> String {
    format!("{bytes} bytes")
}
#[test]
fn history_is_not_reannounced_and_progress_does_not_duplicate_requests() {
    let mut tracker = Tracker::default();
    let mut files = vec![
        transfer("old", TransferState::Complete, true),
        transfer("pending", TransferState::Offered, true),
    ];
    tracker.update(&files, size, |waiting, received, newest, offered| {
        assert_eq!((waiting, received), (1, 0));
        assert_eq!(newest, "pending");
        assert_eq!(offered, "pending\ttest.bin\t10 bytes");
        true
    });
    tracker.update(&files, size, |_, _, _, _| {
        panic!("unchanged offers must not create notifications")
    });
    files[1].state = TransferState::Running;
    tracker.update(&files, size, |waiting, received, newest, offered| {
        assert_eq!((waiting, received), (0, 0));
        assert!(newest.is_empty() && offered.is_empty());
        true
    });
    files[1].state = TransferState::Complete;
    tracker.update(&files, size, |waiting, received, _, _| {
        assert_eq!((waiting, received), (0, 1));
        true
    });
    tracker.update(&files, size, |_, _, _, _| {
        panic!("completed receipt must be announced only once")
    });
}
#[test]
fn unavailable_activity_does_not_lose_a_new_receipt_or_pending_offer() {
    let mut tracker = Tracker::default();
    tracker.update(&[], size, |_, _, _, _| false);
    let files = vec![
        transfer("new", TransferState::Complete, true),
        transfer("offer", TransferState::Offered, true),
    ];
    tracker.update(&files, size, |waiting, received, _, _| {
        assert_eq!((waiting, received), (1, 1));
        false
    });
    tracker.update(&files, size, |waiting, received, newest, offered| {
        assert_eq!((waiting, received), (1, 1));
        assert_eq!(newest, "offer");
        assert_eq!(offered, "offer\ttest.bin\t10 bytes");
        true
    });
    tracker.resend();
    tracker.update(&files, size, |waiting, received, _, _| {
        assert_eq!((waiting, received), (1, 0));
        true
    });
}
#[test]
fn outgoing_cancelled_and_failed_files_are_not_incoming_notifications() {
    let mut tracker = Tracker::default();
    tracker.update(&[], size, |_, _, _, _| true);
    let files = vec![
        transfer("outgoing", TransferState::Complete, false),
        transfer("cancelled", TransferState::Cancelled, true),
        transfer(
            "failed",
            TransferState::Failed {
                reason: "test".into(),
            },
            true,
        ),
    ];
    tracker.update(&files, size, |_, _, _, _| {
        panic!("no incoming receipt or offer")
    });
}
