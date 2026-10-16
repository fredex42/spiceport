//! H.264 stream frames (`SPICE_VIDEO_CODEC_TYPE_H264`), through FFmpeg's
//! libavcodec.
//!
//! spice-server encodes with GStreamer's `x264enc` straight from the
//! guest's RGB surface, so x264 picks 4:4:4 and the stream is High 4:4:4
//! Predictive, which openh264 and the pure-Rust decoders can't handle. It
//! also uses `intra-refresh`: there is one SPS/IDR at the start and no key
//! frames after it. Each `STREAM_DATA` is one Annex B access unit, and a
//! stream keeps one decoder that must see every frame in order, even those
//! too late to show (spice-gtk's GStreamer decoder does the same).
//!
//! libavcodec only decodes; the YUV to xRGB conversion is done here so the
//! decoder stays `Send` and needs no libswscale.

use std::sync::Once;

use ff::format::Pixel;
use ff::util::color;
use ffmpeg_next as ff;

use crate::image::{DecodeError, Format, Image, Result};

/// The largest frame we'll convert, as for JPEG.
const MAX_DIM: u32 = 16384;

fn err(e: ff::Error) -> DecodeError {
    DecodeError::H264(e.to_string())
}

pub struct H264Decoder {
    dec: ff::decoder::Video,
}

impl H264Decoder {
    pub fn new() -> Result<Self> {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = ff::init();
            // Errors come back through `decode`; don't let libavcodec
            // print per-macroblock complaints on stderr.
            ff::util::log::set_level(ff::util::log::Level::Fatal);
        });
        let codec = ff::decoder::find(ff::codec::Id::H264)
            .ok_or_else(|| DecodeError::H264("libavcodec has no H.264 decoder".into()))?;
        let mut ctx = ff::codec::Context::new_with_codec(codec);
        // Output each frame as soon as it's decoded; slice threads (x264
        // runs with sliced-threads) add no latency, frame threads would.
        ctx.set_flags(ff::codec::Flags::LOW_DELAY);
        let mut threading = ctx.threading();
        threading.kind = ff::codec::threading::Type::Slice;
        threading.count = 0;
        ctx.set_threading(threading);
        let dec = ctx.decoder().video().map_err(err)?;
        Ok(Self { dec })
    }

    /// Decode one access unit to xRGB. `Ok(None)` means the decoder has no
    /// picture yet (e.g. parameter sets only, or waiting for a key frame).
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Image>> {
        if data.is_empty() {
            // An empty packet would mean end of stream to libavcodec.
            return Ok(None);
        }
        self.dec.send_packet(&ff::Packet::copy(data)).map_err(err)?;
        let mut frame = ff::frame::Video::empty();
        let mut latest = None;
        loop {
            match self.dec.receive_frame(&mut frame) {
                Ok(()) => latest = Some(to_xrgb(&frame)?),
                Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => break,
                Err(ff::Error::Eof) => break,
                Err(e) => return Err(err(e)),
            }
        }
        Ok(latest)
    }
}

/// Fixed-point (16.16) YCbCr to RGB coefficients.
struct Matrix {
    y_off: i32,
    y: i32,
    rv: i32,
    gu: i32,
    gv: i32,
    bu: i32,
}

impl Matrix {
    /// From the luma weights `kr`/`kb` of the colour space; `full` range
    /// uses all of 0..=255, limited ("MPEG") range 16..=235 / 16..=240.
    fn new(kr: f32, kb: f32, full: bool) -> Self {
        let kg = 1.0 - kr - kb;
        let (y_off, ys, cs) = if full {
            (0, 1.0, 1.0)
        } else {
            (16, 255.0 / 219.0, 255.0 / 224.0)
        };
        let fx = |v: f32| (v * 65536.0).round() as i32;
        Self {
            y_off,
            y: fx(ys),
            rv: fx(2.0 * (1.0 - kr) * cs),
            gu: fx(2.0 * kb * (1.0 - kb) / kg * cs),
            gv: fx(2.0 * kr * (1.0 - kr) / kg * cs),
            bu: fx(2.0 * (1.0 - kb) * cs),
        }
    }

    /// The frame's signalled matrix. Unspecified falls back the way
    /// GStreamer does: BT.709 for HD sizes, BT.601 below.
    fn for_frame(f: &ff::frame::Video, full_range_format: bool) -> Self {
        let full = full_range_format || f.color_range() == color::Range::JPEG;
        let (kr, kb) = match f.color_space() {
            color::Space::BT709 => (0.2126, 0.0722),
            color::Space::BT2020NCL | color::Space::BT2020CL => (0.2627, 0.0593),
            color::Space::SMPTE240M => (0.212, 0.087),
            color::Space::FCC => (0.30, 0.11),
            color::Space::BT470BG | color::Space::SMPTE170M => (0.299, 0.114),
            _ if f.height() > 576 => (0.2126, 0.0722),
            _ => (0.299, 0.114),
        };
        Self::new(kr, kb, full)
    }

    #[inline]
    fn rgb(&self, y: u8, u: u8, v: u8) -> u32 {
        let y = (y as i32 - self.y_off) * self.y + (1 << 15);
        let (u, v) = (u as i32 - 128, v as i32 - 128);
        let c = |x: i32| (x >> 16).clamp(0, 255) as u32;
        let r = c(y + self.rv * v);
        let g = c(y - self.gu * u - self.gv * v);
        let b = c(y + self.bu * u);
        r << 16 | g << 8 | b
    }
}

/// Convert a decoded planar 8-bit frame to an xRGB [`Image`].
fn to_xrgb(f: &ff::frame::Video) -> Result<Image> {
    let (w, h) = (f.width(), f.height());
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
        return Err(DecodeError::TooLarge {
            width: w,
            height: h,
        });
    }
    let (wu, hu) = (w as usize, h as usize);
    // (log2 horizontal, log2 vertical chroma subsampling, full range)
    let (sx, sy, full) = match f.format() {
        Pixel::YUV420P => (1, 1, false),
        Pixel::YUVJ420P => (1, 1, true),
        Pixel::YUV422P => (1, 0, false),
        Pixel::YUVJ422P => (1, 0, true),
        Pixel::YUV444P => (0, 0, false),
        Pixel::YUVJ444P => (0, 0, true),
        Pixel::GBRP => return Ok(gbr_to_xrgb(f)),
        other => {
            return Err(DecodeError::Unsupported(format!(
                "H.264 output format {other:?}"
            )));
        }
    };
    let m = Matrix::for_frame(f, full);
    let (yp, up, vp) = (f.data(0), f.data(1), f.data(2));
    let (ys, us, vs) = (f.stride(0), f.stride(1), f.stride(2));
    let mut out = Vec::with_capacity(wu * hu);
    for row in 0..hu {
        let yr = &yp[row * ys..][..wu];
        let cr = row >> sy;
        let ur = &up[cr * us..];
        let vr = &vp[cr * vs..];
        out.extend(
            yr.iter()
                .enumerate()
                .map(|(x, &yv)| m.rgb(yv, ur[x >> sx], vr[x >> sx])),
        );
    }
    Ok(Image::from_data(w, h, Format::Xrgb32, out))
}

/// x264's RGB mode: planes are G, B, R.
fn gbr_to_xrgb(f: &ff::frame::Video) -> Image {
    let (w, h) = (f.width() as usize, f.height() as usize);
    let (g, b, r) = (f.data(0), f.data(1), f.data(2));
    let (gs, bs, rs) = (f.stride(0), f.stride(1), f.stride(2));
    let mut out = Vec::with_capacity(w * h);
    for row in 0..h {
        out.extend((0..w).map(|x| {
            (r[row * rs + x] as u32) << 16 | (g[row * gs + x] as u32) << 8 | b[row * bs + x] as u32
        }));
    }
    Image::from_data(w as u32, h as u32, Format::Xrgb32, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split an Annex B stream into access units at its AUD NAL units
    /// (spice-server's x264enc runs with `aud=true`), as STREAM_DATA
    /// messages carry them.
    fn access_units(stream: &[u8]) -> Vec<&[u8]> {
        let aud = [0, 0, 0, 1, 9];
        let starts: Vec<usize> = (0..stream.len().saturating_sub(4))
            .filter(|&i| stream[i..].starts_with(&aud))
            .collect();
        starts
            .iter()
            .zip(starts.iter().skip(1).copied().chain([stream.len()]))
            .map(|(&a, b)| &stream[a..b])
            .collect()
    }

    /// The first three bars of `videotestsrc pattern=smpte` (full
    /// intensity): white, yellow, cyan.
    fn check_bars(img: &Image) {
        let px = |x: usize| {
            let p = img.data[img.width as usize * 10 + x];
            [(p >> 16) as u8, (p >> 8) as u8, p as u8]
        };
        let near = |got: [u8; 3], want: [u8; 3]| {
            got.iter()
                .zip(want)
                .all(|(&g, w)| (g as i32 - w as i32).abs() <= 16)
        };
        for (x, want) in [
            (4, [255, 255, 255]),
            (13, [255, 255, 0]),
            (22, [0, 255, 255]),
        ] {
            assert!(near(px(x), want), "x={x}: {:?}, want {want:?}", px(x));
        }
    }

    fn decode_fixture(stream: &[u8]) {
        let aus = access_units(stream);
        assert_eq!(aus.len(), 10);
        let mut dec = H264Decoder::new().unwrap();
        let mut shown = 0;
        for au in aus {
            if let Some(img) = dec.decode(au).unwrap() {
                assert_eq!((img.width, img.height), (64, 48));
                check_bars(&img);
                shown += 1;
            }
        }
        // Low delay: every access unit yields its picture straight away.
        assert_eq!(shown, 10);
    }

    /// What stock spice-server sends: High 4:4:4 Predictive.
    #[test]
    fn decodes_spice_server_444_stream() {
        let s = include_bytes!("../tests/data/smpte-64x48-444.h264");
        assert_eq!(s[11], 244, "profile_idc");
        decode_fixture(s);
    }

    #[test]
    fn decodes_420_stream() {
        let s = include_bytes!("../tests/data/smpte-64x48-420.h264");
        assert_eq!(s[11], 66, "profile_idc");
        decode_fixture(s);
    }

    #[test]
    fn matrix_extremes() {
        let m = Matrix::new(0.299, 0.114, false);
        assert_eq!(m.rgb(16, 128, 128), 0);
        assert_eq!(m.rgb(235, 128, 128), 0xffffff);
        let m = Matrix::new(0.2126, 0.0722, true);
        assert_eq!(m.rgb(0, 128, 128), 0);
        assert_eq!(m.rgb(255, 128, 128), 0xffffff);
        assert_eq!(m.rgb(128, 128, 128), 0x808080);
    }

    #[test]
    fn garbage_does_not_panic() {
        let mut dec = H264Decoder::new().unwrap();
        for data in [&b""[..], b"not h264", &[0, 0, 0, 1, 0x65, 0xff, 0xff, 0xff]] {
            assert!(matches!(dec.decode(data), Ok(None) | Err(_)));
        }
        // Truncated access units from a real stream.
        let s = include_bytes!("../tests/data/smpte-64x48-444.h264");
        for au in access_units(s) {
            let _ = dec.decode(&au[..au.len() / 2]);
        }
    }
}
