//! Canaan K230D's VeriSilicon ISP8000 (the MRV-derived ISP behind the vendor kernel's
//! `vvcam-isp` misc device, `/dev/vvcam-isp.0`) from Rust: its register map, the register
//! access its parameter builder writes through ([`RegisterAccess`]), the statistics registers
//! read into [`stats::Raw`], and, with the `device` feature, the device's register ioctls on
//! Linux.
//!
//! - [`regs`]: the register map: block bases, register offsets and the field positions, and the
//!   sources they come from (the MIT headers of the vendor's VeriSilicon ISP).
//! - [`RegisterAccess`]: a register read and write. The statistics reads and the parameter
//!   writes both go through it, and the device implements it.
//! - [`stats`]: the statistics registers as [`stats::Raw`] ([`stats::read_raw`]).
//! - `device` (feature `device`): the ioctls of `/dev/vvcam-isp.0`, and the
//!   [`RegisterAccess`] over them. Linux only.
//!
//! Status: written, untested on board. The register layouts agree with each other across the
//! struct and the field comments, except where the crate says otherwise. The K230's own blocks
//! (the front end at `0x3D60`, the FUSA blocks) have no field layouts in the sources, so they
//! are left out. The unconfirmed constants are named where they are defined.

#![cfg_attr(not(feature = "std"), no_std)]

mod access;
#[cfg(feature = "device")]
pub mod device;
pub mod regs;
pub mod stats;

pub use access::RegisterAccess;
