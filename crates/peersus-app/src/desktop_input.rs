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
use crate::platform_services::{Call, Response, input_arg};
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
#[cfg(target_os = "linux")]
pub(crate) use cursor::authority_file;

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
/// The most devices one extension replays for at once: each is a pointer of its own on this desktop
/// and state kept here, so the names an extension sends must not make them without limit.
const MOST_OWNERS: usize = 16;
/// How long a compositor needs to find and open a new device: what is sent before is lost, including
/// what makes its pointer appear, after which the pointer is ignored for good.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(600);
/// A device's own pointer with nothing pressed that has not moved for this long is taken off the
/// screen, as a resting mouse pointer is by many desktops: a pointer left standing looks stuck.
/// Leaving the page alone does not take it away (see `release`); this pause does. A desktop that
/// forgets the cursor of a tool that left draws it again over windows that ask for one (Peersus
/// itself, through its patched winit, and toolkit applications such as GTK).
const IDLE_HIDE: std::time::Duration = std::time::Duration::from_secs(3);
/// What a controller that has sent nothing for this long still holds is let go: a button or key kept
/// down by a device that went away would otherwise stay down for ever. It is the time a silent
/// connection is given before it is ended, so a press that is really held ends no sooner than before.
const HELD_LIMIT: std::time::Duration = peersus_plugin::pump::IDLE_LIMIT;
/// The desktop's size when the system does not say it.
const DEFAULT_AREA: (i32, i32) = (1920, 1080);
/// When a device's input was last replayed on this desktop by this process. One for the process,
/// because the desktop is one: the capture at the edges asks it (see [`replayed_within`]) and
/// knows nothing else of what is replayed.
static REPLAYED: Mutex<Option<std::time::Instant>> = Mutex::new(None);
/// Told each time input is replayed, for whoever waits to hear that it began (see [`replayed`]).
static REPLAYING: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Whether a device's input was replayed on this desktop within the last `limit`. While it is, the
/// pointer here is moved by that device's person and not by someone at this desktop.
pub fn replayed_within(limit: std::time::Duration) -> bool {
    REPLAYED
        .lock()
        .ok()
        .and_then(|at| *at)
        .is_some_and(|at| at.elapsed() <= limit)
}

/// Wait until a device's input is replayed on this desktop. It may also return for input replayed
/// before the wait began: ask [`replayed_within`] what is true now.
pub async fn replayed() {
    REPLAYING.notified().await;
}

/// One event for the device: its type, code and value.
pub type Raw = (u16, u16, i32);

/// Where events go. One real device; a recording one under test.
pub trait Device: Send {
    /// Emit one group of events, which the system reads as happening together.
    fn emit(&mut self, events: &[Raw]) -> std::io::Result<()>;
    /// Make now what is otherwise made with the first event that needs it, so the desktop has found it by then
    /// (see [`SETTLE`]). Nothing to do for a device that is whole from the start.
    fn warm(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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

    /// This computer's own pointer brought to where this one is, as events for the shared pointer.
    ///
    /// A desktop gives a pen no wheel: a notch turns what is under the desktop's own pointer, and a pen does not
    /// move that one. Left where this computer's mouse last was, the wheel would scroll whatever lies there, or
    /// nothing, while the pointer the person sees stands over the page they mean.
    fn under(&self, raw: &mut Vec<Raw>) {
        // The middle of the pixel the pen is on, in the shared pointer's steps across the desktop.
        let at = |position: f64, extent: i32| {
            let share = (position.floor() + 0.5) / f64::from(extent.max(1));
            (share * (f64::from(code::ABS_MAX) + 1.))
                .round()
                .clamp(0., f64::from(code::ABS_MAX)) as i32
        };
        let (x, y) = (
            at(self.position.0, self.area.0),
            at(self.position.1, self.area.1),
        );
        // The system does not say again a position a device said last. Said beside itself first, it is said even
        // when the pen has not moved since the last notch and this computer's mouse has.
        raw.extend([
            (code::EV_ABS, code::ABS_X, x ^ 1),
            (code::EV_ABS, code::ABS_Y, y ^ 1),
            (code::EV_ABS, code::ABS_X, x),
            (code::EV_ABS, code::ABS_Y, y),
        ]);
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
                    vec![
                        (code::EV_ABS, code::ABS_X, x),
                        (code::EV_ABS, code::ABS_Y, y),
                    ],
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
            if down {
                queued.insert(tool);
            } else {
                queued.remove(&tool);
            }
        }
        self.pending
            .extend(queued.into_iter().map(|tool| (tool, false)));
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
    /// When each controller last sent input: see [`HELD_LIMIT`].
    heard: HashMap<Owner, std::time::Instant>,
    /// The desktop's size in pixels, as last read: what a pen's range covers.
    area: Option<(i32, i32)>,
    /// Whether the task that hides idle pointers runs.
    watching: bool,
}

#[derive(Clone)]
pub struct DesktopInput {
    factory: Arc<Factory>,
    pens: Arc<PenFactory>,
    independent_pointer: bool,
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
            independent_pointer: false,
            settle: SETTLE,
            state: Arc::new(Mutex::new(State {
                device: None,
                callers: HashMap::new(),
                pens: HashMap::new(),
                heard: HashMap::new(),
                area: None,
                watching: false,
            })),
        }
    }

    /// Devices' own pointers made by `pens`, sent to once `settle` has passed.
    pub fn with_pens(mut self, pens: PenFactory, settle: std::time::Duration) -> Self {
        self.pens = Arc::new(pens);
        self.independent_pointer = true;
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

    /// The system's own keyboard and mouse on Windows: SendInput. Windows draws one pointer only,
    /// so a device's own pointer is refused there and controllers share the mouse.
    #[cfg(windows)]
    pub fn system() -> Self {
        Self::new(Box::new(|| {
            injector::open().map(|d| Box::new(d) as Box<dyn Device>)
        }))
    }

    /// The system's own keyboard and mouse on macOS: Quartz events. Without the person's
    /// Accessibility permission, opening the device fails and every replay says it is unsupported.
    #[cfg(target_os = "macos")]
    pub fn system() -> Self {
        Self::new(Box::new(|| {
            injector::open().map(|d| Box::new(d) as Box<dyn Device>)
        }))
    }

    /// No device this application can make elsewhere yet: every replay says so.
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    pub fn system() -> Self {
        Self::new(Box::new(|| {
            Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
        }))
    }

    pub fn dispatch(&self, call: Call) -> Response {
        self.dispatch_leased(call, None)
    }

    pub(crate) fn dispatch_leased(
        &self,
        call: Call,
        lease: Option<crate::platform_services::InputLease>,
    ) -> Response {
        let this = self.clone();
        Box::pin(async move {
            // A device about to get a pointer of its own: the desktop's size is read first, outside
            // the lock, so the pen covers the desktop as it is now.
            if call.args[input_arg::POINTER] == input_arg::POINTER_OWN {
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
            // Whoever replays input may leave something pressed: the watch is what lets go of it.
            if call.method == "inject" {
                this.watch();
            }
            // The device is written synchronously and briefly: a few small writes per batch.
            let outcome = match call.method.as_str() {
                "inspect" if call.args.as_object().is_some_and(|args| args.is_empty()) => {
                    serde_json::to_value(peersus_api::contributions::InputCapabilities {
                        independent_pointer: this.independent_pointer,
                    })
                    .map_err(|_| "Input capabilities are unavailable")
                }
                "inject" => {
                    let Some(result) =
                        crate::platform_services::replay_input(lease.as_ref(), || {
                            this.inject(&call.caller, &call.args)
                        })
                    else {
                        return Outcome::NotAccepted {};
                    };
                    result
                }
                // `keep` asks for the pointer to stay in reach, which it does either way here.
                "release" => {
                    let Some(result) =
                        crate::platform_services::replay_input(lease.as_ref(), || {
                            owner_of(&call.caller, &call.args)
                                .ok_or("The input was not understood")
                                .and_then(|owner| this.release(&owner))
                                .map(|()| serde_json::json!({"released":true}))
                        })
                    else {
                        return Outcome::NotAccepted {};
                    };
                    result
                }
                _ => Err("Unknown input method"),
            };
            peersus_diagnostics::dev_trace!(
                stage = "input-result",
                method = peersus_diagnostics::method(&call.method),
                outcome = match &outcome {
                    Ok(_) => "ok",
                    Err("This computer cannot give that device a pointer of its own") =>
                        "independent-pointer-unavailable",
                    Err("The input was not understood") => "invalid-input",
                    Err("Unknown input method") => "unknown-method",
                    Err(_) => "backend-unavailable",
                },
                "Input adapter completed"
            );
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
        let own = match args.get(input_arg::POINTER) {
            None => false,
            Some(pointer) if pointer == input_arg::POINTER_OWN && owner.1.is_some() => true,
            Some(_) => return Err(REFUSED),
        };
        let events: Vec<PointerInput> =
            serde_json::from_value(args.get(input_arg::EVENTS).cloned().ok_or(REFUSED)?)
                .map_err(|_| REFUSED)?;
        if events.is_empty() || events.len() > MAX_POINTER_BATCH {
            return Err(REFUSED);
        }
        // All of a batch is checked before any of it is replayed: half a gesture is worse than none.
        for event in &events {
            event.validate().map_err(|_| REFUSED)?;
        }
        let mut state = self.state.lock().map_err(|_| "Input is unavailable")?;
        if !Self::admit(&mut state, &owner) {
            return Err("This extension replays input for too many devices at once");
        }
        let now = std::time::Instant::now();
        if let Ok(mut replayed) = REPLAYED.lock() {
            *replayed = Some(now);
        }
        REPLAYING.notify_one();
        state.heard.insert(owner.clone(), now);
        // Only the application writes `label` (see the runtime); a pointer whose tag changed is made again with the new one.
        let label = args
            .get(input_arg::LABEL)
            .and_then(|label| label.as_str())
            .map(str::to_owned);
        if own && state.pens.get(&owner).is_some_and(|pen| pen.label != label) {
            state.pens.remove(&owner);
        }
        let mut fresh = false;
        if own && !state.pens.contains_key(&owner) {
            let area = state.area.unwrap_or(DEFAULT_AREA);
            let name = owner.1.as_deref().unwrap_or_default();
            let device = (self.pens)(name, label.as_deref(), area).map_err(|_| NO_PEN)?;
            let mut pen = Pen::new(device, area, self.settle);
            pen.label = label;
            state.pens.insert(owner.clone(), pen);
            fresh = true;
        }
        // A device with a pointer of its own needs the shared keyboard and mouse only for its first key or wheel notch,
        // and a desktop needs a moment to find a new device and loses what is sent before (see `SETTLE`): that key or
        // notch. So they are made with its first input of any kind. A computer nobody controls still has none; one that
        // cannot be made now is tried again, and refused aloud, when something needs it.
        if own && state.device.is_none() {
            state.device = (self.factory)().ok();
            fresh = true;
        }
        // The same holds for the pointer its wheel is brought under (see `Pen::under`).
        if fresh && let Some(device) = state.device.as_mut() {
            let _ = device.warm();
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
                    if own
                        && matches!(event, PointerInput::Scroll { .. })
                        && let Some(pen) = state.pens.get(&owner)
                    {
                        pen.under(&mut shared);
                    }
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

    /// Whether `owner` may be replayed for: it already is, or its extension has fewer than
    /// [`MOST_OWNERS`] devices. At the limit, the pointers of this extension's devices that left
    /// (not heard from, nothing pressed) are given up first; one that comes back is made again.
    fn admit(state: &mut State, owner: &Owner) -> bool {
        let State {
            callers,
            pens,
            heard,
            ..
        } = state;
        if heard.contains_key(owner) || pens.contains_key(owner) || callers.contains_key(owner) {
            return true;
        }
        let count = |pens: &HashMap<Owner, Pen>| {
            pens.keys()
                .chain(heard.keys())
                .chain(callers.keys())
                .filter(|known| known.0 == owner.0)
                .collect::<std::collections::HashSet<_>>()
                .len()
        };
        if count(pens) < MOST_OWNERS {
            return true;
        }
        pens.retain(|known, pen| {
            known.0 != owner.0 || heard.contains_key(known) || !pen.held.is_empty()
        });
        count(pens) < MOST_OWNERS
    }

    /// Let go of everything `owner` holds. Its own pointer is lifted here and stays where it is;
    /// what it pressed on the shared keyboard and mouse is returned, to be released there.
    fn let_go(state: &mut State, owner: &Owner, now: std::time::Instant) -> Vec<Raw> {
        state.heard.remove(owner);
        if let Some(pen) = state.pens.get_mut(owner) {
            let up = pen.lift();
            if !up.is_empty() {
                let _ = pen.device.emit(&up);
            }
            pen.last = now;
        }
        state
            .callers
            .remove(owner)
            .map(|gone| {
                gone.held
                    .into_iter()
                    .filter(|button| {
                        !state
                            .callers
                            .values()
                            .any(|caller| caller.held.contains(button))
                    })
                    .map(|button| (code::EV_KEY, button, 0))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A gesture of this controller ended, or it left (its page closed, its app went to the
    /// background): let go of everything it holds, on its own pointer and on the shared keyboard
    /// and mouse. Its pointer stays where it is and in reach: the desktop forgets the cursor of a
    /// tool that leaves and draws it again only over a window that asks for one, so leaving would
    /// make the pointer vanish for good the next time this device controls this computer. A
    /// pointer whose controller does not come back is taken off the screen by the idle watch (see
    /// [`IDLE_HIDE`]).
    fn release(&self, owner: &Owner) -> Result<(), &'static str> {
        self.release_scoped(owner, false)
    }

    fn release_scoped(&self, owner: &Owner, discard_pending: bool) -> Result<(), &'static str> {
        let mut state = self.state.lock().map_err(|_| "Input is unavailable")?;
        if discard_pending && let Some(pen) = state.pens.get_mut(owner) {
            pen.pending.clear();
            pen.waiting = false;
        }
        let raw = Self::let_go(&mut state, owner, std::time::Instant::now());
        if raw.is_empty() {
            return Ok(());
        }
        self.emit(&mut state, &raw)
    }

    pub(crate) fn release_peer(&self, caller: &PluginId, owner: &str) {
        let _ = self.release_scoped(&(caller.clone(), Some(owner.to_owned())), true);
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

    /// Give each device's own pointer the clicks it was handed while it settled, let go of what a
    /// controller not heard from for [`HELD_LIMIT`] still holds, and take every pointer that has
    /// stood still for [`IDLE_HIDE`] off the screen unless something is pressed: a held drag is
    /// the person's for as long as its controller is there.
    fn hide_idle(state: &Mutex<State>, now: std::time::Instant) {
        let Ok(mut state) = state.lock() else { return };
        let silent: Vec<Owner> = state
            .heard
            .iter()
            .filter(|(_, heard)| now.saturating_duration_since(**heard) >= HELD_LIMIT)
            .map(|(owner, _)| owner.clone())
            .collect();
        for owner in silent {
            let raw = Self::let_go(&mut state, &owner, now);
            // Only a device that exists holds anything: one that failed took its keys with it.
            if !raw.is_empty()
                && let Some(device) = state.device.as_mut()
                && device.emit(&raw).is_err()
            {
                state.device = None;
            }
        }
        state.pens.retain(|_, pen| {
            if let Some(frames) = pen.settled(now)
                && frames.iter().any(|frame| pen.device.emit(frame).is_err())
            {
                return false;
            }
            if pen.near
                && pen.held.is_empty()
                && now.saturating_duration_since(pen.last) >= IDLE_HIDE
            {
                let up = pen.leave();
                if pen.device.emit(&up).is_err() {
                    return false;
                }
            }
            true
        });
    }

    /// Start, once, the task that hides idle pointers. It ends with the service.
    fn watch(&self) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
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
            let _ = self.release_scoped(&owner, true);
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
    match args.get(input_arg::OWNER) {
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

/// The shared keyboard and mouse on Windows (SendInput) and macOS (Quartz events, which need the
/// person's Accessibility permission; the system asks for it when this first opens). Events arrive
/// in the device's Linux language and are replayed as the matching input of the platform: relative
/// motion, a position as a share of the main display, whole wheel notches, the three buttons and keys.
#[cfg(any(windows, target_os = "macos"))]
mod injector;

#[cfg(target_os = "linux")]
mod uinput;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inspection_reports_adapter_support_without_opening_or_replaying_input() {
        let (shared_only, _, opens) = input();
        let (independent, _, _) = with_pens();
        for (input, expected) in [(shared_only, false), (independent, true)] {
            let call = |args| Call {
                caller: plugin("synthetic"),
                service: crate::platform_services::INPUT_SERVICE.into(),
                method: "inspect".into(),
                args,
            };
            assert_eq!(
                input.dispatch(call(json!({}))).await,
                Outcome::ok(json!({"independent_pointer":expected}))
            );
            assert!(matches!(
                input.dispatch(call(json!({"unknown":true}))).await,
                Outcome::Error { .. }
            ));
        }
        assert_eq!(
            *opens.lock().unwrap(),
            0,
            "inspection must not create an input device"
        );
    }

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
                let name =
                    label.map_or_else(|| owner.to_owned(), |label| format!("{owner}#{label}"));
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
            let mut args =
                json!({"events": [{"t":"move","dx":1,"dy":1}], "owner": "phone", "pointer": "own"});
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
        let made: Vec<String> = pens
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        // One pointer per tag in effect; the same tag again is the same pointer.
        assert_eq!(made, ["phone", "phone#ROG", "phone"]);
    }

    #[test]
    fn a_wheel_turns_under_the_devices_own_pointer_and_not_where_this_computers_mouse_was_left() {
        let (input, shared, _) = with_pens();
        own(&input, "desk", json!([{"t":"position","x":0.25,"y":0.75}]));
        assert!(
            shared.0.lock().unwrap().is_empty(),
            "a pointer of its own does not move this computer's"
        );
        // The middle of the pixel the pen stands on (479, 809 of 1920 by 1080), beside itself and then itself.
        let (x, y) = (16_367, 49_122);
        let notch = |wheel: i32| {
            vec![
                (code::EV_ABS, code::ABS_X, x ^ 1),
                (code::EV_ABS, code::ABS_Y, y ^ 1),
                (code::EV_ABS, code::ABS_X, x),
                (code::EV_ABS, code::ABS_Y, y),
                (code::EV_REL, code::REL_WHEEL, wheel),
                (code::EV_REL, code::REL_WHEEL_HI_RES, wheel * 120),
            ]
        };
        own(&input, "desk", json!([{"t":"scroll","dx":0,"dy":1}]));
        // The pen has not moved, this computer's mouse may have: the pointer is brought back for every notch.
        own(&input, "desk", json!([{"t":"scroll","dx":0,"dy":-1}]));
        assert_eq!(*shared.0.lock().unwrap(), [notch(-1), notch(1)]);
        // A key is not the pointer's and moves nothing.
        own(
            &input,
            "desk",
            json!([{"t":"key_code","code":30,"down":true}]),
        );
        assert_eq!(
            shared.0.lock().unwrap().last().unwrap(),
            &vec![(code::EV_KEY, 30, 1)]
        );
        // A device without a pointer of its own moves the shared one itself: nothing is added to its wheel.
        inject(&input, "control", json!([{"t":"scroll","dx":0,"dy":1}])).unwrap();
        assert_eq!(
            shared.0.lock().unwrap().last().unwrap(),
            &vec![
                (code::EV_REL, code::REL_WHEEL, -1),
                (code::EV_REL, code::REL_WHEEL_HI_RES, -120),
            ]
        );
    }

    #[test]
    fn a_failed_independent_renderer_is_recreated_on_the_next_input() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Failed;
        impl Device for Failed {
            fn emit(&mut self, _: &[Raw]) -> std::io::Result<()> {
                Err(std::io::Error::other("synthetic renderer disconnect"))
            }
        }
        let (input, _, _) = input();
        let attempts = Arc::new(AtomicUsize::new(0));
        let made = attempts.clone();
        let recorder = Recorder::default();
        let recorded = recorder.clone();
        let input = input.with_pens(
            Box::new(move |_, _, _| {
                if made.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(Box::new(Failed))
                } else {
                    Ok(Box::new(recorded.clone()))
                }
            }),
            std::time::Duration::ZERO,
        );
        let args = json!({"events":[{"t":"move","dx":1,"dy":1}], "owner":"phone", "pointer":"own"});
        assert!(input.inject(&plugin("control"), &args).is_err());
        input.inject(&plugin("control"), &args).unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(
            recorder
                .0
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .any(|event| *event == (code::EV_KEY, code::BTN_TOOL_PEN, 1))
        );
    }

    #[test]
    fn a_renderer_that_fails_while_first_input_settles_is_discarded() {
        struct Failed;
        impl Device for Failed {
            fn emit(&mut self, _: &[Raw]) -> std::io::Result<()> {
                Err(std::io::Error::other("synthetic renderer disconnect"))
            }
        }
        let (input, _, _) = input();
        let input = input.with_pens(Box::new(|_, _, _| Ok(Box::new(Failed))), SETTLE);
        own(&input, "phone", json!([{"t":"move","dx":1,"dy":1}]));
        assert_eq!(input.state.lock().unwrap().pens.len(), 1);
        DesktopInput::hide_idle(&input.state, std::time::Instant::now() + SETTLE);
        assert!(
            input.state.lock().unwrap().pens.is_empty(),
            "failed renderer must be retried, not kept invisible"
        );
    }

    #[test]
    fn first_motion_changes_position_after_the_tool_enters_proximity() {
        let (input, _, pens) = with_pens();
        own(&input, "phone", json!([{"t":"move","dx":10,"dy":-5}]));
        let recorder = pens.lock().unwrap()[0].1.clone();
        let frames = recorder.0.lock().unwrap();
        let entered = frames
            .iter()
            .rposition(|frame| frame.contains(&(code::EV_KEY, code::BTN_TOOL_PEN, 1)))
            .unwrap();
        let last_x = |events: &[Vec<Raw>]| {
            events
                .iter()
                .flatten()
                .rev()
                .find(|event| event.0 == code::EV_ABS && event.1 == code::ABS_X)
                .map(|event| event.2)
        };
        assert_ne!(
            last_x(&frames[..=entered]),
            Some(970),
            "an unchanged coordinate after proximity-in is dropped by evdev, so no cursor appears"
        );
        assert!(frames[entered + 1..].iter().any(|frame| {
            frame.contains(&(code::EV_ABS, code::ABS_X, 970))
                && frame.contains(&(code::EV_ABS, code::ABS_Y, 535))
        }));
    }

    #[test]
    fn input_replayed_here_is_known_to_the_capture() {
        let (input, _, _) = with_pens();
        own(&input, "phone", json!([{"t":"move","dx":1,"dy":1}]));
        assert!(replayed_within(std::time::Duration::from_secs(60)));
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
        assert_eq!(
            pen.0.lock().unwrap().len(),
            before,
            "nothing held: leaving sends nothing"
        );
        // Back later: the same device, in reach the whole time, just moves on.
        own(&input, "phone", json!([{"t":"move","dx":3,"dy":2}]));
        assert_eq!(pens.lock().unwrap().len(), 1);
        assert_eq!(
            pen.0.lock().unwrap().last().unwrap(),
            &vec![
                (code::EV_ABS, code::ABS_X, 973),
                (code::EV_ABS, code::ABS_Y, 537)
            ]
        );
        assert!(
            !pen.0.lock().unwrap()[before..]
                .iter()
                .flatten()
                .any(|e| *e == (code::EV_KEY, code::BTN_TOOL_PEN, 0))
        );
        // Gone for good: the idle watch takes the tool out of reach; returning motion re-enters.
        DesktopInput::hide_idle(&input.state, std::time::Instant::now() + IDLE_HIDE);
        assert_eq!(
            pen.0.lock().unwrap().last().unwrap(),
            &vec![(code::EV_KEY, code::BTN_TOOL_PEN, 0)]
        );
        let hidden = pen.0.lock().unwrap().len();
        own(&input, "phone", json!([{"t":"move","dx":1,"dy":0}]));
        let frames = pen.0.lock().unwrap();
        assert!(
            frames[hidden..]
                .iter()
                .flatten()
                .any(|e| *e == (code::EV_KEY, code::BTN_TOOL_PEN, 1))
        );
        assert_eq!(
            frames.last().unwrap(),
            &vec![
                (code::EV_ABS, code::ABS_X, 974),
                (code::EV_ABS, code::ABS_Y, 537)
            ]
        );
    }

    #[test]
    fn a_pointer_left_standing_by_a_vanished_controller_leaves_unless_something_is_held() {
        let (input, _, pens) = with_pens();
        own(&input, "phone", json!([{"t":"move","dx":1,"dy":1}]));
        own(
            &input,
            "rog",
            json!([{"t":"button","button":"left","down":true}]),
        );
        let out = vec![(code::EV_KEY, code::BTN_TOOL_PEN, 0)];
        let made = pens.lock().unwrap().clone();
        let frames = |i: usize| made[i].1.0.lock().unwrap().clone();
        DesktopInput::hide_idle(&input.state, std::time::Instant::now());
        assert_ne!(frames(0).last().unwrap(), &out, "not idle yet");
        let later = std::time::Instant::now() + IDLE_HIDE;
        DesktopInput::hide_idle(&input.state, later);
        assert_eq!(frames(0).last().unwrap(), &out);
        assert_ne!(
            frames(1).last().unwrap(),
            &out,
            "a held drag outlives the idle pause"
        );
        let hidden = frames(0).len();
        DesktopInput::hide_idle(&input.state, later);
        assert_eq!(frames(0).len(), hidden, "hidden once");
    }

    #[test]
    fn what_a_controller_gone_silent_still_holds_is_let_go_after_the_limit_and_only_its_own() {
        let (input, recorder, pens) = with_pens();
        own(
            &input,
            "rog",
            json!([{"t":"button","button":"left","down":true}]),
        );
        inject(
            &input,
            "one",
            json!([{"t":"key_code","code":30,"down":true}]),
        )
        .unwrap();
        let pen = pens.lock().unwrap()[0].1.clone();
        let (drag, typed) = (
            pen.0.lock().unwrap().len(),
            recorder.0.lock().unwrap().len(),
        );
        let almost = std::time::Instant::now() + HELD_LIMIT - std::time::Duration::from_secs(1);
        DesktopInput::hide_idle(&input.state, almost);
        assert_eq!(
            (
                pen.0.lock().unwrap().len(),
                recorder.0.lock().unwrap().len()
            ),
            (drag, typed),
            "held, and its controller heard from within the limit: nothing is let go"
        );
        // Both were heard from before this moment; a second controller is heard from after it, and
        // what it holds is not touched by the first ones' silence.
        let silent_since = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(2));
        inject(
            &input,
            "two",
            json!([{"t":"button","button":"right","down":true}]),
        )
        .unwrap();
        let pressed = recorder.0.lock().unwrap().len();
        let limit = silent_since + HELD_LIMIT;
        DesktopInput::hide_idle(&input.state, limit);
        assert_eq!(
            pen.0.lock().unwrap().last().unwrap(),
            &vec![(code::EV_KEY, code::BTN_TOUCH, 0)],
            "the drag of a device that went away is ended"
        );
        let after = recorder.0.lock().unwrap().clone();
        assert_eq!(
            after[pressed..],
            [vec![(code::EV_KEY, 30, 0)]],
            "one release, of the silent one's key"
        );
        DesktopInput::hide_idle(&input.state, limit);
        assert_eq!(recorder.0.lock().unwrap().len(), pressed + 1, "let go once");
    }

    #[test]
    fn one_extension_replays_for_a_bounded_number_of_devices_and_takes_the_place_of_one_that_left()
    {
        let (input, _, pens) = with_pens();
        let ask = |caller: &str, owner: &str, pointer: bool| {
            let mut args = json!({"events": [{"t":"move","dx":1,"dy":0}], "owner": owner});
            if pointer {
                args["pointer"] = json!("own");
            }
            input.inject(&plugin(caller), &args)
        };
        let of_control = || {
            let state = input.state.lock().unwrap();
            state
                .pens
                .keys()
                .chain(state.heard.keys())
                .chain(state.callers.keys())
                .filter(|owner| owner.0 == plugin("control"))
                .collect::<std::collections::HashSet<_>>()
                .len()
        };
        for device in 0..MOST_OWNERS {
            ask("control", &format!("device-{device}"), true).unwrap();
        }
        // One more is refused, with a pointer of its own or on the shared mouse, and nothing is
        // made or remembered for it.
        assert!(ask("control", "one-more", true).is_err());
        assert!(ask("control", "and-another", false).is_err());
        assert_eq!(pens.lock().unwrap().len(), MOST_OWNERS);
        assert_eq!(of_control(), MOST_OWNERS);
        // The devices it has go on, and another extension is not counted with it.
        ask("control", "device-0", true).unwrap();
        ask("other", "its-device", true).unwrap();
        // Devices that left (silent, holding nothing) give up their places to one that comes.
        DesktopInput::hide_idle(&input.state, std::time::Instant::now() + HELD_LIMIT);
        ask("control", "newcomer", true).unwrap();
        assert_eq!(of_control(), 1);
    }

    #[test]
    fn a_gesture_that_rests_lets_go_of_its_click_but_keeps_the_pointer_in_reach() {
        let (input, _, pens) = with_pens();
        own(
            &input,
            "phone",
            json!([{"t":"button","button":"left","down":true}]),
        );
        let pen = pens.lock().unwrap()[0].1.clone();
        let owner = (plugin("control"), Some("phone".into()));
        input.release(&owner).unwrap();
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
            json!([{"t":"button","button":"left","down":true},{"t":"key_code","code":30,"down":true}]),
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
            &vec![(code::EV_KEY, 30, 0)],
            "its key, once"
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
            assert!(
                pen.translate(&PointerInput::Move { dx: 10., dy: -5. })
                    .unwrap()
                    .is_empty()
            );
            if click {
                pen.translate(&PointerInput::Button {
                    button: Button::Left,
                    down: true,
                });
                pen.translate(&PointerInput::Button {
                    button: Button::Left,
                    down: false,
                });
            }
            assert!(pen.lift().is_empty());
            assert!(
                pen.settled(pen.ready_at - std::time::Duration::from_millis(1))
                    .is_none()
            );
            let frames = pen
                .settled(pen.ready_at)
                .expect("the first gesture must not vanish");
            assert!(
                frames
                    .iter()
                    .flatten()
                    .any(|e| *e == (code::EV_ABS, code::ABS_X, 970))
            );
            let clicks: Vec<_> = frames
                .iter()
                .flatten()
                .filter(|e| e.1 == code::BTN_TOUCH)
                .copied()
                .collect();
            assert_eq!(
                clicks,
                if click {
                    vec![
                        (code::EV_KEY, code::BTN_TOUCH, 1),
                        (code::EV_KEY, code::BTN_TOUCH, 0),
                    ]
                } else {
                    vec![]
                }
            );
            assert!(
                pen.settled(pen.ready_at).is_none(),
                "the gesture is delivered only once"
            );
            pen.leave();
            assert!(
                pen.settled(pen.ready_at).is_none(),
                "leaving must not resurrect the pointer"
            );
        }
    }

    #[test]
    fn revocation_discards_unreplayed_discovery_clicks_only_for_that_controller() {
        let (mut input, _, pens) = with_pens();
        input.settle = std::time::Duration::from_secs(60);
        for controller in ["gone", "still-active"] {
            own(
                &input,
                controller,
                json!([
                    {"t":"button","button":"left","down":true},
                    {"t":"button","button":"left","down":false}
                ]),
            );
        }
        input.release_peer(&plugin("control"), "gone");
        let now = input
            .state
            .lock()
            .unwrap()
            .pens
            .values()
            .map(|pen| pen.ready_at)
            .max()
            .unwrap();
        DesktopInput::hide_idle(&input.state, now);
        let pens = pens.lock().unwrap();
        assert!(pens[0].1.0.lock().unwrap().is_empty());
        assert!(
            pens[1]
                .1
                .0
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .any(|event| *event == (code::EV_KEY, code::BTN_TOUCH, 1))
        );
    }

    #[test]
    fn scoped_revocation_keeps_a_shared_key_held_by_another_controller() {
        let (input, recorder, _) = input();
        let caller = plugin("control");
        for owner in ["left", "right"] {
            input
                .inject(
                    &caller,
                    &json!({
                        "owner":owner, "events":[{"t":"key_code","code":30,"down":true}],
                    }),
                )
                .unwrap();
        }
        input.release_peer(&caller, "left");
        assert!(
            !recorder
                .0
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .any(|event| *event == (code::EV_KEY, 30, 0))
        );
        input.release_peer(&caller, "right");
        assert_eq!(
            recorder.0.lock().unwrap().last().unwrap(),
            &[(code::EV_KEY, 30, 0)]
        );
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
        own(
            &slow,
            "rog",
            json!([
                {"t":"button","button":"left","down":true},
                {"t":"button","button":"left","down":false}
            ]),
        );
        let pen = pens.lock().unwrap()[0].clone();
        assert!(
            pen.0.lock().unwrap().is_empty(),
            "nothing before the desktop opened it"
        );
        // Once it has, the click made meanwhile is given whole: a press, then its release.
        DesktopInput::hide_idle(
            &slow.state,
            std::time::Instant::now() + std::time::Duration::from_secs(3601),
        );
        let keys: Vec<Raw> = pen
            .0
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .filter(|e| e.1 == code::BTN_TOUCH)
            .copied()
            .collect();
        assert_eq!(
            keys,
            vec![
                (code::EV_KEY, code::BTN_TOUCH, 1),
                (code::EV_KEY, code::BTN_TOUCH, 0)
            ]
        );
        let frames = pen.0.lock().unwrap().clone();
        assert!(
            frames
                .iter()
                .all(|f| f.iter().filter(|e| e.1 == code::BTN_TOUCH).count() <= 1),
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

#[cfg(all(test, windows))]
mod real_device_windows {
    use enigo::{Enigo, Mouse, Settings};

    /// Replays motion, a position and a key through SendInput and reads the real pointer back.
    /// Run by hand in an interactive desktop session (not a service or SSH session):
    /// `cargo test -p peersus-app real_device_windows -- --ignored`.
    #[test]
    #[ignore = "needs an interactive desktop and moves the real pointer"]
    fn motion_positions_and_keys_reach_the_desktop() {
        let input = super::DesktopInput::system();
        let caller = peersus_core::plugins::PluginId::new("control").unwrap();
        let probe = Enigo::new(&Settings::default()).unwrap();
        let (w, h) = probe.main_display().unwrap();
        // The middle of the main display, as a share of it.
        input
            .inject(
                &caller,
                &serde_json::json!({"events":[{"t":"position","x":0.5,"y":0.5}]}),
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let (x, y) = probe.location().unwrap();
        assert!(
            (x - (w - 1) / 2).abs() <= 2 && (y - (h - 1) / 2).abs() <= 2,
            "placed at {x},{y}"
        );
        input
            .inject(
                &caller,
                &serde_json::json!({"events":[{"t":"move","dx":40,"dy":-30}]}),
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let (moved_x, moved_y) = probe.location().unwrap();
        assert_eq!((moved_x - x, moved_y - y), (40, -30));
        // Left Shift (a plain scan code) and Left (an extended key) go down and up without error.
        input
            .inject(
                &caller,
                &serde_json::json!({"events":[
                    {"t":"key_code","code":42,"down":true},{"t":"key_code","code":42,"down":false},
                    {"t":"key_code","code":105,"down":true},{"t":"key_code","code":105,"down":false}]}),
            )
            .unwrap();
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

    /// What moving a pointer costs this desktop, by the way it is moved. Run by hand, one way at a
    /// time, while the compositor's and the display server's CPU time is read from outside: a
    /// circle in the middle of the desktop, sixty batches a second for twelve seconds.
    /// `PEERSUS_POINTER_PROBE` is `own` (a device's own pointer as the service makes it; on a
    /// desktop that is not a Wayland one that is the input device alone), `drawn` (only the drawn pointer, no
    /// input device) or `shared` (the real pointer: it moves). `PEERSUS_POINTER_PROBE_EVENTS` is
    /// how many moves one batch carries, `PEERSUS_POINTER_PROBE_LABEL` the name tag, if any,
    /// `PEERSUS_POINTER_PROBE_RADIUS` the circle's radius in pixels and `PEERSUS_POINTER_PROBE_TURN`
    /// how many batches one turn takes.
    #[test]
    #[ignore = "needs a writable /dev/uinput; draws a second pointer or moves the real one"]
    fn what_a_moving_pointer_costs_this_desktop() {
        let way = std::env::var("PEERSUS_POINTER_PROBE").expect("which pointer to move");
        let per_batch: u32 = std::env::var("PEERSUS_POINTER_PROBE_EVENTS")
            .map_or(1, |events| events.parse().expect("a number of events"));
        let label = std::env::var("PEERSUS_POINTER_PROBE_LABEL").ok();
        let number = |name: &str, otherwise: u32| {
            std::env::var(name).map_or(otherwise, |value| value.parse().expect("a number"))
        };
        const BATCHES: u32 = 720;
        let radius = f64::from(number("PEERSUS_POINTER_PROBE_RADIUS", 150));
        // One turn every two seconds unless said otherwise.
        let turn = std::f64::consts::TAU
            / f64::from(number("PEERSUS_POINTER_PROBE_TURN", 120) * per_batch);
        let step = radius * turn;
        let area = super::DEFAULT_AREA;
        let input = super::DesktopInput::system();
        let caller = peersus_core::plugins::PluginId::new("control").unwrap();
        let mut drawn = (way == "drawn")
            .then(|| super::cursor::Cursor::new(area, label.as_deref()).expect("a drawn pointer"));
        let begun = std::time::Instant::now();
        for batch in 0..BATCHES {
            let angles = (0..per_batch).map(|event| f64::from(batch * per_batch + event) * turn);
            if let Some(cursor) = &mut drawn {
                for angle in angles {
                    let x = f64::from(area.0) / 2. + radius * angle.cos();
                    let y = f64::from(area.1) / 2. + radius * angle.sin();
                    cursor
                        .emit(&[
                            (super::code::EV_ABS, super::code::ABS_X, x as i32),
                            (super::code::EV_ABS, super::code::ABS_Y, y as i32),
                            (super::code::EV_KEY, super::code::BTN_TOOL_PEN, 1),
                        ])
                        .unwrap();
                }
            } else {
                let events: Vec<_> = angles
                    .map(|angle| serde_json::json!({"t":"move","dx":-step * angle.sin(),"dy":step * angle.cos()}))
                    .collect();
                let mut args = serde_json::json!({"events":events});
                match way.as_str() {
                    "own" => {
                        args["owner"] = "probe".into();
                        args["pointer"] = "own".into();
                        if let Some(label) = &label {
                            args["label"] = label.as_str().into();
                        }
                    }
                    "shared" => {}
                    other => panic!("unknown way {other}"),
                }
                input.inject(&caller, &args).unwrap();
            }
            let due = begun + std::time::Duration::from_secs_f64(f64::from(batch + 1) / 60.);
            std::thread::sleep(due.saturating_duration_since(std::time::Instant::now()));
        }
    }
}
