//! The microphone extensions record from ([`peersus_api::audio::RECORD`]): what the device hears, made into one channel at
//! [`RECORD_RATE`] and handed to the extension as a WAV in its own storage when it finishes.
//!
//! WHO MAY. Only an extension the person allowed to record (`audio.record`), and on a platform that asks the person too
//! (Android), only after the system's own permission. One recording in the whole application: another extension's `record`
//! is refused while one runs.
//!
//! THE DEVICE. Opened on a thread of its own only while a recording runs, and closed when it ends: nothing listens for an
//! application that records nothing. The device's own callback takes the samples and never waits for the dispatch loop.
//!
//! WHAT BOUNDS IT. A recording keeps at most the frames its `max_seconds` allows; past them the samples are dropped and the
//! device is closed, and `finish` hands over what was heard. A recording ends, unkept, when its extension stops.
use cpal::{
    FromSample, SampleFormat, SizedSample, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use peersus_api::audio::{
    BAND_WINDOW, BANDS, LEVEL_MS, MAX_LEVELS, MAX_LIVE_LEVELS, RECORD_RATE, level,
};
use peersus_core::plugins::PluginId;
use rustfft::{Fft, FftPlanner, num_complex::Complex32};
use std::{
    sync::{Arc, Condvar, Mutex, OnceLock, PoisonError},
    time::{Duration, Instant},
};

/// Turns the device's frames into one channel at [`RECORD_RATE`]: each output sample is the mean of the input frames it covers.
struct Downmix {
    from: u32,
    channels: usize,
    phase: u32,
    sum: f32,
    count: u32,
}

impl Downmix {
    fn new(from: u32, channels: usize) -> Self {
        Self {
            from: from.max(1),
            channels: channels.max(1),
            phase: 0,
            sum: 0.0,
            count: 0,
        }
    }

    /// Take interleaved frames of `-1.0..=1.0` samples; push each finished output sample.
    fn feed(&mut self, frames: &[f32], out: &mut Vec<i16>, room: usize) {
        for frame in frames.chunks(self.channels) {
            self.sum += frame.iter().sum::<f32>() / frame.len() as f32;
            self.count += 1;
            self.phase += RECORD_RATE;
            // A device slower than the recording repeats a frame; a faster one averages several into one.
            while self.phase >= self.from {
                self.phase -= self.from;
                if out.len() >= room {
                    return;
                }
                let mean = self.sum / self.count.max(1) as f32;
                out.push((mean.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16);
                if self.phase < self.from {
                    self.sum = 0.0;
                    self.count = 0;
                }
            }
        }
    }
}

/// How loud a recording is, one [`LEVEL_MS`] step at a time, kept as the samples come rather than read from them again.
#[derive(Default)]
struct Loudness {
    levels: Vec<u8>,
    /// The largest absolute sample of the step under way, and how many samples it has.
    peak: u16,
    count: usize,
}

impl Loudness {
    const STEP: usize = (RECORD_RATE as u64 * LEVEL_MS / 1000) as usize;

    fn add(&mut self, samples: &[i16]) {
        for sample in samples {
            self.peak = self.peak.max(sample.unsigned_abs());
            self.count += 1;
            if self.count == Self::STEP {
                self.levels.push(level(self.peak));
                (self.peak, self.count) = (0, 0);
            }
        }
    }

    /// Every step so far, the one under way included.
    fn all(&self) -> Vec<u8> {
        let mut levels = self.levels.clone();
        if self.count > 0 {
            levels.push(level(self.peak));
        }
        levels.truncate(MAX_LEVELS);
        levels
    }
}

/// How strong each of [`BANDS`] ranges of frequencies is in `samples` (at most the newest [`BAND_WINDOW`]), lowest first, 0 to
/// 255: a Hann window, one transform, and the strongest frequency of each range on a scale of decibels. The ranges widen
/// with the frequency, as hearing does.
fn bands(samples: &[i16]) -> Vec<u8> {
    /// The lowest and the highest frequency shown, in hertz; the highest is the most a recording at [`RECORD_RATE`] holds.
    const LOW: f32 = 100.0;
    const HIGH: f32 = RECORD_RATE as f32 / 2.0;
    /// What is this far under the loudest possible tone, in decibels, shows nothing.
    const FLOOR: f32 = 54.0;
    static FFT: OnceLock<Arc<dyn Fft<f32>>> = OnceLock::new();

    let samples = &samples[samples.len().saturating_sub(BAND_WINDOW)..];
    if samples.len() < 2 {
        return vec![0; BANDS];
    }
    let mut buffer = vec![Complex32::default(); BAND_WINDOW];
    let last = (samples.len() - 1) as f32;
    for (i, (slot, sample)) in buffer.iter_mut().zip(samples).enumerate() {
        let hann = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / last).cos();
        slot.re = f32::from(*sample) / 32_768.0 * hann;
    }
    FFT.get_or_init(|| FftPlanner::new().plan_fft_forward(BAND_WINDOW))
        .process(&mut buffer);
    let hertz = RECORD_RATE as f32 / BAND_WINDOW as f32;
    // A tone as loud as a sample can be, through this window, has this magnitude.
    let full = BAND_WINDOW as f32 / 4.0;
    let edge = |band: usize| (LOW * (HIGH / LOW).powf(band as f32 / BANDS as f32) / hertz) as usize;
    (0..BANDS)
        .map(|band| {
            let from = edge(band).clamp(1, BAND_WINDOW / 2 - 1);
            let to = edge(band + 1).clamp(from + 1, BAND_WINDOW / 2);
            let strongest = buffer[from..to]
                .iter()
                .map(|bin| bin.norm())
                .fold(0.0, f32::max);
            let decibels = 20.0 * (strongest / full).max(1e-9).log10();
            ((decibels + FLOOR) / FLOOR * 255.0).clamp(0.0, 255.0) as u8
        })
        .collect()
}

struct Recording {
    plugin: PluginId,
    samples: Vec<i16>,
    loudness: Loudness,
    /// Most samples kept: `max_seconds` of the recording.
    limit: usize,
    /// When the microphone stops by itself, however many samples the device gave: `max_seconds` after the start.
    deadline: Instant,
    /// Why the device gave nothing, when it could not be opened.
    failed: Option<&'static str>,
}

impl Recording {
    fn listening(&self) -> bool {
        self.samples.len() < self.limit && Instant::now() < self.deadline
    }

    fn elapsed_ms(&self) -> u64 {
        self.samples.len() as u64 * 1000 / u64::from(RECORD_RATE)
    }
}

/// How long an ended recording its extension neither finished nor cancelled keeps the microphone from another extension.
const ABANDONED: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Shared {
    recording: Option<Recording>,
    /// A thread holds the device open.
    device: bool,
}

/// The application's one microphone, shared with the device's thread.
#[derive(Clone, Default)]
pub(crate) struct Microphone {
    shared: Arc<(Mutex<Shared>, Condvar)>,
}

impl Microphone {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start recording for `plugin`, at most `max_seconds` long. Its own earlier recording is replaced; another extension's is
    /// not interrupted.
    pub(crate) fn start(&self, plugin: &PluginId, max_seconds: u32) -> Result<(), &'static str> {
        let mut shared = self.lock();
        if shared.recording.as_ref().is_some_and(|recording| {
            &recording.plugin != plugin && Instant::now() < recording.deadline + ABANDONED
        }) {
            return Err("The microphone is recording for another extension");
        }
        let limit = RECORD_RATE as usize * max_seconds as usize;
        shared.recording = Some(Recording {
            plugin: plugin.clone(),
            samples: Vec::with_capacity(limit.min(RECORD_RATE as usize * 10)),
            loudness: Loudness::default(),
            limit,
            deadline: Instant::now() + Duration::from_secs(u64::from(max_seconds)),
            failed: None,
        });
        if !shared.device {
            shared.device = true;
            drop(shared);
            self.open_device();
        }
        Ok(())
    }

    /// Stop `plugin`'s recording and take what was heard, with how loud each step of it was.
    pub(crate) fn finish(&self, plugin: &PluginId) -> Result<(Vec<i16>, Vec<u8>), &'static str> {
        let taken = self.take(plugin).ok_or("No recording is running")?;
        match (taken.samples.is_empty(), taken.failed) {
            (true, Some(reason)) => Err(reason),
            (true, None) => Err("Nothing was heard"),
            (false, _) => Ok((taken.samples, taken.loudness.all())),
        }
    }

    /// How long `plugin`'s recording has run, how loud its newest steps were and how strong each range of frequencies is in
    /// what it hears now; `None` when it records nothing.
    pub(crate) fn levels(&self, plugin: &PluginId) -> Option<(u64, Vec<u8>, Vec<u8>)> {
        // Only copied under the lock, measured after it: the device's callback loses the samples of a moment the lock is held.
        let (elapsed, newest, window) = {
            let shared = self.lock();
            let recording = shared.recording.as_ref().filter(|r| &r.plugin == plugin)?;
            let all = recording.loudness.all();
            let samples = &recording.samples;
            (
                recording.elapsed_ms(),
                all[all.len().saturating_sub(MAX_LIVE_LEVELS)..].to_vec(),
                samples[samples.len().saturating_sub(BAND_WINDOW)..].to_vec(),
            )
        };
        Some((elapsed, newest, bands(&window)))
    }

    /// Stop `plugin`'s recording, if it has one, and keep nothing: it cancelled, stopped, restarted or was removed.
    pub(crate) fn clear(&self, plugin: &PluginId) {
        self.take(plugin);
    }

    fn take(&self, plugin: &PluginId) -> Option<Recording> {
        let mut shared = self.lock();
        let taken = shared
            .recording
            .take_if(|recording| &recording.plugin == plugin);
        drop(shared);
        self.shared.1.notify_all();
        taken
    }

    /// Hold the device open on a thread of its own until no recording is there.
    fn open_device(&self) {
        let microphone = self.clone();
        let started = std::thread::Builder::new()
            .name("peersus-microphone".into())
            .spawn(move || {
                if let Err(reason) = microphone.hold_device() {
                    peersus_diagnostics::dev_trace!(
                        stage = "microphone",
                        reason,
                        "Microphone unavailable"
                    );
                    let mut shared = microphone.lock();
                    if let Some(recording) = shared.recording.as_mut() {
                        recording.failed = Some(reason);
                    }
                    shared.device = false;
                }
            });
        if started.is_err() {
            let mut shared = self.lock();
            shared.device = false;
            if let Some(recording) = shared.recording.as_mut() {
                recording.failed = Some("The microphone could not be started");
            }
        }
    }

    fn hold_device(&self) -> Result<(), &'static str> {
        let device = cpal::default_host()
            .default_input_device()
            .ok_or("This device has no microphone")?;
        let default = device
            .default_input_config()
            .map_err(|_| "The microphone has no usable setting")?;
        let config = StreamConfig {
            channels: default.channels(),
            sample_rate: default.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };
        let stream = match default.sample_format() {
            SampleFormat::F32 => self.stream::<f32>(&device, config),
            SampleFormat::I16 => self.stream::<i16>(&device, config),
            SampleFormat::I32 => self.stream::<i32>(&device, config),
            SampleFormat::U16 => self.stream::<u16>(&device, config),
            _ => Err("The microphone gives samples in a form not read here"),
        }?;
        stream.play().map_err(|_| "The microphone did not start")?;
        let mut shared = self.lock();
        while shared.recording.as_ref().is_some_and(Recording::listening) {
            shared = self
                .shared
                .1
                .wait_timeout(shared, Duration::from_millis(250))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        // Let go of the device under the same lock that saw no recording (or a full one): a `record` from now on opens it again.
        shared.device = false;
        drop(shared);
        drop(stream);
        Ok(())
    }

    fn stream<T: SizedSample>(
        &self,
        device: &cpal::Device,
        config: StreamConfig,
    ) -> Result<cpal::Stream, &'static str>
    where
        f32: FromSample<T>,
    {
        let shared = self.shared.clone();
        let mut downmix = Downmix::new(config.sample_rate, usize::from(config.channels));
        let mut frames = Vec::new();
        device
            .build_input_stream(
                config,
                move |data: &[T], _: &cpal::InputCallbackInfo| {
                    frames.clear();
                    frames.extend(data.iter().map(|sample| sample.to_sample::<f32>()));
                    // Never wait for the dispatch loop: a moment it holds the lock loses these samples.
                    if let Ok(mut shared) = shared.0.try_lock()
                        && let Some(recording) = shared.recording.as_mut()
                    {
                        let room = recording.limit;
                        let before = recording.samples.len();
                        downmix.feed(&frames, &mut recording.samples, room);
                        recording.loudness.add(&recording.samples[before..]);
                    }
                },
                |_| {},
                None,
            )
            .map_err(|_| "The microphone did not open")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_devices_frames_become_one_channel_at_the_recording_rate() {
        // 48 kHz stereo: every three frames become one sample, the mean of both channels.
        let mut downmix = Downmix::new(48_000, 2);
        let mut out = Vec::new();
        let frames: Vec<f32> = [
            0.5, 0.5, 0.5, 0.5, 0.5, 0.5, -1.0, 0.0, -1.0, 0.0, -1.0, 0.0,
        ]
        .into();
        downmix.feed(&frames, &mut out, usize::MAX);
        assert_eq!(
            out,
            [
                (0.5 * f32::from(i16::MAX)) as i16,
                (-0.5 * f32::from(i16::MAX)) as i16
            ]
        );
        // 8 kHz mono: each frame is repeated to make two.
        let mut downmix = Downmix::new(8_000, 1);
        let mut out = Vec::new();
        downmix.feed(&[1.0, 0.0], &mut out, usize::MAX);
        assert_eq!(out, [i16::MAX, i16::MAX, 0, 0]);
        // Nothing past the room a recording has.
        let mut out = Vec::new();
        Downmix::new(16_000, 1).feed(&[0.1; 10], &mut out, 4);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn one_recording_at_a_time_and_only_its_extension_finishes_or_clears_it() {
        let microphone = Microphone::default();
        let (a, b) = (PluginId::new("a").unwrap(), PluginId::new("b").unwrap());
        // Without opening a device: a recording as `start` leaves it, with samples as the device would add them.
        let recording = |plugin: &PluginId, samples: Vec<i16>, deadline, failed| {
            let mut loudness = Loudness::default();
            loudness.add(&samples);
            Recording {
                plugin: plugin.clone(),
                samples,
                loudness,
                limit: 16,
                deadline,
                failed,
            }
        };
        let later = Instant::now() + Duration::from_secs(60);
        microphone.lock().recording = Some(recording(&a, vec![1, 2, 3], later, None));
        microphone.lock().device = true;
        assert!(microphone.start(&b, 5).is_err());
        assert!(microphone.finish(&b).is_err());
        assert_eq!(microphone.levels(&b), None);
        microphone.clear(&b);
        assert_eq!(
            microphone.levels(&a).map(|(_, levels, _)| levels.len()),
            Some(1)
        );
        assert_eq!(
            microphone.finish(&a).map(|(samples, _)| samples),
            Ok(vec![1, 2, 3])
        );
        assert!(microphone.finish(&a).is_err());
        let failed = Some("This device has no microphone");
        microphone.lock().recording = Some(recording(&a, Vec::new(), later, failed));
        assert_eq!(microphone.finish(&a), Err("This device has no microphone"));
    }

    #[test]
    fn loudness_is_one_level_a_step_quiet_to_loud_with_the_step_under_way_counted() {
        let mut loudness = Loudness::default();
        // A silent step, a loud one, and half of a quiet one.
        loudness.add(&vec![0; Loudness::STEP]);
        loudness.add(&vec![i16::MIN; Loudness::STEP]);
        loudness.add(&vec![328; Loudness::STEP / 2]);
        let levels = loudness.all();
        assert_eq!(levels.len(), 3);
        assert_eq!((levels[0], levels[1]), (0, 255));
        // A hundredth of full scale still shows: the square root lifts quiet sounds.
        assert!((20..=30).contains(&levels[2]), "{levels:?}");
    }

    #[test]
    fn a_recording_stops_listening_at_its_deadline_and_an_abandoned_one_frees_the_microphone() {
        let microphone = Microphone::default();
        let (a, b) = (PluginId::new("a").unwrap(), PluginId::new("b").unwrap());
        let ended = Instant::now() - Duration::from_secs(1);
        let silent = Recording {
            plugin: a.clone(),
            samples: Vec::new(),
            loudness: Loudness::default(),
            limit: 16,
            deadline: ended,
            failed: None,
        };
        // A device that gives no samples still lets go once the time is up.
        assert!(!silent.listening());
        microphone.lock().recording = Some(silent);
        microphone.lock().device = true;
        // Ended a moment ago: still its extension's to finish.
        assert!(microphone.start(&b, 5).is_err());
        // Ended and left unfinished past the grace: another extension may record.
        microphone.lock().recording.as_mut().unwrap().deadline = ended - ABANDONED;
        assert!(microphone.start(&b, 5).is_ok());
        assert!(microphone.finish(&a).is_err());
        microphone.clear(&b);
    }

    #[test]
    fn the_bands_of_a_tone_peak_where_the_tone_is_and_silence_shows_nothing() {
        let tone = |hertz: f32, loud: f32| -> Vec<i16> {
            (0..BAND_WINDOW)
                .map(|i| {
                    let turn = std::f32::consts::TAU * hertz * i as f32 / RECORD_RATE as f32;
                    (turn.sin() * loud * f32::from(i16::MAX)) as i16
                })
                .collect()
        };
        let strongest = |bands: &[u8]| {
            bands
                .iter()
                .enumerate()
                .max_by_key(|(_, level)| **level)
                .map(|(band, _)| band)
                .unwrap()
        };
        assert_eq!(bands(&[]), vec![0; BANDS]);
        assert_eq!(bands(&vec![0; BAND_WINDOW]), vec![0; BANDS]);

        let low = bands(&tone(200.0, 0.5));
        let high = bands(&tone(3_000.0, 0.5));
        assert_eq!(low.len(), BANDS);
        assert!(strongest(&low) < strongest(&high), "{low:?} {high:?}");
        // A loud tone nearly fills its bar and leaves the far end of the scale empty; a quieter one stands lower.
        assert!(low[strongest(&low)] > 200 && low[BANDS - 1] == 0, "{low:?}");
        let quiet = bands(&tone(200.0, 0.02));
        assert!(
            quiet[strongest(&quiet)] > 0 && quiet[strongest(&quiet)] < low[strongest(&low)],
            "{quiet:?}"
        );
        // More samples than the window: only the newest are measured.
        let mut long = tone(3_000.0, 0.5);
        long.extend(tone(200.0, 0.5));
        assert_eq!(strongest(&bands(&long)), strongest(&low));
    }
}
