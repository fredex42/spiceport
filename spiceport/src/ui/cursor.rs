//! The guest's cursor: an OS cursor in client mouse mode, a sprite we draw
//! ourselves in server mode (`cursor_set`/`cursor_move`/`cursor_hide` and
//! `update_mouse_cursor` in spice-widget.c).

use std::collections::HashMap;
use std::sync::Arc;

use log::warn;
use sdl3::mouse::{Cursor, MouseUtil};
use sdl3::pixels::PixelFormat;
use sdl3::render::{BlendMode, Texture, TextureCreator};
use sdl3::surface::Surface;
use sdl3::video::WindowContext;
use spice_client::{CursorShape, MouseMode};

pub struct CursorState {
    /// `None` = the default arrow (unknown cursor type).
    pub shape: Option<Arc<CursorShape>>,
    pub visible: bool,
    /// Last `CURSOR_MOVE` position, in guest pixels.
    pub guest_pos: Option<(i32, i32)>,
    sdl_cursors: HashMap<u64, Cursor>,
    blank: Option<Cursor>,
    arrow: Option<Cursor>,
}

impl CursorState {
    pub fn new() -> Self {
        Self {
            shape: None,
            visible: true,
            guest_pos: None,
            sdl_cursors: HashMap::new(),
            blank: None,
            arrow: None,
        }
    }

    fn make_cursor(shape: &CursorShape) -> Option<Cursor> {
        let mut bytes: Vec<u8> = bytemuck::cast_slice(&shape.pixels).to_vec();
        let surface = Surface::from_data(
            &mut bytes,
            shape.width,
            shape.height,
            shape.width * 4,
            PixelFormat::ARGB8888,
        )
        .map_err(|e| warn!("cursor surface: {e}"))
        .ok()?;
        Cursor::from_surface(&surface, shape.hot_x as i32, shape.hot_y as i32)
            .map_err(|e| warn!("create cursor: {e}"))
            .ok()
    }

    fn blank_cursor(&mut self) -> Option<&Cursor> {
        if self.blank.is_none() {
            let blank = CursorShape {
                unique: 0,
                width: 1,
                height: 1,
                hot_x: 0,
                hot_y: 0,
                pixels: vec![0],
            };
            self.blank = Self::make_cursor(&blank);
        }
        self.blank.as_ref()
    }

    /// SDL keeps using a cursor after `set`, so ours must stay alive.
    fn set_arrow(&mut self) {
        if self.arrow.is_none() {
            self.arrow = Cursor::from_system(sdl3::mouse::SystemCursor::Arrow).ok();
        }
        if let Some(c) = &self.arrow {
            c.set();
        }
    }

    /// Apply the OS cursor for the current mode and state.
    pub fn apply(&mut self, mouse: &MouseUtil, mode: MouseMode) {
        match mode {
            MouseMode::Client => {
                if !self.visible {
                    if let Some(c) = self.blank_cursor() {
                        c.set();
                    }
                    return;
                }
                match &self.shape {
                    Some(shape) => {
                        let id = shape.unique;
                        if !self.sdl_cursors.contains_key(&id)
                            && let Some(c) = Self::make_cursor(shape)
                        {
                            // Bound the cache; cursors are cheap to recreate.
                            if self.sdl_cursors.len() > 64 {
                                self.sdl_cursors.clear();
                            }
                            self.sdl_cursors.insert(id, c);
                        }
                        if let Some(c) = self.sdl_cursors.get(&id) {
                            c.set();
                        }
                    }
                    None => self.set_arrow(),
                }
                mouse.show_cursor(true);
            }
            MouseMode::Server => {
                self.set_arrow();
                mouse.show_cursor(true);
            }
        }
    }

    /// A texture of the current shape for drawing the sprite in server mode.
    pub fn texture<'tc>(&self, tc: &'tc TextureCreator<WindowContext>) -> Option<Texture<'tc>> {
        let shape = self.shape.as_ref()?;
        let mut t = tc
            .create_texture_static(PixelFormat::ARGB8888, shape.width, shape.height)
            .map_err(|e| warn!("cursor texture: {e}"))
            .ok()?;
        t.set_blend_mode(BlendMode::Blend);
        let _ = t.update(
            None,
            bytemuck::cast_slice(&shape.pixels),
            shape.width as usize * 4,
        );
        Some(t)
    }
}
