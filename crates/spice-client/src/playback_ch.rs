//! The playback channel (`channel-playback.c`).

use std::sync::Arc;

use log::{debug, info, warn};
use spice_proto::caps::{self, Caps};
use spice_proto::enums::{audio_data_mode, msg};
use spice_proto::msg::audio as a;
use spice_proto::{chan_log, warn_once_per};

use crate::audio::{self, AudioBackend, AudioSink};
use crate::channel::{Channel, Message};
use crate::error::Result;

/// How often (in packets) to resync the multimedia clock to the audio
/// delay, as `playback_handle_data` asks for it every 100 frames.
const DELAY_SYNC_EVERY: u32 = 100;

pub struct PlaybackChannel {
    ch: Channel,
    name: Arc<str>,
    backend: Option<Arc<dyn AudioBackend>>,
    sink: Option<Box<dyn AudioSink>>,
    mode: u16,
    #[cfg(feature = "opus")]
    decoder: Option<audio::OpusDecoder>,
    #[cfg_attr(not(feature = "opus"), allow(dead_code))]
    pcm: Vec<u8>,
    last_time: u32,
    frame_count: u32,
    volume: Option<Vec<u16>>,
    mute: bool,
}

impl PlaybackChannel {
    /// `spice_playback_channel_set_capabilities`
    pub fn caps() -> Caps {
        let mut c = Caps::new()
            .with(caps::playback::VOLUME)
            .with(caps::playback::LATENCY);
        if audio::opus_capable(None) {
            c.set(caps::playback::OPUS);
        }
        c
    }

    pub fn new(ch: Channel, backend: Option<Arc<dyn AudioBackend>>) -> Self {
        let name = ch.name.clone();
        Self {
            ch,
            name,
            backend,
            sink: None,
            mode: audio_data_mode::RAW,
            #[cfg(feature = "opus")]
            decoder: None,
            pcm: Vec::new(),
            last_time: 0,
            frame_count: 0,
            volume: None,
            mute: false,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        while let Some(m) = self.ch.next().await? {
            self.handle(m)?;
        }
        Ok(())
    }

    fn handle(&mut self, m: Message) -> Result<()> {
        let b = &m.body;
        match m.ty {
            msg::playback::MODE => {
                let (time, mode) = a::decode_playback_mode(b)?;
                chan_log!(debug, self, "mode {mode} at time {time}");
                if mode != audio_data_mode::RAW && mode != audio_data_mode::OPUS {
                    warn!("{}: unhandled audio mode {mode}", self.name);
                }
                self.mode = mode;
            }
            msg::playback::START => {
                let s = a::Start::decode_playback(b)?;
                info!(
                    "{}: start: {} channels at {} Hz, {}",
                    self.name,
                    s.channels,
                    s.frequency,
                    if self.mode == audio_data_mode::OPUS {
                        "Opus"
                    } else {
                        "raw PCM"
                    }
                );
                self.frame_count = 0;
                self.last_time = s.time;
                #[cfg(feature = "opus")]
                {
                    self.decoder = None;
                    if self.mode == audio_data_mode::OPUS {
                        self.decoder = audio::OpusDecoder::new(s.frequency);
                        if self.decoder.is_none() {
                            warn!("{}: creating the Opus decoder failed", self.name);
                        }
                    }
                }
                self.sink = self
                    .backend
                    .as_ref()
                    .and_then(|be| be.playback_start(s.channels, s.frequency));
                if let Some(sink) = &mut self.sink {
                    if let Some(v) = &self.volume {
                        sink.set_volume(v);
                    }
                    sink.set_mute(self.mute);
                }
            }
            msg::playback::DATA => {
                let (time, data) = a::decode_playback_data(b)?;
                self.data(time, data);
            }
            msg::playback::STOP => {
                chan_log!(debug, self, "stop");
                self.sink = None;
                self.ch.session.set_playback_delay(None);
            }
            msg::playback::VOLUME => {
                let v = a::decode_volume(b)?;
                if v.is_empty() {
                    warn!("{}: volume message with 0 channels", self.name);
                } else {
                    chan_log!(debug, self, "volume {v:?}");
                    if let Some(s) = &mut self.sink {
                        s.set_volume(&v);
                    }
                    self.volume = Some(v);
                }
            }
            msg::playback::MUTE => {
                self.mute = a::decode_mute(b)?;
                chan_log!(debug, self, "mute {}", self.mute);
                if let Some(s) = &mut self.sink {
                    s.set_mute(self.mute);
                }
            }
            msg::playback::LATENCY => {
                let l = a::decode_latency(b)?;
                chan_log!(debug, self, "server asks for latency {l} ms");
            }
            other => warn_once_per!(other, "{}: unhandled message {other}", self.name),
        }
        Ok(())
    }

    /// `playback_handle_data`
    fn data(&mut self, time: u32, data: &[u8]) {
        if (time.wrapping_sub(self.last_time) as i32) < 0 {
            debug!("{}: audio time went backwards", self.name);
        }
        self.last_time = time;
        let Some(sink) = &mut self.sink else {
            return;
        };
        match self.mode {
            audio_data_mode::RAW => sink.write(data),
            #[cfg(feature = "opus")]
            audio_data_mode::OPUS => {
                let Some(dec) = &mut self.decoder else {
                    return;
                };
                if let Err(e) = dec.decode(data, &mut self.pcm) {
                    warn!("{}: Opus decode failed: {e}", self.name);
                    return;
                }
                sink.write(&self.pcm);
            }
            _ => return,
        }
        // spice_playback_channel_set_delay: audio is the clock video follows.
        if self.frame_count.is_multiple_of(DELAY_SYNC_EVERY) {
            let delay = sink.delay_ms();
            chan_log!(trace, self, "playback delay {delay} ms");
            self.ch
                .session
                .set_mm_time(self.last_time.wrapping_sub(delay));
        }
        self.frame_count = self.frame_count.wrapping_add(1);
    }
}
