//! Camera-local label bounds.
use crate::world::Frame;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect(pub [i32; 4]);

impl Rect {
    pub fn viewport(viewport: Frame, frame: Frame) -> Self {
        let [w, h, cw, ch] = [viewport.0, viewport.1, frame.0, frame.1].map(i32::from);
        let [x, y] = [(cw - w) / 2, (ch - h) / 2];
        Self([x, y, x + w, y + h])
    }

    pub(crate) fn contains(self, x: f64, y: f64) -> bool {
        let [left, top, right, bottom] = self.0.map(f64::from);
        (left..right).contains(&x) && (top..bottom).contains(&y)
    }

    pub(crate) fn fits(self, [x, y, width, height]: [f64; 4]) -> bool {
        let [left, top, right, bottom] = self.0.map(f64::from);
        x >= left && y >= top && x + width <= right && y + height <= bottom
    }
}
