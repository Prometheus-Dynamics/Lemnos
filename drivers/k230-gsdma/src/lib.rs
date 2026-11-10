//! The K230's graphics DMA engine (the "GDMA", or GSDMA in the SoC's manual): copies image
//! planes, turned by quarter turns and mirrored, from one place in memory to another. What its
//! kernel interface is, jobs with every address, stride and extent proved inside buffers the
//! caller holds, the descriptors the engine walks, and a claim over a small [`Device`] trait. No
//! vendor library.
//!
//! - [`uapi`]: the node, the registers, the 24-byte descriptor and its codes, transcribed with
//!   their source and pinned by layout tests.
//! - [`job`]: [`Job`], [`Frame`] and [`Plane`], checked into a [`Plan`] against the size of each
//!   buffer the claim measured; [`job`] lists every path by which a caller's number becomes
//!   memory the engine reads or writes, and the check on each. [`Plan::descriptors`] turns a
//!   plan into the engine's descriptors, checking every plane's end against the memory the
//!   engine may reach.
//! - [`Gsdma`] and [`Claim`]: a claim on the engine, the buffers it holds, one job submitted and
//!   completed at a time, and a backoff doubling from 250 ms to 8 s after a failed open. It never
//!   gives up and never turns itself off. Submitting and completing allocate nothing.
//!
//! # Not here: a [`Device`] for Linux
//!
//! The reference gives a caller no way to reach the engine, so no job reaches it yet. Exactly:
//!
//! - **No interface.** The kernel driver (`drivers/soc/canaan/k230-gdma.c`, added by the SDK's
//!   patch 0023) registers no device node, request or mapping. It exports one kernel function,
//!   `canaan_gdma_rotate_sync`, which only the display driver calls, to turn a framebuffer into a
//!   buffer the display driver allocated itself for scan-out. Nothing in user space can name a
//!   destination.
//! - **Nothing in front of the engine.** The engine takes 32-bit bus addresses and has no MMU.
//!   The kernel driver's own check (`k230_dma_addr_in_ddr`, patch 0029) only keeps an address in
//!   the first 2 GiB, which is all of memory; it does not know which buffer an address belongs
//!   to, how wide a row is, or how far a turned plane reaches. Driving the registers from user
//!   space (a register window, `/dev/mem`) would let a job name any memory, and cannot be made
//!   safe there. It is left out, on purpose.
//!
//! What is missing is a kernel interface that takes jobs as dma-bufs: a node with one request
//! carrying, per plane, a dma-buf descriptor, an offset and a pitch on each side, plus the
//! format, size and rotation, which pins the buffers, refuses one the engine cannot see in one
//! piece, and builds the descriptors from their bus addresses. The [`Session`] trait is that
//! request's shape; [`Plan`] is what such an interface needs, already checked; and
//! [`Plan::descriptors`] is the chain it would write.
//!
//! # Source
//!
//! Interface facts only, no code copied, from the Kendryte K230 Linux SDK
//! (`github.com/kendryte/k230_linux_sdk`) at commit `fc6cc2151d54cb307be47f81815023f7ffb0eae6`:
//! `buildroot-overlay/linux/0023-add-gdma-vo-rotation.patch` (the driver, its header, and the
//! display driver's use of it in `drivers/gpu/drm/canaan/canaan_plane.c`),
//! `0029-canaan-add-harden-DMA-address-validation.patch` (the address check) and
//! `0019-dts-add-nonai2d.patch` (the node), applied to the kernel the SDK builds
//! (`github.com/ruyisdk/linux-xuantie-kernel` at `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529`). See
//! [`uapi`].
//!
//! Hardware status: untested on board. The board check is in `testing/board-runs/E22.md`.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod claim;
pub mod job;
pub mod uapi;

#[cfg(test)]
mod tests;

pub use claim::{
    BufferError, Claim, ClaimError, CompleteError, Completion, Device, Gsdma, MAX_BUFFERS, Request,
    Session, SubmitError, TIMEOUT, backoff_ns,
};
pub use job::{
    AddressError, BufferId, Format, Frame, Job, JobError, MAX_FIELD, Plan, Plane, Rotation, Span,
    Turn,
};

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
