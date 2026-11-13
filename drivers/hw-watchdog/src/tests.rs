//! The driver over a fake node: the timeout set and read back, the feed's interval, the retries
//! and their cap, and that the driver never writes the magic close. Time is fake: each test passes
//! the instants it wants to `tick`.

use super::*;
use lemnos_measure::test_alloc::{Counting, allocations};
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
    /// The next this many timeout requests fail.
    set_failures: u32,
    /// Every feed fails from this time (ns), if set.
    feed_fails_from: Option<u64>,
    /// The time of the current call, set by the test.
    now: u64,
    /// Whether feeds and requests are recorded. Off for the allocation test, which must not grow
    /// a vector.
    record: bool,
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
            set_failures: 0,
            feed_fails_from: None,
            now: 0,
            record: true,
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
            d.timeout = seconds.min(d.clamp);
            Ok(d.timeout)
        })
    }

    fn timeout(&mut self) -> std::result::Result<u32, ErrorKind> {
        Ok(self.with(|d| d.timeout))
    }

    fn timeout_range(&mut self) -> (Option<u32>, Option<u32>) {
        (None, None)
    }

    fn has_attribute(&mut self, _name: &str) -> bool {
        false
    }

    fn time_left(&mut self) -> Option<u32> {
        None
    }
}

/// A driver over `fake` asking for `wanted_s`, with no sysfs tree.
fn driver(fake: &Fake, wanted_s: u32) -> HwWatchdog {
    HwWatchdog::new(Held::boxed(Box::new(fake.clone())), wanted_s)
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
fn a_timeout_the_device_applies_differently_is_reported() {
    let fake = Fake::new(60);
    fake.with(|d| d.clamp = 12);
    let mut dog = driver(&fake, 15);
    tick_at(&fake, &mut dog, 0);
    assert_eq!(
        dog.applied_s,
        Some(12),
        "the device's own answer is what is read back"
    );
    assert_eq!(dog.health(), Health::Degraded(TIMEOUT_DIFFERS));
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
