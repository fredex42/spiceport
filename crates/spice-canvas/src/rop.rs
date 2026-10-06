//! Raster operations: `SpiceROP` (`pixman_utils.h`) and the conversion from
//! a `SpiceRopd` descriptor (`ropd_descriptor_to_rop` in `canvas_base.c`).

use spice_proto::enums::ropd;

/// `SpiceROP`: the 16 two-operand boolean raster ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Rop {
    Clear = 0,
    And,
    AndReverse,
    Copy,
    AndInverted,
    Noop,
    Xor,
    Or,
    Nor,
    Equiv,
    Invert,
    OrReverse,
    CopyInverted,
    OrInverted,
    Nand,
    Set,
}

impl Rop {
    #[inline]
    pub fn apply(self, src: u32, dst: u32) -> u32 {
        match self {
            Rop::Clear => 0,
            Rop::And => src & dst,
            Rop::AndReverse => src & !dst,
            Rop::Copy => src,
            Rop::AndInverted => !src & dst,
            Rop::Noop => dst,
            Rop::Xor => src ^ dst,
            Rop::Or => src | dst,
            Rop::Nor => !src & !dst,
            Rop::Equiv => !src ^ dst,
            Rop::Invert => !dst,
            Rop::OrReverse => src | !dst,
            Rop::CopyInverted => !src,
            Rop::OrInverted => !src | dst,
            Rop::Nand => !src | !dst,
            Rop::Set => 0xffff_ffff,
        }
    }
}

/// Which operand of the draw op plays the role of "source".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RopInput {
    Src,
    Brush,
    Dest,
}

/// `ropd_descriptor_to_rop`
pub fn ropd_to_rop(desc: u16, src_input: RopInput, dest_input: RopInput) -> Rop {
    let mask = |i: RopInput| match i {
        RopInput::Src => ropd::INVERS_SRC,
        RopInput::Brush => ropd::INVERS_BRUSH,
        RopInput::Dest => ropd::INVERS_DEST,
    };
    let old = desc;
    let mut desc = desc & !(ropd::INVERS_SRC | ropd::INVERS_DEST);
    if old & mask(src_input) != 0 {
        desc |= ropd::INVERS_SRC;
    }
    if old & mask(dest_input) != 0 {
        desc |= ropd::INVERS_DEST;
    }
    let is = desc & ropd::INVERS_SRC != 0;
    let id = desc & ropd::INVERS_DEST != 0;
    let ir = desc & ropd::INVERS_RES != 0;

    if desc & ropd::OP_PUT != 0 {
        if is != ir {
            Rop::CopyInverted
        } else {
            Rop::Copy
        }
    } else if desc & ropd::OP_OR != 0 {
        match (ir, is, id) {
            (true, true, true) => Rop::And,
            (true, true, false) => Rop::AndReverse,
            (true, false, true) => Rop::AndInverted,
            (true, false, false) => Rop::Nor,
            (false, true, true) => Rop::Nand,
            (false, true, false) => Rop::OrInverted,
            (false, false, true) => Rop::OrReverse,
            (false, false, false) => Rop::Or,
        }
    } else if desc & ropd::OP_AND != 0 {
        match (ir, is, id) {
            (true, true, true) => Rop::Or,
            (true, true, false) => Rop::OrReverse,
            (true, false, true) => Rop::OrInverted,
            (true, false, false) => Rop::Nand,
            (false, true, true) => Rop::Nor,
            (false, true, false) => Rop::AndInverted,
            (false, false, true) => Rop::AndReverse,
            (false, false, false) => Rop::And,
        }
    } else if desc & ropd::OP_XOR != 0 {
        // An odd number of inversions turns XOR into EQUIV.
        if (ir as u8 + is as u8 + id as u8) % 2 == 1 {
            Rop::Equiv
        } else {
            Rop::Xor
        }
    } else if desc & ropd::OP_BLACKNESS != 0 {
        Rop::Clear
    } else if desc & ropd::OP_WHITENESS != 0 {
        Rop::Set
    } else if desc & ropd::OP_INVERS != 0 {
        Rop::Invert
    } else {
        Rop::Copy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RopInput::*;

    #[test]
    fn descriptors() {
        assert_eq!(ropd_to_rop(ropd::OP_PUT, Src, Dest), Rop::Copy);
        assert_eq!(
            ropd_to_rop(ropd::OP_PUT | ropd::INVERS_SRC, Src, Dest),
            Rop::CopyInverted
        );
        // INVERS_BRUSH only counts when the brush is the source input.
        assert_eq!(
            ropd_to_rop(ropd::OP_PUT | ropd::INVERS_BRUSH, Src, Dest),
            Rop::Copy
        );
        assert_eq!(
            ropd_to_rop(ropd::OP_PUT | ropd::INVERS_BRUSH, Brush, Dest),
            Rop::CopyInverted
        );
        assert_eq!(ropd_to_rop(ropd::OP_XOR, Src, Dest), Rop::Xor);
        assert_eq!(
            ropd_to_rop(ropd::OP_XOR | ropd::INVERS_DEST, Src, Dest),
            Rop::Equiv
        );
        assert_eq!(
            ropd_to_rop(ropd::OP_AND | ropd::INVERS_SRC, Src, Dest),
            Rop::AndInverted
        );
        assert_eq!(
            ropd_to_rop(ropd::OP_OR | ropd::INVERS_RES, Src, Dest),
            Rop::Nor
        );
        assert_eq!(ropd_to_rop(ropd::OP_BLACKNESS, Src, Dest), Rop::Clear);
        assert_eq!(ropd_to_rop(ropd::OP_INVERS, Src, Dest), Rop::Invert);
        assert_eq!(ropd_to_rop(0, Src, Dest), Rop::Copy);
    }

    /// Every OR/AND/XOR descriptor must match its boolean definition.
    #[test]
    fn descriptors_match_truth_tables() {
        for op in [ropd::OP_OR, ropd::OP_AND, ropd::OP_XOR] {
            for inv in 0..8u16 {
                let mut d = op;
                if inv & 1 != 0 {
                    d |= ropd::INVERS_SRC;
                }
                if inv & 2 != 0 {
                    d |= ropd::INVERS_DEST;
                }
                if inv & 4 != 0 {
                    d |= ropd::INVERS_RES;
                }
                let rop = ropd_to_rop(d, Src, Dest);
                for (s, t) in [(0u32, 0u32), (0, !0), (!0, 0), (!0, !0)] {
                    let s2 = if inv & 1 != 0 { !s } else { s };
                    let t2 = if inv & 2 != 0 { !t } else { t };
                    let mut r = match op {
                        ropd::OP_OR => s2 | t2,
                        ropd::OP_AND => s2 & t2,
                        _ => s2 ^ t2,
                    };
                    if inv & 4 != 0 {
                        r = !r;
                    }
                    assert_eq!(rop.apply(s, t), r, "op {op:#x} inv {inv} -> {rop:?}");
                }
            }
        }
    }
}
