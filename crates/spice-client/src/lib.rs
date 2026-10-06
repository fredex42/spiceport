//! SPICE client: transport, link handshake, session and channels.
//!
//! This crate has no UI dependency. It reports what happens through
//! [`ClientEvent`]s and takes [`ClientCommand`]s from the frontend.

pub mod uri;

pub use uri::{SpiceUri, UriError};
