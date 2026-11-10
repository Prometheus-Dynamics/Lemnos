//! The video codec blocks behind the vendor kernel's `/dev/mpp_service`, given to a caller: a
//! claim on one block, the caller's dma-bufs, jobs submitted, and their completions. No vendor
//! library: a job is the register values the caller wants on the block, and the driver carries
//! them without reading what they mean. Encoder settings, bitstream parsing and every other part
//! of a codec are the caller's.
//!
//! - [`Service`] and [`Claim`]: one claim at a time on the service, bound to one [`Block`] behind
//!   an IOMMU; the buffers it holds; [`Claim::submit`] and [`Claim::complete`] (see
//!   [`claim`] for the backoff and what dropping a claim does).
//! - [`Job`]: register values, the [`Address`] registers that hold buffers, and the registers to
//!   read back. Every path by which a job's numbers become a descriptor, an address, an index or a
//!   length is checked against the block and the claim's buffers; [`job`] lists them.
//! - [`uapi`]: the kernel's request number, message layout, commands, flags and client types.
//! - [`Session`] and [`Device`]: the system side, which issues the request.
//!
//! # Not yet on a device
//!
//! The request carries a chain of messages whose `data_ptr` the kernel follows, and a register
//! read-back the kernel writes during a later call, so issuing it needs `unsafe`. This crate has
//! none, and neither `lemnos-linux-sys` nor a `-sys` crate offers a safe call that issues it
//! ([`lemnos_linux_sys::Ioctl`] can only be made inside that crate, and is for arguments without
//! pointers). So no [`Session`] that opens the node ships here: the driver is complete up to the
//! [`Session`] trait and is exercised against a played session in the tests. What a session must
//! do is stated on the trait.
//!
//! Hardware status: untested on board. Written from the vendor kernel driver's source; the check
//! is `testing/board-runs/E16.md`.
//!
//! Provenance: written for this tree from the kernel side of `/dev/mpp_service`
//! (rockchip-linux/kernel `develop-6.1` at `77168c8d5ab82399f65a80e9f807b50ba37cf483`:
//! `include/uapi/linux/rk-mpp.h` and `drivers/video/rockchip/mpp/`). Interface facts only are
//! transcribed, with their files named where they are used; no code is copied, and the user-space
//! MPP library is neither used nor followed.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod block;
pub mod claim;
pub mod job;
pub mod message;
pub mod uapi;

#[cfg(test)]
mod tests;

pub use block::{Block, Direct, Facts, MAX_REGISTERS};
pub use claim::{
    BufferError, Claim, ClaimError, CompleteError, Completion, Device, MAX_BUFFERS, Service,
    Session, SubmitError, backoff_ns,
};
pub use job::{Address, BufferId, Job, JobError, MAX_ADDRESSES, ReadBack};
pub use message::{Message, Payload, READ_BACK_BYTES, READ_BACK_SLOTS};
