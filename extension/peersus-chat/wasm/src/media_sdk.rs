//! Optional guest-side bindings for media decoded in the extension's own WASM helper.
//! Copy this file as `media_sdk.rs`; `check-guest-glue.py` checks it against the SDK.

#[cfg(target_family = "wasm")]
mod import {
    #[link(wasm_import_module = "peersus_v1")]
    unsafe extern "C" {
        #[link_name = "picture_decode"]
        pub fn picture_decode(
            input: *const u8,
            input_len: usize,
            max_edge: i32,
            output: *mut u8,
            output_len: usize,
        ) -> i64;
        #[link_name = "audio_open"]
        pub fn audio_open(input: *const u8, input_len: usize) -> i64;
        #[link_name = "audio_read"]
        pub fn audio_read(handle: i64, output: *mut u8, output_len: usize) -> i64;
        #[link_name = "audio_length"]
        pub fn audio_length(handle: i64) -> i64;
        #[link_name = "audio_close"]
        pub fn audio_close(handle: i64) -> i64;
    }
}

/// Decode a bounded picture to unpremultiplied RGBA, preserving its aspect ratio.
#[cfg(target_family = "wasm")]
pub fn decode_picture(encoded: &[u8], max_edge: u32) -> Option<(u32, u32, Vec<u8>)> {
    use peersus_api::pictures::{MAX_DECODE_EDGE, MAX_DECODE_INPUT};

    let edge = max_edge.min(MAX_DECODE_EDGE);
    if edge == 0 || encoded.len() > MAX_DECODE_INPUT {
        return None;
    }
    let mut output = vec![0; (edge * edge * 4) as usize];
    // SAFETY: both slices stay alive during the call; the helper checks their lengths and bounds.
    let answer = unsafe {
        import::picture_decode(
            encoded.as_ptr(),
            encoded.len(),
            edge as i32,
            output.as_mut_ptr(),
            output.len(),
        )
    };
    let packed = u32::try_from(answer).ok()?;
    let (width, height) = (packed >> 16, packed & 0xffff);
    if width == 0 || height == 0 || width > edge || height > edge {
        return None;
    }
    output.truncate((width * height * 4) as usize);
    Some((width, height, output))
}

/// The native host table does not offer picture decoding.
#[cfg(not(target_family = "wasm"))]
pub fn decode_picture(_encoded: &[u8], _max_edge: u32) -> Option<(u32, u32, Vec<u8>)> {
    None
}

/// Open a sound decoder. The helper owns a copy of the encoded input after this call.
#[cfg(target_family = "wasm")]
pub fn open_sound(encoded: &[u8]) -> Option<i64> {
    if encoded.len() > peersus_api::audio::MAX_DECODE_INPUT {
        return None;
    }
    // SAFETY: the input slice is readable throughout the call.
    let handle = unsafe { import::audio_open(encoded.as_ptr(), encoded.len()) };
    (handle > 0).then_some(handle)
}

/// Read complete frames into the caller's buffer, or return zero at the end of the sound.
#[cfg(target_family = "wasm")]
pub fn read_sound(handle: i64, into: &mut [u8]) -> Option<usize> {
    // SAFETY: the output slice is exclusively writable throughout the call.
    let count =
        usize::try_from(unsafe { import::audio_read(handle, into.as_mut_ptr(), into.len()) })
            .ok()?;
    (count <= into.len() && count.is_multiple_of(peersus_api::audio::FRAME_BYTES)).then_some(count)
}

/// Return the total number of frames if the encoded file declares it.
#[cfg(target_family = "wasm")]
pub fn sound_length(handle: i64) -> Option<u64> {
    // SAFETY: the helper validates the numeric handle; no pointers cross this boundary.
    u64::try_from(unsafe { import::audio_length(handle) }).ok()
}

#[cfg(target_family = "wasm")]
pub fn close_sound(handle: i64) {
    // SAFETY: the helper validates the numeric handle; no pointers cross this boundary.
    let _ = unsafe { import::audio_close(handle) };
}

// There is no audio decoder in the native host table. The guest can still send and save the file.
#[cfg(not(target_family = "wasm"))]
pub fn open_sound(_encoded: &[u8]) -> Option<i64> {
    None
}
#[cfg(not(target_family = "wasm"))]
pub fn read_sound(_handle: i64, _into: &mut [u8]) -> Option<usize> {
    None
}
#[cfg(not(target_family = "wasm"))]
pub fn sound_length(_handle: i64) -> Option<u64> {
    None
}
#[cfg(not(target_family = "wasm"))]
pub fn close_sound(_handle: i64) {}
