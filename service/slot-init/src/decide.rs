//! What to do when the slot the boot loader started can't come up. Pure: the board's state in,
//! the next step out, so every rule has its test.
//!
//! The rules, in order:
//!
//! 1. A trial boot that fails restarts plainly: the boot loader then starts the default slot,
//!    whatever else the board holds.
//! 2. The default slot failing more than [`MAX_FALLBACKS`] times in a row (counted in
//!    `board/slots/fallbacks`, which a confirmed boot clears) stops falling back: the board
//!    waits, so a console shows why, and restarts.
//! 3. Otherwise the next slot is started on trial, so that if it fails too the boot loader comes
//!    back to the default. The candidates, best first: the previous slot when it hasn't failed
//!    since its last confirm; any other slot that booted well and hasn't failed, newest first;
//!    the previous slot even though it failed; any other slot, newest first. Only a slot whose
//!    boot files are on the boot partition can be started, and never the one that just failed.
//! 4. With no candidate (a board with a single slot, or one whose previous slot was removed),
//!    the board waits and restarts into the same default: nothing better can start.

/// How many fallbacks in a row before the board stops trying other slots.
pub const MAX_FALLBACKS: u32 = 3;

/// A slot the board holds, as the boot side sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    /// It booted and was confirmed at least once (`GOOD=yes`).
    pub good: bool,
    /// It failed since it was last confirmed (`FAILED=yes`).
    pub failed: bool,
    /// Its boot files are on the boot partition, so the boot loader can start it.
    pub resident: bool,
    /// Its install order: higher is newer.
    pub seq: u64,
}

/// Where the board stands when the started slot failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure<'a> {
    /// The slot that failed.
    pub slot: &'a str,
    /// It was started on trial.
    pub trial: bool,
    /// The board's previous slot, `""` when it has none.
    pub previous: &'a str,
    /// Fallbacks so far without a confirmed boot.
    pub fallbacks: u32,
    /// Every installed slot.
    pub slots: &'a [Candidate],
}

/// The next step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Restart plainly: the boot loader starts the default slot.
    Restart,
    /// Start this slot on trial, counting one more fallback.
    Trial(String),
    /// Nothing else can start: wait, then restart plainly. The reason is for the log.
    Wait(String),
}

/// What the board does after `failure`.
pub fn after_failure(failure: &Failure<'_>) -> Next {
    if failure.trial {
        return Next::Restart;
    }
    if failure.fallbacks >= MAX_FALLBACKS {
        return Next::Wait(format!(
            "fell back {} times in a row without a confirmed boot",
            failure.fallbacks
        ));
    }
    match pick(failure) {
        Some(id) => Next::Trial(id),
        None => Next::Wait("no other slot can start".to_string()),
    }
}

/// The best slot to fall back to (rule 3), if any.
pub fn pick(failure: &Failure<'_>) -> Option<String> {
    let usable = |c: &&Candidate| c.resident && c.id != failure.slot;
    let previous = failure
        .slots
        .iter()
        .filter(usable)
        .find(|c| !failure.previous.is_empty() && c.id == failure.previous);
    let mut others: Vec<&Candidate> = failure
        .slots
        .iter()
        .filter(usable)
        .filter(|c| c.id != failure.previous)
        .collect();
    others.sort_by(|a, b| b.seq.cmp(&a.seq).then_with(|| b.id.cmp(&a.id)));
    previous
        .filter(|c| !c.failed)
        .or_else(|| others.iter().copied().find(|c| c.good && !c.failed))
        .or(previous)
        .or_else(|| others.first().copied())
        .map(|c| c.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(id: &str, seq: u64) -> Candidate {
        Candidate {
            id: id.into(),
            good: true,
            failed: false,
            resident: true,
            seq,
        }
    }

    fn failure<'a>(slot: &'a str, previous: &'a str, slots: &'a [Candidate]) -> Failure<'a> {
        Failure {
            slot,
            trial: false,
            previous,
            fallbacks: 0,
            slots,
        }
    }

    #[test]
    fn rule_1_a_failed_trial_restarts_into_the_default() {
        let slots = [slot("a", 1), slot("b", 2)];
        let mut f = failure("b", "a", &slots);
        f.trial = true;
        assert_eq!(after_failure(&f), Next::Restart);
        // Even with fallbacks used up: the default is still the way back.
        f.fallbacks = MAX_FALLBACKS;
        assert_eq!(after_failure(&f), Next::Restart);
    }

    #[test]
    fn rule_2_falling_back_stops_after_three_in_a_row() {
        let slots = [slot("a", 1), slot("b", 2)];
        let mut f = failure("b", "a", &slots);
        f.fallbacks = MAX_FALLBACKS - 1;
        assert_eq!(after_failure(&f), Next::Trial("a".into()));
        f.fallbacks = MAX_FALLBACKS;
        assert!(matches!(after_failure(&f), Next::Wait(why) if why.contains("3 times")));
    }

    #[test]
    fn rule_3_the_previous_slot_comes_first() {
        let slots = [slot("a", 1), slot("b", 2), slot("c", 3)];
        // c is newer, but a is the previous slot.
        assert_eq!(
            after_failure(&failure("b", "a", &slots)),
            Next::Trial("a".into())
        );
    }

    #[test]
    fn rule_3_a_failed_previous_gives_way_to_a_good_slot() {
        let mut slots = [slot("a", 1), slot("b", 2), slot("c", 3), slot("d", 4)];
        slots[0].failed = true;
        // d is the newest good one; b is the slot that failed now.
        assert_eq!(
            after_failure(&failure("b", "a", &slots)),
            Next::Trial("d".into())
        );
        // A slot that never booted well is no better than the failed previous one.
        slots[2].good = false;
        slots[3].good = false;
        assert_eq!(
            after_failure(&failure("b", "a", &slots)),
            Next::Trial("a".into())
        );
    }

    #[test]
    fn rule_3_with_no_previous_a_good_slot_is_used() {
        let slots = [slot("a", 1), slot("b", 2), slot("c", 3)];
        // The previous slot was removed: the newest good one that isn't the failing one.
        assert_eq!(
            after_failure(&failure("c", "", &slots)),
            Next::Trial("b".into())
        );
        // A previous slot that is gone from the board is no candidate.
        assert_eq!(
            after_failure(&failure("c", "x", &slots)),
            Next::Trial("b".into())
        );
    }

    #[test]
    fn rule_3_only_resident_slots_can_start() {
        let mut slots = [slot("a", 1), slot("b", 2), slot("c", 3)];
        slots[0].resident = false;
        assert_eq!(
            after_failure(&failure("b", "a", &slots)),
            Next::Trial("c".into())
        );
        slots[2].resident = false;
        assert!(matches!(
            after_failure(&failure("b", "a", &slots)),
            Next::Wait(_)
        ));
    }

    #[test]
    fn rule_3_an_untried_slot_is_the_last_resort() {
        let mut slots = [slot("a", 1), slot("b", 2)];
        slots[0].good = false;
        assert_eq!(
            after_failure(&failure("b", "", &slots)),
            Next::Trial("a".into())
        );
    }

    #[test]
    fn rule_3_never_the_slot_that_failed() {
        let slots = [slot("a", 1)];
        assert!(matches!(
            after_failure(&failure("a", "a", &slots)),
            Next::Wait(_)
        ));
    }

    #[test]
    fn rule_4_a_single_slot_board_waits_and_starts_it_again() {
        let slots = [slot("a", 1)];
        assert_eq!(
            after_failure(&failure("a", "", &slots)),
            Next::Wait("no other slot can start".into())
        );
    }
}
