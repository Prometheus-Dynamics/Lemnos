//! The boot count's production side over real files in a temporary directory: the device-tree
//! values (a fake `chosen/bootloader` directory), the state, the slot's metadata, the boot history,
//! and a boot record that counts its writes. The mailbox is a recorder, and the sequence is cut at
//! every step.

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use lemnos_slot_store::arrival::{self, Arrival, Outcome};
use lemnos_slot_store::boots;
use lemnos_slot_store::fallback::POWER_ON_RESET;
use lemnos_slot_store::store::{Layout, Meta, State};

use super::count::{
    ClearCount, Fallback, Firmware, Uncleared, cleared_text, health_text, keep_baseline,
    tick_pending,
};
use crate::boot::BootRecord;

/// A directory for one test, removed after it.
struct Dir(PathBuf);

impl Dir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lemnos-count-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The step log, shared by the record and the mailbox so their order is one order.
type Log = Mutex<Vec<String>>;

/// A boot record that logs each write, and fails the `fail_at`th write (a cut before it).
struct Rec<'a> {
    log: &'a Log,
    default: String,
    episodes: Option<(String, u8)>,
    fail_at: Option<usize>,
    writes: usize,
    baseline: Option<u32>,
}

impl Rec<'_> {
    fn step(&mut self, what: String) -> io::Result<()> {
        self.writes += 1;
        if Some(self.writes) == self.fail_at {
            return Err(io::Error::other("cut"));
        }
        self.log.lock().unwrap().push(what);
        Ok(())
    }
}

impl BootRecord for Rec<'_> {
    fn baseline(&self) -> io::Result<Option<u32>> {
        Ok(self.baseline)
    }

    fn set_baseline(&mut self, count: u32) -> io::Result<()> {
        self.step(format!("baseline={count}"))?;
        self.baseline = Some(count);
        Ok(())
    }

    fn set_default(&mut self, id: &str) -> io::Result<()> {
        self.step(format!("default={id}"))?;
        self.default = id.to_string();
        Ok(())
    }

    fn set_trial(&mut self, _id: &str, _attempts: u8) -> io::Result<()> {
        Ok(())
    }

    fn pending(&self) -> io::Result<Option<(String, u8)>> {
        Ok(None)
    }

    fn set_attempts(&mut self, _attempts: u8) -> io::Result<()> {
        Ok(())
    }

    fn clear_trial(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn install(&self, _slot: &str, _payloads: &[&[u8]; 5]) -> std::io::Result<()> {
        Ok(())
    }

    fn free(&self, _slot: &str) -> std::io::Result<()> {
        Ok(())
    }

    fn restart(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn episodes(&self) -> io::Result<Option<(String, u8)>> {
        Ok(self.episodes.clone())
    }

    fn set_episodes(&mut self, slot: &str, episodes: u8) -> io::Result<()> {
        self.step(format!("episodes={slot}:{episodes}"))?;
        self.episodes = (episodes > 0).then(|| (slot.to_string(), episodes));
        Ok(())
    }

    fn clear_episodes(&mut self) -> io::Result<()> {
        self.step("episodes-clear".into())?;
        self.episodes = None;
        Ok(())
    }
}

/// The firmware mailbox: logs the clear, or refuses it.
struct Mailbox<'a> {
    log: &'a Log,
    refuse: bool,
}

impl ClearCount for Mailbox<'_> {
    fn clear_count(&self) -> io::Result<()> {
        if self.refuse {
            return Err(io::Error::other("no /dev/vcio"));
        }
        self.log.lock().unwrap().push("count-clear".into());
        Ok(())
    }
}

/// A firmware mailbox that refuses the first `refusals` clears and accepts the next. `u32::MAX`
/// refuses for ever. `calls` counts every clear attempt.
struct Flaky {
    refusals: Cell<u32>,
    calls: Cell<u32>,
}

impl Flaky {
    fn new(refusals: u32) -> Self {
        Self {
            refusals: Cell::new(refusals),
            calls: Cell::new(0),
        }
    }
}

impl ClearCount for Flaky {
    fn clear_count(&self) -> io::Result<()> {
        self.calls.set(self.calls.get() + 1);
        let left = self.refusals.get();
        if left > 0 {
            self.refusals.set(left - 1);
            return Err(io::Error::other("injected refusal"));
        }
        Ok(())
    }
}

const SECOND: u64 = 1_000_000_000;

/// The driver's tick, asked again at each time it returns, until the clear is done, or the time
/// goes past `until_ns`. Returns the times the mailbox was tried, and the last time returned.
fn run_ticks(
    layout: &Layout,
    pending: &mut Option<Uncleared>,
    mailbox: &Flaky,
    until_ns: u64,
) -> (Vec<u64>, Option<u64>) {
    let mut attempts = Vec::new();
    let mut now = 0;
    loop {
        let before = mailbox.calls.get();
        let next = tick_pending(layout, pending, now, mailbox);
        if mailbox.calls.get() != before {
            attempts.push(now);
        }
        match next {
            Some(at) if at <= until_ns => now = at,
            last => return (attempts, last),
        }
    }
}

/// M36c: a refused clear is retried from the tick. The wait starts at 1 s, doubles, and stops at
/// 8 s. Six refusals then an accepted clear: the state names each refusal's wait, and the clear
/// once, with its tries (the good boot's first try included).
#[test]
fn a_refused_clear_is_retried_on_a_wait_that_doubles_to_eight_seconds_and_then_clears() {
    let (_dir, layout) = fixture("retry", "boot-1");
    let mailbox = Flaky::new(6);
    // The good boot's clear was refused: that is the first refusal, its wait not yet timed.
    let mut pending = Some(Uncleared::refused("injected refusal"));
    // The first tick times the first wait and tries nothing.
    assert_eq!(
        tick_pending(&layout, &mut pending, 0, &mailbox),
        Some(SECOND)
    );
    assert_eq!(mailbox.calls.get(), 0);
    // The first retry, at 1 s, is refused: the next is two seconds later, and the words say so.
    assert_eq!(
        tick_pending(&layout, &mut pending, SECOND, &mailbox),
        Some(3 * SECOND)
    );
    assert_eq!(
        State::load(&layout).error,
        "the boot count is not cleared: the firmware mailbox could not clear it: injected refusal; retrying every 2s"
    );
    // The rest of the sequence: refused until the sixth refusal, then accepted.
    let (rest, last) = run_ticks(&layout, &mut pending, &mailbox, 3600 * SECOND);
    assert_eq!(last, None, "the clear is done: nothing is owed");
    assert_eq!(pending, None);
    assert_eq!(
        rest,
        [3, 7, 15, 23, 31, 39].map(|s| s * SECOND),
        "the retries after the first, at 3, 7, 15, 23, 31 and 39 s"
    );
    assert_eq!(
        mailbox.calls.get(),
        7,
        "the ticks made seven tries; the good boot's refusal is the eighth"
    );
    assert_eq!(
        State::load(&layout).error,
        "the boot count is cleared after 8 tries"
    );
}

/// M36c: a clear that the firmware refuses for ever is never given up. Ten minutes of ticks each
/// owe a wait of at most 8 s, the words keep saying so, and nothing else changes: no state change,
/// no demotion, no off.
#[test]
fn a_clear_refused_for_ever_is_never_given_up_and_nothing_turns_off() {
    let (_dir, layout) = fixture("forever", "boot-1");
    let mailbox = Flaky::new(u32::MAX);
    let mut pending = Some(Uncleared::refused("injected refusal"));
    let ten_minutes = 600 * SECOND;
    let (attempts, last) = run_ticks(&layout, &mut pending, &mailbox, ten_minutes);
    let last = last.expect("a wait is always owed");
    assert!(
        last > ten_minutes,
        "the retries go on past ten minutes: {last}"
    );
    assert!(pending.is_some(), "the count is still owed");
    let gaps: Vec<u64> = attempts.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps.len() > 60, "about one try every 8 s: {}", gaps.len());
    assert!(
        gaps.iter().all(|gap| *gap <= 8 * SECOND),
        "no wait is longer than 8 s: {gaps:?}"
    );
    assert_eq!(gaps.last(), Some(&(8 * SECOND)));
    let state = State::load(&layout);
    assert_eq!(state.state, "confirmed", "nothing turns off");
    assert_eq!(state.default, "a", "nothing is demoted");
    assert!(
        state.error.ends_with("retrying every 8s"),
        "{}",
        state.error
    );
    assert!(!state.error.contains("cleared after"), "{}", state.error);
}

/// The words: `health` and the state name the wait in seconds, and a cleared count names its tries
/// with the right word for one and for more.
#[test]
fn the_words_name_the_wait_and_the_clear() {
    for (wait_ns, secs) in [
        (SECOND, 1),
        (2 * SECOND, 2),
        (4 * SECOND, 4),
        (8 * SECOND, 8),
    ] {
        assert_eq!(
            health_text(wait_ns),
            format!(
                "the boot count is not cleared: the firmware mailbox could not clear it; retrying every {secs}s"
            )
        );
    }
    assert_eq!(cleared_text(1), "the boot count is cleared after 1 try");
    assert_eq!(cleared_text(3), "the boot count is cleared after 3 tries");
}

/// A data directory with state `default`, `previous`, and a boot history entry for `boot`.
fn fixture(label: &str, boot: &str) -> (Dir, Layout) {
    let dir = Dir::new(label);
    let layout = Layout::new(dir.0.join("data"), dir.0.join("boot"));
    fs::create_dir_all(layout.state_dir()).unwrap();
    State {
        default: "a".into(),
        previous: "b".into(),
        state: "confirmed".into(),
        ..State::default()
    }
    .save(&layout)
    .unwrap();
    boots::append(&layout, boot, "b", "unknown", "power-on", "", 16).unwrap();
    (dir, layout)
}

fn seen(files_match: bool, rsts: u32, episodes_before: u8, threshold: u8) -> Arrival {
    Arrival {
        default: "a".into(),
        previous: "b".into(),
        files_match,
        rsts,
        episodes_before,
        threshold,
    }
}

/// Runs one arrival with the record and mailbox given, returning what it did.
fn run(
    layout: &Layout,
    rec: &mut Rec<'_>,
    mailbox: &Mailbox<'_>,
    seen: &Arrival,
) -> Result<Outcome, String> {
    let mut actions = Fallback {
        layout,
        record: rec,
        mailbox,
        boot: "boot-1",
    };
    arrival::on_arrival(&mut actions, seen)
}

fn rec<'a>(log: &'a Log, episodes: Option<(&str, u8)>) -> Rec<'a> {
    Rec {
        log,
        default: "a".into(),
        episodes: episodes.map(|(s, n)| (s.to_string(), n)),
        fail_at: None,
        writes: 0,
        baseline: None,
    }
}

/// The device-tree directory with `count` and `rsts` written as big-endian words.
fn device_tree(dir: &Dir, count: &[u8], rsts: &[u8]) -> Firmware {
    let root = dir.0.join("device-tree/chosen/bootloader");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("count"), count).unwrap();
    fs::write(root.join("rsts"), rsts).unwrap();
    Firmware {
        device_tree: dir.0.join("device-tree"),
        vcio: dir.0.join("vcio"),
    }
}

#[test]
fn firmware_reads_the_count_and_rsts_big_endian() {
    let dir = Dir::new("values");
    let fw = device_tree(&dir, &[0, 0, 0, 3], &[0, 0, 0x10, 0]);
    assert_eq!(fw.count().unwrap(), 3);
    assert_eq!(fw.rsts().unwrap(), POWER_ON_RESET);
}

#[test]
fn a_count_of_the_wrong_size_or_above_255_is_refused() {
    let dir = Dir::new("bad-values");
    let short = device_tree(&dir, &[0, 3], &[0, 0, 0, 0]);
    assert!(short.count().is_err(), "a two-byte count is not a word");
    let big = device_tree(&dir, &[0, 0, 1, 0], &[0, 0, 0, 0]);
    assert!(big.count().is_err(), "256 does not fit the 8-bit count");
    let missing = Firmware {
        device_tree: dir.0.join("nowhere"),
        vcio: dir.0.join("vcio"),
    };
    assert!(missing.rsts().is_err());
}

/// A changed default demotes at once: the default is pointed at the previous, then the count is
/// cleared, and the history says what the boot found.
#[test]
fn a_changed_default_demotes_at_once_and_clears_the_count_last() {
    let (_dir, layout) = fixture("changed", "boot-1");
    let log = Log::default();
    let mut r = rec(&log, None);
    let m = Mailbox {
        log: &log,
        refuse: false,
    };
    let outcome = run(&layout, &mut r, &m, &seen(false, POWER_ON_RESET, 0, 2)).unwrap();
    assert_eq!(outcome, Outcome::DemotedAtOnce);
    assert_eq!(*log.lock().unwrap(), ["default=b", "count-clear"]);
    let state = State::load(&layout);
    assert_eq!(
        (state.default.as_str(), state.previous.as_str()),
        ("b", "a")
    );
    assert_eq!(Meta::load(&layout, "a").get("FAILED"), "yes");
    let history = boots::load(&layout);
    assert!(
        history
            .last()
            .unwrap()
            .outcome
            .starts_with("previous-by-count"),
        "the boot history names the arrival: {:?}",
        history.last()
    );
}

/// A power-type reset with the files matching: no episode, the count goes, the default stays.
#[test]
fn a_power_reset_records_no_episode() {
    let (_dir, layout) = fixture("power", "boot-1");
    let log = Log::default();
    let mut r = rec(&log, None);
    let m = Mailbox {
        log: &log,
        refuse: false,
    };
    let outcome = run(&layout, &mut r, &m, &seen(true, POWER_ON_RESET, 0, 2)).unwrap();
    assert_eq!(outcome, Outcome::NoEpisode);
    assert_eq!(*log.lock().unwrap(), ["count-clear"]);
    assert_eq!(State::load(&layout).default, "a");
}

/// A warm reset (`Restart`) with the files matching: an episode, recorded before the count is cleared.
#[test]
fn an_episode_is_recorded_before_the_count_is_cleared() {
    let (_dir, layout) = fixture("episode", "boot-1");
    let log = Log::default();
    let mut r = rec(&log, None);
    let m = Mailbox {
        log: &log,
        refuse: false,
    };
    let outcome = run(&layout, &mut r, &m, &seen(true, 0x20, 0, 2)).unwrap();
    assert_eq!(outcome, Outcome::EpisodeRecorded { episodes: 1 });
    assert_eq!(*log.lock().unwrap(), ["episodes=a:1", "count-clear"]);
    assert_eq!(
        State::load(&layout).default,
        "a",
        "below the threshold the default stays"
    );
}

/// The threshold: the episode that reaches it demotes, and the one before it does not.
#[test]
fn the_threshold_demotes_on_its_episode_and_not_before() {
    let (_dir, layout) = fixture("threshold", "boot-1");
    let log = Log::default();
    let mut r = rec(&log, Some(("a", 1)));
    let m = Mailbox {
        log: &log,
        refuse: false,
    };
    let outcome = run(&layout, &mut r, &m, &seen(true, 0x20, 1, 2)).unwrap();
    assert_eq!(outcome, Outcome::Demoted { episodes: 2 });
    assert_eq!(
        *log.lock().unwrap(),
        ["episodes=a:2", "default=b", "count-clear"]
    );
    // A threshold of one demotes on the first episode.
    let (_dir2, layout2) = fixture("threshold-one", "boot-1");
    let log2 = Log::default();
    let mut r2 = rec(&log2, None);
    let m2 = Mailbox {
        log: &log2,
        refuse: false,
    };
    let first = run(&layout2, &mut r2, &m2, &seen(true, 0x20, 0, 1)).unwrap();
    assert_eq!(first, Outcome::Demoted { episodes: 1 });
}

/// A boot that is good clears the episodes only when the record names this slot, and writes
/// nothing to the record when it does not.
#[test]
fn a_boot_good_clears_only_the_episodes_the_record_names() {
    let (_dir, layout) = fixture("good", "boot-1");
    let log = Log::default();
    let m = Mailbox {
        log: &log,
        refuse: false,
    };

    let mut named = rec(&log, Some(("a", 1)));
    {
        let mut actions = Fallback {
            layout: &layout,
            record: &mut named,
            mailbox: &m,
            boot: "boot-1",
        };
        arrival::on_boot_good(&mut actions, "a").unwrap();
    }
    assert_eq!(*log.lock().unwrap(), ["episodes-clear", "count-clear"]);

    log.lock().unwrap().clear();
    let mut other = rec(&log, Some(("x", 1)));
    {
        let mut actions = Fallback {
            layout: &layout,
            record: &mut other,
            mailbox: &m,
            boot: "boot-1",
        };
        arrival::on_boot_good(&mut actions, "a").unwrap();
    }
    assert_eq!(
        *log.lock().unwrap(),
        ["count-clear"],
        "another slot's episodes are kept"
    );

    log.lock().unwrap().clear();
    let mut none = rec(&log, None);
    {
        let mut actions = Fallback {
            layout: &layout,
            record: &mut none,
            mailbox: &m,
            boot: "boot-1",
        };
        arrival::on_boot_good(&mut actions, "a").unwrap();
    }
    assert_eq!(
        *log.lock().unwrap(),
        ["count-clear"],
        "no episodes: no write to the boot partition"
    );
}

/// A cut at every write of each arrival. Whatever the cut, the count is cleared only after the
/// step that comes before it: the episode, or the default pointer. A power cut clears the count
/// too, so the next boot counts once.
#[test]
fn a_cut_at_every_step_never_clears_the_count_before_its_writes() {
    let cases = [
        (seen(false, POWER_ON_RESET, 0, 2), 1usize),
        (seen(true, 0x20, 0, 2), 1),
        (seen(true, 0x20, 1, 2), 2),
        (seen(true, POWER_ON_RESET, 0, 2), 0),
    ];
    for (case, writes) in cases {
        for cut in 1..=writes {
            let (_dir, layout) = fixture("cut", "boot-1");
            let log = Log::default();
            let mut r = rec(
                &log,
                (case.episodes_before > 0).then_some(("a", case.episodes_before)),
            );
            r.fail_at = Some(cut);
            let m = Mailbox {
                log: &log,
                refuse: false,
            };
            let result = run(&layout, &mut r, &m, &case);
            assert!(
                result.is_err(),
                "cut {cut} of {case:?} must stop the sequence"
            );
            let log = log.lock().unwrap();
            if let Some(clear) = log.iter().position(|step| step == "count-clear") {
                assert!(
                    log[..clear]
                        .iter()
                        .any(|s| s.starts_with("episodes=") || s.starts_with("default=")),
                    "the count was cleared before any of its writes: {log:?}"
                );
            }
            let episodes = log.iter().filter(|s| s.starts_with("episodes=a:")).count();
            assert!(
                episodes <= 1,
                "one arrival records at most one episode: {log:?}"
            );
        }
    }
}

/// A count clear the mailbox refuses is named, and the sequence stops there.
#[test]
fn a_refused_count_clear_is_named() {
    let (_dir, layout) = fixture("refused", "boot-1");
    let log = Log::default();
    let mut r = rec(&log, None);
    let m = Mailbox {
        log: &log,
        refuse: true,
    };
    let err = run(&layout, &mut r, &m, &seen(true, POWER_ON_RESET, 0, 2)).unwrap_err();
    assert!(err.contains("couldn't be cleared"), "{err}");
}

/// A restart whose count can't be cleared still goes on, and the state says the boot is counted.
/// With the default unknown the check passes at once, so only the clear is under test.
#[test]
fn a_restart_goes_on_when_the_count_cannot_be_cleared_and_says_so() {
    let (_dir, layout) = fixture("restart", "boot-1");
    State {
        default: String::new(),
        ..State::default()
    }
    .save(&layout)
    .unwrap();
    let dir = Dir::new("restart-dev");
    let firmware = Firmware {
        device_tree: dir.0.join("device-tree"),
        vcio: dir.0.join("not-a-mailbox"),
    };
    fs::write(&firmware.vcio, b"plain").unwrap();
    let log: &'static Log = Box::leak(Box::new(Log::default()));
    let check = crate::guard_check(
        layout.clone(),
        Err("no boot disk in this test".into()),
        Box::new(rec(log, None)),
        Some(firmware),
    );
    assert_eq!((check.0)(true), Ok(None), "the restart is not refused");
    let state = State::load(&layout);
    assert!(
        state.error.contains("this boot is counted"),
        "the state names it: {:?}",
        state.error
    );
}

/// A boot count that cannot be cleared (no mailbox) is named as such, and the boot is still counted:
/// the arrival returns the reason, which starts with the count's named prefix, so the caller shows it.
#[test]
fn a_count_the_mailbox_cannot_clear_is_named_not_refused() {
    let (_dir, layout) = fixture("uncleared", "boot-1");
    let log = Log::default();
    let m = Mailbox {
        log: &log,
        refuse: true,
    };
    let mut none = rec(&log, None);
    let mut actions = Fallback {
        layout: &layout,
        record: &mut none,
        mailbox: &m,
        boot: "boot-1",
    };
    let why = arrival::on_boot_good(&mut actions, "a").unwrap_err();
    assert!(
        why.starts_with(crate::count::COUNT_UNCLEARED),
        "the reason names the count: {why}"
    );
    assert!(why.contains("no /dev/vcio"), "{why}");
}

/// M36b: the good boot keeps the firmware's count as the baseline, whole (above 255 too), before any
/// clear; a record that cannot keep it says so and the count is not taken as the baseline.
#[test]
fn a_good_boot_keeps_the_whole_count_as_the_baseline() {
    let dir = Dir::new("baseline-whole");
    let firmware = device_tree(&dir, &[0, 0, 1, 44], &[0, 0, 0, 0]);
    let log = Log::default();
    let mut rec = rec(&log, None);
    assert_eq!(keep_baseline(&mut rec, &firmware), Ok(300));
    assert_eq!(rec.baseline, Some(300), "not capped at the 8-bit count");
    assert_eq!(log.lock().unwrap().as_slice(), ["baseline=300"]);
}

#[test]
fn a_baseline_the_record_cannot_keep_is_named_and_not_taken() {
    let dir = Dir::new("baseline-cut");
    let firmware = device_tree(&dir, &[0, 0, 0, 50], &[0, 0, 0, 0]);
    let log = Log::default();
    let mut rec = rec(&log, None);
    rec.fail_at = Some(1);
    let why = keep_baseline(&mut rec, &firmware).unwrap_err();
    assert!(why.contains("cut"), "{why}");
    assert_eq!(rec.baseline, None);
}
