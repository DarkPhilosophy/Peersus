//! Screen layout: every device's displays placed on one shared plane, and the crossings that follow
//! from where they touch. The model of Syntra's layout, kept to what it decides.
//!
//! The person edits placements; crossings are derived, never stored. Two displays of different
//! devices that share part of an edge form a crossing in each direction, and a pointer leaving one at
//! a given height enters the other at the same height on the plane. A display beside a taller one
//! therefore only covers the part of the edge it actually faces.
//! A relative placement instead docks a desktop to one side. Its drawing is scaled uniformly;
//! pointer routing uses the same fraction along the two real desktops' sides.
use serde::{Deserialize, Serialize};

/// Edges closer than this many pixels count as touching, so a placement off by a rounding pixel still connects.
pub const TOUCH_TOLERANCE: i64 = 2;
/// Largest distance from the plane origin any edge may have. Far beyond real desktops, yet small enough that sums never overflow.
pub const MAX_COORDINATE: i64 = 1 << 40;
/// Most displays one device may say it has.
pub const MAX_DISPLAYS: usize = 16;
/// Most devices one layout may place.
pub const MAX_PLACED: usize = 64;

/// One display of a device, in that device's own coordinates (its desktop, in logical pixels).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Display {
    /// Stable on that device (a connector name or an index).
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Display {
    /// Within the bounds a device may claim: some area, a short printable name, sizes a screen has.
    pub fn valid(&self) -> bool {
        const MOST_SIDE: u32 = 1 << 16;
        (1..=MOST_SIDE).contains(&self.width)
            && (1..=MOST_SIDE).contains(&self.height)
            && !self.id.is_empty()
            && self.id.len() <= 64
            && !self.id.chars().any(char::is_control)
    }
}

/// Whether what a device says of its displays can be believed: a bounded list of valid displays.
pub fn valid_displays(displays: &[Display]) -> bool {
    displays.len() <= MAX_DISPLAYS && displays.iter().all(Display::valid)
}

/// Where a device's desktop sits on the shared plane: the top left of its desktop's bounding box.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub device: String,
    pub x: i64,
    pub y: i64,
    /// Dock this desktop to a side of the local desktop, ignoring `x` and `y` until undocked.
    #[serde(
        default,
        deserialize_with = "relative_side",
        skip_serializing_if = "Option::is_none"
    )]
    pub relative_to_local: Option<Edge>,
}

fn relative_side<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Edge>, D::Error> {
    // Only absence selects free placement; a present value has to name a side.
    Edge::deserialize(deserializer).map(Some)
}

/// One display of one device, placed on the shared plane in pixels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Monitor {
    pub device: String,
    pub id: String,
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}

impl Monitor {
    // Saturating: placements come from people and peers, so no coordinate may overflow.
    pub fn right(&self) -> i64 {
        self.x.saturating_add(i64::from(self.width))
    }
    pub fn bottom(&self) -> i64 {
        self.y.saturating_add(i64::from(self.height))
    }
}

/// The monitors of `device` placed with its desktop's top left at `placement`.
pub fn place(device: &str, displays: &[Display], placement: (i64, i64)) -> Vec<Monitor> {
    let left = displays.iter().map(|d| i64::from(d.x)).min().unwrap_or(0);
    let top = displays.iter().map(|d| i64::from(d.y)).min().unwrap_or(0);
    displays
        .iter()
        .map(|d| Monitor {
            device: device.to_owned(),
            id: d.id.clone(),
            x: placement.0.saturating_add(i64::from(d.x) - left),
            y: placement.1.saturating_add(i64::from(d.y) - top),
            width: d.width,
            height: d.height,
        })
        .collect()
}

/// The size of a device's desktop: the bounding box of its displays.
pub fn extent(displays: &[Display]) -> (u32, u32) {
    let left = displays.iter().map(|d| i64::from(d.x)).min().unwrap_or(0);
    let top = displays.iter().map(|d| i64::from(d.y)).min().unwrap_or(0);
    let right = displays
        .iter()
        .map(|d| i64::from(d.x) + i64::from(d.width))
        .max()
        .unwrap_or(0);
    let bottom = displays
        .iter()
        .map(|d| i64::from(d.y) + i64::from(d.height))
        .max()
        .unwrap_or(0);
    (
        u32::try_from(right - left).unwrap_or(0),
        u32::try_from(bottom - top).unwrap_or(0),
    )
}

/// The shared-plane rectangles for a desktop docked to `side`. Scale the whole desktop, not each
/// display to a separate size: side-to-side uses the ratio of heights, top/bottom the ratio of
/// widths. A desktop with no usable bounds has no projection and no route.
pub fn place_relative(
    device: &str,
    local: &[Display],
    displays: &[Display],
    side: Edge,
) -> Result<Vec<Monitor>, LayoutError> {
    let (lw, lh) = extent(local);
    let (rw, rh) = extent(displays);
    if [lw, lh, rw, rh].contains(&0)
        || local
            .iter()
            .chain(displays)
            .any(|d| d.width == 0 || d.height == 0)
    {
        return Ok(Vec::new());
    }
    let ratio = match side {
        Edge::Left | Edge::Right => f64::from(lh) / f64::from(rh),
        Edge::Top | Edge::Bottom => f64::from(lw) / f64::from(rw),
    };
    let scaled = |n: f64| (n * ratio).round();
    let mut monitors = Vec::with_capacity(displays.len());
    for d in displays {
        let width = scaled(f64::from(d.width)).max(1.);
        let height = scaled(f64::from(d.height)).max(1.);
        if width > f64::from(u32::MAX) || height > f64::from(u32::MAX) {
            return Err(LayoutError::OutOfRange(MonitorRef {
                device: device.into(),
                id: d.id.clone(),
            }));
        }
        monitors.push(Monitor {
            device: device.into(),
            id: d.id.clone(),
            x: scaled(f64::from(d.x)) as i64,
            y: scaled(f64::from(d.y)) as i64,
            width: width as u32,
            height: height as u32,
        });
    }
    let left = monitors.iter().map(|m| m.x).min().unwrap_or(0);
    let top = monitors.iter().map(|m| m.y).min().unwrap_or(0);
    let width = monitors
        .iter()
        .map(Monitor::right)
        .max()
        .unwrap_or(0)
        .saturating_sub(left);
    let height = monitors
        .iter()
        .map(Monitor::bottom)
        .max()
        .unwrap_or(0)
        .saturating_sub(top);
    let (x, y) = match side {
        Edge::Left => (-width, 0),
        Edge::Right => (i64::from(lw), 0),
        Edge::Top => (0, -height),
        Edge::Bottom => (0, i64::from(lh)),
    };
    for m in &mut monitors {
        m.x = m.x.saturating_sub(left).saturating_add(x);
        m.y = m.y.saturating_sub(top).saturating_add(y);
    }
    Ok(monitors)
}

/// Which side of a display the pointer leaves through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// The side the pointer arrives through on the display it enters.
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }
}

/// A display named by device and id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MonitorRef {
    pub device: String,
    pub id: String,
}

impl From<&Monitor> for MonitorRef {
    fn from(monitor: &Monitor) -> Self {
        Self {
            device: monitor.device.clone(),
            id: monitor.id.clone(),
        }
    }
}

/// Leaving `from` through `edge` within `from_span` enters `to` within `to_span`. Spans are fractions
/// `[start, end]` along the edge, from the top (vertical edges) or the left (horizontal edges).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Crossing {
    pub from: MonitorRef,
    pub edge: Edge,
    pub from_span: (f64, f64),
    pub to: MonitorRef,
    pub to_span: (f64, f64),
}

impl Crossing {
    /// Where on the entered edge a pointer leaving at `along` arrives, or `None` outside the part of
    /// the edge facing `to`.
    pub fn map(&self, along: f64) -> Option<f64> {
        let (start, end) = self.from_span;
        if !(start..=end).contains(&along) || end <= start {
            return None;
        }
        let t = (along - start) / (end - start);
        let (to_start, to_end) = self.to_span;
        Some(to_start + t * (to_end - to_start))
    }
}

/// Every crossing between displays of different devices.
pub fn crossings(monitors: &[Monitor]) -> Vec<Crossing> {
    let mut found = Vec::new();
    for (index, a) in monitors.iter().enumerate() {
        for b in &monitors[index + 1..] {
            if a.device == b.device {
                continue;
            }
            for (from, to) in [(a, b), (b, a)] {
                found.extend(crossing(from, to));
            }
        }
    }
    found
}

fn crossing(from: &Monitor, to: &Monitor) -> Option<Crossing> {
    let touches = |a: i64, b: i64| a.abs_diff(b) <= TOUCH_TOLERANCE.unsigned_abs();
    let edge = if touches(from.right(), to.x) {
        Edge::Right
    } else if touches(from.x, to.right()) {
        Edge::Left
    } else if touches(from.bottom(), to.y) {
        Edge::Bottom
    } else if touches(from.y, to.bottom()) {
        Edge::Top
    } else {
        return None;
    };
    let ((from_start, from_len), (to_start, to_len)) = match edge {
        Edge::Left | Edge::Right => (
            (from.y, i64::from(from.height)),
            (to.y, i64::from(to.height)),
        ),
        Edge::Top | Edge::Bottom => ((from.x, i64::from(from.width)), (to.x, i64::from(to.width))),
    };
    let start = from_start.max(to_start);
    let end = from_start
        .saturating_add(from_len)
        .min(to_start.saturating_add(to_len));
    if end <= start {
        // Corners touching, or edges on one line without overlap.
        return None;
    }
    let fraction = |at: i64, origin: i64, len: i64| at.abs_diff(origin) as f64 / len as f64;
    Some(Crossing {
        from: from.into(),
        edge,
        from_span: (
            fraction(start, from_start, from_len),
            fraction(end, from_start, from_len),
        ),
        to: to.into(),
        to_span: (
            fraction(start, to_start, to_len),
            fraction(end, to_start, to_len),
        ),
    })
}

/// Why a relative placement is inactive, without identifying any other device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockError {
    SideInUse,
    Overlap,
    InvalidGeometry,
}

/// Attach only to sides not used by free crossings. If saved relative placements share a
/// side, the first one in stored order reserves it even without displays, so reconnecting
/// cannot evict another relative route.
pub struct Docking<'a> {
    local: &'a [Display],
    free: &'a [Monitor],
    occupied: [bool; 4],
}

impl<'a> Docking<'a> {
    pub fn new(
        local: &'a [Display],
        free: &'a [Monitor],
        crossings: &[Crossing],
        root: &str,
    ) -> Self {
        let mut occupied = [false; 4];
        for crossing in crossings {
            let side = if crossing.from.device == root {
                crossing.edge
            } else if crossing.to.device == root {
                crossing.edge.opposite()
            } else {
                continue;
            };
            // Relative routing takes the whole side, not just its projected rectangle.
            occupied[side as usize] = true;
        }
        Self {
            local,
            free,
            occupied,
        }
    }

    /// Must be called in stored placement order. An empty projection is a dormant dock.
    pub fn project(
        &mut self,
        device: &str,
        displays: &[Display],
        side: Edge,
    ) -> Result<Vec<Monitor>, DockError> {
        if std::mem::replace(&mut self.occupied[side as usize], true) {
            return Err(DockError::SideInUse);
        }
        if self.local.is_empty() || displays.is_empty() {
            return Ok(Vec::new());
        }
        let projected = place_relative(device, self.local, displays, side)
            .map_err(|_| DockError::InvalidGeometry)?;
        if projected.is_empty() {
            return Err(DockError::InvalidGeometry);
        }
        validate_geometry(&projected).map_err(|error| match error {
            LayoutError::Overlap(..) => DockError::Overlap,
            _ => DockError::InvalidGeometry,
        })?;
        if projected
            .iter()
            .any(|monitor| self.free.iter().any(|other| overlaps(monitor, other)))
        {
            return Err(DockError::Overlap);
        }
        Ok(projected)
    }
}

/// Why a layout cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// A display has no area.
    Empty(MonitorRef),
    /// Two displays cover the same place: the pointer could be on both.
    Overlap(MonitorRef, MonitorRef),
    /// A device does not touch this one, so the pointer never reaches it. Still a valid choice: the
    /// device is simply out of reach, as if left out.
    Detached(String),
    /// A display lies beyond [`MAX_COORDINATE`].
    OutOfRange(MonitorRef),
}

impl LayoutError {
    /// Whether the layout cannot be kept at all. A detached device is a choice; the others are not.
    pub fn blocks_saving(&self) -> bool {
        !matches!(self, Self::Detached(_))
    }
}

fn overlaps(a: &Monitor, b: &Monitor) -> bool {
    let overlap_x = a.right().min(b.right()).saturating_sub(a.x.max(b.x));
    let overlap_y = a.bottom().min(b.bottom()).saturating_sub(a.y.max(b.y));
    // A tolerance-sized sliver is how touching edges look once rounded.
    overlap_x > TOUCH_TOLERANCE && overlap_y > TOUCH_TOLERANCE
}

/// Checks display bounds and overlap, independently of where the pointer can cross.
pub fn validate_geometry(monitors: &[Monitor]) -> Result<(), LayoutError> {
    for monitor in monitors {
        if monitor.width == 0 || monitor.height == 0 {
            return Err(LayoutError::Empty(monitor.into()));
        }
        let in_range = |at: i64| (-MAX_COORDINATE..=MAX_COORDINATE).contains(&at);
        if !(in_range(monitor.x)
            && in_range(monitor.y)
            && in_range(monitor.right())
            && in_range(monitor.bottom()))
        {
            return Err(LayoutError::OutOfRange(monitor.into()));
        }
    }
    for (index, a) in monitors.iter().enumerate() {
        for b in &monitors[index + 1..] {
            if overlaps(a, b) {
                return Err(LayoutError::Overlap(a.into(), b.into()));
            }
        }
    }
    Ok(())
}

/// Checks a layout: displays have area and lie in range, none overlap, and with more than one device
/// every device touches another.
pub fn validate(monitors: &[Monitor]) -> Result<(), LayoutError> {
    validate_geometry(monitors)?;
    let mut devices: Vec<&str> = monitors.iter().map(|m| m.device.as_str()).collect();
    devices.sort_unstable();
    devices.dedup();
    if devices.len() > 1 {
        let linked = crossings(monitors);
        if let Some(lonely) = devices
            .iter()
            .find(|device| !linked.iter().any(|c| c.from.device == **device))
        {
            return Err(LayoutError::Detached((*lonely).to_owned()));
        }
    }
    Ok(())
}

/// [`validate`], plus: every other device touches `root` directly. The pointer starts on the root
/// (this computer) and crosses one edge at a time from it, so a device that only touches another
/// remote device is never reached.
pub fn validate_from(monitors: &[Monitor], root: &str) -> Result<(), LayoutError> {
    validate(monitors)?;
    let linked = crossings(monitors);
    let mut devices: Vec<&str> = monitors
        .iter()
        .map(|m| m.device.as_str())
        .filter(|d| *d != root)
        .collect();
    devices.sort_unstable();
    devices.dedup();
    match devices.into_iter().find(|d| {
        !linked
            .iter()
            .any(|c| c.from.device == root && c.to.device == *d)
    }) {
        Some(lonely) => Err(LayoutError::Detached(lonely.to_owned())),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(device: &str, x: i64, y: i64, width: u32, height: u32) -> Monitor {
        Monitor {
            device: device.into(),
            id: "0".into(),
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn relative_desktop_scales_every_display_by_the_touching_axis() {
        let display = |id: &str, x, y, width, height| Display {
            id: id.into(),
            x,
            y,
            width,
            height,
        };
        let local = [display("desktop", 0, 0, 2560, 1440)];
        let phone = [display("portrait", 0, 0, 1080, 2400)];
        let mut want = monitor("phone", 2560, 0, 648, 1440);
        want.id = "portrait".into();
        assert_eq!(
            place_relative("phone", &local, &phone, Edge::Right),
            Ok(vec![want])
        );

        let local = [display("desktop", 0, 0, 1920, 1080)];
        let peer = [
            display("main", -1280, 0, 2560, 1440),
            display("second", 1280, 180, 1280, 720),
        ];
        let actual = place_relative("peer", &local, &peer, Edge::Top).unwrap();
        assert_eq!(
            actual
                .iter()
                .map(|m| (m.x, m.y, m.width, m.height))
                .collect::<Vec<_>>(),
            [(0, -720, 1280, 720), (1280, -630, 640, 360)]
        );
    }

    #[test]
    fn relative_placement_omits_free_mode_and_rejects_unknown_present_values() {
        let old = r#"{"device":"paired","x":1920,"y":-100}"#;
        let placement: Placement = serde_json::from_str(old).unwrap();
        assert_eq!(serde_json::to_string(&placement).unwrap(), old);
        for value in [r#""diagonal""#, "null", "true"] {
            let bad =
                format!(r#"{{"device":"paired","x":1920,"y":-100,"relative_to_local":{value}}}"#);
            assert!(serde_json::from_str::<Placement>(&bad).is_err());
        }
    }

    #[test]
    fn relative_placement_without_usable_displays_has_no_monitors() {
        let display = Display {
            id: "display".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let zero = Display {
            height: 0,
            ..display.clone()
        };
        assert!(
            place_relative("peer", std::slice::from_ref(&display), &[], Edge::Right)
                .unwrap()
                .is_empty()
        );
        assert!(
            place_relative("peer", &[zero], &[display], Edge::Bottom)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn dormant_dock_reserves_its_side_in_stored_order() {
        let local = [Display {
            id: "screen".into(),
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        }];
        let free = place("local", &local, (0, 0));
        for first_displays in [&[][..], &local[..]] {
            let mut docks = Docking::new(&local, &free, &[], "local");
            assert_eq!(
                docks
                    .project("first", first_displays, Edge::Right)
                    .unwrap()
                    .len(),
                first_displays.len()
            );
            assert_eq!(
                docks.project("second", &local, Edge::Right),
                Err(DockError::SideInUse)
            );
            assert!(
                !docks
                    .project("other", &local, Edge::Left)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn each_dock_has_its_own_geometry_check_even_without_a_free_crossing() {
        let local = [Display {
            id: "screen".into(),
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        }];
        let mut free = place("local", &local, (0, 0));
        free.push(monitor("detached", 20, -80, 30, 30));
        let crossings = crossings(&free);
        assert!(crossings.is_empty());
        let mut docks = Docking::new(&local, &free, &crossings, "local");
        assert_eq!(
            docks.project("top", &local, Edge::Top),
            Err(DockError::Overlap)
        );
        let invalid = [Display {
            width: 0,
            ..local[0].clone()
        }];
        assert_eq!(
            docks.project("invalid", &invalid, Edge::Left),
            Err(DockError::InvalidGeometry)
        );
        assert!(
            !docks
                .project("right", &local, Edge::Right)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn offset_neighbours_cross_where_they_face_each_other() {
        let msi = monitor("msi", 0, 0, 2560, 1600);
        let hp = monitor("hp", -1920, 200, 1920, 1080);
        let all = crossings(&[msi, hp]);
        assert_eq!(all.len(), 2);
        let out = all.iter().find(|c| c.from.device == "msi").unwrap();
        assert_eq!(out.edge, Edge::Left);
        assert_eq!(out.from_span, (0.125, 0.8));
        assert_eq!(out.to_span, (0.0, 1.0));
        let entered = out.map(0.5).unwrap();
        assert!((entered - 600.0 / 1080.0).abs() < 1e-9);
        assert_eq!(out.map(0.05), None);
        let back = all.iter().find(|c| c.from.device == "hp").unwrap();
        assert_eq!(back.edge, Edge::Right);
        assert!((back.map(entered).unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_rounding_gap_still_touches_but_corners_do_not() {
        let a = monitor("a", 0, 0, 100, 100);
        assert_eq!(
            crossings(&[a.clone(), monitor("b", 102, 0, 100, 100)]).len(),
            2
        );
        assert!(crossings(&[a.clone(), monitor("b", 103, 0, 100, 100)]).is_empty());
        assert!(crossings(&[a, monitor("b", 100, 100, 100, 100)]).is_empty());
    }

    #[test]
    fn displays_of_one_device_never_cross_to_each_other() {
        let left = monitor("msi", 0, 0, 100, 100);
        let mut right = monitor("msi", 100, 0, 100, 100);
        right.id = "1".into();
        assert!(crossings(&[left, right]).is_empty());
    }

    #[test]
    fn vertical_neighbours_cross_through_top_and_bottom() {
        let desk = monitor("desk", 0, 0, 1000, 500);
        let laptop = monitor("laptop", 250, 500, 500, 300);
        let down = crossings(&[desk, laptop])
            .into_iter()
            .find(|c| c.from.device == "desk")
            .unwrap();
        assert_eq!(down.edge, Edge::Bottom);
        assert_eq!(down.from_span, (0.25, 0.75));
        assert_eq!(down.map(0.5), Some(0.5));
    }

    #[test]
    fn validation_refuses_overlaps_and_unreachable_devices() {
        let a = monitor("a", 0, 0, 100, 100);
        assert!(matches!(
            validate(&[a.clone(), monitor("b", 50, 50, 100, 100)]),
            Err(LayoutError::Overlap(..))
        ));
        assert_eq!(
            validate(&[a.clone(), monitor("b", 500, 0, 100, 100)]),
            Err(LayoutError::Detached("a".into()))
        );
        assert!(matches!(
            validate(&[monitor("a", 0, 0, 0, 100)]),
            Err(LayoutError::Empty(_))
        ));
        assert_eq!(validate(&[a, monitor("b", 101, 30, 100, 100)]), Ok(()));
    }

    #[test]
    fn a_device_behind_another_is_detached_from_this_one() {
        let row = |device: &str, x: i64, width: u32| monitor(device, x, 0, width, 100);
        let chain = [
            row("local", 0, 100),
            row("hp", -100, 100),
            row("rog", -150, 50),
        ];
        assert!(validate(&chain).is_ok());
        assert_eq!(
            validate_from(&chain, "local"),
            Err(LayoutError::Detached("rog".into()))
        );
        let both = [
            row("local", 0, 100),
            row("hp", -100, 100),
            row("rog", 100, 50),
        ];
        assert_eq!(validate_from(&both, "local"), Ok(()));
    }

    #[test]
    fn extreme_coordinates_do_not_overflow() {
        let far = |device: &str, x: i64, y: i64| Monitor {
            device: device.into(),
            id: "0".into(),
            x,
            y,
            width: u32::MAX,
            height: u32::MAX,
        };
        let all = [far("a", i64::MAX, i64::MAX), far("b", i64::MIN, i64::MIN)];
        let _ = crossings(&all);
        assert!(matches!(validate(&all), Err(LayoutError::OutOfRange(_))));
    }

    #[test]
    fn a_device_s_displays_keep_their_arrangement_where_it_is_placed() {
        let displays = [
            Display {
                id: "a".into(),
                x: -1920,
                y: 100,
                width: 1920,
                height: 1080,
            },
            Display {
                id: "b".into(),
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            },
        ];
        let placed = place("pc", &displays, (500, 500));
        assert_eq!((placed[0].x, placed[0].y), (500, 600));
        assert_eq!((placed[1].x, placed[1].y), (2420, 500));
        assert_eq!(extent(&displays), (4480, 1440));
        assert!(valid_displays(&displays));
        assert!(!valid_displays(&[Display {
            id: "x\n".into(),
            x: 0,
            y: 0,
            width: 1,
            height: 1
        }]));
    }
}
