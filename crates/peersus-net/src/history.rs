//! Bounded transfer metadata; interrupted transfers never reopen as running.
use peersus_core::model::{Transfer, TransferState};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read},
    path::PathBuf,
};

const MAX_RECORDS: usize = 1000;
const MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    transfers: Vec<Transfer>,
}

/// The on-disk shape without cloning every entry solely for serialization.
#[derive(Serialize)]
struct BorrowedDocument<'a> {
    version: u32,
    transfers: &'a [Transfer],
}

pub struct History {
    path: PathBuf,
    transfers: Vec<Transfer>,
}

impl History {
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let mut transfers = match File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_BYTES {
                    return Err(invalid());
                }
                let document: Document = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                if document.version != 1 || document.transfers.len() > MAX_RECORDS {
                    return Err(invalid());
                }
                document.transfers
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        for transfer in &mut transfers {
            // The generation of a files code counts codes made by THIS run of the application and starts again from zero at
            // the next. A record that kept it would be taken, by the first code of the next run, for one of that code's: a code
            // never drawn would open as sent or stopped. What came of which code is only known while the code is.
            transfer.code = None;
            if transfer.state.is_active() {
                transfer.state = TransferState::Failed {
                    reason: "Application stopped before transfer completion".into(),
                };
            }
        }
        Ok(Self { path, transfers })
    }

    pub fn entries(&self) -> &[Transfer] {
        &self.transfers
    }

    /// Persist a full candidate before publishing it to clients.
    pub fn record(&mut self, transfer: Transfer) -> io::Result<()> {
        let mut candidate = self.transfers.clone();
        if let Some(existing) = candidate.iter_mut().find(|item| item.id == transfer.id) {
            *existing = transfer;
        } else {
            if candidate.len() == MAX_RECORDS {
                let removable = candidate
                    .iter()
                    .position(|item| !item.state.is_active())
                    .ok_or_else(|| io::Error::other("Too many active transfers"))?;
                candidate.remove(removable);
            }
            candidate.push(transfer);
        }
        let bytes = serde_json::to_vec(&BorrowedDocument {
            version: 1,
            transfers: &candidate,
        })
        .map_err(|_| invalid())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        crate::atomic::write(&self.path, &bytes)?;
        self.transfers = candidate;
        Ok(())
    }

    /// Forget every transfer that is over (done, cancelled or failed) and keep the ones still waiting or moving: those are still
    /// being worked on and a record of them is what the screen and the owner follow. Persisted before it is published, as for
    /// `record`. Returns how many were forgotten.
    pub fn clear_finished(&mut self) -> io::Result<usize> {
        let kept: Vec<Transfer> = self
            .transfers
            .iter()
            .filter(|item| item.state.is_active())
            .cloned()
            .collect();
        let forgotten = self.transfers.len() - kept.len();
        if forgotten == 0 {
            return Ok(0);
        }
        let bytes = serde_json::to_vec(&BorrowedDocument {
            version: 1,
            transfers: &kept,
        })
        .map_err(|_| invalid())?;
        crate::atomic::write(&self.path, &bytes)?;
        self.transfers = kept;
        Ok(forgotten)
    }

    /// Forget every record that was with `peer`, over or not: its device was forgotten, and nothing of what passed with it is to be
    /// found again, by a screen or by the log that is kept of each device from this list. The caller has stopped every task of that
    /// device first (see `TransferTasks::cancel_peer`), so that none is left to write a last state here and bring a record back; a
    /// row left behind by an exception for "still going" would have no task to end it, and at the next start would open as failed and
    /// seed the log again. Persisted before it is published, as for `record`. Returns how many were forgotten.
    pub fn forget_peer(&mut self, peer: &peersus_core::model::PeerId) -> io::Result<usize> {
        let kept: Vec<Transfer> = self
            .transfers
            .iter()
            .filter(|item| &item.peer != peer)
            .cloned()
            .collect();
        let forgotten = self.transfers.len() - kept.len();
        if forgotten == 0 {
            return Ok(0);
        }
        let bytes = serde_json::to_vec(&BorrowedDocument {
            version: 1,
            transfers: &kept,
        })
        .map_err(|_| invalid())?;
        crate::atomic::write(&self.path, &bytes)?;
        self.transfers = kept;
        Ok(forgotten)
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Transfer history is invalid; original preserved",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_core::model::PeerId;

    fn transfer(state: TransferState) -> Transfer {
        Transfer {
            id: "one".into(),
            peer: PeerId("test-peer".into()),
            peer_name: None,
            name: "example.txt".into(),
            folders: Vec::new(),
            bytes: 10,
            completed_bytes: 0,
            incoming: true,
            state,
            started_at: 1,
            code: None,
            expires_at: None,
            saved_as: None,
        }
    }

    #[test]
    fn interrupted_transfers_reopen_as_failed_and_updates_replace() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let mut history = History::open(&path).unwrap();
        history.record(transfer(TransferState::Offered)).unwrap();
        history.record(transfer(TransferState::Running)).unwrap();
        let reopened = History::open(&path).unwrap();
        assert_eq!(reopened.entries().len(), 1);
        assert!(matches!(
            reopened.entries()[0].state,
            TransferState::Failed { .. }
        ));
        history.record(transfer(TransferState::Complete)).unwrap();
        assert_eq!(
            History::open(path).unwrap().entries()[0].state,
            TransferState::Complete
        );
    }

    #[test]
    fn corrupt_history_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        std::fs::write(&path, b"invalid").unwrap();
        assert!(History::open(&path).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"invalid");
    }

    #[test]
    fn which_code_a_transfer_came_of_is_kept_while_the_run_lasts_and_forgotten_at_the_next() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let mut history = History::open(&path).unwrap();
        history
            .record(Transfer {
                code: Some(1),
                ..transfer(TransferState::Complete)
            })
            .unwrap();
        // Within the run it is there, for the screen that drew that code to find its files by.
        assert_eq!(history.entries()[0].code, Some(1));
        // A new run counts codes from zero again: nothing it loads may claim to be of one of ITS codes.
        let reopened = History::open(&path).unwrap();
        assert_eq!(
            reopened.entries()[0].code,
            None,
            "a record of an earlier run was taken for one of the new run's code"
        );
        // Only that: the rest of the record is as it was.
        assert_eq!(reopened.entries()[0].state, TransferState::Complete);
        assert_eq!(reopened.entries()[0].name, "example.txt");
    }

    #[test]
    fn clearing_forgets_what_is_over_and_keeps_what_is_still_going_and_survives_a_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let mut history = History::open(&path).unwrap();
        let named = |id: &str, state| Transfer {
            id: id.into(),
            ..transfer(state)
        };
        history
            .record(named("done", TransferState::Complete))
            .unwrap();
        history
            .record(named("cancelled", TransferState::Cancelled))
            .unwrap();
        history
            .record(named(
                "failed",
                TransferState::Failed { reason: "x".into() },
            ))
            .unwrap();
        history
            .record(named("waiting", TransferState::Offered))
            .unwrap();
        history
            .record(named("moving", TransferState::Running))
            .unwrap();
        assert_eq!(history.clear_finished().unwrap(), 3);
        let ids: Vec<&str> = history
            .entries()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["waiting", "moving"],
            "what was over stayed, or what was going was forgotten"
        );
        // What was written is what is kept: a reopened history has the same two (as failed, as for any that was interrupted).
        let reopened = History::open(&path).unwrap();
        let ids: Vec<&str> = reopened
            .entries()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, ["waiting", "moving"]);
        // Nothing over, nothing to do: it says so and writes nothing.
        assert_eq!(history.clear_finished().unwrap(), 0);
    }
}
