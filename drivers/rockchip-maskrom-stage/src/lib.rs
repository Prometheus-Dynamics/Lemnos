//! The Rockchip maskrom flasher's on-chip stages: the 471 stub's register sequence, the 472 loader's
//! session on the DWC3 bulk link and the SD card, and the self-relocation of the 472 image. The rules
//! every stage shares (the watchdog-first start, the bounded waits, the feed rule, the serve loop and
//! the order rules) are in `lemnos-flash-stage`; the frames are in `lemnos-flash-wire`.
//!
//! This crate is no_std and holds no `unsafe`: the register and cache primitives, and the stage's
//! assembly, are in `lemnos-driver-rockchip-maskrom-sys`, which calls into here. Its tests run the
//! session against models of the controllers (`models`, test only), with the same frames a PC side sends.
//!
//! The stub is assembly (`drivers/rockchip-maskrom-sys/src/stage471.S`), built for the board's target
//! through the -sys crate. It is not linked here: the bench builder links it in front of the DDR blob,
//! which is not in this repository.

#![cfg_attr(not(test), no_std)]

pub mod bind;
pub mod dwc3link;
pub mod entry472;
pub mod io;
#[cfg(test)]
mod models;
pub mod regs;
pub mod reloc;
pub mod sd;
pub mod wdt;

// The stub is assembled by the -sys crate, which holds the unsafe code; this crate forbids it.
// The test in `wdt.rs` checks the stub source's register sequence.
