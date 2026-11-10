//! The `k230isp` driver's raw layer: the vendor kernel's `vvcam-isp` misc device (the ISP8000's
//! register window and its interrupt), and nothing else.
//!
//! - [`regs`]: the register map (window size, block bases, offsets, fields).
//! - [`access`]: which registers may be written ([`access::WRITABLE`]) and read, and the
//!   statistics registers in the order the driver hands them out ([`access::STATISTICS`]).
//! - [`uapi`]: the device's request numbers and structures, with their sources.
//! - `node` (Linux): [`IspNode`], whose register reads and writes are checked against
//!   [`access`] before they reach the device, and the ISP's interrupt as events.
//!
//! # Why the checks are here
//!
//! The vendor driver writes `base + addr` for any 32-bit `addr` it is given: a write request is
//! a store anywhere in kernel address space past the window, and inside the window the memory
//! interface and front end take bus addresses and start DMA. [`IspNode::write`] is a safe
//! function, so it refuses, before any request, every register outside [`access::WRITABLE`] and
//! every bit outside a register's mask. No register that takes an address, a length or a DMA
//! control is in the table; [`IspNode::read`] reads only the writable and statistics registers.
//! `VVCAM_ISP_FE_START`, `VVCAM_ISP_CLOSE_IRQ`, `VVCAM_ISP_RESET` and `mmap` are not offered (see
//! [`uapi`]).
//!
//! Unsafe code: one `ioctl` call in `node`, over this crate's own integer-only structures.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/E11.md`.

#![cfg_attr(not(target_os = "linux"), no_std)]

pub mod access;
#[cfg(target_os = "linux")]
mod node;
pub mod regs;
pub mod uapi;

#[cfg(target_os = "linux")]
pub use node::IspNode;

#[cfg(test)]
mod tests;
