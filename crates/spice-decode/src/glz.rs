//! GLZ: LZ whose matches may reference pixels of *earlier images*, through a
//! dictionary window shared by every display channel of a session. Port of
//! spice-gtk `decode-glz.c` / `decode-glz-tmpl.c`, plus `decode-zlib.c` for
//! `ZLIB_GLZ_RGB`.
//!
//! Images from different display channels can arrive out of order, so a
//! decode may need an image that hasn't been decoded yet. spice-gtk parks the
//! channel's coroutine until it shows up; here the decoding thread waits on a
//! condition variable, with a timeout instead of a deadlock.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use log::{debug, warn};

use crate::image::{DecodeError, Format, Image, Result};
use crate::lz::{self, Input, LzType, MAX_COPY, Unit};

/// How long to wait for an image another channel hasn't delivered yet.
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

struct WindowImage {
    win_head_dist: u32,
    /// Decode-order pixels (`gross_pixels` of them).
    pixels: Arc<Vec<u32>>,
}

#[derive(Default)]
struct WindowInner {
    images: HashMap<u64, WindowImage>,
    /// Images with id below this have been released.
    oldest: u64,
    /// The lowest id not yet received (`tail_gap`).
    tail_gap: u64,
}

/// `SpiceGlzDecoderWindow`, shared by all display channels of a session.
#[derive(Default)]
pub struct GlzWindow {
    inner: Mutex<WindowInner>,
    added: Condvar,
}

impl GlzWindow {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// `glz_decoder_window_clear`
    pub fn clear(&self) {
        let mut w = self.inner.lock().unwrap();
        *w = WindowInner::default();
    }

    fn get(&self, id: u64, timeout: Duration) -> Option<Arc<Vec<u32>>> {
        let deadline = Instant::now() + timeout;
        let mut w = self.inner.lock().unwrap();
        loop {
            if let Some(img) = w.images.get(&id) {
                return Some(img.pixels.clone());
            }
            if id < w.oldest {
                return None;
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            w = self.added.wait_timeout(w, deadline - now).unwrap().0;
        }
    }

    /// `glz_decoder_window_add` + `glz_decoder_window_release`.
    fn add(&self, id: u64, win_head_dist: u32, pixels: Arc<Vec<u32>>) {
        let mut w = self.inner.lock().unwrap();
        w.images.insert(
            id,
            WindowImage {
                win_head_dist,
                pixels,
            },
        );
        while w.images.contains_key(&w.tail_gap) {
            w.tail_gap += 1;
        }
        // Release images older than the last contiguous image's window.
        if let Some(last) = w.tail_gap.checked_sub(1).and_then(|t| w.images.get(&t)) {
            let oldest = (w.tail_gap - 1).saturating_sub(last.win_head_dist as u64);
            if oldest > w.oldest {
                let old = w.oldest;
                w.images.retain(|&k, _| k >= oldest || k < old);
                w.oldest = oldest;
            }
        }
        drop(w);
        self.added.notify_all();
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `decode_header` (glz): note the type byte also carries `top_down`.
struct GlzHeader {
    ty: LzType,
    top_down: bool,
    width: u32,
    height: u32,
    gross: usize,
    id: u64,
    win_head_dist: u32,
}

fn read_header(inp: &mut Input<'_>) -> Result<GlzHeader> {
    if inp.u32()? != lz::LZ_MAGIC {
        return Err(DecodeError::Corrupt("GLZ (bad magic)"));
    }
    if inp.u32()? != lz::LZ_VERSION {
        return Err(DecodeError::Unsupported("GLZ version".into()));
    }
    let tmp = inp.byte()?;
    let ty =
        LzType::from_u32((tmp & 0x0f) as u32).ok_or(DecodeError::Corrupt("GLZ (image type)"))?;
    let top_down = tmp >> 4 != 0;
    let width = inp.u32()?;
    let height = inp.u32()?;
    let stride = inp.u32()?;
    lz::check_sizes(ty, width, height, stride)?;
    let gross = if ty.is_plt() {
        stride as u64 * ty.plt_pixels_per_byte() as u64 * height as u64
    } else {
        width as u64 * height as u64
    } as usize;
    let id = inp.u64()?;
    let win_head_dist = inp.u32()?;
    Ok(GlzHeader {
        ty,
        top_down,
        width,
        height,
        gross,
        id,
        win_head_dist,
    })
}

/// `FNAME(decode)` from `decode-glz-tmpl.c`.
fn decompress(
    window: &GlzWindow,
    inp: &mut Input<'_>,
    out: &mut [u32],
    image_id: u64,
    unit: Unit<'_>,
    refs: &mut HashMap<u64, Arc<Vec<u32>>>,
) -> Result<()> {
    let limit = out.len();
    let scale = unit.scale() as usize;
    // GLZ has no `len--`, so its biases are one less than LZ's.
    let bias = unit.len_bias() - 1;
    let mut op = 0usize;
    while op < limit {
        let ctrl = inp.byte()? as u32;
        if ctrl >= MAX_COPY {
            let mut len = ctrl >> 5;
            let mut pixel_flag = (ctrl >> 4) & 1;
            let mut pixel_ofs = ctrl & 0x0f;
            if len == 7 {
                loop {
                    let code = inp.byte()?;
                    len += code as u32;
                    if code != 255 {
                        break;
                    }
                }
            }
            pixel_ofs += (inp.byte()? as u32) << 4;
            let code = inp.byte()? as u32;
            let image_flag = (code >> 6) & 3;
            let mut image_dist: u64;
            if pixel_flag == 0 {
                image_dist = (code & 0x3f) as u64;
                for i in 0..image_flag {
                    image_dist += (inp.byte()? as u64) << (6 + 8 * i);
                }
            } else {
                pixel_flag = (code >> 5) & 1;
                pixel_ofs += (code & 0x1f) << 12;
                image_dist = 0;
                for i in 0..image_flag {
                    image_dist += (inp.byte()? as u64) << (8 * i);
                }
                if pixel_flag != 0 {
                    pixel_ofs += (inp.byte()? as u32) << 17;
                }
            }
            len += bias;
            if image_dist == 0 {
                pixel_ofs += 1;
            }
            let (pixel_ofs, len) = (pixel_ofs as usize * scale, len as usize * scale);
            if op + len > limit {
                return Err(DecodeError::Corrupt("GLZ (match overflow)"));
            }
            if image_dist == 0 {
                if pixel_ofs > op {
                    return Err(DecodeError::Corrupt("GLZ (bad reference)"));
                }
                // Overlapping copies (runs) must go pixel by pixel.
                for r in (op - pixel_ofs..).take(len) {
                    let v = out[r];
                    unit.copy_pixel(v, &mut out[op]);
                    op += 1;
                }
            } else {
                let ref_id = image_id
                    .checked_sub(image_dist)
                    .ok_or(DecodeError::Corrupt("GLZ (image distance)"))?;
                let src = match refs.get(&ref_id) {
                    Some(s) => s.clone(),
                    None => {
                        let s = window.get(ref_id, WAIT_TIMEOUT).ok_or_else(|| {
                            warn!("GLZ: image {ref_id} needed by {image_id} never arrived");
                            DecodeError::Corrupt("GLZ (missing reference image)")
                        })?;
                        refs.insert(ref_id, s.clone());
                        s
                    }
                };
                let from = src
                    .get(pixel_ofs..pixel_ofs + len)
                    .ok_or(DecodeError::Corrupt("GLZ (reference outside image)"))?;
                for &v in from {
                    unit.copy_pixel(v, &mut out[op]);
                    op += 1;
                }
            }
        } else {
            let n = ctrl + 1;
            if op + n as usize * scale > limit {
                return Err(DecodeError::Corrupt("GLZ (literal overflow)"));
            }
            for _ in 0..n {
                unit.literal(inp, out, &mut op)?;
            }
        }
    }
    Ok(())
}

/// Decode a GLZ_RGB image, add it to the window, and return it.
/// `palette` entries must already be 32-bit for paletted images.
pub fn decode(window: &GlzWindow, data: &[u8], palette: Option<&[u32]>) -> Result<Image> {
    let mut inp = Input::new(data);
    let h = read_header(&mut inp)?;
    debug!(
        "GLZ {}x{} id {} ref {}",
        h.width,
        h.height,
        h.id,
        h.id.wrapping_sub(h.win_head_dist as u64)
    );
    let mut raw = vec![0u32; h.gross];
    let mut refs = HashMap::new();
    let unit = match h.ty {
        t if t.is_plt() => Unit::Plt(t, palette.ok_or(DecodeError::NoPalette)?),
        LzType::Rgb16 => Unit::Rgb16,
        LzType::Rgb24 | LzType::Rgb32 | LzType::Rgba => Unit::Rgb32,
        t => return Err(DecodeError::Unsupported(format!("GLZ image type {t:?}"))),
    };
    decompress(window, &mut inp, &mut raw, h.id, unit, &mut refs)?;
    let format = if h.ty == LzType::Rgba {
        decompress(window, &mut inp, &mut raw, h.id, Unit::Alpha, &mut refs)?;
        Format::Argb32
    } else {
        Format::Xrgb32
    };
    let raw = Arc::new(raw);
    window.add(h.id, h.win_head_dist, raw.clone());
    let raw = Arc::try_unwrap(raw).unwrap_or_else(|a| (*a).clone());
    Ok(lz::to_image(raw, h.width, h.height, h.top_down, format))
}

/// `SPICE_IMAGE_TYPE_ZLIB_GLZ_RGB`: zlib-deflated GLZ data.
pub fn decode_zlib(
    window: &GlzWindow,
    data: &[u8],
    glz_data_size: u32,
    palette: Option<&[u32]>,
) -> Result<Image> {
    if glz_data_size as u64 > 512 * 1024 * 1024 {
        return Err(DecodeError::Corrupt("ZLIB_GLZ (size)"));
    }
    let mut glz = Vec::with_capacity(glz_data_size as usize);
    flate2::read::ZlibDecoder::new(data)
        .take(glz_data_size as u64)
        .read_to_end(&mut glz)
        .map_err(|_| DecodeError::Corrupt("ZLIB_GLZ (inflate)"))?;
    if glz.len() != glz_data_size as usize {
        return Err(DecodeError::Truncated("ZLIB_GLZ"));
    }
    decode(window, &glz, palette)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(ty: LzType, w: u32, h: u32, id: u64, dist: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&lz::LZ_MAGIC.to_be_bytes());
        v.extend_from_slice(&lz::LZ_VERSION.to_be_bytes());
        v.push(ty as u8 | 1 << 4); // top-down
        for x in [w, h, w * ty.rgb_bytes_per_pixel()] {
            v.extend_from_slice(&x.to_be_bytes());
        }
        v.extend_from_slice(&id.to_be_bytes());
        v.extend_from_slice(&dist.to_be_bytes());
        v
    }

    #[test]
    fn cross_image_reference() {
        let win = GlzWindow::new();
        // Image 0: two literal pixels.
        let mut a = header(LzType::Rgb32, 2, 1, 0, 0);
        a.extend_from_slice(&[1, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let img = decode(&win, &a, None).unwrap();
        assert_eq!(img.data, [0x332211, 0x665544]);
        // Image 1: a 2-pixel match at offset 0 of image 0 (short form,
        // image_dist 1). RGB32 has no length bias in GLZ.
        let mut b = header(LzType::Rgb32, 2, 1, 1, 1);
        // ctrl: len=2 (bits 7..5), pixel_flag 0, pixel_ofs low 4 bits 0
        b.extend_from_slice(&[2 << 5, 0, 1]); // ofs high 0, code: image_flag 0, dist 1
        let img = decode(&win, &b, None).unwrap();
        assert_eq!(img.data, [0x332211, 0x665544]);
        // Image 1's window still reaches back to image 0.
        assert_eq!(win.len(), 2);
        // Image 2 has a window of just itself: older images are released.
        let mut c = header(LzType::Rgb32, 1, 1, 2, 0);
        c.extend_from_slice(&[0, 1, 2, 3]);
        decode(&win, &c, None).unwrap();
        assert_eq!(win.len(), 1);
    }

    #[test]
    fn missing_reference_times_out() {
        let win = GlzWindow::new();
        win.add(0, 0, Arc::new(vec![0]));
        assert!(win.get(5, Duration::from_millis(10)).is_none());
    }

    #[test]
    fn waits_for_other_channel() {
        let win = GlzWindow::new();
        let w2 = win.clone();
        let t = std::thread::spawn(move || w2.get(3, Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(50));
        win.add(3, 0, Arc::new(vec![7]));
        assert_eq!(t.join().unwrap().unwrap()[0], 7);
    }
}
