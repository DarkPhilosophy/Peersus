//! Linux: the virtual mouse, keyboard and pen tablets made through `/dev/uinput`, which the desktop reads like real devices.
use super::{Device, Raw, code};
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, InputEvent, KeyCode, RelativeAxisCode, UinputAbsSetup,
    uinput::VirtualDevice,
};

/// A relative mouse and keyboard, and, made when first needed, an absolute pointer that places the
/// pointer at a share of the desktop as a tablet does: what the layout of devices moves it with.
pub struct Uinput {
    relative: VirtualDevice,
    absolute: Option<VirtualDevice>,
}

fn absolute_device() -> std::io::Result<VirtualDevice> {
    let mut buttons = AttributeSet::<KeyCode>::new();
    for button in [code::BTN_LEFT, code::BTN_RIGHT, code::BTN_MIDDLE] {
        buttons.insert(KeyCode::new(button));
    }
    let axis = |code: u16| {
        UinputAbsSetup::new(
            AbsoluteAxisCode(code),
            AbsInfo::new(0, 0, code::ABS_MAX, 0, 0, 1),
        )
    };
    VirtualDevice::builder()?
        .name("Peersus remote pointer")
        .with_keys(&buttons)?
        .with_absolute_axis(&axis(code::ABS_X))?
        .with_absolute_axis(&axis(code::ABS_Y))?
        .build()
}

/// A device's own pointer: a pen tablet whose range is the desktop in pixels, so one unit is one
/// pixel. It has a physical resolution, without which libinput does not take a tablet.
pub fn pen(owner: &str, label: Option<&str>, area: (i32, i32)) -> std::io::Result<Pen> {
    let mut tools = AttributeSet::<KeyCode>::new();
    for tool in [
        code::BTN_TOOL_PEN,
        code::BTN_TOUCH,
        code::BTN_STYLUS,
        code::BTN_STYLUS2,
    ] {
        tools.insert(KeyCode::new(tool));
    }
    let axis = |code: u16, extent: i32| {
        UinputAbsSetup::new(
            AbsoluteAxisCode(code),
            AbsInfo::new(0, 0, (extent - 1).max(1), 0, 0, 10),
        )
    };
    let name: String = format!("Peersus pointer {owner}")
        .chars()
        .take(79)
        .collect();
    let device = VirtualDevice::builder()?
        .name(&name)
        .with_keys(&tools)?
        .with_absolute_axis(&axis(code::ABS_X, area.0))?
        .with_absolute_axis(&axis(code::ABS_Y, area.1))?
        .build()?;
    let cursor = if super::cursor::needed() {
        Some(super::cursor::Cursor::new(area, label).map_err(|_| {
            tracing::warn!("Independent cursor unavailable; the next input will retry");
            std::io::Error::other("Independent cursor drawing is unavailable")
        })?)
    } else {
        None
    };
    Ok(Pen { device, cursor })
}

pub struct Pen {
    device: VirtualDevice,
    cursor: Option<super::cursor::Cursor>,
}

impl Device for Pen {
    fn emit(&mut self, events: &[Raw]) -> std::io::Result<()> {
        let input: Vec<InputEvent> = events
            .iter()
            .map(|&(kind, code, value)| InputEvent::new(kind, code, value))
            .collect();
        if let Some(cursor) = &mut self.cursor {
            cursor.emit(events)?;
        }
        self.device.emit(&input)?;
        Ok(())
    }
}
/// A virtual mouse and keyboard with exactly the buttons, keys and axes this service sends.
pub fn open() -> std::io::Result<Uinput> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for button in [code::BTN_LEFT, code::BTN_RIGHT, code::BTN_MIDDLE] {
        keys.insert(KeyCode::new(button));
    }
    // Every key a keyboard has, by Linux code: what a keyboard passed through can press. The
    // range of buttons (0x100 to 0x15f) is the mouse's, above.
    for key in (1..0x100).chain(0x160..=0x2ff) {
        keys.insert(KeyCode::new(key));
    }
    let mut axes = AttributeSet::<RelativeAxisCode>::new();
    for axis in [
        code::REL_X,
        code::REL_Y,
        code::REL_WHEEL,
        code::REL_HWHEEL,
        code::REL_WHEEL_HI_RES,
        code::REL_HWHEEL_HI_RES,
    ] {
        axes.insert(RelativeAxisCode(axis));
    }
    let device = VirtualDevice::builder()?
        .name("Peersus remote input")
        .with_keys(&keys)?
        .with_relative_axes(&axes)?
        .build()?;
    Ok(Uinput {
        relative: device,
        absolute: None,
    })
}

impl Device for Uinput {
    fn emit(&mut self, events: &[Raw]) -> std::io::Result<()> {
        // In runs, in order, each to its device: a position and then a press is a click there.
        let mut start = 0;
        while start < events.len() {
            let absolute = events[start].0 == code::EV_ABS;
            let end = events[start..]
                .iter()
                .position(|event| (event.0 == code::EV_ABS) != absolute)
                .map_or(events.len(), |at| start + at);
            let run: Vec<InputEvent> = events[start..end]
                .iter()
                .map(|&(kind, code, value)| InputEvent::new(kind, code, value))
                .collect();
            // `emit` ends the group with a synchronization report, so the system reads it at once.
            if absolute {
                self.warm()?;
                if let Some(device) = self.absolute.as_mut() {
                    device.emit(&run)?;
                }
            } else {
                self.relative.emit(&run)?;
            }
            start = end;
        }
        Ok(())
    }

    fn warm(&mut self) -> std::io::Result<()> {
        if self.absolute.is_none() {
            self.absolute = Some(absolute_device()?);
        }
        Ok(())
    }
}
