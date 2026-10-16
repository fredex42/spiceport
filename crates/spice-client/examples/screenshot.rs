//! Headless screenshot tool, like spice-gtk's `spicy-screenshot`:
//!
//!     cargo run -p spice-client --example screenshot -- spice://host:5900 out.ppm [seconds]
//!
//! Connects, waits for the display to settle, and writes the primary
//! surface of display 0 as a binary PPM.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spice_client::{ClientCommand, ClientEvent, SessionConfig, SharedSurface, SpiceUri};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = env_logger_lite();
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .ok_or("usage: screenshot <url> <out.ppm> [seconds]")?;
    let out = args
        .next()
        .ok_or("usage: screenshot <url> <out.ppm> [seconds]")?;
    let secs: f64 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(3.0);

    let mut cfg = SessionConfig::new(SpiceUri::parse(&url)?);
    // SPICE_COMPRESSION=<SpiceImageCompression number>, e.g. 7 for LZ4.
    cfg.preferred_compression = std::env::var("SPICE_COMPRESSION")
        .ok()
        .and_then(|v| v.parse().ok());
    // SPICE_VIDEO_CODECS=<SpiceVideoCodecType numbers>, e.g. "3,1" for
    // H.264 then MJPEG (H.264 needs the `h264` feature).
    cfg.preferred_video_codecs = std::env::var("SPICE_VIDEO_CODECS")
        .map(|v| v.split(',').filter_map(|c| c.trim().parse().ok()).collect())
        .unwrap_or_default();
    let primary: Arc<Mutex<Option<SharedSurface>>> = Arc::default();
    let p2 = primary.clone();
    let sink = Arc::new(move |ev: ClientEvent| match ev {
        ClientEvent::DisplayPrimary {
            display: 0,
            surface,
            ..
        } => *p2.lock().unwrap() = Some(surface),
        ClientEvent::Invalidate { display: 0 } => {
            if let Some(s) = p2.lock().unwrap().as_ref() {
                let mut s = s.lock().unwrap();
                s.dirty = None;
                s.notify_pending = false;
            }
        }
        _ => {}
    });
    let rt = tokio::runtime::Runtime::new()?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rt.block_on(async {
        let quit = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(secs)).await;
            let _ = quit.send(ClientCommand::Quit);
        });
        spice_client::run(cfg, sink, &mut rx).await
    })?;

    let guard = primary.lock().unwrap();
    let s = guard.as_ref().ok_or("no primary surface received")?;
    let s = s.lock().unwrap();
    let mut f = std::io::BufWriter::new(std::fs::File::create(&out)?);
    write!(f, "P6\n{} {}\n255\n", s.width, s.height)?;
    for p in &s.pixels {
        f.write_all(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8])?;
    }
    eprintln!("wrote {} ({}x{})", out, s.width, s.height);
    Ok(())
}

/// Print `info!`+ (`debug!`+ with SPICE_DEBUG set) to stderr without
/// pulling a logger into the library.
fn env_logger_lite() -> Result<(), log::SetLoggerError> {
    struct L;
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::max_level()
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                eprintln!("{} {}: {}", r.level(), r.target(), r.args());
            }
        }
        fn flush(&self) {}
    }
    let level = if std::env::var_os("SPICE_DEBUG").is_some() {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    log::set_logger(&L).map(|()| log::set_max_level(level))
}
