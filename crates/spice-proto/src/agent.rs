//! The VD agent protocol (`spice/vd_agent.h`), tunnelled through main
//! channel `AGENT_DATA` messages in chunks of at most
//! [`VD_AGENT_MAX_DATA_SIZE`] bytes.

use bytes::{BufMut, BytesMut};

use crate::caps::Caps;
use crate::reader::{ProtoError, Reader, Result};

/// `VD_AGENT_PROTOCOL`
pub const VD_AGENT_PROTOCOL: u32 = 1;
/// `VD_AGENT_MAX_DATA_SIZE`: the largest `AGENT_DATA` payload.
pub const VD_AGENT_MAX_DATA_SIZE: usize = 2048;

/// `VD_AGENT_*` message types.
pub mod msg_type {
    pub const MOUSE_STATE: u32 = 1;
    pub const MONITORS_CONFIG: u32 = 2;
    pub const REPLY: u32 = 3;
    pub const CLIPBOARD: u32 = 4;
    pub const DISPLAY_CONFIG: u32 = 5;
    pub const ANNOUNCE_CAPABILITIES: u32 = 6;
    pub const CLIPBOARD_GRAB: u32 = 7;
    pub const CLIPBOARD_REQUEST: u32 = 8;
    pub const CLIPBOARD_RELEASE: u32 = 9;
    pub const FILE_XFER_START: u32 = 10;
    pub const FILE_XFER_STATUS: u32 = 11;
    pub const FILE_XFER_DATA: u32 = 12;
    pub const CLIENT_DISCONNECTED: u32 = 13;
    pub const MAX_CLIPBOARD: u32 = 14;
    pub const AUDIO_VOLUME_SYNC: u32 = 15;
    pub const GRAPHICS_DEVICE_INFO: u32 = 16;
}

/// `VD_AGENT_CAP_*`
pub mod cap {
    pub const MOUSE_STATE: u32 = 0;
    pub const MONITORS_CONFIG: u32 = 1;
    pub const REPLY: u32 = 2;
    pub const CLIPBOARD: u32 = 3;
    pub const DISPLAY_CONFIG: u32 = 4;
    pub const CLIPBOARD_BY_DEMAND: u32 = 5;
    pub const CLIPBOARD_SELECTION: u32 = 6;
    pub const SPARSE_MONITORS_CONFIG: u32 = 7;
    pub const GUEST_LINEEND_LF: u32 = 8;
    pub const GUEST_LINEEND_CRLF: u32 = 9;
    pub const MAX_CLIPBOARD: u32 = 10;
    pub const AUDIO_VOLUME_SYNC: u32 = 11;
    pub const MONITORS_CONFIG_POSITION: u32 = 12;
    pub const FILE_XFER_DISABLED: u32 = 13;
    pub const FILE_XFER_DETAILED_ERRORS: u32 = 14;
    pub const GRAPHICS_DEVICE_INFO: u32 = 15;
    pub const CLIPBOARD_NO_RELEASE_ON_REGRAB: u32 = 16;
    pub const CLIPBOARD_GRAB_SERIAL: u32 = 17;
    /// `VD_AGENT_END_CAP`
    pub const END: u32 = 18;
}

/// `VD_AGENT_CLIPBOARD_*` data types.
pub mod clipboard_type {
    pub const NONE: u32 = 0;
    pub const UTF8_TEXT: u32 = 1;
    pub const IMAGE_PNG: u32 = 2;
    pub const IMAGE_BMP: u32 = 3;
    pub const IMAGE_TIFF: u32 = 4;
    pub const IMAGE_JPG: u32 = 5;
    pub const FILE_LIST: u32 = 6;
}

/// `VD_AGENT_CLIPBOARD_SELECTION_*`
pub mod selection {
    pub const CLIPBOARD: u8 = 0;
    pub const PRIMARY: u8 = 1;
    pub const SECONDARY: u8 = 2;
}

/// `VD_AGENT_CONFIG_MONITORS_FLAG_*`
pub mod monitors_flag {
    pub const USE_POS: u32 = 1 << 0;
    pub const PHYSICAL_SIZE: u32 = 1 << 1;
}

/// `VDAgentMessage` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageHeader {
    pub protocol: u32,
    pub ty: u32,
    pub opaque: u64,
    pub size: u32,
}

impl MessageHeader {
    /// `sizeof(VDAgentMessage)`
    pub const SIZE: usize = 20;

    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            protocol: r.u32()?,
            ty: r.u32()?,
            opaque: r.u64()?,
            size: r.u32()?,
        })
    }

    pub fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.protocol);
        buf.put_u32_le(self.ty);
        buf.put_u64_le(self.opaque);
        buf.put_u32_le(self.size);
    }
}

/// Encode an agent message and split it into `AGENT_DATA` payloads
/// (`agent_msg_queue_many`).
pub fn chunk_message(ty: u32, body: &[u8]) -> Vec<Vec<u8>> {
    let mut all = BytesMut::with_capacity(MessageHeader::SIZE + body.len());
    MessageHeader {
        protocol: VD_AGENT_PROTOCOL,
        ty,
        opaque: 0,
        size: body.len() as u32,
    }
    .encode(&mut all);
    all.extend_from_slice(body);
    all.chunks(VD_AGENT_MAX_DATA_SIZE)
        .map(|c| c.to_vec())
        .collect()
}

/// The capabilities bitmap size spice-gtk sends (`VD_AGENT_CAPS_BYTES`).
pub const CAPS_BYTES: usize = ((cap::END as usize + 31) / 8) & !3;

/// `VDAgentAnnounceCapabilities`
pub fn encode_announce_caps(request: bool, caps: &Caps) -> Vec<u8> {
    let mut b = BytesMut::new();
    b.put_u32_le(request as u32);
    let mut words = caps.words().to_vec();
    words.resize(CAPS_BYTES / 4, 0);
    for w in words {
        b.put_u32_le(w);
    }
    b.to_vec()
}

/// Parse `VDAgentAnnounceCapabilities`: (request, caps).
pub fn decode_announce_caps(b: &[u8]) -> Result<(bool, Caps)> {
    let mut r = Reader::new(b);
    let request = r.u32()? != 0;
    let n = r.remaining() / 4;
    let words = (0..n).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
    Ok((request, Caps::from_words(words)))
}

/// `VDAgentMonConfig`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MonConfig {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub x: i32,
    pub y: i32,
}

/// `VDAgentMonitorsConfig`, optionally with physical sizes (in mm) after
/// the monitors when `PHYSICAL_SIZE` is set.
pub fn encode_monitors_config(flags: u32, monitors: &[MonConfig], mm: &[(u16, u16)]) -> Vec<u8> {
    let mut b = BytesMut::new();
    b.put_u32_le(monitors.len() as u32);
    b.put_u32_le(flags);
    for m in monitors {
        b.put_u32_le(m.height);
        b.put_u32_le(m.width);
        b.put_u32_le(m.depth);
        b.put_i32_le(m.x);
        b.put_i32_le(m.y);
    }
    if flags & monitors_flag::PHYSICAL_SIZE != 0 {
        for i in 0..monitors.len() {
            let (w, h) = mm.get(i).copied().unwrap_or((0, 0));
            b.put_u16_le(h);
            b.put_u16_le(w);
        }
    }
    b.to_vec()
}

/// Prefix for clipboard messages when the agent has `CLIPBOARD_SELECTION`:
/// a selection byte padded to 4 bytes.
fn selection_prefix(b: &mut BytesMut, with_selection: bool, sel: u8) {
    if with_selection {
        b.put_u8(sel);
        b.put_bytes(0, 3);
    }
}

/// `VDAgentClipboardGrab` (selection and serial only when the agent has
/// the matching caps).
pub fn encode_clipboard_grab(sel: Option<u8>, serial: Option<u32>, types: &[u32]) -> Vec<u8> {
    let mut b = BytesMut::new();
    selection_prefix(&mut b, sel.is_some(), sel.unwrap_or(0));
    if let Some(s) = serial {
        b.put_u32_le(s);
    }
    for t in types {
        b.put_u32_le(*t);
    }
    b.to_vec()
}

/// `VDAgentClipboardRequest`
pub fn encode_clipboard_request(sel: Option<u8>, ty: u32) -> Vec<u8> {
    let mut b = BytesMut::new();
    selection_prefix(&mut b, sel.is_some(), sel.unwrap_or(0));
    b.put_u32_le(ty);
    b.to_vec()
}

/// `VDAgentClipboard`
pub fn encode_clipboard(sel: Option<u8>, ty: u32, data: &[u8]) -> Vec<u8> {
    let mut b = BytesMut::new();
    selection_prefix(&mut b, sel.is_some(), sel.unwrap_or(0));
    b.put_u32_le(ty);
    b.put_slice(data);
    b.to_vec()
}

/// `VDAgentClipboardRelease`
pub fn encode_clipboard_release(sel: Option<u8>) -> Vec<u8> {
    let mut b = BytesMut::new();
    selection_prefix(&mut b, sel.is_some(), sel.unwrap_or(0));
    b.to_vec()
}

/// `VDAgentMaxClipboard`
pub fn encode_max_clipboard(max: i32) -> Vec<u8> {
    max.to_le_bytes().to_vec()
}

/// A parsed agent → client clipboard message (after the selection prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardMsg<'a> {
    Grab {
        serial: Option<u32>,
        types: Vec<u32>,
    },
    Request {
        ty: u32,
    },
    Data {
        ty: u32,
        data: &'a [u8],
    },
    Release,
}

/// Split the selection prefix off a clipboard message body.
pub fn clipboard_selection(body: &[u8], with_selection: bool) -> Result<(u8, &[u8])> {
    if with_selection {
        if body.len() < 4 {
            return Err(ProtoError::Truncated {
                at: 0,
                need: 4,
                have: body.len(),
            });
        }
        Ok((body[0], &body[4..]))
    } else {
        Ok((selection::CLIPBOARD, body))
    }
}

pub fn decode_clipboard(ty: u32, body: &[u8], with_serial: bool) -> Result<ClipboardMsg<'_>> {
    let mut r = Reader::new(body);
    Ok(match ty {
        msg_type::CLIPBOARD_GRAB => {
            let serial = if with_serial { Some(r.u32()?) } else { None };
            let n = r.remaining() / 4;
            let types = (0..n).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
            ClipboardMsg::Grab { serial, types }
        }
        msg_type::CLIPBOARD_REQUEST => ClipboardMsg::Request { ty: r.u32()? },
        msg_type::CLIPBOARD => {
            let ty = r.u32()?;
            ClipboardMsg::Data { ty, data: r.rest() }
        }
        msg_type::CLIPBOARD_RELEASE => ClipboardMsg::Release,
        t => {
            return Err(ProtoError::Invalid {
                what: "clipboard message type",
                value: t as u64,
            });
        }
    })
}

/// `VDAgentReply`: (type, error).
pub fn decode_reply(b: &[u8]) -> Result<(u32, u32)> {
    let mut r = Reader::new(b);
    Ok((r.u32()?, r.u32()?))
}

/// Reassembles agent messages from `AGENT_DATA` chunks
/// (`main_handle_agent_data_msg`).
#[derive(Default)]
pub struct Reassembler {
    header: Vec<u8>,
    hdr: Option<MessageHeader>,
    body: Vec<u8>,
}

/// Largest agent message we accept (clipboard images can be big).
pub const MAX_AGENT_MESSAGE: u32 = 64 * 1024 * 1024;

impl Reassembler {
    /// Feed one `AGENT_DATA` payload; returns completed messages.
    pub fn push(&mut self, mut data: &[u8]) -> Result<Vec<(MessageHeader, Vec<u8>)>> {
        let mut out = Vec::new();
        while !data.is_empty() {
            if self.hdr.is_none() {
                let n = (MessageHeader::SIZE - self.header.len()).min(data.len());
                self.header.extend_from_slice(&data[..n]);
                data = &data[n..];
                if self.header.len() < MessageHeader::SIZE {
                    break;
                }
                let h = MessageHeader::decode(&self.header)?;
                self.header.clear();
                if h.size > MAX_AGENT_MESSAGE {
                    return Err(ProtoError::Invalid {
                        what: "agent message size",
                        value: h.size as u64,
                    });
                }
                self.body = Vec::with_capacity(h.size as usize);
                self.hdr = Some(h);
            }
            let h = self.hdr.unwrap();
            let n = (h.size as usize - self.body.len()).min(data.len());
            self.body.extend_from_slice(&data[..n]);
            data = &data[n..];
            if self.body.len() == h.size as usize {
                out.push((h, std::mem::take(&mut self.body)));
                self.hdr = None;
            }
        }
        Ok(out)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_and_reassembly() {
        let body: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        let chunks = chunk_message(msg_type::CLIPBOARD, &body);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), VD_AGENT_MAX_DATA_SIZE);
        let mut r = Reassembler::default();
        let mut got = Vec::new();
        for c in &chunks {
            // feed in odd-sized pieces
            for piece in c.chunks(7) {
                got.extend(r.push(piece).unwrap());
            }
        }
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0.ty, msg_type::CLIPBOARD);
        assert_eq!(got[0].1, body);
        // Two small messages in one chunk.
        let mut two = chunk_message(msg_type::REPLY, &[1, 0, 0, 0, 2, 0, 0, 0]).concat();
        two.extend(chunk_message(msg_type::CLIPBOARD_RELEASE, &[]).concat());
        let got = r.push(&two).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(decode_reply(&got[0].1).unwrap(), (1, 2));
    }

    #[test]
    fn struct_sizes() {
        assert_eq!(CAPS_BYTES, 4);
        let caps = encode_announce_caps(true, &Caps::new().with(cap::REPLY));
        assert_eq!(caps.len(), 4 + CAPS_BYTES);
        let m = encode_monitors_config(
            monitors_flag::PHYSICAL_SIZE,
            &[MonConfig {
                width: 800,
                height: 600,
                depth: 32,
                x: 0,
                y: 0,
            }],
            &[(300, 200)],
        );
        // VDAgentMonitorsConfig (8) + VDAgentMonConfig (20) + VDAgentMonitorMM (4)
        assert_eq!(m.len(), 32);
        assert_eq!(&m[8..12], &600u32.to_le_bytes(), "height comes first");
        assert_eq!(encode_clipboard_grab(Some(0), Some(5), &[1]).len(), 12);
        assert_eq!(encode_clipboard_request(None, 1).len(), 4);
    }

    #[test]
    fn clipboard_parsing() {
        let body = encode_clipboard_grab(Some(1), Some(9), &[clipboard_type::UTF8_TEXT]);
        let (sel, rest) = clipboard_selection(&body, true).unwrap();
        assert_eq!(sel, 1);
        assert_eq!(
            decode_clipboard(msg_type::CLIPBOARD_GRAB, rest, true).unwrap(),
            ClipboardMsg::Grab {
                serial: Some(9),
                types: vec![clipboard_type::UTF8_TEXT]
            }
        );
        let body = encode_clipboard(None, clipboard_type::UTF8_TEXT, b"hi");
        assert_eq!(
            decode_clipboard(msg_type::CLIPBOARD, &body, false).unwrap(),
            ClipboardMsg::Data { ty: 1, data: b"hi" }
        );
    }
}
