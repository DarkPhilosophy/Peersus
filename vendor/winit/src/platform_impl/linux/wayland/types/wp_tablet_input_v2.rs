//! Handling of `zwp_tablet_v2`: a graphics-tablet tool over a window acts as its pointer.
//!
//! A compositor delivers a tablet tool (a pen, or the pointer another device drives on this
//! desktop through a virtual tablet) only to clients that bind the tablet protocol. Without it the
//! tool moves over the window but nothing in the window sees it or can be clicked with it. This
//! turns the tool into the pointer events winit 0.30 already has: entering, moving and leaving
//! the window, the tip as the left button and the barrel buttons as the right and middle ones.
//! A press of the tool also gives the window the serial it needs to be moved or resized from it,
//! and over a window the tool is given the arrow, which GNOME otherwise does not draw for a tool
//! over a client that binds the tablet protocol.
//!
//! Backported, in substance, from winit 0.31 (`winit-wayland/src/types/wp_tablet_input_v2.rs`),
//! which reports the tool with the tablet-aware pointer events that 0.30 does not have.

use std::sync::Mutex;

use sctk::globals::GlobalData;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{
    delegate_dispatch, event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use sctk::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use sctk::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_manager_v2::ZwpTabletManagerV2;
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_pad_group_v2::{
    self, ZwpTabletPadGroupV2,
};
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2;
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2;
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_pad_v2::{
    self, ZwpTabletPadV2,
};
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_seat_v2::{
    self, ZwpTabletSeatV2,
};
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_tool_v2::{
    ButtonState, Event as ToolEvent, ZwpTabletToolV2,
};
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_v2::ZwpTabletV2;

use crate::dpi::LogicalPosition;
use crate::event::{ElementState, MouseButton, WindowEvent};
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId};

/// Linux evdev codes of the tool buttons the tablet protocol reports.
const BTN_STYLUS: u32 = 0x14b;
const BTN_STYLUS2: u32 = 0x14c;
const BTN_STYLUS3: u32 = 0x149;

/// The tablet manager, bound once for the connection.
#[derive(Debug, Clone)]
pub struct TabletManager {
    manager: ZwpTabletManagerV2,
    /// Gives each tool its cursor over the window, where the compositor has the protocol.
    shapes: Option<WpCursorShapeManagerV1>,
}

impl TabletManager {
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        // Version 1: the later versions only add pad dials and button bindings, unused here.
        let manager = globals.bind(queue_handle, 1..=1, GlobalData)?;
        let shapes = globals.bind(queue_handle, 1..=1, GlobalData).ok();
        Ok(Self { manager, shapes })
    }

    /// The tablets, tools and pads of `seat`; its tools are announced on it as they appear.
    pub fn get_tablet_seat(
        &self,
        seat: &WlSeat,
        queue_handle: &QueueHandle<WinitState>,
    ) -> ZwpTabletSeatV2 {
        self.manager.get_tablet_seat(seat, queue_handle, seat.clone())
    }
}

/// The state of one tool, kept until its frame event delivers what it did.
#[derive(Debug, Default)]
pub struct TabletToolData {
    inner: Mutex<ToolState>,
}

#[derive(Debug, Default)]
struct ToolState {
    /// The seat the tool belongs to, for the window move it may start.
    seat: Option<WlSeat>,
    /// Sets the tool's cursor while it is over the window.
    shape: Option<WpCursorShapeDeviceV1>,
    /// The surface the tool is over, from proximity-in to proximity-out.
    surface: Option<WlSurface>,
    /// The last position on that surface, in logical pixels.
    position: LogicalPosition<f64>,
    /// What happened since the last frame, in order.
    pending: Vec<Pending>,
}

#[derive(Debug, Clone)]
enum Pending {
    Entered,
    /// Each motion keeps where it was, so a frame with several delivers each one, not only the last.
    Moved(LogicalPosition<f64>),
    Button { button: MouseButton, state: ElementState, serial: Option<u32> },
    Left,
}

impl Dispatch<ZwpTabletManagerV2, GlobalData, WinitState> for TabletManager {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletManagerV2,
        _: <ZwpTabletManagerV2 as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        unreachable!("no events defined for zwp_tablet_manager_v2");
    }
}

impl Dispatch<ZwpTabletSeatV2, WlSeat, WinitState> for TabletManager {
    event_created_child!(WinitState, ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, TabletToolData::default()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ()),
    ]);

    fn event(
        state: &mut WinitState,
        _: &ZwpTabletSeatV2,
        event: <ZwpTabletSeatV2 as Proxy>::Event,
        seat: &WlSeat,
        _: &Connection,
        queue_handle: &QueueHandle<WinitState>,
    ) {
        if let zwp_tablet_seat_v2::Event::ToolAdded { id } = event {
            if let Some(data) = id.data::<TabletToolData>() {
                let mut data = data.inner.lock().unwrap();
                data.seat = Some(seat.clone());
                data.shape = state
                    .tablet_manager
                    .as_ref()
                    .and_then(|manager| manager.shapes.as_ref())
                    .map(|shapes| shapes.get_tablet_tool_v2(&id, queue_handle, GlobalData));
            }
        }
    }
}

impl Dispatch<ZwpTabletToolV2, TabletToolData, WinitState> for TabletManager {
    fn event(
        state: &mut WinitState,
        tool: &ZwpTabletToolV2,
        event: <ZwpTabletToolV2 as Proxy>::Event,
        data: &TabletToolData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        let mut data = data.inner.lock().unwrap();
        match event {
            ToolEvent::ProximityIn { serial, surface, .. } => {
                // The arrow, like the mouse has over the window, with the serial of this entry.
                if let Some(shape) = &data.shape {
                    shape.set_shape(serial, Shape::Default);
                }
                data.surface = Some(surface);
                data.pending.push(Pending::Entered);
            },
            ToolEvent::Motion { x, y } => {
                data.position = LogicalPosition::new(x, y);
                let at = data.position;
                data.pending.push(Pending::Moved(at));
            },
            ToolEvent::Down { serial } => data.pending.push(Pending::Button {
                button: MouseButton::Left,
                state: ElementState::Pressed,
                serial: Some(serial),
            }),
            ToolEvent::Up => data.pending.push(Pending::Button {
                button: MouseButton::Left,
                state: ElementState::Released,
                serial: None,
            }),
            ToolEvent::Button { serial, button, state: WEnum::Value(pressed) } => {
                // The order GNOME and SDL give the barrel buttons.
                let button = match button {
                    BTN_STYLUS => MouseButton::Right,
                    BTN_STYLUS2 => MouseButton::Middle,
                    BTN_STYLUS3 => MouseButton::Back,
                    _ => return,
                };
                let (state, serial) = match pressed {
                    ButtonState::Pressed => (ElementState::Pressed, Some(serial)),
                    ButtonState::Released => (ElementState::Released, None),
                    _ => return,
                };
                data.pending.push(Pending::Button { button, state, serial });
            },
            ToolEvent::ProximityOut => data.pending.push(Pending::Left),
            ToolEvent::Frame { .. } => deliver(state, &mut data),
            ToolEvent::Removed => {
                if let Some(shape) = data.shape.take() {
                    shape.destroy();
                }
                tool.destroy();
            },
            _ => {},
        }
    }
}

/// Hand what the tool did since its last frame to the window it is over.
fn deliver(state: &mut WinitState, data: &mut ToolState) {
    let device_id = crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));
    for event in std::mem::take(&mut data.pending) {
        let Some(surface) = data.surface.clone() else { continue };
        let window_id = wayland::make_wid(&surface);
        // Only the windows of this connection, not their decorations or other surfaces.
        let Some(window) = state.windows.get_mut().get(&window_id) else {
            if matches!(event, Pending::Left) {
                data.surface = None;
            }
            continue;
        };
        let mut window = window.lock().unwrap();
        let scale_factor = window.scale_factor();
        let event = match event {
            Pending::Entered => WindowEvent::CursorEntered { device_id },
            Pending::Moved(at) => {
                WindowEvent::CursorMoved { device_id, position: at.to_physical(scale_factor) }
            },
            Pending::Button { button, state: pressed, serial } => {
                // A press of the tool is what a window move or resize may follow, like a touch.
                match (serial, data.seat.as_ref()) {
                    (Some(serial), Some(seat)) => window.touch_down = Some((seat.clone(), serial)),
                    _ if pressed == ElementState::Released => window.touch_down = None,
                    _ => {},
                }
                WindowEvent::MouseInput { device_id, state: pressed, button }
            },
            Pending::Left => {
                window.touch_down = None;
                data.surface = None;
                WindowEvent::CursorLeft { device_id }
            },
        };
        drop(window);
        state.events_sink.push_window_event(event, window_id);
    }
}

impl Dispatch<ZwpTabletV2, (), WinitState> for TabletManager {
    fn event(
        _: &mut WinitState,
        tablet: &ZwpTabletV2,
        event: <ZwpTabletV2 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        if let sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_v2::Event::Removed =
            event
        {
            tablet.destroy();
        }
    }
}

impl Dispatch<ZwpTabletPadV2, (), WinitState> for TabletManager {
    event_created_child!(WinitState, ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, ()),
    ]);

    fn event(
        _: &mut WinitState,
        pad: &ZwpTabletPadV2,
        event: <ZwpTabletPadV2 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        if let zwp_tablet_pad_v2::Event::Removed = event {
            pad.destroy();
        }
    }
}

impl Dispatch<ZwpTabletPadGroupV2, (), WinitState> for TabletManager {
    event_created_child!(WinitState, ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, ()),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, ()),
    ]);

    fn event(
        _: &mut WinitState,
        _: &ZwpTabletPadGroupV2,
        _: <ZwpTabletPadGroupV2 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch<ZwpTabletPadRingV2, (), WinitState> for TabletManager {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletPadRingV2,
        _: <ZwpTabletPadRingV2 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch<ZwpTabletPadStripV2, (), WinitState> for TabletManager {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletPadStripV2,
        _: <ZwpTabletPadStripV2 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

delegate_dispatch!(WinitState: [ZwpTabletManagerV2: GlobalData] => TabletManager);
delegate_dispatch!(WinitState: [ZwpTabletSeatV2: WlSeat] => TabletManager);
delegate_dispatch!(WinitState: [ZwpTabletToolV2: TabletToolData] => TabletManager);
delegate_dispatch!(WinitState: [ZwpTabletV2: ()] => TabletManager);
delegate_dispatch!(WinitState: [ZwpTabletPadV2: ()] => TabletManager);
delegate_dispatch!(WinitState: [ZwpTabletPadGroupV2: ()] => TabletManager);
delegate_dispatch!(WinitState: [ZwpTabletPadRingV2: ()] => TabletManager);
delegate_dispatch!(WinitState: [ZwpTabletPadStripV2: ()] => TabletManager);
