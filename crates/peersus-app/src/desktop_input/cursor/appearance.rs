//! Read-only cursor appearance discovery. The X resource size is already display-scaled.
use std::{io, io::Read};
use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt as _, ImageOrder},
    rust_connection::RustConnection,
};

pub(super) struct Appearance {
    pub width: u16,
    pub height: u16,
    pub hotspot: (i32, i32),
    /// Premultiplied ARGB pixels in the server's image byte order.
    pub pixels: Vec<u8>,
}

impl Appearance {
    pub fn load(connection: &RustConnection, root: u32) -> Self {
        let resources = connection
            .get_property(
                false,
                root,
                AtomEnum::RESOURCE_MANAGER,
                AtomEnum::STRING,
                0,
                65_536,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| String::from_utf8_lossy(&reply.value).into_owned())
            .unwrap_or_default();
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|value| cursor_size(&value))
            .or_else(|| resource(&resources, "Xcursor.size").and_then(cursor_size))
            .unwrap_or(24);
        let theme = std::env::var("XCURSOR_THEME")
            .ok()
            .or_else(gnome_theme)
            .or_else(|| resource(&resources, "Xcursor.theme").map(str::to_owned))
            .unwrap_or_else(|| "default".into());
        let theme = xcursor::CursorTheme::load(&theme);
        let image = ["left_ptr", "default", "arrow"]
            .into_iter()
            .find_map(|name| {
                let path = theme.load_icon(name)?;
                let mut contents = Vec::new();
                std::fs::File::open(path)
                    .ok()?
                    .take(4 * 1024 * 1024)
                    .read_to_end(&mut contents)
                    .ok()?;
                select(xcursor::parser::parse_xcursor(&contents)?, size)
            });
        let mut appearance = image.unwrap_or_else(|| Self::fallback(size));
        if connection.setup().image_byte_order == ImageOrder::MSB_FIRST {
            for pixel in appearance.pixels.chunks_exact_mut(4) {
                pixel.reverse();
            }
        }
        appearance
    }

    fn fallback(size: u16) -> Self {
        // Keep a usable arrow even if a theme has only scalable images or is unavailable.
        let width = size;
        let height = size;
        let mut pixels = vec![0; usize::from(width) * usize::from(height) * 4];
        for y in 0..height {
            let source_y = i32::from(y) * 28 / i32::from(height);
            let (left, right) = if source_y < 19 {
                (0, source_y * 3 / 4)
            } else if source_y < 24 {
                (0, 6)
            } else {
                (5, 10)
            };
            for x in 0..width {
                let source_x = i32::from(x) * 28 / i32::from(width);
                if source_x < left || source_x > right {
                    continue;
                }
                let inside = source_x > left && source_x < right && source_y > 1 && source_y < 27;
                let offset = (usize::from(y) * usize::from(width) + usize::from(x)) * 4;
                pixels[offset..offset + 4].copy_from_slice(&if inside {
                    [255, 255, 255, 255]
                } else {
                    [0, 0, 0, 255]
                });
            }
        }
        Self {
            width,
            height,
            hotspot: (0, 0),
            pixels,
        }
    }
}

fn cursor_size(value: &str) -> Option<u16> {
    value
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|size| (1..=256).contains(size))
}

fn resource<'a>(resources: &'a str, key: &str) -> Option<&'a str> {
    resources.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name.trim() == key).then(|| value.trim())
    })
}

fn gnome_theme() -> Option<String> {
    if !std::env::var("XDG_CURRENT_DESKTOP")
        .ok()?
        .split(':')
        .any(|desktop| desktop.eq_ignore_ascii_case("GNOME"))
    {
        return None;
    }
    // GNOME's native cursor theme can differ from Xwayland's resource left by an X11 client.
    // This is a single read when creating a controller, never a command on the motion path.
    let result = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "cursor-theme"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !result.status.success() {
        return None;
    }
    let theme = String::from_utf8(result.stdout).ok()?;
    let theme = theme.trim().trim_matches('\'');
    (!theme.is_empty()).then(|| theme.to_owned())
}

fn select(images: Vec<xcursor::parser::Image>, size: u16) -> Option<Appearance> {
    let image = images
        .into_iter()
        .filter(|image| {
            (1..=512).contains(&image.width)
                && (1..=512).contains(&image.height)
                && image.xhot < image.width
                && image.yhot < image.height
                && image.pixels_rgba.len() == image.width as usize * image.height as usize * 4
        })
        .min_by_key(|image| image.size.abs_diff(u32::from(size)))?;
    Some(Appearance {
        width: u16::try_from(image.width).ok()?,
        height: u16::try_from(image.height).ok()?,
        hotspot: (
            i32::try_from(image.xhot).ok()?,
            i32::try_from(image.yhot).ok()?,
        ),
        // Xcursor stores little-endian ARGB words, matching little-endian X11 ZPixmap data.
        pixels: image.pixels_rgba,
    })
}

pub(super) fn invalid_visual() -> io::Error {
    io::Error::other("the display has no 32-bit cursor visual")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_size_is_not_scaled_twice() {
        let resources = "Xft.dpi:\t192\nXcursor.size:\t48\n";
        assert_eq!(
            resource(resources, "Xcursor.size").and_then(cursor_size),
            Some(48)
        );
        assert_eq!(cursor_size("0"), None);
        assert_eq!(cursor_size("99999"), None);
    }

    #[test]
    fn selects_nominal_size_and_keeps_the_theme_hotspot() {
        let image = |size, width, xhot| xcursor::parser::Image {
            size,
            width,
            height: width,
            xhot,
            yhot: 2,
            delay: 0,
            pixels_rgba: vec![255; width as usize * width as usize * 4],
            pixels_argb: vec![],
        };
        let chosen = select(vec![image(24, 32, 3), image(48, 64, 7)], 48).unwrap();
        assert_eq!((chosen.width, chosen.height), (64, 64));
        assert_eq!(chosen.hotspot, (7, 2));
    }

    #[test]
    fn fallback_matches_size_and_keeps_transparent_background() {
        let arrow = Appearance::fallback(48);
        assert_eq!((arrow.width, arrow.height), (48, 48));
        assert!(arrow.pixels.chunks_exact(4).any(|pixel| pixel[3] == 0));
        assert!(
            arrow
                .pixels
                .chunks_exact(4)
                .any(|pixel| pixel == [255, 255, 255, 255])
        );
    }
}
