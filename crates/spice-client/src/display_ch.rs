//! The display channel (`channel-display.c`): surfaces, draw operations and
//! caches, driving `spice-canvas`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use log::{info, warn};
use spice_canvas::{Canvas, CanvasError, SharedSurface};
use spice_proto::caps::{self, Caps};
use spice_proto::enums::{msg, surface_flags, surface_fmt};
use spice_proto::msg::display as d;
use spice_proto::{chan_log, warn_once_per};

use crate::channel::{Channel, Message};
use crate::error::Result;
use crate::events::ClientEvent;
use crate::video::{self, Frame, Stream};

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
    streams: HashMap<u32, Stream>,
}

impl DisplayChannel {
    /// `spice_display_channel_set_capabilities`. MJPEG is the only
    /// video codec for now (H.264 is a later phase).
    pub fn caps() -> Caps {
        let mut c = Caps::new()
            .with(caps::display::SIZED_STREAM)
            .with(caps::display::MONITORS_CONFIG)
            .with(caps::display::COMPOSITE)
            .with(caps::display::A8_SURFACE)
            .with(caps::display::LZ4_COMPRESSION)
            .with(caps::display::MULTI_CODEC)
            .with(caps::display::CODEC_MJPEG);
        if std::env::var_os("SPICE_DISABLE_ADAPTIVE_STREAMING").is_none() {
            c.set(caps::display::STREAM_REPORT);
        }
        c
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
            streams: HashMap::new(),
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
        loop {
            let next_frame = self.next_frame_due();
            tokio::select! {
                m = self.ch.next() => match m? {
                    Some(m) => self.handle(m).await?,
                    None => return Ok(()),
                },
                _ = tokio::time::sleep(next_frame.unwrap_or_default()), if next_frame.is_some() => {
                    self.show_due_frames();
                }
            }
            self.notify_primary();
        }
    }

    // ---- video streams ---------------------------------------------------

    fn next_frame_due(&self) -> Option<Duration> {
        let now = self.ch.session.mm_time();
        self.streams
            .values()
            .filter_map(|s| s.due_in(now))
            .min()
            .map(|ms| Duration::from_millis(ms as u64))
    }

    /// `mjpeg_decoder_decode_frame` for every stream with a frame due.
    fn show_due_frames(&mut self) {
        let now = self.ch.session.mm_time();
        let ids: Vec<u32> = self.streams.keys().copied().collect();
        for id in ids {
            while let Some(frame) = self.streams.get_mut(&id).and_then(|s| s.take_due(now)) {
                let st = &self.streams[&id];
                let (surface, flags, clip) = (st.surface_id, st.flags, st.clip.clone());
                match spice_decode::jpeg::decode(&frame.data) {
                    Ok(img) => {
                        let top_down = flags & spice_proto::enums::stream_flags::TOP_DOWN != 0;
                        if let Err(e) = self.canvas.put_image(
                            surface,
                            frame.dest,
                            &img,
                            clip.as_ref(),
                            top_down,
                        ) {
                            warn!("{}: stream {id}: {e}", self.name);
                        }
                    }
                    Err(e) => warn!("{}: stream {id}: bad MJPEG frame: {e}", self.name),
                }
            }
        }
    }

    fn report_invalid_stream(&self, id: u32) {
        if self.ch.remote_has_cap(caps::display::STREAM_REPORT) {
            warn!(
                "{}: telling the server stream {id} doesn't exist",
                self.name
            );
            self.ch.tx.send(&video::invalid_stream_report(id));
        }
    }

    /// `display_handle_stream_data`
    fn stream_data(&mut self, data: d::StreamData<'_>, body: &bytes::Bytes) {
        let id = data.id;
        let now = self.ch.session.mm_time();
        let audio_delay = self.ch.session.playback_delay().unwrap_or(u32::MAX);
        let Some(st) = self.streams.get_mut(&id) else {
            self.report_invalid_stream(id);
            return;
        };
        let mut mm = data.multi_media_time;
        if mm == 0 {
            warn_once_per!(
                id,
                "{}: frame with invalid 0 timestamp; wrong guest driver?",
                self.name
            );
            mm = now.unwrap_or(0).wrapping_add(100);
        }
        let margin = now.map_or(0, |n| video::mm_diff(mm, n));
        let dest = data.sized.map_or(st.dest, |(_, _, r)| r);
        st.queue_frame(
            Frame {
                mm_time: mm,
                dest,
                data: body.slice_ref(data.data),
            },
            now,
        );
        if let Some(report) = st.update_report(id, mm, margin, audio_delay) {
            chan_log!(
                debug,
                self,
                "stream {id} report: {} frames, {} drops, last delay {}",
                report.num_frames,
                report.num_drops,
                report.last_frame_delay
            );
            self.ch.tx.send(&report);
        }
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
            msg::display::STREAM_CREATE => {
                let c = d::StreamCreate::decode(b)?;
                chan_log!(
                    debug,
                    self,
                    "stream {} create: codec {}, {}x{} -> {:?}",
                    c.id,
                    c.codec_type,
                    c.src_width,
                    c.src_height,
                    c.dest
                );
                if c.codec_type != spice_proto::enums::video_codec::MJPEG {
                    warn!(
                        "{}: stream codec {} is not supported",
                        self.name, c.codec_type
                    );
                    self.report_invalid_stream(c.id);
                } else {
                    if self.streams.contains_key(&c.id) {
                        warn!("{}: stream {} created twice", self.name, c.id);
                    }
                    self.streams
                        .insert(c.id, Stream::new(c.surface_id, c.flags, c.dest, &c.clip));
                }
            }
            msg::display::STREAM_DATA | msg::display::STREAM_DATA_SIZED => {
                let data = d::StreamData::decode(b, m.ty == msg::display::STREAM_DATA_SIZED)?;
                self.stream_data(data, &m.body);
                // Frames due right now go out without a timer round trip.
                self.show_due_frames();
            }
            msg::display::STREAM_CLIP => {
                let (id, clip) = d::decode_stream_clip(b)?;
                match self.streams.get_mut(&id) {
                    Some(st) => st.clip = video::clip_region(&clip),
                    None => self.report_invalid_stream(id),
                }
            }
            msg::display::STREAM_DESTROY => {
                let id = d::decode_stream_destroy(b)?;
                if let Some(st) = self.streams.remove(&id) {
                    chan_log!(
                        debug,
                        self,
                        "stream {id} destroyed: {} frames shown, {} dropped",
                        st.frames_shown,
                        st.frames_dropped
                    );
                }
            }
            msg::display::STREAM_DESTROY_ALL => self.streams.clear(),
            msg::display::STREAM_ACTIVATE_REPORT => {
                let r = d::StreamActivateReport::decode(b)?;
                match self.streams.get_mut(&r.stream_id) {
                    Some(st) => st.activate_report(r.unique_id, r.max_window_size, r.timeout_ms),
                    None => self.report_invalid_stream(r.stream_id),
                }
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
        if primary && let Some((pid, s)) = &self.primary {
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
