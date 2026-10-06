//! `channel BaseChannel` messages, valid on every channel.

use bytes::{BufMut, BytesMut};

use super::{ClientMsg, lossy_cstr};
use crate::enums::msgc;
use crate::reader::{Reader, Result};

/// `SpiceMsgMigrate`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migrate {
    pub flags: u32,
}

impl Migrate {
    pub fn decode(b: &[u8]) -> Result<Self> {
        Ok(Self {
            flags: Reader::new(b).u32()?,
        })
    }
}

/// `SpiceMsgSetAck`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAck {
    pub generation: u32,
    pub window: u32,
}

impl SetAck {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            generation: r.u32()?,
            window: r.u32()?,
        })
    }
}

/// `SpiceMsgPing`. The trailing data only pads the message for bandwidth
/// measurement, so it is not kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ping {
    pub id: u32,
    pub timestamp: u64,
    pub data_len: usize,
}

impl Ping {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            id: r.u32()?,
            timestamp: r.u64()?,
            data_len: r.rest().len(),
        })
    }
}

/// `SpiceWaitForChannel`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitForChannel {
    pub channel_type: u8,
    pub channel_id: u8,
    pub message_serial: u64,
}

/// `SpiceMsgWaitForChannels`; also the body of `SPICE_MSG_DISPLAY_INVAL_ALL_PIXMAPS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitForChannels {
    pub wait_list: Vec<WaitForChannel>,
}

impl WaitForChannels {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        let n = r.u8()?;
        let mut wait_list = Vec::with_capacity(n as usize);
        for _ in 0..n {
            wait_list.push(WaitForChannel {
                channel_type: r.u8()?,
                channel_id: r.u8()?,
                message_serial: r.u64()?,
            });
        }
        Ok(Self { wait_list })
    }
}

/// `SpiceMsgDisconnect`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disconnecting {
    pub time_stamp: u64,
    /// `SpiceLinkErr`
    pub reason: u32,
}

impl Disconnecting {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            time_stamp: r.u64()?,
            reason: r.u32()?,
        })
    }
}

/// `SpiceMsgNotify`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notify {
    pub time_stamp: u64,
    pub severity: u32,
    pub visibility: u32,
    pub what: u32,
    pub message: String,
}

impl Notify {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        let time_stamp = r.u64()?;
        let severity = r.u32()?;
        let visibility = r.u32()?;
        let what = r.u32()?;
        let len = r.u32()? as usize;
        // channel-base.c ignores a message_len that overruns the body.
        let rest = r.rest();
        let message = if len <= rest.len() {
            lossy_cstr(&rest[..len])
        } else {
            String::new()
        };
        Ok(Self {
            time_stamp,
            severity,
            visibility,
            what,
            message,
        })
    }
}

/// `SpiceMsgcAckSync`
pub struct AckSync {
    pub generation: u32,
}

impl ClientMsg for AckSync {
    const TYPE: u16 = msgc::base::ACK_SYNC;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.generation);
    }
}

/// `SPICE_MSGC_ACK` (empty)
pub struct Ack;

impl ClientMsg for Ack {
    const TYPE: u16 = msgc::base::ACK;
    fn encode(&self, _: &mut BytesMut) {}
}

/// `SPICE_MSGC_PONG`: echoes the ping's id and timestamp, without its data.
pub struct Pong {
    pub id: u32,
    pub timestamp: u64,
}

impl ClientMsg for Pong {
    const TYPE: u16 = msgc::base::PONG;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.id);
        buf.put_u64_le(self.timestamp);
    }
}

/// `SPICE_MSGC_DISCONNECTING`
pub struct ClientDisconnecting {
    pub time_stamp: u64,
    pub reason: u32,
}

impl ClientMsg for ClientDisconnecting {
    const TYPE: u16 = msgc::base::DISCONNECTING;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u64_le(self.time_stamp);
        buf.put_u32_le(self.reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_pong() {
        let mut b = vec![5, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
        b.extend_from_slice(&[0; 100]);
        let p = Ping::decode(&b).unwrap();
        assert_eq!(p.id, 5);
        assert_eq!(p.timestamp, 0x0807060504030201);
        assert_eq!(p.data_len, 100);
        let mut out = BytesMut::new();
        Pong {
            id: p.id,
            timestamp: p.timestamp,
        }
        .encode(&mut out);
        assert_eq!(&out[..], &b[..12]);
    }

    #[test]
    fn notify_with_bad_length() {
        let mut b = vec![0u8; 20];
        b.extend_from_slice(&[3, 0, 0, 0]);
        b.extend_from_slice(b"hi\0");
        assert_eq!(Notify::decode(&b).unwrap().message, "hi");
        b[20] = 50;
        assert_eq!(Notify::decode(&b).unwrap().message, "");
    }

    #[test]
    fn wait_for_channels() {
        let b = [1u8, 2, 0, 9, 0, 0, 0, 0, 0, 0, 0];
        let w = WaitForChannels::decode(&b).unwrap();
        assert_eq!(
            w.wait_list,
            [WaitForChannel {
                channel_type: 2,
                channel_id: 0,
                message_serial: 9
            }]
        );
        assert!(WaitForChannels::decode(&[2, 0]).is_err());
    }
}
