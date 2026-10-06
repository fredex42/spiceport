//! Draw operations and image fetching (`canvas_base.c`, `sw_canvas.c`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use log::{debug, trace};
use spice_decode::glz::GlzWindow;
use spice_decode::image::{rgb555_to_xrgb, rgb565_to_xrgb};
use spice_decode::{DecodeError, Format, Image};
use spice_proto::enums::{
    bitmap_flags, image_flags, image_type, mask_flags, scale_mode, surface_fmt,
};
use spice_proto::msg::display::{
    Brush, Clip, Copy, CopyBits, DisplayBase, DrawOp, Fill, Image as ImageMsg, ImageData,
    PaletteRef, Point, QMask, Rect,
};
use thiserror::Error;

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

    /// Execute a draw message on its target surface. Ops not implemented yet
    /// return `CanvasError::Unsupported`.
    pub fn draw(&mut self, base: &DisplayBase, op: &DrawOp<'_>) -> Result<()> {
        let surface = self.surface(base.surface_id)?;
        let fmt = surface.lock().unwrap().format;
        match op {
            DrawOp::Fill(f) => self.draw_fill(&surface, fmt, base, f),
            DrawOp::Copy(c) => self.draw_copy(&surface, fmt, base, c),
            DrawOp::Blackness(m) => self.draw_solid(&surface, base, m, 0, Rop::Copy),
            DrawOp::Whiteness(m) => self.draw_solid(&surface, base, m, 0xffff_ffff, Rop::Copy),
            DrawOp::Invers(m) => self.draw_solid(&surface, base, m, 0, Rop::Invert),
            other => Err(CanvasError::Unsupported(format!(
                "draw op {}",
                other.name()
            ))),
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
}
