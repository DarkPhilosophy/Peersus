//! Peersus `PSD1` display-list writer (see docs/api/service-messages.md). Guests can carry
//! this file alongside `host.rs`; it needs no app crate, renderer, or platform libraries.
#![allow(dead_code)] // Each guest uses only the primitives it draws.

pub const SERVICE: &str = "peersus.canvas";
pub const DATA_KIND: i32 = 2;
pub const PRESENT_OK: i32 = 0;
pub const PRESENT_LIMITED: i32 = -2;
pub const PRESENT_STREAM_FAILED: i32 = -3;
pub const MAX_PATH_VERBS: usize = 512;
const MAX_COMMANDS: usize = 4096;
const MAX_VERBS: usize = 65_536;
const MAX_LIST_BYTES: usize = 4 * 1024 * 1024;
const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DIMENSION: u32 = 4096;

#[derive(Clone, Copy)]
pub enum Color {
    Rgba([u8; 4]),
    Surface,
    Raised,
    Text,
    Muted,
    Border,
    Scrim,
    Accent,
    Success,
    Danger,
}

impl Color {
    fn write(self, out: &mut Vec<u8>) {
        out.push(match self {
            Self::Rgba(rgba) => {
                out.extend([0, rgba[0], rgba[1], rgba[2], rgba[3]]);
                return;
            }
            Self::Surface => 2,
            Self::Raised => 3,
            Self::Text => 4,
            Self::Muted => 5,
            Self::Border => 6,
            Self::Scrim => 7,
            Self::Accent => 8,
            Self::Success => 9,
            Self::Danger => 10,
        });
    }
}

/// Straight RGBA in CanvasSize palette order (surface through danger, without `dark`).
pub type Palette = [[u8; 4]; 9];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Raster {
    pub pixels: [u32; 2],
    pub scale: u32,
}

impl Raster {
    /// Pixel dimensions for a size in thousandths of a logical pixel and scale ×1000.
    pub fn new(logical: [u32; 2], scale: u32) -> Option<Self> {
        if !(1..=16_000).contains(&scale) {
            return None;
        }
        let physical = |n| (u64::from(n) * u64::from(scale)).div_ceil(1_000_000) as u32;
        let pixels = logical.map(physical);
        if pixels.iter().any(|&n| !(1..=MAX_DIMENSION).contains(&n))
            || u64::from(pixels[0]) * u64::from(pixels[1]) * 4 > MAX_FRAME_BYTES
        {
            return None;
        }
        Some(Self { pixels, scale })
    }
}

pub struct Frame<'a> {
    pub id: &'a str,
    pub stream: i32,
    pub seq: u64,
    pub settled: u64,
    pub raster: Raster,
    pub palette: &'a Palette,
}

fn numbers(out: &mut Vec<u8>, values: impl IntoIterator<Item = f32>) {
    for value in values {
        out.extend(value.to_le_bytes());
    }
}

/// Compact path data, reusable on successive frames and at different paint styles.
#[derive(Default, Debug, PartialEq)]
pub struct Path {
    verbs: usize,
    bytes: Vec<u8>,
}

impl Path {
    pub fn len(&self) -> usize {
        self.verbs
    }
    pub fn is_empty(&self) -> bool {
        self.verbs == 0
    }
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.verbs = 0;
    }
    pub fn append(&mut self, contours: &Self) {
        self.bytes.extend_from_slice(&contours.bytes);
        self.verbs += contours.verbs;
    }
    fn verb<const N: usize>(&mut self, token: u8, coords: [f32; N]) {
        self.verbs += 1;
        self.bytes.push(token);
        numbers(&mut self.bytes, coords);
    }
    pub fn move_to(&mut self, [x, y]: [f32; 2]) {
        self.verb(0, [x, y]);
    }
    pub fn line_to(&mut self, [x, y]: [f32; 2]) {
        self.verb(1, [x, y]);
    }
    pub fn quad_to(&mut self, coords: [f32; 4]) {
        self.verb(2, coords);
    }
    pub fn cubic_to(&mut self, coords: [f32; 6]) {
        self.verb(3, coords);
    }
    pub fn close(&mut self) {
        self.verb(4, []);
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Copy)]
pub enum Paint {
    Fill(Color, u8),
    /// Logical stroke width, round cap and join, then opacity (0–255).
    Stroke(Color, f32, u8),
}

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Align {
    Left = 0,
    Center = 1,
    Right = 2,
}

/// Build one complete bounded list into a caller-owned reusable buffer.
pub struct List<'a> {
    out: &'a mut Vec<u8>,
    commands: usize,
    verbs: usize,
    valid: bool,
}

impl<'a> List<'a> {
    pub fn begin(out: &'a mut Vec<u8>, frame: Frame<'_>) -> Self {
        out.clear();
        out.extend(b"PSD1");
        out.extend([1, frame.id.len() as u8]);
        out.extend_from_slice(frame.id.as_bytes());
        out.extend(frame.stream.to_le_bytes());
        out.extend(frame.seq.to_le_bytes());
        out.extend(frame.settled.to_le_bytes());
        for n in [
            frame.raster.pixels[0],
            frame.raster.pixels[1],
            frame.raster.scale,
        ] {
            out.extend(n.to_le_bytes());
        }
        for rgba in frame.palette {
            out.extend(rgba);
        }
        Self {
            out,
            commands: 0,
            verbs: 0,
            valid: !frame.id.is_empty()
                && frame.id.len() <= 64
                && frame.stream > 0
                && frame.seq > 0,
        }
    }

    pub fn clear(&mut self, color: Color) {
        self.command(2);
        color.write(self.out);
    }

    pub fn path(&mut self, path: &Path, paint: Paint) {
        if path.is_empty() {
            return;
        }
        let len = path.len();
        if len > MAX_PATH_VERBS || len > MAX_VERBS - self.verbs {
            self.valid = false;
            return;
        }
        self.verbs += len;
        self.command(3);
        let (style, color, width, opacity) = match paint {
            Paint::Fill(color, alpha) => (0, color, 0., alpha),
            Paint::Stroke(color, width, alpha) => (1, color, width, alpha),
        };
        self.out.push(style);
        color.write(self.out);
        numbers(self.out, [width]);
        self.out.extend([1, 1, opacity]);
        self.out.extend((len as u16).to_le_bytes());
        self.out.extend_from_slice(path.bytes());
    }

    pub fn text(&mut self, text: &str, size: f32, color: Color, at: [f32; 2], align: Align) {
        if text.len() > 1024 || text.chars().any(char::is_control) {
            self.valid = false;
            return;
        }
        self.command(4);
        numbers(self.out, [size]);
        color.write(self.out);
        numbers(self.out, at);
        self.out.push(align as u8);
        self.out.extend((text.len() as u16).to_le_bytes());
        self.out.extend_from_slice(text.as_bytes());
    }

    pub fn clipped(&mut self, rect: [f32; 4], draw: impl FnOnce(&mut Self)) {
        self.command(6);
        numbers(self.out, rect);
        draw(self);
        self.command(7);
    }

    /// Apply a local affine transform only to the commands written in `draw`.
    pub fn transformed(&mut self, affine: [f32; 6], draw: impl FnOnce(&mut Self)) {
        self.command(8);
        numbers(self.out, affine);
        draw(self);
        self.command(9);
    }

    pub fn remaining_verbs(&self) -> usize {
        MAX_VERBS - self.verbs
    }

    pub fn remaining_commands(&self) -> usize {
        MAX_COMMANDS.saturating_sub(self.commands)
    }

    fn command(&mut self, opcode: u8) {
        self.commands += 1;
        self.out.push(opcode);
    }

    pub fn finish(self) -> Option<&'a [u8]> {
        if !self.valid || self.commands > MAX_COMMANDS || self.out.len() >= MAX_LIST_BYTES {
            return None;
        }
        self.out.push(11);
        Some(self.out)
    }
}

#[cfg(target_family = "wasm")]
mod import {
    #[link(wasm_import_module = "peersus_v1")]
    unsafe extern "C" {
        pub(super) fn data_open(kind: i32, claim: u64) -> i32;
        pub(super) fn canvas_present(buffer: *const u8, length: usize) -> i32;
    }
}

#[cfg(target_family = "wasm")]
pub fn open(claim: u64) -> Option<i32> {
    // SAFETY: the synchronous ABI accepts a scalar, owner-issued claim.
    let handle = unsafe { import::data_open(DATA_KIND, claim) };
    (handle > 0).then_some(handle)
}

#[cfg(target_family = "wasm")]
pub fn present(list: &[u8]) -> i32 {
    // SAFETY: the synchronous import reads a live immutable slice before returning.
    unsafe { import::canvas_present(list.as_ptr(), list.len()) }
}
