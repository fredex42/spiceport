//! Draw operations and image fetching (`canvas_base.c`, `sw_canvas.c`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use log::{debug, trace};
use spice_decode::glz::GlzWindow;
use spice_decode::image::{rgb555_to_xrgb, rgb565_to_xrgb};
use spice_decode::{DecodeError, Format, Image};
use spice_proto::enums::{
    alpha_flags, bitmap_flags, image_flags, image_type, mask_flags, path_flags, scale_mode,
    surface_fmt,
};
use spice_proto::msg::display::{
    AlphaBlend, Brush, Clip, Composite, Copy, CopyBits, DisplayBase, DrawOp, Fill,
    Image as ImageMsg, ImageData, Opaque, PaletteRef, Point, PointFix, QMask, Rect, Rop3,
    SpiceString, Stroke, Text, Transparent,
};
use thiserror::Error;

use crate::pixops;
use crate::region::Region;
use crate::rop::{Rop, RopInput, ropd_to_rop};
use crate::surface::{SharedSurface, SurfaceData, intersect};

#[derive(Debug, Error)]
pub enum CanvasError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("unknown surface {0}")]
    NoSurface(u32),
    #[error("image {0:#x} is not in the cache")]
    NoImage(u64),
    #[error("palette {0:#x} is not in the cache")]
    NoPalette(u64),
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, CanvasError>;

struct Cached {
    image: Arc<Image>,
    lossy: bool,
}

/// A source of pixels for a draw: an image positioned so that logical
/// pixel `(x, y)` is `image[(x - ox, y - oy)]`. Decoded images have
/// `ox = oy = 0`; snapshots of part of a surface are offset.
#[derive(Clone)]
struct Src {
    img: Arc<Image>,
    ox: i32,
    oy: i32,
}

impl Src {
    fn whole(img: Arc<Image>) -> Self {
        Self { img, ox: 0, oy: 0 }
    }

    fn bounds(&self) -> Rect {
        Rect::new(
            self.ox,
            self.oy,
            self.ox + self.img.width as i32,
            self.oy + self.img.height as i32,
        )
    }

    #[inline]
    fn get(&self, x: i32, y: i32) -> u32 {
        let (ix, iy) = (x - self.ox, y - self.oy);
        self.img.data[iy as usize * self.img.width as usize + ix as usize]
    }

    /// Like `get` but returns `None` outside the image (`PIXMAN_REPEAT_NONE`).
    #[inline]
    fn try_get(&self, x: i32, y: i32) -> Option<u32> {
        let (ix, iy) = (x - self.ox, y - self.oy);
        if ix < 0 || iy < 0 || ix >= self.img.width as i32 || iy >= self.img.height as i32 {
            None
        } else {
            Some(self.img.data[iy as usize * self.img.width as usize + ix as usize])
        }
    }
}

/// The per-display-channel canvas state: surfaces and caches.
pub struct Canvas {
    surfaces: HashMap<u32, SharedSurface>,
    images: HashMap<u64, Cached>,
    palettes: HashMap<u64, Arc<Vec<u32>>>,
    /// Shared by every display channel of the session.
    glz: Arc<GlzWindow>,
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new(GlzWindow::new())
    }
}

impl Canvas {
    pub fn new(glz: Arc<GlzWindow>) -> Self {
        Self {
            surfaces: HashMap::new(),
            images: HashMap::new(),
            palettes: HashMap::new(),
            glz,
        }
    }

    // ---- surfaces ------------------------------------------------------

    pub fn create_surface(
        &mut self,
        id: u32,
        width: u32,
        height: u32,
        format: u32,
    ) -> Result<SharedSurface> {
        if width as u64 * height as u64 > spice_decode::image::MAX_PIXELS {
            return Err(CanvasError::Invalid(format!(
                "surface {width}x{height} is too large"
            )));
        }
        let s = Arc::new(Mutex::new(SurfaceData::new(id, width, height, format)));
        self.surfaces.insert(id, s.clone());
        Ok(s)
    }

    pub fn destroy_surface(&mut self, id: u32) -> Option<SharedSurface> {
        self.surfaces.remove(&id)
    }

    pub fn surface(&self, id: u32) -> Result<SharedSurface> {
        self.surfaces
            .get(&id)
            .cloned()
            .ok_or(CanvasError::NoSurface(id))
    }

    pub fn clear_surfaces(&mut self) {
        self.surfaces.clear();
    }

    // ---- caches --------------------------------------------------------

    pub fn inval_image(&mut self, id: u64) -> bool {
        self.images.remove(&id).is_some()
    }

    pub fn clear_images(&mut self) {
        self.images.clear();
    }

    pub fn inval_palette(&mut self, id: u64) {
        self.palettes.remove(&id);
    }

    pub fn clear_palettes(&mut self) {
        self.palettes.clear();
    }

    fn cache_put(&mut self, id: u64, image: Arc<Image>, lossy: bool) {
        self.images.insert(id, Cached { image, lossy });
    }

    fn cache_get(&self, id: u64, lossless: bool) -> Result<Arc<Image>> {
        let c = self.images.get(&id).ok_or(CanvasError::NoImage(id))?;
        if lossless && c.lossy {
            // spice-gtk blocks until the lossless version arrives; it is
            // normally already here, so just use what we have.
            debug!("lossless image {id:#x} requested but only a lossy one is cached");
        }
        Ok(c.image.clone())
    }

    fn resolve_palette(
        &mut self,
        p: &PaletteRef,
        flags: u8,
        dst_fmt: u32,
    ) -> Result<Option<Arc<Vec<u32>>>> {
        let pal = match p {
            PaletteRef::FromCache(id) => Some(
                self.palettes
                    .get(id)
                    .cloned()
                    .ok_or(CanvasError::NoPalette(*id))?,
            ),
            PaletteRef::None => None,
            PaletteRef::Inline(pal) => {
                let ents = Arc::new(pal.ents.clone());
                if flags & bitmap_flags::PAL_CACHE_ME != 0 {
                    self.palettes.insert(pal.unique, ents.clone());
                }
                Some(ents)
            }
        };
        // canvas_get_localized_palette: 16-bit surfaces have 16-bit palettes.
        Ok(pal.map(|p| match dst_fmt {
            surface_fmt::RGB555 => Arc::new(p.iter().map(|&c| rgb555_to_xrgb(c)).collect()),
            surface_fmt::RGB565 => Arc::new(p.iter().map(|&c| rgb565_to_xrgb(c)).collect()),
            _ => p,
        }))
    }

    /// Decode the image data itself (`get_surface_from_canvas`).
    fn decode_image(&mut self, img: &ImageMsg<'_>, dst_fmt: u32) -> Result<Arc<Image>> {
        let d = &img.descriptor;
        trace!(
            "image {:#x}: type {} {}x{} flags {:#x}",
            d.id, d.ty, d.width, d.height, d.flags
        );
        let image = match &img.data {
            ImageData::Bitmap(b) => {
                let pal = self.resolve_palette(&b.palette, b.flags, dst_fmt)?;
                spice_decode::bitmap::decode(
                    b.format,
                    b.flags,
                    b.x,
                    b.y,
                    b.stride,
                    b.data,
                    pal.as_deref().map(|v| &v[..]),
                )?
            }
            ImageData::Lz4(data) => spice_decode::lz4::decode(data, d.width, d.height)?,
            ImageData::Jpeg(data) => spice_decode::jpeg::decode(data)?,
            ImageData::Quic(data) => spice_decode::quic::decode(data)?,
            ImageData::LzRgb(data) => spice_decode::lz::decode(data, None)?,
            ImageData::LzPlt {
                flags,
                palette,
                data,
            } => {
                let pal = self.resolve_palette(palette, *flags, dst_fmt)?;
                spice_decode::lz::decode(data, pal.as_deref().map(|v| &v[..]))?
            }
            // canvas_get_glz passes no palette: servers only send RGB GLZ.
            ImageData::GlzRgb(data) => spice_decode::glz::decode(&self.glz, data, None)?,
            ImageData::ZlibGlzRgb {
                glz_data_size,
                data,
            } => spice_decode::glz::decode_zlib(&self.glz, data, *glz_data_size, None)?,
            ImageData::JpegAlpha {
                flags,
                jpeg_size,
                data,
            } => {
                let n = (*jpeg_size as usize).min(data.len());
                let mut i = spice_decode::jpeg::decode(&data[..n])?;
                check_size(&i, d.width, d.height)?;
                i.format = Format::Argb32;
                let top_down = flags & spice_proto::enums::jpeg_alpha_flags::TOP_DOWN != 0;
                spice_decode::lz::decode_alpha_into(&data[n..], &mut i, top_down)?;
                i
            }
            ImageData::FromCache => return self.cache_get(d.id, false),
            ImageData::FromCacheLossless => return self.cache_get(d.id, true),
            ImageData::Surface(_) => unreachable!("surfaces are handled by the caller"),
        };
        check_size(&image, d.width, d.height)?;
        Ok(Arc::new(image))
    }

    /// `canvas_get_image_internal`: decode an image, apply the cache flags,
    /// and convert it for drawing on a surface of format `dst_fmt`.
    /// With `real_get == false` the image is only decoded when it must be
    /// cached ("touched"), and `None` is returned.
    fn get_image(
        &mut self,
        img: &ImageMsg<'_>,
        dst_fmt: u32,
        real_get: bool,
    ) -> Result<Option<Arc<Image>>> {
        let d = img.descriptor;
        let has_palette_to_cache = match &img.data {
            ImageData::Bitmap(b) => {
                matches!(b.palette, PaletteRef::Inline(_))
                    && b.flags & bitmap_flags::PAL_CACHE_ME != 0
            }
            ImageData::LzPlt { flags, palette, .. } => {
                matches!(palette, PaletteRef::Inline(_)) && flags & bitmap_flags::PAL_CACHE_ME != 0
            }
            _ => false,
        };
        if !real_get
            && d.flags & (image_flags::CACHE_ME | image_flags::CACHE_REPLACE_ME) == 0
            && !has_palette_to_cache
            && d.ty != image_type::GLZ_RGB
            && d.ty != image_type::ZLIB_GLZ_RGB
        {
            return Ok(None);
        }

        let mut image = self.decode_image(img, dst_fmt)?;
        let from_cache = matches!(
            d.ty,
            image_type::FROM_CACHE | image_type::FROM_CACHE_LOSSLESS
        );

        if d.flags & image_flags::HIGH_BITS_SET != 0
            && !from_cache
            && image.format == Format::Xrgb32
        {
            Arc::make_mut(&mut image)
                .data
                .iter_mut()
                .for_each(|p| *p |= 0xff00_0000);
        }
        if d.flags & image_flags::CACHE_ME != 0 && !from_cache {
            trace!(
                "caching image {:#x} ({}lossy)",
                d.id,
                if d.is_lossy() { "" } else { "not " }
            );
            self.cache_put(d.id, image.clone(), d.is_lossy());
        } else if d.flags & image_flags::CACHE_REPLACE_ME != 0 {
            if d.is_lossy() {
                return Err(CanvasError::Invalid(
                    "cache replace request with a lossy image".into(),
                ));
            }
            self.cache_put(d.id, image.clone(), false);
        }
        if !real_get {
            return Ok(None);
        }
        Ok(Some(convert_for(image, dst_fmt)))
    }

    /// Fetch a draw source; `area` limits how much of a source *surface* is
    /// copied.
    fn get_src(&mut self, img: &ImageMsg<'_>, dst_fmt: u32, area: Rect) -> Result<Src> {
        if let ImageData::Surface(id) = img.data {
            let s = self.surface(id)?;
            let s = s.lock().unwrap();
            let (snap, ox, oy) = s.snapshot(area);
            return Ok(Src {
                img: convert_for(Arc::new(snap), dst_fmt),
                ox,
                oy,
            });
        }
        let image = self
            .get_image(img, dst_fmt, true)?
            .expect("real get returns an image");
        Ok(Src::whole(image))
    }

    fn touch(&mut self, img: &ImageMsg<'_>, dst_fmt: u32) -> Result<()> {
        if !matches!(img.data, ImageData::Surface(_)) {
            self.get_image(img, dst_fmt, false)?;
        }
        Ok(())
    }

    fn touch_brush(&mut self, b: &Brush<'_>, dst_fmt: u32) -> Result<()> {
        if let Brush::Pattern { image, .. } = b {
            self.touch(image, dst_fmt)?;
        }
        Ok(())
    }

    // ---- regions -------------------------------------------------------

    /// `canvas_clip_pixman`: bbox ∩ surface ∩ clip rects.
    fn clip_region(bbox: Rect, bounds: Rect, clip: &Clip) -> Region {
        let r = Region::from_rect(intersect(bbox, bounds));
        match clip {
            Clip::None => r,
            Clip::Rects(rects) => r.intersect(&Region::from_rects(rects)),
        }
    }

    /// `canvas_mask_pixman`: restrict `region` to the set pixels of the mask.
    fn mask_region(&mut self, region: Region, mask: &QMask<'_>, x: i32, y: i32) -> Result<Region> {
        let Some(bitmap) = &mask.bitmap else {
            return Ok(region);
        };
        if region.is_empty() {
            // Still decode it if it has to be cached.
            if !matches!(bitmap.data, ImageData::Surface(_)) {
                self.get_mask_image(bitmap)?;
            }
            return Ok(region);
        }
        let invert = mask.flags & mask_flags::INVERS != 0;
        let image = match bitmap.data {
            ImageData::Surface(id) => {
                let s = self.surface(id)?;
                let s = s.lock().unwrap();
                Arc::new(s.snapshot(s.bounds()).0)
            }
            _ => self.get_mask_image(bitmap)?,
        };
        // Mask pixel (mx, my) covers destination pixel (mx + dx, my + dy).
        let (dx, dy) = (x - mask.pos.x, y - mask.pos.y);
        let ext = region.extents();
        let mrect = intersect(
            Rect::new(ext.left - dx, ext.top - dy, ext.right - dx, ext.bottom - dy),
            Rect::new(0, 0, image.width as i32, image.height as i32),
        );
        let w = image.width as usize;
        let rows = (mrect.top.max(0)..mrect.bottom.max(0)).map(|my| {
            let row = &image.data[my as usize * w..(my as usize + 1) * w];
            let mut spans = Vec::new();
            let mut start = None;
            for mx in mrect.left..mrect.right {
                let on = mask_bit(row[mx as usize], image.format) != invert;
                match (on, start) {
                    (true, None) => start = Some(mx),
                    (false, Some(s)) => {
                        spans.push((s, mx));
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = start {
                spans.push((s, mrect.right));
            }
            (my, spans)
        });
        let mut m = Region::from_row_spans(rows);
        m.translate(dx, dy);
        Ok(region.intersect(&m))
    }

    /// `canvas_get_mask`: 1-bpp bitmaps or cached masks.
    fn get_mask_image(&mut self, img: &ImageMsg<'_>) -> Result<Arc<Image>> {
        let d = img.descriptor;
        let image = match &img.data {
            ImageData::Bitmap(b) => Arc::new(spice_decode::bitmap::decode_mask(
                b.format, b.flags, b.x, b.y, b.stride, b.data, false,
            )?),
            ImageData::FromCache => self.cache_get(d.id, false)?,
            ImageData::FromCacheLossless => self.cache_get(d.id, true)?,
            _ => {
                return Err(CanvasError::Unsupported(format!(
                    "mask image type {}",
                    d.ty
                )));
            }
        };
        if d.flags & image_flags::CACHE_ME != 0 {
            self.cache_put(d.id, image.clone(), false);
        }
        Ok(image)
    }

    /// Region for a draw: clip, then mask.
    fn dest_region(
        &mut self,
        s: &SurfaceData,
        base: &DisplayBase,
        mask: Option<&QMask<'_>>,
    ) -> Result<Region> {
        let r = Self::clip_region(base.bbox, s.bounds(), &base.clip);
        match mask {
            Some(m) => self.mask_region(r, m, base.bbox.left, base.bbox.top),
            None => Ok(r),
        }
    }

    // ---- draw ops --------------------------------------------------------

    /// Execute a draw message on its target surface.
    pub fn draw(&mut self, base: &DisplayBase, op: &DrawOp<'_>) -> Result<()> {
        let surface = self.surface(base.surface_id)?;
        let fmt = surface.lock().unwrap().format;
        match op {
            DrawOp::Fill(f) => self.draw_fill(&surface, fmt, base, f),
            DrawOp::Copy(c) => self.draw_copy(&surface, fmt, base, c),
            // "copy and blend are the same" (canvas_base_init_ops)
            DrawOp::Blend(c) => self.draw_copy(&surface, fmt, base, c),
            DrawOp::Opaque(o) => self.draw_opaque(&surface, fmt, base, o),
            DrawOp::Blackness(m) => self.draw_solid(&surface, base, m, 0, Rop::Copy),
            DrawOp::Whiteness(m) => self.draw_solid(&surface, base, m, 0xffff_ffff, Rop::Copy),
            DrawOp::Invers(m) => self.draw_solid(&surface, base, m, 0, Rop::Invert),
            DrawOp::Transparent(t) => self.draw_transparent(&surface, fmt, base, t),
            DrawOp::AlphaBlend(a) => self.draw_alpha_blend(&surface, fmt, base, a),
            DrawOp::Rop3(r) => self.draw_rop3(&surface, fmt, base, r),
            DrawOp::Composite(c) => self.draw_composite(&surface, fmt, base, c),
            DrawOp::Stroke(st) => self.draw_stroke(&surface, fmt, base, st),
            DrawOp::Text(t) => self.draw_text(&surface, fmt, base, t),
        }
    }

    fn draw_fill(
        &mut self,
        surface: &SharedSurface,
        fmt: u32,
        base: &DisplayBase,
        f: &Fill<'_>,
    ) -> Result<()> {
        let rop = ropd_to_rop(f.rop_descriptor, RopInput::Brush, RopInput::Dest);
        // Resolve the brush before locking the target: a pattern may come from it.
        let region = {
            let s = surface.lock().unwrap();
            self.dest_region(&s, base, Some(&f.mask))?
        };
        if rop == Rop::Noop || region.is_empty() {
            return self.touch_brush(&f.brush, fmt);
        }
        let brush = self.resolve_brush(&f.brush, fmt, region.extents())?;
        let mut s = surface.lock().unwrap();
        fill_brush(&mut s, &region, &brush, rop);
        s.add_dirty(base.bbox);
        Ok(())
    }

    fn draw_solid(
        &mut self,
        surface: &SharedSurface,
        base: &DisplayBase,
        m: &QMask<'_>,
        color: u32,
        rop: Rop,
    ) -> Result<()> {
        let region = {
            let s = surface.lock().unwrap();
            self.dest_region(&s, base, Some(m))?
        };
        if region.is_empty() {
            return Ok(());
        }
        let mut s = surface.lock().unwrap();
        fill_solid(&mut s, &region, color, rop);
        s.add_dirty(base.bbox);
        Ok(())
    }

    fn draw_copy(
        &mut self,
        surface: &SharedSurface,
        fmt: u32,
        base: &DisplayBase,
        c: &Copy<'_>,
    ) -> Result<()> {
        let rop = ropd_to_rop(c.rop_descriptor, RopInput::Src, RopInput::Dest);
        let region = {
            let s = surface.lock().unwrap();
            self.dest_region(&s, base, Some(&c.mask))?
        };
        let Some(src_img) = &c.src_bitmap else {
            return Err(CanvasError::Invalid("copy without a source image".into()));
        };
        if rop == Rop::Noop || region.is_empty() {
            return self.touch(src_img, fmt);
        }
        let src = self.get_src(src_img, fmt, c.src_area)?;
        let mut s = surface.lock().unwrap();
        let bbox = base.bbox;
        if bbox.width() == c.src_area.width() && bbox.height() == c.src_area.height() {
            blit(
                &mut s,
                &region,
                &src,
                bbox.left - c.src_area.left,
                bbox.top - c.src_area.top,
                rop,
            );
        } else {
            scale(&mut s, &region, &src, c.src_area, bbox, c.scale_mode, rop);
        }
        s.add_dirty(bbox);
        Ok(())
    }

    /// `canvas_copy_bits`: move a rectangle within a surface.
    pub fn copy_bits(&mut self, cb: &CopyBits) -> Result<()> {
        let surface = self.surface(cb.base.surface_id)?;
        let mut s = surface.lock().unwrap();
        let bbox = cb.base.bbox;
        let mut region = Self::clip_region(bbox, s.bounds(), &cb.base.clip);
        let dx = bbox.left - cb.src_pos.x;
        let dy = bbox.top - cb.src_pos.y;
        if dx == 0 && dy == 0 {
            return Ok(());
        }
        // Don't read outside the surface.
        region =
            region.intersect_rect(Rect::new(dx, dy, dx + s.width as i32, dy + s.height as i32));
        if region.is_empty() {
            return Ok(());
        }
        let ext = region.extents();
        let (snap, ox, oy) = s.snapshot(Rect::new(
            ext.left - dx,
            ext.top - dy,
            ext.right - dx,
            ext.bottom - dy,
        ));
        let src = Src {
            img: Arc::new(snap),
            ox,
            oy,
        };
        blit(&mut s, &region, &src, dx, dy, Rop::Copy);
        s.add_dirty(bbox);
        Ok(())
    }

    /// `SPICE_MSG_DISPLAY_RESET` / `canvas_clear`: zero a surface.
    pub fn clear_surface(&mut self, id: u32) {
        if let Some(s) = self.surfaces.get(&id) {
            let mut s = s.lock().unwrap();
            s.pixels.fill(0);
            let b = s.bounds();
            s.add_dirty(b);
        }
    }

    fn resolve_brush(&mut self, b: &Brush<'_>, fmt: u32, _area: Rect) -> Result<ResolvedBrush> {
        Ok(match b {
            Brush::None => ResolvedBrush::Solid(0),
            Brush::Solid(c) => ResolvedBrush::Solid(convert_color(*c, fmt)),
            Brush::Pattern { image, pos } => {
                // Tiles are small; take the whole source.
                let src = self.get_src(image, fmt, Rect::new(0, 0, i32::MAX, i32::MAX))?;
                ResolvedBrush::Tile(src.img, *pos)
            }
        })
    }
}

enum ResolvedBrush {
    Solid(u32),
    Tile(Arc<Image>, Point),
}

fn check_size(i: &Image, w: u32, h: u32) -> Result<()> {
    if i.width != w || i.height != h {
        return Err(CanvasError::Invalid(format!(
            "decoded image is {}x{}, descriptor says {w}x{h}",
            i.width, i.height
        )));
    }
    Ok(())
}

#[inline]
fn mask_bit(p: u32, f: Format) -> bool {
    match f {
        Format::Xrgb32 => p & 0x00ff_ffff != 0,
        _ => p >> 24 != 0,
    }
}

/// Solid colours on 16-bit surfaces arrive in the surface's pixel format.
fn convert_color(c: u32, fmt: u32) -> u32 {
    match fmt {
        surface_fmt::RGB555 => rgb555_to_xrgb(c & 0xffff),
        surface_fmt::RGB565 => rgb565_to_xrgb(c & 0xffff),
        surface_fmt::A8 => c << 24,
        _ => c,
    }
}

type PixelFn = fn(u32) -> u32;

/// Convert an image to what drawing onto a `dst_fmt` surface expects
/// (`canvas_get_target_format` + the final conversion in
/// `canvas_get_image_internal`).
fn convert_for(img: Arc<Image>, dst_fmt: u32) -> Arc<Image> {
    match dst_fmt {
        surface_fmt::RGB555 | surface_fmt::RGB565 => {
            // Quantise to the surface depth, as pixman's conversion would.
            let (to, from): (PixelFn, PixelFn) = if dst_fmt == surface_fmt::RGB555 {
                (
                    |p| ((p >> 9) & 0x7c00) | ((p >> 6) & 0x03e0) | ((p >> 3) & 0x001f),
                    rgb555_to_xrgb,
                )
            } else {
                (
                    |p| ((p >> 8) & 0xf800) | ((p >> 5) & 0x07e0) | ((p >> 3) & 0x001f),
                    rgb565_to_xrgb,
                )
            };
            let data = img.data.iter().map(|&p| from(to(p))).collect();
            Arc::new(Image::from_data(
                img.width,
                img.height,
                Format::Xrgb32,
                data,
            ))
        }
        surface_fmt::A8 | surface_fmt::A1 => match img.format {
            Format::A8 => img,
            Format::Argb32 => {
                let data = img.data.iter().map(|&p| p & 0xff00_0000).collect();
                Arc::new(Image::from_data(img.width, img.height, Format::A8, data))
            }
            Format::Xrgb32 => Arc::new(Image::from_data(
                img.width,
                img.height,
                Format::A8,
                vec![0xff00_0000; img.data.len()],
            )),
        },
        _ => match img.format {
            // a8 composited onto xrgb: colour channels are 0.
            Format::A8 => {
                let data = img.data.iter().map(|&p| p & 0xff00_0000).collect();
                Arc::new(Image::from_data(
                    img.width,
                    img.height,
                    Format::Argb32,
                    data,
                ))
            }
            _ => img,
        },
    }
}

fn fill_solid(s: &mut SurfaceData, region: &Region, color: u32, rop: Rop) {
    let w = s.width as usize;
    for r in region.rects() {
        for y in r.top..r.bottom {
            let row =
                &mut s.pixels[y as usize * w + r.left as usize..y as usize * w + r.right as usize];
            if rop == Rop::Copy {
                row.fill(color);
            } else {
                row.iter_mut().for_each(|d| *d = rop.apply(color, *d));
            }
        }
    }
}

fn fill_brush(s: &mut SurfaceData, region: &Region, brush: &ResolvedBrush, rop: Rop) {
    match brush {
        ResolvedBrush::Solid(c) => fill_solid(s, region, *c, rop),
        ResolvedBrush::Tile(tile, pos) => {
            if tile.width == 0 || tile.height == 0 {
                return;
            }
            let (tw, th) = (tile.width as i32, tile.height as i32);
            let w = s.width as usize;
            for r in region.rects() {
                for y in r.top..r.bottom {
                    let ty = (y - pos.y).rem_euclid(th) as usize;
                    let trow = &tile.data[ty * tw as usize..(ty + 1) * tw as usize];
                    let row = &mut s.pixels[y as usize * w..(y as usize + 1) * w];
                    for x in r.left..r.right {
                        let t = trow[(x - pos.x).rem_euclid(tw) as usize];
                        let d = &mut row[x as usize];
                        *d = rop.apply(t, *d);
                    }
                }
            }
        }
    }
}

/// `spice_pixman_blit[_rop]` over a region: destination pixel `(x, y)` takes
/// source pixel `(x - offset_x, y - offset_y)`; pixels with no source are left.
fn blit(s: &mut SurfaceData, region: &Region, src: &Src, offset_x: i32, offset_y: i32, rop: Rop) {
    let w = s.width as usize;
    let sb = src.bounds();
    for r in region.rects() {
        // Clip the rect so its source lies within the image.
        let r = intersect(
            r,
            Rect::new(
                sb.left + offset_x,
                sb.top + offset_y,
                sb.right + offset_x,
                sb.bottom + offset_y,
            ),
        );
        if r.is_empty() {
            continue;
        }
        let n = r.width() as usize;
        for y in r.top..r.bottom {
            let sy = (y - offset_y - src.oy) as usize;
            let sx = (r.left - offset_x - src.ox) as usize;
            let srow = &src.img.data[sy * src.img.width as usize + sx..][..n];
            let drow = &mut s.pixels[y as usize * w + r.left as usize..][..n];
            if rop == Rop::Copy {
                drow.copy_from_slice(srow);
            } else {
                for (d, &p) in drow.iter_mut().zip(srow) {
                    *d = rop.apply(p, *d);
                }
            }
        }
    }
}

/// `__scale_image[_rop]`: draw `src_area` of `src` scaled into `dest`,
/// limited to `region`. Nearest-neighbour or bilinear filtering; samples
/// outside the source are transparent (`PIXMAN_REPEAT_NONE`).
fn scale(
    s: &mut SurfaceData,
    region: &Region,
    src: &Src,
    src_area: Rect,
    dest: Rect,
    mode: u8,
    rop: Rop,
) {
    if dest.is_empty() || src_area.is_empty() {
        return;
    }
    let fx = src_area.width() as f64 / dest.width() as f64;
    let fy = src_area.height() as f64 / dest.height() as f64;
    let w = s.width as usize;
    let nearest = mode == scale_mode::NEAREST;
    for r in region.rects() {
        for y in r.top..r.bottom {
            let sy = src_area.top as f64 + (y - dest.top) as f64 * fy + fy / 2.0;
            for x in r.left..r.right {
                let sx = src_area.left as f64 + (x - dest.left) as f64 * fx + fx / 2.0;
                let p = if nearest {
                    src.try_get(sx.floor() as i32, sy.floor() as i32)
                        .unwrap_or(0)
                } else {
                    bilinear(src, sx - 0.5, sy - 0.5)
                };
                let d = &mut s.pixels[y as usize * w + x as usize];
                *d = rop.apply(p, *d);
            }
        }
    }
}

fn bilinear(src: &Src, x: f64, y: f64) -> u32 {
    let b = src.bounds();
    let clampx = |v: i32| v.clamp(b.left, b.right - 1);
    let clampy = |v: i32| v.clamp(b.top, b.bottom - 1);
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let (ax, ay) = (x - x0 as f64, y - y0 as f64);
    let p00 = src.get(clampx(x0), clampy(y0));
    let p10 = src.get(clampx(x0 + 1), clampy(y0));
    let p01 = src.get(clampx(x0), clampy(y0 + 1));
    let p11 = src.get(clampx(x0 + 1), clampy(y0 + 1));
    let mut out = 0u32;
    for shift in [0, 8, 16, 24] {
        let c = |p: u32| ((p >> shift) & 0xff) as f64;
        let top = c(p00) * (1.0 - ax) + c(p10) * ax;
        let bot = c(p01) * (1.0 - ax) + c(p11) * ax;
        let v = (top * (1.0 - ay) + bot * ay).round() as u32;
        out |= v.min(255) << shift;
    }
    out
}

// ---- group 2 and 3 draw ops ---------------------------------------------

/// `fix_to_int` for 28.4 fixed point: rounds, with .5 going down.
fn fix_to_int(v: i32) -> i32 {
    let r = v & 0x0f;
    let i = v >> 4;
    if r > 8 { i + 1 } else { i }
}

impl Canvas {
    /// `canvas_draw_opaque`: blit the source, then apply the brush with the
    /// rop (brush as source, the blitted image as destination).
    fn draw_opaque(
        &mut self,
        surface: &SharedSurface,
        fmt: u32,
        base: &DisplayBase,
        o: &Opaque<'_>,
    ) -> Result<()> {
        let rop = ropd_to_rop(o.rop_descriptor, RopInput::Brush, RopInput::Src);
        let region = {
            let s = surface.lock().unwrap();
            self.dest_region(&s, base, Some(&o.mask))?
        };
        let Some(src_img) = &o.src_bitmap else {
            return Err(CanvasError::Invalid("opaque without a source image".into()));
        };
        if rop == Rop::Noop || region.is_empty() {
            self.touch(src_img, fmt)?;
            return self.touch_brush(&o.brush, fmt);
        }
        let src = self.get_src(src_img, fmt, o.src_area)?;
        let brush = self.resolve_brush(&o.brush, fmt, region.extents())?;
        let mut s = surface.lock().unwrap();
        let bbox = base.bbox;
        if bbox.width() == o.src_area.width() && bbox.height() == o.src_area.height() {
            blit(
                &mut s,
                &region,
                &src,
                bbox.left - o.src_area.left,
                bbox.top - o.src_area.top,
                Rop::Copy,
            );
        } else {
            scale(
                &mut s,
                &region,
                &src,
                o.src_area,
                bbox,
                o.scale_mode,
                Rop::Copy,
            );
        }
        fill_brush(&mut s, &region, &brush, rop);
        s.add_dirty(bbox);
        Ok(())
    }

    /// `canvas_draw_transparent`: copy pixels that don't match the colour key.
    fn draw_transparent(
        &mut self,
        surface: &SharedSurface,
        fmt: u32,
        base: &DisplayBase,
        t: &Transparent<'_>,
    ) -> Result<()> {
        let region = {
            let s = surface.lock().unwrap();
            self.dest_region(&s, base, None)?
        };
        let Some(src_img) = &t.src_bitmap else {
            return Err(CanvasError::Invalid(
                "transparent without a source image".into(),
            ));
        };
        if region.is_empty() {
            return self.touch(src_img, fmt);
        }
        // Compare in the surface's own depth, as the C code converts the key.
        let key = match fmt {
            surface_fmt::RGB555 => rgb555_to_xrgb(
                ((t.true_color >> 9) & 0x7c00)
                    | ((t.true_color >> 6) & 0x03e0)
                    | ((t.true_color >> 3) & 0x001f),
            ),
            surface_fmt::RGB565 => rgb565_to_xrgb(
                ((t.true_color >> 8) & 0xf800)
                    | ((t.true_color >> 5) & 0x07e0)
                    | ((t.true_color >> 3) & 0x001f),
            ),
            _ => t.true_color,
        } & 0x00ff_ffff;
        let src = self.get_src(src_img, fmt, t.src_area)?;
        let bbox = base.bbox;
        let src = if bbox.width() == t.src_area.width() && bbox.height() == t.src_area.height() {
            src
        } else {
            scaled_copy(&src, t.src_area, bbox, true)
        };
        let (ox, oy) = if bbox.width() == t.src_area.width() && bbox.height() == t.src_area.height()
        {
            (bbox.left - t.src_area.left, bbox.top - t.src_area.top)
        } else {
            (0, 0)
        };
        let mut s = surface.lock().unwrap();
        let w = s.width as usize;
        let sb = src.bounds();
        for r in region.rects() {
            for y in r.top..r.bottom {
                for x in r.left..r.right {
                    let (sx, sy) = (x - ox, y - oy);
                    if !(sb.left..sb.right).contains(&sx) || !(sb.top..sb.bottom).contains(&sy) {
                        continue;
                    }
                    let p = src.get(sx, sy);
                    if p & 0x00ff_ffff != key {
                        s.pixels[y as usize * w + x as usize] = p;
                    }
                }
            }
        }
        s.add_dirty(bbox);
        Ok(())
    }

    /// `canvas_draw_alpha_blend`: premultiplied OVER with a constant alpha.
    fn draw_alpha_blend(
        &mut self,
        surface: &SharedSurface,
        fmt: u32,
        base: &DisplayBase,
        a: &AlphaBlend<'_>,
    ) -> Result<()> {
        let region = {
            let s = surface.lock().unwrap();
            self.dest_region(&s, base, None)?
        };
        let Some(src_img) = &a.src_bitmap else {
            return Err(CanvasError::Invalid(
                "alpha blend without a source image".into(),
            ));
        };
        if a.alpha == 0 || region.is_empty() {
            return self.touch(src_img, fmt);
        }
        let mut src = self.get_src(src_img, fmt, a.src_area)?;
        // A surface source is only treated as having alpha when flagged.
        if matches!(src_img.data, ImageData::Surface(_)) {
            let want = if a.alpha_flags & alpha_flags::SRC_SURFACE_HAS_ALPHA != 0 {
                Format::Argb32
            } else {
                Format::Xrgb32
            };
            if src.img.format != want && src.img.format != Format::A8 {
                Arc::make_mut(&mut src.img).format = want;
            }
        }
        let bbox = base.bbox;
        let dest_fmt = if a.alpha_flags & alpha_flags::DEST_HAS_ALPHA != 0 {
            Format::Argb32
        } else {
            Format::Xrgb32
        };
        let same = bbox.width() == a.src_area.width() && bbox.height() == a.src_area.height();
        let ma = a.alpha as u32;
        let mut s = surface.lock().unwrap();
        let w = s.width as usize;
        let sf = src.img.format;
        for r in region.rects() {
            for y in r.top..r.bottom {
                for x in r.left..r.right {
                    let (sx, sy) = if same {
                        (
                            x - bbox.left + a.src_area.left,
                            y - bbox.top + a.src_area.top,
                        )
                    } else {
                        // NEAREST scaling, sampling at pixel centres.
                        let fx = a.src_area.width() as f64 / bbox.width() as f64;
                        let fy = a.src_area.height() as f64 / bbox.height() as f64;
                        (
                            a.src_area.left
                                + ((x - bbox.left) as f64 * fx + fx / 2.0).floor() as i32,
                            a.src_area.top + ((y - bbox.top) as f64 * fy + fy / 2.0).floor() as i32,
                        )
                    };
                    let Some(p) = src.try_get(sx, sy) else {
                        continue;
                    };
                    let sp = pixops::unpack(p, sf).map(|c| pixops::mul(c, ma));
                    let d = &mut s.pixels[y as usize * w + x as usize];
                    let dp = pixops::unpack(*d, dest_fmt);
                    *d = pixops::pack(
                        pixops::combine(pixops::op::OVER, sp, [sp[0]; 4], dp),
                        dest_fmt,
                    );
                }
            }
        }
        s.add_dirty(bbox);
        Ok(())
    }

    /// `canvas_draw_rop3`
    fn draw_rop3(
        &mut self,
        surface: &SharedSurface,
        fmt: u32,
        base: &DisplayBase,
        r3: &Rop3<'_>,
    ) -> Result<()> {
        let region = {
            let s = surface.lock().unwrap();
            self.dest_region(&s, base, Some(&r3.mask))?
        };
        let Some(src_img) = &r3.src_bitmap else {
            return Err(CanvasError::Invalid("rop3 without a source image".into()));
        };
        let bbox = base.bbox;
        let src = self.get_src(src_img, fmt, r3.src_area)?;
        let (src, sx0, sy0) =
            if bbox.width() == r3.src_area.width() && bbox.height() == r3.src_area.height() {
                (src, r3.src_area.left, r3.src_area.top)
            } else {
                (
                    scaled_copy(
                        &src,
                        r3.src_area,
                        bbox,
                        r3.scale_mode == scale_mode::NEAREST,
                    ),
                    bbox.left,
                    bbox.top,
                )
            };
        let brush = self.resolve_brush(&r3.brush, fmt, region.extents())?;
        if region.is_empty() {
            return Ok(());
        }
        let mut s = surface.lock().unwrap();
        let w = s.width as usize;
        for rc in region.rects() {
            for y in rc.top..rc.bottom {
                for x in rc.left..rc.right {
                    let Some(sp) = src.try_get(sx0 + x - bbox.left, sy0 + y - bbox.top) else {
                        continue;
                    };
                    let pp = match &brush {
                        ResolvedBrush::Solid(c) => *c,
                        ResolvedBrush::Tile(t, pos) => {
                            if t.width == 0 || t.height == 0 {
                                0
                            } else {
                                let tx = (x - pos.x).rem_euclid(t.width as i32) as usize;
                                let ty = (y - pos.y).rem_euclid(t.height as i32) as usize;
                                t.data[ty * t.width as usize + tx]
                            }
                        }
                    };
                    let d = &mut s.pixels[y as usize * w + x as usize];
                    *d = pixops::rop3(r3.rop3, pp, sp, *d);
                }
            }
        }
        s.add_dirty(bbox);
        Ok(())
    }

    /// `canvas_draw_composite`: a pixman composite of source and optional
    /// mask onto the destination.
    fn draw_composite(
        &mut self,
        surface: &SharedSurface,
        fmt: u32,
        base: &DisplayBase,
        c: &Composite<'_>,
    ) -> Result<()> {
        use spice_proto::enums::composite_flags as cf;
        let region = {
            let s = surface.lock().unwrap();
            self.dest_region(&s, base, None)?
        };
        let op = c.flags & cf::OP_MASK;
        if !pixops::op_supported(op) {
            spice_proto::warn_once_per!(op, "composite operator {op} is not supported; using OVER");
        }
        let full = Rect::new(i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2);
        let mut src = self.get_src(&c.src_bitmap, fmt, full)?;
        if c.flags & cf::SOURCE_OPAQUE != 0 && src.img.format == Format::Argb32 {
            Arc::make_mut(&mut src.img).format = Format::Xrgb32;
        }
        let mask = match &c.mask_bitmap {
            Some(m) if c.flags & cf::HAS_MASK != 0 => Some(self.get_src(m, fmt, full)?),
            _ => None,
        };
        if region.is_empty() {
            return Ok(());
        }
        let filter = |bits: u32| matches!(bits, 1 | 2 | 4 | 5 | 6);
        let to_t = |t: &spice_proto::msg::display::Transform| t.t.map(|v| v as i32 as i64);
        let src_pic = pixops::Picture {
            data: &src.img.data,
            width: src.img.width as i64,
            height: src.img.height as i64,
            format: src.img.format,
            transform: c.src_transform.as_ref().map(to_t),
            repeat: pixops::Repeat::from_bits((c.flags >> cf::SRC_REPEAT_SHIFT) & 3),
            bilinear: filter((c.flags >> cf::SRC_FILTER_SHIFT) & 7),
        };
        let mask_pic = mask.as_ref().map(|m| pixops::Picture {
            data: &m.img.data,
            width: m.img.width as i64,
            height: m.img.height as i64,
            format: m.img.format,
            transform: c.mask_transform.as_ref().map(to_t),
            repeat: pixops::Repeat::from_bits((c.flags >> cf::MASK_REPEAT_SHIFT) & 3),
            bilinear: filter((c.flags >> cf::MASK_FILTER_SHIFT) & 7),
        });
        let component_alpha = c.flags & cf::COMPONENT_ALPHA != 0;
        let mut s = surface.lock().unwrap();
        let dest_fmt = s.image_format();
        let w = s.width as usize;
        let bbox = base.bbox;
        for rc in region.rects() {
            for y in rc.top..rc.bottom {
                for x in rc.left..rc.right {
                    let (i, j) = ((x - bbox.left) as i64, (y - bbox.top) as i64);
                    let sp = src_pic.sample(c.src_origin.x as i64 + i, c.src_origin.y as i64 + j);
                    let (sv, sa) = match &mask_pic {
                        None => (sp, [sp[0]; 4]),
                        Some(m) => {
                            let mp =
                                m.sample(c.mask_origin.x as i64 + i, c.mask_origin.y as i64 + j);
                            if component_alpha && m.format != Format::A8 {
                                let mc = [mp[0], mp[1], mp[2], mp[3]];
                                (
                                    [0, 1, 2, 3].map(|k| pixops::mul(sp[k], mc[k])),
                                    [0, 1, 2, 3].map(|k| pixops::mul(sp[0], mc[k])),
                                )
                            } else {
                                let v = sp.map(|v| pixops::mul(v, mp[0]));
                                (v, [v[0]; 4])
                            }
                        }
                    };
                    let d = &mut s.pixels[y as usize * w + x as usize];
                    let dp = pixops::unpack(*d, dest_fmt);
                    *d = pixops::pack(pixops::combine(op, sv, sa, dp), dest_fmt);
                }
            }
        }
        s.add_dirty(bbox);
        Ok(())
    }

    /// `canvas_draw_stroke`: zero-width (cosmetic) lines, optionally dashed,
    /// with Bézier segments flattened.
    fn draw_stroke(
        &mut self,
        surface: &SharedSurface,
        fmt: u32,
        base: &DisplayBase,
        st: &Stroke<'_>,
    ) -> Result<()> {
        let region = {
            let s = surface.lock().unwrap();
            self.dest_region(&s, base, None)?
        };
        if region.is_empty() {
            return self.touch_brush(&st.brush, fmt);
        }
        let rop = ropd_to_rop(st.fore_mode, RopInput::Brush, RopInput::Dest);
        let brush = self.resolve_brush(&st.brush, fmt, region.extents())?;
        let (dashes, dash_offset) = if st.attr.flags & spice_proto::enums::line_flags::STYLED != 0 {
            let mut d: Vec<u32> = st
                .attr
                .style
                .iter()
                .map(|&v| fix_to_int(v).max(0) as u32)
                .collect();
            let mut off = 0;
            if st.attr.flags & spice_proto::enums::line_flags::START_WITH_GAP != 0 && !d.is_empty()
            {
                d.rotate_left(1);
                off = d[0];
            }
            (d, off)
        } else {
            (Vec::new(), 0)
        };
        // Collect the polylines (`stroke_lines_*`).
        let mut lines: Vec<Vec<(i32, i32)>> = Vec::new();
        let mut cur: Vec<(i32, i32)> = Vec::new();
        let fixp = |p: &PointFix| (fix_to_int(p.x), fix_to_int(p.y));
        for seg in &st.path {
            let mut pts = &seg.points[..];
            if seg.flags & path_flags::BEGIN != 0 {
                if !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                }
                if let Some(p) = pts.first() {
                    cur.push(fixp(p));
                    pts = &pts[1..];
                }
            }
            if seg.flags & path_flags::BEZIER != 0 {
                let mut last = cur.last().map(|&(x, y)| PointFix {
                    x: x << 4,
                    y: y << 4,
                });
                for c in pts.chunks(3) {
                    if c.len() < 3 {
                        break;
                    }
                    if let Some(p0) = last {
                        subdivide_bezier(&mut cur, p0, c[0], c[1], c[2]);
                    }
                    last = Some(c[2]);
                }
            } else {
                cur.extend(pts.iter().map(fixp));
            }
            if seg.flags & path_flags::END != 0 {
                if seg.flags & path_flags::CLOSE != 0
                    && let Some(&first) = cur.first()
                {
                    cur.push(first);
                }
                lines.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        let mut s = surface.lock().unwrap();
        let w = s.width as usize;
        let bounds = region.extents();
        for line in &lines {
            pixops::zero_width_polyline(line, &dashes, dash_offset, |x, y, on| {
                if !on || !bounds_contains(&bounds, x, y) || !region_contains(&region, x, y) {
                    return;
                }
                let src = match &brush {
                    ResolvedBrush::Solid(c) => *c,
                    ResolvedBrush::Tile(t, pos) => {
                        if t.width == 0 || t.height == 0 {
                            return;
                        }
                        let tx = (x - pos.x).rem_euclid(t.width as i32) as usize;
                        let ty = (y - pos.y).rem_euclid(t.height as i32) as usize;
                        t.data[ty * t.width as usize + tx]
                    }
                };
                let d = &mut s.pixels[y as usize * w + x as usize];
                *d = rop.apply(src, *d);
            });
        }
        s.add_dirty(base.bbox);
        Ok(())
    }

    /// `canvas_draw_text` (sw_canvas): fill the background area, then
    /// composite the foreground brush through the glyph mask.
    fn draw_text(
        &mut self,
        surface: &SharedSurface,
        fmt: u32,
        base: &DisplayBase,
        t: &Text<'_>,
    ) -> Result<()> {
        let region = {
            let s = surface.lock().unwrap();
            self.dest_region(&s, base, None)?
        };
        if region.is_empty() {
            self.touch_brush(&t.fore_brush, fmt)?;
            return self.touch_brush(&t.back_brush, fmt);
        }
        let back = if !t.back_area.is_empty() {
            Some(self.resolve_brush(&t.back_brush, fmt, t.back_area)?)
        } else {
            None
        };
        let fore = self.resolve_brush(&t.fore_brush, fmt, region.extents())?;
        let (mask, origin) = glyph_mask(&t.str)?;
        let mut s = surface.lock().unwrap();
        if let Some(b) = &back {
            let back_region = region.intersect_rect(t.back_area);
            fill_brush(&mut s, &back_region, b, Rop::Copy);
        }
        if matches!(t.fore_brush, Brush::None) {
            s.add_dirty(base.bbox);
            return Ok(());
        }
        let dest_fmt = s.image_format();
        let w = s.width as usize;
        let mrect = Rect::new(
            origin.x,
            origin.y,
            origin.x + mask.width as i32,
            origin.y + mask.height as i32,
        );
        let glyph_region = region.intersect_rect(mrect);
        for rc in glyph_region.rects() {
            for y in rc.top..rc.bottom {
                for x in rc.left..rc.right {
                    let m = mask.data
                        [(y - origin.y) as usize * mask.width as usize + (x - origin.x) as usize]
                        >> 24;
                    if m == 0 {
                        continue;
                    }
                    let bp = match &fore {
                        ResolvedBrush::Solid(c) => *c | 0xff00_0000,
                        ResolvedBrush::Tile(tile, pos) => {
                            if tile.width == 0 || tile.height == 0 {
                                continue;
                            }
                            let tx = (x - pos.x).rem_euclid(tile.width as i32) as usize;
                            let ty = (y - pos.y).rem_euclid(tile.height as i32) as usize;
                            tile.data[ty * tile.width as usize + tx] | 0xff00_0000
                        }
                    };
                    let sp = pixops::unpack(bp, Format::Argb32).map(|c| pixops::mul(c, m));
                    let d = &mut s.pixels[y as usize * w + x as usize];
                    let dp = pixops::unpack(*d, dest_fmt);
                    *d = pixops::pack(
                        pixops::combine(pixops::op::OVER, sp, [sp[0]; 4], dp),
                        dest_fmt,
                    );
                }
            }
        }
        s.add_dirty(base.bbox);
        Ok(())
    }

    /// `canvas_put_image`: draw a decoded video frame into `dest`, scaled
    /// with nearest-neighbour and clipped to `clip` (the stream's region).
    pub fn put_image(
        &mut self,
        surface_id: u32,
        dest: Rect,
        frame: &Image,
        clip: Option<&Region>,
        top_down: bool,
    ) -> Result<()> {
        let surface = self.surface(surface_id)?;
        let mut s = surface.lock().unwrap();
        let mut region = Region::from_rect(intersect(dest, s.bounds()));
        if let Some(c) = clip {
            region = region.intersect(c);
        }
        if region.is_empty() || frame.width == 0 || frame.height == 0 || dest.is_empty() {
            return Ok(());
        }
        let w = s.width as usize;
        let (fw, fh) = (frame.width as i64, frame.height as i64);
        let (dw, dh) = (dest.width() as i64, dest.height() as i64);
        for rc in region.rects() {
            for y in rc.top..rc.bottom {
                let mut sy = ((y - dest.top) as i64 * fh / dh).min(fh - 1);
                if !top_down {
                    sy = fh - 1 - sy;
                }
                let srow = &frame.data[(sy * fw) as usize..((sy + 1) * fw) as usize];
                let drow = &mut s.pixels[y as usize * w..(y as usize + 1) * w];
                for x in rc.left..rc.right {
                    let sx = ((x - dest.left) as i64 * fw / dw).min(fw - 1);
                    drow[x as usize] = srow[sx as usize] & 0x00ff_ffff;
                }
            }
        }
        s.add_dirty(dest);
        Ok(())
    }
}

fn bounds_contains(r: &Rect, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn region_contains(reg: &Region, x: i32, y: i32) -> bool {
    reg.rects().any(|r| bounds_contains(&r, x, y))
}

/// Scale `src_area` of `src` to the size of `dest` into a new image placed
/// at `dest`'s origin (`canvas_scale_surface` / colorkey scaling).
fn scaled_copy(src: &Src, src_area: Rect, dest: Rect, nearest: bool) -> Src {
    let (w, h) = (dest.width().max(0) as u32, dest.height().max(0) as u32);
    let mut tmp = SurfaceData::new(0, w, h, surface_fmt::ARGB32);
    let region = Region::from_rect(Rect::new(0, 0, w as i32, h as i32));
    let mode = if nearest {
        scale_mode::NEAREST
    } else {
        scale_mode::INTERPOLATE
    };
    scale(
        &mut tmp,
        &region,
        src,
        src_area,
        Rect::new(0, 0, w as i32, h as i32),
        mode,
        Rop::Copy,
    );
    Src {
        img: Arc::new(Image::from_data(w, h, src.img.format, tmp.pixels)),
        ox: dest.left,
        oy: dest.top,
    }
}

/// `subdivide_bezier`: flatten a cubic Bézier until it deviates by less
/// than half a pixel.
fn subdivide_bezier(
    out: &mut Vec<(i32, i32)>,
    p0: PointFix,
    p1: PointFix,
    p2: PointFix,
    p3: PointFix,
) {
    fn dot(x1: i32, y1: i32, x2: i32, y2: i32) -> i64 {
        (x1 as i64 * x2 as i64 + y1 as i64 * y2 as i64) >> 4
    }
    let a2 = dot(p1.x - p0.x, p1.y - p0.y, p1.x - p0.x, p1.y - p0.y);
    let b2 = dot(p3.x - p0.x, p3.y - p0.y, p3.x - p0.x, p3.y - p0.y);
    let c2 = dot(p2.x - p3.x, p2.y - p3.y, p2.x - p3.x, p2.y - p3.y);
    let ab = dot(p1.x - p0.x, p1.y - p0.y, p3.x - p0.x, p3.y - p0.y);
    let cb = dot(p2.x - p3.x, p2.y - p3.y, p0.x - p3.x, p0.y - p3.y);
    let h1 = (a2.saturating_mul(b2) - ab.saturating_mul(ab)) >> 3;
    let h2 = (c2.saturating_mul(b2) - cb.saturating_mul(cb)) >> 3;
    let tiny = (p3.x - p0.x).abs() < 16
        && (p3.y - p0.y).abs() < 16
        && (p1.x - p0.x).abs() < 16
        && (p2.x - p3.x).abs() < 16;
    if (h1 < b2 && h2 < b2) || tiny {
        out.push((fix_to_int(p3.x), fix_to_int(p3.y)));
        return;
    }
    let mid = |a: PointFix, b: PointFix| PointFix {
        x: (a.x + b.x) / 2,
        y: (a.y + b.y) / 2,
    };
    let p01 = mid(p0, p1);
    let p12 = mid(p1, p2);
    let p23 = mid(p2, p3);
    let p012 = mid(p01, p12);
    let p123 = mid(p12, p23);
    let p0123 = mid(p012, p123);
    subdivide_bezier(out, p0, p01, p012, p0123);
    subdivide_bezier(out, p0123, p123, p23, p3);
}

/// `canvas_get_str_mask`: combine the glyphs of a string into one A8 mask.
/// Glyph rows are stored bottom-up; 1-bpp glyphs are MSB first and 4-bpp
/// values become `v << 4`, as the C code does.
fn glyph_mask(s: &SpiceString<'_>) -> Result<(Image, Point)> {
    use spice_proto::enums::string_flags as sf;
    let bpp = if s.flags & sf::RASTER_A1 != 0 {
        1
    } else if s.flags & sf::RASTER_A4 != 0 {
        4
    } else {
        8
    };
    let boxes: Vec<Rect> = s
        .glyphs
        .iter()
        .map(|g| {
            let top = g.render_pos.y + g.glyph_origin.y;
            let left = g.render_pos.x + g.glyph_origin.x;
            Rect::new(left, top, left + g.width as i32, top + g.height as i32)
        })
        .collect();
    let Some(first) = boxes.first() else {
        return Ok((
            Image::from_data(0, 0, Format::A8, Vec::new()),
            Point::default(),
        ));
    };
    let b = boxes.iter().fold(*first, |a, r| {
        Rect::new(
            a.left.min(r.left),
            a.top.min(r.top),
            a.right.max(r.right),
            a.bottom.max(r.bottom),
        )
    });
    let mut mask = Image::new(
        b.width().max(0) as u32,
        b.height().max(0) as u32,
        Format::A8,
    )?;
    let mw = mask.width as usize;
    for (g, gb) in s.glyphs.iter().zip(&boxes) {
        let (gw, gh) = (g.width as usize, g.height as usize);
        let stride = (gw * bpp).div_ceil(8);
        for row in 0..gh {
            // bottom-up source rows
            let src = &g.data[(gh - 1 - row) * stride..(gh - row) * stride];
            let dy = (gb.top - b.top) as usize + row;
            for col in 0..gw {
                let v: u32 = match bpp {
                    1 => {
                        if src[col / 8] & (0x80 >> (col % 8)) != 0 {
                            255
                        } else {
                            0
                        }
                    }
                    4 => {
                        let byte = src[col / 2];
                        (if col % 2 == 0 {
                            byte & 0xf0
                        } else {
                            (byte << 4) & 0xf0
                        }) as u32
                    }
                    _ => src[col] as u32,
                };
                let dx = (gb.left - b.left) as usize + col;
                let d = &mut mask.data[dy * mw + dx];
                *d = (*d >> 24).max(v) << 24;
            }
        }
    }
    Ok((
        mask,
        Point {
            x: b.left,
            y: b.top,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spice_proto::enums::{bitmap_fmt, ropd};
    use spice_proto::msg::display::{Bitmap, ImageDescriptor};

    fn base(l: i32, t: i32, r: i32, b: i32) -> DisplayBase {
        DisplayBase {
            surface_id: 0,
            bbox: Rect::new(l, t, r, b),
            clip: Clip::None,
        }
    }

    fn px(c: &Canvas, x: usize, y: usize) -> u32 {
        let s = c.surface(0).unwrap();
        let s = s.lock().unwrap();
        s.pixels[y * s.width as usize + x]
    }

    fn bitmap_image<'a>(id: u64, flags: u8, w: u32, h: u32, data: &'a [u8]) -> ImageMsg<'a> {
        ImageMsg {
            descriptor: ImageDescriptor {
                id,
                ty: image_type::BITMAP,
                flags,
                width: w,
                height: h,
            },
            data: ImageData::Bitmap(Bitmap {
                format: bitmap_fmt::BIT32,
                flags: bitmap_flags::TOP_DOWN,
                x: w,
                y: h,
                stride: w * 4,
                palette: PaletteRef::None,
                data,
            }),
        }
    }

    #[test]
    fn fill_with_clip_and_xor() {
        let mut c = Canvas::default();
        c.create_surface(0, 8, 8, surface_fmt::XRGB32).unwrap();
        let fill = Fill {
            brush: Brush::Solid(0x00ff00),
            rop_descriptor: ropd::OP_PUT,
            mask: QMask::default(),
        };
        let mut b = base(0, 0, 8, 8);
        b.clip = Clip::Rects(vec![Rect::new(2, 2, 4, 4), Rect::new(3, 3, 5, 5)]);
        c.draw(&b, &DrawOp::Fill(fill)).unwrap();
        assert_eq!(px(&c, 2, 2), 0x00ff00);
        assert_eq!(px(&c, 4, 4), 0x00ff00);
        assert_eq!(px(&c, 4, 2), 0);
        // XOR twice over overlapping clip rects must touch each pixel once.
        let fill = Fill {
            brush: Brush::Solid(0xffffff),
            rop_descriptor: ropd::OP_XOR,
            mask: QMask::default(),
        };
        c.draw(&b, &DrawOp::Fill(fill)).unwrap();
        assert_eq!(px(&c, 3, 3), 0xff00ff);
        let s = c.surface(0).unwrap();
        assert_eq!(s.lock().unwrap().dirty, Some(Rect::new(0, 0, 8, 8)));
    }

    #[test]
    fn copy_caches_and_reuses_images() {
        let mut c = Canvas::default();
        c.create_surface(0, 4, 4, surface_fmt::XRGB32).unwrap();
        let data: Vec<u8> = [1u32, 2, 3, 4]
            .iter()
            .flat_map(|p| p.to_le_bytes())
            .collect();
        let copy = Copy {
            src_bitmap: Some(bitmap_image(77, image_flags::CACHE_ME, 2, 2, &data)),
            src_area: Rect::new(0, 0, 2, 2),
            rop_descriptor: ropd::OP_PUT,
            scale_mode: 0,
            mask: QMask::default(),
        };
        c.draw(&base(1, 1, 3, 3), &DrawOp::Copy(copy)).unwrap();
        assert_eq!(
            [px(&c, 1, 1), px(&c, 2, 1), px(&c, 1, 2), px(&c, 2, 2)],
            [1, 2, 3, 4]
        );

        let from_cache = ImageMsg {
            descriptor: ImageDescriptor {
                id: 77,
                ty: image_type::FROM_CACHE,
                flags: 0,
                width: 2,
                height: 2,
            },
            data: ImageData::FromCache,
        };
        let copy = Copy {
            src_bitmap: Some(from_cache),
            src_area: Rect::new(1, 1, 2, 2),
            rop_descriptor: ropd::OP_PUT,
            scale_mode: 0,
            mask: QMask::default(),
        };
        c.draw(&base(0, 0, 1, 1), &DrawOp::Copy(copy.clone()))
            .unwrap();
        assert_eq!(px(&c, 0, 0), 4);
        c.inval_image(77);
        assert!(matches!(
            c.draw(&base(0, 0, 1, 1), &DrawOp::Copy(copy)),
            Err(CanvasError::NoImage(77))
        ));
    }

    #[test]
    fn noop_still_caches() {
        let mut c = Canvas::default();
        c.create_surface(0, 4, 4, surface_fmt::XRGB32).unwrap();
        let data = 9u32.to_le_bytes();
        let copy = Copy {
            src_bitmap: Some(bitmap_image(5, image_flags::CACHE_ME, 1, 1, &data)),
            src_area: Rect::new(0, 0, 1, 1),
            rop_descriptor: ropd::OP_PUT,
            scale_mode: 0,
            mask: QMask::default(),
        };
        // bbox entirely off-surface: nothing drawn, but the image is cached.
        c.draw(&base(10, 10, 11, 11), &DrawOp::Copy(copy)).unwrap();
        assert!(c.images.contains_key(&5));
    }

    #[test]
    fn copy_bits_overlapping() {
        let mut c = Canvas::default();
        let s = c.create_surface(0, 4, 1, surface_fmt::XRGB32).unwrap();
        s.lock().unwrap().pixels = vec![1, 2, 3, 4];
        c.copy_bits(&CopyBits {
            base: base(1, 0, 4, 1),
            src_pos: Point { x: 0, y: 0 },
        })
        .unwrap();
        assert_eq!(s.lock().unwrap().pixels, [1, 1, 2, 3]);
    }

    #[test]
    fn mask_limits_fill() {
        let mut c = Canvas::default();
        c.create_surface(0, 4, 1, surface_fmt::XRGB32).unwrap();
        let mask_data = [0b0000_0101u8];
        let mask = QMask {
            flags: 0,
            pos: Point { x: 0, y: 0 },
            bitmap: Some(ImageMsg {
                descriptor: ImageDescriptor {
                    id: 1,
                    ty: image_type::BITMAP,
                    flags: 0,
                    width: 4,
                    height: 1,
                },
                data: ImageData::Bitmap(Bitmap {
                    format: bitmap_fmt::BIT1_LE,
                    flags: bitmap_flags::TOP_DOWN,
                    x: 4,
                    y: 1,
                    stride: 1,
                    palette: PaletteRef::None,
                    data: &mask_data,
                }),
            }),
        };
        let fill = Fill {
            brush: Brush::Solid(7),
            rop_descriptor: ropd::OP_PUT,
            mask: mask.clone(),
        };
        c.draw(&base(0, 0, 4, 1), &DrawOp::Fill(fill)).unwrap();
        let s = c.surface(0).unwrap();
        assert_eq!(s.lock().unwrap().pixels, [7, 0, 7, 0]);
        let fill = Fill {
            brush: Brush::Solid(9),
            rop_descriptor: ropd::OP_PUT,
            mask: QMask {
                flags: mask_flags::INVERS,
                ..mask
            },
        };
        c.draw(&base(0, 0, 4, 1), &DrawOp::Fill(fill)).unwrap();
        assert_eq!(s.lock().unwrap().pixels, [7, 9, 7, 9]);
    }

    #[test]
    fn scaled_copy_nearest() {
        let mut c = Canvas::default();
        c.create_surface(0, 4, 2, surface_fmt::XRGB32).unwrap();
        let data: Vec<u8> = [1u32, 2].iter().flat_map(|p| p.to_le_bytes()).collect();
        let copy = Copy {
            src_bitmap: Some(bitmap_image(0, 0, 2, 1, &data)),
            src_area: Rect::new(0, 0, 2, 1),
            rop_descriptor: ropd::OP_PUT,
            scale_mode: scale_mode::NEAREST,
            mask: QMask::default(),
        };
        c.draw(&base(0, 0, 4, 2), &DrawOp::Copy(copy)).unwrap();
        let s = c.surface(0).unwrap();
        assert_eq!(s.lock().unwrap().pixels, [1, 1, 2, 2, 1, 1, 2, 2]);
    }

    #[test]
    fn pattern_brush_tiles() {
        let mut c = Canvas::default();
        c.create_surface(0, 4, 1, surface_fmt::XRGB32).unwrap();
        let data: Vec<u8> = [5u32, 6].iter().flat_map(|p| p.to_le_bytes()).collect();
        let fill = Fill {
            brush: Brush::Pattern {
                image: bitmap_image(0, 0, 2, 1, &data),
                pos: Point { x: 1, y: 0 },
            },
            rop_descriptor: ropd::OP_PUT,
            mask: QMask::default(),
        };
        c.draw(&base(0, 0, 4, 1), &DrawOp::Fill(fill)).unwrap();
        let s = c.surface(0).unwrap();
        assert_eq!(s.lock().unwrap().pixels, [6, 5, 6, 5]);
    }

    fn bgra(px: &[u32]) -> Vec<u8> {
        px.iter().flat_map(|p| p.to_le_bytes()).collect()
    }

    fn surface_px(c: &Canvas) -> Vec<u32> {
        c.surface(0).unwrap().lock().unwrap().pixels.clone()
    }

    #[test]
    fn text_a1_glyph_over_solid() {
        use spice_proto::msg::display::{RasterGlyph, SpiceString, Text};
        let mut c = Canvas::default();
        c.create_surface(0, 4, 2, surface_fmt::XRGB32).unwrap();
        // A 3x2 glyph; rows are stored bottom-up: bottom row "111", top "100".
        let data = [0b1110_0000u8, 0b1000_0000];
        let text = Text {
            str: SpiceString {
                flags: spice_proto::enums::string_flags::RASTER_A1,
                glyphs: vec![RasterGlyph {
                    render_pos: Point { x: 1, y: 0 },
                    glyph_origin: Point { x: 0, y: 0 },
                    width: 3,
                    height: 2,
                    data: &data,
                }],
            },
            back_area: Rect::new(0, 0, 4, 2),
            fore_brush: Brush::Solid(0x00ff_0000),
            back_brush: Brush::Solid(0x0000_00ff),
            fore_mode: ropd::OP_PUT,
            back_mode: ropd::OP_PUT,
        };
        c.draw(&base(0, 0, 4, 2), &DrawOp::Text(text)).unwrap();
        assert_eq!(
            surface_px(&c),
            [
                0xff, 0xff0000, 0xff, 0xff, 0xff, 0xff0000, 0xff0000, 0xff0000
            ]
        );
    }

    #[test]
    fn alpha_blend_and_transparent() {
        use spice_proto::msg::display::{AlphaBlend, Transparent};
        let mut c = Canvas::default();
        let s = c.create_surface(0, 2, 1, surface_fmt::XRGB32).unwrap();
        s.lock().unwrap().pixels = vec![0x0000_00ff, 0x0000_00ff];
        // Half-transparent premultiplied red over blue, then a constant 50%.
        let mut img = bitmap_image(0, 0, 2, 1, &[]);
        let data = bgra(&[0x8080_0000, 0xff00_ff00]);
        if let ImageData::Bitmap(b) = &mut img.data {
            b.format = bitmap_fmt::RGBA;
            b.data = &data;
        }
        let ab = AlphaBlend {
            alpha_flags: 0,
            alpha: 255,
            src_bitmap: Some(img.clone()),
            src_area: Rect::new(0, 0, 2, 1),
        };
        c.draw(&base(0, 0, 2, 1), &DrawOp::AlphaBlend(ab)).unwrap();
        assert_eq!(surface_px(&c), [0x0080_007f, 0x0000_ff00]);
        // Colour key: pixels equal to the key are skipped.
        s.lock().unwrap().pixels = vec![7, 7];
        let key_data = bgra(&[0x0012_3456, 0x0000_0001]);
        let mut key_img = bitmap_image(0, 0, 2, 1, &[]);
        if let ImageData::Bitmap(b) = &mut key_img.data {
            b.data = &key_data;
        }
        let t = Transparent {
            src_bitmap: Some(key_img),
            src_area: Rect::new(0, 0, 2, 1),
            src_color: 0,
            true_color: 0x0012_3456,
        };
        c.draw(&base(0, 0, 2, 1), &DrawOp::Transparent(t)).unwrap();
        assert_eq!(surface_px(&c), [7, 1]);
    }

    #[test]
    fn rop3_patinvert_and_opaque() {
        use spice_proto::msg::display::{Opaque, Rop3};
        let mut c = Canvas::default();
        let s = c.create_surface(0, 2, 1, surface_fmt::XRGB32).unwrap();
        s.lock().unwrap().pixels = vec![0x0f, 0xf0];
        let data = bgra(&[0, 0]);
        let r = Rop3 {
            src_bitmap: Some(bitmap_image(0, 0, 2, 1, &data)),
            src_area: Rect::new(0, 0, 2, 1),
            brush: Brush::Solid(0xff),
            rop3: 0x5a, // PATINVERT: P ^ D
            scale_mode: 0,
            mask: QMask::default(),
        };
        c.draw(&base(0, 0, 2, 1), &DrawOp::Rop3(r)).unwrap();
        assert_eq!(surface_px(&c), [0xf0, 0x0f]);
        // Opaque: copy the source, then brush XOR it.
        let data = bgra(&[0x11, 0x22]);
        let o = Opaque {
            src_bitmap: Some(bitmap_image(0, 0, 2, 1, &data)),
            src_area: Rect::new(0, 0, 2, 1),
            brush: Brush::Solid(0x01),
            rop_descriptor: ropd::OP_XOR,
            scale_mode: 0,
            mask: QMask::default(),
        };
        c.draw(&base(0, 0, 2, 1), &DrawOp::Opaque(o)).unwrap();
        assert_eq!(surface_px(&c), [0x10, 0x23]);
    }

    #[test]
    fn composite_over_with_a8_mask() {
        use spice_proto::enums::composite_flags as cf;
        use spice_proto::msg::display::{Composite, Point16};
        let mut c = Canvas::default();
        let s = c.create_surface(0, 2, 1, surface_fmt::XRGB32).unwrap();
        s.lock().unwrap().pixels = vec![0, 0];
        let src = bgra(&[0x00ff_ffff]);
        let mask = [0xffu8, 0, 0, 0, 0x80, 0, 0, 0]; // A8 bitmap, stride 4
        let mut mask_img = bitmap_image(0, 0, 2, 1, &mask);
        if let ImageData::Bitmap(b) = &mut mask_img.data {
            b.format = bitmap_fmt::BIT8_A;
            b.stride = 4;
        }
        let comp = Composite {
            flags: pixops::op::OVER | cf::HAS_MASK | (1 << cf::SRC_REPEAT_SHIFT),
            src_bitmap: bitmap_image(0, 0, 1, 1, &src),
            mask_bitmap: Some(mask_img),
            src_transform: None,
            mask_transform: None,
            src_origin: Point16::default(),
            mask_origin: Point16 { x: 0, y: 0 },
        };
        // The a8 mask bitmap is 2 pixels with stride 4: pixel 0 = 0xff, pixel 1 = 0x00.
        c.draw(&base(0, 0, 2, 1), &DrawOp::Composite(comp)).unwrap();
        assert_eq!(surface_px(&c), [0x00ff_ffff, 0]);
    }

    #[test]
    fn stroke_polyline() {
        use spice_proto::msg::display::{LineAttr, PathSeg, PointFix, Stroke};
        let mut c = Canvas::default();
        c.create_surface(0, 4, 3, surface_fmt::XRGB32).unwrap();
        let st = Stroke {
            path: vec![PathSeg {
                flags: path_flags::BEGIN | path_flags::END,
                points: vec![PointFix { x: 0, y: 16 }, PointFix { x: 48, y: 16 }],
            }],
            attr: LineAttr {
                flags: 0,
                style: vec![],
            },
            brush: Brush::Solid(9),
            fore_mode: ropd::OP_PUT,
            back_mode: ropd::OP_PUT,
        };
        c.draw(&base(0, 0, 4, 3), &DrawOp::Stroke(st)).unwrap();
        // CapNotLast: x = 0..3 on row 1, not x = 3.
        assert_eq!(&surface_px(&c)[4..8], &[9, 9, 9, 0]);
        assert!(surface_px(&c)[..4].iter().all(|&p| p == 0));
    }

    #[test]
    fn put_image_scales_and_flips() {
        let mut c = Canvas::default();
        c.create_surface(0, 4, 2, surface_fmt::XRGB32).unwrap();
        let frame = Image::from_data(2, 1, Format::Xrgb32, vec![0xff00_0001, 2]);
        c.put_image(0, Rect::new(0, 0, 4, 2), &frame, None, false)
            .unwrap();
        assert_eq!(surface_px(&c), [1, 1, 2, 2, 1, 1, 2, 2]);
        let clip = Region::from_rect(Rect::new(0, 0, 1, 1));
        let frame = Image::from_data(1, 1, Format::Xrgb32, vec![5]);
        c.put_image(0, Rect::new(0, 0, 4, 2), &frame, Some(&clip), true)
            .unwrap();
        assert_eq!(surface_px(&c)[..2], [5, 1]);
    }
}
