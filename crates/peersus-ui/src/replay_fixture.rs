//! Synthetic transfer records shared by the development replay and the local renderer scene.
use peersus_core::model::{PeerId, Transfer, TransferState};

pub(crate) const REPLAY_FRAMES: usize = 8;

pub(crate) fn transfer(index: usize, peer: &PeerId, name: Option<&str>, step: usize) -> Transfer {
    let bytes = 1024_u64 * 1024 * (index as u64 + 1) * 3;
    Transfer {
        id: format!("synthetic-transfer-{index}"),
        peer: peer.clone(),
        peer_name: name.map(str::to_owned),
        name: format!("fixture-file-{:02}.dat", index + 1),
        folders: Vec::new(),
        bytes,
        completed_bytes: match index % 4 {
            1 => bytes / 2 + bytes / 2 * step as u64 / REPLAY_FRAMES as u64,
            _ => bytes,
        },
        incoming: index % 2 == 1,
        state: match index % 4 {
            0 => TransferState::Complete,
            1 => TransferState::Running,
            2 => TransferState::Failed {
                reason: "Synthetic failure for the demo".to_owned(),
            },
            _ => TransferState::Cancelled,
        },
        started_at: 1_700_050_000 + index as u64 * 600,
        code: None,
        expires_at: None,
        saved_as: None,
    }
}
