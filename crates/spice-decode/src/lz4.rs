//! `SPICE_IMAGE_TYPE_LZ4` (`canvas_get_lz4` in spice-common `canvas_base.c`).
//!
//! Layout: `u8 top_down`, `u8 SpiceBitmapFmt`, then LZ4 blocks each preceded
//! by a big-endian u32 compressed size. Blocks are a *stream*: each one may
//! reference the output of the blocks before it.

use crate::image::{DecodeError, Format, Image, Result, rgb555_to_xrgb};

const FMT_16BIT: u8 = 6;
const FMT_24BIT: u8 = 7;
const FMT_32BIT: u8 = 8;
const FMT_RGBA: u8 = 9;

/// LZ4's maximum match distance; older output can't be referenced.
const LZ4_WINDOW: usize = 64 * 1024;

pub fn decode(data: &[u8], width: u32, height: u32) -> Result<Image> {
    if data.len() < 2 {
        return Err(DecodeError::Truncated("LZ4 header"));
    }
    let top_down = data[0] != 0;
    let fmt = data[1];
    let (bpp, out_fmt) = match fmt {
        FMT_16BIT => (2, Format::Xrgb32),
        FMT_24BIT => (3, Format::Xrgb32),
        FMT_32BIT => (4, Format::Xrgb32),
        FMT_RGBA => (4, Format::Argb32),
        _ => return Err(DecodeError::Unsupported(format!("LZ4 bitmap format {fmt}"))),
    };
    let mut img = Image::new(width, height, out_fmt)?;
    let row = width as usize * bpp;
    let total = row * height as usize;
    let mut raw = vec![0u8; total];
    let mut pos = 0usize;
    let mut d = &data[2..];
    while !d.is_empty() {
        if d.len() < 4 {
            return Err(DecodeError::Truncated("LZ4 block header"));
        }
        let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) as usize;
        d = &d[4..];
        if n > d.len() {
            return Err(DecodeError::Truncated("LZ4 block"));
        }
        let (prev, out) = raw.split_at_mut(pos);
        let dict = &prev[pos.saturating_sub(LZ4_WINDOW)..];
        let got = lz4_flex::block::decompress_into_with_dict(&d[..n], out, dict)
            .map_err(|_| DecodeError::Corrupt("LZ4"))?;
        if got == 0 {
            return Err(DecodeError::Corrupt("LZ4"));
        }
        pos += got;
        d = &d[n..];
    }
    if pos != total {
        return Err(DecodeError::Mismatch {
            what: "LZ4 decoded size",
            expected: total as u64,
            got: pos as u64,
        });
    }
    let w = width as usize;
    for y in 0..height as usize {
        let src = &raw[y * row..(y + 1) * row];
        let dy = if top_down { y } else { height as usize - 1 - y };
        let dst = &mut img.data[dy * w..(dy + 1) * w];
        match bpp {
            2 => {
                for (o, s) in dst.iter_mut().zip(src.as_chunks::<2>().0.iter()) {
                    *o = rgb555_to_xrgb(u16::from_le_bytes([s[0], s[1]]) as u32);
                }
            }
            3 => {
                // PIXMAN_LE_r8g8b8: bytes are b, g, r
                for (o, s) in dst.iter_mut().zip(src.as_chunks::<3>().0.iter()) {
                    *o = (s[2] as u32) << 16 | (s[1] as u32) << 8 | s[0] as u32;
                }
            }
            _ => {
                for (o, s) in dst.iter_mut().zip(src.as_chunks::<4>().0.iter()) {
                    *o = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                }
            }
        }
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(raw: &[u8], fmt: u8, top_down: bool, block: usize) -> Vec<u8> {
        // Independent blocks are a valid (if suboptimal) LZ4 stream.
        let mut out = vec![top_down as u8, fmt];
        for chunk in raw.chunks(block) {
            let c = lz4_flex::block::compress(chunk);
            out.extend_from_slice(&(c.len() as u32).to_be_bytes());
            out.extend_from_slice(&c);
        }
        out
    }

    #[test]
    fn round_trip_32bit() {
        let px: Vec<u32> = (0..64 * 8).map(|i| i * 0x010203).collect();
        let raw: Vec<u8> = px.iter().flat_map(|p| p.to_le_bytes()).collect();
        let img = decode(&encode(&raw, FMT_32BIT, true, 300), 64, 8).unwrap();
        assert_eq!(img.data, px);
        let img = decode(&encode(&raw, FMT_32BIT, false, 4096), 64, 8).unwrap();
        assert_eq!(img.row(7), &px[..64]);
    }

    #[test]
    fn stream_references_previous_block() {
        // Second block is pure back-references into the first one's output,
        // which is how spice-server's LZ4 stream encoder behaves.
        let raw = [7u8; 64];
        let mut enc = vec![1u8, FMT_32BIT];
        let b1 = lz4_flex::block::compress(&raw[..32]);
        enc.extend_from_slice(&(b1.len() as u32).to_be_bytes());
        enc.extend_from_slice(&b1);
        // The second block is compressed against the first one's output.
        let b2 = lz4_flex::block::compress_with_dict(&raw[32..], &raw[..32]);
        enc.extend_from_slice(&(b2.len() as u32).to_be_bytes());
        enc.extend_from_slice(&b2);
        let img = decode(&enc, 4, 4).unwrap();
        assert!(img.data.iter().all(|&p| p == 0x0707_0707));
    }

    #[test]
    fn errors() {
        assert!(decode(&[], 1, 1).is_err());
        assert!(decode(&[1, 99], 1, 1).is_err());
        assert!(decode(&[1, FMT_32BIT, 0, 0, 0, 9, 1], 1, 1).is_err());
        let raw = [0u8; 16];
        assert!(decode(&encode(&raw, FMT_32BIT, true, 16), 2, 1).is_err());
    }

    proptest::proptest! {
        #[test]
        fn never_panics(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..200),
                        w in 0u32..20, h in 0u32..20) {
            let _ = decode(&data, w, h);
        }
    }
}
