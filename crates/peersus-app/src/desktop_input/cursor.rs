//! Owner-side cursor drawing, separate from tablet input and from the application's windows.
//!
//! Xwayland lets a small override-redirect surface be composed alongside native Wayland windows.
//! Its input region is empty: it never receives a click, takes focus, or moves the system pointer.
//! The uinput tablet continues to deliver all input; this surface only makes its position visible.
use super::{Raw, code};
use std::io;
use x11rb::{
    connection::Connection,
    protocol::{
        shape::SK,
        xfixes::ConnectionExt as _,
        xproto::{
            ColormapAlloc, ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux,
            ImageFormat, StackMode, VisualClass, WindowClass,
        },
    },
    rust_connection::RustConnection,
};

mod appearance;
mod session;
pub(crate) use session::authority_file;

pub(super) struct Cursor {
    connection: RustConnection,
    window: u32,
    colormap: u32,
    hotspot: (i32, i32),
    position: (i32, i32),
    area: (i32, i32),
    screen: (u16, u16),
    visible: bool,
}

fn failure(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

/// Whether a device's own pointer has to be drawn by this application: on a Wayland desktop, which
/// draws none for it. Asked of the session, not only of what this process was started with.
pub(super) fn needed() -> bool {
    session::wayland()
}

impl Cursor {
    pub(super) fn new(area: (i32, i32), label: Option<&str>) -> io::Result<Self> {
        let (connection, screen_index) = session::connect()?;
        connection
            .xfixes_query_version(5, 0)
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        let screen = &connection.setup().roots[screen_index];
        let dimensions = (screen.width_in_pixels, screen.height_in_pixels);
        let image = appearance::Appearance::load(&connection, screen.root);
        let visual = screen
            .allowed_depths
            .iter()
            .filter(|depth| depth.depth == 32)
            .flat_map(|depth| &depth.visuals)
            .find(|visual| {
                visual.class == VisualClass::TRUE_COLOR
                    && visual.red_mask == 0x00ff_0000
                    && visual.green_mask == 0x0000_ff00
                    && visual.blue_mask == 0x0000_00ff
            })
            .ok_or_else(appearance::invalid_visual)?
            .visual_id;
        let colormap = connection.generate_id().map_err(failure)?;
        connection
            .create_colormap(ColormapAlloc::NONE, colormap, screen.root, visual)
            .map_err(failure)?
            .check()
            .map_err(failure)?;
        let tag = label
            .and_then(|label| {
                Tag::measure(&connection, label, (image.width, image.height)).transpose()
            })
            .transpose()?;
        let (width, height) = tag
            .as_ref()
            .map_or((image.width, image.height), |tag| tag.extent);
        let pixmap = connection.generate_id().map_err(failure)?;
        connection
            .create_pixmap(32, pixmap, screen.root, width, height)
            .map_err(failure)?
            .check()
            .map_err(failure)?;
        let gc = connection.generate_id().map_err(failure)?;
        connection
            .create_gc(gc, pixmap, &CreateGCAux::new())
            .map_err(failure)?
            .check()
            .map_err(failure)?;
        // Everything outside the arrow and its tag is see-through.
        connection
            .change_gc(
                gc,
                &x11rb::protocol::xproto::ChangeGCAux::new().foreground(0),
            )
            .map_err(failure)?;
        connection
            .poly_fill_rectangle(
                pixmap,
                gc,
                &[x11rb::protocol::xproto::Rectangle {
                    x: 0,
                    y: 0,
                    width,
                    height,
                }],
            )
            .map_err(failure)?;
        let stride = usize::from(image.width) * 4;
        // Upload once, bounded below the server's request limit even for a large cursor.
        for y in (0..image.height).step_by(64) {
            let rows = (image.height - y).min(64);
            let start = usize::from(y) * stride;
            let end = start + usize::from(rows) * stride;
            connection
                .put_image(
                    ImageFormat::Z_PIXMAP,
                    pixmap,
                    gc,
                    image.width,
                    rows,
                    0,
                    i16::try_from(y).map_err(failure)?,
                    0,
                    32,
                    &image.pixels[start..end],
                )
                .map_err(failure)?
                .check()
                .map_err(failure)?;
        }
        if let Some(tag) = &tag {
            tag.draw(&connection, pixmap, gc)?;
        }
        let window = connection.generate_id().map_err(failure)?;
        connection
            .create_window(
                32,
                window,
                screen.root,
                0,
                0,
                width,
                height,
                0,
                WindowClass::INPUT_OUTPUT,
                visual,
                &CreateWindowAux::new()
                    .override_redirect(1)
                    .background_pixmap(pixmap)
                    .border_pixel(0)
                    .colormap(colormap),
            )
            .map_err(failure)?
            .check()
            .map_err(failure)?;
        let empty = connection.generate_id().map_err(failure)?;
        connection
            .xfixes_create_region(empty, &[])
            .map_err(failure)?
            .check()
            .map_err(failure)?;
        connection
            .xfixes_set_window_shape_region(window, SK::INPUT, 0, 0, empty)
            .map_err(failure)?
            .check()
            .map_err(failure)?;
        connection.xfixes_destroy_region(empty).map_err(failure)?;
        connection.free_gc(gc).map_err(failure)?;
        // The window retains the painted background; no repaint or image allocation per motion.
        connection.free_pixmap(pixmap).map_err(failure)?;
        connection.flush().map_err(failure)?;
        Ok(Self {
            connection,
            window,
            colormap,
            hotspot: image.hotspot,
            position: (0, 0),
            area,
            screen: dimensions,
            visible: false,
        })
    }

    pub(super) fn emit(&mut self, events: &[Raw]) -> io::Result<()> {
        let previous = self.visible;
        for &(kind, key, value) in events {
            match (kind, key) {
                (code::EV_ABS, code::ABS_X) => self.position.0 = value,
                (code::EV_ABS, code::ABS_Y) => self.position.1 = value,
                (code::EV_KEY, code::BTN_TOOL_PEN) => self.visible = value != 0,
                _ => {}
            }
        }
        if self.visible {
            let (x, y) = screen_position(self.position, self.area, self.screen);
            self.connection
                .configure_window(
                    self.window,
                    &ConfigureWindowAux::new()
                        .x(x - self.hotspot.0)
                        .y(y - self.hotspot.1)
                        .stack_mode(StackMode::ABOVE),
                )
                .map_err(failure)?;
            if !previous {
                self.connection.map_window(self.window).map_err(failure)?;
            }
        } else if previous {
            self.connection.unmap_window(self.window).map_err(failure)?;
        }
        self.connection.flush().map_err(failure)
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        let _ = self.connection.destroy_window(self.window);
        let _ = self.connection.free_colormap(self.colormap);
        let _ = self.connection.flush();
    }
}

/// A device's name drawn beside its pointer, in the display server's own built-in `fixed` font, so it needs nothing that a
/// desktop may lack, enlarged [`TAG_SCALE`] times so it reads at a glance. Only Latin-1 can be drawn that way; any other character
/// is shown as `?`, never dropped silently.
struct Tag {
    text: Vec<u8>,
    font: u32,
    /// The text as the font draws it, before enlarging: its size and baseline.
    glyphs: (u16, u16),
    ascent: u16,
    /// Top left of the box and its size.
    origin: (i16, i16),
    size: (u16, u16),
    /// The whole surface: the arrow and the tag.
    extent: (u16, u16),
}

/// The longest tag: a name is for telling devices apart at a glance, not for reading.
const MOST_TAG: usize = 24;
/// Opaque white text on a dark box at about four fifths opacity, premultiplied ARGB.
const TAG_TEXT: u32 = 0xFFFF_FFFF;
const TAG_BOX: u32 = 0xCC1C_1C1C;
const TAG_PAD: u16 = 6;
/// How many times the font's own pixels are enlarged: `fixed` is small on today's screens.
const TAG_SCALE: u16 = 2;

impl Tag {
    fn measure(
        connection: &RustConnection,
        label: &str,
        arrow: (u16, u16),
    ) -> io::Result<Option<Self>> {
        let text: Vec<u8> = label
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(MOST_TAG)
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .collect();
        if text.is_empty() {
            return Ok(None);
        }
        let font = connection.generate_id().map_err(failure)?;
        connection
            .open_font(font, b"fixed")
            .map_err(failure)?
            .check()
            .map_err(failure)?;
        let info = connection
            .query_font(font)
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        let advance = u16::try_from(info.max_bounds.character_width.max(1)).map_err(failure)?;
        let ascent = u16::try_from(info.font_ascent.max(1)).map_err(failure)?;
        let descent = u16::try_from(info.font_descent.max(0)).map_err(failure)?;
        let count = u16::try_from(text.len()).map_err(failure)?;
        let glyphs = (advance * count, ascent + descent);
        let size = (
            glyphs.0 * TAG_SCALE + 2 * TAG_PAD,
            glyphs.1 * TAG_SCALE + 2 * TAG_PAD,
        );
        // Below and to the right of the arrow's tip area, where it does not hide what the pointer points at.
        let origin = (
            i16::try_from(arrow.0 * 2 / 3).map_err(failure)?,
            i16::try_from(arrow.1 * 3 / 4).map_err(failure)?,
        );
        let extent = (
            arrow.0.max(origin.0 as u16 + size.0),
            arrow.1.max(origin.1 as u16 + size.1),
        );
        Ok(Some(Self {
            text,
            font,
            glyphs,
            ascent,
            origin,
            size,
            extent,
        }))
    }

    fn draw(&self, connection: &RustConnection, pixmap: u32, gc: u32) -> io::Result<()> {
        use x11rb::protocol::xproto::{ChangeGCAux, Rectangle};
        connection
            .change_gc(gc, &ChangeGCAux::new().foreground(TAG_BOX))
            .map_err(failure)?;
        connection
            .poly_fill_rectangle(
                pixmap,
                gc,
                &[Rectangle {
                    x: self.origin.0,
                    y: self.origin.1,
                    width: self.size.0,
                    height: self.size.1,
                }],
            )
            .map_err(failure)?;
        // The text is drawn once at the font's size on a scratch surface, read back and put down enlarged.
        let scratch = connection.generate_id().map_err(failure)?;
        connection
            .create_pixmap(32, scratch, pixmap, self.glyphs.0, self.glyphs.1)
            .map_err(failure)?
            .check()
            .map_err(failure)?;
        connection
            .change_gc(
                gc,
                &ChangeGCAux::new()
                    .foreground(TAG_TEXT)
                    .background(TAG_BOX)
                    .font(self.font),
            )
            .map_err(failure)?;
        connection
            .image_text8(scratch, gc, 0, self.ascent as i16, &self.text)
            .map_err(failure)?
            .check()
            .map_err(failure)?;
        let small = connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                scratch,
                0,
                0,
                self.glyphs.0,
                self.glyphs.1,
                u32::MAX,
            )
            .map_err(failure)?
            .reply()
            .map_err(failure)?
            .data;
        connection.free_pixmap(scratch).map_err(failure)?;
        connection.close_font(self.font).map_err(failure)?;
        let large = enlarge(&small, self.glyphs, TAG_SCALE)
            .ok_or_else(|| failure("the tag image has an unexpected size"))?;
        let (width, height) = (self.glyphs.0 * TAG_SCALE, self.glyphs.1 * TAG_SCALE);
        let stride = usize::from(width) * 4;
        // In bands, below the server's request limit.
        for top in (0..height).step_by(64) {
            let rows = (height - top).min(64);
            let start = usize::from(top) * stride;
            connection
                .put_image(
                    ImageFormat::Z_PIXMAP,
                    pixmap,
                    gc,
                    width,
                    rows,
                    self.origin.0 + TAG_PAD as i16,
                    self.origin.1 + (TAG_PAD + top) as i16,
                    0,
                    32,
                    &large[start..start + usize::from(rows) * stride],
                )
                .map_err(failure)?
                .check()
                .map_err(failure)?;
        }
        Ok(())
    }
}

/// 32-bit pixels enlarged `scale` times by repeating each one: the font's pixels stay crisp.
fn enlarge(pixels: &[u8], (width, height): (u16, u16), scale: u16) -> Option<Vec<u8>> {
    let (w, h, s) = (usize::from(width), usize::from(height), usize::from(scale));
    if pixels.len() != w * h * 4 || s == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4 * s * s);
    for row in pixels.chunks_exact(w * 4) {
        let mut line = Vec::with_capacity(w * 4 * s);
        for pixel in row.chunks_exact(4) {
            for _ in 0..s {
                line.extend_from_slice(pixel);
            }
        }
        for _ in 0..s {
            out.extend_from_slice(&line);
        }
    }
    Some(out)
}

fn screen_position(position: (i32, i32), area: (i32, i32), screen: (u16, u16)) -> (i32, i32) {
    let scale = |value: i32, extent: i32, pixels: u16| {
        let value = i64::from(value.clamp(0, (extent - 1).max(0)));
        let result = value * i64::from(pixels.saturating_sub(1)) / i64::from((extent - 1).max(1));
        i32::try_from(result).unwrap_or(i32::MAX)
    };
    (
        scale(position.0, area.0, screen.0),
        scale(position.1, area.1, screen.1),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_tag_is_enlarged_by_repeating_each_pixel_and_a_wrong_size_is_refused() {
        let pixels = [1, 1, 1, 1, 2, 2, 2, 2];
        let large = super::enlarge(&pixels, (2, 1), 2).unwrap();
        assert_eq!(
            large,
            [
                1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2,
                2, 2, 2, 2
            ]
        );
        assert!(super::enlarge(&pixels, (3, 1), 2).is_none());
    }

    use super::*;

    #[test]
    fn scales_hotspot_without_moving_the_system_pointer() {
        assert_eq!(
            screen_position((960, 540), (1921, 1081), (3841, 2161)),
            (1920, 1080)
        );
        assert_eq!(
            screen_position((-10, 2000), (1920, 1080), (1920, 1080)),
            (0, 1079)
        );
        assert_eq!(screen_position((0, 0), (1, 1), (1, 1)), (0, 0));
    }

    #[test]
    #[ignore = "requires an isolated X server; verifies drawing without moving the real pointer"]
    fn draws_two_independent_cursors_without_input_or_focus() {
        use x11rb::protocol::{
            shape::ConnectionExt as _,
            xproto::{ImageFormat, MapState},
        };
        let mut first = Cursor::new((1280, 720), Some("ROG")).unwrap();
        let mut second = Cursor::new((1280, 720), None).unwrap();
        let conn = &first.connection;
        let root = conn.setup().roots[0].root;
        let pointer = conn.query_pointer(root).unwrap().reply().unwrap();
        let focus = conn.get_input_focus().unwrap().reply().unwrap().focus;
        let show = |x, y| {
            [
                (code::EV_ABS, code::ABS_X, x),
                (code::EV_ABS, code::ABS_Y, y),
                (code::EV_KEY, code::BTN_TOOL_PEN, 1),
            ]
        };
        first.emit(&show(100, 120)).unwrap();
        second.emit(&show(600, 400)).unwrap();
        second
            .connection
            .get_input_focus()
            .unwrap()
            .reply()
            .unwrap();
        let conn = &first.connection;
        let geometry = conn.get_geometry(first.window).unwrap().reply().unwrap();
        let other = conn.get_geometry(second.window).unwrap().reply().unwrap();
        assert_ne!((geometry.x, geometry.y), (other.x, other.y));
        let expected = appearance::Appearance::load(conn, root);
        // The tagged pointer's surface holds the arrow and, beside it, its tag; the untagged one is the arrow alone.
        assert!(
            geometry.width > expected.width && geometry.height > expected.height,
            "the tag is drawn beside the arrow: {geometry:?}"
        );
        assert_eq!(
            (other.width, other.height),
            (expected.width, expected.height)
        );
        assert_eq!(
            (
                i32::from(geometry.x) + expected.hotspot.0,
                i32::from(geometry.y) + expected.hotspot.1
            ),
            screen_position((100, 120), (1280, 720), first.screen),
            "the themed arrow's tip stays at the input position"
        );
        assert_eq!(
            conn.get_window_attributes(first.window)
                .unwrap()
                .reply()
                .unwrap()
                .map_state,
            MapState::VIEWABLE
        );
        assert!(
            conn.shape_get_rectangles(first.window, SK::INPUT)
                .unwrap()
                .reply()
                .unwrap()
                .rectangles
                .is_empty()
        );
        assert!(
            !conn
                .shape_get_rectangles(first.window, SK::BOUNDING)
                .unwrap()
                .reply()
                .unwrap()
                .rectangles
                .is_empty()
        );
        let image_of = |window: u32, width: u16, height: u16| {
            conn.get_image(ImageFormat::Z_PIXMAP, window, 0, 0, width, height, u32::MAX)
                .unwrap()
                .reply()
                .unwrap()
                .data
        };
        assert_eq!(
            image_of(second.window, other.width, other.height),
            expected.pixels,
            "theme colors and alpha survive the upload unchanged"
        );
        // The tag is there: its box and its white letters.
        let tagged = image_of(first.window, geometry.width, geometry.height);
        let pixel = |bytes: [u8; 4]| tagged.chunks_exact(4).any(|pixel| pixel == bytes);
        assert!(pixel(TAG_BOX.to_le_bytes()), "the tag's box is drawn");
        assert!(pixel(TAG_TEXT.to_le_bytes()), "the tag's letters are drawn");
        let after = conn.query_pointer(root).unwrap().reply().unwrap();
        assert_eq!(
            (after.root_x, after.root_y),
            (pointer.root_x, pointer.root_y)
        );
        assert_eq!(
            conn.get_input_focus().unwrap().reply().unwrap().focus,
            focus
        );
        first
            .emit(&[(code::EV_KEY, code::BTN_TOOL_PEN, 0)])
            .unwrap();
        assert_eq!(
            first
                .connection
                .get_window_attributes(first.window)
                .unwrap()
                .reply()
                .unwrap()
                .map_state,
            MapState::UNMAPPED
        );
        assert_eq!(
            second
                .connection
                .get_window_attributes(second.window)
                .unwrap()
                .reply()
                .unwrap()
                .map_state,
            MapState::VIEWABLE
        );
        first.emit(&show(300, 200)).unwrap();
        assert_eq!(
            first
                .connection
                .get_window_attributes(first.window)
                .unwrap()
                .reply()
                .unwrap()
                .map_state,
            MapState::VIEWABLE
        );
        let window = first.window;
        drop(first);
        assert!(
            second
                .connection
                .get_window_attributes(window)
                .unwrap()
                .reply()
                .is_err()
        );
    }
}
