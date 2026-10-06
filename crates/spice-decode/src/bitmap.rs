//! Uncompressed `SPICE_IMAGE_TYPE_BITMAP` conversion
//! (`spice_bitmap_to_pixman` and friends in spice-common `pixman_utils.c`).

use crate::image::{DecodeError, Format, Image, Result, rgb555_to_xrgb};

// SpiceBitmapFmt
const FMT_1BIT_LE: u8 = 1;
const FMT_1BIT_BE: u8 = 2;
const FMT_4BIT_LE: u8 = 3;
const FMT_4BIT_BE: u8 = 4;
const FMT_8BIT: u8 = 5;
const FMT_16BIT: u8 = 6;
const FMT_24BIT: u8 = 7;
const FMT_32BIT: u8 = 8;
const FMT_RGBA: u8 = 9;
const FMT_8BIT_A: u8 = 10;
/// `SPICE_BITMAP_FLAGS_TOP_DOWN`
const TOP_DOWN: u8 = 1 << 2;

fn bits_per_pixel(fmt: u8) -> Option<u32> {
    Some(match fmt {
        FMT_1BIT_LE | FMT_1BIT_BE => 1,
        FMT_4BIT_LE | FMT_4BIT_BE => 4,
        FMT_8BIT | FMT_8BIT_A => 8,
        FMT_16BIT => 16,
        FMT_24BIT => 24,
        FMT_32BIT | FMT_RGBA => 32,
        _ => return None,
    })
}

/// Iterate over the source rows of a bitmap in top-down order.
fn rows<'a>(
    data: &'a [u8],
    width: u32,
    height: u32,
    stride: u32,
    fmt: u8,
    flags: u8,
) -> Result<impl Iterator<Item = &'a [u8]> + 'a> {
    let bpp = bits_per_pixel(fmt)
        .ok_or_else(|| DecodeError::Unsupported(format!("bitmap format {fmt}")))?;
    let row_bytes = (width as u64 * bpp as u64).div_ceil(8);
    if (stride as u64) < row_bytes {
        return Err(DecodeError::Corrupt("bitmap stride"));
    }
    if (data.len() as u64) < stride as u64 * height as u64 {
        return Err(DecodeError::Truncated("bitmap"));
    }
    let stride = stride as usize;
    let row_bytes = row_bytes as usize;
    let top_down = flags & TOP_DOWN != 0;
    let h = height as usize;
    Ok((0..h).map(move |y| {
        let sy = if top_down { y } else { h - 1 - y };
        &data[sy * stride..sy * stride + row_bytes]
    }))
}

/// Convert a bitmap to an [`Image`]. `palette` entries must already be in
/// 32-bit xRGB form (expand 16-bit palettes first).
pub fn decode(
    fmt: u8,
    flags: u8,
    width: u32,
    height: u32,
    stride: u32,
    data: &[u8],
    palette: Option<&[u32]>,
) -> Result<Image> {
    let out_fmt = match fmt {
        FMT_RGBA => Format::Argb32,
        FMT_8BIT_A => Format::A8,
        FMT_1BIT_LE | FMT_4BIT_LE => {
            return Err(DecodeError::Unsupported(format!("bitmap format {fmt}")));
        }
        _ => Format::Xrgb32,
    };
    let mut img = Image::new(width, height, out_fmt)?;
    let w = width as usize;
    let pal = |i: usize| -> Result<u32> {
        let p = palette.ok_or(DecodeError::NoPalette)?;
        Ok(p.get(i).copied().unwrap_or(0))
    };
    // Look palette entries up once per image rather than per pixel.
    let lut: Vec<u32> = match fmt {
        FMT_8BIT => (0..256).map(pal).collect::<Result<_>>()?,
        FMT_4BIT_BE => (0..16).map(pal).collect::<Result<_>>()?,
        FMT_1BIT_BE => (0..2).map(pal).collect::<Result<_>>()?,
        _ => Vec::new(),
    };
    for (y, src) in rows(data, width, height, stride, fmt, flags)?.enumerate() {
        let dst = &mut img.data[y * w..(y + 1) * w];
        match fmt {
            FMT_32BIT | FMT_RGBA => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(4)) {
                    *d = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                }
            }
            FMT_24BIT => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(3)) {
                    // bgr byte order
                    *d = (s[2] as u32) << 16 | (s[1] as u32) << 8 | s[0] as u32;
                }
            }
            FMT_16BIT => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(2)) {
                    *d = rgb555_to_xrgb(u16::from_le_bytes([s[0], s[1]]) as u32);
                }
            }
            FMT_8BIT_A => {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = (*s as u32) << 24;
                }
            }
            FMT_8BIT => {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = lut[*s as usize];
                }
            }
            FMT_4BIT_BE => {
                for (x, d) in dst.iter_mut().enumerate() {
                    let b = src[x / 2];
                    let i = if x % 2 == 0 { b >> 4 } else { b & 0xf };
                    *d = lut[i as usize];
                }
            }
            FMT_1BIT_BE => {
                for (x, d) in dst.iter_mut().enumerate() {
                    let bit = (src[x / 8] >> (7 - (x % 8))) & 1;
                    *d = lut[bit as usize];
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(img)
}

/// A 1-bpp bitmap used as a mask (`canvas_get_bitmap_mask`). The result is
/// an `A8` image whose pixels are fully opaque where the bit is set (or
/// clear, with `invert`).
pub fn decode_mask(
    fmt: u8,
    flags: u8,
    width: u32,
    height: u32,
    stride: u32,
    data: &[u8],
    invert: bool,
) -> Result<Image> {
    let msb_first = match fmt {
        FMT_1BIT_LE => false,
        FMT_1BIT_BE => true,
        _ => {
            return Err(DecodeError::Unsupported(format!(
                "mask bitmap format {fmt}"
            )));
        }
    };
    let mut img = Image::new(width, height, Format::A8)?;
    let w = width as usize;
    for (y, src) in rows(data, width, height, stride, fmt, flags)?.enumerate() {
        for x in 0..w {
            let b = src[x / 8];
            let bit = if msb_first {
                (b >> (7 - (x % 8))) & 1
            } else {
                (b >> (x % 8)) & 1
            };
            let on = (bit != 0) != invert;
            img.data[y * w + x] = if on { 0xff00_0000 } else { 0 };
        }
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_up_24bit() {
        // 2x2, stride 8 (6 bytes used), bottom-up.
        let data = [
            1, 2, 3, 4, 5, 6, 0, 0, // bottom row
            7, 8, 9, 10, 11, 12, 0, 0, // top row
        ];
        let img = decode(FMT_24BIT, 0, 2, 2, 8, &data, None).unwrap();
        assert_eq!(img.data, [0x090807, 0x0c0b0a, 0x030201, 0x060504]);
    }

    #[test]
    fn paletted() {
        let pal = [0x111111, 0x222222, 0x333333];
        let img = decode(FMT_8BIT, TOP_DOWN, 3, 1, 4, &[2, 0, 9, 0], Some(&pal)).unwrap();
        assert_eq!(img.data, [0x333333, 0x111111, 0]);
        let img = decode(FMT_4BIT_BE, TOP_DOWN, 3, 1, 2, &[0x21, 0x00], Some(&pal)).unwrap();
        assert_eq!(img.data, [0x333333, 0x222222, 0x111111]);
        let img = decode(FMT_1BIT_BE, TOP_DOWN, 3, 1, 1, &[0b1010_0000], Some(&pal)).unwrap();
        assert_eq!(img.data, [0x222222, 0x111111, 0x222222]);
        assert_eq!(
            decode(FMT_8BIT, TOP_DOWN, 1, 1, 1, &[0], None),
            Err(DecodeError::NoPalette)
        );
    }

    #[test]
    fn masks() {
        let m = decode_mask(FMT_1BIT_LE, TOP_DOWN, 3, 1, 1, &[0b0000_0101], false).unwrap();
        assert_eq!(m.data, [0xff00_0000, 0, 0xff00_0000]);
        let m = decode_mask(FMT_1BIT_BE, TOP_DOWN, 3, 1, 1, &[0b1000_0000], true).unwrap();
        assert_eq!(m.data, [0, 0xff00_0000, 0xff00_0000]);
    }

    #[test]
    fn rejects_short_data() {
        assert!(decode(FMT_32BIT, 0, 2, 2, 8, &[0; 15], None).is_err());
        assert!(decode(FMT_32BIT, 0, 2, 2, 4, &[0; 16], None).is_err());
        assert!(decode(0, 0, 2, 2, 8, &[0; 16], None).is_err());
    }
}
