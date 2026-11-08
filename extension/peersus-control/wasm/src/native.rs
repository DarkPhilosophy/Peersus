//! The same guest built as a native library (`native_library`, second tier, not recommended): the functions the
//! WebAssembly build imports from `peersus_v1` are here calls through the table the native host hands to
//! `peersus_plugin_main` (`crates/peersus-plugin-runtime/sdk/peersus_plugin.h`). Nothing else differs between the two
//! builds. Unlike the interpreter, a native library runs as machine code with the application user's privileges.
#![allow(dead_code)]

use std::{cell::Cell, ffi::c_void};

/// `PeersusHostV1`, field for field.
#[repr(C)]
pub struct HostV1 {
    abi_version: u32,
    struct_size: u32,
    context: *mut c_void,
    receive: unsafe extern "C" fn(*mut c_void, *mut u8, u32) -> i32,
    send: unsafe extern "C" fn(*mut c_void, *const u8, u32) -> i32,
    data_open: unsafe extern "C" fn(*mut c_void, i32, u64) -> i32,
    data_read: unsafe extern "C" fn(*mut c_void, i32, *mut u8, u32) -> i32,
    data_write: unsafe extern "C" fn(*mut c_void, i32, *const u8, u32) -> i32,
    data_close: unsafe extern "C" fn(*mut c_void, i32) -> i32,
    data_shutdown: unsafe extern "C" fn(*mut c_void, i32) -> i32,
    /// Optional: present only when `struct_size` covers it.
    poll: unsafe extern "C" fn(*mut c_void, *const i32, u32, i32) -> i32,
}

const ABI_VERSION: u32 = 1;

thread_local! {
    static HOST: Cell<*const HostV1> = const { Cell::new(std::ptr::null()) };
}

/// Accept the host's table for the duration of `peersus_plugin_main`; false when it is not one this build understands.
pub fn enter(host: *const HostV1) -> bool {
    if host.is_null() {
        return false;
    }
    // SAFETY: the native host passes a pointer to its table, valid for the whole call of `peersus_plugin_main`; only the
    // two leading fixed fields are read before the size is known.
    let (version, size) = unsafe { ((*host).abi_version, (*host).struct_size) };
    if version != ABI_VERSION || (size as usize) < std::mem::offset_of!(HostV1, poll) {
        return false;
    }
    HOST.with(|cell| cell.set(host));
    true
}

/// The table is not used after `peersus_plugin_main` returns.
pub fn leave() {
    HOST.with(|cell| cell.set(std::ptr::null()));
}

fn table() -> Option<&'static HostV1> {
    let host = HOST.with(Cell::get);
    // SAFETY: set by `enter` from the host's table, which outlives every call made while `peersus_plugin_main` runs;
    // cleared by `leave` before it returns.
    unsafe { host.as_ref() }
}

/// A length the native ABI can carry: a larger buffer is offered as its first 4 GiB, which no message comes near.
fn width(length: usize) -> u32 {
    u32::try_from(length).unwrap_or(u32::MAX)
}

pub unsafe fn receive(buffer: *mut u8, capacity: usize) -> i32 {
    // SAFETY: the caller's buffer is valid for `capacity` bytes, as the wasm import requires too.
    table().map_or(-1, |h| unsafe { (h.receive)(h.context, buffer, width(capacity)) })
}

pub unsafe fn send(buffer: *const u8, length: usize) -> i32 {
    // SAFETY: the caller's buffer is valid for `length` bytes.
    table().map_or(-1, |h| unsafe { (h.send)(h.context, buffer, width(length)) })
}

pub unsafe fn data_open(kind: i32, number: u64) -> i32 {
    // SAFETY: plain values only.
    table().map_or(-1, |h| unsafe { (h.data_open)(h.context, kind, number) })
}

pub unsafe fn data_read(handle: i32, buffer: *mut u8, capacity: usize) -> i32 {
    // SAFETY: the caller's buffer is valid for `capacity` bytes.
    table().map_or(-1, |h| unsafe { (h.data_read)(h.context, handle, buffer, width(capacity)) })
}

pub unsafe fn data_write(handle: i32, buffer: *const u8, length: usize) -> i32 {
    // SAFETY: the caller's buffer is valid for `length` bytes.
    table().map_or(-1, |h| unsafe { (h.data_write)(h.context, handle, buffer, width(length)) })
}

pub unsafe fn data_close(handle: i32) -> i32 {
    // SAFETY: plain values only.
    table().map_or(-1, |h| unsafe { (h.data_close)(h.context, handle) })
}

pub unsafe fn data_shutdown(handle: i32) -> i32 {
    // SAFETY: plain values only.
    table().map_or(-1, |h| unsafe { (h.data_shutdown)(h.context, handle) })
}

pub unsafe fn poll(handles: *const i32, count: usize, timeout_ms: i32) -> i32 {
    let Some(h) = table() else { return -2 };
    if (h.struct_size as usize) < std::mem::size_of::<HostV1>() {
        // A host without the optional readiness wait: a transport failure, as the header says.
        return -2;
    }
    // SAFETY: the caller's slice is valid for `count` handles; the member is present (checked above).
    unsafe { (h.poll)(h.context, handles, width(count), timeout_ms) }
}
