//! The Rockchip JPEG encoder block (the VPU720 class, `rk-mpp:jpeg-encoder`): its register file
//! and a builder that makes rk-mpp's `Job` for it. No vendor library; no JPEG policy.
//!
//! - [`vpu720`]: the block's 79-word file, every word and bit field as the vendor header
//!   (`hal_jpege_vpu720_reg.h`, rockchip-linux/mpp `9b56348`) declares it. Pinned by gcc and clang
//!   in `src/tests/pins.rs`.
//! - [`layout`]: the layout types the file uses.
//! - [`job`]: [`job::RegisterSet`], which takes the caller's values and buffers and makes the
//!   `Job`. Each check it makes, and what it does not check, is listed in its module docs.
//!
//! The caller chooses every value the block is not given by the picture: quantisation table
//! contents, rate and restart settings, and the bitstream's lengths. Nothing here writes a JPEG
//! header or a bitstream. Buffers come from a rk-mpp claim, by [`lemnos_driver_rk_mpp::BufferId`].
//!
//! Hardware status: untested on board. Written from the vendor header and the vendor HAL's own
//! register writes; the check is `testing/board-runs/K1b.md`.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod job;
pub mod layout;
pub mod vpu720;

#[cfg(test)]
mod tests;

pub use job::{Buffer, JobError, READ_BACK, RegisterSet, SetError, Source, WRITE_WORDS};
