//! Server message demarshallers and client message marshallers, written by
//! hand from spice-common `spice.proto` for the subset spiceport uses.

pub mod base;
pub mod cursor;
pub mod display;
pub mod inputs;
pub mod main;

use bytes::BytesMut;

/// A client → server message (`SPICE_MSGC_*`).
pub trait ClientMsg {
    /// The `SPICE_MSGC_*` message type.
    const TYPE: u16;
    fn encode(&self, buf: &mut BytesMut);
}

/// Read a NUL-terminated or length-delimited string leniently.
pub(crate) fn lossy_cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}
