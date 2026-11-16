//! The flash link's wire, shared by the PC-side flasher and the on-chip stages it loads.
//!
//! The request and reply frames and their CRC ([`frame`]), the reset kinds ([`frame::ResetKind`]), the
//! identity record a stage answers with ([`identity`]), the regions the stage polices ([`region`]), the
//! timing contract ([`timing`]) and the USB identity the device presents ([`usb`]). Every layout is a
//! constant in this crate, and the layout hash ([`layout`]) is folded from those constants at compile
//! time. Compatibility is structural: a PC side pins [`WIRE_HASH`], and a stage states its own in its
//! identity record. A skew is a typed refusal ([`identity::Skew`]), never a version number to compare.
//!
//! The rules and the layout are in `README.md`. No dependencies, no `unsafe`, no_std.

#![cfg_attr(not(test), no_std)]

pub mod frame;
pub mod hash;
pub mod identity;
pub mod layout;
pub mod region;
pub mod timing;
pub mod usb;

pub use layout::{WIRE_HASH, WIRE_SHORT};
