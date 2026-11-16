//! The rules every on-chip flash stage shares, for a chip's stage to build on: the entry contract (arm
//! the watchdog first, read the arming back, lower the period, report the cause before any reset), the
//! bounded waits, the feed rule (only after a verified round), the serve loop over a [`link::Link`], and
//! the two order rules, which the types enforce: a chunk is written only once verified ([`region`]), and
//! a boot-area or header write only with the compared mark.
//!
//! The wire is `lemnos-flash-wire`. Nothing here names a chip, and nothing is `unsafe`. The stage of a
//! chip implements [`guard::Guard`], [`link::Link`] and [`link::Storage`] over its own registers.
//!
//! `README.md` has the rules, and what a chip's stage must still show on its own.

#![cfg_attr(not(any(test, feature = "sim")), no_std)]

pub mod chunk;
pub mod guard;
pub mod link;
pub mod region;
pub mod serve;
#[cfg(any(test, feature = "sim"))]
pub mod sim;
pub mod wait;
