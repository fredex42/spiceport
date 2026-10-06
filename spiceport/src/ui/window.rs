//! A window showing one display channel's primary surface.

use anyhow::anyhow;
use log::{debug, warn};
use sdl3::pixels::{Color, PixelFormat};
use sdl3::render::{Texture, TextureCreator, WindowCanvas};
use sdl3::sys::render::SDL_RendererLogicalPresentation;
use sdl3::video::WindowContext;
use spice_client::SharedSurface;

use super::UiOptions;

pub struct DisplayWindow {
    canvas: WindowCanvas,
    surface: Option<SharedSurface>,
    size: (u32, u32),
    mark: bool,
    vm_name: Option<String>,
    host: String,
}

impl DisplayWindow {
    pub fn new(video: &sdl3::VideoSubsystem, opts: &UiOptions) -> anyhow::Result<Self> {
        let mut b = video.window("spiceport", 640, 480);
        b.resizable().high_pixel_density();
        if opts.fullscreen {
            b.fullscreen();
        }
        let window = b.build().map_err(|e| anyhow!("{e}"))?;
        let mut canvas = window.into_canvas();
        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();
        canvas.present();
        let mut w = Self {
            canvas,
            surface: None,
            size: (0, 0),
            mark: false,
            vm_name: None,
            host: opts.title_host.clone(),
        };
        w.update_title();
        Ok(w)
    }

    pub fn texture_creator(&self) -> TextureCreator<WindowContext> {
        self.canvas.texture_creator()
    }

    pub fn set_vm_name(&mut self, name: &str) {
        self.vm_name = Some(name.to_string());
        self.update_title();
    }

    fn update_title(&mut self) {
        let base = self.vm_name.as_deref().unwrap_or(&self.host);
        let title = format!("{base} - spiceport");
        if let Err(e) = self.canvas.window_mut().set_title(&title) {
            warn!("set title: {e}");
        }
    }

    /// A new primary surface: size the window to it and make a texture.
    pub fn set_primary<'tc>(
        &mut self,
        tc: &'tc TextureCreator<WindowContext>,
        surface: SharedSurface,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Texture<'tc>> {
        debug!("primary {width}x{height}");
        let mut tex = tc
            .create_texture_streaming(PixelFormat::XRGB8888, width, height)
            .map_err(|e| anyhow!("create texture: {e}"))?;
        if self.size != (width, height) {
            self.size = (width, height);
            let win = self.canvas.window_mut();
            if win.fullscreen_state() == sdl3::video::FullscreenType::Off && !win.is_maximized() {
                let _ = win.set_size(width, height);
            }
            self.canvas
                .set_logical_size(width, height, SDL_RendererLogicalPresentation::LETTERBOX)
                .map_err(|e| anyhow!("logical size: {e}"))?;
        }
        // Upload everything once; afterwards only dirty areas.
        {
            let mut s = surface.lock().unwrap();
            s.dirty = Some(s.bounds());
        }
        self.surface = Some(surface);
        self.upload_dirty(&mut tex);
        Ok(tex)
    }

    pub fn clear_primary(&mut self) {
        self.surface = None;
    }

    pub fn set_mark(&mut self, mark: bool) {
        self.mark = mark;
    }

    /// Copy the surface's dirty area into the texture. Holds the surface
    /// lock only for the copy.
    pub fn upload_dirty(&mut self, tex: &mut Texture<'_>) -> bool {
        let Some(surface) = &self.surface else {
            return false;
        };
        let mut s = surface.lock().unwrap();
        s.notify_pending = false;
        let Some(d) = s.dirty.take() else {
            return false;
        };
        let w = s.width as usize;
        let start = d.top as usize * w + d.left as usize;
        let pixels: &[u8] = bytemuck::cast_slice(&s.pixels[start..]);
        let rect = sdl3::rect::Rect::new(d.left, d.top, d.width() as u32, d.height() as u32);
        if let Err(e) = tex.update(rect, pixels, w * 4) {
            warn!("texture update failed: {e}");
        }
        true
    }

    pub fn present(&mut self, tex: Option<&Texture<'_>>) {
        self.canvas.set_draw_color(Color::RGB(0, 0, 0));
        self.canvas.clear();
        if let Some(t) = tex
            && self.surface.is_some()
            && let Err(e) = self.canvas.copy(t, None, None)
        {
            warn!("render copy failed: {e}");
        }
        self.canvas.present();
    }
}
