//! Lemnos's C surface: the `lemnosd` device client behind a small, versioned
//! set of `extern "C"` functions ([`surface`], declared in `include/lemnos.h`),
//! and [`Client`], a Rust wrapper that calls through those same functions.
//!
//! This is the first core, surface and wrapper set: the core is
//! `lemnos-ipc`'s `DeviceClient`, the surface is this crate's exports
//! (`staticlib` and `cdylib`), and the wrapper is [`Client`], linked
//! statically. Its building blocks (status codes, handles, the version
//! handshake, the panic guard) are `lemnos-abi`'s.
//!
//! Granularity: one call per device read or subscription event, never per
//! channel value in a loop, so a call across a shared library costs the
//! reading, not the call.
//!
//! The unsafe code of the C boundary lives here, one operation per block, each
//! with a `// SAFETY:` comment. Everything public in Rust is safe to call.

mod client;
pub mod surface;

pub use client::{Channel, Client, Device, Error, Reading};
pub use surface::{ChannelInfo, DeviceInfo, MAX_CHANNELS, RawReading, VERSION};

#[cfg(test)]
mod tests;
