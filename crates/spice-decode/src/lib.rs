//! Pure image decoders for SPICE's compression formats.
//!
//! Every decoder takes untrusted bytes and returns an [`Image`] or a
//! [`DecodeError`]; none of them may panic on bad input.

#![forbid(unsafe_code)]

pub mod bitmap;
pub mod image;
pub mod jpeg;
pub mod lz4;

pub use image::{DecodeError, Format, Image, Result};
