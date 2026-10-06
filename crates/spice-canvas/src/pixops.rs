//! Pixel-level helpers for the less common draw ops: premultiplied
//! compositing (the subset of pixman spice uses), ternary raster ops, and
//! zero-width line rasterisation.

use spice_decode::Format;

/// A premultiplied ARGB pixel as four 0..=255 channels: `[a, r, g, b]`.
pub type Argb = [u32; 4];

#[inline]
pub fn unpack(p: u32, f: Format) -> Argb {
    match f {
        Format::Xrgb32 => [255, (p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff],
        Format::Argb32 => [p >> 24, (p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff],
        Format::A8 => [p >> 24, 0, 0, 0],
    }
}

/// Store a pixel in `f`'s layout. x8r8g8b8 stores leave the x byte clear,
/// as pixman's accessors do; a8 keeps only alpha.
#[inline]
pub fn pack(c: Argb, f: Format) -> u32 {
    match f {
        Format::Xrgb32 => c[1] << 16 | c[2] << 8 | c[3],
        Format::Argb32 => c[0] << 24 | c[1] << 16 | c[2] << 8 | c[3],
        Format::A8 => c[0] << 24,
    }
}

/// `x * y / 255`, rounded (pixman's `MUL_UN8`).
#[inline]
pub fn mul(x: u32, y: u32) -> u32 {
    let t = x * y + 0x80;
    (t + (t >> 8)) >> 8
}

/// `pixman_op_t` values for the Porter-Duff operators we implement.
pub mod op {
    pub const CLEAR: u32 = 0;
    pub const SRC: u32 = 1;
    pub const DST: u32 = 2;
    pub const OVER: u32 = 3;
    pub const OVER_REVERSE: u32 = 4;
    pub const IN: u32 = 5;
    pub const IN_REVERSE: u32 = 6;
    pub const OUT: u32 = 7;
    pub const OUT_REVERSE: u32 = 8;
    pub const ATOP: u32 = 9;
    pub const ATOP_REVERSE: u32 = 10;
    pub const XOR: u32 = 11;
    pub const ADD: u32 = 12;
    pub const SATURATE: u32 = 13;
}

pub fn op_supported(o: u32) -> bool {
    o <= op::SATURATE
}

/// Combine a source (already multiplied by the mask) with a destination.
/// `sa` holds the per-channel source alpha used for the destination factor
/// (all equal unless the mask has component alpha).
#[inline]
pub fn combine(o: u32, s: Argb, sa: Argb, d: Argb) -> Argb {
    let da = d[0];
    let mut out = [0u32; 4];
    for c in 0..4 {
        let (fs, fd) = match o {
            op::CLEAR => (0, 0),
            op::SRC => (255, 0),
            op::DST => (0, 255),
            op::OVER => (255, 255 - sa[c]),
            op::OVER_REVERSE => (255 - da, 255),
            op::IN => (da, 0),
            op::IN_REVERSE => (0, sa[c]),
            op::OUT => (255 - da, 0),
            op::OUT_REVERSE => (0, 255 - sa[c]),
            op::ATOP => (da, 255 - sa[c]),
            op::ATOP_REVERSE => (255 - da, sa[c]),
            op::XOR => (255 - da, 255 - sa[c]),
            op::ADD => (255, 255),
            op::SATURATE => {
                let f = ((255 - da) * 255).checked_div(sa[0]).map_or(255, |f| f.min(255));
                (f, 255)
            }
            _ => (255, 255 - sa[c]),
        };
        out[c] = (mul(s[c], fs) + mul(d[c], fd)).min(255);
    }
    out
}

/// `pixman_repeat_t`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    None,
    Normal,
    Pad,
    Reflect,
}

impl Repeat {
    pub fn from_bits(b: u32) -> Self {
        match b {
            1 => Repeat::Normal,
            2 => Repeat::Pad,
            3 => Repeat::Reflect,
            _ => Repeat::None,
        }
    }

    /// Map a coordinate into `0..len`, or `None` for transparent.
    #[inline]
    pub fn apply(self, v: i64, len: i64) -> Option<i64> {
        if len <= 0 {
            return None;
        }
        match self {
            Repeat::None => (0..len).contains(&v).then_some(v),
            Repeat::Normal => Some(v.rem_euclid(len)),
            Repeat::Pad => Some(v.clamp(0, len - 1)),
            Repeat::Reflect => {
                let m = v.rem_euclid(2 * len);
                Some(if m < len { m } else { 2 * len - 1 - m })
            }
        }
    }
}

/// A source or mask picture for compositing: an image, an optional
/// 16.16 affine transform, repeat and filter.
pub struct Picture<'a> {
    pub data: &'a [u32],
    pub width: i64,
    pub height: i64,
    pub format: Format,
    /// Row-major 2x3 matrix of 16.16 fixed values.
    pub transform: Option<[i64; 6]>,
    pub repeat: Repeat,
    pub bilinear: bool,
}

impl Picture<'_> {
    #[inline]
    fn texel(&self, x: i64, y: i64) -> Argb {
        match (
            self.repeat.apply(x, self.width),
            self.repeat.apply(y, self.height),
        ) {
            (Some(x), Some(y)) => unpack(self.data[(y * self.width + x) as usize], self.format),
            _ => [0; 4],
        }
    }

    /// Sample at integer picture coordinates `(x, y)` (pixel centres).
    pub fn sample(&self, x: i64, y: i64) -> Argb {
        let Some(t) = self.transform else {
            return self.texel(x, y);
        };
        // pixman samples at pixel centres in 16.16 fixed point.
        let (fx, fy) = ((x << 16) + 0x8000, (y << 16) + 0x8000);
        let vx = ((t[0] * fx + t[1] * fy) >> 16) + t[2];
        let vy = ((t[3] * fx + t[4] * fy) >> 16) + t[5];
        if !self.bilinear {
            return self.texel((vx - 1) >> 16, (vy - 1) >> 16);
        }
        let (bx, by) = (vx - 0x8000, vy - 0x8000);
        let (x0, y0) = (bx >> 16, by >> 16);
        let (wx, wy) = ((bx & 0xffff) as u64, (by & 0xffff) as u64);
        let (p00, p10, p01, p11) = (
            self.texel(x0, y0),
            self.texel(x0 + 1, y0),
            self.texel(x0, y0 + 1),
            self.texel(x0 + 1, y0 + 1),
        );
        let mut out = [0u32; 4];
        for c in 0..4 {
            let top = p00[c] as u64 * (0x10000 - wx) + p10[c] as u64 * wx;
            let bot = p01[c] as u64 * (0x10000 - wx) + p11[c] as u64 * wx;
            out[c] = ((top * (0x10000 - wy) + bot * wy + (1 << 31)) >> 32) as u32;
        }
        out
    }
}

/// Evaluate a Windows ternary raster op on 32 bits at once. Bit
/// `(p << 2) | (s << 1) | d` of `rop3` gives the result for that input.
#[inline]
pub fn rop3(code: u8, p: u32, s: u32, d: u32) -> u32 {
    let mut r = 0u32;
    for k in 0..8u8 {
        if code & (1 << k) != 0 {
            let pp = if k & 4 != 0 { p } else { !p };
            let ss = if k & 2 != 0 { s } else { !s };
            let dd = if k & 1 != 0 { d } else { !d };
            r |= pp & ss & dd;
        }
    }
    r
}

/// Rasterise a zero-width polyline the X11 way (Bresenham, joints drawn
/// once, the final point left out as with `CapNotLast`), calling `plot` for
/// each pixel with whether it falls in an "on" dash.
pub fn zero_width_polyline(
    points: &[(i32, i32)],
    dashes: &[u32],
    dash_offset: u32,
    mut plot: impl FnMut(i32, i32, bool),
) {
    let total: u64 = dashes.iter().map(|&d| d.max(1) as u64).sum();
    let mut pos: u64 = dash_offset as u64;
    let on = |pos: u64| -> bool {
        if dashes.is_empty() || total == 0 {
            return true;
        }
        let mut p = pos % total;
        for (i, &d) in dashes.iter().enumerate() {
            let d = d.max(1) as u64;
            if p < d {
                return i % 2 == 0;
            }
            p -= d;
        }
        true
    };
    if points.len() == 1 {
        let (x, y) = points[0];
        plot(x, y, on(pos));
        return;
    }
    for w in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut err) = (x0, y0, dx + dy);
        // Each segment excludes its last point; the next segment starts there.
        while (x, y) != (x1, y1) {
            plot(x, y, on(pos));
            pos += 1;
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porter_duff() {
        let s = [128, 128, 0, 0]; // half-transparent red, premultiplied
        let d = [255, 0, 0, 255]; // opaque blue
        let sa = [s[0]; 4];
        assert_eq!(combine(op::SRC, s, sa, d), s);
        assert_eq!(combine(op::DST, s, sa, d), d);
        assert_eq!(combine(op::OVER, s, sa, d), [255, 128, 0, 127]);
        assert_eq!(combine(op::CLEAR, s, sa, d), [0; 4]);
        assert_eq!(
            combine(op::ADD, [200, 200, 0, 0], [200; 4], [100, 100, 0, 0]),
            [255, 255, 0, 0]
        );
    }

    #[test]
    fn rop3_matches_rop2_cases() {
        let (p, s, d) = (0xf0f0_f0f0u32, 0xcccc_ccccu32, 0xaaaa_aaaau32);
        assert_eq!(rop3(0xcc, p, s, d), s); // SRCCOPY
        assert_eq!(rop3(0xf0, p, s, d), p); // PATCOPY
        assert_eq!(rop3(0xaa, p, s, d), d); // no-op
        assert_eq!(rop3(0x66, p, s, d), s ^ d); // SRCINVERT
        assert_eq!(rop3(0x88, p, s, d), s & d); // SRCAND
        assert_eq!(rop3(0x00, p, s, d), 0);
        assert_eq!(rop3(0xff, p, s, d), !0);
        assert_eq!(rop3(0x5a, p, s, d), p ^ d); // PATINVERT
    }

    #[test]
    fn repeat_modes() {
        assert_eq!(Repeat::None.apply(-1, 4), None);
        assert_eq!(Repeat::Normal.apply(-1, 4), Some(3));
        assert_eq!(Repeat::Pad.apply(9, 4), Some(3));
        assert_eq!(Repeat::Reflect.apply(4, 4), Some(3));
        assert_eq!(Repeat::Reflect.apply(-1, 4), Some(0));
    }

    #[test]
    fn transformed_sampling() {
        let data = [1u32, 2, 3, 4];
        let pic = Picture {
            data: &data,
            width: 2,
            height: 2,
            format: Format::Argb32,
            // scale by 2 (each source pixel covers 2 dest pixels)
            transform: Some([0x8000, 0, 0, 0, 0x8000, 0]),
            repeat: Repeat::None,
            bilinear: false,
        };
        let px = |x, y| pack(pic.sample(x, y), Format::Argb32);
        assert_eq!([px(0, 0), px(1, 0), px(2, 0), px(3, 0)], [1, 1, 2, 2]);
        assert_eq!(px(0, 3), 3);
        assert_eq!(px(4, 0), 0, "outside with REPEAT_NONE is transparent");
    }

    #[test]
    fn polyline_cap_not_last_and_dashes() {
        let mut px = Vec::new();
        zero_width_polyline(&[(0, 0), (4, 0), (4, 2)], &[], 0, |x, y, on| {
            if on {
                px.push((x, y))
            }
        });
        assert_eq!(px, [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (4, 1)]);
        let mut px = Vec::new();
        zero_width_polyline(&[(0, 0), (8, 0)], &[2, 2], 0, |x, _, on| {
            if on {
                px.push(x)
            }
        });
        assert_eq!(px, [0, 1, 4, 5]);
    }
}
