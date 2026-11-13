//! The BMM150 geomagnetic sensor as a magnetometer, over an I2C bus.
//!
//! The first read after bind powers the chip up and returns `AGAIN`. Each
//! later read checks the chip, reads its factory trim, starts normal mode,
//! and returns the field in tesla. The chip needs about 3 ms after power-up;
//! the host's next read provides that gap, so the driver never sleeps.
//!
//! Any bus or chip fault puts the chip back to power-up on the next read. The
//! driver never gives up: it reports the fault, and the host decides how long
//! to back off.
//!
//! An axis the chip cannot measure (overflow, or a trim that cannot compensate
//! it) reads as NaN.
//!
//! On a device the board gives to the host (`owner = "host"`) the operating
//! system's driver runs the chip, and this one only reads it: the first read
//! checks that the chip is powered and is a BMM150 and reads its trim, writing
//! nothing but register addresses, and every read returns the data registers as
//! they stand. `preset` and `data_rate` are the owner's then, and ignored; the
//! data-ready bit is too, since the owner's own reads clear it. A chip its owner
//! has powered down reads as `AGAIN`, with health saying so.

#![no_std]

mod chip;
#[cfg(test)]
mod tests;

pub use chip::{DataRate, Preset};

use chip::{Fault, Settings, Trim};
use lemnos_contract_magnetometer::Magnetometer;
use lemnos_contract_self_test::{
    Bench, Flow, Measure, Record, Runner, Scratch, SelfTest, Spec, reason,
};
use lemnos_core::{
    AddressWidth, CallerId, Config, ConfigValue, Description, Health, I2c, Item, JobId, JobState,
    Lifecycle, MethodId, Reply, Result, Status, Unit, Vector3, driver,
};

/// The 7-bit address with SDO and CSB2 low; 0x11-0x13 are the alternatives.
pub const DEFAULT_ADDRESS: u8 = 0x10;

const READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};
/// `self-test/1`'s `run`, the second contract the driver serves.
const RUN: MethodId = MethodId {
    contract: 1,
    method: 2,
};
/// The checks this driver offers. Its names are stable.
const SPECS: &[Spec] = &[Spec::read(
    "identity",
    "the chip ID register reads 0x32",
    20,
)];

/// Where the chip is in its power-up sequence.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Powered down, or the last operation failed.
    Off,
    /// Power is on; the next read starts the chip up.
    Starting,
    /// Measuring, with the trim the chip reported at start-up.
    Ready(Trim),
}

#[driver("bmm150", provides = [Magnetometer, SelfTest])]
pub struct Bmm150 {
    #[needs]
    bus: I2c,
    address: u8,
    settings: Settings,
    phase: Phase,
    /// The last conversion, repeated by a read with no new one.
    last: Option<Vector3>,
    /// Why the chip is not measuring, while it is not.
    fault: &'static str,
    /// The operating system's driver runs the chip: read it, never set it up.
    host_owned: bool,
    /// The self-test checks: their runs and the last result of each.
    runner: Runner,
    /// Who the method being run is for (see [`Lifecycle::caller`]).
    calling: CallerId,
}

impl Lifecycle for Bmm150 {
    type Needs = Bmm150Needs;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
    // A one-byte register address moves the chip's pointer and nothing else.
    const I2C_REGISTER_ADDRESS: Option<AddressWidth> = Some(AddressWidth::Bits8);
    // The self-test runs its steps from the tick, and tracks who uses the chip.
    const TICKS: bool = true;
    const JOBS: bool = true;
    const TRACKS_CALLERS: bool = true;

    fn start(needs: Bmm150Needs, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            bus: needs.bus,
            address: address(cfg)?,
            settings: settings(cfg)?,
            phase: Phase::Off,
            last: None,
            fault: "not started",
            host_owned: cfg.host_owned(),
            runner: Runner::new(SPECS),
            calling: CallerId::LOCAL,
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let mut bench = Identity {
            bus: &mut self.bus,
            address: self.address,
        };
        self.runner.tick(now_ns, &mut bench)
    }

    fn caller(&mut self, caller: CallerId) {
        self.calling = caller;
        self.runner.caller(caller);
    }

    fn caller_gone(&mut self, caller: CallerId) {
        self.runner.caller_gone(caller);
    }

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        self.runner.job(job, result)
    }

    fn job_cancel(&mut self, job: JobId) {
        let mut bench = Identity {
            bus: &mut self.bus,
            address: self.address,
        };
        self.runner.cancel(job, &mut bench);
    }

    fn describe(&self, out: &mut Description) {
        out.set_address(self.address);
        for (name, field) in [
            ("magnetic_field.x", "x"),
            ("magnetic_field.y", "y"),
            ("magnetic_field.z", "z"),
        ] {
            // The chip's full scale: ±1300 µT on x and y, ±2500 µT on z (the datasheet's measurement
            // range), in tesla as the read gives them.
            let full = if field == "z" { 2500e-6 } else { 1300e-6 };
            out.add(
                Item::value(name, READ)
                    .unit(Unit::Tesla, 0)
                    .field(field)
                    .group("magnetic_field")
                    .real_range(-full, full),
            );
        }
        out.add(Item::action("self_test", RUN));
    }

    fn health(&self) -> Health {
        match self.phase {
            Phase::Ready(_) => match self.runner.health() {
                Some(why) => Health::Degraded(why),
                None => Health::Ok,
            },
            Phase::Starting => Health::Degraded("powering up"),
            Phase::Off => Health::Failed(self.fault),
        }
    }
}

impl SelfTest for Bmm150 {
    fn tests(&mut self, into: &mut [u8]) -> Result<usize> {
        self.runner.write_tests(into)
    }

    fn run(&mut self, name: &str, confirmed: bool) -> Result<JobId> {
        self.runner.start(name, confirmed, self.calling)
    }

    fn last(&mut self, into: &mut [u8]) -> Result<usize> {
        self.runner.write_last(into)
    }
}

/// The identity test's steps. It reads the chip ID and writes nothing, so it restores nothing.
struct Identity<'a> {
    bus: &'a mut I2c,
    address: u8,
}

impl Bench for Identity<'_> {
    fn step(&mut self, _test: usize, _now_ns: u64, _scratch: &mut Scratch) -> Flow {
        Flow::Done(match chip::chip_id(self.bus, self.address) {
            Ok(chip::CHIP_ID) => Record::pass(reason::OK)
                .said("the chip ID is 0x32")
                .measure(
                    Measure::new("chip_id", i64::from(chip::CHIP_ID), 0, "")
                        .within(i64::from(chip::CHIP_ID), i64::from(chip::CHIP_ID)),
                ),
            Ok(found) => Record::fail(reason::IDENTITY)
                .said("the chip ID is not 0x32")
                .measure(Measure::new("chip_id", i64::from(found), 0, "")),
            Err(_) => Record::fail(reason::NO_RESPONSE).said("the chip ID register did not read"),
        })
    }

    fn restore(&mut self, _scratch: &mut Scratch) -> core::result::Result<(), Status> {
        Ok(())
    }
}

impl Bmm150 {
    /// Records a fault, so the next read powers the chip up again.
    fn fail(&mut self, fault: Fault) -> Status {
        self.phase = Phase::Off;
        self.fault = fault.reason();
        fault.status()
    }
}

impl Magnetometer for Bmm150 {
    fn read(&mut self) -> Result<Vector3> {
        let trim = match self.phase {
            Phase::Off if self.host_owned => match chip::attach(&mut self.bus, self.address) {
                Ok(trim) => {
                    self.phase = Phase::Ready(trim);
                    trim
                }
                Err(fault) => return Err(self.fail(fault)),
            },
            Phase::Off => {
                if let Err(fault) = chip::power_on(&mut self.bus, self.address) {
                    return Err(self.fail(fault));
                }
                self.phase = Phase::Starting;
                return Err(Status::AGAIN);
            }
            Phase::Starting => match chip::start_up(&mut self.bus, self.address, self.settings) {
                Ok(trim) => {
                    self.phase = Phase::Ready(trim);
                    trim
                }
                Err(fault) => return Err(self.fail(fault)),
            },
            Phase::Ready(trim) => trim,
        };
        let raw = match chip::read_raw(&mut self.bus, self.address) {
            Ok(raw) => raw,
            Err(fault) => return Err(self.fail(fault)),
        };
        // The owner's reads clear the data-ready bit, so on its chip the data is read as it is.
        if !raw.data_ready && !self.host_owned {
            return self.last.ok_or(Status::AGAIN);
        }
        let field = trim.compensate_fixed(raw).to_vector();
        self.last = Some(field);
        Ok(field)
    }
}

/// The `address` config: a 7-bit address that is not reserved.
fn address(cfg: &Config<'_>) -> Result<u8> {
    match cfg.get("address") {
        None => Ok(DEFAULT_ADDRESS),
        Some(ConfigValue::Int(address)) if (0x08..=0x77).contains(&address) => Ok(address as u8),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}

/// The `preset` (a name) and `data_rate` (whole hertz) config.
fn settings(cfg: &Config<'_>) -> Result<Settings> {
    let preset = match cfg.get("preset") {
        None => Preset::default(),
        Some(ConfigValue::Str(name)) => Preset::from_name(name).ok_or(Status::INVALID_ARGUMENT)?,
        Some(_) => return Err(Status::INVALID_ARGUMENT),
    };
    let data_rate = match cfg.get("data_rate") {
        None => DataRate::default(),
        Some(ConfigValue::Int(hz)) => DataRate::from_hz(hz).ok_or(Status::INVALID_ARGUMENT)?,
        Some(_) => return Err(Status::INVALID_ARGUMENT),
    };
    Ok(Settings { preset, data_rate })
}
