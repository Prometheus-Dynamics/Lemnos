//! The simulated chain's clock. It moves only when the simulator makes a sensor start, a job
//! starts a frame or a back-end job completes: never with the host's clock, so host scheduling
//! cannot change frame starts, completions, stamps or drops. It is written to a clock file when
//! one is installed, for the chain proof in another process to read (test only).
//!
//! These functions act on the process's own chain. An instance has its own clock
//! (`Sim::advance_to`, [`crate::chain::Clock`]).
//!
//! There is one sensor schedule: the starts every consumer of the sensor shares. [`peek_start`]
//! is the next start, [`take_start`] takes it.

use std::io;
use std::path::Path;

use crate::chain::LEGACY;

/// The clock's start: ten seconds in, so a frame stamped a little earlier than its start is still
/// a time above zero (zero is how a placeholder frame is told).
pub const ORIGIN_NS: u64 = crate::chain::ORIGIN_NS;

/// The simulated time now, in nanoseconds.
#[must_use]
pub fn now() -> u64 {
    LEGACY.clock.now()
}

/// Writes the clock to a file at `path` from now on, starting at the current time.
///
/// # Errors
/// The file cannot be created.
pub fn install_file(path: &Path) -> io::Result<()> {
    LEGACY.clock.install_file(path)
}

/// Moves the clock on to `ns`. A time not later than now changes nothing.
pub fn advance_to(ns: u64) {
    LEGACY.clock.advance_to(ns);
}

/// The next sensor start, `period` ns after the clock if the schedule has none yet.
#[must_use]
pub fn peek_start(period: u64) -> u64 {
    LEGACY.clock.peek_start(period)
}

/// Takes the next sensor start: the schedule moves on by `period`.
#[must_use]
pub fn take_start(period: u64) -> u64 {
    LEGACY.clock.take_start(period)
}

/// Forgets the schedule: the next start is a period after the clock. A new run starts this way.
pub fn reset_schedule() {
    LEGACY.clock.reset_schedule();
}
