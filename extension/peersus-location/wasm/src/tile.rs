//! Public vector tiles: decode only the layers this globe draws and project them through its camera.
use crate::canvas_sdk::List;
use crate::world::{Camera, Frame, View};
use std::{f64::consts::PI, rc::Rc};

pub mod layers;
mod packed;

const MERCATOR_LIMIT: f64 = 85.051_128_78;
const MAX_PLAIN: usize = 4 * 1024 * 1024;
const MAX_POINTS: usize = 250_000;
const MAX_FEATURES: usize = 24_000;
const TABLE_ENTRIES: usize = 8192;
const FEATURE_POINTS: usize = 16_384;
// The points of one feature that are kept. One feature is prepared in one turn, so what it may cost is bounded here:
// a provider's landuse or water polygon of many thousands of points is thinned to this many (see `thinned`), which at
// the size a tile is drawn cannot be told from the whole.
const FEATURE_KEPT: usize = 2_048;
// Bound a single untrusted feature, and spread a layer's scan and decode across turns.
const FEATURE_BYTES: usize = 32_768;
// What one turn of scanning may cost, a plain field counting one and a value kept as text `VALUE_COST` more: keeping
// a value allocates, and a turn of nothing but values cost ten times a turn of feature ranges.
const SCAN_PER_TURN: usize = 4_096;
const VALUE_COST: usize = 11;
const POINTS_PER_TURN: usize = 4_096;
// What the features of one turn may cost to prepare, in points handled (see `packed::Packed::add`): a polygon that has
// to be split is read again at every split, and a turn bounded by its points alone was lost to a few such features.
const WORK_PER_TURN: usize = 8_192;
/// What one bounded part of reading a tile may cost, as parts say their cost: in thousandths of it.
const WHOLE: usize = 1_000;
pub const MAX_TILES: usize = 9;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Key {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

fn xy(z: u8, lon: f64, lat: f64) -> (f64, f64) {
    let n = f64::from(1_u32 << z);
    let lat = lat.clamp(-MERCATOR_LIMIT, MERCATOR_LIMIT).to_radians();
    (
        (lon + 180.) / 360. * n,
        (1. - lat.tan().asinh() / PI) * n / 2.,
    )
}

#[cfg(test)]
impl Key {
    fn project(self, extent: f64, point: (i32, i32), camera: &Camera) -> Option<(f64, f64)> {
        let count = f64::from(1_u32 << self.z);
        let x = (f64::from(self.x) + f64::from(point.0) / extent) / count;
        let y = (f64::from(self.y) + f64::from(point.1) / extent) / count;
        camera.mercator_point(360. * x - 180., PI * (1. - 2. * y))
    }
}

pub fn visible(view: View, frame: Frame, max: u8) -> Vec<Key> {
    let area = Camera::new(view, frame).visible_area();
    if area.south > MERCATOR_LIMIT || area.north < -MERCATOR_LIMIT {
        return Vec::new();
    }
    let (lon, lat) = view.centre();
    // z10 gives a regional road/water/land-use network before loading dense city tiles.
    // The rectangular crop may still need one coarser level to stay under the view budget.
    let preferred = if (0.50..=1.25).contains(&view.span) {
        10
    } else {
        (360. / view.span).log2().ceil() as u8 + 1
    };
    let start = preferred.min(max.min(20));
    for z in (0..=start).rev() {
        let last = (1_u32 << z) - 1;
        let index = |n: f64| (n.floor() as u32).min(last);
        let top = index(xy(z, lon, area.north).1);
        let bot = index(xy(z, lon, area.south).1);
        let mut keys = Vec::new();
        for (from, to) in area.longitude_ranges() {
            let left = index(xy(z, from, lat).0);
            let right = index(xy(z, to, lat).0);
            if u64::from(right - left + 1) * u64::from(bot - top + 1) > MAX_TILES as u64 {
                keys.clear();
                break;
            }
            keys.extend((top..=bot).flat_map(|y| (left..=right).map(move |x| Key { z, x, y })));
        }
        if !keys.is_empty() && keys.len() <= MAX_TILES {
            let (cx, cy) = xy(z, lon, lat);
            keys.sort_by(|a, b| {
                let distance = |k: &Key| {
                    let dx = (f64::from(k.x) + 0.5 - cx).abs();
                    dx.min(f64::from(last + 1) - dx)
                        .hypot(f64::from(k.y) + 0.5 - cy)
                };
                distance(a).total_cmp(&distance(b))
            });
            keys.dedup();
            return keys;
        }
    }
    Vec::new()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Road,
    LocalRoad,
    Path,
    StreetName,
    Place,
    Water,
    Park,
    BuiltUp,
    Building,
}

impl Layer {
    fn parse(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"transportation" => Self::Road,
            b"transportation_name" => Self::StreetName,
            b"place" => Self::Place,
            b"water" => Self::Water,
            b"park" => Self::Park,
            b"landuse" => Self::BuiltUp,
            b"building" => Self::Building,
            _ => return None,
        })
    }

    fn road(class: &str) -> Option<Self> {
        Some(match class {
            "motorway" | "trunk" | "primary" | "secondary" | "tertiary" => Self::Road,
            "minor" | "residential" | "unclassified" | "living_street" => Self::LocalRoad,
            "service" | "track" | "path" => Self::Path,
            _ => return None,
        })
    }

    fn shown(self, span: f64) -> bool {
        match self {
            Self::Road | Self::Water | Self::BuiltUp | Self::Place => true,
            Self::LocalRoad | Self::Park | Self::Building => span <= 0.2,
            Self::Path | Self::StreetName => span <= 0.075,
        }
    }

    fn line(self) -> bool {
        matches!(self, Self::Road | Self::LocalRoad | Self::Path)
    }
}

type Point = (i32, i32);
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pen {
    Move(Point),
    Line(Point),
    Close,
}
struct Feature {
    kind: Layer,
    extent: u32,
    geometry: Vec<Pen>,
    name: Option<String>,
}

pub struct Tile {
    key: Key,
    art: Rc<packed::Packed>,
    names: Vec<Feature>,
    features: usize,
}

impl Tile {
    pub fn source_bytes(&self) -> usize {
        self.art.source_bytes()
            + self.names.capacity() * size_of::<Feature>()
            + self
                .names
                .iter()
                .map(|feature| {
                    feature.geometry.capacity() * size_of::<Pen>()
                        + feature.name.as_ref().map_or(0, String::capacity)
                })
                .sum::<usize>()
    }

    fn new(key: Key) -> Self {
        Self {
            key,
            art: Rc::default(),
            names: Vec::new(),
            features: 0,
        }
    }

    /// Keep `feature`, and say what preparing it cost in points handled.
    fn add(&mut self, feature: Feature) -> usize {
        self.features += 1;
        if feature.name.is_some() {
            let work = feature.geometry.len();
            self.names.push(feature);
            work
        } else {
            Rc::get_mut(&mut self.art)
                .expect("a tile is prepared before publication")
                .add(&feature)
        }
    }
}

// Only wire types used by the MVT specification are interpreted. Unknown fields are skipped.
#[derive(Clone, Copy)]
enum Item<'a> {
    Number(u64),
    Bytes(&'a [u8]),
    Other,
}
impl<'a> Item<'a> {
    fn bytes(self) -> Option<&'a [u8]> {
        if let Self::Bytes(s) = self {
            Some(s)
        } else {
            None
        }
    }
    fn number(self) -> Option<u64> {
        if let Self::Number(n) = self {
            Some(n)
        } else {
            None
        }
    }
}

fn var(bytes: &mut &[u8]) -> Option<u64> {
    let mut n = 0_u64;
    for shift in (0..=63).step_by(7) {
        let (&b, rest) = bytes.split_first()?;
        *bytes = rest;
        if shift == 63 && b > 1 {
            return None;
        }
        n |= u64::from(b & 127) << shift;
        if b & 128 == 0 {
            return Some(n);
        }
    }
    None
}

fn fields(mut bytes: &[u8]) -> impl Iterator<Item = Option<(u64, Item<'_>)>> {
    std::iter::from_fn(move || {
        if bytes.is_empty() {
            return None;
        }
        let item = field(&mut bytes);
        if item.is_none() {
            bytes = &[];
        }
        Some(item)
    })
}

fn field<'a>(bytes: &mut &'a [u8]) -> Option<(u64, Item<'a>)> {
    let key = var(bytes)?;
    let tag = key >> 3;
    if tag == 0 {
        return None;
    }
    let value = match key & 7 {
        0 => Item::Number(var(bytes)?),
        2 => {
            let length = usize::try_from(var(bytes)?).ok()?;
            let (value, tail) = bytes.split_at_checked(length)?;
            *bytes = tail;
            Item::Bytes(value)
        }
        1 | 5 => {
            *bytes = bytes.get(if key & 7 == 1 { 8.. } else { 4.. })?;
            Item::Other
        }
        _ => return None,
    };
    Some((tag, value))
}

fn geometry(mut commands: &[u8], extent: u32, limit: usize) -> Option<Vec<Pen>> {
    let (mut x, mut y) = (0_i32, 0_i32);
    let mut points = Vec::new();
    let delta = |b: &mut &[u8]| {
        let n = u32::try_from(var(b)?).ok()?;
        Some(((n >> 1) as i32) ^ -((n & 1) as i32))
    };
    while !commands.is_empty() {
        let n = var(&mut commands)?;
        let count = usize::try_from(n >> 3).ok()?;
        if count == 0 || count > limit.saturating_sub(points.len()) {
            return None;
        }
        match n & 7 {
            op @ (1 | 2) => {
                for _ in 0..count {
                    x = x.checked_add(delta(&mut commands)?)?;
                    y = y.checked_add(delta(&mut commands)?)?;
                    if x.unsigned_abs() > extent * 4 || y.unsigned_abs() > extent * 4 {
                        return None;
                    }
                    points.push(if op == 1 {
                        Pen::Move((x, y))
                    } else {
                        Pen::Line((x, y))
                    });
                }
            }
            7 if count == 1 => points.push(Pen::Close),
            _ => return None,
        }
    }
    (!points.is_empty()).then_some(points)
}

fn name<'a>(mut tags: &[u8], key: usize, values: &'a [Option<String>]) -> Option<&'a str> {
    while !tags.is_empty() {
        let k = usize::try_from(var(&mut tags)?).ok()?;
        let v = usize::try_from(var(&mut tags)?).ok()?;
        if k == key {
            return values.get(v)?.as_deref();
        }
    }
    None
}

// Use the romanized fields on the bundled Latin typeface. When the provider
// has no romanization, omit an unsupported label instead of drawing tofu.
fn readable_name(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|c| {
            matches!(
                u32::from(c),
                0x20..=0x7e | 0xa0..=0x24f | 0x1e00..=0x1eff | 0x2010..=0x2026
            )
        })
}

pub struct Decode {
    bytes: Vec<u8>,
    tile: Tile,
    remaining: usize,
    unnamed: Vec<std::ops::Range<usize>>,
    inspected: usize,
    layers: Vec<(Layer, std::ops::Range<usize>)>,
    active: Option<LayerInput>,
}

impl Decode {
    pub fn start(key: Key, bytes: Vec<u8>) -> Option<Self> {
        if bytes.len() > MAX_PLAIN {
            return None;
        }
        let mut unnamed = Vec::new();
        for field in fields(&bytes).take(257) {
            let (tag, value) = field?;
            if tag != 3 {
                continue;
            }
            let layer = value.bytes()?;
            let from = layer.as_ptr() as usize - bytes.as_ptr() as usize;
            unnamed.push(from..from + layer.len());
        }
        Some(Self {
            bytes,
            tile: Tile::new(key),
            remaining: MAX_POINTS,
            unnamed,
            inspected: 0,
            layers: Vec::new(),
            active: None,
        })
    }

    /// One turn of reading the tile: parts of it, one after another, until about half of what a turn may cost is spent.
    /// Each part is bounded by itself (a scan by `SCAN_PER_TURN`, features by `WORK_PER_TURN` and what goes with it), but
    /// most are far smaller than their bound, a layer's few fields or a handful of features, and a turn for each made a
    /// city's tile take three seconds and more to read at the ten turns a second a page gets. Half, because what the next
    /// part costs is known only once it is done, and it may be a whole one: together they stay inside the turn.
    pub fn step(&mut self) -> Result<bool, ()> {
        let mut spent = 0;
        loop {
            let (done, cost) = self.part()?;
            spent += cost;
            if done || 2 * spent >= WHOLE {
                return Ok(done);
            }
        }
    }

    /// One bounded part of the reading: whether the tile is read whole, and what the part cost in thousandths of what a
    /// part may ([`WHOLE`]).
    fn part(&mut self) -> Result<(bool, usize), ()> {
        if !self.unnamed.is_empty() {
            self.discover().ok_or(())?;
            // A layer's name leads it, so finding the layers is light unless it ran out of its fields.
            let cost = if self.unnamed.is_empty() {
                WHOLE / 8
            } else {
                WHOLE
            };
            return Ok((false, cost));
        }
        if self.active.is_none() {
            let Some((kind, range)) = self.layers.pop() else {
                return Ok((true, 0));
            };
            let limit = match kind {
                Layer::Place => 2048,
                Layer::StreetName => 8192,
                _ => MAX_POINTS,
            };
            self.active = Some(LayerInput::new(kind, range, self.remaining.min(limit)));
        }
        let input = self.active.as_mut().ok_or(())?;
        if !input.range.is_empty() {
            let spent = input.scan(&self.bytes).ok_or(())?;
            return Ok((false, spent * WHOLE / SCAN_PER_TURN));
        }
        let (done, cost) = input
            .decode(&self.bytes, &mut self.tile, &mut self.remaining)
            .ok_or(())?;
        if done {
            self.active = None;
            return Ok((self.layers.is_empty(), cost));
        }
        Ok((false, cost))
    }

    // Protobuf fields can arrive in any order, even the layer name. A malformed or
    // unusually large tile may not spend a whole interpreter turn just to discover it.
    fn discover(&mut self) -> Option<()> {
        let mut work = SCAN_PER_TURN;
        while let Some(range) = self.unnamed.last() {
            let mut unread = &self.bytes[range.start + self.inspected..range.end];
            while !unread.is_empty() {
                if work == 0 {
                    self.inspected = range.len() - unread.len();
                    return Some(());
                }
                work -= 1;
                let (tag, value) = field(&mut unread)?;
                if tag == 1 {
                    if let Some(kind) = Layer::parse(value.bytes()?)
                        && !self.layers.iter().any(|(k, _)| *k == kind)
                    {
                        self.layers.push((kind, range.clone()));
                    }
                    break;
                }
            }
            self.unnamed.pop();
            self.inspected = 0;
        }
        // Reserve the decoded geometry budget for roads and labels first.
        self.layers
            .sort_unstable_by_key(|(kind, _)| std::cmp::Reverse(*kind));
        Some(())
    }

    pub fn finish(self) -> Tile {
        self.tile
    }
}

/// MVT feature fields precede the tables they refer to. Keep ranges, not a copy of each feature.
struct LayerInput {
    kind: Layer,
    range: std::ops::Range<usize>,
    point_limit: usize,
    extent: u32,
    keys: usize,
    class_key: Option<usize>,
    name_keys: [Option<usize>; 3],
    values: Vec<Option<String>>,
    raw: Vec<std::ops::Range<usize>>,
    next: usize,
}

impl LayerInput {
    fn new(kind: Layer, range: std::ops::Range<usize>, point_limit: usize) -> Self {
        Self {
            kind,
            range,
            point_limit,
            extent: 4096,
            keys: 0,
            class_key: None,
            name_keys: [None; 3],
            values: Vec::new(),
            raw: Vec::new(),
            next: 0,
        }
    }

    /// Read on through the layer for one turn, and say how much of the turn was spent.
    fn scan(&mut self, bytes: &[u8]) -> Option<usize> {
        let mut unread = &bytes[self.range.clone()];
        let classified = matches!(self.kind, Layer::Road | Layer::StreetName | Layer::BuiltUp);
        let labels = matches!(self.kind, Layer::Place | Layer::StreetName);
        let mut spent = 0;
        while spent < SCAN_PER_TURN && !unread.is_empty() {
            spent += 1;
            let (tag, item) = field(&mut unread)?;
            match tag {
                2 if self.raw.len() < MAX_FEATURES => {
                    let feature = item.bytes()?;
                    if feature.len() <= FEATURE_BYTES {
                        let start = feature.as_ptr() as usize - bytes.as_ptr() as usize;
                        self.raw.push(start..start + feature.len());
                    }
                }
                3 if labels || classified => {
                    if self.keys >= TABLE_ENTRIES {
                        return None;
                    }
                    match item.bytes()? {
                        b"name:latin" => self.name_keys[0] = Some(self.keys),
                        b"name:en" | b"name_en" => self.name_keys[1] = Some(self.keys),
                        b"name" => self.name_keys[2] = Some(self.keys),
                        b"class" if classified => self.class_key = Some(self.keys),
                        _ => {}
                    }
                    self.keys += 1;
                }
                4 if labels || classified => {
                    if self.values.len() >= TABLE_ENTRIES {
                        return None;
                    }
                    spent += VALUE_COST;
                    let data = item.bytes()?;
                    let text = (data.len() <= 1024)
                        .then(|| {
                            fields(data).flatten().find_map(|(tag, item)| {
                                if tag != 1 {
                                    return None;
                                }
                                let value = item.bytes()?;
                                (value.len() <= 512)
                                    .then(|| std::str::from_utf8(value).ok())
                                    .flatten()
                            })
                        })
                        .flatten();
                    self.values.push(text.map(str::to_owned));
                }
                5 => self.extent = u32::try_from(item.number()?).ok()?,
                _ => {}
            }
        }
        self.range.start = self.range.end - unread.len();
        Some(spent)
    }

    /// Prepare features for one turn: whether the layer is done, and what the part cost (see [`WHOLE`]).
    fn decode(
        &mut self,
        bytes: &[u8],
        tile: &mut Tile,
        remaining: &mut usize,
    ) -> Option<(bool, usize)> {
        if !(1..=16_384).contains(&self.extent) {
            return None;
        }
        let mut points = 0;
        let mut work = 0;
        let mut cost = 0;
        // The largest share of its bounds the part has used: it ends at the first one it reaches.
        let spent = |points: usize, work: usize, cost: usize| {
            (points * WHOLE / POINTS_PER_TURN)
                .max(work * WHOLE / FEATURE_BYTES)
                .max(cost * WHOLE / WORK_PER_TURN)
        };
        while self.next < self.raw.len() && self.point_limit > 0 && tile.features < MAX_FEATURES {
            let feature = &bytes[self.raw[self.next].clone()];
            self.next += 1;
            work += feature.len().max(1);
            let limit = self.point_limit.min(FEATURE_POINTS);
            if let Some(feature) = self.feature(feature, limit) {
                let count = feature.geometry.len();
                points += count;
                if count > *remaining {
                    return None;
                }
                *remaining -= count;
                self.point_limit -= count;
                cost += tile.add(feature);
            }
            if spent(points, work, cost) >= WHOLE {
                return Some((false, spent(points, work, cost)));
            }
        }
        Some((true, spent(points, work, cost)))
    }

    fn feature(&self, feature: &[u8], limit: usize) -> Option<Feature> {
        let (mut tags, mut geom, mut ty) = (&[][..], &[][..], 0);
        for value in fields(feature) {
            let (field, value) = value?;
            match field {
                2 => tags = value.bytes()?,
                3 => ty = value.number()?,
                4 => geom = value.bytes()?,
                _ => {}
            }
        }
        let kind = match (self.kind, ty) {
            (Layer::Road, 2) => Layer::road(name(tags, self.class_key?, &self.values)?)?,
            (Layer::StreetName, 2) => {
                if let Some(class) = self.class_key.and_then(|key| name(tags, key, &self.values)) {
                    Layer::road(class)?;
                }
                self.kind
            }
            (Layer::Place, 1) => self.kind,
            (Layer::BuiltUp, 3) => match name(tags, self.class_key?, &self.values)? {
                "residential" | "commercial" | "industrial" | "retail" | "suburb" | "quarter"
                | "neighbourhood" => self.kind,
                _ => return None,
            },
            (Layer::Water | Layer::Park | Layer::Building, 3) => self.kind,
            _ => return None,
        };
        let label = if matches!(kind, Layer::Place | Layer::StreetName) {
            let value = self
                .name_keys
                .iter()
                .flatten()
                .find_map(|&k| name(tags, k, &self.values).filter(|s| readable_name(s)))?;
            if kind == Layer::StreetName
                && matches!(
                    value.to_lowercase().as_str(),
                    "strada fără nume"
                        | "strada fara nume"
                        | "unnamed"
                        | "unnamed road"
                        | "unnamed street"
                )
            {
                return None;
            }
            Some(
                value
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(68)
                    .collect::<String>(),
            )
        } else {
            None
        };
        let geometry = thinned(geometry(geom, self.extent, limit)?, FEATURE_KEPT);
        Some(Feature {
            kind,
            extent: self.extent,
            geometry,
            name: label,
        })
    }
}

/// At most about `most` points of a feature's geometry, for one too large to prepare in a turn. A long run or ring keeps
/// its first and last point and every nth between; a short one is kept whole, so the small shapes of a feature made of
/// many (a provider's combined footprints) are not thinned away; and once `most` is reached what is left of the feature
/// is not kept at all.
fn thinned(geometry: Vec<Pen>, most: usize) -> Vec<Pen> {
    const WHOLE: usize = 8;
    if geometry.len() <= most {
        return geometry;
    }
    let nth = geometry.len().div_ceil(most).max(2);
    let mut kept = Vec::with_capacity(most);
    let mut start = 0;
    while start < geometry.len() {
        // One run: where it begins, its lines, and its closing when it has one.
        let mut end = start + 1;
        while matches!(geometry.get(end), Some(Pen::Line(_))) {
            end += 1;
        }
        let lines = &geometry[start + 1..end];
        let closes = matches!(geometry.get(end), Some(Pen::Close));
        let whole = lines.len() <= WHOLE;
        let needed = 2 + if whole {
            lines.len()
        } else {
            lines.len() / nth + 1
        };
        if kept.len() + needed > most {
            break;
        }
        kept.push(geometry[start]);
        if whole {
            kept.extend_from_slice(lines);
        } else {
            kept.extend(
                lines
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| (index + 1) % nth == 0 || index + 1 == lines.len())
                    .map(|(_, pen)| *pen),
            );
        }
        if closes {
            kept.push(Pen::Close);
            end += 1;
        }
        start = end;
    }
    kept
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Full,
    Gesture,
    Pressure,
    /// A reduced first frame with the offline map and controls, before detailed layers.
    Opening,
}

impl Detail {
    pub fn preview(self) -> bool {
        self != Self::Full
    }
}

#[derive(Default)]
pub struct Drawing {
    tiles: Vec<packed::Placement>,
    pub labels: Vec<Label>,
    span: f64,
}

pub struct Label {
    pub x: i32,
    pub y: i32,
    pub width: u16,
    pub height: u16,
    pub name: String,
    pub street: Option<StreetLabel>,
}

#[derive(Clone, Copy)]
pub struct StreetLabel {
    pub centre: [f32; 2],
    pub direction: [f32; 2],
}

impl Label {
    pub const HEIGHT: u16 = 24;
    pub const FONT: f32 = 15.;
    pub const STREET_FONT: f32 = 13.;

    pub fn bounds(&self) -> [f64; 4] {
        [
            self.x as f64,
            self.y as f64,
            self.width.into(),
            self.height.into(),
        ]
    }

    pub fn place(
        name: &str,
        (x, y): (f64, f64),
        bounds: layers::Rect,
        labels: &[Self],
        clear: impl Fn([f64; 4]) -> bool,
    ) -> Option<Self> {
        if !bounds.contains(x, y) {
            return None;
        }
        let width = text_width(name, f64::from(Self::FONT)) as i32 + 8;
        let [left, top, right, bottom] = bounds.0;
        let height = i32::from(Self::HEIGHT);
        let [left, top, right, bottom] =
            [left + 4, top + 4, right - width - 4, bottom - height - 4];
        if right < left || bottom < top {
            return None;
        }
        let x = x.round() as i32;
        let y = y.round() as i32;
        for (x, y) in [
            (x - width / 2, y - height - 5),
            (x - width / 2, y + 5),
            (x - width - 7, y - height / 2),
            (x + 7, y - height / 2),
        ] {
            let (x, y) = (x.clamp(left, right), y.clamp(top, bottom));
            let rect = [x as f64, y as f64, width as f64, f64::from(Self::HEIGHT)];
            if clear(rect)
                && labels
                    .iter()
                    .all(|label| !crate::overlaps(rect, label.bounds()))
            {
                return Some(Self {
                    x,
                    y,
                    width: width as u16,
                    name: name.to_owned(),
                    height: Self::HEIGHT,
                    street: None,
                });
            }
        }
        None
    }
}

impl Label {
    fn on_street(
        feature: &Feature,
        tile: &packed::Placement,
        bounds: layers::Rect,
        labels: &[Self],
    ) -> Option<Self> {
        let name = feature.name.as_deref()?;
        if labels.iter().any(|label| label.name == name) {
            return None;
        }
        let needed = text_width(name, f64::from(Self::STREET_FONT)) + 8.;
        let height = 20.;
        let mut previous = None;
        let mut best = None;
        let mut longest = needed;
        for &pen in &feature.geometry {
            match pen {
                Pen::Move(p) => previous = Some(tile.point(feature.extent, p)),
                Pen::Close => previous = None,
                Pen::Line(p) => {
                    let end = tile.point(feature.extent, p);
                    let Some(start) = previous.replace(end) else {
                        continue;
                    };
                    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
                    let length = dx.hypot(dy);
                    if length <= longest {
                        continue;
                    }
                    let (x, y) = (start.0.midpoint(end.0), start.1.midpoint(end.1));
                    let sign = if dx < 0. || dx == 0. && dy > 0. {
                        -1.
                    } else {
                        1.
                    };
                    let (cos, sin) = (sign * dx / length, sign * dy / length);
                    let w = needed * cos.abs() + height * sin.abs();
                    let h = needed * sin.abs() + height * cos.abs();
                    let l = (x - w / 2.).floor();
                    let t = (y - h / 2.).floor();
                    let area = [l, t, (x + w / 2.).ceil() - l, (y + h / 2.).ceil() - t];
                    if !bounds.fits(area)
                        || labels
                            .iter()
                            .any(|label| crate::overlaps(label.bounds(), area))
                    {
                        continue;
                    }
                    longest = length;
                    best = Some(Self {
                        x: l as i32,
                        y: t as i32,
                        width: area[2] as u16,
                        height: area[3] as u16,
                        name: name.to_owned(),
                        street: Some(StreetLabel {
                            centre: [x as f32, y as f32],
                            direction: [cos as f32, sin as f32],
                        }),
                    });
                }
            }
        }
        best
    }
}

/// Room for a complete single-line label, including wide and fallback glyphs.
pub fn text_width(text: &str, em: f64) -> f64 {
    // The Canvas gives us a font size, not a shaper. Overestimate the bundled
    // font's advances rather than clip a long label to an undersized rectangle.
    let units: u32 = text
        .chars()
        .map(|c| match c {
            ' ' | '\u{202f}' | 'i' | 'l' | 'I' | 't' | 'f' | '\'' | '’' | '.' | ',' => 44,
            'W' | 'M' | 'w' | 'm' | '@' => 104,
            c if c.is_ascii_digit() => 68,
            c if c.is_ascii_uppercase() => 84,
            c if c.is_ascii() => 72,
            _ => 116,
        })
        .sum();
    (f64::from(units) * em / 100.).ceil()
}

impl Drawing {
    #[cfg(test)]
    pub fn render<'a>(
        tiles: impl Iterator<Item = &'a Tile> + Clone,
        view: View,
        frame: Frame,
    ) -> Self {
        Self::render_on(
            tiles,
            view,
            frame,
            &Camera::new(view, frame),
            layers::Rect::viewport(frame, frame),
        )
    }

    pub fn render_on<'a>(
        tiles: impl Iterator<Item = &'a Tile> + Clone,
        view: View,
        frame: Frame,
        camera: &Camera,
        label_bounds: layers::Rect,
    ) -> Self {
        let mut draw = Self {
            span: view.span,
            ..Self::default()
        };
        let Some(first) = tiles.clone().next() else {
            return draw;
        };
        let Some(grid) = packed::Grid::new(camera, view, frame, first.key.z) else {
            return draw;
        };
        let chosen: Vec<_> = tiles
            .filter_map(|tile| {
                grid.place(tile.key, Rc::clone(&tile.art))
                    .map(|place| (tile, place))
            })
            .collect();
        for (kind, count) in [(Layer::Place, 24), (Layer::StreetName, 12)] {
            if !kind.shown(view.span) {
                continue;
            }
            let mut left = count;
            'tiles: for (tile, place) in &chosen {
                for feature in tile.names.iter().filter(|f| f.kind == kind) {
                    if left == 0 {
                        break 'tiles;
                    }
                    let Some(name) = feature.name.as_deref() else {
                        continue;
                    };
                    if draw.labels.iter().any(|label| label.name == name) {
                        continue;
                    }
                    let label = if kind == Layer::StreetName {
                        Label::on_street(feature, place, label_bounds, &draw.labels)
                    } else {
                        feature.geometry.iter().find_map(|pen| match *pen {
                            Pen::Move(p) | Pen::Line(p) => Label::place(
                                name,
                                place.point(feature.extent, p),
                                label_bounds,
                                &draw.labels,
                                |_| true,
                            ),
                            Pen::Close => None,
                        })
                    };
                    if let Some(label) = label {
                        draw.labels.push(label);
                        left -= 1;
                    }
                }
            }
        }
        draw.tiles = chosen.into_iter().map(|(_, place)| place).collect();
        draw
    }

    pub fn has_roads(&self) -> bool {
        self.path_count(Layer::Road) > 0
            || self.path_count(Layer::LocalRoad) > 0
            || self.path_count(Layer::Path) > 0
    }

    pub fn has_detail(&self) -> bool {
        !self.labels.is_empty() || packed::has_detail(&self.tiles, self.span)
    }

    fn path_count(&self, kind: Layer) -> usize {
        if !kind.shown(self.span) {
            return 0;
        }
        self.tiles.iter().map(|tile| tile.count(kind)).sum()
    }

    #[cfg(test)]
    pub fn packed_counts(&self) -> [(usize, usize); 7] {
        packed::counts(&self.tiles, self.span)
    }

    pub fn paint(&self, list: &mut List<'_>, detail: Detail, scale: u32) -> bool {
        packed::paint(&self.tiles, list, self.span, detail, scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feature_too_large_for_a_turn_is_thinned_and_small_shapes_stay_whole() {
        let line = |n: i32| Pen::Line((n, n));
        // What fits is not touched.
        let small: Vec<Pen> = [Pen::Move((0, 0))]
            .into_iter()
            .chain((1..100).map(line))
            .collect();
        assert_eq!(thinned(small.clone(), 100), small);
        // One long ring: where it begins, where it ends and its closing stay, and no more than the most.
        let ring: Vec<Pen> = [Pen::Move((0, 0))]
            .into_iter()
            .chain((1..=9_000).map(line))
            .chain([Pen::Close])
            .collect();
        let kept = thinned(ring, 2_048);
        assert!(kept.len() <= 2_048 && kept.len() > 1_500, "{}", kept.len());
        assert_eq!(kept[0], Pen::Move((0, 0)));
        assert_eq!(kept[kept.len() - 2..], [line(9_000), Pen::Close]);
        // Many small shapes in one feature (a provider's combined footprints): each kept is kept whole, and what is past
        // the most is not kept in part.
        let footprints: Vec<Pen> = (0..1_000)
            .flat_map(|n| {
                [
                    Pen::Move((n, 0)),
                    Pen::Line((n + 1, 0)),
                    Pen::Line((n + 1, 1)),
                    Pen::Line((n, 1)),
                    Pen::Close,
                ]
            })
            .collect();
        let kept = thinned(footprints.clone(), 2_048);
        assert!(kept.len() <= 2_048 && kept.len() % 5 == 0, "{}", kept.len());
        assert_eq!(kept[..], footprints[..kept.len()]);
        assert!(kept.len() >= 2_000);
    }

    #[test]
    fn the_city_view_does_not_lose_buildings_behind_a_dense_road_layer() {
        let mut features: Vec<_> = (0..3_400)
            .map(|i| {
                let y = 850 + i % 2_200;
                Feature {
                    kind: Layer::Road,
                    extent: 4096,
                    geometry: vec![Pen::Move((800, y)), Pen::Line((3_200, y))],
                    name: None,
                }
            })
            .collect();
        features.push(Feature {
            kind: Layer::Building,
            extent: 4096,
            geometry: vec![
                Pen::Move((1_700, 1_750)),
                Pen::Line((2_150, 1_750)),
                Pen::Line((2_150, 2_200)),
                Pen::Line((1_700, 2_200)),
                Pen::Close,
            ],
            name: None,
        });
        let mut tile = Tile::new(Key {
            z: 14,
            x: 8192,
            y: 8192,
        });
        for feature in features {
            tile.add(feature);
        }
        let view = View {
            lon: 0.011,
            lat: -0.011,
            span: 0.092,
            roll: 0.,
        };
        for frame in [Frame(100, 100), Frame(1616, 930), Frame(3392, 1600)] {
            let draw = Drawing::render([&tile].into_iter(), view, frame);
            assert!(draw.has_roads());
            assert!(
                draw.path_count(Layer::Building) > 0,
                "no buildings at {}×{}",
                frame.0,
                frame.1
            );
        }
    }

    #[test]
    fn street_labels_follow_long_visible_roads_upright_and_ignore_unnamed() {
        fn field(tag: u8, bytes: &[u8]) -> Vec<u8> {
            assert!(bytes.len() < 128);
            [[tag << 3 | 2, bytes.len() as u8].as_slice(), bytes].concat()
        }
        fn zigzag(bytes: &mut Vec<u8>, n: i32) {
            let mut n = ((n << 1) ^ (n >> 31)) as u32;
            while n >= 128 {
                bytes.push(n as u8 | 128);
                n >>= 7;
            }
            bytes.push(n as u8);
        }
        let layer = |name: &str, class: &str, (x, y): Point, (end_x, end_y): Point| {
            let mut geometry = vec![9];
            zigzag(&mut geometry, x);
            zigzag(&mut geometry, y);
            geometry.push(10);
            zigzag(&mut geometry, end_x - x);
            zigzag(&mut geometry, end_y - y);
            let mut feature = vec![18, 4, 0, 0, 1, 1, 24, 2];
            feature.extend(field(4, &geometry));
            let mut layer = field(1, b"transportation_name");
            layer.extend(field(2, &feature));
            layer.extend(field(3, b"name"));
            layer.extend(field(3, b"class"));
            layer.extend(field(4, &field(1, name.as_bytes())));
            layer.extend(field(4, &field(1, class.as_bytes())));
            field(3, &layer)
        };
        let key = Key {
            z: 14,
            x: 8192,
            y: 8192,
        };
        let frame = Frame(1000, 760);
        let view = View {
            lon: 0.011,
            lat: -0.011,
            span: 0.029,
            roll: 18.,
        };
        let (start, end) = ((2800, 2200), (1300, 1450));
        for (name, class, end, shown) in [
            ("Strada Fagului", "minor", end, true),
            ("Strada fără nume", "minor", end, false),
            ("Vaporetto (V1)", "ferry", end, false),
            ("Strada Scurtă", "minor", (2798, 2201), false),
        ] {
            let mut decoder = Decode::start(key, layer(name, class, start, end)).unwrap();
            while !decoder.step().unwrap() {}
            let tile = decoder.finish();
            let drawing = Drawing::render([&tile].into_iter(), view, frame);
            let Some(label) = drawing.labels.first() else {
                assert!(!shown, "{name} should be shown on its road");
                continue;
            };
            assert!(shown, "{name} does not belong on the map");
            assert_eq!(label.name, name);
            let street = label.street.unwrap();
            let camera = Camera::new(view, frame);
            let ((ax, ay), (bx, by)) = (
                key.project(4096., start, &camera).unwrap(),
                key.project(4096., end, &camera).unwrap(),
            );
            let [cx, cy] = street.centre.map(f64::from);
            assert!((cx - ax.midpoint(bx)).hypot(cy - ay.midpoint(by)) < 1.);
            let [dx, dy] = street.direction.map(f64::from);
            assert!(dx > 0. && dy.abs() > 0.1);
            assert!((dx * (by - ay) - dy * (bx - ax)).abs() < 1.);
        }
    }

    #[test]
    fn city_scale_keeps_main_roads_without_promoting_paths_or_rail_to_streets() {
        fn field(tag: u8, bytes: &[u8]) -> Vec<u8> {
            [[tag << 3 | 2, bytes.len() as u8].as_slice(), bytes].concat()
        }
        let mut layer = field(1, b"transportation");
        for (value, y) in [0, 1, 2].into_iter().zip([100, 106, 112]) {
            let mut feature = field(2, &[0, value]);
            feature.extend([24, 2]);
            feature.extend(field(4, &[9, 80, 200 | (y - 100), 1, 10, 100, 0]));
            layer.extend(field(2, &feature));
        }
        layer.extend(field(3, b"class"));
        for class in [b"path".as_slice(), b"rail", b"primary"] {
            layer.extend(field(4, &field(1, class)));
        }
        layer.extend([40, 0x80, 0x20]);
        let key = Key {
            z: 12,
            x: 2048,
            y: 2048,
        };
        let mut decoder = Decode::start(key, field(3, &layer)).unwrap();
        while !decoder.step().unwrap() {}
        let tile = decoder.finish();
        let view = View {
            lon: 0.002,
            lat: -0.0022,
            span: 0.18,
            roll: 0.,
        };
        let draw = Drawing::render([&tile].into_iter(), view, Frame(800, 500));
        assert!(draw.has_roads());
        assert_eq!(
            draw.path_count(Layer::Path),
            0,
            "paths stay off at city scale"
        );
    }

    #[test]
    fn a_long_way_is_split_into_bounded_connected_batches() {
        let key = Key {
            z: 14,
            x: 8192,
            y: 8192,
        };
        let way = |geometry| Feature {
            kind: Layer::Road,
            extent: 4096,
            geometry,
            name: None,
        };
        let frame = Frame(888, 449);
        let view = View {
            lon: 0.01,
            lat: -0.01,
            span: 0.045,
            roll: 0.,
        };
        let short = way(vec![Pen::Move((100, 2000)), Pen::Line((3300, 2000))]);
        let mut tile = Tile::new(key);
        tile.add(short);
        assert_eq!(
            Drawing::render([&tile].into_iter(), view, frame).path_count(Layer::Road),
            1
        );
        let mut zigzag = vec![Pen::Move((100, 400))];
        zigzag.extend(
            (1..2900).map(|i| Pen::Line((100 + i % 1000, if i % 2 == 0 { 400 } else { 2800 }))),
        );
        tile.add(way(zigzag));
        assert!(
            Drawing::render([&tile].into_iter(), view, frame).path_count(Layer::Road) > 1,
            "a long way is split into complete connected batches instead of being discarded"
        );
    }

    #[test]
    fn detailed_views_cover_both_sides_of_the_date_line_without_exceeding_the_budget() {
        let view = View {
            lon: 179.999,
            lat: 0.,
            span: 0.04,
            roll: 0.,
        };
        let keys = visible(view, Frame(400, 750), 14);
        assert!(keys.len() <= MAX_TILES);
        assert!(keys.iter().any(|k| k.x == 0));
        assert!(keys.iter().any(|k| k.x == (1 << k.z) - 1));
    }
    #[test]
    fn overflowing_or_truncated_geometry_is_not_read() {
        assert!(geometry(&[0x09, 0xff], 4096, 10).is_none());
        assert!(geometry(&[0xff; 20], 4096, 10).is_none());
        assert!(Decode::start(Key { z: 0, x: 0, y: 0 }, vec![0x1a, 0x10, 0xff]).is_none());
    }
}
