//! Optional paired-device messaging, entirely implemented by the guest.
extern crate peersus_api as sdk;
extern crate self as serde_json;
use sdk::serde;
pub use sdk::serde_json::*;

mod core;
#[cfg(test)]
mod tests;
#[cfg(any(test, target_family = "wasm", feature = "native"))]
mod ui;
mod wire;

#[cfg(any(target_family = "wasm", feature = "native"))]
mod host;
#[cfg(any(target_family = "wasm", feature = "native"))]
mod media_sdk;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
mod native;
#[cfg(any(target_family = "wasm", feature = "native"))]
mod runner;
#[cfg(any(target_family = "wasm", feature = "native"))]
mod store;

#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    runner::run(&mut host::Host)
}

#[cfg(all(not(target_family = "wasm"), feature = "native"))]
#[unsafe(no_mangle)]
/// # Safety
/// `table` must point to the native SDK table for the duration of this call.
pub unsafe extern "C" fn peersus_plugin_main(table: *const native::HostV1) -> i32 {
    if !native::enter(table) {
        return 1;
    }
    let code = runner::run(&mut host::Host);
    native::leave();
    code
}
