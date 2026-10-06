//! Logging backend: `colog` on stderr, optionally teed to a plain-text file.
//!
//! Every crate logs through the `log` facade; only this binary configures the
//! backend. `RUST_LOG` overrides the verbosity flags, so
//! `RUST_LOG=spice_client::inputs_ch=debug` works with no extra setup.

use std::fs::File;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::Context;
use log::LevelFilter;

const CRATES: &[&str] = &[
    "spiceport",
    "spice_client",
    "spice_canvas",
    "spice_decode",
    "spice_proto",
];

/// Map `-q`/`-v` counts to a level: -1 error, 0 warn, 1 info, 2 debug, 3+ trace.
pub fn level_for(verbosity: i8) -> LevelFilter {
    match verbosity {
        ..=-1 => LevelFilter::Error,
        0 => LevelFilter::Warn,
        1 => LevelFilter::Info,
        2 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    }
}

pub fn init(verbosity: i8, log_file: Option<&Path>) -> anyhow::Result<()> {
    #[cfg(windows)]
    let _ = colored::control::set_virtual_terminal(true);

    // `colored` decides from *stdout* being a TTY, but we log to stderr.
    let colour = std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
        && std::io::stderr().is_terminal();
    let force = std::env::var("CLICOLOR_FORCE").is_ok_and(|v| v != "0");
    colored::control::set_override(colour || force);

    let level = level_for(verbosity).to_string().to_lowercase();
    let filters = CRATES
        .iter()
        .map(|c| format!("{c}={level}"))
        .collect::<Vec<_>>()
        .join(",");

    let mut b = env_logger::Builder::new();
    b.filter_level(LevelFilter::Off).parse_filters(&filters);
    if let Ok(env) = std::env::var("RUST_LOG") {
        b.parse_filters(&env);
    }

    let colog_fmt = colog::formatter(colog::format::DefaultCologStyle);
    match log_file {
        None => {
            b.format(colog_fmt);
        }
        Some(path) => {
            let file = File::create(path)
                .with_context(|| format!("cannot create log file {}", path.display()))?;
            let file = Mutex::new(file);
            let start = Instant::now();
            b.format(move |buf, rec| {
                // The file never gets colour codes, whatever the terminal does.
                if let Ok(mut f) = file.lock() {
                    let t = start.elapsed();
                    let _ = writeln!(
                        f,
                        "{:>5}.{:03} {:<5} {}: {}",
                        t.as_secs(),
                        t.subsec_millis(),
                        rec.level(),
                        rec.target(),
                        rec.args()
                    );
                }
                colog_fmt(buf, rec)
            });
        }
    }
    b.try_init().context("logger already initialised")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbosity_levels() {
        assert_eq!(level_for(-1), LevelFilter::Error);
        assert_eq!(level_for(0), LevelFilter::Warn);
        assert_eq!(level_for(1), LevelFilter::Info);
        assert_eq!(level_for(2), LevelFilter::Debug);
        assert_eq!(level_for(3), LevelFilter::Trace);
        assert_eq!(level_for(9), LevelFilter::Trace);
    }
}
