//! The rules every on-chip flash stage shares, for a chip's stage to build on.
//!
//! - The entry contract ([`guard`]): arm the watchdog first, read the arming back, lower the period, and report
//!   the cause before any reset.
//! - The bounded waits ([`wait`]), and the feed rule ([`chunk`]): the watchdog is fed once per verified round.
//! - The serve loop ([`serve`]) over a [`link::Link`], with the order rules ([`region`]): a write needs a
//!   verified payload, and a boot area or header needs the compared mark.
//! - The device side ([`usb`]): the control endpoint's standard requests, the descriptors, enumeration, and the
//!   bulk pair as a link. A chip implements [`usb::Controller`] over its controller and nothing else.
//! - The simulated device, the frame-level [`sim::Transport`] and the conformance list ([`sim`], feature `sim`).
//!
//! The wire is `lemnos-flash-wire`. Nothing here names a chip, and nothing is `unsafe`.
//! `README.md` has the rules and what a chip's stage must still show on its own.

#![cfg_attr(not(any(test, feature = "sim")), no_std)]

pub mod chunk;
pub mod guard;
pub mod link;
pub mod region;
pub mod serve;
pub mod session;
#[cfg(any(test, feature = "sim"))]
pub mod sim;
pub mod usb;
pub mod wait;
