//! `channel MainChannel` messages.

use bytes::{BufMut, BytesMut};

use super::{ClientMsg, lossy_cstr};
use crate::enums::msgc;
use crate::reader::{Reader, Result};

/// `SpiceMsgMainInit`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Init {
    pub session_id: u32,
    pub display_channels_hint: u32,
    pub supported_mouse_modes: u32,
    pub current_mouse_mode: u32,
    pub agent_connected: u32,
    pub agent_tokens: u32,
    pub multi_media_time: u32,
    pub ram_hint: u32,
}

impl Init {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            session_id: r.u32()?,
            display_channels_hint: r.u32()?,
            supported_mouse_modes: r.u32()?,
            current_mouse_mode: r.u32()?,
            agent_connected: r.u32()?,
            agent_tokens: r.u32()?,
            multi_media_time: r.u32()?,
            ram_hint: r.u32()?,
        })
    }
}

/// `SpiceChannelId`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelId {
    pub channel_type: u8,
    pub id: u8,
}

/// `SpiceMsgChannels`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsList {
    pub channels: Vec<ChannelId>,
}

impl ChannelsList {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        let n = r.u32()?;
        let raw = r.array_bytes(n as usize, 2)?;
        Ok(Self {
            channels: raw
                .chunks_exact(2)
                .map(|c| ChannelId {
                    channel_type: c[0],
                    id: c[1],
                })
                .collect(),
        })
    }
}

/// `SpiceMsgMainMouseMode`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseMode {
    pub supported_modes: u16,
    pub current_mode: u16,
}

impl MouseMode {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            supported_modes: r.u16()?,
            current_mode: r.u16()?,
        })
    }
}

/// A message whose body is a single u32: `SpiceMsgMainMultiMediaTime`,
/// `SpiceMsgMainAgentDisconnect`, `SpiceMsgMainAgentTokens`,
/// `SpiceMsgMainAgentConnectedTokens`.
pub fn decode_u32(b: &[u8]) -> Result<u32> {
    Reader::new(b).u32()
}

/// `SpiceMsgMainName`
pub fn decode_name(b: &[u8]) -> Result<String> {
    let mut r = Reader::new(b);
    let n = r.u32()?;
    Ok(lossy_cstr(r.bytes(n as usize)?))
}

/// `SpiceMsgMainUuid`
pub fn decode_uuid(b: &[u8]) -> Result<[u8; 16]> {
    Ok(Reader::new(b).bytes(16)?.try_into().unwrap())
}

/// Format a UUID as `spice_uuid_to_string` does.
pub fn uuid_to_string(u: &[u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u[0],
        u[1],
        u[2],
        u[3],
        u[4],
        u[5],
        u[6],
        u[7],
        u[8],
        u[9],
        u[10],
        u[11],
        u[12],
        u[13],
        u[14],
        u[15]
    )
}

/// `SPICE_MSGC_MAIN_ATTACH_CHANNELS` (empty)
pub struct AttachChannels;

impl ClientMsg for AttachChannels {
    const TYPE: u16 = msgc::main::ATTACH_CHANNELS;
    fn encode(&self, _: &mut BytesMut) {}
}

/// `SpiceMsgcMainMouseModeRequest`
pub struct MouseModeRequest {
    pub mode: u16,
}

impl ClientMsg for MouseModeRequest {
    const TYPE: u16 = msgc::main::MOUSE_MODE_REQUEST;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u16_le(self.mode);
    }
}

/// `SPICE_MSGC_MAIN_AGENT_START`
pub struct AgentStart {
    pub num_tokens: u32,
}

impl ClientMsg for AgentStart {
    const TYPE: u16 = msgc::main::AGENT_START;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.num_tokens);
    }
}

/// `SPICE_MSGC_MAIN_AGENT_TOKEN`
pub struct AgentToken {
    pub num_tokens: u32,
}

impl ClientMsg for AgentToken {
    const TYPE: u16 = msgc::main::AGENT_TOKEN;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.num_tokens);
    }
}

/// `SPICE_MSGC_MAIN_AGENT_DATA`: a chunk of the VD agent byte stream.
pub struct AgentData<'a>(pub &'a [u8]);

impl ClientMsg for AgentData<'_> {
    const TYPE: u16 = msgc::main::AGENT_DATA;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_slice(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init() {
        let b: Vec<u8> = (1u32..=8).flat_map(|v| v.to_le_bytes()).collect();
        let i = Init::decode(&b).unwrap();
        assert_eq!(i.session_id, 1);
        assert_eq!(i.ram_hint, 8);
        assert!(Init::decode(&b[..31]).is_err());
    }

    #[test]
    fn channels_list() {
        let b = [3u8, 0, 0, 0, 1, 0, 2, 0, 3, 0];
        let l = ChannelsList::decode(&b).unwrap();
        assert_eq!(l.channels.len(), 3);
        assert_eq!(
            l.channels[1],
            ChannelId {
                channel_type: 2,
                id: 0
            }
        );
        // A count larger than the body is rejected.
        assert!(ChannelsList::decode(&[0xff, 0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn name_and_uuid() {
        assert_eq!(decode_name(b"\x04\0\0\0vm1\0").unwrap(), "vm1");
        let u: [u8; 16] = core::array::from_fn(|i| i as u8);
        assert_eq!(
            uuid_to_string(&decode_uuid(&u).unwrap()),
            "00010203-0405-0607-0809-0a0b0c0d0e0f"
        );
    }
}
