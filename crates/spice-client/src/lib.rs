//! SPICE client: transport, link handshake, session and channels.
//!
//! This crate has no UI dependency. It reports what happens through
//! [`ClientEvent`]s and takes [`ClientCommand`]s from the frontend.

pub mod audio;
pub mod channel;
pub mod cursor;
mod cursor_ch;
mod display_ch;
pub mod error;
pub mod events;
mod inputs_ch;
pub mod link;
mod main_ch;
mod playback_ch;
mod record_ch;
pub mod session;
mod text;
mod tls;
pub mod transport;
pub mod uri;
mod video;

pub use error::{ClientError, Result};
pub use events::{
    ClientCommand, ClientEvent, CursorShape, EventSink, InputCmd, MouseMode, SharedSurface,
    SurfaceData,
};
pub use session::{SessionConfig, TlsOptions, run};
pub use uri::{SpiceUri, UriError};

/// Channel type for a name such as `"display"` (`spice_channel_string_to_type`).
pub fn channel_type_from_name(name: &str) -> Option<u8> {
    spice_proto::enums::channel_type::from_name(name)
}
