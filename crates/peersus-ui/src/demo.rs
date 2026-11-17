//! The read-only demo the `demo` feature builds into the interface: the same window, the same
//! screens and the same `View` the owner feeds, drawn from ONE synthetic snapshot held in
//! memory and from nothing else.
//!
//! What keeps it a demo and never a product run in disguise:
//!
//! * It exists only in a build made with `--features demo`. A build without the feature has
//!   no flag to read and no code to answer with: `peersus-ui --demo` there fails as an unknown
//!   argument before an owner is ever looked for.
//! * Its arguments are judged BEFORE [`crate::link::Session::open`] can run, so a demo run can
//!   never reach the owner's socket or start one, and a flag meant for something else fails
//!   instead of being taken along.
//! * Every command is refused with the same explicit sentence
//!   ([`peersus_app::runtime::DEMO_READ_ONLY`]); nothing writes state, touches the network or
//!   a file, the keyring, the clipboard or a platform dialog.
//! * The snapshot's own device name says what it is, so a window of it cannot be mistaken for
//!   a real one.
//!
//! `--demo-extension=DIR` is the one demo that is not the fixture: an extension shown as it really runs, on owners of the
//! demo's own that live in this process and a thrown-away folder ([`crate::demo_owner`]). It too never reaches the person's
//! owner, settings, keyring or platform services; its commands go to those owners, and nothing is accepted or allowed for the
//! person.
//!
//! The same fixture feeds the `replay_bench` example (through [`snapshot_sized`]), so what the
//! demo shows and what the bench measures are the same data.

use crate::Page;
use crate::view::SettingsTab;
use peersus_core::model::{PeerId, Snapshot, Transfer, TransferState};
use std::collections::BTreeMap;

/// The name the fixture's own device goes by: said of itself, everywhere a device name is
/// drawn, so that nobody can take this window for theirs.
const MARK: &str = "Synthetic Demo (read-only)";

/// How large the fixture's peer list may be asked to grow: enough to fill a screen of devices,
/// never enough to make the demo slow to open. The same bound as the replay bench's.
pub const MAX_PEERS: usize = 64;
/// How large the fixture's transfer list may be asked to grow: enough to fill Transfers,
/// bounded so a mistaken number cannot turn a demo into a stress test.
pub const MAX_TRANSFERS: usize = 1024;

/// How often a replaying demo may be asked to change its snapshot, at most: past a screen's
/// refresh rate a faster replay shows nothing a slower one does not.
pub const MAX_REPLAY_HZ: usize = 120;
/// How many different snapshots a replaying demo goes round.
const REPLAY_FRAMES: usize = 8;

/// What a demo run was told: which screen to open on, how large the fixture is, whether it
/// stands still and whether the run measures itself.
pub(crate) struct Spec {
    pub(crate) page: Option<(Page, SettingsTab)>,
    pub(crate) peers: usize,
    pub(crate) transfers: usize,
    /// How many times a second the snapshot changes; 0 is the still demo.
    pub(crate) replay_hz: usize,
    /// How long the run measures itself for before it reports and ends; `None` is a window
    /// that stays open and measures nothing.
    pub(crate) probe: Option<crate::probe::Run>,
    /// The language file the demo is shown in, read and checked; `None` is English. So that a screen can be opened, drawn and
    /// tested in a language whose words are longest, with no saved preference touched and no owner to ask for the file.
    pub(crate) language: Option<DemoLanguage>,
    /// The folders of unpacked extension packages to show as they really run (`--demo-extension`, once for each): the demo then
    /// starts owners of its own in this process (one, and a paired one), offers each package there as the person's choice, and
    /// the window drives them. Empty is the still fixture with no owner at all.
    pub(crate) extension: Vec<std::path::PathBuf>,
    /// A store link the window is opened with (`--demo-link`), as the desktop entry opens it with one.
    pub(crate) link: Option<String>,
    /// The system's theme as the demo says it (`--demo-theme=dark|light`), so that a picture of a screen is drawn in the theme
    /// it is shown beside; `None` is a system that says nothing, which the window draws light.
    pub(crate) dark: Option<bool>,
}

/// The most extensions one demo shows.
const MAX_EXTENSIONS: usize = 8;

/// A language file a demo is shown in: its language and its text.
pub(crate) struct DemoLanguage {
    pub(crate) language: peersus_core::config::Language,
    pub(crate) name: String,
    /// How many messages it translates, as the owner would count a held file.
    pub(crate) translated: u32,
    pub(crate) text: String,
}

/// Reads the language file at `path` for a demo: at most a language file's size, and one that names its language.
fn demo_language(path: &str) -> Result<DemoLanguage, String> {
    use peersus_api::language::{Catalog, MAX_FILE_BYTES};
    use std::io::Read;
    let refused =
        || format!("--demo-language takes the path of a language file (.po); not {path:?}");
    if path.is_empty() {
        return Err(refused());
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .map_err(|_| refused())?
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|_| refused())?;
    let catalog = Catalog::parse(&text).map_err(|_| refused())?;
    let language = catalog.language().ok_or_else(refused)?;
    Ok(DemoLanguage {
        language,
        name: catalog.name().unwrap_or(language.tag()).to_owned(),
        translated: u32::try_from(catalog.counts().1).unwrap_or(u32::MAX),
        text,
    })
}

/// A bound `--demo-peers` and `--demo-transfers` share: the number must parse and stay under
/// `most`, and the reason names the flag the person wrote.
fn bounded(flag: &str, value: &str, most: usize) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(number) if number <= most => Ok(number),
        _ => Err(format!(
            "{flag} takes a number from 0 to {most}, not {value:?}"
        )),
    }
}

/// Judge the arguments of a demo run. `--demo` itself, or any `--demo-*` argument, is what
/// brought the run here (see [`crate::config_from`]); within them only the known set is
/// allowed, so a typo fails loudly rather than silently doing nothing of what was meant.
///
/// * `--demo` opens the Send screen, with the fixture's own size.
/// * `--demo-page=send|receive|peers|transfers|settings` opens that screen.
/// * `--demo-peers=N`, `--demo-transfers=N` size the fixture, bounded.
/// * `--demo-replay=HZ` changes the snapshot that many times a second (see [`replay`]).
/// * `--demo-probe=SECONDS` measures the window for that long after its first frame, writes
///   the report (see [`crate::probe`]) and ends; `--demo-probe-out=PATH` names the file the
///   report goes to instead of standard output, and means nothing without `--demo-probe`.
/// * `--demo-language=FILE` shows the demo in the language of that language file (`languages/<release>/ro.po`, …).
/// * `--demo-extension=DIR` shows the extension whose unpacked package is `DIR` as it really runs, its pages, settings and
///   permissions, on an owner of the demo's own in a thrown-away folder (see [`crate::demo_owner`]). The fixture's sizes and
///   replay mean nothing then and are refused with it.
///
/// `Err` carries the sentence the program prints; nothing is started or touched.
pub(crate) fn parse(args: &[String]) -> Result<Spec, String> {
    let mut spec = Spec {
        page: None,
        peers: 3,
        transfers: 4,
        replay_hz: 0,
        probe: None,
        language: None,
        extension: Vec::new(),
        link: None,
        dark: None,
    };
    let mut seconds = None;
    let mut out = None;
    for argument in args {
        if argument == "--demo" {
            continue;
        }
        if let Some(name) = argument.strip_prefix("--demo-page=") {
            spec.page = Some(match name {
                "send" => (Page::Send, SettingsTab::default()),
                "receive" => (Page::Receive, SettingsTab::default()),
                "peers" => (Page::Peers, SettingsTab::default()),
                "transfers" => (Page::Transfers, SettingsTab::default()),
                "settings" => (Page::Settings, SettingsTab::default()),
                "extensions" => (Page::Extensions, SettingsTab::default()),
                other => {
                    return Err(format!(
                        "--demo-page takes one of send, receive, peers, transfers, settings, \
                         extensions; not {other:?}"
                    ));
                }
            });
        } else if let Some(value) = argument.strip_prefix("--demo-peers=") {
            spec.peers = bounded("--demo-peers", value, MAX_PEERS)?;
        } else if let Some(value) = argument.strip_prefix("--demo-transfers=") {
            spec.transfers = bounded("--demo-transfers", value, MAX_TRANSFERS)?;
        } else if let Some(value) = argument.strip_prefix("--demo-replay=") {
            spec.replay_hz = bounded("--demo-replay", value, MAX_REPLAY_HZ)?;
        } else if let Some(value) = argument.strip_prefix("--demo-probe=") {
            seconds = Some(
                match bounded("--demo-probe", value, crate::probe::MAX_SECONDS)? {
                    0 => return Err("--demo-probe takes at least 1 second".to_owned()),
                    seconds => seconds as u64,
                },
            );
        } else if let Some(path) = argument.strip_prefix("--demo-language=") {
            spec.language = Some(demo_language(path)?);
        } else if let Some(path) = argument.strip_prefix("--demo-extension=") {
            let folder = std::path::PathBuf::from(path);
            if !folder.join("plugin.json").is_file() && !folder.join("plugin.toml").is_file() {
                return Err(format!(
                    "--demo-extension takes the folder of an unpacked extension package (with its plugin.json); not {path:?}"
                ));
            }
            if spec.extension.len() == MAX_EXTENSIONS {
                return Err(format!(
                    "--demo-extension is given at most {MAX_EXTENSIONS} times"
                ));
            }
            spec.extension.push(folder);
        } else if let Some(theme) = argument.strip_prefix("--demo-theme=") {
            spec.dark = Some(match theme {
                "dark" => true,
                "light" => false,
                _ => return Err(format!("--demo-theme takes dark or light; not {theme:?}")),
            });
        } else if let Some(link) = argument.strip_prefix("--demo-link=") {
            if peersus_core::model::store_link_plugin(link).is_none() {
                return Err(format!(
                    "--demo-link takes a store link, peersus://store/<id>; not {link:?}"
                ));
            }
            spec.link = Some(link.to_owned());
        } else if let Some(path) = argument.strip_prefix("--demo-probe-out=") {
            if path.is_empty() {
                return Err("--demo-probe-out takes the path of the report".to_owned());
            }
            out = Some(std::path::PathBuf::from(path));
        } else {
            return Err(format!(
                "Unknown argument {argument:?}: the demo takes only --demo, --demo-page, \
                 --demo-peers, --demo-transfers, --demo-language, --demo-extension, --demo-link, \
                 --demo-theme, --demo-replay, --demo-probe and --demo-probe-out"
            ));
        }
    }
    spec.probe = match (seconds, out) {
        (Some(seconds), out) => Some(crate::probe::Run {
            seconds,
            out,
            peers: spec.peers,
            transfers: spec.transfers,
            replay_hz: spec.replay_hz,
        }),
        (None, Some(_)) => {
            return Err("--demo-probe-out names a report that only --demo-probe writes".to_owned());
        }
        (None, None) => None,
    };
    if !spec.extension.is_empty() && (spec.replay_hz != 0 || spec.probe.is_some()) {
        return Err(
            "--demo-extension shows a real extension on an owner of its own: --demo-replay and --demo-probe measure the fixture"
                .to_owned(),
        );
    }
    Ok(spec)
}

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
/// fixture has none. `step` is how far round a replay the record is (0 in the still demo): the
/// transfers that move have moved that much further.
fn fixture_transfer(index: usize, peer: &peersus_core::model::Peer, step: usize) -> Transfer {
    let bytes = 1024_u64 * 1024 * (index as u64 + 1) * 3;
    Transfer {
        id: format!("synthetic-transfer-{index}"),
        peer: peer.id.clone(),
        peer_name: Some(peer.name.clone()),
        name: format!("fixture-file-{:02}.dat", index + 1),
        folders: Vec::new(),
        bytes,
        completed_bytes: match index % 4 {
            // The one still moving shows half done, and goes on from there round a replay;
            // the rest sit at their end.
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

/// The fixture's extensions: two installed and one only listed, so that every card of Settings → Extensions is drawn (the
/// one that is up to date, the one with an update waiting, the one not installed), with every control each of them has.
/// As made up as the rest: no such extension exists, and the link is the reserved example address.
fn fixture_extensions() -> (
    Vec<peersus_core::model::PluginInfo>,
    peersus_core::model::StoreState,
) {
    use peersus_core::model::{
        PluginInfo, PluginStatus, Standing, StoreListing, StoreState, StoreStatus,
    };
    let id = |name: &str| peersus_core::plugins::PluginId::new(name).ok();
    let listing = |name: &str, installed: Option<&str>| {
        Some(StoreListing {
            plugin: id(name)?,
            version: "1.2.0".to_owned(),
            author: "Synthetic authors".to_owned(),
            link: "https://example.invalid/extension".to_owned(),
            description: "A made-up extension of the demo: it does nothing and cannot be run."
                .to_owned(),
            bytes: 71_253,
            installed_version: installed.map(str::to_owned),
        })
    };
    let installed = |name: &str, status| {
        Some(PluginInfo {
            id: id(name)?,
            manifest: Default::default(),
            standing: Standing::Ready,
            status,
            grantable: Default::default(),
            program: None,
            package: None,
            agreed: false,
            from_store: false,
        })
    };
    (
        [
            installed("synthetic-notes", PluginStatus::Running),
            installed("synthetic-gallery", PluginStatus::Stopped),
        ]
        .into_iter()
        .flatten()
        .collect(),
        StoreState {
            status: StoreStatus::Ready,
            listings: [
                listing("synthetic-notes", Some("1.2.0")),
                listing("synthetic-gallery", Some("1.1.0")),
                listing("synthetic-maps", None),
            ]
            .into_iter()
            .flatten()
            .collect(),
            error: None,
            asked: None,
        },
    )
}

/// The one snapshot the demo serves and the replay bench times, at the size it was asked for.
///
/// Deterministic: two runs, and the demo and the bench, see exactly the same data. Nothing in
/// it is real — no key, address, path or name of a device that exists — and its own device is
/// marked so the window cannot pass for a live one.
pub fn snapshot_sized(peers: usize, transfers: usize) -> Snapshot {
    snapshot_at(peers, transfers, 0)
}

/// The snapshots a replaying demo goes round, in order, the first being [`snapshot_sized`]:
/// the same fixture with its moving transfers further on in each. What changes is what
/// changes most often in a real window (progress), and it is as deterministic as the fixture.
pub fn replay(peers: usize, transfers: usize) -> Vec<Snapshot> {
    (0..REPLAY_FRAMES)
        .map(|step| snapshot_at(peers, transfers, step))
        .collect()
}

/// The snapshots a demo run shows, in order: [`replay`] of its fixture, in the language it was asked for.
pub(crate) fn frames(spec: &Spec) -> Vec<Snapshot> {
    let mut frames = replay(spec.peers, spec.transfers);
    if let Some(demo) = &spec.language {
        let held = peersus_core::model::HeldLanguage {
            file: peersus_core::model::LanguageFile {
                language: demo.language,
                name: demo.name.clone(),
                translated: demo.translated,
                bytes: 0,
                machine: false,
            },
            release: None,
        };
        for frame in &mut frames {
            frame.settings.language = Some(demo.language);
            frame.languages.shown = demo.language;
            frame.languages.shown_file = Some(held.clone());
            frame.languages.held = vec![held.clone()];
        }
    }
    frames
}

fn snapshot_at(peers: usize, transfers: usize, step: usize) -> Snapshot {
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
        .map(|index| fixture_transfer(index, &peer(index), step))
        .collect();
    let (plugins, store) = fixture_extensions();
    Snapshot {
        settings: peersus_core::config::Settings {
            device_name: MARK.to_owned(),
            ..peersus_core::config::Settings::default()
        },
        peers,
        grants: BTreeMap::new(),
        plugins,
        staged: Vec::new(),
        store,
        update: Default::default(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::Session;
    use peersus_app::runtime::DEMO_READ_ONLY;
    use peersus_core::model::Command;
    use std::time::Duration;

    #[test]
    fn the_demo_arguments_and_only_theirs_are_accepted() {
        assert!(parse(&["--demo".to_owned()]).is_ok());
        for page in ["send", "receive", "peers", "transfers", "settings"] {
            assert!(
                parse(&[format!("--demo-page={page}")]).is_ok(),
                "--demo-page={page}"
            );
        }
        assert!(
            parse(&[
                "--demo".to_owned(),
                "--demo-peers=8".to_owned(),
                "--demo-transfers=64".to_owned(),
            ])
            .is_ok()
        );
        for bad in [
            "--demo-page=menu",
            "--demo-page=",
            "--demo-peers=x",
            "--demo-peers=9999",
            "--demo-transfers=-1",
            "--verbose",
            "--demo-page",
            "--demo-replay=121",
            "--demo-probe=0",
            "--demo-probe=601",
            "--demo-probe-out=",
            "--demo-language=",
            "--demo-language=no-such-file.po",
            "--demo-extension=",
            "--demo-extension=no-such-folder",
            // A report nothing would write.
            "--demo-probe-out=report.json",
        ] {
            assert!(parse(&[bad.to_owned()]).is_err(), "{bad} must be refused");
        }
        let measured = parse(&[
            "--demo-probe=5".to_owned(),
            "--demo-probe-out=report.json".to_owned(),
            "--demo-replay=30".to_owned(),
        ])
        .ok()
        .and_then(|spec| spec.probe);
        assert!(measured.is_some_and(|run| run.seconds == 5 && run.replay_hz == 30));
        // An extension is shown from the folder of its unpacked package, and never with what measures the fixture.
        let package = tempfile::tempdir().unwrap();
        std::fs::write(package.path().join("plugin.json"), "{}").unwrap();
        let folder = format!("--demo-extension={}", package.path().display());
        let spec = parse(std::slice::from_ref(&folder)).unwrap();
        assert_eq!(spec.extension, [package.path()]);
        // Several are shown side by side, each named once, and only so many.
        let twice = parse(&[folder.clone(), folder.clone()]).unwrap();
        assert_eq!(twice.extension.len(), 2);
        assert!(parse(&vec![folder.clone(); MAX_EXTENSIONS + 1]).is_err());
        for with in ["--demo-replay=30", "--demo-probe=5"] {
            assert!(parse(&[folder.clone(), with.to_owned()]).is_err(), "{with}");
        }
    }

    #[test]
    fn a_replay_starts_at_the_fixture_and_every_frame_of_it_differs() {
        let frames = replay(3, 4);
        assert_eq!(frames.first(), Some(&snapshot_sized(3, 4)));
        for (index, frame) in frames.iter().enumerate() {
            assert!(
                frames.iter().skip(index + 1).all(|later| later != frame),
                "frame {index} is shown twice in one round"
            );
        }
    }

    #[test]
    fn the_fixture_is_marked_valid_and_round_trips_the_wire_encoding() {
        let fixture = snapshot_sized(3, 4);
        // The marker is how nobody takes the window for a real device.
        assert_eq!(fixture.settings.device_name, MARK);
        assert!(
            fixture
                .peers
                .iter()
                .all(|peer| peer.id.0.starts_with("synthetic-peer-"))
        );
        // The same frame the owner/window socket carries: length-prefixed JSON.
        let bytes = serde_json::to_vec(&fixture).expect("encode");
        let back: Snapshot = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(back, fixture);
        // A real view comes of it, with the data the pages would show.
        let view = crate::view::View::of(&fixture);
        assert_eq!(view.devices.len(), 3);
        assert_eq!(view.transfers.len(), 4);
        assert!(!view.needs_a_name(), "the demo must not open on Welcome");
    }

    #[test]
    fn refresh_reports_unchanged_as_false_and_a_real_change_as_true() {
        let fixture = snapshot_sized(3, 4);
        let mut view = crate::view::View::of(&fixture);
        assert!(!crate::view::refresh(&mut view, &fixture));
        let mut moved = fixture.clone();
        moved.errors.push("Something changed".to_owned());
        assert!(crate::view::refresh(&mut view, &moved));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_demo_client_shows_one_snapshot_and_refuses_every_command() {
        let spec = Spec {
            page: Some((Page::Peers, SettingsTab::default())),
            peers: 2,
            transfers: 3,
            replay_hz: 0,
            probe: None,
            language: None,
            extension: Vec::new(),
            link: None,
            dark: None,
        };
        let session = Session::demo(&spec).expect("demo session");
        assert!(session.start == spec.page);
        let demo_link = session.link.as_ref().expect("demo link");
        assert!(
            demo_link.read_only(),
            "the demo's link is the read-only one"
        );
        // No platform bridge exists: nothing can reach a dialog, a camera or a platform setting.
        assert!(session.platform.file_picker.is_none());
        assert!(session.platform.camera.is_none());
        assert!(session.platform.service_settings.is_empty());
        let link = session.link.as_ref().expect("demo link");
        let client = link.client();
        assert_eq!(client.snapshot().settings.device_name, MARK);
        assert_eq!(client.snapshot().peers.len(), 2);
        // Commands do not silently do nothing: each is refused, in words.
        for command in [
            Command::ClearHistory {},
            Command::CreateInvitation {},
            Command::TestLink {
                peer: PeerId("synthetic-peer-0".to_owned()),
            },
        ] {
            assert_eq!(client.command(command).await.err(), Some(DEMO_READ_ONLY));
        }
        // The one snapshot is all there will ever be: a subscriber waits for a change that
        // never comes rather than being told the owner went away.
        let mut updates = client.subscribe();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), updates.changed())
                .await
                .is_err()
        );
        // The session's executor may only be dropped off an async context.
        tokio::task::spawn_blocking(move || drop(session))
            .await
            .expect("session dropped");
    }
}
