//! The SDL3 frontend: window, rendering, input and audio. Runs on the main
//! thread; talks to the network thread through [`ClientEvent`]s (pushed via
//! an mpsc queue plus an SDL wakeup event) and [`ClientCommand`]s.

mod window;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use anyhow::{Context, anyhow};
use log::{debug, info, warn};
use sdl3::event::{Event, EventSender, WindowEvent};
use spice_client::{ClientCommand, ClientEvent, EventSink, SessionConfig};

use crate::net::NetThread;
use window::DisplayWindow;

/// Frontend options that don't concern the session itself.
#[derive(Debug, Clone)]
pub struct UiOptions {
    pub fullscreen: bool,
    pub title_host: String,
}

/// The SDL user event that wakes `wait_event` when client events are queued.
struct Wake;

enum AppEvent {
    Client(ClientEvent),
    SessionEnded,
}

/// Bridges client events from the network thread into the SDL loop.
struct Bridge {
    tx: mpsc::Sender<AppEvent>,
    wake: EventSender,
    pending: Arc<AtomicBool>,
}

impl Bridge {
    fn push(&self, ev: AppEvent) {
        let _ = self.tx.send(ev);
        // One wakeup per batch: the UI clears `pending` before draining.
        if !self.pending.swap(true, Ordering::AcqRel) {
            let _ = self.wake.push_custom_event(Wake);
        }
    }
}

impl EventSink for Bridge {
    fn emit(&self, ev: ClientEvent) {
        self.push(AppEvent::Client(ev));
    }
}

pub fn run(cfg: SessionConfig, opts: UiOptions) -> anyhow::Result<()> {
    let sdl = sdl3::init().map_err(|e| anyhow!("SDL init failed: {e}"))?;
    let video = sdl
        .video()
        .map_err(|e| anyhow!("SDL video init failed: {e}"))?;
    let events = sdl
        .event()
        .map_err(|e| anyhow!("SDL events init failed: {e}"))?;
    events
        .register_custom_event::<Wake>()
        .map_err(|e| anyhow!("SDL custom event: {e}"))?;
    info!("SDL video driver: {}", video.current_video_driver());

    let (tx, rx) = mpsc::channel();
    let pending = Arc::new(AtomicBool::new(false));
    let bridge = Arc::new(Bridge {
        tx: tx.clone(),
        wake: events.event_sender(),
        pending: pending.clone(),
    });
    let exit_bridge = bridge.clone();
    let net = NetThread::spawn(cfg, bridge, move || {
        exit_bridge.push(AppEvent::SessionEnded)
    })?;

    let mut pump = sdl
        .event_pump()
        .map_err(|e| anyhow!("SDL event pump: {e}"))?;
    let mut win = DisplayWindow::new(&video, &opts).context("creating window")?;
    let texture_creator = win.texture_creator();
    let mut texture = None;

    let mut running = true;
    while running {
        let ev = pump.wait_event();
        let mut redraw = false;
        for ev in std::iter::once(ev).chain(pump.poll_iter()) {
            match ev {
                Event::Quit { .. } => {
                    info!("quit requested");
                    net.send(ClientCommand::Quit);
                }
                Event::Window { win_event, .. } => match win_event {
                    WindowEvent::CloseRequested => net.send(ClientCommand::Quit),
                    WindowEvent::Exposed
                    | WindowEvent::Resized(..)
                    | WindowEvent::PixelSizeChanged(..)
                    | WindowEvent::Shown
                    | WindowEvent::Restored => redraw = true,
                    _ => {}
                },
                Event::User { .. } if ev.as_user_event_type::<Wake>().is_some() => {
                    pending.store(false, Ordering::Release);
                    while let Ok(app_ev) = rx.try_recv() {
                        match app_ev {
                            AppEvent::SessionEnded => running = false,
                            AppEvent::Client(ce) => {
                                redraw |= handle_client_event(
                                    ce,
                                    &mut win,
                                    &texture_creator,
                                    &mut texture,
                                )?
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if redraw {
            win.present(texture.as_ref());
        }
        if !running || net.is_finished() {
            break;
        }
    }
    drop(texture);
    net.join().map_err(Into::into)
}

fn handle_client_event<'tc>(
    ev: ClientEvent,
    win: &mut DisplayWindow,
    tc: &'tc sdl3::render::TextureCreator<sdl3::video::WindowContext>,
    texture: &mut Option<sdl3::render::Texture<'tc>>,
) -> anyhow::Result<bool> {
    match ev {
        ClientEvent::Name(name) => win.set_vm_name(&name),
        ClientEvent::DisplayPrimary {
            display,
            surface,
            width,
            height,
        } => {
            if display != 0 {
                warn!("display {display}: only one display is supported for now");
                return Ok(false);
            }
            *texture = Some(win.set_primary(tc, surface, width, height)?);
            return Ok(true);
        }
        ClientEvent::DisplayPrimaryDestroyed { display: 0 } => {
            win.clear_primary();
            *texture = None;
            return Ok(true);
        }
        ClientEvent::Invalidate { display: 0 } => {
            if let Some(t) = texture.as_mut() {
                return Ok(win.upload_dirty(t));
            }
        }
        ClientEvent::DisplayMark { display: 0, mark } => {
            debug!("display mark: {mark}");
            win.set_mark(mark);
            return Ok(true);
        }
        other => debug!("client event: {other:?}"),
    }
    Ok(false)
}
