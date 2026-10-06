//! A y-x banded region, standing in for the subset of `pixman_region32` the
//! canvas uses: rectangles are grouped into horizontal bands, sorted, and
//! never overlap, so every pixel is visited at most once (which matters for
//! XOR-style raster ops).

use spice_proto::msg::display::Rect;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Band {
    y1: i32,
    y2: i32,
    /// Sorted, disjoint, non-adjacent `[x1, x2)` spans.
    xs: Vec<(i32, i32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Region {
    bands: Vec<Band>,
}

#[derive(Clone, Copy)]
enum Op {
    Union,
    Intersect,
    Subtract,
}

impl Region {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rect(r: Rect) -> Self {
        if r.is_empty() {
            return Self::default();
        }
        Self {
            bands: vec![Band {
                y1: r.top,
                y2: r.bottom,
                xs: vec![(r.left, r.right)],
            }],
        }
    }

    /// The union of possibly overlapping rectangles.
    pub fn from_rects(rects: &[Rect]) -> Self {
        let mut ys: Vec<i32> = rects
            .iter()
            .filter(|r| !r.is_empty())
            .flat_map(|r| [r.top, r.bottom])
            .collect();
        ys.sort_unstable();
        ys.dedup();
        let mut out = Region::default();
        for w in ys.windows(2) {
            let (ya, yb) = (w[0], w[1]);
            let mut xs: Vec<(i32, i32)> = rects
                .iter()
                .filter(|r| !r.is_empty() && r.top <= ya && r.bottom >= yb)
                .map(|r| (r.left, r.right))
                .collect();
            xs.sort_unstable();
            let mut merged: Vec<(i32, i32)> = Vec::with_capacity(xs.len());
            for (a, b) in xs {
                match merged.last_mut() {
                    Some(last) if a <= last.1 => last.1 = last.1.max(b),
                    _ => merged.push((a, b)),
                }
            }
            out.push_band(ya, yb, merged);
        }
        out
    }

    /// Build a region from per-row spans, e.g. the set pixels of a mask.
    /// `rows` yields `(y, spans)` in increasing `y` with sorted, disjoint spans.
    pub fn from_row_spans(rows: impl IntoIterator<Item = (i32, Vec<(i32, i32)>)>) -> Self {
        let mut out = Region::default();
        for (y, xs) in rows {
            out.push_band(y, y + 1, xs);
        }
        out
    }

    fn push_band(&mut self, y1: i32, y2: i32, xs: Vec<(i32, i32)>) {
        if xs.is_empty() || y2 <= y1 {
            return;
        }
        if let Some(last) = self.bands.last_mut()
            && last.y2 == y1
            && last.xs == xs
        {
            last.y2 = y2;
            return;
        }
        self.bands.push(Band { y1, y2, xs });
    }

    pub fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    pub fn extents(&self) -> Rect {
        let Some(first) = self.bands.first() else {
            return Rect::default();
        };
        let last = self.bands.last().unwrap();
        let left = self.bands.iter().map(|b| b.xs[0].0).min().unwrap();
        let right = self
            .bands
            .iter()
            .map(|b| b.xs.last().unwrap().1)
            .max()
            .unwrap();
        Rect::new(left, first.y1, right, last.y2)
    }

    /// The rectangles, in y-then-x order.
    pub fn rects(&self) -> impl DoubleEndedIterator<Item = Rect> + '_ {
        self.bands.iter().flat_map(|b| {
            b.xs.iter()
                .map(move |&(x1, x2)| Rect::new(x1, b.y1, x2, b.y2))
        })
    }

    pub fn translate(&mut self, dx: i32, dy: i32) {
        for b in &mut self.bands {
            b.y1 += dy;
            b.y2 += dy;
            for x in &mut b.xs {
                x.0 += dx;
                x.1 += dx;
            }
        }
    }

    pub fn intersect(&self, other: &Region) -> Region {
        combine(self, other, Op::Intersect)
    }

    pub fn intersect_rect(&self, r: Rect) -> Region {
        self.intersect(&Region::from_rect(r))
    }

    pub fn union(&self, other: &Region) -> Region {
        combine(self, other, Op::Union)
    }

    pub fn subtract(&self, other: &Region) -> Region {
        combine(self, other, Op::Subtract)
    }

    /// `pixman_region32_inverse`: the parts of `bound` not in `self`.
    pub fn inverse(&self, bound: Rect) -> Region {
        Region::from_rect(bound).subtract(self)
    }
}

fn combine(a: &Region, b: &Region, op: Op) -> Region {
    let mut ys: Vec<i32> = a
        .bands
        .iter()
        .chain(&b.bands)
        .flat_map(|band| [band.y1, band.y2])
        .collect();
    ys.sort_unstable();
    ys.dedup();
    let (mut ia, mut ib) = (0, 0);
    let mut out = Region::default();
    let empty: Vec<(i32, i32)> = Vec::new();
    for w in ys.windows(2) {
        let (ya, yb) = (w[0], w[1]);
        while ia < a.bands.len() && a.bands[ia].y2 <= ya {
            ia += 1;
        }
        while ib < b.bands.len() && b.bands[ib].y2 <= ya {
            ib += 1;
        }
        let ax = a
            .bands
            .get(ia)
            .filter(|band| band.y1 <= ya)
            .map_or(&empty, |band| &band.xs);
        let bx = b
            .bands
            .get(ib)
            .filter(|band| band.y1 <= ya)
            .map_or(&empty, |band| &band.xs);
        out.push_band(ya, yb, combine_spans(ax, bx, op));
    }
    out
}

fn combine_spans(a: &[(i32, i32)], b: &[(i32, i32)], op: Op) -> Vec<(i32, i32)> {
    match op {
        Op::Intersect if a.is_empty() || b.is_empty() => return Vec::new(),
        Op::Subtract if b.is_empty() => return a.to_vec(),
        Op::Subtract | Op::Union if a.is_empty() => {
            return if matches!(op, Op::Union) {
                b.to_vec()
            } else {
                Vec::new()
            };
        }
        Op::Union if b.is_empty() => return a.to_vec(),
        _ => {}
    }
    let mut xs: Vec<i32> = a.iter().chain(b).flat_map(|&(x1, x2)| [x1, x2]).collect();
    xs.sort_unstable();
    xs.dedup();
    let inside = |spans: &[(i32, i32)], i: &mut usize, x: i32| {
        while *i < spans.len() && spans[*i].1 <= x {
            *i += 1;
        }
        *i < spans.len() && spans[*i].0 <= x
    };
    let (mut i, mut j) = (0, 0);
    let mut out: Vec<(i32, i32)> = Vec::new();
    for w in xs.windows(2) {
        let (x1, x2) = (w[0], w[1]);
        let ina = inside(a, &mut i, x1);
        let inb = inside(b, &mut j, x1);
        let keep = match op {
            Op::Union => ina || inb,
            Op::Intersect => ina && inb,
            Op::Subtract => ina && !inb,
        };
        if keep {
            match out.last_mut() {
                Some(last) if last.1 == x1 => last.1 = x2,
                _ => out.push((x1, x2)),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn r(l: i32, t: i32, rt: i32, b: i32) -> Rect {
        Rect::new(l, t, rt, b)
    }

    fn pixels(reg: &Region) -> std::collections::HashSet<(i32, i32)> {
        let mut s = std::collections::HashSet::new();
        for rc in reg.rects() {
            for y in rc.top..rc.bottom {
                for x in rc.left..rc.right {
                    assert!(s.insert((x, y)), "overlapping rects at {x},{y}");
                }
            }
        }
        s
    }

    fn rect_pixels(rects: &[Rect]) -> std::collections::HashSet<(i32, i32)> {
        rects
            .iter()
            .flat_map(|rc| {
                (rc.top..rc.bottom).flat_map(move |y| (rc.left..rc.right).map(move |x| (x, y)))
            })
            .collect()
    }

    #[test]
    fn union_of_overlapping() {
        let reg = Region::from_rects(&[r(0, 0, 4, 4), r(2, 2, 6, 6)]);
        assert_eq!(pixels(&reg).len(), 16 + 16 - 4);
        assert_eq!(reg.extents(), r(0, 0, 6, 6));
    }

    #[test]
    fn intersect_and_inverse() {
        let a = Region::from_rect(r(0, 0, 10, 10));
        let b = Region::from_rects(&[r(5, 5, 15, 15)]);
        assert_eq!(
            a.intersect(&b).rects().collect::<Vec<_>>(),
            [r(5, 5, 10, 10)]
        );
        let inv = b.inverse(r(0, 0, 10, 10));
        assert_eq!(pixels(&inv).len(), 100 - 25);
        assert!(Region::from_rect(r(3, 3, 3, 9)).is_empty());
    }

    #[test]
    fn bands_coalesce() {
        let reg = Region::from_row_spans((0..5).map(|y| (y, vec![(1, 3)])));
        assert_eq!(reg.rects().collect::<Vec<_>>(), [r(1, 0, 3, 5)]);
    }

    proptest! {
        #[test]
        fn ops_match_pixel_sets(
            a in proptest::collection::vec((0i32..12, 0i32..12, 0i32..8, 0i32..8), 0..5),
            b in proptest::collection::vec((0i32..12, 0i32..12, 0i32..8, 0i32..8), 0..5),
        ) {
            let ar: Vec<Rect> = a.iter().map(|&(x, y, w, h)| r(x, y, x + w, y + h)).collect();
            let br: Vec<Rect> = b.iter().map(|&(x, y, w, h)| r(x, y, x + w, y + h)).collect();
            let (ra, rb) = (Region::from_rects(&ar), Region::from_rects(&br));
            let (pa, pb) = (rect_pixels(&ar), rect_pixels(&br));
            prop_assert_eq!(pixels(&ra), pa.clone());
            prop_assert_eq!(pixels(&ra.union(&rb)), pa.union(&pb).copied().collect());
            prop_assert_eq!(pixels(&ra.intersect(&rb)), pa.intersection(&pb).copied().collect());
            prop_assert_eq!(pixels(&ra.subtract(&rb)), pa.difference(&pb).copied().collect());
        }
    }
}
