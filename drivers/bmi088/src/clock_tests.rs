//! The accelerometer's sample clock, end to end. A simulated chip runs its own oscillator, and the
//! driver reads it through its own ticks, FIFO batches and sensor time. The host clock's bracket
//! around each sensor read, and the tick's own time, carry the host's jitter. Every stamp is
//! checked against the true host time of its sample, which the oscillator gives exactly.
//!
//! Shared with `tests.rs`, so the batch checks there measure the same model.

extern crate std;

use super::*;
use core::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use lemnos_contract_accelerometer::Sample as AccelSample;
use lemnos_core::{
    Config, ConfigValue, Decode, Driver, Held, HostClock, LocalCaller, Provider, Reader, Resources,
    Tag,
};
use lemnos_hal::ErrorKind;
use lemnos_hal::i2c::{ErrorType, I2c as HalI2c, Operation};
use lemnos_sim::{FifoLevel, Layout, SimBoard, SimI2c};
use std::boxed::Box;
use std::sync::Arc;
use std::vec::Vec;

const ACCEL: u8 = DEFAULT_ACCEL_ADDRESS;
const GYRO: u8 = DEFAULT_GYRO_ADDRESS;
const ACC_ID: u8 = 0x00;
const ACC_FIFO_LENGTH: u8 = 0x24;
const ACC_FIFO_DATA: u8 = 0x26;
const GYR_ID: u8 = 0x00;
const GYR_FIFO_STATUS: u8 = 0x0e;
const GYR_FIFO_DATA: u8 = 0x3f;

/// The sensor time register, read as three bytes.
const SENSORTIME: u8 = 0x18;
/// Sensor ticks per second: one tick is 39.0625 us.
pub(crate) const TICKS_PER_S: f64 = 25_600.0;
/// Sensor ticks per sample at 200 Hz.
pub(crate) const GRID: f64 = 128.0;
/// The driver's batch at 200 Hz: 73 samples, 365 ms. Its tick drains once this has passed.
pub(crate) const BATCH_NS: u64 = 365_000_000;
/// The accelerometer's counts to m/s² at 6 g.
pub(crate) const SCALE: f64 = 6.0 * 9.806_65 / 32_768.0;
/// The host time between a drain's first sensor time read and its FIFO length read, and between
/// that and its second sensor time read: one short transfer each.
pub(crate) const LEAD_NS: u64 = 150_000;
pub(crate) const LATENCY_NS: u64 = 150_000;
/// Where the harness starts, on the chip's own clock. The first batch is one batch later.
pub(crate) const START_NS: u64 = 100_000_000;

/// A chip's oscillator: its rate is `1 + eps + drift * t` (t in seconds), so it is `eps` fast at
/// the start, and its rate changes by `drift` per second.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Oscillator {
    pub(crate) eps: f64,
    pub(crate) drift: f64,
}

impl Oscillator {
    /// Sensor ticks at host time `t` seconds.
    pub(crate) fn ticks(&self, t: f64) -> f64 {
        TICKS_PER_S * ((1.0 + self.eps) * t + 0.5 * self.drift * t * t)
    }

    /// The host time (seconds) at which the chip reaches `ticks`.
    pub(crate) fn time_of(&self, ticks: f64) -> f64 {
        let c = ticks / TICKS_PER_S;
        let r = 1.0 + self.eps;
        if self.drift == 0.0 {
            c / r
        } else {
            (-r + (r * r + 2.0 * self.drift * c).sqrt()) / self.drift
        }
    }

    /// The true host time, in ns, of chip sample `c`. Sample `c` is made at tick `GRID * (c + 1)`.
    pub(crate) fn sample_ns(&self, c: u64) -> f64 {
        self.time_of(GRID * (c + 1) as f64) * 1e9
    }
}

/// The chip and the host clock's readings, shared with the bus and the clock the driver holds.
pub(crate) struct Chip {
    osc: Oscillator,
    /// The true host time of the drain's first sensor read, ns, just before its length read.
    drain_ns: AtomicU64,
    /// The true host time of the drain's second sensor read, ns, which the host clock brackets.
    at_ns: AtomicU64,
    /// Sensor reads in the drain in progress.
    reads: AtomicU32,
    /// The accelerometer registers read, in order, while `logging` is set.
    log: std::sync::Mutex<Vec<u8>>,
    logging: core::sync::atomic::AtomicBool,
    /// Sensor ticks added to the chip's count: a reset steps it back, a jump forward.
    shift: AtomicI64,
    /// The host clock's offsets for the next bracket: before the read, and after it.
    pre: AtomicI64,
    post: AtomicI64,
    calls: AtomicU32,
}

impl Chip {
    fn sensor_time(&self) -> u32 {
        let at = if self.reads.fetch_add(1, Ordering::SeqCst).is_multiple_of(2) {
            &self.drain_ns
        } else {
            &self.at_ns
        };
        let t = at.load(Ordering::SeqCst) as f64 * 1e-9;
        let ticks = self.osc.ticks(t).floor() as i64 + self.shift.load(Ordering::SeqCst);
        ticks.rem_euclid(1 << 24) as u32
    }

    /// The host clock as the driver reads it: the first reading of a bracket, then the second.
    fn host_now(&self) -> u64 {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let off = if n.is_multiple_of(2) {
            self.pre.load(Ordering::SeqCst)
        } else {
            self.post.load(Ordering::SeqCst)
        };
        (self.at_ns.load(Ordering::SeqCst) as i64 + off).max(0) as u64
    }
}

struct HostClk(Arc<Chip>);

impl HostClock for HostClk {
    fn now_ns(&self) -> u64 {
        self.0.host_now()
    }
}

/// The simulated chip's I2C bus, with the sensor time answered from the oscillator.
struct Clocked {
    sim: SimI2c,
    chip: Arc<Chip>,
}

impl ErrorType for Clocked {
    type Error = ErrorKind;
}

impl HalI2c for Clocked {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> core::result::Result<(), ErrorKind> {
        if address == ACCEL
            && self.chip.logging.load(Ordering::SeqCst)
            && let [Operation::Write(reg), Operation::Read(_)] = operations
            && let Some(&first) = reg.first()
        {
            self.chip.log.lock().unwrap().push(first);
        }
        if let [Operation::Write(reg), Operation::Read(buf)] = operations
            && address == ACCEL
            && reg.len() == 1
            && reg[0] == SENSORTIME
            && buf.len() == 3
        {
            let raw = self.chip.sensor_time().to_le_bytes();
            buf.copy_from_slice(&raw[..3]);
            return Ok(());
        }
        self.sim.transaction(address, operations)
    }
}

/// Binds the driver to the simulated chip: the bus and the host clock are the chip's.
struct Rig {
    _board: SimBoard,
    sim: SimI2c,
    chip: Arc<Chip>,
}

impl Provider for Rig {
    fn provided(&self, _need: &str) -> bool {
        true
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> lemnos_core::Result<LocalCaller> {
        Err(lemnos_core::Status::NOT_SUPPORTED)
    }

    fn i2c(&mut self, _need: &str) -> lemnos_core::Result<lemnos_core::I2c> {
        Ok(lemnos_core::I2c(Held::boxed(Box::new(Clocked {
            sim: self.sim.clone(),
            chip: Arc::clone(&self.chip),
        }))))
    }

    fn host_clock(&mut self, _need: &str) -> lemnos_core::Result<lemnos_core::HostClockHandle> {
        Ok(lemnos_core::HostClockHandle::boxed(Box::new(HostClk(
            Arc::clone(&self.chip),
        ))))
    }
}

/// A chip fault to inject into one batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// The FIFO overflowed: `n` samples were lost inside the chip.
    Overflow(u8),
    /// The chip reset: its sensor time starts again from zero.
    Reset,
    /// The sensor time jumped forward by this many ticks.
    Jump(i64),
}

/// What one batch does on the host side.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Plan {
    /// How late the host ticks after the drain is due, as its size: a host ticks on time, or
    /// late, never early.
    pub(crate) tick_jitter_ns: i64,
    /// The host clock's offsets for the sensor read's bracket, before and after it.
    pub(crate) pre_ns: i64,
    pub(crate) post_ns: i64,
    /// How much earlier than the drain the time the host hands the tick is: a host reads its clock
    /// once and then ticks its devices one after another.
    pub(crate) report_early_ns: u64,
    pub(crate) fault: Option<Fault>,
}

impl Plan {
    /// A clean batch: no jitter, a clean bracket, no fault.
    pub(crate) fn clean() -> Self {
        Self {
            tick_jitter_ns: 0,
            pre_ns: 0,
            post_ns: 0,
            report_early_ns: 0,
            fault: None,
        }
    }
}

/// One delivered sample: its driver sequence number, and its stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Delivered {
    pub(crate) seq: u64,
    pub(crate) at_ns: u64,
}

/// The driver on the simulated chip, driven one batch at a time.
pub(crate) struct Harness {
    pub(crate) dev: Bmi088,
    sim: SimI2c,
    pub(crate) chip: Arc<Chip>,
    /// The chip's sample index of the driver's sequence number 0.
    pub(crate) base: u64,
    /// The driver's next sequence number (frames pushed, samples the chip dropped included).
    pub(crate) seq: u64,
    /// The true time of the last batch's drain.
    pub(crate) t_ns: u64,
    /// The accelerometer's next drain is due at this time, as the driver last said.
    accel_due_ns: u64,
    /// The batch index, from 0.
    pub(crate) batch: u64,
    /// Health after the last tick, before its read: the note a batch raised.
    pub(crate) note: Option<&'static str>,
}

impl Harness {
    /// A driver with an accelerometer at `rate` Hz and a gyroscope, both ready, on a chip whose
    /// oscillator is `osc`.
    pub(crate) fn new(osc: Oscillator, rate: i64) -> Self {
        let board = SimBoard::new();
        let sim = board.add_i2c("bus");
        sim.attach(ACCEL, Layout::Bytes);
        sim.set_bytes(ACCEL, ACC_ID, &[0x1e]);
        sim.attach(GYRO, Layout::Bytes);
        sim.set_bytes(GYRO, GYR_ID, &[0x0f]);
        sim.add_fifo(
            ACCEL,
            ACC_FIFO_DATA,
            FifoLevel::Bytes(ACC_FIFO_LENGTH),
            1024,
        );
        sim.add_fifo(
            GYRO,
            GYR_FIFO_DATA,
            FifoLevel::Frames {
                register: GYR_FIFO_STATUS,
                frame: 6,
            },
            600,
        );
        let chip = Arc::new(Chip {
            osc,
            drain_ns: AtomicU64::new(0),
            at_ns: AtomicU64::new(0),
            reads: AtomicU32::new(0),
            log: std::sync::Mutex::new(Vec::new()),
            logging: core::sync::atomic::AtomicBool::new(false),
            shift: AtomicI64::new(0),
            pre: AtomicI64::new(0),
            post: AtomicI64::new(0),
            calls: AtomicU32::new(0),
        });
        let mut rig = Rig {
            _board: board,
            sim: sim.clone(),
            chip: Arc::clone(&chip),
        };
        let entries = [
            ("fifo", ConfigValue::Bool(true)),
            ("accel_rate", ConfigValue::Int(rate)),
        ];
        let mut res = Resources::new(&mut rig, None);
        let mut dev = Bmi088::bind(&mut res, &Config::new(&entries)).unwrap();
        let mut step = 0u64;
        while Driver::health(&dev) != Health::Ok {
            Driver::tick(&mut dev, step * 1_000_000);
            step += 1;
            assert!(step < 10_000, "the device never came up");
        }
        // The chip's samples before the start are not in the FIFO: numbering starts with the first.
        let base = osc.ticks(START_NS as f64 * 1e-9) as u64 / GRID as u64;
        Self {
            dev,
            sim,
            chip,
            base,
            seq: 0,
            t_ns: START_NS,
            accel_due_ns: START_NS + BATCH_NS,
            batch: 0,
            note: None,
        }
    }

    /// The true host time of driver sequence number `seq`.
    pub(crate) fn truth(&self, seq: u64) -> f64 {
        self.chip.osc.sample_ns(self.base + seq)
    }

    /// Runs one batch: the chip's FIFO fills to the drain time, the tick drains it and reads the
    /// sensor time, then a batch read takes what the ring holds. Returns what the batch delivered.
    pub(crate) fn step(&mut self, plan: Plan) -> Vec<Delivered> {
        self.fill_and_tick(plan);
        self.take(160)
    }

    /// The batch's half of [`Harness::step`]: the FIFO fills, and the tick drains it.
    pub(crate) fn fill_and_tick(&mut self, plan: Plan) {
        // The host ticks once the accelerometer's drain is due, late by the host's jitter. The
        // tick's drain reads the FIFO's length a short transfer later: the chip has made every
        // sample up to that time.
        let tick = self.accel_due_ns.max(self.t_ns + 1) + plan.tick_jitter_ns.unsigned_abs();
        let drain = tick + LEAD_NS;
        self.t_ns = drain;
        let read = drain + LATENCY_NS;
        let produced = (self.chip.osc.ticks(drain as f64 * 1e-9) / GRID).floor() as u64;
        let mut fifo = Vec::new();
        match plan.fault {
            Some(Fault::Overflow(lost)) => {
                // Frames the chip dropped: a skip frame, which takes their numbers.
                fifo.extend_from_slice(&[0x40, lost]);
                self.seq += u64::from(lost);
            }
            Some(Fault::Reset) => {
                // The sensor time starts again from zero at this drain.
                let now = self.chip.osc.ticks(drain as f64 * 1e-9).floor() as i64;
                self.chip.shift.store(-now, Ordering::SeqCst);
            }
            Some(Fault::Jump(ticks)) => {
                self.chip.shift.fetch_add(ticks, Ordering::SeqCst);
            }
            None => {}
        }
        // A data frame is 7 bytes: its header, then X (the sequence number's low 16 bits), Y and Z.
        while self.base + self.seq < produced {
            fifo.push(0x84);
            let x = (self.seq as u16) as i16;
            fifo.extend_from_slice(&x.to_le_bytes());
            fifo.extend_from_slice(&[0, 0, 0, 0]);
            self.seq += 1;
        }
        if !fifo.is_empty() {
            self.sim.fifo_push(ACCEL, &fifo);
        }
        // The drain reads the sensor time just before the FIFO's length (which counts what the
        // chip made by `drain`), then again just after it, between two host clock readings.
        self.chip.drain_ns.store(tick, Ordering::SeqCst);
        self.chip.reads.store(0, Ordering::SeqCst);
        self.chip.at_ns.store(read, Ordering::SeqCst);
        self.chip.pre.store(plan.pre_ns, Ordering::SeqCst);
        self.chip.post.store(plan.post_ns, Ordering::SeqCst);
        self.chip.calls.store(0, Ordering::SeqCst);
        // The next drain is when the driver says it is due.
        let _ = Driver::tick(&mut self.dev, tick.saturating_sub(plan.report_early_ns));
        self.accel_due_ns = self.dev.accel.due.expect("a drain is due");
        self.note = match Driver::health(&self.dev) {
            Health::Degraded(text) => Some(text),
            _ => None,
        };
        self.batch += 1;
    }

    /// A batch read of at most `records` records: what the ring holds that the caller has not had.
    pub(crate) fn take(&mut self, records: usize) -> Vec<Delivered> {
        let mut delivered = Vec::new();
        let mut buf = std::vec![0u8; RECORD_LEN * records];
        let n = Accelerometer::read_batch(&mut self.dev, &mut buf).unwrap();
        let newest = self.seq;
        for record in buf[..n].as_chunks::<RECORD_LEN>().0 {
            let sample = AccelSample::decode(&mut Reader::new(record)).unwrap();
            // The value carries the sequence number's low 16 bits: the newest one it can be is the
            // last pushed, so the number is the nearest one at or below it.
            let x = (sample.value.x as f64 / SCALE).round() as i64 as u16;
            let back = (newest.wrapping_sub(1).wrapping_sub(u64::from(x))) & 0xffff;
            let seq = newest - 1 - back;
            delivered.push(Delivered {
                seq,
                at_ns: sample.at_ns,
            });
        }
        delivered
    }

    /// The newest sample a read returns, in m/s², and its sequence number.
    pub(crate) fn read_newest(&mut self) -> (f64, u64) {
        let sample = Accelerometer::read(&mut self.dev).unwrap();
        let newest = self.seq;
        let x = (sample.x as f64 / SCALE).round() as i64 as u16;
        let back = (newest.wrapping_sub(1).wrapping_sub(u64::from(x))) & 0xffff;
        (sample.x as f64, newest - 1 - back)
    }
}

/// A deterministic jitter in `[-spread, spread]`: a small LCG, so every run is the same.
pub(crate) struct Jitter(u64);

impl Jitter {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next(&mut self, spread: i64) -> i64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let unit = (self.0 >> 11) as f64 / (1u64 << 53) as f64;
        ((unit * 2.0 - 1.0) * spread as f64) as i64
    }
}

/// The error of each delivered stamp, in ns, against the true time of its sample.
pub(crate) fn errors(h: &Harness, delivered: &[Delivered]) -> Vec<f64> {
    delivered
        .iter()
        .map(|d| d.at_ns as f64 - h.truth(d.seq))
        .collect()
}

/// The note a reset raises, as health shows it.
pub(crate) const RESET_NOTE: &str =
    "accelerometer: its sensor time went back; its sample clock was re-anchored";
/// The note a jump raises.
pub(crate) const JUMP_NOTE: &str =
    "accelerometer: its sensor time jumped; its sample clock was re-anchored";
/// The note a FIFO overflow raises.
pub(crate) const OVERFLOW_NOTE: &str = "accelerometer: FIFO overflowed, samples lost";

/// The stamps of consecutive delivered samples never go back, and never leave a gap of more than
/// one and a half sample periods (7.5 ms at 200 Hz) between two that were taken one apart.
pub(crate) fn no_going_back(delivered: &[Delivered]) -> Option<(u64, u64)> {
    delivered
        .windows(2)
        .find(|w| w[1].at_ns < w[0].at_ns)
        .map(|w| (w[0].at_ns, w[1].at_ns))
}

/// The largest gap between the stamps of consecutive samples, ns.
pub(crate) fn largest_gap(delivered: &[Delivered]) -> u64 {
    delivered
        .windows(2)
        .map(|w| w[1].at_ns.saturating_sub(w[0].at_ns))
        .max()
        .unwrap_or(0)
}

/// Runs `batches` batches with `plan(batch)` for each, collecting what each delivered.
pub(crate) fn run(
    h: &mut Harness,
    batches: u64,
    mut plan: impl FnMut(u64) -> Plan,
) -> Vec<(u64, Delivered)> {
    let mut out = Vec::new();
    for b in 0..batches {
        let p = plan(b);
        for d in h.step(p) {
            out.push((b, d));
        }
    }
    out
}

/// The first batch that delivered anything.
pub(crate) fn first_delivery(delivered: &[(u64, Delivered)]) -> Option<u64> {
    delivered.first().map(|(b, _)| *b)
}

/// A batch's host clock: the tick's jitter, and a bracket that is clean (200 us wide, the read's
/// own time) except for a rare wide one, which the host was preempted inside.
fn board_plan(jitter: &mut Jitter, b: u64) -> Plan {
    let wide = b % 37 == 18;
    let tick_outlier = b % 37 == 36;
    Plan {
        tick_jitter_ns: if tick_outlier {
            jitter.next(9_000_000)
        } else {
            jitter.next(300_000)
        },
        pre_ns: if wide {
            -2_500_000
        } else {
            jitter.next(50_000) - 100_000
        },
        post_ns: if wide {
            2_500_000
        } else {
            jitter.next(50_000) + 100_000
        },
        report_early_ns: 0,
        fault: None,
    }
}

/// Every delivered sample is the next one after the last, once each.
fn contiguous(delivered: &[(u64, Delivered)]) {
    for pair in delivered.windows(2) {
        assert_eq!(
            pair[1].1.seq,
            pair[0].1.seq + 1,
            "a sample was skipped or repeated: {:?} then {:?}",
            pair[0].1,
            pair[1].1
        );
    }
}

/// The board run's oscillator (`testing/board-runs/B8.md`): 9206 records in 45.00 s at a nominal
/// 200 Hz, so the die is 2.29 % fast.
pub(crate) const BOARD_EPS: f64 = 9206.0 / 9000.0 - 1.0;

/// The limit the board run's span check asks for: 1 ms.
pub(crate) const BOARD_LIMIT_NS: f64 = 1_000_000.0;

/// The batch-miss note, as health shows it.
const BATCH_NOTE: &str = "accelerometer: batch missed samples";

/// The board run, as it was taken (B8, 2026-11-17): a die 2.29 % fast at 200 Hz, 45 s, the host's
/// tick jitter with the rare 9 ms outlier the run's 9.13 ms gap came from, and a batch read after
/// every drain. From the first record on, every sample is delivered once, in order, within 1 ms of
/// its true time; the stamps span the samples' true span to within 1 ms (the run was 668 ms off);
/// no two consecutive stamps are more than 1.5 periods apart (the run had 46 such gaps); and
/// health never reports a batch miss, the convergence's dropped samples included.
#[test]
fn a_board_run_of_45_s_is_stamped_within_a_millisecond_without_gaps() {
    let osc = Oscillator {
        eps: BOARD_EPS,
        drift: 0.0,
    };
    let mut h = Harness::new(osc, 200);
    let mut jitter = Jitter::new(0x9e37_79b9);
    // 123 batches is 45 s.
    let mut delivered = Vec::new();
    for b in 0..123 {
        let out = h.step(board_plan(&mut jitter, b));
        assert_ne!(h.note, Some(BATCH_NOTE), "batch {b}: a miss after its tick");
        assert_ne!(
            Driver::health(&h.dev),
            Health::Degraded(BATCH_NOTE),
            "batch {b}: a miss after its read"
        );
        delivered.extend(out.into_iter().map(|d| (b, d)));
    }
    let first = first_delivery(&delivered).expect("the clock never converged");
    assert!(
        first <= 20,
        "the clock converges at batch {first}, which is past the 20 the run allows"
    );
    let stamps: Vec<Delivered> = delivered.iter().map(|(_, d)| *d).collect();
    contiguous(&delivered);
    assert_eq!(
        stamps.last().unwrap().seq,
        h.seq - 1,
        "every sample up to the newest is delivered"
    );
    // The chip's 45 s at 2.29 % fast, less what came before the first record.
    let expected = (h.seq - stamps[0].seq) as f64;
    let rate = expected / ((h.truth(h.seq - 1) - h.truth(stamps[0].seq)) * 1e-9);
    assert!(
        (rate - 200.0 * (1.0 + BOARD_EPS)).abs() < 1.0,
        "the run's rate is {rate} Hz"
    );
    assert!(no_going_back(&stamps).is_none(), "a stamp went back");
    let gap = largest_gap(&stamps);
    assert!(
        gap <= 7_500_000,
        "a gap of {gap} ns between samples (limit 7.5 ms)"
    );
    let worst = errors(&h, &stamps)
        .into_iter()
        .map(f64::abs)
        .fold(0.0f64, f64::max);
    assert!(
        worst <= BOARD_LIMIT_NS,
        "a stamp was {worst} ns from the true time once converged"
    );
    let span = stamps.last().unwrap().at_ns as f64 - stamps[0].at_ns as f64;
    let true_span = h.truth(stamps.last().unwrap().seq) - h.truth(stamps[0].seq);
    assert!(
        (span - true_span).abs() <= BOARD_LIMIT_NS,
        "the stamps span {span} ns, the samples {true_span} ns"
    );
}

#[test]
fn a_run_of_an_hour_with_0_1_percent_drift_per_hour_stays_within_a_millisecond() {
    // The rate is 1.5 % fast, and drifts by 0.1 % per hour as the die warms.
    let osc = Oscillator {
        eps: 0.015,
        drift: 0.001 / 3600.0,
    };
    let mut h = Harness::new(osc, 200);
    let mut jitter = Jitter::new(0x5851_f42d);
    let mut worst = 0.0f64;
    let mut count = 0u64;
    let mut last = None;
    let mut converged_at = None;
    let mut b = 0;
    while h.t_ns < START_NS + 3_600_000_000_000 {
        let out = h.step(board_plan(&mut jitter, b));
        if !out.is_empty() && converged_at.is_none() {
            converged_at = Some(b);
        }
        b += 1;
        for d in &out {
            let err = (d.at_ns as f64 - h.truth(d.seq)).abs();
            worst = worst.max(err);
            count += 1;
            if let Some(prev) = last {
                assert!(d.at_ns >= prev, "a stamp went back at batch {b}");
            }
            last = Some(d.at_ns);
        }
    }
    assert!(
        converged_at.is_some_and(|b| b <= 20),
        "converged at {converged_at:?}"
    );
    assert!(count > 700_000, "only {count} samples were delivered");
    assert!(
        worst <= BOARD_LIMIT_NS,
        "a stamp was {worst} ns from the true time over the hour"
    );
}

/// Uniform jitter of 9 ms on every batch's tick and on every host reading of the bracket: the
/// model still never goes back, leaves no gap over 1.5 periods, and is within the bound below.
///
/// The bound is set from measurement: over 48 seeds, 123 batches (45 s) each, the worst error was
/// 6.3 ms and the largest gap 6.7 ms, and every run converged by batch 35. 7 ms bounds the error,
/// and 7.5 ms is the gap limit. The error is the fit's: every host reading the line is drawn
/// through is off by up to 9 ms.
pub(crate) const STRESS_BOUND_NS: f64 = 7_000_000.0;

#[test]
fn a_uniform_9_ms_jitter_stress_is_monotonic_without_gaps_within_its_stated_bound() {
    let osc = Oscillator {
        eps: 0.015,
        drift: 0.0,
    };
    let mut h = Harness::new(osc, 200);
    let mut jitter = Jitter::new(0x1234_5678);
    let delivered = run(&mut h, 123, |_| {
        // Both readings of the bracket are off by the same amount: the host's error, not its wait.
        let host = jitter.next(9_000_000);
        Plan {
            tick_jitter_ns: jitter.next(9_000_000),
            pre_ns: host,
            post_ns: host,
            report_early_ns: 0,
            fault: None,
        }
    });
    // The bracket is narrow even when its readings are off, so these pairs are all used.
    let stamps: Vec<Delivered> = delivered.iter().map(|(_, d)| *d).collect();
    assert!(
        first_delivery(&delivered).is_some(),
        "the clock never converged"
    );
    assert!(
        no_going_back(&stamps).is_none(),
        "a stamp went back under jitter"
    );
    let gap = largest_gap(&stamps);
    assert!(
        gap <= 7_500_000,
        "a gap of {gap} ns under jitter (limit 7.5 ms)"
    );
    let worst = errors(&h, &stamps)
        .into_iter()
        .map(f64::abs)
        .fold(0.0f64, f64::max);
    assert!(
        worst <= STRESS_BOUND_NS,
        "a stamp was {worst} ns from the true time under 9 ms jitter (bound {STRESS_BOUND_NS} ns)"
    );
}

#[test]
fn stamps_stay_monotonic_while_the_model_is_updated_on_every_batch() {
    // Every batch moves the fit, under a different jitter from the stress test's, and the time the
    // host hands each tick is up to 9 ms before the drain: a stamp taken from it would go back.
    let osc = Oscillator {
        eps: 0.015,
        drift: -0.001 / 45.0,
    };
    let mut h = Harness::new(osc, 200);
    let mut jitter = Jitter::new(0xdead_beef);
    let delivered = run(&mut h, 300, |_| {
        let host = jitter.next(9_000_000);
        Plan {
            tick_jitter_ns: jitter.next(9_000_000),
            pre_ns: host,
            post_ns: host,
            report_early_ns: jitter.next(9_000_000).unsigned_abs(),
            fault: None,
        }
    });
    let stamps: Vec<Delivered> = delivered.iter().map(|(_, d)| *d).collect();
    assert!(
        first_delivery(&delivered).is_some(),
        "the clock never converged"
    );
    assert!(
        no_going_back(&stamps).is_none(),
        "a stamp went back as the model was updated"
    );
    let updates = delivered
        .iter()
        .map(|(b, _)| *b)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        updates.len() > 200,
        "the model was only fed {} batches",
        updates.len()
    );
}

/// A reset, a jump and an overflow each restart the model. Health names it, the stamps never go
/// back, nothing is published until the model has converged again, and once it has, the stamps
/// are within the limit again.
fn fault_at(fault: Fault, label: &str, note: &str) {
    let osc = Oscillator {
        eps: 0.015,
        drift: 0.0,
    };
    let mut h = Harness::new(osc, 200);
    let mut jitter = Jitter::new(0x0bad_cafe);
    let fault_batch = 40;
    let mut seen_note = false;
    let mut delivered = Vec::new();
    for b in 0..120 {
        let mut plan = board_plan(&mut jitter, b);
        if b == fault_batch {
            plan.fault = Some(fault);
        }
        let out = h.step(plan);
        if b == fault_batch {
            seen_note = h.note == Some(note);
        }
        for d in out {
            delivered.push((b, d));
        }
    }
    assert!(
        seen_note,
        "{label}: health did not say {note:?} at batch {fault_batch}"
    );
    let stamps: Vec<Delivered> = delivered.iter().map(|(_, d)| *d).collect();
    assert!(
        no_going_back(&stamps).is_none(),
        "{label}: a stamp went back"
    );
    // Nothing is published between the fault and the convergence that follows it: at least
    // BATCHES host-timed batches must pass first.
    let resumed = delivered
        .iter()
        .find(|(b, _)| *b > fault_batch)
        .map(|(b, _)| *b)
        .expect("the clock never converged again");
    assert!(
        resumed >= fault_batch + 16,
        "{label}: stamps resumed at batch {resumed}, before the model had reconverged"
    );
    let after: Vec<&(u64, Delivered)> = delivered
        .iter()
        .filter(|(b, _)| *b >= resumed + 2)
        .collect();
    let worst = after
        .iter()
        .map(|(_, d)| (d.at_ns as f64 - h.truth(d.seq)).abs())
        .fold(0.0f64, f64::max);
    assert!(
        worst <= BOARD_LIMIT_NS,
        "{label}: a stamp after reconverging was {worst} ns from the true time"
    );
}

#[test]
fn a_sensor_time_reset_re_anchors_reports_it_and_never_goes_back() {
    fault_at(Fault::Reset, "reset", RESET_NOTE);
}

#[test]
fn a_sensor_time_jump_re_anchors_reports_it_and_never_goes_back() {
    fault_at(Fault::Jump(25_600), "jump", JUMP_NOTE);
}

#[test]
fn a_fifo_overflow_re_anchors_reports_it_and_never_goes_back() {
    fault_at(Fault::Overflow(3), "overflow", OVERFLOW_NOTE);
}

/// The rates at which the earlier model's phase stuck more than 1 ms off: its drains, a fixed time
/// apart, met the sample grid at a few points only. Every rate from 4 % slow to 4 % fast, in steps
/// of 0.5 %, and those, are stamped within 1 ms.
#[test]
fn every_rate_within_4_percent_is_stamped_within_a_millisecond() {
    let stuck = [
        -0.027, -0.014, -0.011, -0.009, -0.007, 0.003, 0.007, 0.014, 0.023,
    ];
    let steps = (-8..=8).map(|i| f64::from(i) * 0.005);
    for eps in steps.chain(stuck) {
        let mut h = Harness::new(Oscillator { eps, drift: 0.0 }, 200);
        let mut jitter = Jitter::new(0x9e37_79b9);
        let delivered = run(&mut h, 60, |b| board_plan(&mut jitter, b));
        let first = first_delivery(&delivered);
        assert!(
            first.is_some_and(|b| b <= 20),
            "{eps}: converged at {first:?}"
        );
        let stamps: Vec<Delivered> = delivered.iter().map(|(_, d)| *d).collect();
        let worst = errors(&h, &stamps)
            .into_iter()
            .map(f64::abs)
            .fold(0.0f64, f64::max);
        assert!(
            worst <= BOARD_LIMIT_NS,
            "{eps}: a stamp was {worst} ns from the true time"
        );
    }
}

/// A tick's drain reads the sensor time just before the FIFO's length and just after it, and the
/// FIFO's data only after both: the FIFO's read takes milliseconds, and a sensor time read after
/// it would put the phase that much late.
#[test]
fn a_drain_reads_the_sensor_time_around_the_fifo_length_and_before_the_data() {
    let mut h = Harness::new(
        Oscillator {
            eps: 0.0,
            drift: 0.0,
        },
        200,
    );
    h.chip.logging.store(true, Ordering::SeqCst);
    h.fill_and_tick(Plan::clean());
    h.chip.logging.store(false, Ordering::SeqCst);
    let log = h.chip.log.lock().unwrap().clone();
    assert!(log.len() > 4, "the drain read {log:02x?}");
    assert_eq!(
        log[..4],
        [SENSORTIME, ACC_FIFO_LENGTH, SENSORTIME, ACC_FIFO_DATA],
        "the drain read {log:02x?}"
    );
    assert!(
        log[4..].iter().all(|reg| *reg == ACC_FIFO_DATA),
        "the drain read {log:02x?}"
    );
}

// The gyroscope. It has no sensor time: its clock is the count of samples its FIFO holds, read
// between two host clock readings. The chip's own oscillator makes the samples, so its stamps are
// checked against the true time of each sample, as the accelerometer's are.

use lemnos_contract_gyroscope::RECORD_LEN as GYRO_RECORD_LEN;

/// The gyroscope's sample clock as the chip keeps it: `rate` samples per second at its nominal
/// rate, `eps` fast at the start, and its rate changing by `drift` per second.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GyroOsc {
    pub(crate) rate: f64,
    pub(crate) eps: f64,
    pub(crate) drift: f64,
}

impl GyroOsc {
    /// Samples made by host time `t` seconds, as a real number.
    fn count(&self, t: f64) -> f64 {
        self.rate * ((1.0 + self.eps) * t + 0.5 * self.drift * t * t)
    }

    /// The host time (seconds) at which `count` samples have been made.
    fn time_of(&self, count: f64) -> f64 {
        let c = count / self.rate;
        let r = 1.0 + self.eps;
        if self.drift == 0.0 {
            c / r
        } else {
            (-r + (r * r + 2.0 * self.drift * c).sqrt()) / self.drift
        }
    }

    /// The true host time, ns, of the chip's sample `n` (counted from the chip's start): the time
    /// its count reaches `n + 1`.
    pub(crate) fn sample_ns(&self, n: u64) -> f64 {
        self.time_of((n + 1) as f64) * 1e9
    }
}

/// The batch by which the gyroscope's clock has converged in these runs: its model publishes after
/// 64 reads (`gyro_clock.rs`), and a drain every 250 ms at 200 Hz makes that about 16 s, batch 64,
/// plus the drains the window takes to close. The run measured 70 (board, 2 %) and 75 (hour, 2 %
/// drifting), so 80 is the bound the runs are checked against.
const GYRO_CONVERGED_BY: u64 = 80;

/// The gyroscope rate the board run uses: 200 Hz, as the accelerometer's (B8).
pub(crate) const GYRO_RATE: i64 = 200;
/// A gyroscope drain's frames are read at the tick's time plus this (one short transfer).
const GYRO_LEAD_NS: u64 = 150_000;
/// The gyroscope's board-run gap limit: 1.5 sample periods at 200 Hz.
const GYRO_GAP_NS: u64 = 7_500_000;

/// Binds the driver to a simulated chip whose accelerometer is nominal and idle (its FIFO is never
/// fed), and whose gyroscope runs on `osc`. The driver's gyroscope drains are driven by hand.
pub(crate) struct GyroHarness {
    pub(crate) dev: Bmi088,
    sim: SimI2c,
    chip: Arc<Chip>,
    pub(crate) osc: GyroOsc,
    /// The chip's sample count at the start of the harness: the driver's sequence number 0.
    pub(crate) base: u64,
    /// The next sequence number to make a frame for (samples the chip dropped included).
    pub(crate) seq: u64,
    /// The true time of the last drain's tick.
    pub(crate) t_ns: u64,
    /// The gyroscope's next drain is due at this time, as the driver last said.
    due_ns: u64,
    /// The batch index, from 0.
    pub(crate) batch: u64,
    /// Health after the last tick.
    pub(crate) note: Option<&'static str>,
}

impl GyroHarness {
    pub(crate) fn new(osc: GyroOsc) -> Self {
        let nominal = Oscillator {
            eps: 0.0,
            drift: 0.0,
        };
        let board = SimBoard::new();
        let sim = board.add_i2c("bus");
        sim.attach(ACCEL, Layout::Bytes);
        sim.set_bytes(ACCEL, ACC_ID, &[0x1e]);
        sim.attach(GYRO, Layout::Bytes);
        sim.set_bytes(GYRO, GYR_ID, &[0x0f]);
        sim.add_fifo(
            ACCEL,
            ACC_FIFO_DATA,
            FifoLevel::Bytes(ACC_FIFO_LENGTH),
            1024,
        );
        sim.add_fifo(
            GYRO,
            GYR_FIFO_DATA,
            FifoLevel::Frames {
                register: GYR_FIFO_STATUS,
                frame: 6,
            },
            600,
        );
        let chip = Arc::new(Chip {
            osc: nominal,
            drain_ns: AtomicU64::new(0),
            at_ns: AtomicU64::new(0),
            reads: AtomicU32::new(0),
            log: std::sync::Mutex::new(Vec::new()),
            logging: core::sync::atomic::AtomicBool::new(false),
            shift: AtomicI64::new(0),
            pre: AtomicI64::new(0),
            post: AtomicI64::new(0),
            calls: AtomicU32::new(0),
        });
        let mut rig = Rig {
            _board: board,
            sim: sim.clone(),
            chip: Arc::clone(&chip),
        };
        let entries = [
            ("fifo", ConfigValue::Bool(true)),
            ("gyro_rate", ConfigValue::Int(GYRO_RATE)),
        ];
        let mut res = Resources::new(&mut rig, None);
        let mut dev = Bmi088::bind(&mut res, &Config::new(&entries)).unwrap();
        // The chip's count is since its configuration, at time 0: during the bind every tick reads
        // the FIFO at its own true time, with the frames the chip has made by then. A drain's count
        // is then the chip's count, as it is on a board.
        let mut seq = 0u64;
        let mut step = 0u64;
        while Driver::health(&dev) != Health::Ok {
            let t = step * 1_000_000;
            let produced = osc.count(t as f64 * 1e-9).floor() as u64;
            let mut bytes = Vec::new();
            while seq < produced {
                bytes.extend_from_slice(&(seq as i16).to_le_bytes());
                bytes.extend_from_slice(&[0, 0, 0, 0]);
                seq += 1;
            }
            if !bytes.is_empty() {
                sim.fifo_push(GYRO, &bytes);
            }
            chip.drain_ns.store(t, Ordering::SeqCst);
            chip.reads.store(0, Ordering::SeqCst);
            chip.at_ns.store(t, Ordering::SeqCst);
            chip.pre.store(0, Ordering::SeqCst);
            chip.post.store(0, Ordering::SeqCst);
            chip.calls.store(0, Ordering::SeqCst);
            Driver::tick(&mut dev, t);
            step += 1;
            assert!(step < 10_000, "the device never came up");
        }
        let t_ns = (step - 1) * 1_000_000;
        let due_ns = dev.gyro.due.unwrap_or(t_ns);
        Self {
            dev,
            sim,
            chip,
            osc,
            base: 0,
            seq,
            t_ns,
            due_ns,
            batch: 0,
            note: None,
        }
    }

    /// The true host time of driver sequence number `seq`.
    pub(crate) fn truth(&self, seq: u64) -> f64 {
        self.osc.sample_ns(self.base + seq)
    }

    /// One gyroscope drain: the chip makes its samples up to the drain's read, the host ticks late
    /// by the plan's jitter, and the driver reads the FIFO's fill between two host readings (the
    /// plan's bracket), then drains it.
    pub(crate) fn fill_and_tick(&mut self, plan: Plan) {
        let tick = self.due_ns.max(self.t_ns + 1) + plan.tick_jitter_ns.unsigned_abs();
        let read = tick + GYRO_LEAD_NS;
        self.t_ns = tick;
        // The chip has made every sample up to the read. The FIFO holds 100 frames and drops the
        // rest; the frames it dropped still take their numbers.
        let produced =
            (self.osc.count(read as f64 * 1e-9).floor() as u64).saturating_sub(self.base);
        let mut bytes = Vec::new();
        while self.seq < produced {
            bytes.extend_from_slice(&(self.seq as i16).to_le_bytes());
            bytes.extend_from_slice(&[0, 0, 0, 0]);
            self.seq += 1;
        }
        if !bytes.is_empty() {
            self.sim.fifo_push(GYRO, &bytes);
        }
        // The accelerometer's sensor time is read too, but its FIFO is empty and it is not checked.
        self.chip.drain_ns.store(read, Ordering::SeqCst);
        self.chip.reads.store(0, Ordering::SeqCst);
        self.chip.at_ns.store(read, Ordering::SeqCst);
        self.chip.pre.store(plan.pre_ns, Ordering::SeqCst);
        self.chip.post.store(plan.post_ns, Ordering::SeqCst);
        self.chip.calls.store(0, Ordering::SeqCst);
        let _ = Driver::tick(&mut self.dev, tick.saturating_sub(plan.report_early_ns));
        self.due_ns = self.dev.gyro.due.expect("a gyroscope drain is due");
        self.note = match Driver::health(&self.dev) {
            Health::Degraded(text) => Some(text),
            _ => None,
        };
        self.batch += 1;
    }

    /// A drain, then a batch read of at most `records` records.
    pub(crate) fn step(&mut self, plan: Plan) -> Vec<Delivered> {
        self.fill_and_tick(plan);
        self.take(160)
    }

    /// A batch read of at most `records` gyroscope records: what the ring holds that the caller has
    /// not had. A record's sequence number is the chip's, from the frame's X value.
    pub(crate) fn take(&mut self, records: usize) -> Vec<Delivered> {
        let mut delivered = Vec::new();
        let mut buf = std::vec![0u8; GYRO_RECORD_LEN * records];
        let n = Gyroscope::read_batch(&mut self.dev, &mut buf).unwrap();
        let newest = self.seq;
        let scale = chip::gyro_setting(2000, GYRO_RATE).unwrap().scale as f64;
        for record in buf[..n].as_chunks::<GYRO_RECORD_LEN>().0 {
            let sample = GyroSample::decode(&mut Reader::new(record)).unwrap();
            let raw = (sample.value.x as f64 / scale).round() as i64 as u16;
            let back = (newest.wrapping_sub(1).wrapping_sub(u64::from(raw))) & 0xffff;
            let seq = newest - 1 - back;
            delivered.push(Delivered {
                seq,
                at_ns: sample.at_ns,
            });
        }
        delivered
    }
}

/// The error of each gyroscope stamp, in ns, against the true time of its sample.
pub(crate) fn gyro_errors(h: &GyroHarness, delivered: &[Delivered]) -> Vec<f64> {
    delivered
        .iter()
        .map(|d| d.at_ns as f64 - h.truth(d.seq))
        .collect()
}

/// Uniform jitter of `spread` ns on every tick.
fn gyro_plan(jitter: &mut Jitter, b: u64, spread: i64, bracket: i64) -> Plan {
    let wide = b % 37 == 18;
    Plan {
        tick_jitter_ns: jitter.next(spread),
        pre_ns: if wide {
            -2_500_000
        } else {
            jitter.next(bracket) - bracket / 2
        },
        post_ns: if wide {
            2_500_000
        } else {
            jitter.next(bracket) + bracket / 2
        },
        report_early_ns: 0,
        fault: None,
    }
}

/// A board-shaped plan: 0.3 ms of tick jitter with a 9 ms outlier every 37th tick, and a bracket
/// about 0.1 ms wide, with a wide one (the host was preempted inside the read) every 37th.
fn gyro_board_plan(jitter: &mut Jitter, b: u64) -> Plan {
    let mut plan = gyro_plan(jitter, b, 300_000, 100_000);
    if b % 37 == 36 {
        plan.tick_jitter_ns = jitter.next(9_000_000);
    }
    plan
}

/// The gyroscope's board run, as B8 would take it on the same chip: 45 s at 200 Hz with the die 2 %
/// fast, the host's tick jitter and outliers as the accelerometer's run had them.
#[test]
fn a_gyroscope_board_run_of_45_s_is_stamped_from_its_clock_within_a_millisecond_without_gaps() {
    let osc = GyroOsc {
        rate: 200.0,
        eps: 0.02,
        drift: 0.0,
    };
    let mut h = GyroHarness::new(osc);
    let mut jitter = Jitter::new(0x9e37_79b9);
    let mut delivered = Vec::new();
    // 45 s at a drain every 250 ms.
    for b in 0..180 {
        let out = h.step(gyro_board_plan(&mut jitter, b));
        delivered.extend(out.into_iter().map(|d| (b, d)));
    }
    let first = delivered
        .first()
        .map(|(b, _)| *b)
        .expect("the clock never converged");
    assert!(
        first <= GYRO_CONVERGED_BY,
        "the clock converges at batch {first}, past {GYRO_CONVERGED_BY}"
    );
    let stamps: Vec<Delivered> = delivered.iter().map(|(_, d)| *d).collect();
    assert!(no_going_back(&stamps).is_none(), "a stamp went back");
    let gap = largest_gap(&stamps);
    assert!(
        gap <= GYRO_GAP_NS,
        "a gap of {gap} ns between gyroscope samples (limit 7.5 ms)"
    );
    let worst = gyro_errors(&h, &stamps)
        .into_iter()
        .map(f64::abs)
        .fold(0.0f64, f64::max);
    assert!(
        worst <= BOARD_LIMIT_NS,
        "a stamp was {worst} ns from its true time"
    );
    let span = stamps.last().unwrap().at_ns as f64 - stamps[0].at_ns as f64;
    let true_span = h.truth(stamps.last().unwrap().seq) - h.truth(stamps[0].seq);
    assert!(
        (span - true_span).abs() <= BOARD_LIMIT_NS,
        "the stamps span {span} ns, the samples {true_span} ns"
    );
}

#[test]
fn a_gyroscope_run_of_an_hour_with_0_1_percent_drift_per_hour_stays_within_a_millisecond() {
    // 2 % fast at the start, and drifting by 0.1 % per hour as the die warms.
    let osc = GyroOsc {
        rate: 200.0,
        eps: 0.02,
        drift: 0.001 / 3600.0,
    };
    let mut h = GyroHarness::new(osc);
    let mut jitter = Jitter::new(0x5851_f42d);
    let mut worst = 0.0f64;
    let mut count = 0u64;
    let mut last = None;
    let mut converged_at = None;
    let mut b = 0;
    while h.t_ns < START_NS + 3_600_000_000_000 {
        let out = h.step(gyro_board_plan(&mut jitter, b));
        if !out.is_empty() && converged_at.is_none() {
            converged_at = Some(b);
        }
        b += 1;
        for d in &out {
            let err = (d.at_ns as f64 - h.truth(d.seq)).abs();
            worst = worst.max(err);
            count += 1;
            if let Some(prev) = last {
                assert!(d.at_ns >= prev, "a stamp went back at batch {b}");
            }
            last = Some(d.at_ns);
        }
    }
    assert!(
        converged_at.is_some_and(|b| b <= GYRO_CONVERGED_BY),
        "converged at {converged_at:?}, past {GYRO_CONVERGED_BY}"
    );
    assert!(
        count > 600_000,
        "only {count} gyroscope samples were delivered"
    );
    assert!(
        worst <= BOARD_LIMIT_NS,
        "a stamp was {worst} ns from its true time over the hour"
    );
}

/// The gyroscope's stress bounds under 9 ms of host error, from measurement over 12 seeds of 45 s
/// each (`GYRO_STRESS_SEEDS`). Its phase is read against the host clock at each bracket, so a
/// reading 9 ms off moves that read's window by 9 ms, and the worst stamp in the 12 runs was 14.9 ms
/// off, with a largest gap of 24.6 ms at a window's jump. The tick-time stamping this replaces is
/// 4.8 ms and 9.7 ms under the same runs (it reads the host clock once per tick, not at the bracket),
/// so under this stress the clock is the worse of the two. The bounds are the measured values,
/// rounded up: 16 ms and 25 ms. They are not 1 ms and 7.5 ms, which the board-shaped runs hold.
pub(crate) const GYRO_STRESS_BOUND_NS: f64 = 16_000_000.0;
pub(crate) const GYRO_STRESS_GAP_NS: u64 = 25_000_000;
/// The seeds the stress runs: each is 45 s with its own host errors.
const GYRO_STRESS_SEEDS: u64 = 12;

#[test]
fn a_gyroscope_uniform_9_ms_jitter_stress_is_monotonic_within_its_stated_bounds() {
    for seed in 0..GYRO_STRESS_SEEDS {
        let osc = GyroOsc {
            rate: 200.0,
            eps: 0.015,
            drift: 0.0,
        };
        let mut h = GyroHarness::new(osc);
        let mut jitter = Jitter::new(0x1234_5678 ^ (seed * 7919));
        let mut stamps = Vec::new();
        for _ in 0..180 {
            // Both readings of the bracket are off by the same amount: the host's error, not its wait.
            let host = jitter.next(9_000_000);
            stamps.extend(h.step(Plan {
                tick_jitter_ns: jitter.next(9_000_000),
                pre_ns: host,
                post_ns: host,
                report_early_ns: 0,
                fault: None,
            }));
        }
        assert!(!stamps.is_empty(), "seed {seed}: the clock never converged");
        assert!(
            no_going_back(&stamps).is_none(),
            "seed {seed}: a stamp went back under jitter"
        );
        let gap = largest_gap(&stamps);
        assert!(
            gap <= GYRO_STRESS_GAP_NS,
            "seed {seed}: a gap of {gap} ns under jitter (bound {GYRO_STRESS_GAP_NS} ns)"
        );
        let worst = gyro_errors(&h, &stamps)
            .into_iter()
            .map(f64::abs)
            .fold(0.0f64, f64::max);
        assert!(
            worst <= GYRO_STRESS_BOUND_NS,
            "seed {seed}: a stamp was {worst} ns from its true time under 9 ms jitter (bound {GYRO_STRESS_BOUND_NS} ns)"
        );
    }
}

#[test]
fn a_gyroscope_rate_sweep_from_minus_4_to_plus_4_percent_is_stamped_within_a_millisecond() {
    // 17 rates, from 4 % slow to 4 % fast, each a 45 s board-shaped run.
    for step in -8i64..=8 {
        let eps = step as f64 * 0.005;
        let osc = GyroOsc {
            rate: 200.0,
            eps,
            drift: 0.0,
        };
        let mut h = GyroHarness::new(osc);
        let mut jitter = Jitter::new(0x0bad_cafe ^ step as u64);
        let mut stamps = Vec::new();
        for b in 0..180 {
            for d in h.step(gyro_board_plan(&mut jitter, b)) {
                stamps.push(d);
            }
        }
        assert!(!stamps.is_empty(), "eps {eps}: the clock never converged");
        assert!(
            no_going_back(&stamps).is_none(),
            "eps {eps}: a stamp went back"
        );
        let worst = gyro_errors(&h, &stamps)
            .into_iter()
            .map(f64::abs)
            .fold(0.0f64, f64::max);
        assert!(
            worst <= BOARD_LIMIT_NS,
            "eps {eps}: a stamp was {worst} ns from its true time"
        );
        let gap = largest_gap(&stamps);
        assert!(gap <= GYRO_GAP_NS, "eps {eps}: a gap of {gap} ns");
    }
}

#[test]
fn gyroscope_stamps_stay_monotonic_while_the_model_is_updated_on_every_drain() {
    // Every drain moves the fit, under jitter that puts the tick up to 9 ms before the read.
    let osc = GyroOsc {
        rate: 200.0,
        eps: 0.015,
        drift: -0.001 / 45.0,
    };
    let mut h = GyroHarness::new(osc);
    let mut jitter = Jitter::new(0xdead_beef);
    let mut stamps = Vec::new();
    for _ in 0..180 {
        let host = jitter.next(9_000_000);
        stamps.extend(h.step(Plan {
            tick_jitter_ns: jitter.next(9_000_000),
            pre_ns: host,
            post_ns: host,
            report_early_ns: jitter.next(9_000_000).unsigned_abs(),
            fault: None,
        }));
    }
    assert!(!stamps.is_empty(), "the clock never converged");
    assert!(
        no_going_back(&stamps).is_none(),
        "a gyroscope stamp went back as the model was updated"
    );
}

/// An overflow of the gyroscope's FIFO re-anchors its clock, says so by name, and the stamps after
/// it converge again within the limit, without going back.
#[test]
fn a_gyroscope_fifo_overflow_re_anchors_its_clock_and_says_so() {
    let osc = GyroOsc {
        rate: 200.0,
        eps: 0.02,
        drift: 0.0,
    };
    let mut h = GyroHarness::new(osc);
    let mut jitter = Jitter::new(0x7777_1111);
    let mut stamps = Vec::new();
    for b in 0..80 {
        stamps.extend(h.step(gyro_board_plan(&mut jitter, b)));
    }
    assert!(
        !stamps.is_empty(),
        "the clock never converged before the overflow"
    );
    // The next drain comes 700 ms late: the chip makes about 140 frames, and the FIFO holds 100.
    let before = h.seq;
    h.due_ns = h.t_ns + 700_000_000;
    stamps.extend(h.step(gyro_board_plan(&mut jitter, 80)));
    let flooded = h.seq - before;
    assert!(flooded >= 100, "the flood made only {flooded} frames");
    let note = match Driver::health(&h.dev) {
        Health::Degraded(text) => text,
        other => panic!("after the overflow health is {other:?}"),
    };
    assert_eq!(
        note, "gyroscope: FIFO overflowed, samples lost; its sample clock was re-anchored",
        "the overflow was not reported by name"
    );
    for b in 81..180 {
        stamps.extend(h.step(gyro_board_plan(&mut jitter, b)));
    }
    assert!(
        no_going_back(&stamps).is_none(),
        "a stamp went back across the overflow"
    );
    let tail: Vec<Delivered> = stamps.iter().rev().take(60).rev().copied().collect();
    let worst = gyro_errors(&h, &tail)
        .into_iter()
        .map(f64::abs)
        .fold(0.0f64, f64::max);
    assert!(
        worst <= BOARD_LIMIT_NS,
        "after re-anchoring a stamp was {worst} ns from its true time"
    );
}
