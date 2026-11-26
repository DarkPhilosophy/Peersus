//! The bridge to the camera.
//!
//! Like the picker's, it decides nothing. What becomes of the frames is `peersus_ui::scanner::Scanner`, tried without a
//! phone; this passes each frame in, and starts and stops the camera in Java.
//!
//! WHAT CROSSES. A frame is the brightness plane of what the camera sees, one byte a pixel, copied by Java without the
//! padding at the end of each row, so that `width * height` bytes is exactly what arrives. It comes thirty or more times
//! a second on the camera's own thread, so nothing here waits: the frame is handed to the scanner, which looks at one
//! in a few and passes on the rest at once.
//!
//! NO PANIC CROSSES. As in the picker's bridge, every native runs inside `catch_unwind`, since a panic that unwinds into
//! the virtual machine ends the process without a word.
#![allow(unsafe_code)]

use crate::bridge::call_activity;
use jni::{
    JNIEnv,
    objects::{JByteArray, JClass, JObject},
    sys::jint,
};
use peersus_ui::scanner::{Colour, Scanner};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Mutex, OnceLock},
    time::Instant,
};

/// The scan in progress, if any. One for the process, because there is one camera, and because a native method of Java
/// has no other way to find it. Replaced for each scan, so that a scan that was left cannot be joined to a new one.
static SCAN: OnceLock<Mutex<Option<Scanner>>> = OnceLock::new();

/// Why the camera is not giving frames, when it was asked to and cannot: said once, for the interface to read.
static STOPPED: OnceLock<Mutex<bool>> = OnceLock::new();

fn scan() -> &'static Mutex<Option<Scanner>> {
    SCAN.get_or_init(|| Mutex::new(None))
}

fn stopped() -> &'static Mutex<bool> {
    STOPPED.get_or_init(|| Mutex::new(false))
}

/// Open the camera for `scanner`, which gets its frames until it is over. Returns at once.
///
/// The scan before this one, if any, is stopped first: its frames must not reach the new one, and what it would have
/// found must not be joined late.
pub fn open_camera(scanner: Scanner) -> Result<(), String> {
    if let Ok(mut current) = scan().lock()
        && let Some(previous) = current.replace(scanner)
    {
        previous.stop();
    }
    if let Ok(mut flag) = stopped().lock() {
        *flag = false;
    }
    call_activity("openCamera", "()V", &[])
}

/// Close the camera and end the scan: the person left, or the code was read. Safe when nothing is open.
pub fn close_camera() {
    if let Ok(mut current) = scan().lock()
        && let Some(previous) = current.take()
    {
        previous.stop();
    }
    let _ = call_activity("closeCamera", "()V", &[]);
}

/// Whether the camera said it could not be used (the permission was refused), read once.
pub fn take_refusal() -> bool {
    stopped()
        .lock()
        .map(|mut flag| std::mem::take(&mut *flag))
        .unwrap_or(false)
}

/// How many quarter turns clockwise a frame must be turned to stand upright, as Java last said. Java knows the sensor's
/// mounting and the screen's turn and says the result each time the camera starts and each time the screen is turned;
/// this only holds it, so that asking never calls into Java (a call from the watcher's thread could wait on the
/// interface's). The camera before it is first said is as the usual back camera is: upright with one turn.
static TURNS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// The quarter turns Java last reported.
pub fn turns() -> u32 {
    TURNS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Java says how many quarter turns a frame needs to stand upright: see [`turns`]. Any number is held modulo four.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_CodeCamera_cameraTurns<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    turns: jint,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        TURNS.store(
            turns.rem_euclid(4) as u32,
            std::sync::atomic::Ordering::Relaxed,
        );
    }));
}

/// One frame of the camera: its brightness plane, `width` bytes to a row, and the colour of its centre square: `u` and `v`, each
/// `chroma_side` bytes to a row and as many rows (half the side of the square). A `chroma_side` of nothing, or planes that are
/// not as long as they say, is a frame without colour, which is shown grey and read all the same.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_CodeCamera_cameraFrame<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    luma: JByteArray<'local>,
    u: JByteArray<'local>,
    v: JByteArray<'local>,
    chroma_side: jint,
    width: jint,
    height: jint,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let (Ok(width), Ok(height)) = (usize::try_from(width), usize::try_from(height)) else {
            return;
        };
        // Taken out of the lock before the frame is read, which costs: the interface asks the scanner questions too.
        let Some(scanner) = scan().lock().ok().and_then(|current| current.clone()) else {
            return;
        };
        // Cheap to refuse before copying anything: a scan that is over takes no frames.
        if scanner.is_over() {
            return;
        }
        let Ok(bytes) = env.convert_byte_array(&luma) else {
            return;
        };
        let colour = usize::try_from(chroma_side)
            .ok()
            .filter(|side| *side > 0)
            .and_then(|side| {
                Some(Colour {
                    side,
                    u: env.convert_byte_array(&u).ok()?,
                    v: env.convert_byte_array(&v).ok()?,
                })
            });
        scanner.frame_coloured(&bytes, width, height, colour, Instant::now());
    }));
}

/// The camera could not be used: the permission was refused.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_cameraStopped<'local>(
    _env: JNIEnv<'local>,
    _activity: JObject<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Ok(mut flag) = stopped().lock() {
            *flag = true;
        }
    }));
}
