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
//! Power-up is timed work, and the driver never sleeps. The host ticks a device
//! from its bind, so each die's power-up runs without any client: the first
//! tick soft-resets the die, and later ticks check the chip ID, power the
//! accelerometer on and write its range and rate, one step at a time. Each step
//! is due when the chip's own wait since the step before has passed (the
//! datasheets' times: 1 ms after an accelerometer reset, 30 ms after a
//! gyroscope's, 1 ms between the accelerometer's suspend-mode writes, 5 ms after
//! it powers on), and the tick returns that time. Until a die is ready, a read
//! says `AGAIN` and steps nothing. The range and rate are read back, so a write
//! that landed too early fails its readback and the step is retried. A failed
//! step is retried by the tick from 100 ms, doubling, capped at 8 s; a wrong chip
//! ID is refused and nothing is written. A die that was ready and fails a
//! transfer starts over at the next tick. The driver never gives up and never
//! switches itself off.
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
//! Stamps. The accelerometer's samples are stamped from the chip's own sensor
//! time (`sensor_clock.rs`), not from the tick. A tick's drain reads the sensor
//! time just before the FIFO's length and again just after it, the second time
//! between two host clock readings, and reads the FIFO's data only then. The
//! two reads bound the sample grid's phase from both sides, and the next drain
//! is aimed at the middle of the bounds on the grid, so they close in a few
//! drains to the time between the two reads (one short transfer). A sample's
//! stamp is the host time of its sensor tick: the phase, then a least-squares
//! line from sensor ticks to host time through the second reads, which forgets
//! old reads with exponential weights (O(1) state, no stored history), so a
//! slow drift of the chip's oscillator is followed without lag. A read whose
//! bracket is wider than 2 ms is not used for host time: the host was
//! preempted inside it. A stamp is never earlier than one already published.
//! Until the bounds exist, each drain waits up to one sample period more than
//! a half FIFO, on a golden-ratio sequence, so the first drains meet the grid
//! at different points whatever the chip's rate.
//!
//! The model publishes only once it has converged: 16 host-timed batches, about
//! 6 s at 200 Hz (a batch drains about every 365 ms), and the phase's bounds
//! within 1 ms of each other. A batch holds its samples until then. The ring
//! keeps the newest of them (146 frames, 0.73 s at 200 Hz), and the older ones
//! are dropped without a loss in health: they never had a time, so they were
//! never part of a batch, as a sample taken before the first tick never was. A
//! batch's records therefore start at most one ring's worth of samples before
//! the model converged, and from there every sample is delivered. On the
//! simulated chip, with host jitter of 0.3 ms and rare 9 ms outliers, the
//! converged stamps were within 0.23 ms of the true time at every rate from 4 %
//! slow to 4 % fast, on 45 s runs and on a run of an hour with the oscillator
//! drifting 0.1 % per hour. Under 9 ms of uniform error on every host clock
//! reading the worst error over 48 runs was 6.3 ms and no gap was over 6.7 ms
//! (the test's bound is 7 ms, and its gap limit is 1.5 periods, 7.5 ms).
//!
//! A chip reset (the sensor time steps back), a sensor time that jumps by more
//! than the host clock agrees with (more than a tenth of the step, and 2 ms),
//! and a FIFO overflow each restart the model. Health names the first two, and
//! the overflow keeps its own message. The stamps keep going forward: nothing
//! is published between the restart and the next convergence.
//!
//! The gyroscope has no sensor time, so it is stamped from the host clock at
//! the tick, as before: the newest sample at the tick's time, each older one
//! back by the measured period. Its error is the tick's own jitter (`Timing`).
//! Stamping it from its own clock is a follow-up.
//!
//! A read's drain has no time of its own, and reads no sensor time. The
//! accelerometer's samples from a read are stamped by the same model once it
//! has converged, and the gyroscope's from the last tick's
//! stamp on its grid. A gyroscope sample drained before any tick has no stamp,
//! and is not queued for a batch.
//!
//! A loss (a FIFO overflow, or a batch that the ring overwrote before it was
//! read) is reported in health from the call that found it, and until the read
//! after that call has ended. A FIFO overflow found by a tick is therefore
//! reported to the next read. The loss messages are `FIFO overflowed, samples
//! lost` and `batch missed samples`.
//!
//! Not in this driver: the gyroscope alignment, which needs the sensor time of
//! the gyroscope (it has none) and a host clock read around each of its
//! transactions, and the auto-calibration, which the caller does (the chip does
//! not calibrate itself).
//!
//! Retries of a failed die are its own ticks, under the backoff above. Reads never
//! step a die: a read that comes before the die is ready returns `AGAIN`.

#![no_std]

mod chip;
#[cfg(test)]
mod clock_tests;
mod sensor_clock;
#[cfg(test)]
mod tests;

use chip::{Failure, Frame, Kind, Setting};
use lemnos_contract_accelerometer::{Accelerometer, Sample as AccelSample};
use lemnos_contract_gyroscope::{Gyroscope, Sample as GyroSample};
use lemnos_core::{
    Config, ConfigError, ConfigValue, Description, Encode, Health, HostClock, HostClockHandle, I2c,
    Item, Key, Lifecycle, MethodId, RefusalKind, Result, Status, Unit, Vector3, Writer, driver,
    status_of,
};
use sensor_clock::{Event, MAX_BRACKET_NS, SensorClock};

/// The accelerometer's 7-bit address with SDO1 low.
pub const DEFAULT_ACCEL_ADDRESS: u8 = 0x18;
/// The gyroscope's 7-bit address with SDO2 low.
pub const DEFAULT_GYRO_ADDRESS: u8 = 0x68;

/// Every setting the driver reads from the board file. Each has a default, so none is required.
const KEYS: &[Key] = &[
    Key::optional("accel_address", lemnos_core::Kind::Int),
    Key::optional("accel_range", lemnos_core::Kind::Int),
    Key::optional("accel_rate", lemnos_core::Kind::Float),
    Key::optional("gyro_address", lemnos_core::Kind::Int),
    Key::optional("gyro_range", lemnos_core::Kind::Int),
    Key::optional("gyro_rate", lemnos_core::Kind::Int),
    Key::optional("fifo", lemnos_core::Kind::Bool),
];

/// Bytes of one batch record (see the contract crates).
const RECORD_LEN: usize = 4 + 8 + (4 + 3 * 4);
/// Ring slots: each die's FIFO depth in frames.
const ACC_RING: usize = chip::ACC_FIFO_FRAMES;
const GYRO_RING: usize = chip::GYRO_FIFO_FRAMES;
/// The waits the chips need between power-up steps, in ns: the dev line's `init` waits, from the
/// datasheets (`lemnos-drivers-bmi088`, `ACC_RESET_US` and the others). The gyroscope's
/// configure step waits 5 ms too, which is more than it needs: conservative, not measured.
const ACC_RESET_NS: u64 = 1_000_000;
const GYR_RESET_NS: u64 = 30_000_000;
const ACC_SUSPEND_NS: u64 = 1_000_000;
const ACC_POWER_NS: u64 = 5_000_000;
/// A failed power-up step is retried after this, doubling each time, capped at 8 s.
const RETRY_FIRST_NS: u64 = 100_000_000;
const RETRY_CAP_NS: u64 = 8_000_000_000;

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
    /// Reset, waiting for the chip to restart.
    Resetting,
    /// Accelerometer only: powered on in suspend mode, waiting for the chip to start.
    PoweringOn,
    /// Waiting for the range and rate to be written.
    Configuring,
    WrongId,
    Transfer,
    /// The FIFO overflowed: samples were lost inside the chip.
    Overflow,
    /// The ring overwrote samples before a batch took them.
    Batch,
    /// The sensor time went back: the sample clock was re-anchored.
    Reset,
    /// The sensor time jumped by more than the host clock agrees with: the sample clock was
    /// re-anchored.
    Jump,
}

fn text(kind: Kind, note: Note) -> &'static str {
    match (kind, note) {
        (Kind::Accel, Note::NotStarted) => "accelerometer: waiting for its first power-up step",
        (Kind::Accel, Note::Resetting) => "accelerometer: waiting for the chip to reset",
        (Kind::Accel, Note::PoweringOn) => "accelerometer: waiting for the chip to power on",
        (Kind::Accel, Note::Configuring) => {
            "accelerometer: waiting for its range and rate to be written"
        }
        (Kind::Accel, Note::WrongId) => "accelerometer: wrong chip id, retrying",
        (Kind::Accel, Note::Transfer) => "accelerometer: transfer failed, retrying",
        (Kind::Accel, Note::Overflow) => "accelerometer: FIFO overflowed, samples lost",
        (Kind::Accel, Note::Batch) => "accelerometer: batch missed samples",
        (Kind::Accel, Note::Reset) => {
            "accelerometer: its sensor time went back; its sample clock was re-anchored"
        }
        (Kind::Accel, Note::Jump) => {
            "accelerometer: its sensor time jumped; its sample clock was re-anchored"
        }
        (Kind::Gyro, Note::NotStarted) => "gyroscope: waiting for its first power-up step",
        (Kind::Gyro, Note::Resetting) => "gyroscope: waiting for the chip to reset",
        (Kind::Gyro, Note::PoweringOn) => "gyroscope: waiting for the chip to power on",
        (Kind::Gyro, Note::Configuring) => {
            "gyroscope: waiting for its range and rate to be written"
        }
        (Kind::Gyro, Note::WrongId) => "gyroscope: wrong chip id, retrying",
        (Kind::Gyro, Note::Transfer) => "gyroscope: transfer failed, retrying",
        (Kind::Gyro, Note::Overflow) => "gyroscope: FIFO overflowed, samples lost",
        (Kind::Gyro, Note::Batch) => "gyroscope: batch missed samples",
        (Kind::Gyro, Note::Reset) => "gyroscope: its clock went back",
        (Kind::Gyro, Note::Jump) => "gyroscope: its clock jumped",
    }
}

/// One sample in the ring. `seq` is its number, `u64::MAX` when the slot holds
/// no sample.
#[derive(Debug, Clone, Copy)]
struct Entry {
    seq: u64,
    at_ns: u64,
    raw: [i16; 3],
    /// Stamped: a batch may take it. An unstamped sample holds the batch until it is stamped.
    ready: bool,
}

const EMPTY: Entry = Entry {
    seq: u64::MAX,
    at_ns: 0,
    raw: [0; 3],
    ready: false,
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
    /// The accelerometer's sample clock (the gyroscope keeps `timing`).
    clock: SensorClock,
    /// The next sample to stamp. Samples before it are stamped, or dropped unstamped.
    stamped: u64,
    /// When the next tick drain is due, host ns (`None`: not draining).
    due: Option<u64>,
    /// The accelerometer's last extra wait before a drain, in 2^-32 of a sample period (see
    /// `spread`).
    spread: u32,
    /// A loss a drain or batch found, not yet shown to a read.
    pending: Option<Note>,
    /// The loss a read shows in health.
    shown: Option<Note>,
    /// The next power-up step (or retry) is due, host ns. `None`: now.
    up_due: Option<u64>,
    /// The current retry wait after a failed step, ns. Zero when the last step worked.
    retry_ns: u64,
    /// A transfer failed and the die has not been ready since: a read reports the failure, so the
    /// host counts it, and not `AGAIN` (which is only the wait for a first start).
    failed: bool,
}

impl<const RING: usize> Die<RING> {
    #[inline(always)]
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
            clock: SensorClock::new(setting.period_ns),
            stamped: 0,
            due: None,
            spread: 0,
            pending: None,
            shown: None,
            up_due: None,
            retry_ns: 0,
            failed: false,
        }
    }

    /// What a read says while the die is not ready: the failure, when one happened, else `AGAIN`.
    fn not_ready(&self) -> Status {
        if self.failed {
            Status::IO
        } else {
            Status::AGAIN
        }
    }

    fn set(&mut self, phase: Phase) {
        self.phase = phase;
        let note = match phase {
            Phase::Off => Note::NotStarted,
            Phase::Reset => Note::Resetting,
            Phase::Power => Note::PoweringOn,
            Phase::Configure => Note::Configuring,
            Phase::Ready => return,
        };
        self.note = text(self.kind, note);
    }

    /// How long the chip needs after the step that brought it to its current phase.
    fn gap(&self) -> u64 {
        match (self.phase, self.kind) {
            (Phase::Reset, Kind::Accel) => ACC_RESET_NS,
            (Phase::Reset, Kind::Gyro) => GYR_RESET_NS,
            (Phase::Power, _) => ACC_SUSPEND_NS,
            (Phase::Configure, _) => ACC_POWER_NS,
            _ => 0,
        }
    }

    /// One power-up step, once its time has come. Returns when the next step, or the retry of
    /// this one, is due.
    fn step_up(&mut self, now: u64, bus: &mut I2c, clock: &dyn HostClock) -> Option<u64> {
        if let Some(due) = self.up_due
            && now < due
        {
            return Some(due);
        }
        self.up_due = None;
        match self.power(bus) {
            Ok(true) => {
                self.retry_ns = 0;
                self.failed = false;
                self.tick(now, bus, clock)
            }
            Ok(false) => {
                self.retry_ns = 0;
                let due = now.saturating_add(self.gap());
                self.up_due = Some(due);
                Some(due)
            }
            Err(failure) => {
                self.fail(&failure);
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

    /// A read's sample: the die's newest one. `None` while the die is still powering up, which a
    /// read does not step (the tick does).
    fn sample(&mut self, bus: &mut I2c) -> chip::Step<Option<[i16; 3]>> {
        if self.phase != Phase::Ready {
            return Ok(None);
        }
        if self.fifo {
            self.drain(bus, None, None)?;
            Ok(self.last)
        } else {
            chip::read_axes(bus, self.address, self.kind).map(Some)
        }
    }

    /// Drains the FIFO into the ring, stamps what it took, and records any loss.
    /// `now` is the tick's time, for the gyroscope's stamps; `clock` is the host clock a tick
    /// reads the accelerometer's sensor time against. Both are `None` for a read's drain, which has
    /// no time of its own.
    fn drain(
        &mut self,
        bus: &mut I2c,
        now: Option<u64>,
        clock: Option<&dyn HostClock>,
    ) -> chip::Step<()> {
        let first = self.seq;
        // A tick's accelerometer drain is timed: the sensor time is read just before the FIFO's
        // length and again just after it, the second time between two host clock readings, and
        // only then is the FIFO read (see `sensor_clock.rs`).
        let timed = match clock {
            Some(clock) if self.kind == Kind::Accel => Some(clock),
            _ => None,
        };
        let mut sensed = None;
        let mut counted = None;
        if let Some(clock) = timed {
            let raw = chip::accel_sensortime(bus, self.address)?;
            let event = self.clock.before(raw);
            self.note_event(event);
            counted = Some(chip::accel_fifo_bytes(bus, self.address)?);
            sensed = Some(sense(bus, self.address, clock)?);
        }
        let (overflow, result) = self.pull(bus, counted);
        if overflow {
            self.pending = Some(Note::Overflow);
            // The skipped frames keep their numbers, but the sample clock is not trusted across a
            // loss: it is restarted, and its samples wait for it (see `publish`).
            if self.kind == Kind::Accel {
                self.clock.restart();
            }
        }
        if self.kind != Kind::Accel {
            self.stamp(first, now);
            return result;
        }
        if let Some((raw, host)) = sensed {
            let newest = self.seq.checked_sub(1);
            let event = self.clock.after(raw, newest, result.is_ok(), host);
            self.note_event(event);
        }
        // A read's drain has no time of its own: its samples wait for the model, or take it.
        self.publish();
        result
    }

    /// Records a restart of the sample clock, by name.
    fn note_event(&mut self, event: Option<Event>) {
        if let Some(event) = event {
            self.pending = Some(match event {
                Event::Reset => Note::Reset,
                Event::Jump => Note::Jump,
            });
        }
    }

    /// Stamps the samples the clock can now stamp, in order, from the first unstamped one. Stops at
    /// the first it cannot stamp yet, which holds it and every later sample for the batches.
    fn publish(&mut self) {
        let oldest = self.seq.saturating_sub(RING as u64);
        if self.stamped < oldest {
            // The ring overwrote samples the clock never stamped. They were never part of a batch,
            // so a batch that had taken everything stamped skips them without a loss: the loss is
            // the restart's, which said so (an overflow, a reset, a jump), or the first
            // convergence's, before which no sample has a time.
            if self.batch_next >= self.stamped {
                self.batch_next = self.batch_next.max(oldest);
            }
            self.stamped = oldest;
        }
        let mut n = self.stamped;
        while n < self.seq {
            match self.clock.publish(n) {
                Some(at_ns) => self.set_stamp(n, at_ns),
                None => break,
            }
            n += 1;
        }
        self.stamped = n;
    }

    /// Takes the FIFO's frames into the ring. Returns whether the FIFO
    /// overflowed, and the transfer's result: what was taken is kept either way.
    /// `counted` is the accelerometer FIFO's length when the drain has read it already.
    fn pull(&mut self, bus: &mut I2c, counted: Option<usize>) -> (bool, chip::Step<()>) {
        let (address, kind) = (self.address, self.kind);
        let mut overflow = false;
        let result = match kind {
            Kind::Accel => match counted {
                Some(bytes) => Ok(bytes),
                None => chip::accel_fifo_bytes(bus, address),
            }
            .and_then(|bytes| {
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
            ready: false,
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
                slot.ready = true;
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
                if !entry.ready || written == capacity {
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
    fn tick(&mut self, now: u64, bus: &mut I2c, clock: &dyn HostClock) -> Option<u64> {
        if self.phase != Phase::Ready {
            return self.step_up(now, bus, clock);
        }
        if !self.fifo {
            self.due = None;
            return None;
        }
        if self.due.is_none_or(|due| now >= due) {
            if let Err(failure) = self.drain(bus, Some(now), Some(clock)) {
                self.fail(&failure);
                return self.retry_at(now);
            }
            let half_depth = RING as u64 * self.setting.period_ns / 2;
            let earliest = now.saturating_add(half_depth);
            let spread = self.spread();
            let next = match self.kind {
                // Aimed at the sample clock's phase once it has one, so the drain splits its bounds.
                Kind::Accel => self
                    .clock
                    .aim(earliest)
                    .unwrap_or(earliest.saturating_add(spread)),
                Kind::Gyro => earliest,
            };
            self.due = Some(next);
        }
        self.due
    }

    /// The accelerometer's extra wait before its next drain, in `[0, period)`, until the sample
    /// clock has the bounds to aim its drains with; none for the gyroscope. The first bounds come
    /// from drains at any point of the sample grid, and drains a fixed time apart cycle through a
    /// few points when that time is close to a simple fraction of periods. Drain `j` waits `j * golden`
    /// (mod one period) more, so after `k` drains the offset on the grid has moved by
    /// `golden * k * (k + 1) / 2` periods plus the beat: a quadratic with an irrational step,
    /// spread evenly over the period whatever the chip's rate is.
    fn spread(&mut self) -> u64 {
        if self.kind != Kind::Accel {
            return 0;
        }
        /// 2^32 / the golden ratio.
        const GOLDEN: u32 = 2_654_435_769;
        self.spread = self.spread.wrapping_add(GOLDEN);
        (u64::from(self.spread) * self.setting.period_ns) >> 32
    }

    /// Records a failure. A wrong chip, or a failure while measuring, sends the
    /// die back to power-up for a later read. Any other failure is retried in
    /// place.
    fn fail(&mut self, failure: &Failure) -> Status {
        self.failed = true;
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
            self.up_due = None;
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

/// The accelerometer's sensor time, and the host time of the read: the middle of the two host clock
/// readings around it. A bracket wider than `MAX_BRACKET_NS` (the host was preempted inside the
/// read) gives no host time.
fn sense(bus: &mut I2c, address: u8, clock: &dyn HostClock) -> chip::Step<(u32, Option<u64>)> {
    let before = clock.now_ns();
    let raw = chip::accel_sensortime(bus, address)?;
    let after = clock.now_ns();
    let host = (after >= before && after - before <= MAX_BRACKET_NS)
        .then(|| before + (after - before) / 2);
    Ok((raw, host))
}

/// A read of one die: its sample in SI units, `AGAIN` while it powers up, or
/// the failure's status.
fn read_die<const RING: usize>(die: &mut Die<RING>, bus: &mut I2c) -> Result<Vector3> {
    let out = match die.sample(bus) {
        Ok(Some(raw)) => Ok(chip::to_vector(raw, die.setting.scale)),
        Ok(None) => Err(die.not_ready()),
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
        // Not ready: the tick steps the power-up, so a batch says `AGAIN` and steps nothing.
        if die.phase != Phase::Ready {
            Err(die.not_ready())
        } else {
            match die.drain(bus, None, None) {
                Err(failure) => Err(die.fail(&failure)),
                Ok(()) => Ok(die.deliver(into, record)),
            }
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
    /// The host clock the accelerometer's sensor time is read against.
    #[needs]
    clock: HostClockHandle,
    accel: Die<ACC_RING>,
    gyro: Die<GYRO_RING>,
}

/// The bind's one ID read, of the accelerometer die (see the crate notes): absent when the die does
/// not acknowledge its address, failed when it answers another ID (named with the one expected),
/// and failed on any other bus fault.
fn identify_at_bind(bus: &mut I2c, address: u8, cfg: &Config<'_>) -> Result<()> {
    match chip::probe(bus, address, Kind::Accel) {
        chip::Probe::Found => Ok(()),
        chip::Probe::Wrong { found, expected } => Err(cfg.refuse(
            Status::NOT_SUPPORTED,
            RefusalKind::Failed,
            format_args!(
                "the accelerometer at {address:#04x} answers ID {found:#04x}, expected {expected:#04x}"
            ),
        )),
        chip::Probe::Absent => Err(cfg.refuse(
            Status::NO_DEVICE,
            RefusalKind::Absent,
            format_args!("no device at {address:#04x}: nothing answers at its address"),
        )),
        chip::Probe::Fault(kind) => Err(cfg.refuse(
            status_of(kind),
            RefusalKind::Failed,
            format_args!("the accelerometer at {address:#04x} did not answer its ID read"),
        )),
    }
}

impl Lifecycle for Bmi088 {
    type Needs = Bmi088Needs;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        // Each range is checked at a rate the other setting never refuses (100 Hz for the
        // accelerometer, 2000 Hz for the gyroscope), so a refusal names the setting that is wrong.
        if chip::accel_setting(cfg.int("accel_range").unwrap_or(6), 100.0).is_none() {
            return Err(ConfigError::new("accel_range", "must be 3, 6, 12 or 24"));
        }
        if chip::accel_setting(
            cfg.int("accel_range").unwrap_or(6),
            cfg.float("accel_rate").unwrap_or(100.0),
        )
        .is_none()
        {
            return Err(ConfigError::new(
                "accel_rate",
                "must be 12.5, 25, 50, 100, 200, 400, 800 or 1600",
            ));
        }
        if chip::gyro_setting(cfg.int("gyro_range").unwrap_or(2000), 2000).is_none() {
            return Err(ConfigError::new(
                "gyro_range",
                "must be 125, 250, 500, 1000 or 2000",
            ));
        }
        if chip::gyro_setting(2000, cfg.int("gyro_rate").unwrap_or(2000)).is_none() {
            return Err(ConfigError::new(
                "gyro_rate",
                "must be 100, 200, 400, 1000 or 2000",
            ));
        }
        for key in ["accel_address", "gyro_address"] {
            if let Some(address) = cfg.int(key)
                && !(0x08..=0x77).contains(&address)
            {
                return Err(ConfigError::new(
                    key,
                    "must be a 7-bit address from 0x08 to 0x77",
                ));
            }
        }
        Ok(())
    }

    fn start(needs: Bmi088Needs, cfg: &Config<'_>) -> Result<Self> {
        let mut place = None;
        Self::start_in(&mut place, needs, cfg)?;
        place.ok_or(Status::IO)
    }

    /// Builds the driver where it will live. Its two FIFO rings are most of 6 KB: on a
    /// microcontroller they are filled in the driver's slot and never cross the stack.
    fn start_in(place: &mut Option<Self>, needs: Bmi088Needs, cfg: &Config<'_>) -> Result<()> {
        let fifo = match cfg.get("fifo") {
            None => false,
            Some(ConfigValue::Bool(on)) => on,
            Some(_) => {
                return Err(cfg.refuse(
                    Status::INVALID_ARGUMENT,
                    RefusalKind::Failed,
                    format_args!("the setting fifo must be true or false"),
                ));
            }
        };
        let accel = chip::accel_setting(
            int(cfg, "accel_range", 6)?,
            number(cfg, "accel_rate", 100.0)?,
        )
        .ok_or_else(|| {
            cfg.refuse(
                Status::INVALID_ARGUMENT,
                RefusalKind::Failed,
                format_args!("the accelerometer range and rate cannot be used together"),
            )
        })?;
        let gyro = chip::gyro_setting(int(cfg, "gyro_range", 2000)?, int(cfg, "gyro_rate", 2000)?)
            .ok_or_else(|| {
                cfg.refuse(
                    Status::INVALID_ARGUMENT,
                    RefusalKind::Failed,
                    format_args!("the gyroscope range and rate cannot be used together"),
                )
            })?;
        let accel_address = address(cfg, "accel_address", DEFAULT_ACCEL_ADDRESS)?;
        let gyro_address = address(cfg, "gyro_address", DEFAULT_GYRO_ADDRESS)?;
        let mut bus = needs.bus;
        identify_at_bind(&mut bus, accel_address, cfg)?;
        // Nothing fallible is left, and nothing large has been made yet.
        *place = Some(Self {
            bus,
            clock: needs.clock,
            accel: Die::new(Kind::Accel, accel_address, fifo, accel),
            gyro: Die::new(Kind::Gyro, gyro_address, fifo, gyro),
        });
        Ok(())
    }

    fn describe(&self, out: &mut Description) {
        // Real numbers in base units (m/s², rad/s): the values `read` returns are f32 in those, so the
        // exponent is 0. An exponent of -3 showed 1 g as 0.00981 m/s².
        // Each axis spans the die's full scale, one count is a step.
        let accel = &self.accel.setting;
        let gyro = &self.gyro.setting;
        for (name, field) in [
            ("acceleration.x", "x"),
            ("acceleration.y", "y"),
            ("acceleration.z", "z"),
        ] {
            out.add(
                Item::value(name, ACCEL_READ)
                    .unit(Unit::MetrePerSecondSquared, 0)
                    .field(field)
                    .group("acceleration")
                    .real_range(-accel.full_scale(), accel.full_scale())
                    .real_step(accel.step()),
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
                    .field(field)
                    .group("angular_rate")
                    .real_range(-gyro.full_scale(), gyro.full_scale())
                    .real_step(gyro.step()),
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
        let accel = self.accel.tick(now_ns, &mut self.bus, &*self.clock);
        let gyro = self.gyro.tick(now_ns, &mut self.bus, &*self.clock);
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
        Some(_) => Err(cfg.refuse(
            Status::INVALID_ARGUMENT,
            RefusalKind::Failed,
            format_args!("the setting {key} must be a whole number"),
        )),
    }
}

/// A number setting (an integer is accepted), or `default` when absent.
fn number(cfg: &Config<'_>, key: &str, default: f64) -> Result<f64> {
    match cfg.get(key) {
        None => Ok(default),
        Some(ConfigValue::Int(value)) => Ok(value as f64),
        Some(ConfigValue::Float(value)) => Ok(value),
        Some(_) => Err(cfg.refuse(
            Status::INVALID_ARGUMENT,
            RefusalKind::Failed,
            format_args!("the setting {key} must be a number"),
        )),
    }
}

/// A 7-bit address, or `default` when absent.
fn address(cfg: &Config<'_>, key: &str, default: u8) -> Result<u8> {
    // The range is checked by the driver's `check_config`, so this only narrows the type.
    u8::try_from(int(cfg, key, i64::from(default))?).map_err(|_| {
        cfg.refuse(
            Status::INVALID_ARGUMENT,
            RefusalKind::Failed,
            format_args!("the setting {key} must be a 7-bit address"),
        )
    })
}
