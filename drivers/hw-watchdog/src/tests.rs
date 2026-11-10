//! Tests run on a mock device: a plain file under the test's temporary directory stands in for
//! the node, and a fake sysfs tree holds the attributes. No test touches `/dev` or `/sys`.
//! Time is fake: each test passes the instants it wants to `tick`.

use super::*;
use lemnos_core::ConfigValue;
use lemnos_measure::test_alloc::{Counting, allocations};
use std::fs::{self, File, OpenOptions};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const MS: u64 = 1_000_000;
const S: u64 = 1_000_000_000;

/// A mock device tree: `dev/watchdog0` (a plain file) and `sys/class/watchdog/watchdog0/`.
/// Removed on drop.
struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("hw-watchdog-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("dev")).unwrap();
        fs::create_dir_all(root.join("sys/class/watchdog/watchdog0")).unwrap();
        fs::write(root.join("dev/watchdog0"), b"").unwrap();
        Self(root)
    }

    fn dev_root(&self) -> String {
        self.0.join("dev").display().to_string()
    }

    fn sys_root(&self) -> String {
        self.0.join("sys").display().to_string()
    }

    fn node(&self) -> PathBuf {
        self.0.join("dev/watchdog0")
    }

    fn attr(&self, name: &str) -> PathBuf {
        self.0.join("sys/class/watchdog/watchdog0").join(name)
    }

    fn set_attr(&self, name: &str, text: &str) {
        fs::write(self.attr(name), text).unwrap();
    }

    fn set_timeout(&self, seconds: u64) {
        self.set_attr("timeout", &format!("{seconds}\n"));
    }

    /// Every byte the driver has written to the mock node so far.
    fn bytes(&self) -> Vec<u8> {
        fs::read(self.node()).unwrap()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Binds the driver on `tree` with `device = "watchdog0"`, and `timeout_s` when given.
fn start(tree: &Tree, expected_s: Option<i64>) -> Result<HwWatchdog> {
    let dev_root = tree.dev_root();
    let sys_root = tree.sys_root();
    let mut entries = vec![
        ("device", ConfigValue::Str("watchdog0")),
        ("dev_root", ConfigValue::Str(&dev_root)),
        ("sys_root", ConfigValue::Str(&sys_root)),
    ];
    if let Some(s) = expected_s {
        entries.push(("timeout_s", ConfigValue::Int(s)));
    }
    HwWatchdog::start(HwWatchdogNeeds {}, &Config::new(&entries))
}

/// Makes every feed fail: the driver's handle becomes read-only, so its writes are refused. The
/// node itself is untouched.
fn fail_feeds(dev: &mut HwWatchdog, tree: &Tree) {
    dev.node = File::open(tree.node()).unwrap();
}

/// Restores a writable handle on the same mock node.
fn heal_feeds(dev: &mut HwWatchdog, tree: &Tree) {
    // Append, so the new handle adds to the mock's bytes rather than overwriting them.
    dev.node = OpenOptions::new().append(true).open(tree.node()).unwrap();
}

fn described(dev: &HwWatchdog) -> Vec<&'static str> {
    let mut out = Description::default();
    dev.describe(&mut out);
    out.items().map(|item| item.name).collect()
}

#[test]
fn feeds_at_half_the_timeout_on_schedule() {
    let tree = Tree::new("schedule");
    tree.set_timeout(10);
    let mut dev = start(&tree, None).unwrap();
    // The host ticks once after start, so the first feed is due at once.
    assert_eq!(dev.tick(0), Some(5 * S));
    assert_eq!(tree.bytes().len(), 1);
    // Early ticks feed nothing and say when the feed is due.
    assert_eq!(dev.tick(4 * S + 999 * MS), Some(5 * S));
    assert_eq!(tree.bytes().len(), 1);
    assert_eq!(dev.tick(5 * S), Some(10 * S));
    assert_eq!(tree.bytes().len(), 2);
    // A late tick feeds at once, and the next feed is half a timeout after that.
    assert_eq!(dev.tick(11 * S), Some(16 * S));
    assert_eq!(tree.bytes().len(), 3);
    assert_eq!(dev.health(), Health::Ok);
}

#[test]
fn a_minute_of_ticks_at_each_due_time_gives_one_feed_per_interval() {
    let tree = Tree::new("minute");
    tree.set_timeout(10);
    let mut dev = start(&tree, None).unwrap();
    let mut now = 0;
    // Feeds at 0, 5, 10, ... 60 s: thirteen in all.
    for _ in 0..13 {
        now = dev.tick(now).unwrap();
    }
    assert_eq!(now, 65 * S);
    assert_eq!(tree.bytes().len(), 13);
}

#[test]
fn the_only_byte_written_is_the_feed_and_never_the_magic_close() {
    let tree = Tree::new("bytes");
    tree.set_timeout(4);
    tree.set_attr("identity", "mock-wdt\n");
    tree.set_attr("bootstatus", "0\n");
    let attrs_before = [
        fs::read(tree.attr("identity")).unwrap(),
        fs::read(tree.attr("timeout")).unwrap(),
        fs::read(tree.attr("bootstatus")).unwrap(),
    ];
    let mut dev = start(&tree, None).unwrap();
    let mut now = 0;
    for _ in 0..50 {
        now = dev.tick(now).unwrap();
    }
    // Unbinding stops the feeding and writes nothing: the device is left to reset the board.
    drop(dev);
    let bytes = tree.bytes();
    assert_eq!(bytes.len(), 50);
    assert!(
        bytes.iter().all(|b| *b == FEED),
        "a byte other than the feed was written"
    );
    assert!(
        !bytes.contains(&b'V'),
        "the magic close character was written"
    );
    assert_ne!(FEED, b'V');
    // Sysfs is read only: nothing the driver could see was changed.
    let attrs_after = [
        fs::read(tree.attr("identity")).unwrap(),
        fs::read(tree.attr("timeout")).unwrap(),
        fs::read(tree.attr("bootstatus")).unwrap(),
    ];
    assert_eq!(attrs_before, attrs_after);
}

#[test]
fn a_failed_feed_is_retried_with_a_backoff_capped_at_8_seconds() {
    let tree = Tree::new("backoff");
    // An hour is long enough that the time left never holds the backoff down.
    tree.set_timeout(3600);
    let mut dev = start(&tree, None).unwrap();
    assert_eq!(dev.tick(0), Some(1800 * S));
    fail_feeds(&mut dev, &tree);

    let mut now = 1800 * S;
    let gaps_ms = [250, 500, 1000, 2000, 4000, 8000, 8000, 8000, 8000];
    for gap_ms in gaps_ms {
        let next = dev.tick(now).unwrap();
        assert_eq!(next - now, gap_ms * MS, "retry after a failure");
        assert_eq!(dev.health(), Health::Degraded(FEED_FAILING));
        now = next;
    }
    // Nothing reached the node while feeds failed.
    assert_eq!(tree.bytes().len(), 1);

    // The first good feed after the failures is back on schedule and clears the state.
    heal_feeds(&mut dev, &tree);
    assert_eq!(dev.tick(now), Some(now + 1800 * S));
    assert_eq!(dev.health(), Health::Ok);
    assert_eq!(tree.bytes().len(), 2);
}

#[test]
fn backoff_doubles_from_250_ms_and_stops_at_8_seconds() {
    assert_eq!(backoff_ns(1), 250 * MS);
    assert_eq!(backoff_ns(2), 500 * MS);
    assert_eq!(backoff_ns(5), 4 * S);
    assert_eq!(backoff_ns(6), 8 * S);
    assert_eq!(backoff_ns(7), 8 * S);
    assert_eq!(backoff_ns(u32::MAX), 8 * S);
}

#[test]
fn retry_is_held_to_the_time_left_in_timeleft() {
    let tree = Tree::new("timeleft");
    tree.set_timeout(3600);
    tree.set_attr("timeleft", "6\n");
    let mut dev = start(&tree, None).unwrap();
    dev.tick(0);
    fail_feeds(&mut dev, &tree);
    // Uncapped the backoff would reach 8 s. With 6 s left a retry waits at most 3 s.
    let mut now = 1800 * S;
    for gap_ms in [250, 500, 1000, 2000, 3000, 3000, 3000] {
        let next = dev.tick(now).unwrap();
        assert_eq!(next - now, gap_ms * MS);
        now = next;
    }
}

#[test]
fn no_time_left_still_retries_without_spinning() {
    let tree = Tree::new("timeleft-zero");
    tree.set_timeout(3600);
    tree.set_attr("timeleft", "0\n");
    let mut dev = start(&tree, None).unwrap();
    dev.tick(0);
    fail_feeds(&mut dev, &tree);
    let next = dev.tick(1800 * S).unwrap();
    assert_eq!(next - 1800 * S, RETRY_FLOOR_NS);
}

#[test]
fn retry_lands_before_the_device_resets_when_the_last_good_feed_is_known() {
    // No timeleft attribute: the time left comes from the last good feed and the timeout.
    let tree = Tree::new("deadline");
    tree.set_timeout(10);
    let mut dev = start(&tree, None).unwrap();
    dev.tick(0); // good: the device resets at 10 s
    fail_feeds(&mut dev, &tree);
    let deadline = 10 * S;
    let mut now = 5 * S;
    // Keep failing until the time left is short. Every retry lands before the reset.
    while deadline - now > 100 * MS {
        let next = dev.tick(now).unwrap();
        assert!(
            next < deadline,
            "retry at {next} ns is after the reset at {deadline} ns"
        );
        now = next;
    }
}

#[test]
fn unknown_timeout_feeds_every_second_and_says_so() {
    let tree = Tree::new("unknown");
    let mut dev = start(&tree, None).unwrap();
    assert_eq!(dev.tick(0), Some(S));
    assert_eq!(dev.tick(S), Some(2 * S));
    assert_eq!(tree.bytes().len(), 2);
    assert_eq!(dev.health(), Health::Degraded(TIMEOUT_UNKNOWN));
}

#[test]
fn a_zero_timeout_counts_as_unknown() {
    let tree = Tree::new("zero");
    tree.set_timeout(0);
    let dev = start(&tree, None).unwrap();
    assert_eq!(dev.health(), Health::Degraded(TIMEOUT_UNKNOWN));
}

#[test]
fn a_timeout_that_differs_from_the_board_is_degraded_and_not_set() {
    let tree = Tree::new("mismatch");
    tree.set_timeout(30);
    let mut dev = start(&tree, Some(10)).unwrap();
    assert_eq!(dev.health(), Health::Degraded(TIMEOUT_DIFFERS));
    // The driver feeds at half the device's own timeout, and does not change it.
    assert_eq!(dev.tick(0), Some(15 * S));
    assert_eq!(fs::read_to_string(tree.attr("timeout")).unwrap(), "30\n");
}

#[test]
fn a_timeout_that_matches_the_board_is_ok() {
    let tree = Tree::new("match");
    tree.set_timeout(30);
    let dev = start(&tree, Some(30)).unwrap();
    assert_eq!(dev.health(), Health::Ok);
}

#[test]
fn an_absent_node_fails_the_bind_and_creates_nothing() {
    let tree = Tree::new("absent");
    fs::remove_file(tree.node()).unwrap();
    let result = start(&tree, None);
    assert_eq!(result.err(), Some(Status::NOT_FOUND));
    assert!(!tree.node().exists(), "the bind created the node");
}

#[test]
fn settings_must_name_one_plain_device() {
    let tree = Tree::new("names");
    let dev_root = tree.dev_root();
    let sys_root = tree.sys_root();
    for name in ["", "../watchdog0", "a/b", ".", ".."] {
        let entries = [
            ("device", ConfigValue::Str(name)),
            ("dev_root", ConfigValue::Str(&dev_root)),
            ("sys_root", ConfigValue::Str(&sys_root)),
        ];
        let result = HwWatchdog::start(HwWatchdogNeeds {}, &Config::new(&entries));
        assert_eq!(
            result.err(),
            Some(Status::INVALID_ARGUMENT),
            "name {name:?}"
        );
    }
    let no_name = [("dev_root", ConfigValue::Str(&dev_root))];
    let result = HwWatchdog::start(HwWatchdogNeeds {}, &Config::new(&no_name));
    assert_eq!(result.err(), Some(Status::INVALID_ARGUMENT));
    let zero = [
        ("device", ConfigValue::Str("watchdog0")),
        ("dev_root", ConfigValue::Str(&dev_root)),
        ("timeout_s", ConfigValue::Int(0)),
    ];
    let result = HwWatchdog::start(HwWatchdogNeeds {}, &Config::new(&zero));
    assert_eq!(result.err(), Some(Status::INVALID_ARGUMENT));
}

#[test]
fn describes_the_attributes_sysfs_offers() {
    let tree = Tree::new("describe");
    tree.set_timeout(10);
    tree.set_attr("identity", "mock-wdt\n");
    tree.set_attr("bootstatus", "0\n");
    let dev = start(&tree, None).unwrap();
    assert_eq!(described(&dev), ["identity", "timeout", "bootstatus"]);
    let mut out = Description::default();
    dev.describe(&mut out);
    assert_eq!(out.find("timeout").unwrap().unit, Unit::Second);
}

#[test]
fn absent_attributes_are_not_described_and_do_not_fail_the_bind() {
    let tree = Tree::new("bare");
    let dev = start(&tree, None).unwrap();
    assert!(described(&dev).is_empty());
    assert_eq!(dev.health(), Health::Degraded(TIMEOUT_UNKNOWN));
}

#[test]
fn provides_no_contract() {
    assert!(
        <HwWatchdog as lemnos_core::Driver>::MANIFEST
            .provides
            .is_empty()
    );
}

#[test]
fn steady_feeding_allocates_nothing() {
    let tree = Tree::new("alloc");
    tree.set_timeout(4);
    let mut dev = start(&tree, None).unwrap();
    let mut now = dev.tick(0).unwrap();
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            now = dev.tick(now).unwrap();
        }
    });
    assert_eq!(allocated, 0);
    assert_eq!(tree.bytes().len(), 1001);
}
