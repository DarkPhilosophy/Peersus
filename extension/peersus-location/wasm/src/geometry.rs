//! Camera-local geometry. A batch can hold several PSD1 paths without ever splitting a ring.
use crate::canvas_sdk::{List, MAX_PATH_VERBS, Paint, Path};

pub type Point = (i32, i32);

#[derive(Default, Debug, PartialEq)]
pub struct Paths {
    batches: Vec<Path>,
    last: Option<Point>,
    verbs: usize,
}

impl Paths {
    pub fn is_empty(&self) -> bool {
        self.verbs == 0
    }

    pub fn len(&self) -> usize {
        self.verbs
    }

    fn batch(&mut self, count: usize) -> &mut Path {
        if self
            .batches
            .last()
            .is_none_or(|p| p.len() + count > MAX_PATH_VERBS)
        {
            self.batches.push(Path::default());
            self.last = None;
        }
        self.batches.last_mut().expect("space for the bounded path")
    }

    pub fn line(&mut self, a: Point, b: Point) {
        if a == b {
            return;
        }
        // Leave room for an explicit move when the previous scene batch is full.
        self.batch(2);
        let draw = self.batches.last_mut().expect("a batch was obtained");
        if self.last != Some(a) {
            draw.move_to([a.0 as f32, a.1 as f32]);
            self.verbs += 1;
        }
        draw.line_to([b.0 as f32, b.1 as f32]);
        self.verbs += 1;
        self.last = Some(b);
    }

    pub fn append(&mut self, contour: &Path) {
        if contour.is_empty() || contour.len() > MAX_PATH_VERBS {
            return;
        }
        self.batch(contour.len()).append(contour);
        self.last = None;
        self.verbs += contour.len();
    }

    pub fn paint(&self, list: &mut List<'_>, paint: Paint) {
        for batch in &self.batches {
            list.path(batch, paint);
        }
    }

    #[cfg(test)]
    pub fn contours(&self) -> Vec<Vec<(f64, f64)>> {
        let mut rings = Vec::new();
        for batch in &self.batches {
            let mut bytes = batch.bytes();
            while let Some((&op, rest)) = bytes.split_first() {
                bytes = rest;
                match op {
                    0 | 1 => {
                        let (xy, rest) = bytes.split_at(8);
                        bytes = rest;
                        let read =
                            |i| f64::from(f32::from_le_bytes(xy[i..i + 4].try_into().unwrap()));
                        if op == 0 {
                            rings.push(Vec::new());
                        }
                        rings.last_mut().unwrap().push((read(0), read(4)));
                    }
                    4 => {}
                    _ => panic!("expected line geometry, got {op}"),
                }
            }
        }
        rings
    }
}

/// The complete ring, with adjacent equal projected vertices removed.
pub fn ring(path: &mut Path, points: impl IntoIterator<Item = (f64, f64)>) {
    let mut points = points
        .into_iter()
        .map(|(x, y)| [x.round() as f32, y.round() as f32]);
    let Some(first) = points.next() else {
        return;
    };
    path.move_to(first);
    let mut last = first;
    for point in points {
        if last != point {
            path.line_to(point);
            last = point;
        }
    }
    path.close();
}
