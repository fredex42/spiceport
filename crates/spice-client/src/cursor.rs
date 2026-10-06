//! Cursor shape conversion (`set_cursor` / `mono_cursor` in
//! `channel-cursor.c`, `spice_mono_edge_highlight` in `spice-util.c`).

use spice_proto::enums::cursor_type;
use spice_proto::msg::cursor::CursorHeader;

/// A cursor image: `width * height` pixels of `0xAARRGGBB`, straight alpha.
#[derive(Clone, PartialEq, Eq)]
pub struct CursorShape {
    pub unique: u64,
    pub width: u32,
    pub height: u32,
    pub hot_x: u32,
    pub hot_y: u32,
    pub pixels: Vec<u32>,
}

impl std::fmt::Debug for CursorShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CursorShape({:#x} {}x{} hot {},{})",
            self.unique, self.width, self.height, self.hot_x, self.hot_y
        )
    }
}

/// Why a cursor couldn't be converted.
#[derive(Debug, PartialEq, Eq)]
pub enum CursorError {
    Truncated,
    /// A type spice-gtk doesn't implement either; show the default cursor.
    Unsupported(u8),
}

fn mask_bit(data: &[u8], offset: usize, i: usize) -> Result<bool, CursorError> {
    let b = data.get(offset + (i >> 3)).ok_or(CursorError::Truncated)?;
    Ok(b & (0x80 >> (i % 8)) != 0)
}

/// `get_pix_hack`: a checkerboard for "invert" pixels, which we can't draw.
fn pix_hack(i: usize, width: usize) -> u32 {
    if ((i % width) ^ (i / width)) & 1 != 0 {
        0xc030_3030
    } else {
        0x3050_5050
    }
}

pub fn convert(hdr: &CursorHeader, data: &[u8]) -> Result<CursorShape, CursorError> {
    let (w, h) = (hdr.width as usize, hdr.height as usize);
    let n = w * h;
    let mut px = vec![0u32; n];
    match hdr.ty {
        cursor_type::MONO => mono(w, h, data, &mut px)?,
        cursor_type::ALPHA => {
            let d = data.get(..n * 4).ok_or(CursorError::Truncated)?;
            for (p, c) in px.iter_mut().zip(d.chunks_exact(4)) {
                *p = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            }
        }
        cursor_type::COLOR32 => {
            let size = n * 4;
            let d = data.get(..size).ok_or(CursorError::Truncated)?;
            for (i, (p, c)) in px.iter_mut().zip(d.chunks_exact(4)).enumerate() {
                let v = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                let m = mask_bit(data, size, i)?;
                *p = if m && v == 0xffffff {
                    pix_hack(i, w)
                } else {
                    v | if m { 0 } else { 0xff00_0000 }
                };
            }
        }
        cursor_type::COLOR16 => {
            let size = n * 2;
            let d = data.get(..size).ok_or(CursorError::Truncated)?;
            for (i, (p, c)) in px.iter_mut().zip(d.chunks_exact(2)).enumerate() {
                let v = u16::from_le_bytes([c[0], c[1]]) as u32;
                let m = mask_bit(data, size, i)?;
                *p = if m && v == 0x7fff {
                    pix_hack(i, w)
                } else {
                    ((v & 0x1f) << 3)
                        | ((v & 0x3e0) << 6)
                        | ((v & 0x7c00) << 9)
                        | if m { 0 } else { 0xff00_0000 }
                };
            }
        }
        cursor_type::COLOR4 => {
            let size = w.div_ceil(2) * h;
            let pal_bytes = data.get(size..size + 64).ok_or(CursorError::Truncated)?;
            let pal: Vec<u32> = pal_bytes
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            for (i, p) in px.iter_mut().enumerate() {
                let b = data[i >> 1];
                let idx = if i & 1 != 0 { b & 0x0f } else { b >> 4 };
                let v = pal[idx as usize];
                let m = mask_bit(data, size + 64, i)?;
                *p = if m && v == 0xffffff {
                    pix_hack(i, w)
                } else {
                    v | if m { 0 } else { 0xff00_0000 }
                };
            }
        }
        t => return Err(CursorError::Unsupported(t)),
    }
    Ok(CursorShape {
        unique: hdr.unique,
        width: w as u32,
        height: h as u32,
        hot_x: (hdr.hot_spot_x as u32).min(w as u32),
        hot_y: (hdr.hot_spot_y as u32).min(h as u32),
        pixels: px,
    })
}

/// Mono cursors have an AND and an XOR plane; AND=1,XOR=1 means "invert",
/// which can't be drawn, so (like spice-gtk) draw those pixels white with a
/// black outline.
fn mono(w: usize, h: usize, data: &[u8], out: &mut [u32]) -> Result<(), CursorError> {
    let bpl = w.div_ceil(8);
    let and = data.get(..bpl * h).ok_or(CursorError::Truncated)?;
    let xor = data
        .get(bpl * h..2 * bpl * h)
        .ok_or(CursorError::Truncated)?;
    let and_ones = and.iter().all(|&b| b == 0xff);
    let bit = |plane: &[u8], x: usize, y: usize| plane[bpl * y + x / 8] & (0x80 >> (x % 8)) != 0;
    let is_edge = |x: usize, y: usize| {
        if x == 0 || x == w - 1 || y == 0 || y == h - 1 {
            return false;
        }
        !bit(xor, x, y)
            && (bit(xor, x - 1, y + 1)
                || bit(xor, x, y + 1)
                || bit(xor, x + 1, y + 1)
                || bit(xor, x - 1, y)
                || bit(xor, x + 1, y)
                || bit(xor, x - 1, y - 1)
                || bit(xor, x, y - 1)
                || bit(xor, x + 1, y - 1))
    };
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = if and_ones && is_edge(x, y) {
                0xff00_0000
            } else {
                match (bit(and, x, y), bit(xor, x, y)) {
                    (true, true) | (false, true) => 0xffff_ffff,
                    (true, false) => 0, // unchanged → transparent
                    (false, false) => 0xff00_0000,
                }
            };
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(ty: u8, w: u16, h: u16) -> CursorHeader {
        CursorHeader {
            unique: 1,
            ty,
            width: w,
            height: h,
            hot_spot_x: 99,
            hot_spot_y: 0,
        }
    }

    /// Ported from spice-gtk tests/util.c test_mono_edge_highlight.
    #[test]
    fn mono_edge_highlight() {
        let and = "111111111111111111111111111111111111111111111111";
        let xor = "000000000000000000000100000001000000000000000000";
        let dest_alpha = [
            "00000000", "00001110", "00001110", "00001110", "00001110", "00000000",
        ];
        let dest_white = [
            "00000000", "00000000", "00000100", "00000100", "00000000", "00000000",
        ];
        let pack = |s: &str| -> Vec<u8> {
            s.as_bytes()
                .chunks(8)
                .map(|c| c.iter().fold(0u8, |acc, &b| (acc << 1) | (b - b'0')))
                .collect()
        };
        let mut data = pack(and);
        data.extend(pack(xor));
        let c = convert(&hdr(cursor_type::MONO, 8, 6), &data).unwrap();
        assert_eq!(c.hot_x, 8, "hot spot is capped to the width");
        for y in 0..6 {
            for x in 0..8 {
                let p = c.pixels[y * 8 + x];
                let alpha = dest_alpha[y].as_bytes()[x] == b'1';
                let white = dest_white[y].as_bytes()[x] == b'1';
                assert_eq!(p >> 24 == 0xff, alpha, "alpha at {x},{y}");
                assert_eq!(p & 0xffffff == 0xffffff, white, "colour at {x},{y}");
            }
        }
    }

    #[test]
    fn color32_mask() {
        // 2x1: pixel 0 opaque red, pixel 1 masked (transparent)
        let mut data = Vec::new();
        data.extend(0x00ff0000u32.to_le_bytes());
        data.extend(0x00000000u32.to_le_bytes());
        data.push(0b0100_0000);
        let c = convert(&hdr(cursor_type::COLOR32, 2, 1), &data).unwrap();
        assert_eq!(c.pixels, [0xffff0000, 0x00000000]);
        assert_eq!(
            convert(&hdr(cursor_type::COLOR32, 2, 1), &data[..8]),
            Err(CursorError::Truncated)
        );
        assert_eq!(
            convert(&hdr(cursor_type::COLOR8, 2, 1), &data),
            Err(CursorError::Unsupported(cursor_type::COLOR8))
        );
    }
}
