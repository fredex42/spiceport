//! Bounds-checked little-endian reader over a message body.
//!
//! Many server messages contain `@ptr32` fields: 32-bit offsets *from the
//! start of the message body* to images, palettes, clip rects and so on.
//! [`Reader::at_offset`] follows those safely.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProtoError {
    #[error("message truncated: needed {need} bytes at offset {at}, have {have}")]
    Truncated { at: usize, need: usize, have: usize },
    #[error("offset {offset} points outside the {len}-byte message")]
    BadOffset { offset: u32, len: usize },
    #[error("invalid {what}: {value}")]
    Invalid { what: &'static str, value: u64 },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ProtoError>;

#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    /// The whole message body; `@ptr32` offsets are relative to its start.
    msg: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(msg: &'a [u8]) -> Self {
        Self { msg, pos: 0 }
    }

    /// Current position from the start of the message.
    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.msg.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// The whole message this reader walks, for following further offsets.
    pub fn message(&self) -> &'a [u8] {
        self.msg
    }

    /// A reader positioned at `offset` bytes from the start of the message
    /// (an `@ptr32` field). Offset 0 is NULL and is the caller's to handle.
    pub fn at_offset(&self, offset: u32) -> Result<Reader<'a>> {
        let o = offset as usize;
        if o > self.msg.len() {
            return Err(ProtoError::BadOffset {
                offset,
                len: self.msg.len(),
            });
        }
        Ok(Reader {
            msg: self.msg,
            pos: o,
        })
    }

    /// Follow an optional pointer: `Ok(None)` for 0.
    pub fn opt_offset(&self, offset: u32) -> Result<Option<Reader<'a>>> {
        if offset == 0 {
            Ok(None)
        } else {
            self.at_offset(offset).map(Some)
        }
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(ProtoError::Truncated {
                at: self.pos,
                need: n,
                have: self.remaining(),
            });
        }
        let s = &self.msg[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.bytes(n).map(|_| ())
    }

    /// Everything from the current position to the end of the message.
    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.msg[self.pos..];
        self.pos = self.msg.len();
        s
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("length checked"))
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.array()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }
    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    /// `count` items of `size` bytes each, with overflow-safe length checks.
    pub fn array_bytes(&mut self, count: usize, size: usize) -> Result<&'a [u8]> {
        let n = count.checked_mul(size).ok_or(ProtoError::Invalid {
            what: "array length",
            value: count as u64,
        })?;
        self.bytes(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn reads_little_endian() {
        let b = [1, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 0xff];
        let mut r = Reader::new(&b);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.u32().unwrap(), 0x12345678);
        assert_eq!(r.remaining(), 1);
        assert!(matches!(r.u16(), Err(ProtoError::Truncated { .. })));
        // A failed read doesn't advance.
        assert_eq!(r.u8().unwrap(), 0xff);
        assert!(r.is_empty());
    }

    #[test]
    fn offsets_are_from_message_start() {
        let b = [0u8, 0, 0, 0, 9, 8];
        let mut r = Reader::new(&b);
        r.skip(5).unwrap();
        let mut o = r.at_offset(4).unwrap();
        assert_eq!(o.u8().unwrap(), 9);
        assert_eq!(r.at_offset(6).unwrap().remaining(), 0);
        assert!(r.at_offset(7).is_err());
        assert!(r.opt_offset(0).unwrap().is_none());
    }

    proptest! {
        #[test]
        fn never_panics(data in proptest::collection::vec(any::<u8>(), 0..64),
                        ops in proptest::collection::vec((0u8..8, any::<u32>()), 0..32)) {
            let mut r = Reader::new(&data);
            for (op, arg) in ops {
                let _ = match op {
                    0 => r.u8().map(|_| ()),
                    1 => r.u16().map(|_| ()),
                    2 => r.u32().map(|_| ()),
                    3 => r.u64().map(|_| ()),
                    4 => r.bytes(arg as usize % 80).map(|_| ()),
                    5 => r.at_offset(arg % 80).map(|n| r = n),
                    6 => r.array_bytes(arg as usize, arg as usize).map(|_| ()),
                    _ => { r.rest(); Ok(()) }
                };
                prop_assert!(r.pos() <= data.len());
            }
        }
    }
}
