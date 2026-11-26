//! Where the pointer goes when it leaves this computer: the layout of devices turned into barriers on
//! this desktop, an entry point on the device beyond, the pointer followed there, and the point it
//! comes back at. Pure: what the capture reports goes in, what to send and where to let go comes out.
//!
//! The pointer on the other device is followed here, not there: every motion is added to where it is
//! on that device's desktop, kept within its displays, and sent as a position. So the two never
//! disagree about where it is, whatever acceleration either system applies. The source measures any
//! extra push at the return edge as well.
use peersus_core::layout::{self, Crossing, Display, Edge, Monitor, Placement};
use std::{collections::BTreeMap, num::NonZeroU16};

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
    relative_to_local: Option<Edge>,
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

fn tangent<T: Copy>((x, y): (T, T), edge: Edge) -> T {
    match edge {
        Edge::Left | Edge::Right => y,
        Edge::Top | Edge::Bottom => x,
    }
}

fn span(m: &Monitor, edge: Edge) -> (i64, i64) {
    (
        tangent((m.x, m.y), edge),
        tangent((m.right(), m.bottom()), edge),
    )
}

fn boundary(m: &Monitor, edge: Edge) -> i64 {
    match edge {
        Edge::Left => m.x,
        Edge::Right => m.right(),
        Edge::Top => m.y,
        Edge::Bottom => m.bottom(),
    }
}

/// Pick the outside monitor at the requested tangent coordinate. A gap lands on the
/// closest screen; two screens covering the same coordinate use the one farther out.
fn outer_monitor<'a>(
    monitors: impl Iterator<Item = &'a Monitor>,
    edge: Edge,
    at: f64,
) -> Option<&'a Monitor> {
    let rank = |m: &Monitor| {
        let (start, end) = span(m, edge);
        let gap = (start as f64 - at).max(at - end as f64).max(0.);
        let outside = match edge {
            Edge::Left | Edge::Top => boundary(m, edge),
            Edge::Right | Edge::Bottom => -boundary(m, edge),
        };
        (gap, outside)
    };
    monitors.min_by(|a, b| {
        let a = rank(a);
        let b = rank(b);
        a.0.total_cmp(&b.0).then(a.1.cmp(&b.1))
    })
}

impl Placed {
    fn fraction(&self, edge: Edge, point: (f64, f64)) -> f64 {
        let start = tangent(self.at, edge) as f64;
        let len = f64::from(tangent(self.extent, edge));
        ((tangent(point, edge) - start) / len).clamp(0., 1.)
    }
}

impl Plane {
    /// The plane of this computer's displays and the devices placed around it whose displays are known.
    /// Invalid free geometry gives a plane with no way out. An invalid dock leaves other routes alone.
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
                relative_to_local: None,
            },
        )]);
        // Establish the free plane first. Neither a dormant dock nor its projected geometry
        // is allowed to change its crossings.
        for placement in layout.iter().filter(|p| p.relative_to_local.is_none()) {
            let Some(displays) = peers.get(&placement.device).filter(|d| !d.is_empty()) else {
                continue;
            };
            let at = (placement.x, placement.y);
            monitors.extend(layout::place(&placement.device, displays, at));
            devices.insert(
                placement.device.clone(),
                Placed {
                    at,
                    origin: origin(displays),
                    extent: layout::extent(displays),
                    relative_to_local: None,
                },
            );
        }
        if layout::validate_geometry(&monitors).is_err() {
            return Self::default();
        }
        let crossings = layout::crossings(&monitors);
        let mut docks = layout::Docking::new(local, &monitors, &crossings, LOCAL);
        let mut relative_monitors = Vec::new();
        for (placement, side) in layout
            .iter()
            .filter_map(|p| p.relative_to_local.map(|side| (p, side)))
        {
            let displays = peers.get(&placement.device).map_or(&[][..], Vec::as_slice);
            match docks.project(&placement.device, displays, side) {
                Ok(projected) if !projected.is_empty() => {}
                _ => continue,
            }
            // A relative visit still uses native logical pixels; only the drawing and
            // boundary mapping are scaled.
            let at = (0, 0);
            relative_monitors.extend(layout::place(&placement.device, displays, at));
            devices.insert(
                placement.device.clone(),
                Placed {
                    at,
                    origin: origin(displays),
                    extent: layout::extent(displays),
                    relative_to_local: Some(side),
                },
            );
        }
        monitors.extend(relative_monitors);
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
    fn onto_plane(&self, local: (f64, f64)) -> Option<(f64, f64)> {
        let placed = self.devices.get(LOCAL)?;
        Some((
            local.0 - f64::from(placed.origin.0),
            local.1 - f64::from(placed.origin.1),
        ))
    }

    fn relative_to(&self, edge: Edge) -> Option<&str> {
        self.devices.iter().find_map(|(device, placed)| {
            (placed.relative_to_local == Some(edge)).then_some(device.as_str())
        })
    }

    fn relative_landing(&self, device: &str, edge: Edge, fraction: f64) -> Option<(&Monitor, f64)> {
        let placed = self.devices.get(device)?;
        let coordinate =
            tangent(placed.at, edge) as f64 + fraction * f64::from(tangent(placed.extent, edge));
        let monitor = outer_monitor(
            self.monitors.iter().filter(|m| m.device == device),
            edge,
            coordinate,
        )?;
        let (start, end) = span(monitor, edge);
        // `inside_by` stops at the last pixel. Use its inverse so a round trip does not
        // accumulate a one-pixel drift on every crossing.
        let along = ((coordinate - start as f64) / (end - start - 1).max(1) as f64).clamp(0., 1.);
        Some((monitor, along))
    }

    fn relative_barriers(&self, edge: Edge, device: &str, barriers: &mut Vec<Barrier>) {
        let Some(local) = self.devices.get(LOCAL) else {
            return;
        };
        let monitors = || self.monitors.iter().filter(|m| m.device == LOCAL);
        let mut boundaries: Vec<_> = monitors()
            .flat_map(|m| {
                let (start, end) = span(m, edge);
                [start, end]
            })
            .collect();
        boundaries.sort_unstable();
        boundaries.dedup();
        for range in boundaries.windows(2) {
            let [start, end] = [range[0], range[1]];
            let Some(m) = outer_monitor(monitors(), edge, (start + end) as f64 / 2.) else {
                continue;
            };
            let (first, last) = span(m, edge);
            if start < first || end > last {
                continue; // a gap in the local contour, not a barrier in any capture zone
            }
            let tangent_origin = i64::from(tangent(local.origin, edge));
            let normal_origin = match edge {
                Edge::Left | Edge::Right => local.origin.0,
                Edge::Top | Edge::Bottom => local.origin.1,
            };
            let normal = boundary(m, edge) + i64::from(normal_origin);
            let (Ok(n), Ok(a), Ok(b)) = (
                i32::try_from(normal),
                i32::try_from(start + tangent_origin),
                i32::try_from(end - 1 + tangent_origin),
            ) else {
                continue;
            };
            let line = match edge {
                Edge::Left | Edge::Right => (n, a, n, b),
                Edge::Top | Edge::Bottom => (a, n, b, n),
            };
            barriers.push(Barrier {
                edge,
                line,
                device: device.into(),
            });
        }
    }

    /// The barriers to set on this desktop: one for each stretch of an edge that faces another device.
    pub fn barriers(&self) -> Vec<Barrier> {
        let mut barriers: Vec<_> = self
            .crossings
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
            .collect();
        for (device, placed) in &self.devices {
            if let Some(edge) = placed.relative_to_local {
                self.relative_barriers(edge, device, &mut barriers);
            }
        }
        barriers
    }

    /// The pointer was stopped at a barrier of `edge` with the cursor at `cursor` (this desktop's
    /// coordinates): the device it goes to and where it appears there.
    pub fn enter(&self, edge: Edge, cursor: (f64, f64)) -> Option<Visit> {
        let point = self.onto_plane(cursor)?;
        if let Some(device) = self.relative_to(edge) {
            if !cursor.0.is_finite() || !cursor.1.is_finite() {
                return None;
            }
            let fraction = self.devices.get(LOCAL)?.fraction(edge, point);
            let (to, arrive) = self.relative_landing(device, edge.opposite(), fraction)?;
            return Some(Visit {
                device: device.into(),
                at: inside(to, edge.opposite(), arrive),
                from: let_go(edge, cursor),
                returning: None,
                relative_to_local: Some(edge),
            });
        }
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
            returning: None,
            relative_to_local: None,
        })
    }

    /// Whether `visit` can go on here: its device still placed (online, permitted) and the point it
    /// is at still on one of that device's screens, as a layout that changed meanwhile may not keep.
    pub fn keeps(&self, visit: &Visit) -> bool {
        if self
            .devices
            .get(&visit.device)
            .is_none_or(|p| p.relative_to_local != visit.relative_to_local)
        {
            return false;
        }
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

/// A push beyond an eligible screen edge, either at the local barrier or on the visited desktop.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Push {
    edge: Edge,
    amount: NonZeroU16,
    out: f64,
    aside: f64,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Pushed {
    More,
    Through { extra: f64 },
    Back,
}

impl Push {
    pub(crate) fn at(edge: Edge, amount: NonZeroU16) -> Self {
        Self {
            edge,
            amount,
            out: 0.,
            aside: 0.,
        }
    }

    pub(crate) fn moved(&mut self, dx: f64, dy: f64) -> Pushed {
        let (out, aside) = Self::components(self.edge, dx, dy);
        self.add(out, aside)
    }

    fn components(edge: Edge, dx: f64, dy: f64) -> (f64, f64) {
        match edge {
            Edge::Left => (-dx, dy),
            Edge::Right => (dx, dy),
            Edge::Top => (-dy, dx),
            Edge::Bottom => (dy, dx),
        }
    }

    fn add(&mut self, out: f64, aside: f64) -> Pushed {
        // Do not give up on normal hand jitter, or treat sliding along the edge as a push.
        const BACK_SLACK: f64 = 16.;
        const ASIDE_SLACK: f64 = 64.;
        if !out.is_finite() || !aside.is_finite() {
            return Pushed::More;
        }
        self.out += out;
        self.aside += aside.abs();
        let extra = self.out - f64::from(self.amount.get());
        if extra >= 0. {
            Pushed::Through { extra }
        } else if self.out <= -BACK_SLACK || self.aside > 2. * self.out.max(0.) + ASIDE_SLACK {
            Pushed::Back
        } else {
            Pushed::More
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Returning {
    /// Index into the current plane. A route change drops the push.
    monitor: usize,
    push: Push,
}

fn on_screen(m: &Monitor, point: (f64, f64)) -> (f64, f64) {
    (
        point.0.clamp(m.x as f64, m.right() as f64 - 1.),
        point.1.clamp(m.y as f64, m.bottom() as f64 - 1.),
    )
}

/// A taller visited device may have only a partial shared stretch. Return at the closest
/// point of that stretch even when the pointer is farther along the same edge.
fn return_landing<'a>(
    plane: &'a Plane,
    here: &Monitor,
    edge: Edge,
    point: (f64, f64),
) -> Option<(&'a Monitor, f64)> {
    let placed = plane.devices.get(&here.device)?;
    if let Some(side) = placed.relative_to_local {
        if side.opposite() != edge {
            return None;
        }
        let outer = outer_monitor(
            plane.monitors.iter().filter(|m| m.device == here.device),
            edge,
            tangent(point, edge),
        )?;
        if boundary(outer, edge) != boundary(here, edge) {
            return None;
        }
        return plane.relative_landing(LOCAL, side, placed.fraction(edge, point));
    }
    let at = along(here, edge, point);
    let back = plane
        .crossings
        .iter()
        .filter(|c| {
            c.from.device == here.device
                && c.from.id == here.id
                && c.edge == edge
                && c.to.device == LOCAL
                && c.from_span.1 > c.from_span.0
        })
        .min_by(|a, b| {
            let gap = |c: &Crossing| (at - at.clamp(c.from_span.0, c.from_span.1)).abs();
            gap(a).total_cmp(&gap(b))
        })?;
    let to = plane.monitor(LOCAL, &back.to.id)?;
    let arrive = back.map(at.clamp(back.from_span.0, back.from_span.1))?;
    Some((to, arrive))
}

fn back_to(plane: &Plane, to: &Monitor, edge: Edge, arrive: f64, extra: f64) -> Step {
    let depth = extra.clamp(RETURN_DEPTH.0, RETURN_DEPTH.1);
    Step::Back {
        at: plane.to_local(inside_by(to, edge.opposite(), arrive, depth)),
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
    returning: Option<Returning>,
    relative_to_local: Option<Edge>,
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

    /// Whether another motion can add to a pending return push.
    #[cfg(target_os = "linux")]
    pub(crate) fn pushing_back(&self) -> bool {
        self.returning.is_some()
    }

    /// Forget a pending push without leaving the visited desktop.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn clear_return(&mut self) {
        self.returning = None;
    }

    /// Move by `(dx, dy)` on the visited device, measuring any required extra push to return.
    pub fn moved(
        &mut self,
        plane: &Plane,
        dx: f64,
        dy: f64,
        pressure: Option<NonZeroU16>,
    ) -> Option<Step> {
        let wanted = (self.at.0 + dx, self.at.1 + dy);
        if let Some(mut returning) = self
            .returning
            .take()
            .filter(|pending| Some(pending.push.amount) == pressure)
        {
            let here = plane.monitors.get(returning.monitor)?;
            let edge = returning.push.edge;
            let point = on_screen(here, wanted);
            match returning.push.moved(dx, dy) {
                Pushed::More => {
                    // Keep the normal coordinate pinned through jitter; only the tangent moves.
                    self.at = match edge {
                        Edge::Left | Edge::Right => (self.at.0, point.1),
                        Edge::Top | Edge::Bottom => (point.0, self.at.1),
                    };
                    self.returning = Some(returning);
                }
                Pushed::Back => self.at = point,
                Pushed::Through { extra } => {
                    let (to, arrive) = return_landing(plane, here, edge, point)?;
                    return Some(back_to(plane, to, edge, arrive, extra));
                }
            }
            return self.there(plane);
        }

        let on = |p: (f64, f64)| {
            plane.monitors.iter().position(|m| {
                m.device == self.device
                    && p.0 >= m.x as f64
                    && p.0 < m.right() as f64
                    && p.1 >= m.y as f64
                    && p.1 < m.bottom() as f64
            })
        };
        if on(wanted).is_some() {
            self.at = wanted;
            return self.there(plane);
        }
        let monitor = on(self.at)?;
        let here = &plane.monitors[monitor];
        let edge = if wanted.0 < here.x as f64 {
            Edge::Left
        } else if wanted.0 >= here.right() as f64 {
            Edge::Right
        } else if wanted.1 < here.y as f64 {
            Edge::Top
        } else {
            Edge::Bottom
        };
        let point = on_screen(here, wanted);
        if let Some((to, arrive)) = return_landing(plane, here, edge, point) {
            let Some(amount) = pressure else {
                // Off keeps the original return point: depth measured from the last visible pixel.
                let past = match edge {
                    Edge::Left | Edge::Right => (wanted.0 - point.0).abs(),
                    Edge::Top | Edge::Bottom => (wanted.1 - point.1).abs(),
                };
                return Some(back_to(plane, to, edge, arrive, past));
            };
            // Only the part past the boundary is pressure, never the approach within the display.
            let past = match edge {
                Edge::Left => here.x as f64 - wanted.0,
                Edge::Right => wanted.0 - here.right() as f64,
                Edge::Top => here.y as f64 - wanted.1,
                Edge::Bottom => wanted.1 - here.bottom() as f64,
            };
            let mut push = Push::at(edge, amount);
            // For the initial diagonal, count the tangent of the after-edge segment, too.
            let (normal, tangent) = Push::components(edge, dx, dy);
            match push.add(past, tangent * (past / normal)) {
                Pushed::Through { extra } => return Some(back_to(plane, to, edge, arrive, extra)),
                Pushed::More => self.returning = Some(Returning { monitor, push }),
                Pushed::Back => {}
            }
        }
        self.at = point;
        self.there(plane)
    }

    fn there(&self, plane: &Plane) -> Option<Step> {
        let (x, y) = self.share(plane)?;
        Some(Step::There { x, y })
    }
}

/// The extension that transports input and the plane of desktops its per-device guard permits.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Route {
    pub carrier: Option<peersus_core::plugins::PluginId>,
    pub plane: Plane,
    pub(crate) pressure: Option<NonZeroU16>,
}

/// Input forwarding is declared independently of pages, with a guard for each destination.
pub fn route(snapshot: &peersus_core::model::Snapshot) -> Route {
    use peersus_core::{
        model::{PluginStatus, Standing},
        plugins::Value,
    };
    let carrier = snapshot.plugins.iter().find_map(|plugin| {
        if plugin.standing != Standing::Ready || plugin.status != PluginStatus::Running {
            return None;
        }
        Some((plugin, plugin.manifest.contributions.input.as_ref()?))
    });
    let Some((carrier, input)) = carrier else {
        return Route::default();
    };
    let plugin = &carrier.id;
    let pressure = match input.pressure_setting(&carrier.manifest.contributions.settings) {
        Err(_) => return Route::default(),
        Ok(None) => None,
        Ok(Some(setting)) => {
            let Value::Number(value) = snapshot.settings.plugins.get(plugin).map_or_else(
                || Value::default_for(&setting.kind),
                |saved| saved.effective(setting),
            ) else {
                return Route::default();
            };
            let Ok(value) = u16::try_from(value) else {
                return Route::default();
            };
            NonZeroU16::new(value)
        }
    };
    // A sleeping device (its screen off) shows no pointer, and a visit there gives the pointer
    // back. Keep the saved order of paired docks even while their screens are unavailable.
    let permission = peersus_api::permission::Permission::new(plugin.as_str(), &input.capability);
    let mut placements = Vec::new();
    let mut displays = BTreeMap::new();
    for placement in &snapshot.settings.layout {
        let Some(peer) = snapshot
            .peers
            .iter()
            .find(|peer| peer.id.0 == placement.device)
        else {
            continue;
        };
        placements.push(placement.clone());
        if peer.online
            && !peer.sleeping
            && permission.as_ref().is_some_and(|permission| {
                peer.grants
                    .get(plugin)
                    .is_some_and(|grants| grants.allows(permission))
            })
            && let Some(said) = snapshot.peer_displays.get(&peer.id)
        {
            displays.insert(placement.device.clone(), said.clone());
        }
    }
    Route {
        carrier: Some(plugin.clone()),
        plane: Plane::new(&snapshot.displays, &placements, &displays),
        pressure,
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
                relative_to_local: None,
            }],
            &BTreeMap::from([("laptop".to_owned(), vec![screen("b", 0, 0, 1280, 800)])]),
        )
    }

    #[test]
    fn relative_crossing_maps_the_whole_edge_in_both_directions() {
        for (edge, local, remote, depart, outward, back, tangent) in [
            (
                Edge::Right,
                (1920, 1080),
                (1080, 2400),
                (1920., 270.),
                (0., 1200.),
                (-10., 0.),
                0.75,
            ),
            (
                Edge::Bottom,
                (2560, 1440),
                (1920, 1080),
                (640., 1440.),
                (960., 0.),
                (0., -10.),
                0.75,
            ),
        ] {
            let plane = Plane::new(
                &[screen("local", 0, 0, local.0, local.1)],
                &[Placement {
                    device: "peer".into(),
                    x: -37,
                    y: 22,
                    relative_to_local: Some(edge),
                }],
                &BTreeMap::from([(
                    "peer".into(),
                    vec![screen("remote", -200, -50, remote.0, remote.1)],
                )]),
            );
            let barriers = plane.barriers();
            assert_eq!(barriers.len(), 1);
            assert_eq!(
                barriers[0].line,
                match edge {
                    Edge::Right => (1920, 0, 1920, 1079),
                    Edge::Bottom => (0, 1440, 2559, 1440),
                    _ => unreachable!(),
                }
            );
            let entered = plane.enter(edge, depart).unwrap();
            let vertical = edge == Edge::Right;
            let check = |visit: &Visit, fraction: f32| {
                let (x, y) = visit.share(&plane).unwrap();
                let actual = if vertical { y } else { x };
                assert!((actual - fraction).abs() < 1e-6);
            };
            check(&entered, 0.25);
            let mut unchanged = entered.clone();
            let Some(Step::Back { at }) = unchanged.moved(&plane, back.0, back.1, None) else {
                panic!("the relative edge did not return");
            };
            assert!(
                (if vertical {
                    at.1 - depart.1
                } else {
                    at.0 - depart.0
                })
                .abs()
                    < 1e-6
            );
            let mut moved = entered;
            assert!(matches!(
                moved.moved(&plane, outward.0, outward.1, None),
                Some(Step::There { .. })
            ));
            check(&moved, tangent);
            let Some(Step::Back { at }) = moved.moved(&plane, back.0, back.1, None) else {
                panic!("the relative edge did not return after motion");
            };
            let actual = if vertical {
                at.1 / f64::from(local.1)
            } else {
                at.0 / f64::from(local.0)
            };
            assert!((actual - f64::from(tangent)).abs() < 1e-6);
        }
    }

    #[test]
    fn dormant_relative_peer_cannot_remove_crossings_when_its_displays_arrive() {
        let local = [screen("local", 0, 0, 1920, 1080)];
        // The relative placement was saved while the phone had no displays. Its projected
        // desktop now overlaps the laptop's existing free placement on the right.
        let configuration = [
            Placement {
                device: "phone".into(),
                x: 0,
                y: 0,
                relative_to_local: Some(Edge::Right),
            },
            Placement {
                device: "laptop".into(),
                x: 1920,
                y: 100,
                relative_to_local: None,
            },
            Placement {
                device: "desk".into(),
                x: -1280,
                y: 0,
                relative_to_local: None,
            },
        ];
        let mut displays = BTreeMap::from([
            ("laptop".into(), vec![screen("laptop", 0, 0, 1280, 800)]),
            ("desk".into(), vec![screen("desk", 0, 0, 1280, 800)]),
        ]);
        let before = Plane::new(&local, &configuration, &displays);
        let laptop = before.enter(Edge::Right, (1919., 500.)).unwrap();
        assert_eq!(laptop.device, "laptop");
        assert_eq!(before.enter(Edge::Left, (0., 500.)).unwrap().device, "desk");
        assert_eq!(before.barriers().len(), 2);

        displays.insert("phone".into(), vec![screen("phone", 0, 0, 1080, 2400)]);
        let after = Plane::new(&local, &configuration, &displays);
        assert_eq!(
            after.barriers(),
            before.barriers(),
            "the waking dock cannot take any existing barrier"
        );
        assert!(
            after.keeps(&laptop),
            "an existing visit must not be terminated by the conflict"
        );
        assert_eq!(
            after.enter(Edge::Right, (1919., 500.)).unwrap().device,
            "laptop"
        );
        assert_eq!(after.enter(Edge::Left, (0., 500.)).unwrap().device, "desk");
    }

    #[test]
    fn a_free_crossing_keeps_its_side_even_outside_the_docks_projection() {
        let local = [
            screen("wide", 0, 0, 200, 100),
            screen("narrow", 0, 100, 100, 100),
        ];
        let placements = [
            Placement {
                device: "right-dock".into(),
                x: 0,
                y: 0,
                relative_to_local: Some(Edge::Right),
            },
            Placement {
                device: "top-dock".into(),
                x: 0,
                y: 0,
                relative_to_local: Some(Edge::Top),
            },
            Placement {
                device: "free".into(),
                x: 100,
                y: 130,
                relative_to_local: None,
            },
        ];
        let displays = BTreeMap::from([
            ("free".into(), vec![screen("free", 0, 0, 50, 50)]),
            ("right-dock".into(), vec![screen("right", 0, 0, 100, 100)]),
            ("top-dock".into(), vec![screen("top", 0, 0, 100, 100)]),
        ]);
        let before = Plane::new(&local, &placements[1..], &displays);
        let plane = Plane::new(&local, &placements, &displays);
        // The right dock draws from x=200, entirely clear of the free desktop at x=100..150.
        // It still cannot take the right edge of the narrower local screen where the free route is.
        assert_eq!(plane.barriers(), before.barriers());
        assert_eq!(
            plane.enter(Edge::Right, (99., 150.)).unwrap().device,
            "free"
        );
        assert!(plane.enter(Edge::Right, (199., 50.)).is_none());
        assert_eq!(
            plane.enter(Edge::Top, (50., 0.)).unwrap().device,
            "top-dock"
        );
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
            edge_pressure: peersus_api::contributions::EdgePressure::Off {},
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
        use peersus_api::contributions::{EdgePressure, Input, Kind, Scope, Setting, Source};
        use peersus_core::{
            model::{PluginInfo, PluginStatus, Snapshot, Standing},
            plugins::{PluginId, Value},
        };
        let plugin = PluginId::new("carrier").unwrap();
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
            relative_to_local: None,
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
        let contributions = &mut snapshot.plugins[0].manifest.contributions;
        let setting = Setting {
            id: "push-needed".into(),
            label: "Push needed".into(),
            platforms: vec![peersus_api::platform::OperatingSystem::Linux],
            help: None,
            scope: Scope::Global,
            kind: Kind::Number {
                min: 0,
                max: 200,
                default: 80,
            },
            public_read: None,
        };
        contributions.settings.push(setting.clone());
        contributions.input = Some(Input {
            source: Source {
                service: "transport".into(),
                method: "send".into(),
            },
            capability: "transport.use".into(),
            edge_pressure: EdgePressure::Setting {
                id: setting.id.clone(),
            },
        });
        assert!(route(&snapshot).plane.barriers().is_empty());
        let permission =
            peersus_api::permission::Permission::new(plugin.as_str(), "transport.use").unwrap();
        let grants = serde_json::from_value(serde_json::json!([permission])).unwrap();
        snapshot.grants.insert(plugin.clone(), grants);
        assert!(route(&snapshot).plane.barriers().is_empty());
        snapshot.peers[0].grants = snapshot.grants.clone();
        let default = route(&snapshot);
        assert_eq!(default.carrier, Some(plugin.clone()));
        assert_eq!(default.plane.barriers().len(), 1);
        assert_eq!(default.pressure, NonZeroU16::new(80));
        let save = |snapshot: &mut Snapshot, carrier: PluginId, amount| {
            snapshot
                .settings
                .plugins
                .entry(carrier)
                .or_default()
                .set(
                    std::slice::from_ref(&setting),
                    Scope::Global,
                    &setting.id,
                    Value::Number(amount),
                )
                .unwrap();
        };
        save(&mut snapshot, PluginId::new("another").unwrap(), 17);
        assert_eq!(route(&snapshot).pressure, NonZeroU16::new(80));
        save(&mut snapshot, plugin.clone(), 120);
        assert_eq!(route(&snapshot).pressure, NonZeroU16::new(120));
        save(&mut snapshot, plugin, 0);
        assert_eq!(route(&snapshot).pressure, None);
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
            matches!(visit.moved(&plane, 640., 0., None), Some(Step::There { x, .. }) if (x - 0.5).abs() < 0.01)
        );
        // Up past the laptop's top: held there, it does not lead anywhere.
        assert!(
            matches!(visit.moved(&plane, 0., -5000., None), Some(Step::There { y, .. }) if y == 0.)
        );
        // Back left through the edge it came by: let go on this screen inside its right edge, as far in as it was pushed (bounded).
        let Some(Step::Back { at }) = visit.moved(&plane, -5000., 0., None) else {
            panic!("did not come back");
        };
        assert!(
            (at.0 - (1918. - RETURN_DEPTH.1)).abs() < 1. && (at.1 - 100.).abs() < 1.,
            "{at:?}"
        );
        // A small push is let go at the least depth, never at the barrier itself.
        let mut again = plane.enter(Edge::Right, (1919., 500.)).unwrap();
        let Some(Step::Back { at }) = again.moved(&plane, -3., 0., None) else {
            panic!("did not come back");
        };
        assert!((at.0 - (1918. - RETURN_DEPTH.0)).abs() < 1., "{at:?}");
    }

    #[test]
    fn return_pressure_counts_only_overshoot_and_crosses_on_the_threshold() {
        let plane = beside();
        let pressure = NonZeroU16::new(80);
        let mut visit = plane.enter(Edge::Right, (1919., 500.)).unwrap();
        assert!(matches!(
            visit.moved(&plane, 199., 0., pressure),
            Some(Step::There { .. })
        ));
        // Two hundred units to the edge are not part of the push.
        for dx in [-208., -31., 10., -50.] {
            assert!(matches!(
                visit.moved(&plane, dx, 0., pressure),
                Some(Step::There { x: 0., .. })
            ));
        }
        assert!(matches!(
            visit.moved(&plane, -1., 0., pressure),
            Some(Step::Back { .. })
        ));
    }

    #[test]
    fn return_pressure_does_not_arm_on_a_first_sample_sliding_along_the_edge() {
        let plane = beside();
        let amount = NonZeroU16::new(80);
        let mut visit = plane.enter(Edge::Right, (1919., 500.)).unwrap();
        // 79 outward units and over 290 sideways after reaching the boundary: too sideways.
        for (dx, dy) in [(-80., 300.), (-1., 0.)] {
            assert!(matches!(
                visit.moved(&plane, dx, dy, amount),
                Some(Step::There { .. })
            ));
        }
        assert!(matches!(
            visit.moved(&plane, -79., 0., amount),
            Some(Step::Back { .. })
        ));
    }

    #[test]
    fn return_pressure_does_not_count_sideways_motion_before_the_boundary() {
        let plane = beside();
        let amount = NonZeroU16::new(80);
        let mut visit = plane.enter(Edge::Right, (1919., 500.)).unwrap();
        assert!(matches!(
            visit.moved(&plane, 239., 0., amount),
            Some(Step::There { .. })
        ));
        // 79 outward units but only ~149 sideways after the boundary, not the full 600.
        assert!(matches!(
            visit.moved(&plane, -319., 600., amount),
            Some(Step::There { .. })
        ));
        assert!(matches!(
            visit.moved(&plane, -1., 0., amount),
            Some(Step::Back { .. })
        ));
    }

    #[test]
    fn returning_after_backing_in_does_not_continue_the_previous_push() {
        let plane = beside();
        let pressure = NonZeroU16::new(80);
        let mut visit = plane.enter(Edge::Right, (1919., 500.)).unwrap();
        assert!(matches!(
            visit.moved(&plane, -61., 0., pressure),
            Some(Step::There { x: 0., .. })
        ));
        // Sixty units out, then seventy-six back: leave the edge at the exact slack.
        assert!(matches!(
            visit.moved(&plane, 76., 0., pressure),
            Some(Step::There { x, .. }) if x > 0.
        ));
        let approach = visit.at.0 - 1920.;
        assert!(matches!(
            visit.moved(&plane, -approach - 79., 0., pressure),
            Some(Step::There { x: 0., .. })
        ));
        assert!(matches!(
            visit.moved(&plane, -1., 0., pressure),
            Some(Step::Back { .. })
        ));
    }

    #[test]
    fn an_abandoned_return_push_does_not_release_or_add_to_the_next() {
        let plane = beside();
        let pressure = NonZeroU16::new(80);
        let mut visit = plane.enter(Edge::Right, (1919., 500.)).unwrap();
        assert!(matches!(
            visit.moved(&plane, -75., 0., pressure),
            Some(Step::There { .. })
        ));
        visit.clear_return();
        assert!(matches!(
            visit.moved(&plane, -79., 0., pressure),
            Some(Step::There { x: 0., .. })
        ));
        assert!(matches!(
            visit.moved(&plane, -1., 12., pressure),
            Some(Step::Back { .. })
        ));
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
                relative_to_local: None,
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
                relative_to_local: None,
            }],
            &BTreeMap::from([("phone".to_owned(), vec![screen("p", 0, 0, 1080, 2400)])]),
        );
        let mut visit = plane.enter(Edge::Right, (1919., 500.)).unwrap();
        assert!(matches!(
            visit.moved(&plane, 0., 1500., NonZeroU16::new(80)),
            Some(Step::There { .. })
        ));
        let Some(Step::Back { at }) = visit.moved(&plane, -5000., 0., NonZeroU16::new(80)) else {
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
                    relative_to_local: None,
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
                match visit.moved(&plane, 1., 0., None) {
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
                relative_to_local: None,
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
                relative_to_local: None,
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
