//! The sound extensions play ([`peersus_api::audio`]): finished samples, played as they are on the device's output.
//!
//! WHO MADE THEM. The extension, in its own process: the application never decodes a sound for it. It reads each chunk from the
//! extension's private storage, checks only that it is the size the extension said, and queues it.
//!
//! WHAT PLAYS. One sound in the whole application. A `play` replaces whatever plays, whichever extension it is; the one that
//! played sees no sound in its `state` from then on. A sound whose queue has run dry stays a little while (the extension may only
//! be late with its next chunk) and then ends by itself.
//!
//! THE DEVICE. Opened on a thread of its own only while a sound is there, and closed when none is: nothing is held open or woken
//! for an application that plays nothing. The device's own callback takes the samples; it never waits for the dispatch loop, and
//! plays silence for a moment the samples are being changed.
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use peersus_api::audio::{CHANNELS, MAX_QUEUED_BYTES, Playback, RATE};
use peersus_core::plugins::PluginId;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Condvar, Mutex, PoisonError},
};

/// Frames of silence after the last queued sample past which a sound has ended.
const DRAINED_FRAMES: u64 = 2 * RATE as u64;
/// Most samples (of every channel) one extension has queued and not yet played.
const MAX_QUEUED_SAMPLES: usize = MAX_QUEUED_BYTES / 2;
const STEREO: usize = CHANNELS as usize;

struct Playing {
    plugin: PluginId,
    sound: String,
    chunks: VecDeque<Vec<i16>>,
    /// Samples of the front chunk already played.
    offset: usize,
    /// Samples queued and not yet played.
    queued: usize,
    /// Frames played since the sound began.
    played: u64,
    /// Frames of silence since the queue ran dry.
    drained: u64,
    paused: bool,
}

#[derive(Default)]
struct Shared {
    playing: Option<Playing>,
    /// Changes whenever an extension's sound is let go of, so a chunk read before is not played after.
    generations: BTreeMap<PluginId, u64>,
    /// A thread holds the device open.
    device: bool,
}

impl Shared {
    fn generation(&self, plugin: &PluginId) -> u64 {
        self.generations.get(plugin).copied().unwrap_or(0)
    }

    fn play(
        &mut self,
        plugin: &PluginId,
        generation: u64,
        sound: String,
        samples: Vec<i16>,
    ) -> Result<(), &'static str> {
        if generation != self.generation(plugin) {
            return Err("The extension stopped while the sound was read");
        }
        self.playing = Some(Playing {
            plugin: plugin.clone(),
            sound,
            queued: samples.len(),
            chunks: VecDeque::from([samples]),
            offset: 0,
            played: 0,
            drained: 0,
            paused: false,
        });
        Ok(())
    }

    fn queue(
        &mut self,
        plugin: &PluginId,
        generation: u64,
        sound: &str,
        samples: Vec<i16>,
    ) -> Result<(), &'static str> {
        if generation != self.generation(plugin) {
            return Err("The extension stopped while the sound was read");
        }
        let Some(playing) = self.playing_for(plugin).filter(|p| p.sound == sound) else {
            return Err("This sound is not playing");
        };
        if playing.queued + samples.len() > MAX_QUEUED_SAMPLES {
            return Err("This extension has queued as much as it may");
        }
        playing.queued += samples.len();
        playing.drained = 0;
        playing.chunks.push_back(samples);
        Ok(())
    }

    fn playing_for(&mut self, plugin: &PluginId) -> Option<&mut Playing> {
        self.playing.as_mut().filter(|p| p.plugin == *plugin)
    }

    fn pause(&mut self, plugin: &PluginId, paused: bool) {
        if let Some(playing) = self.playing_for(plugin) {
            playing.paused = paused;
        }
    }

    fn stop(&mut self, plugin: &PluginId) {
        if self.playing_for(plugin).is_some() {
            self.playing = None;
        }
    }

    fn clear(&mut self, plugin: &PluginId) {
        let generation = self.generations.entry(plugin.clone()).or_default();
        *generation = generation.wrapping_add(1);
        self.stop(plugin);
    }

    fn state(&self, plugin: &PluginId) -> Playback {
        match self.playing.as_ref().filter(|p| p.plugin == *plugin) {
            Some(playing) => Playback {
                sound: Some(playing.sound.clone()),
                playing: !playing.paused,
                position_ms: playing.played * 1000 / u64::from(RATE),
                queued_ms: (playing.queued / STEREO) as u64 * 1000 / u64::from(RATE),
            },
            None => Playback {
                sound: None,
                playing: false,
                position_ms: 0,
                queued_ms: 0,
            },
        }
    }

    /// The next stereo frame for the device: silence while paused, between chunks or once the queue ran dry.
    fn next_frame(&mut self) -> [i16; 2] {
        let Some(playing) = self.playing.as_mut() else {
            return [0; 2];
        };
        if playing.paused {
            return [0; 2];
        }
        let Some(front) = playing.chunks.front() else {
            playing.drained += 1;
            if playing.drained >= DRAINED_FRAMES {
                self.playing = None;
            }
            return [0; 2];
        };
        let frame = [front[playing.offset], front[playing.offset + 1]];
        playing.offset += STEREO;
        playing.queued -= STEREO;
        playing.played += 1;
        if playing.offset >= front.len() {
            playing.chunks.pop_front();
            playing.offset = 0;
        }
        frame
    }
}

/// The application's one sound output, shared with the reads a `play` or `queue` runs off the dispatch loop.
#[derive(Clone, Default)]
pub(crate) struct SoundOutput {
    shared: Arc<(Mutex<Shared>, Condvar)>,
}

impl SoundOutput {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What a `play` or `queue` begun now must still match when its samples arrive.
    pub(crate) fn generation(&self, plugin: &PluginId) -> u64 {
        self.lock().generation(plugin)
    }

    /// Replace whatever plays with `plugin`'s `sound`, starting with `bytes` of samples, if `generation` is still the extension's.
    pub(crate) fn play(
        &self,
        plugin: &PluginId,
        generation: u64,
        sound: String,
        bytes: &[u8],
    ) -> Result<(), &'static str> {
        let mut shared = self.lock();
        shared.play(plugin, generation, sound, samples(bytes))?;
        if !shared.device {
            shared.device = true;
            drop(shared);
            self.open_device();
        }
        Ok(())
    }

    /// Add the next `bytes` of samples to the `sound` `plugin` plays.
    pub(crate) fn queue(
        &self,
        plugin: &PluginId,
        generation: u64,
        sound: &str,
        bytes: &[u8],
    ) -> Result<(), &'static str> {
        self.lock().queue(plugin, generation, sound, samples(bytes))
    }

    pub(crate) fn pause(&self, plugin: &PluginId, paused: bool) {
        self.lock().pause(plugin, paused);
    }

    pub(crate) fn stop(&self, plugin: &PluginId) {
        self.lock().stop(plugin);
        self.shared.1.notify_all();
    }

    /// Stop what `plugin` plays and every `play` or `queue` it has begun: it stopped, restarted, was turned off or removed.
    pub(crate) fn clear(&self, plugin: &PluginId) {
        self.lock().clear(plugin);
        self.shared.1.notify_all();
    }

    pub(crate) fn state(&self, plugin: &PluginId) -> Playback {
        self.lock().state(plugin)
    }

    /// Hold the device open on a thread of its own until no sound is there.
    fn open_device(&self) {
        let output = self.clone();
        let started = std::thread::Builder::new()
            .name("peersus-sound".into())
            .spawn(move || {
                if let Err(reason) = output.hold_device() {
                    peersus_diagnostics::dev_trace!(
                        stage = "sound-output",
                        reason,
                        "Sound output unavailable"
                    );
                    let mut shared = output.lock();
                    shared.playing = None;
                    shared.device = false;
                }
            });
        if started.is_err() {
            let mut shared = self.lock();
            shared.device = false;
            shared.playing = None;
        }
    }

    fn hold_device(&self) -> Result<(), &'static str> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no output device")?;
        let default = device
            .default_output_config()
            .map_err(|_| "no output configuration")?;
        let config = StreamConfig {
            channels: default.channels(),
            sample_rate: RATE,
            buffer_size: cpal::BufferSize::Default,
        };
        let stream = match default.sample_format() {
            SampleFormat::F32 => self.stream::<f32>(&device, config),
            SampleFormat::I16 => self.stream::<i16>(&device, config),
            SampleFormat::I32 => self.stream::<i32>(&device, config),
            SampleFormat::U16 => self.stream::<u16>(&device, config),
            _ => Err("unsupported sample format"),
        }?;
        stream.play().map_err(|_| "the output did not start")?;
        let mut shared = self.lock();
        while shared.playing.is_some() {
            // The device's callback ends a sound by itself; look again now and then rather than be told by it.
            shared = self
                .shared
                .1
                .wait_timeout(shared, std::time::Duration::from_millis(500))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        // Let go of the device under the same lock that saw no sound: a `play` from now on opens it again itself.
        shared.device = false;
        drop(shared);
        drop(stream);
        Ok(())
    }

    fn stream<T: SizedSample + FromSample<i16>>(
        &self,
        device: &cpal::Device,
        config: StreamConfig,
    ) -> Result<cpal::Stream, &'static str> {
        let channels = usize::from(config.channels);
        let shared = self.shared.clone();
        device
            .build_output_stream(
                config,
                move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                    let mut playing = shared.0.try_lock().ok();
                    for frame in data.chunks_mut(channels) {
                        let [left, right] = playing.as_mut().map_or([0; 2], |s| s.next_frame());
                        write_frame(frame, left, right);
                    }
                },
                |_| {},
                None,
            )
            .map_err(|_| "the output did not open")
    }
}

/// One stereo frame on a device with `frame.len()` channels: both on a mono one, the first two of more.
fn write_frame<T: Sample + FromSample<i16>>(frame: &mut [T], left: i16, right: i16) {
    match frame {
        [only] => *only = T::from_sample(((i32::from(left) + i32::from(right)) / 2) as i16),
        [first, second, rest @ ..] => {
            *first = T::from_sample(left);
            *second = T::from_sample(right);
            rest.fill(T::EQUILIBRIUM);
        }
        [] => {}
    }
}

fn samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }

    #[test]
    fn a_sound_plays_its_chunks_in_order_and_counts_where_it_is() {
        let mut shared = Shared::default();
        let a = plugin("a");
        shared.play(&a, 0, "s".into(), vec![1, -1, 2, -2]).unwrap();
        shared.queue(&a, 0, "s", vec![3, -3]).unwrap();
        assert_eq!(shared.state(&a).queued_ms, 3 * 1000 / u64::from(RATE));
        assert_eq!(shared.next_frame(), [1, -1]);
        assert_eq!(shared.next_frame(), [2, -2]);
        assert_eq!(shared.next_frame(), [3, -3]);
        let state = shared.state(&a);
        assert_eq!(
            (state.sound.as_deref(), state.playing, state.queued_ms),
            (Some("s"), true, 0)
        );
        // Dry: silence for a while, then the sound has ended by itself.
        for _ in 0..DRAINED_FRAMES {
            assert_eq!(shared.next_frame(), [0, 0]);
        }
        assert_eq!(shared.state(&a).sound, None);
    }

    #[test]
    fn a_play_replaces_another_extensions_sound_and_only_the_player_controls_it() {
        let mut shared = Shared::default();
        let (a, b) = (plugin("a"), plugin("b"));
        shared.play(&a, 0, "first".into(), vec![1, 1]).unwrap();
        shared.play(&b, 0, "second".into(), vec![2, 2]).unwrap();
        assert_eq!(shared.state(&a).sound, None);
        assert!(shared.queue(&a, 0, "first", vec![1, 1]).is_err());
        shared.pause(&a, true);
        shared.stop(&a);
        assert_eq!(shared.state(&b).sound.as_deref(), Some("second"));
        shared.pause(&b, true);
        assert_eq!(shared.next_frame(), [0, 0], "a paused sound plays silence");
        shared.pause(&b, false);
        assert_eq!(shared.next_frame(), [2, 2]);
    }

    #[test]
    fn past_the_queue_bound_or_after_the_extension_stopped_nothing_is_let_in() {
        let mut shared = Shared::default();
        let a = plugin("a");
        shared
            .play(&a, 0, "s".into(), vec![0; MAX_QUEUED_SAMPLES])
            .unwrap();
        assert!(shared.queue(&a, 0, "s", vec![0; 2]).is_err());
        assert!(shared.queue(&a, 0, "other", vec![0; 2]).is_err());
        let begun = shared.generation(&a);
        shared.clear(&a);
        assert_eq!(shared.state(&a).sound, None);
        assert!(shared.play(&a, begun, "s".into(), vec![0; 2]).is_err());
        assert_eq!(shared.state(&a).sound, None);
        shared
            .play(&a, shared.generation(&a), "s".into(), vec![0; 2])
            .unwrap();
    }

    #[test]
    fn a_frame_goes_to_every_kind_of_device() {
        let mut mono = [0i16; 1];
        write_frame(&mut mono, 100, 300);
        assert_eq!(mono, [200]);
        let mut surround = [7f32; 4];
        write_frame(&mut surround, i16::MAX, i16::MIN);
        assert_eq!(surround[2..], [0.0, 0.0]);
        assert!(surround[0] > 0.99 && surround[1] < -0.99);
    }
}
