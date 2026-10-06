//! The software canvas: surfaces, clipping, raster ops and the image and
//! palette caches. Replaces spice-common's `canvas_base.c` + `sw_canvas.c`
//! and the parts of pixman they use.

#![forbid(unsafe_code)]

mod canvas;
pub mod region;
pub mod rop;
mod surface;

pub use canvas::{Canvas, CanvasError, Result};
pub use region::Region;
pub use rop::Rop;
pub use spice_decode::{Format, Image};
pub use spice_proto::msg::display::Rect;
pub use surface::{SharedSurface, SurfaceData};
