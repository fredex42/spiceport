use std::sync::{Arc, Mutex};

use spice_decode::{Format, Image};
use spice_proto::enums::surface_fmt;
use spice_proto::msg::display::Rect;

/// A drawing surface. Pixels are `0xAARRGGBB` u32s regardless of the SPICE
/// surface format: 16-bit surfaces are kept expanded to 32 bits and `8_A`
/// surfaces keep their alpha in the top byte.
pub struct SurfaceData {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    /// `SpiceSurfaceFmt`
    pub format: u32,
    pub pixels: Vec<u32>,
    /// Union of the areas drawn since the frontend last uploaded.
    pub dirty: Option<Rect>,
    /// Set when an invalidate notification is outstanding, so draws between
    /// two frames produce one notification.
    pub notify_pending: bool,
}

pub type SharedSurface = Arc<Mutex<SurfaceData>>;

impl std::fmt::Debug for SurfaceData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Surface({} {}x{} fmt {})",
            self.id, self.width, self.height, self.format
        )
    }
}

impl SurfaceData {
    pub fn new(id: u32, width: u32, height: u32, format: u32) -> Self {
        Self {
            id,
            width,
            height,
            format,
            pixels: vec![0; width as usize * height as usize],
            dirty: None,
            notify_pending: false,
        }
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    pub fn image_format(&self) -> Format {
        match self.format {
            surface_fmt::ARGB32 => Format::Argb32,
            surface_fmt::A8 | surface_fmt::A1 => Format::A8,
            _ => Format::Xrgb32,
        }
    }

    pub fn is_16bpp(&self) -> bool {
        matches!(self.format, surface_fmt::RGB555 | surface_fmt::RGB565)
    }

    /// Record that `r` changed.
    pub fn add_dirty(&mut self, r: Rect) {
        let r = intersect(r, self.bounds());
        if r.is_empty() {
            return;
        }
        self.dirty = Some(match self.dirty {
            None => r,
            Some(d) => Rect::new(
                d.left.min(r.left),
                d.top.min(r.top),
                d.right.max(r.right),
                d.bottom.max(r.bottom),
            ),
        });
    }

    /// Copy `area` (clipped to the surface) into an image; returns the image
    /// and the position of its top-left pixel.
    pub fn snapshot(&self, area: Rect) -> (Image, i32, i32) {
        let a = intersect(area, self.bounds());
        if a.is_empty() {
            return (
                Image::from_data(0, 0, self.image_format(), Vec::new()),
                0,
                0,
            );
        }
        let (w, h) = (a.width() as usize, a.height() as usize);
        let mut data = Vec::with_capacity(w * h);
        let sw = self.width as usize;
        for y in a.top as usize..a.bottom as usize {
            data.extend_from_slice(
                &self.pixels[y * sw + a.left as usize..y * sw + a.right as usize],
            );
        }
        (
            Image::from_data(w as u32, h as u32, self.image_format(), data),
            a.left,
            a.top,
        )
    }
}

pub fn intersect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
}
