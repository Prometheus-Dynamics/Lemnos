//! Texas Instruments INA226, INA238 and INA260 power monitors on I2C, served as
//! `power-monitor/1`: bus volts, shunt volts, amps and watts.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `chip`: `"ina226"`, `"ina238"` or `"ina260"` (required, any case).
//! - `address`: the 7-bit I2C address, default `0x40`.
//! - `shunt_micro_ohms` (integer) or `shunt_ohms` (number): the external shunt.
//! - `max_current_micro_amps` (integer) or `max_current_a` (number): the largest
//!   expected current, which sets the current resolution.
//! - `self_test_bus_min_mv` and `self_test_bus_max_mv` (integers): the range the
//!   `bus_voltage` self-test accepts, in millivolts. Without both, the test is skipped.
//!
//! The INA260 has an integrated 2 mΩ shunt, so its shunt and current settings are
//! ignored.
//!
//! Binding reads the chip's manufacturer ID, once, and nothing else. A chip that
//! does not acknowledge its address is absent, and one that answers another
//! manufacturer is failed: either refuses the bind, which the host retries under
//! the failure rule, so a chip that appears later binds on a later retry. The chip
//! is verified and calibrated by the host's tick from its bind, and again after
//! any read fails, so a chip that resets is picked up without a client. Until
//! then health reports `Degraded`, naming the check it waits on. A failed attempt
//! is retried by the tick from 100 ms, doubling, capped at 8 s. The driver never
//! gives up and never switches itself off. A chip needs no time after its
//! configuration write, so the verification has no wait of its own.
//!
//! On a device the board gives to the host (`owner = "host"`) the operating
//! system's driver runs the chip, and this one only reads it: the first read
//! verifies the chip and reads its shunt range (INA238), writing nothing but
//! register addresses, so its configuration and calibration stay the owner's.
//! Current and power then come from the shunt voltage and the board's
//! `shunt_*` setting rather than from the chip's calibrated registers, which
//! are scaled by a calibration this driver didn't choose. The INA260's are
//! fixed, and read as they are.

#![no_std]

pub mod chip;

#[cfg(test)]
mod tests;

use chip::{Calibration, DEFAULT_ADDRESS, Model};
use lemnos_contract_power_monitor::{PowerMonitor, PowerReading};
use lemnos_contract_self_test::{
    Bench, Flow, Measure, Record, Runner, Scratch, SelfTest, Spec, reason,
};
use lemnos_core::{
    AddressWidth, CallerId, Config, ConfigError, Description, Health, I2c, Item, JobId, JobState,
    Key, Kind, Lifecycle, MethodId, RefusalKind, Reply, Result, Status, Unit, driver, status_of,
};
use lemnos_hal::HalError;

/// The largest 7-bit I2C address.
const MAX_ADDRESS: u8 = 0x7f;
/// A failed verification is retried after this, doubling each time, capped at 8 s.
const RETRY_FIRST_NS: u64 = 100_000_000;
const RETRY_CAP_NS: u64 = 8_000_000_000;

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
    "bus_voltage",
    "the bus voltage is in the board file's range",
    20,
)];

#[driver("ina2xx", provides = [PowerMonitor, SelfTest])]
pub struct Ina2xx {
    #[needs]
    bus: I2c,
    chip: chip::Ina,
    verified: bool,
    /// The next verification attempt is due, host ns. `None`: now.
    up_due: Option<u64>,
    /// The current retry wait after a failed attempt, ns. Zero when the last attempt worked.
    retry_ns: u64,
    /// The operating system's driver runs the chip: read it, never set it up.
    host_owned: bool,
    /// The board's warning limits, in nano-units (the items' raw scale). Unset means none.
    warn: Warnings,
    /// The range the `bus_voltage` test accepts, in millivolts, from the board file.
    bus_range_mv: Option<(i64, i64)>,
    /// The self-test checks: their runs and the last result of each.
    runner: Runner,
    /// Who the method being run is for (see [`Lifecycle::caller`]).
    calling: CallerId,
}

/// The rail's warning limits from the board file: `bus_warn_below_v` and `bus_warn_above_v` for the
/// bus voltage, `current_warn_above_a` for the current, in volts and amps. Set by the board, which
/// knows its rail; an unset limit is no limit.
#[derive(Debug, Clone, Copy, Default)]
struct Warnings {
    bus_below_nv: Option<i64>,
    bus_above_nv: Option<i64>,
    current_above_na: Option<i64>,
}

/// A span in nano-units, as the item's real value (volts, amps or watts).
fn real(nano: i64) -> f64 {
    nano as f64 * 1e-9
}

/// A value in volts or amps as nano-units, rounded to the nearest.
fn nano_units(value: f64) -> i64 {
    let scaled = value * 1e9;
    (if scaled < 0.0 {
        scaled - 0.5
    } else {
        scaled + 0.5
    }) as i64
}

fn warnings(cfg: &Config<'_>) -> Result<Warnings> {
    // Finite values only: check_config refuses the rest.
    let nano = |key: &str| cfg.float(key).map(nano_units);
    Ok(Warnings {
        bus_below_nv: nano("bus_warn_below_v"),
        bus_above_nv: nano("bus_warn_above_v"),
        current_above_na: nano("current_warn_above_a"),
    })
}

/// The settings this driver reads from its board file config.
const KEYS: &[Key] = &[
    Key::required("chip", Kind::Str),
    Key::optional("address", Kind::Int),
    Key::optional("shunt_micro_ohms", Kind::Int),
    Key::optional("shunt_ohms", Kind::Float),
    Key::optional("max_current_micro_amps", Kind::Int),
    Key::optional("max_current_a", Kind::Float),
    Key::optional("bus_warn_below_v", Kind::Float),
    Key::optional("bus_warn_above_v", Kind::Float),
    Key::optional("current_warn_above_a", Kind::Float),
    Key::optional("self_test_bus_min_mv", Kind::Int),
    Key::optional("self_test_bus_max_mv", Kind::Int),
];

impl Lifecycle for Ina2xx {
    type Needs = Ina2xxNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
    // A one-byte register pointer moves the chip's pointer and nothing else.
    const I2C_REGISTER_ADDRESS: Option<AddressWidth> = Some(AddressWidth::Bits8);
    // The self-test runs its steps from the tick, and tracks who uses the chip.
    const TICKS: bool = true;
    const JOBS: bool = true;
    const TRACKS_CALLERS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        let model =
            model_of(cfg).ok_or(ConfigError::new("chip", "must be ina226, ina238 or ina260"))?;
        if let Some(address) = cfg.int("address")
            && !(0..=i64::from(MAX_ADDRESS)).contains(&address)
        {
            return Err(ConfigError::new(
                "address",
                "must be a 7-bit address, 0 to 127",
            ));
        }
        // The calibration the chip cannot take: the same check the chip makes when it is built.
        let shunt_key = if cfg.get("shunt_micro_ohms").is_some() {
            "shunt_micro_ohms"
        } else {
            "shunt_ohms"
        };
        let address = cfg.int("address").and_then(|a| u8::try_from(a).ok());
        chip::Ina::new(
            model,
            address.unwrap_or(DEFAULT_ADDRESS),
            calibration_of(cfg),
        )
        .map_err(|error| ConfigError::new(shunt_key, error.0))?;
        for key in [
            "bus_warn_below_v",
            "bus_warn_above_v",
            "current_warn_above_a",
        ] {
            if cfg.float(key).is_some_and(|value| !value.is_finite()) {
                return Err(ConfigError::new(key, "must be a finite number"));
            }
        }
        if let (Some(min), Some(max)) = (
            cfg.int("self_test_bus_min_mv"),
            cfg.int("self_test_bus_max_mv"),
        ) && min > max
        {
            return Err(ConfigError::new(
                "self_test_bus_min_mv",
                "must not be above self_test_bus_max_mv",
            ));
        }
        Ok(())
    }

    fn start(needs: Ina2xxNeeds, cfg: &Config<'_>) -> Result<Self> {
        let chip = chip_from_config(cfg)?;
        let mut bus = needs.bus;
        identify_at_bind(&chip, &mut bus, cfg)?;
        Ok(Self {
            bus,
            chip,
            verified: false,
            up_due: None,
            retry_ns: 0,
            host_owned: cfg.host_owned(),
            warn: warnings(cfg)?,
            bus_range_mv: bus_range_from_config(cfg)?,
            runner: Runner::new(SPECS),
            calling: CallerId::LOCAL,
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let verifying = self.verify_step(now_ns);
        let mut bench = BusVoltage {
            chip: &self.chip,
            bus: &mut self.bus,
            range_mv: self.bus_range_mv,
        };
        let jobs = self.runner.tick(now_ns, &mut bench);
        match (verifying, jobs) {
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
        let mut bench = BusVoltage {
            chip: &self.chip,
            bus: &mut self.bus,
            range_mv: self.bus_range_mv,
        };
        self.runner.cancel(job, &mut bench);
    }

    fn describe(&self, out: &mut Description) {
        // The spans are in nano-units (the register scale of the chip, see `Setup::spans`). The items
        // are real numbers in volts, amps and watts: their values come from `read`'s f32 fields, and
        // the renderer scales a float field by the item's exponent, so the exponent is 0 and the
        // limits are real numbers (`real_range`, `real_step`). Exponent -9 here showed 4.5 V as
        // 4.5e-9 V.
        let spans = self.chip.setup.spans(self.chip.attached());
        out.set_address(self.chip.address());
        let mut bus = Item::value("bus_voltage", READ)
            .unit(Unit::Volt, 0)
            .field("bus_v")
            .real_range(0.0, real(spans.bus_max_nv))
            .real_step(real(spans.bus_lsb_nv));
        if let Some(low) = self.warn.bus_below_nv {
            bus = bus.real_warn_below(real(low));
        }
        if let Some(high) = self.warn.bus_above_nv {
            bus = bus.real_warn_above(real(high));
        }
        out.add(bus);
        let shunt = Item::value("shunt_voltage", READ)
            .unit(Unit::Volt, 0)
            .field("shunt_v");
        out.add(match (spans.shunt_max_nv, spans.shunt_lsb_nv) {
            (Some(max), Some(step)) => shunt
                .real_range(-real(max), real(max))
                .real_step(real(step)),
            _ => shunt,
        });
        let mut current = Item::value("current", READ)
            .unit(Unit::Ampere, 0)
            .field("current_a")
            .real_range(-real(spans.current_max_na), real(spans.current_max_na))
            .real_step(real(spans.current_lsb_na));
        if let Some(high) = self.warn.current_above_na {
            current = current.real_warn_above(real(high));
        }
        out.add(current);
        out.add(
            Item::value("power", READ)
                .unit(Unit::Watt, 0)
                .field("power_w")
                .real_range(0.0, real(spans.power_max_nw))
                .real_step(real(spans.power_lsb_nw.max(1))),
        );
        out.add(Item::action("self_test", RUN));
    }

    fn health(&self) -> Health {
        if !self.verified {
            Health::Degraded("chip not verified: retrying its ID check and configuration write")
        } else if let Some(why) = self.runner.health() {
            Health::Degraded(why)
        } else {
            Health::Ok
        }
    }
}

impl SelfTest for Ina2xx {
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

/// The `bus_voltage` test's steps: one read of the bus voltage register, which writes nothing.
struct BusVoltage<'a> {
    chip: &'a chip::Ina,
    bus: &'a mut I2c,
    range_mv: Option<(i64, i64)>,
}

impl Bench for BusVoltage<'_> {
    fn step(&mut self, _test: usize, _now_ns: u64, _scratch: &mut Scratch) -> Flow {
        let Some((min_mv, max_mv)) = self.range_mv else {
            return Flow::Done(
                Record::skipped(reason::NOT_CONFIGURED)
                    .said("the board file gives no self_test_bus_min_mv and self_test_bus_max_mv"),
            );
        };
        let (low, high) = (min_mv * 1000, max_mv * 1000);
        Flow::Done(match self.chip.read_fixed(&mut *self.bus) {
            Ok(raw) => {
                let micro_volts = i64::from(raw.bus_voltage_uv);
                let measure = Measure::new("bus_voltage", micro_volts, -6, "V").within(low, high);
                if (low..=high).contains(&micro_volts) {
                    Record::pass(reason::OK).said("the bus voltage is in the board file's range")
                } else {
                    Record::fail(reason::OUT_OF_RANGE)
                        .said("the bus voltage is outside the board file's range")
                }
                .measure(measure)
            }
            Err(_) => Record::fail(reason::NO_RESPONSE).said("the chip did not answer the read"),
        })
    }

    fn restore(&mut self, _scratch: &mut Scratch) -> core::result::Result<(), Status> {
        Ok(())
    }
}

/// The `self_test_bus_min_mv` and `self_test_bus_max_mv` config: both or neither. The order of
/// the two is checked by check_config.
fn bus_range_from_config(cfg: &Config<'_>) -> Result<Option<(i64, i64)>> {
    match (
        cfg.int("self_test_bus_min_mv"),
        cfg.int("self_test_bus_max_mv"),
    ) {
        (Some(min), Some(max)) => Ok(Some((min, max))),
        _ => Ok(None),
    }
}

impl Ina2xx {
    /// One verification attempt, once it is due, while the chip is not verified. Returns when the
    /// next attempt is due.
    fn verify_step(&mut self, now: u64) -> Option<u64> {
        if self.verified {
            return None;
        }
        if let Some(due) = self.up_due
            && now < due
        {
            return Some(due);
        }
        self.up_due = None;
        let verified = if self.host_owned {
            self.chip.attach(&mut self.bus)
        } else {
            self.chip.init(&mut self.bus)
        };
        match verified {
            Ok(()) => {
                self.verified = true;
                self.retry_ns = 0;
                None
            }
            Err(_) => {
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
    }
}

impl PowerMonitor for Ina2xx {
    fn read(&mut self) -> Result<PowerReading> {
        if !self.verified {
            let verified = if self.host_owned {
                self.chip.attach(&mut self.bus)
            } else {
                self.chip.init(&mut self.bus)
            };
            verified.map_err(|error| status_of(error.kind()))?;
            self.verified = true;
            self.up_due = None;
            self.retry_ns = 0;
        }
        match self.chip.read_fixed(&mut self.bus) {
            Ok(raw) => Ok(power_reading(raw)),
            Err(error) => {
                // A failed transfer may mean a reset: verify and calibrate again
                // on the next read.
                self.verified = false;
                Err(status_of(error.kind()))
            }
        }
    }
}

fn power_reading(raw: chip::ReadingFixed) -> PowerReading {
    PowerReading {
        bus_v: raw.bus_voltage_uv as f32 * 1e-6,
        shunt_v: raw.shunt_voltage_nv as f32 * 1e-9,
        current_a: raw.current_na as f32 * 1e-9,
        power_w: raw.power_nw as f32 * 1e-9,
    }
}

/// The bind's one ID read (see the crate notes): absent when the chip does not acknowledge its
/// address, failed when it answers another manufacturer, and the bus fault's status otherwise.
fn identify_at_bind(chip: &chip::Ina, bus: &mut I2c, cfg: &Config<'_>) -> Result<()> {
    let address = chip.address();
    match chip.manufacturer(bus) {
        Ok(chip::MANUFACTURER_TI) => Ok(()),
        Ok(found) => Err(cfg.refuse(
            Status::NOT_SUPPORTED,
            RefusalKind::Failed,
            format_args!(
                "the chip at {address:#04x} answers manufacturer ID {found:#06x}, expected {:#06x}",
                chip::MANUFACTURER_TI
            ),
        )),
        Err(error) => match error.kind() {
            lemnos_core::ErrorKind::Nack | lemnos_core::ErrorKind::Timeout => Err(cfg.refuse(
                Status::NO_DEVICE,
                RefusalKind::Absent,
                format_args!("no device at {address:#04x}: nothing answers at its address"),
            )),
            kind => Err(cfg.refuse(
                status_of(kind),
                RefusalKind::Failed,
                format_args!("the chip at {address:#04x} did not answer its ID read"),
            )),
        },
    }
}

/// Builds the chip from the board file's settings. Anything missing or out of
/// range is `INVALID_ARGUMENT`.
fn chip_from_config(cfg: &Config<'_>) -> Result<chip::Ina> {
    let model = model_of(cfg).ok_or_else(|| {
        cfg.refuse(
            Status::INVALID_ARGUMENT,
            RefusalKind::Failed,
            format_args!("the setting chip must name an INA226, INA238 or INA260"),
        )
    })?;
    let address = match cfg.int("address") {
        None => DEFAULT_ADDRESS,
        Some(address) => u8::try_from(address).map_err(|_| {
            cfg.refuse(
                Status::INVALID_ARGUMENT,
                RefusalKind::Failed,
                format_args!("the setting address must be a 7-bit address"),
            )
        })?,
    };
    chip::Ina::new(model, address, calibration_of(cfg)).map_err(|_| {
        cfg.refuse(
            Status::INVALID_ARGUMENT,
            RefusalKind::Failed,
            format_args!("the calibration settings cannot be used for this chip"),
        )
    })
}

/// The chip the board file names, in any case, or `None` when it names none of the three.
fn model_of(cfg: &Config<'_>) -> Option<Model> {
    let name = cfg.str("chip")?;
    if name.eq_ignore_ascii_case("ina226") {
        Some(Model::Ina226)
    } else if name.eq_ignore_ascii_case("ina238") {
        Some(Model::Ina238)
    } else if name.eq_ignore_ascii_case("ina260") {
        Some(Model::Ina260)
    } else {
        None
    }
}

/// The shunt and the largest expected current. A missing or unusable value becomes 0, which the
/// chip rejects (see check_config).
fn calibration_of(cfg: &Config<'_>) -> Calibration {
    let shunt_micro_ohms = match cfg.int("shunt_micro_ohms") {
        Some(value) => u32::try_from(value).unwrap_or(0),
        None => micro(cfg.float("shunt_ohms").unwrap_or(0.0)),
    };
    let max_current_micro_amps = match cfg.int("max_current_micro_amps") {
        Some(value) => u32::try_from(value).unwrap_or(0),
        None => micro(cfg.float("max_current_a").unwrap_or(0.0)),
    };
    Calibration::from_micro(shunt_micro_ohms, max_current_micro_amps)
}

/// A value in its base unit, as millionths. NaN and negative values become 0.
fn micro(value: f64) -> u32 {
    (value * 1e6 + 0.5) as u32
}
