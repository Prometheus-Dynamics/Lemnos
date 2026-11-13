//! lemnosd's count-based fallback, as an ordered sequence of steps over an [`Actions`] trait. The
//! order is the rule: a cut between any two steps leaves a state the next boot can finish from.
//!
//! The daemon implements [`Actions`] over the real files, the boot-history file, the indicator and
//! the mailbox; the test implementation here models the board and cuts at every step.
//!
//! - Arrival on PREVIOUS by the count: report first, then decide ([`crate::fallback::decide`]):
//!   - files do not match the manifest: mark the default not good, point the default at the previous,
//!     clear the count;
//!   - warm reset (`Restart`), files match: record the episode; if the threshold is reached, mark not
//!     good and point at the previous; then clear the count;
//!   - `Power` or `Unknown` (no bit rule names the reset), files match: clear the count, no episode.
//! - Boot good: clear the slot's episodes, then the count.
//! - Before an intended restart of Lemnos's own: clear the count.

use crate::fallback::{self, Cause, Decision};

/// What the daemon knows when it arrives on the previous slot by the count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrival {
    /// The default slot the count skipped.
    pub default: String,
    /// The previous slot the board is running.
    pub previous: String,
    /// The default's boot files match its manifest.
    pub files_match: bool,
    /// The last reset's `rsts` value.
    pub rsts: u32,
    /// Episodes already recorded against the default.
    pub episodes_before: u8,
    /// The threshold (`[slots] demote_after_episodes`).
    pub threshold: u8,
}

/// What the sequence did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The files did not match: demoted at once.
    DemotedAtOnce,
    /// The threshold was reached with this episode: demoted.
    Demoted {
        /// The episodes recorded, including this one.
        episodes: u8,
    },
    /// An episode was recorded below the threshold; the default stays.
    EpisodeRecorded {
        /// The episodes recorded, including this one.
        episodes: u8,
    },
    /// A power-type reset: no episode; the default stays.
    NoEpisode,
}

/// The side effects the sequence performs. Each call is one step, and each is durable when it
/// returns (one-sector writes, the history append, the mailbox clear).
pub trait Actions {
    /// The step's failure type (a cut, in the tests; an I/O error in the daemon).
    type Error;
    /// The loud report: state, indicator, and a boot-history entry with the count, `rsts` and the
    /// manifest result.
    fn report(&mut self, arrival: &Arrival, cause: Cause) -> Result<(), Self::Error>;
    /// Marks the slot not good.
    fn mark_not_good(&mut self, slot: &str) -> Result<(), Self::Error>;
    /// Records the episode count against the slot.
    fn record_episode(&mut self, slot: &str, episodes: u8) -> Result<(), Self::Error>;
    /// Writes `default.txt` and `previous.txt` to point at `to`.
    fn point_default(&mut self, to: &str) -> Result<(), Self::Error>;
    /// Clears the slot's episodes (a boot-good on the slot).
    fn clear_episodes(&mut self, slot: &str) -> Result<(), Self::Error>;
    /// Clears the boot count (mailbox tag `0x0003808d`, value zero).
    fn clear_count(&mut self) -> Result<(), Self::Error>;
}

/// The count-based fallback's sequence for one arrival on the previous slot.
pub fn on_arrival<A: Actions>(a: &mut A, arrival: &Arrival) -> Result<Outcome, A::Error> {
    let cause = fallback::cause(arrival.rsts);
    a.report(arrival, cause)?;
    match fallback::decide(
        arrival.files_match,
        cause,
        arrival.episodes_before,
        arrival.threshold,
    ) {
        Decision::DemoteNow => {
            demote(a, arrival)?;
            a.clear_count()?;
            Ok(Outcome::DemotedAtOnce)
        }
        Decision::Episode { episodes, reached } => {
            a.record_episode(&arrival.default, episodes)?;
            if reached {
                demote(a, arrival)?;
                a.clear_count()?;
                return Ok(Outcome::Demoted { episodes });
            }
            a.clear_count()?;
            Ok(Outcome::EpisodeRecorded { episodes })
        }
        Decision::NoEpisode => {
            a.clear_count()?;
            Ok(Outcome::NoEpisode)
        }
    }
}

/// Boot good on `slot`: its episodes go, then the count.
pub fn on_boot_good<A: Actions>(a: &mut A, slot: &str) -> Result<(), A::Error> {
    a.clear_episodes(slot)?;
    a.clear_count()
}

/// Lemnos's own restart: the count is cleared first, so an intended restart never counts.
pub fn before_own_restart<A: Actions>(a: &mut A) -> Result<(), A::Error> {
    a.clear_count()
}

fn demote<A: Actions>(a: &mut A, arrival: &Arrival) -> Result<(), A::Error> {
    a.mark_not_good(&arrival.default)?;
    a.point_default(&arrival.previous)
}

#[cfg(test)]
#[path = "arrival_tests.rs"]
mod tests;
