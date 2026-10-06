//! spiceport: a SPICE client using SDL3 for display, input and audio.

mod cli;
mod logging;

use std::io::{BufRead, IsTerminal};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::Parser;
use log::{error, info};
use spice_client::{ClientCommand, ClientEvent, SessionConfig, SpiceUri, TlsOptions};
use tokio::sync::mpsc;

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
    match run(cfg) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e:#}");
            ExitCode::from(1)
        }
    }
}

fn session_config(cli: &cli::Cli) -> anyhow::Result<SessionConfig> {
    let uri = SpiceUri::parse(&cli.url).with_context(|| "invalid SPICE URL")?;
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
        if c != "all" && spice_proto_channel(c).is_none() {
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

fn spice_proto_channel(name: &str) -> Option<u8> {
    spice_client::channel_type_from_name(name)
}

fn run(mut cfg: SessionConfig) -> anyhow::Result<()> {
    info!("spiceport {} starting", env!("CARGO_PKG_VERSION"));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("spice-net")
        .build()?;

    let mut prompted = false;
    loop {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let sink = Arc::new(|ev: ClientEvent| log::debug!("event: {ev:?}"));
        let r = rt.block_on(async {
            let quit = cmd_tx.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    let _ = quit.send(ClientCommand::Quit);
                }
            });
            spice_client::run(cfg.clone(), sink, cmd_rx).await
        });
        match r {
            Err(e) if e.is_auth_failure() && !prompted && std::io::stdin().is_terminal() => {
                prompted = true;
                let pw = rpassword::prompt_password("Password: ").context("reading password")?;
                cfg.password = Some(pw);
            }
            r => return r.map_err(Into::into),
        }
    }
}
