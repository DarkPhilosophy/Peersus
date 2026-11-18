//! Snapshot-driven notifications: startup history is not a new receipt, progress is not a new offer.
use peersus_core::model::{Transfer, TransferState};
use std::collections::BTreeSet;

#[derive(Default)]
pub struct Tracker {
    initialized: bool,
    /// The offers that waited when the notification was last issued: a changed set is a changed notification, even at the same count,
    /// since its buttons answer one offer and must never answer one that is gone.
    waiting: Option<BTreeSet<String>>,
    completed: BTreeSet<String>,
}

impl Tracker {
    pub fn resend(&mut self) {
        self.waiting = None;
    }

    /// Commit delivery only when the bridge succeeds; an unavailable Activity must not lose receipts. `size` says a number of bytes in
    /// words: it is given and not looked up, so that this file stands alone and can be tried from any crate.
    pub fn update(
        &mut self,
        transfers: &[Transfer],
        size: impl Fn(u64) -> String,
        deliver: impl FnOnce(i32, i32, &str, &str) -> bool,
    ) {
        // The offer with the latest deadline: offers expire after the same time, so it is the one that arrived last. Ties go to the
        // greater id, so that the choice does not depend on the order of the list.
        let offered: Vec<&Transfer> = transfers
            .iter()
            .filter(|transfer| transfer.incoming && transfer.state == TransferState::Offered)
            .collect();
        let waiting: BTreeSet<String> =
            offered.iter().map(|transfer| transfer.id.clone()).collect();
        let newest = offered
            .iter()
            .max_by(|a, b| {
                a.expires_at
                    .cmp(&b.expires_at)
                    .then_with(|| a.id.cmp(&b.id))
            })
            .map_or("", |transfer| transfer.id.as_str());
        // One line for each file that waits: its transfer id, where it goes beneath the folder it is received into with its name, and its size,
        // tab separated. The panel answers for each file by the id; the notification shows the last two. Sorted by place, so the list does not
        // move when the owner publishes the transfers in another order.
        let mut lines: Vec<(String, String)> = offered
            .iter()
            .map(|transfer| {
                let place = transfer
                    .folders
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once(transfer.name.as_str()))
                    .collect::<Vec<_>>()
                    .join("/");
                (
                    place.clone(),
                    format!("{}\t{place}\t{}", transfer.id, size(transfer.bytes)),
                )
            })
            .collect();
        lines.sort();
        let files = lines
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>()
            .join("\n");
        let completed: BTreeSet<_> = transfers
            .iter()
            .filter(|transfer| transfer.incoming && transfer.state == TransferState::Complete)
            .map(|transfer| transfer.id.clone())
            .collect();
        if !self.initialized {
            self.completed.clone_from(&completed);
            self.initialized = true;
        }
        let received = completed.difference(&self.completed).count();
        if self.waiting.as_ref() == Some(&waiting) && received == 0 {
            self.completed = completed;
            return;
        }
        if deliver(
            waiting.len().min(i32::MAX as usize) as i32,
            received.min(i32::MAX as usize) as i32,
            newest,
            &files,
        ) {
            self.completed = completed;
            self.waiting = Some(waiting);
        }
    }
}
