//! The K230's video codec block (an Arm Linlon VPU, H.264, H.265 and JPEG, encode and decode)
//! through the SDK kernel's `mvx` V4L2 memory-to-memory driver, given to a caller: a claim on a
//! codec session, the caller's dma-bufs, bitstream and frames submitted, and their completions.
//! No vendor library: the block runs firmware the kernel loads itself, and the driver carries the
//! caller's bytes and settings without reading them. Parsing a stream, choosing an encoder's
//! settings and every other part of a codec are the caller's.
//!
//! - [`Vpu`] and [`Claim`]: a session opened with a [`Setup`] (direction, both formats, buffer
//!   counts, controls), the buffers it holds, [`Claim::submit`] and [`Claim::give`], and
//!   completions from both sides (see [`claim`] for the order, the backoff and what dropping a
//!   claim does).
//! - [`Source`] and [`Destination`]: one dma-buf per memory plane, each checked against the size
//!   the claim measured before anything is queued; [`job`] lists every path by which a caller's
//!   number becomes memory the block reaches.
//! - [`uapi`]: the driver's name and its private formats, flags, controls and requests,
//!   transcribed and pinned by layout tests.
//! - [`Device`]: the node; [`LinuxDevice`] on Linux.
//!
//! # Not covered
//!
//! - **The private requests** (`VIDIOC_S_MVX_*`: regions of interest, QP maps, SEI user data, the
//!   rate-control block, downscaling, OSD, crops, colour description and the rest). They are
//!   transcribed and pinned in [`uapi`] but never issued: issuing them needs a request
//!   `lemnos-linux-sys` does not make public, and so a crate allowed `unsafe`. Their settings that
//!   are also controls go through [`Claim::set_control`].
//! - **The private buffer flags and `reserved2` bits** on queued buffers (rotation, mirroring,
//!   scaling, regions, general buffers, mini-frames, OSD): queued buffers carry none. The general
//!   and mini-frame bits make the kernel read headers and offsets from the planes' reserved
//!   words, and on the source queue it indexes `bufs[index]` before the core checks the index.
//! - **MMAP and user-pointer buffers.** Only the caller's dma-bufs.
//! - **Single-planar formats.** Multi-planar only; the driver's single-planar path moves
//!   `bytesused` and offsets around on dequeue.
//! - **Two controls**: [`REFUSED_CONTROLS`].
//! - **A contract.** No merged contract describes a codec session; this is a library, and nothing
//!   is registered in `lemnosd`.
//!
//! # Source
//!
//! Interface facts only, no code copied, from the Kendryte K230 Linux SDK
//! (`github.com/kendryte/k230_linux_sdk`) at commit `fc6cc2151d54cb307be47f81815023f7ffb0eae6`,
//! whose `buildroot-overlay/configs/k230_canmv_defconfig` builds `github.com/ruyisdk/linux-xuantie-kernel`
//! at `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529`; in it `drivers/media/platform/canaan/vpu/`:
//! `mvx-v4l2-controls.h` (see [`uapi`]), `mvx_ext_v4l2.c` (one video node, the requests it
//! takes), `mvx_v4l2_vidioc.c` (`QUERYCAP`, `QBUF`, `DQBUF`, the queues' `vb2_dma_sg` memory and
//! plane sizes), `mvx_v4l2_buffer.c` (how `bytesused`, `data_offset` and flags become the
//! firmware's buffer), `mvx_buffer.c` (`mvx_buffer_filled_set`, the per-session mapping),
//! `mvx_mmu.c` (the block's page tables), `mvx_session.c` and `mvx_v4l2_ctrls.c` (the controls).
//! None of the SDK's kernel patches (`buildroot-overlay/linux/`) touch the codec.
//!
//! Hardware status: untested on board. The board check is in `testing/board-runs/E21.md`.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod claim;
pub mod job;
mod linux;
mod setup;
pub mod uapi;

#[cfg(test)]
mod tests;

pub use claim::{
    BufferError, Claim, ClaimError, Completion, ControlError, Device, MAX_BUFFERS, SubmitError,
    Vpu, backoff_ns,
};
pub use job::{BufferId, Destination, JobError, Payload, Source};
pub use lemnos_driver_m2m_decode::{PixFormatMplane, PlaneFormat, Ready, SourceChange};
pub use linux::LinuxDevice;
pub use setup::{Control, Direction, REFUSED_CONTROLS, Setup, SetupError};

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
