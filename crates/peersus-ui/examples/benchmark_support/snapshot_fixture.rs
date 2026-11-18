use peersus_core::model::{PeerId, Snapshot, Transfer, TransferState};
use std::collections::BTreeMap;
const MARK: &str = "Synthetic benchmark";
const MAX_PEERS: usize = 64;
const MAX_TRANSFERS: usize = 1024;
/// One synthetic remembered device. Every field is made up: the id is no public key and the
/// name no real device. `index` makes a page of them differ in the ways that matter to what
/// the screens show (online, asleep, away).
fn fixture_peer(index: usize) -> peersus_core::model::Peer {
    const NAMES: [&str; 6] = [
        "Alice's laptop",
        "Office desktop",
        "Living-room TV",
        "Bob's phone",
        "Studio workstation",
        "Travel tablet",
    ];
    peersus_core::model::Peer {
        id: PeerId(format!("synthetic-peer-{index}")),
        name: NAMES[index % NAMES.len()].to_owned(),
        paired_at: 1_700_000_000 + index as u64 * 86_400,
        last_seen: Some(1_700_100_000 + index as u64 * 3_600),
        online: index % 3 != 2,
        sleeping: index % 3 == 1,
        statuses: Vec::new(),
        grants: BTreeMap::new(),
        plugin_values: BTreeMap::new(),
    }
}

/// One synthetic transfer record. The four states cycle so a list of any size shows waiting,
/// moving, done and failed rows; `peer` names a device of the fixture, or the marker when the
/// fixture has none.
fn fixture_transfer(index: usize, peer: &peersus_core::model::Peer) -> Transfer {
    let bytes = 1024_u64 * 1024 * (index as u64 + 1) * 3;
    Transfer {
        id: format!("synthetic-transfer-{index}"),
        peer: peer.id.clone(),
        peer_name: Some(peer.name.clone()),
        name: format!("fixture-file-{:02}.dat", index + 1),
        folders: Vec::new(),
        bytes,
        completed_bytes: match index % 4 {
            // The one still moving shows half done; the rest sit at their end.
            1 => bytes / 2,
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

/// The one snapshot the demo serves and the replay bench times, at the size it was asked for.
///
/// Deterministic: two runs, and the demo and the bench, see exactly the same data. Nothing in
/// it is real — no key, address, path or name of a device that exists — and its own device is
/// marked so the window cannot pass for a live one.
pub fn snapshot_sized(peers: usize, transfers: usize) -> Snapshot {
    let peers: Vec<peersus_core::model::Peer> =
        (0..peers.min(MAX_PEERS)).map(fixture_peer).collect();
    let peer = |index: usize| {
        peers
            .get(index % peers.len().max(1))
            .cloned()
            .unwrap_or_else(|| fixture_peer(0))
    };
    // The transfers are built before the literal: `peers` moves into the snapshot, and the
    // closure that borrows it cannot be called after that move. The size is bounded here and not
    // only by the caller: the exported fixture cannot grow past MAX_PEERS and MAX_TRANSFERS.
    let transfers = (0..transfers.min(MAX_TRANSFERS))
        .map(|index| fixture_transfer(index, &peer(index)))
        .collect();
    Snapshot {
        settings: peersus_core::config::Settings {
            device_name: MARK.to_owned(),
            ..peersus_core::config::Settings::default()
        },
        peers,
        grants: BTreeMap::new(),
        plugins: Vec::new(),
        staged: Vec::new(),
        store: peersus_core::model::StoreState::default(),
        update: peersus_core::model::UpdateState::default(),
        actions: Vec::new(),
        transfers,
        pairing: Vec::new(),
        extension_prompts: Vec::new(),
        notifications: Vec::new(),
        errors: Vec::new(),
        errors_said: 0,
        profile_picture: None,
        peer_pictures: BTreeMap::new(),
        displays: Vec::new(),
        peer_displays: BTreeMap::new(),
        files_code: None,
        link_tests: BTreeMap::new(),
        typed_addresses: BTreeMap::new(),
        listening_port: Some(44_710),
        relay: None,
        relay_probes: BTreeMap::new(),
        page_views: Vec::new(),
        nearby: Default::default(),
        languages: Default::default(),
    }
}
