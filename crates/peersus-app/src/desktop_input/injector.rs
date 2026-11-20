//! Windows and macOS: input replayed through the system's own input calls (SendInput, Quartz events), by `enigo`.
use super::{Device, Raw, code};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};

/// `VK_*` codes of keys whose Linux code is not their set-1 scan code (the extended keys).
#[cfg(windows)]
fn extended(linux: u16) -> Option<u32> {
    Some(match linux {
        96 => 0x0D,  // keypad Enter: Enter
        97 => 0xA3,  // right Ctrl
        98 => 0x6F,  // keypad /
        99 => 0x2C,  // Print Screen
        100 => 0xA5, // right Alt
        102 => 0x24, // Home
        103 => 0x26, // Up
        104 => 0x21, // Page Up
        105 => 0x25, // Left
        106 => 0x27, // Right
        107 => 0x23, // End
        108 => 0x28, // Down
        109 => 0x22, // Page Down
        110 => 0x2D, // Insert
        111 => 0x2E, // Delete
        125 => 0x5B, // left Windows
        126 => 0x5C, // right Windows
        127 => 0x5D, // Menu
        _ => return None,
    })
}

pub struct SendInput(Enigo);

pub fn open() -> std::io::Result<SendInput> {
    Enigo::new(&settings())
        .map(SendInput)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::Unsupported))
}

/// The device is made again at every batch of input until it can be made, so a system that asks the person for a permission
/// each time it is refused would put the same dialog up sixty times a second: it is asked for once in a run, and after that
/// the permission is only looked at, so that granting it takes effect at once and refusing it is not repeated.
#[cfg(target_os = "macos")]
fn settings() -> Settings {
    static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    Settings {
        open_prompt_to_get_permissions: !ASKED.swap(true, std::sync::atomic::Ordering::Relaxed),
        ..Settings::default()
    }
}

#[cfg(windows)]
fn settings() -> Settings {
    Settings::default()
}

#[cfg(target_os = "macos")]
/// `CGKeyCode`s (ANSI layout) of the Linux key codes a device sends: the keys, not the characters,
/// so what is typed follows the receiving Mac's own layout.
fn virtual_key(linux: u16) -> Option<u16> {
    Some(match linux {
        1 => 53,
        2..=11 => [18, 19, 20, 21, 23, 22, 26, 28, 25, 29][usize::from(linux - 2)],
        12 => 27,
        13 => 24,
        14 => 51,
        15 => 48,
        16..=25 => [12, 13, 14, 15, 17, 16, 32, 34, 31, 35][usize::from(linux - 16)],
        26 => 33,
        27 => 30,
        28 => 36,
        29 => 59,
        30..=38 => [0, 1, 2, 3, 5, 4, 38, 40, 37][usize::from(linux - 30)],
        39 => 41,
        40 => 39,
        41 => 50,
        42 => 56,
        43 => 42,
        44..=50 => [6, 7, 8, 9, 11, 45, 46][usize::from(linux - 44)],
        51 => 43,
        52 => 47,
        53 => 44,
        54 => 60,
        55 => 67,
        56 => 58,
        57 => 49,
        58 => 57,
        59..=68 => [122, 120, 99, 118, 96, 97, 98, 100, 101, 109][usize::from(linux - 59)],
        87 => 103,
        88 => 111,
        96 => 76,
        97 => 62,
        98 => 75,
        100 => 61,
        102 => 115,
        103 => 126,
        104 => 116,
        105 => 123,
        106 => 124,
        107 => 119,
        108 => 125,
        109 => 121,
        110 => 114,
        111 => 117,
        125 => 55,
        126 => 54,
        127 => 110,
        _ => return None,
    })
}

impl SendInput {
    #[cfg(windows)]
    fn key(&mut self, key: u16, direction: Direction) -> enigo::InputResult<()> {
        match extended(key) {
            Some(vk) => self.0.key(enigo::Key::Other(vk), direction),
            // Linux codes up to F12 are the PC set-1 scan codes.
            None if (1..=88).contains(&key) => self.0.raw(key, direction),
            None => Ok(()),
        }
    }

    /// A key without a Mac key is left out, as a character without a key is.
    #[cfg(target_os = "macos")]
    fn key(&mut self, key: u16, direction: Direction) -> enigo::InputResult<()> {
        match virtual_key(key) {
            Some(keycode) => self.0.raw(keycode, direction),
            None => Ok(()),
        }
    }

    fn replay(&mut self, events: &[Raw]) -> enigo::InputResult<()> {
        let (mut dx, mut dy) = (0, 0);
        let mut absolute = (None, None);
        for &(kind, what, value) in events {
            match (kind, what) {
                (code::EV_REL, code::REL_X) => dx += value,
                (code::EV_REL, code::REL_Y) => dy += value,
                // Whole notches; the high-resolution twin of each carries the same motion.
                (code::EV_REL, code::REL_WHEEL) => self.0.scroll(-value, Axis::Vertical)?,
                (code::EV_REL, code::REL_HWHEEL) => self.0.scroll(value, Axis::Horizontal)?,
                (code::EV_ABS, code::ABS_X) => absolute.0 = Some(value),
                (code::EV_ABS, code::ABS_Y) => absolute.1 = Some(value),
                (code::EV_KEY, button @ (code::BTN_LEFT | code::BTN_RIGHT | code::BTN_MIDDLE)) => {
                    self.motion(&mut dx, &mut dy, &mut absolute)?;
                    let button = match button {
                        code::BTN_LEFT => Button::Left,
                        code::BTN_RIGHT => Button::Right,
                        _ => Button::Middle,
                    };
                    self.0.button(button, direction(value))?;
                }
                (code::EV_KEY, key) => self.key(key, direction(value))?,
                _ => {}
            }
        }
        self.motion(&mut dx, &mut dy, &mut absolute)
    }

    /// Motion gathered so far is replayed before a click, so the click lands where it should.
    fn motion(
        &mut self,
        dx: &mut i32,
        dy: &mut i32,
        absolute: &mut (Option<i32>, Option<i32>),
    ) -> enigo::InputResult<()> {
        if let (Some(x), Some(y)) = std::mem::take(absolute) {
            let (w, h) = self.0.main_display()?;
            let at = |share: i32, size: i32| {
                (i64::from(share) * i64::from(size.max(1) - 1) / i64::from(code::ABS_MAX)) as i32
            };
            self.0.move_mouse(at(x, w), at(y, h), Coordinate::Abs)?;
        }
        if (*dx, *dy) != (0, 0) {
            self.0.move_mouse(*dx, *dy, Coordinate::Rel)?;
            (*dx, *dy) = (0, 0);
        }
        Ok(())
    }
}

fn direction(value: i32) -> Direction {
    if value == 0 {
        Direction::Release
    } else {
        Direction::Press
    }
}

impl Device for SendInput {
    fn emit(&mut self, events: &[Raw]) -> std::io::Result<()> {
        self.replay(events)
            .map_err(|_| std::io::Error::other("the input could not be replayed"))
    }
}
