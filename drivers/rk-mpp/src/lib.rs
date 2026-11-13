//! The video codec blocks behind the vendor kernel's `/dev/mpp_service`, given to a caller: a
//! claim on one block, the caller's dma-bufs, jobs submitted, and their completions. No vendor
//! library: a job is the register values the caller wants on the block, and the driver carries
//! them without reading what they mean. Encoder settings, bitstream parsing and every other part
//! of a codec are the caller's.
//!
//! - [`Service`] and [`Claim`]: one claim at a time on the service, bound to one [`Block`] behind
//!   an IOMMU; the buffers it holds; [`Claim::submit`] and [`Claim::complete`] (see
//!   [`claim`] for the backoff and what dropping a claim does).
//! - [`ParamJob`]: a parameter block's job (the IEP2's), with every descriptor word placed by
//!   the driver from the claim's buffers and every word the kernel uses as an index, a size or a
//!   count checked; [`job`] lists the checks.
//! - [`Job`]: register values, the [`Address`] registers that hold buffers, and the registers to
//!   read back. Every path by which a job's numbers become a descriptor, an address, an index or a
//!   length is checked against the block and the claim's buffers; [`job`] lists them.
//! - [`MppBlock`]: a block as `codec.block/1` has it (see [`codec`] for its slots and item
//!   tags): a job's work becomes a [`Job`], its buffer references the claim's buffers.
//! - [`uapi`]: the kernel's request number, message layout, commands, flags and client types.
//! - [`Session`] and [`Device`]: the system side, which issues the request.
//!
//! # On a device
//!
//! The request carries a chain of messages whose `data_ptr` the kernel follows, and a register
//! read-back the kernel writes during a later call, so issuing it needs `unsafe`, which this crate
//! does not hold. The [`Session`] and [`Device`] that reach the node are in
//! `lemnos-driver-rk-mpp-sys` (`ServiceSession` and `ServiceNode`), which depends on this crate
//! for the traits, the messages and the layouts; this crate does not depend on it. The tests here
//! run the driver against a played session. What a session must do is stated on the trait.
//!
//! Hardware status: untested on board. Written from the vendor kernel driver's source; the check
//! is `testing/board-runs/E16.md`.
//!
//! Provenance: written for this tree from the kernel side of `/dev/mpp_service`
//! (rockchip-linux/kernel `develop-6.1` at `77168c8d5ab82399f65a80e9f807b50ba37cf483`:
//! `include/uapi/linux/rk-mpp.h` and `drivers/video/rockchip/mpp/`). Interface facts only are
//! transcribed, with their files named where they are used; no code is copied. Vendor user-space
//! code is governed by the 2026-11-13 ruling: register facts may be transcribed from open-source
//! vendor user-space code with provenance (source file and version); the libraries themselves are
//! neither used nor linked.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod block;
#[cfg(feature = "device")]
pub mod claim;
#[cfg(feature = "device")]
pub mod codec;
pub mod job;
pub mod message;
#[cfg(any(test, feature = "played"))]
pub mod played;
pub mod uapi;

#[cfg(all(test, feature = "device"))]
mod tests;

pub use block::{
    Block, Check, Descriptor, Direct, Facts, MAX_JOB_WORDS, MAX_REGISTERS, ParamFacts, Window, Word,
};
#[cfg(feature = "device")]
pub use claim::{
    BufferError, Claim, ClaimError, ClaimOf, CompleteError, Completion, Device, MAX_BUFFERS,
    OwnedClaim, Service, Session, SubmitError, backoff_ns,
};
#[cfg(feature = "device")]
pub use codec::{BlockClaim, BoxedDevice, MppBlock};
pub use job::{
    Address, BufferId, Job, JobError, MAX_ADDRESSES, MAX_RUNS, ParamAddress, ParamJob, ReadBack,
    Run,
};
#[cfg(any(test, feature = "device", feature = "played"))]
pub use message::{Message, Payload};
pub use message::{READ_BACK_BYTES, READ_BACK_SLOTS};
