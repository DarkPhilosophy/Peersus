//! Handling of wp_tablet_input_v2.

use std::sync::Mutex;

use dpi::LogicalPosition;
use sctk::compositor::SurfaceData;
use sctk::dispatch2::Dispatch2;
use sctk::globals::GlobalData;
use sctk::reexports::client::backend::smallvec::SmallVec;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Proxy, QueueHandle, WEnum, event_created_child};
use sctk::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use sctk::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_manager_v2::ZwpTabletManagerV2;
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_pad_v2::ZwpTabletPadV2;
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_seat_v2::{
    self, ZwpTabletSeatV2,
};
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_tool_v2::{
    ButtonState, Event as ToolEvent, Type as ToolType, ZwpTabletToolV2,
};
use sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_v2::ZwpTabletV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_dial_v2::ZwpTabletPadDialV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_group_v2::{
    self, ZwpTabletPadGroupV2,
};
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_v2;
use winit_core::event::{
    ButtonSource, ElementState, Force, PointerKind, PointerSource, TabletToolButton,
    TabletToolData as CoreTabletToolData, TabletToolKind, TabletToolTilt, WindowEvent,
};

use crate::state::WinitState;

/// The tablet manager and optional cursor shape protocol.
#[derive(Debug, Clone)]
pub struct TabletManager {
    manager: ZwpTabletManagerV2,
    shapes: Option<WpCursorShapeManagerV1>,
}

impl TabletManager {
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        // Ignore v2 since we are not interested in its events.
        let manager = globals.bind(queue_handle, 1..=1, ())?;
        let shapes = globals.bind(queue_handle, 1..=1, GlobalData).ok();
        Ok(Self { manager, shapes })
    }

    pub fn get_tablet_seat(
        &self,
        seat: &WlSeat,
        queue_handle: &QueueHandle<WinitState>,
    ) -> ZwpTabletSeatV2 {
        self.manager.get_tablet_seat(seat, queue_handle, TabletSeat(seat.clone()))
    }
}
impl Dispatch2<ZwpTabletManagerV2, WinitState> for () {
    fn event(
        &self,
        _: &mut WinitState,
        _: &ZwpTabletManagerV2,
        _: <ZwpTabletManagerV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        unreachable!("no events defined for zwp_tablet_manager_v2");
    }
}

struct TabletSeat(WlSeat);

impl Dispatch2<ZwpTabletSeatV2, WinitState> for TabletSeat {
    event_created_child!(WinitState, ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, TabletToolData::default()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ())
    ]);

    fn event(
        &self,
        state: &mut WinitState,
        _: &ZwpTabletSeatV2,
        event: <ZwpTabletSeatV2 as Proxy>::Event,
        _: &Connection,
        queue_handle: &QueueHandle<WinitState>,
    ) {
        if let zwp_tablet_seat_v2::Event::ToolAdded { id } = event
            && let Some(data) = id.data::<TabletToolData>()
        {
            let mut data = data.inner.lock().unwrap();
            data.seat = Some(self.0.clone());
            data.shape = state
                .tablet_state
                .as_ref()
                .and_then(|manager| manager.shapes.as_ref())
                .map(|shapes| shapes.get_tablet_tool_v2(&id, queue_handle, GlobalData));
        }
    }
}

impl Dispatch2<ZwpTabletToolV2, WinitState> for TabletToolData {
    fn event(
        &self,
        state: &mut WinitState,
        tool: &ZwpTabletToolV2,
        event: <ZwpTabletToolV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        let mut data = self.inner.lock().unwrap();

        match event {
            ToolEvent::Type { tool_type: WEnum::Value(tool_type) } => {
                data.ty = match tool_type {
                    ToolType::Pen => TabletToolKind::Pen,
                    ToolType::Eraser => TabletToolKind::Eraser,
                    ToolType::Brush => TabletToolKind::Brush,
                    ToolType::Pencil => TabletToolKind::Pencil,
                    ToolType::Airbrush => TabletToolKind::Airbrush,
                    ToolType::Finger => TabletToolKind::Finger,
                    ToolType::Mouse => TabletToolKind::Mouse,
                    ToolType::Lens => TabletToolKind::Lens,
                    _ => return,
                };
            },
            ToolEvent::Capability { .. } => {},
            ToolEvent::Done => (),
            ToolEvent::ProximityIn { serial, surface, .. } => {
                data.pending.push(TabletEvent::Enter { serial, surface });
            },
            ToolEvent::ProximityOut => data.pending.push(TabletEvent::Left),
            ToolEvent::Down { serial } => {
                let event = TabletEvent::Button {
                    state: ElementState::Pressed,
                    serial: Some(serial),
                    button: TabletToolButton::Contact,
                };

                data.pending.push(event);
            },
            ToolEvent::Up => {
                let event = TabletEvent::Button {
                    state: ElementState::Released,
                    serial: None,
                    button: TabletToolButton::Contact,
                };

                data.pending.push(event);
            },
            ToolEvent::Tilt { tilt_x, tilt_y } => {
                data.tool_state.tilt = Some(TabletToolTilt { x: tilt_x as i8, y: tilt_y as i8 });
            },
            ToolEvent::Motion { x, y } => {
                data.position = (x, y).into();
                let location = data.position;
                data.pending.push(TabletEvent::Moved(location));
            },
            ToolEvent::Pressure { pressure } => {
                data.tool_state.force = Some(Force::Normalized(pressure as f64 / u16::MAX as f64));
            },
            ToolEvent::Rotation { degrees } => {
                data.tool_state.twist = Some(degrees as u16);
            },
            ToolEvent::Button { serial, button, state: WEnum::Value(state) } => {
                let state = match state {
                    ButtonState::Released => ElementState::Released,
                    ButtonState::Pressed => ElementState::Pressed,
                    _ => return,
                };

                // Down/Up report the tip. The three side buttons remain right, middle and back.
                let button = match button {
                    0x14b => TabletToolButton::Barrel,   // BTN_STYLUS
                    0x14c => TabletToolButton::Other(1), // BTN_STYLUS2
                    0x149 => TabletToolButton::Other(3), // BTN_STYLUS3
                    // There's no defined conversion for any of that.
                    button => TabletToolButton::Other(button as u16),
                };

                let event = TabletEvent::Button {
                    button,
                    state,
                    serial: state.is_pressed().then_some(serial),
                };
                data.pending.push(event);
            },
            ToolEvent::Frame { .. } => {
                let kind = data.ty;
                for event in std::mem::take(&mut data.pending) {
                    if let TabletEvent::Enter { surface, serial } = &event {
                        if let Some(shape) = &data.shape {
                            shape.set_shape(*serial, Shape::Default);
                        }
                        data.surface = Some(surface.clone());
                    }

                    // Handle events only for top-level surface.
                    let surface = match data
                        .surface
                        .as_ref()
                        .map(|surface| (surface, surface.data::<SurfaceData<()>>()))
                    {
                        Some((surface, Some(surface_data)))
                            if surface_data.parent_surface().is_none() =>
                        {
                            surface
                        },
                        _ => continue,
                    };

                    let window_id = crate::make_wid(surface);

                    // Ensure that window exists.
                    let mut window = match state.windows.get_mut().get_mut(&window_id) {
                        Some(window) => window.lock().unwrap(),
                        None => continue,
                    };

                    let position = data.position.to_physical(window.scale_factor());

                    let window_event = match event {
                        TabletEvent::Enter { .. } => WindowEvent::PointerEntered {
                            device_id: None,
                            position,
                            primary: true,
                            kind: PointerKind::TabletTool(kind),
                        },
                        TabletEvent::Moved(location) => WindowEvent::PointerMoved {
                            device_id: None,
                            position: location.to_physical(window.scale_factor()),
                            primary: true,
                            source: PointerSource::TabletTool {
                                kind,
                                data: data.tool_state.clone(),
                            },
                        },
                        TabletEvent::Button { button, state, serial } => {
                            if let (Some(seat), Some(serial)) = (data.seat.as_ref(), serial) {
                                window.tablet_down = Some((seat.clone(), serial));
                                data.latest_button_serial = Some(serial);
                            } else if state == ElementState::Released
                                && let Some(serial) = data.latest_button_serial.take()
                                && window.tablet_down.as_ref().is_some_and(|(_, active)| *active == serial)
                            {
                                window.tablet_down = None;
                            }

                            WindowEvent::PointerButton {
                                device_id: None,
                                state,
                                position,
                                primary: true,
                                button: ButtonSource::TabletTool {
                                    kind,
                                    button,
                                    data: data.tool_state.clone(),
                                },
                                is_macos_activation_click: false,
                            }
                        },
                        TabletEvent::Left => WindowEvent::PointerLeft {
                            device_id: None,
                            position: Some(position),
                            primary: true,
                            kind: PointerKind::TabletTool(kind),
                        },
                    };

                    state.events_sink.push_window_event(window_event, window_id);

                    // Clear up the surface after we've processed `Left` event.
                    if matches!(event, TabletEvent::Left) {
                        if let Some(serial) = data.latest_button_serial.take()
                            && window.tablet_down.as_ref().is_some_and(|(_, active)| *active == serial)
                        {
                            window.tablet_down = None;
                        }
                        data.surface = None;
                        data.tool_state = Default::default();
                    }
                }
            },
            ToolEvent::Removed => {
                if let Some(shape) = data.shape.take() {
                    shape.destroy();
                }
                tool.destroy();
            },
            _ => (),
        }
    }
}

#[derive(Debug, Default)]
struct TabletToolData {
    inner: Mutex<TabletToolDataInner>,
}

#[derive(Debug, Default)]
pub(crate) struct TabletToolDataInner {
    pub(crate) ty: TabletToolKind,

    /// Core tablet tool data.
    pub(crate) tool_state: CoreTabletToolData,

    /// Pending events until the `frame` is received.
    pub(crate) pending: SmallVec<[TabletEvent; 4]>,

    /// Surface the tablet most recently entered.
    pub(crate) surface: Option<WlSurface>,

    /// Position relative to the surface.
    pub(crate) position: LogicalPosition<f64>,

    seat: Option<WlSeat>,
    shape: Option<WpCursorShapeDeviceV1>,

    /// The serial of the button that began the current implicit grab.
    pub(crate) latest_button_serial: Option<u32>,
}

// Due to wayland using logical coordinates,
// delay the conversion to physical until the dispatch actually happens,
// so the scaling is applied at the time of actual dispatch, since it
// can technically change before the `frame` event.
#[derive(Debug, Clone)]
pub(crate) enum TabletEvent {
    Enter { serial: u32, surface: WlSurface },
    Left,
    Moved(LogicalPosition<f64>),
    Button { button: TabletToolButton, state: ElementState, serial: Option<u32> },
}

impl Dispatch2<ZwpTabletV2, WinitState> for () {
    fn event(
        &self,
        _: &mut WinitState,
        _: &ZwpTabletV2,
        _: <ZwpTabletV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch2<ZwpTabletPadV2, WinitState> for () {
    event_created_child!(WinitState, ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, ()),
    ]);

    fn event(
        &self,
        _: &mut WinitState,
        _: &ZwpTabletPadV2,
        _: <ZwpTabletPadV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch2<ZwpTabletPadGroupV2, WinitState> for () {
    event_created_child!(WinitState, ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, ()),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, ()),
        zwp_tablet_pad_group_v2::EVT_DIAL_OPCODE => (ZwpTabletPadDialV2, ()),
    ]);

    fn event(
        &self,
        _: &mut WinitState,
        _: &ZwpTabletPadGroupV2,
        _: <ZwpTabletPadGroupV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch2<ZwpTabletPadRingV2, WinitState> for () {
    fn event(
        &self,
        _: &mut WinitState,
        _: &ZwpTabletPadRingV2,
        _: <ZwpTabletPadRingV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch2<ZwpTabletPadStripV2, WinitState> for () {
    fn event(
        &self,
        _: &mut WinitState,
        _: &ZwpTabletPadStripV2,
        _: <ZwpTabletPadStripV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch2<ZwpTabletPadDialV2, WinitState> for () {
    fn event(
        &self,
        _: &mut WinitState,
        _: &ZwpTabletPadDialV2,
        _: <ZwpTabletPadDialV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}
