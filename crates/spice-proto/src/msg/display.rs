//! `channel DisplayChannel` messages.
//!
//! Draw messages reference images, palettes, masks, brushes, paths and
//! strings through `@ptr32` offsets from the start of the message body. The
//! parsed structures borrow pixel data from the message.

use bytes::{BufMut, BytesMut};

use super::ClientMsg;
use super::base::WaitForChannels;
use crate::enums::{
    bitmap_flags, brush_type, clip_type, composite_flags, image_type, line_flags, msgc,
    string_flags,
};
use crate::reader::{ProtoError, Reader, Result};

/// `SpiceRect`. On the wire the order is top, left, bottom, right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let top = r.i32()?;
        let left = r.i32()?;
        let bottom = r.i32()?;
        let right = r.i32()?;
        Ok(Self {
            left,
            top,
            right,
            bottom,
        })
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}

/// `SpicePoint`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            x: r.i32()?,
            y: r.i32()?,
        })
    }
}

/// `SpicePoint16`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point16 {
    pub x: i16,
    pub y: i16,
}

impl Point16 {
    pub fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            x: r.i16()?,
            y: r.i16()?,
        })
    }
}

/// `SpicePointFix`: 28.4 fixed point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PointFix {
    pub x: i32,
    pub y: i32,
}

/// `SpiceClip`
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Clip {
    #[default]
    None,
    Rects(Vec<Rect>),
}

impl Clip {
    pub fn decode(r: &mut Reader<'_>) -> Result<Self> {
        match r.u8()? {
            clip_type::NONE => Ok(Clip::None),
            clip_type::RECTS => {
                let n = r.u32()?;
                // Each rect is 16 bytes; check before allocating.
                if (n as usize).saturating_mul(16) > r.remaining() {
                    return Err(ProtoError::Truncated {
                        at: r.pos(),
                        need: n as usize * 16,
                        have: r.remaining(),
                    });
                }
                let mut v = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    v.push(Rect::decode(r)?);
                }
                Ok(Clip::Rects(v))
            }
            t => Err(ProtoError::Invalid {
                what: "clip type",
                value: t as u64,
            }),
        }
    }
}

/// `SpiceMsgDisplayBase`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayBase {
    pub surface_id: u32,
    pub bbox: Rect,
    pub clip: Clip,
}

impl DisplayBase {
    pub fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            surface_id: r.u32()?,
            bbox: Rect::decode(r)?,
            clip: Clip::decode(r)?,
        })
    }
}

/// `SpiceImageDescriptor`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDescriptor {
    pub id: u64,
    pub ty: u8,
    pub flags: u8,
    pub width: u32,
    pub height: u32,
}

impl ImageDescriptor {
    /// `spice_image_descriptor_is_lossy`
    pub fn is_lossy(&self) -> bool {
        self.ty == image_type::JPEG || self.ty == image_type::JPEG_ALPHA
    }
}

/// `SpicePalette`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub unique: u64,
    pub ents: Vec<u32>,
}

impl Palette {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let unique = r.u64()?;
        let n = r.u16()?;
        let raw = r.array_bytes(n as usize, 4)?;
        Ok(Self {
            unique,
            ents: raw
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect(),
        })
    }
}

/// How a paletted image gets its palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteRef {
    None,
    /// `SPICE_BITMAP_FLAGS_PAL_FROM_CACHE`
    FromCache(u64),
    Inline(Palette),
}

fn decode_palette_ref(r: &mut Reader<'_>, flags: u8) -> Result<PaletteRef> {
    if flags & bitmap_flags::PAL_FROM_CACHE != 0 {
        Ok(PaletteRef::FromCache(r.u64()?))
    } else {
        let off = r.u32()?;
        match r.opt_offset(off)? {
            None => Ok(PaletteRef::None),
            Some(mut p) => Ok(PaletteRef::Inline(Palette::decode(&mut p)?)),
        }
    }
}

/// `SpiceBitmap`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap<'a> {
    /// `SpiceBitmapFmt`
    pub format: u8,
    pub flags: u8,
    pub x: u32,
    pub y: u32,
    pub stride: u32,
    pub palette: PaletteRef,
    pub data: &'a [u8],
}

/// The type-specific part of a `SpiceImage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageData<'a> {
    Bitmap(Bitmap<'a>),
    Quic(&'a [u8]),
    LzRgb(&'a [u8]),
    GlzRgb(&'a [u8]),
    Jpeg(&'a [u8]),
    Lz4(&'a [u8]),
    LzPlt {
        flags: u8,
        palette: PaletteRef,
        data: &'a [u8],
    },
    ZlibGlzRgb {
        glz_data_size: u32,
        data: &'a [u8],
    },
    JpegAlpha {
        flags: u8,
        jpeg_size: u32,
        data: &'a [u8],
    },
    Surface(u32),
    FromCache,
    FromCacheLossless,
}

/// `SpiceImage`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image<'a> {
    pub descriptor: ImageDescriptor,
    pub data: ImageData<'a>,
}

fn binary<'a>(r: &mut Reader<'a>) -> Result<&'a [u8]> {
    let n = r.u32()?;
    r.bytes(n as usize)
}

impl<'a> Image<'a> {
    pub fn decode(r: &mut Reader<'a>) -> Result<Self> {
        let descriptor = ImageDescriptor {
            id: r.u64()?,
            ty: r.u8()?,
            flags: r.u8()?,
            width: r.u32()?,
            height: r.u32()?,
        };
        let data = match descriptor.ty {
            image_type::BITMAP => {
                let format = r.u8()?;
                let flags = r.u8()?;
                let x = r.u32()?;
                let y = r.u32()?;
                let stride = r.u32()?;
                let palette = decode_palette_ref(r, flags)?;
                let data = r.array_bytes(stride as usize, y as usize)?;
                ImageData::Bitmap(Bitmap {
                    format,
                    flags,
                    x,
                    y,
                    stride,
                    palette,
                    data,
                })
            }
            image_type::QUIC => ImageData::Quic(binary(r)?),
            image_type::LZ_RGB => ImageData::LzRgb(binary(r)?),
            image_type::GLZ_RGB => ImageData::GlzRgb(binary(r)?),
            image_type::JPEG => ImageData::Jpeg(binary(r)?),
            image_type::LZ4 => ImageData::Lz4(binary(r)?),
            image_type::LZ_PLT => {
                let flags = r.u8()?;
                let size = r.u32()?;
                let palette = decode_palette_ref(r, flags)?;
                ImageData::LzPlt {
                    flags,
                    palette,
                    data: r.bytes(size as usize)?,
                }
            }
            image_type::ZLIB_GLZ_RGB => {
                let glz_data_size = r.u32()?;
                ImageData::ZlibGlzRgb {
                    glz_data_size,
                    data: binary(r)?,
                }
            }
            image_type::JPEG_ALPHA => {
                let flags = r.u8()?;
                let jpeg_size = r.u32()?;
                ImageData::JpegAlpha {
                    flags,
                    jpeg_size,
                    data: binary(r)?,
                }
            }
            image_type::SURFACE => ImageData::Surface(r.u32()?),
            image_type::FROM_CACHE => ImageData::FromCache,
            image_type::FROM_CACHE_LOSSLESS => ImageData::FromCacheLossless,
            t => {
                return Err(ProtoError::Invalid {
                    what: "image type",
                    value: t as u64,
                });
            }
        };
        Ok(Self { descriptor, data })
    }

    /// Follow an `Image *` field: `None` for a NULL pointer.
    pub fn decode_ptr(r: &mut Reader<'a>) -> Result<Option<Self>> {
        let off = r.u32()?;
        match r.opt_offset(off)? {
            None => Ok(None),
            Some(mut ir) => Ok(Some(Image::decode(&mut ir)?)),
        }
    }

    fn decode_ptr_nonnull(r: &mut Reader<'a>) -> Result<Self> {
        Self::decode_ptr(r)?.ok_or(ProtoError::Invalid {
            what: "null image pointer",
            value: 0,
        })
    }
}

/// `SpiceBrush`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Brush<'a> {
    None,
    Solid(u32),
    Pattern { image: Image<'a>, pos: Point },
}

impl<'a> Brush<'a> {
    pub fn decode(r: &mut Reader<'a>) -> Result<Self> {
        match r.u8()? {
            brush_type::NONE => Ok(Brush::None),
            brush_type::SOLID => Ok(Brush::Solid(r.u32()?)),
            brush_type::PATTERN => {
                let image = Image::decode_ptr_nonnull(r)?;
                let pos = Point::decode(r)?;
                Ok(Brush::Pattern { image, pos })
            }
            t => Err(ProtoError::Invalid {
                what: "brush type",
                value: t as u64,
            }),
        }
    }
}

/// `SpiceQMask`
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QMask<'a> {
    pub flags: u8,
    pub pos: Point,
    pub bitmap: Option<Image<'a>>,
}

impl<'a> QMask<'a> {
    pub fn decode(r: &mut Reader<'a>) -> Result<Self> {
        Ok(Self {
            flags: r.u8()?,
            pos: Point::decode(r)?,
            bitmap: Image::decode_ptr(r)?,
        })
    }
}

/// `SpiceFill`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fill<'a> {
    pub brush: Brush<'a>,
    pub rop_descriptor: u16,
    pub mask: QMask<'a>,
}

/// `SpiceOpaque`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opaque<'a> {
    pub src_bitmap: Option<Image<'a>>,
    pub src_area: Rect,
    pub brush: Brush<'a>,
    pub rop_descriptor: u16,
    pub scale_mode: u8,
    pub mask: QMask<'a>,
}

/// `SpiceCopy` (also used by `DRAW_BLEND`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copy<'a> {
    pub src_bitmap: Option<Image<'a>>,
    pub src_area: Rect,
    pub rop_descriptor: u16,
    pub scale_mode: u8,
    pub mask: QMask<'a>,
}

/// `SpiceRop3`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rop3<'a> {
    pub src_bitmap: Option<Image<'a>>,
    pub src_area: Rect,
    pub brush: Brush<'a>,
    pub rop3: u8,
    pub scale_mode: u8,
    pub mask: QMask<'a>,
}

/// `SpicePathSeg`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSeg {
    pub flags: u8,
    pub points: Vec<PointFix>,
}

/// `SpiceLineAttr`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineAttr {
    pub flags: u8,
    /// Fixed 28.4 dash lengths when `STYLED`.
    pub style: Vec<i32>,
}

/// `SpiceStroke`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stroke<'a> {
    pub path: Vec<PathSeg>,
    pub attr: LineAttr,
    pub brush: Brush<'a>,
    pub fore_mode: u16,
    pub back_mode: u16,
}

/// `SpiceRasterGlyph`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterGlyph<'a> {
    pub render_pos: Point,
    pub glyph_origin: Point,
    pub width: u16,
    pub height: u16,
    pub data: &'a [u8],
}

/// `SpiceString`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiceString<'a> {
    pub flags: u8,
    pub glyphs: Vec<RasterGlyph<'a>>,
}

/// `SpiceText`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text<'a> {
    pub str: SpiceString<'a>,
    pub back_area: Rect,
    pub fore_brush: Brush<'a>,
    pub back_brush: Brush<'a>,
    pub fore_mode: u16,
    pub back_mode: u16,
}

/// `SpiceTransparent`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transparent<'a> {
    pub src_bitmap: Option<Image<'a>>,
    pub src_area: Rect,
    pub src_color: u32,
    pub true_color: u32,
}

/// `SpiceAlphaBlend`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlphaBlend<'a> {
    pub alpha_flags: u8,
    pub alpha: u8,
    pub src_bitmap: Option<Image<'a>>,
    pub src_area: Rect,
}

/// `SpiceTransform`: a 2x3 matrix of 16.16 fixed point values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Transform {
    pub t: [u32; 6],
}

/// `SpiceComposite`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composite<'a> {
    pub flags: u32,
    pub src_bitmap: Image<'a>,
    pub mask_bitmap: Option<Image<'a>>,
    pub src_transform: Option<Transform>,
    pub mask_transform: Option<Transform>,
    pub src_origin: Point16,
    pub mask_origin: Point16,
}

/// A parsed draw message: `DisplayBase` + the op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrawOp<'a> {
    Fill(Fill<'a>),
    Opaque(Opaque<'a>),
    Copy(Copy<'a>),
    Blend(Copy<'a>),
    Blackness(QMask<'a>),
    Whiteness(QMask<'a>),
    Invers(QMask<'a>),
    Rop3(Rop3<'a>),
    Stroke(Stroke<'a>),
    Text(Text<'a>),
    Transparent(Transparent<'a>),
    AlphaBlend(AlphaBlend<'a>),
    Composite(Composite<'a>),
}

impl DrawOp<'_> {
    /// Every image the op references (sources, brushes, masks).
    pub fn images(&self) -> Vec<&Image<'_>> {
        let mut v: Vec<&Image<'_>> = Vec::new();
        match self {
            DrawOp::Fill(f) => {
                if let Brush::Pattern { image, .. } = &f.brush {
                    v.push(image);
                }
                v.extend(f.mask.bitmap.as_ref());
            }
            DrawOp::Opaque(o) => {
                v.extend(o.src_bitmap.as_ref());
                if let Brush::Pattern { image, .. } = &o.brush {
                    v.push(image);
                }
                v.extend(o.mask.bitmap.as_ref());
            }
            DrawOp::Copy(c) | DrawOp::Blend(c) => {
                v.extend(c.src_bitmap.as_ref());
                v.extend(c.mask.bitmap.as_ref());
            }
            DrawOp::Blackness(m) | DrawOp::Whiteness(m) | DrawOp::Invers(m) => {
                v.extend(m.bitmap.as_ref())
            }
            DrawOp::Rop3(r) => {
                v.extend(r.src_bitmap.as_ref());
                if let Brush::Pattern { image, .. } = &r.brush {
                    v.push(image);
                }
                v.extend(r.mask.bitmap.as_ref());
            }
            DrawOp::Stroke(s) => {
                if let Brush::Pattern { image, .. } = &s.brush {
                    v.push(image);
                }
            }
            DrawOp::Text(t) => {
                for b in [&t.fore_brush, &t.back_brush] {
                    if let Brush::Pattern { image, .. } = b {
                        v.push(image);
                    }
                }
            }
            DrawOp::Transparent(t) => v.extend(t.src_bitmap.as_ref()),
            DrawOp::AlphaBlend(a) => v.extend(a.src_bitmap.as_ref()),
            DrawOp::Composite(c) => {
                v.push(&c.src_bitmap);
                v.extend(c.mask_bitmap.as_ref());
            }
        }
        v
    }

    /// Whether decoding this op may need to wait for another channel's GLZ
    /// images.
    pub fn uses_glz(&self) -> bool {
        self.images().iter().any(|i| {
            matches!(
                i.descriptor.ty,
                image_type::GLZ_RGB | image_type::ZLIB_GLZ_RGB
            )
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            DrawOp::Fill(_) => "fill",
            DrawOp::Opaque(_) => "opaque",
            DrawOp::Copy(_) => "copy",
            DrawOp::Blend(_) => "blend",
            DrawOp::Blackness(_) => "blackness",
            DrawOp::Whiteness(_) => "whiteness",
            DrawOp::Invers(_) => "invers",
            DrawOp::Rop3(_) => "rop3",
            DrawOp::Stroke(_) => "stroke",
            DrawOp::Text(_) => "text",
            DrawOp::Transparent(_) => "transparent",
            DrawOp::AlphaBlend(_) => "alpha_blend",
            DrawOp::Composite(_) => "composite",
        }
    }
}

fn decode_path(r: &mut Reader<'_>) -> Result<Vec<PathSeg>> {
    let n = r.u32()?;
    if (n as usize).saturating_mul(4) > r.remaining() {
        return Err(ProtoError::Invalid {
            what: "path segment count",
            value: n as u64,
        });
    }
    let mut segs = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let off = r.u32()?;
        let mut s = r.at_offset(off)?;
        let flags = s.u8()?;
        let count = s.u32()?;
        let raw = s.array_bytes(count as usize, 8)?;
        let points = raw
            .chunks_exact(8)
            .map(|c| PointFix {
                x: i32::from_le_bytes(c[0..4].try_into().unwrap()),
                y: i32::from_le_bytes(c[4..8].try_into().unwrap()),
            })
            .collect();
        segs.push(PathSeg { flags, points });
    }
    Ok(segs)
}

fn decode_line_attr(r: &mut Reader<'_>) -> Result<LineAttr> {
    let flags = r.u8()?;
    let mut style = Vec::new();
    if flags & line_flags::STYLED != 0 {
        let nseg = r.u8()?;
        let off = r.u32()?;
        if let Some(mut s) = r.opt_offset(off)? {
            for _ in 0..nseg {
                style.push(s.i32()?);
            }
        }
    }
    Ok(LineAttr { flags, style })
}

fn decode_string<'a>(r: &mut Reader<'a>) -> Result<SpiceString<'a>> {
    let length = r.u16()?;
    let flags = r.u8()?;
    let bpp = if flags & string_flags::RASTER_A1 != 0 {
        1
    } else if flags & string_flags::RASTER_A4 != 0 {
        4
    } else if flags & string_flags::RASTER_A8 != 0 {
        8
    } else {
        return Err(ProtoError::Invalid {
            what: "string flags",
            value: flags as u64,
        });
    };
    let mut glyphs = Vec::with_capacity(length as usize);
    for _ in 0..length {
        let off = r.u32()?;
        let mut g = r.at_offset(off)?;
        let render_pos = Point::decode(&mut g)?;
        let glyph_origin = Point::decode(&mut g)?;
        let width = g.u16()?;
        let height = g.u16()?;
        let row = (width as usize * bpp).div_ceil(8);
        let data = g.array_bytes(row, height as usize)?;
        glyphs.push(RasterGlyph {
            render_pos,
            glyph_origin,
            width,
            height,
            data,
        });
    }
    Ok(SpiceString { flags, glyphs })
}

fn decode_transform(r: &mut Reader<'_>) -> Result<Transform> {
    let mut t = [0u32; 6];
    for v in &mut t {
        *v = r.u32()?;
    }
    Ok(Transform { t })
}

/// Decode a `SPICE_MSG_DISPLAY_DRAW_*` message.
pub fn decode_draw(msg_type: u16, body: &[u8]) -> Result<(DisplayBase, DrawOp<'_>)> {
    use crate::enums::msg::display as d;
    let mut r = Reader::new(body);
    let base = DisplayBase::decode(&mut r)?;
    let op = match msg_type {
        d::DRAW_FILL => DrawOp::Fill(Fill {
            brush: Brush::decode(&mut r)?,
            rop_descriptor: r.u16()?,
            mask: QMask::decode(&mut r)?,
        }),
        d::DRAW_OPAQUE => DrawOp::Opaque(Opaque {
            src_bitmap: Image::decode_ptr(&mut r)?,
            src_area: Rect::decode(&mut r)?,
            brush: Brush::decode(&mut r)?,
            rop_descriptor: r.u16()?,
            scale_mode: r.u8()?,
            mask: QMask::decode(&mut r)?,
        }),
        d::DRAW_COPY | d::DRAW_BLEND => {
            let c = Copy {
                src_bitmap: Image::decode_ptr(&mut r)?,
                src_area: Rect::decode(&mut r)?,
                rop_descriptor: r.u16()?,
                scale_mode: r.u8()?,
                mask: QMask::decode(&mut r)?,
            };
            if msg_type == d::DRAW_COPY {
                DrawOp::Copy(c)
            } else {
                DrawOp::Blend(c)
            }
        }
        d::DRAW_BLACKNESS => DrawOp::Blackness(QMask::decode(&mut r)?),
        d::DRAW_WHITENESS => DrawOp::Whiteness(QMask::decode(&mut r)?),
        d::DRAW_INVERS => DrawOp::Invers(QMask::decode(&mut r)?),
        d::DRAW_ROP3 => DrawOp::Rop3(Rop3 {
            src_bitmap: Image::decode_ptr(&mut r)?,
            src_area: Rect::decode(&mut r)?,
            brush: Brush::decode(&mut r)?,
            rop3: r.u8()?,
            scale_mode: r.u8()?,
            mask: QMask::decode(&mut r)?,
        }),
        d::DRAW_STROKE => {
            let off = r.u32()?;
            let mut p = r.at_offset(off)?;
            let path = decode_path(&mut p)?;
            DrawOp::Stroke(Stroke {
                path,
                attr: decode_line_attr(&mut r)?,
                brush: Brush::decode(&mut r)?,
                fore_mode: r.u16()?,
                back_mode: r.u16()?,
            })
        }
        d::DRAW_TEXT => {
            let off = r.u32()?;
            let mut s = r.at_offset(off)?;
            let str = decode_string(&mut s)?;
            DrawOp::Text(Text {
                str,
                back_area: Rect::decode(&mut r)?,
                fore_brush: Brush::decode(&mut r)?,
                back_brush: Brush::decode(&mut r)?,
                fore_mode: r.u16()?,
                back_mode: r.u16()?,
            })
        }
        d::DRAW_TRANSPARENT => DrawOp::Transparent(Transparent {
            src_bitmap: Image::decode_ptr(&mut r)?,
            src_area: Rect::decode(&mut r)?,
            src_color: r.u32()?,
            true_color: r.u32()?,
        }),
        d::DRAW_ALPHA_BLEND => DrawOp::AlphaBlend(AlphaBlend {
            alpha_flags: r.u8()?,
            alpha: r.u8()?,
            src_bitmap: Image::decode_ptr(&mut r)?,
            src_area: Rect::decode(&mut r)?,
        }),
        d::DRAW_COMPOSITE => {
            let flags = r.u32()?;
            let src_bitmap = Image::decode_ptr_nonnull(&mut r)?;
            let mask_bitmap = if flags & composite_flags::HAS_MASK != 0 {
                Image::decode_ptr(&mut r)?
            } else {
                None
            };
            let src_transform = if flags & composite_flags::HAS_SRC_TRANSFORM != 0 {
                Some(decode_transform(&mut r)?)
            } else {
                None
            };
            let mask_transform = if flags & composite_flags::HAS_MASK_TRANSFORM != 0 {
                Some(decode_transform(&mut r)?)
            } else {
                None
            };
            DrawOp::Composite(Composite {
                flags,
                src_bitmap,
                mask_bitmap,
                src_transform,
                mask_transform,
                src_origin: Point16::decode(&mut r)?,
                mask_origin: Point16::decode(&mut r)?,
            })
        }
        t => {
            return Err(ProtoError::Invalid {
                what: "draw message type",
                value: t as u64,
            });
        }
    };
    Ok((base, op))
}

/// `SpiceMsgDisplayMode`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    pub x_res: u32,
    pub y_res: u32,
    pub bits: u32,
}

impl Mode {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            x_res: r.u32()?,
            y_res: r.u32()?,
            bits: r.u32()?,
        })
    }
}

/// `SpiceMsgDisplayCopyBits`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyBits {
    pub base: DisplayBase,
    pub src_pos: Point,
}

impl CopyBits {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            base: DisplayBase::decode(&mut r)?,
            src_pos: Point::decode(&mut r)?,
        })
    }
}

/// `SpiceResourceList` entries: (`SpiceResourceType`, id).
pub fn decode_inval_list(b: &[u8]) -> Result<Vec<(u8, u64)>> {
    let mut r = Reader::new(b);
    let n = r.u16()?;
    let mut v = Vec::with_capacity(n as usize);
    for _ in 0..n {
        v.push((r.u8()?, r.u64()?));
    }
    Ok(v)
}

/// `SPICE_MSG_DISPLAY_INVAL_ALL_PIXMAPS`
pub fn decode_inval_all_pixmaps(b: &[u8]) -> Result<WaitForChannels> {
    WaitForChannels::decode(b)
}

/// `SpiceMsgDisplayInvalOne`
pub fn decode_inval_one(b: &[u8]) -> Result<u64> {
    Reader::new(b).u64()
}

/// `SpiceMsgSurfaceCreate`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceCreate {
    pub surface_id: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub flags: u32,
}

impl SurfaceCreate {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            surface_id: r.u32()?,
            width: r.u32()?,
            height: r.u32()?,
            format: r.u32()?,
            flags: r.u32()?,
        })
    }
}

/// `SpiceMsgSurfaceDestroy`
pub fn decode_surface_destroy(b: &[u8]) -> Result<u32> {
    Reader::new(b).u32()
}

/// `SpiceHead`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head {
    pub monitor_id: u32,
    pub surface_id: u32,
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    pub flags: u32,
}

/// `SpiceMsgDisplayMonitorsConfig`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorsConfig {
    pub max_allowed: u16,
    pub heads: Vec<Head>,
}

impl MonitorsConfig {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        let count = r.u16()?;
        let max_allowed = r.u16()?;
        let mut heads = Vec::with_capacity(count as usize);
        for _ in 0..count {
            heads.push(Head {
                monitor_id: r.u32()?,
                surface_id: r.u32()?,
                width: r.u32()?,
                height: r.u32()?,
                x: r.u32()?,
                y: r.u32()?,
                flags: r.u32()?,
            });
        }
        Ok(Self { max_allowed, heads })
    }
}

/// `SpiceMsgDisplayStreamCreate`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamCreate {
    pub surface_id: u32,
    pub id: u32,
    pub flags: u8,
    pub codec_type: u8,
    pub stamp: u64,
    pub stream_width: u32,
    pub stream_height: u32,
    pub src_width: u32,
    pub src_height: u32,
    pub dest: Rect,
    pub clip: Clip,
}

impl StreamCreate {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            surface_id: r.u32()?,
            id: r.u32()?,
            flags: r.u8()?,
            codec_type: r.u8()?,
            stamp: r.u64()?,
            stream_width: r.u32()?,
            stream_height: r.u32()?,
            src_width: r.u32()?,
            src_height: r.u32()?,
            dest: Rect::decode(&mut r)?,
            clip: Clip::decode(&mut r)?,
        })
    }
}

/// `SpiceMsgDisplayStreamData` / `StreamDataSized`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamData<'a> {
    pub id: u32,
    pub multi_media_time: u32,
    /// Only in `STREAM_DATA_SIZED`: the frame size and destination.
    pub sized: Option<(u32, u32, Rect)>,
    pub data: &'a [u8],
}

impl<'a> StreamData<'a> {
    pub fn decode(b: &'a [u8], sized: bool) -> Result<Self> {
        let mut r = Reader::new(b);
        let id = r.u32()?;
        let multi_media_time = r.u32()?;
        let sized = if sized {
            let w = r.u32()?;
            let h = r.u32()?;
            Some((w, h, Rect::decode(&mut r)?))
        } else {
            None
        };
        let n = r.u32()?;
        Ok(Self {
            id,
            multi_media_time,
            sized,
            data: r.bytes(n as usize)?,
        })
    }
}

/// `SpiceMsgDisplayStreamClip`
pub fn decode_stream_clip(b: &[u8]) -> Result<(u32, Clip)> {
    let mut r = Reader::new(b);
    Ok((r.u32()?, Clip::decode(&mut r)?))
}

/// `SpiceMsgDisplayStreamDestroy`
pub fn decode_stream_destroy(b: &[u8]) -> Result<u32> {
    Reader::new(b).u32()
}

/// `SpiceMsgDisplayStreamActivateReport`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamActivateReport {
    pub stream_id: u32,
    pub unique_id: u32,
    pub max_window_size: u32,
    pub timeout_ms: u32,
}

impl StreamActivateReport {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b);
        Ok(Self {
            stream_id: r.u32()?,
            unique_id: r.u32()?,
            max_window_size: r.u32()?,
            timeout_ms: r.u32()?,
        })
    }
}

/// `SpiceMsgcDisplayInit`
pub struct Init {
    pub pixmap_cache_id: u8,
    pub pixmap_cache_size: i64,
    pub glz_dictionary_id: u8,
    pub glz_dictionary_window_size: i32,
}

impl ClientMsg for Init {
    const TYPE: u16 = msgc::display::INIT;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u8(self.pixmap_cache_id);
        buf.put_i64_le(self.pixmap_cache_size);
        buf.put_u8(self.glz_dictionary_id);
        buf.put_i32_le(self.glz_dictionary_window_size);
    }
}

/// `SpiceMsgcDisplayPreferredCompression`
pub struct PreferredCompression(pub u8);

impl ClientMsg for PreferredCompression {
    const TYPE: u16 = msgc::display::PREFERRED_COMPRESSION;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u8(self.0);
    }
}

/// `SpiceMsgcDisplayPreferredVideoCodecType`
pub struct PreferredVideoCodecType(pub Vec<u8>);

impl ClientMsg for PreferredVideoCodecType {
    const TYPE: u16 = msgc::display::PREFERRED_VIDEO_CODEC_TYPE;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u8(self.0.len() as u8);
        buf.put_slice(&self.0);
    }
}

/// `SpiceMsgcDisplayStreamReport`
pub struct StreamReport {
    pub stream_id: u32,
    pub unique_id: u32,
    pub start_frame_mm_time: u32,
    pub end_frame_mm_time: u32,
    pub num_frames: u32,
    pub num_drops: u32,
    pub last_frame_delay: i32,
    pub audio_delay: u32,
}

impl ClientMsg for StreamReport {
    const TYPE: u16 = msgc::display::STREAM_REPORT;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.stream_id);
        buf.put_u32_le(self.unique_id);
        buf.put_u32_le(self.start_frame_mm_time);
        buf.put_u32_le(self.end_frame_mm_time);
        buf.put_u32_le(self.num_frames);
        buf.put_u32_le(self.num_drops);
        buf.put_i32_le(self.last_frame_delay);
        buf.put_u32_le(self.audio_delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::{bitmap_fmt, msg::display as d, ropd};

    /// A little builder for message bodies with @ptr32 fields.
    #[derive(Default)]
    struct W(Vec<u8>);
    impl W {
        fn u8(&mut self, v: u8) -> &mut Self {
            self.0.push(v);
            self
        }
        fn u16(&mut self, v: u16) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn u32(&mut self, v: u32) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn u64(&mut self, v: u64) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn rect(&mut self, l: i32, t: i32, r: i32, b: i32) -> &mut Self {
            self.u32(t as u32).u32(l as u32).u32(b as u32).u32(r as u32)
        }
        fn pos(&self) -> u32 {
            self.0.len() as u32
        }
    }

    #[test]
    fn draw_fill_solid() {
        let mut w = W::default();
        w.u32(0).rect(1, 2, 3, 4).u8(clip_type::NONE);
        w.u8(brush_type::SOLID).u32(0xff00ff).u16(ropd::OP_PUT);
        w.u8(0).u32(0).u32(0).u32(0); // mask: flags, pos, null bitmap
        let (base, op) = decode_draw(d::DRAW_FILL, &w.0).unwrap();
        assert_eq!(base.bbox, Rect::new(1, 2, 3, 4));
        let DrawOp::Fill(f) = op else { panic!() };
        assert_eq!(f.brush, Brush::Solid(0xff00ff));
        assert_eq!(f.rop_descriptor, ropd::OP_PUT);
        assert!(f.mask.bitmap.is_none());
    }

    #[test]
    fn draw_copy_bitmap_with_clip_and_palette() {
        let mut w = W::default();
        w.u32(0)
            .rect(0, 0, 2, 1)
            .u8(clip_type::RECTS)
            .u32(1)
            .rect(0, 0, 1, 1);
        let img_ptr_at = w.pos();
        w.u32(0); // src_bitmap, patched below
        w.rect(0, 0, 2, 1).u16(ropd::OP_PUT).u8(0);
        w.u8(0).u32(0).u32(0).u32(0); // mask
        let img = w.pos();
        w.0[img_ptr_at as usize..img_ptr_at as usize + 4].copy_from_slice(&img.to_le_bytes());
        // ImageDescriptor
        w.u64(42).u8(image_type::BITMAP).u8(0).u32(2).u32(1);
        // SpiceBitmap: 8bit, top-down, 2x1, stride 4, palette ptr
        w.u8(bitmap_fmt::BIT8)
            .u8(bitmap_flags::TOP_DOWN)
            .u32(2)
            .u32(1)
            .u32(4);
        let pal_ptr_at = w.pos();
        w.u32(0);
        w.u8(1).u8(0).u8(0).u8(0); // pixel data (stride 4 * 1 row)
        let pal = w.pos();
        w.0[pal_ptr_at as usize..pal_ptr_at as usize + 4].copy_from_slice(&pal.to_le_bytes());
        w.u64(7).u16(2).u32(0x111111).u32(0x222222);

        let (base, op) = decode_draw(d::DRAW_COPY, &w.0).unwrap();
        assert_eq!(base.clip, Clip::Rects(vec![Rect::new(0, 0, 1, 1)]));
        let DrawOp::Copy(c) = op else { panic!() };
        let src = c.src_bitmap.unwrap();
        assert_eq!(src.descriptor.id, 42);
        let ImageData::Bitmap(b) = src.data else {
            panic!()
        };
        assert_eq!(b.data, &[1, 0, 0, 0]);
        assert_eq!(
            b.palette,
            PaletteRef::Inline(Palette {
                unique: 7,
                ents: vec![0x111111, 0x222222]
            })
        );
    }

    #[test]
    fn bad_offsets_are_errors() {
        let mut w = W::default();
        w.u32(0).rect(0, 0, 1, 1).u8(clip_type::NONE);
        w.u32(9999); // src_bitmap points past the end
        w.rect(0, 0, 1, 1).u16(0).u8(0).u8(0).u32(0).u32(0).u32(0);
        assert!(decode_draw(d::DRAW_COPY, &w.0).is_err());
        // Huge clip count must not allocate or panic.
        let mut w = W::default();
        w.u32(0).rect(0, 0, 1, 1).u8(clip_type::RECTS).u32(u32::MAX);
        assert!(decode_draw(d::DRAW_FILL, &w.0).is_err());
    }

    #[test]
    fn surface_create_and_init() {
        let mut w = W::default();
        w.u32(0).u32(640).u32(480).u32(32).u32(1);
        let s = SurfaceCreate::decode(&w.0).unwrap();
        assert_eq!((s.width, s.height, s.format, s.flags), (640, 480, 32, 1));
        let mut b = BytesMut::new();
        Init {
            pixmap_cache_id: 1,
            pixmap_cache_size: 5,
            glz_dictionary_id: 1,
            glz_dictionary_window_size: 6,
        }
        .encode(&mut b);
        assert_eq!(b.len(), 14); // sizeof(SpiceMsgcDisplayInit)
    }

    proptest::proptest! {
        #[test]
        fn draw_decode_never_panics(ty in 302u16..=318, body in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let _ = decode_draw(ty, &body);
        }
    }
}
