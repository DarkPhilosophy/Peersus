//! Control's extension-owned interface, gesture interpretation and remote input transport.
#![cfg(any(target_family = "wasm", feature = "native", test))]

mod host;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
mod native;
mod remote;
mod layout;
mod ui;

/// The entry the interpreter calls. Returns zero when the application goes away, non-zero on a
/// broken protocol.
/// The entry the interpreter calls.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    run()
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
    let result = run();
    native::leave();
    result
}

#[cfg(any(target_family = "wasm", feature = "native"))]
fn run() -> i32 {
    use host::{Host, Wire};
    let mut host = Host;
    let mut state = remote::State::default();
    if state.configure_pages(&mut host).is_err() {
        return 1;
    }
    loop {
        let delivery = match state.next(&mut host) {
            Ok(Some(delivery)) => delivery,
            Ok(None) => continue,
            Err(_) => return 0,
        };
        if let Some(reply) = remote::handle(&mut host, &mut state, &delivery)
            && host.send(reply).is_err()
        {
            return 1;
        }
    }
}
