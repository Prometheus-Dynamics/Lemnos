//! The host's clock facts, read from sysfs: whether the kernel has a clock source, and whether a
//! real-time clock (RTC) is present and was the one that set the system clock at boot.
//!
//! This driver describes values only. It provides no contract, so no method answers its items:
//! a reader learns what exists on the host and in what unit, and nothing is read on demand.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `sys_root` (string, default `"/sys"`): where sysfs is mounted. Tests point it at a fake tree.
//! - `rtc` (string, optional): the RTC's directory under `class/rtc`, such as `"rtc0"`. It must
//!   exist, or the bind fails with `NOT_FOUND`. Without it, the first RTC in name order is used.
//!
//! Binding reads the facts once, and an unreadable attribute fails the bind so the host retries
//! it. A missing clock source or a missing RTC is a fact, not a failure: the item is left out
//! (and a missing RTC leaves the device `Degraded`). The driver never gives up and never switches
//! itself off, and it has no timed work.
//!
//! The items described are:
//!
//! - `clock_source`: the kernel reports a current clock source.
//! - `rtc`: an RTC is present.
//! - `rtc_hctosys`: that RTC set the system clock at boot (its `hctosys` attribute is `1`).

#[cfg(test)]
mod tests;

use lemnos_core::{Config, Description, Health, Item, Lifecycle, MethodId, Result, Status, driver};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The method the items point at. No contract is provided, so nothing answers it.
const FACT: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// Where sysfs is mounted when the board file does not say.
const DEFAULT_SYS_ROOT: &str = "/sys";

/// The facts found at bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Facts {
    clock_source: bool,
    /// `Some(hctosys)` when an RTC is present.
    rtc: Option<bool>,
}

#[driver("host-clock", provides = [])]
pub struct HostClock {
    facts: Facts,
}

impl Lifecycle for HostClock {
    type Needs = HostClockNeeds;
    // Reaches the kernel's files itself, and on a host-owned device only reads them.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ReadsOnly;

    fn start(_needs: HostClockNeeds, cfg: &Config<'_>) -> Result<Self> {
        let root = PathBuf::from(cfg.str("sys_root").unwrap_or(DEFAULT_SYS_ROOT));
        let facts = read_facts(&root, cfg.str("rtc"))?;
        Ok(Self { facts })
    }

    fn describe(&self, out: &mut Description) {
        if self.facts.clock_source {
            out.add(Item::value("clock_source", FACT).dimensionless());
        }
        if let Some(hctosys) = self.facts.rtc {
            out.add(Item::value("rtc", FACT).dimensionless());
            if hctosys {
                out.add(Item::value("rtc_hctosys", FACT).dimensionless());
            }
        }
    }

    fn health(&self) -> Health {
        match self.facts.rtc {
            Some(_) => Health::Ok,
            None => Health::Degraded("no real-time clock"),
        }
    }
}

/// Reads the facts under `root`. An RTC named in `rtc` that does not exist is `NOT_FOUND`.
fn read_facts(root: &Path, rtc: Option<&str>) -> Result<Facts> {
    let clock_source = read_text(&root.join(CLOCK_SOURCE))?.is_some_and(|s| !s.is_empty());
    let rtc = match rtc {
        Some(name) => {
            let dir = root.join(RTC_CLASS).join(name);
            if !dir.is_dir() {
                return Err(Status::NOT_FOUND);
            }
            Some(dir)
        }
        None => first_rtc(&root.join(RTC_CLASS))?,
    };
    let rtc = match rtc {
        Some(dir) => Some(read_text(&dir.join("hctosys"))?.as_deref() == Some("1")),
        None => None,
    };
    Ok(Facts { clock_source, rtc })
}

/// `clock_source` is the current clock source, relative to the sysfs root.
const CLOCK_SOURCE: &str = "devices/system/clocksource/clocksource0/current_clocksource";
/// Where the kernel lists RTC devices, relative to the sysfs root.
const RTC_CLASS: &str = "class/rtc";

/// The first RTC directory in name order, or `None` when there are none.
fn first_rtc(class: &Path) -> Result<Option<PathBuf>> {
    let entries = match fs::read_dir(class) {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_status(&e)),
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(io::Result::ok)
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    Ok(dirs.into_iter().next())
}

/// A text attribute, trimmed, or `None` when it does not exist.
fn read_text(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.trim().to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_status(&e)),
    }
}

fn io_status(error: &io::Error) -> Status {
    match error.kind() {
        io::ErrorKind::NotFound => Status::NOT_FOUND,
        io::ErrorKind::PermissionDenied => Status::ACCESS,
        _ => Status::IO,
    }
}
