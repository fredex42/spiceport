//! SDL scancode → SPICE scancode mapping.

#[rustfmt::skip]
mod generated {
    include!("../keymap_generated.rs");
}

use sdl3::keyboard::Scancode;

/// SDL scancodes are USB HID usage IDs, so one table serves every OS.
/// Returns the "xtkbd" scancode the inputs channel expects, or `None` for
/// keys with no PC equivalent.
pub fn xtkbd(sc: Scancode) -> Option<u32> {
    let usb = sc.to_i32();
    if usb < 0 {
        return None;
    }
    match generated::USB_TO_XTKBD.get(usb as usize) {
        Some(&0) | None => None,
        Some(&v) => Some(v as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_keys() {
        assert_eq!(xtkbd(Scancode::A), Some(0x1e));
        assert_eq!(xtkbd(Scancode::Escape), Some(0x01));
        assert_eq!(xtkbd(Scancode::Return), Some(0x1c));
        assert_eq!(xtkbd(Scancode::LCtrl), Some(0x1d));
        assert_eq!(xtkbd(Scancode::RCtrl), Some(0x11d));
        assert_eq!(xtkbd(Scancode::Up), Some(0x148));
        assert_eq!(xtkbd(Scancode::KpEnter), Some(0x11c));
        assert_eq!(xtkbd(Scancode::LGui), Some(0x15b));
        assert_eq!(xtkbd(Scancode::F12), Some(0x58));
        assert_eq!(xtkbd(Scancode::NonUsBackslash), Some(0x56));
    }
}
