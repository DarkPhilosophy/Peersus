//! Sound an extension plays: decoded in the extension's own process, played by the application as finished samples.
//!
//! WHO DOES WHAT. The extension turns an encoded sound (a voice message, a song) into samples with the decoder its own helper
//! process offers it ([`DECODE`], the guest functions [`OPEN_FUNCTION`], [`READ_FUNCTION`], [`LENGTH_FUNCTION`] and
//! [`CLOSE_FUNCTION`]); the application never decodes a sound for an extension. The extension then hands the samples over in
//! chunks, each read from its own private storage: [`PLAY`] starts a sound with its first chunk and [`QUEUE`] adds the next ones
//! while it plays. The application checks only the size of each chunk and sends the samples to the device as they are.
//!
//! THE SAMPLES. Always [`RATE`] frames a second of [`CHANNELS`] interleaved signed 16-bit little-endian samples, the format the
//! decoder gives: the application neither resamples nor mixes them.
//!
//! WHAT BOUNDS IT. One sound plays at a time in the whole application: a `play` replaces what any extension plays, and the one
//! that played sees it stopped in its [`STATE`]. A chunk is at most [`MAX_CHUNK_BYTES`]; an extension holds at most
//! [`MAX_QUEUED_BYTES`] not yet played, and a `queue` past it is refused and changes nothing. What an extension plays stops when
//! it stops, restarts, is turned off or is removed.
//!
//! RECORDING. With the person's permission ([`RECORD_CAPABILITY`]) an extension records from the device's microphone:
//! [`RECORD`] starts it, [`FINISH`] stops it and hands the sound over as a sealed stage of the extension's own storage (a WAV of
//! [`RECORD_RATE`] frames a second, one signed 16-bit channel), which the extension commits or aborts like any stage; [`CANCEL`]
//! stops it and keeps nothing. One recording at a time in the whole application, at most [`MAX_RECORD_SECONDS`] long: past that
//! the microphone stops by itself and `finish` hands over what was heard. A recording ends, unkept, when its extension stops.
use crate::{Error, Result, elements::layer_id, extension_data::Source};
use serde::{Deserialize, Serialize};

pub const SERVICE: &str = "peersus.audio";
/// Stop whatever plays and start a sound with its first chunk.
pub const PLAY: &str = "play";
/// Add the next chunk of the sound this extension plays.
pub const QUEUE: &str = "queue";
/// Pause the sound this extension plays, keeping what is queued.
pub const PAUSE: &str = "pause";
/// Go on playing a paused sound.
pub const RESUME: &str = "resume";
/// Stop the sound this extension plays and let go of what is queued.
pub const STOP: &str = "stop";
/// What this extension plays now ([`Playback`]).
pub const STATE: &str = "state";
/// Declares that the extension decodes sounds in its own process. It is never answered through the hub: a WASM guest that
/// imports it runs in the helper that links the decoder, and calls the guest functions below.
pub const DECODE: &str = "decode";
/// How loud the caller's recording is now ([`Levels`]).
pub const LEVELS: &str = "levels";
/// Start recording from the microphone ([`Record`]).
pub const RECORD: &str = "record";
/// Stop recording and hand the sound over ([`Recorded`]).
pub const FINISH: &str = "finish";
/// Stop recording and keep nothing.
pub const CANCEL: &str = "cancel";
/// What guards [`RECORD`], [`FINISH`] and [`CANCEL`].
pub const RECORD_CAPABILITY: &str = "audio.record";
/// Frames a second of a recording.
pub const RECORD_RATE: u32 = 16_000;
/// Longest recording.
pub const MAX_RECORD_SECONDS: u32 = 120;
/// Bytes of the WAV header before a recording's samples.
pub const WAV_HEADER_BYTES: usize = 44;

/// Frames a second of every sample the application plays.
pub const RATE: u32 = 48_000;
/// Interleaved channels of every frame.
pub const CHANNELS: u16 = 2;
/// Bytes of one frame: [`CHANNELS`] signed 16-bit samples.
pub const FRAME_BYTES: usize = CHANNELS as usize * 2;
/// Most bytes of one chunk: eight seconds.
pub const MAX_CHUNK_BYTES: usize = 8 * RATE as usize * FRAME_BYTES;
/// Most bytes an extension has queued and not yet played: thirty seconds.
pub const MAX_QUEUED_BYTES: usize = 30 * RATE as usize * FRAME_BYTES;

/// `audio_open(input, input_len) -> i64`, in the `peersus_v1` module: start decoding an encoded WAV, MP3, FLAC, Ogg Vorbis or
/// AAC in MP4 of at most [`MAX_DECODE_INPUT`] bytes. The helper copies the input, so the guest may reuse its memory. The answer
/// is a positive handle, or a negative `DECODE_*` code. At most [`MAX_DECODERS`] are open at once.
pub const OPEN_FUNCTION: &str = "audio_open";
/// `audio_read(handle, output, output_len) -> i64`: the next samples, in the format above, as many whole frames as fit in
/// `output`. The answer is the number of bytes written, `0` once the sound has ended, or a negative `DECODE_*` code.
pub const READ_FUNCTION: &str = "audio_read";
/// `audio_length(handle) -> i64`: how many frames the whole sound is at [`RATE`], when the file says so, or
/// [`DECODE_UNKNOWN_LENGTH`].
pub const LENGTH_FUNCTION: &str = "audio_length";
/// `audio_close(handle) -> i64`: let go of a decoder; `0`, or [`DECODE_NO_HANDLE`].
pub const CLOSE_FUNCTION: &str = "audio_close";
/// Longest encoded sound [`OPEN_FUNCTION`] reads, in bytes.
pub const MAX_DECODE_INPUT: usize = 24 * 1024 * 1024;
/// Most decoders one helper keeps open.
pub const MAX_DECODERS: usize = 2;
/// This helper has no decoder: the extension did not import [`DECODE`].
pub const DECODE_UNSUPPORTED: i64 = -1;
/// Not a sound this decoder reads, or a damaged one.
pub const DECODE_INVALID: i64 = -2;
/// The input is past [`MAX_DECODE_INPUT`].
pub const DECODE_TOO_LARGE: i64 = -3;
/// [`MAX_DECODERS`] are open already.
pub const DECODE_BUSY: i64 = -4;
/// No open decoder has this handle.
pub const DECODE_NO_HANDLE: i64 = -5;
/// The file does not say how long the sound is.
pub const DECODE_UNKNOWN_LENGTH: i64 = -6;

/// One chunk of samples in the caller's private storage: a committed object or a sealed stage of exactly `bytes` bytes, a whole
/// number of frames, read once when it is handed over.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    /// The extension's own name for the sound (the grammar of a kept layer's id); a `queue` names the sound it continues.
    pub sound: String,
    pub bytes: u32,
    pub source: Source,
}

impl Chunk {
    /// Read and check the arguments of a `play` or a `queue`.
    pub fn parse(value: &serde_json::Value) -> Result<Self> {
        let chunk = Self::deserialize(value).map_err(|_| Error::InvalidManifest)?;
        let bytes = chunk.bytes as usize;
        if layer_id(&chunk.sound)
            && bytes > 0
            && bytes <= MAX_CHUNK_BYTES
            && bytes.is_multiple_of(FRAME_BYTES)
        {
            Ok(chunk)
        } else {
            Err(Error::InvalidManifest)
        }
    }
}

/// The arguments of `record`: how long it may last at most, in seconds (1 to [`MAX_RECORD_SECONDS`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub max_seconds: u32,
}

impl Record {
    pub fn parse(value: &serde_json::Value) -> Result<Self> {
        let record = Self::deserialize(value).map_err(|_| Error::InvalidManifest)?;
        (1..=MAX_RECORD_SECONDS)
            .contains(&record.max_seconds)
            .then_some(record)
            .ok_or(Error::InvalidManifest)
    }
}

/// The answer to `finish`: the sealed stage holding the recording as a WAV file, how long it is, and how loud it was, one
/// [`Levels`] step at a time, for the waveform a voice message is drawn with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recorded {
    pub stage: u64,
    pub bytes: u64,
    pub sha256: crate::extension_data::ScopeId,
    pub duration_ms: u64,
    /// The loudest moment of each [`LEVEL_MS`] of the whole recording, 0 (silence) to 255, at most [`MAX_LEVELS`].
    pub levels: Vec<u8>,
}

/// How long one level covers.
pub const LEVEL_MS: u64 = 100;
/// Most levels a whole recording has.
pub const MAX_LEVELS: usize = (MAX_RECORD_SECONDS as u64 * 1000 / LEVEL_MS) as usize;
/// Most levels [`LEVELS`] answers: the newest, enough for the moving waveform of a recording under way.
pub const MAX_LIVE_LEVELS: usize = 48;
/// The ranges of frequencies [`LEVELS`] answers, from the lowest to the highest a recording holds.
pub const BANDS: usize = 32;
/// The newest samples of a recording the bands are measured over.
pub const BAND_WINDOW: usize = 1024;

/// The answer to `levels`: how long the caller's recording has run, how loud its newest moments were, oldest first, and how
/// strong each range of frequencies is in what the microphone hears now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Levels {
    pub elapsed_ms: u64,
    /// The loudest moment of each of the newest [`LEVEL_MS`], 0 to 255, at most [`MAX_LIVE_LEVELS`].
    pub levels: Vec<u8>,
    /// How strong each of [`BANDS`] ranges of frequencies is in the newest [`BAND_WINDOW`] samples, lowest first, 0 (nothing)
    /// to 255: an equaliser's bars.
    pub bands: Vec<u8>,
}

/// The level of `peak` (the largest absolute sample of a step): a square root, so that a quiet voice still shows.
pub fn level(peak: u16) -> u8 {
    let share = f32::from(peak.min(i16::MAX as u16)) / f32::from(i16::MAX);
    (share.sqrt() * 255.0).round() as u8
}

/// A WAV file of `samples` at [`RECORD_RATE`], one channel, signed 16-bit little-endian: what `finish` hands over.
pub fn wav(samples: &[i16]) -> Vec<u8> {
    let data = samples.len() * 2;
    let mut out = Vec::with_capacity(WAV_HEADER_BYTES + data);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RECORD_RATE.to_le_bytes());
    out.extend_from_slice(&(RECORD_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data as u32).to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// The answer to `state`: the sound this extension plays, if any.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Playback {
    /// The sound this extension plays or paused; `None` once it stopped, ended or another sound replaced it.
    pub sound: Option<String>,
    pub playing: bool,
    /// How far into the sound the device has played, counted from its first chunk.
    pub position_ms: u64,
    /// How much is queued and not yet played.
    pub queued_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_chunk_names_a_sound_and_whole_frames_within_the_bound() {
        let chunk = Chunk::parse(&json!({
            "sound": "voice/1", "bytes": MAX_CHUNK_BYTES, "source": {"kind": "stage", "stage": 3}
        }))
        .unwrap();
        assert_eq!(chunk.bytes as usize, MAX_CHUNK_BYTES);
        for refused in [
            json!({"sound": "voice/1", "bytes": 0, "source": {"kind": "stage", "stage": 3}}),
            json!({"sound": "voice/1", "bytes": 6, "source": {"kind": "stage", "stage": 3}}),
            json!({"sound": "voice/1", "bytes": MAX_CHUNK_BYTES + FRAME_BYTES, "source": {"kind": "stage", "stage": 3}}),
            json!({"sound": "Voice", "bytes": 4, "source": {"kind": "stage", "stage": 3}}),
            json!({"sound": "v", "bytes": 4, "source": {"kind": "file", "path": "/tmp/a.wav"}}),
            json!({"sound": "v", "bytes": 4, "source": {"kind": "stage", "stage": 3}, "rate": 44100}),
        ] {
            assert!(Chunk::parse(&refused).is_err(), "{refused} was taken");
        }
    }

    #[test]
    fn a_recording_lasts_one_to_the_longest_seconds_and_is_handed_over_as_a_plain_wav() {
        assert_eq!(
            Record::parse(&json!({"max_seconds": 1}))
                .unwrap()
                .max_seconds,
            1
        );
        assert!(Record::parse(&json!({"max_seconds": MAX_RECORD_SECONDS})).is_ok());
        for refused in [
            json!({"max_seconds": 0}),
            json!({"max_seconds": MAX_RECORD_SECONDS + 1}),
            json!({}),
            json!({"max_seconds": 5, "rate": 48000}),
        ] {
            assert!(Record::parse(&refused).is_err(), "{refused} was taken");
        }
        let file = wav(&[1, -2]);
        assert_eq!(file.len(), WAV_HEADER_BYTES + 4);
        assert_eq!(&file[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(file[4..8].try_into().unwrap()), 40);
        assert_eq!(
            u32::from_le_bytes(file[24..28].try_into().unwrap()),
            RECORD_RATE
        );
        assert_eq!(&file[44..], &[1, 0, 0xfe, 0xff]);
    }
}
