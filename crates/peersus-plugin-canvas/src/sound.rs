//! Sounds decoded in this helper for the extension that declared it ([`peersus_api::audio::DECODE`]): the application never
//! decodes a sound for an extension, it only plays the samples the extension hands over.
//!
//! Every decoder answers in the one format the application plays ([`RATE`] frames a second of [`CHANNELS`] signed 16-bit
//! samples): a mono sound is put on both channels, one with more keeps its first two, and another rate is turned into
//! [`RATE`] by linear interpolation between neighbouring frames.
use peersus_api::audio::{
    CHANNELS, DECODE_BUSY, DECODE_INVALID, DECODE_NO_HANDLE, DECODE_TOO_LARGE,
    DECODE_UNKNOWN_LENGTH, FRAME_BYTES, MAX_DECODE_INPUT, MAX_DECODERS, RATE,
};
use std::{collections::BTreeMap, io::Cursor};
use symphonia::core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    errors::Error,
    formats::{FormatOptions, FormatReader, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};

/// Damaged packets in a row after which the sound is taken as unreadable rather than skipped over.
const MAX_BAD_PACKETS: usize = 64;

/// The decoders one helper keeps open, by the handle the guest was given.
#[derive(Default)]
pub(crate) struct Sounds {
    open: BTreeMap<i64, Sound>,
    next: i64,
}

impl Sounds {
    pub(crate) fn open(&mut self, encoded: &[u8]) -> i64 {
        if encoded.len() > MAX_DECODE_INPUT {
            return DECODE_TOO_LARGE;
        }
        if self.open.len() >= MAX_DECODERS {
            return DECODE_BUSY;
        }
        let Some(sound) = Sound::new(encoded.to_vec()) else {
            return DECODE_INVALID;
        };
        self.next = self.next.checked_add(1).unwrap_or(1);
        self.open.insert(self.next, sound);
        self.next
    }

    pub(crate) fn read(&mut self, handle: i64, into: &mut [u8]) -> i64 {
        let Some(sound) = self.open.get_mut(&handle) else {
            return DECODE_NO_HANDLE;
        };
        match sound.read(into) {
            Some(written) => i64::try_from(written).unwrap_or(DECODE_INVALID),
            None => DECODE_INVALID,
        }
    }

    pub(crate) fn length(&self, handle: i64) -> i64 {
        match self.open.get(&handle) {
            Some(sound) => sound
                .length
                .and_then(|frames| i64::try_from(frames).ok())
                .unwrap_or(DECODE_UNKNOWN_LENGTH),
            None => DECODE_NO_HANDLE,
        }
    }

    pub(crate) fn close(&mut self, handle: i64) -> i64 {
        if self.open.remove(&handle).is_some() {
            0
        } else {
            DECODE_NO_HANDLE
        }
    }
}

struct Sound {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    /// Frames of the whole sound at [`RATE`], when the file says.
    length: Option<u64>,
    /// Decoded stereo frames, interleaved, at the source's own rate, not yet all turned into output.
    pending: Vec<f32>,
    /// The source's rate, from the last decoded packet.
    rate: u32,
    /// Where the next output frame falls between the pending frames, in source frames.
    position: f64,
    ended: bool,
    scratch: Vec<f32>,
}

impl Sound {
    fn new(encoded: Vec<u8>) -> Option<Self> {
        let source = MediaSourceStream::new(Box::new(Cursor::new(encoded)), Default::default());
        let format = symphonia::default::get_probe()
            .probe(
                &Hint::new(),
                source,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .ok()?;
        let track = format.default_track(TrackType::Audio)?;
        let params = track.codec_params.as_ref()?.audio()?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .ok()?;
        let rate = params.sample_rate.filter(|rate| *rate > 0)?;
        let length = track
            .num_frames
            .map(|frames| frames.saturating_mul(u64::from(RATE)) / u64::from(rate));
        let track = track.id;
        Some(Self {
            format,
            decoder,
            track,
            length,
            pending: Vec::new(),
            rate,
            position: 0.0,
            ended: false,
            scratch: Vec::new(),
        })
    }

    /// Fill `into` with whole output frames; `None` when the sound cannot be read on.
    fn read(&mut self, into: &mut [u8]) -> Option<usize> {
        let frames = into.len() / FRAME_BYTES;
        let mut written = 0;
        while written < frames {
            let step = f64::from(self.rate) / f64::from(RATE);
            let index = self.position as usize;
            let available = self.pending.len() / 2;
            if index + 1 >= available {
                if self.ended {
                    // The last frame has no neighbour to lean towards: it is played as it is.
                    if index < available {
                        write_frame(into, written, self.frame(index));
                        written += 1;
                        self.position += step;
                    }
                    break;
                }
                self.decode_more()?;
                continue;
            }
            let fraction = (self.position - index as f64) as f32;
            let (a, b) = (self.frame(index), self.frame(index + 1));
            write_frame(
                into,
                written,
                [
                    a[0] + (b[0] - a[0]) * fraction,
                    a[1] + (b[1] - a[1]) * fraction,
                ],
            );
            written += 1;
            self.position += step;
        }
        // Forget the frames every later output frame is past.
        let done = (self.position as usize).min(self.pending.len() / 2);
        if done > 0 {
            self.pending.drain(..done * 2);
            self.position -= done as f64;
        }
        Some(written * FRAME_BYTES)
    }

    fn frame(&self, index: usize) -> [f32; 2] {
        [self.pending[index * 2], self.pending[index * 2 + 1]]
    }

    /// Decode the next packet of the track into `pending`, or mark the end. `None` when it cannot be read on.
    fn decode_more(&mut self) -> Option<()> {
        let mut bad = 0;
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                // The end of the file, or of what can be read of it.
                Ok(None) | Err(Error::IoError(_)) => {
                    self.ended = true;
                    return Some(());
                }
                Err(_) => return None,
            };
            if packet.track_id != self.track {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(Error::DecodeError(_)) => {
                    bad += 1;
                    if bad > MAX_BAD_PACKETS {
                        return None;
                    }
                    continue;
                }
                Err(_) => return None,
            };
            let channels = decoded.spec().channels().count();
            let rate = decoded.spec().rate();
            if channels == 0 || rate == 0 {
                return None;
            }
            decoded.copy_to_vec_interleaved(&mut self.scratch);
            self.rate = rate;
            self.pending.reserve(self.scratch.len() / channels * 2);
            for frame in self.scratch.chunks_exact(channels) {
                let left = frame[0];
                let right = if channels == 1 { left } else { frame[1] };
                self.pending.extend([left, right]);
            }
            return Some(());
        }
    }
}

fn write_frame(into: &mut [u8], frame: usize, [left, right]: [f32; 2]) {
    debug_assert_eq!(usize::from(CHANNELS), 2);
    let at = frame * FRAME_BYTES;
    for (offset, sample) in [(0, left), (2, right)] {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        into[at + offset..at + offset + 2].copy_from_slice(&value.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mono 16-bit WAV of `samples` at `rate`.
    fn wav(rate: u32, samples: &[i16]) -> Vec<u8> {
        let data = samples.len() as u32 * 2;
        let mut out = Vec::new();
        out.extend(b"RIFF");
        out.extend((36 + data).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(rate.to_le_bytes());
        out.extend((rate * 2).to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend(16u16.to_le_bytes());
        out.extend(b"data");
        out.extend(data.to_le_bytes());
        for sample in samples {
            out.extend(sample.to_le_bytes());
        }
        out
    }

    fn read_all(sounds: &mut Sounds, handle: i64) -> Vec<[i16; 2]> {
        let mut frames = Vec::new();
        let mut buffer = [0u8; 4 * 100];
        loop {
            let written = sounds.read(handle, &mut buffer);
            assert!(written >= 0, "read failed: {written}");
            if written == 0 {
                return frames;
            }
            for frame in buffer[..written as usize].chunks_exact(4) {
                frames.push([
                    i16::from_le_bytes([frame[0], frame[1]]),
                    i16::from_le_bytes([frame[2], frame[3]]),
                ]);
            }
        }
    }

    #[test]
    fn a_mono_sound_at_the_output_rate_comes_out_on_both_channels_as_it_is() {
        let samples: Vec<i16> = (0..1000).map(|n| (n * 16) as i16).collect();
        let mut sounds = Sounds::default();
        let handle = sounds.open(&wav(RATE, &samples));
        assert!(handle > 0);
        assert_eq!(sounds.length(handle), 1000);
        let frames = read_all(&mut sounds, handle);
        assert_eq!(frames.len(), 1000);
        for (frame, sample) in frames.iter().zip(&samples) {
            // Through floating point and back: within one step of the original.
            assert!((i32::from(frame[0]) - i32::from(*sample)).abs() <= 1);
            assert_eq!(frame[0], frame[1]);
        }
    }

    #[test]
    fn another_rate_is_turned_into_the_output_rate_and_keeps_its_length() {
        let samples = vec![8000i16; 24_000];
        let mut sounds = Sounds::default();
        let handle = sounds.open(&wav(24_000, &samples));
        assert_eq!(sounds.length(handle), i64::from(RATE));
        let frames = read_all(&mut sounds, handle);
        assert!(
            (frames.len() as i64 - i64::from(RATE)).abs() <= 2,
            "{} frames",
            frames.len()
        );
        assert!(frames.iter().all(|f| (i32::from(f[0]) - 8000).abs() <= 1));
    }

    #[test]
    fn what_is_not_a_sound_is_refused_and_the_bounds_hold() {
        let mut sounds = Sounds::default();
        assert_eq!(sounds.open(b"not a sound at all"), DECODE_INVALID);
        assert_eq!(
            sounds.open(&vec![0; MAX_DECODE_INPUT + 1]),
            DECODE_TOO_LARGE
        );
        let sound = wav(RATE, &[0; 10]);
        let first = sounds.open(&sound);
        for _ in 1..MAX_DECODERS {
            assert!(sounds.open(&sound) > 0);
        }
        assert_eq!(sounds.open(&sound), DECODE_BUSY);
        assert_eq!(sounds.close(first), 0);
        assert_eq!(sounds.close(first), DECODE_NO_HANDLE);
        assert_eq!(sounds.read(first, &mut [0; 4]), DECODE_NO_HANDLE);
        assert!(sounds.open(&sound) > 0);
    }

    #[test]
    fn a_truncated_sound_plays_what_it_has_and_then_ends() {
        let mut sound = wav(RATE, &[1000; 500]);
        sound.truncate(sound.len() - 400);
        let mut sounds = Sounds::default();
        let handle = sounds.open(&sound);
        assert!(handle > 0);
        let frames = read_all(&mut sounds, handle);
        assert!(!frames.is_empty() && frames.len() <= 500);
    }
}
