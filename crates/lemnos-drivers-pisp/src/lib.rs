//! The Raspberry Pi 5 ISP ("PiSP") from Rust, with no C libraries at run time.
//!
//! - [`uapi`]: `#[repr(C)]` mirrors of the kernel's PiSP config and statistics structures,
//!   with compile-time size/offset checks against the headers the device runs.
//! - [`fe::FrontEnd`]: builds the front end (`pisp-fe`) configuration: input, black level,
//!   statistics (AWB zones, AGC zones/histogram, CDAF, floating regions), outputs.
//! - [`stats::Statistics`]: decodes the front end statistics buffer.
//! - [`be::BackEnd`]: builds the back end (`pispbe`) configuration (Bayer pipeline, demosaic,
//!   CCM, gamma, YCbCr, resampling, output formats) and its tiles.
//! - [`device`] (feature `device`): runs the front end and back end nodes through
//!   `lemnos-v4l2`, with dma-heap buffers from `lemnos-linux`.
//!
//! # Refusing bad configurations before submit
//!
//! A bad back end job is not an error the hardware reports and moves past: a job that never
//! finishes stops the back end for every client of it, and only a reboot clears it. So every
//! configuration is checked when it is built, and a refusal (for example 8-bit uncompressed
//! Bayer input) returns an error before anything is submitted. Keeping those refusals is a
//! hard requirement: a change that lets a bad configuration reach the device is a bug, whatever
//! the speed gained.
//!
//! # Provenance and licences
//!
//! Layouts and constants come from the Linux uAPI headers (`GPL-2.0-only WITH
//! Linux-syscall-note`). The finalisation rules of the front end, the back end block defaults
//! (gamma curve, YCbCr matrices, resampling filters, sharpening, demosaic), the stride/offset
//! helpers and the back end tiling algorithm are ported from libpisp 1.3.0
//! (<https://github.com/raspberrypi/libpisp>), which is BSD-2-Clause:
//!
//! ```text
//! Copyright (c) 2023, Raspberry Pi Ltd All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without modification,
//! are permitted provided that the following conditions are met:
//!
//! 1. Redistributions of source code must retain the above copyright notice,
//! this list of conditions and the following disclaimer.
//!
//! 2. Redistributions in binary form must reproduce the above copyright notice,
//! this list of conditions and the following disclaimer in the documentation
//! and/or other materials provided with the distribution.
//!
//! THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
//! AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
//! IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
//! ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
//! LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
//! DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
//! SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
//! CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
//! OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
//! OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//! ```
//!
//! Each ported file names its source. The files derived from libpisp carry the notice above;
//! `be::lsc` is derived from the Raspberry Pi IPA in libcamera (BSD-2-Clause, its own
//! copyright line in that file). The README has the full licence texts.
//!
//! # `no_std`
//!
//! Without the default `std` feature the crate is `no_std` + `alloc`: everything but
//! [`device`]. The README says how it is built and checked.

#![forbid(unsafe_code)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod be;
#[cfg(feature = "device")]
pub mod device;
pub mod fe;
pub mod format;
#[cfg(not(feature = "std"))]
mod math;
pub mod stats;
pub mod uapi;
