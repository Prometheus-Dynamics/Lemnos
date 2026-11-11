//! The BMI088 accelerometer and gyroscope, served as `accelerometer/1` (m/s²)
//! and `gyroscope/1` (rad/s), over I2C.
//!
//! The chip is two dies on one bus at two addresses. Each contract reads its
//! own die, so the two are independent: a missing gyroscope leaves the
//! accelerometer working. Health is the device's, and names the first die that
//! is not working. A die nobody has read yet is not working.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `accel_address` (default `0x18`) and `gyro_address` (default `0x68`): the
//!   7-bit addresses.
//! - `accel_range` (g: 3, 6, 12 or 24; default 6) and `accel_rate` (Hz: 12.5,
//!   25, 50, 100, 200, 400, 800 or 1600; default 100).
//! - `gyro_range` (degrees per second: 125, 250, 500, 1000 or 2000; default
//!   2000) and `gyro_rate` (Hz: 100, 200, 400, 1000 or 2000; default 2000).
//! - `fifo` (bool; default false): read through the chip's FIFOs. See below.
//!
//! Power-up is spread over reads, and the driver never sleeps. Each die's first
//! read soft-resets it and returns `AGAIN`; the next steps check the chip ID,
//! power the die on and write its range and rate, one step a read. The host's
//! read interval gives the chip time between steps. Each step can be retried,
//! and the range and rate are read back, so a write that landed too early fails
//! its readback and is written again on a later read. A failed transfer reports
//! on that read, and the die starts over on a later one (a failed power-up step
//! is retried in place). A wrong chip ID is refused and nothing is written. The
//! driver never gives up and never switches itself off. The pacing of retries
//! is the host's: the driver asks for no timer of its own while a die is
//! powering up.
//!
//! Without `fifo`, each read takes the die's current sample, and there are no
//! batches (`read_batch` is `NOT_SUPPORTED`).
//!
//! With `fifo`, the driver drains each die's FIFO into a fixed ring of its
//! last samples, and stamps them from the host's clock:
//!
//! - `tick` drains each die once its drain is due. The period is half the
//!   FIFO's depth at the configured rate (146 frames at 200 Hz for the
//!   accelerometer: 365 ms), so the FIFO cannot fill while nobody calls.
//! - `read` drains the FIFO too, and returns the newest sample. A read that
//!   finds nothing new returns the last one.
//! - `read_batch` drains, then returns every sample the ring holds that the
//!   caller has not had, oldest first, as stamped records (see the contract
//!   crates). A batch that does not fit the caller's buffer keeps the rest for
//!   the next call.
//!
//! Stamps. A tick's drain stamps its newest sample with the tick's time, and
//! each older one back from it by the die's sample period. The newest sample
//! was taken less than one period before the FIFO was read, and the read came
//! within one transaction after the tick, so the newest stamp is up to one
//! period late and up to one transaction early. An older sample adds its age
//! times the error in the period. The period is nominal until the die's own is
//! measured: the average since the first tick drain, once that spans 32
//! samples and is within 5 % of nominal. The average's error is under one
//! period divided by the samples since the first drain, so an older sample's
//! error is under its age times that. Before the measurement, a die 1 % fast
//! drifts by about 0.7 of a period across a 200 Hz batch of 73.
//!
//! A read's drain has no time of its own. Its samples are stamped on the sample
//! grid from the last tick's stamp, so they are as accurate as that stamp and
//! the period. A sample drained before any tick has no stamp, and is not
//! queued for a batch.
//!
//! A loss (a FIFO overflow, or a batch that the ring overwrote before it was
//! read) is reported in health from the call that found it, and until the read
//! after that call has ended. A FIFO overflow found by a tick is therefore
//! reported to the next read. The loss messages are `FIFO overflowed, samples
//! lost` and `batch missed samples`.
//!
//! Not in this driver: the gyroscope alignment, which needs a host clock read
//! at the start and end of each bus transaction (the driver gets one time per
//! call, at the call's start), and the auto-calibration, which the caller does
//! (the chip does not calibrate itself).
//!
//! Retries of a failed die are paced by the host: a die that failed is started
//! again by the next `read` or `read_batch`, never by a timer of its own.

#![no_std]

mod chip;
#[cfg(test)]
mod tests;

use chip::{Failure, Frame, Kind, Setting};
use lemnos_contract_accelerometer::{Accelerometer, Sample as AccelSample};
use lemnos_contract_gyroscope::{Gyroscope, Sample as GyroSample};
use lemnos_core::{
    Config, ConfigValue, Description, Encode, Health, I2c, Item, Lifecycle, MethodId, Result,
    Status, Unit, Vector3, Writer, driver,
};

/// The accelerometer's 7-bit address with SDO1 low.
pub const DEFAULT_ACCEL_ADDRESS: u8 = 0x18;
/// The gyroscope's 7-bit address with SDO2 low.
pub const DEFAULT_GYRO_ADDRESS: u8 = 0x68;

/// Bytes of one batch record (see the contract crates).
const RECORD_LEN: usize = 20;
/// Ring slots: each die's FIFO depth in frames.
const ACC_RING: usize = chip::ACC_FIFO_FRAMES;
const GYRO_RING: usize = chip::GYRO_FIFO_FRAMES;

const ACCEL_READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};
const GYRO_READ: MethodId = MethodId {
    contract: 1,
    method: 1,
};

/// Where a die is in its power-up sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Not started, or its last failure sent it back here.
    Off,
    /// Soft-reset; the next step checks the chip ID again.
    Reset,
    /// Accelerometer only: suspend mode is off; the next step powers it on.
    Power,
    /// The next step writes the range and rate (and the FIFO setup).
    Configure,
    /// Measuring.
    Ready,
}

/// What a die is doing, as a health reason.
#[derive(Debug, Clone, Copy)]
enum Note {
    NotStarted,
    Powering,
    WrongId,
    Transfer,
    /// The FIFO overflowed: samples were lost inside the chip.
    Overflow,
    /// The ring overwrote samples before a batch took them.
    Batch,
}

fn text(kind: Kind, note: Note) -> &'static str {
    match (kind, note) {
        (Kind::Accel, Note::NotStarted) => "accelerometer: not started",
        (Kind::Accel, Note::Powering) => "accelerometer: powering up",
        (Kind::Accel, Note::WrongId) => "accelerometer: wrong chip id",
        (Kind::Accel, Note::Transfer) => "accelerometer: transfer failed",
        (Kind::Accel, Note::Overflow) => "accelerometer: FIFO overflowed, samples lost",
        (Kind::Accel, Note::Batch) => "accelerometer: batch missed samples",
        (Kind::Gyro, Note::NotStarted) => "gyroscope: not started",
        (Kind::Gyro, Note::Powering) => "gyroscope: powering up",
        (Kind::Gyro, Note::WrongId) => "gyroscope: wrong chip id",
        (Kind::Gyro, Note::Transfer) => "gyroscope: transfer failed",
        (Kind::Gyro, Note::Overflow) => "gyroscope: FIFO overflowed, samples lost",
        (Kind::Gyro, Note::Batch) => "gyroscope: batch missed samples",
    }
}

/// One sample in the ring. `seq` is its number, `u64::MAX` when the slot holds
/// no sample.
#[derive(Debug, Clone, Copy)]
struct Entry {
    seq: u64,
    at_ns: u64,
    raw: [i16; 3],
}

const EMPTY: Entry = Entry {
    seq: u64::MAX,
    at_ns: 0,
    raw: [0; 3],
};

/// A die's clock: where its samples fall on the host's clock. Both fields are
/// set by tick drains, which are the only ones with a time.
#[derive(Debug, Clone, Copy, Default)]
struct Timing {
    /// The newest sample's number and the host time of the tick that drained
    /// it.
    anchor: Option<(u64, u64)>,
    /// The first tick drain that had samples: the same pair. The period is
    /// measured from it.
    first: Option<(u64, u64)>,
    /// The period measured from `first`, once it spans enough samples and is
    /// within 5 % of nominal.
    measured: Option<u64>,
}

/// The samples that span a measurement, at least.
const MIN_SPAN: u64 = 32;

impl Timing {
    /// A tick drain saw newest sample `newest` at host time `now`.
    fn measure(&mut self, newest: u64, now: u64, nominal: u64) {
        match self.first {
            None => self.first = Some((newest, now)),
            Some((first, t0)) => {
                let span = newest.saturating_sub(first);
                if span >= MIN_SPAN && now > t0 {
                    let period = (now - t0) / span;
                    if period.abs_diff(nominal) * 20 <= nominal {
                        self.measured = Some(period);
                    }
                }
            }
        }
        self.anchor = Some((newest, now));
    }

    fn period(&self, nominal: u64) -> u64 {
        self.measured.unwrap_or(nominal)
    }
}

/// One die: its address, settings, power-up phase, FIFO ring and newest sample.
struct Die<const RING: usize> {
    kind: Kind,
    address: u8,
    fifo: bool,
    setting: Setting,
    phase: Phase,
    /// The newest sample; a read with no new one repeats it.
    last: Option<[i16; 3]>,
    note: &'static str,
    /// Samples the FIFO has produced so far, the ones it dropped included. The
    /// newest is `seq - 1`.
    seq: u64,
    ring: [Entry; RING],
    /// The next sample a batch takes.
    batch_next: u64,
    timing: Timing,
    /// When the next tick drain is due, host ns (`None`: not draining).
    due: Option<u64>,
    /// A loss a drain or batch found, not yet shown to a read.
    pending: Option<Note>,
    /// The loss a read shows in health.
    shown: Option<Note>,
}

impl<const RING: usize> Die<RING> {
    fn new(kind: Kind, address: u8, fifo: bool, setting: Setting) -> Self {
        Self {
            kind,
            address,
            fifo,
            setting,
            phase: Phase::Off,
            last: None,
            note: text(kind, Note::NotStarted),
            seq: 0,
            ring: [EMPTY; RING],
            batch_next: 0,
            timing: Timing::default(),
            due: None,
            pending: None,
            shown: None,
        }
    }

    fn set(&mut self, phase: Phase) {
        self.phase = phase;
        if phase != Phase::Ready {
            self.note = text(self.kind, Note::Powering);
        }
    }

    /// One step of power-up. `Ok(true)` once the die is ready to measure.
    fn power(&mut self, bus: &mut I2c) -> chip::Step<bool> {
        let (address, kind) = (self.address, self.kind);
        match self.phase {
            Phase::Off => {
                chip::check_id(bus, address, kind)?;
                chip::soft_reset(bus, address, kind);
                self.set(Phase::Reset);
                return Ok(false);
            }
            Phase::Reset => {
                chip::check_id(bus, address, kind)?;
                match kind {
                    Kind::Accel => {
                        chip::accel_power_config(bus, address)?;
                        self.set(Phase::Power);
                    }
                    Kind::Gyro => self.set(Phase::Configure),
                }
                return Ok(false);
            }
            Phase::Power => {
                chip::accel_power_control(bus, address)?;
                self.set(Phase::Configure);
                return Ok(false);
            }
            Phase::Configure => {
                chip::configure(bus, address, kind, self.setting, self.fifo)?;
                self.phase = Phase::Ready;
            }
            Phase::Ready => {}
        }
        Ok(true)
    }

    /// A read's step: power-up, or the die's newest sample. `None` while the
    /// die is still powering up.
    fn sample(&mut self, bus: &mut I2c) -> chip::Step<Option<[i16; 3]>> {
        if !self.power(bus)? {
            return Ok(None);
        }
        if self.fifo {
            self.drain(bus, None)?;
            Ok(self.last)
        } else {
            chip::read_axes(bus, self.address, self.kind).map(Some)
        }
    }

    /// Drains the FIFO into the ring, stamps what it took, and records any loss.
    /// `now` is the tick's time; `None` for a read's drain.
    fn drain(&mut self, bus: &mut I2c, now: Option<u64>) -> chip::Step<()> {
        let first = self.seq;
        let (overflow, result) = self.pull(bus);
        if overflow {
            self.pending = Some(Note::Overflow);
        }
        self.stamp(first, now);
        result
    }

    /// Takes the FIFO's frames into the ring. Returns whether the FIFO
    /// overflowed, and the transfer's result: what was taken is kept either way.
    fn pull(&mut self, bus: &mut I2c) -> (bool, chip::Step<()>) {
        let (address, kind) = (self.address, self.kind);
        let mut overflow = false;
        let result = match kind {
            Kind::Accel => chip::accel_fifo_bytes(bus, address).and_then(|bytes| {
                chip::drain_accel(bus, address, bytes, &mut |frame| match frame {
                    Frame::Skipped(count) => {
                        // The dropped frames are older than the next ones, so
                        // they take their numbers: stamps stay on the grid.
                        overflow = true;
                        self.seq = self.seq.saturating_add(u64::from(count));
                    }
                    Frame::Sample(raw) => self.push(raw),
                })
            }),
            Kind::Gyro => chip::gyro_fifo_frames(bus, address).and_then(|frames| {
                overflow = frames >= chip::GYRO_FIFO_FRAMES;
                if overflow {
                    // The frames lost are not counted, so the grid is gone.
                    self.timing = Timing::default();
                }
                chip::drain_gyro(bus, address, frames, &mut |raw| self.push(raw))
            }),
        };
        (overflow, result)
    }

    fn push(&mut self, raw: [i16; 3]) {
        self.last = Some(raw);
        let slot = (self.seq % RING as u64) as usize;
        self.ring[slot] = Entry {
            seq: self.seq,
            at_ns: 0,
            raw,
        };
        self.seq = self.seq.saturating_add(1);
    }

    /// Gives the samples numbered `first..seq` their stamps: from the tick's
    /// time when a tick took them, otherwise on the grid from the last tick.
    fn stamp(&mut self, first: u64, now: Option<u64>) {
        if self.seq == first {
            return;
        }
        let nominal = self.setting.period_ns;
        let newest = self.seq - 1;
        match now {
            Some(now) => {
                self.timing.measure(newest, now, nominal);
                let period = self.timing.period(nominal);
                for n in first..self.seq {
                    let back = newest - n;
                    self.set_stamp(n, now.saturating_sub(back.saturating_mul(period)));
                }
            }
            None => {
                let period = self.timing.period(nominal);
                for n in first..self.seq {
                    match self.timing.anchor {
                        Some((at_n, at_ns)) => {
                            let ahead = n.saturating_sub(at_n).saturating_mul(period);
                            self.set_stamp(n, at_ns.saturating_add(ahead));
                        }
                        // No tick yet: no time to give it. Not queued.
                        None => self.set_stamp(n, u64::MAX),
                    }
                }
            }
        }
    }

    /// Sets sample `n`'s stamp, or unqueues it when `at_ns` is `u64::MAX`.
    fn set_stamp(&mut self, n: u64, at_ns: u64) {
        let slot = &mut self.ring[(n % RING as u64) as usize];
        if slot.seq == n {
            if at_ns == u64::MAX {
                *slot = EMPTY;
            } else {
                slot.at_ns = at_ns;
            }
        }
    }

    /// Writes the next samples a batch has not taken, oldest first, as records
    /// of `RECORD_LEN` bytes into `into`. Returns the bytes written.
    fn deliver(
        &mut self,
        into: &mut [u8],
        record: fn(&mut Writer<'_>, u64, Vector3) -> Result<()>,
    ) -> usize {
        let capacity = into.len() / RECORD_LEN;
        let oldest = self.seq.saturating_sub(RING as u64);
        if self.batch_next < oldest {
            // The ring overwrote samples nobody took.
            self.pending = Some(Note::Batch);
            self.batch_next = oldest;
        }
        let mut writer = Writer::new(into);
        let mut written = 0;
        while self.batch_next < self.seq {
            let entry = self.ring[(self.batch_next % RING as u64) as usize];
            if entry.seq == self.batch_next {
                if written == capacity {
                    break;
                }
                let value = chip::to_vector(entry.raw, self.setting.scale);
                if record(&mut writer, entry.at_ns, value).is_err() {
                    break;
                }
                written += 1;
            }
            self.batch_next += 1;
        }
        writer.len()
    }

    /// Runs a tick: drains the FIFO if its drain is due. Returns when the next
    /// drain is due.
    fn tick(&mut self, now: u64, bus: &mut I2c) -> Option<u64> {
        if !self.fifo || self.phase != Phase::Ready {
            self.due = None;
            return None;
        }
        if self.due.is_none_or(|due| now >= due) {
            if let Err(failure) = self.drain(bus, Some(now)) {
                self.fail(&failure);
                return None;
            }
            let half_depth = RING as u64 * self.setting.period_ns / 2;
            self.due = Some(now.saturating_add(half_depth));
        }
        self.due
    }

    /// Records a failure. A wrong chip, or a failure while measuring, sends the
    /// die back to power-up for a later read. Any other failure is retried in
    /// place.
    fn fail(&mut self, failure: &Failure) -> Status {
        let wrong_id = matches!(failure, Failure::WrongChip);
        self.note = text(
            self.kind,
            if wrong_id {
                Note::WrongId
            } else {
                Note::Transfer
            },
        );
        if wrong_id || self.phase == Phase::Ready {
            self.phase = Phase::Off;
            self.last = None;
            self.timing = Timing::default();
            self.due = None;
            self.pending = None;
            self.shown = None;
        }
        failure.status()
    }

    /// Ends a method call: a loss found during it, or before it by a tick, is
    /// shown in health until the next call.
    fn settle(&mut self) {
        self.shown = self.pending.take();
    }

    /// Why the die is not fully working, if it is not.
    fn trouble(&self) -> Option<&'static str> {
        if self.phase != Phase::Ready {
            Some(self.note)
        } else {
            self.pending
                .or(self.shown)
                .map(|note| text(self.kind, note))
        }
    }
}

/// A read of one die: its sample in SI units, `AGAIN` while it powers up, or
/// the failure's status.
fn read_die<const RING: usize>(die: &mut Die<RING>, bus: &mut I2c) -> Result<Vector3> {
    let out = match die.sample(bus) {
        Ok(Some(raw)) => Ok(chip::to_vector(raw, die.setting.scale)),
        Ok(None) => Err(Status::AGAIN),
        Err(failure) => Err(die.fail(&failure)),
    };
    die.settle();
    out
}

/// A batch read of one die: drains the FIFO, then writes the samples the batch
/// has not taken. `AGAIN` while the die powers up.
fn batch_die<const RING: usize>(
    die: &mut Die<RING>,
    bus: &mut I2c,
    into: &mut [u8],
    record: fn(&mut Writer<'_>, u64, Vector3) -> Result<()>,
) -> Result<usize> {
    let out = if !die.fifo {
        Err(Status::NOT_SUPPORTED)
    } else {
        match die.power(bus) {
            Ok(false) => Err(Status::AGAIN),
            Err(failure) => Err(die.fail(&failure)),
            Ok(true) => match die.drain(bus, None) {
                Err(failure) => Err(die.fail(&failure)),
                Ok(()) => Ok(die.deliver(into, record)),
            },
        }
    };
    die.settle();
    out
}

fn accel_record(out: &mut Writer<'_>, at_ns: u64, value: Vector3) -> Result<()> {
    AccelSample { at_ns, value }.encode(out)
}

fn gyro_record(out: &mut Writer<'_>, at_ns: u64, value: Vector3) -> Result<()> {
    GyroSample { at_ns, value }.encode(out)
}

/// The BMI088's two dies on one I2C bus.
#[driver("bmi088", provides = [Accelerometer, Gyroscope])]
pub struct Bmi088 {
    #[needs]
    bus: I2c,
    accel: Die<ACC_RING>,
    gyro: Die<GYRO_RING>,
}

impl Lifecycle for Bmi088 {
    type Needs = Bmi088Needs;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
    const TICKS: bool = true;

    fn start(needs: Bmi088Needs, cfg: &Config<'_>) -> Result<Self> {
        let fifo = match cfg.get("fifo") {
            None => false,
            Some(ConfigValue::Bool(on)) => on,
            Some(_) => return Err(Status::INVALID_ARGUMENT),
        };
        let accel = chip::accel_setting(
            int(cfg, "accel_range", 6)?,
            number(cfg, "accel_rate", 100.0)?,
        )
        .ok_or(Status::INVALID_ARGUMENT)?;
        let gyro = chip::gyro_setting(int(cfg, "gyro_range", 2000)?, int(cfg, "gyro_rate", 2000)?)
            .ok_or(Status::INVALID_ARGUMENT)?;
        Ok(Self {
            bus: needs.bus,
            accel: Die::new(
                Kind::Accel,
                address(cfg, "accel_address", DEFAULT_ACCEL_ADDRESS)?,
                fifo,
                accel,
            ),
            gyro: Die::new(
                Kind::Gyro,
                address(cfg, "gyro_address", DEFAULT_GYRO_ADDRESS)?,
                fifo,
                gyro,
            ),
        })
    }

    fn describe(&self, out: &mut Description) {
        for (name, field) in [
            ("acceleration.x", "x"),
            ("acceleration.y", "y"),
            ("acceleration.z", "z"),
        ] {
            out.add(
                Item::value(name, ACCEL_READ)
                    .unit(Unit::MetrePerSecondSquared, 0)
                    .field(field),
            );
        }
        for (name, field) in [
            ("angular_rate.x", "x"),
            ("angular_rate.y", "y"),
            ("angular_rate.z", "z"),
        ] {
            out.add(
                Item::value(name, GYRO_READ)
                    .unit(Unit::RadianPerSecond, 0)
                    .field(field),
            );
        }
    }

    fn health(&self) -> Health {
        match self.accel.trouble().or_else(|| self.gyro.trouble()) {
            Some(reason) => Health::Degraded(reason),
            None => Health::Ok,
        }
    }

    /// Drains each die whose drain is due. Returns the earliest next drain.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let accel = self.accel.tick(now_ns, &mut self.bus);
        let gyro = self.gyro.tick(now_ns, &mut self.bus);
        match (accel, gyro) {
            (Some(a), Some(g)) => Some(a.min(g)),
            (a, g) => a.or(g),
        }
    }
}

impl Accelerometer for Bmi088 {
    fn read(&mut self) -> Result<Vector3> {
        read_die(&mut self.accel, &mut self.bus)
    }

    fn read_batch(&mut self, into: &mut [u8]) -> Result<usize> {
        batch_die(&mut self.accel, &mut self.bus, into, accel_record)
    }
}

impl Gyroscope for Bmi088 {
    fn read(&mut self) -> Result<Vector3> {
        read_die(&mut self.gyro, &mut self.bus)
    }

    fn read_batch(&mut self, into: &mut [u8]) -> Result<usize> {
        batch_die(&mut self.gyro, &mut self.bus, into, gyro_record)
    }
}

/// An integer setting, or `default` when absent. Any other type is refused.
fn int(cfg: &Config<'_>, key: &str, default: i64) -> Result<i64> {
    match cfg.get(key) {
        None => Ok(default),
        Some(ConfigValue::Int(value)) => Ok(value),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}

/// A number setting (an integer is accepted), or `default` when absent.
fn number(cfg: &Config<'_>, key: &str, default: f64) -> Result<f64> {
    match cfg.get(key) {
        None => Ok(default),
        Some(ConfigValue::Int(value)) => Ok(value as f64),
        Some(ConfigValue::Float(value)) => Ok(value),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}

/// A 7-bit address that is not reserved, or `default` when absent.
fn address(cfg: &Config<'_>, key: &str, default: u8) -> Result<u8> {
    let value = int(cfg, key, i64::from(default))?;
    if (0x08..=0x77).contains(&value) {
        Ok(value as u8)
    } else {
        Err(Status::INVALID_ARGUMENT)
    }
}
