//! Control's extension-owned interface, gesture interpretation and remote input transport.
#![cfg(any(target_family = "wasm", test))]

mod host;
mod remote;
mod layout;
mod ui;

/// The entry the interpreter calls. Returns zero when the application goes away, non-zero on a
/// broken protocol.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
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
