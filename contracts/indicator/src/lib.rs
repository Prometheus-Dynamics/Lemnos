//! The `indicator/1` contract: a device that shows named states, such as "receiving" or "failed",
//! on LEDs. A caller sets the state; what each state looks like is the board's, not Lemnos's.
//!
//! The states a device accepts are the list [`STATES`], and the `states` method returns the same
//! list as text, comma-separated, so a caller can ask a device which states it knows. A state
//! name the device does not list is refused `INVALID_ARGUMENT`.
//!
//! Locate. `locate` makes the device show its locate look for a number of seconds, over whatever
//! else it shows, so a person can find the board among others; `locating` says whether it is
//! showing now. The look is the board's. It ends at its time, on `locate(0)`, or when the caller
//! that asked goes; what was under it is shown again at once.
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

/// The longest a `locate` may be asked for, in seconds: ten minutes.
pub const MAX_LOCATE_SECONDS: u16 = 600;

#[contract("indicator", version = 1)]
pub trait Indicator {
    /// Shows `state`, with its progress per mille or [`NO_PROGRESS`]. A state the device does not
    /// list, or a progress above [`FULL_PROGRESS`] that isn't [`NO_PROGRESS`], is refused.
    #[method(1)]
    fn set(&mut self, state: &str, progress: u16) -> Result<()>;
    /// The states this device accepts, as text: their names, comma-separated, in [`STATES`]'s order.
    /// Returns its length; `BUFFER_TOO_SMALL` when it doesn't fit.
    #[method(2)]
    fn states(&mut self, #[out(text)] into: &mut [u8]) -> Result<usize>;
    /// Shows the locate look for `seconds`, over anything else the device shows; 0 ends it now.
    /// A new call replaces the one before it. More than [`MAX_LOCATE_SECONDS`] is refused
    /// `INVALID_ARGUMENT`, and `BUSY` while the device runs a test that needs its LEDs.
    #[added]
    #[method(3)]
    fn locate(&mut self, seconds: u16) -> Result<()>;
    /// Whether the locate look is showing now.
    #[added]
    #[method(4, reading = switch)]
    fn locating(&mut self) -> Result<bool>;
}
