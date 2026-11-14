//! The boot count's production side over real files in a temporary directory: the device-tree
//! values (a fake `chosen/bootloader` directory), the state, the slot's metadata, the boot history,
//! and a boot record that counts its writes. The mailbox is a recorder, and the sequence is cut at
//! every step.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use lemnos_slot_store::arrival::{self, Arrival, Outcome};
use lemnos_slot_store::boots;
use lemnos_slot_store::fallback::POWER_ON_RESET;
use lemnos_slot_store::store::{Layout, Meta, State};

use super::count::{ClearCount, Fallback, Firmware};
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
