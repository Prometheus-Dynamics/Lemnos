//! A hardware watchdog (`<dev_root>/<device>`, such as `/dev/watchdog0`), set, fed from `tick`, and
//! kept fed only while the daemon is healthy. This is the escalation a managed board uses: if the
//! daemon dies or wedges, the feeding stops and the hardware resets the board.
//!
//! The board names it as the daemon's keep-alive, so the host ticks it only while the daemon's
//! liveness is good (see `keepalive` in the board file, and `docs/daemon.md`). It names the watchdog
//! either by its device-tree node, the stable form, or by its number:
//!
//! ```toml
//! [[devices]]
//! id = "watchdog"
//! driver = "hw-watchdog"
//! keepalive = true
//! uses = { watchdog = "watchdog:/soc@107c000000/watchdog@7d200000" }
//! config = { timeout_s = 15 }
//! ```
//!
//! The watchdog is the driver's optional need, `watchdog`, so the board names it in `uses`: a `bus`
//! does not stand in for it. The name is resolved by the backend when the device binds
//! (`watchdog:<node>`, or `watchdogN` as it is), and the host hands the driver the open node.
//!
//! Without a `watchdog` name, the node is the `device` setting's, opened from `dev_root`:
//!
//! ```toml
//! [[devices]]
//! id = "watchdog"
//! driver = "hw-watchdog"
//! keepalive = true
//! config = { device = "watchdog0", timeout_s = 15 }
//! ```
//!
//! Settings come from the device's `config`:
//!
//! - `device` (string): the node's name under `dev_root`, such as `"watchdog0"`, used when the
//!   device names no `watchdog`. Required then. A name with a `/` in it, or an empty one, is
//!   `INVALID_ARGUMENT`. The numbered name can change on reboot; `lemnos-ctl validate` warns about it.
//! - `timeout_s` (integer, default 15): the timeout to set, in seconds. It is clamped to the
//!   device's `min_timeout` and `max_timeout` in sysfs when those exist, and the timeout the device
//!   applies is read back. A failure to set it is retried.
//! - `dev_root` (string, default `"/dev"`): where the node is. Tests point it at a temporary
//!   directory.
//! - `sys_root` (string, default `"/sys"`): where sysfs is mounted. Tests point it at a fake tree.
//!
//! Binding fails with `NOT_FOUND` when the node is absent, and the host retries the bind later.
//! Binding opens the node for writing, which arms the watchdog with whatever timeout it has until
//! this driver sets its own.
//!
//! # Feeding
//!
//! The driver writes one feed byte ([`FEED`]) at an interval of half the timeout it applied, or
//! every second while the timeout is unknown. Feeding is the only thing the driver writes to the
//! node, apart from the timeout request.
//!
//! The driver does not decide when the daemon is healthy. The host ticks a keep-alive device only
//! while the daemon's liveness is good, so a daemon that is not healthy makes no feed. A driver
//! elsewhere on the board that panics or hangs does not stop the feeding: the host ticks this
//! device on its own lane.
//!
//! # Never disarmed
//!
//! The driver never writes the magic close character `V`, which is the only byte a watchdog takes
//! as a request to stop. Unbinding, a panic in a call, or a clean shutdown only stops the feeding,
//! and the device is left running. It then resets the board when its timeout runs out. That keeps a
//! hung shutdown from hanging the board.
//!
//! # Failure
//!
//! A failed feed is retried on the following ticks: 250 ms, then doubling, capped at 8 s. The retry
//! never comes later than half the time left before the device resets, when that is known (from the
//! `timeleft` attribute, or from the last good feed and the timeout). A failed timeout request is
//! retried the same way. The driver never gives up. It reports `Degraded` while a feed or the
//! timeout is failing, and `Ok` after the next good one.

#[cfg(test)]
mod tests;

use lemnos_core::{
    Config, ConfigValue, Description, Health, Held, Item, Lifecycle, MethodId, Result, Status,
    Unit, Watchdog, driver,
};
use lemnos_hal::ErrorKind;
use lemnos_hal::raw::RawWatchdog;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

/// The method the items point at. No contract is provided, so nothing answers it.
const FACT: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// The byte written to feed the watchdog. Any byte but the magic close character `V` feeds it;
/// this driver writes NUL and never writes `V`.
pub const FEED: u8 = 0;

/// The timeout the board gets when its file does not say.
pub const DEFAULT_TIMEOUT_S: u32 = 15;

const DEFAULT_DEV_ROOT: &str = "/dev";
const DEFAULT_SYS_ROOT: &str = "/sys";

const NS_PER_MS: u64 = 1_000_000;
const NS_PER_S: u64 = 1_000_000_000;
/// The first retry after a failure. Each failure doubles it.
const BACKOFF_FIRST_NS: u64 = 250 * NS_PER_MS;
/// The longest a retry waits, however many failures there have been.
pub const BACKOFF_CAP_NS: u64 = 8 * NS_PER_S;
/// The shortest retry, so a failure with almost no time left does not spin the host.
const RETRY_FLOOR_NS: u64 = 10 * NS_PER_MS;
/// How often to feed when the timeout is unknown: inside any timeout.
const UNKNOWN_TIMEOUT_FEED_NS: u64 = NS_PER_S;

/// Attributes under `class/watchdog/<device>`, described when present, with their units.
const ATTRIBUTES: [(&str, Unit); 5] = [
    ("identity", Unit::One),
    ("timeout", Unit::Second),
    ("timeleft", Unit::Second),
    ("bootstatus", Unit::One),
    ("state", Unit::One),
];

/// Reason strings for [`Health::Degraded`].
const FEED_FAILING: &str = "feed failed: retried with backoff";
const TIMEOUT_NOT_SET: &str = "timeout not set: retried with backoff";
const TIMEOUT_DIFFERS: &str = "timeout differs from the one asked for";
const TIMEOUT_UNKNOWN: &str = "timeout unknown: fed every 1 s";

/// The node the `device` setting names: `dev_root/<device>`, opened for writing, with its sysfs
/// attributes under `sys_root`. The numbered form of the board's watchdog, kept for a board
/// file that names it by `device`.
fn open_device(cfg: &Config<'_>) -> Result<Watchdog> {
    let name = cfg.str("device").ok_or(Status::INVALID_ARGUMENT)?;
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(Status::INVALID_ARGUMENT);
    }
    let dev_root = cfg.str("dev_root").unwrap_or(DEFAULT_DEV_ROOT);
    let sys_root = cfg.str("sys_root").unwrap_or(DEFAULT_SYS_ROOT);
    // Write-only, no create, no truncate: this opens the device and nothing else.
    let file = OpenOptions::new()
        .write(true)
        .open(Path::new(dev_root).join(name))
        .map_err(|e| io_status(&e))?;
    let sys_dir = Path::new(sys_root).join("class/watchdog").join(name);
    Ok(Held::boxed(Box::new(OpenNode { file, sys_dir })))
}

/// The node a driver opened from the `device` setting: a file, its watchdog requests on its
/// descriptor, and its sysfs attributes beside it.
struct OpenNode {
    file: File,
    sys_dir: PathBuf,
}

impl OpenNode {
    fn seconds(path: &Path) -> Option<u32> {
        read_seconds(path).and_then(|s| u32::try_from(s).ok())
    }
}

impl RawWatchdog for OpenNode {
    fn feed(&mut self) -> std::result::Result<(), ErrorKind> {
        match self.file.write(&[FEED]) {
            Ok(1) => Ok(()),
            _ => Err(ErrorKind::Failed),
        }
    }

    fn set_timeout(&mut self, seconds: u32) -> std::result::Result<u32, ErrorKind> {
        lemnos_driver_hw_watchdog_sys::set_timeout(self.file.as_fd(), seconds)
            .map_err(|_| ErrorKind::Failed)
    }

    fn timeout(&mut self) -> std::result::Result<u32, ErrorKind> {
        lemnos_driver_hw_watchdog_sys::get_timeout(self.file.as_fd()).map_err(|_| ErrorKind::Failed)
    }

    fn timeout_range(&mut self) -> (Option<u32>, Option<u32>) {
        (
            Self::seconds(&self.sys_dir.join("min_timeout")),
            Self::seconds(&self.sys_dir.join("max_timeout")),
        )
    }

    fn has_attribute(&mut self, name: &str) -> bool {
        self.sys_dir.join(name).exists()
    }

    fn time_left(&mut self) -> Option<u32> {
        Self::seconds(&self.sys_dir.join("timeleft"))
    }
}

#[driver("hw-watchdog", provides = [])]
pub struct HwWatchdog {
    /// The node: the board's watchdog, named in `uses` (`watchdog:<node>`, or the numbered
    /// `watchdogN`), or the one the `device` setting names. Always set once the driver is
    /// started. Feeding writes [`FEED`] to it and nothing else.
    #[needs(optional)]
    watchdog: Option<Watchdog>,
    /// Which of [`ATTRIBUTES`] existed at bind.
    /// The device's limits on its timeout, in seconds, from `min_timeout` and `max_timeout`.
    timeout_range: (Option<u32>, Option<u32>),
    present: [bool; ATTRIBUTES.len()],
    /// The timeout to set, after clamping to the device's range.
    wanted_s: u32,
    /// Whether the timeout still has to be set (it is set once at bind, and again after a failure).
    unset: bool,
    /// The timeout the device applied on the last successful request.
    applied_s: Option<u32>,
    /// Timeout requests that have failed in a row.
    set_attempts: u32,
    /// When the next timeout request may be made (ns).
    set_retry_ns: Option<u64>,
    /// When the next feed (or retry) is due, on the host's clock. Zero means the first tick.
    due_ns: u64,
    /// When the device resets, as of the last good feed. `None` until one happens.
    deadline_ns: Option<u64>,
    /// Feeds that have failed in a row. Zero when the last feed was good.
    feed_attempts: u32,
}

impl Lifecycle for HwWatchdog {
    type Needs = HwWatchdogNeeds;

    const TICKS: bool = true;

    fn start(needs: HwWatchdogNeeds, cfg: &Config<'_>) -> Result<Self> {
        let requested = match cfg.get("timeout_s") {
            None => DEFAULT_TIMEOUT_S,
            Some(ConfigValue::Int(s)) if (1..=i64::from(u32::MAX)).contains(&s) => s as u32,
            Some(_) => return Err(Status::INVALID_ARGUMENT),
        };
        let mut node = match needs.watchdog {
            Some(node) => node,
            None => open_device(cfg)?,
        };
        let (min, max) = node.timeout_range();
        let wanted_s = clamp(requested, min.map(u64::from), max.map(u64::from));
        Ok(Self::new(node, wanted_s, (min, max)))
    }

    fn describe(&self, out: &mut Description) {
        // The device's own limits on the timeout, read from sysfs at bind. Time left is never more
        // than the timeout, and never less than 0.
        let (min, max) = self.timeout_range;
        for ((attr, unit), present) in ATTRIBUTES.into_iter().zip(self.present) {
            if !present {
                continue;
            }
            let mut item = Item::value(attr, FACT).unit(unit, 0);
            if attr == "timeout" {
                if let (Some(min), Some(max)) = (min, max) {
                    item = item.range(i64::from(min), i64::from(max)).step(1);
                }
            } else if let (true, Some(max)) = (attr == "timeleft", max) {
                item = item.range(0, i64::from(max)).step(1);
            }
            out.add(item);
        }
    }

    fn health(&self) -> Health {
        if self.feed_attempts > 0 {
            Health::Degraded(FEED_FAILING)
        } else if self.set_attempts > 0 {
            Health::Degraded(TIMEOUT_NOT_SET)
        } else if let Some(applied) = self.applied_s {
            if applied == self.wanted_s {
                Health::Ok
            } else {
                Health::Degraded(TIMEOUT_DIFFERS)
            }
        } else {
            Health::Degraded(TIMEOUT_UNKNOWN)
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if self.unset && self.set_retry_ns.is_none_or(|at| now_ns >= at) {
            self.request_timeout(now_ns);
        }
        if now_ns >= self.due_ns {
            self.feed(now_ns);
        }
        let mut due = Some(self.due_ns);
        if let Some(at) = self.set_retry_ns.filter(|&at| at > now_ns) {
            due = due.map(|d| d.min(at));
        }
        due
    }
}

impl HwWatchdog {
    /// The node the driver feeds and asks. A driver with none is refused.
    fn node(&mut self) -> std::result::Result<&mut Watchdog, ErrorKind> {
        self.watchdog.as_mut().ok_or(ErrorKind::Failed)
    }

    /// A driver over `node`, asking for `wanted_s`.
    fn new(mut node: Watchdog, wanted_s: u32, timeout_range: (Option<u32>, Option<u32>)) -> Self {
        let mut present = [false; ATTRIBUTES.len()];
        for (slot, (attr, _)) in present.iter_mut().zip(ATTRIBUTES) {
            *slot = node.has_attribute(attr);
        }
        Self {
            watchdog: Some(node),
            present,
            timeout_range,
            wanted_s,
            unset: true,
            applied_s: None,
            set_attempts: 0,
            set_retry_ns: None,
            due_ns: 0,
            deadline_ns: None,
            feed_attempts: 0,
        }
    }

    /// Asks the device for the wanted timeout and reads back what it applied. A failure is
    /// retried after a backoff.
    fn request_timeout(&mut self, now_ns: u64) {
        let wanted = self.wanted_s;
        let result = match self.node() {
            Ok(node) => node.set_timeout(wanted).and_then(|_| node.timeout()),
            Err(error) => Err(error),
        };
        match result {
            Ok(applied) => {
                self.applied_s = Some(applied);
                self.unset = false;
                self.set_attempts = 0;
                self.set_retry_ns = None;
                // The feed interval follows the timeout that is now in force.
                self.due_ns = self.due_ns.min(now_ns);
            }
            Err(_) => {
                self.set_attempts = self.set_attempts.saturating_add(1);
                self.set_retry_ns = Some(now_ns.saturating_add(backoff_ns(self.set_attempts)));
            }
        }
    }

    /// Writes the feed and schedules the next one.
    fn feed(&mut self, now_ns: u64) {
        let fed = self.node().and_then(|node| node.feed());
        match fed {
            Ok(()) => {
                self.feed_attempts = 0;
                self.due_ns = now_ns.saturating_add(self.interval_ns());
                self.deadline_ns = self
                    .applied_s
                    .map(|timeout| now_ns.saturating_add(to_ns(u64::from(timeout))));
            }
            Err(_) => {
                self.feed_attempts = self.feed_attempts.saturating_add(1);
                self.due_ns = now_ns.saturating_add(self.retry_ns(now_ns));
            }
        }
    }

    /// How long between good feeds: half the timeout in force, or one second when it is unknown.
    fn interval_ns(&self) -> u64 {
        match self.applied_s {
            Some(timeout) => to_ns(u64::from(timeout)) / 2,
            None => UNKNOWN_TIMEOUT_FEED_NS,
        }
    }

    /// The wait before retrying after a failed feed: the backoff, held to half the time left.
    fn retry_ns(&mut self, now_ns: u64) -> u64 {
        let backoff = backoff_ns(self.feed_attempts);
        let left = [
            self.deadline_ns.map(|d| d.saturating_sub(now_ns)),
            self.watchdog
                .as_mut()
                .and_then(|node| node.time_left())
                .map(|seconds| to_ns(u64::from(seconds))),
        ]
        .into_iter()
        .flatten()
        .min();
        match left {
            Some(left) => backoff.min((left / 2).max(RETRY_FLOOR_NS)),
            None => backoff,
        }
    }
}

/// The retry wait after `attempt` failures in a row: 250 ms, doubling, capped at 8 s.
pub fn backoff_ns(attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(16);
    (BACKOFF_FIRST_NS << shift).min(BACKOFF_CAP_NS)
}

/// `requested`, held to the device's range where the device says what it is.
pub fn clamp(requested: u32, min: Option<u64>, max: Option<u64>) -> u32 {
    let mut value = u64::from(requested);
    if let Some(min) = min {
        value = value.max(min);
    }
    if let Some(max) = max {
        value = value.min(max);
    }
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// A sysfs attribute holding a whole number of seconds. `None` when it is absent, unreadable or
/// not a number. Zero is a value: `timeleft` of zero means no time is left.
fn read_seconds(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn to_ns(seconds: u64) -> u64 {
    seconds.saturating_mul(NS_PER_S)
}

fn io_status(error: &io::Error) -> Status {
    match error.kind() {
        io::ErrorKind::NotFound => Status::NOT_FOUND,
        io::ErrorKind::PermissionDenied => Status::ACCESS,
        _ => Status::IO,
    }
}
