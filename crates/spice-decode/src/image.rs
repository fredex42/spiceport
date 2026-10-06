//! The decoded image type shared by the decoders and the canvas.

use thiserror::Error;

/// Pixel layout of an [`Image`]. Every format stores one `u32` per pixel:
/// `0xAARRGGBB`, the same as pixman's little-endian `a8r8g8b8`. For `Xrgb32`
/// the top byte is undefined; for `A8` only the top byte is meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Xrgb32,
    Argb32,
    A8,
}

/// A top-down image with `width * height` pixels and no row padding.
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub format: Format,
    pub data: Vec<u32>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image({}x{} {:?})", self.width, self.height, self.format)
    }
}

/// Largest image we will allocate (in pixels): 16384 x 16384.
pub const MAX_PIXELS: u64 = 16384 * 16384;

impl Image {
    pub fn new(width: u32, height: u32, format: Format) -> Result<Self> {
        let n = width as u64 * height as u64;
        if n > MAX_PIXELS {
            return Err(DecodeError::TooLarge { width, height });
        }
        Ok(Self {
            width,
            height,
            format,
            data: vec![0; n as usize],
        })
    }

    pub fn from_data(width: u32, height: u32, format: Format, data: Vec<u32>) -> Self {
        debug_assert_eq!(data.len(), width as usize * height as usize);
        Self {
            width,
            height,
            format,
            data,
        }
    }

    pub fn row(&self, y: u32) -> &[u32] {
        let w = self.width as usize;
        &self.data[y as usize * w..(y as usize + 1) * w]
    }

    pub fn row_mut(&mut self, y: u32) -> &mut [u32] {
        let w = self.width as usize;
        &mut self.data[y as usize * w..(y as usize + 1) * w]
    }

    pub fn has_alpha(&self) -> bool {
        self.format != Format::Xrgb32
    }

    /// Reverse the row order (bottom-up → top-down).
    pub fn flip_vertical(&mut self) {
        let w = self.width as usize;
        let h = self.height as usize;
        for y in 0..h / 2 {
            let (a, b) = self.data.split_at_mut((h - 1 - y) * w);
            a[y * w..(y + 1) * w].swap_with_slice(&mut b[..w]);
        }
    }
}

/// `canvas_16bpp_to_32bpp`: expand x1r5g5b5 to x8r8g8b8 by replicating the
/// top bits, as pixman does.
#[inline]
pub fn rgb555_to_xrgb(c: u32) -> u32 {
    let mut r = ((c & 0x001f) << 3) | ((c & 0x001c) >> 2);
    r |= ((c & 0x03e0) << 6) | ((c & 0x0380) << 1);
    r |= ((c & 0x7c00) << 9) | ((c & 0x7000) << 4);
    r
}

/// Expand r5g6b5 to x8r8g8b8.
#[inline]
pub fn rgb565_to_xrgb(c: u32) -> u32 {
    let b = c & 0x1f;
    let g = (c >> 5) & 0x3f;
    let r = (c >> 11) & 0x1f;
    let r = (r << 3) | (r >> 2);
    let g = (g << 2) | (g >> 4);
    let b = (b << 3) | (b >> 2);
    (r << 16) | (g << 8) | b
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DecodeError {
    #[error("{0} data truncated")]
    Truncated(&'static str),
    #[error("image of {width}x{height} is too large")]
    TooLarge { width: u32, height: u32 },
    #[error("{what}: expected {expected}, got {got}")]
    Mismatch {
        what: &'static str,
        expected: u64,
        got: u64,
    },
    #[error("unsupported {0}")]
    Unsupported(String),
    #[error("corrupt {0} data")]
    Corrupt(&'static str),
    #[error("JPEG: {0}")]
    Jpeg(String),
    #[error("missing palette")]
    NoPalette,
}

pub type Result<T> = std::result::Result<T, DecodeError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_555() {
        assert_eq!(rgb555_to_xrgb(0x7fff), 0xffffff);
        assert_eq!(rgb555_to_xrgb(0), 0);
        assert_eq!(rgb555_to_xrgb(0x001f), 0x0000ff);
        assert_eq!(rgb555_to_xrgb(0x7c00), 0xff0000);
        assert_eq!(rgb565_to_xrgb(0xffff), 0xffffff);
        assert_eq!(rgb565_to_xrgb(0x07e0), 0x00ff00);
    }

    #[test]
    fn flip() {
        let mut i = Image::from_data(2, 3, Format::Xrgb32, vec![1, 2, 3, 4, 5, 6]);
        i.flip_vertical();
        assert_eq!(i.data, [5, 6, 3, 4, 1, 2]);
        assert!(Image::new(100_000, 100_000, Format::A8).is_err());
    }
}
