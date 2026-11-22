//! The BMM150 geomagnetic sensor as a magnetometer, over an I2C bus.
//!
//! Power-up is timed work, and the driver never sleeps. The host ticks the
//! device from its bind, so the first tick powers the chip up; 3 ms later (the
//! chip's start-up time) a tick checks it, reads its factory trim and starts
//! normal mode. Until then a read returns `AGAIN` and steps nothing, and the
//! health names the wait. Once it measures, a read returns the field in tesla.
//!
//! Any bus or chip fault puts the chip back to power-up at the next tick. A
//! failed step is retried by the tick from 100 ms, doubling, capped at 8 s: the
//! driver never gives up, and never switches itself off.
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
    AddressWidth, CallerId, Config, ConfigError, ConfigValue, Description, ErrorKind, Health, I2c,
    Item, JobId, JobState, Key, Kind, Lifecycle, MethodId, RefusalKind, Reply, Result, Status,
    Unit, Vector3, driver,
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
/// Every setting the driver reads from the board file. Each has a default, so none is required.
const KEYS: &[Key] = &[
    Key::optional("address", Kind::Int),
    Key::optional("preset", Kind::Str),
    Key::optional("data_rate", Kind::Int),
];

/// The checks this driver offers. Its names are stable.
const SPECS: &[Spec] = &[Spec::read(
    "identity",
    "the chip ID register reads 0x32",
    20,
)];

/// The chip's start-up time after power-on (the dev line's `POWER_ON_US`).
const POWER_ON_NS: u64 = 3_000_000;
/// A failed power-up step is retried after this, doubling each time, capped at 8 s.
const RETRY_FIRST_NS: u64 = 100_000_000;
const RETRY_CAP_NS: u64 = 8_000_000_000;

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
    /// Whether the last thing that happened was a failure (as opposed to waiting to start).
    faulted: bool,
    /// The next power-up step (or retry) is due, host ns. `None`: now.
    up_due: Option<u64>,
    /// The current retry wait after a failed step, ns. Zero when the last step worked.
    retry_ns: u64,
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

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        if let Some(ConfigValue::Int(address)) = cfg.get("address")
            && !(0x08..=0x77).contains(&address)
        {
            return Err(ConfigError::new(
                "address",
                "must be a 7-bit address from 0x08 to 0x77",
            ));
        }
        if let Some(ConfigValue::Str(name)) = cfg.get("preset")
            && Preset::from_name(name).is_none()
        {
            return Err(ConfigError::new(
                "preset",
                "must be low_power, regular, enhanced or high_accuracy",
            ));
        }
        if let Some(ConfigValue::Int(hz)) = cfg.get("data_rate")
            && DataRate::from_hz(hz).is_none()
        {
            return Err(ConfigError::new(
                "data_rate",
                "must be 2, 6, 8, 10, 15, 20, 25 or 30 hertz",
            ));
        }
        Ok(())
    }

    fn start(needs: Bmm150Needs, cfg: &Config<'_>) -> Result<Self> {
        let address = address(cfg)?;
        let mut bus = needs.bus;
        identify_at_bind(&mut bus, address, cfg)?;
        Ok(Self {
            bus,
            address,
            settings: settings(cfg)?,
            phase: Phase::Off,
            last: None,
            fault: "powering up: waiting for the first power-up step",
            faulted: false,
            up_due: None,
            retry_ns: 0,
            host_owned: cfg.host_owned(),
            runner: Runner::new(SPECS),
            calling: CallerId::LOCAL,
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let powering = self.step_up(now_ns);
        let mut bench = Identity {
            bus: &mut self.bus,
            address: self.address,
        };
        let jobs = self.runner.tick(now_ns, &mut bench);
        match (powering, jobs) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
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
            Phase::Starting => Health::Degraded("powering up: waiting 3 ms for the chip to start"),
            Phase::Off if self.faulted => Health::Failed(self.fault),
            Phase::Off => Health::Degraded(self.fault),
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
    /// Records a fault, so the next tick powers the chip up again.
    fn fail(&mut self, fault: Fault) -> Status {
        self.phase = Phase::Off;
        self.fault = fault.reason();
        self.faulted = true;
        fault.status()
    }

    /// One power-up step, once its time has come. Returns when the next step, or the retry of
    /// this one, is due.
    fn step_up(&mut self, now: u64) -> Option<u64> {
        if let Some(due) = self.up_due
            && now < due
        {
            return Some(due);
        }
        self.up_due = None;
        let step = match self.phase {
            Phase::Ready(_) => return None,
            // The operating system's driver runs the chip: this one only attaches.
            Phase::Off if self.host_owned => {
                chip::attach(&mut self.bus, self.address).map(|trim| {
                    self.phase = Phase::Ready(trim);
                    None
                })
            }
            Phase::Off => chip::power_on(&mut self.bus, self.address).map(|()| {
                self.phase = Phase::Starting;
                Some(POWER_ON_NS)
            }),
            Phase::Starting => {
                chip::start_up(&mut self.bus, self.address, self.settings).map(|trim| {
                    self.phase = Phase::Ready(trim);
                    None
                })
            }
        };
        match step {
            Ok(Some(gap)) => {
                self.retry_ns = 0;
                let due = now.saturating_add(gap);
                self.up_due = Some(due);
                Some(due)
            }
            Ok(None) => {
                self.retry_ns = 0;
                self.faulted = false;
                None
            }
            Err(fault) => {
                let _ = self.fail(fault);
                self.retry_at(now)
            }
        }
    }

    /// Schedules the next attempt after a failure: from 100 ms, doubling, capped at 8 s.
    fn retry_at(&mut self, now: u64) -> Option<u64> {
        self.retry_ns = if self.retry_ns == 0 {
            RETRY_FIRST_NS
        } else {
            self.retry_ns.saturating_mul(2).min(RETRY_CAP_NS)
        };
        let due = now.saturating_add(self.retry_ns);
        self.up_due = Some(due);
        Some(due)
    }
}

impl Magnetometer for Bmm150 {
    fn read(&mut self) -> Result<Vector3> {
        // Power-up is the tick's: until the chip is ready, a read says so and steps nothing.
        let Phase::Ready(trim) = self.phase else {
            // A failure since the chip was last ready is reported, so the host counts it.
            return Err(if self.faulted {
                Status::IO
            } else {
                Status::AGAIN
            });
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

/// The bind's ID read. A chip that does not acknowledge its address (or does not answer) is
/// absent; one that answers another ID is failed, and the sentence names both IDs; any other bus
/// fault is failed. A chip in suspend mode (its state after power-on) answers ID 0x00 with its power
/// bit clear: it is there, and its ID is checked once the power-up has taken it out of suspend
/// ([`chip::start_up`]). A refused bind is retried under the failure rule, so a chip whose supply
/// comes up later binds on a later retry.
fn identify_at_bind(bus: &mut I2c, address: u8, cfg: &Config<'_>) -> Result<()> {
    match chip::chip_id(bus, address) {
        Ok(chip::CHIP_ID) => Ok(()),
        Ok(0x00) if matches!(chip::powered(bus, address), Ok(false)) => Ok(()),
        Ok(found) => Err(cfg.refuse(
            Status::NOT_SUPPORTED,
            RefusalKind::Failed,
            format_args!(
                "the chip at {address:#04x} answers ID {found:#04x}, expected {:#04x}",
                chip::CHIP_ID
            ),
        )),
        Err(Fault::Bus(ErrorKind::Nack | ErrorKind::Timeout)) => Err(cfg.refuse(
            Status::NO_DEVICE,
            RefusalKind::Absent,
            format_args!("no device at {address:#04x}: nothing answers at its address"),
        )),
        Err(fault) => Err(cfg.refuse(
            fault.status(),
            RefusalKind::Failed,
            format_args!("the chip at {address:#04x} did not answer its ID read"),
        )),
    }
}

/// The `address` config: a 7-bit address. Its range is checked by `check_config`.
fn address(cfg: &Config<'_>) -> Result<u8> {
    match cfg.get("address") {
        None => Ok(DEFAULT_ADDRESS),
        Some(ConfigValue::Int(address)) => u8::try_from(address).map_err(|_| {
            cfg.refuse(
                Status::INVALID_ARGUMENT,
                RefusalKind::Failed,
                format_args!("the setting address must be a 7-bit address"),
            )
        }),
        Some(_) => Err(cfg.refuse(
            Status::INVALID_ARGUMENT,
            RefusalKind::Failed,
            format_args!("the setting address must be a whole number"),
        )),
    }
}

/// The `preset` (a name) and `data_rate` (whole hertz) config. Their values are checked by
/// `check_config`; the lookups here cannot fail after it.
fn settings(cfg: &Config<'_>) -> Result<Settings> {
    let preset = match cfg.get("preset") {
        None => Preset::default(),
        Some(ConfigValue::Str(name)) => Preset::from_name(name).ok_or_else(|| {
            cfg.refuse(
                Status::INVALID_ARGUMENT,
                RefusalKind::Failed,
                format_args!("the setting preset must name a preset the chip has"),
            )
        })?,
        Some(_) => {
            return Err(cfg.refuse(
                Status::INVALID_ARGUMENT,
                RefusalKind::Failed,
                format_args!("the setting preset must be a name"),
            ));
        }
    };
    let data_rate = match cfg.get("data_rate") {
        None => DataRate::default(),
        Some(ConfigValue::Int(hz)) => DataRate::from_hz(hz).ok_or_else(|| {
            cfg.refuse(
                Status::INVALID_ARGUMENT,
                RefusalKind::Failed,
                format_args!("the setting data_rate must be a rate the chip has"),
            )
        })?,
        Some(_) => {
            return Err(cfg.refuse(
                Status::INVALID_ARGUMENT,
                RefusalKind::Failed,
                format_args!("the setting data_rate must be a whole number"),
            ));
        }
    };
    Ok(Settings { preset, data_rate })
}
