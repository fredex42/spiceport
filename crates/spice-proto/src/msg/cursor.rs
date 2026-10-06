//! `channel CursorChannel` messages.

use super::display::Point16;
use crate::enums::cursor_flags;
use crate::reader::{Reader, Result};

/// `SpiceCursorHeader`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorHeader {
    pub unique: u64,
    /// `SpiceCursorType`
    pub ty: u8,
    pub width: u16,
    pub height: u16,
    pub hot_spot_x: u16,
    pub hot_spot_y: u16,
}

/// `SpiceCursor`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor<'a> {
    pub flags: u16,
    /// Absent when `SPICE_CURSOR_FLAGS_NONE` is set.
    pub header: Option<CursorHeader>,
    pub data: &'a [u8],
}

impl<'a> Cursor<'a> {
    pub fn decode(r: &mut Reader<'a>) -> Result<Self> {
        let flags = r.u16()?;
        let header = if flags & cursor_flags::NONE == 0 {
            Some(CursorHeader {
                unique: r.u64()?,
                ty: r.u8()?,
                width: r.u16()?,
                height: r.u16()?,
                hot_spot_x: r.u16()?,
                hot_spot_y: r.u16()?,
            })
        } else {
            None
        };
        Ok(Self {
            flags,
            header,
            data: r.rest(),
        })
    }
}

/// `SpiceMsgCursorInit`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Init<'a> {
    pub position: Point16,
    pub trail_length: u16,
    pub trail_frequency: u16,
    pub visible: bool,
    pub cursor: Cursor<'a>,
}

impl<'a> Init<'a> {
    pub fn decode(b: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            position: Point16::decode(&mut r)?,
            trail_length: r.u16()?,
            trail_frequency: r.u16()?,
            visible: r.u8()? != 0,
            cursor: Cursor::decode(&mut r)?,
        })
    }
}

/// `SpiceMsgCursorSet`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Set<'a> {
    pub position: Point16,
    pub visible: bool,
    pub cursor: Cursor<'a>,
}

impl<'a> Set<'a> {
    pub fn decode(b: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            position: Point16::decode(&mut r)?,
            visible: r.u8()? != 0,
            cursor: Cursor::decode(&mut r)?,
        })
    }
}

/// `SpiceMsgCursorMove`
pub fn decode_move(b: &[u8]) -> Result<Point16> {
    Point16::decode(&mut Reader::new(b))
}

/// `SpiceMsgDisplayInvalOne` (cursor `INVAL_ONE`)
pub fn decode_inval_one(b: &[u8]) -> Result<u64> {
    Reader::new(b).u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_with_none_flag() {
        let b = [1u8, 0, 2, 0, 1, 1, 0];
        let s = Set::decode(&b).unwrap();
        assert_eq!(s.position, Point16 { x: 1, y: 2 });
        assert!(s.visible);
        assert!(s.cursor.header.is_none());
    }
}
