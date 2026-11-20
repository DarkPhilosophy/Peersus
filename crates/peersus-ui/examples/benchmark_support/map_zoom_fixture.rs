//! The map benchmark's isolated owner. Real runtime, IPC and supervised Wasm; only the
//! platform's location fix and public tile bytes are fixtures. Nothing is installed.
use super::{AnyResult, Span, now_ns};
use peersus_api::{
    contributions::Scope, elements::Node, manifest::Manifest, message::Outcome,
    permission::Permission,
};
use peersus_app::{
    ipc::{self, FrameKey, FramePoll, FrameReader},
    lifetime::Presence,
    platform_services::{self, Call, PlatformServices, Response},
    plugin_host::{PluginSpec, Registry},
    runtime::Runtime,
};
use peersus_core::{
    Settings,
    config::{NetworkSettings, Theme},
    model::{Command, Snapshot},
    plugins::{Grants, PluginId, Value, Values},
};
use peersus_net::{
    grants::StandaloneGrants, history::History, network_host::NetworkHost, peers::Peers,
    settings::SettingsStore, transport,
};
use serde_json::{Value as Json, json};
use std::{
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(super) const PLUGIN: &str = "location-map-bench";
// The guest adds the full-window copy of `where` under the id of its navigation option.
pub(super) const PAGE: &str = "location-page";
pub(super) const SOCKET: &str = "owner.sock";
const ROADS: usize = 256;
const INDEX: &[u8] =
    br#"{"tiles":["https://tiles.openfreemap.org/planet/fixture/{z}/{x}/{y}.pbf"],"maxzoom":14}"#;

struct Platform {
    root: PathBuf,
    span: Span,
    tile: Vec<u8>,
}

impl PlatformServices for Platform {
    fn public_read_directory(&self) -> Option<&Path> {
        Some(&self.root)
    }

    fn dispatch(&self, call: Call) -> Response {
        let answer = match (call.service.as_str(), call.method.as_str()) {
            (platform_services::location::SERVICE, platform_services::location::CURRENT) => {
                // Bucharest has bundled street data; the regional focus is nearby but outside it.
                let longitude = if self.span == Span::Street {
                    26.1025
                } else {
                    25.0
                };
                Some(json!({"longitude":longitude,"latitude":44.4268,"accuracy":10,"age":0}))
            }
            (platform_services::HTTP_SERVICE, method) if self.span == Span::Street => {
                self.public_read(method, &call.args)
            }
            _ => None,
        };
        Box::pin(async move {
            answer.map_or_else(
                || Outcome::error("unavailable", "The benchmark provides no such facility."),
                Outcome::ok,
            )
        })
    }

    fn revoke(&self, _: &PluginId) {}
}

impl Platform {
    fn public_read(&self, method: &str, args: &Json) -> Option<Json> {
        Some(match method {
            "restore-read" => json!({"authorization":"fixture"}),
            "get-resource" => {
                let path = args["path"].as_str()?;
                let (id, bytes) = if path == "/planet" {
                    ("index", INDEX.len())
                } else if path.starts_with("/planet/fixture/") && path.ends_with(".pbf") {
                    ("tile", self.tile.len())
                } else {
                    return None;
                };
                json!({"resource":id,"bytes":bytes,"age":0})
            }
            "read-resource" => {
                let body = match args["resource"].as_str()? {
                    "index" => INDEX,
                    "tile" => &self.tile,
                    _ => return None,
                };
                let start = usize::try_from(args["offset"].as_u64()?).ok()?;
                let tail = body.get(start..)?;
                let chunk = &tail[..tail.len().min(16 * 1024)];
                let mut hex = String::with_capacity(2 * chunk.len());
                for byte in chunk {
                    write!(hex, "{byte:02x}").ok()?;
                }
                json!({"hex":hex,"end":chunk.len()==tail.len()})
            }
            "release-read" => json!({}),
            "resource-cache" => json!({"resources":0,"bytes":0}),
            _ => return None,
        })
    }
}

/// A local Mapbox vector tile: primary roads with eight vertices each, of the same order as
/// the runtime's offline online-map tests. The guest decodes, styles and keeps them itself.
fn road_tile() -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut value: usize) {
        while value > 127 {
            out.push((value as u8 & 127) | 128);
            value >>= 7;
        }
        out.push(value as u8);
    }
    fn bytes(out: &mut Vec<u8>, tag: u8, data: &[u8]) {
        out.push(tag << 3 | 2);
        varint(out, data.len());
        out.extend_from_slice(data);
    }
    let mut layer = Vec::new();
    bytes(&mut layer, 1, b"transportation");
    for road in 1..=ROADS {
        let mut geometry = vec![9, 0]; // MoveTo one point.
        varint(&mut geometry, road * 7_900 / ROADS);
        geometry.push(7 << 3 | 2); // LineTo seven more (zigzag delta pairs).
        for segment in 0..7 {
            varint(&mut geometry, 1_140);
            geometry.push(if (road + segment) % 2 == 0 { 12 } else { 11 });
        }
        let mut feature = vec![18, 2, 0, 0, 24, 2]; // Tags class=primary, type line.
        bytes(&mut feature, 4, &geometry);
        bytes(&mut layer, 2, &feature);
    }
    bytes(&mut layer, 3, b"class");
    bytes(&mut layer, 4, b"\x0a\x07primary");
    layer.extend_from_slice(b"\x28\x80\x20\x78\x02"); // Extent 4096, MVT version 2.
    let mut tile = Vec::new();
    bytes(&mut tile, 3, &layer);
    tile
}

pub(super) fn visit(node: &Node, f: &mut impl FnMut(&Node)) {
    f(node);
    match node {
        Node::Box { children, .. } | Node::Row { children } | Node::Column { children } => {
            for child in children {
                visit(child, f);
            }
        }
        Node::Interval { child, .. } => visit(child, f),
        _ => {}
    }
}

pub(super) fn page(snapshot: &Snapshot) -> Option<&Node> {
    snapshot
        .page_views
        .iter()
        .find(|page| page.plugin.as_str() == PLUGIN && page.page == PAGE)
        .map(|page| &page.view)
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
pub(super) struct Progress {
    pub answers: Vec<u64>,
    pub frames: Vec<u64>,
}

/// The child is started only in a fresh, parent-owned directory. EOF on its stdin shuts
/// down the owner (and its supervised guest) even if the window never comes up.
pub(super) async fn owner(
    root: &Path,
    span: Span,
    manifest: &Path,
    module: &Path,
    worker: &Path,
) -> AnyResult<()> {
    let listener = tokio::net::UnixListener::bind(root.join(SOCKET))?;
    let plugin = PluginId::new(PLUGIN)?;
    let package = peersus_api::package::split(&fs::read(manifest)?)
        .map_err(|error| format!("invalid Location package metadata: {error:?}"))?;
    let manifest: Manifest = serde_json::from_slice(&package.manifest)?;
    let mut values = Values::default();
    values.set(
        &manifest.contributions.settings,
        Scope::Global,
        PAGE,
        Value::Toggle(true),
    )?;
    let resource = manifest
        .contributions
        .settings
        .iter()
        .find(|setting| setting.id == "online-streets")
        .and_then(|setting| setting.public_read.clone());
    let mut grants = StandaloneGrants::open(root.join("grants.json"))?;
    // Synthetic local fix only. The benchmark never invokes the OS location provider.
    grants.set(
        plugin.clone(),
        Grants::new([
            Permission::new("host", platform_services::location::CAPABILITY)
                .ok_or("invalid location permission")?,
        ])?,
    )?;
    // The window only shows agreed extensions. Represent that state in the disposable
    // fixture, without installing or accepting anything in the user's application.
    let agreements = manifest
        .agreements
        .as_ref()
        .ok_or("missing Location agreement")?;
    peersus_app::plugin_agreements::accept(
        root,
        PLUGIN,
        &peersus_core::agreements::digest(agreements),
        1,
    )?;
    let mut registry = Registry::default();
    registry.register(PluginSpec {
        id: plugin.clone(),
        manifest,
        program: Some(module.to_owned()),
        arguments: Vec::new(),
        restart_policy: None,
        languages: Arc::new([]),
    })?;
    let settings = Settings {
        network: NetworkSettings {
            bind: Some(std::net::Ipv4Addr::LOCALHOST.into()),
            allow_relay: false,
            ..NetworkSettings::default()
        },
        device_name: "Map benchmark device".into(),
        theme: Theme::Dark,
        reduced_motion: true,
        plugins: [(plugin.clone(), values)].into(),
        ..Settings::default()
    };
    let endpoint = transport::bind(iroh::SecretKey::generate(), &settings.network).await?;
    let mut store = SettingsStore::open(root.join("preferences.json"))?;
    store.replace(settings.clone())?;
    let network = NetworkHost::new(
        endpoint,
        settings.device_name,
        Peers::open(root.join("peers.json"))?,
        History::open(root.join("history.json"))?,
    );
    let sockets = root.join("worker");
    fs::create_dir(&sockets)?;
    let (owner, client) = Runtime::new(network, grants, store, registry, sockets)?;
    let launcher = peersus_plugin::module::ModuleLauncher::new(worker.into(), module.into())?;
    let owner = owner
        .with_launcher(plugin.clone(), Arc::new(launcher))
        .with_platform_services(Arc::new(Platform {
            root: root.to_owned(),
            span,
            tile: road_tile(),
        }))
        .with_packages(peersus_app::plugin_manager::Manager::new(root));
    let running = tokio::spawn(owner.run());
    if span == Span::Street {
        client
            .command(Command::SetPublicReadSetting {
                plugin,
                setting: "online-streets".into(),
                resource: resource.ok_or("missing Location public-read setting")?,
                enabled: true,
            })
            .await?;
    }
    let progress = Arc::new(Mutex::new(Progress::default()));
    let watch_progress = Arc::clone(&progress);
    let frame_key = FrameKey {
        plugin: PluginId::new(PLUGIN)?,
        page: PAGE.into(),
        canvas: "map".into(),
    };
    let mut state = client.subscribe();
    let frame_client = client.clone();
    let observer = tokio::spawn(async move {
        if state
            .wait_for(|snapshot| {
                page(snapshot).is_some_and(|page| {
                    let mut found = false;
                    visit(page, &mut |node| {
                        found |= matches!(node, Node::Canvas { id, .. } if id == "map")
                    });
                    found
                })
            })
            .await
            .is_err()
        {
            return;
        }
        let Some(mut reader) = FrameReader::new(&frame_client, frame_key) else {
            return;
        };
        let mut last = 0;
        while let Ok(poll) = reader.next().await {
            match poll {
                FramePoll::Frame(frame) => {
                    let stamp = now_ns();
                    let mut progress = watch_progress.lock().unwrap();
                    progress.frames.push(stamp);
                    if frame.settled > last {
                        last = frame.settled;
                        progress.answers.push(stamp);
                    }
                }
                FramePoll::Empty | FramePoll::Dropped => {}
                FramePoll::Closed | FramePoll::Busy => break,
            }
        }
    });
    let server = tokio::spawn(ipc::serve(listener, client.clone(), Presence::default()));
    tokio::task::spawn_blocking(|| io::copy(&mut io::stdin().lock(), &mut io::sink())).await??;
    client.command(Command::Shutdown {}).await?;
    running.await?;
    server.abort();
    observer.abort();
    fs::write(
        root.join("progress.json"),
        serde_json::to_vec(&*progress.lock().unwrap())?,
    )?;
    Ok(())
}
