//! A register-based sensor described by a file, served as `magnetometer/1`, with no code per chip.
//!
//! The contract and its value names are fixed in code. The chip is data: the identity register and
//! its expected value, the start-up writes and waits, which registers hold each axis, byte order,
//! width, sign, and the scale to tesla per count. See [`description`] for the format, and
//! `tests/chips/sim-mag.chip` for a whole file.
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
//! each axis.
//!
//! On a device the board gives to the host (`owner = "host"`) the operating system's driver has
//! started the chip, and this one only reads it: a read checks the identity register and reads
//! the axes, and the start-up writes and waits are skipped. Nothing is written but register
//! addresses.
//!
//! A wrong identity value is reported as `NOT_SUPPORTED`, with health `Failed`, and the next read
//! checks again. A bus fault puts the chip back to start-up on the next read. The driver never
//! gives up and never switches itself off. It makes no retries of its own, so pacing between
//! attempts (capped at 8 s) is the host's job, done by how often it reads.

#![no_std]

#[cfg(feature = "std")]
extern crate std;

pub mod description;
#[cfg(test)]
mod tests;

use description::{Chip, Step};
use lemnos_contract_magnetometer::Magnetometer;
use lemnos_core::{
    Config, ConfigValue, Description, Health, I2c, Item, Lifecycle, MethodId, Result, Status, Unit,
    Vector3, driver, status_of,
};
use lemnos_hal::register::{AddressWidth, I2cRegisters, RegisterBus, RegisterError, decode_value};
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

#[driver("generic-sensor", provides = [Magnetometer])]
pub struct GenericSensor {
    #[needs]
    bus: I2c,
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

impl Lifecycle for GenericSensor {
    type Needs = GenericSensorNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
    // Every register a description names is one byte wide, and its address moves the chip's
    // pointer and nothing else.
    const I2C_REGISTER_ADDRESS: Option<AddressWidth> = Some(AddressWidth::Bits8);

    const TICKS: bool = true;

    fn start(needs: GenericSensorNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            bus: needs.bus,
            address: address(cfg)?,
            chip: chip(cfg)?,
            phase: Phase::Off,
            next: 0,
            now_ns: 0,
            due: None,
            fault: "not started",
            host_owned: cfg.host_owned(),
        })
    }

    fn describe(&self, out: &mut Description) {
        for (name, field) in [
            ("magnetic_field.x", "x"),
            ("magnetic_field.y", "y"),
            ("magnetic_field.z", "z"),
        ] {
            out.add(Item::value(name, READ).unit(Unit::Tesla, 0).field(field));
        }
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
}

impl Magnetometer for GenericSensor {
    fn read(&mut self) -> Result<Vector3> {
        if let Some(due) = self.due {
            if self.now_ns < due {
                return Err(Status::AGAIN);
            }
            self.due = None;
        }
        if self.phase != Phase::Ready {
            match self.start_up() {
                Ok(true) => self.phase = Phase::Ready,
                Ok(false) => return Err(Status::AGAIN),
                Err(fault) => return Err(self.fail(fault)),
            }
        }
        self.sample().map_err(|fault| self.fail(fault))
    }
}

impl GenericSensor {
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
    fn start_up(&mut self) -> core::result::Result<bool, Fault> {
        let Self {
            bus,
            address,
            chip,
            phase,
            next,
            due,
            now_ns,
            host_owned,
            ..
        } = self;
        let mut regs = I2cRegisters::new(bus, *address, AddressWidth::Bits8);
        if *next == 0 {
            *phase = Phase::Starting;
            if let Some((register, expected)) = chip.identity() {
                let found = regs.read(u16::from(register), 1)?;
                if found != u32::from(expected) {
                    return Err(Fault::Identity);
                }
            }
            // Its owner started it: there is nothing to write or wait for.
            if *host_owned {
                return Ok(true);
            }
        }
        while *next < chip.steps().len() {
            let step = chip.steps()[*next];
            *next += 1;
            match step {
                Step::Write { register, value } => {
                    regs.write(u16::from(register), 1, u32::from(value))?
                }
                Step::Wait { millis } => {
                    *due = Some(now_ns.saturating_add(u64::from(millis) * 1_000_000));
                    return Ok(false);
                }
            }
        }
        *next = 0;
        Ok(true)
    }

    /// Reads each axis from its registers, in the contract's unit.
    fn sample(&mut self) -> core::result::Result<Vector3, Fault> {
        let mut regs = I2cRegisters::new(&mut self.bus, self.address, AddressWidth::Bits8);
        let mut out = [0f32; 3];
        for (value, axis) in out.iter_mut().zip(self.chip.axes()) {
            let mut buf = [0u8; 4];
            let bytes = &mut buf[..usize::from(axis.bytes)];
            regs.read_burst(u16::from(axis.register), bytes)?;
            *value = axis.units(decode_value(bytes, axis.endian));
        }
        Ok(Vector3 {
            x: out[0],
            y: out[1],
            z: out[2],
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

/// The `description` setting, parsed. A bad file is `INVALID_ARGUMENT`.
fn chip(cfg: &Config<'_>) -> Result<Chip> {
    match cfg.get("description") {
        Some(ConfigValue::Str(value)) => load(value),
        _ => Err(Status::INVALID_ARGUMENT),
    }
}

#[cfg(not(feature = "std"))]
fn load(text: &str) -> Result<Chip> {
    Chip::parse(text).map_err(|_| Status::INVALID_ARGUMENT)
}

#[cfg(feature = "std")]
fn load(path: &str) -> Result<Chip> {
    let text = std::fs::read_to_string(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => Status::NOT_FOUND,
        _ => Status::IO,
    })?;
    Chip::parse(&text).map_err(|_| Status::INVALID_ARGUMENT)
}
