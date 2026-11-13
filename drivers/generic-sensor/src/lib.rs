//! Register-based sensors described by a file, with no code per chip. One driver per contract:
//!
//! | Driver | Contract | Channels |
//! |---|---|---|
//! | `generic-magnetometer` | `magnetometer/1` | `axis` x, y, z in tesla |
//! | `generic-accelerometer` | `accelerometer/1` | `axis` x, y, z in m/s² |
//! | `generic-gyroscope` | `gyroscope/1` | `axis` x, y, z in rad/s |
//! | `generic-temperature` | `temperature/1` | `value temperature` in °C |
//! | `generic-power-monitor` | `power-monitor/1` | `value` `bus_v`, `shunt_v` (V), `current_a` (A), `power_w` (W) |
//!
//! The contract and its value names are fixed in code. The chip is data: the identity register and
//! its expected value, the start-up writes and waits, which registers hold each channel, byte order,
//! width, position, sign, and the scale and offset to the contract's unit. See [`description`] for
//! the format, and `tests/chips/` for whole files. `accelerometer/1` and `gyroscope/1` batches need
//! a chip's FIFO, which a description does not cover: `read_batch` answers `NOT_SUPPORTED`.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `address` (integer, required): the chip's 7-bit I2C address. This is wiring, so it is not in
//!   the description.
//! - `description` (string, required): the description text itself. With the `std` feature, a path
//!   to the description file, read once at bind.
//!
//! Binding does not touch the bus. A read starts the chip when it is not running: it checks the
//! identity register, runs the start-up writes, and honours each `wait` by answering `AGAIN` until
//! the host's clock passes it (the host ticks the driver at the time it asks for). Then it reads
//! each channel.
//!
//! On a device the board gives to the host (`owner = "host"`) the operating system's driver has
//! started the chip, and this one only reads it: a read checks the identity register and reads
//! the channels, and the start-up writes and waits are skipped. Nothing is written but register
//! addresses.
//!
//! A wrong identity value is reported as `NOT_SUPPORTED`, with health `Failed`, and the next read
//! checks again. A bus fault puts the chip back to start-up on the next read. The driver never
//! gives up and never switches itself off. It makes no retries of its own, so pacing between
//! attempts (capped at 8 s) is the host's job, done by how often it reads.
//!
//! Untested on board: the description form has run only on the simulated bus. The check is
//! `testing/board-runs/H3a.md`.

#![no_std]

#[cfg(feature = "std")]
extern crate std;

pub mod description;
#[cfg(test)]
mod kinds;
#[cfg(test)]
mod tests;

use description::{Chip, Kind, MAX_CHANNELS, Step};
use lemnos_contract_accelerometer::Accelerometer;
use lemnos_contract_gyroscope::Gyroscope;
use lemnos_contract_magnetometer::Magnetometer;
use lemnos_contract_power_monitor::{PowerMonitor, PowerReading};
use lemnos_contract_temperature::Temperature;
use lemnos_core::{
    Config, ConfigValue, Description, Health, I2c, Item, Lifecycle, MethodId, Result, Status, Unit,
    Vector3, driver, status_of,
};
use lemnos_hal::register::{
    AddressWidth, Endian, I2cRegisters, RegisterBus, RegisterError, decode_value,
};
use lemnos_hal::{ErrorKind, HalError};

const READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// Where the chip is in its start-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Not running: never started, or the last operation failed.
    Off,
    /// Running the start-up steps; a `wait` may hold it.
    Starting,
    /// Measuring.
    Ready,
}

/// Why the chip is not measuring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    Bus(ErrorKind),
    Identity,
}

impl From<RegisterError<ErrorKind>> for Fault {
    fn from(error: RegisterError<ErrorKind>) -> Self {
        Self::Bus(error.kind())
    }
}

impl Fault {
    fn status(self) -> Status {
        match self {
            Self::Bus(kind) => status_of(kind),
            Self::Identity => Status::NOT_SUPPORTED,
        }
    }

    fn reason(self) -> &'static str {
        match self {
            Self::Bus(_) => "bus error",
            Self::Identity => "identity mismatch",
        }
    }
}

/// What every generic driver shares: the parsed chip and where its start-up is. The bus stays in
/// the driver type, which is what the host binds.
#[derive(Debug)]
pub struct Sensor {
    address: u8,
    chip: Chip,
    phase: Phase,
    /// The next start-up step, while starting.
    next: usize,
    /// The host's clock, from the last tick.
    now_ns: u64,
    /// When a `wait` lets the start-up go on.
    due: Option<u64>,
    /// Why the chip is not measuring, while it is not.
    fault: &'static str,
    /// The operating system's driver runs the chip: read it, never start it.
    host_owned: bool,
}

impl Sensor {
    /// Reads the settings and the description for `kind`. Nothing touches the bus.
    fn new(cfg: &Config<'_>, kind: Kind) -> Result<Self> {
        Ok(Self {
            address: address(cfg)?,
            chip: chip(cfg, kind)?,
            phase: Phase::Off,
            next: 0,
            now_ns: 0,
            due: None,
            fault: "not started",
            host_owned: cfg.host_owned(),
        })
    }

    fn health(&self) -> Health {
        match self.phase {
            Phase::Ready => Health::Ok,
            Phase::Starting => Health::Degraded("starting"),
            Phase::Off => Health::Failed(self.fault),
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.now_ns = now_ns;
        self.due.filter(|due| *due > now_ns)
    }

    /// Starts the chip if it is not running, then reads every channel in the contract's unit, in
    /// the order of [`Kind::channels`].
    fn read(&mut self, bus: &mut I2c) -> Result<[f32; MAX_CHANNELS]> {
        if let Some(due) = self.due {
            if self.now_ns < due {
                return Err(Status::AGAIN);
            }
            self.due = None;
        }
        if self.phase != Phase::Ready {
            match self.start_up(bus) {
                Ok(true) => self.phase = Phase::Ready,
                Ok(false) => return Err(Status::AGAIN),
                Err(fault) => return Err(self.fail(fault)),
            }
        }
        self.sample(bus).map_err(|fault| self.fail(fault))
    }

    /// Records a fault, so the next read starts the chip again.
    fn fail(&mut self, fault: Fault) -> Status {
        self.phase = Phase::Off;
        self.next = 0;
        self.due = None;
        self.fault = fault.reason();
        fault.status()
    }

    /// Runs start-up steps from `next`. `Ok(true)` when all have run, `Ok(false)` when a `wait`
    /// holds the start-up.
    fn start_up(&mut self, bus: &mut I2c) -> core::result::Result<bool, Fault> {
        let mut regs = I2cRegisters::new(bus, self.address, AddressWidth::Bits8);
        if self.next == 0 {
            self.phase = Phase::Starting;
            if let Some((register, expected)) = self.chip.identity() {
                let found = regs.read(u16::from(register), 1)?;
                if found != u32::from(expected) {
                    return Err(Fault::Identity);
                }
            }
            // Its owner started it: there is nothing to write or wait for.
            if self.host_owned {
                return Ok(true);
            }
        }
        while self.next < self.chip.steps().len() {
            let step = self.chip.steps()[self.next];
            self.next += 1;
            match step {
                Step::Write {
                    register,
                    value,
                    bytes,
                    endian,
                } => {
                    let all = match endian {
                        Endian::Big => value.to_be_bytes(),
                        Endian::Little => value.to_le_bytes(),
                    };
                    let n = usize::from(bytes);
                    let data = match endian {
                        Endian::Big => &all[4 - n..],
                        Endian::Little => &all[..n],
                    };
                    regs.write_burst(u16::from(register), data)?
                }
                Step::Wait { millis } => {
                    self.due = Some(
                        self.now_ns
                            .saturating_add(u64::from(millis).saturating_mul(1_000_000)),
                    );
                    return Ok(false);
                }
            }
        }
        self.next = 0;
        Ok(true)
    }

    /// Reads each channel from its registers, in the contract's unit.
    fn sample(&mut self, bus: &mut I2c) -> core::result::Result<[f32; MAX_CHANNELS], Fault> {
        let mut regs = I2cRegisters::new(bus, self.address, AddressWidth::Bits8);
        let mut out = [0f32; MAX_CHANNELS];
        for (value, channel) in out.iter_mut().zip(self.chip.channels()) {
            let mut buf = [0u8; 4];
            let bytes = &mut buf[..usize::from(channel.bytes)];
            regs.read_burst(u16::from(channel.register), bytes)?;
            *value = channel.units(decode_value(bytes, channel.endian));
        }
        Ok(out)
    }
}

/// The `Lifecycle` every generic driver has the same, for a type with the fields `bus` (its need)
/// and `sensor`.
macro_rules! generic_driver {
    (
        $name:ident, $needs:ident, $kind:expr,
        describe: [$(($item:literal, $field:expr, $unit:expr)),* $(,)?]
    ) => {
        impl Lifecycle for $name {
            type Needs = $needs;
            // Everything this driver does to hardware goes through its needs.
            const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
            // Every register a description names is one byte wide, and its address moves the
            // chip's pointer and nothing else.
            const I2C_REGISTER_ADDRESS: Option<AddressWidth> = Some(AddressWidth::Bits8);

            const TICKS: bool = true;

            fn start(needs: $needs, cfg: &Config<'_>) -> Result<Self> {
                Ok(Self {
                    bus: needs.bus,
                    sensor: Sensor::new(cfg, $kind)?,
                })
            }

            fn describe(&self, out: &mut Description) {
                $(
                    let item = Item::value($item, READ).unit($unit, 0);
                    let item = match $field {
                        Some(field) => item.field(field),
                        None => item,
                    };
                    out.add(item);
                )*
            }

            fn health(&self) -> Health {
                self.sensor.health()
            }

            fn tick(&mut self, now_ns: u64) -> Option<u64> {
                self.sensor.tick(now_ns)
            }
        }

        impl $name {
            /// Every channel, in the contract's unit.
            fn channels(&mut self) -> Result<[f32; MAX_CHANNELS]> {
                self.sensor.read(&mut self.bus)
            }
        }
    };
}

/// `magnetometer/1` from a description.
#[driver("generic-magnetometer", provides = [Magnetometer])]
pub struct GenericMagnetometer {
    #[needs]
    bus: I2c,
    sensor: Sensor,
}

generic_driver! {
    GenericMagnetometer, GenericMagnetometerNeeds, Kind::Magnetometer,
    describe: [
        ("magnetic_field.x", Some("x"), Unit::Tesla),
        ("magnetic_field.y", Some("y"), Unit::Tesla),
        ("magnetic_field.z", Some("z"), Unit::Tesla),
    ]
}

/// `accelerometer/1` from a description.
#[driver("generic-accelerometer", provides = [Accelerometer])]
pub struct GenericAccelerometer {
    #[needs]
    bus: I2c,
    sensor: Sensor,
}

generic_driver! {
    GenericAccelerometer, GenericAccelerometerNeeds, Kind::Accelerometer,
    describe: [
        ("acceleration.x", Some("x"), Unit::MetrePerSecondSquared),
        ("acceleration.y", Some("y"), Unit::MetrePerSecondSquared),
        ("acceleration.z", Some("z"), Unit::MetrePerSecondSquared),
    ]
}

/// `gyroscope/1` from a description.
#[driver("generic-gyroscope", provides = [Gyroscope])]
pub struct GenericGyroscope {
    #[needs]
    bus: I2c,
    sensor: Sensor,
}

generic_driver! {
    GenericGyroscope, GenericGyroscopeNeeds, Kind::Gyroscope,
    describe: [
        ("angular_rate.x", Some("x"), Unit::RadianPerSecond),
        ("angular_rate.y", Some("y"), Unit::RadianPerSecond),
        ("angular_rate.z", Some("z"), Unit::RadianPerSecond),
    ]
}

/// `temperature/1` from a description.
#[driver("generic-temperature", provides = [Temperature])]
pub struct GenericTemperature {
    #[needs]
    bus: I2c,
    sensor: Sensor,
}

generic_driver! {
    GenericTemperature, GenericTemperatureNeeds, Kind::Temperature,
    describe: [("temperature", None::<&'static str>, Unit::DegreeCelsius)]
}

/// `power-monitor/1` from a description.
#[driver("generic-power-monitor", provides = [PowerMonitor])]
pub struct GenericPowerMonitor {
    #[needs]
    bus: I2c,
    sensor: Sensor,
}

generic_driver! {
    GenericPowerMonitor, GenericPowerMonitorNeeds, Kind::PowerMonitor,
    describe: [
        ("bus_voltage", Some("bus_v"), Unit::Volt),
        ("shunt_voltage", Some("shunt_v"), Unit::Volt),
        ("current", Some("current_a"), Unit::Ampere),
        ("power", Some("power_w"), Unit::Watt),
    ]
}

fn vector([x, y, z, _]: [f32; MAX_CHANNELS]) -> Vector3 {
    Vector3 { x, y, z }
}

impl Magnetometer for GenericMagnetometer {
    fn read(&mut self) -> Result<Vector3> {
        self.channels().map(vector)
    }
}

impl Accelerometer for GenericAccelerometer {
    fn read(&mut self) -> Result<Vector3> {
        self.channels().map(vector)
    }

    /// A description has no FIFO, so there are no batches.
    fn read_batch(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
}

impl Gyroscope for GenericGyroscope {
    fn read(&mut self) -> Result<Vector3> {
        self.channels().map(vector)
    }

    /// A description has no FIFO, so there are no batches.
    fn read_batch(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
}

impl Temperature for GenericTemperature {
    fn read(&mut self) -> Result<f32> {
        self.channels().map(|[celsius, ..]| celsius)
    }
}

impl PowerMonitor for GenericPowerMonitor {
    fn read(&mut self) -> Result<PowerReading> {
        self.channels()
            .map(|[bus_v, shunt_v, current_a, power_w]| PowerReading {
                bus_v,
                shunt_v,
                current_a,
                power_w,
            })
    }
}

/// The `address` setting: a 7-bit address that is not reserved.
fn address(cfg: &Config<'_>) -> Result<u8> {
    match cfg.int("address") {
        Some(address) if (0x08..=0x77).contains(&address) => Ok(address as u8),
        _ => Err(Status::INVALID_ARGUMENT),
    }
}

/// The `description` setting, parsed for `kind`. A bad file is `INVALID_ARGUMENT`.
fn chip(cfg: &Config<'_>, kind: Kind) -> Result<Chip> {
    match cfg.get("description") {
        Some(ConfigValue::Str(value)) => load(value, kind),
        _ => Err(Status::INVALID_ARGUMENT),
    }
}

#[cfg(not(feature = "std"))]
fn load(text: &str, kind: Kind) -> Result<Chip> {
    Chip::parse(text, kind).map_err(|_| Status::INVALID_ARGUMENT)
}

#[cfg(feature = "std")]
fn load(path: &str, kind: Kind) -> Result<Chip> {
    let text = std::fs::read_to_string(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => Status::NOT_FOUND,
        _ => Status::IO,
    })?;
    Chip::parse(&text, kind).map_err(|_| Status::INVALID_ARGUMENT)
}
