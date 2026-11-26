use crate::{
    contributed_page::{Avatars, Elements},
    link::{Platform as LinkPlatform, Session},
};
use freya::prelude::*;
use peersus_api::{
    canvas::{self, FrameHeader, FrameLayout},
    elements::{Event, Node, PointerPhase},
    manifest::Manifest,
    message::{Delivery, Operation, Outcome},
};
use peersus_app::{
    plugin_host::{PluginSpec, Registry},
    runtime::Runtime,
};
use peersus_core::{config::NetworkSettings, model::Command, plugins::PluginId};
use peersus_net::{
    grants::StandaloneGrants, history::History, network_host::NetworkHost, peers::Peers,
    settings::SettingsStore, transport,
};
use peersus_plugin::{
    data::DataHello,
    frame::{self, CONTROL_LIMIT},
    link::Hello,
    supervisor::{Bootstrap, Launcher, Lifecycle},
    transport::Stream,
};
use serde_json::{Value, json};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

const LIMIT: Duration = Duration::from_millis(750);

fn tree() -> Value {
    json!({"k":"canvas","id":"scene","observe":["size"],
        "style":{"width":{"unit":"px","value":48},"height":{"unit":"px","value":32}}})
}

#[derive(Default)]
struct Audit {
    opened: AtomicUsize,
    input: AtomicUsize,
    stream_failed: AtomicUsize,
}

struct Guest(Arc<Audit>);
struct Running(tokio::task::JoinHandle<io::Result<()>>);
impl Launcher for Guest {
    fn launch(&self, bootstrap: Bootstrap) -> io::Result<Box<dyn Lifecycle>> {
        Ok(Box::new(Running(tokio::spawn(guest(
            bootstrap,
            self.0.clone(),
        )))))
    }
}
impl Lifecycle for Running {
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            let _ = (&mut self.0).await;
        })
    }
    fn stop(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.0.abort();
        self.wait()
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn guest(bootstrap: Bootstrap, audit: Arc<Audit>) -> io::Result<()> {
    let mut control = peersus_plugin::transport::connect(&bootstrap.socket).await?;
    frame::write(
        &mut control,
        &Hello {
            token: bootstrap.token.clone(),
        },
        1024,
    )
    .await?;
    // Existing guest contract: Open retires the old writer; the first size opens a new
    // writer and paints promptly. There is no extension timer to hide a missed Open.
    let mut writer = None::<Stream>;
    let mut opening = None;
    let mut serial = 0;
    loop {
        match frame::read::<Delivery>(&mut control, CONTROL_LIMIT).await? {
            Delivery::Call { ticket, args, .. } => {
                let event: Event = serde_json::from_value(args["event"].clone())?;
                let value = match event {
                    Event::Open => {
                        writer = None;
                        opening = None;
                        audit.opened.fetch_add(1, Relaxed);
                        tree()
                    }
                    Event::CanvasSize { .. } if writer.is_none() && opening.is_none() => {
                        serial += 1;
                        opening = Some(serial);
                        frame::write(
                            &mut control,
                            &Operation::Call {
                                id: serial,
                                provider: "host".into(),
                                service: canvas::SERVICE.into(),
                                version: 1,
                                method: canvas::OPEN.into(),
                                args: json!({"page":"demo","canvas":"scene"}),
                                timeout_ms: 500,
                            },
                            CONTROL_LIMIT,
                        )
                        .await?;
                        Value::Null
                    }
                    Event::Pointer { .. } => {
                        audit.input.fetch_add(1, Relaxed);
                        Value::Null
                    }
                    _ => Value::Null,
                };
                frame::write(
                    &mut control,
                    &Operation::Reply {
                        ticket,
                        result: Outcome::ok(value),
                    },
                    CONTROL_LIMIT,
                )
                .await?;
            }
            Delivery::Result {
                id,
                result: Outcome::Ok { value },
            } if opening == Some(id) => {
                let mut stream = peersus_plugin::transport::connect(&bootstrap.data_socket).await?;
                frame::write(
                    &mut stream,
                    &DataHello {
                        token: bootstrap.token.clone(),
                        ticket: 0,
                        claim: value["claim"].as_u64(),
                    },
                    1024,
                )
                .await?;
                let mut wire = [0; canvas::MAX_FRAME_HEADER_BYTES];
                let head = FrameHeader {
                    id: "scene",
                    layout: FrameLayout::tight(1, 1).unwrap(),
                    seq: 1,
                    settled: 0,
                };
                let encoded = head.encode(&mut wire).unwrap();
                let pixels = [60 + 30 * id as u8, 25, 12, 255];
                if stream.write_all(encoded).await.is_err()
                    || stream.write_all(&pixels).await.is_err()
                {
                    audit.stream_failed.fetch_add(1, Relaxed);
                }
                // Leave a further frame in progress: a new Open must be able to retire
                // the old stream even though its reader is waiting for the raster body.
                let head = FrameHeader { seq: 2, ..head };
                let _ = stream.write_all(head.encode(&mut wire).unwrap()).await;
                writer = Some(stream);
                opening = None;
            }
            Delivery::Result { id, .. } if opening == Some(id) => {
                opening = None;
            }
            _ => {}
        }
    }
}

#[test]
fn a_cached_canvas_reopens_after_input_and_quick_switches_without_waiting_for_a_tick() {
    let root = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let audit = Arc::new(Audit::default());
    let plugin = PluginId::new("fixture-canvas").unwrap();
    let socket = root.path().join("owner.sock");
    let owner = runtime.block_on(async {
        let settings = NetworkSettings {
            bind: Some(std::net::Ipv4Addr::LOCALHOST.into()),
            allow_relay: false,
            ..Default::default()
        };
        let endpoint = transport::bind(iroh::SecretKey::generate(), &settings)
            .await
            .unwrap();
        let mut registry = Registry::default();
        let split = peersus_api::package::split(include_bytes!(
            "../../../../templates/canvas-guest/plugin.json"
        ))
        .unwrap();
        let manifest: Manifest = serde_json::from_slice(&split.manifest).unwrap();
        registry
            .register(PluginSpec {
                id: plugin.clone(),
                manifest,
                program: None,
                arguments: vec![],
                restart_policy: None,
                languages: Default::default(),
            })
            .unwrap();
        let path = root.path();
        let network = NetworkHost::new(
            endpoint,
            "Canvas fixture".into(),
            Peers::open(path.join("peers.json")).unwrap(),
            History::open(path.join("history.json")).unwrap(),
        );
        let (owner, client) = Runtime::new(
            network,
            StandaloneGrants::open(path.join("grants.json")).unwrap(),
            SettingsStore::open(path.join("settings.json")).unwrap(),
            registry,
            path.join("sockets"),
        )
        .unwrap();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(peersus_app::ipc::serve(
            listener,
            client.clone(),
            Default::default(),
        ));
        tokio::spawn(
            owner
                .with_launcher(plugin.clone(), Arc::new(Guest(audit.clone())))
                .run(),
        );
        client
    });
    let send = |event| Command::PageEvent {
        plugin: plugin.clone(),
        page: "demo".into(),
        event,
    };
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if owner.command(send(Event::Open)).await.is_ok() {
                    while audit.opened.load(Relaxed) == 0 {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    });
    let session = Session::open_at(socket, LinkPlatform::default(), Some(&owner)).unwrap();
    let link = session.link.as_ref().unwrap().clone();
    let cached = Node::parse(&tree()).unwrap();
    let ui_plugin = plugin.clone();
    let (mut runner, mut shown) = freya_testing::TestingRunner::new(
        move || {
            rect().width(Size::fill()).height(Size::fill()).maybe_child(
                use_consume::<State<bool>>()().then(|| Elements {
                    link: link.clone(),
                    plugin: ui_plugin.clone(),
                    page: "demo".into(),
                    measurement_page: "demo".into(),
                    view: Some(cached.clone()),
                    avatars: Avatars::default(),
                    pictures: crate::shelf::Held(Default::default()),
                    palette: crate::theme::DARK,
                    embedded: false,
                }),
            )
        },
        (48., 32.).into(),
        |runner| runner.provide_root_context(|| State::create(false)),
        1.,
    );
    for cycle in 1..=4 {
        shown.set(true);
        let until = Instant::now() + LIMIT;
        loop {
            runner.poll_n(Duration::from_millis(2), 2);
            let picture = image::load_from_memory(runner.render().as_bytes())
                .unwrap()
                .to_rgba8();
            if picture.get_pixel(16, 16).0 == [60 + 30 * cycle as u8, 25, 12, 255] {
                break;
            }
            assert!(
                Instant::now() < until,
                "showing {cycle} did not draw; Open count={}, write failures={}",
                audit.opened.load(Relaxed),
                audit.stream_failed.load(Relaxed)
            );
        }
        shown.set(false);
        runner.sync_and_update();
        if cycle == 1 {
            runtime.block_on(async {
                owner
                    .command(send(Event::Pointer {
                        id: "scene".into(),
                        phase: PointerPhase::Up,
                        x: 0,
                        y: 0,
                        at: 1,
                    }))
                    .await
                    .unwrap();
                tokio::time::timeout(LIMIT, async {
                    while audit.input.load(Relaxed) == 0 {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                })
                .await
                .unwrap();
            });
        }
    }
    assert_eq!(
        audit.opened.load(Relaxed),
        5,
        "prime plus exactly one Open per showing"
    );
    assert_eq!(audit.stream_failed.load(Relaxed), 0);
}
