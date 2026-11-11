//! The A733's deinterlacer (`DI301`) through the vendor kernel's `/dev/deinterlace`: weave, bob,
//! motion-adaptive deinterlacing at frame or field rate, and temporal noise reduction, between
//! dma-bufs. No vendor library: the block is driven only through the kernel's own requests, and
//! the driver carries the caller's frames without reading a pixel.
//!
//! - [`uapi`]: the requests and structures, transcribed with their source and pinned by layout
//!   tests.
//! - [`Frame`], [`Config`] and [`Job`]: a request, built only from frames checked against the real
//!   size of the dma-buf each sits in. The kernel driver checks none of it, takes a negative
//!   descriptor as a physical address, and leaves a zero chroma offset at address 0, so nothing
//!   else builds one.
//! - [`Deinterlacer`]: a block that opens itself, applies a job's settings when they change,
//!   runs jobs, and after a failure closes and reopens, with a backoff doubling from 250 ms and
//!   capped at 8 s. It never gives up and never turns itself off. A job allocates nothing.
//! - [`Device`]: the kernel operations the deinterlacer runs on, one per request.
//!
//! # On a device, and for a caller
//!
//! The requests, the structures and the checked [`Job`] live in `lemnos-driver-a733-di-sys`,
//! which holds the `unsafe`, never maps the node and never issues the requests that deal in
//! physical addresses; this crate re-exports them. [`LinuxDevice`] is the [`Device`] on the
//! real node. [`A733Di`] gives the block to a caller as `codec.block/1` ([`codec`]): a job is
//! the settings and the frames, each frame a part of a buffer of the job stream.
//!
//! Also not covered: the tuning tables (`DI_IOC_GET_TNRPARA`, `DI_IOC_SET_TNRPARA`), the demo
//! crop, crops smaller than the frame, and the timeouts (the kernel's 3 s defaults stand).
//!
//! Hardware status: untested on board. The board check is in `testing/board-runs/E20.md`.
//! Unverified until it runs: which `base_field` value is the top field (the header's comment and
//! the kernel's log disagree), and whether the block reads past a row's last byte.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod codec;
mod engine;
mod linux;

pub use lemnos_driver_a733_di_sys::uapi;

pub use codec::{A733Di, DiBlock};
pub use engine::{Deinterlacer, Device, Processed, RunError, State, backoff_ns};
pub use linux::LinuxDevice;

pub use lemnos_driver_a733_di_sys::{
    Config, Field, Format, Frame, FrameError, Invalid, Job, Layout, Level, MAX_SIZE, MAX_STRIDE,
    Mode, Settings, Tnr,
};

#[cfg(all(test, target_os = "linux"))]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
