//! The board-definition format: a versioned description of a board's
//! devices, in TOML or JSON. `docs/board-definition.md` documents it and
//! `docs/schemas/lemnos-board.schema.json` is its JSON Schema.

use crate::BoardError;
use crate::i2c_select::I2cSelector;
use lemnos_drivers_linux::SysRoot;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

/// The `format` every board definition carries.
pub const FORMAT: &str = "lemnos.board";
/// The newest `schema_version` this crate reads.
pub const SCHEMA_VERSION: u32 = 1;

/// A board: what it is and which devices it has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardDefinition {
    /// Always [`FORMAT`].
    pub format: String,
    /// The schema the file follows (at most [`SCHEMA_VERSION`]).
    pub schema_version: u32,
    pub board: BoardInfo,
    /// The devices, in order.
    #[serde(default)]
    pub devices: Vec<DeviceSpec>,
    /// Named GPIO lines no device owns, for clients' raw claims (with the
    /// state each goes back to when its claim ends).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<LineSpec>,
    /// Named PWM channels no device owns, for clients' raw claims.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pwms: Vec<PwmSpec>,
    /// Clients allowed raw bus and line access (hosts such as `lemnosd`);
    /// empty means any client.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_clients: Vec<String>,
    /// Named looks for the lights (`[looks.<name>]`), over the built-in
    /// ones; see `docs/looks.md`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub looks: BTreeMap<String, toml::Table>,
}

/// A named GPIO line for raw claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineSpec {
    /// The name clients claim it by (`aux-1`).
    pub name: String,
    /// `gpiochipN` or a chip label (`pinctrl-rp1`).
    pub chip: String,
    /// The offset on the chip.
    pub line: u32,
    /// What the line goes back to when a claim ends: `input` (high
    /// impedance, the default), `low` or `high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe: Option<String>,
}

/// A named PWM channel for raw claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PwmSpec {
    pub name: String,
    /// The `pwmchipN` number.
    pub chip: u32,
    pub channel: u32,
}

/// Identity of the board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardInfo {
    /// A stable identifier: lowercase letters, digits and `-` (`raze`).
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The tool that produced the file (`atlas 1.2.0`), when generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_by: Option<String>,
}

/// How a device is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    /// The kernel driver if it is bound, otherwise the userspace driver.
    #[default]
    Auto,
    /// The Lemnos driver over the bus (`/dev/i2c-N`).
    Userspace,
    /// The mainline kernel driver through IIO or hwmon.
    Kernel,
}

/// A bus a device sits on: `i2c-1`, `spi-0.1`, or an I2C adapter found by
/// what it is (`i2c:compatible=i2c-gpio`, `i2c:node=i2c@74000`), since bus
/// numbers depend on probe order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BusRef {
    I2c(u32),
    /// An I2C adapter found by name or device-tree node ([`I2cSelector`]).
    I2cMatch(I2cSelector),
    Spi {
        bus: u32,
        chip_select: u16,
    },
}

impl BusRef {
    /// Whether this is an I2C bus (by number or by selector).
    pub fn is_i2c(&self) -> bool {
        matches!(self, Self::I2c(_) | Self::I2cMatch(_))
    }

    /// The I2C bus number: given, or found under `sys` for a selector.
    /// `None` for a non-I2C bus.
    pub fn i2c_bus(&self, sys: &SysRoot) -> Option<Result<u32, String>> {
        match self {
            Self::I2c(bus) => Some(Ok(*bus)),
            Self::I2cMatch(selector) => Some(selector.resolve(sys)),
            Self::Spi { .. } => None,
        }
    }
}

impl fmt::Display for BusRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::I2c(bus) => write!(f, "i2c-{bus}"),
            Self::I2cMatch(selector) => selector.fmt(f),
            Self::Spi { bus, chip_select } => write!(f, "spi-{bus}.{chip_select}"),
        }
    }
}

impl FromStr for BusRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || {
            format!(
                "bus {s:?}: expected \"i2c-<n>\", \"i2c:<key>=<value>[;...]\" or \"spi-<bus>.<cs>\""
            )
        };
        if let Some(rest) = s.strip_prefix("i2c:") {
            return I2cSelector::parse(rest)
                .map(Self::I2cMatch)
                .map_err(|e| format!("bus {s:?}: {e}"));
        }
        if let Some(n) = s.strip_prefix("i2c-") {
            return n.parse().map(Self::I2c).map_err(|_| bad());
        }
        if let Some(rest) = s.strip_prefix("spi-") {
            let (bus, cs) = rest.split_once('.').ok_or_else(bad)?;
            return Ok(Self::Spi {
                bus: bus.parse().map_err(|_| bad())?,
                chip_select: cs.parse().map_err(|_| bad())?,
            });
        }
        Err(bad())
    }
}

impl Serialize for BusRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BusRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A driver-specific setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigValue {
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    List(Vec<ConfigValue>),
}

impl ConfigValue {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(v) => Some(*v),
            _ => None,
        }
    }

    /// Integers and floats as `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(v) => Some(*v as f64),
            Self::Float(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }
}

/// One device on the board.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSpec {
    /// Unique within the board: lowercase letters, digits and `-` (`imu`).
    pub id: String,
    /// The driver that builds it (`bmi088`, `ina238`, `hwmon-fan`); see
    /// [`DriverRegistry`](crate::DriverRegistry).
    pub driver: String,
    /// A human-readable name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub backend: Backend,
    /// The bus, for bus devices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus: Option<BusRef>,
    /// The 7-bit I2C address; the driver's default when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<u16>,
    /// A device node or sysfs directory, for devices found by path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Attributes that find a kernel class device (`name = "pwm-fan"`,
    /// `type = "cpu-thermal"`).
    #[serde(default, rename = "match", skip_serializing_if = "BTreeMap::is_empty")]
    pub matches: BTreeMap<String, String>,
    /// The fastest hosts read it, in milliseconds: the cap on any
    /// subscription's rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_ms: Option<u32>,
    /// How often hosts read it while no client subscribes, in milliseconds
    /// (0: not at all). Left out: `poll_ms`, except for IMU-class devices,
    /// which are not read until someone subscribes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_poll_ms: Option<u32>,
    /// Clients allowed to write its controls (hosts with a write policy,
    /// such as `lemnosd`); empty means any client.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writers: Vec<String>,
    /// Driver-specific settings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, ConfigValue>,
    /// Clients allowed raw I2C or SPI transactions to this device's
    /// addresses (brokered by the host between the device's own accesses);
    /// empty means none. Lines and PWM channels a device owns are never
    /// handed out raw.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw: Vec<String>,
}

impl DeviceSpec {
    /// A device with only an id and a driver.
    pub fn new(id: impl Into<String>, driver: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            driver: driver.into(),
            label: None,
            backend: Backend::Auto,
            bus: None,
            address: None,
            path: None,
            matches: BTreeMap::new(),
            poll_ms: None,
            idle_poll_ms: None,
            writers: Vec::new(),
            config: BTreeMap::new(),
            raw: Vec::new(),
        }
    }

    pub fn on(mut self, bus: BusRef) -> Self {
        self.bus = Some(bus);
        self
    }

    pub fn at(mut self, address: u16) -> Self {
        self.address = Some(address);
        self
    }

    pub fn with(mut self, key: impl Into<String>, value: ConfigValue) -> Self {
        self.config.insert(key.into(), value);
        self
    }

    pub fn matching(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.matches.insert(key.into(), value.into());
        self
    }

    pub fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = backend;
        self
    }
}

/// Whether a `writers` or `raw_clients` entry admits `client`: an exact name,
/// or a prefix ending in `*` (`orion:*` admits every `orion:` client; a bare
/// `*` admits any client).
pub fn client_matches(entry: &str, client: &str) -> bool {
    match entry.strip_suffix('*') {
        Some(prefix) => client.starts_with(prefix),
        None => entry == client,
    }
}

/// Whether a client entry is well formed: `*` may only end it.
pub fn is_valid_client_entry(entry: &str) -> bool {
    !entry.strip_suffix('*').unwrap_or(entry).contains('*')
}

/// Whether `id` is lowercase letters, digits and inner `-`.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl BoardDefinition {
    /// An empty board with the current format and schema.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            format: FORMAT.into(),
            schema_version: SCHEMA_VERSION,
            board: BoardInfo {
                id: id.into(),
                name: None,
                revision: None,
                generated_by: None,
            },
            devices: Vec::new(),
            lines: Vec::new(),
            pwms: Vec::new(),
            looks: BTreeMap::new(),
            raw_clients: Vec::new(),
        }
    }

    pub fn with_device(mut self, device: DeviceSpec) -> Self {
        self.devices.push(device);
        self
    }

    /// Parses TOML. Does not validate drivers; see [`validate`](Self::validate).
    pub fn from_toml_str(text: &str) -> Result<Self, BoardError> {
        toml::from_str(text).map_err(|e| BoardError::Parse(e.to_string()))
    }

    /// Parses JSON.
    pub fn from_json_str(text: &str) -> Result<Self, BoardError> {
        serde_json::from_str(text).map_err(|e| BoardError::Parse(e.to_string()))
    }

    /// Reads a `.toml` or `.json` file.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, BoardError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| BoardError::Io(format!("{}: {e}", path.display())))?;
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => Self::from_json_str(&text),
            _ => Self::from_toml_str(&text),
        }
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string_pretty(self).expect("board definitions serialize")
    }

    /// The device called `id`.
    pub fn device(&self, id: &str) -> Option<&DeviceSpec> {
        self.devices.iter().find(|d| d.id == id)
    }

    /// Checks the format, schema version, identifiers and, against
    /// `registry`, every device's driver, bus, address and settings. Returns
    /// every problem found.
    pub fn validate(&self, registry: &crate::DriverRegistry) -> Result<(), BoardError> {
        let mut problems = Vec::new();
        if self.format != FORMAT {
            problems.push(format!("format is {:?}, expected {FORMAT:?}", self.format));
        }
        if self.schema_version == 0 || self.schema_version > SCHEMA_VERSION {
            problems.push(format!(
                "schema_version {} is not supported (1..={SCHEMA_VERSION})",
                self.schema_version
            ));
        }
        if !is_valid_id(&self.board.id) {
            problems.push(format!("board id {:?} is not a valid id", self.board.id));
        }
        for (name, table) in &self.looks {
            if let Err(errors) =
                crate::looks::from_value("board.toml", name, &toml::Value::Table(table.clone()))
            {
                problems.extend(errors.iter().map(ToString::to_string));
            }
        }
        for (index, device) in self.devices.iter().enumerate() {
            if !is_valid_id(&device.id) {
                problems.push(format!("device {:?}: not a valid id", device.id));
            }
            if self.devices[..index].iter().any(|d| d.id == device.id) {
                problems.push(format!("device {:?}: duplicate id", device.id));
            }
            for entry in device.writers.iter().filter(|e| !is_valid_client_entry(e)) {
                problems.push(format!(
                    "device {:?}: writers entry {entry:?}: '*' is only allowed at the end",
                    device.id
                ));
            }
            match registry.get(&device.driver) {
                Some(entry) => problems.extend(
                    entry
                        .check(device)
                        .into_iter()
                        .map(|p| format!("device {:?}: {p}", device.id)),
                ),
                None => problems.push(format!(
                    "device {:?}: unknown driver {:?}",
                    device.id, device.driver
                )),
            }
        }
        for (index, line) in self.lines.iter().enumerate() {
            if !is_valid_id(&line.name) {
                problems.push(format!("line {:?}: not a valid name", line.name));
            }
            if self.lines[..index].iter().any(|l| l.name == line.name) {
                problems.push(format!("line {:?}: duplicate name", line.name));
            }
            if let Some(safe) = &line.safe
                && !matches!(safe.as_str(), "input" | "low" | "high")
            {
                problems.push(format!(
                    "line {:?}: safe must be input, low or high, not {safe:?}",
                    line.name
                ));
            }
        }
        for (index, pwm) in self.pwms.iter().enumerate() {
            if !is_valid_id(&pwm.name) {
                problems.push(format!("pwm {:?}: not a valid name", pwm.name));
            }
            if self.pwms[..index].iter().any(|p| p.name == pwm.name) {
                problems.push(format!("pwm {:?}: duplicate name", pwm.name));
            }
        }
        for entry in self
            .raw_clients
            .iter()
            .filter(|e| !is_valid_client_entry(e))
        {
            problems.push(format!(
                "raw_clients entry {entry:?}: '*' is only allowed at the end"
            ));
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(BoardError::Invalid(problems))
        }
    }
}
