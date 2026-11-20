//! Where the pointer goes when it leaves this computer: the layout of devices turned into barriers on
//! this desktop, an entry point on the device beyond, the pointer followed there, and the point it
//! comes back at. Pure: what the capture reports goes in, what to send and where to let go comes out.
//!
//! The pointer on the other device is followed here, not there: every motion is added to where it is
//! on that device's desktop, kept within its displays, and sent as a position. So the two never
//! disagree about where it is, whatever acceleration either system applies, and the way back is found
//! the moment the pointer is pushed through the edge it came in by.
use peersus_core::layout::{self, Crossing, Display, Edge, Monitor, Placement};
use std::collections::BTreeMap;

/// This computer's name on the plane: the same as the layout editor's.
pub const LOCAL: &str = "this-device";

/// A line on this desktop that the pointer is stopped at and captured, and the device it leads to.
#[derive(Clone, Debug, PartialEq)]
pub struct Barrier {
    pub edge: Edge,
    /// `(x1, y1, x2, y2)`, inclusive, in this desktop's own coordinates.
    pub line: (i32, i32, i32, i32),
    pub device: String,
}

/// Where a device's desktop sits on the plane.
#[derive(Clone, Debug, PartialEq)]
struct Placed {
    /// The plane point of the device desktop's top left.
    at: (i64, i64),
    /// The device desktop's own coordinates of that top left.
    origin: (i32, i32),
    extent: (u32, u32),
}

/// The layout as the pointer sees it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plane {
    monitors: Vec<Monitor>,
    crossings: Vec<Crossing>,
    devices: BTreeMap<String, Placed>,
}

fn origin(displays: &[Display]) -> (i32, i32) {
    (
        displays.iter().map(|d| d.x).min().unwrap_or(0),
        displays.iter().map(|d| d.y).min().unwrap_or(0),
    )
}

impl Plane {
    /// The plane of this computer's displays and the devices placed around it whose displays are known.
    /// A layout that cannot be used gives a plane with no way out.
    pub fn new(
        local: &[Display],
        layout: &[Placement],
        peers: &BTreeMap<String, Vec<Display>>,
    ) -> Self {
        let mut monitors = layout::place(LOCAL, local, (0, 0));
        let mut devices = BTreeMap::from([(
            LOCAL.to_owned(),
            Placed {
                at: (0, 0),
                origin: origin(local),
                extent: layout::extent(local),
            },
        )]);
        // A device that has not said what screens it has (a phone whose remote control service is
        // off) cannot show a pointer: no edge leads to it, so this computer's mouse is never taken.
        for placement in layout {
            let Some(displays) = peers.get(&placement.device).filter(|d| !d.is_empty()) else {
                continue;
            };
            monitors.extend(layout::place(
                &placement.device,
                displays,
                (placement.x, placement.y),
            ));
            devices.insert(
                placement.device.clone(),
                Placed {
                    at: (placement.x, placement.y),
                    origin: origin(displays),
                    extent: layout::extent(displays),
                },
            );
        }
        if layout::validate(&monitors).is_err_and(|error| error.blocks_saving()) {
            return Self::default();
        }
        let crossings = layout::crossings(&monitors);
        Self {
            monitors,
            crossings,
            devices,
        }
    }

    fn monitor(&self, device: &str, id: &str) -> Option<&Monitor> {
        self.monitors
            .iter()
            .find(|m| m.device == device && m.id == id)
    }

    /// Plane point to this desktop's coordinates, and back.
    fn to_local(&self, plane: (f64, f64)) -> (f64, f64) {
        let placed = &self.devices[LOCAL];
        (
            plane.0 + f64::from(placed.origin.0),
            plane.1 + f64::from(placed.origin.1),
        )
    }
    fn onto_plane(&self, local: (f64, f64)) -> (f64, f64) {
        let placed = &self.devices[LOCAL];
        (
            local.0 - f64::from(placed.origin.0),
            local.1 - f64::from(placed.origin.1),
        )
    }

    /// The barriers to set on this desktop: one for each stretch of an edge that faces another device.
    pub fn barriers(&self) -> Vec<Barrier> {
        self.crossings
            .iter()
            .filter(|c| c.from.device == LOCAL)
            .filter_map(|c| {
                let m = self.monitor(LOCAL, &c.from.id)?;
                let (x, y) = self.to_local((m.x as f64, m.y as f64));
                let (x, y, w, h) = (x as i32, y as i32, m.width as i32, m.height as i32);
                let span = |length: i32, at: f64| (at * f64::from(length)).ceil() as i32;
                let line = match c.edge {
                    Edge::Left | Edge::Right => {
                        let column = if c.edge == Edge::Left { x } else { x + w };
                        (
                            column,
                            y + span(h, c.from_span.0),
                            column,
                            y + span(h, c.from_span.1) - 1,
                        )
                    }
                    Edge::Top | Edge::Bottom => {
                        let row = if c.edge == Edge::Top { y } else { y + h };
                        (
                            x + span(w, c.from_span.0),
                            row,
                            x + span(w, c.from_span.1) - 1,
                            row,
                        )
                    }
                };
                Some(Barrier {
                    edge: c.edge,
                    line,
                    device: c.to.device.clone(),
                })
            })
            .collect()
    }

    /// The pointer was stopped at a barrier of `edge` with the cursor at `cursor` (this desktop's
    /// coordinates): the device it goes to and where it appears there.
    pub fn enter(&self, edge: Edge, cursor: (f64, f64)) -> Option<Visit> {
        let point = self.onto_plane(cursor);
        let crossing = self.crossings.iter().find(|c| {
            c.from.device == LOCAL
                && c.edge == edge
                && self
                    .monitor(LOCAL, &c.from.id)
                    .and_then(|m| c.map(along(m, edge, point)))
                    .is_some()
        })?;
        let from = self.monitor(LOCAL, &crossing.from.id)?;
        let to = self.monitor(&crossing.to.device, &crossing.to.id)?;
        let arrive = crossing.map(along(from, edge, point))?;
        Some(Visit {
            device: crossing.to.device.clone(),
            at: inside(to, edge.opposite(), arrive),
            from: let_go(edge, cursor),
        })
    }

    /// Whether `visit` can go on here: its device still placed (online, permitted) and the point it
    /// is at still on one of that device's screens, as a layout that changed meanwhile may not keep.
    pub fn keeps(&self, visit: &Visit) -> bool {
        self.monitors.iter().any(|m| {
            m.device == visit.device
                && visit.at.0 >= m.x as f64
                && visit.at.0 < m.right() as f64
                && visit.at.1 >= m.y as f64
                && visit.at.1 < m.bottom() as f64
        })
    }
}

/// How far along the `edge` of `m` the plane `point` is, from 0 at its start to 1 at its end.
fn along(m: &Monitor, edge: Edge, point: (f64, f64)) -> f64 {
    match edge {
        Edge::Left | Edge::Right => ((point.1 - m.y as f64) / f64::from(m.height)).clamp(0., 1.),
        Edge::Top | Edge::Bottom => ((point.0 - m.x as f64) / f64::from(m.width)).clamp(0., 1.),
    }
}

/// A plane point just inside `m`, by its `edge`, at `fraction` along it.
fn inside(m: &Monitor, edge: Edge, fraction: f64) -> (f64, f64) {
    inside_by(m, edge, fraction, 0.)
}

/// How far from the edge a returning pointer is let go: never so close that the hand still moving the way it came (or a pixel of
/// jitter) pushes it straight back through the barrier, which would hold it there, only sliding up and down.
pub(crate) const RETURN_DEPTH: (f64, f64) = (24., 96.);

/// Where a pointer the desktop stopped at `cursor`, at a barrier of `edge`, is let go when it is not to stay taken: the least
/// [`RETURN_DEPTH`] in from that edge, in this desktop's coordinates. Never `cursor` itself, which is on the barrier.
pub fn let_go(edge: Edge, cursor: (f64, f64)) -> (f64, f64) {
    let depth = RETURN_DEPTH.0;
    match edge {
        Edge::Left => (cursor.0 + depth, cursor.1),
        Edge::Right => (cursor.0 - depth, cursor.1),
        Edge::Top => (cursor.0, cursor.1 + depth),
        Edge::Bottom => (cursor.0, cursor.1 - depth),
    }
}

/// As [`inside`], `depth` pixels further in.
fn inside_by(m: &Monitor, edge: Edge, fraction: f64, depth: f64) -> (f64, f64) {
    let (x, y, w, h) = (
        m.x as f64,
        m.y as f64,
        f64::from(m.width),
        f64::from(m.height),
    );
    match edge {
        Edge::Left => ((x + 1. + depth).min(x + w - 1.), y + fraction * (h - 1.)),
        Edge::Right => ((x + w - 2. - depth).max(x), y + fraction * (h - 1.)),
        Edge::Top => (x + fraction * (w - 1.), (y + 1. + depth).min(y + h - 1.)),
        Edge::Bottom => (x + fraction * (w - 1.), (y + h - 2. - depth).max(y)),
    }
}

/// The pointer on another device.
#[derive(Clone, Debug, PartialEq)]
pub struct Visit {
    pub device: String,
    /// Where it is, on the plane.
    at: (f64, f64),
    /// Where it is given back on this desktop if the device goes away meanwhile: in from where it left (see [`let_go`]).
    pub from: (f64, f64),
}

/// What one motion did.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Still on the device, at this share of its desktop's width and height (0 to 1).
    There { x: f32, y: f32 },
    /// Pushed back through the edge it came by: let go of the pointer at this point of this desktop.
    Back { at: (f64, f64) },
}

impl Visit {
    /// Where the pointer is, as a share of the visited desktop.
    pub fn share(&self, plane: &Plane) -> Option<(f32, f32)> {
        let placed = plane.devices.get(&self.device)?;
        Some((
            ((self.at.0 - placed.at.0 as f64) / f64::from(placed.extent.0.max(1))).clamp(0., 1.)
                as f32,
            ((self.at.1 - placed.at.1 as f64) / f64::from(placed.extent.1.max(1))).clamp(0., 1.)
                as f32,
        ))
    }

    /// Move by `(dx, dy)` pixels of the visited device.
    pub fn moved(&mut self, plane: &Plane, dx: f64, dy: f64) -> Option<Step> {
        let wanted = (self.at.0 + dx, self.at.1 + dy);
        let on = |p: (f64, f64)| {
            plane.monitors.iter().find(|m| {
                m.device == self.device
                    && p.0 >= m.x as f64
                    && p.0 < m.right() as f64
                    && p.1 >= m.y as f64
                    && p.1 < m.bottom() as f64
            })
        };
        if on(wanted).is_some() {
            self.at = wanted;
            let (x, y) = self.share(plane)?;
            return Some(Step::There { x, y });
        }
        let here = on(self.at)?.clone();
        // Which edge it was pushed through, and whether that edge leads back to this computer there.
        let edge = if wanted.0 < here.x as f64 {
            Edge::Left
        } else if wanted.0 >= here.right() as f64 {
            Edge::Right
        } else if wanted.1 < here.y as f64 {
            Edge::Top
        } else {
            Edge::Bottom
        };
        let point = (
            wanted.0.clamp(here.x as f64, here.right() as f64 - 1.),
            wanted.1.clamp(here.y as f64, here.bottom() as f64 - 1.),
        );
        // Back through the edge that faces this computer. A taller device (a phone beside a computer)
        // faces it along only part of that edge: pushed past it anywhere, the pointer comes back at the
        // nearest point of the shared stretch, never held on a device the person is trying to leave.
        let at = along(&here, edge, point);
        if let Some(back) = plane
            .crossings
            .iter()
            .filter(|c| {
                c.from.device == self.device
                    && c.from.id == here.id
                    && c.edge == edge
                    && c.to.device == LOCAL
                    && c.from_span.1 > c.from_span.0
            })
            .min_by(|a, b| {
                let gap = |c: &&peersus_core::layout::Crossing| {
                    (at - at.clamp(c.from_span.0, c.from_span.1)).abs()
                };
                gap(a).total_cmp(&gap(b))
            })
            && let Some(to) = plane.monitor(LOCAL, &back.to.id)
            && let Some(arrive) = back.map(at.clamp(back.from_span.0, back.from_span.1))
        {
            // As far in as it was pushed past the edge, within bounds: the hand's motion carries on into this screen.
            let past = match edge {
                Edge::Left => here.x as f64 - wanted.0,
                Edge::Right => wanted.0 - (here.right() as f64 - 1.),
                Edge::Top => here.y as f64 - wanted.1,
                Edge::Bottom => wanted.1 - (here.bottom() as f64 - 1.),
            };
            let depth = past.clamp(RETURN_DEPTH.0, RETURN_DEPTH.1);
            return Some(Step::Back {
                at: plane.to_local(inside_by(to, edge.opposite(), arrive, depth)),
            });
        }
        // Any other edge holds the pointer, as the edge of a screen does.
        self.at = point;
        let (x, y) = self.share(plane)?;
        Some(Step::There { x, y })
    }
}

/// The extension that transports input and the plane of desktops its per-device guard permits.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Route {
    pub carrier: Option<peersus_core::plugins::PluginId>,
    pub plane: Plane,
}

/// Input forwarding is declared independently of pages, with a guard for each destination.
pub fn route(snapshot: &peersus_core::model::Snapshot) -> Route {
    use peersus_core::model::{PluginStatus, Standing};
    let carrier = snapshot.plugins.iter().find_map(|plugin| {
        if plugin.standing != Standing::Ready || plugin.status != PluginStatus::Running {
            return None;
        }
        let input = plugin.manifest.contributions.input.as_ref()?;
        Some((plugin.id.clone(), input.capability.clone()))
    });
    let Some((plugin, capability)) = carrier else {
        return Route::default();
    };
    // A sleeping device (its screen off) shows no pointer: no edge leads to it, and a visit there ends and gives the pointer back.
    let allowed = |peer: &peersus_core::model::Peer| {
        peer.online
            && !peer.sleeping
            && peersus_api::permission::Permission::new(plugin.as_str(), &capability).is_some_and(
                |permission| {
                    peer.grants
                        .get(&plugin)
                        .is_some_and(|grants| grants.allows(&permission))
                },
            )
    };
    let reachable: Vec<Placement> = snapshot
        .settings
        .layout
        .iter()
        .filter(|placement| {
            snapshot
                .peers
                .iter()
                .any(|peer| peer.id.0 == placement.device && allowed(peer))
        })
        .cloned()
        .collect();
    let displays = snapshot
        .peer_displays
        .iter()
        .map(|(peer, displays)| (peer.0.clone(), displays.clone()))
        .collect();
    Route {
        carrier: Some(plugin),
        plane: Plane::new(&snapshot.displays, &reachable, &displays),
    }
}

/// Whether `manifest` declares that it carries input between devices while one of `running`
/// already does. One extension carries it ([`route`] sends to one): a second is not started
/// beside the first, which is said to the person, rather than left running and never used.
pub fn carrier_taken<'a>(
    manifest: &peersus_api::manifest::Manifest,
    running: impl IntoIterator<Item = &'a peersus_api::manifest::Manifest>,
) -> bool {
    manifest.contributions.input.is_some()
        && running
            .into_iter()
            .any(|other| other.contributions.input.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(id: &str, x: i32, y: i32, width: u32, height: u32) -> Display {
        Display {
            id: id.into(),
            x,
            y,
            width,
            height,
        }
    }

    /// This computer 1920×1080; a laptop 1280×800 to its right, its top 100 px lower.
    fn beside() -> Plane {
        Plane::new(
            &[screen("a", 0, 0, 1920, 1080)],
            &[Placement {
                device: "laptop".into(),
                x: 1920,
                y: 100,
            }],
            &BTreeMap::from([("laptop".to_owned(), vec![screen("b", 0, 0, 1280, 800)])]),
        )
    }

    #[test]
    fn a_second_extension_that_carries_input_is_not_taken_beside_the_first() {
        use peersus_api::{
            contributions::{Input, Source},
            manifest::Manifest,
        };
        let plain = Manifest::new(peersus_api::manifest::Execution::Process);
        let mut carrier = Manifest::new(peersus_api::manifest::Execution::Process);
        carrier.contributions.input = Some(Input {
            source: Source {
                service: "example.input".into(),
                method: "forward".into(),
            },
            capability: "forward.use".into(),
        });
        // The first one, and any number of extensions that carry nothing beside it.
        assert!(!carrier_taken(&carrier, []));
        assert!(!carrier_taken(&carrier, [&plain, &plain]));
        assert!(!carrier_taken(&plain, [&carrier]));
        // A second one that carries input, whatever else runs.
        assert!(carrier_taken(&carrier, [&plain, &carrier]));
    }

    #[test]
    fn forwarding_requires_a_declared_transport_and_the_destinations_own_grant() {
        use peersus_core::model::{PluginInfo, PluginStatus, Snapshot, Standing};
        let plugin = peersus_core::plugins::PluginId::new("control").unwrap();
        let mut snapshot: Snapshot = serde_json::from_value(serde_json::json!({
            "settings":peersus_core::Settings::default(),
            "peers":[{"id":"laptop","name":"Remote","paired_at":0,"last_seen":null,"online":true}],
            "transfers":[],"pairing":[],"errors":[]
        }))
        .unwrap();
        snapshot.displays = vec![screen("a", 0, 0, 1920, 1080)];
        snapshot.settings.layout = vec![Placement {
            device: "laptop".into(),
            x: 1920,
            y: 0,
        }];
        snapshot.peer_displays.insert(
            snapshot.peers[0].id.clone(),
            vec![screen("b", 0, 0, 1280, 800)],
        );
        snapshot.plugins.push(PluginInfo {
            id: plugin.clone(),
            manifest: peersus_api::manifest::Manifest::new(
                peersus_api::manifest::Execution::Process,
            ),
            standing: Standing::Ready,
            status: PluginStatus::Running,
            grantable: Default::default(),
            program: None,
            package: None,
            agreed: false,
            from_store: false,
        });
        assert!(route(&snapshot).carrier.is_none());
        snapshot.plugins[0].manifest.contributions.input =
            Some(peersus_api::contributions::Input {
                source: peersus_api::contributions::Source {
                    service: "transport".into(),
                    method: "send".into(),
                },
                capability: "control.use".into(),
            });
        assert!(route(&snapshot).plane.barriers().is_empty());
        let permission =
            peersus_api::permission::Permission::new(plugin.as_str(), "control.use").unwrap();
        let grants = serde_json::from_value(serde_json::json!([permission])).unwrap();
        snapshot.grants.insert(plugin.clone(), grants);
        assert!(route(&snapshot).plane.barriers().is_empty());
        snapshot.peers[0].grants = snapshot.grants.clone();
        assert_eq!(route(&snapshot).carrier, Some(plugin));
        assert_eq!(route(&snapshot).plane.barriers().len(), 1);
        snapshot.peers[0].sleeping = true;
        assert!(
            route(&snapshot).plane.barriers().is_empty(),
            "a sleeping device takes no pointer"
        );
        snapshot.peers[0].sleeping = false;
        snapshot.peers[0].online = false;
        assert!(route(&snapshot).plane.barriers().is_empty());
    }

    #[test]
    fn a_barrier_covers_only_the_stretch_of_the_edge_that_faces_the_device() {
        let barriers = beside().barriers();
        assert_eq!(
            barriers,
            vec![Barrier {
                edge: Edge::Right,
                line: (1920, 100, 1920, 899),
                device: "laptop".into()
            }]
        );
    }

    #[test]
    fn the_pointer_enters_level_moves_there_and_comes_back_where_it_left() {
        let plane = beside();
        let mut visit = plane.enter(Edge::Right, (1919., 500.)).unwrap();
        assert_eq!(visit.device, "laptop");
        // 500 on this screen is 400 down the laptop's 800: half way.
        let (x, y) = visit.share(&plane).unwrap();
        assert!(x < 0.01 && (y - 0.5).abs() < 0.01, "{x} {y}");
        assert!(
            matches!(visit.moved(&plane, 640., 0.), Some(Step::There { x, .. }) if (x - 0.5).abs() < 0.01)
        );
        // Up past the laptop's top: held there, it does not lead anywhere.
        assert!(matches!(visit.moved(&plane, 0., -5000.), Some(Step::There { y, .. }) if y == 0.));
        // Back left through the edge it came by: let go on this screen inside its right edge, as far in as it was pushed (bounded).
        let Some(Step::Back { at }) = visit.moved(&plane, -5000., 0.) else {
            panic!("did not come back");
        };
        assert!(
            (at.0 - (1918. - RETURN_DEPTH.1)).abs() < 1. && (at.1 - 100.).abs() < 1.,
            "{at:?}"
        );
        // A small push is let go at the least depth, never at the barrier itself.
        let mut again = plane.enter(Edge::Right, (1919., 500.)).unwrap();
        let Some(Step::Back { at }) = again.moved(&plane, -3., 0.) else {
            panic!("did not come back");
        };
        assert!((at.0 - (1918. - RETURN_DEPTH.0)).abs() < 1., "{at:?}");
    }

    #[test]
    fn a_pointer_not_kept_is_let_go_in_from_the_edge_it_was_stopped_at() {
        let depth = RETURN_DEPTH.0;
        assert_eq!(let_go(Edge::Left, (0., 500.)), (depth, 500.));
        assert_eq!(let_go(Edge::Right, (1919., 500.)), (1919. - depth, 500.));
        assert_eq!(let_go(Edge::Top, (700., 0.)), (700., depth));
        assert_eq!(let_go(Edge::Bottom, (700., 1079.)), (700., 1079. - depth));
    }

    #[test]
    fn a_visit_ends_when_its_device_or_the_place_it_was_at_leaves_the_layout() {
        let plane = beside();
        let visit = plane.enter(Edge::Right, (1919., 500.)).unwrap();
        assert_eq!(visit.from, (1919. - RETURN_DEPTH.0, 500.));
        assert!(plane.keeps(&visit));
        let gone = Plane::new(&[screen("a", 0, 0, 1920, 1080)], &[], &BTreeMap::new());
        assert!(!gone.keeps(&visit), "the device left");
        let moved = Plane::new(
            &[screen("a", 0, 0, 1920, 1080)],
            &[Placement {
                device: "laptop".into(),
                x: 1920,
                y: 1000,
            }],
            &BTreeMap::from([("laptop".to_owned(), vec![screen("b", 0, 0, 1280, 800)])]),
        );
        assert!(
            !moved.keeps(&visit),
            "its screen was moved away from where the pointer was"
        );
    }

    #[test]
    fn a_pointer_low_on_a_taller_device_still_comes_back_through_the_edge_it_faces() {
        // A phone 1080×2400 to the right of a 1920×1080 screen: they share only its top 1080.
        let plane = Plane::new(
            &[screen("a", 0, 0, 1920, 1080)],
            &[Placement {
                device: "phone".into(),
                x: 1920,
                y: 0,
            }],
            &BTreeMap::from([("phone".to_owned(), vec![screen("p", 0, 0, 1080, 2400)])]),
        );
        let mut visit = plane.enter(Edge::Right, (1919., 500.)).unwrap();
        assert!(matches!(
            visit.moved(&plane, 0., 1500.),
            Some(Step::There { .. })
        ));
        let Some(Step::Back { at }) = visit.moved(&plane, -5000., 0.) else {
            panic!("held on the phone below the shared stretch");
        };
        assert!(
            at.0 > 1800. && at.0 < 1920. - RETURN_DEPTH.0 && at.1 > 1000.,
            "{at:?}"
        );
    }

    #[test]
    fn a_pointer_comes_back_from_a_device_on_the_left_as_it_does_from_one_on_the_right() {
        for (width, height) in [(1080, 2400), (1116, 2480), (1920, 1080)] {
            let plane = Plane::new(
                &[screen("a", 0, 0, 1920, 1080)],
                &[Placement {
                    device: "phone".into(),
                    x: -width,
                    y: 0,
                }],
                &BTreeMap::from([(
                    "phone".to_owned(),
                    vec![screen("p", 0, 0, width as u32, height as u32)],
                )]),
            );
            let mut visit = plane
                .enter(Edge::Left, (0., 500.))
                .expect("enters on the left");
            // Small steps right, as a mouse moves, until it is back.
            let mut back = None;
            for _ in 0..50 {
                match visit.moved(&plane, 1., 0.) {
                    Some(Step::Back { at }) => {
                        back = Some(at);
                        break;
                    }
                    Some(Step::There { .. }) => {}
                    None => panic!("{width}x{height}: lost the pointer on the phone"),
                }
            }
            let at =
                back.unwrap_or_else(|| panic!("{width}x{height}: held at the phone's right edge"));
            assert!(
                at.0 >= RETURN_DEPTH.0 && at.0 <= RETURN_DEPTH.1 + 2.,
                "let go far enough in to not hit the barrier again: {at:?}"
            );
        }
    }

    #[test]
    fn nothing_crosses_where_no_device_is_or_with_an_unusable_layout() {
        let plane = beside();
        assert!(
            plane.enter(Edge::Right, (1919., 50.)).is_none(),
            "above the laptop"
        );
        assert!(plane.enter(Edge::Left, (0., 500.)).is_none());
        let overlapping = Plane::new(
            &[screen("a", 0, 0, 1920, 1080)],
            &[Placement {
                device: "laptop".into(),
                x: 100,
                y: 100,
            }],
            &BTreeMap::from([("laptop".to_owned(), vec![screen("b", 0, 0, 1280, 800)])]),
        );
        assert!(overlapping.barriers().is_empty());
    }

    #[test]
    fn a_desktop_that_does_not_start_at_zero_is_placed_by_its_own_top_left() {
        // This computer's desktop starts at -1920 (a screen on the left of its primary).
        let plane = Plane::new(
            &[
                screen("left", -1920, 0, 1920, 1080),
                screen("main", 0, 0, 1920, 1080),
            ],
            &[Placement {
                device: "hp".into(),
                x: -1080,
                y: 0,
            }],
            &BTreeMap::from([("hp".to_owned(), vec![screen("p", 0, 0, 1080, 1920)])]),
        );
        let barriers = plane.barriers();
        assert_eq!(barriers.len(), 1);
        assert_eq!(barriers[0].edge, Edge::Left);
        assert_eq!(
            barriers[0].line.0, -1920,
            "in this desktop's own coordinates"
        );
        assert!(plane.enter(Edge::Left, (-1920., 500.)).is_some());
    }
}
