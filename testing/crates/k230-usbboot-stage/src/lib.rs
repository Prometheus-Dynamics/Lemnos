//! The K230 USB-boot stage: the code the boot ROM starts over USB, as a state machine the board
//! runs and the host's simulator runs too.
//!
//! The pieces: the wire format both ends share ([`format`]), the watchdog's registers, its model
//! and the feed rule ([`wdt`]), the medium the stage writes ([`medium`], with a RAM medium in
//! [`ram`]), and the stage itself ([`stage`]). Nothing here touches hardware: the `-sys` crate
//! supplies the registers and the entry, and the board's medium and USB device driver are not in
//! this repository yet (see `testing/board-runs/K230-USB.md`).
//!
//! `no_std`, and no `unsafe`: the `-sys` crate holds all of it.

#![cfg_attr(not(test), no_std)]

pub mod format;
pub mod image;
pub mod medium;
pub mod ram;
pub mod sd;
pub mod stage;
pub mod pads;
pub mod probe;
pub mod report;
pub mod sysctl;
pub mod telemetry;
pub mod uart;
pub mod usb_dev;
pub mod wdt;
