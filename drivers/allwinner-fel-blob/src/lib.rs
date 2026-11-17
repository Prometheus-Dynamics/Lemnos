//! The Allwinner FEL thunk blob, as a PC side writes it to SRAM before FEL_RUN.
//!
//! The code is assembled by `lemnos-driver-allwinner-fel-sys` (naked assembly, one function per target). Its
//! built bytes are pinned here ([`pinned`]), and `scripts/check-blob.sh` checks that the build still produces
//! them and that the symbols sit at the offsets in [`layout`]. A PC side takes the pinned bytes, the layout
//! and the version from this crate, and writes the run's fields with [`blob::write`]. Nothing here is hardware:
//! the SoC's watchdog address and values, the SRAM addresses and the SPL entry are the caller's data.
//!
//! No dependencies, no `unsafe`, no_std. See `README.md` for the layout and the pins.

#![cfg_attr(not(test), no_std)]

pub mod blob;
pub mod hash;
pub mod layout;
pub mod pinned;

pub use blob::{BlobError, Fields, Swap, Watchdog, write};
pub use layout::{AARCH64, ARMV7, Layout};
