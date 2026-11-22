//! The data partition's grow, as the daemon reports it.
//!
//! The grow runs in the boot, not here. slot-init runs it before it switches into the slot, when the
//! data partition is mounted writable and the boot disk's node is still root's (see
//! `lemnos_slot_store::grow::run_boot`). It leaves its state in `board/slots/grow-state` on the data
//! partition. This driver only reads that file and reports it: it never opens the disk, and it never
//! writes the data partition (its `board/slots` dir is the one the unit grants). The states are
//! `running` (with its start, its bound and its target), `not-needed`, `grown`, `refused` (named, with
//! the reason), `failed` (every try in that boot failed; the next boot tries again) and `unconfirmed`.
//!
//! A boot that wrote no state yet (an image without the boot grow) is `pending`, and the driver looks
//! again on a slow tick.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use lemnos_slot_store::grow::{Confirm, Disk, Pass, Real, Unconfirmed};
use lemnos_slot_store::grow::{STALLED, STATE_FILE, State, decode_state};
use lemnos_slot_store::mbr::SECTOR;

/// Whether the grow runs, from the board file's `grow_data`: on unless the board says `false`. The
/// boot grows only a disk that validates as the Lemnos layout, so the default is safe. A board that
/// opts out of the grow here still has its boot grow until slot-init learns the setting.
pub fn enabled(board: Option<bool>) -> bool {
    board.unwrap_or(true)
}

/// How long a state file that is not there yet waits before it is read again.
const PENDING_EVERY: Duration = Duration::from_secs(1);
/// How often a grow in flight is read again.
const RUNNING_EVERY: Duration = Duration::from_millis(500);

/// The last failure, so the same one is noted once.
#[derive(Debug, Default)]
pub struct Retry {
    /// The last reason a try failed in the boot (empty when none).
    pub last: String,
}

/// A grow refused by name before any write: its state's name, its words, and the sizes when there are two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub state: &'static str,
    pub reason: String,
    pub sizes: Option<(u64, u64)>,
}

/// The progress of a grow in flight, as the driver last read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// When the grow started (unix ms, as the boot wrote it), in ns.
    pub started_ns: u64,
    /// The time from the start to this read (ns).
    pub elapsed_ns: u64,
    /// The disk's size, which the grow ends at (bytes).
    pub target_bytes: u64,
    /// The bound (ns) the grow is given.
    pub bound_ns: u64,
    /// Whether the elapsed time has passed the bound.
    pub overrun: bool,
}

/// What a tick asks of the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tick {
    /// Read again at this driver-clock time (ns).
    At(u64),
    /// The grow is refused by name; `fresh` is set once, to be noted.
    Refused { why: String, fresh: bool },
    /// The boot's grow failed: its reason, whether it is new (to be noted once), and the next read (ns).
    Failed { why: String, fresh: bool, at: u64 },
    /// The grow is finished (grown, or not needed).
    Finished,
}

/// The grow of one boot, as the daemon reads it: where the data partition is, the disk (or why it is
/// not known, for the sizes the words name), and what the boot's state file last said.
#[derive(Debug)]
pub struct Task {
    /// The boot disk's whole-disk node, or the reason it is not known.
    pub source: Result<PathBuf, String>,
    /// The data partition's mount point.
    pub data: PathBuf,
    /// The last failure read from the state file.
    pub retry: Retry,
    refused: Option<Refusal>,
    unconfirmed: Option<Unconfirmed>,
    progress: Option<Progress>,
    done: Option<String>,
}

impl Task {
    /// A task that reads the state file at once.
    pub fn new(source: Result<PathBuf, String>, data: PathBuf) -> Self {
        Self {
            source,
            data,
            retry: Retry::default(),
            refused: None,
            unconfirmed: None,
            progress: None,
            done: None,
        }
    }

    /// The refusal, when the boot refused the grow by name.
    pub fn refused(&self) -> Option<&Refusal> {
        self.refused.as_ref()
    }

    /// The state the boot left when a resize was accepted and not confirmed.
    pub fn unconfirmed(&self) -> Option<&Unconfirmed> {
        self.unconfirmed.as_ref()
    }

    /// The boot's words for a grow that did nothing, when it said why (`off in the board file`).
    pub fn done(&self) -> Option<&str> {
        self.done.as_deref()
    }

    /// The grow in flight, as the last read saw it.
    pub fn progress(&self) -> Option<Progress> {
        self.progress
    }

    /// Partition 2's length in MiB as the kernel has it now, `None` when the boot disk is not known
    /// or the size cannot be read. Read for a failure's words only.
    pub fn partition_mib(&self) -> Option<u64> {
        let disk = self.source.as_ref().ok()?;
        let sectors = Disk::part_sectors(&Real::new(disk, &self.data)).ok()?;
        Some((sectors * SECTOR as u64) >> 20)
    }

    /// One read of the boot's state file, at driver-clock time `now_ns`.
    pub fn tick_real(&mut self, now_ns: u64) -> Tick {
        let path = self.data.join("board").join("slots").join(STATE_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Tick::At(now_ns + PENDING_EVERY.as_nanos() as u64);
            }
            Err(error) => {
                let why = format!(
                    "the grow's state can't be read ({}): {error}",
                    path.display()
                );
                return self.failed(now_ns, why);
            }
        };
        let Some(state) = decode_state(&text) else {
            return self.failed(
                now_ns,
                format!("the grow's state at {} does not decode", path.display()),
            );
        };
        match state {
            State::Running {
                started_ms,
                bound_ms,
                target_bytes,
                ..
            } => {
                let elapsed_ms = unix_ms().saturating_sub(started_ms);
                self.progress = Some(Progress {
                    started_ns: started_ms.saturating_mul(1_000_000),
                    elapsed_ns: elapsed_ms.saturating_mul(1_000_000),
                    target_bytes,
                    bound_ns: bound_ms.saturating_mul(1_000_000),
                    overrun: elapsed_ms > bound_ms,
                });
                Tick::At(now_ns + RUNNING_EVERY.as_nanos() as u64)
            }
            State::NotNeeded { reason } if !reason.is_empty() => {
                self.progress = None;
                self.done = Some(reason);
                Tick::At(now_ns + RUNNING_EVERY.as_nanos() as u64)
            }
            State::NotNeeded { .. } | State::Grown => {
                self.progress = None;
                Tick::Finished
            }
            State::Refused { state, reason } => {
                let fresh = self.refused.is_none();
                self.refused = Some(Refusal {
                    state: refusal_name(&state),
                    reason: reason.clone(),
                    sizes: None,
                });
                Tick::Refused { why: reason, fresh }
            }
            State::Failed { reason } => self.failed(now_ns, reason),
            State::Unconfirmed {
                reason,
                baseline,
                seen,
            } => {
                self.unconfirmed = Some(Unconfirmed {
                    baseline,
                    seen,
                    reason,
                });
                Tick::At(now_ns + PENDING_EVERY.as_nanos() as u64)
            }
        }
    }

    fn failed(&mut self, now_ns: u64, why: String) -> Tick {
        let fresh = self.retry.last != why;
        self.retry.last = why.clone();
        Tick::Failed {
            why,
            fresh,
            at: now_ns + PENDING_EVERY.as_nanos() as u64,
        }
    }
}

/// The named refusal's static name, as the info keys carry it. A name the boot writes that this
/// daemon does not know is `grow-refused`.
fn refusal_name(state: &str) -> &'static str {
    match state {
        STALLED => STALLED,
        "grow-too-large" => "grow-too-large",
        "data-read-only" => "data-read-only",
        "gpt-unavailable" => "gpt-unavailable",
        "no-disk" => "no-disk",
        _ => "grow-refused",
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests;
