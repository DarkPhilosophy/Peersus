//! The extension's desktop editor. The host only provides scoped geometry and generic elements.
use crate::{
    State,
    host::{Blocked, Wire},
    remote::{Refusal, answer},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
const INITIAL_HEIGHT: f64 = 360.;
// The arrangement service bounds every shared-plane edge (peersus-core::layout::MAX_COORDINATE).
const MAX_PLANE_AT: i64 = 1 << 40;
// Absolute box offsets accepted by peersus-api's page element validator, in milli-pixels.
const MAX_PAGE_AT: i32 = 16_384_000;
const MAX_DEVICE_ROWS: usize = 4;
const MENU_HEADER: f64 = 52.;
const MENU_ROW: f64 = 48.;
const MENU_PAGING: f64 = 44.;

fn placement_fits(at: [i64; 2], extent: [i64; 2]) -> bool {
    at.into_iter()
        .zip(extent)
        .all(|(start, span)| (-MAX_PLANE_AT..=MAX_PLANE_AT - span).contains(&start))
}

fn page_coordinate(value: &Value) -> Result<f64, &'static str> {
    value
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .map(|n| f64::from(n) / 1000.)
        .ok_or(answer::NOT_UNDERSTOOD)
}

#[derive(Clone, Deserialize, PartialEq)]
struct Display {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}
impl Side {
    const ALL: [(Self, &'static str, &'static str); 4] = [
        (Self::Left, "relative-left", "Left"),
        (Self::Right, "relative-right", "Right"),
        (Self::Top, "relative-top", "Top"),
        (Self::Bottom, "relative-bottom", "Bottom"),
    ];
}

#[derive(Clone, Deserialize, PartialEq)]
struct Item {
    device: String,
    name: String,
    displays: Vec<Display>,
    at: Option<[i64; 2]>,
    local: bool,
    #[serde(default)]
    relative_to_local: Option<Side>,
    // Host-derived rectangles in display order; `at` stays the remembered free coordinates.
    #[serde(default)]
    relative_displays: Vec<[i64; 4]>,
}
#[derive(Clone)]
struct Rect {
    device: usize,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}
impl Rect {
    fn right(&self) -> i64 {
        self.x + self.w
    }
    fn bottom(&self) -> i64 {
        self.y + self.h
    }
}
#[derive(Clone)]
enum Grab {
    Pan {
        from: [f64; 2],
        was: [f64; 2],
    },
    Device {
        index: usize,
        from: [f64; 2],
        was: [i64; 2],
    },
    Menu {
        index: usize,
        from: [f64; 2],
        was: Option<[i64; 2]>,
        moved: bool,
    },
    Relative {
        index: usize,
        from: [f64; 2],
        side: Option<Side>,
        moved: bool,
    },
}
#[derive(Default)]
pub struct Ui {
    items: Vec<Item>,
    expected: Value,
    zoom: Option<(f64, [f64; 2])>,
    grab: Option<Grab>,
    bounds: [f64; 4],
    refreshed: u64,
    error: Option<String>,
    /// Why the arrangement could not be read the last time it was asked for, said on the page instead of failing it.
    unseen: Option<String>,
    watch_block: bool,
    arrange_block: bool,
    needs_refresh: bool,
    /// The canvas of each page this is drawn on, by page: the same layout is shown in the settings
    /// and, when asked, as a page of its own, each of its own size.
    pages: std::collections::BTreeMap<String, [f64; 4]>,
    /// The page the gesture in progress is on.
    grab_page: Option<String>,
    choosing: bool,
    menu_page: usize,
    relative_menu: Option<String>,
    pointer: Option<[f64; 2]>,
    rendered_at: Option<u64>,
    menu_anchor: Option<[f64; 2]>,
}
/// What the page says when the person has not let Control see this computer's screens.
const NEEDS_SCREENS: &str = "To arrange devices here, allow Control to see this computer's screens in Settings, Permissions.";
const NEEDS_ARRANGE: &str =
    "Cannot save the arrangement: missing permission displays.arrange on this device.";
fn text(s: impl Into<String>) -> Value {
    json!({"k":"text","text":s.into(),"tone":"muted"})
}
fn px(n: f64) -> Value {
    json!({"unit":"px","value":n.round().clamp(0.,8192.) as u16})
}
fn fill() -> Value {
    json!({"unit":"percent","value":10000})
}
fn bx(id: Option<&str>, style: Value, children: Vec<Value>) -> Value {
    json!({"k":"box","id":id,"style":style,"children":children})
}
fn button(id: impl Into<String>, label: impl Into<String>) -> Value {
    json!({"k":"button","id":id.into(),"label":label.into()})
}
impl Item {
    fn screens(&self, index: usize) -> Vec<Rect> {
        let Some(at) = self.at else { return vec![] };
        if self.relative_to_local.is_some() {
            return self
                .relative_displays
                .iter()
                .map(|&[x, y, w, h]| Rect {
                    device: index,
                    x,
                    y,
                    w,
                    h,
                })
                .collect();
        }
        if self.displays.is_empty() {
            return vec![Rect {
                device: index,
                x: at[0],
                y: at[1],
                w: 1920,
                h: 1080,
            }];
        }
        let left = self.displays.iter().map(|d| d.x).min().unwrap_or(0);
        let top = self.displays.iter().map(|d| d.y).min().unwrap_or(0);
        self.displays
            .iter()
            .map(|d| Rect {
                device: index,
                x: at[0] + (i64::from(d.x) - i64::from(left)),
                y: at[1] + (i64::from(d.y) - i64::from(top)),
                w: i64::from(d.width),
                h: i64::from(d.height),
            })
            .collect()
    }
    fn drawn_bounds(&self) -> Option<([i64; 2], [i64; 2])> {
        let at = self.at?;
        if self.relative_to_local.is_none() {
            return Some((at, self.extent()));
        }
        let rects = &self.relative_displays;
        let x = rects.iter().map(|r| r[0]).min()?;
        let y = rects.iter().map(|r| r[1]).min()?;
        let right = rects.iter().map(|r| r[0] + r[2]).max()?;
        let bottom = rects.iter().map(|r| r[1] + r[3]).max()?;
        Some(([x, y], [right - x, bottom - y]))
    }

    fn placement(&self) -> Option<Value> {
        let [x, y] = self.at?;
        let mut row = json!({"device":self.device,"x":x,"y":y});
        if let Some(side) = self.relative_to_local {
            row["relative_to_local"] = json!(side);
        }
        Some(row)
    }
    fn extent(&self) -> [i64; 2] {
        if self.displays.is_empty() {
            return [1920, 1080];
        }
        let left = self
            .displays
            .iter()
            .map(|d| i64::from(d.x))
            .min()
            .unwrap_or(0);
        let top = self
            .displays
            .iter()
            .map(|d| i64::from(d.y))
            .min()
            .unwrap_or(0);
        [
            self.displays
                .iter()
                .map(|d| i64::from(d.x) + i64::from(d.width))
                .max()
                .unwrap_or(0)
                - left,
            self.displays
                .iter()
                .map(|d| i64::from(d.y) + i64::from(d.height))
                .max()
                .unwrap_or(0)
                - top,
        ]
    }
}
impl Ui {
    fn load(&mut self, value: Value) -> Result<(), &'static str> {
        let items: Vec<Item> =
            serde_json::from_value(value["devices"].clone()).map_err(|_| answer::NOT_UNDERSTOOD)?;
        if items.len() > 65
            || items.iter().any(|i| {
                i.displays.len() > 16
                    || if i.relative_to_local.is_some() {
                        i.local
                            || i.at.is_none_or(|at| !placement_fits(at, [0, 0]))
                            || (!i.relative_displays.is_empty()
                                && i.relative_displays.len() != i.displays.len())
                            || i.relative_displays.iter().any(|&[x, y, w, h]| {
                                w <= 0 || h <= 0 || !placement_fits([x, y], [w, h])
                            })
                    } else {
                        !i.relative_displays.is_empty()
                            || i.at.is_some_and(|at| !placement_fits(at, i.extent()))
                    }
            })
            || !value["placements"].is_array()
        {
            return Err(answer::NOT_UNDERSTOOD);
        }
        // Every paired device can be placed: one whose screens are not known yet is drawn at a
        // default size ("size not known yet") until it says, never left out of the editor.
        self.items = items;
        self.expected = value["placements"].clone();
        Ok(())
    }

    pub(crate) fn permission_changed(&mut self) {
        self.needs_refresh = true;
    }

    fn editable(&self) -> bool {
        !self.watch_block && !self.arrange_block
    }

    fn cancel_edit(&mut self) {
        let previous = match self.grab {
            Some(Grab::Device { index, was, .. }) => Some((index, Some(was))),
            Some(Grab::Menu { index, was, .. }) => Some((index, was)),
            _ => None,
        };
        if let Some((index, at)) = previous
            && let Some(item) = self.items.get_mut(index)
        {
            item.at = at;
        }
        self.grab = None;
        self.grab_page = None;
        self.choosing = false;
        self.menu_anchor = None;
        self.relative_menu = None;
    }

    fn sync_permissions<H: Wire>(
        &mut self,
        state: &mut State,
        host: &mut H,
        opening: bool,
    ) -> bool {
        let watch = state.missing_host("displays.watch").is_some();
        let arrange = state.missing_host("displays.arrange").is_some();
        let changed = (self.watch_block, self.arrange_block) != (watch, arrange);
        if changed {
            self.cancel_edit();
            self.error = None;
        }
        self.arrange_block = arrange;
        self.needs_refresh = false;
        if !opening && self.watch_block != watch {
            self.read_again(state, host);
        }
        changed
    }

    /// A refusal to read the arrangement belongs on the page, not in a failed page response.
    fn read_again<H: Wire>(&mut self, state: &mut State, host: &mut H) {
        let result = self.refresh(state, host);
        self.watch_block = matches!(
            &result,
            Err(Refusal::Blocked(Blocked::MissingPermission(_)))
        );
        if self.watch_block || result == Err(answer::DENIED.into()) {
            self.cancel_edit();
            self.items.clear();
            self.expected = Value::Null;
            self.error = None;
        }
        self.unseen = match result {
            Ok(()) => None,
            Err(Refusal::Other(answer::DENIED)) => Some(NEEDS_SCREENS.into()),
            Err(Refusal::Blocked(Blocked::MissingPermission(permission))) => Some(format!(
                "Cannot show the arrangement: missing permission {} on this device.",
                permission.name
            )),
            Err(Refusal::Blocked(Blocked::NotAccepted)) => Some(answer::NOT_UNDERSTOOD.into()),
            Err(Refusal::Other(reason)) => Some(reason.into()),
        };
    }
    fn refresh<H: Wire>(&mut self, state: &mut State, host: &mut H) -> Result<(), Refusal> {
        self.load(state.call(host, "peersus.displays", "inspect", json!({}))?)
            .map_err(Into::into)
    }
    fn save<H: Wire>(&mut self, state: &mut State, host: &mut H) -> Result<(), String> {
        let placements: Vec<Value> = self
            .items
            .iter()
            .filter(|i| !i.local)
            .filter_map(Item::placement)
            .collect();
        let value = state
            .call(
                host,
                "peersus.displays",
                "arrange",
                json!({"expected":self.expected,"placements":placements}),
            )
            .map_err(|error| match error {
                Refusal::Blocked(Blocked::MissingPermission(permission)) => {
                    self.arrange_block = true;
                    format!(
                        "Cannot save the arrangement: missing permission {} on this device.",
                        permission.name
                    )
                }
                Refusal::Other(answer::CANCELLED) => answer::CANCELLED.into(),
                _ => "The layout could not be saved. Reload before making another change.".into(),
            })?;
        self.load(value).map_err(Into::into)
    }

    fn relative_item(&self) -> Option<usize> {
        let device = self.relative_menu.as_deref()?;
        self.items
            .iter()
            .position(|item| !item.local && item.device == device)
    }

    fn can_refit(&self, index: usize) -> bool {
        let known = |item: &Item| {
            !item.displays.is_empty() && item.displays.iter().all(|d| d.width > 0 && d.height > 0)
        };
        known(&self.items[index]) && self.items.iter().any(|item| item.local && known(item))
    }

    fn set_relative<H: Wire>(
        &mut self,
        state: &mut State,
        host: &mut H,
        index: usize,
        side: Option<Side>,
    ) {
        let before = self.items[index].relative_to_local;
        if side == before || (side.is_some() && !self.can_refit(index)) {
            return;
        }
        self.error = None;
        if side.is_some()
            && self
                .items
                .iter()
                .enumerate()
                .any(|(n, item)| n != index && item.relative_to_local == side)
        {
            self.error = Some("Only one relative device can use each side.".into());
            return;
        }
        let at = self.items[index].at;
        let free = at.unwrap_or_else(|| {
            [
                self.monitors().iter().map(Rect::right).max().unwrap_or(0),
                0,
            ]
        });
        self.items[index].relative_to_local = side;
        self.items[index].at = Some(free);
        if let Err(reason) = self.save(state, host) {
            self.items[index].relative_to_local = before;
            self.items[index].at = at;
            self.error = Some(reason);
        }
    }

    // While moving a docked device, snap to a side of the local desktop, not to another peer.
    fn side_near(&self, point: [f64; 2], scale: f64, origin: [f64; 2]) -> Option<Side> {
        let (at, size) = self.items.iter().find(|i| i.local)?.drawn_bounds()?;
        let x = origin[0] + point[0] / scale - at[0] as f64;
        let y = origin[1] + point[1] / scale - at[1] as f64;
        let (w, h) = (size[0] as f64, size[1] as f64);
        let reach = 16. / scale;
        [
            (
                Side::Left,
                x.abs(),
                x <= reach && (-reach..=h + reach).contains(&y),
            ),
            (
                Side::Right,
                (w - x).abs(),
                x >= w - reach && (-reach..=h + reach).contains(&y),
            ),
            (
                Side::Top,
                y.abs(),
                y <= reach && (-reach..=w + reach).contains(&x),
            ),
            (
                Side::Bottom,
                (h - y).abs(),
                y >= h - reach && (-reach..=w + reach).contains(&x),
            ),
        ]
        .into_iter()
        .filter(|&(_, _, near)| near)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(side, _, _)| side)
    }
    fn monitors(&self) -> Vec<Rect> {
        self.items
            .iter()
            .enumerate()
            .flat_map(|(n, i)| i.screens(n))
            .collect()
    }
    fn height(&self) -> f64 {
        self.bounds[3].max(1.)
    }
    fn view(&self) -> (f64, [f64; 2]) {
        self.zoom.unwrap_or_else(|| {
            let all = self.monitors();
            let w = self.bounds[2].max(1.);
            let l = all.iter().map(|r| r.x).min().unwrap_or(0) as f64;
            let t = all.iter().map(|r| r.y).min().unwrap_or(0) as f64;
            let r = all.iter().map(Rect::right).max().unwrap_or(1920) as f64;
            let b = all.iter().map(Rect::bottom).max().unwrap_or(1080) as f64;
            let sx = (r - l).max(1.);
            let sy = (b - t).max(1.);
            let scale = (w / (sx * 1.3))
                .min(self.height() / (sy * 1.3))
                .clamp(0.005, 2.);
            (
                scale,
                [
                    l - (w / scale - sx) / 2.,
                    t - (self.height() / scale - sy) / 2.,
                ],
            )
        })
    }
    fn verdict(&self) -> (bool, String) {
        let all = self.monitors();
        // Only devices may not overlap each other: a device's own screens may (mirrored outputs share one place).
        for (n, a) in all.iter().enumerate() {
            for b in all[n + 1..].iter().filter(|b| b.device != a.device) {
                if a.right().min(b.right()) - a.x.max(b.x) > 2
                    && a.bottom().min(b.bottom()) - a.y.max(b.y) > 2
                {
                    return (
                        false,
                        format!(
                            "{} and {} overlap: move one of them.",
                            self.items[a.device].name, self.items[b.device].name
                        ),
                    );
                }
            }
        }
        if self
            .items
            .iter()
            .any(|item| item.at.is_some() && item.displays.is_empty())
        {
            return (true, "A placed device has not announced its monitors; its box is a placeholder and pointer pass-through is unavailable until the monitor information arrives.".into());
        }
        if self
            .items
            .iter()
            .any(|item| item.relative_to_local.is_some() && item.relative_displays.is_empty())
        {
            return (
                true,
                "A relative device has no usable monitor geometry, so its edge is not active."
                    .into(),
            );
        }
        let local = self.items.iter().position(|i| i.local);
        for (index, item) in self
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| !i.local && i.at.is_some() && i.relative_to_local.is_none())
        {
            let touches = all.iter().filter(|r| r.device == index).any(|a| {
                all.iter().filter(|r| Some(r.device) == local).any(|b| {
                    ((a.right().abs_diff(b.x) <= 2 || b.right().abs_diff(a.x) <= 2)
                        && a.bottom().min(b.bottom()) > a.y.max(b.y))
                        || ((a.bottom().abs_diff(b.y) <= 2 || b.bottom().abs_diff(a.y) <= 2)
                            && a.right().min(b.right()) > a.x.max(b.x))
                })
            });
            if !touches {
                return (
                    true,
                    format!(
                        "{} does not touch this computer, so the pointer never reaches it.",
                        item.name
                    ),
                );
            }
        }
        (
            true,
            "Every device touches this one: the pointer crosses through the shared edge.".into(),
        )
    }
    fn snap(&self, index: usize, wanted: [i64; 2], scale: f64) -> Option<[i64; 2]> {
        let size = self.items[index].extent();
        let threshold = (16. / scale) as u64;
        let mut best: [Option<(u64, i64)>; 2] = [None, None];
        for other in self.monitors().iter().filter(|r| r.device != index) {
            for (axis, candidates) in [
                (
                    0,
                    [
                        other.right(),
                        other.x - size[0],
                        other.x,
                        other.right() - size[0],
                    ],
                ),
                (
                    1,
                    [
                        other.bottom(),
                        other.y - size[1],
                        other.y,
                        other.bottom() - size[1],
                    ],
                ),
            ] {
                for candidate in candidates {
                    let distance = candidate.abs_diff(wanted[axis]);
                    if distance <= threshold && best[axis].is_none_or(|(d, _)| distance < d) {
                        best[axis] = Some((distance, candidate));
                    }
                }
            }
        }
        let at = [
            best[0].map_or(wanted[0], |(_, n)| n),
            best[1].map_or(wanted[1], |(_, n)| n),
        ];
        // A screen at the edge can suggest a snap outside the plane. Keep a valid unsnapped
        // position in that case, and never replace the last preview with an invalid position.
        [at, wanted]
            .into_iter()
            .find(|&at| placement_fits(at, size))
    }
    fn peer_count(&self) -> usize {
        self.items.iter().filter(|item| !item.local).count()
    }
    fn menu_rows(&self, peers: usize) -> usize {
        let fit = |reserved: f64| {
            (((self.height() - reserved) / MENU_ROW).max(1.) as usize).min(MAX_DEVICE_ROWS)
        };
        let without_navigation = fit(MENU_HEADER);
        if peers <= without_navigation {
            without_navigation
        } else {
            fit(MENU_HEADER + MENU_PAGING)
        }
    }
    fn last_menu_page(&self, peers: usize) -> usize {
        peers.saturating_sub(1) / self.menu_rows(peers)
    }
    fn menu_height(&self) -> f64 {
        let count = self.peer_count();
        let rows = self.menu_rows(count);
        let page = self.menu_page.min(self.last_menu_page(count));
        if self.choosing && self.relative_item().is_some() {
            238.
        } else if self.choosing {
            MENU_HEADER
                + count.saturating_sub(page * rows).min(rows) as f64 * MENU_ROW
                + if count > rows { MENU_PAGING } else { 0. }
        } else {
            44.
        }
    }
    fn menu_width(width: f64) -> f64 {
        220f64.min((width - 16.).max(1.)).round()
    }
    fn menu_position(&self, height: f64) -> [f64; 2] {
        let width = self.bounds[2].max(1.);
        let menu_width = Self::menu_width(width);
        let right = (width - menu_width).max(0.);
        let bottom = (self.height() - height).max(0.);
        if self.choosing
            && let Some(at) = self.menu_anchor
        {
            let fit = |at: f64, end: f64| if at <= end { at } else { (end - 8.).max(0.) };
            return [fit(at[0], right), fit(at[1], bottom)];
        }
        let (scale, origin) = self.view();
        let overlap = |at: [f64; 2]| {
            let mut area = 0.;
            for item in &self.items {
                let Some((point, size)) = item.drawn_bounds() else {
                    continue;
                };
                let (x, y) = (
                    (point[0] as f64 - origin[0]) * scale,
                    (point[1] as f64 - origin[1]) * scale,
                );
                area += ((at[0] + menu_width).min(x + size[0] as f64 * scale) - at[0].max(x))
                    .max(0.)
                    * ((at[1] + height).min(y + size[1] as f64 * scale) - at[1].max(y)).max(0.);
            }
            if self.pointer.is_some_and(|p| {
                p[0] >= at[0] - 24.
                    && p[0] <= at[0] + menu_width + 24.
                    && p[1] >= at[1] - 24.
                    && p[1] <= at[1] + height + 24.
            }) {
                area += menu_width * height;
            }
            area
        };
        let inset = |end: f64| (end - 8.).max(8.).min(end);
        let (left, top) = (8f64.min(right), 8f64.min(bottom));
        let (right, bottom) = (inset(right), inset(bottom));
        let candidates = [[left, top], [right, top], [left, bottom], [right, bottom]];
        candidates
            .into_iter()
            .min_by(|a, b| overlap(*a).total_cmp(&overlap(*b)))
            .unwrap()
    }
    pub fn event<H: Wire>(
        &mut self,
        state: &mut State,
        host: &mut H,
        event: &Value,
        page: Option<&str>,
    ) -> Result<Value, &'static str> {
        let opening = event["t"] == "open";
        let changed =
            (opening || self.needs_refresh) && self.sync_permissions(state, host, opening);
        if changed && let Some(at) = event["at"].as_u64() {
            self.refreshed = at;
        }
        let value = self.page_event(state, host, event, page)?;
        Ok(if changed && value.is_null() {
            self.tree()
        } else {
            value
        })
    }

    fn page_event<H: Wire>(
        &mut self,
        state: &mut State,
        host: &mut H,
        event: &Value,
        page: Option<&str>,
    ) -> Result<Value, &'static str> {
        // One layout is shown on more than one page. A gesture is on one of them: while it lasts, the
        // other page's ticks and measurements must not swap the canvas under it.
        if self.grab.is_some() && page.is_some() && self.grab_page.as_deref() != page {
            return Ok(Value::Null);
        }
        if let Some(bounds) = page.and_then(|page| self.pages.get(page)) {
            self.bounds = *bounds;
        }
        let id = event["id"].as_str().unwrap_or("");
        let (scale, origin) = self.view();
        match event["t"].as_str() {
            Some("open") => {
                if self.bounds[2] <= 0. {
                    self.bounds[2] = 600.;
                }
                if self.bounds[3] <= 0. {
                    self.bounds[3] = INITIAL_HEIGHT;
                }
                self.read_again(state, host);
            }
            Some("tick") => {
                let at = event["at"].as_u64().unwrap_or(0);
                if self.watch_block
                    || self.grab.is_some()
                    || at.saturating_sub(self.refreshed) < 2000
                {
                    return Ok(Value::Null);
                }
                self.refreshed = at;
                let before = (self.items.clone(), self.unseen.take());
                self.read_again(state, host);
                if before.0 == self.items && before.1 == self.unseen {
                    return Ok(Value::Null);
                }
            }
            Some("size") if id == "canvas" => {
                let length = |key: &str| {
                    event[key]
                        .as_i64()
                        .filter(|n| (0..=i64::from(MAX_PAGE_AT)).contains(n))
                        .map(|n| n as f64 / 1000.)
                        .ok_or(answer::NOT_UNDERSTOOD)
                };
                let next = [
                    page_coordinate(&event["x"])?,
                    page_coordinate(&event["y"])?,
                    length("width")?,
                    length("height")?,
                ];
                let resized = (next[2] - self.bounds[2]).abs() >= 0.5
                    || (next[3] - self.bounds[3]).abs() >= 0.5;
                self.bounds = next;
                if let Some(page) = page {
                    self.pages.insert(page.to_owned(), next);
                }
                // Where the canvas sits only matters for the wheel; a new page answered for every move
                // or scroll of it would be measured and answered again, a redraw on every frame.
                if !resized {
                    return Ok(Value::Null);
                }
            }
            Some("wheel") if id == "canvas" => {
                let dy = event["dy"].as_i64().unwrap_or(0);
                if dy == 0 {
                    return Ok(Value::Null);
                }
                let next = (scale * if dy > 0 { 1.15 } else { 1. / 1.15 }).clamp(0.005, 2.);
                let x = page_coordinate(&event["x"])? - self.bounds[0];
                let y = page_coordinate(&event["y"])? - self.bounds[1];
                self.zoom = Some((
                    next,
                    [
                        origin[0] + x / scale - x / next,
                        origin[1] + y / scale - y / next,
                    ],
                ));
            }
            Some("press") => match id {
                "fit" => self.zoom = None,
                "minus" => self.zoom = Some(((scale / 1.25).clamp(0.005, 2.), origin)),
                "plus" => self.zoom = Some(((scale * 1.25).clamp(0.005, 2.), origin)),
                _ if !self.editable() => return Ok(Value::Null),
                "devices" => {
                    let at = self.menu_position(self.menu_height());
                    self.choosing = !self.choosing;
                    self.menu_anchor = self.choosing.then_some(at);
                    self.relative_menu = None;
                }
                "devices-next" | "devices-previous" => {
                    let last = self.last_menu_page(self.peer_count());
                    let page = self.menu_page.min(last);
                    self.menu_page = if id == "devices-next" {
                        (page + 1).min(last)
                    } else {
                        page.saturating_sub(1)
                    };
                }
                // Saving reads every placement, including the drag preview, so no removal
                // may be committed until the gesture finishes or restores the original.
                _ if self.grab.is_some() => return Ok(Value::Null),
                "relative-back" => self.relative_menu = None,
                "relative-free" => {
                    if let Some(index) = self.relative_item() {
                        self.set_relative(state, host, index, None);
                    }
                }
                id if let Some((side, _, _)) =
                    Side::ALL.into_iter().find(|(_, key, _)| *key == id) =>
                {
                    if let Some(index) = self.relative_item() {
                        self.set_relative(state, host, index, Some(side));
                    }
                }
                id if let Some(index) = id
                    .strip_prefix("relative-")
                    .and_then(|n| n.parse::<usize>().ok())
                    && self.choosing =>
                {
                    if let Some(item) = self.items.get(index).filter(|item| !item.local) {
                        self.relative_menu = Some(item.device.clone());
                    }
                }
                _ => {
                    if let Some(index) = id
                        .strip_prefix("remove-")
                        .and_then(|n| n.parse::<usize>().ok())
                        .filter(|n| {
                            *n < self.items.len()
                                && !self.items[*n].local
                                && self.items[*n].at.is_some()
                        })
                    {
                        let old = self.items[index].at;
                        self.error = None;
                        let side = self.items[index].relative_to_local.take();
                        self.items[index].at = None;
                        if let Err(reason) = self.save(state, host) {
                            self.items[index].at = old;
                            self.items[index].relative_to_local = side;
                            self.error = Some(reason);
                        }
                    } else {
                        return Ok(Value::Null);
                    }
                }
            },
            Some("pointer") if id == "device-menu" => return Ok(Value::Null),
            Some("pointer") if !self.editable() && id != "canvas" => return Ok(Value::Null),
            Some("pointer") => {
                let menu_at = if event["phase"] == "down" {
                    self.menu_position(self.menu_height())
                } else {
                    [0., 0.]
                };
                let point = [page_coordinate(&event["x"])?, page_coordinate(&event["y"])?];
                self.pointer = Some([point[0] - self.bounds[0], point[1] - self.bounds[1]]);
                match event["phase"].as_str() {
                    Some("down") => {
                        self.error = None;
                        self.zoom = Some((scale, origin));
                        self.grab = if let Some(index) = id
                            .strip_prefix("relative-screen-")
                            .and_then(|tail| tail.split_once('-'))
                            .and_then(|(n, _)| n.parse::<usize>().ok())
                            .filter(|n| {
                                self.items
                                    .get(*n)
                                    .is_some_and(|i| i.relative_to_local.is_some())
                            }) {
                            Some(Grab::Relative {
                                index,
                                from: point,
                                side: None,
                                moved: false,
                            })
                        } else if let Some(index) = id
                            .strip_prefix("menu-device-")
                            .and_then(|n| n.parse::<usize>().ok())
                            .filter(|n| *n < self.items.len() && !self.items[*n].local)
                        {
                            self.menu_anchor = Some(menu_at);
                            if self.items[index].relative_to_local.is_some() {
                                self.grab = Some(Grab::Relative {
                                    index,
                                    from: point,
                                    side: None,
                                    moved: false,
                                });
                                self.grab_page = page.map(str::to_owned);
                                return Ok(Value::Null);
                            }
                            Some(Grab::Menu {
                                index,
                                from: point,
                                was: self.items[index].at,
                                moved: false,
                            })
                        } else if let Some(index) = id
                            .strip_prefix("desktop-")
                            .and_then(|n| n.parse::<usize>().ok())
                            .filter(|n| *n < self.items.len() && !self.items[*n].local)
                        {
                            self.items[index].at.map(|was| Grab::Device {
                                index,
                                from: point,
                                was,
                            })
                        } else if id == "canvas" {
                            Some(Grab::Pan {
                                from: point,
                                was: origin,
                            })
                        } else {
                            None
                        };
                        self.grab_page = self.grab.as_ref().and(page.map(str::to_owned));
                        return Ok(Value::Null);
                    }
                    Some("move") => {
                        match self.grab.clone() {
                            Some(Grab::Pan { from, was }) => {
                                let next = [
                                    was[0] - (point[0] - from[0]) / scale,
                                    was[1] - (point[1] - from[1]) / scale,
                                ];
                                if origin == next {
                                    return Ok(Value::Null);
                                }
                                self.zoom = Some((scale, next));
                            }
                            Some(Grab::Device { index, from, was }) => {
                                let wanted = [
                                    was[0] + ((point[0] - from[0]) / scale) as i64,
                                    was[1] + ((point[1] - from[1]) / scale) as i64,
                                ];
                                let Some(at) = self.snap(index, wanted, scale) else {
                                    return Ok(Value::Null);
                                };
                                let next = Some(at);
                                if self.items[index].at == next {
                                    return Ok(Value::Null);
                                }
                                self.items[index].at = next;
                            }
                            Some(Grab::Menu {
                                index,
                                from,
                                was,
                                moved,
                            }) => {
                                let moved =
                                    moved || (point[0] - from[0]).hypot(point[1] - from[1]) >= 6.;
                                if !moved {
                                    return Ok(Value::Null);
                                }
                                let size = self.items[index].extent();
                                let wanted = [
                                    (origin[0] + (point[0] - self.bounds[0]) / scale
                                        - size[0] as f64 / 2.)
                                        .round() as i64,
                                    (origin[1] + (point[1] - self.bounds[1]) / scale
                                        - size[1] as f64 / 2.)
                                        .round() as i64,
                                ];
                                self.grab = Some(Grab::Menu {
                                    index,
                                    from,
                                    was,
                                    moved,
                                });
                                let Some(at) = self.snap(index, wanted, scale) else {
                                    return Ok(Value::Null);
                                };
                                self.items[index].at = Some(at);
                            }
                            None => return Ok(Value::Null),
                            Some(Grab::Relative {
                                index,
                                from,
                                side,
                                moved,
                            }) => {
                                let next =
                                    moved || (point[0] - from[0]).hypot(point[1] - from[1]) >= 6.;
                                let wanted = next
                                    .then(|| {
                                        self.side_near(
                                            [point[0] - self.bounds[0], point[1] - self.bounds[1]],
                                            scale,
                                            origin,
                                        )
                                    })
                                    .flatten();
                                if wanted == side && next == moved {
                                    return Ok(Value::Null);
                                }
                                self.grab = Some(Grab::Relative {
                                    index,
                                    from,
                                    side: wanted,
                                    moved: next,
                                });
                            }
                        }
                        // Track every sample, but serialize at most one visual frame per 16ms.
                        // Up/cancel always renders the final state, even inside this interval.
                        if let Some(at) = event["at"].as_u64() {
                            if self
                                .rendered_at
                                .is_some_and(|last| at.saturating_sub(last) < 16)
                            {
                                return Ok(Value::Null);
                            }
                            self.rendered_at = Some(at);
                        }
                    }
                    Some("up" | "cancel") => {
                        self.grab_page = None;
                        match self.grab.take() {
                            Some(Grab::Device { index, was, .. }) => {
                                if event["phase"] == "cancel" || !self.verdict().0 {
                                    self.items[index].at = Some(was)
                                } else if self.items[index].at != Some(was)
                                    && let Err(reason) = self.save(state, host)
                                {
                                    self.items[index].at = Some(was);
                                    self.error = Some(reason);
                                }
                            }
                            Some(Grab::Menu {
                                index, was, moved, ..
                            }) => {
                                let inside = point[0] >= self.bounds[0]
                                    && point[0] <= self.bounds[0] + self.bounds[2]
                                    && point[1] >= self.bounds[1]
                                    && point[1] <= self.bounds[1] + self.height();
                                if event["phase"] == "cancel" || (moved && !inside) {
                                    self.items[index].at = was;
                                } else {
                                    if !moved && was.is_none() {
                                        let at = [
                                            self.monitors()
                                                .iter()
                                                .map(Rect::right)
                                                .max()
                                                .unwrap_or(0),
                                            0,
                                        ];
                                        if placement_fits(at, self.items[index].extent()) {
                                            self.items[index].at = Some(at);
                                        }
                                    }
                                    if !self.verdict().0 {
                                        self.items[index].at = was
                                    } else if self.items[index].at != was
                                        && let Err(reason) = self.save(state, host)
                                    {
                                        self.items[index].at = was;
                                        self.error = Some(reason);
                                    }
                                }
                                self.choosing = false;
                                self.menu_anchor = None;
                            }
                            Some(Grab::Relative { index, moved, .. }) if event["phase"] == "up" => {
                                if moved {
                                    if let Some(side) = self.side_near(
                                        [point[0] - self.bounds[0], point[1] - self.bounds[1]],
                                        scale,
                                        origin,
                                    ) {
                                        self.set_relative(state, host, index, Some(side));
                                    }
                                } else {
                                    self.choosing = true;
                                    self.relative_menu = Some(self.items[index].device.clone());
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => return Err(answer::NOT_UNDERSTOOD),
                }
            }
            _ => return Err(answer::NOT_UNDERSTOOD),
        }
        Ok(self.tree())
    }
    pub fn tree(&self) -> Value {
        let (scale, origin) = self.view();
        let width = self.bounds[2].max(1.);
        let step = [
            100., 250., 500., 1000., 2000., 5000., 10000., 20000., 50000., 100000.,
        ]
        .into_iter()
        .find(|n| n * scale >= 64.)
        .unwrap_or(100000.);
        let mut canvas = Vec::new();
        for (axis, offset) in origin.iter().enumerate() {
            let limit = if axis == 0 { width } else { self.height() };
            let first = (*offset / step).floor();
            for n in 0..=((limit / scale / step).ceil() as usize + 1).min(128) {
                let plane = (first + n as f64) * step;
                let pos = (plane - *offset) * scale;
                if pos < 0. || pos > limit {
                    continue;
                }
                let milli = (pos * 1000.) as i32;
                let mut at = [0; 2];
                at[axis] = milli;
                canvas.push(bx(None,json!({"at":at,"width":if axis==0 {px(1.)}else{fill()},"height":if axis==0 {px(self.height())}else{px(1.)},"background":if plane==0. {"muted"}else{"border"}}),vec![]));
                let offset = if axis == 0 { 4000 } else { 2000 };
                if pos > 36. && milli <= MAX_PAGE_AT - offset {
                    let mut label_at = [4000; 2];
                    label_at[axis] = milli + offset;
                    canvas.push(bx(
                        None,
                        json!({"at":label_at}),
                        vec![text(format!("{plane:.0}"))],
                    ));
                }
            }
        }
        let (savable, verdict) = self.verdict();
        for (index, item) in self.items.iter().enumerate() {
            let relative = item.relative_to_local.is_some();
            let free = (!relative).then_some(item.at).flatten().map(|[x, y]| {
                let [w, h] = item.extent();
                [x, y, w, h]
            });
            let screens = if relative {
                &item.relative_displays[..]
            } else {
                free.as_slice()
            };
            for (slot, &[x, y, w, h]) in screens.iter().enumerate() {
                let x = (x as f64 - origin[0]) * scale;
                let y = (y as f64 - origin[1]) * scale;
                let floor = if relative { 1. } else { 24. };
                let w = (w as f64 * scale).max(floor);
                let h = (h as f64 * scale).max(floor);
                if x + w < 0. || y + h < 0. || x > width || y > self.height() {
                    continue;
                }
                // Only send the visible part: zoom cannot exceed the element coordinate bounds.
                let (left, top, right, bottom) = (
                    x.max(0.),
                    y.max(0.),
                    (x + w).min(width),
                    (y + h).min(self.height()),
                );
                let (w, h) = ((right - left).max(1.), (bottom - top).max(1.));
                let id = if relative {
                    format!("relative-screen-{index}-{slot}")
                } else {
                    format!("desktop-{index}")
                };
                let mut inside = Vec::new();
                if h >= 72. && w >= 56. {
                    inside.push(json!({"k":"avatar","device":item.device,"name":item.name,"size":if h>=96. && w>=96. {32}else{22}}));
                }
                inside.push(json!({"k":"text","text":item.name}));
                if item.displays.is_empty() {
                    inside.push(text("Monitors not announced"));
                }
                if item.local
                    && h >= 56.
                    && let Some([x, y]) = item.at
                {
                    inside.push(text(format!("{x}, {y}")));
                }
                if relative
                    && h >= 40.
                    && let Some(real) = item.displays.get(slot)
                {
                    inside.push(text(format!("{}×{}", real.width, real.height)));
                }
                let edge = if !savable {
                    "danger"
                } else if item.displays.is_empty() {
                    "muted"
                } else if item.local || relative {
                    "accent"
                } else {
                    "success"
                };
                let face = bx(
                    None,
                    json!({"width":fill(),"height":fill(),"radius":[5,5,5,5],"background":"surface","center":true,"gap":4,"padding":2,"clip":true}),
                    inside,
                );
                canvas.push(bx((!item.local && self.editable()).then_some(id.as_str()),json!({"at":[(left*1000.) as i32,(top*1000.) as i32],"width":px(w),"height":px(h),"radius":[6,6,6,6],"background":edge,"padding":2,"clip":true}),vec![face]));
            }
        }
        let peers: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.local)
            .collect();
        let rows = self.menu_rows(peers.len());
        let last = self.last_menu_page(peers.len());
        let page = self.menu_page.min(last);
        let shown = peers.iter().skip(page * rows).take(rows);
        let height = self.menu_height();
        let at = self.menu_position(height);
        let mut menu = vec![button(
            "devices",
            if self.choosing {
                "Devices ▴"
            } else {
                "Devices ▾"
            },
        )];
        if self.choosing
            && let Some(index) = self.relative_item()
        {
            let item = &self.items[index];
            menu.push(text(format!("{}: Relative to this device", item.name)));
            if self.can_refit(index) {
                let sides = Side::ALL
                    .into_iter()
                    .map(|(side, id, label)| {
                        button(
                            id,
                            if item.relative_to_local == Some(side) {
                                format!("✓ {label}")
                            } else {
                                label.into()
                            },
                        )
                    })
                    .collect();
                menu.push(bx(
                    None,
                    json!({"horizontal":true,"wrap":true,"gap":4}),
                    sides,
                ));
            } else {
                menu.push(text(
                    "Both devices must announce their monitors before attaching.",
                ));
            }
            if item.relative_to_local.is_some() {
                menu.push(button("relative-free", "Use real size"));
            }
            menu.push(button("relative-back", "Back"));
        } else if self.choosing {
            for (n, item) in shown {
                let mut row = vec![
                    json!({"k":"avatar","device":item.device,"name":item.name,"size":28}),
                    json!({"k":"text","text":item.name}),
                ];
                row.push(button(
                    format!("relative-{n}"),
                    if item.relative_to_local.is_some() {
                        "Refit ✓"
                    } else {
                        "Refit"
                    },
                ));
                if item.at.is_some() {
                    row.push(button(format!("remove-{n}"), "×"));
                }
                menu.push(bx(
                    Some(&format!("menu-device-{n}")),
                    json!({"horizontal":true,"gap":8,"height":px(40.),"width":fill()}),
                    row,
                ));
            }
            if peers.is_empty() {
                menu.push(text("No paired devices."));
            }
            if last > 0 {
                let mut navigation = Vec::new();
                if page > 0 {
                    navigation.push(button("devices-previous", "Previous"));
                }
                if page < last {
                    navigation.push(button("devices-next", "Next"));
                }
                menu.push(bx(None, json!({"horizontal":true,"gap":8}), navigation));
            }
        }
        if self.editable() {
            canvas.push(bx(Some("device-menu"),json!({"at":[(at[0]*1000.) as i32,(at[1]*1000.) as i32],"width":px(Self::menu_width(width)),"padding":8,"gap":8,"radius":[10,10,10,10],"background":"raised","border":true}),menu));
        }
        let mut plane = bx(
            Some("canvas"),
            json!({"width":fill(),"height":{"unit":"flex","value":1},"clip":true,"radius":[12,12,12,12],"border":true,"background":"surface"}),
            canvas,
        );
        plane["observe"] = json!(["size", "wheel"]);
        let drop_hint = match self.grab {
            Some(Grab::Relative {
                side: Some(side),
                moved: true,
                ..
            }) => Side::ALL
                .into_iter()
                .find(|(s, _, _)| *s == side)
                .map(|(_, _, label)| {
                    format!(
                        "Release to attach on the {} side.",
                        label.to_ascii_lowercase()
                    )
                }),
            _ => None,
        };
        let mut children = vec![
            bx(
                None,
                json!({"width":fill(),"horizontal":true,"gap":8}),
                vec![
                    text("Layout"),
                    button("minus", "−"),
                    button("plus", "+"),
                    button("fit", "Zoom to fit"),
                ],
            ),
            text(if self.editable() {
                "Arrange devices around this computer. In Devices, choose Refit to attach a device proportionally, or drag the device to place it freely. Drag the background to pan and use the wheel to zoom."
            } else {
                "Drag the background to pan and use the wheel to zoom."
            }),
            plane,
            text(
                self.error
                    .as_deref()
                    .or(self.unseen.as_deref())
                    .or(drop_hint.as_deref())
                    .unwrap_or(if self.arrange_block {
                        NEEDS_ARRANGE
                    } else {
                        &verdict
                    }),
            ),
        ];
        if self.arrange_block && self.unseen.is_some() {
            children.push(text(NEEDS_ARRANGE));
        }
        json!({"k":"interval","id":"layout-clock","milliseconds":1000,"child":bx(None,json!({"width":fill(),"height":fill(),"padding":12,"gap":8}),children)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::scripted::Script;

    fn scene() -> Value {
        json!({"devices":[
            {"device":"this-device","name":"Local","displays":[{"x":-100,"y":-50,"width":100,"height":100}],"at":[0,0],"local":true},
            {"device":"scoped","name":"Remote","displays":[{"x":0,"y":0,"width":100,"height":100}],"at":[100,0],"local":false}
        ],"placements":[{"device":"scoped","x":100,"y":0}]})
    }
    fn ready() -> Ui {
        let mut ui = Ui::default();
        ui.load(scene()).unwrap();
        ui.bounds = [10., 20., 600., INITIAL_HEIGHT];
        ui.zoom = Some((1., [0., 0.]));
        ui
    }

    fn canvas(tree: &Value) -> &[Value] {
        tree["child"]["children"][2]["children"].as_array().unwrap()
    }

    fn menu(tree: &Value) -> &Value {
        canvas(tree)
            .iter()
            .find(|node| node["id"] == "device-menu")
            .unwrap()
    }

    #[test]
    fn showing_and_saving_an_arrangement_are_independent_and_known_denials_are_not_polled() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        let mut ready =
            json!({"missing_permissions":[{"owner":"host","name":"displays.arrange"}],"peers":[]});
        state.readiness.update(&ready).unwrap();
        host.results
            .push_back(json!({"status":"ok","value":scene()}));
        let read_only = ui
            .event(&mut state, &mut host, &json!({"t":"open"}), None)
            .unwrap();
        assert!(read_only.to_string().contains(NEEDS_ARRANGE));
        assert!(read_only.to_string().contains("Remote"));
        assert!(
            !canvas(&read_only)
                .iter()
                .any(|node| node["id"] == "device-menu")
        );
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"press","id":"remove-1"}),
            None,
        )
        .unwrap();
        assert_eq!(host.sent.len(), 1, "only the granted inspect was called");
        assert_eq!(host.sent[0]["method"], "inspect");
        assert_eq!(ui.items[1].at, Some([100, 0]));

        ready["missing_permissions"][0]["name"] = json!("displays.watch");
        state.readiness.update(&ready).unwrap();
        ui.permission_changed();
        let denied = ui
            .event(&mut state, &mut host, &json!({"t":"tick","at":2500}), None)
            .unwrap();
        assert!(
            denied
                .to_string()
                .contains("missing permission displays.watch on this device")
        );
        assert!(!denied.to_string().contains(NEEDS_ARRANGE));
        assert!(ui.items.is_empty());
        ui.event(&mut state, &mut host, &json!({"t":"tick","at":5000}), None)
            .unwrap();
        assert_eq!(
            host.sent.len(),
            1,
            "no inspect while the watch grant is absent"
        );
    }

    #[test]
    fn relative_mode_draws_the_hosts_projection_and_restores_the_saved_free_coordinates() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        let mut projected = scene();
        projected["devices"][1]["relative_to_local"] = json!("right");
        projected["devices"][1]["relative_displays"] = json!([[100, 0, 30, 100]]);
        projected["devices"][1]["displays"][0]["width"] = json!(72);
        projected["devices"][1]["displays"][0]["height"] = json!(240);
        projected["placements"][0]["relative_to_local"] = json!("right");
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"press","id":"devices"}),
            None,
        )
        .unwrap();
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"press","id":"relative-1"}),
            None,
        )
        .unwrap();
        host.results
            .push_back(json!({"status":"ok","value":projected.clone()}));
        let tree = ui
            .event(
                &mut state,
                &mut host,
                &json!({"t":"press","id":"relative-right"}),
                None,
            )
            .unwrap();
        let rect = canvas(&tree)
            .iter()
            .find(|n| n["id"] == "relative-screen-1-0")
            .unwrap();
        assert_eq!(rect["style"]["width"]["value"], 30);
        assert!(rect.to_string().contains("72×240"));
        assert_eq!(
            host.sent[0]["args"]["placements"][0],
            json!({"device":"scoped","x":100,"y":0,"relative_to_local":"right"})
        );
        host.results
            .push_back(json!({"status":"ok","value":scene()}));
        let tree = ui
            .event(
                &mut state,
                &mut host,
                &json!({"t":"press","id":"relative-free"}),
                None,
            )
            .unwrap();
        let rect = canvas(&tree)
            .iter()
            .find(|n| n["id"] == "desktop-1")
            .unwrap();
        assert_eq!(rect["style"]["width"]["value"], 100);
        assert_eq!(host.sent[1]["args"]["expected"], projected["placements"]);
        assert_eq!(host.sent[1]["args"]["placements"], scene()["placements"]);
    }

    #[test]
    fn invalid_normalized_edges_leave_the_loaded_scene_unchanged() {
        let mut ui = ready();
        let mut incoming = scene();
        incoming["devices"][1]["displays"] = json!([
            {"x":-100,"y":-100,"width":100,"height":100},
            {"x":0,"y":0,"width":100,"height":100}
        ]);
        for at in [[i64::MAX - 100, 0], [0, i64::MAX - 100], [i64::MIN, 0]] {
            incoming["devices"][1]["at"] = json!(at);
            incoming["placements"] = json!([{"device":"scoped","x":at[0],"y":at[1]}]);
            assert_eq!(ui.load(incoming.clone()), Err(answer::NOT_UNDERSTOOD));
            assert_eq!(ui.items[1].at, Some([100, 0]));
            assert_eq!(ui.expected, scene()["placements"]);
        }
    }

    #[test]
    fn a_drag_past_the_usable_plane_does_not_preview_or_save_a_new_position() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        // This device's right edge is exactly at the arrangement service's limit.
        let x = (1_i64 << 40) - 100;
        let mut edge = scene();
        edge["devices"][1]["at"] = json!([x, 0]);
        edge["placements"][0]["x"] = json!(x);
        ui.load(edge).unwrap();
        ui.zoom = Some((1., [x as f64, 0.]));
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "desktop-1",
            "down",
            10_000,
            20_000,
        );
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "desktop-1",
            "move",
            50_000,
            20_000,
        );
        assert_eq!(ui.items[1].at, Some([x, 0]));
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "desktop-1",
            "up",
            50_000,
            20_000,
        );
        assert_eq!(ui.items[1].at, Some([x, 0]));
        assert!(
            host.sent.is_empty(),
            "an invalid preview must not be submitted"
        );
    }

    #[test]
    fn an_unserializable_measurement_does_not_replace_the_canvas_bounds() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        let known = ui.bounds;
        for [width, height] in [[16_384_001, 360_000], [600_000, 16_384_001], [-1, 360_000]] {
            let size = json!({"t":"size","id":"canvas","x":0,"y":0,"width":width,"height":height});
            assert_eq!(
                ui.event(&mut state, &mut host, &size, None),
                Err(answer::NOT_UNDERSTOOD)
            );
            assert_eq!(ui.bounds, known);
        }
    }

    #[test]
    fn the_largest_canvas_keeps_edge_grid_lines_without_out_of_range_labels() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        ui.zoom = Some((2., [8., 8.]));
        let size =
            json!({"t":"size","id":"canvas","x":0,"y":0,"width":16_384_000,"height":16_384_000});
        let tree = ui.event(&mut state, &mut host, &size, None).unwrap();
        let nodes = canvas(&tree);
        for edge in [json!([16_384_000, 0]), json!([0, 16_384_000])] {
            assert!(nodes.iter().any(|node| node["style"]["at"] == edge));
        }
        assert!(
            nodes
                .iter()
                .all(|node| node["style"]["at"].as_array().is_none_or(|at| at
                    .iter()
                    .all(|axis| axis.as_i64().unwrap().unsigned_abs() <= 16_384_000)))
        );
    }

    #[test]
    fn a_placed_device_without_announced_displays_is_not_reported_as_a_working_crossing() {
        let mut ui = ready();
        ui.items[1].displays.clear();
        let (_, verdict) = ui.verdict();
        assert!(verdict.contains("not announced"), "{verdict}");
        assert!(!verdict.contains("pointer crosses"));
    }
    fn pointer(
        ui: &mut Ui,
        state: &mut State,
        host: &mut Script,
        id: &str,
        phase: &str,
        x: i64,
        y: i64,
    ) -> Value {
        ui.event(
            state,
            host,
            &json!({"t":"pointer","id":id,"phase":phase,"x":x,"y":y}),
            None,
        )
        .unwrap()
    }

    #[test]
    fn height_only_resize_refits_the_plane_and_canvas_uses_the_available_area() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        ui.zoom = None;
        ui.bounds[2] = 2000.;
        ui.items[0].displays[0].height = 1000;
        let before = ui.view().0;
        let tree=ui.event(&mut state,&mut host,&json!({"t":"size","id":"canvas","x":10000,"y":20000,"width":2000000,"height":800000}),None).unwrap();
        assert!(!tree.is_null());
        assert!(ui.view().0 > before);
        assert_eq!(ui.height(), 800.);
        assert_eq!(tree["child"]["style"]["height"], fill());
        assert_eq!(
            tree["child"]["children"][2]["style"]["height"],
            json!({"unit":"flex","value":1})
        );
        assert!(ui.event(&mut state,&mut host,&json!({"t":"size","id":"canvas","x":10000,"y":20000,"width":2000000,"height":800000}),None).unwrap().is_null());
        assert!(host.sent.is_empty());
    }

    #[test]
    fn the_same_layout_on_two_pages_keeps_each_page_s_own_canvas() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        let size = |w: i64, h: i64| json!({"t":"size","id":"canvas","x":0,"y":0,"width":w*1000,"height":h*1000});
        ui.event(&mut state, &mut host, &size(600, 360), Some("layout"))
            .unwrap();
        ui.event(&mut state, &mut host, &size(1400, 900), Some("layout-page"))
            .unwrap();
        let wide = ui.menu_position(44.);
        // A tick for the settings page is answered with the settings canvas, not the wide one, and so
        // the menu does not jump between the corners of two pages with every tick.
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","at":1}),
            Some("layout"),
        )
        .unwrap();
        assert_eq!(ui.bounds, [0., 0., 600., 360.]);
        assert_ne!(ui.menu_position(44.), wide);
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","at":2}),
            Some("layout-page"),
        )
        .unwrap();
        assert_eq!(ui.bounds, [0., 0., 1400., 900.]);
        assert_eq!(ui.menu_position(44.), wide);
    }

    #[test]
    fn a_drag_on_one_page_is_not_disturbed_by_the_other_page_s_ticks_and_sizes() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        let size = |w: i64, h: i64| json!({"t":"size","id":"canvas","x":0,"y":0,"width":w*1000,"height":h*1000});
        ui.event(&mut state, &mut host, &size(600, 360), Some("layout"))
            .unwrap();
        ui.event(&mut state, &mut host, &size(1400, 900), Some("layout-page"))
            .unwrap();
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"pointer","id":"desktop-1","phase":"down","x":100000,"y":0}),
            Some("layout"),
        )
        .unwrap();
        let during = ui.bounds;
        assert!(
            ui.event(
                &mut state,
                &mut host,
                &json!({"t":"tick","at":5000}),
                Some("layout-page")
            )
            .unwrap()
            .is_null()
        );
        assert!(
            ui.event(&mut state, &mut host, &size(1400, 900), Some("layout-page"))
                .unwrap()
                .is_null()
        );
        assert_eq!(
            ui.bounds, during,
            "the other page must not swap the canvas under a gesture"
        );
        let moved = ui
            .event(
                &mut state,
                &mut host,
                &json!({"t":"pointer","id":"desktop-1","phase":"move","x":160000,"y":0,"at":100}),
                Some("layout"),
            )
            .unwrap();
        assert!(!moved.is_null());
        assert_ne!(ui.items[1].at, Some([100, 0]));
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"pointer","id":"desktop-1","phase":"cancel","x":160000,"y":0}),
            Some("layout"),
        )
        .unwrap();
        assert!(ui.grab.is_none());
        assert_eq!(ui.bounds, [0., 0., 600., 360.]);
        assert!(
            ui.event(&mut state, &mut host, &size(1400, 900), Some("layout-page"))
                .unwrap()
                .is_null()
        );
        assert_eq!(
            ui.bounds,
            [0., 0., 1400., 900.],
            "after the gesture the other page is served again"
        );
    }

    #[test]
    fn dragging_from_the_menu_respects_pan_and_zoom() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        ui.items[1].at = None;
        ui.expected = json!([]);
        ui.choosing = true;
        ui.zoom = Some((2., [50., 30.]));
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "menu-device-1",
            "down",
            40000,
            40000,
        );
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "menu-device-1",
            "move",
            210000,
            60000,
        );
        assert_eq!(ui.items[1].at, Some([100, 0]));
        assert!(host.sent.is_empty());
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "menu-device-1",
            "cancel",
            210000,
            60000,
        );
        assert_eq!(ui.items[1].at, None);
    }

    #[test]
    fn clicking_an_unplaced_device_row_places_it_without_an_add_button() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        ui.items[1].at = None;
        ui.expected = json!([]);
        ui.choosing = true;
        host.results
            .push_back(json!({"status":"ok","value":scene()}));
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "menu-device-1",
            "down",
            40000,
            40000,
        );
        assert!(host.sent.is_empty());
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "menu-device-1",
            "up",
            40000,
            40000,
        );
        assert_eq!(ui.items[1].at, Some([100, 0]));
        assert_eq!(host.sent.len(), 1);
        assert_eq!(host.sent[0]["method"], "arrange");
        assert!(!ui.choosing);
    }

    #[test]
    fn dragging_an_unplaced_row_uses_canvas_coordinates_and_saves_only_on_drop() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        ui.items[1].at = None;
        ui.expected = json!([]);
        ui.choosing = true;
        let menu_at = ui.menu_position(100.);
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "menu-device-1",
            "down",
            40000,
            40000,
        );
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "menu-device-1",
            "move",
            160000,
            70000,
        );
        assert_eq!(ui.items[1].at, Some([100, 0]));
        assert_eq!(
            ui.menu_position(100.),
            menu_at,
            "the capture target must not move or disappear during a drag"
        );
        assert!(host.sent.is_empty());
        host.results
            .push_back(json!({"status":"ok","value":scene()}));
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "menu-device-1",
            "up",
            160000,
            70000,
        );
        assert_eq!(host.sent.len(), 1);
        assert_eq!(host.sent[0]["args"]["placements"], scene()["placements"]);
    }

    #[test]
    fn cancelling_or_dropping_outside_the_canvas_does_not_place_the_device() {
        for (phase, x, y) in [("cancel", 160000, 70000), ("up", -100000, -100000)] {
            let mut ui = ready();
            let mut state = State::default();
            let mut host = Script::default();
            ui.items[1].at = None;
            ui.expected = json!([]);
            ui.choosing = true;
            pointer(
                &mut ui,
                &mut state,
                &mut host,
                "menu-device-1",
                "down",
                40000,
                40000,
            );
            pointer(
                &mut ui,
                &mut state,
                &mut host,
                "menu-device-1",
                "move",
                160000,
                70000,
            );
            pointer(&mut ui, &mut state, &mut host, "menu-device-1", phase, x, y);
            assert_eq!(ui.items[1].at, None);
            assert_eq!(ui.expected, json!([]));
            assert!(host.sent.is_empty());
        }
    }

    #[test]
    fn the_open_device_menu_keeps_its_corner_when_the_pointer_moves() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        let closed = ui.menu_position(44.);
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"press","id":"devices"}),
            None,
        )
        .unwrap();
        let opened = ui.menu_position(100.);
        assert_eq!(
            opened[0], closed[0],
            "opening must not switch to the opposite corner"
        );
        ui.pointer = Some([opened[0] + 20., opened[1] + 20.]);
        assert_eq!(ui.menu_position(100.), opened);
    }

    #[test]
    fn all_device_rows_and_paging_fit_a_short_canvas() {
        let mut ui = ready();
        ui.bounds[3] = 200.;
        ui.choosing = true;
        for n in 2..=5 {
            let mut item = ui.items[1].clone();
            item.device = format!("device-{n}");
            ui.items.push(item);
        }
        let mut found = Vec::new();
        let mut host = Script::default();
        let mut state = State::default();
        loop {
            let tree = ui.tree();
            let menu = menu(&tree);
            let y = menu["style"]["at"][1].as_i64().unwrap() as f64 / 1000.;
            assert!(y + ui.menu_height() <= ui.height());
            let nodes = menu["children"].as_array().unwrap();
            found.extend(nodes.iter().filter_map(|node| {
                node["id"]
                    .as_str()?
                    .strip_prefix("menu-device-")?
                    .parse::<usize>()
                    .ok()
            }));
            if !nodes.iter().any(|node| {
                node["children"].as_array().is_some_and(|buttons| {
                    buttons.iter().any(|button| button["id"] == "devices-next")
                })
            }) {
                break;
            }
            assert!(found.len() < 5, "last page must not offer Next");
            ui.event(
                &mut state,
                &mut host,
                &json!({"t":"press","id":"devices-next"}),
                None,
            )
            .unwrap();
        }
        assert_eq!(found, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn an_open_devices_menu_stays_on_the_resized_canvas() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        ui.bounds = [0., 0., 1400., 800.];
        ui.choosing = true;
        ui.menu_anchor = Some([1172., 692.]);
        assert_eq!(menu(&ui.tree())["style"]["at"], json!([1_172_000, 692_000]));
        let size = json!({"t":"size","id":"canvas","x":0,"y":0,"width":600_000,"height":360_000});
        let tree = ui.event(&mut state, &mut host, &size, None).unwrap();
        let at = menu(&tree)["style"]["at"].as_array().unwrap();
        let right = at[0].as_i64().unwrap() + 220_000;
        let bottom = at[1].as_i64().unwrap() + 100_000;
        assert!(right <= 600_000 && bottom <= 360_000);
    }

    #[test]
    fn previous_steps_back_from_the_visible_page_after_the_peer_list_shrinks() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        let mut peers = scene()["devices"].as_array().unwrap().clone();
        peers.extend((2..=13).map(|n| {
            json!({
                "device":format!("device-{n}"),"name":format!("Peer {n}"),
                "displays":[],"at":null,"local":false
            })
        }));
        ui.load(json!({"devices":peers,"placements":scene()["placements"]}))
            .unwrap();
        let press = |ui: &mut Ui, state: &mut State, host: &mut Script, id| {
            ui.event(state, host, &json!({"t":"press","id":id}), None)
                .unwrap()
        };
        let first = |tree: &Value| menu(tree)["children"][1]["id"].clone();
        press(&mut ui, &mut state, &mut host, "devices");
        for (index, row) in ["menu-device-5", "menu-device-9", "menu-device-13"]
            .into_iter()
            .enumerate()
        {
            let tree = press(&mut ui, &mut state, &mut host, "devices-next");
            assert_eq!(first(&tree), row, "page {}", index + 1);
        }

        peers.truncate(6); // local and five peers, so the visible page is now the second
        ui.load(json!({"devices":peers,"placements":scene()["placements"]}))
            .unwrap();
        assert_eq!(first(&ui.tree()), "menu-device-5");
        let tree = press(&mut ui, &mut state, &mut host, "devices-previous");
        assert_eq!(first(&tree), "menu-device-1");
        press(&mut ui, &mut state, &mut host, "devices-next");
        let tree = press(&mut ui, &mut state, &mut host, "devices-previous");
        assert_eq!(
            first(&tree),
            "menu-device-1",
            "an unchanged list still moves back one page"
        );
    }

    #[test]
    fn remove_is_ignored_until_a_drag_preview_is_finished() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        let mut two_peers = scene();
        two_peers["devices"].as_array_mut().unwrap().push(json!({
            "device":"other","name":"Other", "displays":[],"at":[500, 0],"local":false
        }));
        two_peers["placements"]
            .as_array_mut()
            .unwrap()
            .push(json!({"device":"other","x":500,"y":0}));
        ui.load(two_peers.clone()).unwrap();
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "desktop-1",
            "down",
            100_000,
            20_000,
        );
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "desktop-1",
            "move",
            140_000,
            20_000,
        );
        assert_eq!(ui.items[1].at, Some([140, 0]));
        let remove = json!({"t":"press","id":"remove-2"});
        assert!(
            ui.event(&mut state, &mut host, &remove, None)
                .unwrap()
                .is_null()
        );
        assert_eq!(ui.items[2].at, Some([500, 0]));
        assert!(host.sent.is_empty(), "the drag is only a preview");

        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "desktop-1",
            "cancel",
            140_000,
            20_000,
        );
        two_peers["devices"][2]["at"] = Value::Null;
        two_peers["placements"] = scene()["placements"].clone();
        host.results
            .push_back(json!({"status":"ok","value":two_peers}));
        ui.event(&mut state, &mut host, &remove, None).unwrap();
        assert_eq!(host.sent[0]["args"]["placements"], scene()["placements"]);
        assert_eq!(ui.items[2].at, None);
    }

    #[test]
    fn drag_samples_keep_the_latest_position_without_serializing_more_than_one_frame_per_16ms() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "desktop-1",
            "down",
            100000,
            0,
        );
        let mut rendered = 0;
        for n in 1..=240 {
            let tree=ui.event(&mut state,&mut host,&json!({"t":"pointer","id":"desktop-1","phase":"move","x":100000+n*1000,"y":0,"at":n*4}),None).unwrap();
            if !tree.is_null() {
                rendered += 1;
            }
        }
        assert!(
            rendered <= 60,
            "high-rate input must not repaint above the frame cadence"
        );
        assert_eq!(
            ui.items[1].at,
            Some([340, 0]),
            "samples still update geometry between rendered frames"
        );
        assert!(host.sent.is_empty(), "moving is not saving");
        assert!(
            !pointer(
                &mut ui,
                &mut state,
                &mut host,
                "desktop-1",
                "cancel",
                340000,
                0
            )
            .is_null()
        );
        assert_eq!(
            ui.items[1].at,
            Some([100, 0]),
            "cancel renders the restored geometry immediately"
        );
    }

    #[test]
    fn the_corner_picker_avoids_devices_and_the_active_pointer_until_every_corner_is_occupied() {
        let mut ui = ready();
        assert_eq!(
            ui.menu_position(44.),
            [372., 8.],
            "left corners are occupied by devices"
        );
        ui.pointer = Some([400., 20.]);
        assert_eq!(
            ui.menu_position(44.),
            [8., 308.],
            "the pointer excludes the other top corner"
        );
        ui.items[0].displays[0].width = 600;
        ui.items[0].displays[0].height = 360;
        let at = ui.menu_position(44.);
        assert!(at[0] >= 0. && at[0] + 220. <= 600. && at[1] >= 0. && at[1] + 44. <= ui.height());
    }

    #[test]
    fn negative_monitor_origins_normalize_without_changing_relative_positions() {
        let item = Item {
            device: "scoped".into(),
            name: "Remote".into(),
            local: false,
            at: Some([200, 300]),
            relative_to_local: None,
            relative_displays: Vec::new(),
            displays: vec![
                Display {
                    x: -100,
                    y: -50,
                    width: 100,
                    height: 100,
                },
                Display {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 150,
                },
            ],
        };
        let screens = item.screens(3);
        assert_eq!(
            (
                screens[0].x,
                screens[0].y,
                screens[0].right(),
                screens[0].bottom()
            ),
            (200, 300, 300, 400)
        );
        assert_eq!((screens[1].x, screens[1].y), (300, 350));
        assert_eq!(item.extent(), [300, 200]);
    }

    #[test]
    fn snapping_uses_screen_space_threshold_and_overlap_is_rejected() {
        let mut ui = ready();
        assert_eq!(ui.snap(1, [113, 4], 1.), Some([100, 0]));
        assert_eq!(ui.snap(1, [113, 4], 2.), Some([113, 0]));
        assert!(ui.verdict().0);
        ui.items[1].at = Some([50, 0]);
        assert!(!ui.verdict().0);
        ui.items[1].at = Some([300, 0]);
        assert!(ui.verdict().0);
    }

    #[test]
    fn mirrored_screens_of_one_device_do_not_block_placing_another() {
        let mut ui = Ui::default();
        ui.load(json!({"devices":[
            {"device":"this-device","name":"Local","displays":[
                {"x":-100,"y":-50,"width":100,"height":100},{"x":-100,"y":-50,"width":100,"height":100}],"at":[0,0],"local":true},
            {"device":"scoped","name":"Remote","displays":[{"x":0,"y":0,"width":100,"height":100}],"at":[100,0],"local":false}
        ],"placements":[{"device":"scoped","x":100,"y":0}]})).unwrap();
        assert!(
            ui.verdict().0,
            "a device's mirrored outputs share one place"
        );
        ui.items[1].at = Some([50, 0]);
        assert!(!ui.verdict().0, "two devices still may not overlap");
    }

    #[test]
    fn wheel_zoom_preserves_the_plane_point_under_the_pointer() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"wheel","id":"canvas","x":210000,"y":120000,"dy":1}),
            None,
        )
        .unwrap();
        let (scale, origin) = ui.view();
        assert!((origin[0] + 200. / scale - 200.).abs() < 1e-8);
        assert!((origin[1] + 100. / scale - 100.).abs() < 1e-8);
        assert!(host.sent.is_empty());
    }

    #[test]
    fn cancelled_or_overlapping_drag_restores_without_saving() {
        for phase in ["cancel", "up"] {
            let mut ui = ready();
            let mut state = State::default();
            let mut host = Script::default();
            pointer(
                &mut ui,
                &mut state,
                &mut host,
                "desktop-1",
                "down",
                100000,
                0,
            );
            pointer(
                &mut ui,
                &mut state,
                &mut host,
                "desktop-1",
                "move",
                50000,
                0,
            );
            assert!(!ui.verdict().0);
            pointer(&mut ui, &mut state, &mut host, "desktop-1", phase, 50000, 0);
            assert_eq!(ui.items[1].at, Some([100, 0]));
            assert!(ui.grab.is_none());
            assert!(host.sent.is_empty());
        }
    }

    #[test]
    fn a_refused_read_is_said_on_the_page_and_gone_once_it_is_allowed() {
        let mut ui = Ui::default();
        let mut state = State::default();
        let mut host = Script::default();
        // Opened without the permission to see the screens: the page is drawn and says where to give it.
        host.results
            .push_back(json!({"status":"error","code":"denied"}));
        let page = ui
            .event(&mut state, &mut host, &json!({"t":"open"}), None)
            .expect("a refusal does not fail the page");
        assert!(page.to_string().contains(NEEDS_SCREENS), "{page}");
        // Allowed meanwhile: the next tick reads the arrangement and the sentence goes.
        host.results
            .push_back(json!({"status":"ok","value":scene()}));
        let page = ui
            .event(&mut state, &mut host, &json!({"t":"tick","at":5000}), None)
            .unwrap();
        assert!(!page.to_string().contains(NEEDS_SCREENS), "{page}");
        assert_eq!(ui.items.len(), 2);
    }

    #[test]
    fn rejected_save_rolls_back_and_keeps_the_optimistic_token() {
        let mut ui = ready();
        let mut state = State::default();
        let mut host = Script::default();
        host.results
            .push_back(json!({"status":"error","error":"refused"}));
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "desktop-1",
            "down",
            100000,
            0,
        );
        pointer(
            &mut ui,
            &mut state,
            &mut host,
            "desktop-1",
            "move",
            300000,
            0,
        );
        pointer(&mut ui, &mut state, &mut host, "desktop-1", "up", 300000, 0);
        assert_eq!(ui.items[1].at, Some([100, 0]));
        assert!(ui.error.is_some());
        assert_eq!(ui.expected, scene()["placements"]);
        assert_eq!(host.sent.len(), 1);
    }

    #[test]
    fn a_zoomed_large_monitor_is_clipped_before_serializing_coordinates() {
        let mut ui = ready();
        ui.items[1].displays[0].width = 50000;
        ui.zoom = Some((1., [30000., 0.]));
        let tree = ui.tree();
        let cards = tree["child"]["children"][2]["children"].as_array().unwrap();
        let card = cards.iter().find(|node| node["id"] == "desktop-1").unwrap();
        assert_eq!(card["style"]["at"], json!([0, 0]));
        assert_eq!(card["style"]["width"], px(600.));
    }
}
