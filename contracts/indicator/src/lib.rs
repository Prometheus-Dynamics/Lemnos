//! The `indicator/1` contract: a device that shows named states, such as "receiving" or "failed",
//! on LEDs. A caller sets the state; what each state looks like is the board's, not Lemnos's.
//!
//! The states a device accepts are the list [`STATES`], and the `states` method returns the same
//! list as text, comma-separated, so a caller can ask a device which states it knows. A state
//! name the device does not list is refused `INVALID_ARGUMENT`.
//!
//! Progress is a fraction of the job the state belongs to, in per mille (0 to 1000), or
//! [`NO_PROGRESS`] when the state has none.

#![no_std]

use lemnos_core::{Result, contract};

/// The states an indicator accepts, in the order `states` lists them.
pub const STATES: [&str; 6] = [
    "idle",
    "receiving",
    "writing",
    "verifying",
    "done",
    "failed",
];

/// Passed as the progress when a state has none.
pub const NO_PROGRESS: u16 = 0xFFFF;

/// The largest progress a state takes: 1000 per mille, the whole of its job.
pub const FULL_PROGRESS: u16 = 1000;

#[contract("indicator", version = 1)]
pub trait Indicator {
    /// Shows `state`, with its progress per mille or [`NO_PROGRESS`]. A state the device does not
    /// list, or a progress above [`FULL_PROGRESS`] that isn't [`NO_PROGRESS`], is refused.
    #[method(1)]
    fn set(&mut self, state: &str, progress: u16) -> Result<()>;
    /// The states this device accepts, as text: their names, comma-separated, in [`STATES`]'s order.
    /// Returns its length; `BUFFER_TOO_SMALL` when it doesn't fit.
    #[method(2)]
    fn states(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
}
