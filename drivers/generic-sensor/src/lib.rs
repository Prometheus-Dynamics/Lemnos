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
//! Each has a twin for a Modbus RTU device on a UART, `modbus-magnetometer` to
//! `modbus-power-monitor`, which takes the need `uart` where the `generic-*` drivers take `bus`.
//!
//! The contract and its value names are fixed in code. The chip is data: the identity register and
//! its expected value, the start-up writes and waits, which registers hold each channel, byte order,
//! width, position, sign, and the scale and offset to the contract's unit. See [`description`] for
//! the format, and `tests/chips/` for whole files. `accelerometer/1` and `gyroscope/1` batches need
//! a chip's FIFO, which a description does not cover: `read_batch` answers `NOT_SUPPORTED`.
//!
//! A `generic-*` driver reads a chip on I2C (the need `bus`). A `modbus-*` driver reads a Modbus RTU
//! device on a UART (the need `uart`), whose description starts with a `modbus` line: a register
//! read is then a request and its reply on the serial line, checked for its unit, function, length
//! and CRC (see [`modbus`]). A description for the other kind is refused at bind.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `address` (integer, required): the chip's 7-bit I2C address, or its Modbus unit id (1 to
//!   247). This is wiring, so it is not in the description.
//! - `description` (string, required): the description text itself, in every build.
//! - `description_file` (string, `std` builds only): a path to the description file, read once at
//!   bind instead of `description`. A build without a filesystem refuses it by name, and giving both
//!   keys is refused by name. A refused driver binds, its health names the setting, and every read
//!   is `INVALID_ARGUMENT`.
//! - On Modbus only: `baud` (default 9600), `parity` (`"none"`, `"even"` or `"odd"`, default
//!   `"even"`, the Modbus serial line's own default), `stop_bits` (1 or 2, default 1) and
//!   `response_ms` (1 to 500, default 100: how long a reply may take to start, and each pause
//!   within it). Always 8 data bits. On I2C these are refused.
//!
//! Binding does not touch the bus. On Modbus the first read sets the port's frame, and so does the
//! first read after a fault. A read starts the chip when it is not running: it checks the
//! identity register, runs the start-up writes, and honours each `wait` by answering `AGAIN` until
//! the host's clock passes it (the host ticks the driver at the time it asks for). Then it reads
//! each channel.
//!
//! A Modbus chip cannot be read without sending a request, so a host-owned Modbus device is
//! refused at bind with `NOT_SUPPORTED`. On a device the board gives to the host
//! (`owner = "host"`) the operating system's driver has started the chip, and this one only reads it: a read checks the identity register and reads
//! the channels, and the start-up writes and waits are skipped. Nothing is written but register
//! addresses.
//!
//! A wrong identity value is reported as `NOT_SUPPORTED`, with health `Failed`, and the next read
//! checks again. A bus fault puts the chip back to start-up on the next read. The driver never
//! gives up and never switches itself off. It makes no retries of its own, so pacing between
//! attempts (capped at 8 s) is the host's job, done by how often it reads.
//!
//! Untested on board: the description form has run only on the simulated bus. The check is
//! `testing/board-runs/H3a.md`, and over Modbus `testing/board-runs/H3b.md`.

#![no_std]

#[cfg(any(test, feature = "std"))]
extern crate std;

pub mod description;
#[cfg(test)]
mod kinds;
pub mod modbus;
#[cfg(test)]
mod modbus_tests;
#[cfg(test)]
mod tests;

use core::time::Duration;

use description::{Chip, Kind, MAX_CHANNELS, Step, Transport};
use lemnos_contract_accelerometer::Accelerometer;
use lemnos_contract_gyroscope::Gyroscope;
use lemnos_contract_magnetometer::Magnetometer;
use lemnos_contract_power_monitor::{PowerMonitor, PowerReading};
use lemnos_contract_temperature::Temperature;
use lemnos_core::{
    Config, ConfigError, ConfigValue, Description, Health, I2c, Item, Key, Kind as KeyKind,
    Lifecycle, MethodId, Result, Status, Uart, Unit, Vector3, driver, status_of,
};
use lemnos_hal::register::{
    AddressWidth, Endian, I2cRegisters, RegisterBus, RegisterError, decode_value,
};
use lemnos_hal::{ErrorKind, HalError, Parity, StopBits, UartFormat};

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
    /// A Modbus reply that is not the request's.
    Garbled,
    /// A Modbus exception: the device refused the request.
    Exception,
}

impl From<modbus::Fault> for Fault {
    fn from(fault: modbus::Fault) -> Self {
        match fault {
            modbus::Fault::Bus(kind) => Self::Bus(kind),
            modbus::Fault::Garbled => Self::Garbled,
            modbus::Fault::Exception(_) => Self::Exception,
        }
    }
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
            Self::Identity | Self::Exception => Status::NOT_SUPPORTED,
            Self::Garbled => Status::IO,
        }
    }

    fn reason(self) -> &'static str {
        match self {
            Self::Bus(_) => "bus error",
            Self::Identity => "identity mismatch",
            Self::Garbled => "garbled reply",
            Self::Exception => "the device refused a request",
        }
    }
}

/// How the chip is reached, from the description and the settings.
#[derive(Debug, Clone, Copy)]
enum Link {
    I2c {
        address: u8,
    },
    Modbus {
        unit: u8,
        input: bool,
        format: UartFormat,
        wait: Duration,
    },
}

/// What the driver was given: its I2C bus or its UART.
enum Port<'a> {
    I2c(&'a mut I2c),
    Uart(&'a mut Uart),
}

/// Which of the two a driver type takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    I2c,
    Uart,
}

/// What every generic driver shares: the parsed chip and where its start-up is. The bus stays in
/// the driver type, which is what the host binds.
#[derive(Debug)]
pub struct Sensor {
    link: Link,
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
    /// Opens the sensor the settings name: the same [`check`] the host ran before it made the
    /// device, then the chip. Nothing touches the bus.
    fn open(cfg: &Config<'_>, kind: Kind, wire: Wire) -> Result<Self> {
        check(cfg, kind, wire).map_err(|_| Status::INVALID_ARGUMENT)?;
        let chip = load(cfg, kind).map_err(|_| Status::INVALID_ARGUMENT)?;
        Self::new(cfg, chip)
    }

    /// The sensor for a chip the settings give, on the transport the description names. The
    /// transport against the bus, and every setting, were checked by [`check`].
    fn new(cfg: &Config<'_>, chip: Chip) -> Result<Self> {
        let invalid = |_| Status::INVALID_ARGUMENT;
        let link = match chip.transport() {
            Transport::I2c => Link::I2c {
                address: address(cfg).map_err(invalid)?,
            },
            Transport::Modbus { input } => {
                if cfg.host_owned() {
                    return Err(Status::NOT_SUPPORTED);
                }
                Link::Modbus {
                    unit: unit(cfg).map_err(invalid)?,
                    input,
                    format: uart_format(cfg).map_err(invalid)?,
                    wait: response(cfg).map_err(invalid)?,
                }
            }
        };
        Ok(Self {
            link,
            chip,
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
    fn read(&mut self, mut port: Port<'_>) -> Result<[f32; MAX_CHANNELS]> {
        if let Some(due) = self.due {
            if self.now_ns < due {
                return Err(Status::AGAIN);
            }
            self.due = None;
        }
        if self.phase != Phase::Ready {
            match self.start_up(&mut port) {
                Ok(true) => self.phase = Phase::Ready,
                Ok(false) => return Err(Status::AGAIN),
                Err(fault) => return Err(self.fail(fault)),
            }
        }
        self.sample(&mut port).map_err(|fault| self.fail(fault))
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
    fn start_up(&mut self, port: &mut Port<'_>) -> core::result::Result<bool, Fault> {
        if self.next == 0 {
            self.phase = Phase::Starting;
            if let (Link::Modbus { format, .. }, Port::Uart(uart)) = (self.link, &mut *port) {
                uart.configure(format).map_err(Fault::Bus)?;
            }
            if let Some((register, expected)) = self.chip.identity() {
                let mut buf = [0u8; 2];
                let found = match self.link {
                    Link::I2c { .. } => &mut buf[..1],
                    Link::Modbus { .. } => &mut buf[..],
                };
                self.read_registers(port, register, found)?;
                if decode_value(found, Endian::Big) != u32::from(expected) {
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
                    self.write_registers(port, register, data)?;
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

    /// Reads `out.len()` bytes from `register`: consecutive byte registers on I2C, or
    /// `out.len() / 2` Modbus registers, each high byte first.
    fn read_registers(
        &self,
        port: &mut Port<'_>,
        register: u16,
        out: &mut [u8],
    ) -> core::result::Result<(), Fault> {
        match (self.link, port) {
            (Link::I2c { address }, Port::I2c(bus)) => {
                I2cRegisters::new(&mut **bus, address, AddressWidth::Bits8)
                    .read_burst(register, out)?;
            }
            (
                Link::Modbus {
                    unit, input, wait, ..
                },
                Port::Uart(uart),
            ) => {
                let mut link = modbus::Link {
                    uart: &mut ***uart,
                    unit,
                    wait,
                };
                link.read(input, register, out)?;
            }
            _ => return Err(Fault::Bus(ErrorKind::Unsupported)),
        }
        Ok(())
    }

    /// Writes `data` at `register`: consecutive byte registers on I2C, or one Modbus register.
    fn write_registers(
        &self,
        port: &mut Port<'_>,
        register: u16,
        data: &[u8],
    ) -> core::result::Result<(), Fault> {
        match (self.link, port) {
            (Link::I2c { address }, Port::I2c(bus)) => {
                I2cRegisters::new(&mut **bus, address, AddressWidth::Bits8)
                    .write_burst(register, data)?;
            }
            (Link::Modbus { unit, wait, .. }, Port::Uart(uart)) => {
                // The description allows one 16-bit register here.
                let value = u16::from_be_bytes([data[0], data[1]]);
                let mut link = modbus::Link {
                    uart: &mut ***uart,
                    unit,
                    wait,
                };
                link.write(register, value)?;
            }
            _ => return Err(Fault::Bus(ErrorKind::Unsupported)),
        }
        Ok(())
    }

    /// Reads each channel from its registers, in the contract's unit.
    fn sample(&mut self, port: &mut Port<'_>) -> core::result::Result<[f32; MAX_CHANNELS], Fault> {
        let mut out = [0f32; MAX_CHANNELS];
        for (value, channel) in out.iter_mut().zip(self.chip.channels()) {
            let mut buf = [0u8; 4];
            let bytes = &mut buf[..usize::from(channel.bytes)];
            self.read_registers(port, channel.register, bytes)?;
            let raw = match self.link {
                Link::I2c { .. } => decode_value(bytes, channel.endian),
                // Each register is high byte first; `little` puts the low register first.
                Link::Modbus { .. } => {
                    if channel.endian == Endian::Little && bytes.len() == 4 {
                        bytes.rotate_left(2);
                    }
                    decode_value(bytes, Endian::Big)
                }
            };
            *value = channel.units(raw);
        }
        Ok(out)
    }
}

/// The `Lifecycle` every description driver has the same. `$field` is its one need, `bus` (I2C)
/// or `uart` (Modbus), and `$port` the [`Port`] it is.
macro_rules! description_driver {
    ($name:ident, $needs:ident, $kind:expr, $field:ident, $port:ident, $width:expr) => {
        impl Lifecycle for $name {
            type Needs = $needs;
            // Everything this driver does to hardware goes through its needs.
            const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
            // On I2C every register a description names is one byte wide, and its address moves
            // the chip's pointer and nothing else.
            const I2C_REGISTER_ADDRESS: Option<AddressWidth> = $width;

            const TICKS: bool = true;

            fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
                check(cfg, $kind, Wire::$port)
            }

            fn start(needs: $needs, cfg: &Config<'_>) -> Result<Self> {
                let sensor = Sensor::open(cfg, $kind, Wire::$port)?;
                Ok(Self {
                    $field: needs.$field,
                    sensor,
                })
            }

            fn describe(&self, out: &mut Description) {
                describe($kind, &self.sensor, out);
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
                self.sensor.read(Port::$port(&mut self.$field))
            }
        }
    };
}

/// The items each contract's drivers describe.
fn describe(kind: Kind, sensor: &Sensor, out: &mut Description) {
    if let Link::I2c { address } = sensor.link {
        out.set_address(address);
    }
    // The three axes of a vector are one value: their items share a group.
    let group = match kind {
        Kind::Magnetometer => Some("magnetic_field"),
        Kind::Accelerometer => Some("acceleration"),
        Kind::Gyroscope => Some("angular_rate"),
        _ => None,
    };
    let items: &[(&str, Option<&'static str>, Unit)] = match kind {
        Kind::Magnetometer => &[
            ("magnetic_field.x", Some("x"), Unit::Tesla),
            ("magnetic_field.y", Some("y"), Unit::Tesla),
            ("magnetic_field.z", Some("z"), Unit::Tesla),
        ],
        Kind::Accelerometer => &[
            ("acceleration.x", Some("x"), Unit::MetrePerSecondSquared),
            ("acceleration.y", Some("y"), Unit::MetrePerSecondSquared),
            ("acceleration.z", Some("z"), Unit::MetrePerSecondSquared),
        ],
        Kind::Gyroscope => &[
            ("angular_rate.x", Some("x"), Unit::RadianPerSecond),
            ("angular_rate.y", Some("y"), Unit::RadianPerSecond),
            ("angular_rate.z", Some("z"), Unit::RadianPerSecond),
        ],
        Kind::Temperature => &[("temperature", None, Unit::DegreeCelsius)],
        Kind::PowerMonitor => &[
            ("bus_voltage", Some("bus_v"), Unit::Volt),
            ("shunt_voltage", Some("shunt_v"), Unit::Volt),
            ("current", Some("current_a"), Unit::Ampere),
            ("power", Some("power_w"), Unit::Watt),
        ],
    };
    // Each channel's span is what its bits can count, in the contract's unit: the number's smallest
    // and largest count, scaled and offset as the description says. The step is one count.
    let channels = sensor.chip.channels();
    for (index, (name, field, unit)) in items.iter().enumerate() {
        let mut item = Item::value(name, READ).unit(*unit, 0);
        if let Some(channel) = channels.get(index) {
            let (low, high) = count_span(channel.bits, channel.signed);
            let at = |count: f64| count * f64::from(channel.scale) + f64::from(channel.offset);
            let (a, b) = (at(low), at(high));
            item = item
                .real_range(a.min(b), a.max(b))
                .real_step(f64::from(channel.scale).abs());
        }
        if let Some(group) = group {
            item = item.group(group);
        }
        out.add(match field {
            Some(field) => item.field(field),
            None => item,
        });
    }
}

/// The smallest and largest count a number of `bits` takes (`signed` as two's complement).
fn count_span(bits: u8, signed: bool) -> (f64, f64) {
    let bits = u32::from(bits).min(32);
    if signed {
        let half = (1u64 << (bits - 1)) as f64;
        (-half, half - 1.0)
    } else {
        (0.0, ((1u64 << bits) - 1) as f64)
    }
}

/// `magnetometer/1` from a description of an I2C chip.
#[driver("generic-magnetometer", provides = [Magnetometer])]
pub struct GenericMagnetometer {
    #[needs]
    bus: I2c,
    sensor: Sensor,
}

description_driver!(
    GenericMagnetometer,
    GenericMagnetometerNeeds,
    Kind::Magnetometer,
    bus,
    I2c,
    Some(AddressWidth::Bits8)
);

/// `magnetometer/1` from a description of a Modbus RTU device on a UART.
#[driver("modbus-magnetometer", provides = [Magnetometer])]
pub struct ModbusMagnetometer {
    #[needs]
    uart: Uart,
    sensor: Sensor,
}

description_driver!(
    ModbusMagnetometer,
    ModbusMagnetometerNeeds,
    Kind::Magnetometer,
    uart,
    Uart,
    None
);

/// `accelerometer/1` from a description of an I2C chip.
#[driver("generic-accelerometer", provides = [Accelerometer])]
pub struct GenericAccelerometer {
    #[needs]
    bus: I2c,
    sensor: Sensor,
}

description_driver!(
    GenericAccelerometer,
    GenericAccelerometerNeeds,
    Kind::Accelerometer,
    bus,
    I2c,
    Some(AddressWidth::Bits8)
);

/// `accelerometer/1` from a description of a Modbus RTU device on a UART.
#[driver("modbus-accelerometer", provides = [Accelerometer])]
pub struct ModbusAccelerometer {
    #[needs]
    uart: Uart,
    sensor: Sensor,
}

description_driver!(
    ModbusAccelerometer,
    ModbusAccelerometerNeeds,
    Kind::Accelerometer,
    uart,
    Uart,
    None
);

/// `gyroscope/1` from a description of an I2C chip.
#[driver("generic-gyroscope", provides = [Gyroscope])]
pub struct GenericGyroscope {
    #[needs]
    bus: I2c,
    sensor: Sensor,
}

description_driver!(
    GenericGyroscope,
    GenericGyroscopeNeeds,
    Kind::Gyroscope,
    bus,
    I2c,
    Some(AddressWidth::Bits8)
);

/// `gyroscope/1` from a description of a Modbus RTU device on a UART.
#[driver("modbus-gyroscope", provides = [Gyroscope])]
pub struct ModbusGyroscope {
    #[needs]
    uart: Uart,
    sensor: Sensor,
}

description_driver!(
    ModbusGyroscope,
    ModbusGyroscopeNeeds,
    Kind::Gyroscope,
    uart,
    Uart,
    None
);

/// `temperature/1` from a description of an I2C chip.
#[driver("generic-temperature", provides = [Temperature])]
pub struct GenericTemperature {
    #[needs]
    bus: I2c,
    sensor: Sensor,
}

description_driver!(
    GenericTemperature,
    GenericTemperatureNeeds,
    Kind::Temperature,
    bus,
    I2c,
    Some(AddressWidth::Bits8)
);

/// `temperature/1` from a description of a Modbus RTU device on a UART.
#[driver("modbus-temperature", provides = [Temperature])]
pub struct ModbusTemperature {
    #[needs]
    uart: Uart,
    sensor: Sensor,
}

description_driver!(
    ModbusTemperature,
    ModbusTemperatureNeeds,
    Kind::Temperature,
    uart,
    Uart,
    None
);

/// `power-monitor/1` from a description of an I2C chip.
#[driver("generic-power-monitor", provides = [PowerMonitor])]
pub struct GenericPowerMonitor {
    #[needs]
    bus: I2c,
    sensor: Sensor,
}

description_driver!(
    GenericPowerMonitor,
    GenericPowerMonitorNeeds,
    Kind::PowerMonitor,
    bus,
    I2c,
    Some(AddressWidth::Bits8)
);

/// `power-monitor/1` from a description of a Modbus RTU device on a UART.
#[driver("modbus-power-monitor", provides = [PowerMonitor])]
pub struct ModbusPowerMonitor {
    #[needs]
    uart: Uart,
    sensor: Sensor,
}

description_driver!(
    ModbusPowerMonitor,
    ModbusPowerMonitorNeeds,
    Kind::PowerMonitor,
    uart,
    Uart,
    None
);

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

impl Magnetometer for ModbusMagnetometer {
    fn read(&mut self) -> Result<Vector3> {
        self.channels().map(vector)
    }
}

impl Accelerometer for ModbusAccelerometer {
    fn read(&mut self) -> Result<Vector3> {
        self.channels().map(vector)
    }

    /// A description has no FIFO, so there are no batches.
    fn read_batch(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
}

impl Gyroscope for ModbusGyroscope {
    fn read(&mut self) -> Result<Vector3> {
        self.channels().map(vector)
    }

    /// A description has no FIFO, so there are no batches.
    fn read_batch(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
}

impl Temperature for ModbusTemperature {
    fn read(&mut self) -> Result<f32> {
        self.channels().map(|[celsius, ..]| celsius)
    }
}

impl PowerMonitor for ModbusPowerMonitor {
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

/// The settings this driver takes, by key and kind. `address` is the chip's I2C address or the
/// Modbus unit id; the Modbus frame keys are read only on a Modbus driver.
const KEYS: &[Key] = &[
    Key::required("address", KeyKind::Int),
    Key::optional("description", KeyKind::Str),
    Key::optional("description_file", KeyKind::Str),
    Key::optional("baud", KeyKind::Int),
    Key::optional("parity", KeyKind::Str),
    Key::optional("stop_bits", KeyKind::Int),
    Key::optional("response_ms", KeyKind::Int),
];

/// The Modbus frame keys: an I2C chip takes none of them.
const MODBUS_KEYS: &[&str] = &["baud", "parity", "stop_bits", "response_ms"];

/// The reason both `description` and `description_file` are given.
const BOTH_KEYS: &str = "is given with description: give one of them, not both";
/// The reason `description_file` is given to a build without a filesystem.
#[cfg(not(feature = "std"))]
const NO_FILESYSTEM: &str = "needs a build with a filesystem: give the chip's text as description";

/// The settings a generic driver of `wire` and `kind` takes, checked with no bus and no hardware:
/// the key names and kinds, the chip the description names, and the settings the chip's transport
/// needs. The host runs this before it makes the device, and `start` runs it again through
/// [`Sensor::open`], so the two cannot drift.
fn check<'a>(
    cfg: &Config<'a>,
    kind: Kind,
    wire: Wire,
) -> core::result::Result<(), ConfigError<'a>> {
    cfg.check_keys(KEYS)?;
    let chip = load(cfg, kind)?;
    match (chip.transport(), wire) {
        (Transport::I2c, Wire::I2c) => {
            for key in MODBUS_KEYS {
                if cfg.get(key).is_some() {
                    return Err(ConfigError::new(
                        key,
                        "is a Modbus setting, and this chip is on I2C",
                    ));
                }
            }
            address(cfg).map(drop)
        }
        (Transport::Modbus { .. }, Wire::Uart) => {
            unit(cfg).map(drop)?;
            uart_format(cfg).map(drop)?;
            response(cfg).map(drop)
        }
        (Transport::I2c, Wire::Uart) => Err(ConfigError::new(
            "description",
            "is an I2C chip's description, and this driver reads Modbus chips",
        )),
        (Transport::Modbus { .. }, Wire::I2c) => Err(ConfigError::new(
            "description",
            "is a Modbus chip's description, and this driver reads I2C chips",
        )),
    }
}

/// The chip the settings name, for `kind`. `description` is the chip's text in every build.
/// `description_file` is a path, which only a build with a filesystem can read.
fn load<'a>(cfg: &Config<'a>, kind: Kind) -> core::result::Result<Chip, ConfigError<'a>> {
    match (cfg.get("description"), cfg.get("description_file")) {
        (Some(_), Some(_)) => Err(ConfigError::new("description_file", BOTH_KEYS)),
        (Some(ConfigValue::Str(text)), None) => Chip::parse(text, kind).map_err(|_| {
            ConfigError::new(
                "description",
                "is not a chip description: a file's path goes in description_file",
            )
        }),
        (None, Some(ConfigValue::Str(path))) => file(path, kind),
        (None, None) => Err(ConfigError::new(
            "description",
            "is required: give the chip's text, or its file under description_file",
        )),
        _ => Err(ConfigError::new("description", "must be text")),
    }
}

#[cfg(not(feature = "std"))]
fn file<'a>(_path: &str, _kind: Kind) -> core::result::Result<Chip, ConfigError<'a>> {
    Err(ConfigError::new("description_file", NO_FILESYSTEM))
}

#[cfg(feature = "std")]
fn file<'a>(path: &str, kind: Kind) -> core::result::Result<Chip, ConfigError<'a>> {
    let text = std::fs::read_to_string(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            ConfigError::new("description_file", "names no file: check the path")
        }
        _ => ConfigError::new("description_file", "cannot be read"),
    })?;
    Chip::parse(&text, kind)
        .map_err(|_| ConfigError::new("description_file", "is not a chip description file"))
}

/// The `address` setting: a 7-bit I2C address that is not reserved.
fn address<'a>(cfg: &Config<'a>) -> core::result::Result<u8, ConfigError<'a>> {
    match cfg.int("address") {
        Some(address) if (0x08..=0x77).contains(&address) => Ok(address as u8),
        _ => Err(ConfigError::new(
            "address",
            "must be a 7-bit I2C address, 0x08 to 0x77",
        )),
    }
}

/// The `address` setting on Modbus: a unit id, 1 to 247 (0 is broadcast, which nothing answers).
fn unit<'a>(cfg: &Config<'a>) -> core::result::Result<u8, ConfigError<'a>> {
    match cfg.int("address") {
        Some(unit) if (1..=247).contains(&unit) => Ok(unit as u8),
        _ => Err(ConfigError::new(
            "address",
            "must be a Modbus unit id, 1 to 247",
        )),
    }
}

/// The port's frame on Modbus: `baud`, `parity` and `stop_bits`.
fn uart_format<'a>(cfg: &Config<'a>) -> core::result::Result<UartFormat, ConfigError<'a>> {
    let baud = match cfg.get("baud") {
        None => 9600,
        Some(ConfigValue::Int(baud)) => u32::try_from(baud)
            .ok()
            .filter(|baud| *baud > 0)
            .ok_or(ConfigError::new("baud", "must be a whole number above 0"))?,
        Some(_) => return Err(ConfigError::new("baud", "must be a whole number")),
    };
    let parity = match cfg.get("parity") {
        None | Some(ConfigValue::Str("even")) => Parity::Even,
        Some(ConfigValue::Str("none")) => Parity::None,
        Some(ConfigValue::Str("odd")) => Parity::Odd,
        Some(_) => {
            return Err(ConfigError::new(
                "parity",
                "must be \"none\", \"even\" or \"odd\"",
            ));
        }
    };
    let stop_bits = match cfg.get("stop_bits") {
        None | Some(ConfigValue::Int(1)) => StopBits::One,
        Some(ConfigValue::Int(2)) => StopBits::Two,
        Some(_) => return Err(ConfigError::new("stop_bits", "must be 1 or 2")),
    };
    Ok(UartFormat {
        baud,
        parity,
        stop_bits,
    })
}

/// The `response_ms` setting on Modbus.
fn response<'a>(cfg: &Config<'a>) -> core::result::Result<Duration, ConfigError<'a>> {
    match cfg.get("response_ms") {
        None => Ok(Duration::from_millis(100)),
        Some(ConfigValue::Int(ms)) if (1..=500).contains(&ms) => {
            Ok(Duration::from_millis(ms as u64))
        }
        Some(_) => Err(ConfigError::new("response_ms", "must be 1 to 500")),
    }
}
