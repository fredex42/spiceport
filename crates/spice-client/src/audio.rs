//! Audio backend traits (implemented by the frontend) and the codec glue
//! from spice-common `snd_codec.c`.
//!
//! PCM is always signed 16-bit little-endian, interleaved
//! (`SPICE_AUDIO_FMT_S16`).

use tokio::sync::mpsc;

/// `SND_CODEC_OPUS_FRAME_SIZE`: samples per channel in an Opus frame.
pub const OPUS_FRAME_SIZE: usize = 480;
/// `SND_CODEC_PLAYBACK_CHAN`
pub const OPUS_CHANNELS: usize = 2;
/// `SND_CODEC_OPUS_COMPRESSED_FRAME_BYTES`
pub const OPUS_MAX_COMPRESSED: usize = 480;

/// Where decoded playback audio goes. Methods may be called from any
/// network thread.
pub trait AudioSink: Send {
    /// Queue interleaved S16LE PCM.
    fn write(&mut self, pcm: &[u8]);
    /// Per-channel volume, 0..=65535 (`SPICE_MSG_PLAYBACK_VOLUME`).
    fn set_volume(&mut self, volume: &[u16]);
    fn set_mute(&mut self, mute: bool);
    /// Audio queued but not played yet, in milliseconds.
    fn delay_ms(&self) -> u32;
}

/// A microphone capture in progress; dropping it stops capturing.
pub trait AudioSource: Send {
    fn set_volume(&mut self, volume: &[u16]);
    fn set_mute(&mut self, mute: bool);
}

/// Opens audio devices for the playback and record channels.
pub trait AudioBackend: Send + Sync + 'static {
    /// `SPICE_MSG_PLAYBACK_START`: open an output for this format.
    fn playback_start(&self, channels: u32, frequency: u32) -> Option<Box<dyn AudioSink>>;
    /// `SPICE_MSG_RECORD_START`: start capturing; captured S16LE PCM
    /// chunks of any size go to `data`. The OS microphone permission
    /// prompt (if any) appears now, only when the guest asks to record.
    fn record_start(
        &self,
        channels: u32,
        frequency: u32,
        data: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Option<Box<dyn AudioSource>>;
}

/// Whether Opus can be used at `frequency` (`snd_codec_is_capable`).
pub fn opus_capable(frequency: Option<u32>) -> bool {
    cfg!(feature = "opus")
        && std::env::var_os("SPICE_DISABLE_OPUS").is_none()
        && frequency.is_none_or(|f| matches!(f, 48000 | 24000 | 16000 | 12000 | 8000))
}

#[cfg(feature = "opus")]
pub struct OpusDecoder(opus::Decoder);

#[cfg(feature = "opus")]
impl OpusDecoder {
    pub fn new(frequency: u32) -> Option<Self> {
        opus::Decoder::new(frequency, opus::Channels::Stereo)
            .ok()
            .map(Self)
    }

    /// Decode one packet to interleaved S16LE bytes.
    pub fn decode(&mut self, packet: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
        let mut pcm = [0i16; OPUS_FRAME_SIZE * OPUS_CHANNELS];
        let n = self
            .0
            .decode(packet, &mut pcm, false)
            .map_err(|e| e.to_string())?;
        out.clear();
        out.extend(
            pcm[..n * OPUS_CHANNELS]
                .iter()
                .flat_map(|s| s.to_le_bytes()),
        );
        Ok(())
    }
}

#[cfg(feature = "opus")]
pub struct OpusEncoder(opus::Encoder);

#[cfg(feature = "opus")]
impl OpusEncoder {
    pub fn new(frequency: u32) -> Option<Self> {
        opus::Encoder::new(frequency, opus::Channels::Stereo, opus::Application::Audio)
            .ok()
            .map(Self)
    }

    /// Encode exactly one frame (`OPUS_FRAME_SIZE` stereo samples).
    pub fn encode(&mut self, frame: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
        let pcm: Vec<i16> = frame
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        out.resize(OPUS_MAX_COMPRESSED, 0);
        let n = self.0.encode(&pcm, out).map_err(|e| e.to_string())?;
        out.truncate(n);
        Ok(())
    }
}

#[cfg(all(test, feature = "opus"))]
mod tests {
    use super::*;

    #[test]
    fn opus_round_trip() {
        let mut enc = OpusEncoder::new(48000).unwrap();
        let mut dec = OpusDecoder::new(48000).unwrap();
        // A 1 kHz tone.
        let frame: Vec<u8> = (0..OPUS_FRAME_SIZE)
            .flat_map(|i| {
                let s = ((i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / 48000.0).sin() * 8000.0)
                    as i16;
                [s.to_le_bytes(), s.to_le_bytes()].concat()
            })
            .collect();
        let mut packet = Vec::new();
        let mut pcm = Vec::new();
        for _ in 0..5 {
            enc.encode(&frame, &mut packet).unwrap();
            assert!(!packet.is_empty() && packet.len() <= OPUS_MAX_COMPRESSED);
            dec.decode(&packet, &mut pcm).unwrap();
        }
        assert_eq!(pcm.len(), frame.len());
        let energy: f64 = pcm
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f64)
            .map(|s| s * s)
            .sum();
        assert!(
            energy > 1e9,
            "decoded audio should not be silent ({energy})"
        );
    }
}
