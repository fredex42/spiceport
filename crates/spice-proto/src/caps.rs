//! Capability bit sets exchanged during the link handshake (`*_CAP_*` in
//! `spice/protocol.h`). Capability `n` lives in word `n / 32`, bit `n % 32`
//! (`spice_channel_set_capability` / `test_capability` in spice-channel.c).

/// `SPICE_COMMON_CAP_*`
pub mod common {
    pub const PROTOCOL_AUTH_SELECTION: u32 = 0;
    pub const AUTH_SPICE: u32 = 1;
    pub const AUTH_SASL: u32 = 2;
    pub const MINI_HEADER: u32 = 3;
}

/// `SPICE_PLAYBACK_CAP_*`
pub mod playback {
    pub const CELT_0_5_1: u32 = 0;
    pub const VOLUME: u32 = 1;
    pub const LATENCY: u32 = 2;
    pub const OPUS: u32 = 3;
}

/// `SPICE_RECORD_CAP_*`
pub mod record {
    pub const CELT_0_5_1: u32 = 0;
    pub const VOLUME: u32 = 1;
    pub const OPUS: u32 = 2;
}

/// `SPICE_MAIN_CAP_*`
pub mod main {
    pub const SEMI_SEAMLESS_MIGRATE: u32 = 0;
    pub const NAME_AND_UUID: u32 = 1;
    pub const AGENT_CONNECTED_TOKENS: u32 = 2;
    pub const SEAMLESS_MIGRATE: u32 = 3;
}

/// `SPICE_DISPLAY_CAP_*`
pub mod display {
    pub const SIZED_STREAM: u32 = 0;
    pub const MONITORS_CONFIG: u32 = 1;
    pub const COMPOSITE: u32 = 2;
    pub const A8_SURFACE: u32 = 3;
    pub const STREAM_REPORT: u32 = 4;
    pub const LZ4_COMPRESSION: u32 = 5;
    pub const PREF_COMPRESSION: u32 = 6;
    pub const GL_SCANOUT: u32 = 7;
    pub const MULTI_CODEC: u32 = 8;
    pub const CODEC_MJPEG: u32 = 9;
    pub const CODEC_VP8: u32 = 10;
    pub const CODEC_H264: u32 = 11;
    pub const PREF_VIDEO_CODEC_TYPE: u32 = 12;
    pub const CODEC_VP9: u32 = 13;
    pub const CODEC_H265: u32 = 14;
    pub const GL_SCANOUT2: u32 = 15;
}

/// `SPICE_INPUTS_CAP_*`
pub mod inputs {
    pub const KEY_SCANCODE: u32 = 0;
}

/// `SPICE_SPICEVMC_CAP_*`
pub mod spicevmc {
    pub const DATA_COMPRESS_LZ4: u32 = 0;
}

/// A capability bitmap as sent on the wire: a list of u32 words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caps(Vec<u32>);

impl Caps {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_words(words: Vec<u32>) -> Self {
        Self(words)
    }

    pub fn with(mut self, cap: u32) -> Self {
        self.set(cap);
        self
    }

    pub fn set(&mut self, cap: u32) {
        let w = (cap / 32) as usize;
        if self.0.len() <= w {
            self.0.resize(w + 1, 0);
        }
        self.0[w] |= 1 << (cap % 32);
    }

    pub fn test(&self, cap: u32) -> bool {
        self.0
            .get((cap / 32) as usize)
            .is_some_and(|w| w & (1 << (cap % 32)) != 0)
    }

    pub fn words(&self) -> &[u32] {
        &self.0
    }

    /// The set capability numbers, for logging.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.0.iter().enumerate().flat_map(|(i, w)| {
            (0..32)
                .filter(move |b| w & (1 << b) != 0)
                .map(move |b| i as u32 * 32 + b)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_test() {
        let c = Caps::new().with(common::MINI_HEADER).with(40);
        assert_eq!(c.words(), &[1 << 3, 1 << 8]);
        assert!(c.test(3));
        assert!(c.test(40));
        assert!(!c.test(0));
        assert!(!c.test(1000));
        assert_eq!(c.iter().collect::<Vec<_>>(), [3, 40]);
    }
}
