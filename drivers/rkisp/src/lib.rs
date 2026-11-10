//! The vendor kernel's `rkisp` ISP, in three generations: ISP21, ISP30 and ISP35. What a
//! `camera.isp/1` driver for it stands on, with no device access of its own:
//!
//! - [`uapi`]: the parameter and statistics layouts, from the `-sys` crate (bindgen's, checked
//!   against the C compiler). Lemnos never reads them: the caller fills a parameter buffer and
//!   reads a statistics buffer, and does its own 3A and tuning.
//! - [`Generation`]: which ISP a chip has, and the layouts `camera.isp/1` names for it
//!   ([`Generation::config_layout`], [`Generation::statistics_layout`]).
//! - [`graph`]: the ISP path in its media graph (what feeds it, which links to turn on, which
//!   nodes take parameters, give statistics and give images), as pure functions over a graph the
//!   caller read.
//! - [`job`]: the checks a caller's [`IspJob`](lemnos_contract_camera_isp::IspJob) passes before
//!   any of its numbers reach a queue, and the bounded copies of parameters in and statistics
//!   out. They allocate nothing.
//!
//! Not here yet: the `camera.isp/1` driver itself. A job's parameters are bytes the caller puts
//! in a buffer of the config stream, and a driver with no kernel module behind it has no way to
//! read a stream buffer: the stream endpoint offers `write` (a driver filling a frame) but no
//! read. Every other piece the driver needs is here and tested; the driver lands when the endpoint
//! can read.
//!
//! Untested on board: written from the vendor driver's interface facts; the check is
//! `testing/board-runs/E9.md`.
//!
//! Provenance: ported from the dev branch's `crates/lemnos-drivers-rkisp` (`isp.rs`, `isp35.rs`,
//! `device.rs`, `tests.rs`): the SoC table, the node names and the ISP path dev's `IspDevice`
//! linked. Dev's ISP32 support, its `ParamBuffer` trait and its blocking device layer are not
//! ported.

#![forbid(unsafe_code)]

mod generation;
#[cfg(target_os = "linux")]
pub mod graph;
pub mod job;

#[cfg(test)]
mod tests;

pub use generation::{
    Generation, LAYOUT_ISP21_PARAMS, LAYOUT_ISP21_STATISTICS, LAYOUT_ISP30_PARAMS,
    LAYOUT_ISP30_STATISTICS, LAYOUT_ISP35_PARAMS, LAYOUT_ISP35_STATISTICS, OUTPUTS,
};
/// The parameter and statistics layouts: the `-sys` crate, where the generated code lives.
pub use lemnos_driver_rkisp_sys as uapi;
