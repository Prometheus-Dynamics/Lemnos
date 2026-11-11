//! The Rockchip NPU behind the vendor kernel's DRM node (`rknpu`), serving `npu/1`. No vendor
//! library: a program is the caller's bytes, which the driver hands to the block without reading
//! them, and the vendor's runtime and model tools are neither used, read nor followed.
//!
//! - [`driver`]: [`Rknpu`], the `#[driver]` type the daemon runs: bound from the board file, the
//!   contract's methods on a claim and the device's job stream, health, ticks and the failure
//!   rule.
//! - [`program`]: the framing a program is loaded in, a header and a table of places where the
//!   driver writes buffer addresses; the body is never read.
//! - [`request`]: the only requests sent, each from a checked constructor, and every field the
//!   kernel takes from user space with what it does with it (from `lemnos-driver-rknpu-sys`).
//! - [`claim`]: [`Service`] and [`Claim`], one claim on the node at a time; programs, each in an
//!   IOMMU domain that holds only its own objects; runs from the job stream; [`Served`], the
//!   contract on a claim and a stream. What the kernel maps while a job runs, and why each check
//!   is there, is written out at the top of [`claim`].
//! - [`uapi`]: the kernel's request numbers, layouts, flags and limits (from
//!   `lemnos-driver-rknpu-sys`).
//! - [`node`]: the traits on `lemnos-driver-rknpu-sys`'s node and session, and finding the node.
//!
//! # What holds and what does not
//!
//! A program can name any address, so the only sound limit is what the block's IOMMU maps while
//! it runs. With this driver, the domain a job runs in holds that job's program body, scratch and
//! attached buffers' copies, and nothing else the driver made: other programs live in other
//! domains, nothing is imported, objects whose destroy failed keep their domain out of use. That
//! holds as long as the daemon is the node's only opener; the kernel's domains are shared by
//! every opener, and nothing in user space can see another opener's objects. On a kernel built
//! without domain switching every program shares one domain, so one program is loaded at a time.
//!
//! Not known from the source, and checked on a board (`testing/board-runs/E32.md`): whether the
//! block's register command stream can reach the registers of its own IOMMU. If it can, a
//! program could change its own mapping, and no driver in user space could confine it; a Lemnos
//! kernel path for the block would be the fix.
//!
//! # On a device
//!
//! The requests need `unsafe` (the kernel writes into them, and the CPU maps objects), which this
//! crate does not hold. The node and session that reach the device are in
//! `lemnos-driver-rknpu-sys`, which holds the layouts and checked requests too; [`node`]
//! implements [`Device`] and [`Session`] on them. The tests here run the claim and the driver,
//! through a real host, against a kernel played the way `drivers/rknpu/` maps objects into
//! domains.
//!
//! Hardware status: untested on board. Written from the vendor kernel driver's source; the checks
//! are `testing/board-runs/E32.md` and, for the daemon driver, `testing/board-runs/G2.md`.
//!
//! Provenance: written for this tree from the kernel side of the node (rockchip-linux/kernel
//! `develop-6.1` at `77168c8d5ab82399f65a80e9f807b50ba37cf483`: `drivers/rknpu/` and its
//! `include/rknpu_ioctl.h`). Interface facts only are transcribed, with their files named where
//! they are used; no code is copied.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod claim;
pub mod driver;
pub mod node;
pub mod program;

pub use lemnos_driver_rknpu_sys::{request, uapi};

#[cfg(test)]
mod tests;

pub use claim::{
    Claim, ClaimError, Device, Domains, Failure, MAX_BUFFERS, MAX_JOBS_PER_ROUND, MAX_PROGRAMS,
    Served, Service, Session, backoff_ns,
};
pub use driver::{JOB_STREAM, Rknpu, RknpuNeeds};
pub use program::{FramingError, Header, Relocation, Target};
pub use request::{Create, Domain, Kind, Object, Query, RUN_TIMEOUT_MS, Run, RunError};
