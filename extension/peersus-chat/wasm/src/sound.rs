//! The audio message Chat plays. The file this device holds is decoded in this process, a few seconds ahead of the device, and
//! each chunk of samples is handed to the application, which plays it: the application never decodes it.
use super::{Fault, Link, RoomKey, Runner, Wire, host, json};
use crate::{media_sdk, sdk::audio, ui};

/// The name Chat gives the application for the one sound it plays.
const SOUND: &str = "message";
/// Samples handed over at once: two seconds.
const CHUNK_BYTES: usize = 2 * audio::RATE as usize * audio::FRAME_BYTES;
/// Below this much queued and not yet played, the next chunk is handed over.
const AHEAD_MS: u64 = 4000;
/// How often the application is asked how far the sound is while it plays.
pub(super) const ASK_MS: u64 = 250;

impl Runner {
    /// Play the audio message `row` of `room`, or pause or go on with it when it is the one playing.
    pub(super) fn listen<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        room: &RoomKey,
        row: String,
        now: u64,
    ) -> Result<(), Fault> {
        if let Some(heard) = self.listening.as_mut().filter(|heard| heard.row == row) {
            let method = if heard.playing {
                audio::PAUSE
            } else {
                audio::RESUME
            };
            link.call(audio::SERVICE, method, json!({}), 2000)
                .map_err(|_| Fault::Storage)?;
            heard.playing = !heard.playing;
            return Ok(());
        }
        self.stop_sound(link);
        let (asset, bytes) = self
            .engine
            .data
            .rows
            .get(&row)
            .filter(|found| &found.room == room)
            .and_then(|found| {
                let body = found.body.as_ref().filter(|body| body.kind == "audio")?;
                Some((found.asset.clone()?, body.attachments.first()?.bytes))
            })
            .ok_or(Fault::Missing)?;
        let decoder = {
            let encoded = link.read_whole(&asset, self.revision, bytes)?;
            media_sdk::open_sound(&encoded).ok_or(Fault::Invalid)?
        };
        let mut heard = ui::Listening {
            row,
            playing: true,
            position_ms: 0,
            length_ms: media_sdk::sound_length(decoder)
                .map(|frames| frames * 1000 / u64::from(audio::RATE)),
            decoder,
            decoded_all: false,
            asked: now,
        };
        match hand_over(link, &mut heard, audio::PLAY) {
            Ok(true) => {
                self.listening = Some(heard);
                Ok(())
            }
            Ok(false) => {
                media_sdk::close_sound(decoder);
                Err(Fault::Invalid)
            }
            Err(error) => {
                media_sdk::close_sound(decoder);
                Err(error)
            }
        }
    }

    /// While a sound plays: learn how far it is, keep a few seconds handed over ahead of it, and let it go once it ended or
    /// another sound replaced it.
    pub(super) fn pump_sound<H: Wire + host::Data>(&mut self, link: &mut Link<'_, H>, now: u64) {
        let Some(heard) = self.listening.as_mut() else {
            return;
        };
        if now.saturating_sub(heard.asked) < ASK_MS {
            return;
        }
        heard.asked = now;
        let state: audio::Playback = match link.ask(audio::SERVICE, audio::STATE, json!({}), 2000) {
            Ok(state) => state,
            Err(_) => {
                self.stop_sound(link);
                return;
            }
        };
        if state.sound.as_deref() != Some(SOUND) {
            // It ended by itself, or another sound took its place.
            media_sdk::close_sound(heard.decoder);
            self.listening = None;
            return;
        }
        heard.playing = state.playing;
        heard.position_ms = state.position_ms;
        // Played to the end, or the next chunk could not be handed over: either way it stops here.
        let ended = heard.decoded_all && state.queued_ms == 0;
        if ended
            || !heard.decoded_all
                && state.queued_ms < AHEAD_MS
                && hand_over(link, heard, audio::QUEUE).is_err()
        {
            self.stop_sound(link);
        }
    }

    /// Stop the sound Chat plays, if any, and let go of its decoder.
    pub(super) fn stop_sound<H: Wire + host::Data>(&mut self, link: &mut Link<'_, H>) {
        if let Some(heard) = self.listening.take() {
            media_sdk::close_sound(heard.decoder);
            let _ = link.call(audio::SERVICE, audio::STOP, json!({}), 2000);
        }
    }
}

/// Decode the next chunk and hand it to the application with `method` (`play` or `queue`): `Ok(false)` when the sound had no
/// more.
fn hand_over<H: Wire + host::Data>(
    link: &mut Link<'_, H>,
    heard: &mut ui::Listening,
    method: &str,
) -> Result<bool, Fault> {
    let mut samples = vec![0u8; CHUNK_BYTES];
    let written = media_sdk::read_sound(heard.decoder, &mut samples).ok_or(Fault::Invalid)?;
    if written == 0 {
        heard.decoded_all = true;
        return Ok(false);
    }
    samples.truncate(written);
    let stage = link.stage_bytes(&samples)?;
    let handed = link.call(
        audio::SERVICE,
        method,
        json!({"sound": SOUND, "bytes": written, "source": {"kind": "stage", "stage": stage}}),
        5000,
    );
    link.abort(stage);
    handed.map(|_| true).map_err(|_| Fault::Storage)
}
