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
//! # Not here
//!
//! **No Linux [`Device`].** Issuing a request needs `unsafe`, and `lemnos-linux-sys` has no
//! public request this crate can use (only the request-number encoders and the dma-buf size are
//! public). It belongs in a crate allowed `unsafe` (a `-sys` crate for this block, or a module of
//! `lemnos-linux-sys`): the requests in [`uapi`], with `DI_IOC_PROCESS_FB` taking only a
//! [`Job`]'s [`request`](Job::request). That crate must also never map the node (its `mmap` maps
//! any physical page the offset names), never issue `DI_IOC_MEM_REQUEST` (it answers physical
//! addresses), and should refuse to open on a kernel built with `CONFIG_SUNXI_DI_SINGEL_FILE`,
//! whose structures are larger than these (see [`uapi`]).
//!
//! **No contract.** No merged contract describes a deinterlacer (the image contracts scale,
//! convert and blend). The crate is a library; nothing is registered in `lemnosd`.
//!
//! Also not covered: the tuning tables (`DI_IOC_GET_TNRPARA`, `DI_IOC_SET_TNRPARA`), the demo
//! crop, crops smaller than the frame, and the timeouts (the kernel's 3 s defaults stand).
//!
//! Hardware status: untested on board. The board check is in `testing/board-runs/E20.md`.
//! Unverified until it runs: which `base_field` value is the top field (the header's comment and
//! the kernel's log disagree), and whether the block reads past a row's last byte.

#![forbid(unsafe_code)]

mod engine;
mod job;
pub mod uapi;

pub use engine::{Deinterlacer, Device, Processed, RunError, State, backoff_ns};
pub use job::{
    Config, Field, Format, Frame, FrameError, Invalid, Job, Layout, Level, MAX_SIZE, MAX_STRIDE,
    Mode, Settings, Tnr,
};

#[cfg(all(test, target_os = "linux"))]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
