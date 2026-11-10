//! The G2D 2D engine through the BSP kernel's `/dev/g2d`: fill, copy with scaling and format
//! conversion, quarter turns and flips, and Porter-Duff blending of two RGB layers, between
//! dma-bufs. No vendor library: the engine is driven only through the kernel's own requests.
//!
//! - [`Surface`], [`Rect`] and [`Job`]: a request, built only from dma-bufs whose frame was checked
//!   against the dma-buf's real size and rectangles checked against those frames. The kernel
//!   driver checks neither, and its requests can name a physical address, so nothing else builds
//!   one. They live in `lemnos-driver-g2d-sys` with the `unsafe`; this crate has none.
//! - [`G2d`]: an engine that opens itself, runs jobs, and after a failure closes and reopens, with
//!   a backoff doubling from 250 ms and capped at 8 s. It never gives up and never turns itself
//!   off.
//!
//! The driver carries the caller's work and never reads a pixel. Running a job allocates nothing.
//!
//! Not covered: mask ROPs, batched mixer tasks, LBC rotation, stride alignment (a padded row is a
//! wider surface), YUV blend layers (the kernel driver's own check refuses them).
//!
//! Hardware status: untested on board. The structures' sizes and offsets are pinned against the C
//! compiler; no request has reached an engine. The board check is in
//! `testing/board-runs/E4.md`. Unverified until it runs: the fill colour's byte layout, the
//! NV12/NV21 chroma order, and the scaling limits.
//!
//! Provenance: ported from the dev branch (`crates/lemnos-drivers-g2d`, and
//! `crates/lemnos-linux-sys/src/g2d.rs` for the `-sys` crate).

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

mod engine;

pub use engine::{Engine, G2d, RunError, State, backoff_ns, node};
pub use lemnos_driver_g2d_sys::{
    Blend, ColorRange, DEVICE, Device, Gamut, HardwareVersion, Job, MAX_FRAME_BYTES, MAX_SIZE,
    PixelFormat, Rect, Surface, Turn,
};

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
