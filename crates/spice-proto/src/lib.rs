//! SPICE wire protocol types: link handshake, data headers, capabilities and
//! the server/client messages spiceport understands.
//!
//! Ported by hand from spice-protocol (`spice/protocol.h`, `spice/enums.h`,
//! `spice/vd_agent.h`) and spice-common's `spice.proto`. Each item names the C
//! identifier it mirrors so a grep finds both. Everything on the wire is
//! packed little-endian; we encode and decode field by field.
//!
//! This crate does no I/O.

#![forbid(unsafe_code)]

pub mod agent;
pub mod caps;
pub mod enums;
pub mod header;
pub mod logutil;
pub mod msg;
pub mod reader;

pub use reader::{ProtoError, Reader};

/// Writes little-endian values onto a `BytesMut`. Re-exported so callers don't
/// need their own `bytes` dependency to build client messages.
pub use bytes::{BufMut, Bytes, BytesMut};
