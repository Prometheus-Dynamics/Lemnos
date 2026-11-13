//! A Linux thermal zone (`/sys/class/thermal/thermal_zoneN`) as `temperature/1`,
//! in degrees Celsius.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `path` (string, required): the zone's directory, for example
//!   `"/sys/class/thermal/thermal_zone0"`. The `temp` attribute in it is read,
//!   in millidegrees Celsius.
//!
//! Binding does not touch sysfs. Each read opens `temp`, reads it into a stack
//! buffer and parses it, so a zone that is missing, unreadable or not a number
//! fails that read only. Health reports `Degraded` after a failed read and `Ok`
//! again after the next good one. The driver never gives up and never switches
//! itself off. It makes no retries of its own, so pacing between attempts
//! (capped at 8 s) is the host's job, done by how often it reads.

#[cfg(test)]
mod tests;

use lemnos_contract_temperature::Temperature;
use lemnos_core::{
    Config, Description, Health, Item, Lifecycle, MethodId, Result, Status, Unit, driver,
};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// Bytes a `temp` attribute is read into on the stack. A temperature is a few
/// digits, so anything this long is refused.
const SMALL: usize = 64;

#[driver("thermal-zone", provides = [Temperature])]
pub struct ThermalZone {
    /// `<path>/temp`, formatted once at start.
    temp: PathBuf,
    faulted: bool,
    limits: Limits,
}

/// The board's limits on a zone's reading, in whole degrees. Each is set only when the board file
/// says so: an unset limit is unknown, never a default.
#[derive(Debug, Clone, Copy, Default)]
struct Limits {
    min: Option<i64>,
    max: Option<i64>,
    warn_below: Option<i64>,
    warn_above: Option<i64>,
}

impl Lifecycle for ThermalZone {
    type Needs = ThermalZoneNeeds;
    // Reaches the kernel's files itself, and on a host-owned device only reads them.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ReadsOnly;

    fn start(_needs: ThermalZoneNeeds, cfg: &Config<'_>) -> Result<Self> {
        let dir = cfg.str("path").ok_or(Status::INVALID_ARGUMENT)?;
        let limits = Limits {
            min: cfg.int("min_c"),
            max: cfg.int("max_c"),
            warn_below: cfg.int("warn_below_c"),
            warn_above: cfg.int("warn_above_c"),
        };
        if matches!((limits.min, limits.max), (Some(min), Some(max)) if min > max) {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            temp: Path::new(dir).join("temp"),
            faulted: false,
            limits,
        })
    }

    fn describe(&self, out: &mut Description) {
        if let Some(dir) = self.temp.parent().and_then(Path::to_str) {
            out.set_location(dir);
        }
        let mut temperature = Item::value("temperature", READ).unit(Unit::DegreeCelsius, 0);
        if let (Some(min), Some(max)) = (self.limits.min, self.limits.max) {
            temperature = temperature.range(min, max);
        }
        if let Some(low) = self.limits.warn_below {
            temperature = temperature.warn_below(low);
        }
        if let Some(high) = self.limits.warn_above {
            temperature = temperature.warn_above(high);
        }
        out.add(temperature);
    }

    fn health(&self) -> Health {
        if self.faulted {
            Health::Degraded("zone unreadable: retried on each read")
        } else {
            Health::Ok
        }
    }
}

impl Temperature for ThermalZone {
    fn read(&mut self) -> Result<f32> {
        match read_millidegrees(&self.temp) {
            Ok(mc) => {
                self.faulted = false;
                Ok(mc as f32 / 1000.0)
            }
            Err(status) => {
                self.faulted = true;
                Err(status)
            }
        }
    }
}

/// Reads and parses a `temp` attribute. Allocates nothing: the path is
/// converted on the stack and the text is read into `buf`.
fn read_millidegrees(path: &Path) -> Result<i32> {
    let mut file = File::open(path).map_err(|e| io_status(&e))?;
    let mut buf = [0u8; SMALL];
    let mut len = 0;
    loop {
        match file.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_status(&e)),
        }
        if len == SMALL {
            break;
        }
    }
    if len == SMALL {
        return Err(Status::IO);
    }
    std::str::from_utf8(&buf[..len])
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .ok_or(Status::IO)
}

fn io_status(error: &io::Error) -> Status {
    match error.kind() {
        io::ErrorKind::NotFound => Status::NOT_FOUND,
        io::ErrorKind::PermissionDenied => Status::ACCESS,
        _ => Status::IO,
    }
}
