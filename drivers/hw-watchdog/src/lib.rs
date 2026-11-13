//! A hardware watchdog (`<dev_root>/<device>`, such as `/dev/watchdog0`), set, fed from `tick`, and
//! kept fed only while the daemon is healthy. This is the escalation a managed board uses: if the
//! daemon dies or wedges, the feeding stops and the hardware resets the board.
//!
//! The board names it as the daemon's keep-alive, so the host ticks it only while the daemon's
//! liveness is good (see `keepalive` in the board file, and `docs/daemon.md`):
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
//! - `device` (string, required): the node's name under `dev_root`, such as `"watchdog0"`. A name
//!   with a `/` in it, or an empty one, is `INVALID_ARGUMENT`.
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
    Config, ConfigValue, Description, Health, Item, Lifecycle, MethodId, Result, Status, Unit,
    driver,
};
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

/// The device node, as the driver uses it. The real one is a file and the watchdog's requests;
/// tests use a fake.
pub trait Node: Send {
    /// Writes [`FEED`] to the node. Must write exactly one byte.
    fn feed(&mut self) -> io::Result<()>;
    /// Asks for `seconds` and returns the timeout the device applied.
    fn set_timeout(&mut self, seconds: u32) -> io::Result<u32>;
    /// The timeout the device has now.
    fn timeout(&mut self) -> io::Result<u32>;
}

/// The node a driver opened: a file, and the watchdog requests on its descriptor.
struct OpenNode {
    file: File,
}

impl Node for OpenNode {
    fn feed(&mut self) -> io::Result<()> {
        match self.file.write(&[FEED])? {
            1 => Ok(()),
            _ => Err(io::Error::from(io::ErrorKind::WriteZero)),
        }
    }

    fn set_timeout(&mut self, seconds: u32) -> io::Result<u32> {
        lemnos_driver_hw_watchdog_sys::set_timeout(self.file.as_fd(), seconds)
    }

    fn timeout(&mut self) -> io::Result<u32> {
        lemnos_driver_hw_watchdog_sys::get_timeout(self.file.as_fd())
    }
}

#[driver("hw-watchdog", provides = [])]
pub struct HwWatchdog {
    /// The node. Feeding writes [`FEED`] to it and nothing else.
    node: Box<dyn Node>,
    /// `<sys_root>/class/watchdog/<device>`.
    sys_dir: PathBuf,
    /// Which of [`ATTRIBUTES`] existed at bind.
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

    fn start(_needs: HwWatchdogNeeds, cfg: &Config<'_>) -> Result<Self> {
        let name = cfg.str("device").ok_or(Status::INVALID_ARGUMENT)?;
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(Status::INVALID_ARGUMENT);
        }
        let dev_root = cfg.str("dev_root").unwrap_or(DEFAULT_DEV_ROOT);
        let sys_root = cfg.str("sys_root").unwrap_or(DEFAULT_SYS_ROOT);
        let requested = match cfg.get("timeout_s") {
            None => DEFAULT_TIMEOUT_S,
            Some(ConfigValue::Int(s)) if (1..=i64::from(u32::MAX)).contains(&s) => s as u32,
            Some(_) => return Err(Status::INVALID_ARGUMENT),
        };

        // Write-only, no create, no truncate: this opens the device and nothing else.
        let file = OpenOptions::new()
            .write(true)
            .open(Path::new(dev_root).join(name))
            .map_err(|e| io_status(&e))?;

        let sys_dir = Path::new(sys_root).join("class/watchdog").join(name);
        let wanted_s = clamp(
            requested,
            read_seconds(&sys_dir.join("min_timeout")),
            read_seconds(&sys_dir.join("max_timeout")),
        );
        Ok(Self::new(Box::new(OpenNode { file }), sys_dir, wanted_s))
    }

    fn describe(&self, out: &mut Description) {
        for ((attr, unit), present) in ATTRIBUTES.into_iter().zip(self.present) {
            if present {
                out.add(Item::value(attr, FACT).unit(unit, 0));
            }
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
    /// A driver over `node`, whose attributes are under `sys_dir`, asking for `wanted_s`.
    fn new(node: Box<dyn Node>, sys_dir: PathBuf, wanted_s: u32) -> Self {
        let mut present = [false; ATTRIBUTES.len()];
        for (slot, (attr, _)) in present.iter_mut().zip(ATTRIBUTES) {
            *slot = sys_dir.join(attr).exists();
        }
        Self {
            node,
            sys_dir,
            present,
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
        let result = self
            .node
            .set_timeout(self.wanted_s)
            .and_then(|_| self.node.timeout());
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
        match self.node.feed() {
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
    fn retry_ns(&self, now_ns: u64) -> u64 {
        let backoff = backoff_ns(self.feed_attempts);
        let left = [
            self.deadline_ns.map(|d| d.saturating_sub(now_ns)),
            read_seconds(&self.sys_dir.join("timeleft")).map(to_ns),
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
