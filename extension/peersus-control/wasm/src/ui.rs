//! Control owns this entire screen and interprets its gestures. The host renders generic boxes.
use crate::{
    State,
    host::{Blocked, Data, Permission, Wire, missing_permissions},
    remote::{self, Refusal},
};
use serde_json::{Value, json};

#[derive(Default)]
pub struct Ui {
    devices: Vec<Value>,
    list_block: Option<Permission>,
    needs_refresh: bool,
    chosen: Option<String>,
    /// A revoked or refused operation is never reselected by the background list refresh.
    needs_choice: bool,
    /// What the page says when its chosen device is no longer ready.
    dropped: Option<Dropped>,
    /// Buttons locked down by the Left and Right keys: pressed once, held until pressed again.
    locked: Vec<&'static str>,
    /// The pad's left edge and width (thousandths of a logical pixel), from its measured size: a tap
    /// on its left half is a left click, on its right half a right click.
    pad_bounds: Option<(i32, i32)>,
    pad: Option<Pad>,
    /// When and on which half the last tap landed: a press soon after it, held and swiped, drags
    /// with that button.
    last_tap: Option<(u64, &'static str)>,
    /// When Backspace went down, independently of activity on other controls.
    erasing: Option<u64>,
    wheel: Option<i32>,
    owed: i32,
    /// How far the wheel has been turned, for its ridges to follow the finger.
    scrolled: i32,
    last_input: Option<u64>,
    /// Human input on the pad and on the wheel, each apart, even before that control sends any input: a lift lost on
    /// one is found by its own silence, whatever the other does.
    pad_at: Option<u64>,
    wheel_at: Option<u64>,
    refreshed: u64,
    clock: u64,
    /// When the connection now open was opened: it is rotated well before the other side's call
    /// deadline, even through a press held still.
    opened: Option<u64>,
}
/// One finger on the pad and what it has done so far.
struct Pad {
    x: i32,
    y: i32,
    at: u64,
    travelled: f64,
    dragging: bool,
    tap_side: &'static str,
    drag_side: &'static str,
    armed: bool,
}
/// The chosen device that can no longer be controlled, and what the page says of it.
struct Dropped {
    device: String,
    name: String,
    said: String,
}
/// A tap: lifted this soon, having moved less than this (logical pixels).
const TAP_MS: u64 = 250;
const TAP_TRAVEL: f64 = 8.;
/// A press this soon after a tap, then swiped, drags with the tapped button.
const TAP_THEN_HOLD_MS: u64 = 300;
/// A held Backspace first repeats after this, then at every tick of the page's clock.
const REPEAT_AFTER_MS: u64 = 500;
/// A press, drag or scroll with no gesture activity for this long lost its end: it is let go.
const STUCK_MS: u64 = 30_000;
fn text(value: &str) -> Value {
    json!({"k":"text","text":value,"tone":"muted"})
}
/// The caption of one of the page's controls: one line, cut with an ellipsis where the control is narrower than its word (a
/// large interface on a phone), never broken inside the word.
fn control_caption(value: &str) -> Value {
    json!({"k":"text","text":value,"tone":"muted","lines":1})
}
fn px(n: u16) -> Value {
    json!({"unit":"px","value":n})
}
fn fill() -> Value {
    json!({"unit":"percent","value":10000})
}
fn flex() -> Value {
    json!({"unit":"flex","value":1})
}
fn bx(id: Option<&str>, style: Value, children: Vec<Value>) -> Value {
    json!({"k":"box","id":id,"style":style,"children":children})
}
fn button_input(button: &str, down: bool) -> Value {
    json!({"t":"button","button":button,"down":down})
}
/// What the page says of a chosen device that can no longer be controlled; no standing is one that is no longer paired.
fn unselected(name: &str, standing: Option<Standing>) -> String {
    let reason = standing.map_or("it is no longer paired", Standing::reason);
    format!("{name} was unselected: {reason}.")
}
/// Whether a device can be controlled now and, if not, why.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Standing {
    Ready,
    MissingPermission,
    NotAllowed,
    /// Control is not running there, or not allowed there to take this device's input.
    NotThere,
    RefusedThere,
    StoppedHere,
    Sleeping,
    /// A presence this extension has no word for.
    Away,
    Offline,
}
/// The service a paired device takes Control's input on: what it must say it `accepts` to be controlled.
const SERVICE: &str = "control.pointer";
/// The id of the page's device chooser.
const CHOOSER: &str = "device";
impl Standing {
    /// A host that says neither `presence` nor `allowed` is an older one: it says who is online, and
    /// refuses the call itself when Control is not allowed there. One that does not say `accepts` has
    /// not been told by the device: the device refuses the call itself when Control is not there.
    fn of(peer: &Value) -> Self {
        let presence = match peer["presence"].as_str() {
            Some("online") => Self::Ready,
            Some("sleeping") => Self::Sleeping,
            Some("offline") => Self::Offline,
            Some(_) => Self::Away,
            None if peer["online"] == true => Self::Ready,
            None => Self::Offline,
        };
        if presence != Self::Ready {
            presence
        } else if missing_permissions(peer).next().is_some() {
            Self::MissingPermission
        } else if peer["allowed"] == false {
            Self::NotAllowed
        } else if peer["accepts"]
            .as_array()
            .is_some_and(|accepts| !accepts.iter().any(|service| service == SERVICE))
        {
            Self::NotThere
        } else {
            presence
        }
    }
    /// Why it was unselected, as the end of a sentence about it.
    fn reason(self) -> &'static str {
        match self {
            Self::Ready => "it is ready",
            Self::MissingPermission => "a permission is missing on this device",
            Self::NotAllowed => "Control is no longer allowed there",
            Self::NotThere => "Control is not ready there",
            Self::RefusedThere => "it is not accepting control",
            Self::StoppedHere => "control stopped here; select it again to start",
            Self::Sleeping => "it is sleeping",
            Self::Away => "it is away",
            Self::Offline => "it is offline",
        }
    }
}
impl Ui {
    pub(crate) fn permission_changed(&mut self) {
        self.needs_refresh = true;
    }

    fn mark_missing(peer: &mut Value, permission: &Permission) {
        if missing_permissions(peer)
            .any(|pair| pair == (permission.owner.as_str(), permission.name.as_str()))
        {
            return;
        }
        if let Some(missing) = peer["missing_permissions"].as_array_mut() {
            missing.push(permission.value());
        } else {
            peer["missing_permissions"] = json!([permission.value()]);
        }
    }

    fn standing(state: &State, peer: &Value) -> Standing {
        let standing = Standing::of(peer);
        if standing == Standing::Ready
            && peer["device"]
                .as_str()
                .is_some_and(|device| state.remote_block(device))
        {
            Standing::RefusedThere
        } else {
            standing
        }
    }

    fn stop_choice<H: Data>(&mut self, state: &mut State, host: &mut H, reason: Standing) {
        let Some(device) = self.chosen.take() else {
            return;
        };
        state.stop_sending(host, &device);
        self.reset_gesture();
        self.needs_choice = true;
        let name = self
            .devices
            .iter()
            .find(|row| row["device"] == device)
            .and_then(|row| row["name"].as_str())
            .unwrap_or("The device")
            .to_owned();
        self.dropped = Some(Dropped {
            device,
            said: unselected(&name, Some(reason)),
            name,
        });
    }

    fn observe_stop<H: Data>(&mut self, state: &mut State, host: &mut H) -> bool {
        let Some(device) = self.chosen.as_deref() else {
            return false;
        };
        let reason = if let Some(permission) = state.send_block(device) {
            if let Some(row) = self.devices.iter_mut().find(|row| row["device"] == device) {
                Self::mark_missing(row, &permission);
            }
            Standing::MissingPermission
        } else if state.local_block(device) {
            Standing::StoppedHere
        } else if state.remote_block(device) {
            Standing::RefusedThere
        } else {
            return false;
        };
        self.stop_choice(state, host, reason);
        true
    }

    fn refresh<H: Wire + Data>(
        &mut self,
        state: &mut State,
        host: &mut H,
        at: u64,
    ) -> Result<bool, &'static str> {
        self.needs_refresh = false;
        let (mut peers, blocked) = match state.call(host, "peersus.peers", "list", json!({})) {
            Ok(Value::Array(peers)) => (peers, None),
            Ok(_) => return Err(remote::answer::NOT_UNDERSTOOD),
            Err(Refusal::Other(remote::answer::DENIED)) => (Vec::new(), None),
            Err(Refusal::Blocked(Blocked::MissingPermission(permission))) => {
                (Vec::new(), Some(permission))
            }
            Err(Refusal::Blocked(Blocked::NotAccepted)) => {
                return Err(remote::answer::NOT_UNDERSTOOD);
            }
            Err(Refusal::Other(reason)) => return Err(reason),
        };
        for peer in &mut peers {
            if let Some(permission) = peer["device"].as_str().and_then(|id| state.send_block(id)) {
                Self::mark_missing(peer, &permission);
            }
        }
        let before = std::mem::replace(&mut self.devices, peers);
        let mut changed = self.devices != before || self.list_block != blocked;
        self.list_block = blocked;
        self.refreshed = at;
        if self.list_block.is_some() {
            if let Some(device) = self.chosen.take() {
                state.stop_sending(host, &device);
                self.reset_gesture();
                self.needs_choice = true;
            }
            self.dropped = None;
            return Ok(changed);
        }
        if let Some(chosen) = self.chosen.clone() {
            let named = |list: &[Value]| {
                list.iter()
                    .find(|p| p["device"].as_str() == Some(chosen.as_str()))
                    .cloned()
            };
            let now = named(&self.devices);
            let standing = now.as_ref().map(|peer| Self::standing(state, peer));
            if standing != Some(Standing::Ready) {
                // It stopped being a device that can be controlled: its gesture ends, it is unselected and the page says
                // so. Nothing is chosen in its place: what is controlled is only ever the person's own choice.
                let known = now.or_else(|| named(&before));
                let name = known
                    .as_ref()
                    .and_then(|p| p["name"].as_str())
                    .unwrap_or("The device")
                    .to_owned();
                let said = unselected(&name, standing);
                if matches!(
                    standing,
                    Some(Standing::MissingPermission | Standing::RefusedThere)
                ) {
                    self.needs_choice = true;
                    state.stop_sending(host, &chosen);
                    self.reset_gesture();
                } else {
                    let _ = self.end(state, host);
                }
                self.dropped = Some(Dropped {
                    device: chosen,
                    name,
                    said,
                });
                self.chosen = None;
                changed = true;
            }
        }
        if let Some(mut dropped) = self.dropped.take() {
            // Presence may recover on its own, but permission loss is the end of that choice.
            let standing = self
                .devices
                .iter()
                .find(|p| p["device"].as_str() == Some(dropped.device.as_str()))
                .map(|peer| Self::standing(state, peer));
            if standing == Some(Standing::Ready) && !self.needs_choice {
                self.chosen = Some(dropped.device);
                changed = true;
            } else {
                let said = if standing == Some(Standing::Ready) {
                    format!("Select {} again to start control.", dropped.name)
                } else {
                    unselected(&dropped.name, standing)
                };
                changed |= said != dropped.said;
                dropped.said = said;
                self.dropped = Some(dropped);
            }
        }
        if self.chosen.is_none() && self.dropped.is_none() && !self.needs_choice {
            self.chosen = self
                .devices
                .iter()
                .find(|p| Self::standing(state, p) == Standing::Ready)
                .and_then(|p| p["device"].as_str())
                .map(str::to_owned);
        }
        Ok(changed)
    }
    /// Whether the chosen device can be controlled now, by the list last read.
    fn reachable(&self) -> bool {
        self.chosen.as_deref().is_some_and(|chosen| {
            self.devices
                .iter()
                .any(|p| p["device"].as_str() == Some(chosen) && Standing::of(p) == Standing::Ready)
        })
    }
    fn send<H: Wire + Data>(
        &mut self,
        state: &mut State,
        host: &mut H,
        events: Vec<Value>,
        at: u64,
    ) -> Result<bool, &'static str> {
        if events.is_empty() {
            return Ok(false);
        }
        // A device that cannot be controlled is how things are, which the page shows; it is not a failure of each touch.
        // Nothing is sent and whatever was held is let go; the next reading of the list unselects it.
        if !self.reachable() {
            if let Some(peer) = self.chosen.as_deref() {
                if self.devices.iter().any(|row| {
                    row["device"] == peer && Standing::of(row) == Standing::MissingPermission
                }) {
                    self.stop_choice(state, host, Standing::MissingPermission);
                    return Ok(true);
                }
                let _ = self.end(state, host);
            } else {
                self.reset_gesture();
            }
            return Ok(false);
        }
        let peer = self.chosen.as_ref().ok_or(remote::answer::REFUSED)?;
        if state.outgoing.is_none() {
            self.opened = Some(at);
        }
        match state.input(host, &json!({"peer":peer,"events":events,"end":false})) {
            Ok(()) => {}
            Err(Refusal::Other(reason)) => {
                let _ = self.end(state, host);
                return Err(reason);
            }
            Err(Refusal::Blocked(Blocked::MissingPermission(permission))) => {
                if let Some(row) = self
                    .devices
                    .iter_mut()
                    .find(|row| row["device"].as_str() == Some(peer.as_str()))
                {
                    Self::mark_missing(row, &permission);
                }
                self.stop_choice(state, host, Standing::MissingPermission);
                return Ok(true);
            }
            Err(Refusal::Blocked(Blocked::NotAccepted)) => {
                self.stop_choice(state, host, Standing::RefusedThere);
                return Ok(true);
            }
        }
        self.last_input = Some(at);
        Ok(false)
    }
    /// Send what a finger did. At the end of its gesture (it lifted or was cancelled) a failure is not passed on:
    /// the gesture is over either way (`send` already ended the connection, which lets go of everything on the
    /// other side), and the page must be drawn at rest, not left showing a press nobody holds.
    fn send_or_settle<H: Wire + Data>(
        &mut self,
        state: &mut State,
        host: &mut H,
        inputs: Vec<Value>,
        at: u64,
        phase: &str,
    ) -> Result<bool, &'static str> {
        match self.send(state, host, inputs, at) {
            Err(_) if matches!(phase, "up" | "cancel") => Ok(false),
            result => result,
        }
    }
    /// Whether a finger is still on the pad or wheel.
    fn pointer_busy(&self) -> bool {
        self.pad.is_some() || self.wheel.is_some()
    }
    /// Which button a tap at `x` is: left on the left half of the pad, right on its right half.
    fn side(&self, x: i32) -> &'static str {
        match self.pad_bounds {
            Some((left, width))
                if width > 0 && i64::from(x) >= i64::from(left) + i64::from(width) / 2 =>
            {
                "right"
            }
            _ => "left",
        }
    }
    /// Whether anything is held on the other computer: a finger on the pad, the wheel or
    /// Backspace, or a locked button.
    fn holding(&self) -> bool {
        self.pointer_busy() || self.erasing.is_some() || !self.locked.is_empty()
    }
    /// What a drag ending with `button` sends: its release, unless a lock still holds that button.
    fn let_go(&self, button: &'static str) -> Vec<Value> {
        if self.locked.contains(&button) {
            Vec::new()
        } else {
            vec![button_input(button, false)]
        }
    }
    /// A gesture ended with nothing held: the connection is closed so the other computer is free,
    /// and its pointer stays where it is.
    fn rest<H: Wire + Data>(
        &mut self,
        state: &mut State,
        host: &mut H,
    ) -> Result<(), &'static str> {
        if self.holding() {
            return Ok(());
        }
        self.opened = None;
        self.last_input = None;
        self.pad_at = None;
        self.wheel_at = None;
        Self::page_result(state.rest(host))
    }
    fn reset_gesture(&mut self) {
        self.pad = None;
        self.last_tap = None;
        self.wheel = None;
        self.erasing = None;
        self.owed = 0;
        // Ending lets go of everything on the other computer, locked buttons with it.
        self.locked.clear();
        self.last_input = None;
        self.pad_at = None;
        self.wheel_at = None;
        self.opened = None;
    }

    fn end<H: Wire + Data>(&mut self, state: &mut State, host: &mut H) -> Result<(), &'static str> {
        self.reset_gesture();
        Self::page_result(state.finish(host))
    }

    fn page_result(result: Result<(), Refusal>) -> Result<(), &'static str> {
        match result {
            Err(Refusal::Other(reason)) => Err(reason),
            _ => Ok(()), // A typed stop is a state, not a page-event error.
        }
    }
    pub fn event<H: Wire + Data>(
        &mut self,
        state: &mut State,
        host: &mut H,
        event: &Value,
    ) -> Result<Value, &'static str> {
        self.clock = self.clock.max(event["at"].as_u64().unwrap_or(self.clock));
        let at = self.clock;
        let stopped = self.observe_stop(state, host);
        let changed = event["t"] != "open"
            && self.needs_refresh
            && self.refresh(state, host, at).unwrap_or(false);
        let value = self.page_event(state, host, event, at)?;
        Ok(
            if self.observe_stop(state, host) || (stopped || changed) && value.is_null() {
                self.tree()
            } else {
                value
            },
        )
    }

    fn page_event<H: Wire + Data>(
        &mut self,
        state: &mut State,
        host: &mut H,
        event: &Value,
        at: u64,
    ) -> Result<Value, &'static str> {
        let id = event["id"].as_str().unwrap_or("");
        match event["t"].as_str() {
            Some("open") => {
                // A page opened anew starts over: what was said about the last visit is gone.
                self.dropped = None;
                self.refresh(state, host, at)?;
                return Ok(self.tree());
            }
            Some("tick") => {
                // A touch whose up or cancel never arrived must not hold the connection, and with
                // it the other device's button, forever. A press held still is a gesture in
                // progress, so only a long silence ends it.
                let idle = self
                    .last_input
                    .is_some_and(|last| at.saturating_sub(last) >= 2000);
                let silent = |last: Option<u64>| {
                    last.is_some_and(|last| at.saturating_sub(last) >= STUCK_MS)
                };
                let pad_stuck = self.pad.is_some() && silent(self.pad_at);
                let wheel_stuck = self.wheel.is_some() && silent(self.wheel_at);
                // Each control's lost lift ends only that control; the gesture ends when every live control is
                // silent.
                let live = [
                    (self.pad.is_some(), pad_stuck),
                    (self.wheel.is_some(), wheel_stuck),
                ];
                let stuck = live.iter().any(|&(_, quiet)| quiet)
                    && live.iter().all(|&(on, quiet)| !on || quiet);
                let expired_backspace = self
                    .erasing
                    .is_some_and(|down| at.saturating_sub(down) >= STUCK_MS);
                // Ending a gesture nobody holds is housekeeping: the person did nothing that could fail, so a close the
                // other device never acknowledged is not said to them.
                if stuck {
                    // A drag whose finger's lift was lost lets go of its button.
                    if let Some(pad) = self.pad.take().filter(|pad| pad.dragging) {
                        let _ = self.send(state, host, self.let_go(pad.drag_side), at);
                    }
                    let _ = self.end(state, host);
                } else if pad_stuck {
                    // Only the pad's lift was lost: its drag lets go, the wheel turning goes on.
                    if let Some(pad) = self.pad.take().filter(|pad| pad.dragging) {
                        let _ = self.send(state, host, self.let_go(pad.drag_side), at);
                    }
                    self.pad_at = None;
                    return Ok(self.tree());
                } else if wheel_stuck {
                    // Only the wheel's lift was lost: it holds nothing on the other computer, the pad goes on.
                    self.wheel = None;
                    self.wheel_at = None;
                    self.owed = 0;
                    return Ok(self.tree());
                } else if expired_backspace {
                    self.erasing = None;
                    let _ = self.rest(state, host);
                } else if !self.holding() && idle {
                    let _ = self.end(state, host);
                } else if self
                    .erasing
                    .is_some_and(|down| at.saturating_sub(down) >= REPEAT_AFTER_MS)
                {
                    // A repeat is driven by the clock, not a fresh press.
                    if self.send(state, host, vec![json!({"t":"key","key":"backspace"})], at)? {
                        return Ok(self.tree());
                    }
                }
                if self
                    .opened
                    .is_some_and(|opened| at.saturating_sub(opened) >= 5000)
                {
                    self.opened = None;
                    // As after a submit: a rotation that failed leaves nothing held here, whatever the other
                    // computer still holds, and the page says so.
                    match state.rotate(host) {
                        Ok(()) => {}
                        Err(Refusal::Blocked(_)) => {
                            let _ = self.observe_stop(state, host);
                            return Ok(self.tree());
                        }
                        Err(Refusal::Other(_)) => {
                            let _ = self.end(state, host);
                            return Ok(self.tree());
                        }
                    }
                }
                // The list is read again in the background: one read that fails keeps the list as it was.
                let refreshed = self.list_block.is_none()
                    && at.saturating_sub(self.refreshed) >= 2000
                    && self.refresh(state, host, at).unwrap_or(false);
                if stuck || expired_backspace || refreshed {
                    return Ok(self.tree());
                }
            }
            // A device chosen in the application's chooser, as `peersus.peers` names it: chosen only if it can be controlled
            // by the list last read.
            Some("choose") if id == CHOOSER => {
                let device = event["device"]
                    .as_str()
                    .ok_or(remote::answer::NOT_UNDERSTOOD)?;
                if let Some(index) = self
                    .devices
                    .iter()
                    .position(|p| p["device"].as_str() == Some(device))
                {
                    self.choose(state, host, index)?;
                }
                return Ok(self.tree());
            }
            Some("submit") if id == "keyboard" => {
                let value = event["text"]
                    .as_str()
                    .ok_or(remote::answer::NOT_UNDERSTOOD)?;
                let inputs = if value.is_empty() {
                    vec![json!({"t":"key","key":"enter"})]
                } else {
                    // Split to the input service's and the connection's bounds, so a long text arrives whole.
                    remote::typed(value)
                };
                if self.send(state, host, inputs, at)? {
                    return Ok(self.tree());
                }
                if self.holding() {
                    // Close this batch to read its acknowledgement without
                    // releasing the held button or ending another gesture.
                    self.opened = None;
                    match state.rotate(host) {
                        Ok(()) => {}
                        Err(Refusal::Blocked(_)) => {
                            let _ = self.observe_stop(state, host);
                            return Ok(self.tree());
                        }
                        Err(Refusal::Other(reason)) => {
                            let _ = self.end(state, host);
                            return Err(reason);
                        }
                    }
                } else {
                    self.rest(state, host)?;
                }
            }
            // The pad's measured bounds: which half of it a tap lands on says which button it is.
            Some("size") if id == "pad" => {
                let x = event["x"]
                    .as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .ok_or(remote::answer::NOT_UNDERSTOOD)?;
                let width = event["width"]
                    .as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .ok_or(remote::answer::NOT_UNDERSTOOD)?;
                self.pad_bounds = Some((x, width));
                return Ok(Value::Null);
            }
            // The page is no longer seen: nothing is left held on the other computer while nobody can see it.
            Some("visibility") if id == "pad" => {
                let seen = event["visible"].as_bool() == Some(true)
                    && event["foreground"].as_bool() == Some(true);
                if !seen && self.holding() {
                    let _ = self.end(state, host);
                    return Ok(self.tree());
                }
                return Ok(Value::Null);
            }
            Some("pointer") => {
                let phase = event["phase"]
                    .as_str()
                    .ok_or(remote::answer::NOT_UNDERSTOOD)?;
                let x = event["x"]
                    .as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .ok_or(remote::answer::NOT_UNDERSTOOD)?;
                let y = event["y"]
                    .as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .ok_or(remote::answer::NOT_UNDERSTOOD)?;
                let mut inputs = Vec::new();
                match id {
                    // Left and Right lock their button: pressed once it is held on the other computer, pressed
                    // again it is let go. With one locked, a swipe on the pad drags with it.
                    "left" | "right" => {
                        if phase != "down" {
                            return Ok(Value::Null);
                        }
                        let button = if id == "left" { "left" } else { "right" };
                        // A pad drag with the same button already holds it: the lock joins or leaves that
                        // hold without pressing or letting go a second time.
                        let dragging = self
                            .pad
                            .as_ref()
                            .is_some_and(|pad| pad.dragging && pad.drag_side == button);
                        if let Some(index) = self.locked.iter().position(|locked| *locked == button)
                        {
                            self.locked.remove(index);
                            if !dragging {
                                inputs.push(button_input(button, false));
                            }
                        } else {
                            self.locked.push(button);
                            if !dragging {
                                inputs.push(button_input(button, true));
                            }
                        }
                        self.send(state, host, inputs, at)?;
                        let _ = self.rest(state, host);
                        return Ok(self.tree());
                    }
                    // Held, Backspace erases once and then again at every tick, as a keyboard repeats a held key.
                    "backspace" => {
                        match phase {
                            "down" => {
                                self.erasing = Some(at);
                                inputs.push(json!({"t":"key","key":"backspace"}));
                            }
                            "up" | "cancel" => self.erasing = None,
                            _ => return Ok(Value::Null),
                        }
                        self.send_or_settle(state, host, inputs, at, phase)?;
                        if phase != "down" {
                            let _ = self.rest(state, host);
                        }
                        return Ok(self.tree());
                    }
                    // The whole pad is the mouse. A tap clicks with the half it lands on: left on the left half,
                    // right on the right half. A tap and then, soon, a press held and swiped drags with the tapped
                    // button; so does a press held still and then swiped. A swipe alone moves the pointer.
                    "pad" => {
                        match phase {
                            "down" => {
                                self.pad_at = Some(at);
                                let tapped = self
                                    .last_tap
                                    .take()
                                    .filter(|(tap, _)| at.saturating_sub(*tap) <= TAP_THEN_HOLD_MS);
                                let tap_side = self.side(x);
                                self.pad = Some(Pad {
                                    x,
                                    y,
                                    at,
                                    travelled: 0.,
                                    dragging: false,
                                    tap_side,
                                    drag_side: tapped.map_or(tap_side, |(_, side)| side),
                                    armed: tapped.is_some(),
                                });
                            }
                            "move" => {
                                if let Some(pad) = &mut self.pad {
                                    let dx = (f64::from(x) - f64::from(pad.x)) / 1000.;
                                    let dy = (f64::from(y) - f64::from(pad.y)) / 1000.;
                                    if dx != 0. || dy != 0. {
                                        self.pad_at = Some(at);
                                        // Only a press that follows a tap drags; a swipe on its own only moves, however
                                        // long the finger rested before it. With a button locked, the pad only moves.
                                        let starts =
                                            self.locked.is_empty() && !pad.dragging && pad.armed;
                                        if starts {
                                            pad.dragging = true;
                                            inputs.push(button_input(pad.drag_side, true));
                                        }
                                        pad.travelled += dx.hypot(dy);
                                        pad.x = x;
                                        pad.y = y;
                                        inputs
                                            .push(json!({"t":"move","dx":dx * 2.5,"dy":dy * 2.5}));
                                        // The dragging half lights up the moment the drag starts.
                                        if starts {
                                            self.send(state, host, inputs, at)?;
                                            return Ok(self.tree());
                                        }
                                    }
                                }
                            }
                            "up" | "cancel" => {
                                if let Some(pad) = self.pad.take() {
                                    self.pad_at = None;
                                    if pad.dragging {
                                        inputs.extend(self.let_go(pad.drag_side));
                                    } else if self.locked.is_empty()
                                        && phase == "up"
                                        && pad.travelled < TAP_TRAVEL
                                        && at.saturating_sub(pad.at) <= TAP_MS
                                    {
                                        inputs.extend([
                                            button_input(pad.tap_side, true),
                                            button_input(pad.tap_side, false),
                                        ]);
                                        // Only a first tap can arm the next gesture.
                                        if !pad.armed {
                                            self.last_tap = Some((at, pad.tap_side));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                        // Pressed and let go are drawn (the half lights up only while it drags); plain motion is not.
                        if matches!(phase, "down" | "up" | "cancel") {
                            self.send_or_settle(state, host, inputs, at, phase)?;
                            if phase != "down" {
                                let _ = self.rest(state, host);
                            }
                            return Ok(self.tree());
                        }
                    }
                    "wheel" => {
                        match phase {
                            "down" => {
                                self.wheel_at = Some(at);
                                self.wheel = Some(y);
                                self.owed = 0;
                            }
                            "move" => {
                                if let Some(previous) = self.wheel.as_mut() {
                                    let moved = y.saturating_sub(*previous);
                                    *previous = y;
                                    if moved != 0 {
                                        self.wheel_at = Some(at);
                                    }
                                    self.scrolled = self.scrolled.wrapping_add(moved);
                                    self.owed = self.owed.saturating_add(moved);
                                    let notches = self.owed / 24000;
                                    if notches != 0 {
                                        self.owed %= 24000;
                                        inputs.push(json!({"t":"scroll","dx":0,"dy":notches}));
                                    }
                                }
                            }
                            "up" | "cancel" => {
                                self.wheel = None;
                                self.wheel_at = None;
                            }
                            _ => {}
                        }
                        // The wheel is drawn as it turns: its ridges follow the finger.
                        self.send_or_settle(state, host, inputs, at, phase)?;
                        if matches!(phase, "up" | "cancel") {
                            let _ = self.rest(state, host);
                        }
                        return Ok(self.tree());
                    }
                    _ => {}
                }
                if self.send_or_settle(state, host, inputs, at, phase)? {
                    return Ok(self.tree());
                }
                if matches!(phase, "up" | "cancel") {
                    let _ = self.rest(state, host);
                }
            }
            _ => return Err(remote::answer::NOT_UNDERSTOOD),
        }
        // Pointer samples do not invalidate the visual tree or cause frontend snapshot repainting.
        Ok(Value::Null)
    }
    /// Control the device at `index` from now on, if it can be controlled: the gesture on the previous one ends.
    fn choose<H: Wire + Data>(
        &mut self,
        state: &mut State,
        host: &mut H,
        index: usize,
    ) -> Result<(), &'static str> {
        let Some(peer) = self
            .devices
            .get(index)
            .filter(|p| Standing::of(p) == Standing::Ready)
        else {
            return Ok(());
        };
        let chosen = peer["device"].as_str().map(str::to_owned);
        // Best effort: the previous computer may be unreachable and its end may fail, which is no
        // reason to block an explicit switch (a switch is only ever the person's own choice).
        let ended = self.end(state, host);
        crate::host::diagnostic(7, if ended.is_ok() { 0 } else { 4 }, 0, 0, 0);
        if let Some(device) = &chosen {
            state.allow_new_choice(device);
        }
        self.needs_choice = false;
        self.chosen = chosen;
        self.dropped = None;
        Ok(())
    }
    /// The page is the same whether a device is chosen or not: the buttons, the pad and the keyboard keep their
    /// places, and only the chooser says that nothing is chosen and why. What is touched meanwhile goes nowhere.
    pub fn tree(&self) -> Value {
        let selected = self.chosen.as_deref().and_then(|chosen| {
            self.devices
                .iter()
                .find(|p| p["device"].as_str() == Some(chosen))
        });
        // The wheel is a cylinder seen from the side. Its ridges sit at even angles around it, so on screen they
        // are far apart in the middle and crowd toward the top and bottom edge, where they also thin and fade:
        // turning it, they roll over the edge instead of sliding down a flat strip. The surface moves exactly as
        // far as the finger does, and the wheel lights up while it is held.
        let turning = self.wheel.is_some();
        const DRUM: f64 = 38.; // half the visible height, in px: the cylinder's radius
        const STEP: f64 = 0.32; // radians between two ridges
        let turned = f64::from(self.scrolled) / 1000. / DRUM;
        let phase = turned.rem_euclid(STEP);
        let ridges: Vec<Value> = (-6..=6).filter_map(|i| {
            let angle = f64::from(i) * STEP + phase;
            let facing = angle.cos();
            if angle.abs() >= std::f64::consts::FRAC_PI_2 - 0.05 { return None; }
            let thickness = (2.4 * facing).max(0.6);
            let y = DRUM + DRUM * angle.sin() - thickness / 2.;
            Some(bx(None, json!({"width":px(16),"height":{"unit":"px","value":thickness.round().max(1.) as u16},
                "radius":[1,1,1,1],"at":[6000, (y * 1000.).round() as i32],
                "opacity":((facing * facing) * 10000.).round() as u16,
                "background":if turning {"text"} else {"border"}}), vec![]))
        }).collect();
        let wheel = bx(
            Some("wheel"),
            json!({"width":px(48),"height":px(120),"center":true}),
            vec![bx(
                None,
                json!({"width":px(28),"height":px(76),"radius":[14,14,14,14],"clip":true,
                "background":if turning {"accent"} else {"raised"},"border":true}),
                ridges,
            )],
        );
        // Left and Right lock their button; a locked one is lit and says so.
        let lock = |id: &'static str, caption: &str, radius: [u16; 4]| {
            let locked = self.locked.contains(&id);
            bx(
                Some(id),
                json!({"width":flex(),"height":px(120),"center":true,"radius":radius,
                "background":if locked {"accent"} else {"raised"}}),
                vec![control_caption(&if locked {
                    format!("{caption} 🔒")
                } else {
                    caption.to_owned()
                })],
            )
        };
        let buttons = bx(
            None,
            json!({"width":fill(),"horizontal":true,"gap":8,"center":true}),
            vec![
                lock("left", "Left", [130, 8, 8, 8]),
                wheel,
                lock("right", "Right", [8, 130, 8, 8]),
            ],
        );
        // The pad's two halves are its two buttons: a half lights up only while it is held down, dragging.
        let touched = self
            .pad
            .as_ref()
            .filter(|pad| pad.dragging)
            .map(|pad| pad.drag_side);
        let half = |side: &str, caption: &str| {
            bx(
                None,
                json!({"width":flex(),"height":fill(),"center":true,
            "radius":[12,12,12,12],"background":if touched == Some(side) {"raised"} else {"transparent"}}),
                vec![control_caption(caption)],
            )
        };
        let halves = bx(
            None,
            json!({"width":fill(),"height":flex(),"horizontal":true}),
            vec![
                half("left", "Left click"),
                bx(
                    None,
                    json!({"width":px(1),"height":fill(),"background":"border"}),
                    vec![],
                ),
                half("right", "Right click"),
            ],
        );
        let mut pad_content = vec![halves];
        // The chooser is the application's own: the chosen device in a frame, and the menu of the others, those that cannot be
        // controlled dimmed and saying why. Only a device online here, allowed, and where Control is there and allowed to take
        // this device's input, can be chosen: input meant for a person at a sleeping screen does not belong there.
        let mut chooser: Vec<Value> = Vec::new();
        let mut said: Vec<Value> = Vec::new();
        if let Some(blocked) = &self.list_block {
            said.push(text(&format!(
                "Cannot list paired devices: missing permission {} on this device.",
                blocked.name,
            )));
        } else {
            for peer in &self.devices {
                if Standing::of(peer) == Standing::MissingPermission {
                    let name = peer["name"].as_str().unwrap_or("The device");
                    let names = missing_permissions(peer)
                        .map(|(_, name)| name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    said.push(text(&format!(
                        "{name} cannot be controlled: missing permission {names} on this device."
                    )));
                }
            }
            if selected.is_none() {
                let named_blocked = !said.is_empty();
                if let Some(dropped) = &self.dropped
                    && !self.devices.iter().any(|p| {
                        p["device"].as_str() == Some(dropped.device.as_str())
                            && Standing::of(p) == Standing::MissingPermission
                    })
                {
                    said.push(text(&dropped.said));
                }
                if !named_blocked
                    && !self
                        .devices
                        .iter()
                        .any(|p| Standing::of(p) == Standing::Ready)
                {
                    said.push(text("No computer can be controlled now."));
                }
            }
        }
        if !said.is_empty() {
            let scroll = said.len() > 1;
            chooser.push(bx(
                None,
                json!({"width":fill(),"height":if scroll {px(96)} else {json!({"unit":"auto"})},
                    "scroll":scroll,"padding":8,"gap":4,"radius":[12,12,12,12],"background":"surface","border":true}),
                said,
            ));
        }
        chooser.push(
            json!({"k":"devices","id":CHOOSER,"chosen":self.chosen,"above":true,
            "pick":{"presence":["online"],"allowed":true,"accepts":SERVICE}}),
        );
        // Over the pad, against its lower left corner: opening the menu covers the pad, it never squeezes its halves.
        pad_content.push(bx(
            None,
            json!({"overlay":true,"width":fill(),"height":fill(),"bottom":true}),
            vec![bx(None, json!({"gap":4,"padding":4}), chooser)],
        ));
        let mut pad = bx(
            Some("pad"),
            json!({"width":fill(),"height":flex(),"gap":8,"radius":[12,12,12,12]}),
            pad_content,
        );
        // Measured, so a tap knows which half it landed on.
        // Seen leaving (another page, the window hidden or put away): whatever is held is let go.
        pad["observe"] = json!(["size", "visibility"]);
        let surface = pad;
        let erasing = self.erasing.is_some();
        let keyboard = bx(
            None,
            json!({"width":fill(),"height":px(48),"horizontal":true,"gap":8,"center":true}),
            vec![
                bx(
                    None,
                    json!({"width":flex(),"height":px(48)}),
                    vec![
                        json!({"k":"field","id":"keyboard","placeholder":"Type on the computer","value":"","height":48}),
                    ],
                ),
                // A key, as on a keyboard: held, it repeats.
                bx(
                    Some("backspace"),
                    json!({"width":px(56),"height":px(48),"center":true,
                "radius":[12,12,12,12],"background":if erasing {"accent"} else {"raised"}}),
                    vec![json!({"k":"icon","name":"backspace","size":22})],
                ),
            ],
        );
        let content = vec![bx(
            None,
            json!({"width":fill(),"height":flex(),"padding":12,"gap":8,
            "radius":[140,140,32,32],"background":"surface","border":true}),
            vec![buttons, surface, keyboard],
        )];
        json!({"k":"interval","id":"idle","milliseconds":250,"child":
            bx(None,json!({"width":fill(),"height":fill(),"gap":10}),content)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::scripted::{Script, ack};

    // A pointer event is all of these fields; the helper only spells one out.
    #[allow(clippy::too_many_arguments)]
    fn event(
        ui: &mut Ui,
        state: &mut State,
        host: &mut Script,
        id: &str,
        phase: &str,
        x: i32,
        y: i32,
        at: u64,
    ) -> Value {
        ui.event(
            state,
            host,
            &json!({"t":"pointer","id":id,"phase":phase,"x":x,"y":y,"at":at}),
        )
        .unwrap()
    }
    fn ready() -> (Ui, State, Script) {
        let mut host = Script {
            acks: true,
            ..Script::default()
        };
        host.results.push_back(
            json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}),
        );
        let mut ui = Ui::default();
        let mut state = State::default();
        ui.event(&mut state, &mut host, &json!({"t":"open"}))
            .unwrap();
        host.results
            .push_back(json!({"status":"ok","value":{"claim":42}}));
        (ui, state, host)
    }
    #[test]
    fn a_page_allowed_to_see_no_device_opens_with_none_instead_of_failing() {
        let mut host = Script::default();
        host.results
            .push_back(json!({"status":"error","code":"denied","message":"Not allowed"}));
        let mut ui = Ui::default();
        let mut state = State::default();
        assert!(
            ui.event(&mut state, &mut host, &json!({"t":"open"}))
                .is_ok()
        );
        assert!(ui.devices.is_empty());
        // Any other refusal still fails the page, as before.
        host.results
            .push_back(json!({"status":"error","code":"failed","message":"no"}));
        assert!(
            ui.event(&mut state, &mut host, &json!({"t":"open"}))
                .is_err()
        );
    }

    #[test]
    fn each_reachable_computer_names_the_permissions_missing_here_before_any_input() {
        let mut ui = Ui::default();
        let mut state = State::default();
        let mut host = Script::default();
        host.results.push_back(json!({"status":"ok","value":[
            {"device":"main","name":"Desk","presence":"online","allowed":false,"missing_permissions":[{"owner":"control","name":"control.use"}]},
            {"device":"other","name":"Laptop","presence":"online","allowed":false,"missing_permissions":[{"owner":"host","name":"peers.send"}]},
            {"device":"asleep","name":"Sleeping computer","presence":"sleeping","allowed":false,"missing_permissions":[{"owner":"control","name":"control.use"}]}
        ]}));
        let tree = ui
            .event(&mut state, &mut host, &json!({"t":"open"}))
            .unwrap()
            .to_string();
        assert!(
            tree.contains(
                "Desk cannot be controlled: missing permission control.use on this device."
            )
        );
        assert!(tree.contains(
            "Laptop cannot be controlled: missing permission peers.send on this device."
        ));
        assert!(!tree.contains("Sleeping computer"));
        assert!(!tree.contains("No computer can be controlled now."));
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"submit","id":"keyboard","text":"text"}),
        )
        .unwrap();
        assert_eq!(host.sent.len(), 1, "only the readiness read, not a send");
        assert!(host.opened.is_empty());
    }

    #[test]
    fn a_permission_denied_after_the_last_read_redraws_as_blocked_not_as_a_keyboard_failure() {
        let (mut ui, mut state, mut host) = ready();
        host.results.clear();
        host.results.push_back(json!({"status":"missing-permission","permission":{"owner":"host","name":"peers.send"}}));
        let tree = ui
            .event(
                &mut state,
                &mut host,
                &json!({"t":"submit","id":"keyboard","text":"text"}),
            )
            .unwrap()
            .to_string();
        assert!(tree.contains(
            "Computer cannot be controlled: missing permission peers.send on this device."
        ));
        assert!(!tree.contains("No computer can be controlled now."));
        assert!(ui.chosen.is_none());
        assert!(host.opened.is_empty());
    }

    #[test]
    fn a_page_missing_the_watch_permission_names_it_without_guessing_which_devices_it_cannot_see() {
        let mut ui = Ui::default();
        let mut state = State::default();
        let mut host = Script::default();
        host.results.push_back(json!({"status":"missing-permission","permission":{"owner":"host","name":"peers.watch"}}));
        let tree = ui
            .event(&mut state, &mut host, &json!({"t":"open"}))
            .unwrap()
            .to_string();
        assert!(tree.contains("missing permission peers.watch on this device"));
        assert!(!tree.contains("No computer can be controlled now."));
        assert!(ui.devices.is_empty());
        for at in [2000, 4000, 6000] {
            assert!(
                ui.event(&mut state, &mut host, &json!({"t":"tick","at":at}))
                    .unwrap()
                    .is_null()
            );
        }
        assert_eq!(host.sent.len(), 1, "a refused list is not polled");
    }

    #[test]
    fn a_phone_stops_the_active_drag_on_either_grant_loss_and_only_new_choice_reopens_it() {
        for remote in [false, true] {
            let (mut ui, mut state, mut host) = ready();
            event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 10);
            event(
                &mut ui, &mut state, &mut host, "pad", "down", 1000, 1000, 20,
            );
            assert!(ui.holding() && state.outgoing.is_some());
            let bytes = host.written.len();
            let allowed = json!({"device":"scoped","permissions":[
                {"owner":"host","name":"peers.send"},{"owner":"control","name":"control.use"}
            ]});
            let mut config = json!({"plugin":"control","settings":{},"missing_permissions":[],"peers":[allowed]});
            state.placed_pages = Some(vec![]);
            if remote {
                let body = br#"{"status":"not-accepted"}"#;
                let mut frame = (body.len() as u32).to_be_bytes().to_vec();
                frame.extend_from_slice(body);
                host.incoming.push_back(frame);
                assert_eq!(state.next(&mut host), Ok(None));
            } else {
                config["peers"][0]["permissions"] =
                    json!([{"owner":"control","name":"control.use"}]);
                host.results
                    .push_back(json!({"status":"ok","value":config.clone()}));
                remote::handle(
                    &mut host,
                    &mut state,
                    &json!({
                        "type":"event","provider":"host","service":"peersus.config","version":1,"event":"changed","data":{}
                    }),
                );
            }
            let stopped = ui
                .event(&mut state, &mut host, &json!({"t":"tick","at":40}))
                .unwrap();
            assert!(stopped.to_string().contains(if remote {
                "not accepting control"
            } else {
                "missing permission peers.send on this device"
            }));
            assert!(!ui.holding() && ui.chosen.is_none() && state.outgoing.is_none());
            config["peers"][0] = allowed;
            host.results
                .push_back(json!({"status":"ok","value":config}));
            state.configure_pages(&mut host).unwrap();
            host.results.push_back(json!({"status":"ok","value":[
                {"device":"scoped","name":"Computer","presence":"online","allowed":true,"accepts":[SERVICE]}
            ]}));
            ui.event(&mut state, &mut host, &json!({"t":"tick","at":2500}))
                .unwrap();
            event(
                &mut ui, &mut state, &mut host, "pad", "up", 2000, 1000, 2510,
            );
            ui.event(
                &mut state,
                &mut host,
                &json!({"t":"submit","id":"keyboard","text":"discarded"}),
            )
            .unwrap();
            assert_eq!(host.written.len(), bytes);
            assert_eq!(host.opened, [(true, 42)]);
            assert_eq!(host.shutdowns, 0);
            ui.event(
                &mut state,
                &mut host,
                &json!({"t":"choose","id":CHOOSER,"device":"scoped"}),
            )
            .unwrap();
            claim(&mut host);
            ui.event(
                &mut state,
                &mut host,
                &json!({"t":"submit","id":"keyboard","text":"fresh"}),
            )
            .unwrap();
            assert_eq!(host.opened, [(true, 42), (true, 43)]);
            assert!(String::from_utf8_lossy(&host.written[bytes..]).contains("fresh"));
        }
    }
    fn written(host: &Script) -> Vec<Value> {
        String::from_utf8(host.written.clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|line: &Value| *line != json!({"rests":true}))
            .collect()
    }
    fn rests(host: &Script) -> usize {
        String::from_utf8(host.written.clone())
            .unwrap()
            .lines()
            .filter(|l| *l == remote::RESTS)
            .count()
    }
    fn claim(host: &mut Script) {
        host.results
            .push_back(json!({"status":"ok","value":{"claim":43}}));
    }

    #[test]
    fn a_stationary_wheel_press_expires_and_does_not_hold_open_the_keyboard_stream() {
        let (mut ui, mut state, mut host) = ready();
        host.results.clear(); // a stationary wheel press must not open a connection
        event(&mut ui, &mut state, &mut host, "wheel", "down", 0, 0, 100);
        host.results
            .push_back(json!({"status":"ok","value":ui.devices}));
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","at":100 + STUCK_MS - 1}),
        )
        .unwrap();
        assert!(ui.holding(), "the quiet press is still within its lease");
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","at":100 + STUCK_MS}),
        )
        .unwrap();
        assert!(!ui.holding(), "a lost lift expires without sending input");
        assert!(host.opened.is_empty());
        event(
            &mut ui,
            &mut state,
            &mut host,
            "wheel",
            "move",
            0,
            1000,
            101 + STUCK_MS,
        );
        assert!(!ui.holding(), "a late move cannot begin a new wheel press");
        claim(&mut host);
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"submit","id":"keyboard","text":"a"}),
        )
        .unwrap();
        assert_eq!(rests(&host), 1);
        assert!(state.outgoing.is_none());
    }

    #[test]
    fn a_drag_whose_lift_never_arrives_is_let_go_and_its_connection_closed() {
        let (mut ui, mut state, mut host) = ready();
        event(
            &mut ui, &mut state, &mut host, "pad", "down", 1000, 1000, 80,
        );
        event(&mut ui, &mut state, &mut host, "pad", "up", 1000, 1000, 90);
        claim(&mut host);
        event(
            &mut ui, &mut state, &mut host, "pad", "down", 1000, 1000, 100,
        );
        event(
            &mut ui, &mut state, &mut host, "pad", "move", 2000, 1000, 120,
        );
        assert!(
            ui.pad.as_ref().is_some_and(|pad| pad.dragging),
            "tapped, then pressed and swiped: a drag"
        );
        host.results.push_back(
            json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}),
        );
        ui.event(&mut state, &mut host, &json!({"t":"tick","at":3000}))
            .unwrap();
        assert!(
            ui.pad.is_some(),
            "a drag held still is a gesture in progress"
        );
        host.results.push_back(
            json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}),
        );
        let held = ui.tree();
        let settled = ui
            .event(
                &mut state,
                &mut host,
                &json!({"t":"tick","at":600 + STUCK_MS}),
            )
            .unwrap();
        assert_ne!(settled, held);
        assert_eq!(
            settled,
            ui.tree(),
            "return the resting tree with no peer changes"
        );
        assert_eq!(
            host.sent.last().unwrap()["method"],
            "list",
            "still refresh normally"
        );
        assert!(state.outgoing.is_none());
        assert!(ui.pad.is_none());
        assert_eq!(written(&host).last(), Some(&button_input("left", false)));
    }

    fn choose_event(ui: &mut Ui, state: &mut State, host: &mut Script, device: &str) -> Value {
        ui.event(
            state,
            host,
            &json!({"t":"choose","id":CHOOSER,"device":device}),
        )
        .unwrap()
    }

    #[test]
    fn the_chooser_is_the_applications_and_choosing_sends_no_mouse_click() {
        let (mut ui, mut state, mut host) = ready();
        ui.devices
            .push(json!({"device":"offline","name":"Offline","online":false}));
        ui.devices
            .push(json!({"device":"other","name":"Other","online":true}));
        let tree = ui.tree();
        let pad = &tree["child"]["children"][0]["children"][1];
        assert_eq!(pad["id"], "pad");
        assert_eq!(
            pad["observe"],
            json!(["size", "visibility"]),
            "measured, so a tap knows its half, and seen, so leaving lets go of what is held"
        );
        let over = pad["children"].as_array().unwrap().last().unwrap();
        assert_eq!(
            over["style"]["overlay"], true,
            "the chooser floats over the pad instead of taking room in it"
        );
        // The application draws it: only an online device, allowed, where Control takes this device's input. Sleeping is not
        // offered: input meant for a person at a screen does not belong at a sleeping one.
        let chooser = over["children"][0]["children"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(
            chooser,
            &json!({"k":"devices","id":CHOOSER,"chosen":"scoped","above":true,
                "pick":{"presence":["online"],"allowed":true,"accepts":SERVICE}})
        );
        // The page draws no list of its own.
        assert!(!tree.to_string().contains("\"device-"));
        choose_event(&mut ui, &mut state, &mut host, "other");
        assert_eq!(ui.chosen.as_deref(), Some("other"));
        assert!(host.opened.is_empty(), "choosing is not remote input");
        event(&mut ui, &mut state, &mut host, "pad", "down", 0, 0, 30);
        event(&mut ui, &mut state, &mut host, "pad", "up", 0, 0, 40);
        assert_eq!(
            written(&host),
            vec![button_input("left", true), button_input("left", false)]
        );
    }

    #[test]
    fn a_choice_of_a_device_that_cannot_be_controlled_changes_nothing() {
        let (mut ui, mut state, mut host) = ready();
        ui.devices
            .push(json!({"device":"offline","name":"Offline","online":false}));
        ui.devices.push(
            json!({"device":"asleep","name":"Asleep","online":true,"presence":"sleeping","allowed":true}),
        );
        for device in ["offline", "asleep", "unknown"] {
            choose_event(&mut ui, &mut state, &mut host, device);
            assert_eq!(ui.chosen.as_deref(), Some("scoped"), "{device}");
        }
        assert!(host.opened.is_empty() && host.written.is_empty());
    }

    #[test]
    fn a_device_is_controlled_only_where_control_takes_this_devices_input() {
        let peer = |accepts: Value| {
            let mut peer =
                json!({"device":"d","name":"D","online":true,"presence":"online","allowed":true});
            if !accepts.is_null() {
                peer["accepts"] = accepts;
            }
            peer
        };
        assert_eq!(Standing::of(&peer(json!([SERVICE]))), Standing::Ready);
        // Control is not there, not running or not allowed there.
        assert_eq!(Standing::of(&peer(json!([]))), Standing::NotThere);
        // Not said (an older host or device): the device refuses the call itself when Control is not there.
        assert_eq!(Standing::of(&peer(Value::Null)), Standing::Ready);
        assert_eq!(
            unselected("Desk", Some(Standing::NotThere)),
            "Desk was unselected: Control is not ready there."
        );
    }

    #[test]
    fn a_locked_left_drags_with_the_pad_and_only_its_key_lets_it_go() {
        let (mut ui, mut state, mut host) = ready();
        assert!(!event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 10).is_null());
        event(&mut ui, &mut state, &mut host, "left", "up", 0, 0, 20);
        assert_eq!(
            ui.locked,
            ["left"],
            "pressed once: locked, and lifting the finger does not unlock it"
        );
        // With Left locked, the pad only moves: no click of its own, and lifting it lets nothing go.
        event(
            &mut ui, &mut state, &mut host, "pad", "down", 1000, 1000, 30,
        );
        event(
            &mut ui, &mut state, &mut host, "pad", "move", 9000, 1000, 40,
        );
        event(&mut ui, &mut state, &mut host, "pad", "up", 9000, 1000, 60);
        assert_eq!(ui.locked, ["left"]);
        assert!(state.outgoing.is_some(), "the drag is still in progress");
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 700);
        assert!(ui.locked.is_empty(), "pressed again: let go");
        let events = written(&host);
        assert_eq!(events.first(), Some(&button_input("left", true)));
        assert_eq!(events.last(), Some(&button_input("left", false)));
        assert_eq!(
            events.iter().filter(|e| e["t"] == "button").count(),
            2,
            "one press, one release: {events:?}"
        );
        assert!(events.iter().any(|e| e["t"] == "move"));
    }

    #[test]
    fn extension_interprets_motion_and_taps_without_rebuilding_the_tree() {
        let (mut ui, mut state, mut host) = ready();
        assert!(
            !event(
                &mut ui, &mut state, &mut host, "pad", "down", 1000, 2000, 10
            )
            .is_null(),
            "touched: the pad lights up"
        );
        assert_eq!(
            event(
                &mut ui, &mut state, &mut host, "pad", "move", 11000, 22000, 20
            ),
            Value::Null,
            "motion is not redrawn"
        );
        event(
            &mut ui, &mut state, &mut host, "pad", "up", 11000, 22000, 30,
        );
        assert_eq!(
            written(&host),
            vec![json!({"t":"move","dx":25.0,"dy":50.0})]
        );
        // The gesture ended: said, and the connection closed so the computer is free meanwhile.
        assert_eq!(rests(&host), 1);
        assert!(state.outgoing.is_none());
        claim(&mut host);
        event(
            &mut ui, &mut state, &mut host, "pad", "down", 1000, 2000, 40,
        );
        event(&mut ui, &mut state, &mut host, "pad", "up", 1000, 2000, 50);
        assert_eq!(
            &written(&host)[1..],
            &[button_input("left", true), button_input("left", false)]
        );
        assert_eq!(host.opened.len(), 2);
    }

    #[test]
    fn a_tap_clicks_with_the_half_it_lands_on_and_cancel_releases_a_drag() {
        let (mut ui, mut state, mut host) = ready();
        // The pad spans x 0..100 px: its right half begins at 50 px.
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"size","id":"pad","x":0,"y":0,"width":100_000,"height":80_000}),
        )
        .unwrap();
        event(
            &mut ui, &mut state, &mut host, "pad", "down", 80_000, 1000, 10,
        );
        event(
            &mut ui, &mut state, &mut host, "pad", "up", 80_000, 1000, 20,
        );
        claim(&mut host);
        // Resting long before swiping is still only a swipe: it moves and never presses.
        event(
            &mut ui, &mut state, &mut host, "pad", "down", 10_000, 0, 1000,
        );
        assert!(
            event(
                &mut ui, &mut state, &mut host, "pad", "move", 11_000, 0, 2000
            )
            .is_null(),
            "a swipe is not drawn as a press"
        );
        event(&mut ui, &mut state, &mut host, "pad", "up", 11_000, 0, 2100);
        // A tap on the left half, then at once pressed and swiped: a left drag, which cancel lets go.
        claim(&mut host);
        event(
            &mut ui, &mut state, &mut host, "pad", "down", 10_000, 0, 3000,
        );
        event(&mut ui, &mut state, &mut host, "pad", "up", 10_000, 0, 3010);
        claim(&mut host);
        event(
            &mut ui, &mut state, &mut host, "pad", "down", 10_000, 0, 3100,
        );
        assert!(
            !event(
                &mut ui, &mut state, &mut host, "pad", "move", 12_000, 0, 3116
            )
            .is_null(),
            "the dragging half lights up"
        );
        event(&mut ui, &mut state, &mut host, "pad", "cancel", 0, 0, 3200);
        let buttons: Vec<Value> = written(&host)
            .into_iter()
            .filter(|e| e["t"] == "button")
            .collect();
        assert_eq!(
            buttons,
            vec![
                button_input("right", true),
                button_input("right", false),
                button_input("left", true),
                button_input("left", false),
                button_input("left", true),
                button_input("left", false)
            ]
        );
    }

    #[test]
    fn wheel_accumulates_fractional_notches_and_idle_closes_the_connection() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui, &mut state, &mut host, "wheel", "down", 0, 0, 10);
        event(
            &mut ui, &mut state, &mut host, "wheel", "move", 0, 12000, 20,
        );
        assert!(host.written.is_empty());
        event(
            &mut ui, &mut state, &mut host, "wheel", "move", 0, 36000, 30,
        );
        assert_eq!(written(&host), vec![json!({"t":"scroll","dx":0,"dy":1})]);
        event(&mut ui, &mut state, &mut host, "wheel", "up", 0, 36000, 35);
        host.incoming.push_back(ack(host.written.len() as u64));
        host.results.push_back(
            json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}),
        );
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","id":"idle","at":2030}),
        )
        .unwrap();
        assert!(state.outgoing.is_none());
        assert_eq!(host.shutdowns, 1);
    }

    #[test]
    fn keyboard_uses_the_last_clock_sample_and_a_locked_button_does_not_expire() {
        let (mut ui, mut state, mut host) = ready();
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","id":"idle","at":1500}),
        )
        .unwrap();
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"submit","id":"keyboard","text":"a"}),
        )
        .unwrap();
        assert!(
            state.outgoing.is_none(),
            "typed: nothing held, the connection is closed"
        );
        assert_eq!(host.shutdowns, 1);
        claim(&mut host);
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 1800);
        host.results.push_back(
            json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}),
        );
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","id":"idle","at":4000}),
        )
        .unwrap();
        assert!(state.outgoing.is_some(), "a locked button does not expire");
        assert_eq!(
            written(&host),
            vec![json!({"t":"text","text":"a"}), button_input("left", true)]
        );
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 4100);
        assert!(state.outgoing.is_none());
        assert_eq!(host.shutdowns, 2);
    }

    #[test]
    fn keyboard_submission_reports_missing_or_short_acknowledgement() {
        for short in [false, true] {
            let (mut ui, mut state, mut host) = ready();
            host.acks = false;
            host.silent = !short;
            if short {
                host.incoming.push_back(ack(0));
            }
            let submit = json!({"t":"submit","id":"keyboard","text":"abc"});
            assert_eq!(
                ui.event(&mut state, &mut host, &submit),
                Err(remote::answer::STOPPED)
            );
            assert_eq!(written(&host), vec![json!({"t":"text","text":"abc"})]);
            assert_eq!(host.closed, 1);
            assert!(state.outgoing.is_none());
        }
    }

    #[test]
    fn keyboard_submission_awaits_acknowledgement_without_releasing_a_locked_button() {
        for confirmed in [false, true] {
            let (mut ui, mut state, mut host) = ready();
            event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 10);
            host.acks = confirmed;
            host.silent = !confirmed;
            let result = ui.event(
                &mut state,
                &mut host,
                &json!({"t":"submit","id":"keyboard","text":"abc","at":11}),
            );
            if confirmed {
                assert!(result.is_ok());
                assert_eq!(ui.locked, ["left"]);
            } else {
                assert_eq!(result, Err(remote::answer::STOPPED));
                assert!(ui.locked.is_empty());
            }
            assert!(state.outgoing.is_none());
            assert_eq!(host.shutdowns, 1);
            assert_eq!(
                written(&host),
                [
                    button_input("left", true),
                    json!({"t":"text","text":"abc"}),
                    json!({"continues":true}),
                ]
            );
        }
    }

    #[test]
    fn wheel_lift_and_cancellation_redraw_at_rest_even_without_an_acknowledgement() {
        for phase in ["up", "cancel"] {
            let (mut ui, mut state, mut host) = ready();
            host.acks = false;
            event(&mut ui, &mut state, &mut host, "wheel", "down", 0, 0, 10);
            let held = event(
                &mut ui, &mut state, &mut host, "wheel", "move", 0, 24_000, 20,
            );
            host.incoming.push_back(ack(0));
            let settled = event(
                &mut ui, &mut state, &mut host, "wheel", phase, 0, 24_000, 30,
            );
            assert_ne!(settled, held);
            assert_eq!(settled, ui.tree());
            assert!(!ui.holding());
            assert!(state.outgoing.is_none());
        }
    }

    #[test]
    fn left_and_right_lock_together_and_each_key_lets_its_own_go() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 10);
        event(&mut ui, &mut state, &mut host, "right", "down", 0, 0, 20);
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 30);
        assert_eq!(ui.locked, ["right"]);
        assert!(state.outgoing.is_some(), "Right still locked");
        event(&mut ui, &mut state, &mut host, "right", "down", 0, 0, 40);
        assert!(ui.locked.is_empty());
        assert_eq!(
            written(&host),
            vec![
                button_input("left", true),
                button_input("right", true),
                button_input("left", false),
                button_input("right", false)
            ]
        );
    }

    #[test]
    fn a_press_held_still_rotates_its_connection_before_the_other_side_gives_up() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 100);
        host.results.push_back(
            json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}),
        );
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","id":"idle","at":5200}),
        )
        .unwrap();
        let lines = String::from_utf8(host.written.clone()).unwrap();
        assert!(
            lines.lines().any(|l| l == remote::CONTINUES),
            "rotated, keeping the press: {lines}"
        );
        assert!(state.outgoing.is_none());
        assert_eq!(ui.locked, ["left"]);
    }

    #[test]
    fn a_chosen_device_that_goes_offline_is_unselected_and_said_so_and_nothing_takes_its_place() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 10);
        host.incoming.push_back(ack(host.written.len() as u64));
        let list = json!([
            {"device":"scoped","name":"Computer","online":false},
            {"device":"another","name":"Other","online":true}
        ]);
        host.results
            .push_back(json!({"status":"ok","value":list.clone()}));
        let page = ui
            .event(
                &mut state,
                &mut host,
                &json!({"t":"tick","id":"idle","at":2010}),
            )
            .unwrap();
        assert_eq!(ui.chosen, None, "never redirected to the other device");
        assert!(ui.locked.is_empty());
        assert!(state.outgoing.is_none());
        assert_eq!(host.shutdowns, 1);
        let drawn = page.to_string();
        assert!(drawn.contains("Computer was unselected: it is offline."));
        // The page is the one it was: the buttons, the pad and the keyboard in their places, the chooser closed.
        let surface = &page["child"]["children"][0]["children"];
        assert_eq!(surface.as_array().map(Vec::len), Some(3));
        assert_eq!(surface[0]["children"][1]["id"], "wheel");
        assert_eq!(surface[1]["id"], "pad");
        assert!(drawn.contains("\"keyboard\"") && drawn.contains("\"backspace\""));
        assert!(drawn.contains("\"k\":\"devices\"") && !drawn.contains("\"chosen\":\"scoped\""));
        // A press on the pad or on a button is drawn at rest and sends nothing.
        let before = host.written.len();
        let pressed = event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 2020);
        assert!(ui.locked.is_empty() && !pressed.to_string().contains("🔒"));
        event(&mut ui, &mut state, &mut host, "pad", "down", 0, 0, 2030);
        event(&mut ui, &mut state, &mut host, "pad", "up", 0, 0, 2040);
        assert!(ui.pad.is_none());
        assert_eq!(host.written.len(), before);
        // Typing now sends nothing and fails nothing.
        let before = host.written.len();
        assert!(
            ui.event(
                &mut state,
                &mut host,
                &json!({"t":"submit","id":"keyboard","text":"a"})
            )
            .is_ok()
        );
        assert_eq!(host.written.len(), before);
        assert_eq!(host.opened.len(), 1);
        // Read again with nothing changed: still nothing is chosen for the person.
        host.results.push_back(json!({"status":"ok","value":list}));
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","id":"idle","at":4020}),
        )
        .unwrap();
        assert_eq!(ui.chosen, None);
        // Chosen in the application's chooser: one that cannot be controlled does nothing; the other one is chosen.
        choose_event(&mut ui, &mut state, &mut host, "scoped");
        assert_eq!(ui.chosen, None);
        choose_event(&mut ui, &mut state, &mut host, "another");
        assert_eq!(ui.chosen.as_deref(), Some("another"));
        let drawn = ui.tree().to_string();
        assert!(drawn.contains("\"pad\"") && !drawn.contains("was unselected"));
    }

    #[test]
    fn a_removed_destination_is_unselected_and_not_replaced() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 10);
        host.incoming.push_back(ack(host.written.len() as u64));
        host.results.push_back(json!({"status":"ok","value":[
            {"device":"another","name":"Other","online":true}
        ]}));
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","id":"idle","at":2010}),
        )
        .unwrap();
        assert_eq!(ui.chosen, None);
        assert!(ui.locked.is_empty());
        assert!(state.outgoing.is_none());
        let drawn = ui.tree().to_string();
        assert!(drawn.contains("Computer was unselected: it is no longer paired."));
        assert!(drawn.contains("\"pad\"") && !drawn.contains("\"device-"));
        assert!(
            drawn.contains("\"k\":\"devices\""),
            "the chooser is still there to choose another"
        );
        // Opening the page anew starts over: the one device that can be controlled is chosen.
        host.results.push_back(json!({"status":"ok","value":[
            {"device":"another","name":"Other","online":true}
        ]}));
        ui.event(&mut state, &mut host, &json!({"t":"open","at":2020}))
            .unwrap();
        assert_eq!(ui.chosen.as_deref(), Some("another"));
    }

    #[test]
    fn only_a_device_that_is_online_and_allows_control_can_be_chosen() {
        let peer = |presence: Option<&str>, online: bool, allowed: Option<bool>| {
            let mut peer = json!({"device":"d","name":"D","online":online});
            if let Some(presence) = presence {
                peer["presence"] = json!(presence);
            }
            if let Some(allowed) = allowed {
                peer["allowed"] = json!(allowed);
            }
            Standing::of(&peer)
        };
        assert_eq!(peer(Some("online"), true, Some(true)), Standing::Ready);
        assert_eq!(
            peer(Some("online"), true, Some(false)),
            Standing::NotAllowed
        );
        assert_eq!(peer(Some("sleeping"), true, Some(true)), Standing::Sleeping);
        assert_eq!(peer(Some("offline"), false, Some(true)), Standing::Offline);
        assert_eq!(
            peer(Some("busy"), true, Some(true)),
            Standing::Away,
            "a presence with no word here is not one to control"
        );
        // An older host says only who is online.
        assert_eq!(peer(None, true, None), Standing::Ready);
        assert_eq!(peer(None, false, None), Standing::Offline);
    }

    #[test]
    fn only_the_ready_devices_can_be_chosen_whatever_is_asked() {
        let (mut ui, mut state, mut host) = ready();
        for (device, presence, allowed) in [
            ("asleep", "sleeping", true),
            ("refused", "online", false),
            ("gone", "offline", true),
            ("fine", "online", true),
        ] {
            ui.devices.push(
                json!({"device":device,"name":device,"online":presence != "offline",
                "presence":presence,"allowed":allowed}),
            );
        }
        // Asked for by name or by its place all the same, one that cannot be controlled is not chosen.
        for device in ["asleep", "refused", "gone"] {
            choose_event(&mut ui, &mut state, &mut host, device);
            assert_eq!(ui.chosen.as_deref(), Some("scoped"), "{device}");
        }
        for index in 1..=3 {
            ui.choose(&mut state, &mut host, index).unwrap();
            assert_eq!(ui.chosen.as_deref(), Some("scoped"));
        }
        choose_event(&mut ui, &mut state, &mut host, "fine");
        assert_eq!(ui.chosen.as_deref(), Some("fine"));
    }

    #[test]
    fn a_chosen_device_whose_permission_is_taken_away_is_unselected() {
        let (mut ui, mut state, mut host) = ready();
        // No gesture comes first here, so the answer the fixture queued for one is not what is read next.
        host.results.clear();
        host.results.push_back(json!({"status":"ok","value":[
            {"device":"scoped","name":"Computer","online":true,"presence":"online","allowed":false}
        ]}));
        let page = ui
            .event(
                &mut state,
                &mut host,
                &json!({"t":"tick","id":"idle","at":2010}),
            )
            .unwrap()
            .to_string();
        assert_eq!(ui.chosen, None);
        assert!(page.contains("Computer was unselected: Control is no longer allowed there."));
        assert!(page.contains("No computer can be controlled now."));
        // It goes to sleep meanwhile: the page says how it stands now.
        host.results.push_back(json!({"status":"ok","value":[
            {"device":"scoped","name":"Computer","online":false,"presence":"sleeping","allowed":true},
            {"device":"another","name":"Other","online":true,"presence":"online","allowed":true}
        ]}));
        let page = ui
            .event(
                &mut state,
                &mut host,
                &json!({"t":"tick","id":"idle","at":4020}),
            )
            .unwrap()
            .to_string();
        assert_eq!(ui.chosen, None, "never redirected to the other device");
        assert!(page.contains("Computer was unselected: it is sleeping."));
        assert!(!page.contains("no longer allowed"));
        // Back, and allowed: it is the person's own choice and is chosen again, with nothing said.
        host.results.push_back(json!({"status":"ok","value":[
            {"device":"scoped","name":"Computer","online":true,"presence":"online","allowed":true},
            {"device":"another","name":"Other","online":true,"presence":"online","allowed":true}
        ]}));
        let page = ui
            .event(
                &mut state,
                &mut host,
                &json!({"t":"tick","id":"idle","at":6030}),
            )
            .unwrap()
            .to_string();
        assert_eq!(ui.chosen.as_deref(), Some("scoped"));
        assert!(!page.contains("was unselected") && !page.contains("Choose a device"));
        assert!(
            host.opened.is_empty(),
            "coming back sends nothing by itself"
        );
    }

    #[test]
    fn failed_destination_clears_a_new_button_press_and_closes_the_old_stream() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui, &mut state, &mut host, "pad", "down", 0, 0, 10);
        event(&mut ui, &mut state, &mut host, "pad", "move", 10000, 0, 20);
        ui.devices[0]["online"] = json!(false);
        host.incoming.push_back(ack(host.written.len() as u64));
        let before = host.written.len();
        assert!(
            ui.event(
                &mut state,
                &mut host,
                &json!({"t":"pointer","id":"left","phase":"down","x":0,"y":0,"at":30})
            )
            .is_ok()
        );
        assert_eq!(
            host.written.len(),
            before,
            "nothing is sent to a computer that is offline"
        );
        assert!(ui.locked.is_empty());
        assert!(ui.pad.is_none());
        assert!(state.outgoing.is_none());
        assert_eq!(host.shutdowns, 1);
        assert_eq!(host.opened.len(), 1);
        assert_eq!(written(&host), vec![json!({"t":"move","dx":25.0,"dy":0.0})]);
    }

    #[test]
    fn choosing_another_device_is_not_blocked_by_an_unreachable_previous_one() {
        let (mut ui, mut state, mut host) = ready();
        // One tap: a stream to the chosen computer, ended with the pointer left where it was.
        event(&mut ui, &mut state, &mut host, "pad", "down", 0, 0, 10);
        event(&mut ui, &mut state, &mut host, "pad", "up", 0, 0, 20);
        ui.devices
            .push(json!({"device":"other","name":"Other","online":true}));
        // The previous computer is unreachable: its release is refused, the switch still happens.
        host.results
            .push_back(json!({"status":"error","code":"failed","message":"offline"}));
        ui.choose(&mut state, &mut host, 1).unwrap();
        assert_eq!(ui.chosen.as_deref(), Some("other"));
        // And the next gesture goes to exactly the new device, never replayed to the old one.
        host.results
            .push_back(json!({"status":"ok","value":{"claim":44}}));
        event(&mut ui, &mut state, &mut host, "pad", "down", 0, 0, 30);
        event(&mut ui, &mut state, &mut host, "pad", "up", 0, 0, 40);
        let targets: Vec<&str> = host
            .sent
            .iter()
            .filter(|m| m["method"] == "send")
            .filter_map(|m| m["args"]["device"].as_str())
            .collect();
        assert_eq!(
            targets,
            ["scoped", "scoped", "other"],
            "gesture stream, refused release, new stream"
        );
        // The forgotten release is not attempted again on the next idle tick; only the device
        // that still holds a pointer gets its own best-effort release.
        host.results
            .push_back(json!({"status":"ok","value":{"claim":45}}));
        ui.end(&mut state, &mut host).unwrap();
        host.results
            .push_back(json!({"status":"ok","value":ui.devices.clone()}));
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","id":"idle","at":2100}),
        )
        .unwrap();
        let targets: Vec<&str> = host
            .sent
            .iter()
            .filter(|m| m["method"] == "send")
            .filter_map(|m| m["args"]["device"].as_str())
            .collect();
        assert_eq!(targets, ["scoped", "scoped", "other", "other"]);
    }

    #[test]
    fn a_refused_leave_is_not_retried_on_every_idle_tick() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui, &mut state, &mut host, "pad", "down", 0, 0, 10);
        event(&mut ui, &mut state, &mut host, "pad", "up", 0, 0, 20);
        host.results
            .push_back(json!({"status":"error","code":"failed","message":"offline"}));
        ui.end(&mut state, &mut host).unwrap();
        host.results
            .push_back(json!({"status":"ok","value":ui.devices.clone()}));
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","id":"idle","at":2100}),
        )
        .unwrap();
        let sends = host.sent.iter().filter(|m| m["method"] == "send").count();
        assert_eq!(sends, 2, "the gesture stream plus the refused release");
        host.results
            .push_back(json!({"status":"ok","value":ui.devices.clone()}));
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","id":"idle","at":4300}),
        )
        .unwrap();
        assert_eq!(
            host.sent.iter().filter(|m| m["method"] == "send").count(),
            sends,
            "a stale end is not repaid to the same device at every tick"
        );
    }

    #[test]
    fn consecutive_stationary_taps_click_their_own_halves() {
        let (mut ui, mut state, mut host) = ready();
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"size","id":"pad","x":0,"y":0,"width":100_000,"height":80_000}),
        )
        .unwrap();
        event(&mut ui, &mut state, &mut host, "pad", "down", 90_000, 0, 10);
        event(&mut ui, &mut state, &mut host, "pad", "up", 90_000, 0, 20);
        claim(&mut host);
        event(&mut ui, &mut state, &mut host, "pad", "down", 10_000, 0, 30);
        event(&mut ui, &mut state, &mut host, "pad", "up", 10_000, 0, 40);
        assert_eq!(
            written(&host),
            [
                button_input("right", true),
                button_input("right", false),
                button_input("left", true),
                button_input("left", false),
            ]
        );
    }

    /// A left drag under way: tapped on the left half, pressed again at once and swiped.
    fn left_drag(ui: &mut Ui, state: &mut State, host: &mut Script) {
        event(ui, state, host, "pad", "down", 1000, 1000, 80);
        event(ui, state, host, "pad", "up", 1000, 1000, 90);
        claim(host);
        event(ui, state, host, "pad", "down", 1000, 1000, 100);
        event(ui, state, host, "pad", "move", 2000, 1000, 120);
        assert!(ui.pad.as_ref().is_some_and(|pad| pad.dragging));
    }
    fn buttons(host: &Script) -> Vec<Value> {
        written(host)
            .into_iter()
            .filter(|e| e["t"] == "button")
            .collect()
    }

    #[test]
    fn a_lock_taken_during_a_drag_keeps_the_button_down_when_the_finger_lifts() {
        let (mut ui, mut state, mut host) = ready();
        left_drag(&mut ui, &mut state, &mut host);
        let before = buttons(&host).len();
        // The drag already holds left: the lock joins it without a second press, and the lift leaves it down.
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 130);
        event(&mut ui, &mut state, &mut host, "pad", "up", 2000, 1000, 140);
        assert_eq!(
            buttons(&host).len(),
            before,
            "no press or release while one of them holds it"
        );
        assert_eq!(ui.locked, ["left"]);
        // The lock's own key lets it go.
        claim(&mut host);
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 150);
        assert_eq!(buttons(&host)[before..], [button_input("left", false)]);
    }

    #[test]
    fn a_lost_pad_lift_expires_on_its_own_silence_while_the_wheel_turns() {
        let (mut ui, mut state, mut host) = ready();
        left_drag(&mut ui, &mut state, &mut host);
        event(&mut ui, &mut state, &mut host, "wheel", "down", 0, 0, 200);
        // The wheel keeps turning; the pad's finger has said nothing since 120.
        for at in [10_000, 20_000, 30_000] {
            event(
                &mut ui,
                &mut state,
                &mut host,
                "wheel",
                "move",
                0,
                (at / 10) as i32,
                at,
            );
        }
        host.results.push_back(
            json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}),
        );
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"tick","at":120 + STUCK_MS}),
        )
        .unwrap();
        assert!(ui.pad.is_none(), "the pad's drag is let go");
        assert_eq!(buttons(&host).last(), Some(&button_input("left", false)));
        assert!(ui.wheel.is_some(), "the wheel turning goes on");
    }

    #[test]
    fn leaving_the_page_lets_go_of_a_locked_button() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 100);
        assert!(ui.holding());
        // Still seen: nothing changes.
        let seen = json!({"t":"visibility","id":"pad","visible":true,"foreground":true});
        assert!(ui.event(&mut state, &mut host, &seen).unwrap().is_null());
        assert!(ui.holding());
        // The page is no longer seen: the hold ends and the connection with it.
        let gone = json!({"t":"visibility","id":"pad","visible":false,"foreground":false});
        ui.event(&mut state, &mut host, &gone).unwrap();
        assert!(!ui.holding() && ui.locked.is_empty());
        assert!(state.outgoing.is_none());
    }

    #[test]
    fn a_failed_background_rotation_lets_go_here_and_redraws() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui, &mut state, &mut host, "left", "down", 0, 0, 100);
        assert!(ui.holding() && ui.opened.is_some());
        // The other side never acknowledges the rotation.
        host.acks = false;
        host.results.push_back(
            json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}),
        );
        let drawn = ui
            .event(&mut state, &mut host, &json!({"t":"tick","at":100 + 5000}))
            .unwrap();
        assert!(
            !ui.holding() && ui.locked.is_empty(),
            "nothing is shown held after it"
        );
        assert_eq!(drawn, ui.tree());
    }

    #[test]
    fn a_tap_then_a_press_swiped_soon_after_drags_with_the_tapped_button() {
        let (mut ui, mut state, mut host) = ready();
        ui.event(
            &mut state,
            &mut host,
            &json!({"t":"size","id":"pad","x":0,"y":0,"width":100_000,"height":80_000}),
        )
        .unwrap();
        // Tapped on the right half, then pressed again at once (anywhere) and swiped: a right-button drag.
        event(
            &mut ui, &mut state, &mut host, "pad", "down", 90_000, 1000, 10,
        );
        event(
            &mut ui, &mut state, &mut host, "pad", "up", 90_000, 1000, 20,
        );
        claim(&mut host);
        event(
            &mut ui,
            &mut state,
            &mut host,
            "pad",
            "down",
            10_000,
            1000,
            20 + TAP_THEN_HOLD_MS,
        );
        event(
            &mut ui,
            &mut state,
            &mut host,
            "pad",
            "move",
            30_000,
            1000,
            20 + TAP_THEN_HOLD_MS + 16,
        );
        event(
            &mut ui,
            &mut state,
            &mut host,
            "pad",
            "up",
            30_000,
            1000,
            20 + TAP_THEN_HOLD_MS + 40,
        );
        let buttons: Vec<Value> = written(&host)
            .into_iter()
            .filter(|e| e["t"] == "button")
            .collect();
        assert_eq!(
            buttons,
            vec![
                button_input("right", true),
                button_input("right", false),
                button_input("right", true),
                button_input("right", false)
            ]
        );
        // A swipe alone, long after, only moves.
        claim(&mut host);
        let before = written(&host).len();
        event(
            &mut ui, &mut state, &mut host, "pad", "down", 10_000, 1000, 5000,
        );
        event(
            &mut ui, &mut state, &mut host, "pad", "move", 40_000, 1000, 5016,
        );
        event(
            &mut ui, &mut state, &mut host, "pad", "up", 40_000, 1000, 5040,
        );
        assert!(written(&host)[before..].iter().all(|e| e["t"] == "move"));
    }

    #[test]
    fn a_held_backspace_repeats_at_the_clock_and_stops_when_let_go() {
        let (mut ui, mut state, mut host) = ready();
        assert!(
            !event(
                &mut ui,
                &mut state,
                &mut host,
                "backspace",
                "down",
                0,
                0,
                100
            )
            .is_null(),
            "lit while held"
        );
        let key = json!({"t":"key","key":"backspace"});
        for at in [350, 600, 850] {
            host.results.push_back(json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}));
            ui.event(
                &mut state,
                &mut host,
                &json!({"t":"tick","id":"idle","at":at}),
            )
            .unwrap();
        }
        assert_eq!(
            written(&host),
            vec![key.clone(), key.clone(), key.clone()],
            "once, then from 500 ms at each tick"
        );
        event(&mut ui, &mut state, &mut host, "backspace", "up", 0, 0, 900);
        assert!(ui.erasing.is_none());
        assert!(state.outgoing.is_none(), "let go: the connection is closed");
    }

    #[test]
    fn a_backspace_whose_lift_is_lost_stops_erasing_at_the_stuck_input_timeout() {
        let (mut ui, mut state, mut host) = ready();
        event(
            &mut ui,
            &mut state,
            &mut host,
            "backspace",
            "down",
            0,
            0,
            100,
        );
        let key = json!({"t":"key","key":"backspace"});
        let tick = |ui: &mut Ui, state: &mut State, host: &mut Script, at: u64| {
            host.results.push_back(
                json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}),
            );
            ui.event(state, host, &json!({"t":"tick","id":"idle","at":at}))
                .unwrap()
        };
        // A repeat is sent, but it is not a touch: the clock still runs from the press at 100.
        tick(&mut ui, &mut state, &mut host, 600);
        assert_eq!(written(&host), vec![key.clone(), key.clone()]);
        let held = ui.tree();
        let settled = tick(&mut ui, &mut state, &mut host, 100 + STUCK_MS);
        assert_ne!(settled, held);
        assert_eq!(
            settled,
            ui.tree(),
            "the erased key must be redrawn unpressed"
        );
        assert!(ui.erasing.is_none(), "the lost lift is let go");
        assert!(state.outgoing.is_none(), "and the connection closed");
        assert_eq!(
            written(&host),
            vec![key.clone(), key],
            "no erasing after the timeout"
        );
    }

    #[test]
    fn wheel_activity_does_not_prolong_a_backspace_whose_lift_was_lost() {
        let (mut ui, mut state, mut host) = ready();
        event(
            &mut ui,
            &mut state,
            &mut host,
            "backspace",
            "down",
            0,
            0,
            100,
        );
        event(
            &mut ui, &mut state, &mut host, "wheel", "down", 0, 0, STUCK_MS,
        );
        event(
            &mut ui,
            &mut state,
            &mut host,
            "wheel",
            "move",
            0,
            24000,
            STUCK_MS + 1,
        );
        host.results.push_back(
            json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}),
        );
        let out = ui
            .event(
                &mut state,
                &mut host,
                &json!({"t":"tick","id":"idle","at":100 + STUCK_MS}),
            )
            .unwrap();
        assert_eq!(out, ui.tree());
        assert!(ui.erasing.is_none());
        assert!(ui.wheel.is_some());
        let input: Vec<_> = written(&host)
            .into_iter()
            .filter(|event| event.get("t").is_some())
            .collect();
        assert_eq!(
            input,
            [
                json!({"t":"key","key":"backspace"}),
                json!({"t":"scroll","dx":0,"dy":1}),
            ]
        );
    }

    #[test]
    fn a_lift_whose_send_fails_still_draws_the_page_at_rest() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui, &mut state, &mut host, "wheel", "down", 0, 0, 10);
        // The other computer goes away: what the lift would send cannot go anywhere.
        ui.devices[0]["online"] = json!(false);
        let page = ui
            .event(
                &mut state,
                &mut host,
                &json!({"t":"pointer","id":"wheel","phase":"up","x":0,"y":0,"at":30}),
            )
            .unwrap();
        assert!(ui.wheel.is_none());
        assert!(
            serde_json::to_string(&page)
                .unwrap()
                .contains("\"id\":\"wheel\"")
        );
        assert!(
            !serde_json::to_string(&page).unwrap().contains("\"accent\""),
            "nothing is drawn as held"
        );
    }
}
