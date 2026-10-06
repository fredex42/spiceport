//! SPICE LZ (`SPICE_IMAGE_TYPE_LZ_RGB` / `LZ_PLT`), a FastLZ derivative
//! working in pixel units. Port of the decoding half of spice-common
//! `lz.c` and `lz_decompress_tmpl.c`.

use crate::image::{DecodeError, Format, Image, Result, rgb555_to_xrgb};

/// ASCII "LZ  "
pub const LZ_MAGIC: u32 = 0x2020_5a4c;
/// `LZ_VERSION` 1.1
pub const LZ_VERSION: u32 = 0x0001_0001;
pub const MAX_COPY: u32 = 32;
/// Distances above this use the 16-bit "far" encoding.
const MAX_DISTANCE: u32 = 8191;
/// `SPICE_MAX_IMAGE_SIZE`
const MAX_IMAGE_SIZE: u64 = 512 * 1024 * 1024 - 1;

/// `LzImageType`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum LzType {
    Plt1Le = 1,
    Plt1Be = 2,
    Plt4Le = 3,
    Plt4Be = 4,
    Plt8 = 5,
    Rgb16 = 6,
    Rgb24 = 7,
    Rgb32 = 8,
    Rgba = 9,
    Xxxa = 10,
    A8 = 11,
}

impl LzType {
    pub fn from_u32(v: u32) -> Option<Self> {
        use LzType::*;
        Some(match v {
            1 => Plt1Le,
            2 => Plt1Be,
            3 => Plt4Le,
            4 => Plt4Be,
            5 => Plt8,
            6 => Rgb16,
            7 => Rgb24,
            8 => Rgb32,
            9 => Rgba,
            10 => Xxxa,
            11 => A8,
            _ => return None,
        })
    }

    pub fn is_plt(self) -> bool {
        (self as u32) <= LzType::Plt8 as u32
    }

    /// `PLT_PIXELS_PER_BYTE`
    pub fn plt_pixels_per_byte(self) -> u32 {
        match self {
            LzType::Plt1Le | LzType::Plt1Be => 8,
            LzType::Plt4Le | LzType::Plt4Be => 2,
            _ => 1,
        }
    }

    /// `RGB_BYTES_PER_PIXEL`
    pub fn rgb_bytes_per_pixel(self) -> u32 {
        match self {
            LzType::Rgb16 => 2,
            LzType::Rgb24 => 3,
            LzType::Rgb32 | LzType::Rgba | LzType::Xxxa => 4,
            _ => 1,
        }
    }
}

/// How one literal unit of the compressed stream becomes output pixels.
#[derive(Clone, Copy)]
pub(crate) enum Unit<'p> {
    /// b, g, r bytes → xRGB (`LZ_RGB32`, also used for RGB24).
    Rgb32,
    /// Two bytes of x1r5g5b5, high byte first → xRGB.
    Rgb16,
    /// One byte into the alpha channel only (`LZ_RGB_ALPHA`).
    Alpha,
    /// One byte of alpha with zero colour (`LZ_A8` to an A8 image).
    A8,
    /// Palette indices (`TO_RGB32` variants).
    Plt(LzType, &'p [u32]),
}

impl Unit<'_> {
    /// Output pixels per compressed unit (`CAST_PLT_DISTANCE`).
    pub(crate) fn scale(&self) -> u32 {
        match self {
            Unit::Plt(t, _) => t.plt_pixels_per_byte(),
            _ => 1,
        }
    }

    /// The bias added to the encoded match length (2 + 1 for byte-sized
    /// pixels, 1 + 1 for RGB16, 1 for RGB32), after LZ's `len--`.
    pub(crate) fn len_bias(&self) -> u32 {
        match self {
            Unit::Plt(..) | Unit::Alpha | Unit::A8 => 3,
            Unit::Rgb16 => 2,
            Unit::Rgb32 => 1,
        }
    }

    /// Whether copies move only the alpha byte.
    pub(crate) fn alpha_only(&self) -> bool {
        matches!(self, Unit::Alpha)
    }

    /// `COPY_COMP_PIXEL`: read one literal unit and write its pixels.
    #[inline]
    pub(crate) fn literal(
        &self,
        inp: &mut Input<'_>,
        out: &mut [u32],
        op: &mut usize,
    ) -> Result<()> {
        let plt_ent = |pal: &[u32], i: usize| pal.get(i).copied().unwrap_or(0) & 0x00ff_ffff;
        match *self {
            Unit::Rgb32 => {
                let b = inp.byte()? as u32;
                let g = inp.byte()? as u32;
                let r = inp.byte()? as u32;
                *slot(out, *op)? = r << 16 | g << 8 | b;
                *op += 1;
            }
            Unit::Rgb16 => {
                let hi = inp.byte()? as u32;
                let lo = inp.byte()? as u32;
                *slot(out, *op)? = rgb555_to_xrgb(hi << 8 | lo);
                *op += 1;
            }
            Unit::Alpha => {
                let a = inp.byte()? as u32;
                let p = slot(out, *op)?;
                *p = (*p & 0x00ff_ffff) | a << 24;
                *op += 1;
            }
            Unit::A8 => {
                *slot(out, *op)? = (inp.byte()? as u32) << 24;
                *op += 1;
            }
            Unit::Plt(t, pal) => {
                let byte = inp.byte()?;
                match t {
                    LzType::Plt8 => {
                        *slot(out, *op)? = plt_ent(pal, byte as usize);
                        *op += 1;
                    }
                    LzType::Plt4Be | LzType::Plt4Le => {
                        let n = pal.len().max(1);
                        let (first, second) = if t == LzType::Plt4Be {
                            (byte >> 4, byte & 0x0f)
                        } else {
                            (byte & 0x0f, byte >> 4)
                        };
                        *slot(out, *op)? = plt_ent(pal, first as usize % n);
                        *slot(out, *op + 1)? = plt_ent(pal, second as usize % n);
                        *op += 2;
                    }
                    _ => {
                        let (fore, back) = (plt_ent(pal, 1), plt_ent(pal, 0));
                        for k in 0..8 {
                            let bit = if t == LzType::Plt1Be { 7 - k } else { k };
                            *slot(out, *op + k)? = if (byte >> bit) & 1 != 0 { fore } else { back };
                        }
                        *op += 8;
                    }
                }
            }
        }
        Ok(())
    }

    /// Copy `len` pixels from `src` (or the alpha byte only).
    #[inline]
    pub(crate) fn copy_pixel(&self, from: u32, to: &mut u32) {
        if self.alpha_only() {
            *to = (*to & 0x00ff_ffff) | (from & 0xff00_0000);
        } else {
            *to = from;
        }
    }
}

#[inline]
fn slot(out: &mut [u32], i: usize) -> Result<&mut u32> {
    out.get_mut(i)
        .ok_or(DecodeError::Corrupt("LZ (output overflow)"))
}

/// A bounds-checked byte cursor.
pub(crate) struct Input<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) pos: usize,
}

impl<'a> Input<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[inline]
    pub(crate) fn byte(&mut self) -> Result<u8> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or(DecodeError::Truncated("LZ"))?;
        self.pos += 1;
        Ok(b)
    }

    /// Big-endian u32 (`decode_32`).
    pub(crate) fn u32(&mut self) -> Result<u32> {
        let mut w = 0u32;
        for _ in 0..4 {
            w = w << 8 | self.byte()? as u32;
        }
        Ok(w)
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok((self.u32()? as u64) << 32 | self.u32()? as u64)
    }
}

/// `FNAME(decompress)` from `lz_decompress_tmpl.c`: fill `out` completely.
pub(crate) fn decompress(inp: &mut Input<'_>, out: &mut [u32], unit: Unit<'_>) -> Result<()> {
    let limit = out.len();
    let mut op = 0usize;
    let scale = unit.scale() as usize;
    while op < limit {
        let ctrl = inp.byte()? as u32;
        if ctrl >= MAX_COPY {
            let mut len = (ctrl >> 5) - 1;
            let mut ofs = (ctrl & 31) << 8;
            if len == 7 - 1 {
                loop {
                    let code = inp.byte()?;
                    len += code as u32;
                    if code != 255 {
                        break;
                    }
                }
            }
            let code = inp.byte()? as u32;
            ofs += code;
            if code == 255 && ofs - code == 31 << 8 {
                ofs = (inp.byte()? as u32) << 8;
                ofs += inp.byte()? as u32;
                ofs += MAX_DISTANCE;
            }
            len += unit.len_bias();
            ofs += 1;
            let (ofs, len) = (ofs as usize * scale, len as usize * scale);
            if ofs > op || op + len > limit {
                return Err(DecodeError::Corrupt("LZ (bad reference)"));
            }
            // Overlapping copies (runs) must go pixel by pixel.
            for r in (op - ofs..).take(len) {
                let v = out[r];
                unit.copy_pixel(v, &mut out[op]);
                op += 1;
            }
        } else {
            let n = ctrl + 1;
            if op + n as usize * scale > limit {
                return Err(DecodeError::Corrupt("LZ (literal overflow)"));
            }
            for _ in 0..n {
                unit.literal(inp, out, &mut op)?;
            }
        }
    }
    Ok(())
}

/// Parsed `lz_decode_begin` header.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Header {
    pub ty: LzType,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub top_down: bool,
}

impl Header {
    /// Pixels the stream decodes to, including palette row padding.
    pub(crate) fn gross_pixels(&self) -> u64 {
        if self.ty.is_plt() {
            self.stride as u64 * self.ty.plt_pixels_per_byte() as u64 * self.height as u64
        } else {
            self.width as u64 * self.height as u64
        }
    }
}

/// `lz_set_sizes` checks.
pub(crate) fn check_sizes(ty: LzType, width: u32, height: u32, stride: u32) -> Result<()> {
    if ty.is_plt() {
        let ppb = ty.plt_pixels_per_byte();
        if stride > width / ppb && (width.is_multiple_of(ppb) || stride - width / ppb > 1) {
            return Err(DecodeError::Corrupt("LZ (palette stride)"));
        }
    } else if stride as u64 != width as u64 * ty.rgb_bytes_per_pixel() as u64 {
        return Err(DecodeError::Corrupt("LZ (stride)"));
    }
    if stride as u64 * height as u64 > MAX_IMAGE_SIZE
        || width as u64 * height as u64 > crate::image::MAX_PIXELS
    {
        return Err(DecodeError::TooLarge { width, height });
    }
    if width == 0 || height == 0 {
        return Err(DecodeError::Corrupt("LZ (empty image)"));
    }
    Ok(())
}

pub(crate) fn read_header(inp: &mut Input<'_>) -> Result<Header> {
    if inp.u32()? != LZ_MAGIC {
        return Err(DecodeError::Corrupt("LZ (bad magic)"));
    }
    if inp.u32()? != LZ_VERSION {
        return Err(DecodeError::Unsupported("LZ version".into()));
    }
    let ty = LzType::from_u32(inp.u32()?).ok_or(DecodeError::Corrupt("LZ (image type)"))?;
    let width = inp.u32()?;
    let height = inp.u32()?;
    let stride = inp.u32()?;
    check_sizes(ty, width, height, stride)?;
    let top_down = inp.u32()? != 0;
    Ok(Header {
        ty,
        width,
        height,
        stride,
        top_down,
    })
}

/// Turn decode-order pixels (rows of `gross / height` pixels, bottom-up
/// unless `top_down`) into a top-down image of `width` pixels per row.
pub(crate) fn to_image(
    raw: Vec<u32>,
    width: u32,
    height: u32,
    top_down: bool,
    format: Format,
) -> Image {
    let pitch = raw.len() / height as usize;
    if pitch == width as usize && top_down {
        return Image::from_data(width, height, format, raw);
    }
    let mut data = Vec::with_capacity(width as usize * height as usize);
    for y in 0..height as usize {
        let sy = if top_down { y } else { height as usize - 1 - y };
        data.extend_from_slice(&raw[sy * pitch..sy * pitch + width as usize]);
    }
    Image::from_data(width, height, format, data)
}

/// Decode an LZ_RGB or LZ_PLT image (`canvas_get_lz` + `lz_decode`).
/// `palette` entries must already be 32-bit for paletted images.
pub fn decode(data: &[u8], palette: Option<&[u32]>) -> Result<Image> {
    let mut inp = Input::new(data);
    let h = read_header(&mut inp)?;
    let gross = h.gross_pixels() as usize;
    let mut raw = vec![0u32; gross];
    let format = match h.ty {
        LzType::Rgba => Format::Argb32,
        LzType::A8 => Format::A8,
        LzType::Xxxa => {
            return Err(DecodeError::Unsupported(
                "LZ XXXA outside JPEG_ALPHA".into(),
            ));
        }
        _ => Format::Xrgb32,
    };
    match h.ty {
        t if t.is_plt() => {
            let pal = palette.ok_or(DecodeError::NoPalette)?;
            decompress(&mut inp, &mut raw, Unit::Plt(t, pal))?;
        }
        LzType::Rgb16 => decompress(&mut inp, &mut raw, Unit::Rgb16)?,
        LzType::Rgb24 | LzType::Rgb32 => decompress(&mut inp, &mut raw, Unit::Rgb32)?,
        LzType::Rgba => {
            decompress(&mut inp, &mut raw, Unit::Rgb32)?;
            decompress(&mut inp, &mut raw, Unit::Alpha)?;
        }
        LzType::A8 => decompress(&mut inp, &mut raw, Unit::A8)?,
        _ => unreachable!(),
    }
    Ok(to_image(raw, h.width, h.height, h.top_down, format))
}

/// Decode the `LZ_IMAGE_TYPE_XXXA` alpha plane of a JPEG_ALPHA image into
/// the alpha bytes of `img`.
pub fn decode_alpha_into(data: &[u8], img: &mut Image, top_down: bool) -> Result<()> {
    let mut inp = Input::new(data);
    let h = read_header(&mut inp)?;
    if h.ty != LzType::Xxxa {
        return Err(DecodeError::Corrupt("JPEG_ALPHA (alpha type)"));
    }
    if h.top_down != top_down || h.width != img.width || h.height != img.height {
        return Err(DecodeError::Corrupt("JPEG_ALPHA (alpha geometry)"));
    }
    if top_down {
        decompress(&mut inp, &mut img.data, Unit::Alpha)
    } else {
        // The alpha plane is stored bottom-up.
        img.flip_vertical();
        let r = decompress(&mut inp, &mut img.data, Unit::Alpha);
        img.flip_vertical();
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(ty: LzType, w: u32, h: u32, stride: u32, top_down: bool) -> Vec<u8> {
        let mut v = Vec::new();
        for x in [
            LZ_MAGIC,
            LZ_VERSION,
            ty as u32,
            w,
            h,
            stride,
            top_down as u32,
        ] {
            v.extend_from_slice(&x.to_be_bytes());
        }
        v
    }

    #[test]
    fn literals_and_run() {
        // 4 pixels RGB32: one literal, then a run of 3 copies of it.
        let mut d = header(LzType::Rgb32, 4, 1, 16, true);
        d.extend_from_slice(&[0, 3, 2, 1]); // ctrl 0: 1 literal, b g r
        // reference: len field 2 → len = (2-1)+1 = 2?  Use len bits = 3 → (3-1)+1 = 3
        d.extend_from_slice(&[3 << 5, 0]); // ofs = 0 + 1 = 1
        let img = decode(&d, None).unwrap();
        assert_eq!(img.data, [0x010203; 4]);
    }

    #[test]
    fn bottom_up_and_palette() {
        let mut d = header(LzType::Plt8, 2, 2, 2, false);
        d.extend_from_slice(&[3, 0, 1, 1, 0]); // 4 literal palette indices
        let pal = [0x111111, 0x222222];
        let img = decode(&d, Some(&pal)).unwrap();
        // stream rows are bottom-up
        assert_eq!(img.data, [0x222222, 0x111111, 0x111111, 0x222222]);
        assert_eq!(decode(&d, None), Err(DecodeError::NoPalette));
    }

    #[test]
    fn bad_references_fail() {
        let mut d = header(LzType::Rgb32, 4, 1, 16, true);
        d.extend_from_slice(&[0, 3, 2, 1]);
        d.extend_from_slice(&[3 << 5, 5]); // offset 6 > 1 pixel decoded
        assert!(decode(&d, None).is_err());
        let d = header(LzType::Rgb32, 4, 1, 15, true);
        assert!(decode(&d, None).is_err());
    }

    /// JPEG_ALPHA = a JPEG plus an LZ XXXA alpha plane (`canvas_get_jpeg_alpha`).
    #[test]
    fn jpeg_alpha_plane() {
        let (w, h) = (4u32, 2u32);
        let rgb = vec![200u8; (w * h * 3) as usize];
        let jpeg = crate::jpeg::tests::encode_rgb(w as u16, h as u16, &rgb);
        for top_down in [true, false] {
            let mut alpha = header(LzType::Xxxa, w, h, w * 4, top_down);
            alpha.push((w * h - 1) as u8); // one literal run of 8 bytes
            alpha.extend(0..(w * h) as u8);
            let mut img = crate::jpeg::decode(&jpeg).unwrap();
            decode_alpha_into(&alpha, &mut img, top_down).unwrap();
            let a: Vec<u32> = img.data.iter().map(|p| p >> 24).collect();
            if top_down {
                assert_eq!(a, [0, 1, 2, 3, 4, 5, 6, 7]);
            } else {
                assert_eq!(a, [4, 5, 6, 7, 0, 1, 2, 3]);
            }
            assert!(
                img.data.iter().all(|p| (p >> 16) & 0xff > 180),
                "colour kept"
            );
            // A mismatched orientation flag is rejected, like the C asserts.
            let mut img = crate::jpeg::decode(&jpeg).unwrap();
            assert!(decode_alpha_into(&alpha, &mut img, !top_down).is_err());
        }
    }

    proptest::proptest! {
        #[test]
        fn never_panics(body in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..300),
                        ty in 1u32..12, w in 1u32..20, h in 1u32..20, td: bool) {
            let t = LzType::from_u32(ty).unwrap();
            let stride = if t.is_plt() { w.div_ceil(t.plt_pixels_per_byte()) } else { w * t.rgb_bytes_per_pixel() };
            let mut d = header(t, w, h, stride, td);
            d.extend(body);
            let _ = decode(&d, Some(&[1, 2, 3]));
        }
    }
}
