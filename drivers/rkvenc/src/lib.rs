//! Rockchip video encoder blocks: register layouts and job builders, with no vendor library.
//!
//! - [`layout`]: the layout types (register, field, address register) that every module uses.
//! - [`vepu541_h264`]: the VEPU541 H.264 encoder's write side ([`vepu541_h264::set`]), its
//!   read-back side ([`vepu541_h264::ret`]) and its second window ([`vepu541_h264::l2`]), and
//!   [`vepu541_h264::RegisterSet`], which builds a job for `lemnos-driver-rk-mpp`'s `Job`.
//! - [`vepu580_h264`] and [`vepu580_h265`]: the VEPU580 H.264 and H.265 encoders' register layouts,
//!   and the job builder for each ([`vepu580_h264::RegisterSet`], [`vepu580_h265::RegisterSet`]).
//!
//! The caller supplies every value: rate control, slice and picture parameters, and the SPS and
//! PPS it writes itself. This crate knows where each value goes and which values the hardware
//! takes. It has no codec policy and writes no bitstream.
//!
//! Register facts are transcribed from open-source vendor user-space code with provenance (source
//! file and version); the libraries themselves are neither used nor linked. See each module.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod layout;
pub mod vepu541_h264;
pub mod vepu541_h265;
pub mod vepu580_h264;
pub mod vepu580_h265;

#[cfg(test)]
mod tests;
