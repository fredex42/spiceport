//! spiceport: a SPICE client using SDL3 for display, input and audio.

mod cli;
mod logging;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    if let Err(e) = logging::init(cli.verbosity(), cli.log_file.as_deref()) {
        eprintln!("spiceport: {e:#}");
        return ExitCode::from(1);
    }
    log::info!("spiceport {} starting", env!("CARGO_PKG_VERSION"));
    ExitCode::SUCCESS
}
