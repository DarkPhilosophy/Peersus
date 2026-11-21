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
    // `poll` follows as an optional append-only member. It is not declared here: a host may pass a
    // table that ends before it, and a reference to this struct must never cover memory the host did
    // not supply. It is read through [`poll_member`] only when `struct_size` covers it.
}

/// `PeersusHostV1::poll`, the optional member right after the fixed ones.
type PollFn = unsafe extern "C" fn(*mut c_void, *const i32, u32, i32) -> i32;

/// The fixed members end where `poll` starts: every member is pointer-aligned after the two `u32`s.
const POLL_OFFSET: usize = std::mem::size_of::<HostV1>();

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
    if version != ABI_VERSION || (size as usize) < std::mem::size_of::<HostV1>() {
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
    table().map_or(-1, |h| unsafe {
        (h.receive)(h.context, buffer, width(capacity))
    })
}

pub unsafe fn send(buffer: *const u8, length: usize) -> i32 {
    // SAFETY: the caller's buffer is valid for `length` bytes.
    table().map_or(-1, |h| unsafe {
        (h.send)(h.context, buffer, width(length))
    })
}

pub unsafe fn data_open(kind: i32, number: u64) -> i32 {
    // SAFETY: plain values only.
    table().map_or(-1, |h| unsafe { (h.data_open)(h.context, kind, number) })
}

pub unsafe fn data_read(handle: i32, buffer: *mut u8, capacity: usize) -> i32 {
    // SAFETY: the caller's buffer is valid for `capacity` bytes.
    table().map_or(-1, |h| unsafe {
        (h.data_read)(h.context, handle, buffer, width(capacity))
    })
}

pub unsafe fn data_write(handle: i32, buffer: *const u8, length: usize) -> i32 {
    // SAFETY: the caller's buffer is valid for `length` bytes.
    table().map_or(-1, |h| unsafe {
        (h.data_write)(h.context, handle, buffer, width(length))
    })
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
    // A host without the optional readiness wait: a transport failure, as the header says.
    let Some(poll) = poll_member(h) else {
        return -2;
    };
    // SAFETY: the caller's slice is valid for `count` handles.
    unsafe { poll(h.context, handles, width(count), timeout_ms) }
}

/// The host's `poll`, if its table is long enough to hold one.
fn poll_member(h: &HostV1) -> Option<PollFn> {
    if (h.struct_size as usize) < POLL_OFFSET + std::mem::size_of::<Option<PollFn>>() {
        return None;
    }
    // SAFETY: `struct_size` says the host's table extends over the member, which the pinned header
    // places right after the fixed ones; it is read unaligned-safe as an optional function pointer,
    // so a null member is `None` rather than a call through null.
    unsafe {
        (h as *const HostV1)
            .cast::<u8>()
            .add(POLL_OFFSET)
            .cast::<Option<PollFn>>()
            .read_unaligned()
    }
}
