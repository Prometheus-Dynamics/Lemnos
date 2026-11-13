//! The `system.security-precondition/1` contract: what a driver with a stake in the board's
//! security switch says about a change to it. The switch's device asks each stakeholder it is
//! bound to (`uses`); the stakeholders answer for themselves only.
//!
//! The reply is text, one fact per line, and empty (or `ok`) when nothing stops the change:
//!
//! - `missing: <what, and how to fix it>`: the change to on cannot be made yet.
//! - `busy: <what is running>`: a change is refused while this work runs, in either direction.

#![no_std]

use lemnos_core::{Result, contract};

/// The most one answer may say: the caller's buffer for `precondition` must be this large.
pub const TEXT_MAX: usize = 1024;

#[contract("system.security-precondition", version = 1, kind = "system")]
pub trait SecurityPrecondition {
    /// What stops the switch changing to `on` (`true`) or to off (`false`), one line each.
    #[method(1)]
    fn precondition(&mut self, on: bool, #[out] into: &mut [u8]) -> Result<usize>;

    /// Makes this stakeholder's own boot-side mark match `on`, durable when it returns (the slots'
    /// marker, which the boot side reads). The switch calls it as part of a change: before its
    /// record for a change to on, after it for a change to off; and at each daemon start, from the
    /// record. Appended after `precondition` (method 1), which is unchanged.
    #[method(2)]
    fn apply(&mut self, on: bool) -> Result<()>;
}
