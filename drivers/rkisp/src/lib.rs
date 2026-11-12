//! The vendor kernel's `rkisp` ISP, in three generations (ISP21, ISP30 and ISP35), as
//! `camera.isp/1`: [`Rkisp`] runs the vendor driver's stock nodes through `lemnos-v4l2`, carrying
//! each job's parameters in and its statistics and images out. See the `driver` module's
//! documentation (on [`Rkisp`]) for the streams, the job steps, the settings and the failure rule.
//!
//! What it stands on, each with no device access of its own. All but [`job`] and [`hw`] are the
//! library half: built with no features (no `device`), they need no driver, engine or daemon.
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
//!   and images out. They allocate nothing.
//! - [`hw`]: the kernel's nodes as the driver sees them ([`Hardware`]), and the Linux
//!   implementation over the media device, subdevices and video nodes.
//!
//! Untested on board: written from the vendor driver's interface facts; the check is
//! `testing/board-runs/E9.md`.
//!
//! Provenance: ported from the dev branch's `crates/lemnos-drivers-rkisp` (`isp.rs`, `isp35.rs`,
//! `device.rs`, `tests.rs`): the SoC table, the node names, the ISP path and the formats dev's
//! `IspDevice` set. Dev's ISP32 support, its `ParamBuffer` trait and its blocking device layer
//! are not ported; the driver here is new, on the frame stream.

#![forbid(unsafe_code)]

#[cfg(all(feature = "device", target_os = "linux"))]
mod driver;
mod generation;
#[cfg(target_os = "linux")]
pub mod graph;
#[cfg(all(feature = "device", target_os = "linux"))]
pub mod hw;
#[cfg(feature = "device")]
pub mod job;

#[cfg(all(test, feature = "device", target_os = "linux"))]
mod driver_tests;
#[cfg(all(test, feature = "device"))]
mod job_tests;
#[cfg(test)]
mod tests;

#[cfg(all(feature = "device", target_os = "linux"))]
pub use driver::{
    CONFIG_STREAM, MAX_DIMENSION, MAX_PENDING, MAX_STREAM_BUFFERS, OUTPUT_STREAMS, Options, Rkisp,
    RkispNeeds, STATISTICS_STREAM,
};
#[cfg(all(feature = "device", target_os = "linux"))]
pub use hw::{Hardware, LinuxHardware, Settings};

pub use generation::{
    BufferLayout, Generation, LAYOUT_ISP21_PARAMS, LAYOUT_ISP21_STATISTICS, LAYOUT_ISP30_PARAMS,
    LAYOUT_ISP30_STATISTICS, LAYOUT_ISP35_PARAMS, LAYOUT_ISP35_STATISTICS, OUTPUTS,
};
/// The parameter and statistics layouts: the `-sys` crate, where the generated code lives.
pub use lemnos_driver_rkisp_sys as uapi;
