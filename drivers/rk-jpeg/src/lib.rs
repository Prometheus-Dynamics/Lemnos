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
//! # Running a job on these blocks
//!
//! A caller runs JPEG jobs through the `rk-mpp` driver (`drivers/rk-mpp-codec`), which serves
//! both blocks as `codec.block/1` under the names `rk-mpp:jpeg-decoder` and
//! `rk-mpp:jpeg-encoder`: the caller supplies the whole register file as its work and names
//! the buffers behind the address registers, and the shared service layer places and checks
//! every address. That is the block rule: what a picture is coded with is the caller's.
//!
//! **This crate still builds no job of its own.** To build one it would have to know what the
//! registers mean: how far the block reads and writes from each address is fixed by fields
//! (sizes, strides, formats, stream and bitstream lengths) that the vendor kernel's driver
//! source does not name. It names the address registers, the start and status bits and the
//! picture-size fields, which is what [`regs`], [`Addresses`] and [`status`] hold for a
//! caller that builds register files. No other public kernel source was found to name the
//! rest, so no field layout is guessed here.
//!
//! # On a device
//!
//! The session runs on the shared service layer: [`open`] opens the service node through
//! `lemnos-driver-rk-mpp-sys`, whose session implements this crate's [`Device`] (bind to the
//! block, ask its hardware id, let go of the session's buffers). The copies of the service's
//! layouts in [`mpp`] are checked against the shared layer's by the tests.
//!
//! A job stream is not served from this crate: `rk-mpp` serves these blocks.
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
pub use device::{Device, find, open};
pub use regs::Variant;
pub use session::{Jpeg, SessionError, State, backoff_ns};
