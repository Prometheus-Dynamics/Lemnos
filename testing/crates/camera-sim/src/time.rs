//! The simulated chain's clock. It moves only when the simulator makes a sensor start, a job
//! starts a frame or a back-end job completes: never with the host's clock, so host scheduling
//! cannot change frame starts, completions, stamps or drops. It is written to a clock file when
//! one is installed, for the chain proof in another process to read (test only).
//!
//! There is one sensor schedule: the starts every consumer of the sensor shares. [`peek_start`]
//! is the next start, [`take_start`] takes it.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use lemnos_sim_clock::ClockWriter;

/// The clock's start: ten seconds in, so a frame stamped a little earlier than its start is still
/// a time above zero (zero is how a placeholder frame is told).
pub const ORIGIN_NS: u64 = 10_000_000_000;

static NOW: AtomicU64 = AtomicU64::new(ORIGIN_NS);
static FILE: Mutex<Option<ClockWriter>> = Mutex::new(None);
/// The next sensor start, once one has been set from the clock.
static SCHEDULE: Mutex<Option<u64>> = Mutex::new(None);

fn file() -> MutexGuard<'static, Option<ClockWriter>> {
    FILE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn schedule() -> MutexGuard<'static, Option<u64>> {
    SCHEDULE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The simulated time now, in nanoseconds.
#[must_use]
pub fn now() -> u64 {
    NOW.load(Ordering::SeqCst)
}

/// Writes the clock to a file at `path` from now on, starting at the current time.
///
/// # Errors
/// The file cannot be created.
pub fn install_file(path: &Path) -> io::Result<()> {
    let mut guard = file();
    *guard = Some(ClockWriter::create(path, now())?);
    Ok(())
}

/// Moves the clock on to `ns`. A time not later than now changes nothing.
pub fn advance_to(ns: u64) {
    let mut file = file();
    if ns <= NOW.load(Ordering::SeqCst) {
        return;
    }
    NOW.store(ns, Ordering::SeqCst);
    if let Some(writer) = file.as_mut() {
        writer.set(ns).expect("sim clock file");
    }
}

/// The next sensor start, `period` ns after the clock if the schedule has none yet.
#[must_use]
pub fn peek_start(period: u64) -> u64 {
    let mut next = schedule();
    *next.get_or_insert_with(|| now() + period)
}

/// Takes the next sensor start: the schedule moves on by `period`.
#[must_use]
pub fn take_start(period: u64) -> u64 {
    let mut next = schedule();
    let at = *next.get_or_insert_with(|| now() + period);
    *next = Some(at + period);
    at
}

/// Forgets the schedule: the next start is a period after the clock. A new run starts this way.
pub fn reset_schedule() {
    *schedule() = None;
}
