//! Rockchip ISP21 (RK3566 / RK3568), ISP30 (RK3588 / RK3588S), ISP32 (RV1106 / RV1103, RK3562)
//! and ISP35 (RV1126B) image signal processors from Rust: the vendor kernel's `rkisp` driver's parameter and
//! statistics buffers, which SoC has which ISP, and, with the `device` feature, the driver's
//! video nodes.
//!
//! - [`uapi`]: the buffers' layouts, re-exported from the `-sys` crate `lemnos-rkisp-sys` (generated
//!   from the kernel's `rk-isp21-config.h`, `rk-isp3-config.h` and `rk-isp32-config.h`, checked
//!   against the C compiler's sizes and offsets).
//! - [`Isp`]: the generation a SoC has, from its devicetree compatible strings
//!   ([`Isp::detect`] reads this machine's).
//! - [`ParamBuffer`]: a parameter buffer the device takes as it is. The camera stack's
//!   builders implement it.
//! - [`isp32`]: the ISP32 variant ([`isp32::Variant`]) and the video node names each variant
//!   exposes.
//! - [`isp35`]: the video node names the ISP35 exposes.
//! - `cif` (feature `device`, Linux): the vendor `rkcif` capture block: its media devices per
//!   chip, and the call that routes a MIPI channel to its capture node.
//! - `device` (feature `device`, Linux): the vendor driver's ISP, parameter, statistics and
//!   main path nodes through `lemnos-v4l2`, for every generation.
//!
//! Status: written, untested on board. None of these ISPs has run on its chip yet; every item
//! that touches a device says so in its documentation.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(feature = "device")]
pub mod cif;
#[cfg(feature = "device")]
pub mod device;
mod isp;
pub mod isp32;
pub mod isp35;

pub use isp::{Isp, ParamBuffer};
/// The uAPI layouts: the `-sys` crate `lemnos-rkisp-sys` (the crate's unsafe code lives there).
pub use lemnos_rkisp_sys as uapi;

#[cfg(test)]
mod tests;
