//! Keyboard and mouse handling (`key_event`, `button_event`, `motion_event`,
//! `scroll_event` and the grab logic in spice-widget.c).

use std::collections::BTreeSet;

use log::{debug, info};
use sdl3::keyboard::{Mod, Scancode};
use sdl3::mouse::MouseButton;
use spice_client::{ClientCommand, InputCmd, MouseMode};
use spice_proto_consts::*;

use super::keymap;

/// Re-exported SPICE constants (kept local to avoid a direct spice-proto dep).
mod spice_proto_consts {
    pub const BUTTON_LEFT: u8 = 1;
    pub const BUTTON_MIDDLE: u8 = 2;
    pub const BUTTON_RIGHT: u8 = 3;
    pub const BUTTON_UP: u8 = 4;
    pub const BUTTON_DOWN: u8 = 5;
    pub const BUTTON_SIDE: u8 = 6;
    pub const BUTTON_EXTRA: u8 = 7;
    pub const MASK_LEFT: u16 = 1 << 0;
    pub const MASK_MIDDLE: u16 = 1 << 1;
    pub const MASK_RIGHT: u16 = 1 << 2;
    pub const MASK_SIDE: u16 = 1 << 5;
    pub const MASK_EXTRA: u16 = 1 << 6;
    pub const LOCK_SCROLL: u16 = 1 << 0;
    pub const LOCK_NUM: u16 = 1 << 1;
    pub const LOCK_CAPS: u16 = 1 << 2;
}

/// A key combination like `ctrl+alt` (`spice-grabsequence.c`).
#[derive(Debug, Clone)]
pub struct GrabSequence {
    keys: Vec<Scancode>,
}

impl GrabSequence {
    /// Parse `ctrl+alt`, `lctrl+lalt`, `shift+f12`, ... Unqualified modifier
    /// names mean the left key, as spice-gtk's default `Control_L+Alt_L`.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let mut keys = Vec::new();
        for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            let sc = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "lctrl" => Scancode::LCtrl,
                "rctrl" => Scancode::RCtrl,
                "alt" | "lalt" | "option" => Scancode::LAlt,
                "ralt" | "altgr" => Scancode::RAlt,
                "shift" | "lshift" => Scancode::LShift,
                "rshift" => Scancode::RShift,
                "super" | "win" | "gui" | "cmd" | "lgui" | "lsuper" => Scancode::LGui,
                "rgui" | "rsuper" => Scancode::RGui,
                other => Scancode::from_name(other).ok_or_else(|| {
                    anyhow::anyhow!("unknown key '{part}' in key combination '{s}'")
                })?,
            };
            keys.push(sc);
        }
        if keys.is_empty() {
            anyhow::bail!("empty key combination");
        }
        Ok(Self { keys })
    }

    pub fn describe(&self) -> String {
        self.keys
            .iter()
            .map(|k| match k {
                Scancode::LCtrl | Scancode::RCtrl => "Ctrl".to_string(),
                Scancode::LAlt | Scancode::RAlt => "Alt".to_string(),
                Scancode::LShift | Scancode::RShift => "Shift".to_string(),
                Scancode::LGui | Scancode::RGui => "Super".to_string(),
                k => k.name().to_string(),
            })
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// What the event loop should do after an input event.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Actions {
    pub grab_mouse: Option<bool>,
    pub grab_keyboard: Option<bool>,
    pub toggle_fullscreen: bool,
    pub quit: bool,
}

pub struct Input {
    grab_seq: GrabSequence,
    active: Vec<bool>,
    seq_pressed: bool,
    /// Keys we have sent "down" for and not yet "up".
    pressed: BTreeSet<u32>,
    buttons: u16,
    /// The user released the keyboard grab with the key sequence.
    pub keyboard_grab_released: bool,
    pub mouse_grabbed: bool,
    pub mouse_mode: MouseMode,
    guest_locks: Option<u16>,
    wheel_y: f32,
    frac_x: f32,
    frac_y: f32,
}

impl Input {
    pub fn new(grab_seq: GrabSequence) -> Self {
        let n = grab_seq.keys.len();
        Self {
            grab_seq,
            active: vec![false; n],
            seq_pressed: false,
            pressed: BTreeSet::new(),
            buttons: 0,
            keyboard_grab_released: false,
            mouse_grabbed: false,
            mouse_mode: MouseMode::Server,
            guest_locks: None,
            wheel_y: 0.0,
            frac_x: 0.0,
            frac_y: 0.0,
        }
    }

    pub fn release_hint(&self) -> String {
        self.grab_seq.describe()
    }

    /// `check_for_grab_key`: on presses, mark keys of the sequence; it fires
    /// when all are down. Any release resets it (and vice versa).
    fn check_seq(&mut self, sc: Scancode, pressed: bool, want_pressed: bool) -> bool {
        if pressed == want_pressed {
            for (i, k) in self.grab_seq.keys.iter().enumerate() {
                if *k == sc {
                    self.active[i] = true;
                }
            }
            if self.active.iter().all(|&a| a) {
                self.active.fill(false);
                return true;
            }
            false
        } else {
            // The opposite event type resets the whole sequence.
            self.active.fill(false);
            self.seq_pressed = false;
            false
        }
    }

    pub fn key(
        &mut self,
        sc: Scancode,
        keymod: Mod,
        pressed: bool,
        repeat: bool,
        out: &mut Vec<ClientCommand>,
    ) -> Actions {
        let mut act = Actions::default();
        if !repeat {
            if !self.seq_pressed && self.check_seq(sc, pressed, true) {
                debug!("grab sequence pressed");
                if self.mouse_mode == MouseMode::Server {
                    act.grab_mouse = Some(!self.mouse_grabbed);
                }
                self.seq_pressed = true;
            } else if self.seq_pressed && self.check_seq(sc, pressed, false) {
                debug!("grab sequence released");
                self.release_keys(out);
                self.keyboard_grab_released = !self.keyboard_grab_released;
                act.grab_keyboard = Some(!self.keyboard_grab_released);
                self.seq_pressed = false;
            }
        }

        // Client shortcuts.
        let combo_held = self.grab_seq_held(keymod);
        if pressed && combo_held && sc == Scancode::Q {
            act.quit = true;
            return act;
        }
        if pressed && combo_held && sc == Scancode::End {
            // Ctrl+Alt+Del can't be captured on every OS; send it on demand.
            info!("sending Ctrl+Alt+Del");
            for sc in [0x1d, 0x38, 0x153] {
                out.push(ClientCommand::Input(InputCmd::KeyDown(sc)));
            }
            for sc in [0x153, 0x38, 0x1d] {
                out.push(ClientCommand::Input(InputCmd::KeyUp(sc)));
            }
            return act;
        }
        if pressed && sc == Scancode::F11 && keymod.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD) {
            act.toggle_fullscreen = true;
            return act;
        }

        if sc == Scancode::Pause {
            // send_pause: the hardware sequence is "Ctrl"(e1 1d) + NumLock.
            let cmd = |c| {
                ClientCommand::Input(if pressed {
                    InputCmd::KeyDown(c)
                } else {
                    InputCmd::KeyUp(c)
                })
            };
            out.push(cmd(0x21d));
            out.push(cmd(0x45));
            return act;
        }
        let Some(code) = keymap::xtkbd(sc) else {
            debug!("no PC scancode for {sc:?}");
            return act;
        };
        if pressed {
            self.pressed.insert(code);
            out.push(ClientCommand::Input(InputCmd::KeyDown(code)));
        } else if self.pressed.remove(&code) {
            out.push(ClientCommand::Input(InputCmd::KeyUp(code)));
        }
        act
    }

    /// True while every modifier of the release sequence is held.
    fn grab_seq_held(&self, m: Mod) -> bool {
        self.grab_seq.keys.iter().all(|k| match k {
            Scancode::LCtrl | Scancode::RCtrl => m.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD),
            Scancode::LAlt | Scancode::RAlt => m.intersects(Mod::LALTMOD | Mod::RALTMOD),
            Scancode::LShift | Scancode::RShift => m.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD),
            Scancode::LGui | Scancode::RGui => m.intersects(Mod::LGUIMOD | Mod::RGUIMOD),
            _ => false,
        })
    }

    /// `release_keys`: send "up" for everything we think is down.
    pub fn release_keys(&mut self, out: &mut Vec<ClientCommand>) {
        for code in std::mem::take(&mut self.pressed) {
            out.push(ClientCommand::Input(InputCmd::KeyUp(code)));
        }
    }

    pub fn focus_lost(&mut self, out: &mut Vec<ClientCommand>) {
        self.release_keys(out);
        self.active.fill(false);
        self.seq_pressed = false;
    }

    /// The local lock-key state as `SpiceKeyboardModifierFlags`.
    pub fn client_locks(m: Mod) -> u16 {
        let mut l = 0;
        if m.contains(Mod::NUMMOD) {
            l |= LOCK_NUM;
        }
        if m.contains(Mod::CAPSMOD) {
            l |= LOCK_CAPS;
        }
        if m.contains(Mod::SCROLLMOD) {
            l |= LOCK_SCROLL;
        }
        l
    }

    /// `spice_gtk_session_sync_keyboard_modifiers_for_channel`
    pub fn sync_locks(&mut self, m: Mod, force: bool, out: &mut Vec<ClientCommand>) {
        let client = Self::client_locks(m);
        if force || self.guest_locks != Some(client) {
            debug!(
                "syncing lock keys: client {client:#x}, guest {:?}",
                self.guest_locks
            );
            self.guest_locks = Some(client);
            out.push(ClientCommand::Input(InputCmd::SetKeyLocks(client)));
        }
    }

    pub fn guest_locks_changed(&mut self, locks: u16) {
        self.guest_locks = Some(locks);
    }

    fn button(b: MouseButton) -> Option<(u8, u16)> {
        Some(match b {
            MouseButton::Left => (BUTTON_LEFT, MASK_LEFT),
            MouseButton::Middle => (BUTTON_MIDDLE, MASK_MIDDLE),
            MouseButton::Right => (BUTTON_RIGHT, MASK_RIGHT),
            MouseButton::X1 => (BUTTON_SIDE, MASK_SIDE),
            MouseButton::X2 => (BUTTON_EXTRA, MASK_EXTRA),
            MouseButton::Unknown => return None,
        })
    }

    /// Mouse button; coordinates are in guest display pixels. Returns true
    /// when the click should start a pointer grab instead.
    pub fn mouse_button(
        &mut self,
        b: MouseButton,
        pressed: bool,
        x: f32,
        y: f32,
        size: (u32, u32),
        out: &mut Vec<ClientCommand>,
    ) -> bool {
        let Some((button, mask)) = Self::button(b) else {
            return false;
        };
        let inside = x >= 0.0 && y >= 0.0 && (x as u32) < size.0 && (y as u32) < size.1;
        match self.mouse_mode {
            MouseMode::Client if !inside => return false,
            MouseMode::Server if !self.mouse_grabbed => return pressed,
            _ => {}
        }
        let buttons = self.buttons;
        if pressed {
            self.buttons |= mask;
            out.push(ClientCommand::Input(InputCmd::Press { button, buttons }));
        } else {
            self.buttons &= !mask;
            out.push(ClientCommand::Input(InputCmd::Release { button, buttons }));
        }
        false
    }

    pub fn mouse_motion(
        &mut self,
        x: f32,
        y: f32,
        xrel: f32,
        yrel: f32,
        size: (u32, u32),
        out: &mut Vec<ClientCommand>,
    ) {
        match self.mouse_mode {
            MouseMode::Client => {
                if x >= 0.0 && y >= 0.0 && (x as u32) < size.0 && (y as u32) < size.1 {
                    out.push(ClientCommand::Input(InputCmd::Position {
                        x: x as u32,
                        y: y as u32,
                        display: 0,
                        buttons: self.buttons,
                    }));
                }
            }
            MouseMode::Server if self.mouse_grabbed => {
                // Keep sub-pixel remainders so slow movement isn't lost.
                self.frac_x += xrel;
                self.frac_y += yrel;
                let (dx, dy) = (self.frac_x.trunc(), self.frac_y.trunc());
                self.frac_x -= dx;
                self.frac_y -= dy;
                if dx != 0.0 || dy != 0.0 {
                    out.push(ClientCommand::Input(InputCmd::Motion {
                        dx: dx as i32,
                        dy: dy as i32,
                        buttons: self.buttons,
                    }));
                }
            }
            MouseMode::Server => {}
        }
    }

    /// `scroll_event`: each notch is a press+release of UP/DOWN.
    pub fn wheel(&mut self, y: f32, out: &mut Vec<ClientCommand>) {
        if self.mouse_mode == MouseMode::Server && !self.mouse_grabbed {
            return;
        }
        self.wheel_y += y;
        while self.wheel_y.abs() >= 1.0 {
            let button = if self.wheel_y > 0.0 {
                self.wheel_y -= 1.0;
                BUTTON_UP
            } else {
                self.wheel_y += 1.0;
                BUTTON_DOWN
            };
            let buttons = self.buttons;
            out.push(ClientCommand::Input(InputCmd::Press { button, buttons }));
            out.push(ClientCommand::Input(InputCmd::Release { button, buttons }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds(v: &[ClientCommand]) -> Vec<InputCmd> {
        v.iter()
            .map(|c| match c {
                ClientCommand::Input(i) => *i,
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn parse_sequences() {
        assert_eq!(
            GrabSequence::parse("ctrl+alt").unwrap().describe(),
            "Ctrl+Alt"
        );
        assert!(GrabSequence::parse("ctrl+nope").is_err());
        assert!(GrabSequence::parse("").is_err());
        assert_eq!(
            GrabSequence::parse("shift+F12").unwrap().keys,
            [Scancode::LShift, Scancode::F12]
        );
    }

    #[test]
    fn keys_and_release() {
        let mut i = Input::new(GrabSequence::parse("ctrl+alt").unwrap());
        let mut out = Vec::new();
        i.key(Scancode::A, Mod::NOMOD, true, false, &mut out);
        i.key(Scancode::Up, Mod::NOMOD, true, false, &mut out);
        i.focus_lost(&mut out);
        assert_eq!(
            cmds(&out),
            [
                InputCmd::KeyDown(0x1e),
                InputCmd::KeyDown(0x148),
                InputCmd::KeyUp(0x1e),
                InputCmd::KeyUp(0x148)
            ]
        );
        // A release we never sent a press for is dropped.
        out.clear();
        i.key(Scancode::B, Mod::NOMOD, false, false, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn grab_sequence_toggles() {
        let mut i = Input::new(GrabSequence::parse("ctrl+alt").unwrap());
        i.mouse_grabbed = true;
        let mut out = Vec::new();
        assert_eq!(
            i.key(Scancode::LCtrl, Mod::LCTRLMOD, true, false, &mut out)
                .grab_mouse,
            None
        );
        let a = i.key(
            Scancode::LAlt,
            Mod::LCTRLMOD | Mod::LALTMOD,
            true,
            false,
            &mut out,
        );
        assert_eq!(a.grab_mouse, Some(false));
        i.key(Scancode::LAlt, Mod::LCTRLMOD, false, false, &mut out);
        let a = i.key(Scancode::LCtrl, Mod::NOMOD, false, false, &mut out);
        assert_eq!(a.grab_keyboard, Some(false));
        assert!(i.keyboard_grab_released);
    }

    #[test]
    fn wheel_and_buttons() {
        let mut i = Input::new(GrabSequence::parse("ctrl+alt").unwrap());
        i.mouse_mode = MouseMode::Client;
        let mut out = Vec::new();
        i.mouse_button(MouseButton::Left, true, 5.0, 5.0, (10, 10), &mut out);
        i.wheel(-2.0, &mut out);
        i.mouse_button(MouseButton::Left, false, 5.0, 5.0, (10, 10), &mut out);
        assert_eq!(
            cmds(&out),
            [
                InputCmd::Press {
                    button: BUTTON_LEFT,
                    buttons: 0
                },
                InputCmd::Press {
                    button: BUTTON_DOWN,
                    buttons: MASK_LEFT
                },
                InputCmd::Release {
                    button: BUTTON_DOWN,
                    buttons: MASK_LEFT
                },
                InputCmd::Press {
                    button: BUTTON_DOWN,
                    buttons: MASK_LEFT
                },
                InputCmd::Release {
                    button: BUTTON_DOWN,
                    buttons: MASK_LEFT
                },
                InputCmd::Release {
                    button: BUTTON_LEFT,
                    buttons: MASK_LEFT
                },
            ]
        );
        // Server mode: an ungrabbed click asks for a grab and sends nothing.
        i.mouse_mode = MouseMode::Server;
        out.clear();
        assert!(i.mouse_button(MouseButton::Left, true, 1.0, 1.0, (10, 10), &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn relative_motion_keeps_fractions() {
        let mut i = Input::new(GrabSequence::parse("ctrl+alt").unwrap());
        i.mouse_grabbed = true;
        let mut out = Vec::new();
        i.mouse_motion(0.0, 0.0, 0.6, 0.0, (10, 10), &mut out);
        i.mouse_motion(0.0, 0.0, 0.6, 0.0, (10, 10), &mut out);
        assert_eq!(
            cmds(&out),
            [InputCmd::Motion {
                dx: 1,
                dy: 0,
                buttons: 0
            }]
        );
    }
}
