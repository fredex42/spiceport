//! `SPICE_IMAGE_TYPE_JPEG` and MJPEG stream frames (`decode-jpeg.c`).

use std::io::Cursor;

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::image::{DecodeError, Format, Image, Result};

/// Decode a JPEG to xRGB. Returns the image at its own size; callers check
/// it against the descriptor.
pub fn decode(data: &[u8]) -> Result<Image> {
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
            .chunks_exact(3)
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

    #[test]
    fn garbage_is_an_error() {
        assert!(decode(b"not a jpeg").is_err());
        assert!(decode(&[]).is_err());
    }
}
