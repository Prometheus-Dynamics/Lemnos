//! The `k230-capture` driver's raw layer: the ISP8000's frame path behind the vendor kernel's
//! `vvcam-isp` device, and the CSI receiver behind its `vvcam-mipi` device.
//!
//! - [`regs`]: the frame path's registers, with their sources and what is unconfirmed.
//! - [`access`]: which of them a caller may write ([`access::WRITABLE`]) and read. None holds an
//!   address, a size the memory interface wraps at, an offset counter or a path enable.
//! - [`memory`]: [`FrameMemory`], DMA memory from the host checked to be reachable, and
//!   [`Planes`], a frame's planes checked against each other.
//! - [`mipi`]: the `vvcam-mipi` request and its structure, and a checked builder for it.
//! - `capture` (Linux): [`Capture`], which writes the table's registers, and alone writes the
//!   memory interface's address registers, from memory it holds; see its module documentation for
//!   every path by which a caller's number reaches the block and the check on each.
//! - `node` (Linux): [`MipiNode`], and the `vvcam-isp` requests, crate-private.
//!
//! # Why the checks are here
//!
//! The vendor driver writes `base + addr` for any 32-bit `addr` it is given, and the window holds
//! the memory interface, whose registers take 32-bit bus addresses with no MMU in front of them.
//! A safe function that wrote a caller's value there would let a caller make the block write any
//! memory. So the address registers are not writable through [`Capture::write`] at all:
//! [`Capture`] points them only into [`FrameMemory`] it holds, and only switches the path on once
//! they all do. The address of a [`FrameMemory`] is the host's (`DmaBuffers`), never a caller's.
//!
//! # Why not the vendor's own capture path
//!
//! The SDK's V4L2 capture node does import a dma-buf and work out its bus address in the kernel,
//! but it hands that address only to its binary daemon, by writing it into a kernel page the
//! daemon must map by its physical address (`vvcam_isp_qbuf_event`,
//! `buildroot-overlay/package/vvcam/v4l2/isp/vvcam_isp_event.c`); the node has no `mmap` for it,
//! and the first format request fails if no daemon acknowledges in that page. That needs raw
//! physical memory access from user space, so it is not used, and the stream's own dma-buf
//! buffers cannot be filled by the block: nothing on this kernel gives user space the bus address
//! of a dma-buf.
//!
//! Requests the devices have and this crate does not issue: `VVCAM_ISP_FE_START`,
//! `VVCAM_ISP_CLOSE_IRQ` and `VVCAM_ISP_RESET` (see the k230isp sys crate), `mmap` of the
//! register window, and `VVCAM_MIPI_RESET` and `VVCAM_MIPI_SET_STREAM` (see [`mipi`]).
//!
//! Unsafe code: one `ioctl` call in `node`, over integer-only structures.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/E30.md`.

#![cfg_attr(not(target_os = "linux"), no_std)]

pub mod access;
#[cfg(target_os = "linux")]
mod capture;
pub mod memory;
pub mod mipi;
#[cfg(target_os = "linux")]
mod node;
pub mod regs;

#[cfg(target_os = "linux")]
pub use capture::{Capture, CaptureError, Dma, EID_MIV2_MIS, MiEvent, Output, RELEASE_AFTER, RING};
pub use memory::{FrameMemory, MemoryError, Plane, Planes};
#[cfg(target_os = "linux")]
pub use node::MipiNode;

#[cfg(all(test, target_os = "linux"))]
mod tests;
