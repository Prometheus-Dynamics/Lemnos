//! The ISP8000 behind the vendor kernel's `vvcam-isp` device as `camera.isp/1`: [`K230Isp`]
//! writes each job's register list and hands out the statistics registers of the frame that list
//! was in force for. See the `driver` module's documentation (on [`K230Isp`]) for the streams, the
//! job steps, what it does and does not do about frames, and the failure rule.
//!
//! What it stands on, each with no device access of its own:
//!
//! - [`config`]: the configuration layout (a list of register writes) and its check, which refuses
//!   a whole list when any write reaches past the sys crate's table of registers that carry no
//!   address.
//! - [`stats`]: the statistics layout (the statistics registers as read) and a decoder for
//!   callers ([`stats::Raw`]).
//! - [`hw`]: the device as the driver sees it ([`Hardware`]), and the Linux implementation over
//!   `lemnos-driver-k230isp-sys`.
//! - [`regs`]: the register map, from the sys crate.
//!
//! Lemnos does no 3A and holds no tuning: what the registers are set to is the caller's.
//!
//! Untested on board: written from the vendor driver's interface facts; the check is
//! `testing/board-runs/E11.md`.
//!
//! Provenance: ported from the dev branch's `crates/lemnos-drivers-k230isp` (`regs.rs`,
//! `access.rs`, `stats.rs`, `device.rs`) and `crates/lemnos-linux-sys/src/vvcam_isp.rs`. Dev's
//! open-ended `RegisterAccess` trait (any address, any value) is not carried over: every write now
//! goes through the sys crate's table. Dev's `stats::read_raw` is split into the driver's
//! unread copy of the registers and [`stats::Raw::decode`]. The driver itself is new.

#![forbid(unsafe_code)]

pub mod config;
#[cfg(target_os = "linux")]
mod driver;
pub mod hw;
pub mod stats;

#[cfg(all(test, target_os = "linux"))]
mod driver_tests;
#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub use driver::{
    CONFIG_STREAM, FRAMES_TO_SETTLE, JOB_TIMEOUT_NS, K230Isp, K230IspNeeds, MAX_STREAM_BUFFERS,
    STATISTICS_STREAM,
};
#[cfg(target_os = "linux")]
pub use hw::LinuxHardware;
pub use hw::{DEVICE_PATH, Event, Hardware, Settings};

pub use config::{CONFIG_BYTES, LAYOUT_WRITES, MAX_WRITES};
/// The register map, from the sys crate.
pub use lemnos_driver_k230isp_sys::regs;
pub use stats::{LAYOUT_STATISTICS, STATISTICS_BYTES};
