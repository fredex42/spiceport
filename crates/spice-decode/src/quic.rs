//! QUIC: SPICE's lossless photographic image codec (SFALIC-derived adaptive
//! Golomb-Rice coding). A port of the decoding half of spice-common
//! `quic.c`, `quic_tmpl.c` and `quic_family_tmpl.c`.
//!
//! The model state (buckets, counters, "wait mask" update schedule, MELCODE
//! run-length state and the correlate rows, which persist across rows and
//! are *not* refreshed for run-length pixels) must evolve exactly as the
//! encoder's does, so this follows the C control flow closely.

use std::sync::LazyLock;

use crate::image::{DecodeError, Format, Image, Result};

/// ASCII "QUIC"
const QUIC_MAGIC: u32 = 0x4349_5551;
const QUIC_VERSION: u32 = 0;
const MAXNUMCODES: usize = 8;
/// Model evolution; only 1, 3 and 5 are valid. spice uses 3.
const DEFEVOL: usize = 3;
const DEFWMISTART: u32 = 0;
const DEFMAXCLEN: u32 = 26;
const DEFWMIMAX: u32 = 6;
const DEFWMINEXT: u32 = 2048;
/// `SPICE_MAX_IMAGE_SIZE`
const MAX_IMAGE_SIZE: u64 = 512 * 1024 * 1024 - 1;

// QuicImageType
const TYPE_GRAY: u32 = 1;
const TYPE_RGB16: u32 = 2;
const TYPE_RGB24: u32 = 3;
const TYPE_RGB32: u32 = 4;
const TYPE_RGBA: u32 = 5;

#[inline]
fn bppmask(n: u32) -> u32 {
    if n >= 32 { u32::MAX } else { (1u32 << n) - 1 }
}

const BESTTRIGTAB: [[u32; 11]; 3] = [
    [550, 900, 800, 700, 500, 350, 300, 200, 180, 180, 160],
    [110, 550, 900, 800, 550, 400, 350, 250, 140, 160, 140],
    [100, 120, 550, 900, 700, 500, 400, 300, 220, 250, 160],
];

/// MELCODE run-length states.
const MELCSTATES: usize = 32;
const J: [u32; MELCSTATES] = [
    0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13,
    14, 15,
];

const TABRAND_CHAOS: [u32; 256] = [
    0x02c57542, 0x35427717, 0x2f5a2153, 0x9244f155, 0x7bd26d07, 0x354c6052, 0x57329b28, 0x2993868e,
    0x6cd8808c, 0x147b46e0, 0x99db66af, 0xe32b4cac, 0x1b671264, 0x9d433486, 0x62a4c192, 0x06089a4b,
    0x9e3dce44, 0xdaabee13, 0x222425ea, 0xa46f331d, 0xcd589250, 0x8bb81d7f, 0xc8b736b9, 0x35948d33,
    0xd7ac7fd0, 0x5fbe2803, 0x2cfbc105, 0x013dbc4e, 0x7a37820f, 0x39f88e9e, 0xedd58794, 0xc5076689,
    0xfcada5a4, 0x64c2f46d, 0xb3ba3243, 0x8974b4f9, 0x5a05aebd, 0x20afcd00, 0x39e2b008, 0x88a18a45,
    0x600bde29, 0xf3971ace, 0xf37b0a6b, 0x7041495b, 0x70b707ab, 0x06beffbb, 0x4206051f, 0xe13c4ee3,
    0xc1a78327, 0x91aa067c, 0x8295f72a, 0x732917a6, 0x1d871b4d, 0x4048f136, 0xf1840e7e, 0x6a6048c1,
    0x696cb71a, 0x7ff501c3, 0x0fc6310b, 0x57e0f83d, 0x8cc26e74, 0x11a525a2, 0x946934c7, 0x7cd888f0,
    0x8f9d8604, 0x4f86e73b, 0x04520316, 0xdeeea20c, 0xf1def496, 0x67687288, 0xf540c5b2, 0x22401484,
    0x3478658a, 0xc2385746, 0x01979c2c, 0x5dad73c8, 0x0321f58b, 0xf0fedbee, 0x92826ddf, 0x284bec73,
    0x5b1a1975, 0x03df1e11, 0x20963e01, 0xa17cf12b, 0x740d776e, 0xa7a6bf3c, 0x01b5cce4, 0x1118aa76,
    0xfc6fac0a, 0xce927e9b, 0x00bf2567, 0x806f216c, 0xbca69056, 0x795bd3e9, 0xc9dc4557, 0x8929b6c2,
    0x789d52ec, 0x3f3fbf40, 0xb9197368, 0xa38c15b5, 0xc3b44fa8, 0xca8333b0, 0xb7e8d590, 0xbe807feb,
    0xbf5f8360, 0xd99e2f5c, 0x372928e1, 0x7c757c4c, 0x0db5b154, 0xc01ede02, 0x1fc86e78, 0x1f3985be,
    0xb4805c77, 0x00c880fa, 0x974c1b12, 0x35ab0214, 0xb2dc840d, 0x5b00ae37, 0xd313b026, 0xb260969d,
    0x7f4c8879, 0x1734c4d3, 0x49068631, 0xb9f6a021, 0x6b863e6f, 0xcee5debf, 0x29f8c9fb, 0x53dd6880,
    0x72b61223, 0x1f67a9fd, 0x0a0f6993, 0x13e59119, 0x11cca12e, 0xfe6b6766, 0x16b6effc, 0x97918fc4,
    0xc2b8a563, 0x94f2f741, 0x0bfa8c9a, 0xd1537ae8, 0xc1da349c, 0x873c60ca, 0x95005b85, 0x9b5c080e,
    0xbc8abbd9, 0xe1eab1d2, 0x6dac9070, 0x4ea9ebf1, 0xe0cf30d4, 0x1ef5bd7b, 0xd161043e, 0x5d2fa2e2,
    0xff5d3cae, 0x86ed9f87, 0x2aa1daa1, 0xbd731a34, 0x9e8f4b22, 0xb1c2c67a, 0xc21758c9, 0xa182215d,
    0xccb01948, 0x8d168df7, 0x04238cfe, 0x368c3dbc, 0x0aeadca5, 0xbad21c24, 0x0a71fee5, 0x9fc5d872,
    0x54c152c6, 0xfc329483, 0x6783384a, 0xeddb3e1c, 0x65f90e30, 0x884ad098, 0xce81675a, 0x4b372f7d,
    0x68bf9a39, 0x43445f1e, 0x40f8d8cb, 0x90d5acb6, 0x4cd07282, 0x349eeb06, 0x0c9d5332, 0x520b24ef,
    0x80020447, 0x67976491, 0x2f931ca3, 0xfe9b0535, 0xfcd30220, 0x61a9e6cc, 0xa487d8d7, 0x3f7c5dd1,
    0x7d0127c5, 0x48f51d15, 0x60dea871, 0xc9a91cb7, 0x58b53bb3, 0x9d5e0b2d, 0x624a78b4, 0x30dbee1b,
    0x9bdf22e7, 0x1df5c299, 0x2d5643a7, 0xf4dd35ff, 0x03ca8fd6, 0x53b47ed8, 0x6f2c19aa, 0xfeb0c1f4,
    0x49e54438, 0x2f2577e6, 0xbf876969, 0x72440ea9, 0xfa0bafb8, 0x74f5b3a0, 0x7dd357cd, 0x89ce1358,
    0x6ef2cdda, 0x1e7767f3, 0xa6be9fdb, 0x4f5f88f8, 0xba994a3a, 0x08ca6b65, 0xe0893818, 0x9e00a16a,
    0xf42bfc8f, 0x9972eedc, 0x749c8b51, 0x32c05f5e, 0xd706805f, 0x6bfbb7cf, 0xd9210a10, 0x31a1db97,
    0x923a9559, 0x37a7a1f6, 0x059f8861, 0xca493e62, 0x65157e81, 0x8f6467dd, 0xab85ff9f, 0x9331aff2,
    0x8616b9f5, 0xedbd5695, 0xee7e29b1, 0x313ac44f, 0xb903112f, 0x432ef649, 0xdc0a36c0, 0x61cf2bba,
    0x81474925, 0xa8b6c7ad, 0xee5931de, 0xb2f8158d, 0x59fb7409, 0x2e3dfaed, 0x9af25a3f, 0xe1fed4d5,
];

/// `QuicFamily`: Golomb code tables for one bit depth.
struct Family {
    n_gr_codewords: [u32; MAXNUMCODES],
    not_gr_cwlen: [u32; MAXNUMCODES],
    not_gr_prefixmask: [u32; MAXNUMCODES],
    not_gr_suffixlen: [u32; MAXNUMCODES],
    golomb_code_len: [[u32; MAXNUMCODES]; 256],
    xlat_l2u: [u32; 256],
}

fn ceil_log_2(val: u32) -> u32 {
    if val <= 1 {
        return 0;
    }
    32 - (val - 1).leading_zeros()
}

/// `family_init`
fn family_init(bpc: u32, limit: u32) -> Family {
    let mut f = Family {
        n_gr_codewords: [0; MAXNUMCODES],
        not_gr_cwlen: [0; MAXNUMCODES],
        not_gr_prefixmask: [0; MAXNUMCODES],
        not_gr_suffixlen: [0; MAXNUMCODES],
        golomb_code_len: [[0; MAXNUMCODES]; 256],
        xlat_l2u: [0; 256],
    };
    for l in 0..bpc {
        let li = l as usize;
        let altprefixlen = (limit - bpc).min(bppmask(bpc - l));
        let altcodewords = bppmask(bpc) + 1 - (altprefixlen << l);
        f.n_gr_codewords[li] = altprefixlen << l;
        f.not_gr_suffixlen[li] = ceil_log_2(altcodewords);
        f.not_gr_cwlen[li] = altprefixlen + f.not_gr_suffixlen[li];
        f.not_gr_prefixmask[li] = bppmask(32 - altprefixlen);
        for b in 0..256u32 {
            // golomb_coding_slow
            f.golomb_code_len[b as usize][li] = if b < f.n_gr_codewords[li] {
                (b >> l) + l + 1
            } else {
                f.not_gr_cwlen[li]
            };
        }
    }
    // correlate_init
    let mask = bppmask(bpc);
    for s in 0..=mask {
        f.xlat_l2u[s as usize] = if s & 1 != 0 { mask - (s >> 1) } else { s >> 1 };
    }
    f
}

static FAMILY_8BPC: LazyLock<Family> = LazyLock::new(|| family_init(8, DEFMAXCLEN));
static FAMILY_5BPC: LazyLock<Family> = LazyLock::new(|| family_init(5, DEFMAXCLEN));

impl Family {
    /// `golomb_decoding`: returns (value, codeword length).
    #[inline]
    fn decode(&self, l: usize, bits: u32) -> (u32, u32) {
        if bits > self.not_gr_prefixmask[l] {
            let zeroprefix = bits.leading_zeros();
            let cwlen = zeroprefix + 1 + l as u32;
            (
                (zeroprefix << l) | ((bits >> (32 - cwlen)) & bppmask(l as u32)),
                cwlen,
            )
        } else {
            let cwlen = self.not_gr_cwlen[l];
            (
                self.n_gr_codewords[l]
                    + ((bits >> (32 - cwlen)) & bppmask(self.not_gr_suffixlen[l])),
                cwlen,
            )
        }
    }
}

/// The per-channel context model (`FamilyStat` + buckets).
struct Model {
    levels: usize,
    /// Bucket index for each context value.
    bucket_of: Vec<u8>,
    best: Vec<u8>,
    counters: Vec<u32>,
}

impl Model {
    /// `find_model_params` + `fill_model_structures` for `DEFevol == 3`
    /// (bucket sizes 1, 2, 4, 8, ...).
    fn new(bpc: u32) -> Self {
        let (repfirst, firstsize, repnext, mulsize) = match DEFEVOL {
            1 => (3u32, 1u32, 2u32, 2u32),
            3 => (1, 1, 1, 2),
            _ => (1, 1, 1, 4),
        };
        let levels = 1usize << bpc;
        let mut bucket_of = vec![0u8; levels];
        let mut nbuckets = 0usize;
        let mut repcntr = repfirst + 1;
        let mut bsize = firstsize as usize;
        let mut bend = 0usize;
        loop {
            let bstart = if nbuckets > 0 { bend + 1 } else { 0 };
            repcntr -= 1;
            if repcntr == 0 {
                repcntr = repnext;
                bsize *= mulsize as usize;
            }
            bend = bstart + bsize - 1;
            if bend + bsize >= levels {
                bend = levels - 1;
            }
            for b in &mut bucket_of[bstart..=bend] {
                *b = nbuckets as u8;
            }
            nbuckets += 1;
            if bend >= levels - 1 {
                break;
            }
        }
        Self {
            levels,
            bucket_of,
            best: vec![0; nbuckets],
            counters: vec![0; nbuckets * MAXNUMCODES],
        }
    }

    fn reset(&mut self, bpc: u32) {
        self.counters.fill(0);
        self.best.fill((bpc - 1) as u8);
    }

    #[inline]
    fn bucket(&self, ctx: u8) -> usize {
        // The mask guards against garbage, as find_bucket does.
        self.bucket_of[ctx as usize & (self.levels - 1)] as usize
    }
}

/// `CommonState`
#[derive(Default)]
struct State {
    waitcnt: u32,
    tabrand_seed: u32,
    wm_trigger: u32,
    wmidx: u32,
    wmileft: u32,
    melcstate: usize,
    melclen: u32,
    melcorder: u32,
}

impl State {
    fn reset(&mut self) {
        self.waitcnt = 0;
        self.tabrand_seed = 0xff; // stabrand()
        self.wmidx = DEFWMISTART;
        self.wmileft = DEFWMINEXT;
        self.set_wm_trigger();
        // encoder_init_rle
        self.melcstate = 0;
        self.melclen = J[0];
        self.melcorder = 1 << self.melclen;
    }

    fn set_wm_trigger(&mut self) {
        let wm = self.wmidx.min(10) as usize;
        self.wm_trigger = BESTTRIGTAB[DEFEVOL / 2][wm];
    }

    #[inline]
    fn tabrand(&mut self) -> u32 {
        self.tabrand_seed = self.tabrand_seed.wrapping_add(1);
        TABRAND_CHAOS[(self.tabrand_seed & 0xff) as usize]
    }
}

/// Which `CommonState` a plane uses: RGB planes share one, alpha has its own.
#[derive(Clone, Copy)]
enum Sel {
    Rgb,
    Alpha,
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    io_word: u32,
    io_next_word: u32,
    io_available_bits: u32,
    overrun: bool,
    family: &'static Family,
    bpc: u32,
    bpc_mask: u32,
    models: [Model; 4],
    /// Correlate rows, offset by one: index 0 is C's `correlate_row[-1]`.
    corr: [Vec<u8>; 4],
    rgb_state: State,
    alpha_state: State,
}

/// Pixels being decoded: up to four components (r, g, b, a) per pixel.
type Px = [u8; 4];

impl<'a> Decoder<'a> {
    #[inline]
    fn read_io_word(&mut self) {
        let p = self.pos * 4;
        self.io_next_word = match self.data.get(p..p + 4) {
            Some(b) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            None => {
                self.overrun = true;
                0
            }
        };
        self.pos += 1;
    }

    /// `decode_eatbits`
    #[inline]
    fn eat(&mut self, len: u32) {
        debug_assert!(len > 0 && len < 32);
        self.io_word <<= len;
        let delta = self.io_available_bits as i32 - len as i32;
        if delta >= 0 {
            self.io_available_bits = delta as u32;
            self.io_word |= self
                .io_next_word
                .checked_shr(self.io_available_bits)
                .unwrap_or(0);
            return;
        }
        let delta = (-delta) as u32;
        self.io_word |= self.io_next_word << delta;
        self.read_io_word();
        self.io_available_bits = 32 - delta;
        self.io_word |= self.io_next_word >> self.io_available_bits;
    }

    fn eat32(&mut self) {
        self.eat(16);
        self.eat(16);
    }

    fn state(&mut self, s: Sel) -> &mut State {
        match s {
            Sel::Rgb => &mut self.rgb_state,
            Sel::Alpha => &mut self.alpha_state,
        }
    }

    /// Decode one symbol for channel `c` at pixel `i`, using the context
    /// `corr[c][i - 1]`; stores and returns the correlated value.
    #[inline]
    fn decode_sym(&mut self, c: usize, i: usize) -> u32 {
        let ctx = self.corr[c][i];
        let m = &self.models[c];
        let l = m.best[m.bucket(ctx)] as usize;
        let (v, len) = self.family.decode(l, self.io_word);
        let v = v as u8;
        self.corr[c][i + 1] = v;
        self.eat(len);
        v as u32
    }

    /// `update_model` for every channel in `chans`, at pixel `idx`.
    fn update_model(&mut self, sel: Sel, chans: &[usize], idx: usize) {
        let bpp = self.bpc as usize;
        let trigger = self.state(sel).wm_trigger;
        let family = self.family;
        for &c in chans {
            let ctx = self.corr[c][idx];
            let curval = self.corr[c][idx + 1] as usize;
            let m = &mut self.models[c];
            let bucket = m.bucket(ctx);
            let pc = &mut m.counters[bucket * MAXNUMCODES..bucket * MAXNUMCODES + MAXNUMCODES];
            let codelen = &family.golomb_code_len[curval];
            let mut bestcode = bpp - 1;
            pc[bestcode] += codelen[bestcode];
            let mut bestcodelen = pc[bestcode];
            for i in (0..bpp - 1).rev() {
                pc[i] += codelen[i];
                if pc[i] < bestcodelen {
                    bestcode = i;
                    bestcodelen = pc[i];
                }
            }
            m.best[bucket] = bestcode as u8;
            if bestcodelen > trigger {
                for v in &mut pc[..bpp] {
                    *v >>= 1;
                }
            }
        }
    }

    /// `decode_state_run`: a run length in MELCODE.
    fn decode_run(&mut self, sel: Sel) -> u64 {
        let mut runlen: u64 = 0;
        loop {
            // number of leading ones in the input, up to 8
            let temp = (!(self.io_word >> 24) as u8).leading_zeros();
            let st = self.state(sel);
            for _ in 0..temp {
                runlen += st.melcorder as u64;
                if st.melcstate < MELCSTATES - 1 {
                    st.melcstate += 1;
                    st.melclen = J[st.melcstate];
                    st.melcorder = 1 << st.melclen;
                }
            }
            if temp != 8 {
                self.eat(temp + 1);
                break;
            }
            self.eat(8);
        }
        let melclen = self.state(sel).melclen;
        if melclen != 0 {
            runlen += (self.io_word >> (32 - melclen)) as u64;
            self.eat(melclen);
        }
        let st = self.state(sel);
        if st.melcstate != 0 {
            st.melcstate -= 1;
            st.melclen = J[st.melcstate];
            st.melcorder = 1 << st.melclen;
        }
        runlen
    }

    /// The `wmidx`/`wmileft` schedule shared by `uncompress_row0` and
    /// `uncompress_row`: split the row into segments at wait-mask changes.
    fn row(
        &mut self,
        sel: Sel,
        chans: &[usize],
        prev: Option<&[Px]>,
        cur: &mut [Px],
    ) -> Result<()> {
        let mut width = cur.len() as u32;
        let mut pos = 0u32;
        loop {
            let st = self.state(sel);
            if !(DEFWMIMAX > st.wmidx && st.wmileft <= width) {
                break;
            }
            let wmileft = st.wmileft;
            if wmileft != 0 {
                let waitmask = bppmask(st.wmidx);
                self.seg(
                    sel,
                    chans,
                    prev,
                    cur,
                    pos as usize,
                    (pos + wmileft) as usize,
                    waitmask,
                )?;
                pos += wmileft;
                width -= wmileft;
            }
            let st = self.state(sel);
            st.wmidx += 1;
            st.set_wm_trigger();
            st.wmileft = DEFWMINEXT;
        }
        if width != 0 {
            let waitmask = bppmask(self.state(sel).wmidx);
            self.seg(
                sel,
                chans,
                prev,
                cur,
                pos as usize,
                (pos + width) as usize,
                waitmask,
            )?;
            let st = self.state(sel);
            if DEFWMIMAX > st.wmidx {
                st.wmileft -= width;
            }
        }
        Ok(())
    }

    /// One pixel's components: `uncompress_one[_row0][_0]`.
    #[inline]
    fn pixel(&mut self, chans: &[usize], prev: Option<&[Px]>, cur: &mut [Px], i: usize) {
        let mask = self.bpc_mask;
        for &c in chans {
            let corr = self.decode_sym(c, i);
            let l2u = self.family.xlat_l2u[corr as usize];
            let pred = match (prev, i) {
                (None, 0) => 0,
                (None, _) => cur[i - 1][c] as u32,
                (Some(p), 0) => p[0][c] as u32,
                (Some(p), _) => (cur[i - 1][c] as u32 + p[i][c] as u32) >> 1,
            };
            cur[i][c] = ((l2u + pred) & mask) as u8;
        }
    }

    #[inline]
    fn same(chans: &[usize], a: &Px, b: &Px) -> bool {
        chans.iter().all(|&c| a[c] == b[c])
    }

    /// `uncompress_row0_seg` / `uncompress_row_seg`.
    #[allow(clippy::too_many_arguments)]
    fn seg(
        &mut self,
        sel: Sel,
        chans: &[usize],
        prev: Option<&[Px]>,
        cur: &mut [Px],
        mut i: usize,
        end: usize,
        waitmask: u32,
    ) -> Result<()> {
        let mut stopidx;
        if i == 0 {
            self.pixel(chans, prev, cur, 0);
            let st = self.state(sel);
            if st.waitcnt != 0 {
                st.waitcnt -= 1;
            } else {
                st.waitcnt = st.tabrand() & waitmask;
                self.update_model(sel, chans, 0);
            }
            i = 1;
            stopidx = i + self.state(sel).waitcnt as usize;
        } else {
            stopidx = i + self.state(sel).waitcnt as usize;
        }

        let Some(prev) = prev else {
            // Row 0: no run-length prediction.
            while stopidx < end {
                while i <= stopidx {
                    self.pixel(chans, None, cur, i);
                    i += 1;
                }
                self.update_model(sel, chans, stopidx);
                stopidx = i + (self.state(sel).tabrand() & waitmask) as usize;
            }
            while i < end {
                self.pixel(chans, None, cur, i);
                i += 1;
            }
            self.state(sel).waitcnt = (stopidx - end) as u32;
            return Ok(());
        };

        let mut run_index = 0usize;
        // RLE_PRED_IMP
        let run_here = |cur: &[Px], i: usize, run_index: usize| {
            Self::same(chans, &prev[i - 1], &prev[i])
                && run_index != i
                && i > 2
                && Self::same(chans, &cur[i - 1], &cur[i - 2])
        };
        loop {
            let mut do_run = false;
            'outer: {
                while stopidx < end {
                    while i <= stopidx {
                        if run_here(cur, i, run_index) {
                            do_run = true;
                            break 'outer;
                        }
                        self.pixel(chans, Some(prev), cur, i);
                        i += 1;
                    }
                    self.update_model(sel, chans, stopidx);
                    stopidx = i + (self.state(sel).tabrand() & waitmask) as usize;
                }
                while i < end {
                    if run_here(cur, i, run_index) {
                        do_run = true;
                        break 'outer;
                    }
                    self.pixel(chans, Some(prev), cur, i);
                    i += 1;
                }
            }
            if !do_run {
                self.state(sel).waitcnt = (stopidx - end) as u32;
                return Ok(());
            }
            // do_run:
            self.state(sel).waitcnt = (stopidx - i) as u32;
            run_index = i;
            let run = self.decode_run(sel);
            if run > (end - i) as u64 {
                return Err(DecodeError::Corrupt("QUIC (bad run length)"));
            }
            let run_end = i + run as usize;
            while i < run_end {
                // COPY_PIXEL: only this plane's components.
                for &c in chans {
                    cur[i][c] = cur[i - 1][c];
                }
                i += 1;
            }
            if i == end {
                return Ok(());
            }
            stopidx = i + self.state(sel).waitcnt as usize;
        }
    }

    fn start_row(&mut self, chans: &[usize], first: bool) {
        for &c in chans {
            self.corr[c][0] = if first { 0 } else { self.corr[c][1] };
        }
    }
}

/// Decode a QUIC image. The result is `Xrgb32` (or `Argb32` for RGBA images).
pub fn decode(data: &[u8]) -> Result<Image> {
    if data.len() < 4 {
        return Err(DecodeError::Truncated("QUIC header"));
    }
    let mut d = Decoder {
        data,
        pos: 0,
        io_word: 0,
        io_next_word: 0,
        io_available_bits: 0,
        overrun: false,
        family: &FAMILY_8BPC,
        bpc: 8,
        bpc_mask: 0xff,
        models: [Model::new(8), Model::new(8), Model::new(8), Model::new(8)],
        corr: Default::default(),
        rgb_state: State::default(),
        alpha_state: State::default(),
    };
    // init_decode_io
    d.read_io_word();
    d.io_word = d.io_next_word;
    d.io_available_bits = 0;

    let magic = d.io_word;
    d.eat32();
    if magic != QUIC_MAGIC {
        return Err(DecodeError::Corrupt("QUIC (bad magic)"));
    }
    let version = d.io_word;
    d.eat32();
    if version != QUIC_VERSION {
        return Err(DecodeError::Unsupported(format!(
            "QUIC version {version:#x}"
        )));
    }
    let ty = d.io_word;
    d.eat32();
    let width = d.io_word;
    d.eat32();
    let height = d.io_word;
    d.eat32();
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err(DecodeError::Corrupt("QUIC (invalid size)"));
    }
    if width as u64 * height as u64 > MAX_IMAGE_SIZE {
        return Err(DecodeError::TooLarge { width, height });
    }
    let (bpc, out_fmt, alpha) = match ty {
        TYPE_RGB16 => (5, Format::Xrgb32, false),
        TYPE_RGB24 | TYPE_RGB32 => (8, Format::Xrgb32, false),
        TYPE_RGBA => (8, Format::Argb32, true),
        TYPE_GRAY => return Err(DecodeError::Unsupported("QUIC grayscale image".into())),
        t => return Err(DecodeError::Unsupported(format!("QUIC image type {t}"))),
    };
    let mut img = Image::new(width, height, out_fmt)?;
    if bpc == 5 {
        d.family = &FAMILY_5BPC;
        d.bpc = 5;
        d.bpc_mask = 0x1f;
        d.models = [Model::new(5), Model::new(5), Model::new(5), Model::new(5)];
    }
    // encoder_reset + encoder_reset_channels
    d.rgb_state.reset();
    d.alpha_state.reset();
    for m in &mut d.models {
        m.reset(bpc);
    }
    for c in &mut d.corr {
        *c = vec![0u8; width as usize + 1];
    }

    const RGB: [usize; 3] = [0, 1, 2];
    const A: [usize; 1] = [3];
    let w = width as usize;
    let mut prev: Vec<Px> = vec![[0; 4]; w];
    let mut cur: Vec<Px> = vec![[0; 4]; w];
    for row in 0..height as usize {
        let first = row == 0;
        d.start_row(&RGB, first);
        d.row(Sel::Rgb, &RGB, (!first).then_some(&prev[..]), &mut cur)?;
        if alpha {
            d.start_row(&A, first);
            d.row(Sel::Alpha, &A, (!first).then_some(&prev[..]), &mut cur)?;
        }
        if d.overrun {
            return Err(DecodeError::Truncated("QUIC"));
        }
        let out = img.row_mut(row as u32);
        for (o, p) in out.iter_mut().zip(&cur) {
            let (r, g, b) = if bpc == 5 {
                let x = |v: u8| ((v as u32) << 3) | ((v as u32 & 0x1f) >> 2);
                (x(p[0]), x(p[1]), x(p[2]))
            } else {
                (p[0] as u32, p[1] as u32, p[2] as u32)
            };
            let a = if alpha { p[3] as u32 } else { 0 };
            *o = a << 24 | r << 16 | g << 8 | b;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_buckets() {
        let m = Model::new(8);
        assert_eq!(m.best.len(), 8);
        assert_eq!(m.bucket_of[0], 0);
        assert_eq!(m.bucket_of[1], 1);
        assert_eq!(m.bucket_of[2], 1);
        assert_eq!(m.bucket_of[3], 2);
        assert_eq!(m.bucket_of[126], 6);
        assert_eq!(m.bucket_of[127], 7);
        assert_eq!(m.bucket_of[255], 7);
        let m = Model::new(5);
        assert_eq!(*m.bucket_of.last().unwrap() as usize, m.best.len() - 1);
    }

    #[test]
    fn family_tables() {
        let f = &*FAMILY_8BPC;
        // l=0: altprefixlen = min(18, 255) = 18; 18 GR codewords of length n+1.
        assert_eq!(f.n_gr_codewords[0], 18);
        assert_eq!(f.golomb_code_len[0][0], 1);
        assert_eq!(f.golomb_code_len[17][0], 18);
        assert_eq!(f.not_gr_cwlen[0], 18 + 8);
        assert_eq!(f.xlat_l2u[0], 0);
        assert_eq!(f.xlat_l2u[1], 255);
        assert_eq!(f.xlat_l2u[2], 1);
        assert_eq!(ceil_log_2(1), 0);
        assert_eq!(ceil_log_2(2), 1);
        assert_eq!(ceil_log_2(5), 3);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(&[]).is_err());
        assert!(decode(b"QUIC").is_err());
        let mut hdr = Vec::new();
        for w in [QUIC_MAGIC, 0, TYPE_RGB32, 4, 4] {
            hdr.extend_from_slice(&w.to_le_bytes());
        }
        // Header only: decoding must run out of data, not panic.
        assert!(decode(&hdr).is_err());
    }

    proptest::proptest! {
        #[test]
        fn never_panics(body in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512),
                        ty in 1u32..6, w in 1u32..40, h in 1u32..40) {
            let mut data = Vec::new();
            for v in [QUIC_MAGIC, 0, ty, w, h] {
                data.extend_from_slice(&v.to_le_bytes());
            }
            data.extend(body);
            let _ = decode(&data);
        }
    }
}
