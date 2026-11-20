//! The Canvas fixture uses only the public Peersus APIs. It has no timer, OS access or renderer:
//! mouse/size events advance its scene, and one display list describes the next complete frame.
#![cfg(target_family = "wasm")]

use peersus_api::{
    canvas::{self, CanvasPalette, FrameLayout, display as dl},
    elements::{Event, FEATURE_CANVAS, PAGE_FEATURES_KEY, PointerPhase},
    message::{Delivery, MAX_PAYLOAD, Operation, Outcome},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const VIEW: &str = "canvas-demo.view";
const CANVAS: &str = "scene";
const CONFIG: u64 = 1;
const PLACEMENT: u64 = 2;
const PATHS: usize = 50 * 30;
const LIGHT_PATHS: usize = 30;
const DENSE_BG: [u8; 4] = [12, 22, 35, 255];
const LIGHT_BG: [u8; 4] = [12, 22, 36, 255];

#[link(wasm_import_module = "peersus_v1")]
unsafe extern "C" {
    fn receive(buffer: *mut u8, capacity: usize) -> i32;
    fn send(buffer: *const u8, length: usize) -> i32;
    fn data_open(kind: i32, number: u64) -> i32;
    fn canvas_present(buffer: *const u8, length: usize) -> i32;
}

fn submit(message: Operation) -> Result<(), &'static str> {
    let bytes = serde_json::to_vec(&message).map_err(|_| "invalid message")?;
    if bytes.len() > MAX_PAYLOAD {
        return Err("oversize message");
    }
    // SAFETY: the nonempty serialized operation lives for the duration of this synchronous import.
    if unsafe { send(bytes.as_ptr(), bytes.len()) } == 0 {
        Ok(())
    } else {
        Err("control closed")
    }
}

fn call(id: u64, service: &str, method: &str, args: Value) -> Result<(), &'static str> {
    submit(Operation::Call {
        id,
        provider: "host".into(),
        service: service.into(),
        version: 1,
        method: method.into(),
        args,
        timeout_ms: 5_000,
    })
}

fn reply(ticket: u64, result: Outcome) -> Result<(), &'static str> {
    submit(Operation::Reply { ticket, result })
}

fn tree() -> Value {
    json!({"k":"column","children":[
        {"k":"text","tone":"title","text":"Canvas demo"},
        {"k":"row","children":[
            {"k":"button","id":"light","label":"Few paths"},
            {"k":"button","id":"dense","label":"Dense paths"},
            {"k":"button","id":"reset","label":"Reset view"}
        ]},
        {"k":"canvas","id":CANVAS,"observe":["size","wheel"],
         "style":{"width":{"unit":"px","value":640},"height":{"unit":"px","value":400},
                   "clip":true,"follow":{"drag":true,"wheel_doubling":120}},
         "a11y":[{"id":"description","role":"text","label":"Synthetic flow field. Drag to pan or scroll to zoom.",
                   "rect":{"x":12000,"y":12000,"width":480000,"height":40000}}]}
    ]})
}

struct Viewport {
    x: i32,
    y: i32,
    layout: FrameLayout,
    scale: u32,
    palette: CanvasPalette,
}

struct Scene {
    viewport: Option<Viewport>,
    stream: Option<i32>,
    opening: Option<u64>,
    seq: u64,
    settled: u64,
    dragging: Option<[i32; 2]>,
    shift: [f32; 2],
    zoom: f32,
    dense: bool,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            viewport: None,
            stream: None,
            opening: None,
            seq: 0,
            settled: 0,
            dragging: None,
            shift: [0.; 2],
            zoom: 1.,
            dense: true,
        }
    }
}

impl Scene {
    fn input(&mut self, event: Event) -> bool {
        match event {
            Event::CanvasSize {
                id,
                x,
                y,
                width,
                height,
                scale,
                palette,
            } if id == CANVAS => {
                let device = |axis| (u64::from(axis) * u64::from(scale)).div_ceil(1_000_000) as u32;
                let Ok(layout) = FrameLayout::tight(device(width), device(height)) else {
                    return false;
                };
                self.viewport = Some(Viewport {
                    x,
                    y,
                    layout,
                    scale,
                    palette,
                });
                true
            }
            Event::Pointer {
                id,
                phase,
                x,
                y,
                at,
            } if id == CANVAS => match phase {
                PointerPhase::Down => {
                    self.dragging = Some([x, y]);
                    false
                }
                PointerPhase::Up | PointerPhase::Cancel => {
                    self.dragging = None;
                    false
                }
                PointerPhase::Move => {
                    let Some(old) = self.dragging.replace([x, y]) else {
                        self.dragging = None;
                        return false;
                    };
                    self.shift[0] = (self.shift[0] + x.saturating_sub(old[0]) as f32 / 1000.)
                        .clamp(-2000., 2000.);
                    self.shift[1] = (self.shift[1] + y.saturating_sub(old[1]) as f32 / 1000.)
                        .clamp(-2000., 2000.);
                    self.settled = at;
                    true
                }
            },
            Event::Wheel {
                id, x, y, dy, at, ..
            } if id == CANVAS => {
                let Some(v) = &self.viewport else {
                    return false;
                };
                let next =
                    (self.zoom * (-(dy as f32) / 120_000.).clamp(-4., 4.).exp2()).clamp(0.4, 3.);
                let factor = next / self.zoom;
                let anchor = [
                    x.saturating_sub(v.x) as f32 / 1000.,
                    y.saturating_sub(v.y) as f32 / 1000.,
                ];
                for (shift, around) in self.shift.iter_mut().zip(anchor) {
                    *shift = (factor * *shift + (1. - factor) * around).clamp(-2000., 2000.);
                }
                self.zoom = next;
                self.settled = at;
                true
            }
            Event::Press { id } if id == "light" || id == "dense" => {
                self.dense = id == "dense";
                true
            }
            Event::Press { id } if id == "reset" => {
                self.shift = [0.; 2];
                self.zoom = 1.;
                true
            }
            _ => false,
        }
    }

    fn update(&mut self, page: &str, next: &mut u64, art: &mut Art) -> Result<(), &'static str> {
        let Some(v) = &self.viewport else {
            return Ok(());
        };
        let Some(stream) = self.stream else {
            if self.opening.is_none() {
                *next = next.checked_add(1).ok_or("exhausted ids")?;
                call(
                    *next,
                    canvas::SERVICE,
                    canvas::OPEN,
                    json!({"page":page,"canvas": CANVAS}),
                )?;
                self.opening = Some(*next);
            }
            return Ok(());
        };
        self.seq = self.seq.checked_add(1).ok_or("exhausted frames")?;
        let out = &mut art.frame;
        out.clear();
        out.extend(dl::MAGIC);
        out.extend([dl::BEGIN, CANVAS.len() as u8]);
        out.extend(CANVAS.bytes());
        out.extend(stream.to_le_bytes());
        out.extend(self.seq.to_le_bytes());
        out.extend(self.settled.to_le_bytes());
        for n in [v.layout.width(), v.layout.height(), v.scale] {
            out.extend(n.to_le_bytes());
        }
        let p = &v.palette;
        for c in [
            p.surface, p.raised, p.text, p.muted, p.border, p.scrim, p.accent, p.success, p.danger,
        ] {
            out.extend(c);
        }
        out.extend([dl::CLEAR, 0]);
        out.extend(if self.dense { DENSE_BG } else { LIGHT_BG });
        out.push(dl::TRANSFORM_PUSH);
        numbers(
            out,
            [self.zoom, 0., 0., self.zoom, self.shift[0], self.shift[1]],
        );
        out.extend_from_slice(if self.dense { &art.dense } else { &art.light });
        out.push(dl::TRANSFORM_POP);
        text(out, "FLOW FIELD", 21., 22., 31., [233, 244, 255, 255]);
        text(
            out,
            if self.dense {
                "1,500 stroked paths"
            } else {
                "30 stroked paths"
            },
            15.,
            22.,
            55.,
            [159, 194, 242, 255],
        );
        text(
            out,
            "Drag to pan  /  Scroll to zoom",
            15.,
            22.,
            377.,
            [211, 228, 244, 255],
        );
        out.push(dl::END);
        // SAFETY: the synchronous import reads this complete bytes slice while it is borrowed.
        match unsafe { canvas_present(out.as_ptr(), out.len()) } {
            canvas::PRESENT_OK | canvas::PRESENT_LIMITED => Ok(()),
            canvas::PRESENT_STREAM_FAILED => {
                self.stream = None;
                Ok(())
            }
            _ => Err("invalid canvas display list"),
        }
    }
}

struct Art {
    dense: Vec<u8>,
    light: Vec<u8>,
    frame: Vec<u8>,
}

fn numbers(out: &mut Vec<u8>, numbers: impl IntoIterator<Item = f32>) {
    for number in numbers {
        out.extend(number.to_le_bytes());
    }
}

fn text(out: &mut Vec<u8>, text: &str, size: f32, x: f32, y: f32, rgba: [u8; 4]) {
    out.push(dl::TEXT);
    numbers(out, [size]);
    out.push(0);
    out.extend(rgba);
    numbers(out, [x, y]);
    out.push(0); // left aligned
    out.extend((text.len() as u16).to_le_bytes());
    out.extend(text.bytes());
}

impl Art {
    fn new() -> Self {
        const PATH_BYTES: usize = 42;
        let mut dense = Vec::with_capacity(PATHS * PATH_BYTES);
        let mut light = Vec::with_capacity(LIGHT_PATHS * PATH_BYTES);
        for i in 0..PATHS {
            let col = i % 50;
            let row = i / 50;
            let (x, y) = (24. + col as f32 * 12., 76. + row as f32 * 9.4);
            let wave = (col as f32 * 0.14 + row as f32 * 0.22).sin();
            let start = dense.len();
            dense.extend([
                dl::PATH,
                1,
                0,
                50 + row as u8 * 4,
                105 + col as u8 * 2,
                225,
                235,
            ]);
            numbers(&mut dense, [1.9]);
            dense.extend([1, 1, 255]); // round cap and join, full opacity
            dense.extend(2u16.to_le_bytes());
            dense.push(0); // move
            numbers(&mut dense, [x, y]);
            dense.push(2); // quadratic curve
            numbers(&mut dense, [x + 6., y - 4. * wave, x + 10., y + 4. * wave]);
            if col == (row * 13) % 50 {
                light.extend_from_slice(&dense[start..]);
            }
        }
        Self {
            frame: Vec::with_capacity(dense.len() + 256),
            dense,
            light,
        }
    }
}

fn run() -> Result<(), &'static str> {
    let mut scene = BTreeMap::<String, Scene>::new();
    let mut art = Art::new();
    let mut next = PLACEMENT;
    let mut control = vec![0; MAX_PAYLOAD];
    call(CONFIG, "peersus.config", "get", json!({}))?;
    loop {
        // SAFETY: the import can write only into this live control buffer.
        let received = unsafe { receive(control.as_mut_ptr(), control.len()) };
        if received == 0 {
            return Ok(());
        }
        let bytes = usize::try_from(received)
            .ok()
            .and_then(|len| control.get(..len))
            .ok_or("invalid control size")?;
        match serde_json::from_slice(bytes).map_err(|_| "invalid delivery")? {
            Delivery::Result {
                id: CONFIG,
                result: Outcome::Ok { value },
            } => {
                if !value[PAGE_FEATURES_KEY]
                    .as_array()
                    .is_some_and(|list| list.iter().any(|feature| feature == FEATURE_CANVAS))
                {
                    return Err("desktop Canvas is not available");
                }
                if value["settings"]["show-canvas"] == true {
                    call(
                        PLACEMENT,
                        "peersus.pages",
                        "set",
                        json!({"pages":[{"id":"show-canvas","page":"demo","placement":"primary"}]}),
                    )?;
                }
            }
            Delivery::Result { id, result } if id > PLACEMENT => {
                if let Some((page, state)) = scene
                    .iter_mut()
                    .find(|(_, entry)| entry.opening == Some(id))
                {
                    state.opening = None;
                    if let Outcome::Ok { value } = result
                        && let Some(claim) = value["claim"].as_u64()
                    {
                        // SAFETY: canvas open is a scalar import; the owner issued this one-shot claim.
                        let stream = unsafe { data_open(canvas::CANVAS_DATA_KIND, claim) };
                        if stream > 0 {
                            state.stream = Some(stream);
                            state.update(page, &mut next, &mut art)?;
                        }
                    }
                }
            }
            Delivery::Call {
                ticket,
                caller,
                service,
                version: 1,
                method,
                mut args,
            } if caller == "host" && service == VIEW && method == "page" => {
                let page = args["page"]
                    .as_str()
                    .filter(|p| ["demo", "show-canvas"].contains(p))
                    .ok_or("invalid page")?
                    .to_owned();
                let event = serde_json::from_value::<Event>(args["event"].take())
                    .map_err(|_| "invalid event")?;
                event.validate().map_err(|_| "invalid event values")?;
                let state = scene.entry(page.clone()).or_default();
                if matches!(event, Event::Open) {
                    reply(ticket, Outcome::ok(tree()))?;
                } else {
                    if state.input(event) {
                        state.update(&page, &mut next, &mut art)?;
                    }
                    reply(ticket, Outcome::ok(Value::Null))?;
                }
            }
            Delivery::Call { ticket, .. } => reply(
                ticket,
                Outcome::error("unknown-method", "This fixture only draws its own page"),
            )?,
            _ => {}
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    i32::from(run().is_err())
}
