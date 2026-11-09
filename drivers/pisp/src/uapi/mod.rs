//! `#[repr(C)]` mirrors of the PiSP uAPI structures.
//!
//! Provenance: transcribed from the Linux uAPI headers
//! `include/uapi/linux/media/raspberrypi/pisp_be_config.h`, `pisp_common.h` and the `rp1-cfe`
//! driver's `pisp_fe_config.h`, `pisp_statistics.h`, `pisp_types.h`
//! (`GPL-2.0-only WITH Linux-syscall-note`: a userspace interface definition; only layouts,
//! names and constants are mirrored here, no kernel source). Field names follow the headers;
//! type names are Rust-style (`struct pisp_fe_config` is [`FeConfig`]).

mod be;
mod be_config;
mod common;
mod fe;
mod layout;
mod stats;
mod wire;

pub use be::*;
pub use be_config::*;
pub use common::*;
pub use fe::*;
pub use stats::*;
pub use wire::{BufferTooSmall, Reader, Wire, Writer};
