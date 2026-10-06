//! The inputs channel (`channel-inputs.c`): keys, lock keys, and mouse
//! motion with `MOUSE_MOTION_ACK` flow control.

use std::sync::Arc;

use spice_proto::caps::{self, Caps};
use spice_proto::enums::{SPICE_INPUT_MOTION_ACK_BUNCH, mouse_button, mouse_button_mask, msg};
use spice_proto::msg::inputs as i;
use spice_proto::{chan_log, warn_once_per};
use tokio::sync::mpsc;

use crate::channel::{Channel, Message};
use crate::error::Result;
use crate::events::ClientEvent;

/// Input commands routed to the inputs channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputCmd {
    /// An "xtkbd" scancode (`0x100 | code` for 0xe0-prefixed keys).
    KeyDown(u32),
    KeyUp(u32),
    /// Press and release atomically (`KEY_SCANCODE` when supported).
    KeyPressRelease(u32),
    /// `SpiceKeyboardModifierFlags` for the guest's lock keys.
    SetKeyLocks(u16),
    /// Relative motion (server mouse mode).
    Motion {
        dx: i32,
        dy: i32,
        buttons: u16,
    },
    /// Absolute position (client mouse mode).
    Position {
        x: u32,
        y: u32,
        display: u8,
        buttons: u16,
    },
    Press {
        button: u8,
        buttons: u16,
    },
    Release {
        button: u8,
        buttons: u16,
    },
}

pub struct InputsChannel {
    ch: Channel,
    name: Arc<str>,
    cmds: mpsc::UnboundedReceiver<InputCmd>,
    motion_count: i32,
    dx: i32,
    dy: i32,
    buttons: u16,
    /// Pending absolute position, if any (`dpy == -1` in C means none).
    position: Option<(u32, u32, u8)>,
}

impl InputsChannel {
    pub fn caps() -> Caps {
        Caps::new().with(caps::inputs::KEY_SCANCODE)
    }

    pub fn new(ch: Channel, cmds: mpsc::UnboundedReceiver<InputCmd>) -> Self {
        let name = ch.name.clone();
        Self {
            ch,
            name,
            cmds,
            motion_count: 0,
            dx: 0,
            dy: 0,
            buttons: 0,
            position: None,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        loop {
            tokio::select! {
                m = self.ch.next() => match m? {
                    Some(m) => self.handle(m)?,
                    None => return Ok(()),
                },
                Some(cmd) = self.cmds.recv() => self.command(cmd),
            }
        }
    }

    fn handle(&mut self, m: Message) -> Result<()> {
        match m.ty {
            msg::inputs::INIT | msg::inputs::KEY_MODIFIERS => {
                let mods = i::decode_modifiers(&m.body)?;
                chan_log!(debug, self, "guest key modifiers {mods:#x}");
                self.ch.session.emit(ClientEvent::KeyModifiers(mods));
            }
            msg::inputs::MOUSE_MOTION_ACK => {
                self.motion_count -= SPICE_INPUT_MOTION_ACK_BUNCH as i32;
                self.send_motion();
                self.send_position();
            }
            other => warn_once_per!(other, "{}: unhandled message {other}", self.name),
        }
        Ok(())
    }

    /// `mouse_motion`: flush accumulated relative motion.
    fn send_motion(&mut self) {
        if self.dx == 0 && self.dy == 0 {
            return;
        }
        self.ch.tx.send(&i::MouseMotion {
            dx: self.dx,
            dy: self.dy,
            buttons: self.buttons,
        });
        self.motion_count += 1;
        self.dx = 0;
        self.dy = 0;
    }

    /// `mouse_position`: flush the latest absolute position.
    fn send_position(&mut self) {
        let Some((x, y, display_id)) = self.position.take() else {
            return;
        };
        self.ch.tx.send(&i::MousePosition {
            x,
            y,
            buttons: self.buttons,
            display_id,
        });
        self.motion_count += 1;
    }

    fn under_ack_limit(&self) -> bool {
        self.motion_count < SPICE_INPUT_MOTION_ACK_BUNCH as i32 * 2
    }

    fn command(&mut self, cmd: InputCmd) {
        match cmd {
            InputCmd::KeyDown(sc) => self.ch.tx.send(&i::KeyDown(sc)),
            InputCmd::KeyUp(sc) => self.ch.tx.send(&i::KeyUp(sc)),
            InputCmd::KeyPressRelease(sc) => {
                if self.ch.remote_has_cap(caps::inputs::KEY_SCANCODE) {
                    self.ch.tx.send(&i::KeyPressRelease(sc));
                } else {
                    self.ch.tx.send(&i::KeyDown(sc));
                    self.ch.tx.send(&i::KeyUp(sc));
                }
            }
            InputCmd::SetKeyLocks(locks) => self.ch.tx.send(&i::KeyModifiers(locks)),
            InputCmd::Motion { dx, dy, buttons } => {
                if dx == 0 && dy == 0 {
                    return;
                }
                self.buttons = buttons;
                self.dx += dx;
                self.dy += dy;
                if self.under_ack_limit() {
                    self.send_motion();
                }
            }
            InputCmd::Position {
                x,
                y,
                display,
                buttons,
            } => {
                self.buttons = buttons;
                self.position = Some((x, y, display));
                if self.under_ack_limit() {
                    self.send_position();
                } else {
                    chan_log!(
                        trace,
                        self,
                        "over SPICE_INPUT_MOTION_ACK_BUNCH * 2, dropping"
                    );
                }
            }
            InputCmd::Press { button, buttons } => {
                // spice_inputs_channel_button_press
                let buttons = buttons | button_mask(button);
                self.buttons = buttons;
                self.send_motion();
                self.send_position();
                self.ch.tx.send(&i::MousePress { button, buttons });
            }
            InputCmd::Release { button, buttons } => {
                let buttons = buttons & !button_mask(button);
                self.buttons = buttons;
                self.send_motion();
                self.send_position();
                self.ch.tx.send(&i::MouseRelease { button, buttons });
            }
        }
    }
}

/// The state-mask bit for a button; wheel "buttons" have none in spice-gtk.
fn button_mask(button: u8) -> u16 {
    match button {
        mouse_button::LEFT => mouse_button_mask::LEFT,
        mouse_button::MIDDLE => mouse_button_mask::MIDDLE,
        mouse_button::RIGHT => mouse_button_mask::RIGHT,
        mouse_button::SIDE => mouse_button_mask::SIDE,
        mouse_button::EXTRA => mouse_button_mask::EXTRA,
        _ => 0,
    }
}
