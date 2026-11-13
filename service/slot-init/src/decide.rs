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
    /// The board may start it under its signature policy: always, unless the board requires
    /// signing and the slot's signature did not verify.
    pub signed: bool,
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
    let usable = |c: &&Candidate| c.resident && c.signed && c.id != failure.slot;
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

/// What the default slot's start does about a pending tryboot trial, before it mounts anything
/// but the boot partition. The firmware has no counter, so the default slot's kernel takes the
/// attempt: one per trial boot, so a trial that never reaches the daemon uses one up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TryPass {
    /// Carry on as usual: no trial file, a trial boot already, or no counter beside it.
    Boot,
    /// Write `left` attempts, then restart with the tryboot argument, which boots the trial.
    Spend { left: u8 },
    /// The trial has no attempts left: its file goes, and this boot starts the default.
    Out,
}

/// The tryboot rule. `trial_file` is whether `trial.txt` names a slot, `flag` whether the firmware
/// booted this as a trial, `attempts` the counter beside it (`None` when there is none).
pub fn tryboot_pass(trial_file: bool, flag: bool, attempts: Option<u8>) -> TryPass {
    match attempts {
        Some(0) if trial_file && !flag => TryPass::Out,
        Some(n) if trial_file && !flag => TryPass::Spend { left: n - 1 },
        _ => TryPass::Boot,
    }
}

/// What the `file` record says the boot starts, and what it writes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePick {
    /// The trial, with `left` attempts still to be taken by a later boot. It stays the trial until
    /// they are used up.
    Trial { slot: String, left: u8 },
    /// The default. `clear` when a trial with no attempts left was in the record, which is removed.
    Default { clear: bool },
}

/// The `file` record's choice for one boot: a trial with attempts left takes one now.
pub fn file_pick(trial: &str, attempts: u8) -> FilePick {
    if trial.is_empty() {
        FilePick::Default { clear: false }
    } else if attempts > 0 {
        FilePick::Trial {
            slot: trial.to_string(),
            left: attempts - 1,
        }
    } else {
        FilePick::Default { clear: true }
    }
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
            signed: true,
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

    /// The board requires signing and this slot did not verify: the boot side never falls back to it.
    #[test]
    fn a_slot_the_signature_policy_refuses_is_never_the_fallback() {
        let mut unsigned = slot("os-1", 1);
        unsigned.signed = false;
        let slots = vec![slot("os-2", 2), unsigned];
        let fail = failure("os-3", "", &slots);
        assert_eq!(pick(&fail).as_deref(), Some("os-2"));
        let only_unsigned = [{
            let mut c = slot("os-1", 1);
            c.signed = false;
            c
        }];
        assert_eq!(pick(&failure("os-3", "", &only_unsigned)), None);
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

    #[test]
    fn the_tryboot_pass_takes_one_attempt_per_trial_boot_and_stops_at_zero() {
        assert_eq!(
            tryboot_pass(true, false, Some(3)),
            TryPass::Spend { left: 2 }
        );
        assert_eq!(
            tryboot_pass(true, false, Some(1)),
            TryPass::Spend { left: 0 }
        );
        assert_eq!(tryboot_pass(true, false, Some(0)), TryPass::Out);
        // A trial boot takes nothing more; no trial file means nothing to do; no counter is as
        // the board always booted.
        assert_eq!(tryboot_pass(true, true, Some(2)), TryPass::Boot);
        assert_eq!(tryboot_pass(false, false, Some(2)), TryPass::Boot);
        assert_eq!(tryboot_pass(true, false, None), TryPass::Boot);
    }

    /// Three attempts on the tryboot form: each default boot takes one and restarts into the trial,
    /// each failed trial restarts plainly, and the default boot after the last attempt boots the
    /// default. Exactly three trial boots, as the loader's ladder gives.
    #[test]
    fn three_attempts_on_the_tryboot_form_are_three_trial_boots_then_the_default() {
        let (mut left, mut trials) = (Some(3u8), 0);
        loop {
            // A default boot (the firmware's flag is clear) runs the pass.
            match tryboot_pass(true, false, left) {
                TryPass::Spend { left: next } => {
                    left = Some(next);
                    // The restart with the tryboot argument boots the trial, which fails.
                    trials += 1;
                }
                TryPass::Out => break,
                TryPass::Boot => panic!("the pass boots nothing with attempts left"),
            }
        }
        assert_eq!(trials, 3);
        assert_eq!(left, Some(0));
    }

    #[test]
    fn the_file_record_picks_a_trial_with_attempts_and_clears_one_without() {
        assert_eq!(file_pick("", 2), FilePick::Default { clear: false });
        assert_eq!(
            file_pick("b", 2),
            FilePick::Trial {
                slot: "b".into(),
                left: 1
            }
        );
        assert_eq!(file_pick("b", 0), FilePick::Default { clear: true });
    }
}
