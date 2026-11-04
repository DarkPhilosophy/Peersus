//! The interpreter shared by desktop helpers and Android's separately managed services.
//! No WASI, filesystem, networking, native imports, or permission decisions live here.
//! Only the already-authenticated broker transport is supplied by the process bootstrap.
#![forbid(unsafe_code)]

use peersus_plugin_host::frame::CONTROL_LIMIT;
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    ops::Range,
};
use wasmi::{
    Caller, Config, EnforcedLimits, Engine, Error, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder,
};

#[cfg(unix)]
pub mod socket;

/// Limits apply independently to each supervised generation, never to the application thread.
pub const MAX_MODULE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_OPEN_STREAMS: usize = 32;
const TURN_FUEL: u64 = 2_000_000;
const MAX_IO_BYTES: usize = CONTROL_LIMIT;

/// Transport authority is provided by the existing host, not by the guest module.
/// Bootstrap attaches the existing DataHello token and ticket; guests cannot pick an identity.
pub trait Transport {
    type Stream: Read + Write;
    /// Exactly one bounded control message, without its framing prefix; zero means EOF.
    fn receive(&mut self, into: &mut [u8]) -> io::Result<usize>;
    fn send(&mut self, message: &[u8]) -> io::Result<()>;
    fn open_data(&mut self, request: DataRequest) -> io::Result<Self::Stream>;
    /// Finish outgoing bytes without closing the read half; wait for peer EOF before Reply.
    fn shutdown_data(&mut self, stream: &mut Self::Stream) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataRequest {
    Ticket(u64),
    Claim(u64),
}

struct State<'a, T: Transport> {
    transport: &'a mut T,
    limits: StoreLimits,
    streams: BTreeMap<i32, T::Stream>,
    next_handle: i32,
}

fn memory<T: Transport>(caller: &mut Caller<'_, State<'_, T>>) -> Result<Memory, Error> {
    caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .ok_or_else(|| Error::new("a plugin must export its memory"))
}

fn range(pointer: i32, length: i32) -> Result<Range<usize>, Error> {
    let start = usize::try_from(pointer).map_err(|_| Error::new("invalid guest memory range"))?;
    let length = usize::try_from(length).map_err(|_| Error::new("invalid guest memory range"))?;
    if length > MAX_IO_BYTES {
        return Err(Error::new("guest I/O exceeds its bound"));
    }
    let end = start
        .checked_add(length)
        .ok_or_else(|| Error::new("invalid guest memory range"))?;
    Ok(start..end)
}

fn progress<T: Transport>(
    caller: &mut Caller<'_, State<'_, T>>,
    amount: usize,
) -> Result<i32, Error> {
    // Only a real incoming message or positive data progress permits another work turn.
    // EOF, failed opens, arbitrary sends and a guest's compute loop never refill fuel.
    if amount > 0 {
        caller.set_fuel(TURN_FUEL)?;
    }
    i32::try_from(amount).map_err(|_| Error::new("invalid transport response"))
}

/// Execute one module in its already isolated worker, returning when it leaves or traps.
/// Callers must never execute an extension on an owner, UI, or Android main thread.
pub fn run<T: Transport>(bytes: &[u8], transport: &mut T) -> Result<(), Error> {
    if bytes.len() > MAX_MODULE_BYTES {
        return Err(Error::new("plugin module exceeds its size bound"));
    }
    let mut config = Config::default();
    config.consume_fuel(true);
    config.set_max_stack_height(16 * 1024);
    config.set_max_recursion_depth(256);
    config.allow_start_fn(false);
    config.enforced_limits(EnforcedLimits::strict());
    let engine = Engine::new(&config);
    let module = Module::new(&engine, bytes)?;
    let limits = StoreLimitsBuilder::new()
        .memory_size(MAX_MEMORY_BYTES)
        .table_elements(4096)
        .instances(1)
        .memories(1)
        .tables(1)
        .build();
    let mut store = Store::new(
        &engine,
        State {
            transport,
            limits,
            streams: BTreeMap::new(),
            next_handle: 1,
        },
    );
    store.limiter(|state| &mut state.limits);
    store.set_fuel(TURN_FUEL)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap(
        "peersus_v1",
        "receive",
        |mut caller: Caller<'_, State<'_, T>>, pointer: i32, length: i32| -> Result<i32, Error> {
            let memory = memory(&mut caller)?;
            let range = range(pointer, length)?;
            let (bytes, state) = memory.data_and_store_mut(&mut caller);
            let into = bytes
                .get_mut(range)
                .ok_or_else(|| Error::new("invalid guest memory range"))?;
            let amount = state
                .transport
                .receive(into)
                .map_err(|_| Error::new("control receive failed"))?;
            if amount > into.len() {
                return Err(Error::new("invalid transport response"));
            }
            progress(&mut caller, amount)
        },
    )?;
    linker.func_wrap(
        "peersus_v1",
        "send",
        |mut caller: Caller<'_, State<'_, T>>, pointer: i32, length: i32| -> Result<i32, Error> {
            let memory = memory(&mut caller)?;
            let range = range(pointer, length)?;
            let (bytes, state) = memory.data_and_store_mut(&mut caller);
            let message = bytes
                .get(range)
                .ok_or_else(|| Error::new("invalid guest memory range"))?;
            if message.is_empty() {
                return Err(Error::new("empty control message"));
            }
            state
                .transport
                .send(message)
                .map_err(|_| Error::new("control send failed"))?;
            Ok(0)
        },
    )?;
    linker.func_wrap(
        "peersus_v1",
        "data_open",
        |mut caller: Caller<'_, State<'_, T>>, kind: i32, number: i64| -> Result<i32, Error> {
            let request = match kind {
                0 => DataRequest::Ticket(number as u64),
                1 => DataRequest::Claim(number as u64),
                _ => return Err(Error::new("invalid data request")),
            };
            let state = caller.data_mut();
            if state.streams.len() >= MAX_OPEN_STREAMS {
                return Err(Error::new("too many open data streams"));
            }
            let handle = state.next_handle;
            state.next_handle = handle
                .checked_add(1)
                .ok_or_else(|| Error::new("data handles exhausted"))?;
            let stream = state
                .transport
                .open_data(request)
                .map_err(|_| Error::new("data connection failed"))?;
            state.streams.insert(handle, stream);
            Ok(handle)
        },
    )?;
    linker.func_wrap(
        "peersus_v1",
        "data_read",
        |mut caller: Caller<'_, State<'_, T>>,
         handle: i32,
         pointer: i32,
         length: i32|
         -> Result<i32, Error> {
            let memory = memory(&mut caller)?;
            let range = range(pointer, length)?;
            let (bytes, state) = memory.data_and_store_mut(&mut caller);
            let into = bytes
                .get_mut(range)
                .ok_or_else(|| Error::new("invalid guest memory range"))?;
            let stream = state
                .streams
                .get_mut(&handle)
                .ok_or_else(|| Error::new("invalid data handle"))?;
            let amount = stream
                .read(into)
                .map_err(|_| Error::new("data read failed"))?;
            progress(&mut caller, amount)
        },
    )?;
    linker.func_wrap(
        "peersus_v1",
        "data_write",
        |mut caller: Caller<'_, State<'_, T>>,
         handle: i32,
         pointer: i32,
         length: i32|
         -> Result<i32, Error> {
            let memory = memory(&mut caller)?;
            let range = range(pointer, length)?;
            let (bytes, state) = memory.data_and_store_mut(&mut caller);
            let from = bytes
                .get(range)
                .ok_or_else(|| Error::new("invalid guest memory range"))?;
            let stream = state
                .streams
                .get_mut(&handle)
                .ok_or_else(|| Error::new("invalid data handle"))?;
            let amount = stream
                .write(from)
                .map_err(|_| Error::new("data write failed"))?;
            progress(&mut caller, amount)
        },
    )?;
    linker.func_wrap(
        "peersus_v1",
        "data_shutdown",
        |mut caller: Caller<'_, State<'_, T>>, handle: i32| -> Result<i32, Error> {
            let state = caller.data_mut();
            let stream = state
                .streams
                .get_mut(&handle)
                .ok_or_else(|| Error::new("invalid data handle"))?;
            state
                .transport
                .shutdown_data(stream)
                .map_err(|_| Error::new("data shutdown failed"))?;
            Ok(0)
        },
    )?;
    linker.func_wrap(
        "peersus_v1",
        "data_close",
        |mut caller: Caller<'_, State<'_, T>>, handle: i32| -> Result<i32, Error> {
            caller
                .data_mut()
                .streams
                .remove(&handle)
                .ok_or_else(|| Error::new("invalid data handle"))?;
            Ok(0)
        },
    )?;
    let instance = linker.instantiate_and_start(&mut store, &module)?;
    memory_from_instance(&instance, &store)?;
    let entry = instance.get_typed_func::<(), i32>(&store, "peersus_main")?;
    match entry.call(&mut store, ())? {
        0 => Ok(()),
        _ => Err(Error::new("plugin entry returned an error")),
    }
}

fn memory_from_instance<T: Transport>(
    instance: &wasmi::Instance,
    store: &Store<State<'_, T>>,
) -> Result<(), Error> {
    instance
        .get_memory(store, "memory")
        .map(|_| ())
        .ok_or_else(|| Error::new("a plugin must export its memory"))
}

#[cfg(test)]
mod tests;
