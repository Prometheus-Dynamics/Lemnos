//! A Linux hwmon fan (`pwm1`, `pwm1_enable`, `fan1_input`) served as `fan/1`. Duty is per
//! mille, 0 to 1000.
//!
//! Use this for a fan the kernel exposes through a hwmon device, such as one behind an embedded
//! controller. A fan on a PWM channel of the board is `pwm-channel-fan`, which needs no kernel
//! driver in between. The fan's safe state is full duty: a controller runs it there on a stale input or
//! a failed write, and this driver puts it there when it lets go on a failure. A controller that stops
//! hands the fan back to its baseline instead (`hand_back`).
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `hwmon`: the name of the hwmon directory, such as `"hwmon2"`. Or
//! - `name`: the hwmon `name` attribute to match, such as `"pwmfan"`. With neither, the first hwmon
//!   device that has a `pwm1` is used.
//! - `sys_root`: where sysfs is mounted, `"/sys"` by default. Tests point it at a fake tree.
//!
//! A missing fan fails the bind with `NOT_FOUND`, and the host retries the bind later.
//!
//! The `spin` self-test turns the fan to `self_test_duty` (per mille, default 500) for three
//! seconds, reads `fan1_input` and checks it against `self_test_rpm_min` and
//! `self_test_rpm_max`, which must both be given (else the test is skipped). It puts back the
//! duty and the `pwm1_enable` mode it found, on every end, and refuses a duty change or a
//! release while it runs. The fan must be free to spin.
//!
//! The first `set_duty` takes the fan over. Before it writes anything it records the fan's baseline:
//! `pwm1` and `pwm1_enable` as they are. Then it sets manual mode and writes `pwm1`; the kernel applies
//! the duty only in manual mode.
//!
//! Two ways to let the fan go:
//!
//! - `hand_back` puts the baseline back: `pwm1` first, in manual mode, then `pwm1_enable`, and the
//!   driver forgets the fan. A controller calls it when it is stopped or disabled, so the fan is as it
//!   was before the take-over. Nothing is written if this driver never took the fan. A failed write
//!   fails the call, and the baseline is kept until the write works.
//! - `release` puts the fan at full duty and leaves it in manual mode. It is the safe state for a
//!   failure (a write that will not go, a controller that cannot run).
//!
//! Dropping the driver puts the fan at full duty if it still holds it, as a device that stops or is
//! rebound, or a call that panics, leaves it. A controller that is dropped in an orderly way hands the
//! fan back first, so the drop then finds nothing held. The fan is not handed to the kernel's thermal
//! governor, except that the baseline mode is restored, which is the mode the fan had. A process killed
//! with `SIGKILL` runs no code, so the fan keeps the duty it was last given.
//!
//! Failure rule: an attribute that cannot be read or written fails that call and is tried again
//! on the next one. The driver never switches itself off.

mod sysfs;

#[cfg(test)]
mod tests;

use lemnos_contract_fan::Fan;
use lemnos_contract_self_test::{
    Bench, Flow, Measure, Record, Runner, Scratch, SelfTest, Spec, reason,
};
use lemnos_core::{
    CallerId, Config, ConfigError, Description, Health, Item, JobId, JobState, Key, Kind,
    Lifecycle, MethodId, Reply, Result, Status, Unit, driver,
};
use std::path::{Path, PathBuf};

/// The largest raw `pwm1` value.
pub const PWM_MAX: u32 = 255;

/// The board's limits on the fan's speed, in rpm: the fastest the fan turns (`max_rpm`), and the
/// speed below which it has stalled (`warn_below_rpm`). Unset means unknown.
#[derive(Debug, Clone, Copy, Default)]
struct RpmLimits {
    max: Option<i64>,
    warn_below: Option<i64>,
}

fn rpm_limits(cfg: &Config<'_>) -> Result<RpmLimits> {
    // A whole number, not negative: check_config refuses the rest before bind gets here.
    let optional = |key: &str| match cfg.get(key) {
        None => Ok(None),
        Some(lemnos_core::ConfigValue::Int(value)) => Ok(Some(value)),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    };
    Ok(RpmLimits {
        max: optional("max_rpm")?,
        warn_below: optional("warn_below_rpm")?,
    })
}
/// `pwm1_enable` for manual control, the only mode in which `pwm1` sets the duty.
const MODE_MANUAL: i32 = 1;
/// The largest duty, per mille.
const MAX_DUTY: u16 = 1000;

/// Method numbers in the `fan/1` contract.
const SET_DUTY: MethodId = MethodId {
    contract: 0,
    method: 1,
};
const DUTY: MethodId = MethodId {
    contract: 0,
    method: 2,
};
const RPM: MethodId = MethodId {
    contract: 0,
    method: 3,
};
const RELEASE: MethodId = MethodId {
    contract: 0,
    method: 4,
};
/// `self-test/1`'s `run`, the second contract the driver serves.
const RUN: MethodId = MethodId {
    contract: 1,
    method: 2,
};
/// The checks this driver offers. Its names are stable.
const SPECS: &[Spec] = &[Spec::intrusive(
    "spin",
    "the fan turns at the test duty, and its tachometer reads in the board file's range",
    "the fan free to spin",
    3000,
)];
/// How long the fan has to reach the test duty before its tachometer is read.
const SETTLE_NS: u64 = 3_000_000_000;
/// The duty the `spin` test sets when the board file names none.
const TEST_DUTY: u16 = 500;

/// A raw `pwm1` value as per mille, rounded.
pub fn pwm_to_duty(pwm: u32) -> u16 {
    ((pwm.min(PWM_MAX) * 1000 + PWM_MAX / 2) / PWM_MAX) as u16
}

/// Per mille as a raw `pwm1` value, rounded. Values above 1000 are clamped.
pub fn duty_to_pwm(duty: u16) -> u32 {
    (u32::from(duty.min(MAX_DUTY)) * PWM_MAX + 500) / 1000
}

#[driver("hwmon-fan", provides = [Fan, SelfTest])]
pub struct HwmonFan {
    /// The hwmon directory's attribute paths, built once at bind.
    pwm: PathBuf,
    enable: PathBuf,
    rpm: PathBuf,
    /// Whether this driver has taken the fan: `pwm1_enable` is manual since its first write.
    manual: bool,
    /// The fan as the take-over found it, which `hand_back` puts back. `None` until the take-over.
    baseline: Option<Baseline>,
    /// Whether the last call failed.
    failing: bool,
    /// The operating system owns this fan: nothing is ever opened for writing.
    reads_only: bool,
    /// The board's limits on the speed (`max_rpm`, `warn_below_rpm`).
    rpm_limits: RpmLimits,
    /// What the `spin` test sets and expects, from the board file.
    test: TestSettings,
    /// The self-test checks: their runs and the last result of each.
    runner: Runner,
    /// Who the method being run is for (see [`Lifecycle::caller`]).
    calling: CallerId,
}

/// The fan's `pwm1` and `pwm1_enable` before this driver took it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Baseline {
    pwm: u32,
    mode: i64,
}

/// The baselines this process has recorded, by the `pwm` attribute they belong to. A driver instance is
/// let go of by the host when it faults, and its drop leaves full duty on the fan: a bind after that must
/// not take that as the baseline. Cleared when the baseline is handed back.
static RECORDS: std::sync::Mutex<Vec<(PathBuf, Baseline)>> = std::sync::Mutex::new(Vec::new());

fn recorded(pwm: &Path) -> Option<Baseline> {
    let records = RECORDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    records
        .iter()
        .find(|(path, _)| path == pwm)
        .map(|(_, base)| *base)
}

fn remember(pwm: &Path, base: Baseline) {
    let mut records = RECORDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    records.retain(|(path, _)| path != pwm);
    records.push((pwm.to_path_buf(), base));
}

fn forget(pwm: &Path) {
    let mut records = RECORDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    records.retain(|(path, _)| path != pwm);
}

/// The board file's settings for the `spin` test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TestSettings {
    duty: u16,
    /// The tachometer range the test accepts, in revolutions per minute, when both are given.
    rpm: Option<(i64, i64)>,
}

/// The settings this driver reads from its board file config.
const KEYS: &[Key] = &[
    Key::optional("sys_root", Kind::Str),
    Key::optional("hwmon", Kind::Str),
    Key::optional("name", Kind::Str),
    Key::optional("max_rpm", Kind::Int),
    Key::optional("warn_below_rpm", Kind::Int),
    Key::optional("self_test_duty", Kind::Int),
    Key::optional("self_test_rpm_min", Kind::Int),
    Key::optional("self_test_rpm_max", Kind::Int),
];

impl Lifecycle for HwmonFan {
    type Needs = HwmonFanNeeds;
    // Reaches the kernel's files itself, and on a host-owned device only reads them.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ReadsOnly;
    // The self-test runs its steps from the tick, and tracks who drives the fan.
    const TICKS: bool = true;
    const JOBS: bool = true;
    const TRACKS_CALLERS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        if let Some(dir) = cfg.str("hwmon")
            && (dir.is_empty() || dir.contains('/'))
        {
            return Err(ConfigError::new(
                "hwmon",
                "must be a directory name, not a path",
            ));
        }
        for key in ["max_rpm", "warn_below_rpm"] {
            if cfg.int(key).is_some_and(|value| value < 0) {
                return Err(ConfigError::new(key, "must not be negative"));
            }
        }
        if let Some(duty) = cfg.int("self_test_duty")
            && !(1..=i64::from(MAX_DUTY)).contains(&duty)
        {
            return Err(ConfigError::new("self_test_duty", "must be 1 to 1000"));
        }
        if let (Some(min), Some(max)) = (cfg.int("self_test_rpm_min"), cfg.int("self_test_rpm_max"))
            && min > max
        {
            return Err(ConfigError::new(
                "self_test_rpm_min",
                "must not be above self_test_rpm_max",
            ));
        }
        Ok(())
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let mut bench = Spin {
            pwm: &self.pwm,
            enable: &self.enable,
            rpm: &self.rpm,
            manual: &mut self.manual,
            reads_only: self.reads_only,
            settings: self.test,
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
        let mut bench = Spin {
            pwm: &self.pwm,
            enable: &self.enable,
            rpm: &self.rpm,
            manual: &mut self.manual,
            reads_only: self.reads_only,
            settings: self.test,
        };
        self.runner.cancel(job, &mut bench);
    }

    fn start(_needs: HwmonFanNeeds, cfg: &Config<'_>) -> Result<Self> {
        let sys = Path::new(cfg.str("sys_root").unwrap_or("/sys"));
        let hwmon = sys.join("class/hwmon");
        let fan = match cfg.str("hwmon") {
            Some(dir) => {
                let fan = hwmon.join(dir);
                if !fan.join("pwm1").exists() {
                    return Err(Status::NOT_FOUND);
                }
                fan
            }
            None => find(&hwmon, cfg.str("name"))?.ok_or(Status::NOT_FOUND)?,
        };
        let mut driver = Self::new(&fan);
        driver.reads_only = cfg.host_owned();
        driver.rpm_limits = rpm_limits(cfg)?;
        driver.test = test_settings(cfg)?;
        Ok(driver)
    }

    fn describe(&self, out: &mut Description) {
        if let Some(dir) = self.pwm.parent().and_then(Path::to_str) {
            out.set_location(dir);
        }
        let mut rpm = Item::value("rpm", RPM)
            .unit(Unit::RevolutionPerMinute, 0)
            .step(1);
        if let Some(max) = self.rpm_limits.max {
            rpm = rpm.range(0, max);
        }
        if let Some(low) = self.rpm_limits.warn_below {
            rpm = rpm.warn_below(low);
        }
        out.add(rpm);
        // Duty is a count of per mille: exponent 0, so 302 shows as 302. The control has its own
        // name, so a client's `set` finds it and not the read.
        out.add(
            Item::value("duty", DUTY)
                .unit(Unit::One, 0)
                .range(0, i64::from(MAX_DUTY))
                .step(1),
        );
        out.add(
            Item::control("set_duty", SET_DUTY)
                .unit(Unit::One, 0)
                .range(0, i64::from(MAX_DUTY))
                .step(1),
        );
        out.add(Item::action("release", RELEASE));
        out.add(Item::action("self_test", RUN));
    }

    fn health(&self) -> Health {
        if self.failing {
            Health::Degraded("an attribute is unavailable: retried on the next call")
        } else if let Some(why) = self.runner.health() {
            Health::Degraded(why)
        } else {
            Health::Ok
        }
    }
}

impl SelfTest for HwmonFan {
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

/// The `spin` test's steps. Its first step reads everything it will put back, then turns the fan;
/// its second reads the tachometer. Restoring writes back what the first step read.
struct Spin<'a> {
    pwm: &'a Path,
    enable: &'a Path,
    rpm: &'a Path,
    manual: &'a mut bool,
    reads_only: bool,
    settings: TestSettings,
}

/// `scratch.saved` indices for the `spin` test.
const SAVED_MODE: usize = 0;
const SAVED_PWM: usize = 1;
const SAVED_MANUAL: usize = 2;
/// Set once the test has changed the fan, so a put-back is owed.
const SAVED_CHANGED: usize = 3;

impl Bench for Spin<'_> {
    fn step(&mut self, _test: usize, now_ns: u64, scratch: &mut Scratch) -> Flow {
        if scratch.step == 0 {
            return match self.start(scratch) {
                None => Flow::Wait(now_ns + SETTLE_NS),
                Some(record) => Flow::Done(record),
            };
        }
        Flow::Done(match sysfs::read_number_optional::<u32>(self.rpm) {
            Err(_) => Record::fail(reason::NO_RESPONSE).said("the tachometer did not read"),
            Ok(None) => Record::skipped(reason::ABSENT).said("the fan has no tachometer"),
            Ok(Some(rpm)) => {
                let rpm = i64::from(rpm);
                let (low, high) = self.settings.rpm.unwrap_or((0, 0));
                let measure = Measure::new("rpm", rpm, 0, "rpm").within(low, high);
                if (low..=high).contains(&rpm) {
                    Record::pass(reason::OK).said("the fan turns in the board file's range")
                } else {
                    Record::fail(reason::OUT_OF_RANGE)
                        .said("the fan's tachometer is outside the board file's range")
                }
                .measure(measure)
            }
        })
    }

    fn restore(&mut self, scratch: &mut Scratch) -> core::result::Result<(), Status> {
        if scratch.saved[SAVED_CHANGED] == 0 {
            return Ok(());
        }
        sysfs::write_number(self.pwm, scratch.saved[SAVED_PWM])?;
        if scratch.saved[SAVED_MODE] != i64::from(MODE_MANUAL) {
            sysfs::write_number(self.enable, scratch.saved[SAVED_MODE])?;
        }
        *self.manual = scratch.saved[SAVED_MANUAL] != 0;
        scratch.saved[SAVED_CHANGED] = 0;
        Ok(())
    }
}

impl Spin<'_> {
    /// Reads what the test will put back, then turns the fan. Everything that can be missing is
    /// found before the fan is touched, so a skip changes nothing. `Some` is the answer when the
    /// test ends here; `None` means the fan is turning and the tachometer is read after the settle.
    fn start(&mut self, scratch: &mut Scratch) -> Option<Record> {
        if self.settings.rpm.is_none() {
            return Some(
                Record::skipped(reason::NOT_CONFIGURED)
                    .said("the board file gives no self_test_rpm_min and self_test_rpm_max"),
            );
        }
        if self.reads_only {
            return Some(
                Record::skipped(reason::READ_ONLY)
                    .said("the operating system owns this fan: the test would write to it"),
            );
        }
        match sysfs::read_number_optional::<u32>(self.rpm) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Some(Record::skipped(reason::ABSENT).said("the fan has no tachometer"));
            }
            Err(_) => {
                return Some(Record::fail(reason::NO_RESPONSE).said("the tachometer did not read"));
            }
        }
        let (Ok(mode), Ok(pwm)) = (
            sysfs::read_number::<i64>(self.enable),
            sysfs::read_number::<i64>(self.pwm),
        ) else {
            return Some(
                Record::fail(reason::NO_RESPONSE).said("the fan's duty or mode did not read"),
            );
        };
        // Marked as changed before the first write, so a put-back covers a part-made change.
        scratch.saved = [mode, pwm, i64::from(*self.manual), 1];
        scratch.step = 1;
        *self.manual = true;
        if mode != i64::from(MODE_MANUAL) && sysfs::write_number(self.enable, MODE_MANUAL).is_err()
        {
            return Some(
                Record::fail(reason::NO_RESPONSE).said("the fan would not take manual mode"),
            );
        }
        match sysfs::write_number(self.pwm, duty_to_pwm(self.settings.duty)) {
            Ok(()) => None,
            Err(_) => {
                Some(Record::fail(reason::NO_RESPONSE).said("the fan would not take the test duty"))
            }
        }
    }
}

impl HwmonFan {
    fn new(fan: &Path) -> Self {
        Self {
            pwm: fan.join("pwm1"),
            enable: fan.join("pwm1_enable"),
            rpm: fan.join("fan1_input"),
            manual: false,
            baseline: None,
            failing: false,
            reads_only: false,
            rpm_limits: RpmLimits::default(),
            test: TestSettings {
                duty: TEST_DUTY,
                rpm: None,
            },
            runner: Runner::new(SPECS),
            calling: CallerId::LOCAL,
        }
    }

    fn note<T>(&mut self, result: Result<T>) -> Result<T> {
        self.failing = result.is_err();
        result
    }

    /// Takes the fan over if it is not yet, and sets its duty.
    fn drive(&mut self, per_mille: u16) -> Result<()> {
        // Every write this driver makes starts here. On a host-owned fan none is made, so there
        // is never anything to put at the safe state either.
        if self.reads_only {
            return Err(Status::NOT_PERMITTED);
        }
        if !self.manual {
            // The take-over records what the fan is at now, before its first write. A fan this process
            // took over and let go of by a drop keeps the record of its first take-over: a drop leaves the
            // safe state (full duty) on the fan, which is not its baseline.
            let base = match recorded(&self.pwm) {
                Some(base) => base,
                None => {
                    let mode = sysfs::read_number::<i64>(&self.enable)?;
                    let pwm = sysfs::read_number::<u32>(&self.pwm)?;
                    let base = Baseline { pwm, mode };
                    remember(&self.pwm, base);
                    base
                }
            };
            self.baseline = Some(base);
            sysfs::write_number(&self.enable, MODE_MANUAL)?;
            self.manual = true;
        }
        sysfs::write_number(&self.pwm, duty_to_pwm(per_mille))
    }

    /// Puts the fan back as the take-over found it: `pwm1` in manual mode first, then the mode. The
    /// baseline is kept until both writes have worked, so a failed one is tried again on the next call.
    fn hand_back_fan(&mut self) -> Result<()> {
        // A bind after a drop took nothing itself, but the record of the take-over before it says what the
        // fan goes back to.
        let Some(base) = self.baseline.or_else(|| recorded(&self.pwm)) else {
            return if self.manual {
                self.put_at_safe_state()
            } else {
                Ok(())
            };
        };
        sysfs::write_number(&self.pwm, base.pwm)?;
        if base.mode != i64::from(MODE_MANUAL) {
            sysfs::write_number(&self.enable, base.mode)?;
        }
        self.manual = false;
        self.baseline = None;
        forget(&self.pwm);
        Ok(())
    }

    /// Puts the fan at full duty, its safe state, if this driver has taken it. It stays in manual
    /// mode: nothing is handed to the kernel's governor. A failed write is tried again on the next
    /// release, or when the driver is dropped.
    fn put_at_safe_state(&mut self) -> Result<()> {
        if !self.manual {
            return Ok(());
        }
        sysfs::write_number(&self.pwm, PWM_MAX)
    }
}

impl Fan for HwmonFan {
    fn set_duty(&mut self, per_mille: u16) -> Result<()> {
        if per_mille > MAX_DUTY {
            return Err(Status::INVALID_ARGUMENT);
        }
        // A self-test is turning the fan, and puts its duty back when it ends.
        if self.runner.busy() {
            return Err(Status::BUSY);
        }
        let result = self.drive(per_mille);
        self.note(result)
    }

    fn duty(&mut self) -> Result<u16> {
        let result = sysfs::read_number::<u32>(&self.pwm).map(pwm_to_duty);
        self.note(result)
    }

    fn rpm(&mut self) -> Result<Option<u32>> {
        let result = sysfs::read_number_optional(&self.rpm);
        self.note(result)
    }

    fn release(&mut self) -> Result<()> {
        if self.runner.busy() {
            return Err(Status::BUSY);
        }
        let result = self.put_at_safe_state();
        self.note(result)
    }

    fn hand_back(&mut self) -> Result<()> {
        if self.runner.busy() {
            return Err(Status::BUSY);
        }
        let result = self.hand_back_fan();
        self.note(result)
    }
}

impl Drop for HwmonFan {
    fn drop(&mut self) {
        // Nothing can report an error from here, so a failed write is dropped.
        let _ = self.put_at_safe_state();
    }
}

/// The `self_test_duty` (per mille, 1 to 1000) and `self_test_rpm_min` and `self_test_rpm_max`
/// config for the `spin` test.
fn test_settings(cfg: &Config<'_>) -> Result<TestSettings> {
    // The range of each value is checked by check_config, so only the conversion is left here.
    let duty = match cfg.int("self_test_duty") {
        None => TEST_DUTY,
        Some(duty) => u16::try_from(duty).map_err(|_| Status::INVALID_ARGUMENT)?,
    };
    let rpm = match (cfg.int("self_test_rpm_min"), cfg.int("self_test_rpm_max")) {
        (Some(min), Some(max)) => Some((min, max)),
        _ => None,
    };
    Ok(TestSettings { duty, rpm })
}

/// The first hwmon directory under `hwmon` that has a `pwm1` and, if `name` is given, a `name`
/// attribute equal to it.
fn find(hwmon: &Path, name: Option<&str>) -> Result<Option<PathBuf>> {
    for entry in sysfs::entries(hwmon)? {
        if !entry.join("pwm1").exists() {
            continue;
        }
        let found = sysfs::read_text_optional(&entry.join("name"))?;
        if name.is_none_or(|name| found.as_deref() == Some(name)) {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}
