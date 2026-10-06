//! Command-line interface (PLAN.md section 7).

use std::path::PathBuf;

use clap::{ArgAction, Parser, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "spiceport",
    version,
    about = "Connect to a SPICE server (QEMU/libvirt) and show the guest display",
    after_help = "The password can also come from the SPICE_PASSWORD environment variable.\n\
                  Logging can be tuned per module with RUST_LOG, e.g. RUST_LOG=spice_client::display_ch=trace"
)]
pub struct Cli {
    /// spice://host[:port][?port=N&tls-port=M&password=P], spice+tls://host:port,
    /// or spice+unix:///path/to/socket (Unix only)
    #[arg(value_name = "URL")]
    pub url: String,

    /// CA bundle (PEM) for TLS; the system roots are used when omitted
    #[arg(long, value_name = "PEM")]
    pub ca_file: Option<PathBuf>,

    /// Expected certificate subject, e.g. "C=GB,O=Example,CN=host"
    #[arg(long, value_name = "SUBJ")]
    pub host_subject: Option<String>,

    /// Channels that must use TLS: comma-separated names (main,display,inputs,
    /// cursor,playback,record) or "all"
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    pub secure_channels: Vec<String>,

    /// Read the password from the first line of stdin
    #[arg(long)]
    pub password_stdin: bool,

    /// Start in fullscreen
    #[arg(short, long)]
    pub fullscreen: bool,

    /// Disable audio playback
    #[arg(long)]
    pub no_audio: bool,

    /// Disable audio recording (microphone)
    #[arg(long)]
    pub no_record: bool,

    /// Don't resize the guest display to follow the window
    #[arg(long)]
    pub no_agent_resize: bool,

    /// Image compression to ask the server for
    #[arg(long, value_enum, value_name = "MODE")]
    pub preferred_compression: Option<Compression>,

    /// Key combination that releases the keyboard/mouse grab
    #[arg(long, value_name = "COMBO", default_value = "ctrl+alt")]
    pub release_keys: String,

    /// Raise log level (warn → info → debug → trace)
    #[arg(short, long, action = ArgAction::Count)]
    pub verbose: u8,

    /// Errors only
    #[arg(short, long, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Also write logs (without colour) to a file
    #[arg(long, value_name = "PATH")]
    pub log_file: Option<PathBuf>,
}

impl Cli {
    pub fn verbosity(&self) -> i8 {
        if self.quiet {
            -1
        } else {
            self.verbose.min(i8::MAX as u8) as i8
        }
    }
}

/// `SpiceImageCompression` (spice-protocol `enums.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Compression {
    #[value(name = "auto-glz")]
    AutoGlz,
    #[value(name = "auto-lz")]
    AutoLz,
    Quic,
    Glz,
    Lz,
    Lz4,
    Off,
}

impl Compression {
    /// The `SpiceImageCompression` value.
    pub fn wire(self) -> u8 {
        match self {
            Compression::Off => 1,
            Compression::AutoGlz => 2,
            Compression::AutoLz => 3,
            Compression::Quic => 4,
            Compression::Glz => 5,
            Compression::Lz => 6,
            Compression::Lz4 => 7,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn verbosity_flags() {
        let c = Cli::parse_from(["spiceport", "-vv", "spice://h"]);
        assert_eq!(c.verbosity(), 2);
        let c = Cli::parse_from(["spiceport", "-q", "spice://h"]);
        assert_eq!(c.verbosity(), -1);
        assert!(Cli::try_parse_from(["spiceport", "-q", "-v", "spice://h"]).is_err());
    }

    #[test]
    fn secure_channels_list() {
        let c = Cli::parse_from(["spiceport", "--secure-channels", "main,inputs", "spice://h"]);
        assert_eq!(c.secure_channels, ["main", "inputs"]);
    }
}
