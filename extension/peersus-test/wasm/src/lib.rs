//! Test: one page of its own, drawn by the application from plain elements, that tries local interactions and services
//! through the extension contract. Its drawing stays beside its input, statuses show their result here and on paired
//! devices, and notifications are presented by the application. When its setting asks, it can also misbehave on purpose.
#![cfg(any(target_family = "wasm", feature = "native", test))]

mod host;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
mod native;
mod page;
mod probes;
mod trouble;

use host::Wire;
use serde_json::{Value, json};

/// The setting that shows the buttons that misbehave on purpose.
const SHOW_TROUBLE: &str = "show_trouble";
const DEFAULT_STATUS_LABEL: &str = "Testing";
/// How long the application may take to answer a call of this extension.
const CALL_MS: u64 = 10_000;
/// How long a question outside the window may wait for the person.
const NOTIFY_MS: u64 = 120_000;
/// Most wheel steps the drawing keeps.
const WHEEL_STEPS: usize = 18;

/// What a call this extension made is for, by its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    Config,
    Probe(usize),
    Notification,
}

#[derive(Default)]
pub(crate) struct State {
    pressed: u32,
    on: bool,
    reveal: bool,
    said: String,
    rows: u32,
    /// The proposed and last accepted words for the single status the test owns.
    status_input: String,
    status_label: Option<String>,
    status_on: bool,
    spammed: u32,
    /// How many times one of its key-bindable actions ran (`bump`).
    keyed: u32,
    /// Notifications asked for, how many have no answer yet, and the last answer.
    asked: u32,
    notifying: u32,
    told: String,
    /// The last answer of each probe, in `probes::PROBES` order.
    probed: Vec<Option<String>>,
    /// Paired devices as the last answer of `peersus.peers` `list` named them: alias and name.
    peers: Vec<(String, String)>,
    show_trouble: bool,
    /// The last pointer, size and wheel of the pad, in words, and what the drawing makes of them.
    pointer: String,
    size: String,
    wheel: String,
    pointer_share: Option<u32>,
    pad_width: Option<i32>,
    pad_x: i32,
    wheel_steps: Vec<i32>,
    ticks: u64,
    /// The device chosen in the chooser: `Some(None)` is this device.
    chosen: Option<Option<String>>,
    /// Calls out, by id, and what each is for.
    calls: Vec<(u64, Asked)>,
    next: u64,
}

impl State {
    fn new() -> Self {
        Self {
            probed: vec![None; probes::PROBES.len()],
            status_input: DEFAULT_STATUS_LABEL.into(),
            pointer: "No touch yet".into(),
            size: "Not measured yet".into(),
            wheel: "No wheel yet".into(),
            ..Self::default()
        }
    }

    fn waiting(&self) -> bool {
        !self.calls.is_empty()
    }

    fn pending_probe(&self, index: usize) -> bool {
        self.calls
            .iter()
            .any(|(_, asked)| *asked == Asked::Probe(index))
    }

    /// A call to the application, remembered by its id until it is answered.
    fn call(
        &mut self,
        asked: Asked,
        service: &str,
        method: &str,
        args: Value,
        timeout_ms: u64,
    ) -> Value {
        self.next = self.next.checked_add(1).unwrap_or(1);
        self.calls.push((self.next, asked));
        json!({"op":"call","id":self.next,"provider":"host","service":service,"version":1,
            "method":method,"args":args,"timeout_ms":timeout_ms})
    }

    /// Its settings: read once it starts and each time its page opens, so a changed setting shows.
    fn ask_config(&mut self) -> Value {
        self.call(Asked::Config, "peersus.config", "get", json!({}), CALL_MS)
    }

    /// The page after `event`, which changes only what this extension keeps.
    fn answer(&mut self, event: &Value) -> Value {
        let id = event["id"].as_str().unwrap_or_default();
        match event["t"].as_str() {
            Some("press") => match id {
                "count" => self.pressed = self.pressed.saturating_add(1),
                "grow" => self.rows = (self.rows + 1).min(12),
                "clear" => self.rows = 0,
                "flood" => return page::flood(),
                "spam" => self.spammed = self.spammed.saturating_add(1),
                _ => {}
            },
            Some("toggle") if id == "switch" => self.on = event["on"].as_bool().unwrap_or(false),
            Some("toggle") if id == "reveal" => {
                self.reveal = event["on"].as_bool().unwrap_or(false)
            }
            Some("submit") if id == "say" => {
                self.said = probes::cut(event["text"].as_str().unwrap_or_default(), 200);
            }
            Some("pointer") if id == "pad" => {
                let x = event["x"].as_i64().unwrap_or(0);
                let y = event["y"].as_i64().unwrap_or(0);
                self.pointer = format!(
                    "Pointer {} at {:.1}, {:.1}",
                    event["phase"].as_str().unwrap_or("?"),
                    x as f64 / 1000.0,
                    y as f64 / 1000.0
                );
                if let Some(width) = self.pad_width.filter(|width| *width > 0) {
                    let across = (x - i64::from(self.pad_x)).clamp(0, i64::from(width));
                    self.pointer_share = u32::try_from(across * 10_000 / i64::from(width)).ok();
                }
            }
            Some("size") if id == "pad" => {
                let number = |key: &str| {
                    event[key]
                        .as_i64()
                        .and_then(|n| i32::try_from(n).ok())
                        .unwrap_or(0)
                };
                self.pad_x = number("x");
                self.pad_width = Some(number("width"));
                self.size = format!(
                    "Measured {:.0} × {:.0} at {:.0}, {:.0}",
                    f64::from(number("width")) / 1000.0,
                    f64::from(number("height")) / 1000.0,
                    f64::from(number("x")) / 1000.0,
                    f64::from(number("y")) / 1000.0
                );
            }
            Some("wheel") if id == "pad" => {
                let dy = event["dy"]
                    .as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .unwrap_or(0);
                self.wheel = format!("Wheel {}, {}", event["dx"].as_i64().unwrap_or(0), dy);
                if self.wheel_steps.len() == WHEEL_STEPS {
                    self.wheel_steps.remove(0);
                }
                self.wheel_steps.push(dy / 1000);
            }
            Some("tick") if id == "clock" => self.ticks = self.ticks.saturating_add(1),
            Some("choose") if id == "chooser" => {
                self.chosen = Some(
                    event["device"]
                        .as_str()
                        .map(|device| probes::cut(device, 64)),
                );
            }
            _ => {}
        }
        self.page()
    }

    /// The calls a delivery leads to, sent before it is answered so that the page that comes back already shows them as
    /// asked: a probe pressed, a notification asked for by its button or a bound key, the settings when the page opens.
    fn calls_for(&mut self, delivery: &Value) -> Vec<Value> {
        if delivery["type"] != "call" {
            return Vec::new();
        }
        let event = &delivery["args"]["event"];
        let kind = match delivery["method"].as_str() {
            Some("event") if event["t"] == "open" => return vec![self.ask_config()],
            Some("event") if event["t"] == "press" => event["id"].as_str().unwrap_or_default(),
            Some(method) => method,
            None => return Vec::new(),
        };
        if delivery["method"] == "event" && event["t"] == "submit" && event["id"] == "status-label"
        {
            self.status_input = event["text"].as_str().unwrap_or_default().trim().to_owned();
            return vec![self.define_status()];
        }
        if delivery["method"] == "event" && event["t"] == "press" && event["id"] == "reset-status" {
            self.status_input = DEFAULT_STATUS_LABEL.into();
            return vec![self.define_status()];
        }
        if let Some(index) = probes::pressed(kind) {
            let probe = &probes::PROBES[index];
            let args = if probe.id == "define" {
                json!({"id":"testing","label":self.status_input})
            } else {
                probes::args(probe)
            };
            return vec![self.call(
                Asked::Probe(index),
                probe.service,
                probe.method,
                args,
                CALL_MS,
            )];
        }
        let args = match kind {
            "notify" => json!({"text":"The Test extension says hello."}),
            "question" => {
                json!({"text":"The Test extension asks: which one?","reach":"outside","buttons":[
                    {"id":"this","label":"This one"},
                    {"id":"that","label":"That one"}
                ]})
            }
            _ => return Vec::new(),
        };
        self.asked = self.asked.saturating_add(1);
        self.notifying = self.notifying.saturating_add(1);
        vec![self.call(
            Asked::Notification,
            "peersus.notify",
            "show",
            args,
            NOTIFY_MS,
        )]
    }

    fn define_status(&mut self) -> Value {
        let index = probes::PROBES
            .iter()
            .position(|probe| probe.id == "define")
            .expect("the test extension declares its define probe");
        let probe = &probes::PROBES[index];
        self.call(
            Asked::Probe(index),
            probe.service,
            probe.method,
            json!({"id":"testing","label":self.status_input}),
            CALL_MS,
        )
    }

    /// The application's answer to a call this extension made. Any other delivery changes nothing.
    fn told(&mut self, delivery: &Value) {
        if delivery["type"] != "result" {
            return;
        }
        let Some(id) = delivery["id"].as_u64() else {
            return;
        };
        let Some(position) = self.calls.iter().position(|(call, _)| *call == id) else {
            return;
        };
        let (_, asked) = self.calls.remove(position);
        let result = &delivery["result"];
        match asked {
            Asked::Config => {
                if let Some(on) = result["value"]["settings"][SHOW_TROUBLE].as_bool() {
                    self.show_trouble = on;
                }
            }
            Asked::Probe(index) => {
                if probes::PROBES[index].id == "peers"
                    && let Some(peers) = result["value"].as_array()
                {
                    self.peers = peers
                        .iter()
                        .filter_map(|peer| {
                            Some((
                                probes::cut(peer["device"].as_str()?, 64),
                                probes::cut(peer["name"].as_str()?, 80),
                            ))
                        })
                        .collect();
                }
                if probes::PROBES[index].id == "config"
                    && let Some(on) = result["value"]["settings"][SHOW_TROUBLE].as_bool()
                {
                    self.show_trouble = on;
                }
                if result["status"] == "ok" {
                    match probes::PROBES[index].id {
                        "define" => {
                            if let Some(label) = result["value"]["label"].as_str() {
                                self.status_label = Some(label.to_owned());
                            }
                        }
                        "set" => self.status_on = true,
                        "clear" => self.status_on = false,
                        _ => {}
                    }
                }
                self.probed[index] = Some(if probes::PROBES[index].id == "own" {
                    probes::own_summary(result)
                } else {
                    probes::summary(result)
                });
            }
            Asked::Notification => {
                self.notifying = self.notifying.saturating_sub(1);
                self.told = match (
                    result["status"].as_str(),
                    result["value"]["answer"].as_str(),
                ) {
                    (Some("ok"), Some("pressed")) => {
                        format!(
                            "pressed {}",
                            probes::cut(result["value"]["button"].as_str().unwrap_or_default(), 64)
                        )
                    }
                    (Some("ok"), Some("dismissed")) => "closed".to_owned(),
                    (Some("ok"), _) => "shown".to_owned(),
                    _ => format!(
                        "refused ({})",
                        probes::cut(result["code"].as_str().unwrap_or_default(), 32)
                    ),
                };
            }
        }
    }
}

/// The answer to one call of the application. `None` for anything that is not a call.
fn handle(state: &mut State, delivery: &Value) -> Option<Value> {
    if delivery["type"] != "call" {
        return None;
    }
    let ticket = delivery["ticket"].as_u64()?;
    let result = match delivery["method"].as_str() {
        Some("event") => json!({"status":"ok","value":state.answer(&delivery["args"]["event"])}),
        // Every one of its key-bindable actions that counts calls this; the count shows next time the page is drawn.
        Some("bump") => {
            state.keyed = state.keyed.saturating_add(1);
            json!({"status":"ok","value":null})
        }
        // A bound key asked for a notification, which `calls_for` already sent.
        Some("notify" | "question") => json!({"status":"ok","value":null}),
        _ => json!({"status":"error","code":"failed","message":"Nothing here answers that"}),
    };
    Some(json!({"op":"reply","ticket":ticket,"result":result}))
}

/// Why the loop ends.
#[derive(Debug, PartialEq, Eq)]
enum End {
    /// The application went away: nothing more can be sent or received.
    Gone,
    /// "Leave" was pressed.
    Leave,
}

/// One delivery: the answer it carries, the trouble it asks for, the calls it leads to and the reply.
fn step<H: Wire>(host: &mut H, state: &mut State, delivery: &Value) -> Result<(), End> {
    state.told(delivery);
    if let Some(trouble) = trouble::asked(delivery) {
        let ticket = delivery["ticket"].as_u64().unwrap_or(0);
        match trouble::make(host, trouble, ticket) {
            trouble::After::Answer => {}
            trouble::After::Skip => return Ok(()),
            trouble::After::Leave => return Err(End::Leave),
        }
    }
    for call in state.calls_for(delivery) {
        host.send(call).map_err(|_| End::Gone)?;
    }
    if let Some(reply) = handle(state, delivery) {
        host.send(reply).map_err(|_| End::Gone)?;
    }
    Ok(())
}

/// The whole life of the guest. Returns zero when the application goes away, non-zero when it leaves on purpose.
#[cfg(any(target_family = "wasm", feature = "native"))]
fn run<H: Wire>(host: &mut H) -> i32 {
    let mut state = State::new();
    let config = state.ask_config();
    if host.send(config).is_err() {
        return 0;
    }
    loop {
        let Ok(delivery) = host.receive() else {
            return 0;
        };
        match step(host, &mut state, &delivery) {
            Ok(()) => {}
            Err(End::Gone) => return 0,
            Err(End::Leave) => return 1,
        }
    }
}

/// The entry the interpreter calls.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    run(&mut host::Host)
}

/// The entry the native host calls, with its table of the same functions the interpreter offers.
///
/// # Safety
/// `host` is the native host's table, valid for the whole call.
#[cfg(all(feature = "native", not(target_family = "wasm")))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn peersus_plugin_main(host: *const native::HostV1) -> i32 {
    if !native::enter(host) {
        return 1;
    }
    let result = run(&mut host::Host);
    native::leave();
    result
}

#[cfg(test)]
mod tests;
