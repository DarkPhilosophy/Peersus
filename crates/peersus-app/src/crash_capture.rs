//! Crash capture of a Peersus program (the `crash_capture` preference) and the crash reports the Diagnostics page lists.
//!
//! The program never handles a crash itself: `peersus_diagnostics::crash_watch` starts `peersus-diag` beside it, which
//! watches it from outside. The owner keeps one of these for itself; each window keeps its own, since a window is a
//! process of its own that can crash on its own.
use peersus_core::model::{CaptureState, CrashReport, DiagnosticsState};
use peersus_diagnostics::crash_watch::{self, Controller, Status, Unavailable};
use std::path::{Path, PathBuf};
use tokio::sync::watch;

/// The wire form of how capture of one program stands.
pub fn state(status: Status) -> CaptureState {
    match status {
        Status::Off => CaptureState::Off,
        Status::Starting => CaptureState::Starting,
        Status::Watching => CaptureState::Watching,
        Status::Unavailable(Unavailable::System) => CaptureState::NotThisSystem,
        Status::Unavailable(Unavailable::Missing) => CaptureState::Missing,
        Status::Unavailable(Unavailable::NotStarted) => CaptureState::NotStarted,
        Status::Unavailable(Unavailable::Refused) => CaptureState::Refused,
    }
}

/// The crash reports in `folder`, newest first, in their wire form.
pub fn reports(folder: &Path) -> Vec<CrashReport> {
    crash_watch::reports(folder)
        .into_iter()
        .map(|report| CrashReport {
            file: report.file,
            bytes: report.bytes,
            unix_seconds: report.unix_seconds,
        })
        .collect()
}

/// The owner's capture and the reports it last read. Capture starts and stops off the owner's loop; the loop is woken
/// by [`CrashCapture::changed`] to publish what changed.
pub struct CrashCapture {
    controller: Controller,
    changes: watch::Receiver<Status>,
    folder: PathBuf,
    reports: Vec<CrashReport>,
}

impl CrashCapture {
    /// Capture of `program`, writing under `data_directory`; off until [`CrashCapture::set`].
    pub fn new(program: &'static str, data_directory: &Path) -> Self {
        let folder = crash_watch::folder(data_directory);
        let (sender, changes) = watch::channel(Status::Off);
        let controller = Controller::new(program, folder.clone(), move |status| {
            let _ = sender.send(status);
        });
        let reports = reports(&folder);
        Self {
            controller,
            changes,
            folder,
            reports,
        }
    }

    pub fn set(&self, on: bool) {
        self.controller.set(on);
    }

    /// Completes when capture's status changed, and never when it cannot change again.
    pub async fn changed(&mut self) {
        if self.changes.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    /// Reads the folder again.
    pub fn rescan(&mut self) {
        self.reports = reports(&self.folder);
    }

    /// Deletes every crash report and nothing else; returns how many went.
    pub fn delete(&mut self) -> usize {
        let deleted = crash_watch::delete_reports(&self.folder);
        self.rescan();
        deleted
    }

    pub fn state(&self) -> DiagnosticsState {
        DiagnosticsState {
            owner: state(self.controller.status()),
            reports: self.reports.clone(),
            folder: Some(self.folder.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_has_its_own_wire_word() {
        let all = [
            Status::Off,
            Status::Starting,
            Status::Watching,
            Status::Unavailable(Unavailable::System),
            Status::Unavailable(Unavailable::Missing),
            Status::Unavailable(Unavailable::NotStarted),
            Status::Unavailable(Unavailable::Refused),
        ];
        let words: std::collections::BTreeSet<String> = all
            .iter()
            .map(|status| serde_json::to_string(&state(*status)).unwrap())
            .collect();
        assert_eq!(words.len(), all.len());
    }

    #[tokio::test]
    async fn switched_on_without_its_helper_says_so_and_switched_off_it_is_off_again() {
        let data = tempfile::tempdir().unwrap();
        let mut capture = CrashCapture::new("peersus-test", data.path());
        assert_eq!(capture.state().owner, CaptureState::Off);
        capture.set(true);
        // The test binary has no `peersus-diag` beside it: capture starts, then names what is missing.
        let expected = if cfg!(target_os = "linux") {
            CaptureState::Missing
        } else {
            CaptureState::NotThisSystem
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while capture.state().owner != expected {
                capture.changed().await;
            }
        })
        .await
        .expect("the status settles");
        capture.set(false);
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while capture.state().owner != CaptureState::Off {
                capture.changed().await;
            }
        })
        .await
        .expect("capture switches off");
    }

    #[test]
    fn the_reports_are_read_from_the_diagnostics_folder_and_deleting_empties_it_of_them() {
        let data = tempfile::tempdir().unwrap();
        let folder = crash_watch::folder(data.path());
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("crash-peersus-1.dmp"), b"dump").unwrap();
        let mut capture = CrashCapture::new("peersus-test", data.path());
        assert_eq!(capture.state().reports.len(), 1);
        assert_eq!(capture.state().folder.as_deref(), Some(folder.as_path()));
        std::fs::write(folder.join("crash-peersus-ui-2.dmp"), b"dump").unwrap();
        capture.rescan();
        assert_eq!(capture.state().reports.len(), 2);
        assert_eq!(capture.delete(), 2);
        assert!(capture.state().reports.is_empty());
    }
}
