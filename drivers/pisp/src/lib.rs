//! PiSP (the BCM2712 ISP) configuration, with no hardware access.
//!
//! - [`uapi`]: `#[repr(C)]` mirrors of the kernel's config and statistics structures, with
//!   compile-time size and offset checks against the headers, and field-by-field serialisation
//!   ([`uapi::Wire`]) so nothing here needs `unsafe`.
//! - [`fe::FrontEnd`]: builds the front end configuration (input, black level, statistics,
//!   outputs).
//! - [`be::BackEnd`]: builds the back end configuration and its tiles.
//! - [`stats::Statistics`]: decodes the front end statistics buffer.
//!
//! # Refusing bad configurations before submit
//!
//! A bad back end job is not an error the hardware reports and moves past: a job that never
//! finishes stops the back end for every client of it, and only a reboot clears it. So every
//! configuration is checked when it is built, and a refusal (for example 8-bit uncompressed
//! Bayer input) returns an error before anything is submitted. Keeping those refusals is a
//! hard requirement: a change that lets a bad configuration reach the device is a bug.
//!
//! # Tuning
//!
//! Gamma curves and the choice of colour encoding, filter or sharpening strength are the
//! caller's: the crate carries the mechanics that turn them into hardware tables. The hardware's
//! own defaults (the libpisp colour encodings, the debin, demosaic and false-colour settings, the
//! resampler and sharpening filters) are in [`be::defaults`], and a block enabled with a default
//! runs with it. The README lists the few fixed values that are kept and why.
//!
//! # `no_std`, no allocator
//!
//! Without the `device` feature the crate is `no_std` and never allocates. Buffers are fixed arrays; errors are enums
//! of `&'static str`. Lengths and sizes from outside (a statistics buffer, a tile count) are
//! checked and refused, not indexed.
//!
//! # Provenance
//!
//! The uAPI layouts come from the Linux headers (`GPL-2.0-only WITH Linux-syscall-note`), and
//! the finalisation rules and tiling are ported from libpisp (BSD-2-Clause). The README has the
//! details and the licence text.

#![forbid(unsafe_code)]
#![cfg_attr(not(any(test, feature = "device")), no_std)]

pub mod be;
#[cfg(all(feature = "device", target_os = "linux"))]
mod be_device;
#[cfg(all(feature = "device", target_os = "linux"))]
pub mod be_hw;
pub mod be_job;
pub mod fe;
#[cfg(all(feature = "device", target_os = "linux"))]
mod fe_device;
#[cfg(all(feature = "device", target_os = "linux"))]
pub mod fe_hw;
pub mod fe_job;
pub mod format;
mod math;
pub mod stats;
pub mod uapi;

#[cfg(all(feature = "device", target_os = "linux"))]
pub use be_device::{
    CONFIG_STREAM as BE_CONFIG_STREAM, INPUT_ALIGN as BE_INPUT_ALIGN,
    INPUT_STREAM as BE_INPUT_STREAM, OUTPUT_STREAMS as BE_OUTPUT_STREAMS, Options as BeOptions,
    PispBe, PispBeNeeds,
};
#[cfg(all(feature = "device", target_os = "linux"))]
pub use fe_device::{
    CONFIG_STREAM as FE_CONFIG_STREAM, Link as FeLink, OUTPUT_STREAMS as FE_OUTPUT_STREAMS,
    Options as FeOptions, PispFe, PispFeNeeds, STATISTICS_STREAM as FE_STATISTICS_STREAM,
};
