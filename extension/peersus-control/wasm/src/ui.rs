//! Control owns this entire screen and interprets its gestures. The host renders generic boxes.
use crate::{host::{Data, Wire}, remote::{self, State}};
use serde_json::{Value, json};

#[derive(Default)]
pub struct Ui {
    devices: Vec<Value>,
    chosen: Option<String>,
    selecting: bool,
    /// The device row a finger is on in the open chooser.
    touching: Option<usize>,
    /// Buttons locked down by the Left and Right keys: pressed once, held until pressed again.
    locked: Vec<&'static str>,
    /// The pad's left edge and width (thousandths of a logical pixel), from its measured size: a tap
    /// on its left half is a left click, on its right half a right click.
    pad_bounds: Option<(i32, i32)>,
    pad: Option<Pad>,
    /// When and on which half the last tap landed: a press soon after it, held and swiped, drags
    /// with that button.
    last_tap: Option<(u64, &'static str)>,
    /// Backspace held: when it is said again, as a keyboard repeats a held key.
    erasing: Option<u64>,
    wheel: Option<i32>,
    owed: i32,
    /// How far the wheel has been turned, for its ridges to follow the finger.
    scrolled: i32,
    last_input: Option<u64>,
    refreshed: u64,
    clock: u64,
    /// When the connection now open was opened: it is rotated well before the other side's call
    /// deadline, even through a press held still.
    opened: Option<u64>,
}
/// One finger on the pad and what it has done so far.
struct Pad { x: i32, y: i32, at: u64, travelled: f64, dragging: bool, side: &'static str, armed: bool }
/// A tap: lifted this soon, having moved less than this (logical pixels).
const TAP_MS: u64 = 250;
const TAP_TRAVEL: f64 = 8.;
/// A press this soon after a tap, then swiped, drags with the tapped button.
const TAP_THEN_HOLD_MS: u64 = 300;
/// A held Backspace first repeats after this, then at every tick of the page's clock.
const REPEAT_AFTER_MS: u64 = 500;
/// A press, drag or scroll with no input for this long lost its end: it is let go.
const STUCK_MS: u64 = 30_000;
fn text(value: &str) -> Value { json!({"k":"text","text":value,"tone":"muted"}) }
fn px(n: u16) -> Value { json!({"unit":"px","value":n}) }
fn fill() -> Value { json!({"unit":"percent","value":10000}) }
fn flex() -> Value { json!({"unit":"flex","value":1}) }
fn bx(id: Option<&str>, style: Value, children: Vec<Value>) -> Value {
    json!({"k":"box","id":id,"style":style,"children":children})
}
fn button_input(button: &str, down: bool) -> Value {
    json!({"t":"button","button":button,"down":down})
}
impl Ui {
    fn refresh<H: Wire + Data>(&mut self, state: &mut State, host: &mut H, at: u64) -> Result<bool, &'static str> {
        let peers = state.call(host, "peersus.peers", "list", json!({}))?;
        let peers = peers.as_array().ok_or(remote::answer::NOT_UNDERSTOOD)?;
        let changed = self.devices != *peers;
        self.devices = peers.clone();
        self.refreshed = at;
        let present = self.chosen.as_deref().is_some_and(|chosen| self.devices.iter().any(|p| p["device"].as_str() == Some(chosen)));
        if self.chosen.is_some() && !present {
            // Removed or unpaired: nothing to wait for, so its gesture ends and an online device may take its place.
            let _ = self.end(state, host);
            self.chosen = None;
        }
        if self.chosen.is_none() {
            self.chosen = self.devices.iter().find(|p| p["online"] == true)
                .and_then(|p| p["device"].as_str()).map(str::to_owned);
        } else if !self.devices.iter().any(|p| p["device"].as_str() == self.chosen.as_deref() && p["online"] == true) {
            // A destination still paired but gone offline ends its gesture; it never redirects it to another device.
            let _ = self.end(state, host);
        }
        Ok(changed)
    }
    fn send<H: Wire + Data>(&mut self, state: &mut State, host: &mut H, events: Vec<Value>, at: u64) -> Result<(), &'static str> {
        if events.is_empty() { return Ok(()); }
        let peer = self.chosen.as_ref().ok_or(remote::answer::REFUSED)?;
        if !self.devices.iter().any(|p| p["device"].as_str() == Some(peer.as_str()) && p["online"] == true) {
            let _ = self.end(state, host);
            return Err(remote::answer::REFUSED);
        }
        if state.outgoing.is_none() { self.opened = Some(at); }
        if let Err(reason) = state.input(host, &json!({"peer":peer,"events":events,"end":false})) {
            let _ = self.end(state, host);
            return Err(reason);
        }
        self.last_input = Some(at);
        Ok(())
    }
    /// Send what a finger did. At the end of its gesture (it lifted or was cancelled) a failure is not passed on:
    /// the gesture is over either way (`send` already ended the connection, which lets go of everything on the
    /// other side), and the page must be drawn at rest, not left showing a press nobody holds.
    fn send_or_settle<H: Wire + Data>(&mut self, state: &mut State, host: &mut H, inputs: Vec<Value>, at: u64, phase: &str) -> Result<(), &'static str> {
        match self.send(state, host, inputs, at) {
            Err(_) if matches!(phase, "up" | "cancel") => Ok(()),
            result => result,
        }
    }
    /// Whether a finger is still on something that sends input: the pad, the wheel or Backspace.
    fn busy(&self) -> bool {
        self.pad.is_some() || self.wheel.is_some() || self.erasing.is_some()
    }
    /// Which button a tap at `x` is: left on the left half of the pad, right on its right half.
    fn side(&self, x: i32) -> &'static str {
        match self.pad_bounds {
            Some((left, width)) if width > 0 && i64::from(x) >= i64::from(left) + i64::from(width) / 2 => "right",
            _ => "left",
        }
    }
    /// Whether anything is held on the other computer: a finger on the pad, the wheel or
    /// Backspace, or a locked button.
    fn holding(&self) -> bool {
        self.busy() || !self.locked.is_empty()
    }
    /// A gesture ended with nothing held: the connection is closed so the other computer is free,
    /// and its pointer stays where it is.
    fn rest<H: Wire + Data>(&mut self, state: &mut State, host: &mut H) {
        if self.holding() { return; }
        self.opened = None;
        self.last_input = None;
        let _ = state.rest(host);
    }
    fn end<H: Wire + Data>(&mut self, state: &mut State, host: &mut H) -> Result<(), &'static str> {
        self.pad = None;
        self.wheel = None;
        self.erasing = None;
        // Ending lets go of everything on the other computer, locked buttons with it.
        self.locked.clear();
        self.last_input = None;
        self.opened = None;
        state.finish(host)
    }
    pub fn event<H: Wire + Data>(&mut self, state: &mut State, host: &mut H, event: &Value) -> Result<Value, &'static str> {
        self.clock = self.clock.max(event["at"].as_u64().unwrap_or(self.clock));
        let at = self.clock;
        let id = event["id"].as_str().unwrap_or("");
        match event["t"].as_str() {
            Some("open") => { self.selecting = false; self.refresh(state, host, at)?; return Ok(self.tree()); }
            Some("tick") => {
                // A touch whose up or cancel never arrived must not hold the connection, and with
                // it the other device's button, forever. A press held still is a gesture in
                // progress, so only a long silence ends it.
                let idle = self.last_input.is_some_and(|last| at.saturating_sub(last) >= 2000);
                let stuck = self.busy() && self.last_input.is_some_and(|last| at.saturating_sub(last) >= STUCK_MS);
                if stuck {
                    // A drag whose finger's lift was lost lets go of its button.
                    if let Some(pad) = self.pad.take().filter(|pad| pad.dragging) {
                        let _ = self.send(state, host, vec![button_input(pad.side, false)], at);
                    }
                    self.end(state, host)?;
                } else if !self.holding() && idle {
                    self.end(state, host)?;
                } else if let Some(next) = self.erasing.filter(|next| at >= *next) {
                    // Backspace held repeats, at every tick of the page's clock.
                    self.erasing = Some(at.max(next));
                    self.send(state, host, vec![json!({"t":"key","key":"backspace"})], at)?;
                }
                if self.opened.is_some_and(|opened| at.saturating_sub(opened) >= 5000) {
                    self.opened = None;
                    let _ = state.rotate(host);
                }
                if at.saturating_sub(self.refreshed) >= 2000 && self.refresh(state, host, at)? {
                    return Ok(self.tree());
                }
            }
            Some("press") => {
                if id == "refresh" { self.refresh(state, host, at)?; }
                else if let Some(index) = id.strip_prefix("device-").and_then(|n| n.parse::<usize>().ok()) {
                    self.choose(state, host, index)?;
                }
                return Ok(self.tree());
            }
            Some("submit") if id == "keyboard" => {
                let value = event["text"].as_str().ok_or(remote::answer::NOT_UNDERSTOOD)?;
                let input = if value.is_empty() { json!({"t":"key","key":"enter"}) }
                    else { json!({"t":"text","text":value}) };
                self.send(state, host, vec![input], at)?;
                self.rest(state, host);
            }
            // The pad's measured bounds: which half of it a tap lands on says which button it is.
            Some("size") if id == "pad" => {
                let x = event["x"].as_i64().and_then(|n| i32::try_from(n).ok()).ok_or(remote::answer::NOT_UNDERSTOOD)?;
                let width = event["width"].as_i64().and_then(|n| i32::try_from(n).ok()).ok_or(remote::answer::NOT_UNDERSTOOD)?;
                self.pad_bounds = Some((x, width));
                return Ok(Value::Null);
            }
            Some("pointer") => {
                let phase = event["phase"].as_str().ok_or(remote::answer::NOT_UNDERSTOOD)?;
                let x = event["x"].as_i64().and_then(|n| i32::try_from(n).ok()).ok_or(remote::answer::NOT_UNDERSTOOD)?;
                let y = event["y"].as_i64().and_then(|n| i32::try_from(n).ok()).ok_or(remote::answer::NOT_UNDERSTOOD)?;
                // A device in the open chooser is chosen by touching its row: lit while touched,
                // chosen when the finger lifts on it.
                if self.selecting && let Some(index) = id.strip_prefix("device-").and_then(|n| n.parse::<usize>().ok()) {
                    match phase {
                        "down" => self.touching = Some(index),
                        "up" if self.touching == Some(index) => { self.touching = None; self.choose(state, host, index)?; }
                        // A finger always trembles a little: moving on the row keeps it, unredrawn.
                        "move" => return Ok(Value::Null),
                        _ => self.touching = None,
                    }
                    return Ok(self.tree());
                }
                if self.selecting && !matches!(id, "choose" | "devices" | "menu") {
                    if phase == "down" {
                        self.selecting = false;
                        return Ok(self.tree());
                    }
                    return Ok(Value::Null);
                }
                let mut inputs = Vec::new();
                match id {
                    "choose" if phase == "up" => { self.selecting = !self.selecting; return Ok(self.tree()); }
                    // Left and Right lock their button: pressed once it is held on the other computer, pressed
                    // again it is let go. With one locked, a swipe on the pad drags with it.
                    "left" | "right" => {
                        if phase != "down" { return Ok(Value::Null); }
                        let button = if id == "left" { "left" } else { "right" };
                        if let Some(index) = self.locked.iter().position(|locked| *locked == button) {
                            self.locked.remove(index);
                            inputs.push(button_input(button, false));
                        } else {
                            self.locked.push(button);
                            inputs.push(button_input(button, true));
                        }
                        self.send(state, host, inputs, at)?;
                        self.rest(state, host);
                        return Ok(self.tree());
                    }
                    // Held, Backspace erases once and then again at every tick, as a keyboard repeats a held key.
                    "backspace" => {
                        match phase {
                            "down" => {
                                self.erasing = Some(at.saturating_add(REPEAT_AFTER_MS));
                                inputs.push(json!({"t":"key","key":"backspace"}));
                            }
                            "up" | "cancel" => self.erasing = None,
                            _ => return Ok(Value::Null),
                        }
                        self.send_or_settle(state, host, inputs, at, phase)?;
                        if phase != "down" { self.rest(state, host); }
                        return Ok(self.tree());
                    }
                    // The whole pad is the mouse. A tap clicks with the half it lands on: left on the left half,
                    // right on the right half. A tap and then, soon, a press held and swiped drags with the tapped
                    // button; so does a press held still and then swiped. A swipe alone moves the pointer.
                    "pad" => { match phase {
                        "down" => {
                            let tapped = self.last_tap.take().filter(|(tap, _)| at.saturating_sub(*tap) <= TAP_THEN_HOLD_MS);
                            let side = tapped.map_or_else(|| self.side(x), |(_, side)| side);
                            self.pad = Some(Pad { x, y, at, travelled: 0., dragging: false, side, armed: tapped.is_some() });
                        }
                        "move" => if let Some(pad) = &mut self.pad {
                            let dx = (f64::from(x) - f64::from(pad.x)) / 1000.;
                            let dy = (f64::from(y) - f64::from(pad.y)) / 1000.;
                            if dx != 0. || dy != 0. {
                                // Only a press that follows a tap drags; a swipe on its own only moves, however
                                // long the finger rested before it. With a button locked, the pad only moves.
                                let starts = self.locked.is_empty() && !pad.dragging && pad.armed;
                                if starts {
                                    pad.dragging = true;
                                    inputs.push(button_input(pad.side, true));
                                }
                                pad.travelled += dx.hypot(dy);
                                pad.x = x; pad.y = y;
                                inputs.push(json!({"t":"move","dx":dx * 2.5,"dy":dy * 2.5}));
                                // The dragging half lights up the moment the drag starts.
                                if starts {
                                    self.send(state, host, inputs, at)?;
                                    return Ok(self.tree());
                                }
                            }
                        },
                        "up" | "cancel" => if let Some(pad) = self.pad.take() {
                            if pad.dragging {
                                inputs.push(button_input(pad.side, false));
                            } else if self.locked.is_empty() && phase == "up" && pad.travelled < TAP_TRAVEL && at.saturating_sub(pad.at) <= TAP_MS {
                                inputs.extend([button_input(pad.side, true), button_input(pad.side, false)]);
                                // A second quick tap is a double click; only a first tap may start a drag.
                                if !pad.armed { self.last_tap = Some((at, pad.side)); }
                            }
                        },
                        _ => {},
                    }
                    // Pressed and let go are drawn (the half lights up only while it drags); plain motion is not.
                    if matches!(phase, "down" | "up" | "cancel") {
                        self.send_or_settle(state, host, inputs, at, phase)?;
                        if phase != "down" { self.rest(state, host); }
                        return Ok(self.tree());
                    }
                    },
                    "wheel" => {
                        match phase {
                            "down" => { self.wheel = Some(y); self.owed = 0; }
                            "move" => if let Some(previous) = self.wheel.replace(y) {
                                let moved = y.saturating_sub(previous);
                                self.scrolled = self.scrolled.wrapping_add(moved);
                                self.owed = self.owed.saturating_add(moved);
                                let notches = self.owed / 24000;
                                if notches != 0 {
                                    self.owed %= 24000;
                                    inputs.push(json!({"t":"scroll","dx":0,"dy":notches}));
                                }
                            },
                            "up" | "cancel" => self.wheel = None,
                            _ => {},
                        }
                        // The wheel is drawn as it turns: its ridges follow the finger.
                        self.send_or_settle(state, host, inputs, at, phase)?;
                        if matches!(phase, "up" | "cancel") { self.rest(state, host); }
                        return Ok(self.tree());
                    }
                    _ => {},
                }
                self.send_or_settle(state, host, inputs, at, phase)?;
                if matches!(phase, "up" | "cancel") { self.rest(state, host); }
            }
            _ => return Err(remote::answer::NOT_UNDERSTOOD),
        }
        // Pointer samples do not invalidate the visual tree or cause frontend snapshot repainting.
        Ok(Value::Null)
    }
    /// Control the device at `index` from now on, if it is online: the gesture on the previous one ends.
    fn choose<H: Wire + Data>(&mut self, state: &mut State, host: &mut H, index: usize) -> Result<(), &'static str> {
        let Some(peer) = self.devices.get(index).filter(|p| p["online"] == true) else { return Ok(()) };
        let chosen = peer["device"].as_str().map(str::to_owned);
        self.end(state, host)?;
        self.chosen = chosen;
        self.selecting = false;
        Ok(())
    }
    fn identity(&self, peer: &Value) -> Vec<Value> {
        vec![json!({"k":"avatar","device":peer["device"],"name":peer["name"],"size":28}),
            text(peer["name"].as_str().unwrap_or("Device"))]
    }
    pub fn tree(&self) -> Value {
        let selected = self.devices.iter().find(|p| p["device"].as_str() == self.chosen.as_deref());
        let mut content = Vec::new();
        if let Some(peer) = selected {
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
            let wheel = bx(Some("wheel"), json!({"width":px(48),"height":px(120),"center":true}), vec![
                bx(None, json!({"width":px(28),"height":px(76),"radius":[14,14,14,14],"clip":true,
                    "background":if turning {"accent"} else {"raised"},"border":true}), ridges)]);
            // Left and Right lock their button; a locked one is lit and says so.
            let lock = |id: &'static str, caption: &str, radius: [u16; 4]| {
                let locked = self.locked.contains(&id);
                bx(Some(id), json!({"width":flex(),"height":px(120),"center":true,"radius":radius,
                    "background":if locked {"accent"} else {"raised"}}),
                    vec![text(&if locked { format!("{caption} 🔒") } else { caption.to_owned() })])
            };
            let buttons = bx(None, json!({"width":fill(),"horizontal":true,"gap":8,"center":true}), vec![
                lock("left", "Left", [130,8,8,8]), wheel, lock("right", "Right", [8,130,8,8])]);
            let mut identity = self.identity(peer);
            identity.push(text(if self.selecting { "▴" } else { "▾" }));
            // The pad's two halves are its two buttons: a half lights up only while it is held down, dragging.
            let touched = self.pad.as_ref().filter(|pad| pad.dragging).map(|pad| pad.side);
            let half = |side: &str, caption: &str| bx(None, json!({"width":flex(),"height":fill(),"center":true,
                "radius":[12,12,12,12],"background":if touched == Some(side) {"raised"} else {"transparent"}}), vec![text(caption)]);
            let halves = bx(None, json!({"width":fill(),"height":flex(),"horizontal":true}), vec![
                half("left", "Left click"),
                bx(None, json!({"width":px(1),"height":fill(),"background":"border"}), vec![]),
                half("right", "Right click")]);
            let mut pad_content = vec![halves];
            // The chooser is one frame that grows upward: closed it holds the selected device; open,
            // the other online devices appear above it in the same frame. The selected device keeps
            // its exact place and the frame its width rule, so opening it moves nothing.
            let mut rows: Vec<Value> = Vec::new();
            if self.selecting {
                let mut others: Vec<Value> = self.devices.iter().enumerate()
                    .filter(|(_, peer)| peer["online"] == true && peer["device"].as_str() != self.chosen.as_deref())
                    .map(|(index, peer)| bx(Some(&format!("device-{index}")), json!({"padding":4,"radius":[8,8,8,8],
                        "background":if self.touching == Some(index) {"raised"} else {"transparent"}}),
                        vec![bx(None, json!({"horizontal":true,"gap":8,"center":true}), self.identity(peer))]))
                    .collect();
                if others.is_empty() { others.push(text("No other device is online")); }
                // The other devices grow out of the selected one, upward, as the menu opens.
                // Named, so it is the same box while it stays open and opens only once.
                rows.push(bx(Some("menu"), json!({"gap":4,"reveal":true}), others));
            }
            // As wide as the name: the frame around the rows is as wide as the longest of them, open or closed.
            rows.push(bx(Some("choose"), json!({"padding":4}), vec![bx(None, json!({"horizontal":true,"gap":8,"center":true}), identity)]));
            // Over the pad, against its lower left corner: opening the menu covers the pad, it never squeezes its halves.
            let frame = bx(Some("devices"), json!({"padding":4,"gap":4,"radius":[12,12,12,12],
                "background":"surface","border":true}), rows);
            pad_content.push(bx(None, json!({"overlay":true,"width":fill(),"height":fill(),"bottom":true}), vec![frame]));
            let mut pad = bx(Some("pad"), json!({"width":fill(),"height":flex(),"gap":8,"radius":[12,12,12,12]}), pad_content);
            // Measured, so a tap knows which half it landed on.
            pad["observe"] = json!(["size"]);
            let surface = pad;
            let erasing = self.erasing.is_some();
            let keyboard = bx(None, json!({"width":fill(),"height":px(48),"horizontal":true,"gap":8,"center":true}), vec![
                bx(None, json!({"width":flex(),"height":px(48)}), vec![json!({"k":"field","id":"keyboard","placeholder":"Type on the computer","value":"","height":48})]),
                // A key, as on a keyboard: held, it repeats.
                bx(Some("backspace"), json!({"width":px(56),"height":px(48),"center":true,
                    "radius":[12,12,12,12],"background":if erasing {"accent"} else {"raised"}}),
                    vec![json!({"k":"icon","name":"backspace","size":22})])]);
            content.push(bx(None, json!({"width":fill(),"height":flex(),"padding":12,"gap":8,
                "radius":[140,140,32,32],"background":"surface","border":true}), vec![buttons,surface,keyboard]));
        } else {
            content.push(text("No computer is online. Keep a paired computer online and allow Control in its extension permissions."));
            content.push(json!({"k":"button","id":"refresh","label":"Refresh devices"}));
        }
        json!({"k":"interval","id":"idle","milliseconds":250,"child":
            bx(None,json!({"width":fill(),"height":fill(),"gap":10,"padding":12}),content)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::scripted::{Script, ack};

    fn event(ui: &mut Ui, state: &mut State, host: &mut Script, id: &str, phase: &str, x: i32, y: i32, at: u64) -> Value {
        ui.event(state, host, &json!({"t":"pointer","id":id,"phase":phase,"x":x,"y":y,"at":at})).unwrap()
    }
    fn ready() -> (Ui, State, Script) {
        let mut host = Script { acks: true, ..Script::default() };
        host.results.push_back(json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}));
        let mut ui = Ui::default();
        let mut state = State::default();
        ui.event(&mut state, &mut host, &json!({"t":"open"})).unwrap();
        host.results.push_back(json!({"status":"ok","value":{"claim":42}}));
        (ui,state,host)
    }
    fn written(host: &Script) -> Vec<Value> {
        String::from_utf8(host.written.clone()).unwrap().lines().map(|line| serde_json::from_str(line).unwrap())
            .filter(|line: &Value| *line != json!({"rests":true})).collect()
    }
    fn rests(host: &Script) -> usize {
        String::from_utf8(host.written.clone()).unwrap().lines().filter(|l| *l == remote::RESTS).count()
    }
    fn claim(host: &mut Script) {
        host.results.push_back(json!({"status":"ok","value":{"claim":43}}));
    }

    #[test]
    fn a_drag_whose_lift_never_arrives_is_let_go_and_its_connection_closed() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui,&mut state,&mut host,"pad","down",1000,1000,80);
        event(&mut ui,&mut state,&mut host,"pad","up",1000,1000,90);
        claim(&mut host);
        event(&mut ui,&mut state,&mut host,"pad","down",1000,1000,100);
        event(&mut ui,&mut state,&mut host,"pad","move",2000,1000,120);
        assert!(ui.pad.as_ref().is_some_and(|pad| pad.dragging), "tapped, then pressed and swiped: a drag");
        host.results.push_back(json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}));
        ui.event(&mut state,&mut host,&json!({"t":"tick","at":3000})).unwrap();
        assert!(ui.pad.is_some(), "a drag held still is a gesture in progress");
        host.results.push_back(json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}));
        ui.event(&mut state,&mut host,&json!({"t":"tick","at":600 + STUCK_MS})).unwrap();
        assert!(state.outgoing.is_none());
        assert!(ui.pad.is_none());
        assert_eq!(written(&host).last(), Some(&button_input("left",false)));
    }

    #[test]
    fn choosing_a_connected_device_does_not_send_a_mouse_click() {
        let (mut ui, mut state, mut host) = ready();
        ui.devices.push(json!({"device":"offline","name":"Offline","online":false}));
        ui.devices.push(json!({"device":"other","name":"Other","online":true}));
        event(&mut ui,&mut state,&mut host,"choose","down",0,0,10);
        event(&mut ui,&mut state,&mut host,"choose","up",0,0,20);
        assert!(ui.selecting);
        assert!(host.opened.is_empty(), "opening the picker is not remote input");
        let tree = ui.tree();
        let pad = &tree["child"]["children"][0]["children"][1];
        assert_eq!(pad["id"], "pad");
        assert_eq!(pad["observe"], json!(["size"]), "measured, so a tap knows its half");
        // One surface: the other online devices, then the selected one, inside the same frame.
        let over = pad["children"].as_array().unwrap().last().unwrap();
        assert_eq!(over["style"]["overlay"], true, "the chooser floats over the pad instead of taking room in it");
        let menu = &over["children"][0];
        assert_eq!(menu["id"], "devices");
        let rows = menu["children"].as_array().unwrap();
        assert_eq!(rows[0]["style"]["reveal"], true, "the other devices grow open");
        let offered: Vec<_> = rows[0]["children"].as_array().unwrap().iter().filter_map(|row| row["id"].as_str()).collect();
        assert_eq!(offered, ["device-2"], "the selected device is not another destination to select");
        assert!(!serde_json::to_string(menu).unwrap().contains("\"button\""), "a row is touched, not a Select button");
        assert_eq!(rows.last().unwrap()["id"], "choose");
        // Touching a row lights it; lifting on it chooses it.
        fn find<'a>(node: &'a Value, id: &str) -> Option<&'a Value> {
            if node["id"] == id { return Some(node); }
            node["children"].as_array().into_iter().flatten().chain(node.get("child")).find_map(|child| find(child, id))
        }
        assert_eq!(find(&ui.tree(), "device-2").unwrap()["style"]["background"], "transparent");
        let lit = event(&mut ui,&mut state,&mut host,"device-2","down",0,0,25);
        assert_eq!(find(&lit, "device-2").unwrap()["style"]["background"], "raised");
        assert!(ui.selecting);
        assert!(event(&mut ui,&mut state,&mut host,"device-2","move",400,200,26).is_null(), "a trembling finger keeps the row");
        assert!(!serde_json::to_string(menu).unwrap().contains("\"height\":{\"unit\":\"px\",\"value\":1}"), "no divider line");
        assert_eq!(rows.last().unwrap()["style"].get("width"), None, "open: as wide as the longest name, not the pad");
        event(&mut ui,&mut state,&mut host,"device-2","up",0,0,28);
        let closed = ui.tree();
        let closed_frame = closed["child"]["children"][0]["children"][1]["children"].as_array().unwrap().last().unwrap()["children"][0].clone();
        assert_eq!(closed_frame["id"], "devices", "closed and open are the same frame");
        assert_eq!(closed_frame["children"].as_array().unwrap().len(), 1);
        assert_eq!(closed_frame["children"][0]["style"].get("width"), None, "closed: as wide as the name, no more");
        assert_eq!(ui.chosen.as_deref(), Some("other"));
        assert!(!ui.selecting);
        assert!(host.opened.is_empty());
        event(&mut ui,&mut state,&mut host,"pad","down",0,0,30);
        event(&mut ui,&mut state,&mut host,"pad","up",0,0,40);
        assert_eq!(written(&host), vec![button_input("left",true),button_input("left",false)]);
    }

    #[test]
    fn touching_outside_the_chooser_dismisses_it_without_remote_input() {
        let (mut ui, mut state, mut host) = ready();
        ui.selecting = true;
        assert!(!event(&mut ui, &mut state, &mut host, "pad", "down", 1000, 1000, 10).is_null());
        assert!(!ui.selecting);
        event(&mut ui, &mut state, &mut host, "pad", "up", 1000, 1000, 20);
        assert!(host.opened.is_empty());
        assert!(host.written.is_empty());
    }

    #[test]
    fn returning_to_the_remote_page_does_not_leave_the_chooser_open() {
        let (mut ui, mut state, mut host) = ready();
        ui.selecting = true;
        host.results.clear();
        host.results.push_back(json!({"status":"ok","value":ui.devices.clone()}));
        ui.event(&mut state, &mut host, &json!({"t":"open"})).unwrap();
        assert!(!ui.selecting);
        assert!(host.opened.is_empty());
    }

    #[test]
    fn a_locked_left_drags_with_the_pad_and_only_its_key_lets_it_go() {
        let (mut ui, mut state, mut host) = ready();
        assert!(!event(&mut ui,&mut state,&mut host,"left","down",0,0,10).is_null());
        event(&mut ui,&mut state,&mut host,"left","up",0,0,20);
        assert_eq!(ui.locked, ["left"], "pressed once: locked, and lifting the finger does not unlock it");
        // With Left locked, the pad only moves: no click of its own, and lifting it lets nothing go.
        event(&mut ui,&mut state,&mut host,"pad","down",1000,1000,30);
        event(&mut ui,&mut state,&mut host,"pad","move",9000,1000,40);
        event(&mut ui,&mut state,&mut host,"pad","up",9000,1000,60);
        assert_eq!(ui.locked, ["left"]);
        assert!(state.outgoing.is_some(), "the drag is still in progress");
        event(&mut ui,&mut state,&mut host,"left","down",0,0,700);
        assert!(ui.locked.is_empty(), "pressed again: let go");
        let events = written(&host);
        assert_eq!(events.first(), Some(&button_input("left",true)));
        assert_eq!(events.last(), Some(&button_input("left",false)));
        assert_eq!(events.iter().filter(|e| e["t"]=="button").count(), 2, "one press, one release: {events:?}");
        assert!(events.iter().any(|e| e["t"]=="move"));
    }

    #[test]
    fn extension_interprets_motion_and_taps_without_rebuilding_the_tree() {
        let (mut ui, mut state, mut host) = ready();
        assert!(!event(&mut ui,&mut state,&mut host,"pad","down",1000,2000,10).is_null(), "touched: the pad lights up");
        assert_eq!(event(&mut ui,&mut state,&mut host,"pad","move",11000,22000,20),Value::Null, "motion is not redrawn");
        event(&mut ui,&mut state,&mut host,"pad","up",11000,22000,30);
        assert_eq!(written(&host),vec![json!({"t":"move","dx":25.0,"dy":50.0})]);
        // The gesture ended: said, and the connection closed so the computer is free meanwhile.
        assert_eq!(rests(&host),1);
        assert!(state.outgoing.is_none());
        claim(&mut host);
        event(&mut ui,&mut state,&mut host,"pad","down",1000,2000,40);
        event(&mut ui,&mut state,&mut host,"pad","up",1000,2000,50);
        assert_eq!(&written(&host)[1..],&[button_input("left",true),button_input("left",false)]);
        assert_eq!(host.opened.len(),2);
    }

    #[test]
    fn a_tap_clicks_with_the_half_it_lands_on_and_cancel_releases_a_drag() {
        let (mut ui, mut state, mut host) = ready();
        // The pad spans x 0..100 px: its right half begins at 50 px.
        ui.event(&mut state,&mut host,&json!({"t":"size","id":"pad","x":0,"y":0,"width":100_000,"height":80_000})).unwrap();
        event(&mut ui,&mut state,&mut host,"pad","down",80_000,1000,10);
        event(&mut ui,&mut state,&mut host,"pad","up",80_000,1000,20);
        claim(&mut host);
        // Resting long before swiping is still only a swipe: it moves and never presses.
        event(&mut ui,&mut state,&mut host,"pad","down",10_000,0,1000);
        assert!(event(&mut ui,&mut state,&mut host,"pad","move",11_000,0,2000).is_null(), "a swipe is not drawn as a press");
        event(&mut ui,&mut state,&mut host,"pad","up",11_000,0,2100);
        // A tap on the left half, then at once pressed and swiped: a left drag, which cancel lets go.
        claim(&mut host);
        event(&mut ui,&mut state,&mut host,"pad","down",10_000,0,3000);
        event(&mut ui,&mut state,&mut host,"pad","up",10_000,0,3010);
        claim(&mut host);
        event(&mut ui,&mut state,&mut host,"pad","down",10_000,0,3100);
        assert!(!event(&mut ui,&mut state,&mut host,"pad","move",12_000,0,3116).is_null(), "the dragging half lights up");
        event(&mut ui,&mut state,&mut host,"pad","cancel",0,0,3200);
        let buttons: Vec<Value> = written(&host).into_iter().filter(|e| e["t"]=="button").collect();
        assert_eq!(buttons, vec![button_input("right",true),button_input("right",false),
            button_input("left",true),button_input("left",false),
            button_input("left",true),button_input("left",false)]);
    }

    #[test]
    fn wheel_accumulates_fractional_notches_and_idle_closes_the_connection() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui,&mut state,&mut host,"wheel","down",0,0,10);
        event(&mut ui,&mut state,&mut host,"wheel","move",0,12000,20);
        assert!(host.written.is_empty());
        event(&mut ui,&mut state,&mut host,"wheel","move",0,36000,30);
        assert_eq!(written(&host),vec![json!({"t":"scroll","dx":0,"dy":1})]);
        event(&mut ui,&mut state,&mut host,"wheel","up",0,36000,35);
        host.incoming.push_back(ack(host.written.len() as u64));
        host.results.push_back(json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}));
        ui.event(&mut state,&mut host,&json!({"t":"tick","id":"idle","at":2030})).unwrap();
        assert!(state.outgoing.is_none());
        assert_eq!(host.shutdowns,1);
    }

    #[test]
    fn keyboard_uses_the_last_clock_sample_and_a_locked_button_does_not_expire() {
        let (mut ui, mut state, mut host) = ready();
        ui.event(&mut state,&mut host,&json!({"t":"tick","id":"idle","at":1500})).unwrap();
        ui.event(&mut state,&mut host,&json!({"t":"submit","id":"keyboard","text":"a"})).unwrap();
        assert!(state.outgoing.is_none(),"typed: nothing held, the connection is closed");
        assert_eq!(host.shutdowns,1);
        claim(&mut host);
        event(&mut ui,&mut state,&mut host,"left","down",0,0,1800);
        host.results.push_back(json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}));
        ui.event(&mut state,&mut host,&json!({"t":"tick","id":"idle","at":4000})).unwrap();
        assert!(state.outgoing.is_some(),"a locked button does not expire");
        assert_eq!(written(&host),vec![json!({"t":"text","text":"a"}),button_input("left",true)]);
        event(&mut ui,&mut state,&mut host,"left","down",0,0,4100);
        assert!(state.outgoing.is_none());
        assert_eq!(host.shutdowns,2);
    }

    #[test]
    fn left_and_right_lock_together_and_each_key_lets_its_own_go() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui,&mut state,&mut host,"left","down",0,0,10);
        event(&mut ui,&mut state,&mut host,"right","down",0,0,20);
        event(&mut ui,&mut state,&mut host,"left","down",0,0,30);
        assert_eq!(ui.locked, ["right"]);
        assert!(state.outgoing.is_some(), "Right still locked");
        event(&mut ui,&mut state,&mut host,"right","down",0,0,40);
        assert!(ui.locked.is_empty());
        assert_eq!(written(&host), vec![button_input("left",true),button_input("right",true),
            button_input("left",false),button_input("right",false)]);
    }

    #[test]
    fn a_press_held_still_rotates_its_connection_before_the_other_side_gives_up() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui,&mut state,&mut host,"left","down",0,0,100);
        host.results.push_back(json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}));
        ui.event(&mut state,&mut host,&json!({"t":"tick","id":"idle","at":5200})).unwrap();
        let lines = String::from_utf8(host.written.clone()).unwrap();
        assert!(lines.lines().any(|l| l == remote::CONTINUES),"rotated, keeping the press: {lines}");
        assert!(state.outgoing.is_none());
        assert_eq!(ui.locked, ["left"]);
    }

    #[test]
    fn disconnect_ends_the_gesture_without_selecting_another_online_device() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui,&mut state,&mut host,"left","down",0,0,10);
        host.incoming.push_back(ack(host.written.len() as u64));
        host.results.push_back(json!({"status":"ok","value":[
            {"device":"scoped","name":"Computer","online":false},
            {"device":"another","name":"Other","online":true}
        ]}));
        ui.event(&mut state,&mut host,&json!({"t":"tick","id":"idle","at":2010})).unwrap();
        assert_eq!(ui.chosen.as_deref(),Some("scoped"));
        assert!(ui.locked.is_empty());
        assert!(state.outgoing.is_none());
        assert_eq!(host.shutdowns,1);
        assert_eq!(ui.event(&mut state,&mut host,&json!({"t":"submit","id":"keyboard","text":"a"})),Err(remote::answer::REFUSED));
        assert_eq!(host.opened.len(),1);
    }

    #[test]
    fn a_removed_destination_is_replaced_by_an_online_device() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui,&mut state,&mut host,"left","down",0,0,10);
        host.incoming.push_back(ack(host.written.len() as u64));
        host.results.push_back(json!({"status":"ok","value":[
            {"device":"another","name":"Other","online":true}
        ]}));
        ui.event(&mut state,&mut host,&json!({"t":"tick","id":"idle","at":2010})).unwrap();
        assert_eq!(ui.chosen.as_deref(),Some("another"));
        assert!(ui.locked.is_empty());
        assert!(state.outgoing.is_none());
        assert!(ui.tree().to_string().contains("\"pad\""), "the remote page is offered again");
    }

    #[test]
    fn failed_destination_clears_a_new_button_press_and_closes_the_old_stream() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui,&mut state,&mut host,"pad","down",0,0,10);
        event(&mut ui,&mut state,&mut host,"pad","move",10000,0,20);
        ui.devices[0]["online"] = json!(false);
        host.incoming.push_back(ack(host.written.len() as u64));
        assert_eq!(ui.event(&mut state,&mut host,&json!({"t":"pointer","id":"left","phase":"down","x":0,"y":0,"at":30})),Err(remote::answer::REFUSED));
        assert!(ui.locked.is_empty());
        assert!(ui.pad.is_none());
        assert!(state.outgoing.is_none());
        assert_eq!(host.shutdowns,1);
        assert_eq!(host.opened.len(),1);
        assert_eq!(written(&host),vec![json!({"t":"move","dx":25.0,"dy":0.0})]);
    }

    #[test]
    fn a_tap_then_a_press_swiped_soon_after_drags_with_the_tapped_button() {
        let (mut ui, mut state, mut host) = ready();
        ui.event(&mut state,&mut host,&json!({"t":"size","id":"pad","x":0,"y":0,"width":100_000,"height":80_000})).unwrap();
        // Tapped on the right half, then pressed again at once (anywhere) and swiped: a right-button drag.
        event(&mut ui,&mut state,&mut host,"pad","down",90_000,1000,10);
        event(&mut ui,&mut state,&mut host,"pad","up",90_000,1000,20);
        claim(&mut host);
        event(&mut ui,&mut state,&mut host,"pad","down",10_000,1000,20 + TAP_THEN_HOLD_MS);
        event(&mut ui,&mut state,&mut host,"pad","move",30_000,1000,20 + TAP_THEN_HOLD_MS + 16);
        event(&mut ui,&mut state,&mut host,"pad","up",30_000,1000,20 + TAP_THEN_HOLD_MS + 40);
        let buttons: Vec<Value> = written(&host).into_iter().filter(|e| e["t"]=="button").collect();
        assert_eq!(buttons, vec![button_input("right",true),button_input("right",false),
            button_input("right",true),button_input("right",false)]);
        // A swipe alone, long after, only moves.
        claim(&mut host);
        let before = written(&host).len();
        event(&mut ui,&mut state,&mut host,"pad","down",10_000,1000,5000);
        event(&mut ui,&mut state,&mut host,"pad","move",40_000,1000,5016);
        event(&mut ui,&mut state,&mut host,"pad","up",40_000,1000,5040);
        assert!(written(&host)[before..].iter().all(|e| e["t"]=="move"));
    }

    #[test]
    fn a_held_backspace_repeats_at_the_clock_and_stops_when_let_go() {
        let (mut ui, mut state, mut host) = ready();
        assert!(!event(&mut ui,&mut state,&mut host,"backspace","down",0,0,100).is_null(), "lit while held");
        let key = json!({"t":"key","key":"backspace"});
        for at in [350, 600, 850] {
            host.results.push_back(json!({"status":"ok","value":[{"device":"scoped","name":"Computer","online":true}]}));
            ui.event(&mut state,&mut host,&json!({"t":"tick","id":"idle","at":at})).unwrap();
        }
        assert_eq!(written(&host), vec![key.clone(),key.clone(),key.clone()], "once, then from 500 ms at each tick");
        event(&mut ui,&mut state,&mut host,"backspace","up",0,0,900);
        assert!(ui.erasing.is_none());
        assert!(state.outgoing.is_none(), "let go: the connection is closed");
    }

    #[test]
    fn a_lift_whose_send_fails_still_draws_the_page_at_rest() {
        let (mut ui, mut state, mut host) = ready();
        event(&mut ui,&mut state,&mut host,"wheel","down",0,0,10);
        // The other computer goes away: what the lift would send cannot go anywhere.
        ui.devices[0]["online"] = json!(false);
        let page = ui.event(&mut state,&mut host,&json!({"t":"pointer","id":"wheel","phase":"up","x":0,"y":0,"at":30})).unwrap();
        assert!(ui.wheel.is_none());
        assert!(serde_json::to_string(&page).unwrap().contains("\"id\":\"wheel\""));
        assert!(!serde_json::to_string(&page).unwrap().contains("\"accent\""), "nothing is drawn as held");
    }
}
