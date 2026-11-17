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
//! Binding reads the serial and the running slot. A source that cannot be read at all (a missing
//! file, an I/O error) fails the bind, so the host retries it with the failure rule's backoff. A
//! value that is read but does not fit its format, and a slot that is not installed yet, leave
//! their fact absent: the device is `Degraded` and the bind succeeds. Each `facts` call reads the
//! running slot again, so a change of the boot marker shows without a rebind; the serial and the
//! board's own facts are read once.

#[cfg(test)]
mod tests;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lemnos_contract_system_identity::SystemIdentity;
use lemnos_core::{
    Config, ConfigError, Description, Health, Item, Key, Kind, Lifecycle, MethodId, RefusalKind,
    Result, Status, driver,
};
use lemnos_identity::{Facts, SerialError, SerialSource, Settings};
use lemnos_slot_store::manifest::Manifest;

/// A setting read, or the key it is refused on.
type Checked<T> = core::result::Result<T, ConfigError<'static>>;

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
const MACHINE_ID: MethodId = MethodId {
    contract: 0,
    method: 5,
};

/// Where the machine id is read: `/etc/machine-id`, which slot-init binds read-only over the slot's
/// own file at boot (`docs/slots.md`, "The machine id"). It is read on each call, so a driver bound
/// before the bind answers `NOT_FOUND` until it does. The id is not read from `/run/lemnos`: that is
/// the initramfs's directory, and it is gone once `switch_root` has run.
pub const MACHINE_ID_PATH: &str = "/etc/machine-id";

#[driver("system-identity", provides = [SystemIdentity])]
pub struct Identity {
    facts: Facts,
    /// The data partition the running slot is read from again on each `facts` call.
    data: PathBuf,
    /// No serial could be read or fit its format: the device is `Degraded`.
    missing_serial: bool,
    /// The file the machine id is read from (`MACHINE_ID_PATH`; a test's own file).
    machine_id: PathBuf,
}

/// Every setting the driver reads. All are optional: with no `serial` source the device is
/// degraded, not refused.
const KEYS: &[Key] = &[
    Key::optional("name", Kind::Str),
    Key::optional("revision", Kind::Str),
    Key::optional("data", Kind::Str),
    Key::optional("serial", Kind::Str),
    Key::optional("serial_path", Kind::Str),
    Key::optional("serial_key", Kind::Str),
    Key::optional("serial_offset", Kind::Int),
    Key::optional("serial_length", Kind::Int),
    Key::optional("serial_byte_order", Kind::Str),
];

impl Lifecycle for Identity {
    type Needs = IdentityNeeds;
    // Reads the board's own files: serial and slot. A host-owned board reads them only.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ReadsOnly;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        serial_source(cfg).map(|_| ())
    }

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
        let source = serial_source(cfg).map_err(|error| {
            cfg.refuse(
                Status::INVALID_ARGUMENT,
                RefusalKind::Failed,
                format_args!("{error}"),
            )
        })?;
        match source {
            // No serial source configured: a normal, healthy state. No machine id is expected.
            None => {}
            Some(source) => match source.read() {
                Ok(serial) => set(&mut facts, "serial", &serial),
                Err(SerialError::Read {
                    path,
                    source: error,
                }) => {
                    let status = io_status(&error);
                    return Err(match error.kind() {
                        io::ErrorKind::NotFound => cfg.refuse(
                            status,
                            RefusalKind::Absent,
                            format_args!("no serial source at {}", path.display()),
                        ),
                        _ => cfg.refuse(
                            status,
                            RefusalKind::Failed,
                            format_args!("cannot read the serial source at {}", path.display()),
                        ),
                    });
                }
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
        slot_facts(&data, &mut facts).map_err(|status| {
            cfg.refuse(
                status,
                RefusalKind::Failed,
                format_args!("cannot read the running slot under {}", data.display()),
            )
        })?;
        // The daemon's own version: the host gives it to every driver it binds.
        if !cfg.host_version().is_empty() {
            set(&mut facts, "version.lemnos", cfg.host_version());
        }

        Ok(Self {
            facts,
            data,
            missing_serial,
            machine_id: PathBuf::from(MACHINE_ID_PATH),
        })
    }

    fn describe(&self, out: &mut Description) {
        if self.facts.get("serial").is_some() {
            out.add(Item::value("serial", SERIAL).dimensionless());
        }
        if self.facts.get("family").is_some() {
            out.add(Item::value("family", FAMILY).dimensionless());
        }
        if self.facts.get("model").is_some() {
            out.add(Item::value("model", MODEL).dimensionless());
        }
        out.add(Item::value("facts", FACTS).dimensionless());
        // Not a fact: the machine id is answered to local callers only, so it is never in `facts`.
        out.add(Item::value("machine_id", MACHINE_ID).dimensionless());
    }

    /// Degraded only for a fault: a configured serial that does not fit, or an id on the node that is
    /// not this board's (`TRANSIENT`). No id on the node is a normal state (a boot that is not a slot's,
    /// or slot-init refused its bind with a logged reason), and so is no serial source configured.
    fn health(&self) -> Health {
        if self.missing_serial {
            Health::Degraded("no serial")
        } else if self.facts.get("serial").is_some()
            && read_machine_id(&self.machine_id).is_some()
            && self.bound_id().is_none()
        {
            Health::Degraded(TRANSIENT)
        } else {
            Health::Ok
        }
    }
}

/// Why the node's id is refused: it is not the derivation of this board's serial, so it is systemd's
/// transient id (slot-init's bind failed or never ran), not the one slot-init bound.
const TRANSIENT: &str = "transient: slot-init did not bind (see machine-id-* in the kernel log)";

impl Identity {
    /// The id on the node, when it is the derivation of this board's serial (the same derivation
    /// slot-init binds with). `None` when the node holds none, when the serial is absent (the id then
    /// cannot be checked), or when the id is not the board's: systemd's transient id is never answered.
    fn bound_id(&self) -> Option<String> {
        let id = read_machine_id(&self.machine_id)?;
        let serial = self.facts.get("serial")?;
        (lemnos_identity::machine_id::derive(serial) == id).then_some(id)
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

    /// The machine id slot-init bound over `/etc/machine-id`, read again on each call. `NOT_FOUND`
    /// when the file is absent, empty, or holds no 32 lowercase hex digits: the root ships an empty
    /// file, so an unbound one is absent. Served to local programs and operators only (the bridge's
    /// `call` policy), so it is never published as a fact.
    fn machine_id(&mut self, into: &mut [u8]) -> Result<usize> {
        let id = self.bound_id();
        copy(id.as_deref(), into)
    }

    /// The running slot is read again on every call: its boot marker moves when the board
    /// boots a slot, and a reader that asks again then gets the new slot without a rebind. A
    /// slot file that cannot be read now makes its three facts absent for this answer; the
    /// rest are still given.
    fn facts(&mut self, into: &mut [u8]) -> Result<usize> {
        if slot_facts(&self.data, &mut self.facts).is_err() {
            remove_slot_facts(&mut self.facts);
        }
        // In the order `docs/identity.md` lists the facts, whatever order they were set in.
        let lines: String = FACTS_ORDER
            .iter()
            .filter_map(|name| {
                self.facts
                    .get(name)
                    .map(|value| format!("{name}={value}\n"))
            })
            .collect();
        copy(Some(&lines), into)
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
/// The parse and the read are `lemnos_identity::read_serial`, which slot-init also calls.
///
/// # Errors
///
/// When the config names no source, names one wrongly, or the source cannot be read or does not
/// fit the serial format.
pub fn board_serial(cfg: &Config<'_>) -> core::result::Result<String, String> {
    lemnos_identity::read_serial(&serial_settings(cfg))
}

/// The serial settings of this device's config, as the shared parse takes them.
fn serial_settings<'a>(cfg: &'a Config<'_>) -> Settings<'a> {
    Settings {
        kind: cfg.str("serial"),
        path: cfg.str("serial_path"),
        offset: cfg.int("serial_offset"),
        length: cfg.int("serial_length"),
        byte_order: cfg.str("serial_byte_order"),
        key: cfg.str("serial_key"),
    }
}

/// The serial source the config names, or `None` when it names none. Only the settings are read
/// here, so this is also the serial part of the settings check; a refusal names its key.
fn serial_source(cfg: &Config<'_>) -> Checked<Option<SerialSource>> {
    serial_settings(cfg)
        .source()
        .map_err(|error| ConfigError::new(error.key, error.reason))
}

/// The running slot's `version.slot`, `version.product` and `version.os`, from its manifest.
/// A slot that is not installed yet leaves all three absent. An unreadable file fails the bind.
fn slot_facts(data: &Path, facts: &mut Facts) -> Result<()> {
    // What the marker says now replaces what it said before; an absent one leaves none.
    remove_slot_facts(facts);
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

/// The order `facts` writes them in.
const FACTS_ORDER: [&str; 7] = [
    "family",
    "serial",
    "model",
    "version.slot",
    "version.product",
    "version.os",
    "version.lemnos",
];

/// Takes the three running-slot facts away.
fn remove_slot_facts(facts: &mut Facts) {
    for name in ["version.slot", "version.product", "version.os"] {
        facts.remove(name);
    }
}

/// Reads a file: `None` when it does not exist, the failure's status for any other error.
fn read(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_status(&e)),
    }
}

/// The machine id in `path`, as [`SystemIdentity::machine_id`] answers it: 32 lowercase hex digits
/// and a newline. `None` when the file is absent, unreadable, empty, or not an id.
fn read_machine_id(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let id = text.strip_suffix('\n').unwrap_or(&text);
    let fits = id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    fits.then(|| format!("{id}\n"))
}

fn io_status(error: &io::Error) -> Status {
    match error.kind() {
        io::ErrorKind::NotFound => Status::NOT_FOUND,
        io::ErrorKind::PermissionDenied => Status::ACCESS,
        _ => Status::IO,
    }
}
