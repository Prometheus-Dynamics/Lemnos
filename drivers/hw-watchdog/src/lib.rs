//! A hardware watchdog (`<dev_root>/<device>`, such as `/dev/watchdog0`), fed from `tick`.
//!
//! Binding opens the device node for writing, which arms the watchdog. From then on the driver
//! writes one feed byte ([`FEED`]) to the node at an interval of half the device's timeout. If
//! lemnosd hangs or dies, the feeding stops and the hardware resets the board. That is the point.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `device` (string, required): the node's name under `dev_root`, such as `"watchdog0"`. A name
//!   with a `/` in it, or an empty one, is `INVALID_ARGUMENT`.
//! - `timeout_s` (integer, optional): the timeout the board expects, in seconds. The driver never
//!   sets the timeout (there are no ioctls). A device whose timeout differs is reported `Degraded`.
//! - `dev_root` (string, default `"/dev"`): where the node is. Tests point it at a temporary
//!   directory.
//! - `sys_root` (string, default `"/sys"`): where sysfs is mounted. Tests point it at a fake tree.
//!
//! Binding fails with `NOT_FOUND` when the node is absent, and the host retries the bind later.
//!
//! # What is read
//!
//! From `<sys_root>/class/watchdog/<device>/`, read-only: `timeout` at bind (the feeding interval
//! comes from it), and `timeleft` when a feed fails (it bounds the retry). `identity`, `timeout`,
//! `timeleft`, `bootstatus` and `state` are described when present at bind. An absent attribute is
//! fine. A timeout that cannot be read means the interval is unknown: the driver then feeds every
//! second, which is inside any timeout, and reports `Degraded` until it is known.
//!
//! # Never switched off
//!
//! The driver never writes the magic close character `V`, which is the only byte a watchdog takes
//! as a request to stop. Unbinding, a panic in a call, or shutdown only stops the feeding; the
//! device is left running and resets the board when it runs out. Closing the node without `V`
//! does not stop it either. No code path in this driver writes anything but [`FEED`].
//!
//! # Failure
//!
//! A failed feed write is retried on the following ticks: 250 ms, then doubling, capped at 8 s.
//! The retry never comes later than half the time left before the device resets, when that is
//! known: from the `timeleft` attribute, or from the last good feed and the timeout. The driver
//! never gives up. While a feed is failing it reports `Degraded`, and it reports `Ok` again after
//! the next good feed.

#[cfg(test)]
mod tests;

use lemnos_core::{
    Config, Description, Health, Item, Lifecycle, MethodId, Result, Status, Unit, driver,
};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The method the items point at. No contract is provided, so nothing answers it.
const FACT: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// The byte written to feed the watchdog. Any byte but the magic close character `V` feeds it;
/// this driver writes NUL and never writes `V`.
pub const FEED: u8 = 0;

const DEFAULT_DEV_ROOT: &str = "/dev";
const DEFAULT_SYS_ROOT: &str = "/sys";

const NS_PER_MS: u64 = 1_000_000;
const NS_PER_S: u64 = 1_000_000_000;
/// The first retry after a failed feed. Each failure doubles it.
const BACKOFF_FIRST_NS: u64 = 250 * NS_PER_MS;
/// The longest a retry waits, however many feeds have failed.
pub const BACKOFF_CAP_NS: u64 = 8 * NS_PER_S;
/// The shortest retry, so a failure with almost no time left does not spin the host.
const RETRY_FLOOR_NS: u64 = 10 * NS_PER_MS;
/// How often to feed when the device's timeout is unknown: inside any timeout.
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
const TIMEOUT_DIFFERS: &str = "timeout differs from the board's";
const TIMEOUT_UNKNOWN: &str = "timeout unknown: fed every 1 s";

#[driver("hw-watchdog", provides = [])]
pub struct HwWatchdog {
    /// The node, opened for writing at bind. Feeding writes [`FEED`] to it and nothing else.
    node: File,
    /// `<sys_root>/class/watchdog/<device>`.
    sys_dir: PathBuf,
    /// Which of [`ATTRIBUTES`] existed at bind.
    present: [bool; ATTRIBUTES.len()],
    /// The device's timeout, read at bind, if it could be.
    timeout_ns: Option<u64>,
    /// The timeout the board expects, if it says.
    expected_ns: Option<u64>,
    /// When the next feed (or retry) is due, on the host's clock. Zero means the first tick.
    due_ns: u64,
    /// When the device resets, as of the last good feed. `None` until one happens.
    deadline_ns: Option<u64>,
    /// Feeds that have failed in a row. Zero when the last feed was good.
    attempts: u32,
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
        let expected_ns = match cfg.int("timeout_s") {
            Some(s) if s > 0 => Some(s as u64 * NS_PER_S),
            Some(_) => return Err(Status::INVALID_ARGUMENT),
            None => None,
        };

        // Write-only, no create, no truncate: this opens the device and nothing else.
        let node = OpenOptions::new()
            .write(true)
            .open(Path::new(dev_root).join(name))
            .map_err(|e| io_status(&e))?;

        let sys_dir = Path::new(sys_root).join("class/watchdog").join(name);
        let mut present = [false; ATTRIBUTES.len()];
        for (slot, (attr, _)) in present.iter_mut().zip(ATTRIBUTES) {
            *slot = sys_dir.join(attr).exists();
        }
        let timeout_ns = read_seconds(&sys_dir.join("timeout"))
            .filter(|s| *s > 0)
            .map(to_ns);

        Ok(Self {
            node,
            sys_dir,
            present,
            timeout_ns,
            expected_ns,
            due_ns: 0,
            deadline_ns: None,
            attempts: 0,
        })
    }

    fn describe(&self, out: &mut Description) {
        for ((attr, unit), present) in ATTRIBUTES.into_iter().zip(self.present) {
            if present {
                out.add(Item::value(attr, FACT).unit(unit, 0));
            }
        }
    }

    fn health(&self) -> Health {
        if self.attempts > 0 {
            Health::Degraded(FEED_FAILING)
        } else if self.timeout_differs() {
            Health::Degraded(TIMEOUT_DIFFERS)
        } else if self.timeout_ns.is_none() {
            Health::Degraded(TIMEOUT_UNKNOWN)
        } else {
            Health::Ok
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if now_ns < self.due_ns {
            return Some(self.due_ns);
        }
        match self.node.write(&[FEED]) {
            Ok(1) => {
                self.attempts = 0;
                self.due_ns = now_ns.saturating_add(self.interval_ns());
                self.deadline_ns = self
                    .timeout_ns
                    .map(|timeout| now_ns.saturating_add(timeout));
            }
            _ => {
                self.attempts = self.attempts.saturating_add(1);
                self.due_ns = now_ns.saturating_add(self.retry_ns(now_ns));
            }
        }
        Some(self.due_ns)
    }
}

impl HwWatchdog {
    /// How long between good feeds: half the timeout, or one second when it is unknown.
    fn interval_ns(&self) -> u64 {
        match self.timeout_ns {
            Some(timeout) => timeout / 2,
            None => UNKNOWN_TIMEOUT_FEED_NS,
        }
    }

    fn timeout_differs(&self) -> bool {
        matches!((self.timeout_ns, self.expected_ns), (Some(t), Some(e)) if t != e)
    }

    /// The wait before retrying after a failed feed: the backoff, held to half the time left.
    fn retry_ns(&self, now_ns: u64) -> u64 {
        let backoff = backoff_ns(self.attempts);
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

/// The retry wait after `attempt` failed feeds in a row: 250 ms, doubling, capped at 8 s.
pub fn backoff_ns(attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(16);
    (BACKOFF_FIRST_NS << shift).min(BACKOFF_CAP_NS)
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
