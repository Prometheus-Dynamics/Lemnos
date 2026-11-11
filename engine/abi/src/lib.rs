//! The pieces every C-ABI surface over Lemnos needs, written once: a surface
//! (an `extern "C"` layer over a Rust core) and its wrappers use these instead
//! of inventing their own.
//!
//! - [`Status`]: an `i32` result. 0 is success, a negative Linux errno is that
//!   errno, and the codes below [`Status::LEMNOS_BASE`] are this crate's.
//! - [`Handle`]: a generation-counted `u64`; 0 is null. [`Slab`] (feature
//!   `alloc`) hands them out and refuses stale ones.
//! - [`BufferDesc`] and [`FenceDesc`]: a dma-buf frame and a fence, by file
//!   descriptor.
//! - [`MessageSink`]: a C callback that takes a message, such as an error's
//!   text.
//! - [`AbiVersion`]: the handshake, with a hash of the layouts above.
//! - [`Features`]: a surface's optional features, a `u64` bit set from its
//!   own call, beside the version.
//! - [`write_out`] and [`write_text`]: a value into a buffer the caller
//!   supplies, all or nothing, with the length it needs
//!   ([`Status::BUFFER_TOO_SMALL`] when it does not fit).
//! - [`guard`] (feature `std`): runs an entry point's body and turns a panic
//!   into [`Status::PANIC`] instead of unwinding into C.
//!
//! Every type that crosses the boundary is `#[repr(C)]` or
//! `#[repr(transparent)]` over a fixed-size integer, so a value the peer does
//! not know (a newer status code, flag or fence kind) stays defined behaviour:
//! no Rust enum crosses. Surfaces sit at per-frame or per-job granularity, not
//! per pixel: a call across a shared-library boundary is not inlined.
//!
//! [`package`] is the boundary between a compiled driver file and the host that loads it, and
//! [`load`] (feature `load`, Linux) is how the host gets such a file into its process.
//!
//! `no_std`. No dependencies unless feature `load` is on. Unsafe code only in [`package`] and
//! [`load`].

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod desc;
mod features;
#[cfg(feature = "std")]
mod guard;
mod handle;
#[cfg(all(feature = "load", target_os = "linux"))]
pub mod load;
mod out;
pub mod package;
mod sink;
#[cfg(feature = "alloc")]
mod slab;
mod status;
mod version;

pub use desc::{BufferDesc, FenceDesc, MAX_PLANES, PlaneDesc};
pub use features::Features;
#[cfg(feature = "std")]
pub use guard::{guard, guard_or};
pub use handle::Handle;
pub use out::{Written, write_out, write_text};
pub use sink::MessageSink;
#[cfg(feature = "alloc")]
pub use slab::Slab;
pub use status::Status;
pub use version::{AbiVersion, LAYOUT_HASH, VERSION, layout_hash};

#[cfg(test)]
mod tests;
