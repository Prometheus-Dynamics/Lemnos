//! The state of one simulated chain: its sensor bus, its clock, its sensor-start queue, its
//! receiver block, its switches and its lock-step frame log.
//!
//! There are two kinds of chain. The process's own chain ([`LEGACY`]) is what the functions at the
//! crate root and in [`crate::time`] act on: the chain proof and the off-board daemon use it, one per
//! process, as they always have. Every other chain is an instance ([`Chain::instance`]) made by
//! [`crate::Sim`]: it owns all of its state, two instances in one process share none of it, and
//! its daemon's drivers find it by the board's name (the only thing a driver's `start` is given that
//! names its daemon; see [`Home::lookup`]).
//!
//! The PiSP blocks are not in here: the driver's simulated-block hook is installed once per process
//! (`lemnos-driver-pisp`), so the front ends stay the process's ([`crate::pisp`]).

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use lemnos_core::{Result, Status};
use lemnos_sim_clock::ClockWriter;

use crate::{Model, World, new_world_in, period_ps_in, scene::Scene};

/// The clock's start: ten seconds in, so a frame stamped a little earlier than its start is still
/// a time above zero (zero is how a placeholder frame is told).
pub const ORIGIN_NS: u64 = 10_000_000_000;

/// How long a sensor step waits for a receiver's tick before it is named as stuck.
pub const STEP_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How the receiver makes its frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Pace {
    /// On demand: a frame is made when a buffer is free, at the sensor's next start on the sim
    /// clock (the daemon's default, and the chain proof's).
    Demand = 0,
    /// On the sensor's steps: each step makes a frame, and a step with no buffer free drops it
    /// and counts it (the chain proof's paced runs use this).
    Stepped = 1,
    /// Against the clock: frames fall due on the sensor's frame period from its start, and a due
    /// frame with no free buffer is dropped and counted.
    Real = 2,
    /// One frame per grant ([`Chain::grant`]): a grant is not complete until its frame is made, and
    /// a frame waits for a buffer.
    Lockstep = 3,
}

impl Pace {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Stepped,
            2 => Self::Real,
            3 => Self::Lockstep,
            _ => Self::Demand,
        }
    }
}

/// A sensor fault, as [`crate::Sim::sensor_fault`] sets it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SensorFault {
    /// The sensor's bus answers with an error until the fault is cleared. The sensor stays bound.
    Failed(&'static str),
    /// The sensor is gone from its bus: the next transaction to it finds no device, and the fault
    /// is spent once the host has seen it. The host binds the sensor again.
    Gone(&'static str),
}

/// What the switches say, per chain.
#[derive(Default)]
pub struct Faults {
    pub sensor: Option<SensorFault>,
    /// The receiver's next starts that are refused with `IO`.
    pub refusals: u32,
    /// The receiver makes no frames and raises no fault. The first frame it did not make is kept.
    pub silent: bool,
    pub first_unmade: Option<u64>,
    /// The receiver reports [`lemnos_core::Health::Gone`] with this reason, for good.
    pub gone: Option<&'static str>,
    /// The next tick of the receiver ends its consumer.
    pub consumer: bool,
    /// How many times the receiver has reported itself gone (a health query while `gone` is set).
    pub gone_reports: u64,
}

/// A control a lock-step grant can carry to its frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    /// The exposure in microseconds.
    Exposure(u32),
    /// The analogue gain in thousandths (1000 is 1x).
    Gain(u32),
}

/// The lock-step frame log: what each frame was made with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub exposure_us: u32,
    pub analogue_gain_milli: u32,
    /// The controls that took effect on this frame.
    pub applied: [Option<Control>; 2],
    /// The sensor start the frame was made at (sim ns).
    pub sensor_timestamp_ns: u64,
}

#[derive(Default)]
pub struct Lock {
    /// Grants issued: frame ids below this are made or on their way.
    pub granted: u64,
    pub exposure_us: u32,
    pub gain_milli: u32,
    /// Controls waiting for their frame.
    pub pending: BTreeMap<u64, Vec<Control>>,
    /// The frames made, most recent [`META_KEEP`] of them.
    pub meta: BTreeMap<u64, Meta>,
}

/// How many frames' meta is kept.
pub const META_KEEP: u64 = 64;

/// The clock: the sim time, the sensor schedule, and the file a chain proof reads it from.
pub struct Clock {
    now: AtomicU64,
    file: Mutex<Option<ClockWriter>>,
    schedule: Mutex<Option<u64>>,
}

impl Clock {
    pub const fn new() -> Self {
        Self {
            now: AtomicU64::new(ORIGIN_NS),
            file: Mutex::new(None),
            schedule: Mutex::new(None),
        }
    }

    /// The simulated time now, in nanoseconds.
    pub fn now(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    /// Writes the clock to a file at `path` from now on, starting at the current time.
    pub fn install_file(&self, path: &Path) -> io::Result<()> {
        *self.file() = Some(ClockWriter::create(path, self.now())?);
        Ok(())
    }

    /// Moves the clock on to `ns`. A time not later than now changes nothing.
    pub fn advance_to(&self, ns: u64) {
        let mut file = self.file();
        if ns <= self.now.load(Ordering::SeqCst) {
            return;
        }
        self.now.store(ns, Ordering::SeqCst);
        if let Some(writer) = file.as_mut() {
            writer.set(ns).expect("sim clock file");
        }
    }

    /// The next sensor start, `period` ns after the clock if the schedule has none yet.
    pub fn peek_start(&self, period: u64) -> u64 {
        let mut next = self.schedule();
        *next.get_or_insert_with(|| self.now() + period)
    }

    /// Takes the next sensor start: the schedule moves on by `period`.
    pub fn take_start(&self, period: u64) -> u64 {
        let mut next = self.schedule();
        let at = *next.get_or_insert_with(|| self.now() + period);
        *next = Some(at + period);
        at
    }

    /// Forgets the schedule: the next start is a period after the clock. A new run starts this way.
    pub fn reset_schedule(&self) {
        *self.schedule() = None;
    }

    fn file(&self) -> MutexGuard<'_, Option<ClockWriter>> {
        self.file.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn schedule(&self) -> MutexGuard<'_, Option<u64>> {
        self.schedule.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

/// The receiver's sensor-start queue, and the receivers running on it.
pub struct Steps {
    pending: VecDeque<u64>,
    /// Steps asked for, and steps the receivers have made.
    issued: u64,
    done: u64,
    /// Receivers running on this chain.
    receivers: usize,
}

pub struct Chain {
    pub(crate) world: Mutex<Option<World>>,
    pub clock: Clock,
    steps: Mutex<Steps>,
    steps_cv: Condvar,
    /// The module's exclusion of the raw stream and the front end (see [`crate::EXCLUSIVE`]).
    pub(crate) exclusive: AtomicU8,
    pace: AtomicU8,
    /// Whether a test has injected the clock: a real-paced receiver then reads it, not the host's.
    injected: AtomicBool,
    pub(crate) faults: Mutex<Faults>,
    pub(crate) lock: Mutex<Lock>,
    pub(crate) scene: Mutex<Scene>,
}

/// The process's own chain, which the root functions of the crate act on.
pub static LEGACY: Chain = Chain::new();

impl Chain {
    pub const fn new() -> Self {
        Self {
            world: Mutex::new(None),
            clock: Clock::new(),
            steps: Mutex::new(Steps {
                pending: VecDeque::new(),
                issued: 0,
                done: 0,
                receivers: 0,
            }),
            steps_cv: Condvar::new(),
            exclusive: AtomicU8::new(crate::NOBODY),
            pace: AtomicU8::new(Pace::Demand as u8),
            injected: AtomicBool::new(false),
            faults: Mutex::new(Faults {
                sensor: None,
                refusals: 0,
                silent: false,
                first_unmade: None,
                gone: None,
                consumer: false,
                gone_reports: 0,
            }),
            lock: Mutex::new(Lock {
                granted: 0,
                exposure_us: crate::scene::REFERENCE_EXPOSURE_US,
                gain_milli: crate::scene::REFERENCE_GAIN_MILLI,
                pending: BTreeMap::new(),
                meta: BTreeMap::new(),
            }),
            scene: Mutex::new(Scene::Pattern),
        }
    }

    /// A new instance with its own world (the sensor at rest, its claims on).
    #[must_use]
    pub fn instance(model: Model) -> Self {
        let chain = Self::new();
        *chain.world.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(new_world_in(true, model));
        chain
    }

    pub fn with_world<R>(&self, f: impl FnOnce(&mut World) -> R) -> R {
        let mut guard = self.world.lock().unwrap_or_else(PoisonError::into_inner);
        f(guard.as_mut().expect("world"))
    }

    pub fn pace(&self) -> Pace {
        Pace::from_u8(self.pace.load(Ordering::SeqCst))
    }

    pub fn set_pace(&self, pace: Pace) {
        self.pace.store(pace as u8, Ordering::SeqCst);
    }

    pub fn injected(&self) -> bool {
        self.injected.load(Ordering::SeqCst)
    }

    pub fn set_injected(&self, on: bool) {
        self.injected.store(on, Ordering::SeqCst);
    }

    /// The frame period in ns from the registers (at least 1).
    pub fn period_ns(&self) -> u64 {
        let ps = self.with_world(|w| period_ps_in(w));
        u64::try_from(ps / 1000).unwrap_or(u64::MAX).max(1)
    }

    /// One sensor start on this chain: the clock moves to it, and it is returned.
    pub fn sensor_start(&self) -> u64 {
        let period = self.period_ns().max(1);
        let start = self.clock.take_start(period);
        self.clock.advance_to(start);
        start
    }

    /// The chain's clock for a sensor start and the grid: the injected clock, or the host's
    /// `CLOCK_MONOTONIC` (the same clock a daemon's tick is given).
    pub fn source_now(&self) -> u64 {
        if self.injected() {
            self.clock.now()
        } else {
            lemnos_linux_sys::time::monotonic_ns().unwrap_or(0).max(1)
        }
    }

    /// Whether a grant is queued and not yet taken by the receiver.
    pub fn grant_waiting(&self) -> bool {
        !self.steps().pending.is_empty()
    }

    /// Grants one frame (lock-step): the next sensor start is queued for the receiver, which makes
    /// it when a buffer is free. Returns the start.
    pub fn grant(&self) -> u64 {
        let start = self.sensor_start();
        self.queue_start(start);
        self.lock().granted += 1;
        start
    }

    /// Queues one start for the receivers, without waiting. The receiver makes it when it can.
    pub fn queue_start(&self, start: u64) {
        let mut s = self.steps();
        s.issued += 1;
        s.pending.push_back(start);
        self.steps_cv.notify_all();
    }

    /// Queues one start and waits until every receiver on the chain has taken it.
    ///
    /// # Panics
    /// A running receiver does not take the step within ten seconds: its daemon's tick is not running.
    pub fn step_receivers(&self, start: u64) {
        let mut s = self.steps();
        if s.receivers == 0 {
            return;
        }
        s.issued += 1;
        let wanted = s.issued;
        s.pending.push_back(start);
        self.steps_cv.notify_all();
        let deadline = std::time::Instant::now() + STEP_WAIT;
        while s.done < wanted {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                !left.is_zero(),
                "sensor start {start} was not taken by the receiver within {STEP_WAIT:?}: its tick is not running"
            );
            s = self
                .steps_cv
                .wait_timeout(s, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The receiver's next start to make, if one was queued.
    pub fn peek_step(&self) -> Option<u64> {
        self.steps().pending.front().copied()
    }

    /// Takes the next queued start off the queue, for the receiver to make. It leaves the queue
    /// before its frame is published, so a grant made when the frame is read is never blocked by
    /// the start that made it. [`Chain::untake_step`] puts it back if it cannot be made yet.
    pub fn take_step(&self) -> Option<u64> {
        self.steps().pending.pop_front()
    }

    /// Puts a taken start back at the front of the queue (no buffer was free for it).
    pub fn untake_step(&self, start: u64) {
        self.steps().pending.push_front(start);
    }

    /// The receiver has made the start it took.
    pub fn finish_step(&self) {
        let mut s = self.steps();
        s.done += 1;
        self.steps_cv.notify_all();
    }

    /// A receiver started (`true`) or stopped (`false`) on this chain.
    pub fn receivers(&self, started: bool) {
        let mut s = self.steps();
        if started {
            s.receivers += 1;
        } else {
            s.receivers = s.receivers.saturating_sub(1);
            // Starts it would have made are not waited for any more.
            s.pending.clear();
            s.done = s.issued;
            self.steps_cv.notify_all();
        }
    }

    /// Whether a receiver is running on this chain.
    pub fn has_receivers(&self) -> bool {
        self.steps().receivers > 0
    }

    /// Takes the block for `who` ([`crate::take`]).
    pub fn take(&self, who: u8) -> Result<()> {
        match self.exclusive.compare_exchange(
            crate::NOBODY,
            who,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => Ok(()),
            Err(held) if held == who && who == crate::FRONT_END => Ok(()),
            Err(_) => Err(Status::BUSY),
        }
    }

    /// Lets go of the block if `who` holds it.
    pub fn release(&self, who: u8) {
        let _ =
            self.exclusive
                .compare_exchange(who, crate::NOBODY, Ordering::SeqCst, Ordering::SeqCst);
    }

    fn steps(&self) -> MutexGuard<'_, Steps> {
        self.steps.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn faults(&self) -> MutexGuard<'_, Faults> {
        self.faults.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn lock(&self) -> MutexGuard<'_, Lock> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn scene(&self) -> MutexGuard<'_, Scene> {
        self.scene.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for Chain {
    fn default() -> Self {
        Self::new()
    }
}

/// Which chain a driver's block belongs to: the process's own one, or an instance found by the
/// board's name it was started with.
#[derive(Clone)]
pub enum Home {
    Legacy,
    Own(Arc<Chain>),
}

/// The instances this process runs, by the name of their board. An instance is listed while it
/// runs; a name is unique because [`crate::Sim`] makes it so.
static INSTANCES: Mutex<Vec<(String, Arc<Chain>)>> = Mutex::new(Vec::new());

impl Home {
    /// The instance whose board has this name, or the process's own chain when none has.
    pub fn lookup(board: &str) -> Self {
        let instances = INSTANCES.lock().unwrap_or_else(PoisonError::into_inner);
        match instances.iter().find(|(name, _)| name == board) {
            Some((_, chain)) => Self::Own(chain.clone()),
            None => Self::Legacy,
        }
    }

    /// Lists `chain` under `name` until the returned guard is dropped.
    pub fn register(name: &str, chain: Arc<Chain>) -> Registration {
        INSTANCES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((name.to_string(), chain));
        Registration {
            name: name.to_string(),
        }
    }

    /// The chain this home is.
    pub fn chain(&self) -> &Chain {
        match self {
            Self::Legacy => &LEGACY,
            Self::Own(chain) => chain,
        }
    }

    /// Whether this is an instance's chain (not the process's own).
    pub fn is_instance(&self) -> bool {
        matches!(self, Self::Own(_))
    }
}

/// A listed instance: unlisted when dropped.
pub struct Registration {
    name: String,
}

impl Drop for Registration {
    fn drop(&mut self) {
        INSTANCES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(name, _)| *name != self.name);
    }
}
