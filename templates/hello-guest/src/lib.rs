//! The smallest extension that answers a call.
//!
//! A guest has no clock, no files, no network and no standard input: the application owns every one of those, and
//! this module can only exchange JSON messages with it through `peersus_v1`. `receive` waits for the next message and
//! returns zero length when the application goes away, which is the signal to stop.
#![cfg(target_family = "wasm")]

use serde_json::{Value, json};

/// The largest control message the application sends or accepts in either direction.
const LIMIT: usize = 48 * 1024;

#[link(wasm_import_module = "peersus_v1")]
unsafe extern "C" {
    // Pointers and lengths are `usize`: 32 bits in a wasm32 module and 64 in a wasm64 one, as the host expects of each.
    #[link_name = "receive"]
    fn receive(buffer: *mut u8, capacity: usize) -> i32;
    #[link_name = "send"]
    fn send(buffer: *const u8, length: usize) -> i32;
}

/// The next message from the application, or `None` once it has gone away or sent something that is not JSON.
fn next_message() -> Option<Value> {
    let mut buffer = vec![0u8; LIMIT];
    let count = unsafe { receive(buffer.as_mut_ptr(), buffer.len()) };
    let count = usize::try_from(count)
        .ok()
        .filter(|count| *count > 0 && *count <= LIMIT)?;
    serde_json::from_slice(&buffer[..count]).ok()
}

fn say(message: &Value) -> bool {
    let bytes = message.to_string().into_bytes();
    unsafe { send(bytes.as_ptr(), bytes.len()) == 0 }
}

/// The entry the application calls. Returns zero when the application goes away, non-zero when the protocol breaks.
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    while let Some(message) = next_message() {
        // Only calls are answered. Anything else (a notice, a result for a call this guest never made) is not for it.
        if message["type"] != "call" {
            continue;
        }
        let Some(ticket) = message["ticket"].as_u64() else {
            return 1;
        };
        let result = match message["method"].as_str() {
            Some("greet") => json!({"status": "ok", "value": "Hello from an extension"}),
            _ => {
                json!({"status": "error", "code": "unknown", "message": "There is no such method"})
            }
        };
        if !say(&json!({"op": "reply", "ticket": ticket, "result": result})) {
            return 1;
        }
    }
    0
}
