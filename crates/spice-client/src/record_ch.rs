//! The record channel (`channel-record.c`): microphone audio to the guest.

use std::sync::Arc;

use log::{info, warn};
use spice_proto::caps::{self, Caps};
use spice_proto::enums::{audio_data_mode, audio_fmt, msg};
use spice_proto::msg::audio as a;
use spice_proto::{chan_log, warn_once_per};
use tokio::sync::mpsc;

use crate::audio::{self, AudioBackend, AudioSource};
use crate::channel::{Channel, Message};
use crate::error::Result;

pub struct RecordChannel {
    ch: Channel,
    name: Arc<str>,
    backend: Option<Arc<dyn AudioBackend>>,
    source: Option<Box<dyn AudioSource>>,
    pcm_rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    mode: u16,
    #[cfg(feature = "opus")]
    encoder: Option<audio::OpusEncoder>,
    /// Bytes per frame we send (one Opus frame, or the same for raw).
    frame_bytes: usize,
    /// A partial frame carried over to the next capture chunk.
    pending: Vec<u8>,
    started: bool,
    volume: Option<Vec<u16>>,
    mute: bool,
}

impl RecordChannel {
    /// `spice_record_channel_set_capabilities`
    pub fn caps() -> Caps {
        let mut c = Caps::new().with(caps::record::VOLUME);
        if audio::opus_capable(None) {
            c.set(caps::record::OPUS);
        }
        c
    }

    pub fn new(ch: Channel, backend: Option<Arc<dyn AudioBackend>>) -> Self {
        let name = ch.name.clone();
        Self {
            ch,
            name,
            backend,
            source: None,
            pcm_rx: None,
            mode: audio_data_mode::RAW,
            #[cfg(feature = "opus")]
            encoder: None,
            frame_bytes: 0,
            pending: Vec::new(),
            started: false,
            volume: None,
            mute: false,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        loop {
            let capturing = self.pcm_rx.is_some();
            let rx = self.pcm_rx.as_mut();
            tokio::select! {
                m = self.ch.next() => match m? {
                    Some(m) => self.handle(m)?,
                    None => return Ok(()),
                },
                Some(pcm) = async { match rx { Some(rx) => rx.recv().await, None => None } }, if capturing => {
                    self.send_data(&pcm);
                }
            }
        }
    }

    /// `spice_record_desired_mode`
    fn desired_mode(&self, frequency: u32) -> u16 {
        if audio::opus_capable(Some(frequency)) && self.ch.remote_has_cap(caps::record::OPUS) {
            audio_data_mode::OPUS
        } else {
            audio_data_mode::RAW
        }
    }

    fn handle(&mut self, m: Message) -> Result<()> {
        let b = &m.body;
        match m.ty {
            msg::record::START => {
                let s = a::Start::decode_record(b)?;
                if s.format != audio_fmt::S16 {
                    warn!("{}: unsupported record format {}", self.name, s.format);
                    return Ok(());
                }
                self.mode = self.desired_mode(s.frequency);
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
                #[cfg(feature = "opus")]
                {
                    self.encoder = None;
                    if self.mode == audio_data_mode::OPUS {
                        self.encoder = audio::OpusEncoder::new(s.frequency);
                        if self.encoder.is_none() {
                            warn!("{}: creating the Opus encoder failed", self.name);
                            return Ok(());
                        }
                    }
                }
                // frame_bytes = frame_size * 16 * channels / 8
                self.frame_bytes = audio::OPUS_FRAME_SIZE * 2 * s.channels as usize;
                self.pending.clear();
                self.started = false;
                self.source = None;
                let (tx, rx) = mpsc::unbounded_channel();
                self.source = self
                    .backend
                    .as_ref()
                    .and_then(|be| be.record_start(s.channels, s.frequency, tx));
                if let Some(src) = &mut self.source {
                    if let Some(v) = &self.volume {
                        src.set_volume(v);
                    }
                    src.set_mute(self.mute);
                    self.pcm_rx = Some(rx);
                } else {
                    self.pcm_rx = None;
                }
            }
            msg::record::STOP => {
                chan_log!(debug, self, "stop");
                self.source = None;
                self.pcm_rx = None;
                self.started = false;
            }
            msg::record::VOLUME => {
                let v = a::decode_volume(b)?;
                if !v.is_empty() {
                    if let Some(s) = &mut self.source {
                        s.set_volume(&v);
                    }
                    self.volume = Some(v);
                }
            }
            msg::record::MUTE => {
                self.mute = a::decode_mute(b)?;
                if let Some(s) = &mut self.source {
                    s.set_mute(self.mute);
                }
            }
            other => warn_once_per!(other, "{}: unhandled message {other}", self.name),
        }
        Ok(())
    }

    /// `spice_record_channel_send_data`: cut captured PCM into frames.
    fn send_data(&mut self, data: &[u8]) {
        if self.frame_bytes == 0 {
            return;
        }
        // The backends don't timestamp audio; spice-gtk sends 0 too.
        let time = 0;
        if !self.started {
            self.ch.tx.send(&a::RecordMode {
                time,
                mode: self.mode,
            });
            self.ch.tx.send(&a::RecordStartMark { time });
            self.started = true;
        }
        self.pending.extend_from_slice(data);
        let mut out = Vec::new();
        while self.pending.len() >= self.frame_bytes {
            let frame: Vec<u8> = self.pending.drain(..self.frame_bytes).collect();
            let payload: &[u8] = match self.mode {
                #[cfg(feature = "opus")]
                audio_data_mode::OPUS => {
                    let Some(enc) = &mut self.encoder else {
                        return;
                    };
                    if let Err(e) = enc.encode(&frame, &mut out) {
                        warn!("{}: Opus encode failed: {e}", self.name);
                        return;
                    }
                    &out
                }
                _ => &frame,
            };
            self.ch.tx.send(&a::RecordData {
                time,
                data: payload,
            });
        }
        let _ = &mut out;
    }
}
