use crate::{
    geometry::Paths,
    world::{Camera, Frame},
};
const DATA: &[u8] = include_bytes!("world/borders.bin");
fn n(b: &[u8], i: usize) -> f64 {
    f64::from(i16::from_le_bytes([b[i], b[i + 1]])) / 100.
}

fn outlines(span: f64) -> &'static [u8] {
    let offset = |i| u32::from_le_bytes(DATA[i..i + 4].try_into().unwrap()) as usize + 8;
    if span > 160. {
        &DATA[offset(4)..]
    } else if span > 45. {
        &DATA[offset(0)..offset(4)]
    } else {
        &DATA[8..offset(0)]
    }
}
/// Clip an open segment, not a polygon: the edge of the window is never drawn as a border.
pub(crate) fn clipped(a: (f64, f64), b: (f64, f64), f: Frame) -> Option<((i32, i32), (i32, i32))> {
    let (width, height) = (f64::from(f.0), f64::from(f.1));
    let round = |(x, y): (f64, f64)| (x.round() as i32, y.round() as i32);
    let inside = |(x, y)| (0. ..=width).contains(&x) && (0. ..=height).contains(&y);
    if inside(a) && inside(b) {
        return Some((round(a), round(b)));
    }
    if [a.0.max(b.0), a.1.max(b.1)].into_iter().any(|n| n < 0.)
        || a.0.min(b.0) > width
        || a.1.min(b.1) > height
    {
        return None;
    }
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t, mut u) = (0_f64, 1_f64);
    for (p, q) in [
        (-dx, a.0),
        (dx, width - a.0),
        (-dy, a.1),
        (dy, height - a.1),
    ] {
        if p == 0. {
            if q < 0. {
                return None;
            }
        } else if p < 0. {
            t = t.max(q / p);
        } else {
            u = u.min(q / p);
        }
        if t > u {
            return None;
        }
    }
    let at = |v: f64| round((a.0 + dx * v, a.1 + dy * v));
    Some((at(t), at(u)))
}
pub fn path(camera: &Camera, f: Frame, span: f64) -> Paths {
    let area = camera.visible_area();
    let mut visible = Vec::new();
    let mut data = outlines(span);
    while data.len() >= 10 {
        let count = u16::from_le_bytes([data[8], data[9]]) as usize;
        let (raw, rest) = data[10..].split_at(count * 4);
        let (west, south, east, north) = (n(data, 0), n(data, 2), n(data, 4), n(data, 6));
        data = rest;
        if north >= area.south
            && south <= area.north
            && area.longitude_ranges().any(|(a, b)| east >= a && west <= b)
        {
            visible.push(raw);
        }
    }
    let mut points = 0;
    let mut longest = 0;
    for line in &visible {
        let count = line.len() / 4;
        points += count;
        longest = longest.max(count);
    }
    let mut stride = points.div_ceil(1_250).max(1);
    loop {
        let (out, complete) = draw(camera, f, &visible, stride);
        if complete || stride >= longest {
            return out;
        }
        // Widen sampling across all lines before spending the last bytes on
        // only the earliest borders in the file.
        stride *= 2;
    }
}

fn draw(camera: &Camera, f: Frame, visible: &[&[u8]], stride: usize) -> (Paths, bool) {
    let mut out = Paths::default();
    for &raw in visible {
        let transform = |p: &[u8]| camera.vector(n(p, 0), n(p, 2));
        let mut a = transform(&raw[..4]);
        let (body, last) = raw[4..].split_at(raw.len() - 8);
        for p in body
            .chunks_exact(4)
            .skip(stride - 1)
            .step_by(stride)
            .chain([last])
        {
            let b = transform(p);
            let screen = camera.visible_edge(a, b);
            a = b;
            let Some((a, b)) = screen.and_then(|(a, b)| clipped(a, b, f)) else {
                continue;
            };
            if a == b {
                continue;
            }
            if out.len() + 2 > 1650 {
                return (out, false);
            }
            out.line(a, b);
        }
    }
    (out, true)
}
