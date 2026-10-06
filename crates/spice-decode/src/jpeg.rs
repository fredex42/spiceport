//! `SPICE_IMAGE_TYPE_JPEG` and MJPEG stream frames (`decode-jpeg.c`).

use std::io::Cursor;

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::image::{DecodeError, Format, Image, Result};

/// Whether the JPEG has a DHT (Huffman table) segment before its first scan.
fn has_huffman_tables(data: &[u8]) -> bool {
    let mut i = 2;
    while i + 4 <= data.len() {
        if data[i] != 0xff {
            return false;
        }
        let marker = data[i + 1];
        match marker {
            0xc4 => return true,
            0xda | 0xd9 => return false, // SOS / EOI
            0xd0..=0xd7 | 0x01 | 0xff => i += 2,
            _ => i += 2 + u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize,
        }
    }
    false
}

/// MJPEG frames (and spice-server's) may leave out the Huffman tables and
/// rely on the standard ones from JPEG Annex K, which libjpeg fills in.
/// zune-jpeg does too, but only when it sees an "AVI1" APP0 segment, so
/// insert one when the frame has no tables of its own.
fn with_mjpeg_marker(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 4 || data[0] != 0xff || data[1] != 0xd8 || has_huffman_tables(data) {
        return None;
    }
    let mut v = Vec::with_capacity(data.len() + 9);
    v.extend_from_slice(&data[..2]);
    v.extend_from_slice(&[0xff, 0xe0, 0x00, 0x07, b'A', b'V', b'I', b'1', 0]);
    v.extend_from_slice(&data[2..]);
    Some(v)
}

/// Decode a JPEG to xRGB. Returns the image at its own size; callers check
/// it against the descriptor.
pub fn decode(data: &[u8]) -> Result<Image> {
    let patched = with_mjpeg_marker(data);
    let data = patched.as_deref().unwrap_or(data);
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(16384)
        .set_max_height(16384);
    let mut d = JpegDecoder::new_with_options(Cursor::new(data), opts);
    let rgb = d.decode().map_err(|e| DecodeError::Jpeg(e.to_string()))?;
    let info = d
        .info()
        .ok_or_else(|| DecodeError::Jpeg("no image info".into()))?;
    let (w, h) = (info.width as u32, info.height as u32);
    let px = w as usize * h as usize;
    let out_cs = d.output_colorspace().unwrap_or(ColorSpace::RGB);
    let data = match out_cs.num_components() {
        3 if rgb.len() >= px * 3 => rgb
            .as_chunks::<3>()
            .0
            .iter()
            .take(px)
            .map(|c| (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32)
            .collect(),
        1 if rgb.len() >= px => rgb[..px]
            .iter()
            .map(|&g| (g as u32) << 16 | (g as u32) << 8 | g as u32)
            .collect(),
        n => {
            return Err(DecodeError::Jpeg(format!(
                "unexpected output: {n} components, {} bytes",
                rgb.len()
            )));
        }
    };
    Ok(Image::from_data(w, h, Format::Xrgb32, data))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn encode_rgb(w: u16, h: u16, rgb: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let enc = jpeg_encoder::Encoder::new(&mut out, 100);
        enc.encode(rgb, w, h, jpeg_encoder::ColorType::Rgb).unwrap();
        out
    }

    #[test]
    fn decodes_colours() {
        let (w, h) = (16u16, 8u16);
        let mut rgb = Vec::new();
        for _y in 0..h {
            for x in 0..w {
                if x < 8 {
                    rgb.extend_from_slice(&[250, 10, 10]);
                } else {
                    rgb.extend_from_slice(&[10, 10, 250]);
                }
            }
        }
        let img = decode(&encode_rgb(w, h, &rgb)).unwrap();
        assert_eq!((img.width, img.height), (16, 8));
        let red = img.data[2];
        let blue = img.data[13];
        assert!((red >> 16) & 0xff > 200 && red & 0xff < 50, "{red:06x}");
        assert!(blue & 0xff > 200 && (blue >> 16) & 0xff < 50, "{blue:06x}");
    }

    /// Strip the DHT segments from a JPEG, as spice-server's MJPEG frames
    /// come: decoding must fall back to the standard tables.
    #[test]
    fn decodes_without_huffman_tables() {
        let (w, h) = (16u16, 16u16);
        let rgb: Vec<u8> = (0..w as usize * h as usize)
            .flat_map(|i| [(i * 7) as u8, 128, 30])
            .collect();
        let full = encode_rgb(w, h, &rgb);
        assert!(has_huffman_tables(&full));
        let mut stripped = full[..2].to_vec();
        let mut i = 2;
        while i < full.len() {
            let marker = full[i + 1];
            if marker == 0xda {
                stripped.extend_from_slice(&full[i..]);
                break;
            }
            let len = u16::from_be_bytes([full[i + 2], full[i + 3]]) as usize;
            if marker != 0xc4 {
                stripped.extend_from_slice(&full[i..i + 2 + len]);
            }
            i += 2 + len;
        }
        assert!(!has_huffman_tables(&stripped));
        // jpeg-encoder uses the standard tables, so the result must match.
        assert_eq!(decode(&stripped).unwrap(), decode(&full).unwrap());
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(decode(b"not a jpeg").is_err());
        assert!(decode(&[]).is_err());
    }
}
