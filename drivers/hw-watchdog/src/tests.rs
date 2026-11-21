//! The driver over a fake node: the timeout set and read back, the feed's interval, the retries
//! and their cap, and that the driver never writes the magic close. Time is fake: each test passes
//! the instants it wants to `tick`.

use super::*;
use lemnos_contract_self_test::SelfTest;
use lemnos_core::{RefusalKind, RefusalSink};
use lemnos_measure::test_alloc::{Counting, allocations};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const MS: u64 = 1_000_000;
const S: u64 = 1_000_000_000;

/// What the fake device has seen, and how it answers.
#[derive(Debug)]
struct Device {
    /// Every byte written, in order.
    written: Vec<u8>,
    /// Each timeout request, with its time.
    sets: Vec<(u64, u32)>,
    /// Feeds made, with their times.
    feeds: Vec<u64>,
    /// The timeout the device has.
    timeout: u32,
    /// It applies at most this (clamps larger requests). `u32::MAX` for none.
    clamp: u32,
    /// It applies this, whatever is asked (a device with power-of-two periods rounds up). None for none.
    applies: Option<u32>,
    /// The next this many timeout requests fail.
    set_failures: u32,
    /// Every feed fails from this time (ns), if set.
    feed_fails_from: Option<u64>,
    /// The time of the current call, set by the test.
    now: u64,
    /// Whether feeds and requests are recorded. Off for the allocation test, which must not grow
    /// a vector.
    record: bool,
    /// Every timeout read fails.
    read_fails: bool,
    /// What the device reports as time left (`timeleft`), if it has the attribute.
    time_left: Option<u32>,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Device>>);

impl Fake {
    fn new(timeout: u32) -> Self {
        Self(Arc::new(Mutex::new(Device {
            written: Vec::new(),
            sets: Vec::new(),
            feeds: Vec::new(),
            timeout,
            clamp: u32::MAX,
            applies: None,
            set_failures: 0,
            feed_fails_from: None,
            now: 0,
            record: true,
            read_fails: false,
            time_left: None,
        })))
    }

    fn with<T>(&self, f: impl FnOnce(&mut Device) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }
}

impl RawWatchdog for Fake {
    fn feed(&mut self) -> std::result::Result<(), ErrorKind> {
        self.with(|d| {
            if d.feed_fails_from.is_some_and(|from| d.now >= from) {
                return Err(ErrorKind::Failed);
            }
            if d.record {
                d.written.push(FEED);
                d.feeds.push(d.now);
            }
            Ok(())
        })
    }

    fn set_timeout(&mut self, seconds: u32) -> std::result::Result<u32, ErrorKind> {
        self.with(|d| {
            if d.record {
                d.sets.push((d.now, seconds));
            }
            if d.set_failures > 0 {
                d.set_failures -= 1;
                return Err(ErrorKind::Failed);
            }
            d.timeout = d.applies.unwrap_or(seconds.min(d.clamp));
            Ok(d.timeout)
        })
    }

    fn timeout(&mut self) -> std::result::Result<u32, ErrorKind> {
        self.with(|d| match d.read_fails {
            true => Err(ErrorKind::Failed),
            false => Ok(d.timeout),
        })
    }

    fn timeout_range(&mut self) -> (Option<u32>, Option<u32>) {
        (None, None)
    }

    fn has_attribute(&mut self, _name: &str) -> bool {
        false
    }

    fn time_left(&mut self) -> Option<u32> {
        self.with(|d| d.time_left)
    }
}

/// A driver over `fake` asking for `wanted_s`, with no sysfs tree.
fn driver(fake: &Fake, wanted_s: u32) -> HwWatchdog {
    HwWatchdog::new(Held::boxed(Box::new(fake.clone())), wanted_s, (None, None))
}

/// Ticks at `at` (ns), with the fake's clock at the same time.
fn tick_at(fake: &Fake, dog: &mut HwWatchdog, at: u64) -> Option<u64> {
    fake.with(|d| d.now = at);
    dog.tick(at)
}

#[test]
fn the_timeout_is_set_at_once_and_read_back() {
    let fake = Fake::new(60);
    let mut dog = driver(&fake, 15);
    assert_eq!(dog.health(), Health::Degraded(TIMEOUT_UNKNOWN));
    tick_at(&fake, &mut dog, 0);
    assert_eq!(fake.with(|d| d.sets.clone()), vec![(0, 15)]);
    assert_eq!(fake.with(|d| d.timeout), 15);
    assert_eq!(dog.applied_s, Some(15), "read back");
    assert_eq!(dog.health(), Health::Ok);
}

#[test]
fn the_timeout_is_not_set_again_once_it_is_set() {
    let fake = Fake::new(60);
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    tick_at(&fake, &mut dog, 7 * S);
    tick_at(&fake, &mut dog, 15 * S);
    assert_eq!(fake.with(|d| d.sets.len()), 1);
}

#[test]
fn a_timeout_the_device_applies_differently_is_read_back_and_healthy() {
    let fake = Fake::new(60);
    fake.with(|d| d.clamp = 12);
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    assert_eq!(
        dog.applied_s,
        Some(12),
        "the device's own answer is what is read back"
    );
    assert_eq!(dog.health(), Health::Ok);
}

#[test]
fn the_wanted_timeout_is_held_to_the_range_the_device_reports() {
    assert_eq!(clamp(15, Some(1), Some(10)), 10);
    assert_eq!(clamp(15, Some(20), None), 20);
    assert_eq!(clamp(15, None, None), 15);
    assert_eq!(clamp(5, Some(2), Some(30)), 5);
}

#[test]
fn a_failed_timeout_request_is_retried_on_the_backoff_and_then_succeeds() {
    let fake = Fake::new(60);
    fake.with(|d| d.set_failures = 2);
    let mut dog = driver(&fake, 15);
    let next = tick_at(&fake, &mut dog, 0);
    assert_eq!(dog.health(), Health::Degraded(TIMEOUT_NOT_SET));
    assert_eq!(next, Some(250 * MS), "the first retry is 250 ms on");
    tick_at(&fake, &mut dog, 250 * MS);
    assert_eq!(dog.health(), Health::Degraded(TIMEOUT_NOT_SET));
    tick_at(&fake, &mut dog, 750 * MS);
    assert_eq!(
        fake.with(|d| d.sets.clone()),
        vec![(0, 15), (250 * MS, 15), (750 * MS, 15)],
        "each retry waits twice as long as the one before"
    );
    assert_eq!(dog.health(), Health::Ok);
}

#[test]
fn a_device_that_rounds_the_request_up_is_fed_at_half_the_period_it_applies() {
    // A power-of-two watchdog asked for 15 s applies 22 s. The feed follows the 22 s the device
    // reads back, not the 15 s asked for: 11 s apart, so the device is fed before it resets.
    let fake = Fake::new(60);
    fake.with(|d| d.applies = Some(22));
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    assert_eq!(dog.applied_s, Some(22), "read back after the request");
    assert_eq!(dog.health(), Health::Ok);
    assert_eq!(tick_at(&fake, &mut dog, 11 * S), Some(22 * S));
    assert_eq!(tick_at(&fake, &mut dog, 22 * S), Some(33 * S));
    assert_eq!(
        fake.with(|d| d.feeds.clone()),
        vec![0, 11 * S, 22 * S],
        "a feed every 11 s, half of 22 s"
    );
}

#[test]
fn the_feed_is_every_half_the_timeout_in_force() {
    let fake = Fake::new(60);
    let mut dog = driver(&fake, 15);
    assert_eq!(
        tick_at(&fake, &mut dog, 0),
        Some(15 * S / 2),
        "first feed at once, next at 7.5 s"
    );
    assert_eq!(tick_at(&fake, &mut dog, 7 * S + 500 * MS), Some(15 * S));
    assert_eq!(fake.with(|d| d.feeds.clone()), vec![0, 7 * S + 500 * MS]);
}

#[test]
fn an_early_tick_does_not_feed() {
    let fake = Fake::new(15);
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    tick_at(&fake, &mut dog, 3 * S);
    assert_eq!(fake.with(|d| d.feeds.len()), 1);
}

#[test]
fn the_feed_is_every_second_while_the_timeout_is_unknown() {
    let fake = Fake::new(15);
    fake.with(|d| d.set_failures = u32::MAX);
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    tick_at(&fake, &mut dog, S);
    tick_at(&fake, &mut dog, 2 * S);
    assert_eq!(
        fake.with(|d| d.feeds.clone()),
        vec![0, S, 2 * S],
        "fed every second"
    );
    assert!(matches!(dog.health(), Health::Degraded(_)));
}

#[test]
fn a_failed_feed_is_retried_on_the_backoff_and_capped_at_8_seconds() {
    let fake = Fake::new(15);
    fake.with(|d| d.feed_fails_from = Some(0));
    let mut dog = driver(&fake, 15);
    let mut at = 0;
    let mut waits = Vec::new();
    for _ in 0..8 {
        let due = tick_at(&fake, &mut dog, at).unwrap();
        waits.push(due - at);
        at = due;
    }
    let expected = [250 * MS, 500 * MS, S, 2 * S, 4 * S, 8 * S, 8 * S, 8 * S];
    assert_eq!(waits, expected);
    assert_eq!(BACKOFF_CAP_NS, 8 * S);
    assert_eq!(dog.health(), Health::Degraded(FEED_FAILING));
}

#[test]
fn a_feed_that_works_again_clears_the_failure() {
    let fake = Fake::new(15);
    fake.with(|d| d.feed_fails_from = Some(0));
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    fake.with(|d| d.feed_fails_from = None);
    tick_at(&fake, &mut dog, 250 * MS);
    assert_eq!(dog.health(), Health::Ok);
    assert_eq!(fake.with(|d| d.feeds.clone()), vec![250 * MS]);
}

#[test]
fn the_driver_never_writes_the_magic_close() {
    let fake = Fake::new(15);
    fake.with(|d| d.set_failures = 1);
    let mut dog = driver(&fake, 15);
    let mut at = 0;
    for _ in 0..40 {
        at = tick_at(&fake, &mut dog, at).unwrap_or(at + S);
    }
    drop(dog);
    let written = fake.with(|d| d.written.clone());
    assert!(written.len() > 5);
    assert!(written.iter().all(|&byte| byte == FEED && byte != b'V'));
}

#[test]
fn dropping_the_driver_leaves_the_device_armed() {
    let fake = Fake::new(15);
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    let before = fake.with(|d| d.written.clone());
    drop(dog);
    assert_eq!(
        fake.with(|d| d.written.clone()),
        before,
        "nothing written on drop"
    );
}

#[test]
fn a_steady_feed_allocates_nothing() {
    let fake = Fake::new(15);
    fake.with(|d| d.record = false);
    let mut dog = driver(&fake, 15);
    let mut at = 0;
    for _ in 0..10 {
        at = tick_at(&fake, &mut dog, at).unwrap();
    }
    let ((), count) = allocations(|| {
        for _ in 0..1000 {
            at = tick_at(&fake, &mut dog, at).unwrap();
        }
    });
    assert_eq!(count, 0, "allocations in 1000 feeds");
}

#[test]
fn the_device_gives_the_timeout_range_read_at_bind() {
    use lemnos_core::{Description, Driver};
    let mut with_range = HwWatchdog::new(
        Held::boxed(Box::new(Fake::new(15))),
        15,
        (Some(1), Some(60)),
    );
    // Every attribute the device has: its timeout and time left take the device's range.
    with_range.present = [true; ATTRIBUTES.len()];
    let mut out = Description::new();
    Driver::describe(&with_range, &mut out);
    let timeout = out.find("timeout").unwrap();
    assert_eq!(
        (timeout.min, timeout.max, timeout.step),
        (Some(1), Some(60), Some(1))
    );
    let left = out.find("timeleft").unwrap();
    assert_eq!((left.min, left.max), (Some(0), Some(60)));
    assert_eq!(out.find("state").unwrap().min, None);
}

/// Runs the `answers` test to its answer on the driver's tick, and reads the answer back.
fn run_answers(
    dog: &mut HwWatchdog,
) -> (
    lemnos_core::JobState,
    Option<lemnos_contract_self_test::Record>,
) {
    let job = SelfTest::run(dog, "answers", false).unwrap();
    lemnos_core::Driver::tick(dog, 0);
    let mut buffer = [0u8; lemnos_contract_self_test::RECORD_BYTES + 8];
    let mut writer = lemnos_core::Writer::new(&mut buffer);
    let state = lemnos_core::Lifecycle::job(dog, job, &mut writer);
    let used = writer.len();
    let record = match state {
        lemnos_core::JobState::Finished(lemnos_core::Status::OK) => Some(
            lemnos_contract_self_test::Record::read(&mut lemnos_core::Reader::new(&buffer[..used]))
                .unwrap(),
        ),
        _ => None,
    };
    (state, record)
}

/// A watchdog that has done its first look at the device (the timeout is set, and the first feed
/// is made), so a test can measure what its own steps change.
fn settled(fake: &Fake) -> HwWatchdog {
    let mut dog = driver(fake, 15);
    tick_at(fake, &mut dog, 0);
    dog
}

#[test]
fn the_answers_test_reports_the_timeout_and_changes_nothing() {
    let fake = Fake::new(15);
    let mut dog = settled(&fake);
    let (feeds, sets) = fake.with(|d| (d.feeds.len(), d.sets.len()));
    let (_, record) = run_answers(&mut dog);
    let record = record.unwrap();
    assert_eq!(record.outcome, lemnos_contract_self_test::Outcome::Pass);
    assert_eq!(record.measures().next().unwrap().value, 15);
    assert_eq!(
        fake.with(|d| (d.feeds.len(), d.sets.len())),
        (feeds, sets),
        "no feed, no request"
    );
}

#[test]
fn a_device_reporting_no_timeout_fails_the_answers_test() {
    let fake = Fake::new(0);
    let mut dog = driver(&fake, 15);
    let (_, record) = run_answers(&mut dog);
    assert_eq!(
        record.unwrap().reason,
        lemnos_contract_self_test::reason::OUT_OF_RANGE
    );
}

#[test]
fn a_device_that_does_not_answer_fails_the_answers_test_and_is_not_faulted() {
    let fake = Fake::new(15);
    let mut dog = settled(&fake);
    fake.with(|d| d.read_fails = true);
    let before = lemnos_core::Driver::health(&dog);
    let (_, record) = run_answers(&mut dog);
    assert_eq!(
        record.unwrap().reason,
        lemnos_contract_self_test::reason::NO_RESPONSE
    );
    assert_eq!(lemnos_core::Driver::health(&dog), before);
}

/// The refusal `check_config` gives for `entries`, or `None` when it accepts them.
fn refusal<'a>(entries: &'a [(&'a str, ConfigValue<'a>)]) -> Option<ConfigError<'a>> {
    <HwWatchdog as lemnos_core::Lifecycle>::check_config(&Config::new(entries)).err()
}

#[test]
fn check_config_accepts_a_full_config() {
    let entries = [
        ("device", ConfigValue::Str("watchdog0")),
        ("timeout_s", ConfigValue::Int(15)),
        ("dev_root", ConfigValue::Str("/dev")),
        ("sys_root", ConfigValue::Str("/sys")),
    ];
    assert_eq!(refusal(&entries), None);
}

#[test]
fn check_config_refuses_an_unknown_key_by_name_and_suggests_the_near_one() {
    let err = refusal(&[("timeout", ConfigValue::Int(15))]).unwrap();
    assert_eq!(err.key, "timeout");
    assert_eq!(err.suggestion, Some("timeout_s"));
    assert_eq!(
        refusal(&[("bogus", ConfigValue::Int(1))]).unwrap().key,
        "bogus"
    );
}

#[test]
fn check_config_refuses_a_wrong_kind() {
    let err = refusal(&[("timeout_s", ConfigValue::Str("15"))]).unwrap();
    assert_eq!(err.key, "timeout_s");
}

#[test]
fn check_config_refuses_a_device_that_is_a_path_and_accepts_a_name() {
    for bad in ["", "../watchdog0", "a/b", ".", ".."] {
        assert_eq!(
            refusal(&[("device", ConfigValue::Str(bad))]).unwrap().key,
            "device"
        );
    }
    assert_eq!(refusal(&[("device", ConfigValue::Str("watchdog0"))]), None);
}

#[test]
fn check_config_keeps_the_timeout_to_one_second_or_more() {
    assert_eq!(
        refusal(&[("timeout_s", ConfigValue::Int(0))]).unwrap().key,
        "timeout_s"
    );
    assert_eq!(
        refusal(&[("timeout_s", ConfigValue::Int(-5))]).unwrap().key,
        "timeout_s"
    );
    assert_eq!(refusal(&[("timeout_s", ConfigValue::Int(1))]), None);
    assert_eq!(
        refusal(&[("timeout_s", ConfigValue::Int(i64::from(u32::MAX)))]),
        None
    );
    assert!(refusal(&[("timeout_s", ConfigValue::Int(i64::from(u32::MAX) + 1))]).is_some());
}

#[test]
fn a_watchdog_node_that_cannot_be_opened_is_refused_as_absent_and_names_its_path() {
    // Nothing under this directory exists, so the node cannot be opened. No real device is touched.
    let root = std::env::temp_dir().join(format!("hw-watchdog-absent-{}", std::process::id()));
    let dev_root = root.join("dev").display().to_string();
    let sys_root = root.join("sys").display().to_string();
    let entries = [
        ("device", ConfigValue::Str("watchdog0")),
        ("dev_root", ConfigValue::Str(&dev_root)),
        ("sys_root", ConfigValue::Str(&sys_root)),
    ];
    let seen = std::sync::Mutex::new(None);
    let sink = |kind: RefusalKind, reason: core::fmt::Arguments<'_>| {
        *seen.lock().unwrap() = Some((kind, reason.to_string()));
    };
    let config = Config::new(&entries).with_refusals(RefusalSink::new(&sink));
    let result = HwWatchdog::start(HwWatchdogNeeds { watchdog: None }, &config);
    assert_eq!(result.err(), Some(Status::NOT_FOUND));
    let (kind, reason) = seen
        .into_inner()
        .unwrap()
        .expect("a refused bind gives its reason");
    assert_eq!(kind, RefusalKind::Absent);
    let node = Path::new(&dev_root).join("watchdog0").display().to_string();
    assert!(reason.contains(&node), "{reason}");
}

#[test]
fn a_failed_feed_waits_half_the_time_left_before_the_deadline_when_that_is_less_than_the_backoff() {
    // Fed at 0 with a 15 s timeout, so the deadline is 15 s; the feed fails from 7.5 s on. The
    // backoff doubles (250 ms, 500 ms, 1 s, 2 s, 4 s), but from the fifth failure on the wait is
    // held to half the time left before the deadline: 3.75 s left at 11.25 s, so 1.875 s, not 4 s.
    let fake = Fake::new(15);
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    fake.with(|d| d.feed_fails_from = Some(15 * S / 2));
    let mut at = 15 * S / 2;
    let mut waits = Vec::new();
    for _ in 0..5 {
        let due = tick_at(&fake, &mut dog, at).unwrap();
        waits.push(due - at);
        at = due;
    }
    assert_eq!(
        waits,
        [250 * MS, 500 * MS, S, 2 * S, 1_875 * MS],
        "the fifth wait is half of the 3.75 s left, not the 4 s backoff"
    );
}

#[test]
fn a_failed_feed_retry_is_never_shorter_than_the_floor_however_close_the_deadline() {
    // A 1 s timeout: the deadline is 1 s after the feed at 0. Failing from 0.5 s on, the retry
    // halves the time left each time, and once that is under 20 ms the wait is the 10 ms floor and
    // no shorter.
    let fake = Fake::new(1);
    let mut dog = driver(&fake, 1);
    tick_at(&fake, &mut dog, 0);
    fake.with(|d| d.feed_fails_from = Some(S / 2));
    let mut at = S / 2;
    let mut waits = Vec::new();
    for _ in 0..40 {
        let due = tick_at(&fake, &mut dog, at).unwrap();
        waits.push(due - at);
        at = due;
    }
    assert_eq!(
        *waits.iter().min().unwrap(),
        RETRY_FLOOR_NS,
        "no wait below the floor: {waits:?}"
    );
}

#[test]
fn a_timeout_set_late_feeds_at_once_when_the_set_succeeds() {
    // The first feed is at 0 with the timeout unknown (one second to the next). The timeout request
    // fails at 0 and succeeds at 250 ms: from then the feed is due at once, not at the old 1 s mark.
    let fake = Fake::new(15);
    fake.with(|d| d.set_failures = 1);
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    let next = tick_at(&fake, &mut dog, 250 * MS);
    assert_eq!(
        fake.with(|d| d.feeds.clone()),
        vec![0, 250 * MS],
        "fed when the timeout is set"
    );
    assert_eq!(next, Some(250 * MS + 15 * S / 2));
}

/// A failing feed names how many feeds have failed in a row and the period the device applied, as
/// figures that go with the reason. A feed that works again has none.
#[test]
fn a_failing_feed_names_its_failed_count_and_the_applied_period() {
    let fake = Fake::new(15);
    fake.with(|d| d.feed_fails_from = Some(0));
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    tick_at(&fake, &mut dog, 250 * MS);
    assert_eq!(
        lemnos_core::Lifecycle::figures(&dog)
            .iter()
            .collect::<Vec<_>>(),
        vec![("failed_feeds", 2), ("timeout_s", 15)]
    );
    fake.with(|d| d.feed_fails_from = None);
    tick_at(&fake, &mut dog, 3 * S);
    assert_eq!(
        lemnos_core::Lifecycle::figures(&dog),
        lemnos_core::Figures::NONE
    );
}

// The open, handover and close paths on the real node, and the feed's retry and deadline rules.

/// A scratch tree under the temp dir, removed on drop: `dev/` for nodes, `sys/` for sysfs.
struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("hw-watchdog-mut-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("dev")).unwrap();
        Self(root)
    }
    fn dev(&self) -> String {
        self.0.join("dev").display().to_string()
    }
    fn sys(&self) -> String {
        self.0.join("sys").display().to_string()
    }
    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Starts a driver from `entries` as the daemon does, and returns it with the refusal it gave, if any.
fn start_from(
    needs: HwWatchdogNeeds,
    entries: &[(&str, ConfigValue<'_>)],
) -> (
    core::result::Result<HwWatchdog, Status>,
    Option<(RefusalKind, String)>,
) {
    let seen = std::sync::Mutex::new(None);
    let sink = |kind: RefusalKind, reason: core::fmt::Arguments<'_>| {
        *seen.lock().unwrap() = Some((kind, reason.to_string()));
    };
    let config = Config::new(entries).with_refusals(RefusalSink::new(&sink));
    let result = HwWatchdog::start(needs, &config);
    (result, seen.into_inner().unwrap())
}

#[test]
fn a_failed_feed_retry_is_held_to_the_timeleft_the_device_reports() {
    // Every feed fails and the device says 2 s are left. The backoff runs 250 ms, 500 ms, 1 s, then
    // 2 s and 4 s, but each wait is held to half the 2 s left: 1 s from the third failure on.
    let fake = Fake::new(15);
    fake.with(|d| {
        d.feed_fails_from = Some(0);
        d.time_left = Some(2);
    });
    let mut dog = driver(&fake, 15);
    let mut at = 0;
    let mut waits = Vec::new();
    for _ in 0..5 {
        let due = tick_at(&fake, &mut dog, at).unwrap();
        waits.push(due - at);
        at = due;
    }
    assert_eq!(waits, [250 * MS, 500 * MS, S, S, S]);
}

#[test]
fn a_failed_feed_retry_is_held_to_the_deadline_of_the_applied_timeout_not_the_requested() {
    // The device rounds 15 s up to 22 s. The last good feed at 0 sets the deadline at 22 s, and from
    // 11 s every feed fails. Each wait is the backoff, or half the time left to 22 s when that is less.
    // Measured from 15 s (the requested timeout) the fourth wait would be 1.125 s, not 2 s.
    let fake = Fake::new(60);
    fake.with(|d| d.applies = Some(22));
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    fake.with(|d| d.feed_fails_from = Some(11 * S));
    let mut at = 11 * S;
    let mut waits = Vec::new();
    for _ in 0..6 {
        let due = tick_at(&fake, &mut dog, at).unwrap();
        waits.push(due - at);
        at = due;
    }
    assert_eq!(
        waits,
        [250 * MS, 500 * MS, S, 2 * S, 3_625 * MS, 1_812_500_000],
    );
}

#[test]
fn a_failed_timeout_request_is_not_asked_again_before_its_retry_is_due() {
    // The first request fails at 0, so its retry waits the 250 ms backoff. A tick at 100 ms asks nothing.
    let fake = Fake::new(60);
    fake.with(|d| d.set_failures = 1);
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    tick_at(&fake, &mut dog, 100 * MS);
    assert_eq!(
        fake.with(|d| d.sets.len()),
        1,
        "no request before the retry is due"
    );
}

#[test]
fn a_failed_timeout_request_waits_the_backoff_for_its_second_retry() {
    // Fails at 0 and at 250 ms. The second retry waits 500 ms, to 750 ms: a tick at 500 ms asks nothing.
    let fake = Fake::new(60);
    fake.with(|d| d.set_failures = 2);
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    tick_at(&fake, &mut dog, 250 * MS);
    tick_at(&fake, &mut dog, 500 * MS);
    assert_eq!(
        fake.with(|d| d.sets.clone()),
        vec![(0, 15), (250 * MS, 15)],
        "the third request waits until 750 ms"
    );
    tick_at(&fake, &mut dog, 750 * MS);
    assert_eq!(fake.with(|d| d.sets.len()), 3);
}

#[test]
fn opening_an_absent_node_refuses_it_and_creates_nothing() {
    let tree = Tree::new("absent");
    let dev = tree.dev();
    let sys = tree.sys();
    let entries = [
        ("device", ConfigValue::Str("watchdog0")),
        ("dev_root", ConfigValue::Str(&dev)),
        ("sys_root", ConfigValue::Str(&sys)),
    ];
    let (result, refusal) = start_from(HwWatchdogNeeds { watchdog: None }, &entries);
    assert_eq!(result.err(), Some(Status::NOT_FOUND));
    assert_eq!(refusal.unwrap().0, RefusalKind::Absent);
    assert!(
        !tree.path("dev/watchdog0").exists(),
        "the node is opened, never created"
    );
}

#[test]
fn opening_a_node_keeps_what_it_holds() {
    let tree = Tree::new("keep");
    let dev = tree.dev();
    let sys = tree.sys();
    fs::write(tree.path("dev/watchdog0"), b"keep").unwrap();
    let entries = [
        ("device", ConfigValue::Str("watchdog0")),
        ("dev_root", ConfigValue::Str(&dev)),
        ("sys_root", ConfigValue::Str(&sys)),
    ];
    let (result, _) = start_from(HwWatchdogNeeds { watchdog: None }, &entries);
    assert!(result.is_ok());
    assert_eq!(fs::read(tree.path("dev/watchdog0")).unwrap(), b"keep");
}

#[test]
fn a_node_that_refuses_the_open_for_another_reason_is_refused_as_failed() {
    // A directory is there under the node's name: opening it for writing fails, and it is not absent.
    let tree = Tree::new("dir");
    let dev = tree.dev();
    let sys = tree.sys();
    fs::create_dir(tree.path("dev/watchdog0")).unwrap();
    let entries = [
        ("device", ConfigValue::Str("watchdog0")),
        ("dev_root", ConfigValue::Str(&dev)),
        ("sys_root", ConfigValue::Str(&sys)),
    ];
    let (result, refusal) = start_from(HwWatchdogNeeds { watchdog: None }, &entries);
    assert_eq!(result.err(), Some(Status::IO));
    assert_eq!(refusal.unwrap().0, RefusalKind::Failed);
}

#[test]
fn the_device_range_and_attributes_come_from_the_nodes_own_sysfs_directory() {
    let tree = Tree::new("sysfs");
    let dev = tree.dev();
    let sys = tree.sys();
    fs::write(tree.path("dev/watchdog0"), b"").unwrap();
    let class = tree.path("sys/class/watchdog/watchdog0");
    fs::create_dir_all(&class).unwrap();
    fs::write(class.join("min_timeout"), "1\n").unwrap();
    fs::write(class.join("max_timeout"), "60\n").unwrap();
    fs::write(class.join("timeout"), "15\n").unwrap();
    fs::write(class.join("timeleft"), "5\n").unwrap();
    let entries = [
        ("device", ConfigValue::Str("watchdog0")),
        ("timeout_s", ConfigValue::Int(120)),
        ("dev_root", ConfigValue::Str(&dev)),
        ("sys_root", ConfigValue::Str(&sys)),
    ];
    let (result, _) = start_from(HwWatchdogNeeds { watchdog: None }, &entries);
    let dog = result.ok().unwrap();
    assert_eq!(
        dog.timeout_range,
        (Some(1), Some(60)),
        "read from the node's own directory"
    );
    assert_eq!(
        dog.wanted_s, 60,
        "the configured 120 s is held to the device's maximum"
    );
    // ATTRIBUTES: identity, timeout, timeleft, bootstatus, state. Only the first is absent.
    assert_eq!(dog.present, [false, true, true, false, false]);
}

#[test]
fn the_timeout_asked_for_by_default_is_15_seconds_and_the_configured_one_is_used() {
    let fake = Fake::new(60);
    let (dog, _) = start_from(
        HwWatchdogNeeds {
            watchdog: Some(Held::boxed(Box::new(fake.clone()))),
        },
        &[("dev_root", ConfigValue::Str("/nonexistent-hw-watchdog-mut"))],
    );
    assert_eq!(dog.unwrap().wanted_s, DEFAULT_TIMEOUT_S);
    assert_eq!(DEFAULT_TIMEOUT_S, 15);
    let (dog, _) = start_from(
        HwWatchdogNeeds {
            watchdog: Some(Held::boxed(Box::new(fake))),
        },
        &[("timeout_s", ConfigValue::Int(20))],
    );
    assert_eq!(dog.unwrap().wanted_s, 20);
}

#[test]
fn a_handed_over_node_is_the_one_used_and_the_device_setting_is_not_opened() {
    // The device setting names a node that does not exist. The node the host hands over is used:
    // the driver starts, and its timeout request reaches that node.
    let fake = Fake::new(60);
    let entries = [
        ("device", ConfigValue::Str("absent0")),
        ("dev_root", ConfigValue::Str("/nonexistent-hw-watchdog-mut")),
        ("timeout_s", ConfigValue::Int(20)),
    ];
    let (result, refusal) = start_from(
        HwWatchdogNeeds {
            watchdog: Some(Held::boxed(Box::new(fake.clone()))),
        },
        &entries,
    );
    assert!(refusal.is_none(), "{refusal:?}");
    let mut dog = result.ok().unwrap();
    tick_at(&fake, &mut dog, 0);
    assert_eq!(fake.with(|d| d.sets.clone()), vec![(0, 20)]);
}

#[test]
fn the_real_node_gets_only_feed_bytes_and_is_left_running_when_the_driver_is_dropped() {
    // The real open node, not the fake: every byte it takes is a feed (zero), and dropping the driver
    // writes nothing, so the device stays armed.
    let tree = Tree::new("real");
    let dev = tree.dev();
    let sys = tree.sys();
    fs::write(tree.path("dev/watchdog0"), b"").unwrap();
    let entries = [
        ("device", ConfigValue::Str("watchdog0")),
        ("dev_root", ConfigValue::Str(&dev)),
        ("sys_root", ConfigValue::Str(&sys)),
    ];
    let (result, _) = start_from(HwWatchdogNeeds { watchdog: None }, &entries);
    let mut dog = result.ok().unwrap();
    let mut at = 0;
    for _ in 0..6 {
        at = dog.tick(at).unwrap();
    }
    let before = fs::read(tree.path("dev/watchdog0")).unwrap();
    assert!(before.len() >= 3, "fed on the real node: {}", before.len());
    assert!(before.iter().all(|&byte| byte == FEED), "{before:?}");
    drop(dog);
    assert_eq!(
        fs::read(tree.path("dev/watchdog0")).unwrap(),
        before,
        "nothing written on drop"
    );
}
