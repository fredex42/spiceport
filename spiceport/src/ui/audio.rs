//! SDL3 audio for the playback and record channels.
//!
//! SDL3 audio streams are thread-safe, so the network thread pushes decoded
//! PCM straight into them; no hop through the main thread is needed (SDL
//! resamples, so 44.1 kHz vs 48 kHz is handled too). The safe `sdl3`
//! wrapper's stream types aren't `Send`, so this module talks to
//! `sdl3-sys` directly; all of spiceport's audio `unsafe` is here.

use std::ffi::{CStr, c_int, c_void};
use std::ptr;

use log::{debug, info, warn};
use sdl3::sys::audio::{
    SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK, SDL_AUDIO_DEVICE_DEFAULT_RECORDING, SDL_AUDIO_S16LE,
    SDL_AudioSpec, SDL_AudioStream, SDL_ClearAudioStream, SDL_DestroyAudioStream,
    SDL_GetAudioStreamData, SDL_GetAudioStreamQueued, SDL_OpenAudioDeviceStream,
    SDL_PutAudioStreamData, SDL_ResumeAudioStreamDevice, SDL_SetAudioStreamGain,
};
use spice_client::audio::{AudioBackend, AudioSink, AudioSource};
use tokio::sync::mpsc;

/// Queued playback beyond this is dropped so latency can't grow forever
/// (for example after the process was stopped for a while).
const MAX_QUEUED_MS: u32 = 1000;

/// `VOLUME_NORMAL` in spice-gstaudio.c.
const VOLUME_NORMAL: f32 = 65535.0;

/// Implements the client's audio backend with SDL. The SDL audio
/// subsystem must be initialised (on the main thread) for as long as this
/// is in use.
pub struct SdlAudio;

fn sdl_error() -> String {
    // SAFETY: SDL_GetError returns a valid, NUL-terminated thread-local string.
    unsafe { CStr::from_ptr(sdl3::sys::error::SDL_GetError()) }
        .to_string_lossy()
        .into_owned()
}

/// An owned `SDL_AudioStream`.
struct Stream(*mut SDL_AudioStream);

// SAFETY: every SDL3 audio stream function may be called from any thread;
// SDL serialises access with the stream's internal lock.
unsafe impl Send for Stream {}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: we own the stream; this also closes the device that
        // SDL_OpenAudioDeviceStream opened for it. After it returns, the
        // stream's callback (if any) can no longer run.
        unsafe { SDL_DestroyAudioStream(self.0) };
    }
}

impl Stream {
    fn set_gain(&self, gain: f32) {
        // SAFETY: valid stream.
        if !unsafe { SDL_SetAudioStreamGain(self.0, gain) } {
            debug!("SDL_SetAudioStreamGain: {}", sdl_error());
        }
    }
}

fn spec(channels: u32, frequency: u32) -> SDL_AudioSpec {
    SDL_AudioSpec {
        format: SDL_AUDIO_S16LE,
        channels: channels as c_int,
        freq: frequency as c_int,
    }
}

fn gain(volume: &[u16], mute: bool) -> f32 {
    if mute || volume.is_empty() {
        return if mute { 0.0 } else { 1.0 };
    }
    // One gain for the stream: average the channels.
    volume.iter().map(|&v| v as f32).sum::<f32>() / volume.len() as f32 / VOLUME_NORMAL
}

struct Playback {
    stream: Stream,
    bytes_per_sec: u32,
    volume: Vec<u16>,
    mute: bool,
}

impl AudioSink for Playback {
    fn write(&mut self, pcm: &[u8]) {
        if self.delay_ms() > MAX_QUEUED_MS {
            debug!("playback queue too long; dropping it");
            // SAFETY: valid stream.
            unsafe { SDL_ClearAudioStream(self.stream.0) };
        }
        // SAFETY: valid stream; SDL copies `pcm`.
        if !unsafe {
            SDL_PutAudioStreamData(self.stream.0, pcm.as_ptr().cast(), pcm.len() as c_int)
        } {
            warn!("SDL_PutAudioStreamData: {}", sdl_error());
        }
    }

    fn set_volume(&mut self, volume: &[u16]) {
        self.volume = volume.to_vec();
        self.stream.set_gain(gain(&self.volume, self.mute));
    }

    fn set_mute(&mut self, mute: bool) {
        self.mute = mute;
        self.stream.set_gain(gain(&self.volume, self.mute));
    }

    fn delay_ms(&self) -> u32 {
        // SAFETY: valid stream.
        let queued = unsafe { SDL_GetAudioStreamQueued(self.stream.0) }.max(0) as u64;
        (queued * 1000 / self.bytes_per_sec.max(1) as u64) as u32
    }
}

struct Record {
    stream: Stream,
    /// Owned `Box<UnboundedSender>` handed to SDL as callback userdata;
    /// freed after the stream (and so the callback) is gone.
    userdata: *mut mpsc::UnboundedSender<Vec<u8>>,
    volume: Vec<u16>,
    mute: bool,
}

// SAFETY: the stream is Send (see Stream); `userdata` is only touched by
// SDL's callback and by Drop after the stream is destroyed.
unsafe impl Send for Record {}

impl Drop for Record {
    fn drop(&mut self) {
        // Destroy the stream first so the callback can't run any more.
        // SAFETY: valid stream, destroyed exactly once (null afterwards).
        unsafe { SDL_DestroyAudioStream(self.stream.0) };
        self.stream.0 = ptr::null_mut();
        // SAFETY: allocated with Box::into_raw in record_start, freed once.
        drop(unsafe { Box::from_raw(self.userdata) });
    }
}

impl AudioSource for Record {
    fn set_volume(&mut self, volume: &[u16]) {
        self.volume = volume.to_vec();
        self.stream.set_gain(gain(&self.volume, self.mute));
    }

    fn set_mute(&mut self, mute: bool) {
        self.mute = mute;
        self.stream.set_gain(gain(&self.volume, self.mute));
    }
}

/// Runs on SDL's audio thread when captured data is available.
unsafe extern "C" fn record_callback(
    userdata: *mut c_void,
    stream: *mut SDL_AudioStream,
    additional: c_int,
    _total: c_int,
) {
    if additional <= 0 {
        return;
    }
    // SAFETY: userdata is the live Box<UnboundedSender> from record_start.
    let tx = unsafe { &*(userdata as *const mpsc::UnboundedSender<Vec<u8>>) };
    let mut buf = vec![0u8; additional as usize];
    // SAFETY: valid stream (we're in its callback) and buffer.
    let n = unsafe { SDL_GetAudioStreamData(stream, buf.as_mut_ptr().cast(), additional) };
    if n > 0 {
        buf.truncate(n as usize);
        let _ = tx.send(buf);
    }
}

impl AudioBackend for SdlAudio {
    fn playback_start(&self, channels: u32, frequency: u32) -> Option<Box<dyn AudioSink>> {
        let spec = spec(channels, frequency);
        // SAFETY: valid spec; no callback (we push data).
        let s = unsafe {
            SDL_OpenAudioDeviceStream(
                SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK,
                &spec,
                None,
                ptr::null_mut(),
            )
        };
        if s.is_null() {
            warn!("cannot open audio playback: {}", sdl_error());
            return None;
        }
        let stream = Stream(s);
        // SAFETY: valid stream; devices from SDL_OpenAudioDeviceStream start paused.
        unsafe { SDL_ResumeAudioStreamDevice(stream.0) };
        info!("audio playback: {channels} channels at {frequency} Hz");
        Some(Box::new(Playback {
            stream,
            bytes_per_sec: frequency * channels * 2,
            volume: Vec::new(),
            mute: false,
        }))
    }

    fn record_start(
        &self,
        channels: u32,
        frequency: u32,
        data: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Option<Box<dyn AudioSource>> {
        let spec = spec(channels, frequency);
        let userdata = Box::into_raw(Box::new(data));
        // SAFETY: valid spec; userdata stays alive until Record::drop.
        let s = unsafe {
            SDL_OpenAudioDeviceStream(
                SDL_AUDIO_DEVICE_DEFAULT_RECORDING,
                &spec,
                Some(record_callback),
                userdata.cast(),
            )
        };
        if s.is_null() {
            warn!("cannot open audio recording: {}", sdl_error());
            // SAFETY: SDL didn't take it.
            drop(unsafe { Box::from_raw(userdata) });
            return None;
        }
        // SAFETY: valid stream.
        unsafe { SDL_ResumeAudioStreamDevice(s) };
        info!("audio recording: {channels} channels at {frequency} Hz");
        Some(Box::new(Record {
            stream: Stream(s),
            userdata,
            volume: Vec::new(),
            mute: false,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::gain;

    #[test]
    fn volume_to_gain() {
        assert_eq!(gain(&[65535, 65535], false), 1.0);
        assert_eq!(gain(&[0, 65535], false), 0.5);
        assert_eq!(gain(&[65535], true), 0.0);
        assert_eq!(gain(&[], false), 1.0);
    }
}
