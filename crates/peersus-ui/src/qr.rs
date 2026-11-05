//! The pairing code as a picture another device can scan.
//!
//! One raster image, not a grid of elements: a code for a long invitation has thousands of
//! modules, and a picture is one node. It is always black on white with a quiet zone around it,
//! in both themes, because scanners need that contrast; and it is drawn with whole pixels per
//! module and no smoothing, so no module edge is blurred.
use freya::{elements::image::ImageHandle, engine::prelude::AlphaType, prelude::*};
use qrcodegen::{QrCode, QrCodeEcc};

/// Modules of empty white around the code. The standard asks for at least four.
const QUIET: i32 = 4;
/// Modules of white around the code in the round style. The wide white frame the standard asks for makes a code look like a square
/// set in a circle; one module is enough for the three finder patterns to stay rings of their own, which is what a reader finds first.
const ROUND_QUIET: i32 = 1;
/// Fewest pixels on a side of one module that a camera reads reliably. Below this the disc must
/// be made bigger; the code is never shrunk past it.
const LEAST_SCALE: i32 = 3;
/// Pixels on a side of one module when no size is asked for.
const SCALE: i32 = 4;
/// Diameter of the dark disc in the middle of a screen, in pixels. The one place it is set: the
/// portal is drawn at this size and the code is fitted to it. It is the size at which the real
/// invitation, at the error correction below, still has modules of the least readable size; the
/// portal's inner ring is the same size, so the portal as a whole does not grow.
pub const PORTAL_DISC: f32 = 384.;

/// The diameter, in physical pixels, of the disc as it is really drawn: its share of a portal of `extent` logical
/// units, times `scale` physical pixels to a logical unit. A code is drawn for this disc and not for
/// [`PORTAL_DISC`], which is the size the prototype has on a desktop at a scale of 1: on a phone the disc is
/// physically about twice as large, and a code drawn for the smaller one is half the size it could be.
///
/// A scale that is not a positive number, or an extent that is not, leaves the disc at the prototype's size.
pub fn disc_physical(extent: f32, scale: f64) -> f32 {
    let usable = scale.is_finite() && scale > 0. && extent.is_finite() && extent > 0.;
    if usable {
        (f64::from(extent) * f64::from(PORTAL_DISC) / f64::from(PORTAL_OUTER) * scale) as f32
    } else {
        PORTAL_DISC
    }
}

/// Width and height of the whole portal, rings included, at the size it is drawn on a desktop. The disc is
/// inside it, and everything else about the portal is a share of this: a smaller portal is the same picture
/// made smaller.
pub const PORTAL_OUTER: f32 = 420.;

/// Error correction of the code. Medium recovers about 15% of it being unreadable: a screen shown
/// to a camera has glare and blur, and the lowest level survived under 1% of damage in testing.
/// Higher levels add modules and would need a disc larger than the portal.
const ECC: QrCodeEcc = QrCodeEcc::Medium;
/// How an invitation begins, as the owner writes and reads it.
const INVITATION_PREFIX: &str = "peersus:1:";

/// The invitation in one frame of a camera, or `None` when there is none in it.
///
/// `luma` is the frame's brightness plane, one byte for each pixel, `width` pixels to a row, as a camera hands it
/// over before it is turned into colour: a code is read from brightness alone, so nothing is converted. What is
/// found is returned as the owner reads it, in lower case and with its prefix, and only if it plainly is an
/// invitation: any other code in the frame (a link, a ticket, something on a wall) is passed over, so that a
/// camera pointed at the wrong thing never makes the owner join anything.
///
/// A frame that does not hold `width * height` bytes is refused whole and not read short.
pub fn invitation_in_frame(luma: &[u8], width: usize, height: usize) -> Option<String> {
    if width == 0 || height == 0 || luma.len() != width.checked_mul(height)? {
        return None;
    }
    let mut image =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| luma[y * width + x]);
    image
        .detect_grids()
        .iter()
        .filter_map(|grid| grid.decode().ok())
        .find_map(|(_, text)| invitation_to_join(&text))
        // The code the portal draws has rings for finder patterns, which the reader above takes no notice of: it looks for their corners.
        .or_else(|| {
            crate::qr_reader::read(luma, width, height).and_then(|text| invitation_to_join(&text))
        })
}

/// The corners of a code seen in a frame, in the frame's pixels, in the order the decoder finds them.
pub type Corners = [(usize, usize); 4];

/// What one look at a frame found: the corners of every code the decoder located (whether or not it could be read, since
/// a code that is seen and cannot yet be read is exactly what the person needs to be told about), and the invitation
/// among them, if one could be read.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub corners: Vec<Corners>,
    pub invitation: Option<String>,
}

/// A point as the decoder gives it, which is signed and may lie outside the frame (a code only partly in view has
/// corners past its edge), held to the frame: a negative one is the edge at nothing, one past the far side the last
/// pixel. A mark is then never placed off the picture it is drawn on, and no index is out of range.
pub fn held_to_frame(x: i32, y: i32, width: usize, height: usize) -> (usize, usize) {
    let hold = |value: i32, size: usize| {
        usize::try_from(value.max(0))
            .unwrap_or(0)
            .min(size.saturating_sub(1))
    };
    (hold(x, width), hold(y, height))
}

/// [`invitation_in_frame`], and with it where every code in the frame was found, from the same single pass, so that
/// showing the person what is seen costs nothing beyond reading it. A frame of the wrong length is refused whole.
pub fn look_in_frame(luma: &[u8], width: usize, height: usize) -> Seen {
    if width == 0 || height == 0 || width.checked_mul(height) != Some(luma.len()) {
        return Seen::default();
    }
    let mut image =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| luma[y * width + x]);
    let grids = image.detect_grids();
    let mut corners: Vec<Corners> = grids
        .iter()
        .map(|grid| {
            grid.bounds
                .map(|point| held_to_frame(point.x, point.y, width, height))
        })
        .collect();
    let mut invitation = grids
        .iter()
        .filter_map(|grid| grid.decode().ok())
        .find_map(|(_, text)| invitation_to_join(&text));
    if invitation.is_none() {
        // Nothing was read by the corners of a ring: look again by the middles of the three finder patterns, which a ring has as much
        // as a square. A code located here is shown as located whether or not it could be read.
        if let Some(found) = crate::qr_reader::find(luma, width, height) {
            if corners.is_empty() {
                corners.push(found.corners.map(|point| {
                    held_to_frame(
                        point.x.round() as i32,
                        point.y.round() as i32,
                        width,
                        height,
                    )
                }));
            }
            invitation = found.text.and_then(|text| invitation_to_join(&text));
        }
    }
    Seen {
        corners,
        invitation,
    }
}

/// What the picture is made of: its side in pixels and its RGBA bytes.
#[derive(Debug, PartialEq, Eq)]
pub struct Raster {
    pub side: u32,
    pub rgba: Vec<u8>,
}

/// How a code of some number of modules is drawn inside a disc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fit {
    /// Pixels on a side of one module, a whole number so that no edge is blurred.
    pub scale: u8,
    /// Pixels on a side of the whole picture, quiet zone included.
    pub side: u16,
}

/// The largest code of `modules` modules that lies wholly inside a disc of `diameter` pixels,
/// leaving `reserved` pixels of height for what is written with it.
///
/// The corners of a square leave a circle first, at half its diagonal, so the side may be at
/// most the diameter over the square root of two. Each module is a whole number of pixels, or
/// its edges would be blurred. `None` when that leaves modules too small for a camera: the
/// answer is a bigger disc, not a smaller code.
pub fn fit(modules: i32, diameter: f32, reserved: f32) -> Option<Fit> {
    let room = (diameter / std::f32::consts::SQRT_2 - reserved).max(0.);
    let total = modules + 2 * QUIET;
    let scale = (room / total as f32).floor() as i32;
    if scale < LEAST_SCALE {
        return None;
    }
    Some(Fit {
        scale: scale as u8,
        side: (total * scale) as u16,
    })
}

/// The smallest disc, in whole pixels, that holds a code of `modules` modules at the least scale a camera
/// reads reliably: the inverse of [`fit`]. A portal drawn smaller than this has no room for its code.
pub fn least_disc(modules: i32) -> f32 {
    let total = (modules + 2 * QUIET) as f32;
    let mut disc = (LEAST_SCALE as f32 * total * std::f32::consts::SQRT_2).ceil();
    // The formula is the inverse on paper; on floats the last pixel is checked against `fit` itself.
    while fit(modules, disc, 0.).is_none() {
        disc += 1.;
    }
    disc
}

/// Modules, across, of a finder pattern: the three squares in the corners of every code.
const FINDER: i32 = 7;
/// How round the rings of a finder pattern are, from 0 (square) to 1 (a full circle): the corners of each of its three
/// layers are rounded by this fraction of the layer's half-width.
///
/// A FULL CIRCLE. The first reader of a camera's frames (`rqrr`) finds a finder pattern by the four extreme points of its
/// outer ring and finds none on a round one (measured: a full circle was found on no disc at all), so the frames are read a
/// second time by [`crate::qr_reader`], which finds a finder by the proportions 1:1:3:1:1 of a line through its middle, and a
/// ring has those as much as a square has. The drawing is tested to be read back by that path alone (see the tests).
const FINDER_ROUNDING: f32 = 1.0;

/// How far outside a rounded square of half-side `half` and corner radius `radius` a point lies,
/// measured from its middle: negative or zero inside.
fn rounded_square(dx: f32, dy: f32, half: f32, radius: f32) -> f32 {
    let (qx, qy) = (dx.abs() - (half - radius), dy.abs() - (half - radius));
    qx.max(0.).hypot(qy.max(0.)) + qx.max(qy).min(0.) - radius
}

/// Whether the pixel `(x, y)` of a code drawn in the ROUND style is dark. The matrix and its
/// places are those of the standard, so a reader finds what it expects; only the ink differs:
/// a data module is a whole square, and a finder pattern (a dark ring, a light ring and a dark
/// middle) keeps its proportions as rings, see [`FINDER_ROUNDING`].
fn round_dark(code: &QrCode, fx: f32, fy: f32) -> bool {
    let size = code.size();
    let (mx, my) = (fx.floor() as i32, fy.floor() as i32);
    for (ox, oy) in [(0, 0), (size - FINDER, 0), (0, size - FINDER)] {
        if (ox..ox + FINDER).contains(&mx) && (oy..oy + FINDER).contains(&my) {
            let (dx, dy) = (fx - ox as f32 - 3.5, fy - oy as f32 - 3.5);
            let outer = rounded_square(dx, dy, 3.5, 3.5 * FINDER_ROUNDING) <= 0.;
            let gap = rounded_square(dx, dy, 2.5, 2.5 * FINDER_ROUNDING) <= 0.;
            let middle = rounded_square(dx, dy, 1.5, 1.5 * FINDER_ROUNDING) <= 0.;
            return (outer && !gap) || middle;
        }
    }
    code.get_module(mx, my)
}

/// The code for `text` at `scale` pixels a module, or `None` if it is too long for any code.
fn raster_at(text: &str, scale: i32) -> Option<Raster> {
    // Medium error correction (see `ECC`): a screen shown to a camera has glare and blur, so the
    // code carries enough redundancy to be read through some of it.
    let code = QrCode::encode_text(text, ECC).ok()?;
    let side = (code.size() + 2 * QUIET) * scale;
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let dark = code.get_module(x / scale - QUIET, y / scale - QUIET);
            let value = if dark { 0x00 } else { 0xff };
            rgba.extend_from_slice(&[value, value, value, 0xff]);
        }
    }
    Some(Raster {
        side: side as u32,
        rgba,
    })
}

/// Sub-samples across and down a pixel when the round style is drawn: a dot of a few pixels is
/// then a dark middle with a grey edge, which is what makes it look round at any size.
const SAMPLES: i32 = 4;

/// The grey of pixel `(x, y)` when `ink(fx, fy)` says, for a point `(fx, fy)` in units of a
/// module, whether it is ink: white where none of the pixel is ink, black where all of it is.
fn coverage(x: i32, y: i32, scale: i32, ink: impl Fn(f32, f32) -> bool) -> u8 {
    let mut covered = 0;
    for sy in 0..SAMPLES {
        for sx in 0..SAMPLES {
            // At the middle of the sub-sample, in module units from the pixel grid's origin.
            let fx = (x as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / scale as f32;
            let fy = (y as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / scale as f32;
            covered += i32::from(ink(fx, fy));
        }
    }
    (255 - covered * 255 / (SAMPLES * SAMPLES)) as u8
}

/// The same code drawn in the ROUND style (see `round_dark`), at `scale` pixels a module. Every
/// pixel is the share of it that is ink, so edges are smooth and the centre of a dot stays black.
fn round_style_at(text: &str, scale: i32, quiet: i32) -> Option<Raster> {
    let code = QrCode::encode_text(text, ECC).ok()?;
    let side = (code.size() + 2 * quiet) * scale;
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let value = coverage(x, y, scale, |fx, fy| {
                round_dark(&code, fx - quiet as f32, fy - quiet as f32)
            });
            rgba.extend_from_slice(&[value, value, value, 0xff]);
        }
    }
    Some(Raster {
        side: side as u32,
        rgba,
    })
}

/// The code for `text` at the default scale.
pub fn raster(text: &str) -> Option<Raster> {
    raster_at(text, SCALE)
}

/// Draw real invitations as bounded binary/Base32, keeping copied/API text unchanged.
/// Legacy-shaped non-JSON text still uses the older rendering, including its regression fixtures.
pub fn for_code(text: &str) -> String {
    peersus_net::invitation::Invitation::compact_for_qr(text)
        .unwrap_or_else(|_| long_form_for_code(text))
}

/// The long form of an invitation, as the owner writes and reads it (`peersus:1:` and hex), in capitals for a QR. The form the compact one is
/// expanded back to when scanned, and the fallback when a text cannot be compressed: not an older version's, but the invitation's own.
pub fn long_form_for_code(text: &str) -> String {
    match text.strip_prefix(INVITATION_PREFIX) {
        Some(body) if body.bytes().all(|b| b.is_ascii_hexdigit()) => {
            format!(
                "{}{}",
                INVITATION_PREFIX.to_ascii_uppercase(),
                body.to_ascii_uppercase()
            )
        }
        _ => text.to_owned(),
    }
}

/// Restore both the compact and the long QR forms to the owner's unchanged text format.
pub fn from_code(scanned: &str) -> String {
    if let Ok(long_form) = peersus_net::invitation::Invitation::expand_qr(scanned) {
        return long_form;
    }
    let prefix = INVITATION_PREFIX.to_ascii_uppercase();
    match scanned.strip_prefix(prefix.as_str()) {
        Some(body) if body.bytes().all(|b| b.is_ascii_hexdigit()) => {
            format!("{INVITATION_PREFIX}{}", body.to_ascii_lowercase())
        }
        _ => scanned.to_owned(),
    }
}

/// What to hand the owner to join, from whatever was pasted or scanned: the invitation in the
/// form the owner reads, or `None` if it is not one. Anything else a clipboard or a camera can
/// offer (a web address, someone else's code, nothing) is never sent on: the owner is asked to
/// join only what is plainly an invitation.
pub fn invitation_to_join(text: &str) -> Option<String> {
    let normal = from_code(text.trim());
    let body = normal.strip_prefix(INVITATION_PREFIX)?;
    (!body.is_empty() && body.bytes().all(|b| b.is_ascii_hexdigit())).then_some(normal)
}

/// Modules, across, that the logo takes in the middle of the code. Measured, on codes of 61 to 81
/// modules with 1% of the data also damaged: with nothing blanked 19 of 20 were read; a square of
/// 5 modules cost about one more, 7 about two, 9 four or more. Seven is the largest whose cost is
/// small, and at three pixels a module it is a picture of 21 by 21 pixels.
const LOGO_MODULES: i32 = 7;

/// Where the logo goes in a code of `modules` modules: the first module and the width in modules
/// of a square in the very middle. Centred, so the same room is left on every side, and far from
/// the three corners that hold the finder patterns and from the timing lines.
pub fn logo_area(modules: i32) -> (i32, i32) {
    // Odd, so that it has a middle module of its own, and never wider than the code allows.
    let size = LOGO_MODULES.min(modules / 12) | 1;
    ((modules - size) / 2, size)
}

/// Blank the middle of a drawn code to the light background, where the logo is then drawn.
fn clear_logo_area(raster: &mut Raster, modules: i32, scale: i32, quiet: i32) {
    let (from, size) = logo_area(modules);
    let side = raster.side as i32;
    let (start, end) = ((from + quiet) * scale, (from + size + quiet) * scale);
    for y in start..end.min(side) {
        for x in start..end.min(side) {
            let at = ((y * side + x) * 4) as usize;
            raster.rgba[at..at + 3].fill(0xff);
        }
    }
}

/// The code for `text` at `scale` pixels a module with the application's icon in the middle: the
/// middle is blanked to white and the icon, brought to exactly that many pixels, is laid over it.
/// The icon has see-through corners, so it is composed over the white, not left to show the code.
#[cfg(test)]
fn raster_with_logo(text: &str, scale: i32) -> Option<Raster> {
    with_logo(text, scale, QUIET, raster_at(text, scale)?)
}

/// `raster`, a drawing of `text` at `scale` with `quiet` modules of light around the code, with
/// the icon laid over its middle.
fn with_logo(text: &str, scale: i32, quiet: i32, mut raster: Raster) -> Option<Raster> {
    let modules = QrCode::encode_text(text, ECC).ok()?.size();
    clear_logo_area(&mut raster, modules, scale, quiet);
    let (from, size) = logo_area(modules);
    let pixels = (size * scale) as u32;
    let icon = crate::icon::scaled(pixels)?;
    let side = raster.side as usize;
    let origin = ((from + quiet) * scale) as usize;
    for row in 0..pixels as usize {
        for column in 0..pixels as usize {
            let source = (row * pixels as usize + column) * 4;
            let target = ((origin + row) * side + origin + column) * 4;
            let alpha = u32::from(icon[source + 3]);
            for channel in 0..3 {
                // Over white: what the icon covers, plus what it lets through.
                let over = u32::from(icon[source + channel]) * alpha + 255 * (255 - alpha);
                raster.rgba[target + channel] = (over / 255) as u8;
            }
        }
    }
    Some(raster)
}

/// The code for `text` drawn on a LIGHT DISC instead of a white square, with the application's
/// icon in the middle, at `scale` pixels a module.
///
/// The matrix of a code is square and stays so: a camera finds the three finder patterns in three
/// of its corners, and a code clipped to a circle would lose them. What is round is the field the
/// code is drawn on. The disc is made big enough to hold the whole square (its corners are at
/// half the diagonal), and what is outside the disc is see-through, so that there is no white
/// block left on the screen and the code sits on the portal's own disc.
fn round_raster_with_logo(text: &str, scale: i32, diameter: f32) -> Option<Raster> {
    let square = with_logo(
        text,
        scale,
        ROUND_QUIET,
        round_style_at(text, scale, ROUND_QUIET)?,
    )?;
    round_plain(text, square, scale, diameter)
}

/// Whether the module `(gx, gy)` of the disc's grid, counted from the matrix's own top left module and lying OUTSIDE the matrix, is dark.
///
/// It is the code's own DATA repeated as a mosaic: the data area (the matrix without its three finders and their light separators) is
/// tiled over the plane, so the field round the code has the density and the look of the code itself and the edge of the matrix cannot be
/// told from it. A noise of half dark modules was tried and showed the square plainly, since a code is not half dark.
///
/// One module of light is left round each finder, outside the matrix: a finder is told from the data by the light that stands round its
/// outer ring, and a ring that touches dark modules on its outer side has the proportions 1:1:3:1:1 on no line through it.
fn decor_dark(code: &QrCode, gx: i32, gy: i32) -> bool {
    let size = code.size();
    for (ox, oy) in [(0, 0), (size - FINDER, 0), (0, size - FINDER)] {
        if (ox - 1..ox + FINDER + 1).contains(&gx) && (oy - 1..oy + FINDER + 1).contains(&gy) {
            return false;
        }
    }
    let span = size - 2 * (FINDER + 1);
    let (mx, my) = (
        FINDER + 1 + gx.rem_euclid(span),
        FINDER + 1 + gy.rem_euclid(span),
    );
    code.get_module(mx, my)
}

/// The complete QR square on a disc that is FILLED with modules: the matrix keeps its place in the middle (and with it the place a camera
/// expects its three finder patterns), and every module of the disc's grid beyond it is the code's own data repeated, in the same size and
/// the same ink, up to the edge of the circle. What lies outside the circle is see-through.
///
/// The repetition says nothing new and is not read: a reader finds the matrix by its three finders and samples only inside them.
fn round_plain(text: &str, square: Raster, scale: i32, diameter: f32) -> Option<Raster> {
    let code = QrCode::encode_text(text, ECC).ok()?;
    let inner = square.side as i32;
    // The disc that holds the square: its radius is half the diagonal, rounded up, and a whole number of modules so the grid is whole.
    let holds_the_square =
        (((inner as f32) * std::f32::consts::SQRT_2 / 2.).ceil() as i32 / scale + 1) * scale;
    // The disc is as large as the one asked for, in whole modules: the field round the matrix is only the code's own data repeated, so a larger
    // field costs the reading nothing and fills the portal's disc instead of leaving a ring of nothing between it and the edge. Never smaller than
    // what holds the square, and never past what was asked for (a code that needs more than the disc has asked for the disc it needs, see `portal_raster`).
    let asked = if diameter.is_finite() && diameter > 0. {
        (diameter / 2.).floor() as i32 / scale * scale
    } else {
        0
    };
    let radius = holds_the_square.max(asked);
    let side = 2 * radius;
    let offset = radius - inner / 2;
    // `square` carries `ROUND_QUIET` modules of light on each side of the matrix, so the matrix's own corner is that far in.
    let origin = ROUND_QUIET * scale;
    let mut rgba = vec![0u8; (side * side * 4) as usize];
    for y in 0..side {
        for x in 0..side {
            let (dx, dy) = (
                x as f32 + 0.5 - radius as f32,
                y as f32 + 0.5 - radius as f32,
            );
            if dx.hypot(dy) > radius as f32 {
                continue; // outside the disc: see-through
            }
            let at = ((y * side + x) * 4) as usize;
            let (sx, sy) = (x - offset, y - offset);
            let value = if (origin..inner - origin).contains(&sx)
                && (origin..inner - origin).contains(&sy)
            {
                square.rgba[((sy * inner + sx) * 4) as usize]
            } else {
                // The module of the disc's grid this pixel is in, counted from the matrix's own corner so the repetition is on the same grid.
                let (gx, gy) = (
                    (x - offset - origin).div_euclid(scale),
                    (y - offset - origin).div_euclid(scale),
                );
                if decor_dark(&code, gx, gy) {
                    0x00
                } else {
                    0xff
                }
            };
            rgba[at..at + 4].copy_from_slice(&[value, value, value, 0xff]);
        }
    }
    Some(Raster {
        side: side as u32,
        rgba,
    })
}

/// The round drawing of `text` for a disc of `diameter` pixels with `reserved` pixels of height
/// left for a caption: the costly part of showing a code, which depends on nothing but its
/// arguments and so can be done once for a code and reused. `None` only when it cannot be made.
///
/// `diameter` is a requested size rather than a ceiling: a larger invitation must
/// enlarge the drawing instead of shrinking modules below a camera-readable size.
pub fn portal_raster(text: &str, diameter: f32, reserved: f32) -> Option<Raster> {
    let modules = QrCode::encode_text(text, ECC).ok()?.size();
    let diameter = diameter.max(least_disc(modules) + reserved * std::f32::consts::SQRT_2);
    let fit = fit(modules, diameter, reserved)?;
    let scale = i32::from(fit.scale);
    // The field of the drawing grows to the disc (less the room a caption keeps), so that no ring of nothing is left between the code and the edge.
    let field = diameter - reserved * std::f32::consts::SQRT_2;
    // A code without its logo is better than no code: if the icon cannot be put in, draw it plain.
    round_raster_with_logo(text, scale, field).or_else(|| {
        round_style_at(text, scale, ROUND_QUIET)
            .and_then(|square| round_plain(text, square, scale, field))
    })
}

/// The decoded picture for a drawing from [`portal_raster`]. Made once and kept: it is shared by
/// reference, so the page can hand out as many cheap copies as it draws, and a copy is the same
/// image to the toolkit, which then does not repaint it.
pub fn portal_handle(raster: &Raster) -> Option<ImageHandle> {
    // Unpremultiplied: the corners outside the disc are see-through and must stay so.
    ImageHandle::from_rgba(
        raster.side,
        raster.side,
        Bytes::from(raster.rgba.clone()),
        AlphaType::Unpremul,
    )
}

/// The width and height, in the toolkit's logical units, at which a picture of `pixels` pixels on a side is shown
/// on a screen of `scale` physical pixels to a logical unit, so that it is `pixels` physical pixels on the glass.
///
/// A picture drawn for a number of physical pixels and then shown as that number of logical units is `scale`
/// times too large: on a phone at 3.1 it was three times wider than the screen. A scale that is not a positive
/// number cannot divide anything, and then the picture is shown 1 to 1, as it was, and not made to vanish.
pub fn logical_side(pixels: f32, scale: f64) -> f32 {
    if scale.is_finite() && scale > 0. {
        (f64::from(pixels) / scale) as f32
    } else {
        pixels
    }
}

/// The picture node for a handle from [`portal_handle`], shown at the size on the glass it was drawn for. Cheap,
/// so it may be made on every draw.
pub fn picture_from(handle: &ImageHandle, scale: f64) -> Element {
    let side = logical_side(handle.image.width() as f32, scale);
    image(handle.clone())
        .sampling_mode(SamplingMode::Nearest)
        .width(Size::px(side))
        .height(Size::px(side))
        .into()
}

/// The code for `text` as an element that lies wholly inside a disc of `diameter` pixels, with
/// `reserved` pixels of height left for a caption, shown at `scale` physical pixels to a logical unit.
pub fn picture(text: &str, diameter: f32, reserved: f32, scale: f64) -> Option<Element> {
    Some(picture_from(
        &portal_handle(&portal_raster(text, diameter, reserved)?)?,
        scale,
    ))
}

/// The drawing of a code that was made last, with what it was made for. Asked for a code and a disc, it gives
/// the drawing it has if it was made for exactly those and makes a new one otherwise.
///
/// This is the answer to a drawing that kept its first size: a memo that captures a size by value never sees it
/// change, so a window that was resized or turned went on showing a code drawn for the old one. What decides here
/// is the pair (code, disc), compared, and nothing the toolkit has to track.
#[derive(Default)]
pub struct Drawn {
    made_for: Option<(String, u32)>,
    handle: Option<ImageHandle>,
    made: u32,
}

impl Drawn {
    /// The drawing of `code` for a disc of `diameter` physical pixels, from the last one if it was made for the
    /// same code and the same disc. A disc is compared by its whole pixels: a fraction of one is no change.
    pub fn of(&mut self, code: &str, diameter: f32) -> Option<&ImageHandle> {
        let disc = diameter.max(0.).round() as u32;
        if self
            .made_for
            .as_ref()
            .map(|(text, size)| (text.as_str(), *size))
            != Some((code, disc))
        {
            self.handle = portal_raster(&for_code(code), diameter, 0.)
                .and_then(|raster| portal_handle(&raster));
            self.made = self.made.saturating_add(1);
            self.made_for = Some((code.to_owned(), disc));
        }
        self.handle.as_ref()
    }

    /// How many drawings were really made, as opposed to given again: what tells keeping a drawing from making
    /// the same one over and over, which a picture alone cannot.
    pub fn drawings_made(&self) -> u32 {
        self.made
    }

    /// Whether the drawing it holds was made for this code and this disc: what a test can ask without a window.
    pub fn is_for(&self, code: &str, diameter: f32) -> bool {
        self.made_for
            .as_ref()
            .map(|(text, size)| (text.as_str(), *size))
            == Some((code, diameter.max(0.).round() as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(raster: &Raster, x: u32, y: u32) -> u8 {
        raster.rgba[((y * raster.side + x) * 4) as usize]
    }

    #[test]
    fn the_picture_is_square_and_has_four_bytes_per_pixel() {
        let raster = raster("hello").unwrap();
        assert_eq!(raster.rgba.len() as u32, raster.side * raster.side * 4);
    }

    /// Where the first dark pixel is, scanning rows then columns: `(x, y)` of the top left of the
    /// finder pattern, which is the outermost dark thing in the picture.
    fn first_dark(raster: &Raster) -> (u32, u32) {
        for y in 0..raster.side {
            for x in 0..raster.side {
                if pixel(raster, x, y) == 0x00 {
                    return (x, y);
                }
            }
        }
        panic!("a code has dark modules");
    }

    #[test]
    fn there_are_at_least_four_modules_of_white_before_the_code_on_every_side() {
        // Stated as a number, not derived from the constants: the standard asks for four
        // modules, and a module is at least four pixels wide for a camera to read it.
        let raster = raster("hello").unwrap();
        let (x, y) = first_dark(&raster);
        assert!(x >= 16 && y >= 16, "dark starts at ({x}, {y})");
        // The code is square and centred, so the same margin is left on the far sides.
        let last = raster.side - 1;
        for i in 0..raster.side {
            for edge in 0..16 {
                assert_eq!(pixel(&raster, i, edge), 0xff, "top row {edge}");
                assert_eq!(pixel(&raster, edge, i), 0xff, "left column {edge}");
                assert_eq!(pixel(&raster, i, last - edge), 0xff, "bottom row {edge}");
                assert_eq!(pixel(&raster, last - edge, i), 0xff, "right column {edge}");
            }
        }
    }

    #[test]
    fn the_finder_pattern_is_dark_at_its_corner() {
        // The top left module of every code is dark: the corner of a finder pattern.
        let raster = raster("hello").unwrap();
        let (x, y) = first_dark(&raster);
        assert_eq!(
            (pixel(&raster, x, y), pixel(&raster, x + 3, y + 3)),
            (0x00, 0x00)
        );
    }

    #[test]
    fn a_module_is_scale_pixels_wide_with_no_blur() {
        let raster = raster("hello").unwrap();
        let first = (QUIET * SCALE) as u32;
        for dx in 0..SCALE as u32 {
            for dy in 0..SCALE as u32 {
                assert_eq!(pixel(&raster, first + dx, first + dy), 0x00);
            }
        }
        // Only pure black and pure white ever appear.
        assert!(
            raster
                .rgba
                .chunks(4)
                .all(|p| (p[0] == 0 || p[0] == 255) && p[3] == 255)
        );
    }

    /// Read a raster back the way a camera would: as a grey picture, finding and decoding grids.
    fn scan(raster: &Raster) -> Vec<String> {
        let side = raster.side as usize;
        let mut image = rqrr::PreparedImage::prepare_from_greyscale(side, side, |x, y| {
            raster.rgba[(y * side + x) * 4]
        });
        image
            .detect_grids()
            .iter()
            .filter_map(|grid| grid.decode().ok().map(|(_, text)| text))
            .collect()
    }

    #[test]
    fn a_real_sized_invitation_is_scanned_back_to_exactly_the_same_text() {
        // The shape of a real invitation: a prefix and a few hundred hexadecimal characters.
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        assert!(invitation.len() > 500);
        let raster = raster(&invitation).unwrap();
        assert_eq!(scan(&raster), vec![invitation]);
    }

    #[test]
    fn a_scanner_finds_nothing_in_a_picture_with_no_code() {
        let blank = Raster {
            side: 200,
            rgba: vec![0xff; 200 * 200 * 4],
        };
        assert!(scan(&blank).is_empty());
    }

    #[test]
    fn text_too_long_for_any_code_is_refused_not_truncated() {
        assert!(raster(&"x".repeat(8000)).is_none());
    }

    #[test]
    fn a_real_sized_invitation_still_fits() {
        // An invitation is a few hundred characters of hexadecimal.
        let invitation = format!("peersus:1:{}", "ab".repeat(276));
        assert!(raster(&invitation).is_some());
    }

    #[test]
    fn a_code_fitted_to_a_disc_lies_wholly_inside_it() {
        // The corners of the square are what leave a circle first: they are at half the diagonal.
        for disc in [300., 330., 360., 420.] {
            for modules in [21, 45, 81, 117] {
                let Some(fit) = fit(modules, disc, 0.) else {
                    continue;
                };
                let half_diagonal = f32::from(fit.side) * std::f32::consts::SQRT_2 / 2.;
                assert!(
                    half_diagonal <= disc / 2.,
                    "{modules} modules in {disc}: corner at {half_diagonal} of {}",
                    disc / 2.
                );
            }
        }
    }

    /// A dense fixture: the longest form an invitation takes, to exercise fitting the largest codes too.
    fn long_form_modules() -> i32 {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        QrCode::encode_text(&for_code(&invitation), ECC)
            .unwrap()
            .size()
    }

    #[test]
    fn a_fitted_code_is_whole_pixels_per_module_so_no_edge_is_blurred() {
        let modules = long_form_modules();
        let fit = fit(modules, PORTAL_DISC, 0.).unwrap();
        assert_eq!(
            i32::from(fit.side),
            (modules + 2 * 4) * i32::from(fit.scale)
        );
        assert!(fit.scale >= 3);
    }

    #[test]
    fn room_taken_by_a_caption_makes_the_code_smaller_never_bigger() {
        let modules = long_form_modules();
        let bare = fit(modules, PORTAL_DISC, 0.).unwrap();
        // A caption that takes room may leave too little for a readable code (None), which is
        // the honest answer; when there is still a code, it is never bigger.
        if let Some(captioned) = fit(modules, PORTAL_DISC, 10.) {
            assert!(captioned.side <= bare.side);
        }
        assert!(
            fit(modules, PORTAL_DISC, 60.).is_none(),
            "a big caption must not squeeze the code"
        );
    }

    #[test]
    fn a_disc_too_small_for_a_scannable_code_says_so_instead_of_shrinking_it() {
        // Below three pixels a module is not read reliably by a camera: the caller must make the
        // disc bigger, not the code smaller.
        assert_eq!(fit(117, 200., 0.), None);
    }

    #[test]
    fn the_real_invitation_fits_the_real_portal_with_modules_a_camera_can_read() {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let drawn = for_code(&invitation);
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        // With the caption outside the disc: the whole disc is the code's.
        let fit = fit(modules, PORTAL_DISC, 0.).expect("the real code must fit the real portal");
        assert!(fit.scale >= 3, "modules of {} px", fit.scale);
    }

    #[test]
    fn the_least_disc_is_exactly_the_edge_of_what_a_code_can_be_fitted_into() {
        for modules in [21, 45, 73, 77, 85, 117] {
            let least = least_disc(modules);
            let at = fit(modules, least, 0.).unwrap_or_else(|| {
                panic!("{modules} modules do not fit the disc said to hold them")
            });
            assert!(
                at.scale >= 3,
                "the code at {modules} modules is under the least scale on its least disc"
            );
            // One pixel less and there is no room: it is the least, not just enough.
            assert_eq!(
                fit(modules, least - 1., 0.),
                None,
                "a disc a pixel smaller still held {modules} modules"
            );
        }
    }

    #[test]
    fn the_real_invitation_has_no_room_in_a_portal_narrower_than_its_least_disc() {
        let modules = long_form_modules();
        let least = least_disc(modules);
        // The disc the portal draws is enough, and a smaller one would have no code at all: so the portal
        // is never made smaller than this, whatever the width, and the rings that are only for show are
        // what a narrow screen may cut.
        assert!(fit(modules, PORTAL_DISC, 0.).is_some());
        assert!(
            least <= PORTAL_DISC,
            "the portal's own disc is smaller than the code needs"
        );
        assert!(fit(modules, least - 1., 0.).is_none());
    }

    #[test]
    fn real_invitations_are_compact_and_camera_readable_without_changing_copied_text() {
        use iroh::{EndpointAddr, SecretKey};
        use peersus_net::invitation::Invitation;
        let address = EndpointAddr::new(SecretKey::from([4; 32]).public())
            .with_ip_addr("192.0.2.12:4567".parse().unwrap())
            .with_ip_addr("[2001:db8::12]:4568".parse().unwrap())
            .with_relay_url("https://relay.example.org/".parse().unwrap());
        let invitation = Invitation {
            address,
            capability: [7; 32],
            expires_at: 400,
        };
        let long_form = invitation.encode().unwrap();
        let drawn = for_code(&long_form);
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        let old_modules = QrCode::encode_text(&long_form.to_ascii_uppercase(), ECC)
            .unwrap()
            .size();
        assert!(
            modules * modules * 2 < old_modules * old_modules,
            "compact QR has {modules} modules per side; the long form has {old_modules}"
        );
        let fit = fit(modules, PORTAL_DISC, 0.).expect("compact code fits the portal");
        assert!(i32::from(fit.scale) >= LEAST_SCALE);
        let (luma, side) = luma_of(&portal_raster(&drawn, PORTAL_DISC, 0.).unwrap());
        assert!(invitation_in_frame(&luma, side, side).as_deref() == Some(long_form.as_str()));
    }

    /// An invitation as long as the one measured on a phone with several addresses: 888 characters.
    fn phone_invitation() -> String {
        let prefix = "peersus:1:";
        format!("{prefix}{}", "7b2261646472657373".repeat(100))[..888].to_owned()
    }

    #[test]
    fn an_invitation_as_long_as_a_phone_makes_is_drawn_and_not_shown_as_nothing() {
        let text = for_code(&phone_invitation());
        assert_eq!(phone_invitation().len(), 888);
        let modules = QrCode::encode_text(&text, ECC).unwrap().size();
        // Measured: this is what did not fit the 384 the page asks for, and left the circle on "creating".
        assert!(
            fit(modules, PORTAL_DISC, 0.).is_none(),
            "{modules} modules fit {PORTAL_DISC} after all: the premise is gone"
        );
        let raster =
            portal_raster(&text, PORTAL_DISC, 0.).expect("a long invitation was not drawn at all");
        // Drawn at the least size a camera reads, and not squeezed below it.
        let side = raster.side as i32;
        assert!(
            side / (modules + 2 * ROUND_QUIET) >= LEAST_SCALE,
            "a module is under {LEAST_SCALE} px: {side} px for {modules} modules"
        );
        // It is a drawing of the disc it needed, which is larger than the one asked for and is what `side` says.
        assert!(
            raster.side as f32 > PORTAL_DISC,
            "{} px is not larger than the disc asked for",
            raster.side
        );
    }

    #[test]
    fn the_disc_asked_for_is_still_used_when_the_code_fits_it() {
        // A short code is drawn for the disc asked for, as before: only a code that cannot fit it asks for more.
        let short = for_code("peersus:1:7b22");
        let raster = portal_raster(&short, PORTAL_DISC, 0.).expect("a short code");
        assert!(
            raster.side as f32 <= PORTAL_DISC,
            "{} px: a short code was drawn larger than the disc asked for",
            raster.side
        );
    }

    /// The brightness plane of a drawn code, one byte per pixel, as a camera would hand it over.
    fn luma_of(raster: &Raster) -> (Vec<u8>, usize) {
        let side = raster.side as usize;
        (
            (0..side * side)
                .map(|index| raster.rgba[index * 4])
                .collect(),
            side,
        )
    }

    #[test]
    fn a_frame_with_the_real_invitation_in_it_gives_back_exactly_what_the_owner_wrote() {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let (luma, side) = luma_of(&raster(&for_code(&invitation)).unwrap());
        assert_eq!(
            invitation_in_frame(&luma, side, side).as_deref(),
            Some(invitation.as_str())
        );
    }

    #[test]
    fn the_disc_is_the_size_it_is_really_drawn_at_and_not_the_desktops_at_a_scale_of_one() {
        // The Asus, measured: a portal of 346 logical units on a screen of scale 3.125.
        let disc = disc_physical(346., 3.125);
        assert!((disc - 346. * 384. / 420. * 3.125).abs() < 0.5, "{disc} px");
        // On a desktop at a scale of 1 and its own size the disc is the prototype's, unchanged.
        assert_eq!(disc_physical(PORTAL_OUTER, 1.0), PORTAL_DISC);
        // Twice the screen's density is twice the pixels for the same portal.
        assert!((disc_physical(346., 2.0) - 2. * disc_physical(346., 1.0)).abs() < 0.5);
        // Unusable inputs leave the disc at the prototype's size, never nothing and never NaN.
        for bad in [0., -1., f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(disc_physical(346., bad), PORTAL_DISC, "scale {bad}");
        }
        for bad in [0., -1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(disc_physical(bad, 3.125), PORTAL_DISC, "extent {bad}");
        }
    }

    #[test]
    fn the_real_invitation_drawn_for_the_real_disc_fills_it_where_the_old_size_left_it_half_empty()
    {
        let text = for_code(&phone_invitation());
        let disc = disc_physical(346., 3.125);
        let for_the_real_disc = portal_raster(&text, disc, 0.).expect("drawn for the real disc");
        let for_the_prototype =
            portal_raster(&text, PORTAL_DISC, 0.).expect("drawn for the prototype's disc");
        // The measured result on the phone was 402 px, a module scale of 4. Drawn for the disc it really is, it is
        // larger: more pixels to a module is what makes it readable from further away.
        assert!(
            for_the_real_disc.side > for_the_prototype.side,
            "{} px for the real disc is not larger than {} px",
            for_the_real_disc.side,
            for_the_prototype.side
        );
        // And it still lies inside the disc it was drawn for.
        assert!(
            for_the_real_disc.side as f32 <= disc,
            "the code is wider than the disc it was drawn for"
        );
    }

    #[test]
    fn a_drawing_is_kept_for_the_same_code_and_disc_and_made_again_when_either_changes() {
        let code = phone_invitation();
        let mut drawn = Drawn::default();
        assert!(
            !drawn.is_for(&code, 989.),
            "a drawing was said to exist before anything was drawn"
        );
        assert_eq!(drawn.drawings_made(), 0);
        assert!(drawn.of(&code, 989.).is_some());
        assert_eq!(drawn.drawings_made(), 1);
        // Same code, same disc: the drawing already made is given again. Counted, because a drawing made again is
        // identical to the first and could not be told apart by what it looks like.
        assert!(drawn.of(&code, 989.).is_some());
        assert!(drawn.of(&code, 989.).is_some());
        assert_eq!(
            drawn.drawings_made(),
            1,
            "the drawing was made again for the same code and disc"
        );
        // A disc that is a fraction of a pixel different is no change: no redraw for a rounding of the scale.
        assert!(drawn.is_for(&code, 989.4));
        assert!(drawn.of(&code, 989.4).is_some());
        assert_eq!(
            drawn.drawings_made(),
            1,
            "a fraction of a pixel made it draw again"
        );
        // The window was turned: a different disc is a different drawing, and it is the one held now.
        assert!(drawn.of(&code, 700.).is_some());
        assert_eq!(drawn.drawings_made(), 2);
        assert!(drawn.is_for(&code, 700.));
        assert!(
            !drawn.is_for(&code, 989.),
            "the drawing for the old disc was kept after the disc changed"
        );
        // Another code is another drawing.
        let other = format!("{code}ab");
        assert!(drawn.of(&other, 700.).is_some());
        assert_eq!(drawn.drawings_made(), 3);
        assert!(drawn.is_for(&other, 700.));
        assert!(!drawn.is_for(&code, 700.));
    }

    #[test]
    fn the_code_that_is_drawn_and_the_older_long_form_both_read_back_to_the_same_invitation() {
        let code = peersus_net::invitation::Invitation::create(
            iroh::EndpointAddr::new(iroh::SecretKey::from([9; 32]).public()),
            100,
        )
        .unwrap()
        .encode()
        .unwrap();
        // What is drawn is the compact form, and a scan of it joins exactly the invitation that was made.
        assert!(Drawn::default().of(&code, 700.).is_some());
        assert!(invitation_to_join(&for_code(&code)).as_deref() == Some(code.as_str()));
        // A code in the long form still joins too: scanning does not depend on which form was on a screen.
        assert!(invitation_to_join(&long_form_for_code(&code)).as_deref() == Some(code.as_str()));
    }

    #[test]
    fn a_code_that_cannot_be_drawn_is_not_asked_again_on_every_draw() {
        // A text longer than the largest code can hold: encoding itself refuses, so there is nothing to draw. The
        // disc cannot help, since a code that needs a larger one is given it. Drawing is deterministic, so asking
        // again for the same code and disc cannot succeed where it failed; the refusal is remembered and a page does
        // not repeat the work each time it is drawn.
        let too_long = "7".repeat(8000);
        let mut drawn = Drawn::default();
        assert!(
            drawn.of(&too_long, 989.).is_none(),
            "a text no code can hold was drawn"
        );
        assert!(
            drawn.is_for(&too_long, 989.),
            "the failure of a drawing was not remembered"
        );
        assert_eq!(drawn.drawings_made(), 1);
        assert!(drawn.of(&too_long, 989.).is_none());
        assert_eq!(
            drawn.drawings_made(),
            1,
            "a drawing that failed was tried again for the same code and disc"
        );
    }

    #[test]
    fn a_frame_is_refused_whole_when_it_does_not_hold_what_it_says() {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let (luma, side) = luma_of(&raster(&for_code(&invitation)).unwrap());
        // Short, long, empty, and sizes that cannot be: none is read, and none panics.
        assert_eq!(
            invitation_in_frame(&luma[..luma.len() - 1], side, side),
            None
        );
        let mut longer = luma.clone();
        longer.push(0);
        assert_eq!(invitation_in_frame(&longer, side, side), None);
        assert_eq!(invitation_in_frame(&[], 0, 0), None);
        assert_eq!(invitation_in_frame(&luma, 0, side), None);
        assert_eq!(invitation_in_frame(&luma, side, 0), None);
        assert_eq!(
            invitation_in_frame(&luma, usize::MAX, 2),
            None,
            "a size that overflows was read"
        );
        // Only a checked multiplication tells these apart: the product is 2^64, which wraps to 0, the length of an
        // empty frame, so an unchecked one would find the frame "complete" and go on to read pixels that are not there.
        let half = 1_usize << (usize::BITS / 2);
        assert_eq!(
            invitation_in_frame(&[], half, half),
            None,
            "a size that wraps to the frame's length was accepted"
        );
        // A frame of blank brightness has no code.
        assert_eq!(invitation_in_frame(&vec![255; 400], 20, 20), None);
        assert_eq!(invitation_in_frame(&vec![0; 400], 20, 20), None);
    }

    /// The corners of the code drawn in `luma`, as a camera would find them in it.
    fn seen_in(luma: &[u8], side: usize) -> Seen {
        look_in_frame(luma, side, side)
    }

    #[test]
    fn a_look_at_a_good_frame_finds_the_corners_of_the_code_and_the_invitation_in_it() {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let (luma, side) = luma_of(&raster(&for_code(&invitation)).unwrap());
        let seen = seen_in(&luma, side);
        assert_eq!(seen.invitation.as_deref(), Some(invitation.as_str()));
        assert_eq!(
            seen.corners.len(),
            1,
            "one code was drawn and {} were found",
            seen.corners.len()
        );
        // Every corner is a pixel of the frame, and the four are not one and the same point.
        for &(x, y) in &seen.corners[0] {
            assert!(
                x < side && y < side,
                "a corner ({x}, {y}) is outside a frame of {side}"
            );
        }
        let distinct: std::collections::HashSet<_> = seen.corners[0].iter().collect();
        assert!(
            distinct.len() == 4,
            "the four corners are not four points: {:?}",
            seen.corners[0]
        );
        // What it says is what the one that only reads says: the two never disagree on the same frame.
        assert_eq!(seen.invitation, invitation_in_frame(&luma, side, side));
    }

    #[test]
    fn a_code_that_is_seen_but_cannot_be_read_still_has_its_corners_shown() {
        // The case the person needs told most: the camera has the code in view and cannot read it yet (too far, blurred),
        // and what helps is to see that it is found. Its three finder squares are left alone and the middle is
        // wiped to one brightness, so the decoder locates a code and fails to decode it.
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let (mut luma, side) = luma_of(&raster(&for_code(&invitation)).unwrap());
        let (from, to) = (side * 3 / 10, side * 7 / 10);
        for y in from..to {
            for x in from..to {
                luma[y * side + x] = 255;
            }
        }
        let seen = seen_in(&luma, side);
        assert_eq!(
            seen.invitation, None,
            "a code with its middle wiped was read"
        );
        assert_eq!(
            seen.corners.len(),
            1,
            "the unreadable code was not located: {} found",
            seen.corners.len()
        );
    }

    #[test]
    fn a_blank_or_wrong_sized_frame_finds_nothing_and_another_kind_of_code_is_located_but_not_joined()
     {
        assert_eq!(look_in_frame(&vec![255; 400], 20, 20), Seen::default());
        assert_eq!(look_in_frame(&vec![0; 400], 20, 20), Seen::default());
        assert_eq!(look_in_frame(&[], 0, 0), Seen::default());
        assert_eq!(
            look_in_frame(&[1, 2, 3], 4, 4),
            Seen::default(),
            "a short frame was looked at"
        );
        assert_eq!(
            look_in_frame(
                &[],
                1_usize << (usize::BITS / 2),
                1_usize << (usize::BITS / 2)
            ),
            Seen::default()
        );
        // A code that is not an invitation (a web address): it is located, so the person can see it is found, and it is
        // never joined, as the reading alone already holds.
        let (luma, side) = luma_of(&raster(&for_code("https://example.com/a/page")).unwrap());
        let seen = seen_in(&luma, side);
        assert_eq!(seen.invitation, None);
        assert_eq!(seen.corners.len(), 1);
    }

    #[test]
    fn a_corner_the_decoder_gives_outside_the_frame_is_held_to_it() {
        // Signed, as the decoder gives them: a code only partly in view has corners before the first pixel or past the last.
        assert_eq!(held_to_frame(5, 7, 40, 20), (5, 7));
        assert_eq!(
            held_to_frame(-3, -1, 40, 20),
            (0, 0),
            "a negative corner was not held at the edge"
        );
        assert_eq!(
            held_to_frame(100, 50, 40, 20),
            (39, 19),
            "a corner past the far edge was not held at the last pixel"
        );
        assert_eq!(
            held_to_frame(39, 19, 40, 20),
            (39, 19),
            "the last pixel is inside"
        );
        assert_eq!(
            held_to_frame(40, 20, 40, 20),
            (39, 19),
            "one past is not inside"
        );
        assert_eq!(held_to_frame(i32::MIN, i32::MAX, 40, 20), (0, 19));
        // A frame with no size has no pixel to hold a point to, and gives the nearest it can without panicking.
        assert_eq!(held_to_frame(5, 5, 0, 0), (0, 0));
    }

    #[test]
    fn a_picture_is_shown_at_the_size_on_the_glass_it_was_drawn_for_at_any_screen_density() {
        // The Asus, measured on the device: 1080 physical pixels across, at a scale of 3.125.
        let asus = 500. / 160.;
        let code = 361.;
        // A code of 361 physical pixels is 115.5 logical units there: a third of the screen's 345.6, not 1128 pixels
        // on a 1080-pixel screen as it was when the pixels were shown as units.
        let shown = logical_side(code, asus);
        assert!((shown - 115.52).abs() < 0.01, "{shown} units");
        assert!(shown < 345.6, "the code is wider than the screen it is on");
        // On a desktop at a scale of 1, nothing changes: it is the size it always was.
        assert_eq!(logical_side(code, 1.0), code);
        // On the glass it is `code` pixels at any density: shown size times the scale is the pixels drawn.
        for scale in [1.0, 1.25, 1.5, 2.0, 2.625, 3.0, 3.125, 4.0] {
            let back = f64::from(logical_side(code, scale)) * scale;
            assert!(
                (back - f64::from(code)).abs() < 0.01,
                "at {scale} the code is {back} px on the glass"
            );
        }
        // A scale that cannot divide anything leaves the picture as it was, never nothing.
        for bad in [0., -1., f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                logical_side(code, bad),
                code,
                "scale {bad} changed the picture"
            );
        }
    }

    #[test]
    fn a_code_that_is_not_an_invitation_is_passed_over_so_a_camera_never_joins_the_wrong_thing() {
        for text in [
            "https://example.com/pay?to=someone",
            "peersus:2:7b7d",
            "PEERSUS:1:ZZZZ",
            "just some words",
            "peersus:1:",
        ] {
            let (luma, side) = luma_of(&raster(text).expect(text));
            assert_eq!(
                invitation_in_frame(&luma, side, side),
                None,
                "{text:?} was taken for an invitation"
            );
        }
    }

    #[test]
    fn capitals_make_the_same_invitation_a_smaller_code() {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let plain = QrCode::encode_text(&invitation, ECC).unwrap().size();
        let capitals = QrCode::encode_text(&for_code(&invitation), ECC)
            .unwrap()
            .size();
        assert!(
            capitals < plain,
            "{capitals} modules in capitals, {plain} as it was"
        );
    }

    #[test]
    fn what_is_drawn_in_capitals_is_read_back_exactly_as_the_owner_wrote_it() {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let raster = raster(&for_code(&invitation)).unwrap();
        let scanned = scan(&raster);
        assert_eq!(scanned.len(), 1);
        // The scanner gets capitals; the owner only accepts its prefix in lower case.
        assert_eq!(from_code(&scanned[0]), invitation);
    }

    #[test]
    fn text_that_is_not_an_invitation_is_left_alone_by_both_directions() {
        assert_eq!(for_code("hello World"), "hello World");
        assert_eq!(from_code("HELLO WORLD"), "HELLO WORLD");
    }

    #[test]
    fn the_code_at_the_scale_the_real_disc_allows_is_read_back_exactly() {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let drawn = for_code(&invitation);
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        let fit = fit(modules, PORTAL_DISC, 0.).unwrap();
        let raster = raster_at(&drawn, i32::from(fit.scale)).unwrap();
        assert_eq!(raster.side, u32::from(fit.side));
        let scanned = scan(&raster);
        assert_eq!(scanned.len(), 1, "scale {} px a module", fit.scale);
        assert_eq!(from_code(&scanned[0]), invitation);
    }

    /// Damage `percent` of the data modules of a drawn code, never the corners that hold the
    /// finder patterns nor the timing lines, which no level of correction protects. Returns how
    /// many of `trials` differently damaged copies were read back.
    fn survivors(text: &str, percent: usize, trials: u64) -> u64 {
        use rand::{RngExt, SeedableRng, rngs::StdRng};
        let modules = QrCode::encode_text(text, ECC).unwrap().size() as usize;
        let scale = 4usize;
        let mut read = 0;
        for seed in 0..trials {
            let mut random = StdRng::seed_from_u64(seed);
            let mut raster = raster_at(text, scale as i32).unwrap();
            let side = raster.side as usize;
            let target = modules * modules * percent / 100;
            let mut flipped = 0;
            while flipped < target {
                let (mx, my) = (
                    random.random_range(0..modules),
                    random.random_range(0..modules),
                );
                let corner =
                    ((mx < 9 || mx >= modules - 8) && my < 9) || (mx < 9 && my >= modules - 8);
                if corner || mx == 6 || my == 6 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let at = (((my + 4) * scale + dy) * side + (mx + 4) * scale + dx) * 4;
                        let value = 255 - raster.rgba[at];
                        raster.rgba[at..at + 3].fill(value);
                    }
                }
                flipped += 1;
            }
            if scan(&raster).len() == 1 {
                read += 1;
            }
        }
        read
    }

    #[test]
    fn one_percent_of_the_data_damaged_is_almost_always_still_read() {
        // Measured, 40 copies: Medium read 39, the lowest level read 11. A glare or a scratch
        // on a screen is damage of this kind; going back to a lower level must make this fail.
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let read = survivors(&for_code(&invitation), 1, 40);
        assert!(read >= 36, "only {read} of 40 damaged copies were read");
    }

    #[test]
    fn the_logo_covers_only_a_small_central_part_of_the_code() {
        for modules in [41, 57, 69, 77, 89] {
            let (from, size) = logo_area(modules);
            let covered = (size * size) as f32 / (modules * modules) as f32;
            assert!(
                covered <= 0.02,
                "{modules} modules: logo covers {:.1}%",
                covered * 100.
            );
            // Centred, so it is the same distance from both edges.
            assert_eq!(
                from + size + from,
                modules,
                "{modules} modules: not centred"
            );
        }
    }

    #[test]
    fn the_logo_never_reaches_a_finder_corner_or_a_timing_line() {
        for modules in [41, 57, 69, 77, 89] {
            let (from, size) = logo_area(modules);
            // The finder patterns with their separators take 9 modules in three corners, and
            // the timing lines run along row and column 6.
            assert!(from > 9, "{modules} modules: logo starts at {from}");
            assert!(
                from + size < modules - 9,
                "{modules} modules: logo ends at {}",
                from + size
            );
            assert!(from > 6 && from + size > 6);
        }
    }

    #[test]
    fn a_code_with_its_logo_in_place_is_still_read_back_exactly() {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let drawn = for_code(&invitation);
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        let fit = fit(modules, PORTAL_DISC, 0.).unwrap();
        let mut raster = raster_at(&drawn, i32::from(fit.scale)).unwrap();
        clear_logo_area(&mut raster, modules, i32::from(fit.scale), QUIET);
        let scanned = scan(&raster);
        assert_eq!(scanned.len(), 1, "logo area cleared at scale {}", fit.scale);
        assert_eq!(from_code(&scanned[0]), invitation);
    }

    #[test]
    fn clearing_the_logo_area_changes_only_that_area() {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let drawn = for_code(&invitation);
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        let whole = raster_at(&drawn, 3).unwrap();
        let mut cleared = raster_at(&drawn, 3).unwrap();
        clear_logo_area(&mut cleared, modules, 3, QUIET);
        let (from, size) = logo_area(modules);
        let (lo, hi) = ((from + 4) as usize * 3, (from + size + 4) as usize * 3);
        let side = whole.side as usize;
        for y in 0..side {
            for x in 0..side {
                let inside = x >= lo && x < hi && y >= lo && y < hi;
                let at = (y * side + x) * 4;
                if !inside {
                    assert_eq!(
                        whole.rgba[at], cleared.rgba[at],
                        "pixel ({x}, {y}) outside changed"
                    );
                }
            }
        }
        // And inside it is the plain light background the logo is drawn on.
        assert_eq!(cleared.rgba[(lo * side + lo) * 4], 0xff);
    }

    #[test]
    fn what_is_pasted_or_scanned_is_joined_only_if_it_is_an_invitation() {
        let lower = format!("peersus:1:{}", "7b22".repeat(8));
        // Pasted as the owner wrote it.
        assert_eq!(invitation_to_join(&lower).as_deref(), Some(lower.as_str()));
        // Scanned in capitals, as it is drawn.
        assert_eq!(
            invitation_to_join(&for_code(&lower)).as_deref(),
            Some(lower.as_str())
        );
        // Spaces and a newline around what was pasted are not part of it.
        assert_eq!(
            invitation_to_join(&format!("  {lower}\n")).as_deref(),
            Some(lower.as_str())
        );
        // Nothing, or anything else a camera or a clipboard can offer, is not sent to the owner.
        assert_eq!(invitation_to_join(""), None);
        assert_eq!(invitation_to_join("   "), None);
        assert_eq!(invitation_to_join("https://example.com"), None);
        assert_eq!(invitation_to_join("peersus:2:7b22"), None);
        assert_eq!(invitation_to_join("peersus:1:not hex"), None);
        assert_eq!(invitation_to_join("peersus:1:"), None);
    }

    #[test]
    fn the_logo_is_drawn_in_the_middle_of_the_code_and_only_there() {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let drawn = for_code(&invitation);
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        let plain = raster_at(&drawn, 3).unwrap();
        let with_logo = raster_with_logo(&drawn, 3).unwrap();
        assert_eq!(plain.side, with_logo.side);
        let (from, size) = logo_area(modules);
        let (lo, hi) = (
            (from + QUIET) as usize * 3,
            (from + size + QUIET) as usize * 3,
        );
        let side = plain.side as usize;
        let mut coloured = 0;
        for y in 0..side {
            for x in 0..side {
                let at = (y * side + x) * 4;
                let inside = x >= lo && x < hi && y >= lo && y < hi;
                if inside {
                    let p = &with_logo.rgba[at..at + 3];
                    if p[0] != p[1] || p[1] != p[2] {
                        coloured += 1;
                    }
                } else {
                    assert_eq!(
                        plain.rgba[at..at + 4],
                        with_logo.rgba[at..at + 4],
                        "pixel ({x}, {y}) outside changed"
                    );
                }
            }
        }
        assert!(
            coloured > 20,
            "only {coloured} coloured pixels: the logo is not there"
        );
    }

    #[test]
    fn a_code_carrying_the_real_logo_is_read_back_exactly() {
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let drawn = for_code(&invitation);
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        let fit = fit(modules, PORTAL_DISC, 0.).unwrap();
        let raster = raster_with_logo(&drawn, i32::from(fit.scale)).unwrap();
        let scanned = scan(&raster);
        assert_eq!(scanned.len(), 1, "scale {}", fit.scale);
        assert_eq!(from_code(&scanned[0]), invitation);
    }

    /// The raster of `text` drawn in the round style, at `scale`, with a side of `side` pixels.
    fn round_of(text: &str, scale: i32) -> Raster {
        round_raster_with_logo(text, scale, 0.).expect("the code must be drawable")
    }

    fn real_invitation() -> String {
        for_code(&format!("peersus:1:{}", "7b226164647265737322".repeat(28)))
    }

    /// Not a check: writes the round drawing of a REAL compact invitation to `PEERSUS_QR_OUT` so that it can be looked at, and says how it is
    /// sized against the disc it is drawn for. Run on purpose with `--ignored`; it changes nothing.
    #[test]
    #[ignore = "writes a picture for a person to look at"]
    fn write_the_round_drawing_for_inspection() {
        use iroh::{EndpointAddr, SecretKey};
        use peersus_net::invitation::Invitation;
        let Some(path) = std::env::var_os("PEERSUS_QR_OUT") else {
            return;
        };
        let address = EndpointAddr::new(SecretKey::from([4; 32]).public())
            .with_ip_addr("192.0.2.12:4567".parse().unwrap())
            .with_ip_addr("[2001:db8::12]:4568".parse().unwrap())
            .with_relay_url("https://relay.example.org/".parse().unwrap());
        let invitation = Invitation {
            address,
            capability: [7; 32],
            expires_at: 400,
        };
        let text = for_code(&invitation.encode().unwrap());
        let modules = QrCode::encode_text(&text, ECC).unwrap().size();
        let raster = portal_raster(&text, PORTAL_DISC, 0.).expect("a drawing");
        let side = raster.side;
        println!(
            "chars {} | modules {modules} | least_disc {} | asked {PORTAL_DISC} | drawn {side}",
            text.len(),
            least_disc(modules)
        );
        image::RgbaImage::from_raw(side, side, raster.rgba.clone())
            .expect("a square picture")
            .save(&path)
            .expect("written");
    }
    #[test]
    fn the_round_drawing_is_read_back_exactly_at_the_scale_the_disc_allows() {
        // At the scale the real portal draws, logo included, read by finding the three finder
        // patterns by their middles: the reader in `rqrr` takes the corners of the ring instead,
        // and a ring has none.
        let drawn = real_invitation();
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        let fit = fit(modules, PORTAL_DISC, 0.).unwrap();
        let scale = i32::from(fit.scale);
        let raster = round_of(&drawn, scale);
        let centres = finder_centres(&raster);
        assert_eq!(centres.len(), 3, "scale {scale}: finders found {centres:?}");
        let grid = sample_grid(&raster, &centres, scale as f32).expect("a grid");
        let (_, read) = rqrr::Grid::new(grid).decode().expect("decodes");
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        assert_eq!(from_code(&read), invitation);
    }

    /// The brightness plane of a drawn code as a camera would see it on a screen: what is see-through (the corners outside
    /// the disc) is the screen's white behind it, and the rest is the brightness of the ink. Not the raster's red channel
    /// alone, which is black wherever the disc is see-through.
    fn luma_on_white(raster: &Raster) -> (Vec<u8>, usize) {
        let side = raster.side as usize;
        let luma = (0..side * side)
            .map(|index| {
                let (value, alpha) = (
                    u16::from(raster.rgba[index * 4]),
                    u16::from(raster.rgba[index * 4 + 3]),
                );
                ((value * alpha + 255 * (255 - alpha)) / 255) as u8
            })
            .collect();
        (luma, side)
    }

    #[test]
    fn the_code_the_portal_really_draws_is_read_by_the_entry_the_camera_uses() {
        // The whole point of the code is that the camera reads it. This is the drawing the portal puts on the screen, for
        // the disc the phone has, taken through `look_in_frame`, the one place frames from the camera are read. Nothing
        // here is test-only: not the finder search, not the sampling, not a reader of the code's own.
        let text = real_invitation();
        let disc = disc_physical(346., 3.125);
        let raster = portal_raster(&text, disc, 0.).expect("the real invitation was not drawn");
        let (luma, side) = luma_on_white(&raster);
        let seen = look_in_frame(&luma, side, side);
        assert_eq!(
            seen.corners.len(),
            1,
            "the code the portal draws was not located by the camera's own reader"
        );
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        assert_eq!(
            seen.invitation.as_deref(),
            Some(invitation.as_str()),
            "it was located and could not be read"
        );
    }

    /// The brightness plane `luma` (`side` pixels square) turned by `degrees` and scaled by `zoom` about its middle onto a white frame of
    /// `frame` pixels square, sampled bilinearly: what a camera held askew, or nearer or further, hands over.
    fn seen_turned(luma: &[u8], side: usize, degrees: f32, zoom: f32, frame: usize) -> Vec<u8> {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let (middle_in, middle_out) = (side as f32 / 2., frame as f32 / 2.);
        let at = |x: f32, y: f32| -> f32 {
            let (x, y) = (
                x.clamp(0., side as f32 - 1.001),
                y.clamp(0., side as f32 - 1.001),
            );
            let (x0, y0) = (x.floor() as usize, y.floor() as usize);
            let (fx, fy) = (x - x0 as f32, y - y0 as f32);
            let p = |dx: usize, dy: usize| f32::from(luma[(y0 + dy) * side + x0 + dx]);
            (p(0, 0) * (1. - fx) + p(1, 0) * fx) * (1. - fy)
                + (p(0, 1) * (1. - fx) + p(1, 1) * fx) * fy
        };
        (0..frame * frame)
            .map(|index| {
                let (ox, oy) = (
                    (index % frame) as f32 - middle_out,
                    (index / frame) as f32 - middle_out,
                );
                // Back from the frame to the picture: undo the zoom, then the turn.
                let (rx, ry) = (ox / zoom, oy / zoom);
                let (ix, iy) = (
                    rx * cos + ry * sin + middle_in,
                    -rx * sin + ry * cos + middle_in,
                );
                if ix < 0. || iy < 0. || ix > side as f32 - 1. || iy > side as f32 - 1. {
                    255
                } else {
                    at(ix, iy) as u8
                }
            })
            .collect()
    }

    /// Every pixel the mean of its neighbours within `radius`: a lens a little out of focus.
    fn blurred(luma: &[u8], side: usize, radius: usize) -> Vec<u8> {
        let r = radius as isize;
        (0..side * side)
            .map(|index| {
                let (cx, cy) = ((index % side) as isize, (index / side) as isize);
                let (mut total, mut count) = (0u32, 0u32);
                for dy in -r..=r {
                    for dx in -r..=r {
                        let (x, y) = (
                            (cx + dx).clamp(0, side as isize - 1) as usize,
                            (cy + dy).clamp(0, side as isize - 1) as usize,
                        );
                        total += u32::from(luma[y * side + x]);
                        count += 1;
                    }
                }
                (total / count) as u8
            })
            .collect()
    }

    #[test]
    #[ignore = "timing probe, run on purpose"]
    fn how_long_the_fallback_takes_on_a_camera_frame_without_a_code() {
        // 1280x720 frames of the kind a camera hands over, with no code in them: the common case, and the one in which the fallback
        // always runs (the first reader found nothing, as there was nothing). One is busy with texture, the other a smooth scene.
        let (w, h) = (1280usize, 720usize);
        let busy: Vec<u8> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as u32, (i / w) as u32);
                let noise = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) >> 27;
                (110 + (x / 40 + y / 40) % 3 * 30 + noise) as u8
            })
            .collect();
        let smooth: Vec<u8> = (0..w * h)
            .map(|i| (40 + (i % w) * 150 / w + (i / w) * 40 / h) as u8)
            .collect();
        for (name, frame) in [("busy", &busy), ("smooth", &smooth)] {
            let first = std::time::Instant::now();
            let seen = look_in_frame(frame, w, h);
            println!(
                "{name}: look_in_frame {:?} (corners {}, read {})",
                first.elapsed(),
                seen.corners.len(),
                seen.invitation.is_some()
            );
            let only_middles = std::time::Instant::now();
            let middles = crate::qr_reader::finder_middles(frame, w, h);
            println!(
                "{name}: finder_middles alone {:?} ({} found)",
                only_middles.elapsed(),
                middles.len()
            );
        }
    }

    #[test]
    fn the_finder_middle_reader_reads_the_drawing_the_portal_makes_by_itself() {
        // The control for the rest: nothing turned, scaled or blurred, read by this reader alone and not through the entry the camera uses.
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let raster =
            portal_raster(&real_invitation(), disc_physical(346., 3.125), 0.).expect("drawn");
        let (luma, side) = luma_on_white(&raster);
        let found =
            crate::qr_reader::find(&luma, side, side).expect("the three finders were not found");
        assert_eq!(
            found.text.as_deref().map(from_code).as_deref(),
            Some(invitation.as_str()),
            "found and not read"
        );
    }

    #[test]
    fn a_turned_and_resized_drawing_is_still_read_by_the_finder_middle_reader() {
        // A phone is not held square to a screen, nor a fixed distance from it. The real invitation on the real disc, turned and brought
        // nearer or further, read back exactly.
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let raster =
            portal_raster(&real_invitation(), disc_physical(346., 3.125), 0.).expect("drawn");
        let (luma, side) = luma_on_white(&raster);
        for (degrees, zoom) in [
            (7., 1.),
            (-12., 0.9),
            (25., 0.8),
            (-38., 1.1),
            (90., 1.),
            (180., 0.9),
            (-90., 1.2),
        ] {
            let frame = (side as f32 * 1.6) as usize;
            let turned = seen_turned(&luma, side, degrees, zoom, frame);
            let read = crate::qr_reader::read(&turned, frame, frame);
            assert_eq!(
                read.as_deref().map(from_code).as_deref(),
                Some(invitation.as_str()),
                "turned {degrees} degrees at {zoom}x was not read"
            );
        }
    }

    #[test]
    fn a_blurred_drawing_is_still_read_by_the_finder_middle_reader() {
        // A screen shown to a camera that has not quite focused: a lens spreads each point over its neighbours.
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let raster =
            portal_raster(&real_invitation(), disc_physical(346., 3.125), 0.).expect("drawn");
        let (luma, side) = luma_on_white(&raster);
        for radius in [1, 2] {
            let soft = blurred(&luma, side, radius);
            let read = crate::qr_reader::read(&soft, side, side);
            assert_eq!(
                read.as_deref().map(from_code).as_deref(),
                Some(invitation.as_str()),
                "a blur of radius {radius} was not read"
            );
        }
    }

    #[test]
    fn the_finder_middle_reader_says_nothing_where_there_is_no_code() {
        let noise: Vec<u8> = (0..300 * 300)
            .map(|i| ((i as u32).wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        assert_eq!(crate::qr_reader::read(&noise, 300, 300), None);
        assert_eq!(look_in_frame(&noise, 300, 300).invitation, None);
        assert!(crate::qr_reader::find(&vec![255u8; 300 * 300], 300, 300).is_none());
    }

    #[test]
    fn the_code_is_read_by_the_camera_entry_on_every_disc_a_phone_may_have_and_not_by_luck_on_one()
    {
        // One disc passing proves nothing about the next: with the finder rings rounded, the real reader found the code at
        // some roundness and lost it at its neighbour, so a code that read on one phone could fail on another. Discs are
        // chosen and not gridded (a grid of 36 took 49 s, which is what makes a test get skipped): the smallest and the
        // largest, the two that failed with the rounded rings (300 at 2.0, and 300 at 3.125), the Asus's own (346 at
        // 3.125), and a spread between. Each is read with the real reader and the real invitation, exactly.
        let invitation = format!("peersus:1:{}", "7b226164647265737322".repeat(28));
        let text = real_invitation();
        let discs = [
            (300., 2.0),
            (300., 3.125),
            (300., 3.5),
            (320., 2.625),
            (346., 2.0),
            (346., 3.125),
            (360., 2.75),
            (360., 3.5),
            (400., 2.0),
            (400., 3.0),
            (440., 2.625),
            (440., 3.5),
        ];
        for (extent, scale) in discs {
            let disc = disc_physical(extent, scale);
            let raster = portal_raster(&text, disc, 0.).expect("the real invitation was not drawn");
            let (luma, side) = luma_on_white(&raster);
            let seen = look_in_frame(&luma, side, side);
            assert_eq!(
                seen.invitation.as_deref(),
                Some(invitation.as_str()),
                "a portal of {extent} at scale {scale} (disc {disc:.0} px, drawn {} px): {} codes located, none read",
                raster.side,
                seen.corners.len()
            );
        }
    }

    #[test]
    fn the_round_drawing_is_a_disc_with_see_through_corners_not_a_square() {
        let raster = round_of(&real_invitation(), 3);
        let side = raster.side as usize;
        let alpha = |x: usize, y: usize| raster.rgba[(y * side + x) * 4 + 3];
        for (x, y) in [(0, 0), (side - 1, 0), (0, side - 1), (side - 1, side - 1)] {
            assert_eq!(alpha(x, y), 0, "corner ({x}, {y}) is painted");
        }
        assert_eq!(alpha(side / 2, side / 2), 255);
    }

    #[test]
    fn nothing_of_the_round_drawing_lies_outside_its_disc() {
        let raster = round_of(&real_invitation(), 3);
        let side = raster.side as usize;
        let half = side as f32 / 2.;
        for y in 0..side {
            for x in 0..side {
                if raster.rgba[(y * side + x) * 4 + 3] != 0 {
                    let (dx, dy) = (x as f32 + 0.5 - half, y as f32 + 0.5 - half);
                    assert!(dx.hypot(dy) <= half + 0.5, "({x}, {y}) is outside the disc");
                }
            }
        }
    }

    #[test]
    fn the_three_finder_patterns_stay_whole_inside_the_round_disc() {
        // The finder patterns are what a camera finds first, and they sit in three corners of
        // the square: a disc drawn too small would cut them and the code would be dead.
        let drawn = real_invitation();
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        let scale = 3;
        let inner = ((modules + 2 * QUIET) * scale) as f32;
        let raster = round_of(&drawn, scale);
        let radius = raster.side as f32 / 2.;
        // The square is centred in the disc: its top left is `radius - inner / 2` from the edge.
        let shift = radius - inner / 2.;
        let corner = |mx: i32, my: i32| {
            let (x, y) = (
                shift + ((mx + QUIET) * scale) as f32,
                shift + ((my + QUIET) * scale) as f32,
            );
            (x - radius).hypot(y - radius)
        };
        let far = modules;
        // The outer corner of each finder's 7x7 square, the farthest point of it from the middle.
        for (x, y) in [(0, 0), (far, 0), (0, far)] {
            assert!(
                corner(x, y) <= radius,
                "the finder corner ({x}, {y}) is cut"
            );
        }
    }

    /// The grey of the pixel at module `(mx, my)` of the matrix, `(dx, dy)` of the way across it (0 to 1), in the round drawing at `scale`.
    fn grey_in_module(
        raster: &Raster,
        scale: i32,
        modules: i32,
        mx: i32,
        my: i32,
        dx: f32,
        dy: f32,
    ) -> u8 {
        let side = raster.side as i32;
        // The matrix sits in the middle of the disc, and `round_plain` puts `ROUND_QUIET` modules of the square it was given before it.
        let inner = (modules + 2 * ROUND_QUIET) * scale;
        let origin = (side / 2 - inner / 2) + ROUND_QUIET * scale;
        let x = origin + mx * scale + (dx * (scale - 1) as f32).round() as i32;
        let y = origin + my * scale + (dy * (scale - 1) as f32).round() as i32;
        raster.rgba[((y * side + x) * 4) as usize]
    }

    #[test]
    fn a_data_module_is_a_whole_square_all_the_way_to_its_corners() {
        // The drawing is of square modules, as in a code: a dark module is dark at its corner as much as at its middle.
        let drawn = real_invitation();
        let code = QrCode::encode_text(&drawn, ECC).unwrap();
        let modules = code.size();
        let scale = 6;
        let raster = round_of(&drawn, scale);
        let (mx, my) = (10..modules - 10)
            .flat_map(|x| (10..modules - 10).map(move |y| (x, y)))
            .find(|&(x, y)| {
                code.get_module(x, y) && !(30..48).contains(&x) && !(30..48).contains(&y)
            })
            .expect("a dark data module");
        for (dx, dy) in [(0.5, 0.5), (0., 0.), (1., 0.), (0., 1.), (1., 1.)] {
            assert_eq!(
                grey_in_module(&raster, scale, modules, mx, my, dx, dy),
                0x00,
                "a dark module is not dark at ({dx}, {dy}) of it"
            );
        }
    }

    #[test]
    fn a_finder_is_rings_with_light_corners() {
        // Three rings: a dark middle, a light ring, a dark ring. Round, so the corner of the 7x7 square it is drawn in is LIGHT.
        // The camera finds it by the proportions 1:1:3:1:1 of a line through its middle, which a ring has as much as a square.
        let drawn = real_invitation();
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        let scale = 6;
        let raster = round_of(&drawn, scale);
        let at = |dx: f32, dy: f32| grey_in_module(&raster, scale, modules, 0, 0, dx * 7., dy * 7.);
        assert_eq!(at(0.5, 0.5), 0x00, "the middle is dark");
        assert_eq!(
            grey_in_module(&raster, scale, modules, 3, 1, 0.5, 0.5),
            0xff,
            "the ring between the middle and the outer ring is light"
        );
        assert_eq!(
            grey_in_module(&raster, scale, modules, 3, 0, 0.5, 0.5),
            0x00,
            "the outer ring is dark"
        );
        assert_eq!(
            grey_in_module(&raster, scale, modules, 0, 0, 0., 0.),
            0xff,
            "the corner of the finder's square is light: it is a ring"
        );
    }

    #[test]
    fn the_disc_is_filled_with_modules_and_leaves_no_white_frame_round_the_code() {
        // The measure of what the user saw go wrong, twice: a square code with a frame of white in a circle. Here the field outside the
        // matrix carries the code's own data, so it is as dark as the code is. Measured on a band just outside each side of the matrix.
        let drawn = real_invitation();
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        let scale = 6;
        let raster = round_of(&drawn, scale);
        let side = raster.side as i32;
        let inner = (modules + 2 * ROUND_QUIET) * scale;
        let origin = (side / 2 - inner / 2) + ROUND_QUIET * scale;
        let matrix_side = modules * scale;
        let grey = |x: i32, y: i32| raster.rgba[((y * side + x) * 4) as usize];
        let alpha = |x: i32, y: i32| raster.rgba[((y * side + x) * 4 + 3) as usize];
        let half = side as f32 / 2.;
        let in_disc =
            |x: i32, y: i32| (x as f32 + 0.5 - half).hypot(y as f32 + 0.5 - half) < half - 2.;
        // Dark share of the pixels inside the matrix's data area, and of those in the disc beyond the matrix on every side.
        let (mut inside_dark, mut inside_all, mut outside_dark, mut outside_all) = (0, 0, 0, 0);
        for y in 0..side {
            for x in 0..side {
                if !in_disc(x, y) || alpha(x, y) != 0xff {
                    continue;
                }
                let (rx, ry) = (x - origin, y - origin);
                let in_matrix = (0..matrix_side).contains(&rx) && (0..matrix_side).contains(&ry);
                // Leave out the finders (and the light about them) and the logo, which are not data.
                let (gx, gy) = (rx.div_euclid(scale), ry.div_euclid(scale));
                let in_finder_zone =
                    [(0, 0), (modules - 7, 0), (0, modules - 7)]
                        .iter()
                        .any(|(ox, oy)| {
                            (ox - 1..ox + 8).contains(&gx) && (oy - 1..oy + 8).contains(&gy)
                        });
                let logo = logo_area(modules);
                let in_logo = (logo.0..logo.0 + logo.1).contains(&gx)
                    && (logo.0..logo.0 + logo.1).contains(&gy);
                if in_finder_zone || in_logo {
                    continue;
                }
                if in_matrix {
                    inside_all += 1;
                    inside_dark += i32::from(grey(x, y) < 128);
                } else {
                    outside_all += 1;
                    outside_dark += i32::from(grey(x, y) < 128);
                }
            }
        }
        assert!(
            outside_all > inside_all / 8,
            "there is hardly any disc beyond the matrix to measure: {outside_all} against {inside_all}"
        );
        let (inside, outside) = (
            inside_dark as f32 / inside_all as f32,
            outside_dark as f32 / outside_all as f32,
        );
        assert!(
            (inside - outside).abs() < 0.08,
            "the field beyond the matrix is {outside:.2} dark and the matrix {inside:.2}: the edge shows"
        );
    }

    #[test]
    fn every_module_of_the_round_drawing_is_on_the_one_grid_of_the_matrix() {
        // The decoration is not drawn at some other offset or size: across the whole disc, each module-sized cell of the grid that starts at
        // the matrix's corner is one grey, so that the repeated data cannot be told from the matrix by a seam.
        let drawn = real_invitation();
        let modules = QrCode::encode_text(&drawn, ECC).unwrap().size();
        let scale = 6;
        let raster = round_of(&drawn, scale);
        let side = raster.side as i32;
        let inner = (modules + 2 * ROUND_QUIET) * scale;
        let origin = (side / 2 - inner / 2) + ROUND_QUIET * scale;
        let (mut cells, mut mixed) = (0, 0);
        let mut gy = -origin.div_euclid(scale) - 1;
        while origin + gy * scale < side {
            let mut gx = -origin.div_euclid(scale) - 1;
            while origin + gx * scale < side {
                let (x0, y0) = (origin + gx * scale, origin + gy * scale);
                if x0 >= 0 && y0 >= 0 && x0 + scale <= side && y0 + scale <= side {
                    let at = |dx: i32, dy: i32| {
                        (
                            raster.rgba[(((y0 + dy) * side + x0 + dx) * 4 + 3) as usize],
                            raster.rgba[(((y0 + dy) * side + x0 + dx) * 4) as usize],
                        )
                    };
                    // Only cells wholly inside the disc, drawn in the decorative field, not the matrix (the matrix has its own logo and rings).
                    let whole = [
                        (0, 0),
                        (scale - 1, 0),
                        (0, scale - 1),
                        (scale - 1, scale - 1),
                    ]
                    .iter()
                    .all(|&(dx, dy)| at(dx, dy).0 == 0xff);
                    let outside_matrix = !(0..modules).contains(&gx) || !(0..modules).contains(&gy);
                    if whole && outside_matrix {
                        cells += 1;
                        let first = at(0, 0).1;
                        if (0..scale).any(|dy| (0..scale).any(|dx| at(dx, dy).1 != first)) {
                            mixed += 1;
                        }
                    }
                }
                gx += 1;
            }
            gy += 1;
        }
        assert!(
            cells > 200,
            "too few decorative cells were checked: {cells}"
        );
        assert_eq!(
            mixed, 0,
            "{mixed} of {cells} decorative cells are not one grey: they are off the matrix's grid"
        );
    }

    #[test]
    fn the_portal_drawing_depends_only_on_the_code_so_it_can_be_kept() {
        // The page keeps one drawing per code and does not redo it when something else changes,
        // which is only right if the same code always gives the same drawing and another code
        // gives another.
        let a = for_code(&format!("peersus:1:{}", "7b226164647265737322".repeat(28)));
        let b = for_code(&format!("peersus:1:{}", "7b226164647265737323".repeat(28)));
        let first = portal_raster(&a, PORTAL_DISC, 0.).expect("a drawing");
        assert_eq!(first, portal_raster(&a, PORTAL_DISC, 0.).unwrap());
        assert_ne!(first, portal_raster(&b, PORTAL_DISC, 0.).unwrap());
    }

    #[test]
    fn the_round_style_keeps_far_less_white_round_the_code_than_the_square_one_does() {
        // The measure of what the user saw go wrong: a wide white frame. The round margin is a fraction of the standard's four modules.
        const { assert!(ROUND_QUIET < QUIET) };
        const { assert!(ROUND_QUIET >= 1) };
    }

    // ---- a reader that finds the finder patterns by their middle, as a phone does ----------

    /// Run lengths of a line of greys, as `(is_dark, length)`.
    fn runs(line: &[u8]) -> Vec<(bool, usize)> {
        let mut runs: Vec<(bool, usize)> = Vec::new();
        for &v in line {
            let dark = v < 128;
            match runs.last_mut() {
                Some((d, n)) if *d == dark => *n += 1,
                _ => runs.push((dark, 1)),
            }
        }
        runs
    }

    /// Whether five runs have the proportions 1:1:3:1:1 of a finder pattern, within half a unit.
    fn is_finder_ratio(five: &[(bool, usize)]) -> bool {
        let total: usize = five.iter().map(|r| r.1).sum();
        if total < 7 || !five[0].0 {
            return false;
        }
        let unit = total as f32 / 7.;
        let want = [1., 1., 3., 1., 1.];
        five.iter()
            .zip(want)
            .all(|(r, w)| (r.1 as f32 - w * unit).abs() <= unit * 0.35 + 0.5)
    }

    /// Centres of the finder patterns found by scanning rows for 1:1:3:1:1 and keeping the
    /// places where the column through that centre agrees. Neighbouring hits are merged.
    fn finder_centres(raster: &Raster) -> Vec<(f32, f32)> {
        let side = raster.side as usize;
        let grey = |x: usize, y: usize| raster.rgba[(y * side + x) * 4];
        let mut found: Vec<(f32, f32, usize)> = Vec::new();
        for y in 0..side {
            let row: Vec<u8> = (0..side).map(|x| grey(x, y)).collect();
            let r = runs(&row);
            let mut at = 0usize;
            for (i, run) in r.iter().enumerate() {
                if i + 5 <= r.len() && is_finder_ratio(&r[i..i + 5]) {
                    let width: usize = r[i..i + 5].iter().map(|r| r.1).sum();
                    let cx = at as f32 + width as f32 / 2.;
                    // The column through that middle must have the same proportions.
                    let column: Vec<u8> = (0..side)
                        .map(|yy| grey((cx as usize).min(side - 1), yy))
                        .collect();
                    let cr = runs(&column);
                    let mut cat = 0usize;
                    for (j, _) in cr.iter().enumerate() {
                        if j + 5 <= cr.len() && is_finder_ratio(&cr[j..j + 5]) {
                            let h: usize = cr[j..j + 5].iter().map(|r| r.1).sum();
                            let cy = cat as f32 + h as f32 / 2.;
                            if (cy - y as f32).abs() <= h as f32 / 2. {
                                match found.iter_mut().find(|f| {
                                    (f.0 - cx).abs() < width as f32 && (f.1 - cy).abs() < h as f32
                                }) {
                                    Some(f) => f.2 += 1,
                                    None => found.push((cx, cy, 1)),
                                }
                            }
                        }
                        cat += cr[j].1;
                    }
                }
                at += run.1;
            }
        }
        // A finder pattern is seen on every row through its middle third, a few rows at least;
        // a chance match in the data is seen on one or two.
        let grey = |x: f32, y: f32| -> u8 {
            let (x, y) = (
                (x as isize).clamp(0, side as isize - 1) as usize,
                (y as isize).clamp(0, side as isize - 1) as usize,
            );
            raster.rgba[(y * side + x) * 4]
        };
        // Both diagonals through the middle must also show 1:1:3:1:1 centred on it: a round ring
        // and a square one both do, a chance match in the data does not.
        let diagonal_ok = |cx: f32, cy: f32, step: f32| -> bool {
            let reach = side as f32;
            let line: Vec<u8> = (-(reach as i32)..reach as i32)
                .map(|t| grey(cx + t as f32 * step * 0.5, cy + t as f32 * 0.5))
                .collect();
            let r = runs(&line);
            let mut at = 0usize;
            for (i, run) in r.iter().enumerate() {
                if i + 5 <= r.len() && is_finder_ratio(&r[i..i + 5]) {
                    let w: usize = r[i..i + 5].iter().map(|x| x.1).sum();
                    let middle = at as f32 + w as f32 / 2.;
                    if (middle - reach).abs() <= w as f32 / 7. + 1. {
                        return true;
                    }
                }
                at += run.1;
            }
            false
        };
        found
            .into_iter()
            .filter(|f| f.2 >= 3)
            .filter(|f| diagonal_ok(f.0, f.1, 1.) && diagonal_ok(f.0, f.1, -1.))
            .map(|f| (f.0, f.1))
            .collect()
    }

    #[test]
    fn the_reader_finds_exactly_three_finders_in_a_square_code() {
        // The control: the reader must work on the plain code before it is trusted on a round one.
        let raster = raster_at(&real_invitation(), 4).unwrap();
        assert_eq!(
            finder_centres(&raster).len(),
            3,
            "{:?}",
            finder_centres(&raster)
        );
    }

    #[test]
    fn the_reader_finds_nothing_in_a_blank_picture() {
        let blank = Raster {
            side: 200,
            rgba: vec![0xff; 200 * 200 * 4],
        };
        assert!(finder_centres(&blank).is_empty());
    }

    /// Sample a raster into the code's own grid once the three finder middles are known: the
    /// origin comes from the top left middle, the size from the distance to the top right one.
    fn sample_grid(raster: &Raster, centres: &[(f32, f32)], unit: f32) -> Option<rqrr::SimpleGrid> {
        if centres.len() != 3 {
            return None;
        }
        let side = raster.side as usize;
        let mut sorted = centres.to_vec();
        sorted.sort_by(|a, b| (a.0 + a.1).total_cmp(&(b.0 + b.1)));
        let top_left = sorted[0];
        let (a, b) = (sorted[1], sorted[2]);
        let top_right = if a.1 < b.1 { a } else { b };
        // Between two finder middles there are `size - 7` modules.
        let size = ((top_right.0 - top_left.0) / unit).round() as usize + 7;
        let origin = (top_left.0 - 3.5 * unit, top_left.1 - 3.5 * unit);
        Some(rqrr::SimpleGrid::from_func(size, |mx, my| {
            let x = (origin.0 + (mx as f32 + 0.5) * unit) as usize;
            let y = (origin.1 + (my as f32 + 0.5) * unit) as usize;
            raster.rgba[(y.min(side - 1) * side + x.min(side - 1)) * 4] < 128
        }))
    }

    #[test]
    fn the_centre_based_reader_decodes_a_square_code_too() {
        // Control: it must decode the plain code before it is allowed to judge a round one.
        let text = real_invitation();
        let scale = 4;
        let raster = raster_at(&text, scale).unwrap();
        let grid = sample_grid(&raster, &finder_centres(&raster), scale as f32).expect("a grid");
        let (_, read) = rqrr::Grid::new(grid).decode().expect("decodes");
        assert_eq!(read, text);
    }

    #[test]
    fn the_round_style_is_found_and_decoded_by_the_centre_based_reader() {
        let text = real_invitation();
        let scale = 4;
        let raster = round_style_at(&text, scale, ROUND_QUIET).unwrap();
        let centres = finder_centres(&raster);
        assert_eq!(centres.len(), 3, "finders found: {centres:?}");
        let grid = sample_grid(&raster, &centres, scale as f32).expect("a grid");
        let (_, read) = rqrr::Grid::new(grid).decode().expect("decodes");
        assert_eq!(read, text);
    }

    #[test]
    fn the_drawing_fills_the_disc_it_is_made_for_and_never_goes_past_it() {
        for text in [real_invitation(), for_code("peersus:1:7b22")] {
            for disc in [PORTAL_DISC, 402., 1000.] {
                let raster = portal_raster(&text, disc, 0.).unwrap();
                let side = raster.side as f32;
                // Not past the disc asked for, unless the code needs a larger one (see `portal_raster`).
                let modules = QrCode::encode_text(&text, ECC).unwrap().size();
                let wanted = disc.max(least_disc(modules));
                assert!(side <= wanted, "{side} px is past the disc of {wanted} px");
                // And it fills it, to within the whole module the grid is made of: a ring of nothing between the code and the edge is what this removes.
                let module = f32::from(fit(modules, wanted, 0.).unwrap().scale);
                assert!(
                    side > wanted - 2. * module,
                    "{side} px leaves a ring of nothing in a disc of {wanted} px"
                );
            }
        }
    }

    #[test]
    fn growing_the_field_round_the_code_does_not_move_or_change_the_matrix() {
        // The matrix is what is read; the field round it is only its own data repeated. Drawn for a larger disc, the pixels of the matrix are the same
        // ones, at the same distance from the middle, as in the smallest drawing that holds the square.
        let text = real_invitation();
        let scale = 3;
        let small = round_raster_with_logo(&text, scale, 0.).unwrap();
        let large = round_raster_with_logo(&text, scale, PORTAL_DISC).unwrap();
        assert!(large.side > small.side, "the field did not grow");
        let (s, l) = (small.side as i32, large.side as i32);
        let modules = QrCode::encode_text(&text, ECC).unwrap().size();
        let half = modules * scale / 2;
        for dy in -half..half {
            for dx in -half..half {
                let a = ((s / 2 + dy) * s + s / 2 + dx) * 4;
                let b = ((l / 2 + dy) * l + l / 2 + dx) * 4;
                assert_eq!(
                    small.rgba[a as usize], large.rgba[b as usize],
                    "the matrix changed at ({dx}, {dy}) from the middle"
                );
            }
        }
    }

    #[test]
    fn the_real_invitation_drawn_to_fill_the_disc_is_read_back_by_the_cameras_own_reader() {
        let text = real_invitation();
        for disc in [PORTAL_DISC, disc_physical(346., 3.125), 1000.] {
            let raster = portal_raster(&text, disc, 0.).unwrap();
            let (luma, side) = luma_on_white(&raster);
            let seen = look_in_frame(&luma, side, side);
            assert_eq!(
                seen.corners.len(),
                1,
                "the code drawn to fill a disc of {disc} px was not located"
            );
            assert_eq!(
                seen.invitation.as_deref(),
                Some(from_code(&text).as_str()),
                "the code drawn to fill a disc of {disc} px was not read"
            );
        }
    }
}
