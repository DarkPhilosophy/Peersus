//! `peersus.input`: pointer and keyboard input replayed on this computer for an extension that
//! holds the person's permission, in the typed form every frontend sends ([`PointerInput`]).
//!
//! The extension never reaches a device: it hands plain data to this service, which owns the one
//! virtual device and decides what each input becomes. Whatever an extension holds down is
//! released when it lets go, ends or loses its permission: a key left pressed by a vanished
//! controller would keep typing on this computer.
//!
//! On Linux the device is a uinput mouse and keyboard, which every desktop (X11, any Wayland
//! compositor, the console) reads like a plugged-in one. Typed text is struck as keys of a US
//! layout; characters that layout has no key for are not typed.
use crate::platform_services::{Call, Response};
use peersus_api::{
    contributions::{Button, Key, MAX_POINTER_BATCH, PointerInput},
    message::Outcome,
};
use peersus_core::plugins::PluginId;
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
};

#[cfg(target_os = "linux")]
mod cursor;

/// Linux input event types and codes (`linux/input-event-codes.h`): the language of the device.
mod code {
    pub const EV_KEY: u16 = 1;
    pub const EV_REL: u16 = 2;
    pub const REL_X: u16 = 0;
    pub const REL_Y: u16 = 1;
    pub const REL_HWHEEL: u16 = 6;
    pub const REL_WHEEL: u16 = 8;
    pub const REL_WHEEL_HI_RES: u16 = 11;
    pub const REL_HWHEEL_HI_RES: u16 = 12;
    pub const BTN_LEFT: u16 = 0x110;
    pub const BTN_RIGHT: u16 = 0x111;
    pub const BTN_MIDDLE: u16 = 0x112;
    pub const KEY_ESC: u16 = 1;
    pub const KEY_BACKSPACE: u16 = 14;
    pub const KEY_TAB: u16 = 15;
    pub const KEY_ENTER: u16 = 28;
    pub const KEY_LEFTSHIFT: u16 = 42;
    pub const KEY_SPACE: u16 = 57;
    pub const KEY_UP: u16 = 103;
    pub const KEY_LEFT: u16 = 105;
    pub const KEY_RIGHT: u16 = 106;
    pub const KEY_DOWN: u16 = 108;
    pub const EV_ABS: u16 = 3;
    pub const ABS_X: u16 = 0;
    pub const ABS_Y: u16 = 1;
    /// The largest absolute coordinate: a position is a share of the desktop in this many steps.
    pub const ABS_MAX: i32 = 65_535;
    // A pen tablet: what a device's own pointer is made of (see `Pen`).
    pub const BTN_TOOL_PEN: u16 = 0x140;
    pub const BTN_TOUCH: u16 = 0x14a;
    pub const BTN_STYLUS: u16 = 0x14b;
    pub const BTN_STYLUS2: u16 = 0x14c;
}

/// Whose input it is: an extension, and the device it replays for (`None` when it names none).
/// What one holds is let go without touching what another holds.
type Owner = (PluginId, Option<String>);

/// Longest name a device may be replayed for.
const MOST_OWNER: usize = 64;
/// How long a compositor needs to find and open a new device: what is sent before is lost, including
/// what makes its pointer appear, after which the pointer is ignored for good.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(600);
/// A device's own pointer with nothing pressed that has not moved for this long is taken off the
/// screen, as a resting mouse pointer is by many desktops: a pointer left standing looks stuck.
/// Leaving the page alone does not take it away (see `release`); this pause does. A desktop that
/// forgets the cursor of a tool that left draws it again over windows that ask for one (Peersus
/// itself, through its patched winit, and toolkit applications such as GTK).
const IDLE_HIDE: std::time::Duration = std::time::Duration::from_secs(3);
/// The desktop's size when the system does not say it.
const DEFAULT_AREA: (i32, i32) = (1920, 1080);

/// One event for the device: its type, code and value.
pub type Raw = (u16, u16, i32);

/// Where events go. One real device; a recording one under test.
pub trait Device: Send {
    /// Emit one group of events, which the system reads as happening together.
    fn emit(&mut self, events: &[Raw]) -> std::io::Result<()>;
}

/// Makes the device the first time something is replayed, so a computer nobody controls never has
/// one, and a device that could not be made is tried again on the next input.
pub type Factory = Box<dyn Fn() -> std::io::Result<Box<dyn Device>> + Send + Sync>;

/// Makes a device's own pointer: a pen tablet named for it, whose range is the desktop in pixels.
/// The second argument is the name tag the application decided for it, if any: drawn beside the pointer.
pub type PenFactory =
    Box<dyn Fn(&str, Option<&str>, (i32, i32)) -> std::io::Result<Box<dyn Device>> + Send + Sync>;

/// A device's own pointer on this desktop.
///
/// A desktop that gives each pen-tablet tool a pointer of its own (GNOME does) draws one for it,
/// apart from this computer's mouse: two people move two pointers. Motion is added to where it is,
/// so a phone's touchpad moves it as it moves the mouse; a position places it outright.
struct Pen {
    device: Box<dyn Device>,
    area: (i32, i32),
    position: (f64, f64),
    near: bool,
    held: BTreeSet<u16>,
    /// Until then only where it is is followed: see [`SETTLE`].
    ready_at: std::time::Instant,
    /// When it last moved or clicked: see [`IDLE_HIDE`].
    last: std::time::Instant,
    /// Clicks made while it settles, in order: given the moment it is ready, never dropped.
    pending: Vec<(u16, bool)>,
    /// Motion or clicks received before discovery still need their first frame.
    waiting: bool,
    /// The name tag drawn beside it, as the application decided it.
    label: Option<String>,
}

impl Pen {
    fn new(device: Box<dyn Device>, area: (i32, i32), settle: std::time::Duration) -> Self {
        Self {
            device,
            area,
            position: (f64::from(area.0) / 2., f64::from(area.1) / 2.),
            near: false,
            held: BTreeSet::new(),
            ready_at: std::time::Instant::now() + settle,
            last: std::time::Instant::now(),
            pending: Vec::new(),
            waiting: false,
            label: None,
        }
    }

    fn place(&mut self, x: f64, y: f64) {
        let most = (f64::from(self.area.0 - 1), f64::from(self.area.1 - 1));
        self.position = (x.clamp(0., most.0), y.clamp(0., most.1));
    }

    /// The frames `event` makes on the pen, or `None` for input that is not the pointer's (keys,
    /// text, the wheel), which goes to the shared keyboard and mouse.
    fn translate(&mut self, event: &PointerInput) -> Option<Vec<Vec<Raw>>> {
        let now = std::time::Instant::now();
        let ready = now >= self.ready_at;
        match event {
            PointerInput::Move { dx, dy } => {
                self.place(
                    self.position.0 + f64::from(*dx),
                    self.position.1 + f64::from(*dy),
                );
            }
            PointerInput::Position { x, y } => self.place(
                f64::from(*x) * f64::from(self.area.0 - 1),
                f64::from(*y) * f64::from(self.area.1 - 1),
            ),
            PointerInput::Button { button, down } => {
                // The tip is the left button; on GNOME the first barrel button arrives as the right
                // click and the second as the middle one.
                let tool = match button {
                    Button::Left => code::BTN_TOUCH,
                    Button::Right => code::BTN_STYLUS,
                    Button::Middle => code::BTN_STYLUS2,
                };
                self.pending.push((tool, *down));
            }
            _ => return None,
        }
        self.last = now;
        self.waiting = true;
        if !ready {
            return Some(Vec::new());
        }
        Some(self.frames())
    }

    /// What is owed to the device now that it is ready: being in reach, the clicks waiting, and
    /// where it is. Every click is its own frame, at the position it was made.
    fn frames(&mut self) -> Vec<Vec<Raw>> {
        self.waiting = false;
        let mut frames = Vec::new();
        let (x, y) = (self.position.0 as i32, self.position.1 as i32);
        if !self.near {
            // Entry is beside the actual position, so the next frame changes an axis even when
            // returning to the same point. Cursor drawing belongs to the device's renderer; never
            // move input over unrelated desktop controls just to initialize its appearance.
            self.near = true;
            let nudged = if x > 0 { x - 1 } else { x + 1 };
            frames.push(vec![
                (code::EV_ABS, code::ABS_X, nudged),
                (code::EV_ABS, code::ABS_Y, y),
                (code::EV_KEY, code::BTN_TOOL_PEN, 1),
            ]);
            frames.push(vec![(code::EV_KEY, code::BTN_TOOL_PEN, 0)]);
            frames.push(vec![(code::EV_KEY, code::BTN_TOOL_PEN, 1)]);
        }
        let mut frame = vec![
            (code::EV_ABS, code::ABS_X, x),
            (code::EV_ABS, code::ABS_Y, y),
        ];
        for (tool, down) in std::mem::take(&mut self.pending) {
            let changed = if down {
                self.held.insert(tool)
            } else {
                self.held.remove(&tool)
            };
            if !changed {
                continue;
            }
            if frame.iter().any(|&(kind, _, _)| kind == code::EV_KEY) {
                // A press and its release in one frame would be no click at all.
                frames.push(std::mem::replace(
                    &mut frame,
                    vec![(code::EV_ABS, code::ABS_X, x), (code::EV_ABS, code::ABS_Y, y)],
                ));
            }
            frame.push((code::EV_KEY, tool, i32::from(down)));
        }
        frames.push(frame);
        frames
    }

    /// A first gesture received during discovery is delivered even if it already ended.
    fn settled(&mut self, now: std::time::Instant) -> Option<Vec<Vec<Raw>>> {
        (now >= self.ready_at && self.waiting).then(|| self.frames())
    }

    /// Let go of what is pressed but stay in reach: a gesture ended, the person is still there.
    fn lift(&mut self) -> Vec<Raw> {
        // Preserve completed clicks queued during discovery. An unfinished queued press is
        // completed as well, so discovery can never introduce a stuck button after rest.
        let mut queued = BTreeSet::new();
        for &(tool, down) in &self.pending {
            if down { queued.insert(tool); } else { queued.remove(&tool); }
        }
        self.pending.extend(queued.into_iter().map(|tool| (tool, false)));
        std::mem::take(&mut self.held)
            .into_iter()
            .map(|tool| (code::EV_KEY, tool, 0))
            .collect()
    }

    /// Hide the tool without destroying its device, so returning input needs no discovery delay.
    fn leave(&mut self) -> Vec<Raw> {
        self.waiting = false;
        self.pending.clear();
        let mut events: Vec<_> = std::mem::take(&mut self.held)
            .into_iter()
            .map(|tool| (code::EV_KEY, tool, 0))
            .collect();
        if std::mem::take(&mut self.near) {
            events.push((code::EV_KEY, code::BTN_TOOL_PEN, 0));
        }
        events
    }
}

/// What one extension has done that is still in effect.
#[derive(Default)]
struct Caller {
    /// Fractions of a pixel not yet moved: slow motion is many small ones, and must add up.
    remainder: (f32, f32),
    /// Buttons this extension pressed and has not released.
    held: BTreeSet<u16>,
}

struct State {
    device: Option<Box<dyn Device>>,
    callers: HashMap<Owner, Caller>,
    pens: HashMap<Owner, Pen>,
    /// The desktop's size in pixels, as last read: what a pen's range covers.
    area: Option<(i32, i32)>,
    /// Whether the task that hides idle pointers runs.
    watching: bool,
}

#[derive(Clone)]
pub struct DesktopInput {
    factory: Arc<Factory>,
    pens: Arc<PenFactory>,
    settle: std::time::Duration,
    state: Arc<Mutex<State>>,
}

impl DesktopInput {
    pub fn new(factory: Factory) -> Self {
        Self {
            factory: Arc::new(factory),
            pens: Arc::new(Box::new(|_, _, _| {
                Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
            })),
            settle: SETTLE,
            state: Arc::new(Mutex::new(State {
                device: None,
                callers: HashMap::new(),
                pens: HashMap::new(),
                area: None,
                watching: false,
            })),
        }
    }

    /// Devices' own pointers made by `pens`, sent to once `settle` has passed.
    pub fn with_pens(mut self, pens: PenFactory, settle: std::time::Duration) -> Self {
        self.pens = Arc::new(pens);
        self.settle = settle;
        self
    }

    /// The system's own device on Linux: uinput.
    #[cfg(target_os = "linux")]
    pub fn system() -> Self {
        Self::new(Box::new(|| {
            uinput::open().map(|d| Box::new(d) as Box<dyn Device>)
        }))
        .with_pens(
            Box::new(|owner, label, area| {
                uinput::pen(owner, label, area).map(|d| Box::new(d) as Box<dyn Device>)
            }),
            SETTLE,
        )
    }

    /// No device this application can make elsewhere yet: every replay says so.
    #[cfg(not(target_os = "linux"))]
    pub fn system() -> Self {
        Self::new(Box::new(|| {
            Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
        }))
    }

    pub fn dispatch(&self, call: Call) -> Response {
        let this = self.clone();
        Box::pin(async move {
            // A device about to get a pointer of its own: the desktop's size is read first, outside
            // the lock, so the pen covers the desktop as it is now.
            if call.args["pointer"] == "own" {
                let owner = owner_of(&call.caller, &call.args);
                let known = this
                    .state
                    .lock()
                    .is_ok_and(|state| owner.as_ref().is_some_and(|o| state.pens.contains_key(o)));
                if !known {
                    let area = desktop_area().await;
                    if let Ok(mut state) = this.state.lock() {
                        state.area = Some(area);
                    }
                }
            }
            if call.args["pointer"] == "own" {
                this.watch();
            }
            // The device is written synchronously and briefly: a few small writes per batch.
            let outcome = match call.method.as_str() {
                "inject" => this.inject(&call.caller, &call.args),
                "release" => owner_of(&call.caller, &call.args)
                    .ok_or("The input was not understood")
                    .and_then(|owner| {
                        if call.args["keep"] == true {
                            this.rest(&owner)
                        } else {
                            this.release(&owner)
                        }
                    })
                    .map(|()| serde_json::json!({"released":true})),
                _ => Err("Unknown input method"),
            };
            match outcome {
                Ok(value) => Outcome::Ok { value },
                Err(message) => Outcome::Error {
                    code: "failed".into(),
                    message: message.into(),
                },
            }
        })
    }

    fn inject(
        &self,
        caller: &PluginId,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, &'static str> {
        const REFUSED: &str = "The input was not understood";
        const NO_PEN: &str = "This computer cannot give that device a pointer of its own";
        let owner = owner_of(caller, args).ok_or(REFUSED)?;
        let own = match args.get("pointer") {
            None => false,
            Some(pointer) if pointer == "own" && owner.1.is_some() => true,
            Some(_) => return Err(REFUSED),
        };
        let events: Vec<PointerInput> =
            serde_json::from_value(args.get("events").cloned().ok_or(REFUSED)?)
                .map_err(|_| REFUSED)?;
        if events.is_empty() || events.len() > MAX_POINTER_BATCH {
            return Err(REFUSED);
        }
        // All of a batch is checked before any of it is replayed: half a gesture is worse than none.
        for event in &events {
            event.validate().map_err(|_| REFUSED)?;
        }
        let mut state = self.state.lock().map_err(|_| "Input is unavailable")?;
        // Only the application writes `label` (see the runtime); a pointer whose tag changed is made again with the new one.
        let label = args.get("label").and_then(|label| label.as_str()).map(str::to_owned);
        if own && state.pens.get(&owner).is_some_and(|pen| pen.label != label) {
            state.pens.remove(&owner);
        }
        if own && !state.pens.contains_key(&owner) {
            let area = state.area.unwrap_or(DEFAULT_AREA);
            let name = owner.1.as_deref().unwrap_or_default();
            let device = (self.pens)(name, label.as_deref(), area).map_err(|_| NO_PEN)?;
            let mut pen = Pen::new(device, area, self.settle);
            pen.label = label;
            state.pens.insert(owner.clone(), pen);
        }
        let mut skipped = 0usize;
        // In order: what is the pen's goes to the pen, the rest to the shared keyboard and mouse.
        let mut shared = Vec::new();
        for event in &events {
            let frames = if own {
                state
                    .pens
                    .get_mut(&owner)
                    .and_then(|pen| pen.translate(event))
            } else {
                None
            };
            match frames {
                Some(frames) => {
                    if !shared.is_empty() {
                        self.emit(&mut state, &std::mem::take(&mut shared))?;
                    }
                    let Some(pen) = state.pens.get_mut(&owner) else {
                        continue;
                    };
                    if frames.iter().any(|frame| pen.device.emit(frame).is_err()) {
                        // A pen that failed is made again on the next input.
                        state.pens.remove(&owner);
                        return Err(NO_PEN);
                    }
                }
                None => {
                    let caller = state.callers.entry(owner.clone()).or_default();
                    skipped += translate(event, caller, &mut shared);
                }
            }
        }
        if !shared.is_empty() {
            self.emit(&mut state, &shared)?;
        }
        Ok(serde_json::json!({"applied": events.len(), "untyped": skipped}))
    }

    /// A gesture of this controller ended: let go of what it holds, but leave its pointer where it
    /// is, to be hidden only once it has stood still for [`IDLE_HIDE`].
    fn rest(&self, owner: &Owner) -> Result<(), &'static str> {
        let mut state = self.state.lock().map_err(|_| "Input is unavailable")?;
        if let Some(pen) = state.pens.get_mut(owner) {
            let up = pen.lift();
            if !up.is_empty() {
                let _ = pen.device.emit(&up);
            }
            pen.last = std::time::Instant::now();
        }
        // What it pressed on the shared keyboard and mouse is let go exactly as at its end.
        let Some(gone) = state.callers.remove(owner) else { return Ok(()) };
        let raw: Vec<Raw> = gone.held.iter().map(|&b| (code::EV_KEY, b, 0)).collect();
        if raw.is_empty() {
            return Ok(());
        }
        self.emit(&mut state, &raw)
    }

    /// This controller left (its page closed, its app went to the background): let go of everything
    /// it holds, on its own pointer and on the shared keyboard and mouse. Its pointer stays where
    /// it is and in reach: the desktop forgets the cursor of a tool that leaves and draws it again
    /// only over a window that asks for one, so leaving would make the pointer vanish for good the
    /// next time this device controls this computer. A pointer whose controller does not come
    /// back is taken off the screen by the idle watch (see [`IDLE_HIDE`]).
    fn release(&self, owner: &Owner) -> Result<(), &'static str> {
        let mut state = self.state.lock().map_err(|_| "Input is unavailable")?;
        if let Some(pen) = state.pens.get_mut(owner) {
            let up = pen.lift();
            if !up.is_empty() {
                let _ = pen.device.emit(&up);
            }
            pen.last = std::time::Instant::now();
        }
        let Some(gone) = state.callers.remove(owner) else {
            return Ok(());
        };
        let raw: Vec<Raw> = gone
            .held
            .iter()
            .map(|&button| (code::EV_KEY, button, 0))
            .collect();
        if raw.is_empty() {
            return Ok(());
        }
        self.emit(&mut state, &raw)
    }

    fn emit(&self, state: &mut State, raw: &[Raw]) -> Result<(), &'static str> {
        const UNAVAILABLE: &str =
            "This computer cannot be controlled: no input device could be made";
        if state.device.is_none() {
            state.device = Some((self.factory)().map_err(|_| UNAVAILABLE)?);
        }
        let device = state.device.as_mut().ok_or(UNAVAILABLE)?;
        if device.emit(raw).is_err() {
            // A device that failed is made again next time rather than written to for ever.
            state.device = None;
            return Err(UNAVAILABLE);
        }
        Ok(())
    }

    /// Give each device's own pointer the clicks it was handed while it settled, and take every one
    /// that has stood still for [`IDLE_HIDE`] off the screen, unless something is pressed: a held
    /// drag is the person's, however long it waits.
    fn hide_idle(state: &Mutex<State>, now: std::time::Instant) {
        let Ok(mut state) = state.lock() else { return };
        for pen in state.pens.values_mut() {
            if let Some(frames) = pen.settled(now) {
                for frame in frames {
                    let _ = pen.device.emit(&frame);
                }
            }
            if pen.near && pen.held.is_empty() && now.saturating_duration_since(pen.last) >= IDLE_HIDE {
                let up = pen.leave();
                let _ = pen.device.emit(&up);
            }
        }
    }


    /// Start, once, the task that hides idle pointers. It ends with the service.
    fn watch(&self) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
        let Ok(mut state) = self.state.lock() else { return };
        if std::mem::replace(&mut state.watching, true) {
            return;
        }
        let weak = Arc::downgrade(&self.state);
        handle.spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let Some(state) = weak.upgrade() else { return };
                Self::hide_idle(&state, std::time::Instant::now());
            }
        });
    }

    /// An extension's generation ended: whatever it held, for every device, is let go.
    pub fn revoke(&self, caller: &PluginId) {
        let owners: Vec<Owner> = self
            .state
            .lock()
            .map(|state| {
                state
                    .callers
                    .keys()
                    .chain(state.pens.keys())
                    .filter(|(plugin, _)| plugin == caller)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for owner in owners {
            let _ = self.release(&owner);
            // The extension is gone: so is every pointer it gave a device.
            if let Ok(mut state) = self.state.lock() {
                state.pens.remove(&owner);
            }
        }
    }
}

/// Whose input a call is: the calling extension and the device it names in `owner`, if any. A name
/// that is not short printable text is no name, and the call is refused.
fn owner_of(caller: &PluginId, args: &serde_json::Value) -> Option<Owner> {
    match args.get("owner") {
        None => Some((caller.clone(), None)),
        Some(serde_json::Value::String(name))
            if !name.is_empty()
                && name.chars().count() <= MOST_OWNER
                && !name.chars().any(char::is_control) =>
        {
            Some((caller.clone(), Some(name.clone())))
        }
        Some(_) => None,
    }
}

/// The desktop's size in pixels, which a pen's range covers.
async fn desktop_area() -> (i32, i32) {
    #[cfg(target_os = "linux")]
    {
        let displays = crate::displays::current().await;
        let (w, h) = peersus_core::layout::extent(&displays);
        if let (Ok(w @ 1..), Ok(h @ 1..)) = (i32::try_from(w), i32::try_from(h)) {
            return (w, h);
        }
    }
    DEFAULT_AREA
}

/// What `event` becomes on the device, appended to `raw`. Returns how many characters of typed
/// text had no key and were left out.
fn translate(event: &PointerInput, caller: &mut Caller, raw: &mut Vec<Raw>) -> usize {
    match event {
        PointerInput::Move { dx, dy } => {
            let x = caller.remainder.0 + dx;
            let y = caller.remainder.1 + dy;
            let (whole_x, whole_y) = (x.trunc(), y.trunc());
            caller.remainder = (x - whole_x, y - whole_y);
            if whole_x != 0. {
                raw.push((code::EV_REL, code::REL_X, whole_x as i32));
            }
            if whole_y != 0. {
                raw.push((code::EV_REL, code::REL_Y, whole_y as i32));
            }
            0
        }
        PointerInput::Button { button, down } => {
            let button = match button {
                Button::Left => code::BTN_LEFT,
                Button::Right => code::BTN_RIGHT,
                Button::Middle => code::BTN_MIDDLE,
            };
            // A release of a button not held, or a press of one held, changes nothing: no stray click.
            let changed = if *down {
                caller.held.insert(button)
            } else {
                caller.held.remove(&button)
            };
            if changed {
                raw.push((code::EV_KEY, button, i32::from(*down)));
            }
            0
        }
        PointerInput::Scroll { dx, dy } => {
            // Down is positive in the input and negative on the wheel, as a wheel turned towards you.
            if *dy != 0 {
                raw.push((code::EV_REL, code::REL_WHEEL, -dy));
                raw.push((code::EV_REL, code::REL_WHEEL_HI_RES, -dy * 120));
            }
            if *dx != 0 {
                raw.push((code::EV_REL, code::REL_HWHEEL, *dx));
                raw.push((code::EV_REL, code::REL_HWHEEL_HI_RES, dx * 120));
            }
            0
        }
        PointerInput::Key { key } => {
            strike(raw, key_code(*key), false);
            0
        }
        PointerInput::Position { x, y } => {
            let at = |share: f32| (share.clamp(0., 1.) * code::ABS_MAX as f32).round() as i32;
            raw.push((code::EV_ABS, code::ABS_X, at(*x)));
            raw.push((code::EV_ABS, code::ABS_Y, at(*y)));
            0
        }
        PointerInput::KeyCode { code: key, down } => {
            // Held like a button, so whatever is down when the controller goes is let go.
            let changed = if *down {
                caller.held.insert(*key)
            } else {
                caller.held.remove(key)
            };
            if changed {
                raw.push((code::EV_KEY, *key, i32::from(*down)));
            }
            0
        }
        PointerInput::Text { text } => {
            let mut skipped = 0;
            for ch in text.chars() {
                match us_key(ch) {
                    Some((key, shift)) => strike(raw, key, shift),
                    None => skipped += 1,
                }
            }
            skipped
        }
    }
}

fn key_code(key: Key) -> u16 {
    match key {
        Key::Enter => code::KEY_ENTER,
        Key::Backspace => code::KEY_BACKSPACE,
        Key::Tab => code::KEY_TAB,
        Key::Escape => code::KEY_ESC,
        Key::Left => code::KEY_LEFT,
        Key::Right => code::KEY_RIGHT,
        Key::Up => code::KEY_UP,
        Key::Down => code::KEY_DOWN,
    }
}

/// Press and release one key, with Shift around it when needed.
fn strike(raw: &mut Vec<Raw>, key: u16, shift: bool) {
    if shift {
        raw.push((code::EV_KEY, code::KEY_LEFTSHIFT, 1));
    }
    raw.push((code::EV_KEY, key, 1));
    raw.push((code::EV_KEY, key, 0));
    if shift {
        raw.push((code::EV_KEY, code::KEY_LEFTSHIFT, 0));
    }
}

/// The key, and whether Shift is needed, that types `ch` on a US layout.
fn us_key(ch: char) -> Option<(u16, bool)> {
    const LETTERS: [u16; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17,
        45, 21, 44,
    ];
    const DIGITS: [u16; 10] = [11, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    Some(match ch {
        'a'..='z' => (LETTERS[(ch as u8 - b'a') as usize], false),
        'A'..='Z' => (LETTERS[(ch as u8 - b'A') as usize], true),
        '0'..='9' => (DIGITS[(ch as u8 - b'0') as usize], false),
        ' ' => (code::KEY_SPACE, false),
        '-' => (12, false),
        '_' => (12, true),
        '=' => (13, false),
        '+' => (13, true),
        '[' => (26, false),
        '{' => (26, true),
        ']' => (27, false),
        '}' => (27, true),
        ';' => (39, false),
        ':' => (39, true),
        '\'' => (40, false),
        '"' => (40, true),
        '`' => (41, false),
        '~' => (41, true),
        '\\' => (43, false),
        '|' => (43, true),
        ',' => (51, false),
        '<' => (51, true),
        '.' => (52, false),
        '>' => (52, true),
        '/' => (53, false),
        '?' => (53, true),
        '!' => (2, true),
        '@' => (3, true),
        '#' => (4, true),
        '$' => (5, true),
        '%' => (6, true),
        '^' => (7, true),
        '&' => (8, true),
        '*' => (9, true),
        '(' => (10, true),
        ')' => (11, true),
        _ => return None,
    })
}

#[cfg(target_os = "linux")]
mod uinput {
    use super::{Device, Raw, code};
    use evdev::{
        AbsInfo, AbsoluteAxisCode, AttributeSet, InputEvent, KeyCode, RelativeAxisCode,
        UinputAbsSetup, uinput::VirtualDevice,
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
        let cursor = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            match super::cursor::Cursor::new(area, label) {
                Ok(cursor) => Some(cursor),
                Err(error) => {
                    tracing::warn!(%error, "independent cursor drawing unavailable");
                    None
                }
            }
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
            self.device.emit(&input)?;
            if let Some(cursor) = &mut self.cursor
                && let Err(error) = cursor.emit(events)
            {
                tracing::warn!(%error, "independent cursor drawing stopped");
                self.cursor = None;
            }
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
                    if self.absolute.is_none() {
                        self.absolute = Some(absolute_device()?);
                    }
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_is_a_share_of_the_desktop_and_a_passed_key_is_held_until_released_or_let_go() {
        let (input, recorder, _) = input();
        inject(
            &input,
            "control",
            json!([
                {"t":"position","x":0.5,"y":1.0},
                {"t":"key_code","code":30,"down":true}
            ]),
        )
        .unwrap();
        assert_eq!(
            recorder.0.lock().unwrap()[0],
            vec![
                (code::EV_ABS, code::ABS_X, 32_768),
                (code::EV_ABS, code::ABS_Y, code::ABS_MAX),
                (code::EV_KEY, 30, 1),
            ]
        );
        // The controller goes with the key down: it is let go, not left typing.
        input.revoke(&plugin("control"));
        assert_eq!(
            recorder.0.lock().unwrap().last().unwrap(),
            &vec![(code::EV_KEY, 30, 0)]
        );
        for bad in [
            json!([{"t":"position","x":1.5,"y":0}]),
            json!([{"t":"key_code","code":0x110,"down":true}]),
            json!([{"t":"key_code","code":0,"down":true}]),
        ] {
            assert!(inject(&input, "control", bad).is_err());
        }
    }

    /// Each device's own pointer, by the name it was made for.
    type Pens = Arc<Mutex<Vec<(String, Recorder)>>>;

    /// An input whose devices' own pointers are recorded: each pen by the name it was made for.
    fn with_pens() -> (DesktopInput, Recorder, Pens) {
        let (input, shared, _) = input();
        let pens = Arc::new(Mutex::new(Vec::new()));
        let made = pens.clone();
        let input = input.with_pens(
            Box::new(move |owner, label, area| {
                assert_eq!(area, DEFAULT_AREA);
                let pen = Recorder::default();
                // A tagged pointer is recorded as `owner#tag`, so a test sees which tag it was made with.
                let name = label.map_or_else(|| owner.to_owned(), |label| format!("{owner}#{label}"));
                made.lock().unwrap().push((name, pen.clone()));
                Ok(Box::new(pen) as Box<dyn Device>)
            }),
            std::time::Duration::ZERO,
        );
        (input, shared, pens)
    }

    fn own(input: &DesktopInput, owner: &str, events: serde_json::Value) {
        input
            .inject(
                &plugin("control"),
                &json!({"events": events, "owner": owner, "pointer": "own"}),
            )
            .unwrap();
    }

    #[test]
    fn a_pointer_carries_the_tag_the_application_gave_and_is_made_again_when_it_changes() {
        let (input, _, pens) = with_pens();
        let tagged = |label: Option<&str>| {
            let mut args = json!({"events": [{"t":"move","dx":1,"dy":1}], "owner": "phone", "pointer": "own"});
            if let Some(label) = label {
                args["label"] = json!(label);
            }
            input.inject(&plugin("control"), &args).unwrap();
        };
        tagged(None);
        tagged(None);
        tagged(Some("ROG"));
        tagged(Some("ROG"));
        tagged(None);
        let made: Vec<String> = pens.lock().unwrap().iter().map(|(name, _)| name.clone()).collect();
        // One pointer per tag in effect; the same tag again is the same pointer.
        assert_eq!(made, ["phone", "phone#ROG", "phone"]);
    }

    #[test]
    fn first_motion_changes_position_after_the_tool_enters_proximity() {
        let (input, _, pens) = with_pens();
        own(&input, "phone", json!([{"t":"move","dx":10,"dy":-5}]));
        let recorder = pens.lock().unwrap()[0].1.clone();
        let frames = recorder.0.lock().unwrap();
        let entered = frames.iter().rposition(|frame| {
            frame.contains(&(code::EV_KEY, code::BTN_TOOL_PEN, 1))
        }).unwrap();
        let last_x = |events: &[Vec<Raw>]| events.iter().flatten().rev()
            .find(|event| event.0 == code::EV_ABS && event.1 == code::ABS_X).map(|event| event.2);
        assert_ne!(
            last_x(&frames[..=entered]), Some(970),
            "an unchanged coordinate after proximity-in is dropped by evdev, so no cursor appears"
        );
        assert!(frames[entered + 1..].iter().any(|frame| {
            frame.contains(&(code::EV_ABS, code::ABS_X, 970))
                && frame.contains(&(code::EV_ABS, code::ABS_Y, 535))
        }));
    }

    #[test]
    fn each_device_moves_a_pointer_of_its_own_and_its_keys_still_type() {
        let (input, shared, pens) = with_pens();
        own(&input, "rog", json!([{"t":"move","dx":10,"dy":-5}]));
        own(&input, "hp", json!([{"t":"position","x":0.0,"y":0.0}]));
        let pens = pens.lock().unwrap();
        assert_eq!(pens.len(), 2, "one pointer per device");
        let rog = pens[0].1.0.lock().unwrap().clone();
        assert_eq!(pens[0].0, "rog");
        // Entry stays beside the requested position, never over unrelated desktop controls.
        assert_eq!(
            rog,
            vec![
                vec![
                    (code::EV_ABS, code::ABS_X, 969),
                    (code::EV_ABS, code::ABS_Y, 535),
                    (code::EV_KEY, code::BTN_TOOL_PEN, 1)
                ],
                vec![(code::EV_KEY, code::BTN_TOOL_PEN, 0)],
                vec![(code::EV_KEY, code::BTN_TOOL_PEN, 1)],
                vec![
                    (code::EV_ABS, code::ABS_X, 970),
                    (code::EV_ABS, code::ABS_Y, 535)
                ],
            ]
        );
        let hp = pens[1].1.0.lock().unwrap().clone();
        assert_eq!(
            hp.last().unwrap(),
            &vec![
                (code::EV_ABS, code::ABS_X, 0),
                (code::EV_ABS, code::ABS_Y, 0)
            ]
        );
        drop(pens);
        // Typing goes to the shared keyboard, not to a pen.
        own(&input, "rog", json!([{"t":"key","key":"enter"}]));
        assert_eq!(
            shared.0.lock().unwrap().last().unwrap(),
            &vec![
                (code::EV_KEY, code::KEY_ENTER, 1),
                (code::EV_KEY, code::KEY_ENTER, 0)
            ]
        );
    }

    #[test]
    fn leaving_keeps_the_pointer_in_reach_and_only_the_idle_watch_takes_it_away() {
        let (input, _, pens) = with_pens();
        own(&input, "phone", json!([{"t":"move","dx":10,"dy":-5}]));
        let pen = pens.lock().unwrap()[0].1.clone();
        let owner = (plugin("control"), Some("phone".into()));
        let before = pen.0.lock().unwrap().len();
        input.release(&owner).unwrap();
        assert_eq!(pen.0.lock().unwrap().len(), before, "nothing held: leaving sends nothing");
        // Back later: the same device, in reach the whole time, just moves on.
        own(&input, "phone", json!([{"t":"move","dx":3,"dy":2}]));
        assert_eq!(pens.lock().unwrap().len(), 1);
        assert_eq!(
            pen.0.lock().unwrap().last().unwrap(),
            &vec![(code::EV_ABS, code::ABS_X, 973), (code::EV_ABS, code::ABS_Y, 537)]
        );
        assert!(!pen.0.lock().unwrap()[before..].iter().flatten().any(|e| *e == (code::EV_KEY, code::BTN_TOOL_PEN, 0)));
        // Gone for good: the idle watch takes the tool out of reach; returning motion re-enters.
        DesktopInput::hide_idle(&input.state, std::time::Instant::now() + IDLE_HIDE);
        assert_eq!(pen.0.lock().unwrap().last().unwrap(), &vec![(code::EV_KEY, code::BTN_TOOL_PEN, 0)]);
        let hidden = pen.0.lock().unwrap().len();
        own(&input, "phone", json!([{"t":"move","dx":1,"dy":0}]));
        let frames = pen.0.lock().unwrap();
        assert!(frames[hidden..].iter().flatten().any(|e| *e == (code::EV_KEY, code::BTN_TOOL_PEN, 1)));
        assert_eq!(frames.last().unwrap(), &vec![(code::EV_ABS, code::ABS_X, 974), (code::EV_ABS, code::ABS_Y, 537)]);
    }

    #[test]
    fn a_pointer_left_standing_by_a_vanished_controller_leaves_unless_something_is_held() {
        let (input, _, pens) = with_pens();
        own(&input, "phone", json!([{"t":"move","dx":1,"dy":1}]));
        own(&input, "rog", json!([{"t":"button","button":"left","down":true}]));
        let out = vec![(code::EV_KEY, code::BTN_TOOL_PEN, 0)];
        let made = pens.lock().unwrap().clone();
        let frames = |i: usize| made[i].1.0.lock().unwrap().clone();
        DesktopInput::hide_idle(&input.state, std::time::Instant::now());
        assert_ne!(frames(0).last().unwrap(), &out, "not idle yet");
        let later = std::time::Instant::now() + IDLE_HIDE;
        DesktopInput::hide_idle(&input.state, later);
        assert_eq!(frames(0).last().unwrap(), &out);
        assert_ne!(frames(1).last().unwrap(), &out, "a held drag is never taken away");
        let hidden = frames(0).len();
        DesktopInput::hide_idle(&input.state, later);
        assert_eq!(frames(0).len(), hidden, "hidden once");
    }

    #[test]
    fn a_gesture_that_rests_lets_go_of_its_click_but_keeps_the_pointer_in_reach() {
        let (input, _, pens) = with_pens();
        own(&input, "phone", json!([{"t":"button","button":"left","down":true}]));
        let pen = pens.lock().unwrap()[0].1.clone();
        let owner = (plugin("control"), Some("phone".into()));
        input.rest(&owner).unwrap();
        assert_eq!(
            pen.0.lock().unwrap().last().unwrap(),
            &vec![(code::EV_KEY, code::BTN_TOUCH, 0)]
        );
        let frames = pen.0.lock().unwrap().len();
        own(&input, "phone", json!([{"t":"move","dx":1,"dy":0}]));
        let after = pen.0.lock().unwrap()[frames..].to_vec();
        assert!(
            !after.iter().flatten().any(|e| e.1 == code::BTN_TOOL_PEN),
            "still in reach, no proximity out and in again: {after:?}"
        );
    }

    #[test]
    fn a_device_lets_go_of_its_own_click_and_keys_and_never_another_s() {
        let (input, shared, pens) = with_pens();
        own(
            &input,
            "rog",
            json!([{"t":"button","button":"left","down":true},{"t":"key_code","code":42,"down":true}]),
        );
        own(
            &input,
            "hp",
            json!([{"t":"key_code","code":42,"down":true}]),
        );
        let rog_pen = pens.lock().unwrap()[0].1.clone();
        assert!(rog_pen.0.lock().unwrap().last().unwrap().contains(&(
            code::EV_KEY,
            code::BTN_TOUCH,
            1
        )));
        let before = shared.0.lock().unwrap().len();
        input
            .release(&(plugin("control"), Some("rog".into())))
            .unwrap();
        assert_eq!(
            rog_pen.0.lock().unwrap().last().unwrap(),
            &vec![(code::EV_KEY, code::BTN_TOUCH, 0)],
            "its click is let go; its pointer stays in reach"
        );
        let after = shared.0.lock().unwrap().clone();
        assert_eq!(after.len(), before + 1);
        assert_eq!(
            after.last().unwrap(),
            &vec![(code::EV_KEY, 42, 0)],
            "its Shift, once"
        );
        // The other device still holds its Shift: nothing more was sent for it.
        input.revoke(&plugin("control"));
        assert_eq!(
            shared.0.lock().unwrap().last().unwrap(),
            &vec![(code::EV_KEY, 42, 0)]
        );
        assert_eq!(
            shared.0.lock().unwrap().len(),
            before + 2,
            "and it is let go when the extension ends"
        );
    }

    #[test]
    fn a_short_first_gesture_is_delivered_after_discovery_even_when_it_already_rested() {
        for click in [false, true] {
            let recorder = Recorder::default();
            let mut pen = Pen::new(Box::new(recorder), DEFAULT_AREA, SETTLE);
            assert!(pen.translate(&PointerInput::Move { dx: 10., dy: -5. }).unwrap().is_empty());
            if click {
                pen.translate(&PointerInput::Button { button: Button::Left, down: true });
                pen.translate(&PointerInput::Button { button: Button::Left, down: false });
            }
            assert!(pen.lift().is_empty());
            assert!(pen.settled(pen.ready_at - std::time::Duration::from_millis(1)).is_none());
            let frames = pen.settled(pen.ready_at).expect("the first gesture must not vanish");
            assert!(frames.iter().flatten().any(|e| *e == (code::EV_ABS, code::ABS_X, 970)));
            let clicks: Vec<_> = frames.iter().flatten().filter(|e| e.1 == code::BTN_TOUCH).copied().collect();
            assert_eq!(clicks, if click { vec![(code::EV_KEY, code::BTN_TOUCH, 1), (code::EV_KEY, code::BTN_TOUCH, 0)] } else { vec![] });
            assert!(pen.settled(pen.ready_at).is_none(), "the gesture is delivered only once");
            pen.leave();
            assert!(pen.settled(pen.ready_at).is_none(), "leaving must not resurrect the pointer");
        }
    }

    #[test]
    fn a_pointer_of_its_own_needs_a_device_name_and_a_pen_follows_but_says_nothing_until_settled() {
        let (checked, _, _) = with_pens();
        assert!(
            checked
                .inject(
                    &plugin("control"),
                    &json!({"events":[{"t":"move","dx":1,"dy":1}],"pointer":"own"})
                )
                .is_err()
        );
        assert!(
            checked
                .inject(
                    &plugin("control"),
                    &json!({"events":[{"t":"move","dx":1,"dy":1}],"owner":"\n"})
                )
                .is_err()
        );
        let (shared, _, _) = input();
        let pens = Arc::new(Mutex::new(Vec::new()));
        let made = pens.clone();
        let slow = shared.with_pens(
            Box::new(move |_, _, _| {
                let pen = Recorder::default();
                made.lock().unwrap().push(pen.clone());
                Ok(Box::new(pen) as Box<dyn Device>)
            }),
            std::time::Duration::from_secs(3600),
        );
        own(&slow, "rog", json!([{"t":"move","dx":100,"dy":0}]));
        own(&slow, "rog", json!([
            {"t":"button","button":"left","down":true},
            {"t":"button","button":"left","down":false}
        ]));
        let pen = pens.lock().unwrap()[0].clone();
        assert!(pen.0.lock().unwrap().is_empty(), "nothing before the desktop opened it");
        // Once it has, the click made meanwhile is given whole: a press, then its release.
        DesktopInput::hide_idle(&slow.state, std::time::Instant::now() + std::time::Duration::from_secs(3601));
        let keys: Vec<Raw> = pen.0.lock().unwrap().iter().flatten()
            .filter(|e| e.1 == code::BTN_TOUCH).copied().collect();
        assert_eq!(keys, vec![(code::EV_KEY, code::BTN_TOUCH, 1), (code::EV_KEY, code::BTN_TOUCH, 0)]);
        let frames = pen.0.lock().unwrap().clone();
        assert!(
            frames.iter().all(|f| f.iter().filter(|e| e.1 == code::BTN_TOUCH).count() <= 1),
            "press and release never share a frame: {frames:?}"
        );
    }
    use serde_json::json;

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<Vec<Raw>>>>);
    impl Device for Recorder {
        fn emit(&mut self, events: &[Raw]) -> std::io::Result<()> {
            self.0.lock().unwrap().push(events.to_vec());
            Ok(())
        }
    }

    fn input() -> (DesktopInput, Recorder, Arc<Mutex<usize>>) {
        let recorder = Recorder::default();
        let made = Arc::new(Mutex::new(0));
        let (r, m) = (recorder.clone(), made.clone());
        let input = DesktopInput::new(Box::new(move || {
            *m.lock().unwrap() += 1;
            Ok(Box::new(r.clone()) as Box<dyn Device>)
        }));
        (input, recorder, made)
    }

    fn plugin(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }

    fn inject(
        input: &DesktopInput,
        caller: &str,
        events: serde_json::Value,
    ) -> Result<serde_json::Value, &'static str> {
        input.inject(&plugin(caller), &json!({ "events": events }))
    }

    #[test]
    fn motion_buttons_scroll_and_keys_become_device_events_and_fractions_add_up() {
        let (input, recorder, made) = input();
        assert_eq!(
            *made.lock().unwrap(),
            0,
            "no device before anything is replayed"
        );
        inject(
            &input,
            "control",
            json!([
                {"t":"move","dx":0.6,"dy":-2.5},
                {"t":"move","dx":0.6,"dy":0},
                {"t":"button","button":"left","down":true},
                {"t":"button","button":"left","down":false},
                {"t":"scroll","dx":0,"dy":2},
                {"t":"key","key":"enter"}
            ]),
        )
        .unwrap();
        assert_eq!(
            recorder.0.lock().unwrap()[0],
            vec![
                (code::EV_REL, code::REL_Y, -2),
                // 0.6 + 0.6: the first move had no whole pixel across, the second makes one.
                (code::EV_REL, code::REL_X, 1),
                (code::EV_KEY, code::BTN_LEFT, 1),
                (code::EV_KEY, code::BTN_LEFT, 0),
                (code::EV_REL, code::REL_WHEEL, -2),
                (code::EV_REL, code::REL_WHEEL_HI_RES, -240),
                (code::EV_KEY, code::KEY_ENTER, 1),
                (code::EV_KEY, code::KEY_ENTER, 0),
            ]
        );
        assert_eq!(*made.lock().unwrap(), 1);
    }

    #[test]
    fn text_is_struck_as_keys_with_shift_and_untyped_characters_are_counted() {
        let (input, recorder, _) = input();
        let answer = inject(&input, "control", json!([{"t":"text","text":"aA ș"}])).unwrap();
        assert_eq!(answer["untyped"], 1);
        assert_eq!(
            recorder.0.lock().unwrap()[0],
            vec![
                (code::EV_KEY, 30, 1),
                (code::EV_KEY, 30, 0),
                (code::EV_KEY, code::KEY_LEFTSHIFT, 1),
                (code::EV_KEY, 30, 1),
                (code::EV_KEY, 30, 0),
                (code::EV_KEY, code::KEY_LEFTSHIFT, 0),
                (code::EV_KEY, code::KEY_SPACE, 1),
                (code::EV_KEY, code::KEY_SPACE, 0),
            ]
        );
    }

    #[test]
    fn a_held_button_is_released_when_its_extension_lets_go_or_ends_and_only_its_own() {
        let (input, recorder, _) = input();
        inject(
            &input,
            "one",
            json!([{"t":"button","button":"left","down":true}]),
        )
        .unwrap();
        inject(
            &input,
            "two",
            json!([{"t":"button","button":"right","down":true}]),
        )
        .unwrap();
        input.revoke(&plugin("one"));
        assert_eq!(
            recorder.0.lock().unwrap().last().unwrap(),
            &vec![(code::EV_KEY, code::BTN_LEFT, 0)],
            "only what the ended extension held is let go"
        );
        input.release(&(plugin("two"), None)).unwrap();
        assert_eq!(
            recorder.0.lock().unwrap().last().unwrap(),
            &vec![(code::EV_KEY, code::BTN_RIGHT, 0)]
        );
        let before = recorder.0.lock().unwrap().len();
        input.release(&(plugin("two"), None)).unwrap();
        assert_eq!(
            recorder.0.lock().unwrap().len(),
            before,
            "nothing held, nothing sent"
        );
    }

    #[test]
    fn a_repeated_press_or_a_stray_release_sends_nothing() {
        let (input, recorder, _) = input();
        inject(
            &input,
            "control",
            json!([
                {"t":"button","button":"left","down":false},
                {"t":"button","button":"left","down":true},
                {"t":"button","button":"left","down":true}
            ]),
        )
        .unwrap();
        assert_eq!(
            recorder.0.lock().unwrap()[0],
            vec![(code::EV_KEY, code::BTN_LEFT, 1)]
        );
    }

    #[test]
    fn a_batch_with_anything_out_of_bounds_is_refused_whole() {
        let (input, recorder, made) = input();
        for bad in [
            json!([]),
            json!([{"t":"move","dx":1,"dy":1},{"t":"text","text":"\u{1b}[2J"}]),
            json!([{"t":"scroll","dx":0,"dy":1000}]),
            json!([{"t":"teleport"}]),
            json!(
                (0..=MAX_POINTER_BATCH)
                    .map(|_| json!({"t":"move","dx":1,"dy":0}))
                    .collect::<Vec<_>>()
            ),
        ] {
            assert!(inject(&input, "control", bad).is_err());
        }
        assert!(recorder.0.lock().unwrap().is_empty());
        assert_eq!(*made.lock().unwrap(), 0);
    }

    #[test]
    fn a_device_that_cannot_be_made_is_an_error_and_is_tried_again() {
        let tries = Arc::new(Mutex::new(0));
        let t = tries.clone();
        let input = DesktopInput::new(Box::new(move || {
            *t.lock().unwrap() += 1;
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        }));
        for _ in 0..2 {
            assert!(inject(&input, "control", json!([{"t":"move","dx":5,"dy":0}])).is_err());
        }
        assert_eq!(*tries.lock().unwrap(), 2);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod real_device {
    /// Makes the real uinput device and moves the pointer there and back. Run by hand, on a
    /// session where `/dev/uinput` is writable: `cargo test -p peersus-app real_device -- --ignored`.
    #[test]
    #[ignore = "needs a writable /dev/uinput and moves the real pointer"]
    fn the_system_device_is_made_and_takes_motion() {
        let input = super::DesktopInput::system();
        let caller = peersus_core::plugins::PluginId::new("control").unwrap();
        for dx in [40, -40] {
            input
                .inject(
                    &caller,
                    &serde_json::json!({"events":[{"t":"move","dx":dx,"dy":0}]}),
                )
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }

    /// Makes a device's own pointer (a pen tablet) and moves it around the middle of the desktop
    /// for a few seconds, so that whether the desktop draws it can be seen.
    #[test]
    #[ignore = "needs a writable /dev/uinput and draws a second pointer"]
    fn a_device_s_own_pointer_is_drawn() {
        let input = super::DesktopInput::system();
        let caller = peersus_core::plugins::PluginId::new("control").unwrap();
        for step in 0..120 {
            let (dx, dy) = match (step / 15) % 4 {
                0 => (8, 0),
                1 => (0, 8),
                2 => (-8, 0),
                _ => (0, -8),
            };
            input
                .inject(
                    &caller,
                    &serde_json::json!({"events":[{"t":"move","dx":dx,"dy":dy}],"owner":"probe","pointer":"own"}),
                )
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Puts a device's own pointer on the top bar, then over the middle of the screen, three
    /// seconds each: whether a desktop draws it only where no window lies under it can be seen.
    #[test]
    #[ignore = "needs a writable /dev/uinput and draws a second pointer"]
    fn a_device_s_own_pointer_over_the_bar_then_over_the_middle() {
        let input = super::DesktopInput::system();
        let caller = peersus_core::plugins::PluginId::new("control").unwrap();
        for y in [0.005, 0.5] {
            for step in 0..60 {
                let x = 0.3 + 0.4 * (f64::from(step) / 60.);
                input
                    .inject(
                        &caller,
                        &serde_json::json!({"events":[{"t":"position","x":x,"y":y}],"owner":"probe","pointer":"own"}),
                    )
                    .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}
