//! Golden tests against the original C encoders (PLAN.md §6.3).
//!
//! Builds `tests/c-oracle` (spice-common's quic.c/lz.c + a tiny harness),
//! encodes generated images with it, and checks the Rust decoders give back
//! exactly the original pixels. Skipped, with a message, when a C compiler,
//! glib or the spice-gtk submodule are missing, unless
//! `SPICEPORT_REQUIRE_ORACLE=1` is set (as CI does on Linux).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use spice_decode::image::rgb555_to_xrgb;
use spice_decode::{lz, quic};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn oracle() -> Option<&'static Path> {
    static ORACLE: OnceLock<Option<PathBuf>> = OnceLock::new();
    ORACLE
        .get_or_init(|| {
            let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("spice-oracle");
            let ok = root()
                .join("spice-gtk/subprojects/spice-common/common/quic.c")
                .exists()
                && Command::new("sh")
                    .arg(root().join("tests/c-oracle/build.sh"))
                    .arg(&out)
                    .status()
                    .is_ok_and(|s| s.success());
            if ok {
                Some(out)
            } else {
                if std::env::var_os("SPICEPORT_REQUIRE_ORACLE").is_some() {
                    panic!("C oracle could not be built");
                }
                eprintln!("skipping C oracle tests: could not build tests/c-oracle");
                None
            }
        })
        .as_deref()
}

/// Deterministic xorshift so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
}

/// Test patterns: noise, gradients, flat runs and a mix, which exercise
/// literals, matches, runs and model adaptation.
fn pattern(kind: u32, w: u32, h: u32, rng: &mut Rng) -> Vec<u32> {
    let mut v = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            v.push(match kind {
                0 => rng.next(),
                1 => (x * 255 / w.max(1)) << 16 | (y * 255 / h.max(1)) << 8 | ((x + y) & 0xff),
                2 => {
                    if (x / 7 + y / 5) % 2 == 0 {
                        0x0033_6699
                    } else {
                        0x00ff_eedd
                    }
                }
                _ => {
                    if rng.next().is_multiple_of(4) {
                        rng.next()
                    } else {
                        0x0080_8080 + (y << 8)
                    }
                }
            });
        }
    }
    v
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    std::process::id() as u64 * 1_000_000 + N.fetch_add(1, Ordering::Relaxed)
}

/// Run the oracle with `args` plus input/output file paths.
fn encode(args: &[String], input: &[u8]) -> Vec<u8> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let id = unique();
    let inp = dir.join(format!("in-{id}.raw"));
    let out = dir.join(format!("out-{id}.bin"));
    std::fs::write(&inp, input).unwrap();
    let st = Command::new(oracle().unwrap())
        .args(args)
        .arg(&inp)
        .arg(&out)
        .status()
        .unwrap();
    assert!(st.success(), "oracle failed for {args:?}");
    let r = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(inp);
    let _ = std::fs::remove_file(out);
    r
}

const SIZES: &[(u32, u32)] = &[
    (1, 1),
    (3, 2),
    (17, 9),
    (64, 33),
    (300, 7),
    (2100, 3),
    (5, 700),
];

fn quic_args(ty: u32, w: u32, h: u32) -> Vec<String> {
    vec!["quic".into(), ty.to_string(), w.to_string(), h.to_string()]
}

#[test]
fn quic_matches_c_encoder() {
    if oracle().is_none() {
        return;
    }
    let mut rng = Rng(0x1234_5678_9abc_def0);
    for &(w, h) in SIZES {
        for kind in 0..4 {
            let px = pattern(kind, w, h, &mut rng);
            let rgb: Vec<u32> = px.iter().map(|p| p & 0xff_ffff).collect();

            // QUIC_IMAGE_TYPE_RGB32 (4): b g r x
            let raw: Vec<u8> = px.iter().flat_map(|p| p.to_le_bytes()).collect();
            let img = quic::decode(&encode(&quic_args(4, w, h), &raw))
                .unwrap_or_else(|e| panic!("quic rgb32 {w}x{h} kind {kind}: {e}"));
            assert_eq!((img.width, img.height), (w, h));
            assert!(img.data == rgb, "quic rgb32 {w}x{h} kind {kind} differs");

            // QUIC_IMAGE_TYPE_RGBA (5): b g r a
            let argb: Vec<u32> = px.iter().map(|&p| p ^ (rng.next() & 0x3f00_0000)).collect();
            let raw: Vec<u8> = argb.iter().flat_map(|p| p.to_le_bytes()).collect();
            let img = quic::decode(&encode(&quic_args(5, w, h), &raw))
                .unwrap_or_else(|e| panic!("quic rgba {w}x{h} kind {kind}: {e}"));
            assert!(img.data == argb, "quic rgba {w}x{h} kind {kind} differs");

            // QUIC_IMAGE_TYPE_RGB24 (3): b g r
            let raw: Vec<u8> = px
                .iter()
                .flat_map(|p| p.to_le_bytes()[..3].to_vec())
                .collect();
            let img = quic::decode(&encode(&quic_args(3, w, h), &raw)).unwrap();
            assert!(img.data == rgb, "quic rgb24 {w}x{h} kind {kind} differs");

            // QUIC_IMAGE_TYPE_RGB16 (2): x1r5g5b5
            let p16: Vec<u16> = px.iter().map(|&p| (p as u16) & 0x7fff).collect();
            let raw: Vec<u8> = p16.iter().flat_map(|p| p.to_le_bytes()).collect();
            let img = quic::decode(&encode(&quic_args(2, w, h), &raw)).unwrap();
            let want: Vec<u32> = p16.iter().map(|&p| rgb555_to_xrgb(p as u32)).collect();
            assert!(img.data == want, "quic rgb16 {w}x{h} kind {kind} differs");
        }
    }
}

#[test]
fn lz_matches_c_encoder() {
    if oracle().is_none() {
        return;
    }
    let mut rng = Rng(0xfeed_beef_cafe_f00d);
    let palette: Vec<u32> = (0..256u32).map(|i| (i * 0x01_0101) ^ 0x0000_a050).collect();
    for &(w, h) in SIZES {
        for kind in 0..4 {
            let px = pattern(kind, w, h, &mut rng);
            for top_down in [true, false] {
                let tag = format!("{w}x{h} kind {kind} top_down {top_down}");
                // The encoder reads rows in stream order: bottom-up images
                // are given bottom row first, and the decoder must flip them.
                let rows = |f: &dyn Fn(&[u32]) -> Vec<u8>| -> Vec<u8> {
                    let mut rows: Vec<Vec<u8>> = px.chunks(w as usize).map(f).collect();
                    if !top_down {
                        rows.reverse();
                    }
                    rows.concat()
                };
                let lz = |ty: u32, stride: u32, raw: &[u8]| {
                    let args = vec![
                        "lz".into(),
                        ty.to_string(),
                        w.to_string(),
                        h.to_string(),
                        stride.to_string(),
                        (top_down as u8).to_string(),
                    ];
                    encode(&args, raw)
                };
                let rgb: Vec<u32> = px.iter().map(|p| p & 0xff_ffff).collect();

                // LZ_IMAGE_TYPE_RGB32 (8) and RGBA (9)
                let raw = rows(&|r| r.iter().flat_map(|p| p.to_le_bytes()).collect());
                let img = lz::decode(&lz(8, w * 4, &raw), None).unwrap();
                assert!(img.data == rgb, "lz rgb32 {tag}");
                let img = lz::decode(&lz(9, w * 4, &raw), None).unwrap();
                assert!(img.data == px, "lz rgba {tag}");

                // RGB24 (7)
                let raw = rows(&|r| {
                    r.iter()
                        .flat_map(|p| p.to_le_bytes()[..3].to_vec())
                        .collect()
                });
                let img = lz::decode(&lz(7, w * 3, &raw), None).unwrap();
                assert!(img.data == rgb, "lz rgb24 {tag}");

                // RGB16 (6): x1r5g5b5, little endian
                let raw = rows(&|r| {
                    r.iter()
                        .flat_map(|p| ((*p as u16) & 0x7fff).to_le_bytes())
                        .collect()
                });
                let img = lz::decode(&lz(6, w * 2, &raw), None).unwrap();
                let want: Vec<u32> = px.iter().map(|&p| rgb555_to_xrgb(p & 0x7fff)).collect();
                assert!(img.data == want, "lz rgb16 {tag}");

                // A8 (11)
                let raw = rows(&|r| r.iter().map(|p| *p as u8).collect());
                let img = lz::decode(&lz(11, w, &raw), None).unwrap();
                let want: Vec<u32> = px.iter().map(|&p| (p & 0xff) << 24).collect();
                assert!(img.data == want, "lz a8 {tag}");

                // PLT8 (5)
                let img = lz::decode(&lz(5, w, &raw), Some(&palette)).unwrap();
                let want: Vec<u32> = px.iter().map(|&p| palette[(p & 0xff) as usize]).collect();
                assert!(img.data == want, "lz plt8 {tag}");

                // PLT4_BE (4) and PLT4_LE (3): two pixels per byte
                for (ty, be) in [(4u32, true), (3, false)] {
                    let stride = w.div_ceil(2);
                    let raw = rows(&|r| {
                        let mut row = vec![0u8; stride as usize];
                        for (x, p) in r.iter().enumerate() {
                            let shift = if (x % 2 == 0) == be { 4 } else { 0 };
                            row[x / 2] |= ((*p & 0xf) as u8) << shift;
                        }
                        row
                    });
                    let img = lz::decode(&lz(ty, stride, &raw), Some(&palette[..16])).unwrap();
                    let want: Vec<u32> = px.iter().map(|&p| palette[(p & 0xf) as usize]).collect();
                    assert!(img.data == want, "lz plt4 type {ty} {tag}");
                }

                // PLT1_BE (2) and PLT1_LE (1): eight pixels per byte
                for (ty, be) in [(2u32, true), (1, false)] {
                    let stride = w.div_ceil(8);
                    let raw = rows(&|r| {
                        let mut row = vec![0u8; stride as usize];
                        for (x, p) in r.iter().enumerate() {
                            let bit = if be { 7 - x % 8 } else { x % 8 };
                            row[x / 8] |= ((*p & 1) as u8) << bit;
                        }
                        row
                    });
                    let img = lz::decode(&lz(ty, stride, &raw), Some(&palette[..2])).unwrap();
                    let want: Vec<u32> = px.iter().map(|&p| palette[(p & 1) as usize]).collect();
                    assert!(img.data == want, "lz plt1 type {ty} {tag}");
                }
            }
        }
    }
}
