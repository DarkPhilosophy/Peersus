//! The interpreter shared by desktop helpers and Android's separately managed services.
//! No WASI, filesystem, networking, native imports, or permission decisions live here.
//! Only the already-authenticated broker transport is supplied by the process bootstrap.
#![forbid(unsafe_code)]

use peersus_plugin::frame::CONTROL_LIMIT;
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    ops::Range,
};
pub use wasmi::Error as RunError;
use wasmi::{
    Caller, Config, EnforcedLimits, Engine, Error, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder,
};

pub mod desktop;
mod diagnostic;
pub mod socket;

/// Limits apply independently to each supervised generation, never to the application thread.
pub const MAX_MODULE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_OPEN_STREAMS: usize = 32;
/// The desktop window, rather than the ordinary file-data deadline, bounds a stalled canvas write.
const CANVAS_STREAM_IO_WAIT: std::time::Duration = std::time::Duration::from_millis(25);
/// Work one guest may do for one message or one piece of data, in interpreter fuel. Measured: answering one event with a
/// page of a dozen rows through serde_json costs a guest about four million; two million, the bound before, ended guests
/// in the middle of ordinary work (a page, a batch of pointer input) and they were restarted without a word. Still a bound:
/// a guest that spins is stopped after a few tens of milliseconds of work, never left running.
const TURN_FUEL: u64 = 50_000_000;
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
    /// Wait for control (0) or one selected readable data handle, including EOF.
    /// `None` means timeout; implementations must not consume any bytes.
    fn poll(
        &mut self,
        _streams: &[(i32, &Self::Stream)],
        _timeout_ms: i32,
    ) -> io::Result<Option<i32>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "transport readiness is unavailable",
        ))
    }

    /// A measurement transport can opt in; the branch is absent for ordinary workers.
    const MEASURE_TURNS: bool = false;

    fn turn_usage(&mut self, _fuel: u64, _memory_bytes: usize) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataRequest {
    Ticket(u64),
    Claim(u64),
    /// An application-issued claim for a canvas stream, with bounded writes.
    CanvasClaim(u64),
}

/// Optional renderer integration. The interpreter and ordinary data handles require no graphics library.
pub trait CanvasStreams<S> {
    fn count(&self) -> usize;
    fn open(&mut self, handle: i32, stream: S);
    fn close(&mut self, handle: i32) -> bool;
    fn present(&mut self, list: &[u8]) -> i32;
    /// Decode a picture in this helper ([`peersus_api::pictures::DECODE_FUNCTION`]): its width, height and unpremultiplied
    /// RGBA, or a negative `DECODE_*` code.
    fn decode_picture(&mut self, encoded: &[u8], max_edge: u32)
    -> Result<(u32, u32, Vec<u8>), i64>;
    /// Start decoding a sound in this helper ([`peersus_api::audio::OPEN_FUNCTION`]): a positive handle or a negative
    /// `audio::DECODE_*` code.
    fn audio_open(&mut self, encoded: &[u8]) -> i64;
    /// The next samples into `into` ([`peersus_api::audio::READ_FUNCTION`]): bytes written, `0` at the end, or a code.
    fn audio_read(&mut self, handle: i64, into: &mut [u8]) -> i64;
    /// Frames of the whole sound ([`peersus_api::audio::LENGTH_FUNCTION`]), or a code.
    fn audio_length(&mut self, handle: i64) -> i64;
    /// Let go of a decoder ([`peersus_api::audio::CLOSE_FUNCTION`]).
    fn audio_close(&mut self, handle: i64) -> i64;
}

struct State<'a, T: Transport> {
    transport: &'a mut T,
    limits: StoreLimits,
    streams: BTreeMap<i32, T::Stream>,
    canvases: Option<&'a mut dyn CanvasStreams<T::Stream>>,
    next_handle: i32,
    /// The guest's exported memory, taken once after instantiation. Host functions use it
    /// instead of asking the caller for the export on every call: that lookup reads the
    /// calling instance through the interpreter's frame, and on Windows development builds
    /// that read faulted (access violation inside `Caller::get_export`).
    memory: Option<Memory>,
    diagnostic: diagnostic::Budget,
}

impl<T: Transport> State<'_, T> {
    fn stream_count(&self) -> usize {
        self.streams.len()
            + self
                .canvases
                .as_ref()
                .map_or(0, |canvases| canvases.count())
    }

    fn close_stream(&mut self, handle: i32) -> bool {
        self.streams.remove(&handle).is_some()
            || self
                .canvases
                .as_mut()
                .is_some_and(|canvases| canvases.close(handle))
    }
}

fn memory<T: Transport>(caller: &mut Caller<'_, State<'_, T>>) -> Result<Memory, Error> {
    if let Some(memory) = caller.data().memory {
        return Ok(memory);
    }
    // Only a start function runs before the memory is taken.
    caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .ok_or_else(|| Error::new("a plugin must export its memory"))
}

/// A guest pointer or length as its module declares it: 32 bits in a wasm32 module, 64 in a wasm64 one.
/// Whatever the width, it becomes one `usize` here and nowhere else, and anything that does not fit is refused
/// rather than wrapped: a negative 64-bit pointer must never turn into a valid small address.
trait Width: Copy + wasmi::WasmTy {
    fn to_usize(self) -> Result<usize, Error>;
}
impl Width for i32 {
    fn to_usize(self) -> Result<usize, Error> {
        usize::try_from(self).map_err(|_| Error::new("invalid guest memory range"))
    }
}
impl Width for i64 {
    fn to_usize(self) -> Result<usize, Error> {
        usize::try_from(self).map_err(|_| Error::new("invalid guest memory range"))
    }
}

fn range<W: Width>(pointer: W, length: W) -> Result<Range<usize>, Error> {
    let start = pointer.to_usize()?;
    let length = length.to_usize()?;
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

/// How wide a module's pointers are, as its own memory declares them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bits {
    Thirty32,
    Sixty64,
}

/// Read the width from the module's exported `memory`. Deterministic: one module always gives the same answer, and a
/// module without that export is refused, as it always was.
fn memory_width(module: &Module) -> Result<Bits, Error> {
    module
        .exports()
        .find(|export| export.name() == "memory")
        .and_then(|export| export.ty().memory().map(|memory| memory.is_64()))
        .map(|wide| if wide { Bits::Sixty64 } else { Bits::Thirty32 })
        .ok_or_else(|| Error::new("a plugin must export its memory"))
}

// The rules of the four imports that carry a pointer. Each takes the guest range already converted to one `usize`, so
// the checks below are the same whatever the width and exist exactly once.
fn do_receive<T: Transport>(
    mut caller: Caller<'_, State<'_, T>>,
    range: Result<Range<usize>, Error>,
) -> Result<i32, Error> {
    let memory = memory(&mut caller)?;
    let range = range?;
    if T::MEASURE_TURNS {
        let used = TURN_FUEL.saturating_sub(caller.get_fuel()?);
        let bytes = memory.data_size(&caller);
        caller.data_mut().transport.turn_usage(used, bytes);
    }
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
}

fn do_send<T: Transport>(
    mut caller: Caller<'_, State<'_, T>>,
    range: Result<Range<usize>, Error>,
) -> Result<i32, Error> {
    let memory = memory(&mut caller)?;
    let range = range?;
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
}

fn do_data_read<T: Transport>(
    mut caller: Caller<'_, State<'_, T>>,
    handle: i32,
    range: Result<Range<usize>, Error>,
) -> Result<i32, Error> {
    let memory = memory(&mut caller)?;
    let range = range?;
    let (bytes, state) = memory.data_and_store_mut(&mut caller);
    let into = bytes
        .get_mut(range)
        .ok_or_else(|| Error::new("invalid guest memory range"))?;
    let stream = state
        .streams
        .get_mut(&handle)
        .ok_or_else(|| Error::new("invalid data handle"))?;
    // A connection that broke (the other side went away) is said to the guest, -1, never a fault that ends it.
    let amount = match stream.read(into) {
        Ok(amount) => amount,
        Err(error) => {
            peersus_diagnostics::dev_trace!(stage = "guest-read", handle, outcome = "io-error", kind = ?error.kind(), "Guest data read failed");
            return Ok(-1);
        }
    };
    peersus_diagnostics::dev_trace!(
        stage = "guest-read",
        handle,
        bytes = amount,
        outcome = "ok",
        "Guest data read completed"
    );
    progress(&mut caller, amount)
}

fn do_data_write<T: Transport>(
    mut caller: Caller<'_, State<'_, T>>,
    handle: i32,
    range: Result<Range<usize>, Error>,
) -> Result<i32, Error> {
    let memory = memory(&mut caller)?;
    let range = range?;
    let (bytes, state) = memory.data_and_store_mut(&mut caller);
    let from = bytes
        .get(range)
        .ok_or_else(|| Error::new("invalid guest memory range"))?;
    let stream = state
        .streams
        .get_mut(&handle)
        .ok_or_else(|| Error::new("invalid data handle"))?;
    let amount = match stream.write(from) {
        Ok(amount) => amount,
        Err(error) => {
            peersus_diagnostics::dev_trace!(stage = "guest-write", handle, outcome = "io-error", kind = ?error.kind(), "Guest data write failed");
            return Ok(-1);
        }
    };
    peersus_diagnostics::dev_trace!(
        stage = "guest-write",
        handle,
        bytes = amount,
        outcome = "ok",
        "Guest data write completed"
    );
    progress(&mut caller, amount)
}

fn do_poll<T: Transport, W: Width>(
    mut caller: Caller<'_, State<'_, T>>,
    pointer: W,
    count: W,
    timeout_ms: i32,
) -> Result<i32, Error> {
    let count = count.to_usize()?;
    if count > MAX_OPEN_STREAMS || !(-1..=30_000).contains(&timeout_ms) {
        return Err(Error::new("invalid readiness request"));
    }
    let start = pointer.to_usize()?;
    let end = start
        .checked_add(count * 4)
        .ok_or_else(|| Error::new("invalid guest memory range"))?;
    let memory = memory(&mut caller)?;
    let bytes = memory
        .data(&caller)
        .get(start..end)
        .ok_or_else(|| Error::new("invalid guest memory range"))?;
    let mut handles = Vec::with_capacity(count);
    for bytes in bytes.chunks_exact(4) {
        let handle = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if handle <= 0 || handles.contains(&handle) {
            return Err(Error::new("invalid readiness handle"));
        }
        handles.push(handle);
    }
    let state = caller.data_mut();
    let streams = handles
        .iter()
        .map(|handle| {
            state
                .streams
                .get(handle)
                .map(|stream| (*handle, stream))
                .ok_or_else(|| Error::new("invalid data handle"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Readiness is not progress: polling must never renew a guest's compute fuel.
    match state.transport.poll(&streams, timeout_ms) {
        Ok(Some(handle)) if handle == 0 || handles.contains(&handle) => Ok(handle),
        Ok(None) => Ok(-1),
        _ => Ok(-2),
    }
}

fn do_canvas_present<T: Transport, W: Width>(
    mut caller: Caller<'_, State<'_, T>>,
    pointer: W,
    length: W,
) -> Result<i32, Error> {
    use peersus_api::canvas::{
        PRESENT_INVALID, PRESENT_LIMITED, PRESENT_OK, PRESENT_STREAM_FAILED,
        display::MAX_DISPLAY_LIST_BYTES,
    };

    let (Ok(start), Ok(length)) = (pointer.to_usize(), length.to_usize()) else {
        return Ok(PRESENT_INVALID);
    };
    if length > MAX_DISPLAY_LIST_BYTES {
        return Ok(PRESENT_LIMITED);
    }
    let Some(end) = start.checked_add(length) else {
        return Ok(PRESENT_INVALID);
    };
    let memory = memory(&mut caller)?;
    let (bytes, state) = memory.data_and_store_mut(&mut caller);
    let Some(list) = bytes.get(start..end) else {
        return Ok(PRESENT_INVALID);
    };
    // The render and write are independently bounded; presenting never renews guest fuel.
    let result = state
        .canvases
        .as_mut()
        .map_or(PRESENT_INVALID, |canvases| canvases.present(list));
    peersus_diagnostics::dev_trace!(
        stage = "canvas-present",
        outcome = match result {
            PRESENT_OK => "ok",
            PRESENT_LIMITED => "limited",
            PRESENT_STREAM_FAILED => "stream-failed",
            _ => "invalid",
        },
        display_list_bytes = length,
        "Canvas presentation completed"
    );
    Ok(result)
}

/// What one decode costs a guest in fuel, whatever the picture: a guest that decodes in a loop spends its turn as any loop does.
const DECODE_FUEL: u64 = 2_000_000;

fn do_picture_decode<T: Transport, W: Width>(
    mut caller: Caller<'_, State<'_, T>>,
    (input, input_len): (W, W),
    max_edge: i32,
    (output, output_len): (W, W),
) -> Result<i64, Error> {
    use peersus_api::pictures::{
        DECODE_INVALID, DECODE_SHORT_OUTPUT, DECODE_TOO_LARGE, DECODE_UNSUPPORTED, MAX_DECODE_INPUT,
    };
    let fuel = caller.get_fuel()?;
    caller.set_fuel(fuel.saturating_sub(DECODE_FUEL))?;
    let (Ok(input), Ok(input_len), Ok(output), Ok(output_len), Ok(max_edge)) = (
        input.to_usize(),
        input_len.to_usize(),
        output.to_usize(),
        output_len.to_usize(),
        u32::try_from(max_edge),
    ) else {
        return Ok(DECODE_INVALID);
    };
    if input_len > MAX_DECODE_INPUT {
        return Ok(DECODE_TOO_LARGE);
    }
    let memory = memory(&mut caller)?;
    let (bytes, state) = memory.data_and_store_mut(&mut caller);
    let Some(canvases) = state.canvases.as_mut() else {
        return Ok(DECODE_UNSUPPORTED);
    };
    let Some(encoded) = input
        .checked_add(input_len)
        .and_then(|end| bytes.get(input..end))
    else {
        return Err(Error::new("invalid guest memory range"));
    };
    let (width, height, rgba) = match canvases.decode_picture(encoded, max_edge) {
        Ok(decoded) => decoded,
        Err(code) => return Ok(code),
    };
    if rgba.len() > output_len {
        return Ok(DECODE_SHORT_OUTPUT);
    }
    let Some(into) = output
        .checked_add(rgba.len())
        .and_then(|end| bytes.get_mut(output..end))
    else {
        return Err(Error::new("invalid guest memory range"));
    };
    into.copy_from_slice(&rgba);
    Ok(i64::from(width) << 16 | i64::from(height))
}

/// What starting one sound costs a guest in fuel, and what each read of its samples costs.
const AUDIO_OPEN_FUEL: u64 = 2_000_000;
const AUDIO_READ_FUEL: u64 = 500_000;

fn charge<T: Transport>(caller: &mut Caller<'_, State<'_, T>>, fuel: u64) -> Result<(), Error> {
    let left = caller.get_fuel()?;
    caller.set_fuel(left.saturating_sub(fuel))?;
    Ok(())
}

fn do_audio_open<T: Transport, W: Width>(
    mut caller: Caller<'_, State<'_, T>>,
    (input, input_len): (W, W),
) -> Result<i64, Error> {
    use peersus_api::audio::{
        DECODE_INVALID, DECODE_TOO_LARGE, DECODE_UNSUPPORTED, MAX_DECODE_INPUT,
    };
    charge(&mut caller, AUDIO_OPEN_FUEL)?;
    let (Ok(input), Ok(input_len)) = (input.to_usize(), input_len.to_usize()) else {
        return Ok(DECODE_INVALID);
    };
    if input_len > MAX_DECODE_INPUT {
        return Ok(DECODE_TOO_LARGE);
    }
    let memory = memory(&mut caller)?;
    let (bytes, state) = memory.data_and_store_mut(&mut caller);
    let Some(canvases) = state.canvases.as_mut() else {
        return Ok(DECODE_UNSUPPORTED);
    };
    let Some(encoded) = input
        .checked_add(input_len)
        .and_then(|end| bytes.get(input..end))
    else {
        return Err(Error::new("invalid guest memory range"));
    };
    Ok(canvases.audio_open(encoded))
}

fn do_audio_read<T: Transport, W: Width>(
    mut caller: Caller<'_, State<'_, T>>,
    handle: i64,
    (output, output_len): (W, W),
) -> Result<i64, Error> {
    use peersus_api::audio::{DECODE_INVALID, DECODE_UNSUPPORTED};
    charge(&mut caller, AUDIO_READ_FUEL)?;
    let (Ok(output), Ok(output_len)) = (output.to_usize(), output_len.to_usize()) else {
        return Ok(DECODE_INVALID);
    };
    // One read never decodes more than one chunk the application takes, whatever room the guest offers.
    let output_len = output_len.min(peersus_api::audio::MAX_CHUNK_BYTES);
    let memory = memory(&mut caller)?;
    let (bytes, state) = memory.data_and_store_mut(&mut caller);
    let Some(canvases) = state.canvases.as_mut() else {
        return Ok(DECODE_UNSUPPORTED);
    };
    let Some(into) = output
        .checked_add(output_len)
        .and_then(|end| bytes.get_mut(output..end))
    else {
        return Err(Error::new("invalid guest memory range"));
    };
    Ok(canvases.audio_read(handle, into))
}

fn do_audio_handle<T: Transport>(
    mut caller: Caller<'_, State<'_, T>>,
    handle: i64,
    close: bool,
) -> Result<i64, Error> {
    let Some(canvases) = caller.data_mut().canvases.as_mut() else {
        return Ok(peersus_api::audio::DECODE_UNSUPPORTED);
    };
    Ok(if close {
        canvases.audio_close(handle)
    } else {
        canvases.audio_length(handle)
    })
}

/// One registration function per width. Only the parameter type differs; every body is one of the functions above.
macro_rules! register_width {
    ($name:ident, $int:ty) => {
        fn $name<T: Transport>(linker: &mut Linker<State<'_, T>>) -> Result<(), Error> {
            linker.func_wrap(
                "peersus_v1",
                "receive",
                |caller: Caller<'_, State<'_, T>>, pointer: $int, length: $int| {
                    do_receive(caller, range(pointer, length))
                },
            )?;
            linker.func_wrap(
                "peersus_v1",
                "send",
                |caller: Caller<'_, State<'_, T>>, pointer: $int, length: $int| {
                    do_send(caller, range(pointer, length))
                },
            )?;
            linker.func_wrap(
                "peersus_v1",
                "data_read",
                |caller: Caller<'_, State<'_, T>>, handle: i32, pointer: $int, length: $int| {
                    do_data_read(caller, handle, range(pointer, length))
                },
            )?;
            linker.func_wrap(
                "peersus_v1",
                "data_write",
                |caller: Caller<'_, State<'_, T>>, handle: i32, pointer: $int, length: $int| {
                    do_data_write(caller, handle, range(pointer, length))
                },
            )?;
            linker.func_wrap(
                "peersus_v1",
                "poll",
                |caller: Caller<'_, State<'_, T>>, pointer: $int, count: $int, timeout_ms: i32| {
                    do_poll(caller, pointer, count, timeout_ms)
                },
            )?;
            linker.func_wrap(
                "peersus_v1",
                "canvas_present",
                |caller: Caller<'_, State<'_, T>>, pointer: $int, length: $int| {
                    do_canvas_present(caller, pointer, length)
                },
            )?;
            linker.func_wrap(
                "peersus_v1",
                peersus_api::pictures::DECODE_FUNCTION,
                |caller: Caller<'_, State<'_, T>>,
                 input: $int,
                 input_len: $int,
                 max_edge: i32,
                 output: $int,
                 output_len: $int| {
                    do_picture_decode(caller, (input, input_len), max_edge, (output, output_len))
                },
            )?;
            linker.func_wrap(
                "peersus_v1",
                peersus_api::audio::OPEN_FUNCTION,
                |caller: Caller<'_, State<'_, T>>, input: $int, input_len: $int| {
                    do_audio_open(caller, (input, input_len))
                },
            )?;
            linker.func_wrap(
                "peersus_v1",
                peersus_api::audio::READ_FUNCTION,
                |caller: Caller<'_, State<'_, T>>, handle: i64, output: $int, output_len: $int| {
                    do_audio_read(caller, handle, (output, output_len))
                },
            )?;
            linker.func_wrap(
                "peersus_v1",
                peersus_api::audio::LENGTH_FUNCTION,
                |caller: Caller<'_, State<'_, T>>, handle: i64| {
                    do_audio_handle(caller, handle, false)
                },
            )?;
            linker.func_wrap(
                "peersus_v1",
                peersus_api::audio::CLOSE_FUNCTION,
                |caller: Caller<'_, State<'_, T>>, handle: i64| {
                    do_audio_handle(caller, handle, true)
                },
            )?;
            Ok(())
        }
    };
}
register_width!(register_32, i32);
register_width!(register_64, i64);

/// Execute one module in its already isolated worker, returning when it leaves or traps.
/// Callers must never execute an extension on an owner, UI, or Android main thread.
pub fn run<T: Transport>(bytes: &[u8], transport: &mut T) -> Result<(), Error> {
    run_worker(bytes, transport, None)
}

pub fn run_with_canvas<'a, T: Transport>(
    bytes: &[u8],
    transport: &'a mut T,
    canvases: &'a mut dyn CanvasStreams<T::Stream>,
) -> Result<(), Error> {
    run_worker(bytes, transport, Some(canvases))
}

fn run_worker<'a, T: Transport>(
    bytes: &[u8],
    transport: &'a mut T,
    canvases: Option<&'a mut dyn CanvasStreams<T::Stream>>,
) -> Result<(), Error> {
    let started = std::time::Instant::now();
    peersus_diagnostics::dev_trace!(
        stage = "worker-init",
        "Plugin worker entered the interpreter"
    );
    let result = run_inner(bytes, transport, canvases);
    // Only the trap's category ever reaches diagnostics; a trap or host error
    // message may carry whatever the guest or the engine put in it.
    let trap = result.as_ref().err().and_then(Error::as_trap_code);
    let outcome = match (&result, trap.is_some()) {
        (Ok(()), _) => "ok",
        (Err(_), true) => "trap",
        (Err(_), false) => "error",
    };
    peersus_diagnostics::dev_trace!(
        stage = "worker-end",
        outcome,
        trap = ?trap,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "Plugin worker left the interpreter"
    );
    result
}

fn run_inner<'a, T: Transport>(
    bytes: &[u8],
    transport: &'a mut T,
    canvases: Option<&'a mut dyn CanvasStreams<T::Stream>>,
) -> Result<(), Error> {
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
    peersus_diagnostics::dev_trace!(stage = "validate", "Plugin module validation started");
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
            canvases,
            next_handle: 1,
            memory: None,
            diagnostic: diagnostic::Budget::default(),
        },
    );
    store.limiter(|state| &mut state.limits);
    store.set_fuel(TURN_FUEL)?;
    let mut linker = Linker::new(&engine);
    // The module's own memory says how wide its pointers are, and nothing it or its manifest declares can say otherwise:
    // the engine validated that type. The protocol is `peersus_v1` either way; only what a pointer is differs.
    match memory_width(&module)? {
        Bits::Thirty32 => register_32(&mut linker)?,
        Bits::Sixty64 => register_64(&mut linker)?,
    }
    // These take no pointer or length, so they are one function whatever the width.
    {
        let module_name = "peersus_v1";
        linker.func_wrap(
            module_name,
            "diagnostic",
            |mut caller: Caller<'_, State<'_, T>>,
             stage: i32,
             outcome: i32,
             correlation: i64,
             actual: i64,
             expected: i64|
             -> i32 {
                // Scalar-only for both memory widths. This never renews fuel or
                // touches control, data, guest memory or platform authority.
                caller
                    .data_mut()
                    .diagnostic
                    .emit(stage, outcome, correlation, actual, expected)
            },
        )?;
        linker.func_wrap(
            module_name,
            "data_open",
            |mut caller: Caller<'_, State<'_, T>>, kind: i32, number: i64| -> Result<i32, Error> {
                let request = match kind {
                    0 => DataRequest::Ticket(number as u64),
                    1 => DataRequest::Claim(number as u64),
                    peersus_api::canvas::CANVAS_DATA_KIND if number != 0 => {
                        DataRequest::CanvasClaim(number as u64)
                    }
                    peersus_api::canvas::CANVAS_DATA_KIND => return Ok(-1),
                    _ => return Err(Error::new("invalid data request")),
                };
                let request_kind = match request {
                    DataRequest::Ticket(_) => "ticket",
                    DataRequest::Claim(_) => "claim",
                    DataRequest::CanvasClaim(_) => "canvas",
                };
                // A claim is a bearer right, not a diagnostic correlation ID.
                let diagnostic_number = if request_kind == "ticket" { number } else { 0 };
                let state = caller.data_mut();
                // A connection that cannot be had (the other device refused it, went away, or too many are open) is an
                // answer the guest is given, -1, and never a fault that ends it: a guest that controls another device
                // must outlive that device saying no.
                let open = state.stream_count();
                if matches!(request, DataRequest::CanvasClaim(_))
                    && state.canvases.as_ref().is_none_or(|canvas| {
                        canvas.count() >= peersus_api::canvas::MAX_CANVASES_PER_EXTENSION
                    })
                {
                    return Ok(-1);
                }
                if open >= MAX_OPEN_STREAMS {
                    peersus_diagnostics::dev_trace!(
                        kind = request_kind,
                        number = diagnostic_number,
                        stage = "stream-open",
                        outcome = "full",
                        "Guest data stream refused"
                    );
                    return Ok(-1);
                }
                let handle = state.next_handle;
                state.next_handle = handle
                    .checked_add(1)
                    .ok_or_else(|| Error::new("data handles exhausted"))?;
                let Ok(stream) = state.transport.open_data(request) else {
                    peersus_diagnostics::dev_trace!(
                        kind = request_kind,
                        number = diagnostic_number,
                        stage = "stream-open",
                        outcome = "refused",
                        "Guest data stream refused"
                    );
                    return Ok(-1);
                };
                match request {
                    DataRequest::CanvasClaim(_) => {
                        let Some(canvases) = state.canvases.as_mut() else {
                            return Ok(-1);
                        };
                        canvases.open(handle, stream);
                    }
                    _ => {
                        state.streams.insert(handle, stream);
                    }
                }
                peersus_diagnostics::dev_trace!(
                    kind = request_kind,
                    number = diagnostic_number,
                    handle,
                    open = open + 1,
                    stage = "stream-open",
                    outcome = "ok",
                    "Guest data stream opened"
                );
                Ok(handle)
            },
        )?;
        linker.func_wrap(
            module_name,
            "data_shutdown",
            |mut caller: Caller<'_, State<'_, T>>, handle: i32| -> Result<i32, Error> {
                let state = caller.data_mut();
                let stream = state
                    .streams
                    .get_mut(&handle)
                    .ok_or_else(|| Error::new("invalid data handle"))?;
                if state.transport.shutdown_data(stream).is_err() {
                    peersus_diagnostics::dev_trace!(
                        handle,
                        stage = "stream-shutdown",
                        outcome = "failed",
                        "Guest data stream shutdown failed"
                    );
                    return Ok(-1);
                }
                peersus_diagnostics::dev_trace!(
                    handle,
                    stage = "stream-shutdown",
                    outcome = "ok",
                    "Guest data stream finished writing"
                );
                Ok(0)
            },
        )?;
        linker.func_wrap(
            module_name,
            "data_close",
            |mut caller: Caller<'_, State<'_, T>>, handle: i32| -> Result<i32, Error> {
                if !caller.data_mut().close_stream(handle) {
                    peersus_diagnostics::dev_trace!(
                        handle,
                        stage = "stream-close",
                        outcome = "invalid",
                        "Guest data stream close refused"
                    );
                    return Err(Error::new("invalid data handle"));
                }
                peersus_diagnostics::dev_trace!(
                    handle,
                    stage = "stream-close",
                    outcome = "ok",
                    "Guest data stream closed"
                );
                Ok(0)
            },
        )?;
    }
    peersus_diagnostics::dev_trace!(stage = "instantiate", "Plugin module instantiation started");
    let instance = linker.instantiate_and_start(&mut store, &module)?;
    let memory = instance
        .get_memory(&store, "memory")
        .ok_or_else(|| Error::new("a plugin must export its memory"))?;
    store.data_mut().memory = Some(memory);
    let entry = instance.get_typed_func::<(), i32>(&store, "peersus_main")?;
    peersus_diagnostics::dev_trace!(stage = "call", "Plugin entry call started");
    match entry.call(&mut store, ())? {
        0 => Ok(()),
        _ => Err(Error::new("plugin entry returned an error")),
    }
}

#[cfg(test)]
mod tests;
