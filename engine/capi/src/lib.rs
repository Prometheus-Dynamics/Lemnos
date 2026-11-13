//! The Lemnos client behind a small, versioned set of `extern "C"` functions
//! ([`surface`], declared in `include/lemnos.h`), and [`Client`], a Rust
//! wrapper that calls through those same functions.
//!
//! The surface is one call per request or value, never per field in a loop, so
//! crossing a shared library costs the request, not the call.
//!
//! The unsafe code of the C boundary lives here, one operation per block, each
//! with a `// SAFETY:` comment. Everything public in Rust is safe to call.

mod ffi;
#[cfg(target_os = "linux")]
mod single;
pub mod surface;
mod wrapper;

pub use wrapper::{Client, Stream, StreamFrame, Value};

#[cfg(test)]
mod tests;
