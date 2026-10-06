//! spiceport: a SPICE client using SDL3 for display, input and audio.

mod cli;
mod logging;
mod net;
mod ui;

use std::io::BufRead;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::Parser;
use log::{error, info};
use spice_client::{SessionConfig, SpiceUri, TlsOptions};

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    if let Err(e) = logging::init(cli.verbosity(), cli.log_file.as_deref()) {
        eprintln!("spiceport: {e:#}");
        return ExitCode::from(1);
    }
    let cfg = match session_config(&cli) {
        Ok(c) => c,
        Err(e) => {
            // A bad URL or option is a usage error, like clap's own.
            error!("{e:#}");
            return ExitCode::from(2);
        }
    };
    let opts = ui::UiOptions {
        fullscreen: cli.fullscreen,
        title_host: cfg
            .uri
            .host
            .clone()
            .or_else(|| cfg.uri.unix_path.clone())
            .unwrap_or_default(),
    };
    info!("spiceport {} starting", env!("CARGO_PKG_VERSION"));
    match ui::run(cfg, opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e:#}");
            ExitCode::from(1)
        }
    }
}

fn session_config(cli: &cli::Cli) -> anyhow::Result<SessionConfig> {
    let uri = SpiceUri::parse(&cli.url).with_context(|| "invalid SPICE URL")?;
    if uri.unix_path.is_some() && cfg!(not(unix)) {
        bail!("spice+unix:// URLs are not supported on this platform");
    }
    let mut cfg = SessionConfig::new(uri);
    if cli.password_stdin {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .context("reading password from stdin")?;
        cfg.password = Some(line.trim_end_matches(['\r', '\n']).to_string());
    } else if cfg.password.is_none()
        && let Ok(p) = std::env::var("SPICE_PASSWORD")
    {
        cfg.password = Some(p);
    }
    cfg.tls = TlsOptions {
        ca_file: cli.ca_file.clone(),
        host_subject: cli.host_subject.clone(),
    };
    for c in &cli.secure_channels {
        if c != "all" && spice_client::channel_type_from_name(c).is_none() {
            bail!("unknown channel name '{c}' in --secure-channels");
        }
    }
    cfg.secure_channels = cli.secure_channels.clone();
    cfg.preferred_compression = cli.preferred_compression.map(cli::Compression::wire);
    cfg.enable_audio = !cli.no_audio;
    cfg.enable_record = !cli.no_record;
    cfg.agent_resize = !cli.no_agent_resize;
    Ok(cfg)
}
