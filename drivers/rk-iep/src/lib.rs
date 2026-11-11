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
//! The call into the service goes through [`Device`], and nothing implements it on a real
//! node yet. The shared service layer (`drivers/rk-mpp`, issued by `drivers/rk-mpp-sys`) runs
//! register-file jobs: a write, the translation table of the job's address registers, offsets
//! and a read-back, each checked against the block's registers. An IEP2 job is another shape:
//! its descriptors sit at fixed words inside a parameter structure, the kernel translates
//! those words itself with no table, and the messages carry `MPP_FLAGS_REG_NO_OFFSET`. The
//! shared layer needs that second job type before this crate can move onto it: a block whose
//! facts are the parameter structure's size and the words that hold descriptors, a job that
//! must place a held buffer in every one of those words, and the session's chain check
//! taught the shape. Until then no deinterlace job reaches this block.
//!
//! No contract is served. `codec.block/1` carries a deinterlace job as it stands (the A733's
//! deinterlacer is served that way), and this block will be served under it once it runs.
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
