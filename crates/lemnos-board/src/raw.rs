//! Raw access for hosts that hand buses and lines to clients (`lemnosd`):
//! the resources a board's devices own, and type-erased claimed lines, PWM
//! channels and SPI devices.

use crate::schema::{BoardDefinition, BusRef, ConfigValue, DeviceSpec};
use crate::{DriverRegistry, Interface};
use lemnos_drivers_linux::SysRoot;
use lemnos_hal::ErrorKind;
use lemnos_hal::raw::{
    Edge, LineConfig, PwmConfig, RawLine, RawPwm, RawSpi, SafeState, SpiSegment,
};
use std::os::fd::RawFd;

/// A bus address, line or channel a client may ask for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Resource {
    I2c {
        bus: u32,
        address: u16,
    },
    Spi {
        bus: u32,
        chip_select: u16,
    },
    /// `chip` as the host identifies it ([`crate::Buses::line_chip_id`]).
    Line {
        chip: String,
        offset: u32,
    },
    Pwm {
        chip: u32,
        channel: u32,
    },
}

/// Which board device owns a resource, and which clients it lets make raw
/// transactions on it (I2C and SPI only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub device: String,
    pub raw: Vec<String>,
}

/// The resources `board`'s devices own: each I2C device's address (plus any
/// integer `*_address` setting, such as the BMI088's `gyro_address`), each
/// SPI device's chip select, and the line of each `gpio-*` device. `chip_id`
/// maps a chip name or label to the host's identity for it. Devices whose
/// bus cannot be resolved own nothing until it can.
pub fn owned_resources(
    board: &BoardDefinition,
    registry: &DriverRegistry,
    sys: &SysRoot,
    chip_id: &dyn Fn(&str) -> String,
) -> Vec<(Resource, Owner)> {
    let mut owned = Vec::new();
    for spec in &board.devices {
        let owner = Owner {
            device: spec.id.clone(),
            raw: spec.raw.clone(),
        };
        let entry = registry.get(&spec.driver);
        match &spec.bus {
            Some(BusRef::Spi { bus, chip_select }) => owned.push((
                Resource::Spi {
                    bus: *bus,
                    chip_select: *chip_select,
                },
                owner.clone(),
            )),
            Some(bus) if bus.is_i2c() => {
                let Some(Ok(number)) = bus.i2c_bus(sys) else {
                    continue;
                };
                let default = entry.and_then(|e| e.default_address);
                for address in spec
                    .address
                    .or(default)
                    .into_iter()
                    .chain(extra_addresses(spec))
                {
                    owned.push((
                        Resource::I2c {
                            bus: number,
                            address,
                        },
                        owner.clone(),
                    ));
                }
            }
            _ => {}
        }
        if entry.is_some_and(|e| e.interface == Interface::Platform)
            && spec.driver.starts_with("gpio-")
            && let (Some(chip), Some(line)) = (
                spec.config.get("chip").and_then(ConfigValue::as_str),
                spec.config
                    .get("line")
                    .and_then(ConfigValue::as_i64)
                    .and_then(|l| u32::try_from(l).ok()),
            )
        {
            owned.push((
                Resource::Line {
                    chip: chip_id(chip),
                    offset: line,
                },
                owner.clone(),
            ));
        }
        // A power switch's fault input is the device's too.
        if spec.driver == "gpio-power-switch"
            && let Some(line) = spec
                .config
                .get("fault_line")
                .and_then(ConfigValue::as_i64)
                .and_then(|l| u32::try_from(l).ok())
        {
            let chip = spec
                .config
                .get("fault_chip")
                .or_else(|| spec.config.get("chip"))
                .and_then(ConfigValue::as_str)
                .unwrap_or_default();
            owned.push((
                Resource::Line {
                    chip: chip_id(chip),
                    offset: line,
                },
                owner,
            ));
        }
    }
    owned
}

/// Integer settings named `*_address`: further addresses a device answers on.
fn extra_addresses(spec: &DeviceSpec) -> impl Iterator<Item = u16> + '_ {
    spec.config.iter().filter_map(|(key, value)| {
        key.ends_with("_address")
            .then(|| value.as_i64().and_then(|v| u16::try_from(v).ok()))
            .flatten()
    })
}

/// The state the board line `chip`/`offset` (`chip` in the host's identity)
/// goes back to when a claim ends, if the board names it with one.
pub fn line_safe_state(
    board: &BoardDefinition,
    chip: &str,
    offset: u32,
    chip_id: &dyn Fn(&str) -> String,
) -> Option<SafeState> {
    board
        .lines
        .iter()
        .find(|l| l.line == offset && chip_id(&l.chip) == chip)
        .and_then(|l| l.safe.as_deref().and_then(SafeState::parse))
}

/// A claimed line of any type, with the file descriptor that becomes
/// readable when an edge is pending (if it has one).
pub struct DynLine {
    line: Box<dyn RawLine + Send>,
    fd: Option<RawFd>,
}

impl DynLine {
    /// `fd` must stay valid as long as `line` lives (it is `line`'s own).
    pub fn new(line: impl RawLine + Send + 'static, fd: Option<RawFd>) -> Self {
        Self {
            line: Box::new(line),
            fd,
        }
    }

    pub fn fd(&self) -> Option<RawFd> {
        self.fd
    }
}

impl RawLine for DynLine {
    fn configure(&mut self, config: &LineConfig) -> Result<(), ErrorKind> {
        self.line.configure(config)
    }
    fn get(&mut self) -> Result<bool, ErrorKind> {
        self.line.get()
    }
    fn set(&mut self, value: bool) -> Result<(), ErrorKind> {
        self.line.set(value)
    }
    fn read_edge(&mut self) -> Result<Option<Edge>, ErrorKind> {
        self.line.read_edge()
    }
}

impl std::fmt::Debug for DynLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynLine").field("fd", &self.fd).finish()
    }
}

/// A claimed PWM channel of any type.
pub struct DynPwm(Box<dyn RawPwm + Send>);

impl DynPwm {
    pub fn new(pwm: impl RawPwm + Send + 'static) -> Self {
        Self(Box::new(pwm))
    }
}

impl RawPwm for DynPwm {
    fn configure(&mut self, config: &PwmConfig) -> Result<(), ErrorKind> {
        self.0.configure(config)
    }
    fn config(&mut self) -> Result<PwmConfig, ErrorKind> {
        self.0.config()
    }
}

impl std::fmt::Debug for DynPwm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DynPwm")
    }
}

/// An SPI device of any type.
pub struct DynSpi(Box<dyn RawSpi + Send>);

impl DynSpi {
    pub fn new(spi: impl RawSpi + Send + 'static) -> Self {
        Self(Box::new(spi))
    }
}

impl RawSpi for DynSpi {
    fn transfer(&mut self, segments: &mut [SpiSegment<'_>]) -> Result<(), ErrorKind> {
        self.0.transfer(segments)
    }
}

impl std::fmt::Debug for DynSpi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DynSpi")
    }
}
