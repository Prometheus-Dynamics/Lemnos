//! The Rockchip IEP2 deinterlacer, behind the vendor kernel's `/dev/mpp_service`. No vendor
//! library: the caller chooses the mode and the detectors' settings, and the driver carries them
//! to the block without reading a pixel.
//!
//! - [`Job`]: three input frames, two outputs and the block's two scratch buffers, each placed in
//!   a dma-buf the caller holds and checked against its size, so a job cannot reach memory the
//!   caller does not hold (see the [`job`] module for every check).
//! - [`Iep`]: opens the service, binds it to the IEP2 and runs jobs. A device failure closes the
//!   file and backs off (250 ms doubling to an 8 s cap, cleared by a job that runs); it never
//!   gives up and never turns itself off.
//! - [`Params`], [`Results`], [`mpp`] and [`regs`]: the layouts the vendor kernel takes, with their
//!   sources, pinned by the layout tests.
//!
//! The call into the service goes through [`Device`]. Issuing it needs `unsafe` (the messages
//! carry pointers into this process), which this crate does not hold, and no crate on `next`
//! offers it yet: it belongs in the shared service layer (`drivers/rk-mpp`), and this crate moves
//! onto that layer when it lands.
//!
//! No contract is served: none of the merged image contracts describes deinterlacing.
//!
//! Left out: the `I1O1T` mode (the block reads past its buffer), the planar output (it has no third
//! address), the `P0` chroma layout, the output-mode bit and the ROI.
//!
//! Hardware status: untested on board. Written from the vendor driver's source; the check is
//! `testing/board-runs/E17.md`.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

mod device;
mod iep;
pub mod job;
pub mod mpp;
mod params;
pub mod regs;

#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod tests;

pub use device::{Device, find};
pub use iep::{Iep, RunError, State, backoff_ns};
pub use job::{
    At, Buffer, Chroma, Data, Frame, Input, Job, JobError, MAX_HEIGHT, MAX_STRIDE, MAX_WIDTH, Mode,
    Output, STRIDE_ALIGN, Slot, TILE_HEIGHT, TILE_WIDTH, scratch_len,
};
pub use params::{
    Addr, DilMode, FieldOrder, MTN_TAB_LEN, MV_HIST_BINS, OSD_AREAS, Params, PdComp, Results,
    Sampling, Swap, Tuning,
};
