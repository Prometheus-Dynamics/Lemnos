//! The board's identity facts, served as `system.identity/1` (`docs/identity.md`).
//!
//! Settings come from the device's `config` in the board file. Every key is optional unless it
//! says otherwise:
//!
//! - `data` (default `"/data"`): the data partition, where the running slot is read from.
//! - `name` and `revision`: the board's display name and revision. The `model` fact is the name,
//!   then a space and the revision when given.
//! - `serial` (required for a serial): the source kind, one of `"device-tree"`, `"cpuinfo"`,
//!   `"nvmem"`, `"cmdline"`. Each kind takes:
//!   - `serial_path`: the file (defaults: the device tree's `serial-number`, `/proc/cpuinfo`,
//!     `/proc/cmdline`; required for `nvmem`);
//!   - `serial_offset` (default 0) and `serial_length` (required, 4 to 16): for `nvmem`;
//!   - `serial_byte_order` (`"big"` or `"little"`, default `"big"`): for `nvmem`;
//!   - `serial_key`: the command-line key, for `cmdline`.
//!
//! The `family` fact is the board file's `[board] id`, which the host hands the driver as
//! `Config::board()`. `version.lemnos` is the daemon's package version, `Config::host_version()`.
//!
//! Binding reads the serial and the running slot once. A source that cannot be read at all (a
//! missing file, an I/O error) fails the bind, so the host retries it with the failure rule's
//! backoff. A value that is read but does not fit its format, and a slot that is not installed
//! yet, leave their fact absent: the device is `Degraded` and the bind succeeds.

#[cfg(test)]
mod tests;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lemnos_contract_system_identity::SystemIdentity;
use lemnos_core::{Config, Description, Health, Item, Lifecycle, MethodId, Result, Status, driver};
use lemnos_driver_system_slots::manifest::Manifest;
use lemnos_identity::{ByteOrder, Facts, SerialError, SerialSource};

const SERIAL: MethodId = MethodId {
    contract: 0,
    method: 1,
};
const FAMILY: MethodId = MethodId {
    contract: 0,
    method: 2,
};
const MODEL: MethodId = MethodId {
    contract: 0,
    method: 3,
};
const FACTS: MethodId = MethodId {
    contract: 0,
    method: 4,
};

#[driver("system-identity", provides = [SystemIdentity])]
pub struct Identity {
    facts: Facts,
    /// `name=value` lines, built once at bind.
    lines: String,
    /// No serial could be read or fit its format: the device is `Degraded`.
    missing_serial: bool,
}

impl Lifecycle for Identity {
    type Needs = IdentityNeeds;
    // Reads the board's own files: serial and slot. A host-owned board reads them only.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ReadsOnly;

    fn start(_needs: IdentityNeeds, cfg: &Config<'_>) -> Result<Self> {
        let mut facts = Facts::default();
        let mut missing_serial = false;
        // A value that does not fit its format leaves its fact absent; `set` says so and goes on.
        let set = |facts: &mut Facts, name: &str, value: &str| {
            let _ = facts.set(name, value);
        };

        if !cfg.board().is_empty() {
            set(&mut facts, "family", cfg.board());
        }
        match serial_source(cfg)? {
            None => missing_serial = true,
            Some(source) => match source.read() {
                Ok(serial) => set(&mut facts, "serial", &serial),
                Err(SerialError::Read { source, .. }) => return Err(io_status(&source)),
                Err(SerialError::Refused(_)) => missing_serial = true,
            },
        }

        if let Some(name) = cfg.str("name").filter(|n| !n.is_empty()) {
            let model = match cfg.str("revision").filter(|r| !r.is_empty()) {
                Some(revision) => format!("{name} {revision}"),
                None => name.to_string(),
            };
            set(&mut facts, "model", &model);
        }
        let data = PathBuf::from(cfg.str("data").unwrap_or("/data"));
        slot_facts(&data, &mut facts)?;
        // The daemon's own version: the host gives it to every driver it binds.
        if !cfg.host_version().is_empty() {
            set(&mut facts, "version.lemnos", cfg.host_version());
        }

        let lines: String = facts.iter().map(|(n, v)| format!("{n}={v}\n")).collect();
        Ok(Self {
            facts,
            lines,
            missing_serial,
        })
    }

    fn describe(&self, out: &mut Description) {
        if self.facts.get("serial").is_some() {
            out.add(Item::value("serial", SERIAL));
        }
        if self.facts.get("family").is_some() {
            out.add(Item::value("family", FAMILY));
        }
        if self.facts.get("model").is_some() {
            out.add(Item::value("model", MODEL));
        }
        out.add(Item::value("facts", FACTS));
    }

    fn health(&self) -> Health {
        if self.missing_serial {
            Health::Degraded("no serial")
        } else {
            Health::Ok
        }
    }
}

impl SystemIdentity for Identity {
    fn serial(&mut self, into: &mut [u8]) -> Result<usize> {
        copy(self.facts.get("serial"), into)
    }

    fn family(&mut self, into: &mut [u8]) -> Result<usize> {
        copy(self.facts.get("family"), into)
    }

    fn model(&mut self, into: &mut [u8]) -> Result<usize> {
        copy(self.facts.get("model"), into)
    }

    fn facts(&mut self, into: &mut [u8]) -> Result<usize> {
        copy(Some(&self.lines), into)
    }
}

/// Writes `text` into `into`, or answers `NOT_FOUND` for an absent fact and `BUFFER_TOO_SMALL`
/// for a buffer that cannot hold it.
fn copy(text: Option<&str>, into: &mut [u8]) -> Result<usize> {
    let text = text.ok_or(Status::NOT_FOUND)?;
    let bytes = text.as_bytes();
    let dst = into
        .get_mut(..bytes.len())
        .ok_or(Status::BUFFER_TOO_SMALL)?;
    dst.copy_from_slice(bytes);
    Ok(bytes.len())
}

/// The board's serial as this device's config names its source, read and normalised. The daemon
/// reads it once, at setup, and gives it to a gadget that names this device as its `identity`
/// (`docs/identity.md`, `docs/boards.md`). The reason is text: it is what the board log says.
///
/// # Errors
///
/// When the config names no source, names one wrongly, or the source cannot be read or does not
/// fit the serial format.
pub fn board_serial(cfg: &Config<'_>) -> core::result::Result<String, String> {
    match serial_source(cfg) {
        Err(status) => Err(format!("the serial settings are wrong: {status:?}")),
        Ok(None) => Err("the identity device names no `serial` source".into()),
        Ok(Some(source)) => source.read().map_err(|error| error.to_string()),
    }
}

/// The serial source the config names, or `None` when it names none.
fn serial_source(cfg: &Config<'_>) -> Result<Option<SerialSource>> {
    let Some(kind) = cfg.str("serial") else {
        return Ok(None);
    };
    let path = cfg.str("serial_path");
    let source = match kind {
        "device-tree" => SerialSource::DeviceTree {
            path: path.unwrap_or(lemnos_identity::serial::DEVICE_TREE).into(),
        },
        "cpuinfo" => SerialSource::Cpuinfo {
            path: path.unwrap_or(lemnos_identity::serial::CPUINFO).into(),
        },
        "cmdline" => SerialSource::Cmdline {
            key: cfg
                .str("serial_key")
                .ok_or(Status::INVALID_ARGUMENT)?
                .to_string(),
            path: path.unwrap_or(lemnos_identity::serial::CMDLINE).into(),
        },
        "nvmem" => SerialSource::Nvmem {
            path: path.ok_or(Status::INVALID_ARGUMENT)?.into(),
            offset: u64::try_from(cfg.int("serial_offset").unwrap_or(0))
                .map_err(|_| Status::INVALID_ARGUMENT)?,
            length: usize::try_from(cfg.int("serial_length").ok_or(Status::INVALID_ARGUMENT)?)
                .map_err(|_| Status::INVALID_ARGUMENT)?,
            byte_order: match cfg.str("serial_byte_order").unwrap_or("big") {
                "big" => ByteOrder::Big,
                "little" => ByteOrder::Little,
                _ => return Err(Status::INVALID_ARGUMENT),
            },
        },
        _ => return Err(Status::INVALID_ARGUMENT),
    };
    Ok(Some(source))
}

/// The running slot's `version.slot`, `version.product` and `version.os`, from its manifest.
/// A slot that is not installed yet leaves all three absent. An unreadable file fails the bind.
fn slot_facts(data: &Path, facts: &mut Facts) -> Result<()> {
    let booted = match read(&data.join("board/slots/booted"))? {
        Some(text) => text,
        None => return Ok(()),
    };
    let Some(slot) = booted
        .lines()
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };
    let Some(text) = read(&data.join("slots").join(format!("{slot}.json")))? else {
        return Ok(());
    };
    let Ok(manifest) = Manifest::parse(&text) else {
        return Ok(());
    };
    // Any value the facts refuse leaves its fact absent, as the rest of the facts do.
    let _ = facts.set("version.slot", slot);
    let _ = facts.set("version.product", &manifest.product);
    let _ = facts.set("version.os", &manifest.version);
    Ok(())
}

/// Reads a file: `None` when it does not exist, the failure's status for any other error.
fn read(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
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
