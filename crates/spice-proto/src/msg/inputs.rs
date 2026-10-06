//! `channel InputsChannel` messages.

use bytes::{BufMut, BytesMut};

use super::ClientMsg;
use crate::enums::msgc::inputs as c;
use crate::reader::{Reader, Result};

/// `SpiceMsgInputsInit` / `SpiceMsgInputsKeyModifiers`: the guest's lock keys.
pub fn decode_modifiers(b: &[u8]) -> Result<u16> {
    Reader::new(b).u16()
}

/// `spice_make_scancode`: turn an "xtkbd" scancode (0xe0-prefixed codes
/// folded into `0x100 | code`) into the KEY_DOWN/KEY_UP `code` value: the
/// scancode bytes in wire order, release adding 0x80.
pub fn make_scancode(scancode: u32, release: bool) -> u16 {
    let mut s = scancode & 0x37f;
    if release {
        s |= 0x80;
    }
    if s < 0x100 {
        s as u16
    } else {
        (0xe000 | (s - 0x100) as u16).swap_bytes()
    }
}

/// `SpiceMsgcKeyDown`
pub struct KeyDown(pub u32);

impl ClientMsg for KeyDown {
    const TYPE: u16 = c::KEY_DOWN;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(make_scancode(self.0, false) as u32);
    }
}

/// `SpiceMsgcKeyUp`
pub struct KeyUp(pub u32);

impl ClientMsg for KeyUp {
    const TYPE: u16 = c::KEY_UP;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(make_scancode(self.0, true) as u32);
    }
}

/// `SPICE_MSGC_INPUTS_KEY_SCANCODE`: press and release in one message
/// (`spice_inputs_channel_key_press_and_release`).
pub struct KeyPressRelease(pub u32);

impl ClientMsg for KeyPressRelease {
    const TYPE: u16 = c::KEY_SCANCODE;
    fn encode(&self, buf: &mut BytesMut) {
        let (down, up) = (make_scancode(self.0, false), make_scancode(self.0, true));
        if self.0 < 0x100 {
            buf.put_u8(down as u8);
            buf.put_u8(up as u8);
        } else {
            buf.put_u16_le(down);
            buf.put_u16_le(up);
        }
    }
}

/// `SpiceMsgcKeyModifiers`
pub struct KeyModifiers(pub u16);

impl ClientMsg for KeyModifiers {
    const TYPE: u16 = c::KEY_MODIFIERS;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u16_le(self.0);
    }
}

/// `SpiceMsgcMouseMotion`
pub struct MouseMotion {
    pub dx: i32,
    pub dy: i32,
    pub buttons: u16,
}

impl ClientMsg for MouseMotion {
    const TYPE: u16 = c::MOUSE_MOTION;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_i32_le(self.dx);
        buf.put_i32_le(self.dy);
        buf.put_u16_le(self.buttons);
    }
}

/// `SpiceMsgcMousePosition`
pub struct MousePosition {
    pub x: u32,
    pub y: u32,
    pub buttons: u16,
    pub display_id: u8,
}

impl ClientMsg for MousePosition {
    const TYPE: u16 = c::MOUSE_POSITION;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.x);
        buf.put_u32_le(self.y);
        buf.put_u16_le(self.buttons);
        buf.put_u8(self.display_id);
    }
}

/// `SpiceMsgcMousePress`
pub struct MousePress {
    pub button: u8,
    pub buttons: u16,
}

impl ClientMsg for MousePress {
    const TYPE: u16 = c::MOUSE_PRESS;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u8(self.button);
        buf.put_u16_le(self.buttons);
    }
}

/// `SpiceMsgcMouseRelease`
pub struct MouseRelease {
    pub button: u8,
    pub buttons: u16,
}

impl ClientMsg for MouseRelease {
    const TYPE: u16 = c::MOUSE_RELEASE;
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u8(self.button);
        buf.put_u16_le(self.buttons);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scancodes() {
        assert_eq!(make_scancode(0x1e, false), 0x1e); // A
        assert_eq!(make_scancode(0x1e, true), 0x9e);
        // Up arrow: e0 48 / e0 c8, bytes in wire order.
        assert_eq!(make_scancode(0x148, false).to_le_bytes(), [0xe0, 0x48]);
        assert_eq!(make_scancode(0x148, true).to_le_bytes(), [0xe0, 0xc8]);
        let mut b = BytesMut::new();
        KeyDown(0x148).encode(&mut b);
        assert_eq!(&b[..], &[0xe0, 0x48, 0, 0]);
        let mut b = BytesMut::new();
        KeyPressRelease(0x1e).encode(&mut b);
        assert_eq!(&b[..], &[0x1e, 0x9e]);
        let mut b = BytesMut::new();
        KeyPressRelease(0x148).encode(&mut b);
        assert_eq!(&b[..], &[0xe0, 0x48, 0xe0, 0xc8]);
    }

    #[test]
    fn sizes() {
        let mut b = BytesMut::new();
        MouseMotion {
            dx: 1,
            dy: -1,
            buttons: 0,
        }
        .encode(&mut b);
        assert_eq!(b.len(), 10);
        b.clear();
        MousePosition {
            x: 1,
            y: 2,
            buttons: 0,
            display_id: 0,
        }
        .encode(&mut b);
        assert_eq!(b.len(), 11);
        b.clear();
        MousePress {
            button: 1,
            buttons: 1,
        }
        .encode(&mut b);
        assert_eq!(b.len(), 3);
    }
}
