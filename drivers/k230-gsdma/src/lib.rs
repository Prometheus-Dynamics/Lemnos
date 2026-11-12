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
//! - [`LinuxDevice`] and [`LinuxSession`]: the [`Device`] on Linux, over the engine's node in
//!   `lemnos.ko`.
//!
//! # How a job reaches the engine on Linux
//!
//! The vendor kernel gives user space no way to reach the engine: its driver
//! (`drivers/soc/canaan/k230-gdma.c`, added by the SDK's patch 0023) registers no device node and
//! only serves the display driver, and its own address check (`k230_dma_addr_in_ddr`, patch
//! 0029) only keeps an address in the first 2 GiB. The engine takes 32-bit bus addresses and has
//! no MMU, so driving its registers from user space would let a job name any memory. So the
//! engine is driven by `lemnos.ko` instead (its `gsdma` parameter names the block), and its
//! registers never reach user space. A [`LinuxSession`] is a frame stream on that node: each
//! buffer the claim holds is registered on the stream, which pins it; a job is the checked
//! [`Plan`] as device addresses. The module reads the job once into its own memory, works out
//! every byte the engine will read and write on each plane, proves each span inside one
//! registered buffer, below the engine's reach and clear of every other plane it writes, and
//! only then builds the descriptors in memory of its own. A job that runs too long is stopped
//! there (the channel turned off and on, as the vendor driver does) and ends as
//! [`Completion::stopped`]; an engine that can't be shown stopped keeps the job's memory until a
//! reboot. [`linux`] lists the checks this side makes first.
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
//! Hardware status: untested on board. The board checks are in `testing/board-runs/E22.md` and
//! `testing/board-runs/H7b.md`.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod claim;
pub mod job;
pub mod linux;
pub mod uapi;

#[cfg(test)]
mod tests;

pub use claim::{
    BufferError, Claim, ClaimError, CompleteError, Completion, Device, Finished, Gsdma,
    MAX_BUFFERS, Request, Session, SubmitError, TIMEOUT, backoff_ns,
};
pub use job::{
    AddressError, BufferId, Format, Frame, Job, JobError, MAX_FIELD, Plan, Plane, Rotation, Span,
    Turn,
};
pub use linux::{LinuxDevice, LinuxSession, Settings};

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
