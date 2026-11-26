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
    let host = HOST.with(Cell::get);
    if host.is_null() {
        return -2;
    }
    // A host without the optional readiness wait: a transport failure, as the header says.
    // SAFETY: `enter` saved the original pointer to the live host table, not a reference to its prefix.
    let Some(poll) = (unsafe { poll_member(host) }) else {
        return -2;
    };
    // SAFETY: `host` still points to the live table; the caller's slice is valid for `count` handles.
    unsafe { poll((*host).context, handles, width(count), timeout_ms) }
}

/// The host's `poll`, if its table is long enough to hold one.
///
/// Call with the original, non-null pointer to the live table, including any optional tail.
unsafe fn poll_member(host: *const HostV1) -> Option<PollFn> {
    // SAFETY: `struct_size` is in the fixed prefix the caller supplies.
    let size = unsafe { (*host).struct_size } as usize;
    if size < POLL_OFFSET + std::mem::size_of::<Option<PollFn>>() {
        return None;
    }
    // SAFETY: `struct_size` covers the member the pinned header places after the fixed ones.
    // The original pointer covers the whole table, unlike one derived from a prefix reference.
    // Reading an optional function pointer handles a null member without calling through it.
    unsafe {
        host.cast::<u8>()
            .add(POLL_OFFSET)
            .cast::<Option<PollFn>>()
            .read_unaligned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    fn prefix(size: usize, context: *mut c_void) -> HostV1 {
        macro_rules! unused {
            ($($arg:ty),*) => {{
                unsafe extern "C" fn call($(_: $arg),*) -> i32 {
                    -1
                }
                call
            }};
        }
        HostV1 {
            abi_version: ABI_VERSION,
            struct_size: size.try_into().unwrap(),
            context,
            receive: unused!(*mut c_void, *mut u8, u32),
            send: unused!(*mut c_void, *const u8, u32),
            data_open: unused!(*mut c_void, i32, u64),
            data_read: unused!(*mut c_void, i32, *mut u8, u32),
            data_write: unused!(*mut c_void, i32, *const u8, u32),
            data_close: unused!(*mut c_void, i32),
            data_shutdown: unused!(*mut c_void, i32),
        }
    }

    #[test]
    fn polling_accepts_only_an_available_member_of_the_host_table() {
        #[repr(C)]
        struct Extended {
            prefix: HostV1,
            poll: Option<PollFn>,
        }
        unsafe extern "C" fn ready(
            context: *mut c_void,
            handles: *const i32,
            count: u32,
            timeout_ms: i32,
        ) -> i32 {
            if context.is_null() || count != 1 || timeout_ms != 50 {
                return -1;
            }
            // SAFETY: the fixture supplies an i32 context and one i32 handle.
            unsafe { context.cast::<i32>().read() + handles.read() }
        }

        assert_eq!(std::mem::offset_of!(Extended, poll), POLL_OFFSET);
        let handles = [7];
        // SAFETY: the buffer is valid for the one handle passed to the ABI.
        let wait = || unsafe { poll(handles.as_ptr(), handles.len(), 50) };
        assert_eq!(wait(), -2);
        let old = prefix(std::mem::size_of::<HostV1>(), ptr::null_mut());
        assert!(enter(ptr::from_ref(&old)));
        assert_eq!(wait(), -2);
        leave();

        let context = 11;
        for (member, expected) in [(None, -2), (Some(ready as PollFn), 18)] {
            let table = Extended {
                prefix: prefix(
                    std::mem::size_of::<Extended>(),
                    ptr::from_ref(&context).cast_mut().cast(),
                ),
                poll: member,
            };
            assert!(enter(ptr::from_ref(&table).cast()));
            assert_eq!(wait(), expected);
            leave();
        }
        assert_eq!(wait(), -2);
    }
}
