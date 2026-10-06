//! The display channel (`channel-display.c`): surfaces, draw operations and
//! caches, driving `spice-canvas`.

use std::sync::Arc;

use log::{info, warn};
use spice_canvas::{Canvas, CanvasError, SharedSurface};
use spice_proto::caps::{self, Caps};
use spice_proto::enums::{msg, surface_flags, surface_fmt};
use spice_proto::msg::display as d;
use spice_proto::{chan_log, warn_once_per};

use crate::channel::{Channel, Message};
use crate::error::Result;
use crate::events::ClientEvent;

/// `IMAGES_CACHE_SIZE_DEFAULT` in spice-session.c (bytes).
const IMAGES_CACHE_SIZE: i64 = 1024 * 1024 * 80;
/// `MAX_GLZ_WINDOW_SIZE_DEFAULT`: min(LZ_MAX_WINDOW_SIZE * 4, 64 MiB).
const GLZ_WINDOW_SIZE: i32 = 1024 * 1024 * 64;

pub struct DisplayChannel {
    ch: Channel,
    name: Arc<str>,
    canvas: Canvas,
    primary: Option<(u32, SharedSurface)>,
    mark: bool,
}

impl DisplayChannel {
    /// `spice_display_channel_set_capabilities`
    pub fn caps() -> Caps {
        Caps::new()
            .with(caps::display::SIZED_STREAM)
            .with(caps::display::MONITORS_CONFIG)
            .with(caps::display::LZ4_COMPRESSION)
            .with(caps::display::MULTI_CODEC)
    }

    pub fn new(ch: Channel) -> Self {
        let name = ch.name.clone();
        let canvas = Canvas::new(ch.session.glz_window.clone());
        Self {
            ch,
            name,
            canvas,
            primary: None,
            mark: false,
        }
    }

    fn display(&self) -> u8 {
        self.ch.id
    }

    /// `spice_display_channel_up`
    fn channel_up(&mut self) {
        self.ch.tx.send(&d::Init {
            pixmap_cache_id: 1,
            pixmap_cache_size: IMAGES_CACHE_SIZE / 4,
            glz_dictionary_id: 1,
            glz_dictionary_window_size: GLZ_WINDOW_SIZE / 4,
        });
        if let Some(c) = self.ch.session.config.preferred_compression {
            if self.ch.remote_has_cap(caps::display::PREF_COMPRESSION) {
                chan_log!(debug, self, "changing preferred compression to {c}");
                self.ch.tx.send(&d::PreferredCompression(c));
            } else {
                chan_log!(info, self, "server can't change the preferred compression");
            }
        }
    }

    pub async fn run(mut self) -> Result<()> {
        self.channel_up();
        while let Some(m) = self.ch.next().await? {
            self.handle(m).await?;
            self.notify_primary();
        }
        Ok(())
    }

    fn notify_primary(&mut self) {
        if let Some((_, s)) = &self.primary {
            let mut s = s.lock().unwrap();
            if s.dirty.is_some() && !s.notify_pending {
                s.notify_pending = true;
                drop(s);
                self.ch.session.emit(ClientEvent::Invalidate {
                    display: self.display(),
                });
            }
        }
    }

    async fn handle(&mut self, m: Message) -> Result<()> {
        let b = &m.body;
        match m.ty {
            msg::display::DRAW_FILL..=msg::display::DRAW_ALPHA_BLEND
            | msg::display::DRAW_COMPOSITE => {
                let (base, op) = d::decode_draw(m.ty, b)?;
                let r = if op.uses_glz() {
                    // GLZ may wait for an image another display channel is
                    // still decoding; let tokio move other tasks off this thread.
                    tokio::task::block_in_place(|| self.canvas.draw(&base, &op))
                } else {
                    self.canvas.draw(&base, &op)
                };
                if let Err(e) = r {
                    self.draw_error(m.ty, op.name(), e);
                }
            }
            msg::display::COPY_BITS => {
                let cb = d::CopyBits::decode(b)?;
                if let Err(e) = self.canvas.copy_bits(&cb) {
                    self.draw_error(m.ty, "copy_bits", e);
                }
            }
            msg::display::SURFACE_CREATE => {
                let c = d::SurfaceCreate::decode(b)?;
                self.surface_create(
                    c.surface_id,
                    c.width,
                    c.height,
                    c.format,
                    c.flags & surface_flags::PRIMARY != 0,
                );
            }
            msg::display::SURFACE_DESTROY => {
                let id = d::decode_surface_destroy(b)?;
                if self.primary.as_ref().is_some_and(|(pid, _)| *pid == id) {
                    chan_log!(debug, self, "primary surface {id} destroyed");
                    self.primary = None;
                    self.ch.session.emit(ClientEvent::DisplayPrimaryDestroyed {
                        display: self.display(),
                    });
                }
                self.canvas.destroy_surface(id);
            }
            msg::display::MODE => {
                // Pre-surface protocol: the mode message creates the primary.
                let mode = d::Mode::decode(b)?;
                let fmt = if mode.bits == 32 {
                    surface_fmt::XRGB32
                } else {
                    surface_fmt::RGB555
                };
                self.surface_create(0, mode.x_res, mode.y_res, fmt, true);
            }
            msg::display::MARK => {
                chan_log!(debug, self, "mark");
                self.set_mark(true);
            }
            msg::display::RESET => {
                chan_log!(debug, self, "reset");
                if let Some((id, _)) = &self.primary {
                    self.canvas.clear_surface(*id);
                }
                self.canvas.clear_palettes();
                self.set_mark(false);
            }
            msg::display::INVAL_LIST => {
                for (ty, id) in d::decode_inval_list(b)? {
                    if ty == 1 {
                        // SPICE_RES_TYPE_PIXMAP
                        if !self.canvas.inval_image(id) {
                            chan_log!(debug, self, "failed to remove image {id:#x}");
                        }
                    }
                }
            }
            msg::display::INVAL_ALL_PIXMAPS => {
                let w = d::decode_inval_all_pixmaps(b)?;
                self.ch.wait_for_channels(&w).await;
                self.canvas.clear_images();
            }
            msg::display::INVAL_PALETTE => self.canvas.inval_palette(d::decode_inval_one(b)?),
            msg::display::INVAL_ALL_PALETTES => self.canvas.clear_palettes(),
            msg::display::MONITORS_CONFIG => {
                let mc = d::MonitorsConfig::decode(b)?;
                if mc.heads.is_empty() {
                    chan_log!(debug, self, "received empty monitor config");
                } else {
                    for h in &mc.heads {
                        chan_log!(
                            debug,
                            self,
                            "monitor id {}, surface {}, +{}+{} {}x{}",
                            h.monitor_id,
                            h.surface_id,
                            h.x,
                            h.y,
                            h.width,
                            h.height
                        );
                    }
                    self.ch.session.emit(ClientEvent::MonitorsConfig {
                        display: self.display(),
                        max_allowed: mc.max_allowed,
                        heads: mc.heads,
                    });
                }
            }
            msg::display::STREAM_CREATE
            | msg::display::STREAM_DATA
            | msg::display::STREAM_DATA_SIZED
            | msg::display::STREAM_CLIP
            | msg::display::STREAM_DESTROY
            | msg::display::STREAM_DESTROY_ALL
            | msg::display::STREAM_ACTIVATE_REPORT => {
                warn_once_per!(
                    m.ty,
                    "{}: video streams are not supported yet (message {})",
                    self.name,
                    m.ty
                );
            }
            msg::display::GL_SCANOUT_UNIX
            | msg::display::GL_SCANOUT2_UNIX
            | msg::display::GL_DRAW => {
                warn_once_per!(m.ty, "{}: GL scanout is not supported", self.name);
            }
            msg::display::QUALITY_INDICATOR => {}
            other => warn_once_per!(other, "{}: unhandled message {other}", self.name),
        }
        Ok(())
    }

    fn draw_error(&self, ty: u16, op: &str, e: CanvasError) {
        match e {
            CanvasError::Unsupported(what) => {
                warn_once_per!(
                    ty,
                    "{}: {op}: {what} is not supported yet; skipping",
                    self.name
                );
            }
            e => warn!("{}: {op} failed: {e}", self.name),
        }
    }

    fn set_mark(&mut self, mark: bool) {
        if self.mark != mark {
            self.mark = mark;
            self.ch.session.emit(ClientEvent::DisplayMark {
                display: self.display(),
                mark,
            });
        }
    }

    /// `display_handle_surface_create` + `create_canvas`.
    fn surface_create(&mut self, id: u32, width: u32, height: u32, format: u32, primary: bool) {
        chan_log!(
            debug,
            self,
            "create {}surface {id}: {width}x{height} format {format}",
            if primary { "primary " } else { "" }
        );
        if primary {
            if let Some((pid, s)) = &self.primary {
                let same = {
                    let s = s.lock().unwrap();
                    s.width == width && s.height == height
                };
                if same && *pid == id {
                    chan_log!(debug, self, "reusing existing primary surface");
                    return;
                }
                let pid = *pid;
                self.canvas.destroy_surface(pid);
                self.primary = None;
                self.ch.session.emit(ClientEvent::DisplayPrimaryDestroyed {
                    display: self.display(),
                });
            }
        }
        match self.canvas.create_surface(id, width, height, format) {
            Ok(s) => {
                if primary {
                    info!("{}: primary surface {width}x{height}", self.name);
                    self.primary = Some((id, s.clone()));
                    self.ch.session.emit(ClientEvent::DisplayPrimary {
                        display: self.display(),
                        surface: s,
                        width,
                        height,
                    });
                }
            }
            Err(e) => warn!("{}: cannot create surface {id}: {e}", self.name),
        }
    }
}
