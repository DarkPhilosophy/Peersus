//! Plugin-rendered canvases: page metadata and binary frame and display-list contracts.
//! This module does not open streams, allocate pixel buffers, or interpret drawings.

use crate::name;
use serde::{Deserialize, Serialize};

pub const MAX_CANVAS_ID_BYTES: usize = 64;
pub const MAX_CANVAS_A11Y: usize = 512;
pub const MAX_CANVAS_DIMENSION: u32 = 4096;
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_CANVAS_FPS: usize = 60;
pub const MAX_CANVAS_BYTES_PER_SECOND: usize = 256 * 1024 * 1024;
pub const MAX_CANVASES_PER_EXTENSION: usize = 4;
/// Total frame storage retained by the owner, and separately by a window, for one extension.
pub const MAX_CANVAS_BYTES_PER_EXTENSION: usize = 64 * 1024 * 1024;
/// Highest supported device-pixels/logical-pixel ratio, in thousandths.
pub const MAX_CANVAS_SCALE: u32 = 16_000;

/// Canvas host service, version 1. `open` takes `{page, canvas}` and returns `{claim}`.
/// A canvas must be declared in the page and mounted in a window before a claim is issued.
pub const SERVICE: &str = "peersus.canvas";
pub const OPEN: &str = "open";

/// `data_open` kind for an application-issued canvas claim. WASM presents a display list with
/// `canvas_present`; native SDK users write finished `PSF1` frames with `data_write`.
/// `data_close` releases the handle; a new connection needs a fresh claim.
pub const CANVAS_DATA_KIND: i32 = 2;
pub const PRESENT_OK: i32 = 0;
pub const PRESENT_INVALID: i32 = -1;
pub const PRESENT_LIMITED: i32 = -2;
/// Streaming failed; the helper closed this handle. Obtain a fresh claim before reopening.
pub const PRESENT_STREAM_FAILED: i32 = -3;

pub fn valid_id(id: &str) -> bool {
    id.len() <= MAX_CANVAS_ID_BYTES && name::valid(id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasRole {
    Button,
    Link,
    Image,
    Text,
    Group,
}

/// Canvas-local logical coordinates in thousandths of a pixel; windows clip to the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl CanvasRect {
    fn valid(self) -> bool {
        const LIMIT: i64 = 16_384_000;
        let valid_axis = |start: i32, length: u32| {
            let start = i64::from(start);
            let length = i64::from(length);
            (-LIMIT..=LIMIT).contains(&start)
                && (1..=LIMIT).contains(&length)
                && start + length <= LIMIT
        };
        valid_axis(self.x, self.width) && valid_axis(self.y, self.height)
    }
}

/// One semantic overlay over the raster. Actionable roles send the ordinary `press` event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasA11y {
    pub id: String,
    pub role: CanvasRole,
    pub label: String,
    pub rect: CanvasRect,
}

impl CanvasA11y {
    pub(crate) fn valid(&self) -> bool {
        name::valid(&self.id)
            && name::text(&self.label, crate::contributions::MAX_LABEL)
            && self.rect.valid()
    }
}

/// The frontend's theme tokens as straight sRGB RGBA bytes. `dark` is a rendering hint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasPalette {
    pub dark: bool,
    pub surface: [u8; 4],
    pub raised: [u8; 4],
    pub text: [u8; 4],
    pub muted: [u8; 4],
    pub border: [u8; 4],
    pub scrim: [u8; 4],
    pub accent: [u8; 4],
    pub success: [u8; 4],
    pub danger: [u8; 4],
}

pub const FRAME_MAGIC: [u8; 4] = *b"PSF1";
/// Read these bytes first, then use [`FrameHeader::encoded_len`] to read only the remaining header.
pub const FRAME_PREFIX_BYTES: usize = FRAME_MAGIC.len() + 1;
const FRAME_SUFFIX_BYTES: usize = 4 + 4 + 4 + 1 + 8 + 8;
pub const MAX_FRAME_HEADER_BYTES: usize =
    FRAME_PREFIX_BYTES + MAX_CANVAS_ID_BYTES + FRAME_SUFFIX_BYTES;
pub const RGBA8_PREMUL_SRGB: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("invalid canvas frame header")]
    Header,
    #[error("invalid canvas identifier")]
    Id,
    #[error("canvas frame dimensions or stride exceed their bounds")]
    Layout,
    #[error("unsupported canvas pixel format")]
    Format,
    #[error("canvas frame sequence must be nonzero")]
    Sequence,
}

/// A checked RGBA pixel layout. Dimensions are device pixels and stride is bytes per row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameLayout {
    width: u32,
    height: u32,
    stride: u32,
}

impl FrameLayout {
    pub fn new(width: u32, height: u32, stride: u32) -> Result<Self, FrameError> {
        if !(1..=MAX_CANVAS_DIMENSION).contains(&width)
            || !(1..=MAX_CANVAS_DIMENSION).contains(&height)
            || stride < width * 4
            || !stride.is_multiple_of(4)
            || u64::from(stride) * u64::from(height) > MAX_FRAME_BYTES as u64
        {
            return Err(FrameError::Layout);
        }
        Ok(Self {
            width,
            height,
            stride,
        })
    }

    pub fn tight(width: u32, height: u32) -> Result<Self, FrameError> {
        Self::new(
            width,
            height,
            width.checked_mul(4).ok_or(FrameError::Layout)?,
        )
    }

    pub fn width(self) -> u32 {
        self.width
    }
    pub fn height(self) -> u32 {
        self.height
    }
    pub fn stride(self) -> u32 {
        self.stride
    }
    pub fn byte_len(self) -> usize {
        self.stride as usize * self.height as usize
    }
}

/// One `PSF1` header, followed on the wire by exactly `layout.byte_len()` bytes of RGBA8
/// premultiplied sRGB. `seq` increases on this stream; `settled` identifies the last gesture
/// the extension rendered (`0` if none). Numeric fields are little-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader<'a> {
    pub id: &'a str,
    pub layout: FrameLayout,
    pub seq: u64,
    pub settled: u64,
}

impl<'a> FrameHeader<'a> {
    /// Wire length including prefix and ID, excluding pixels. Does not trust the ID bytes yet.
    pub fn encoded_len(prefix: &[u8; FRAME_PREFIX_BYTES]) -> Result<usize, FrameError> {
        let id_len = usize::from(prefix[FRAME_MAGIC.len()]);
        if !prefix.starts_with(&FRAME_MAGIC) || !(1..=MAX_CANVAS_ID_BYTES).contains(&id_len) {
            return Err(FrameError::Header);
        }
        Ok(FRAME_PREFIX_BYTES + id_len + FRAME_SUFFIX_BYTES)
    }

    /// Read a complete header, before allocating or reading any pixel bytes.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, FrameError> {
        let (prefix, rest) = bytes
            .split_first_chunk::<FRAME_PREFIX_BYTES>()
            .ok_or(FrameError::Header)?;
        if bytes.len() != Self::encoded_len(prefix)? {
            return Err(FrameError::Header);
        }
        let (id, mut fields) = rest.split_at(usize::from(prefix[FRAME_MAGIC.len()]));
        let id = std::str::from_utf8(id).map_err(|_| FrameError::Id)?;
        if !valid_id(id) {
            return Err(FrameError::Id);
        }
        let width = u32::from_le_bytes(take(&mut fields)?);
        let height = u32::from_le_bytes(take(&mut fields)?);
        let stride = u32::from_le_bytes(take(&mut fields)?);
        let layout = FrameLayout::new(width, height, stride)?;
        if take::<1>(&mut fields)?[0] != RGBA8_PREMUL_SRGB {
            return Err(FrameError::Format);
        }
        let seq = u64::from_le_bytes(take(&mut fields)?);
        if seq == 0 {
            return Err(FrameError::Sequence);
        }
        let settled = u64::from_le_bytes(take(&mut fields)?);
        Ok(Self {
            id,
            layout,
            seq,
            settled,
        })
    }

    /// Encode into a fixed-size caller-owned buffer; return just the header bytes written.
    pub fn encode(self, into: &mut [u8; MAX_FRAME_HEADER_BYTES]) -> Result<&[u8], FrameError> {
        if !valid_id(self.id) {
            return Err(FrameError::Id);
        }
        if self.seq == 0 {
            return Err(FrameError::Sequence);
        }
        let mut at = 0;
        for field in [
            FRAME_MAGIC.as_slice(),
            &[self.id.len() as u8],
            self.id.as_bytes(),
            &self.layout.width.to_le_bytes(),
            &self.layout.height.to_le_bytes(),
            &self.layout.stride.to_le_bytes(),
            &[RGBA8_PREMUL_SRGB],
            &self.seq.to_le_bytes(),
            &self.settled.to_le_bytes(),
        ] {
            into[at..at + field.len()].copy_from_slice(field);
            at += field.len();
        }
        Ok(&into[..at])
    }
}

fn take<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N], FrameError> {
    let (field, rest) = bytes.split_first_chunk::<N>().ok_or(FrameError::Header)?;
    *bytes = rest;
    Ok(*field)
}

/// `PSD1` display list (little-endian), passed in its entirety to `canvas_present(ptr,len)`.
/// `PSD1` + `BEGIN` + id length (`u8`) and ID + canvas handle (`i32`) + seq and settled
/// (`u64` each) + device width, height, scale (`u32` each; scale in thousandths) + palette
/// (9 consecutive straight sRGB RGBA values: surface, raised, text, muted, border, scrim,
/// accent, success, danger). After those fields come commands and finally `END`, with no suffix.
///
/// Coordinates, sizes, and affine coefficients are finite `f32` in logical pixels (except
/// image definition's pixel dimensions). A colour is `u8`: 0 plus four straight RGBA bytes,
/// 1 for transparent, or 2–10 for the colours in the palette order above.
pub mod display {
    pub const MAGIC: [u8; 4] = *b"PSD1";
    pub const BEGIN: u8 = 1;
    /// Colour.
    pub const CLEAR: u8 = 2;
    /// Mode (`u8`: fill=0, stroke=1), colour, stroke width (`f32`), cap and join (`u8` each:
    /// butt/round/square and miter/round/bevel), opacity (`u8`: 0–255), verb count (`u16`),
    /// then compact verbs (each `u8` + coordinates): move=0, line=1, quad=2, cubic=3, close=4.
    pub const PATH: u8 = 3;
    /// Font size, colour, x, y (baseline), alignment (`u8`: start=0, center=1, end=2),
    /// byte length (`u16`), one line of UTF-8.
    pub const TEXT: u8 = 4;
    /// An already defined image ID (`u32`), then x, y, width, height (`f32`).
    pub const IMAGE: u8 = 5;
    /// x, y, width, height (`f32`). Push/pop are strictly nested across both kinds of stack entry.
    pub const CLIP_PUSH: u8 = 6;
    pub const CLIP_POP: u8 = 7;
    /// Six `f32` values a,b,c,d,e,f (maps x,y to a*x+c*y+e,b*x+d*y+f).
    pub const TRANSFORM_PUSH: u8 = 8;
    pub const TRANSFORM_POP: u8 = 9;
    /// Unique per-frame image ID (`u32`), width and height (`u16` each), then tightly packed
    /// straight sRGB RGBA. The definition must precede any draw of the image.
    pub const IMAGE_DATA: u8 = 10;
    pub const END: u8 = 11;

    pub const PALETTE_BYTES: usize = 9 * 4;
    pub const MAX_DISPLAY_LIST_BYTES: usize = 4 * 1024 * 1024;
    pub const MAX_COMMANDS: usize = 4096;
    pub const MAX_STACK_DEPTH: usize = 32;
    pub const MAX_PATH_VERBS: usize = 512;
    pub const MAX_FRAME_PATH_VERBS: usize = 65_536;
    pub const MAX_TEXT_BYTES: usize = 1024;
    pub const MAX_IMAGES: usize = 16;
    pub const MAX_COORDINATE: f32 = 65_536.;
    pub const MAX_STROKE_WIDTH: f32 = 1024.;
    pub const MAX_TEXT_SIZE: f32 = 512.;
    /// Time allowed for a render, checked between bounded native drawing operations. It is spent in the extension's own
    /// process, so it bounds that process's frame time, not the application's: a dense full-screen map at 4K needs ~50–60 ms.
    pub const MAX_RENDER_MILLIS: u64 = 120;
}
