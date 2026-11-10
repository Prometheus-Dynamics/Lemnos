//! The K230 non-AI 2D engine through the `nonai-2d` V4L2 mem2mem driver of the K230 Linux SDK:
//! pixel-format conversion between dma-bufs, at one size, up to 1920x1080. No vendor library:
//! the engine is driven only through the kernel's own V4L2 requests, and the driver carries the
//! caller's frames without reading a pixel.
//!
//! - [`Layout`] and [`PixelFormat`]: the formats the driver converts and the planes it computes
//!   for each (`nonai_2d_fill_plane_layout`). A layout is a format and a size the driver keeps
//!   unchanged; it chooses the strides itself.
//! - [`Frame`]: a frame in dma-bufs, one per plane, each checked to hold its plane by the size
//!   the kernel answers for it. A conversion takes nothing else.
//! - [`Device`]: the node set up for one source and one destination layout (multi-planar, one
//!   dma-buf buffer each side), through `lemnos-driver-m2m-decode`'s session.
//! - [`Nonai2d`]: an engine that opens itself for the layouts a conversion needs, and after a
//!   failure closes and reopens, with a backoff doubling from 250 ms and capped at 8 s. It never
//!   gives up and never turns itself off. A conversion in the layouts it is open for allocates
//!   nothing.
//! - [`Overlay`] and [`Device::draw`]: the OSD and border modes, images blended into a frame and
//!   rectangles drawn on it in place, every rectangle checked to lie inside the frame. See
//!   [`overlay`] for what reaches the engine and what checks it.
//! - [`Converter`]: `image.convert/1` jobs served from a stream whose buffers are dma-bufs. A job
//!   names two of the stream's buffers; the driver borrows their handles from the stream and
//!   builds checked frames over them. See [`convert`].
//!
//! # Not covered
//!
//! - Drawing through jobs or through [`Nonai2d`]'s reopening: no contract carries an overlay, so
//!   it is [`Device::draw`] on a device the caller opens, and a caller reopens it after a failure.
//! - The conversion coefficient and colour gamut controls, and the driver's three-plane RGB.
//! - The 4:2:0 formats through jobs: a job names one buffer and the engine takes each plane from
//!   the start of its own dma-buf. They go through [`Device`] and [`Nonai2d`] with a dma-buf per
//!   plane.
//! - The `image.convert/1` contract's methods: this is a library, as the other image drivers'
//!   job servers are; nothing is registered in `lemnosd`.
//!
//! A conversion that times out closes the node, and the job completes as failed (empty). The
//! kernel lets go of the dma-bufs when the node closes; an engine that is really still running
//! could write the output after that, which the empty completion tells the consumer to ignore.
//!
//! # Source
//!
//! Interface facts only, no code copied, from the Kendryte K230 Linux SDK
//! (`github.com/kendryte/k230_linux_sdk`) at commit `fc6cc2151d54cb307be47f81815023f7ffb0eae6`,
//! `buildroot-overlay/package/nonai2d/src/nonai_2d_dev.c`: the node's name (`NONAI_2D_NAME`),
//! `MAX_WIDTH` and `MAX_HEIGHT`, the format table and plane counts, the plane layouts
//! (`nonai_2d_fill_plane_layout`), the fall-back to the first format and the clamping in
//! `nonai_2d_vidioc_try_fmt`, the `vb2_dma_contig` queues taking dma-bufs, the per-plane address
//! from `vb2_dma_contig_plane_dma_addr` with no data offset (`device_run`), and the conversion
//! mode being the default (`K_2D_CALC_MODE_CSC`). Ported from the dev branch
//! (`crates/lemnos-drivers-nonai2d`).
//!
//! Hardware status: untested on board. The board checks are in `testing/board-runs/E5.md` and,
//! for drawing, `testing/board-runs/E6.md`.
//! Unverified until it runs: that the engine's byte order for each format is the V4L2 name's, and
//! that a 4:2:0 frame's chroma order is as named.

#![forbid(unsafe_code)]

pub mod layout;
pub mod overlay;

#[cfg(target_os = "linux")]
pub mod convert;
#[cfg(target_os = "linux")]
mod device;
#[cfg(target_os = "linux")]
mod engine;
#[cfg(target_os = "linux")]
mod frame;

#[cfg(target_os = "linux")]
pub use convert::Converter;
#[cfg(target_os = "linux")]
pub use device::{CONVERT_TIMEOUT, DRIVER, Device, Payloads, find};
#[cfg(target_os = "linux")]
pub use engine::{Engine, Nonai2d, RunError, State, backoff_ns, node};
#[cfg(target_os = "linux")]
pub use frame::{Frame, FrameError};
pub use layout::{
    Layout, LayoutError, MAX_HEIGHT, MAX_PLANES, MAX_WIDTH, PixelFormat, Plane, Planes,
};
pub use overlay::{Border, Mode, Order, OsdFormat, Overlay, OverlayError, Rect, Region};

#[cfg(all(test, target_os = "linux"))]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
