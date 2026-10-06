//! Link handshake structures and per-message data headers (`spice/protocol.h`).

use bytes::{BufMut, BytesMut};

use crate::caps::Caps;
use crate::enums::{SPICE_MAGIC, SPICE_TICKET_PUBKEY_BYTES};
use crate::reader::{ProtoError, Reader, Result};

/// `SpiceLinkHeader`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkHeader {
    pub magic: u32,
    pub major_version: u32,
    pub minor_version: u32,
    /// Size of the `SpiceLinkMess`/`SpiceLinkReply` (plus caps) that follows.
    pub size: u32,
}

impl LinkHeader {
    pub const SIZE: usize = 16;

    pub fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.magic);
        buf.put_u32_le(self.major_version);
        buf.put_u32_le(self.minor_version);
        buf.put_u32_le(self.size);
    }

    pub fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            magic: r.u32()?,
            major_version: r.u32()?,
            minor_version: r.u32()?,
            size: r.u32()?,
        })
    }
}

/// `SpiceLinkMess` followed by the common and channel capability words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMess {
    pub connection_id: u32,
    pub channel_type: u8,
    pub channel_id: u8,
    pub common_caps: Caps,
    pub channel_caps: Caps,
}

impl LinkMess {
    /// `sizeof(SpiceLinkMess)`, without the caps.
    pub const SIZE: usize = 18;

    pub fn encoded_len(&self) -> usize {
        Self::SIZE + 4 * (self.common_caps.words().len() + self.channel_caps.words().len())
    }

    /// Encode `SpiceLinkHeader` + `SpiceLinkMess` + caps, as
    /// `spice_channel_send_link` does.
    pub fn encode_with_header(&self, major: u32, minor: u32, buf: &mut BytesMut) {
        LinkHeader {
            magic: SPICE_MAGIC,
            major_version: major,
            minor_version: minor,
            size: self.encoded_len() as u32,
        }
        .encode(buf);
        self.encode(buf);
    }

    pub fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.connection_id);
        buf.put_u8(self.channel_type);
        buf.put_u8(self.channel_id);
        buf.put_u32_le(self.common_caps.words().len() as u32);
        buf.put_u32_le(self.channel_caps.words().len() as u32);
        buf.put_u32_le(Self::SIZE as u32); // caps_offset
        for w in self
            .common_caps
            .words()
            .iter()
            .chain(self.channel_caps.words())
        {
            buf.put_u32_le(*w);
        }
    }

    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut r = Reader::new(body);
        let connection_id = r.u32()?;
        let channel_type = r.u8()?;
        let channel_id = r.u8()?;
        let ncommon = r.u32()?;
        let nchannel = r.u32()?;
        let off = r.u32()?;
        let (common_caps, channel_caps) = read_caps(&r, off, ncommon, nchannel)?;
        Ok(Self {
            connection_id,
            channel_type,
            channel_id,
            common_caps,
            channel_caps,
        })
    }
}

fn read_caps(r: &Reader<'_>, off: u32, ncommon: u32, nchannel: u32) -> Result<(Caps, Caps)> {
    let mut c = r.at_offset(off)?;
    let mut words = |n: u32| -> Result<Caps> {
        let raw = c.array_bytes(n as usize, 4)?;
        Ok(Caps::from_words(
            raw.as_chunks::<4>()
                .0
                .iter()
                .map(|w| u32::from_le_bytes(*w))
                .collect(),
        ))
    };
    Ok((words(ncommon)?, words(nchannel)?))
}

/// `SpiceLinkReply` followed by the server's capability words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkReply {
    /// `SpiceLinkErr`
    pub error: u32,
    /// DER SubjectPublicKeyInfo of the 1024-bit RSA ticket key.
    pub pub_key: [u8; SPICE_TICKET_PUBKEY_BYTES],
    pub common_caps: Caps,
    pub channel_caps: Caps,
}

impl LinkReply {
    /// `sizeof(SpiceLinkReply)`, without the caps.
    pub const SIZE: usize = 4 + SPICE_TICKET_PUBKEY_BYTES + 12;

    /// Decode the reply body (whose size came from the `LinkHeader`), with the
    /// same checks as `spice_channel_recv_link_msg`.
    pub fn decode(body: &[u8]) -> Result<Self> {
        if body.len() < Self::SIZE {
            return Err(ProtoError::Truncated {
                at: 0,
                need: Self::SIZE,
                have: body.len(),
            });
        }
        let mut r = Reader::new(body);
        let error = r.u32()?;
        let pub_key = r.bytes(SPICE_TICKET_PUBKEY_BYTES)?.try_into().unwrap();
        let ncommon = r.u32()?;
        let nchannel = r.u32()?;
        let off = r.u32()?;
        // Caps are only meaningful on success; a NEED_SECURED reply may omit them.
        let (common_caps, channel_caps) = if error == crate::enums::link_err::OK {
            read_caps(&r, off, ncommon, nchannel)?
        } else {
            (Caps::new(), Caps::new())
        };
        Ok(Self {
            error,
            pub_key,
            common_caps,
            channel_caps,
        })
    }

    pub fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.error);
        buf.put_slice(&self.pub_key);
        buf.put_u32_le(self.common_caps.words().len() as u32);
        buf.put_u32_le(self.channel_caps.words().len() as u32);
        buf.put_u32_le(Self::SIZE as u32);
        for w in self
            .common_caps
            .words()
            .iter()
            .chain(self.channel_caps.words())
        {
            buf.put_u32_le(*w);
        }
    }
}

/// `SpiceLinkAuthMechanism`
pub fn encode_auth_mechanism(mechanism: u32, buf: &mut BytesMut) {
    buf.put_u32_le(mechanism);
}

/// A decoded data header, either `SpiceMiniDataHeader` or `SpiceDataHeader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataHeader {
    /// Only present in the full header.
    pub serial: u64,
    pub msg_type: u16,
    pub size: u32,
    /// Offset of a `SpiceSubMessageList` in the body, or 0. Full header only.
    pub sub_list: u32,
}

impl DataHeader {
    /// `sizeof(SpiceMiniDataHeader)`
    pub const MINI_SIZE: usize = 6;
    /// `sizeof(SpiceDataHeader)`
    pub const FULL_SIZE: usize = 18;

    pub fn size(mini: bool) -> usize {
        if mini {
            Self::MINI_SIZE
        } else {
            Self::FULL_SIZE
        }
    }

    pub fn decode(b: &[u8], mini: bool) -> Result<Self> {
        let mut r = Reader::new(b);
        if mini {
            Ok(Self {
                serial: 0,
                msg_type: r.u16()?,
                size: r.u32()?,
                sub_list: 0,
            })
        } else {
            Ok(Self {
                serial: r.u64()?,
                msg_type: r.u16()?,
                size: r.u32()?,
                sub_list: r.u32()?,
            })
        }
    }

    pub fn encode(&self, mini: bool, buf: &mut BytesMut) {
        if !mini {
            buf.put_u64_le(self.serial);
        }
        buf.put_u16_le(self.msg_type);
        buf.put_u32_le(self.size);
        if !mini {
            buf.put_u32_le(self.sub_list);
        }
    }
}

/// One entry of a `SpiceSubMessageList`: (`SpiceSubMessage` type, body).
pub fn sub_messages(body: &[u8], list_offset: u32) -> Result<Vec<(u16, &[u8])>> {
    let base = Reader::new(body);
    let mut list = base.at_offset(list_offset)?;
    let count = list.u16()?;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let off = list.u32()?;
        let mut sub = base.at_offset(off)?;
        let ty = sub.u16()?;
        let size = sub.u32()?;
        out.push((ty, sub.bytes(size as usize)?));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps;

    #[test]
    fn sizes_match_c_sizeof() {
        let mut b = BytesMut::new();
        LinkHeader {
            magic: SPICE_MAGIC,
            major_version: 2,
            minor_version: 2,
            size: 0,
        }
        .encode(&mut b);
        assert_eq!(b.len(), LinkHeader::SIZE);
        assert_eq!(LinkHeader::SIZE, 16);

        let m = LinkMess {
            connection_id: 0,
            channel_type: 1,
            channel_id: 0,
            common_caps: Caps::new(),
            channel_caps: Caps::new(),
        };
        let mut b = BytesMut::new();
        m.encode(&mut b);
        assert_eq!(b.len(), 18);

        assert_eq!(LinkReply::SIZE, 178);

        let mut b = BytesMut::new();
        DataHeader {
            serial: 1,
            msg_type: 2,
            size: 3,
            sub_list: 0,
        }
        .encode(true, &mut b);
        assert_eq!(b.len(), 6);
        b.clear();
        DataHeader {
            serial: 1,
            msg_type: 2,
            size: 3,
            sub_list: 0,
        }
        .encode(false, &mut b);
        assert_eq!(b.len(), 18);
    }

    #[test]
    fn link_mess_wire_format() {
        let m = LinkMess {
            connection_id: 0x01020304,
            channel_type: 2,
            channel_id: 1,
            common_caps: Caps::new()
                .with(caps::common::PROTOCOL_AUTH_SELECTION)
                .with(caps::common::MINI_HEADER),
            channel_caps: Caps::new().with(caps::display::SIZED_STREAM),
        };
        let mut b = BytesMut::new();
        m.encode_with_header(2, 2, &mut b);
        #[rustfmt::skip]
        let expected = [
            b'R', b'E', b'D', b'Q', 2, 0, 0, 0, 2, 0, 0, 0, 26, 0, 0, 0,
            4, 3, 2, 1, 2, 1, 1, 0, 0, 0, 1, 0, 0, 0, 18, 0, 0, 0,
            0b1001, 0, 0, 0, 1, 0, 0, 0,
        ];
        assert_eq!(&b[..], &expected);
        assert_eq!(LinkMess::decode(&b[16..]).unwrap(), m);
    }

    #[test]
    fn link_reply_round_trip() {
        let r = LinkReply {
            error: 0,
            pub_key: [7; SPICE_TICKET_PUBKEY_BYTES],
            common_caps: Caps::new().with(1).with(3),
            channel_caps: Caps::new().with(33),
        };
        let mut b = BytesMut::new();
        r.encode(&mut b);
        assert_eq!(b.len(), 178 + 12);
        assert_eq!(LinkReply::decode(&b).unwrap(), r);
        // Caps claiming more words than present must fail, not panic.
        let mut bad = b.to_vec();
        bad[166] = 200;
        assert!(LinkReply::decode(&bad).is_err());
        assert!(LinkReply::decode(&b[..100]).is_err());
    }

    #[test]
    fn data_header_round_trip() {
        for mini in [true, false] {
            let h = DataHeader {
                serial: if mini { 0 } else { 99 },
                msg_type: 103,
                size: 1234,
                sub_list: 0,
            };
            let mut b = BytesMut::new();
            h.encode(mini, &mut b);
            assert_eq!(DataHeader::decode(&b, mini).unwrap(), h);
        }
    }

    #[test]
    fn sub_message_list() {
        // body: [sub0: type 4 size 2 "ab"][list: count 1, offset 0]
        let body = [4u8, 0, 2, 0, 0, 0, b'a', b'b', 1, 0, 0, 0, 0, 0];
        let subs = sub_messages(&body, 8).unwrap();
        assert_eq!(subs, vec![(4, &b"ab"[..])]);
        assert!(sub_messages(&body, 20).is_err());
    }
}
