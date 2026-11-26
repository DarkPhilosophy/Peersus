//! Pictures an extension shows on its own pages: made in the extension's own process, kept by the application as finished
//! pixels, and placed by id with [`crate::elements::Node::Picture`].
//!
//! WHO DOES WHAT. The extension turns an encoded picture (a photo, a screenshot) into pixels with the decoder its own helper
//! process offers it ([`DECODE`], the guest function [`DECODE_FUNCTION`]); the application never decodes a picture for an
//! extension. The extension then keeps the pixels under an id ([`KEEP`]), read from its own private storage, and its pages place
//! that id. The application checks only the size of what it keeps and draws it as it is.
//!
//! WHAT BOUNDS IT. A kept picture has no side longer than [`MAX_EDGE`] and is exactly `width * height * 4` bytes of
//! unpremultiplied sRGB RGBA. An extension keeps at most [`MAX_KEPT`] pictures and [`MAX_KEPT_BYTES`] of pixels in all; a `keep`
//! past either is refused and changes nothing. What an extension keeps goes with it when it stops, restarts, is turned off or is
//! removed.
use crate::{
    Error, Result,
    elements::{self, layer_id},
    extension_data::Source,
};
use serde::{Deserialize, Serialize};

pub const SERVICE: &str = "peersus.pictures";
/// Keep one picture, or replace the one kept under that id.
pub const KEEP: &str = "keep";
/// Let go of some pictures.
pub const FORGET: &str = "forget";
/// Declares that the extension decodes pictures in its own process. It is never answered through the hub: a WASM guest that
/// imports it runs in the helper that links the decoder, and calls [`DECODE_FUNCTION`].
pub const DECODE: &str = "decode";

/// Longest side of a kept picture, in pixels: enough for a photo opened over a page on a phone, and small enough that the
/// window is sent one in a single reply.
pub const MAX_EDGE: u32 = 640;
/// Most pictures one extension keeps at once.
pub const MAX_KEPT: usize = 64;
/// Most bytes of pixels one extension keeps in all.
pub const MAX_KEPT_BYTES: usize = 8 * 1024 * 1024;

/// The guest function, in the `peersus_v1` module, that decodes a picture in the extension's own helper process:
/// `picture_decode(input, input_len, max_edge, output, output_len) -> i64`. `input` holds an encoded PNG, JPEG, WebP, GIF (its
/// first frame) or BMP of at most [`MAX_DECODE_INPUT`] bytes. The picture is turned upright as its EXIF orientation says and made
/// no larger than `max_edge` (1 to [`MAX_DECODE_EDGE`]) on its longest side, keeping its proportions; it is never enlarged. On
/// success `output` holds `width * height * 4` bytes of unpremultiplied sRGB RGBA and the answer is `width << 16 | height`.
/// A negative answer is one of the `DECODE_*` codes and leaves `output` unspecified.
pub const DECODE_FUNCTION: &str = "picture_decode";
/// Longest encoded picture [`DECODE_FUNCTION`] reads, in bytes.
pub const MAX_DECODE_INPUT: usize = 16 * 1024 * 1024;
/// Largest `max_edge` [`DECODE_FUNCTION`] accepts.
pub const MAX_DECODE_EDGE: u32 = 1024;
/// Most pixels of the picture as it is decoded before it is made smaller (a format that cannot be decoded smaller is decoded
/// whole first). Past it the picture is refused as [`DECODE_TOO_LARGE`].
pub const MAX_DECODE_PIXELS: u64 = 48_000_000;
/// This helper has no decoder: the extension did not import [`DECODE`].
pub const DECODE_UNSUPPORTED: i64 = -1;
/// Not a picture this decoder reads, or a damaged one.
pub const DECODE_INVALID: i64 = -2;
/// The input, the picture or `max_edge` is past its bound.
pub const DECODE_TOO_LARGE: i64 = -3;
/// `output` is shorter than the decoded picture.
pub const DECODE_SHORT_OUTPUT: i64 = -4;

/// The arguments of `peersus.pictures` `keep`: the picture `id` (the grammar of a kept layer's id), its `width` and `height`, and
/// where its pixels are in the caller's private storage: a committed object or a sealed stage holding exactly
/// `width * height * 4` bytes of unpremultiplied sRGB RGBA, read once when it is kept.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keep {
    pub id: String,
    pub width: u16,
    pub height: u16,
    pub source: Source,
}

impl Keep {
    /// Read and check the arguments of a `keep`.
    pub fn parse(value: &serde_json::Value) -> Result<Self> {
        let keep = Self::deserialize(value).map_err(|_| Error::InvalidManifest)?;
        keep.validate()?;
        Ok(keep)
    }

    /// A sound id and both sides from one to [`MAX_EDGE`].
    pub fn validate(&self) -> Result<()> {
        let side = |n: u16| (1..=MAX_EDGE).contains(&u32::from(n));
        if layer_id(&self.id) && side(self.width) && side(self.height) {
            Ok(())
        } else {
            Err(Error::InvalidManifest)
        }
    }

    /// How many bytes the pixels are.
    pub fn bytes(&self) -> usize {
        usize::from(self.width) * usize::from(self.height) * 4
    }
}

/// The arguments of `peersus.pictures` `forget`: the same as a drawing layer's.
pub type Forget = elements::Forget;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_keep_names_a_sound_id_sides_within_the_edge_and_a_storage_source() {
        let keep = Keep::parse(&json!({
            "id": "thumb/1", "width": 640, "height": 1,
            "source": {"kind": "stage", "stage": 7}
        }))
        .unwrap();
        assert_eq!(keep.bytes(), 640 * 4);
        for refused in [
            json!({"id": "Thumb", "width": 1, "height": 1, "source": {"kind": "stage", "stage": 7}}),
            json!({"id": "t", "width": 0, "height": 1, "source": {"kind": "stage", "stage": 7}}),
            json!({"id": "t", "width": 641, "height": 1, "source": {"kind": "stage", "stage": 7}}),
            json!({"id": "t", "width": 1, "height": 1, "source": {"kind": "file", "path": "/etc/passwd"}}),
            json!({"id": "t", "width": 1, "height": 1, "source": {"kind": "stage", "stage": 7}, "rgba": "00"}),
        ] {
            assert!(Keep::parse(&refused).is_err(), "{refused} was kept");
        }
    }
}
