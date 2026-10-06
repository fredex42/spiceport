//! The SDL3 frontend: window, rendering, input and audio. Runs on the main
//! thread; talks to the network thread through [`ClientEvent`]s (pushed via
//! an mpsc queue plus an SDL wakeup event) and [`ClientCommand`]s.

mod cursor;
mod input;
mod keymap;
mod window;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use anyhow::{Context, anyhow};
use log::{debug, info, warn};
use sdl3::event::{Event, EventSender, WindowEvent};
use sdl3::keyboard::KeyboardUtil;
use sdl3::mouse::MouseUtil;
use sdl3::render::{Texture, TextureCreator};
use sdl3::video::WindowContext;
use spice_client::{ClientCommand, ClientEvent, EventSink, MouseMode, SessionConfig};

use crate::net::NetThread;
use cursor::CursorState;
pub use input::GrabSequence;
use input::Input;
use window::DisplayWindow;

/// Frontend options that don't concern the session itself.
#[derive(Debug, Clone)]
pub struct UiOptions {
    pub fullscreen: bool,
    pub title_host: String,
    pub release_keys: GrabSequence,
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

struct App<'tc> {
    win: DisplayWindow,
    tc: &'tc TextureCreator<WindowContext>,
    display_tex: Option<Texture<'tc>>,
    cursor_tex: Option<Texture<'tc>>,
    input: Input,
    cursor: CursorState,
    mouse: MouseUtil,
    keyboard: KeyboardUtil,
    net: NetThread,
    focused: bool,
    out: Vec<ClientCommand>,
    redraw: bool,
    clipboard: sdl3::clipboard::ClipboardUtil,
    /// Text we put on the clipboard from the guest, so the resulting
    /// clipboard-update event isn't echoed back.
    clipboard_from_guest: Option<String>,
    agent_connected: bool,
    have_primary: bool,
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
        tx,
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
    let win = DisplayWindow::new(&video, &opts).context("creating window")?;
    let tc = win.texture_creator();
    let mut app = App {
        win,
        tc: &tc,
        display_tex: None,
        cursor_tex: None,
        input: Input::new(opts.release_keys.clone()),
        cursor: CursorState::new(),
        mouse: sdl.mouse(),
        keyboard: sdl.keyboard(),
        net,
        focused: false,
        out: Vec::new(),
        redraw: true,
        clipboard: video.clipboard(),
        clipboard_from_guest: None,
        agent_connected: false,
        have_primary: false,
    };
    app.win.set_release_hint(&app.input.release_hint());

    let mut running = true;
    while running {
        let first = pump.wait_event();
        for ev in std::iter::once(first).chain(pump.poll_iter()) {
            if let Event::User { .. } = ev
                && ev.as_user_event_type::<Wake>().is_some()
            {
                pending.store(false, Ordering::Release);
                while let Ok(app_ev) = rx.try_recv() {
                    match app_ev {
                        AppEvent::SessionEnded => running = false,
                        AppEvent::Client(ce) => app.client_event(ce)?,
                    }
                }
                continue;
            }
            app.sdl_event(ev);
        }
        app.flush();
        if app.redraw {
            app.redraw = false;
            app.present();
        }
        if !running || app.net.is_finished() {
            break;
        }
    }
    let App {
        net,
        display_tex,
        cursor_tex,
        ..
    } = app;
    drop((display_tex, cursor_tex));
    net.join().map_err(Into::into)
}

impl<'tc> App<'tc> {
    fn flush(&mut self) {
        for c in self.out.drain(..) {
            self.net.send(c);
        }
    }

    fn present(&mut self) {
        let sprite = match (
            self.input.mouse_mode,
            self.input.mouse_grabbed,
            self.cursor.visible,
        ) {
            (MouseMode::Server, true, true) => self
                .cursor
                .guest_pos
                .zip(self.cursor.shape.as_ref())
                .map(|(p, s)| {
                    (
                        p.0 - s.hot_x as i32,
                        p.1 - s.hot_y as i32,
                        s.width,
                        s.height,
                    )
                }),
            _ => None,
        };
        let cursor =
            sprite.and_then(|(x, y, w, h)| self.cursor_tex.as_ref().map(|t| (t, x, y, w, h)));
        self.win.present(self.display_tex.as_ref(), cursor);
    }

    fn set_mouse_grab(&mut self, on: bool) {
        if on == self.input.mouse_grabbed {
            return;
        }
        debug!("pointer grab {}", if on { "on" } else { "off" });
        self.input.mouse_grabbed = on;
        self.mouse.set_relative_mouse_mode(self.win.window(), on);
        self.win.window_mut().set_mouse_grab(on);
        if on {
            self.set_keyboard_grab(true);
        }
        self.win.set_grabbed(on);
        self.redraw = true;
    }

    fn set_keyboard_grab(&mut self, on: bool) {
        let on = on && self.focused && !self.input.keyboard_grab_released;
        self.win.window_mut().set_keyboard_grab(on);
    }

    fn set_mouse_mode(&mut self, mode: MouseMode) {
        self.input.mouse_mode = mode;
        if mode == MouseMode::Client {
            self.set_mouse_grab(false);
        }
        self.cursor.apply(&self.mouse, mode);
        self.redraw = true;
    }

    fn client_event(&mut self, ev: ClientEvent) -> anyhow::Result<()> {
        match ev {
            ClientEvent::Name(name) => self.win.set_vm_name(&name),
            ClientEvent::DisplayPrimary {
                display,
                surface,
                width,
                height,
            } => {
                if display != 0 {
                    warn!("display {display}: only one display is supported for now");
                    return Ok(());
                }
                self.display_tex = Some(self.win.set_primary(self.tc, surface, width, height)?);
                self.have_primary = true;
                self.redraw = true;
            }
            ClientEvent::DisplayPrimaryDestroyed { display: 0 } => {
                self.win.clear_primary();
                self.display_tex = None;
                self.redraw = true;
            }
            ClientEvent::Invalidate { display: 0 } => {
                if let Some(t) = self.display_tex.as_mut() {
                    self.redraw |= self.win.upload_dirty(t);
                }
            }
            ClientEvent::DisplayMark { display: 0, mark } => {
                debug!("display mark: {mark}");
                self.win.set_mark(mark);
                self.redraw = true;
            }
            ClientEvent::MouseMode(m) => self.set_mouse_mode(m),
            ClientEvent::CursorSet(shape) => {
                self.cursor.shape = shape;
                self.cursor.visible = true;
                self.cursor_tex = self.cursor.texture(self.tc);
                self.cursor.apply(&self.mouse, self.input.mouse_mode);
                self.redraw = true;
            }
            ClientEvent::CursorHide => {
                self.cursor.visible = false;
                self.cursor.apply(&self.mouse, self.input.mouse_mode);
                self.redraw = true;
            }
            ClientEvent::CursorMove { x, y } => {
                self.cursor.guest_pos = Some((x, y));
                if !self.cursor.visible {
                    // cursor_move: "apparently we have to restore cursor"
                    self.cursor.visible = true;
                    self.cursor.apply(&self.mouse, self.input.mouse_mode);
                }
                self.redraw = true;
            }
            ClientEvent::CursorReset => {
                self.cursor.shape = None;
                self.cursor_tex = None;
                self.cursor.apply(&self.mouse, self.input.mouse_mode);
            }
            ClientEvent::AgentConnected(c) => {
                self.agent_connected = c;
                if c {
                    // Offer whatever the client clipboard holds already.
                    self.clipboard_changed();
                    self.send_display_size();
                }
            }
            ClientEvent::ClipboardText(t) => {
                if let Err(e) = self.clipboard.set_clipboard_text(&t) {
                    warn!("setting the clipboard failed: {e}");
                }
                self.clipboard_from_guest = Some(t);
            }
            ClientEvent::KeyModifiers(locks) => {
                self.input.guest_locks_changed(locks);
                self.input
                    .sync_locks(self.keyboard.mod_state(), false, &mut self.out);
            }
            other => debug!("client event: {other:?}"),
        }
        Ok(())
    }

    fn sdl_event(&mut self, mut ev: Event) {
        if ev.is_mouse() {
            // Window pixels → guest display pixels (handles letterboxing and HiDPI).
            self.win.to_display_coords(&mut ev);
        }
        let size = self.win.display_size();
        match ev {
            Event::Quit { .. } => {
                info!("quit requested");
                self.net.send(ClientCommand::Quit);
            }
            Event::Window { win_event, .. } => match win_event {
                WindowEvent::CloseRequested => self.net.send(ClientCommand::Quit),
                WindowEvent::FocusGained => {
                    self.focused = true;
                    // focus_in_event
                    self.input.release_keys(&mut self.out);
                    self.input
                        .sync_locks(self.keyboard.mod_state(), false, &mut self.out);
                    self.set_keyboard_grab(true);
                }
                WindowEvent::FocusLost => {
                    self.focused = false;
                    self.input.focus_lost(&mut self.out);
                    self.set_mouse_grab(false);
                    self.set_keyboard_grab(false);
                }
                WindowEvent::MouseEnter => {
                    // motion_event: re-grab the keyboard after a sequence release.
                    if self.input.keyboard_grab_released && self.focused {
                        self.input.keyboard_grab_released = false;
                        self.input.release_keys(&mut self.out);
                        self.set_keyboard_grab(true);
                    }
                }
                WindowEvent::PixelSizeChanged(..) => {
                    self.send_display_size();
                    self.redraw = true;
                }
                WindowEvent::Exposed
                | WindowEvent::Resized(..)
                | WindowEvent::Shown
                | WindowEvent::Restored => self.redraw = true,
                _ => {}
            },
            Event::KeyDown {
                scancode: Some(sc),
                keymod,
                repeat,
                ..
            } => {
                let act = self.input.key(sc, keymod, true, repeat, &mut self.out);
                self.actions(act);
            }
            Event::KeyUp {
                scancode: Some(sc),
                keymod,
                ..
            } => {
                let act = self.input.key(sc, keymod, false, false, &mut self.out);
                self.actions(act);
            }
            Event::MouseMotion {
                x, y, xrel, yrel, ..
            } => self
                .input
                .mouse_motion(x, y, xrel, yrel, size, &mut self.out),
            Event::MouseButtonDown {
                mouse_btn, x, y, ..
            } => {
                if self
                    .input
                    .mouse_button(mouse_btn, true, x, y, size, &mut self.out)
                {
                    self.set_mouse_grab(true);
                }
            }
            Event::MouseButtonUp {
                mouse_btn, x, y, ..
            } => {
                self.input
                    .mouse_button(mouse_btn, false, x, y, size, &mut self.out);
            }
            Event::MouseWheel { y, direction, .. } => {
                let y = if direction == sdl3::mouse::MouseWheelDirection::Flipped {
                    -y
                } else {
                    y
                };
                self.input.wheel(y, &mut self.out);
            }
            Event::ClipboardUpdate { .. } => self.clipboard_changed(),
            Event::RenderTargetsReset { .. } | Event::RenderDeviceReset { .. } => {
                // Textures may be lost: re-upload everything.
                if let Some(t) = self.display_tex.as_mut() {
                    self.win.mark_all_dirty();
                    self.win.upload_dirty(t);
                }
                self.cursor_tex = self.cursor.texture(self.tc);
                self.redraw = true;
            }
            _ => {}
        }
    }

    /// Tell the guest (through the agent) how big the window is, so it can
    /// resize its display to fit (`recalc_geometry` in spice-widget.c).
    fn send_display_size(&mut self) {
        // Before the first primary surface the window has a placeholder
        // size that mustn't shrink the guest.
        if !self.have_primary || !self.agent_connected {
            return;
        }
        let (w, h) = self.win.window().size_in_pixels();
        self.out.push(ClientCommand::DisplaySize {
            display: 0,
            width: w,
            height: h,
        });
    }

    fn clipboard_changed(&mut self) {
        if !self.clipboard.has_clipboard_text() {
            return;
        }
        let Ok(t) = self.clipboard.clipboard_text() else {
            return;
        };
        if self.clipboard_from_guest.as_deref() == Some(t.as_str()) {
            return;
        }
        self.clipboard_from_guest = None;
        self.out.push(ClientCommand::ClipboardText(t));
    }

    fn actions(&mut self, act: input::Actions) {
        if let Some(g) = act.grab_mouse {
            self.set_mouse_grab(g);
        }
        if let Some(g) = act.grab_keyboard {
            self.set_keyboard_grab(g);
        }
        if act.toggle_fullscreen {
            self.win.toggle_fullscreen();
        }
        if act.quit {
            info!("quit shortcut");
            self.net.send(ClientCommand::Quit);
        }
    }
}
