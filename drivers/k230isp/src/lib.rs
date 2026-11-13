//! The K230's ISP8000 as `camera.isp/1`, two ways:
//!
//! - [`K230Isp`] (`k230isp`), behind the vendor kernel's `vvcam-isp` device or through the
//!   configuration role of the block as `lemnos.ko` drives it: writes each job's register list
//!   and hands out the statistics registers of the frame that list was in force for. No frames:
//!   on the module's way `k230-capture` runs them on the same block at the same time. See the
//!   `driver` module's documentation (on [`K230Isp`]) for the streams, the job steps and the
//!   failure rule.
//! - [`K230IspFrames`] (`k230isp-frames`), with the ISP bound to `lemnos.ko`: the same
//!   configuration and statistics, and the main path's frames into the caller's dma-bufs, at
//!   addresses and sizes the module writes from buffers it holds; it holds both of the block's
//!   roles. See [`frames`].
//!
//! What it stands on, each with no device access of its own. All but [`hw`] are the library
//! half: built with no features (no `device`), they need no driver, engine or daemon.
//!
//! - [`config`]: the configuration layout (a list of register writes) and its check, which refuses
//!   a whole list when any write reaches past the sys crate's table of registers that carry no
//!   address.
//! - [`stats`]: the statistics layout (the statistics registers as read) and a decoder for
//!   callers ([`stats::Raw`]).
//! - [`hw`]: the device as the driver sees it ([`Hardware`]), and the Linux implementations over
//!   `lemnos-driver-k230isp-sys` and over the module's configuration session.
//! - [`regs`]: the register map, from the sys crate.
//! - `lemnos-driver-regjob-sys`: the module's register-job requests, which [`LinuxModule`] and
//!   [`LinuxConfigSession`] make.
//!
//! Lemnos does no 3A and holds no tuning: what the registers are set to is the caller's.
//!
//! Untested on board: written from the vendor driver's interface facts and the published register
//! headers; the checks are `testing/board-runs/E11.md` and `testing/board-runs/H8b.md`.
//!
//! Provenance: ported from the dev branch's `crates/lemnos-drivers-k230isp` (`regs.rs`,
//! `access.rs`, `stats.rs`, `device.rs`) and `crates/lemnos-linux-sys/src/vvcam_isp.rs`. Dev's
//! open-ended `RegisterAccess` trait (any address, any value) is not carried over: every write now
//! goes through the sys crate's table. Dev's `stats::read_raw` is split into the driver's
//! unread copy of the registers and [`stats::Raw::decode`]. The driver itself is new.

#![forbid(unsafe_code)]

pub mod config;
#[cfg(all(feature = "device", target_os = "linux"))]
mod driver;
#[cfg(all(feature = "device", target_os = "linux"))]
pub mod frames;
#[cfg(feature = "device")]
pub mod hw;
pub mod stats;

#[cfg(all(test, feature = "device", target_os = "linux"))]
mod driver_tests;
#[cfg(all(test, feature = "device", target_os = "linux"))]
mod frames_tests;
#[cfg(all(test, feature = "device", target_os = "linux"))]
mod module_tests;
#[cfg(test)]
mod tests;

#[cfg(all(feature = "device", target_os = "linux"))]
pub use driver::{
    CONFIG_STREAM, FRAMES_TO_SETTLE, JOB_TIMEOUT_NS, K230Isp, K230IspNeeds, MAX_STREAM_BUFFERS,
    STATISTICS_STREAM,
};
#[cfg(all(feature = "device", target_os = "linux"))]
pub use frames::{K230IspFrames, K230IspFramesNeeds, LinuxModule, Module};
#[cfg(all(feature = "device", target_os = "linux"))]
pub use hw::{ConfigSession, LinuxConfigSession, LinuxHardware, ModuleHardware};
#[cfg(feature = "device")]
pub use hw::{DEVICE_PATH, Event, Hardware, Settings};

pub use config::{CONFIG_BYTES, LAYOUT_WRITES, MAX_WRITES};
/// The register map, from the sys crate.
pub use lemnos_driver_k230isp_sys::regs;
pub use stats::{LAYOUT_STATISTICS, STATISTICS_BYTES};
