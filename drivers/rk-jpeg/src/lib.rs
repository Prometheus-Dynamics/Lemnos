//! The Rockchip JPEG decoder and encoder, behind the vendor kernel's `/dev/mpp_service`. No vendor
//! library: what a picture is coded with is the caller's business.
//!
//! - [`regs`] and [`mpp`]: the register files as far as the vendor kernel names them (sizes,
//!   address tables, start and status registers and bits, the picture-size fields), and the
//!   service's messages, with their sources, pinned by the layout tests.
//! - [`Addresses`]: the address side of a job, each address register a checked place in a
//!   dma-buf the caller holds (see the [`address`] module for every check), and [`read_window`]
//!   for reading registers back inside the file.
//! - [`status`]: what a finished job reports in its read-back registers.
//! - [`Jpeg`]: a session bound to one block, which reopens itself with a backoff capped at 8 s and
//!   never gives up.
//!
//! **No job is submitted.** A job on these blocks is a whole register file, and how far the block
//! reads and writes from each address is fixed by registers (sizes, strides, formats, stream and
//! bitstream lengths) that the vendor kernel's source does not name. Without them a job cannot be
//! checked against the caller's buffers, so none is built; the layouts of those fields are what
//! is missing. Issuing the service call also needs `unsafe` this crate may not hold: it goes
//! through [`Device`], to be served by the shared service layer (`drivers/rk-mpp`), onto which this
//! crate moves when it lands.
//!
//! No contract is served: none of the merged image contracts describes JPEG coding.
//!
//! Hardware status: untested on board. Written from the vendor driver's source; the check is
//! `testing/board-runs/E18.md`.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod address;
mod device;
pub mod mpp;
pub mod regs;
mod session;
pub mod status;

#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod tests;

pub use address::{Addresses, At, Buffer, MAX_ADDRESSES, PlaceError, read_window};
pub use device::{Device, find};
pub use regs::Variant;
pub use session::{Jpeg, SessionError, State, backoff_ns};
