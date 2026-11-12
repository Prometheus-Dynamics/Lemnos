//! The Lemnos socket protocol. `docs/wire.md` is its specification.
//!
//! A frame names a device, a contract position and a method number, and its
//! arguments are bytes this crate never reads. Nothing here names a kind of
//! device.
//!
//! Encoding and decoding allocate nothing on the paths used in steady state:
//! a `Frame` borrows from the bytes it was read from.
//!
//! With no features, only [`ring`]: the shared ring of a frame stream, with no part of the
//! engine behind it, for a program that maps a stream's memory itself. The `protocol` feature
//! (on by default) adds the rest.

#[cfg(feature = "protocol")]
mod protocol;
pub mod ring;

#[cfg(feature = "protocol")]
pub use protocol::*;
