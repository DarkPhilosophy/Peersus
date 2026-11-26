//! Offline regional OSM extracts. The worker never contacts a tile server or learns a file path.
use crate::{
    atlas::Detail,
    world::{Camera, Frame},
};
const REGIONS: [&[u8]; 2] = [
    include_bytes!("world/bucharest.bin"),
    include_bytes!("world/monaco.bin"),
];

#[derive(Default)]
pub struct Progress {
    cell: usize,
    segment: usize,
    path_end: usize,
    label_end: usize,
}

fn u32at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().expect("built-in map header"))
}
fn area(b: &[u8]) -> (i32, i32, i32, i32) {
    (
        u32at(b, 0) as i32,
        u32at(b, 4) as i32,
        i32::from(u16::from_le_bytes([b[8], b[9]])),
        i32::from(u16::from_le_bytes([b[10], b[11]])),
    )
}
pub fn covered(lon: f64, lat: f64) -> bool {
    REGIONS.iter().any(|b| {
        let (x, y, w, h) = area(b);
        let (lon, lat) = (lon * 100., lat * 100.);
        lon >= x as f64 && lon < (x + w) as f64 && lat >= y as f64 && lat < (y + h) as f64
    })
}
pub fn draw(
    camera: &Camera,
    span: f64,
    frame: Frame,
    d: &mut Detail,
    progress: &mut Progress,
    work: &mut crate::atlas::Work,
) -> bool {
    let bounds = camera.visible_area();
    let mut cell = 0;
    for b in REGIONS {
        let (x0, y0, w, h) = area(b);
        let Some((west, east)) = bounds
            .longitude_ranges()
            .find(|&(west, east)| east * 100. >= f64::from(x0) && west * 100. < f64::from(x0 + w))
        else {
            continue;
        };
        let nn = u32at(b, 12) as usize;
        let index = &b[16 + nn..];
        let names = &b[16..16 + nn];
        let data = &index[4 * (w * h + 1) as usize..];
        let (left, right, low, high) = (
            (west * 100.).floor() as i32 - x0,
            (east * 100.).floor() as i32 - x0,
            (bounds.south * 100.).floor() as i32 - y0,
            (bounds.north * 100.).floor() as i32 - y0,
        );
        let mut cells: Vec<_> = (low.max(0)..=high.min(h - 1))
            .flat_map(|y| (left.max(0)..=right.min(w - 1)).map(move |x| (x, y)))
            .collect();
        cells.sort_unstable_by_key(|&(x, y)| {
            let a = x - (left + right) / 2;
            let b = y - (low + high) / 2;
            a * a + b * b
        });
        let count = cells.len().max(1);
        let budget = (1200 / count).max(16);
        let labels = 8_usize.div_ceil(count);
        let [west, east, south, north] = [
            (west * 1e5).floor() as i32,
            (east * 1e5).ceil() as i32,
            (bounds.south * 1e5).floor() as i32,
            (bounds.north * 1e5).ceil() as i32,
        ];
        for (x, y) in cells {
            let current = cell;
            cell += 1;
            if current < progress.cell {
                continue;
            }
            let (lon, lat) = ((x + x0) * 1000, (y + y0) * 1000);
            if progress.segment == 0 {
                progress.path_end = d.roads.len().saturating_add(budget);
                progress.label_end = (d.names.len() + labels).min(8);
            }
            let i = 4 * (y * w + x) as usize;
            let segs = &data[u32at(index, i) as usize..u32at(index, i + 4) as usize];
            for s in segs.chunks_exact(13).skip(progress.segment) {
                if d.roads.len() + 2 >= 1200 {
                    return true;
                }
                if d.roads.len() + 2 >= progress.path_end {
                    break;
                }
                if span > 0.09 && s[0] > 3 {
                    break;
                }
                if span > 0.045 && s[0] > 5 {
                    break;
                }
                if !work.scan() {
                    return false;
                }
                progress.segment += 1;
                let n = |i| i32::from(i16::from_le_bytes([s[i], s[i + 1]]));
                let (ax, ay, bx, by) = (n(1), n(3), n(5), n(7));
                if x != left.max(x + ax.min(bx).div_euclid(1000)).max(0)
                    || y != low.max(y + ay.min(by).div_euclid(1000)).max(0)
                    || lon + ax.min(bx) > east
                    || lon + ax.max(bx) < west
                    || lat + ay.min(by) > north
                    || lat + ay.max(by) < south
                {
                    continue;
                }
                if !work.project() {
                    progress.segment -= 1;
                    return false;
                }
                let project =
                    |a, b| camera.point(f64::from(lon + a) / 1e5, f64::from(lat + b) / 1e5);
                let (Some((a, b)), Some((c, e))) = (project(ax, ay), project(bx, by)) else {
                    continue;
                };
                if a.max(c) < 0.
                    || b.max(e) < 0.
                    || a.min(c) > f64::from(frame.0)
                    || b.min(e) > f64::from(frame.1)
                {
                    continue;
                }
                let start = (a.round() as i32, b.round() as i32);
                let end = (c.round() as i32, e.round() as i32);
                if start == end {
                    continue;
                }
                d.roads.line(start, end);
                if d.names.len() >= progress.label_end {
                    continue;
                }
                let pos = u32at(s, 9) as usize;
                let len = usize::from(names[pos]);
                if len == 0 {
                    continue;
                }
                let Ok(name) = std::str::from_utf8(&names[pos + 1..pos + 1 + len]) else {
                    continue;
                };
                if start.0 >= 2
                    && start.1 >= 4
                    && start.0 + 120 < i32::from(frame.0)
                    && start.1 + 18 < i32::from(frame.1)
                    && !d.names.iter().any(|&(u, v, l)| {
                        l == name || ((start.0 - u).abs() < 125 && (start.1 - v).abs() < 26)
                    })
                {
                    d.names.push((start.0, start.1, name));
                }
            }
            progress.cell += 1;
            progress.segment = 0;
        }
    }
    true
}
