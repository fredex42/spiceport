//! `channel PlaybackChannel` / `channel RecordChannel` messages.

use bytes::{BufMut, BytesMut};

use super::ClientMsg;
use crate::enums::msgc;
use crate::reader::{Reader, Result};

/// `SpiceMsgPlaybackPacket`: (mm time, audio data).
pub fn decode_playback_data(b: &[u8]) -> Result<(u32, &[u8])> {
    let mut r = Reader::new(b);
    Ok((r.u32()?, r.rest()))
}

/// `SpiceMsgPlaybackMode`: (time, `SpiceAudioDataMode`).
pub fn decode_playback_mode(b: &[u8]) -> Result<(u32, u16)> {
    let mut r = Reader::new(b);
    Ok((r.u32()?, r.u16()?))
}

/// `SpiceMsgPlaybackStart` / `SpiceMsgRecordStart` (record has no `time`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Start {
    pub channels: u32,
    /// `SpiceAudioFmt`
    pub format: u16,
    pub frequency: u32,
    pub time: u32,
}

impl Start {
    pub fn decode_playback(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            channels: r.u32()?,
            format: r.u16()?,
            frequency: r.u32()?,
            time: r.u32()?,
        })
    }

    pub fn decode_record(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            channels: r.u32()?,
            format: r.u16()?,
            frequency: r.u32()?,
            time: 0,
        })
    }
}

/// `SpiceMsgAudioVolume`: per-channel volume, 0..=65535.
pub fn decode_volume(b: &[u8]) -> Result<Vec<u16>> {
    let mut r = Reader::new(b);
    let n = r.u8()?;
    (0..n).map(|_| r.u16()).collect()
}

/// `SpiceMsgAudioMute`
pub fn decode_mute(b: &[u8]) -> Result<bool> {
    Ok(Reader::new(b).u8()? != 0)
}

/// `SpiceMsgPlaybackLatency`
pub fn decode_latency(b: &[u8]) -> Result<u32> {
    Reader::new(b).u32()
}

/// `SPICE_MSGC_RECORD_DATA`
pub struct RecordData<'a> {
    pub time: u32,
    pub data: &'a [u8],
}

impl ClientMsg for RecordData<'_> {
    const TYPE: u16 = msgc::record::DATA;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.time);
        buf.put_slice(self.data);
    }
}

/// `SPICE_MSGC_RECORD_MODE`
pub struct RecordMode {
    pub time: u32,
    pub mode: u16,
}

impl ClientMsg for RecordMode {
    const TYPE: u16 = msgc::record::MODE;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.time);
        buf.put_u16_le(self.mode);
    }
}

/// `SPICE_MSGC_RECORD_START_MARK`
pub struct RecordStartMark {
    pub time: u32,
}

impl ClientMsg for RecordStartMark {
    const TYPE: u16 = msgc::record::START_MARK;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.time);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_and_volume() {
        let b = [2, 0, 0, 0, 1, 0, 0x80, 0xbb, 0, 0, 7, 0, 0, 0];
        let s = Start::decode_playback(&b).unwrap();
        assert_eq!(
            (s.channels, s.format, s.frequency, s.time),
            (2, 1, 48000, 7)
        );
        assert_eq!(Start::decode_record(&b[..10]).unwrap().frequency, 48000);
        assert_eq!(decode_volume(&[2, 1, 0, 0xff, 0xff]).unwrap(), [1, 0xffff]);
        assert!(decode_volume(&[3, 0, 0]).is_err());
        let mut m = BytesMut::new();
        RecordMode { time: 1, mode: 3 }.encode(&mut m);
        assert_eq!(m.len(), 6);
    }
}
